//! Durable message queue: enqueue, supersede and per-alias reads.

use crate::adapter::registry;
use crate::error::{Error, Result};
use crate::proto::identifier;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

use super::agents::Agent;
use super::schema::Take;
use super::StoreConn;
use super::{now, Sender, Store};

// Deterministic cost evidence for recurring read paths. Thread-local so
// unrelated parallel tests cannot add to a caller's measurement.
#[cfg(test)]
thread_local! {
    static DECODED_MESSAGES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn take_decoded_messages() -> usize {
    DECODED_MESSAGES.with(|n| n.replace(0))
}

/// The correlated worker result needed by the delivery router, without
/// decoding a kickoff body on every pending-turn check.
#[derive(Debug)]
pub(crate) struct WorkerTurn {
    pub id: String,
    pub state: String,
    pub created: f64,
    pub sha: Option<String>,
}

/// CAD-1822: the context selection committed atomically with a dispatch
/// enqueue. Only an accepted NEW message row persists it; a duplicate or a
/// rejected envelope writes nothing, so a colliding request cannot retarget a
/// queued turn's context.
pub enum IssueSelection {
    /// The accepted kickoff's `context_pin` payload.
    Pin(Value),
    /// The accepted assignment carries no opted-in pin. A `context_disabled`
    /// marker is written only when the alias already has feature state at this
    /// transaction's commit.
    Disabled { task: String, reason: String },
}

#[derive(Debug, Clone)]
pub struct Message {
    pub seq: i64,
    pub id: String,
    pub alias: String,
    pub body: String,
    pub reply_to: Option<String>,
    pub source: String,
    pub state: String,
    pub turn_id: Option<String>,
    pub result: Option<Value>,
    pub error: Option<String>,
    /// The task this delivery carries (a dispatch kickoff or a
    /// `--task` follow-up). NULL = unattached delivery.
    pub task_id: Option<String>,
    /// The dispatch lane: which issue's kickoff this is, and the
    /// worktree it ran against (CAD-467). Written at send by the
    /// daemon — a tracker's `message` ref is forgeable, so the
    /// reported-duplicate check matches only these. NULL = not a
    /// dispatch's.
    pub issue: Option<String>,
    pub worktree: Option<String>,
    /// Delivery rank (CAD-158): `urgent` is claimed ahead of `normal`.
    pub priority: Priority,
    pub created: f64,
    pub started: Option<f64>,
    pub completed: Option<f64>,
    /// CAD-1353: the stall budget chosen for this dispatch. `None` falls
    /// back to the job default, then the agent's `stall_secs`.
    pub stall_secs: Option<i64>,
    /// CAD-819: the daemon-derived sender participant (uid, label). NULL
    /// on a legacy row, which reads as an unknown sender.
    pub sender_uid: Option<String>,
    pub sender: Option<String>,
    pub recipient_uid: Option<String>,
    pub recipient: Option<String>,
    /// The original message a routed copy answers (CAD-819).
    pub origin: Option<String>,
}

pub(super) fn row_message(row: &rusqlite::Row) -> rusqlite::Result<Message> {
    #[cfg(test)]
    DECODED_MESSAGES.with(|n| n.set(n.get() + 1));
    let result: Option<String> = row.get("result")?;
    Ok(Message {
        seq: row.get("seq")?,
        id: row.get("id")?,
        alias: row.get("alias")?,
        body: row.get("body")?,
        reply_to: row.get("reply_to")?,
        source: row.get("source")?,
        state: row.get("state")?,
        turn_id: row.get("turn_id")?,
        result: result.and_then(|r| serde_json::from_str(&r).ok()),
        error: row.get("error")?,
        task_id: row.get("task_id")?,
        issue: row.get("issue")?,
        worktree: row.get("worktree")?,
        priority: Priority::from_rank(row.get("priority")?),
        created: row.get("created")?,
        started: row.get("started")?,
        completed: row.get("completed")?,
        stall_secs: row.get("stall_secs")?,
        sender_uid: row.get("sender_uid")?,
        sender: row.get("sender")?,
        recipient_uid: row.get("recipient_uid")?,
        recipient: row.get("recipient")?,
        origin: row.get("origin")?,
    })
}

impl Message {
    /// A daemon-routed delivery to a `reply_to` target — a worker's
    /// result (`worker_result`), an informational fence/closure
    /// notice (`worker_notice`), or a job notification (`job_event`).
    /// Routed copies carry no `reply_to`, are fire-and-forget on the
    /// recipient, and must never fence or become the recipient's own
    /// turn result.
    pub fn is_routed(&self) -> bool {
        matches!(
            self.source.as_str(),
            "worker_result" | "worker_notice" | "job_event"
        )
    }

    /// CAD-250: an operator/PM `send --nudge` — steering pasted into a
    /// live pty pane without owning a turn. Fire-and-forget like a routed
    /// notification: it passes the one-turn hold, never becomes `running`
    /// or `awaiting_report`, owes no report, and its `unknown` (an
    /// unconfirmed paste) never fences the agent.
    pub fn is_nudge(&self) -> bool {
        self.source == NUDGE_SOURCE
    }

    /// CAD-810: a daemon-origin topic notice. It never owns a turn and never
    /// reaches the provider: the claim consumes it in the store (see
    /// `take_queued_app_proven`), so `holds_turn` and `is_routed` stay false.
    pub fn is_topic_notice(&self) -> bool {
        self.source == crate::proto::TOPIC_NOTICE_SOURCE
    }

    /// CAD-250: a report-owing turn whose paste was delivered and that
    /// has no result report yet — `running` with the pty `submitted`
    /// marker (an ack keeps the marker). Derived, never stored: routed
    /// notifications complete at paste, and managed turns never carry
    /// the marker, so only a pty turn waiting on `message result` is
    /// ever `awaiting_report`.
    pub fn awaiting_report(&self) -> bool {
        self.state == "running"
            && !self.is_routed()
            && self
                .result
                .as_ref()
                .and_then(|r| r.get("status"))
                .and_then(Value::as_str)
                == Some("submitted")
    }

    /// CAD-250: the row holds its actor's one turn — `running` and not a
    /// turnless delivery (routed notice or nudge). Exactly what
    /// `take_queued`'s hold matches, and what the report bound covers, so
    /// no row can hold the queue without a bound — including a pty row
    /// adopted before its `submitted` marker landed.
    pub fn holds_turn(&self) -> bool {
        self.state == "running" && !self.is_routed() && !self.is_nudge()
    }

    /// When the report bound's clock started for a turn-holding row: the
    /// delivery (`started`, else `created`), restarted by the latest valid
    /// ack — an ack is the worker's own report that it holds the turn —
    /// and by an `agent recover-submit` Enter (CAD-152), which is when a
    /// lost submit's turn really reaches the worker. `None` for any other
    /// row.
    pub fn report_clock(&self) -> Option<f64> {
        if !self.holds_turn() {
            return None;
        }
        let delivered = self.started.unwrap_or(self.created);
        let stamp = |ptr: &str| {
            self.result
                .as_ref()
                .and_then(|r| r.pointer(ptr))
                .and_then(Value::as_f64)
        };
        Some(
            [stamp("/ack/at"), stamp("/recovered/at")]
                .into_iter()
                .flatten()
                .fold(delivered, f64::max),
        )
    }

    /// The report bound has run out at `now` — `bound == 0` disables it.
    pub fn report_overdue(&self, bound: u64, now: f64) -> bool {
        bound > 0
            && self
                .report_clock()
                .is_some_and(|clock| now - clock >= bound as f64)
    }

    pub fn to_json(&self) -> Value {
        let mut j = json!({
            "seq": self.seq, "id": self.id, "alias": self.alias,
            "body": self.body, "reply_to": self.reply_to, "source": self.source,
            "state": self.state, "turn_id": self.turn_id,
            "result": self.result, "error": self.error,
            "task_id": self.task_id,
            "issue": self.issue, "worktree": self.worktree,
            "created": self.created, "started": self.started,
            "completed": self.completed,
        });
        if self.source == "app_run_dispatch" {
            j["body"] = json!("[app-owned material: use authorized run surfaces]");
            j["result"] = Value::Null;
            j["error"] = self
                .error
                .as_ref()
                .map(|_| json!("app turn failed; inspect authorized run surfaces"))
                .unwrap_or(Value::Null);
        }
        // Derived and additive: present only on a delivered, unreported
        // pty turn, so every existing reader of `state` is unchanged.
        if self.awaiting_report() {
            j["awaiting_report"] = json!(true);
        }
        if self.is_nudge() {
            j["nudge"] = json!(true);
        }
        // Additive like `nudge`: only a non-default rank is shown.
        if self.priority != Priority::Normal {
            j["priority"] = json!(self.priority.as_str());
        }
        j
    }
}

/// CAD-250: how long a delivered pty turn may wait on its report before
/// it goes `unknown` (agent param `report_timeout_secs`; `0` disables).
pub const DEFAULT_REPORT_TIMEOUT_SECS: u64 = 7200;

/// The agent's report bound: `params.report_timeout_secs` (an integer or
/// a digit string, as `agent set` stores it), else the default.
pub fn report_timeout_secs(params: Option<&Value>) -> u64 {
    params
        .and_then(|p| p.get("report_timeout_secs"))
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(DEFAULT_REPORT_TIMEOUT_SECS)
}

/// The `source` of a `send --nudge` delivery ([`Message::is_nudge`]).
pub const NUDGE_SOURCE: &str = "nudge";

/// CAD-158: a queued message's delivery rank, stored as the
/// `messages.priority` integer. `take_queued` claims `urgent` ahead of
/// every `normal` row — FIFO within a rank — but only at a safe
/// boundary: the one-running-turn hold (CAD-250) and the pty submission
/// gate apply to it unchanged, so it never interrupts a running turn
/// or an open approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Priority {
    #[default]
    Normal,
    Urgent,
}

impl Priority {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "normal" => Ok(Priority::Normal),
            "urgent" => Ok(Priority::Urgent),
            other => Err(Error::rejected(format!(
                "priority must be normal or urgent, not '{other}'"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Priority::Normal => "normal",
            Priority::Urgent => "urgent",
        }
    }

    fn rank(self) -> i64 {
        match self {
            Priority::Normal => 0,
            Priority::Urgent => 1,
        }
    }

    fn from_rank(rank: i64) -> Self {
        if rank > 0 {
            Priority::Urgent
        } else {
            Priority::Normal
        }
    }
}

/// The one delivery order of an agent's queue — rank, then arrival.
/// `take_queued` claims by it and `queued_head` (the stall watch) reads
/// the same head.
const QUEUE_ORDER_SQL: &str = "ORDER BY priority DESC, seq";

/// CAD-158: how a send steers the recipient's queue — its rank, and the
/// still-queued messages it atomically replaces. `by`/`by_kind` is the
/// daemon-derived caller, stamped on every superseded row.
#[derive(Debug, Clone, Copy)]
pub struct Steer<'a> {
    pub priority: Priority,
    pub supersedes: &'a [String],
    pub by: &'a str,
    pub by_kind: &'a str,
}

impl Steer<'_> {
    /// A plain send: normal rank, replaces nothing.
    pub const NONE: Steer<'static> = Steer {
        priority: Priority::Normal,
        supersedes: &[],
        by: "",
        by_kind: "",
    };

    pub fn is_steering(&self) -> bool {
        self.priority != Priority::Normal || !self.supersedes.is_empty()
    }
}

/// Sources that never own a turn — [`Message::is_routed`] plus
/// [`Message::is_nudge`]; they pass the one-turn hold and never hold it.
const TURNLESS_SOURCES_SQL: &str =
    "('worker_result','worker_notice','job_event','nudge','topic_notice')";

/// The `unknown` rows that fence an agent: every one except a nudge's,
/// whose unconfirmed paste belongs to no turn (CAD-250).
pub(super) const FENCING_UNKNOWN_SQL: &str = "state='unknown' AND source != 'nudge'";

/// CAD-818: a topic notice that the claim has made `available` or `refused`
/// is history, not work — it never owns a turn and cannot be dispatched.
/// Only the `topic_notice` source ends this way; every other row keeps its
/// `state NOT IN (terminal)` classification.
const NOTICE_ENDED_SQL: &str = "(source = 'topic_notice' AND state IN ('available','refused'))";

/// Columns for [`Store::message_index`]. Body, result, error, and
/// `turn_id` are not selected.
const INDEX_COLS: &str = "seq, id, alias, state, source, sender, reply_to, task_id, issue, \
     priority, created, started, completed, length(body), \
     CASE WHEN state='running' \
          AND source NOT IN ('worker_result','worker_notice','job_event','nudge','topic_notice') \
          AND json_valid(result) AND json_extract(result,'$.status')='submitted' \
          THEN 1 ELSE 0 END";

/// One index row. `body_chars` is SQLite's character length of the stored
/// body; the text itself is not loaded.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageIndex {
    pub seq: i64,
    pub id: String,
    pub alias: String,
    pub state: String,
    pub source: String,
    pub sender: Option<String>,
    pub reply_to: Option<String>,
    pub task_id: Option<String>,
    pub issue: Option<String>,
    pub priority: Priority,
    pub created: f64,
    pub started: Option<f64>,
    pub completed: Option<f64>,
    pub body_chars: i64,
    pub awaiting_report: bool,
}

impl MessageIndex {
    pub fn to_json(&self) -> Value {
        let mut row = json!({
            "seq": self.seq,
            "id": self.id,
            "alias": self.alias,
            "state": self.state,
            "source": self.source,
            "reply_to": self.reply_to,
            "task_id": self.task_id,
            "issue": self.issue,
            "created": self.created,
            "started": self.started,
            "completed": self.completed,
            "body_chars": self.body_chars,
        });
        if let Some(sender) = &self.sender {
            row["from"] = json!(sender);
        }
        if self.priority != Priority::Normal {
            row["priority"] = json!(self.priority.as_str());
        }
        if self.awaiting_report {
            row["awaiting_report"] = json!(true);
        }
        if self.source == NUDGE_SOURCE {
            row["nudge"] = json!(true);
        }
        row
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MessageIndexPage {
    pub rows: Vec<MessageIndex>,
    pub omitted: usize,
}

/// Filters for [`Store::message_index`]. An empty `states` list matches
/// every state. `since` is a message id of this alias or a unix timestamp.
#[derive(Debug, Clone, Copy)]
pub struct MessageIndexQuery<'a> {
    pub states: &'a [String],
    pub from: Option<&'a str>,
    pub source: Option<&'a str>,
    pub since: Option<&'a str>,
    pub limit: usize,
    /// Operator listings include app-dispatch rows. Agent listings do not.
    pub include_app: bool,
}

#[derive(Clone)]
enum Bind {
    Text(String),
    Real(f64),
    Int(i64),
}

impl rusqlite::types::ToSql for Bind {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        match self {
            Bind::Text(value) => value.to_sql(),
            Bind::Real(value) => value.to_sql(),
            Bind::Int(value) => value.to_sql(),
        }
    }
}

fn index_query(
    conn: &rusqlite::Connection,
    sql: &str,
    binds: &[Bind],
) -> Result<Vec<MessageIndex>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(binds), row_index)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn count_query(conn: &rusqlite::Connection, sql: &str, binds: &[Bind]) -> Result<i64> {
    Ok(conn.query_row(sql, rusqlite::params_from_iter(binds), |row| row.get(0))?)
}

fn row_index(row: &rusqlite::Row) -> rusqlite::Result<MessageIndex> {
    let rank: i64 = row.get(9)?;
    let awaiting: i64 = row.get(14)?;
    Ok(MessageIndex {
        seq: row.get(0)?,
        id: row.get(1)?,
        alias: row.get(2)?,
        state: row.get(3)?,
        source: row.get(4)?,
        sender: row.get(5)?,
        reply_to: row.get(6)?,
        task_id: row.get(7)?,
        issue: row.get(8)?,
        priority: Priority::from_rank(rank),
        created: row.get(10)?,
        started: row.get(11)?,
        completed: row.get(12)?,
        body_chars: row.get(13)?,
        awaiting_report: awaiting == 1,
    })
}

/// Enqueue rejects a body over 48_000 bytes. The inlined spec is cut in
/// bytes, on a char boundary, so a multibyte spec cannot blow that limit.
pub const ENQUEUE_BYTES: usize = 48_000;

impl Store {
    /// Enqueue a durable message. Client-supplied `id` makes retries of the
    /// exact same envelope idempotent; reusing an id with different content
    /// is a conflict, not a duplicate.
    pub fn enqueue(
        &self,
        alias: &str,
        body: &str,
        reply_to: Option<&str>,
        id: &str,
        source: &str,
    ) -> Result<(bool, String)> {
        self.enqueue_task(alias, body, reply_to, id, source, None)
    }

    /// `enqueue` with an optional task attachment (`send --task`,
    /// dispatch kickoffs). The task must exist; the message carries
    /// `task_id` so the daemon can drive task edges off its state.
    pub fn enqueue_task(
        &self,
        alias: &str,
        body: &str,
        reply_to: Option<&str>,
        id: &str,
        source: &str,
        task_id: Option<&str>,
    ) -> Result<(bool, String)> {
        self.enqueue_sent(
            alias,
            body,
            reply_to,
            id,
            source,
            task_id,
            &Sender::Unattributed,
        )
    }

    /// `enqueue_task` with the daemon's attribution of who queued it —
    /// the role of the entry a threaded agent's chat records
    /// (CAD-319). Only the daemon derives `sender`, from the connection.
    #[allow(clippy::too_many_arguments)]
    pub fn enqueue_sent(
        &self,
        alias: &str,
        body: &str,
        reply_to: Option<&str>,
        id: &str,
        source: &str,
        task_id: Option<&str>,
        sender: &Sender,
    ) -> Result<(bool, String)> {
        self.enqueue_steered(
            alias,
            body,
            reply_to,
            id,
            source,
            task_id,
            None,
            None,
            sender,
            &Steer::NONE,
            None,
            None,
            None,
        )
    }

    /// `enqueue_sent` with steering (CAD-158): the new message's rank,
    /// and the still-`queued` messages it replaces — all in ONE
    /// transaction. Every named id must be `alias`'s own, still
    /// `queued` (never claimed, so never pasted), and an instruction:
    /// not a routed notice and not a task kickoff (`task cancel` owns
    /// that). Any other id refuses the whole call, naming it and its
    /// state, and nothing changes. Each superseded row keeps its history
    /// as `cancelled` with reason `superseded by <id>` and the caller,
    /// and its `reply_to` gets one `superseded` notice naming the new id.
    ///
    /// `issue`/`worktree` record the dispatch lane on the row
    /// (CAD-467): which issue's kickoff this is and the worktree it
    /// runs against. Only a dispatch may write them (CAD-378):
    /// `dispatch_send` resolves the lane daemon-side, `task_dispatch`
    /// reads its job rows; `send` refuses the fields outright and
    /// every other enqueue passes `None`.
    /// `refs` is the operator chat's cited rows (CAD-574): they land on
    /// the thread entry's payload and join the retry comparison — a
    /// resend naming different refs is the conflict it always was.
    /// `app` is the operator chat's server-verified App binding
    /// (CAD-802): same treatment — payload stamp plus retry
    /// comparison. Every other enqueue passes `None` for both.
    /// A retry of the same envelope is `duplicate` only when it names
    /// the same superseded set; otherwise it is a conflict.
    #[allow(clippy::too_many_arguments)]
    pub fn enqueue_steered(
        &self,
        alias: &str,
        body: &str,
        reply_to: Option<&str>,
        id: &str,
        source: &str,
        task_id: Option<&str>,
        issue: Option<&str>,
        worktree: Option<&str>,
        sender: &Sender,
        steer: &Steer,
        refs: Option<&Value>,
        app: Option<&Value>,
        attachments: Option<&Value>,
    ) -> Result<(bool, String)> {
        self.enqueue_steered_with_human_for(
            alias,
            None,
            body,
            reply_to,
            id,
            source,
            task_id,
            issue,
            worktree,
            sender,
            steer,
            refs,
            app,
            attachments,
            None,
        )
    }

    /// The one steering-enqueue body (CAD-1374 + CAD-1412): an optional
    /// expected recipient uid checked inside the write transaction AND
    /// the optional verified human stamped on the entry and `queued`
    /// event. Neither is a request field — both are daemon derivations;
    /// `None`/`None` is the ordinary path.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn enqueue_steered_with_human_for(
        &self,
        alias: &str,
        expected_uid: Option<&str>,
        body: &str,
        reply_to: Option<&str>,
        id: &str,
        source: &str,
        task_id: Option<&str>,
        issue: Option<&str>,
        worktree: Option<&str>,
        sender: &Sender,
        steer: &Steer,
        refs: Option<&Value>,
        app: Option<&Value>,
        attachments: Option<&Value>,
        human: Option<&Value>,
    ) -> Result<(bool, String)> {
        self.enqueue_steered_for_with_conversation(
            alias,
            expected_uid,
            body,
            reply_to,
            id,
            source,
            task_id,
            issue,
            worktree,
            sender,
            steer,
            refs,
            app,
            attachments,
            None,
            human,
            None,
            None,
            None,
        )
    }

    /// `authorized_until` is daemon-derived and checked after acquiring
    /// the queue writer. Ordinary sends pass `None`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn enqueue_steered_for_with_conversation(
        &self,
        alias: &str,
        expected_uid: Option<&str>,
        body: &str,
        reply_to: Option<&str>,
        id: &str,
        source: &str,
        task_id: Option<&str>,
        issue: Option<&str>,
        worktree: Option<&str>,
        sender: &Sender,
        steer: &Steer,
        refs: Option<&Value>,
        app: Option<&Value>,
        attachments: Option<&Value>,
        conversation: Option<&str>,
        human: Option<&Value>,
        media_refs: Option<&Value>,
        authorized_until: Option<f64>,
        selection: Option<IssueSelection>,
    ) -> Result<(bool, String)> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            if let Some(uid) = expected_uid {
                if self.agent_in(&tx, alias)?.uid != uid {
                    return Err(Error::rejected(
                        "Recipient changed before the request was queued; nothing was sent",
                    ));
                }
            }
            let mut named: Vec<&str> = Vec::new();
            for m in steer.supersedes {
                if !named.contains(&m.as_str()) {
                    named.push(m);
                }
            }
            let retry = self.message_in(&tx, id)?.is_some();
            let superseded = if retry {
                // The envelope itself is compared by `enqueue_tx`; the
                // superseded set is part of it.
                let mut replaced = Self::superseded_by_in(&tx, id)?;
                replaced.sort();
                let mut wanted: Vec<String> = named.iter().map(|m| m.to_string()).collect();
                wanted.sort();
                if replaced != wanted {
                    return Err(Error::rejected(
                        "Message id was already used with different content",
                    ));
                }
                Vec::new()
            } else {
                named
                    .iter()
                    .map(|m| self.supersedable_in(&tx, alias, m))
                    .collect::<Result<Vec<_>>>()?
            };
            // Existing-envelope retries mint no work; new turns must still
            // be authorized after the connection and SQLite writer waits.
            if !retry
                && authorized_until
                    .is_some_and(|until| crate::issue::time::now_epoch() as f64 >= until)
            {
                return Err(Error::rejected(
                    "the bound grant expired before the queue write — nothing was queued",
                ));
            }
            let out = self.enqueue_tx_with_conversation(
                &tx,
                alias,
                body,
                reply_to,
                id,
                source,
                task_id,
                issue,
                worktree,
                sender,
                steer.priority,
                false,
                refs,
                app,
                attachments,
                human,
                conversation,
                media_refs,
                selection,
            )?;
            for old in &superseded {
                self.supersede_in(&tx, old, id, steer)?;
            }
            self.notify_superseded(&tx, &superseded, id, steer)?;
            if !retry && steer.is_steering() {
                Self::event_scoped(
                    &tx,
                    alias,
                    "steered",
                    json!({"message": id, "priority": steer.priority.as_str(),
                                   "supersedes": named, "by": steer.by,
                                   "by_kind": steer.by_kind}),
                    None,
                    task_id,
                )?;
            }
            Ok(out)
        })
    }

    /// CAD-445: a message the daemon itself originates — a master wake,
    /// and any later system message. The only way in for an id with
    /// [`crate::proto::DAEMON_MESSAGE_PREFIX`] or a source of
    /// [`crate::proto::DAEMON_SOURCES`]; every other enqueue refuses both.
    /// Unattributed (a system entry in a thread), owing no report.
    /// CAD-468: `nudge` is daemon-writable too — the silent-end report
    /// reminder rides the turnless nudge lane; its `sys-` id still
    /// proves daemon provenance (no caller can mint the prefix, and no
    /// caller path reaches this enqueue). The id's kind segment must
    /// be the source — `sys-nudge-…` is a daemon nudge, `sys-wake-…` a
    /// wake; a crossed pair is refused like any other mismatch.
    pub fn enqueue_daemon(
        &self,
        alias: &str,
        body: &str,
        id: &str,
        source: &str,
    ) -> Result<(bool, String)> {
        let daemon_id = format!("{}{}-", crate::proto::DAEMON_MESSAGE_PREFIX, source);
        if !(crate::proto::DAEMON_SOURCES.contains(&source) || source == NUDGE_SOURCE)
            || !id.starts_with(&daemon_id)
        {
            return Err(Error::internal(format!(
                "a daemon message needs a daemon id and source, not {id}/{source}"
            )));
        }
        self.enqueue_daemon_task(alias, body, id, source, None)
    }

    /// `enqueue_daemon` carrying a task tag (CAD-506): a pending
    /// effect's outcome lands as a daemon message on the task's lane —
    /// `task_id` names the task the staged call belonged to, validated
    /// by `enqueue_tx_as` like any other.
    pub fn enqueue_daemon_task(
        &self,
        alias: &str,
        body: &str,
        id: &str,
        source: &str,
        task_id: Option<&str>,
    ) -> Result<(bool, String)> {
        let daemon_id = format!("{}{}-", crate::proto::DAEMON_MESSAGE_PREFIX, source);
        if !(crate::proto::DAEMON_SOURCES.contains(&source) || source == NUDGE_SOURCE)
            || !id.starts_with(&daemon_id)
        {
            return Err(Error::internal(format!(
                "a daemon message needs a daemon id and source, not {id}/{source}"
            )));
        }
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let out = self.enqueue_tx_as(
                &tx,
                alias,
                body,
                None,
                id,
                source,
                task_id,
                None,
                None,
                &Sender::Unattributed,
                Priority::Normal,
                true,
                None,
                None,
                None,
                None,
            )?;
            Ok(out)
        })
    }

    /// The ids a steering send superseded, oldest first.
    fn superseded_by_in(tx: &impl super::StoreConn, id: &str) -> Result<Vec<String>> {
        let ids = tx.query_vec(
            "SELECT id FROM messages WHERE state='cancelled'
             AND json_extract(result,'$.superseded_by')=? ORDER BY seq",
            [id],
            |r| r.get::<_, String>(0),
        )?;
        Ok(ids)
    }

    /// CAD-158 fail-closed gate: `id` may be superseded by a send to
    /// `alias` only while it is `alias`'s own still-`queued` instruction.
    /// The refusal names the id and what it is.
    fn supersedable_in(
        &self,
        tx: &impl super::StoreConn,
        alias: &str,
        id: &str,
    ) -> Result<Message> {
        let refuse = |why: String| {
            Error::rejected(format!(
                "--supersedes refused, nothing changed: message '{id}' {why} — only \
                 {alias}'s own still-queued instructions can be superseded"
            ))
        };
        let Some(message) = self.message_in(tx, id)? else {
            return Err(refuse("is unknown".to_string()));
        };
        if message.alias != alias {
            return Err(refuse(format!(
                "is agent '{}''s (state {})",
                message.alias, message.state
            )));
        }
        if message.state != "queued" {
            return Err(refuse(format!("is {}", message.state)));
        }
        if message.is_routed() {
            return Err(refuse(format!(
                "is a routed {} (state queued), not an instruction",
                message.source
            )));
        }
        if message.source == "job_dispatch" {
            return Err(refuse(format!(
                "is task '{}''s kickoff (state queued) — `cadence task cancel` owns it",
                message.task_id.as_deref().unwrap_or_default()
            )));
        }
        Ok(message)
    }

    /// Cancel one validated `queued` row as superseded by `new_id`. The
    /// state-guarded UPDATE loses cleanly to a concurrent claim, and the
    /// caller's transaction then rolls back the whole send.
    fn supersede_in(
        &self,
        tx: &impl super::StoreConn,
        message: &Message,
        new_id: &str,
        steer: &Steer,
    ) -> Result<()> {
        let reason = format!("superseded by {new_id}");
        let result = json!({
            "status": "cancelled", "via": "supersede",
            "by": steer.by, "by_kind": steer.by_kind,
            "reason": reason, "superseded_by": new_id,
        });
        let n = tx.execute(
            "UPDATE messages SET state='cancelled',result=?,completed=?
             WHERE id=? AND state='queued'",
            params![result.to_string(), now(), message.id],
        )?;
        if n == 0 {
            return Err(Error::rejected(format!(
                "--supersedes refused, nothing changed: message '{}' left queued \
                 state before the supersede committed",
                message.id
            )));
        }
        // Scoped like the row's own `queued` event: a superseded
        // `--task` follow-up leaves its cancellation on the task's
        // (and job's) stream, not only the agent's.
        let job_id: Option<String> = match message.task_id.as_deref() {
            Some(task) => tx
                .query_row("SELECT job_id FROM tasks WHERE id=?", [task], |r| r.get(0))
                .optional()?,
            None => None,
        };
        Self::event_scoped(
            tx,
            &message.alias,
            "cancelled",
            json!({"message": message.id, "by": steer.by, "by_kind": steer.by_kind,
                   "reason": reason, "superseded_by": new_id}),
            job_id.as_deref(),
            message.task_id.as_deref(),
        )?;
        Ok(())
    }

    /// One `superseded` notice per DISTINCT `reply_to` of the rows a
    /// steering send replaced, naming every replaced id and the new one —
    /// and none to the caller itself: a PM superseding its own queued
    /// instructions already knows, and N notices would be N wasted
    /// turns. The deterministic notice id is keyed on the first
    /// replaced row per recipient.
    fn notify_superseded(
        &self,
        tx: &impl super::StoreConn,
        superseded: &[Message],
        new_id: &str,
        steer: &Steer,
    ) -> Result<()> {
        let mut targets: Vec<&str> = Vec::new();
        for m in superseded {
            if let Some(target) = m.reply_to.as_deref() {
                let caller = steer.by_kind == "agent" && target == steer.by;
                if !caller && !targets.contains(&target) {
                    targets.push(target);
                }
            }
        }
        for target in targets {
            let group: Vec<&Message> = superseded
                .iter()
                .filter(|m| m.reply_to.as_deref() == Some(target))
                .collect();
            let result = json!({
                "status": "cancelled", "via": "supersede",
                "by": steer.by, "by_kind": steer.by_kind,
                "reason": format!("superseded by {new_id}"),
                "superseded_by": new_id,
                "superseded": group.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            });
            self.route_notice(tx, group[0], "superseded", &result)?;
        }
        Ok(())
    }

    /// CAD-1015: a native nudge — steering bound to the turn that is
    /// running when the daemon submits it, delivered through the
    /// provider's own exact-turn input (`turn/steer`), never through
    /// the pane. Returns `(duplicate, state, bound_turn)` like
    /// `enqueue`, with the bound turn token the provider call must
    /// name.
    ///
    /// One `BEGIN IMMEDIATE` transaction does all of it:
    ///
    /// - **Validate against the live row.** The agent must be enabled,
    ///   `idle` or `busy` (a live endpoint — `endpoint` non-empty — is
    ///   what `busy` proves and what a native steer needs), and the
    ///   `target` snapshot the caller authorized — generation,
    ///   endpoint, session, thread — must still be exactly this row.
    ///   Any drift (a restart minted a new generation, a re-open moved
    ///   the session) is refused whole before anything is written.
    /// - **Dedupe by the envelope.** `enqueue_tx` compares body,
    ///   source, rank and every stamped field; a retry of the same
    ///   envelope returns `(true, stored_state, stored_turn)` — the
    ///   previously bound turn token, never a re-aim — and a same-id
    ///   different-body send is the usual conflict, never a second
    ///   row.
    /// - **Bind and submit atomically.** While a report-owing turn
    ///   (`running`, non-turnless, `turn_id` set) is in flight the new
    ///   row goes straight `queued -> submitting`, `turn_id` bound and
    ///   `started` stamped, in this same transaction — so no actor
    ///   claim (`take_queued` reads only `queued`) and no stop/restart
    ///   nudge sweep can interpose between the insert and the bind.
    ///   The `native_steer_submitting` event records message, turn and
    ///   caller. With no such turn the row goes terminal `cancelled`
    ///   (`skipped_inactive`, `native_turn_steering`) with a
    ///   `nudge_cancelled` event — steering for a moment that already
    ///   passed is never queued.
    ///
    /// The agent row is never touched here: the nudge owns no turn, so
    /// `agents.state` stays what the turn's own lifecycle left it, and
    /// the kickoff it runs beside is unaffected.
    pub fn begin_native_nudge(
        &self,
        target: &Agent,
        body: &str,
        id: &str,
        sender: &Sender,
        steer: &Steer,
    ) -> Result<(bool, String, Option<String>)> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            // The live row, not the caller's snapshot: every check below is
            // against what is durable now, in the write transaction.
            let current = self.agent_in(&tx, &target.alias)?;
            if !current.enabled {
                return Err(Error::rejected(format!(
                    "native steer refused, nothing changed: agent '{}' is disabled",
                    target.alias
                )));
            }
            if !matches!(current.state.as_str(), "idle" | "busy") {
                return Err(Error::rejected(format!(
                    "native steer refused, nothing changed: agent '{}' is {} — only a live \
                     idle|busy endpoint takes exact-turn input",
                    target.alias, current.state
                )));
            }
            let live_endpoint = current.endpoint.as_deref().filter(|e| !e.is_empty());
            // Stdio-managed sessions have no network endpoint address. Their
            // owned process and native session/thread identify the live runtime.
            let native_process = current.pid.is_some()
                && (current.session_id.as_deref().is_some_and(|s| !s.is_empty())
                    || current.thread_id.as_deref().is_some_and(|s| !s.is_empty()));
            if live_endpoint.is_none() && !native_process {
                return Err(Error::rejected(format!(
                    "native steer refused, nothing changed: agent '{}' has no live endpoint",
                    target.alias
                )));
            }
            // The snapshot the steer was authorized against must still be
            // the row: a re-open or restart that moved generation,
            // endpoint, session or thread makes the old target a different
            // endpoint, and steering it would land nowhere provable.
            let drift = [
                (
                    "generation",
                    target.generation.as_deref(),
                    current.generation.as_deref(),
                ),
                (
                    "endpoint",
                    target.endpoint.as_deref(),
                    current.endpoint.as_deref(),
                ),
                (
                    "session_id",
                    target.session_id.as_deref(),
                    current.session_id.as_deref(),
                ),
                (
                    "thread_id",
                    target.thread_id.as_deref(),
                    current.thread_id.as_deref(),
                ),
            ]
            .iter()
            .filter(|(_, was, now)| was != now)
            .map(|(field, _, _)| *field)
            .collect::<Vec<_>>();
            if !drift.is_empty()
                || target.pid != current.pid
                || target.pid_start != current.pid_start
            {
                return Err(Error::rejected(format!(
                    "native steer refused, nothing changed: agent '{}' no longer matches \
                     the authorized target ({} changed)",
                    target.alias,
                    drift.join(", ")
                )));
            }
            // The turn this nudge would steer: the report-owing row's
            // token, same rule `take_queued`'s hold uses.
            let running_turn: Option<(String, String)> = tx.query_opt(
                &format!(
                    "SELECT id,turn_id FROM messages WHERE alias=? AND state='running'
                         AND source NOT IN {TURNLESS_SOURCES_SQL}
                         AND turn_id IS NOT NULL AND turn_id != '' ORDER BY seq LIMIT 1"
                ),
                [&target.alias],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let (duplicate, _) = self.enqueue_tx(
                &tx,
                &target.alias,
                body,
                None,
                id,
                NUDGE_SOURCE,
                None,
                None,
                None,
                sender,
                steer.priority,
                None,
                None,
                None,
            )?;
            if duplicate {
                // The stored row is authoritative: the retry learns the
                // bound turn and the state it is already in, never a
                // second enqueue. A conflicting envelope already refused
                // above.
                let stored = self
                    .message_in(&tx, id)?
                    .ok_or_else(|| Error::internal("deduped native nudge row vanished"))?;
                return Ok((true, stored.state, stored.turn_id));
            }
            let out = if let Some((target_message, turn)) = running_turn {
                // `submitting` in the same transaction the row was created
                // in: the claim gate and every nudge sweep see only
                // `queued`/`submitting`, so nothing can take or cancel the
                // row between its insert and this bind.
                let n = tx.execute(
                    "UPDATE messages SET state='submitting',turn_id=?,started=?,result=?
                     WHERE id=? AND state='queued'",
                    params![
                        turn,
                        now(),
                        json!({"target_message": target_message, "application": "unconfirmed"})
                            .to_string(),
                        id
                    ],
                )?;
                if n != 1 {
                    return Err(Error::internal(format!(
                        "native nudge {id} left queued state before its turn bind committed"
                    )));
                }
                Self::event(
                    &tx,
                    &target.alias,
                    "native_steer_submitting",
                    json!({"message": id, "turn": turn.clone(), "target_message": target_message,
                           "by": steer.by, "by_kind": steer.by_kind}),
                )?;
                // The returned token is the caller's steer target — the
                // same turn the event payload records.
                (false, "submitting".to_string(), Some(turn))
            } else {
                // No turn to steer: `skipped` is the durable outcome —
                // cancelled, never replayed, with the Codex
                // `skipped_inactive` via and the native-steer delivery
                // label. `nudge_cancelled` is the same event the other
                // nudge exits record.
                let result = json!({
                    "status": "skipped",
                    "via": "skipped_inactive",
                    "delivery": "native_turn_steering",
                    "reason": "no running turn to steer — a nudge is never replayed",
                });
                let n = tx.execute(
                    "UPDATE messages SET state='cancelled',result=?,completed=?
                     WHERE id=? AND state='queued'",
                    params![result.to_string(), now(), id],
                )?;
                if n != 1 {
                    return Err(Error::internal(format!(
                        "native nudge {id} left queued state before its skip committed"
                    )));
                }
                Self::event(
                    &tx,
                    &target.alias,
                    "nudge_cancelled",
                    json!({"message": id, "was": "queued", "state": "cancelled",
                           "reason": "skipped_inactive"}),
                )?;
                (false, "cancelled".to_string(), None)
            };
            Ok(out)
        })
    }

    /// CAD-1015: record what the provider's exact-turn steer answered
    /// for a `submitting` native nudge — the terminal the submit path
    /// left it in. `disposition` is the daemon's allowlisted account of
    /// the adapter's reply:
    ///
    /// - `queued` — the provider accepted the input into the bound
    ///   turn: `completed` with `result.status accepted`, outcome
    ///   `queued`, application `unconfirmed` (a wire ack is not proof
    ///   the model applied it).
    /// - `skipped_inactive` — the turn ended before the steer could
    ///   bind: `cancelled`, status `skipped`, outcome `not_applied`.
    /// - `rejected` — the provider refused the turn precondition:
    ///   `failed`.
    /// - `unknown` — the reply proved nothing either way: `unknown`,
    ///   which for a `nudge` source never fences.
    ///
    /// Only a currently-`submitting` `nudge` row with a non-empty bound
    /// `turn_id` may be written, and the UPDATE is guarded on exactly
    /// that — a stop or restart that already swept the row to its own
    /// terminal keeps that evidence; this call records the disposition
    /// event but overwrites nothing. This is deliberately not
    /// [`Store::finish`]/`finish_in`: the nudge owns no turn, so no
    /// `agents.state` idles, no `reply_to` routes, no task edge runs.
    pub fn finish_native_nudge(
        &self,
        id: &str,
        disposition: &str,
        reason: Option<&str>,
    ) -> Result<()> {
        let (to_state, mut result) = match disposition {
            "queued" => (
                "completed",
                json!({"status": "accepted", "outcome": "queued",
                       "application": "unconfirmed",
                       "via": "native_turn_steering", "reason": reason}),
            ),
            "skipped_inactive" => (
                "cancelled",
                json!({"status": "skipped", "outcome": "not_applied",
                       "via": "native_turn_steering", "reason": reason}),
            ),
            "rejected" => (
                "failed",
                json!({"status": "failed", "outcome": "rejected",
                       "via": "native_turn_steering", "reason": reason}),
            ),
            "unknown" => (
                "unknown",
                json!({"status": "unknown", "outcome": "unknown",
                       "via": "native_turn_steering", "reason": reason}),
            ),
            other => {
                return Err(Error::rejected(format!(
                    "native nudge disposition must be queued|skipped_inactive|rejected|unknown, \
                     not '{other}'"
                )));
            }
        };
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let Some(message) = self.message_in(&tx, id)? else {
                return Err(Error::rejected(format!("No such message '{id}'")));
            };
            if message.source != NUDGE_SOURCE {
                return Err(Error::rejected(format!(
                    "Message '{id}' is source '{}' — only a native nudge takes a \
                     steer disposition",
                    message.source
                )));
            }
            if let Some(target_message) = message
                .result
                .as_ref()
                .and_then(|value| value.get("target_message"))
            {
                result["target_message"] = target_message.clone();
            }
            // The guard that makes this exactly-once and terminal-safe: the
            // row must still be the `submitting` row `begin_native_nudge`
            // wrote, bound to a real turn. `n = 0` means stop/restart or a
            // reconcile already moved it — that terminal evidence stands,
            // and the disposition still lands on the event stream below so
            // the provider's answer is never silently dropped.
            let bound = message
                .turn_id
                .as_deref()
                .filter(|t| !t.is_empty())
                .map(str::to_string);
            let n = match bound.as_deref() {
                Some(turn) => tx.execute(
                    "UPDATE messages SET state=?,result=?,completed=?
                     WHERE id=? AND state='submitting' AND source=? AND turn_id=?",
                    params![to_state, result.to_string(), now(), id, NUDGE_SOURCE, turn],
                )?,
                None => 0,
            };
            Self::event(
                &tx,
                &message.alias,
                "native_steer_disposition",
                json!({"message": id, "disposition": disposition,
                       "reason": reason, "turn": bound,
                       "recorded": n == 1}),
            )?;
            Ok(())
        })
    }

    /// Transactional enqueue — validation, idempotent dedupe, insert,
    /// `queued` event — usable inside a caller's `BEGIN IMMEDIATE`.
    /// Every caller-reachable path (`agent_send`, `thread_send`,
    /// `task_dispatch`, dispatch kickoffs) comes through here, and the
    /// daemon's reserved ids and sources are refused (CAD-445).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn enqueue_tx(
        &self,
        tx: &impl super::StoreConn,
        alias: &str,
        body: &str,
        reply_to: Option<&str>,
        id: &str,
        source: &str,
        task_id: Option<&str>,
        issue: Option<&str>,
        worktree: Option<&str>,
        sender: &Sender,
        priority: Priority,
        refs: Option<&Value>,
        app: Option<&Value>,
        attachments: Option<&Value>,
    ) -> Result<(bool, String)> {
        self.enqueue_tx_with_conversation(
            tx,
            alias,
            body,
            reply_to,
            id,
            source,
            task_id,
            issue,
            worktree,
            sender,
            priority,
            false,
            refs,
            app,
            attachments,
            None,
            None,
            None,
            None,
        )
    }

    /// [`Self::enqueue_tx`] carrying the verified human (CAD-1374).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn enqueue_tx_human(
        &self,
        tx: &impl super::StoreConn,
        alias: &str,
        body: &str,
        reply_to: Option<&str>,
        id: &str,
        source: &str,
        task_id: Option<&str>,
        issue: Option<&str>,
        worktree: Option<&str>,
        sender: &Sender,
        priority: Priority,
        refs: Option<&Value>,
        app: Option<&Value>,
        attachments: Option<&Value>,
        human: Option<&Value>,
    ) -> Result<(bool, String)> {
        self.enqueue_tx_with_conversation(
            tx,
            alias,
            body,
            reply_to,
            id,
            source,
            task_id,
            issue,
            worktree,
            sender,
            priority,
            false,
            refs,
            app,
            attachments,
            human,
            None,
            None,
            None,
        )
    }

    /// Transactional enqueue with optional human attribution and conversation.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn enqueue_tx_with_conversation(
        &self,
        tx: &impl super::StoreConn,
        alias: &str,
        body: &str,
        reply_to: Option<&str>,
        id: &str,
        source: &str,
        task_id: Option<&str>,
        issue: Option<&str>,
        worktree: Option<&str>,
        sender: &Sender,
        priority: Priority,
        daemon: bool,
        refs: Option<&Value>,
        app: Option<&Value>,
        attachments: Option<&Value>,
        human: Option<&Value>,
        conversation: Option<&str>,
        media_refs: Option<&Value>,
        selection: Option<IssueSelection>,
    ) -> Result<(bool, String)> {
        if !daemon {
            crate::proto::caller_message(id, source)?;
        }
        if body.is_empty() || body.len() > 48_000 {
            return Err(Error::rejected("Prompt must contain 1-48000 characters"));
        }
        identifier(id, "Message id")?;
        let target = self.agent_in(tx, alias)?;
        if target.archived_at.is_some() {
            return Err(Error::rejected(format!(
                "Agent '{alias}' is archived — its alias is reserved"
            )));
        }
        let reply_recipient = if let Some(target) = reply_to {
            let recipient = self.agent_in(tx, target)?;
            if target == alias {
                return Err(Error::rejected(
                    "An agent cannot automatically reply to itself",
                ));
            }
            Some(recipient)
        } else {
            None
        };
        if let Some(task) = task_id {
            if source != "app_run_dispatch" {
                Self::refuse_app_task(tx, task)?;
            }
            self.task_in(tx, task)?;
        }
        // CAD-1568: a CLI retry must retain its canonical principal, not
        // merely its lossy display source. Keep the existing local retry
        // contract: starting a chat may add operator attribution later.
        if let Some(old) = self.message_in(tx, id)? {
            let same_sender = match sender {
                Sender::Cli(authority) => {
                    old.sender.as_deref() == Some(authority.sender_label().as_str())
                }
                _ => !old
                    .sender
                    .as_deref()
                    .is_some_and(|label| label.starts_with("cli:")),
            };
            // `refs` lives on the enqueue's thread-entry payload, not
            // the message row — the stored side is read back so a
            // retry naming different refs conflicts like any field.
            let same = old.alias == alias
                && old.body == body
                && old.reply_to.as_deref() == reply_to
                && old.source == source
                && old.task_id.as_deref() == task_id
                && old.issue.as_deref() == issue
                && old.worktree.as_deref() == worktree
                && old.priority == priority
                && same_sender
                && Self::entry_refs_in(tx, id)? == refs.cloned()
                && Self::entry_app_in(tx, id)? == app.cloned()
                && Self::entry_attachments_in(tx, id)? == attachments.cloned()
                && Self::entry_human_in(tx, id)? == human.cloned()
                && Self::entry_media_refs_in(tx, id)? == media_refs.cloned()
                && Self::entry_conversation_in(tx, id)?.as_deref() == conversation;
            if !same {
                return Err(Error::rejected(
                    "Message id was already used with different content",
                ));
            }
            return Ok((true, old.state));
        }
        let (sender_uid, sender_label) = self.sender_identity_in(tx, sender)?;
        let recipient = self.agent_in(tx, alias)?;
        let recipient_uid = recipient.uid.clone();
        let recipient_label = recipient.alias.clone();
        tx.execute(
            "INSERT INTO messages(id,alias,body,reply_to,source,task_id,issue,worktree,priority,created,
                                  sender_uid,recipient_uid,sender,recipient)
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                id,
                alias,
                body,
                reply_to,
                source,
                task_id,
                issue,
                worktree,
                priority.rank(),
                now(),
                sender_uid,
                recipient_uid,
                sender_label,
                recipient_label
            ],
        )?;
        let mut queued = json!({
            "message": id,
            "source": source,
            "reply_to": reply_to,
            "sender": sender.label(),
            "recipient_identity": reply_recipient
                .as_ref()
                .map(Self::agent_identity)
                // A topic notice binds its own recipient's identity: the
                // claim's alias-reuse fence compares it with the live row.
                .or_else(|| {
                    (source == crate::proto::TOPIC_NOTICE_SOURCE)
                        .then(|| Self::agent_identity(&recipient))
                }),
        });
        // CAD-1374: the verified human is recorded only when present, so a
        // legacy enqueue keeps the exact event shape it always had.
        if let Some(human) = human {
            queued["human"] = human.clone();
        }
        Self::event_scoped(tx, alias, "queued", queued, None, task_id)?;
        if source != "app_run_dispatch" {
            Self::thread_note_enqueued(
                tx,
                alias,
                sender,
                source,
                body,
                id,
                refs,
                app,
                attachments,
                human,
                conversation,
                media_refs,
            )?;
        }
        // CAD-1822: the context selection commits with the accepted NEW row.
        // The existing-id comparison above returns before this point, so a
        // duplicate or rejected envelope can never write or retarget
        // selection; a failure here rolls the message row back too.
        if let Some(selection) = selection {
            match selection {
                IssueSelection::Pin(payload) => {
                    Self::event(tx, alias, crate::context_pack::PIN_EVENT, payload)?;
                }
                IssueSelection::Disabled { task, reason } => {
                    if Self::has_context_state_in(tx, alias)? {
                        Self::event(
                            tx,
                            alias,
                            crate::context_pack::DISABLED_EVENT,
                            json!({"message": id, "task": task, "reason": reason}),
                        )?;
                    }
                }
            }
        }
        Ok((false, "queued".to_string()))
    }
    /// CAD-819: the uid of a live agent row, `None` when the alias is gone
    /// (a routed copy from a removed agent keeps its origin, not a uid).
    pub(super) fn agent_uid_of_in(
        &self,
        tx: &impl super::StoreConn,
        alias: &str,
    ) -> Result<Option<String>> {
        Ok(self.agent_opt_in(tx, alias)?.map(|a| a.uid))
    }

    /// CAD-819: the stored participant identity for a daemon-derived
    /// sender: `(uid, label)`. An agent sender carries its uid and alias;
    /// the operator stores the fixed `operator` label with no uid; an
    /// unattributed caller stores neither (an unknown sender).
    fn sender_identity_in(
        &self,
        tx: &impl super::StoreConn,
        sender: &Sender,
    ) -> Result<(Option<String>, Option<String>)> {
        Ok(match sender {
            Sender::Agent(alias) => (Some(self.agent_in(tx, alias)?.uid), Some(alias.clone())),
            Sender::Operator | Sender::OperatorChat => (None, Some("operator".into())),
            // CAD-1380: a member is attributed by label only, never as the
            // operator and never with an agent uid.
            Sender::Member(_) | Sender::VerifiedMember(_) => (None, Some("member".into())),
            // CAD-1568: a remote CLI principal is attributed by its
            // verified canonical `(org, sub)` key, never as the operator
            // and never with an agent uid — `cli:<canonical>` is the
            // durable per-principal identity the enqueue dedupe and a
            // cli-scoped readback own.
            Sender::Cli(auth) => (None, Some(auth.sender_label())),
            Sender::Unattributed => (None, None),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn enqueue_tx_as(
        &self,
        tx: &impl super::StoreConn,
        alias: &str,
        body: &str,
        reply_to: Option<&str>,
        id: &str,
        source: &str,
        task_id: Option<&str>,
        issue: Option<&str>,
        worktree: Option<&str>,
        sender: &Sender,
        priority: Priority,
        daemon: bool,
        refs: Option<&Value>,
        app: Option<&Value>,
        attachments: Option<&Value>,
        human: Option<&Value>,
    ) -> Result<(bool, String)> {
        self.enqueue_tx_with_conversation(
            tx,
            alias,
            body,
            reply_to,
            id,
            source,
            task_id,
            issue,
            worktree,
            sender,
            priority,
            daemon,
            refs,
            app,
            attachments,
            human,
            None,
            None,
            None,
        )
    }

    pub(super) fn message_in(
        &self,
        conn: &impl super::StoreConn,
        id: &str,
    ) -> Result<Option<Message>> {
        match conn.query_row("SELECT * FROM messages WHERE id=?", [id], row_message) {
            Ok(m) => Ok(Some(m)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Read the recipient binding from the source message's durable queued
    /// event. The event log is the existing cursor/idempotency primitive, so
    /// no schema column or migration is needed for this handoff proof.
    fn queued_recipient_identity(
        &self,
        tx: &impl super::StoreConn,
        alias: &str,
        message_id: &str,
        source: &str,
    ) -> Result<Option<Value>> {
        let raws = tx.query_vec(
            "SELECT payload FROM events
             WHERE alias=? AND kind='queued' ORDER BY seq",
            [alias],
            |row| row.get::<_, String>(0),
        )?;
        for payload in raws {
            let Ok(payload) = serde_json::from_str::<Value>(&payload) else {
                continue;
            };
            if payload.get("message").and_then(Value::as_str) == Some(message_id)
                && payload.get("source").and_then(Value::as_str) == Some(source)
            {
                return Ok(payload.get("recipient_identity").cloned());
            }
        }
        Ok(None)
    }

    /// CAD-565: who the enqueue attributed the message to — the durable
    /// `queued` event's `sender` label, or its `source` for rows queued
    /// before the field existed (and for daemon-routed rows, which name
    /// their origin in `source`). `None` only when the queue event is
    /// missing entirely.
    pub fn queued_sender(&self, alias: &str, message_id: &str) -> Result<Option<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT payload FROM events
             WHERE alias=? AND kind='queued' ORDER BY seq",
        )?;
        let mut rows = stmt.query([alias])?;
        while let Some(row) = rows.next()? {
            let payload: String = row.get(0)?;
            let Ok(payload) = serde_json::from_str::<Value>(&payload) else {
                continue;
            };
            if payload.get("message").and_then(Value::as_str) == Some(message_id) {
                return Ok(payload
                    .get("sender")
                    .or_else(|| payload.get("source"))
                    .and_then(Value::as_str)
                    .map(str::to_string));
            }
        }
        Ok(None)
    }

    pub(super) fn recipient_binding(
        &self,
        tx: &impl super::StoreConn,
        source_alias: &str,
        source_message: &str,
        source: &str,
        recipient: &str,
    ) -> Result<(Option<Value>, Option<Agent>, Option<&'static str>)> {
        let expected = self.queued_recipient_identity(tx, source_alias, source_message, source)?;
        let current = self.agent_opt_in(tx, recipient)?;
        let reason = match current.as_ref() {
            None => Some("recipient_missing"),
            Some(agent) if agent.archived_at.is_some() => Some("recipient_archived"),
            Some(_) if expected.is_none() => Some("recipient_identity_unavailable"),
            Some(agent) if !Self::identity_matches(expected.as_ref().unwrap(), agent) => {
                Some("recipient_identity_changed")
            }
            Some(_) => None,
        };
        Ok((expected, current, reason))
    }

    pub(super) fn handoff_unresolved_exists(
        &self,
        tx: &impl super::StoreConn,
        delivery: &str,
    ) -> Result<bool> {
        let raws = tx.query_vec(
            "SELECT payload FROM events
             WHERE alias=? AND kind='handoff_unresolved' ORDER BY seq",
            [Self::DAEMON_STREAM],
            |row| row.get::<_, String>(0),
        )?;
        for payload in raws {
            let Ok(payload) = serde_json::from_str::<Value>(&payload) else {
                continue;
            };
            if payload.get("delivery").and_then(Value::as_str) == Some(delivery) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Record one durable route failure. Missing/replaced recipients are
    /// intentionally not retried into a later alias registration: the
    /// unresolved event keeps the result visible to the monitor/operator.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn handoff_unresolved(
        &self,
        tx: &impl super::StoreConn,
        task_id: Option<&str>,
        recipient: &str,
        delivery: &str,
        source: &str,
        source_message: &str,
        reason: &str,
        expected: Option<&Value>,
        current: Option<&Agent>,
    ) -> Result<()> {
        if self.handoff_unresolved_exists(tx, delivery)? {
            return Ok(());
        }

        let job_id = if let Some(task_id) = task_id {
            match tx.query_row("SELECT job_id FROM tasks WHERE id=?", [task_id], |row| {
                row.get::<_, String>(0)
            }) {
                Ok(job_id) => Some(job_id),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(error) => return Err(error.into()),
            }
        } else {
            None
        };
        Self::event_scoped(
            tx,
            Self::DAEMON_STREAM,
            "handoff_unresolved",
            json!({
                "recipient": recipient,
                "delivery": delivery,
                "source": source,
                "source_message": source_message,
                "task": task_id,
                "reason": reason,
                "expected_identity": expected.cloned().unwrap_or(Value::Null),
                "current_identity": current.map(Self::agent_identity).unwrap_or(Value::Null),
            }),
            job_id.as_deref(),
            task_id,
        )
    }

    fn fail_unresolved_routed(
        &self,
        tx: &impl super::StoreConn,
        message: &Message,
        recipient: &Agent,
        expected: Option<&Value>,
        reason: &str,
    ) -> Result<()> {
        let result = json!({
            "status": "failed",
            "via": "handoff_unresolved",
            "message": message.id,
            "source": message.source,
            "reason": reason,
        });
        tx.execute(
            "UPDATE messages SET state='failed',result=?,error=?,completed=?
             WHERE id=? AND state='queued'",
            params![
                result.to_string(),
                format!("routed delivery withheld: {reason}"),
                now(),
                message.id
            ],
        )?;
        self.handoff_unresolved(
            tx,
            message.task_id.as_deref(),
            &message.alias,
            &message.id,
            &message.source,
            &message.id,
            reason,
            expected,
            Some(recipient),
        )
    }

    /// Atomically take the next queued message for `alias` — the oldest
    /// of the highest rank ([`QUEUE_ORDER_SQL`]) — and mark it
    /// `submitting`. The actor is the only caller; one actor per alias
    /// keeps turns serialized.
    pub fn take_queued(&self, alias: &str) -> Result<Take> {
        self.take_queued_app_proven(alias, None)
    }
    pub(crate) fn take_queued_app_proven(
        &self,
        alias: &str,
        proof: Option<(&str, &str)>,
    ) -> Result<Take> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let agent = self.agent_in(&tx, alias)?;
            if !agent.enabled {
                return Ok(Take::Stop);
            }
            // CAD-250: the actor serializes report-owing turns. While one is
            // `running` (delivered, its report still owed), only routed
            // notifications and nudges — fire-and-forget, complete at paste —
            // may be claimed; every other delivery stays `queued`, never refused,
            // until that turn is reported, reconciled or bounded to
            // `unknown`.
            // CAD-565: the running turn's token rides along — a nudge binds
            // to it at claim (below) and is rechecked against it, in this
            // same transaction, on every later claim.
            let running_turn: Option<String> = tx.query_opt(
                &format!(
                    "SELECT turn_id FROM messages WHERE alias=? AND state='running'
                                 AND source NOT IN {TURNLESS_SOURCES_SQL} ORDER BY seq LIMIT 1"
                ),
                [alias],
                |r| r.get(0),
            )?;
            let holding = running_turn.is_some();
            let next_sql = if holding {
                format!(
                    "SELECT * FROM messages WHERE alias=? AND state='queued'
                             AND source IN {TURNLESS_SOURCES_SQL} {QUEUE_ORDER_SQL} LIMIT 1"
                )
            } else {
                format!(
                    "SELECT * FROM messages WHERE alias=? AND state='queued'
                             {QUEUE_ORDER_SQL} LIMIT 1"
                )
            };
            loop {
                let next = tx.query_row(&next_sql, [alias], row_message).ok();
                // CAD-810: a topic notice never reaches the actor. It is made
                // available here, in this transaction, after its provenance and
                // the recipient's identity are proven; the adapter, the turn
                // slot, `busy` and `submitting` are never touched.
                if let Some(notice) = next.as_ref().filter(|m| m.is_topic_notice()) {
                    self.consume_topic_notice_in(&tx, &agent, notice)?;
                    continue;
                }
                let Some(message) = next else {
                    // A prior routed row in this same transaction may have
                    // been failed as unresolved before the queue became empty.
                    // Commit that durable evidence even though no message is
                    // returned to the actor.
                    return Ok(Take::Empty);
                };
                if message.source == "app_run_dispatch" {
                    let (expected, bundle) = proof.ok_or_else(|| {
                        Error::rejected("app claim requires fresh filesystem installation proof")
                    })?;
                    if expected != message.id {
                        return Err(Error::rejected("app claim head changed"));
                    }
                    self.app_message_admit_in(&tx, &message, bundle)?;
                }
                if message.is_routed() {
                    let expected =
                        self.queued_recipient_identity(&tx, alias, &message.id, &message.source)?;
                    let reason = match expected.as_ref() {
                        None => Some("recipient_identity_unavailable"),
                        Some(expected) if !Self::identity_matches(expected, &agent) => {
                            Some("recipient_identity_changed")
                        }
                        Some(_) => None,
                    };
                    if let Some(reason) = reason {
                        self.fail_unresolved_routed(
                            &tx,
                            &message,
                            &agent,
                            expected.as_ref(),
                            reason,
                        )?;
                        continue;
                    }
                }
                if message.is_nudge() {
                    // CAD-565: a nudge enters the turn that was running when
                    // it was claimed — bound to it here, atomically, and the
                    // binding is rechecked on every claim. Only while *that*
                    // turn still runs does the nudge pass; an idle agent has
                    // nothing to steer and a nudge bound to an ended turn
                    // must never land in a later one. Skipped means
                    // `cancelled` with a persisted `skipped_inactive`
                    // result — Codex's `SkippedInactive` shape — so the
                    // delivery outcome is durable and readable.
                    let live = running_turn.as_deref();
                    let bound = message.turn_id.as_deref();
                    if bound.map_or(live.is_none(), |t| Some(t) != live) {
                        let result = json!({
                            "status": "skipped",
                            "via": "skipped_inactive",
                            "reason": "no running turn to steer — a nudge is never replayed",
                        });
                        let n = tx.execute(
                            "UPDATE messages SET state='cancelled',result=?,completed=?
                                     WHERE id=? AND state='queued'",
                            params![result.to_string(), now(), message.id],
                        )?;
                        if n == 1 {
                            Self::event(
                                &tx,
                                alias,
                                "nudge_cancelled",
                                json!({"message": message.id, "was": "queued",
                                               "state": "cancelled",
                                               "reason": "skipped_inactive"}),
                            )?;
                        }
                        continue;
                    }
                    if bound.is_none() {
                        tx.execute(
                            "UPDATE messages SET turn_id=? WHERE id=? AND state='queued'",
                            params![live.unwrap_or_default(), message.id],
                        )?;
                    }
                }
                tx.execute(
                    "UPDATE messages SET state='submitting',started=? WHERE id=?",
                    params![now(), message.id],
                )?;
                tx.execute(
                    "UPDATE agents SET state='busy',updated=? WHERE alias=?",
                    params![now(), alias],
                )?;
                Self::event(&tx, alias, "submitting", json!({"message": message.id}))?;
                return Ok(Take::Message(Box::new(message)));
            }
        })
    }

    /// CAD-810: make one daemon topic notice available to its recipient,
    /// in the claim's transaction. It is never a turn: no adapter call, no
    /// session switch, no turn slot or token, no `busy`/`submitting`.
    ///
    /// Provenance is required, not a source string: the message must have a
    /// `topic_deliveries` row whose `uid` is the recipient's CURRENT uid and
    /// whose `message_id` is this message. The enqueue-time identity fence
    /// (the same binding routed rows use) must also still match, so a reused
    /// alias never receives a notice queued for its predecessor. A failed
    /// proof fails the notice closed and no turn starts.
    ///
    /// The notice ends `available`: it has been made readable, which is not
    /// the same as seen. A later explicit read exposes it; nothing here
    /// marks it completed or delivered to a turn.
    fn consume_topic_notice_in(
        &self,
        tx: &impl super::StoreConn,
        agent: &Agent,
        notice: &Message,
    ) -> Result<()> {
        let delivery_uid: Option<String> = tx.query_opt(
            "SELECT uid FROM topic_deliveries WHERE message_id=?",
            [notice.id.as_str()],
            |r| r.get(0),
        )?;
        let proven = delivery_uid.as_deref() == Some(agent.uid.as_str());
        let fenced =
            match self.queued_recipient_identity(tx, &agent.alias, &notice.id, &notice.source)? {
                Some(expected) => Self::identity_matches(&expected, agent),
                None => false,
            };
        let state = if proven && fenced {
            "available"
        } else {
            "refused"
        };
        let reason = if !proven {
            "delivery_provenance_mismatch"
        } else if !fenced {
            "recipient_identity_changed"
        } else {
            "ok"
        };
        tx.execute(
            "UPDATE messages SET state=?, result=?, completed=? WHERE id=? AND state='queued'",
            params![
                state,
                json!({"status": state, "via": "topic_notice", "reason": reason}).to_string(),
                now(),
                notice.id
            ],
        )?;
        Self::event(
            tx,
            &agent.alias,
            "topic_notice_available",
            json!({"message": notice.id, "state": state, "reason": reason}),
        )?;
        Ok(())
    }

    /// Record that the provider acknowledged a turn start.
    pub fn mark_running(&self, message_id: &str, turn_id: &str) -> Result<()> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            tx.execute(
                "UPDATE messages SET state='running',turn_id=? WHERE id=?",
                params![turn_id, message_id],
            )?;
            let alias: String =
                tx.query_row("SELECT alias FROM messages WHERE id=?", [message_id], |r| {
                    r.get(0)
                })?;
            Self::event(
                &tx,
                &alias,
                "turn_started",
                json!({"message": message_id, "turn_id": turn_id}),
            )?;
            // Task edge: a task-attached kickoff observed running moves the
            // task dispatched → running (guarded — cancelled/advanced tasks
            // are untouched).
            self.task_on_running(&tx, message_id, &alias)?;
            Ok(())
        })
    }

    /// Claim an `unknown` cloud kickoff for restart/reconcile. The guarded
    /// transition ensures one recovery consumer; the create replay and
    /// stop use the original kickoff identity.
    pub fn claim_cloud_recovery(&self, message_id: &str, turn_id: &str) -> Result<bool> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let changed = tx.execute(
                "UPDATE messages SET state='running',turn_id=? \
                 WHERE id=? AND state='unknown'",
                params![turn_id, message_id],
            )?;
            if changed == 0 {
                return Ok(false);
            }
            let alias: String =
                tx.query_row("SELECT alias FROM messages WHERE id=?", [message_id], |r| {
                    r.get(0)
                })?;
            Self::event(
                &tx,
                &alias,
                "cloud_recovery_claimed",
                json!({"message": message_id, "turn_id": turn_id}),
            )?;
            self.task_on_running(&tx, message_id, &alias)?;
            Ok(true)
        })
    }

    /// Claim a queued kickoff for a non-actor backend. Returns `false` when
    /// the message was cancelled or already claimed, so the caller must not
    /// start an external job or resurrect a terminal delivery.
    pub fn mark_running_queued(&self, message_id: &str, turn_id: &str) -> Result<bool> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let changed = tx.execute(
                "UPDATE messages SET state='running',turn_id=? \
                 WHERE id=? AND state='queued'",
                params![turn_id, message_id],
            )?;
            if changed == 0 {
                return Ok(false);
            }
            let alias: String =
                tx.query_row("SELECT alias FROM messages WHERE id=?", [message_id], |r| {
                    r.get(0)
                })?;
            Self::event(
                &tx,
                &alias,
                "turn_started",
                json!({"message": message_id, "turn_id": turn_id}),
            )?;
            self.task_on_running(&tx, message_id, &alias)?;
            Ok(true)
        })
    }

    /// Record an operator stop request for a running cloud kickoff. The
    /// poller observes it and calls the AOS stop route; the event is the
    /// durable handoff because the runner is not an AgentCtl.
    pub fn request_cloud_stop(&self, message_id: &str) -> Result<()> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let alias: String =
                tx.query_row("SELECT alias FROM messages WHERE id=?", [message_id], |r| {
                    r.get(0)
                })?;
            let publishing: i64 = tx.query_row(
                "SELECT COUNT(*) FROM events WHERE kind='cloud_publication_started' \
                 AND json_extract(payload,'$.message')=? AND alias=?",
                params![message_id, alias],
                |r| r.get(0),
            )?;
            if publishing > 0 {
                return Err(Error::rejected(
                    "cloud dispatch is already completing; the stop was refused",
                ));
            }
            Self::event(
                &tx,
                &alias,
                "cloud_stop_requested",
                json!({"message": message_id}),
            )?;
            Ok(())
        })
    }

    /// Serialize the first publication write with a concurrent operator
    /// stop. A stop that commits first makes this return `false`; once this
    /// commits, `request_cloud_stop` refuses to cancel the completion.
    pub fn begin_cloud_publication(&self, message_id: &str) -> Result<bool> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let alias: String =
                tx.query_row("SELECT alias FROM messages WHERE id=?", [message_id], |r| {
                    r.get(0)
                })?;
            let stopped: i64 = tx.query_row(
                "SELECT COUNT(*) FROM events WHERE kind='cloud_stop_requested' \
                 AND json_extract(payload,'$.message')=? AND alias=?",
                params![message_id, alias],
                |r| r.get(0),
            )?;
            if stopped > 0 {
                return Ok(false);
            }
            Self::event(
                &tx,
                &alias,
                "cloud_publication_started",
                json!({"message": message_id}),
            )?;
            Ok(true)
        })
    }

    /// Mark that a create request is about to be sent. A later definitive
    /// refusal cannot be assumed to mean no remote exists: an earlier
    /// attempt may have created it before its response was lost.
    pub fn note_cloud_create_attempt(&self, message_id: &str) -> Result<()> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let alias: String =
                tx.query_row("SELECT alias FROM messages WHERE id=?", [message_id], |r| {
                    r.get(0)
                })?;
            Self::event(
                &tx,
                &alias,
                "cloud_create_attempted",
                json!({"message": message_id}),
            )?;
            Ok(())
        })
    }

    /// Whether any create request was already attempted for this kickoff.
    pub fn cloud_create_attempted(&self, message_id: &str) -> Result<bool> {
        self.read_tx(|conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM events WHERE kind='cloud_create_attempted' \
                 AND json_extract(payload,'$.message')=? \
                 AND alias=(SELECT alias FROM messages WHERE id=?)",
                params![message_id, message_id],
                |r| r.get(0),
            )?;
            Ok(count > 0)
        })
    }

    /// Authorize one create attempt and record it in the same transaction
    /// as the stop check. A stop that wins before the first attempt means
    /// no remote may be created. Once an attempt is recorded, a later
    /// definitive refusal is not proof that no remote exists.
    /// Returns `(authorized, had_prior_attempt)`.
    pub fn authorize_cloud_create(&self, message_id: &str) -> Result<(bool, bool)> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let alias: String =
                tx.query_row("SELECT alias FROM messages WHERE id=?", [message_id], |r| {
                    r.get(0)
                })?;
            let stopped: i64 = tx.query_row(
                "SELECT COUNT(*) FROM events WHERE kind='cloud_stop_requested' \
                 AND json_extract(payload,'$.message')=? AND alias=?",
                params![message_id, alias],
                |r| r.get(0),
            )?;
            let attempted: i64 = tx.query_row(
                "SELECT COUNT(*) FROM events WHERE kind='cloud_create_attempted' \
                 AND json_extract(payload,'$.message')=? AND alias=?",
                params![message_id, alias],
                |r| r.get(0),
            )?;
            if stopped > 0 && attempted == 0 {
                return Ok((false, false));
            }
            Self::event(
                &tx,
                &alias,
                "cloud_create_attempted",
                json!({"message": message_id}),
            )?;
            Ok((true, attempted > 0))
        })
    }

    /// Whether an operator stop was requested for this kickoff.
    pub fn cloud_stop_requested(&self, message_id: &str) -> Result<bool> {
        self.read_tx(|conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM events WHERE kind='cloud_stop_requested' \
                 AND json_extract(payload,'$.message')=? \
                 AND alias=(SELECT alias FROM messages WHERE id=?)",
                params![message_id, message_id],
                |r| r.get(0),
            )?;
            Ok(count > 0)
        })
    }

    /// Return a cloud kickoff to `queued` when its remote job's state could
    /// not be proven and the stop was not confirmed. The guarded claim
    /// token means only the consumer that claimed this delivery can release
    /// it; a retry then re-enters the same task/revision remote job.
    pub fn requeue_cloud_claim(&self, message_id: &str, turn_id: &str) -> Result<bool> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let changed = tx.execute(
                "UPDATE messages SET state='queued',turn_id=NULL \
                 WHERE id=? AND state='running' AND turn_id=?",
                params![message_id, turn_id],
            )?;
            if changed == 0 {
                return Ok(false);
            }
            let alias: String =
                tx.query_row("SELECT alias FROM messages WHERE id=?", [message_id], |r| {
                    r.get(0)
                })?;
            Self::event(
                &tx,
                &alias,
                "turn_requeued",
                json!({"message": message_id, "turn_id": turn_id,
                       "reason": "cloud sandbox job state unproven"}),
            )?;
            Ok(true)
        })
    }

    /// Return a `submitting` message to `queued` — the submission gate
    /// refused before any paste, so retry is safe.
    pub fn requeue(&self, message_id: &str) -> Result<()> {
        self.write_tx(|conn| {
            conn.execute(
                "UPDATE messages SET state='queued',started=NULL
                         WHERE id=? AND state='submitting'",
                [message_id],
            )?;
            Ok(())
        })
    }

    /// PTY submission was accepted by the terminal: the message stays
    /// `running` (turn_id already recorded) with a durable `submitted`
    /// marker until an explicit ack/result report lands.
    pub fn mark_submitted(&self, message: &Message) -> Result<()> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            tx.execute(
                "UPDATE messages SET result=? WHERE id=? AND state='running'",
                params![
                    json!({"status": "submitted", "ack": Value::Null}).to_string(),
                    message.id
                ],
            )?;
            Self::event(
                &tx,
                &message.alias,
                "submitted",
                json!({"message": message.id, "turn_id": message.turn_id}),
            )?;
            Ok(())
        })
    }

    /// Explicit acknowledgement for a running message; it stays
    /// `running` until a result completes it — a pty result report, or
    /// (CAD-162) the adapter's turn result on a managed endpoint. Only a
    /// pty-style (explicitly reported) turn carries the `submitted`
    /// marker, so a managed ack never makes its turn `awaiting_report`.
    pub fn mark_ack(&self, message: &Message, text: Option<&str>) -> Result<()> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let turn_result = tx
                .query_opt(
                    "SELECT provider, endpoint_kind FROM agents WHERE alias=?",
                    [&message.alias],
                    |r| {
                        Ok(registry::reports_turn_result(
                            &r.get::<_, String>(0)?,
                            &r.get::<_, String>(1)?,
                        ))
                    },
                )?
                .unwrap_or(false);
            let status = if turn_result {
                "acknowledged"
            } else {
                "submitted"
            };
            let n = tx.execute(
                "UPDATE messages SET result=? WHERE id=? AND state='running'",
                params![
                    json!({"status": status,
                                   "ack": {"text": text, "at": now()}})
                    .to_string(),
                    message.id
                ],
            )?;
            if n == 0 {
                return Err(Error::rejected(format!(
                    "Message {} is not awaiting a report (state {})",
                    message.id, message.state
                )));
            }
            Self::event(
                &tx,
                &message.alias,
                "acknowledged",
                json!({"message": message.id, "turn_id": message.turn_id}),
            )?;
            Ok(())
        })
    }

    /// CAD-1752: the agent page's figures, one bounded query each.
    /// `tasks_done` counts its verified/done tasks, `reports` the
    /// `task_reported` events on its stream, and `avg_turn_secs` the mean
    /// of its last 100 completed turns (nudges excluded). `hired_at` is the
    /// registration time. Figures the store cannot attribute to an agent
    /// (merged PRs, drafts sent) stay null rather than being invented.
    pub fn agent_stats(&self, alias: &str) -> Result<Value> {
        let conn = self.conn();
        let hired_at: Option<f64> = conn
            .query_row("SELECT created FROM agents WHERE alias=?", [alias], |r| {
                r.get(0)
            })
            .ok();
        let tasks_done: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE assignee=? AND state IN ('verified','done')",
            [alias],
            |r| r.get(0),
        )?;
        let reports: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE alias=? AND kind='task_reported'",
            [alias],
            |r| r.get(0),
        )?;
        let avg_turn_secs: Option<f64> = conn.query_row(
            "SELECT AVG(completed-started) FROM (
                 SELECT started, completed FROM messages
                 WHERE alias=? AND state='completed' AND source != 'nudge'
                   AND started IS NOT NULL AND completed IS NOT NULL
                 ORDER BY seq DESC LIMIT 100)",
            [alias],
            |r| r.get(0),
        )?;
        Ok(json!({
            "hired_at": hired_at, "tasks_done": tasks_done, "reports": reports,
            "avg_turn_secs": avg_turn_secs,
            "prs_merged": Value::Null, "drafts_sent": Value::Null,
        }))
    }

    /// Count an agent's queued inbound messages (`cadence self` for an
    /// inbox reports this instead of a running turn).
    pub fn queued_count(&self, alias: &str) -> Result<i64> {
        let conn = self.conn();
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE alias=? AND state='queued'",
            [alias],
            |r| r.get(0),
        )?)
    }

    /// The alias's messages as `agent show` lists them — this
    /// registration's only (CAD-304 S4). `agent remove` keeps the rows
    /// job history still resolves under the old alias (CAD-284); a later
    /// agent registered under the same alias must not list them as its
    /// own. Kept rows are always terminal (removal refuses `unknown` and
    /// finishes everything else), so a terminal row older than the
    /// current registration is a previous agent's and is left out;
    /// every non-terminal row is listed whatever its timestamp, so a
    /// clock step can never hide live work. `job show`/`task show` read
    /// the kept rows by id, unchanged.
    pub fn messages(&self, alias: &str) -> Result<Vec<Message>> {
        let conn = self.conn();
        let agent = self.agent_in(&conn, alias)?;
        let mut stmt = conn.prepare(&format!(
            "SELECT * FROM messages WHERE alias=?1
             AND (created >= ?2 OR (state NOT IN
                  ('completed','failed','interrupted','cancelled') AND NOT {NOTICE_ENDED_SQL}))
             ORDER BY seq"
        ))?;
        let rows = stmt.query_map(params![alias, agent.created], row_message)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// A bounded view of [`Self::messages`] (CAD-879): every non-terminal
    /// row — live work is never hidden — plus the newest `limit` terminal
    /// rows at or after `since`. `since` is a message id (rows after it)
    /// or a unix timestamp (rows created at or after it); an id wins when
    /// a message of that alias carries it. Returns the rows in `seq`
    /// order and how many of this registration's rows were left out.
    pub fn messages_window(
        &self,
        alias: &str,
        limit: Option<usize>,
        since: Option<&str>,
    ) -> Result<(Vec<Message>, usize)> {
        let conn = self.conn();
        let agent = self.agent_in(&conn, alias)?;
        let (mut after_seq, mut after_ts) = (0i64, f64::MIN);
        if let Some(since) = since {
            let by_id = conn
                .query_row(
                    "SELECT seq FROM messages WHERE alias=?1 AND id=?2",
                    params![alias, since],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?;
            match (by_id, since.parse::<f64>()) {
                (Some(seq), _) => after_seq = seq,
                (None, Ok(ts)) if ts.is_finite() => after_ts = ts,
                _ => {
                    return Err(Error::rejected(format!(
                        "since '{since}' is neither a message id of '{alias}' nor a timestamp"
                    )));
                }
            }
        }
        let live = format!(
            "state NOT IN ('completed','failed','interrupted','cancelled') AND NOT {NOTICE_ENDED_SQL}"
        );
        let mut rows = {
            let mut stmt =
                conn.prepare(&format!("SELECT * FROM messages WHERE alias=?1 AND {live}"))?;
            let rows = stmt.query_map([alias], row_message)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT * FROM messages WHERE alias=?1 AND NOT ({live})
             AND created >= ?2 AND created >= ?3 AND seq > ?4
             ORDER BY seq DESC LIMIT ?5"
        ))?;
        let cap = limit.map_or(-1, |n| n.min(i64::MAX as usize) as i64);
        let recent = stmt.query_map(
            params![alias, agent.created, after_ts, after_seq, cap],
            row_message,
        )?;
        rows.extend(recent.collect::<rusqlite::Result<Vec<_>>>()?);
        rows.sort_by_key(|m| m.seq);
        let total: i64 = conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM messages WHERE alias=?1 AND (created >= ?2 OR ({live}))"
            ),
            params![alias, agent.created],
            |r| r.get(0),
        )?;
        let omitted = (total as usize).saturating_sub(rows.len());
        Ok((rows, omitted))
    }

    /// Bounded index of one alias's messages. Rows carry no body, result,
    /// error, or turn token — those stay on `message read` and the
    /// owner's own report path. Same registration window as
    /// [`Self::messages_window`]: every matching non-terminal row, plus
    /// the newest `limit` matching terminal rows. `since` is a message
    /// id of this alias (rows after it) or a unix timestamp.
    pub fn message_index(
        &self,
        alias: &str,
        query: &MessageIndexQuery<'_>,
    ) -> Result<MessageIndexPage> {
        let MessageIndexQuery {
            states,
            from,
            source,
            since,
            limit,
            include_app,
        } = *query;
        let conn = self.conn();
        let agent = self.agent_in(&conn, alias)?;
        let (mut after_seq, mut after_ts) = (0i64, f64::MIN);
        if let Some(since) = since {
            let by_id = conn
                .query_row(
                    "SELECT seq FROM messages WHERE alias=?1 AND id=?2",
                    params![alias, since],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?;
            match (by_id, since.parse::<f64>()) {
                (Some(seq), _) => after_seq = seq,
                (None, Ok(ts)) if ts.is_finite() => after_ts = ts,
                _ => {
                    return Err(Error::rejected(format!(
                        "since '{since}' is neither a message id of '{alias}' nor a timestamp"
                    )));
                }
            }
        }
        let mut filter = String::new();
        let mut filter_binds: Vec<Bind> = Vec::new();
        if let Some(from) = from {
            filter.push_str(" AND sender=?");
            filter_binds.push(Bind::Text(from.to_string()));
        }
        if let Some(source) = source {
            filter.push_str(" AND source=?");
            filter_binds.push(Bind::Text(source.to_string()));
        }
        if !states.is_empty() {
            filter.push_str(" AND state IN (");
            for (i, state) in states.iter().enumerate() {
                if i > 0 {
                    filter.push(',');
                }
                filter.push('?');
                filter_binds.push(Bind::Text(state.clone()));
            }
            filter.push(')');
        }
        if !include_app {
            filter.push_str(" AND source!='app_run_dispatch'");
        }
        let live = format!(
            "state NOT IN ('completed','failed','interrupted','cancelled') AND NOT {NOTICE_ENDED_SQL}"
        );
        let cols = INDEX_COLS;
        let mut live_binds = vec![Bind::Text(alias.to_string())];
        live_binds.extend(filter_binds.iter().cloned());
        let live_rows = index_query(
            &conn,
            &format!("SELECT {cols} FROM messages WHERE alias=? AND ({live}){filter} ORDER BY seq"),
            &live_binds,
        )?;
        let mut terminal_binds = vec![
            Bind::Text(alias.to_string()),
            Bind::Real(agent.created),
            Bind::Real(after_ts),
            Bind::Int(after_seq),
        ];
        terminal_binds.extend(filter_binds.iter().cloned());
        terminal_binds.push(Bind::Int(limit as i64));
        let mut rows = live_rows;
        rows.extend(index_query(
            &conn,
            &format!(
                "SELECT {cols} FROM messages WHERE alias=? AND NOT ({live})
                 AND created >= ? AND created >= ? AND seq > ?{filter}
                 ORDER BY seq DESC LIMIT ?"
            ),
            &terminal_binds,
        )?);
        rows.sort_by_key(|row| row.seq);
        // Count the same set the two queries can return: every matching
        // live row, plus terminal rows after the cursor. Older terminal
        // mail is not "omitted" from a cursor page.
        let mut count_binds = vec![
            Bind::Text(alias.to_string()),
            Bind::Real(agent.created),
            Bind::Real(after_ts),
            Bind::Int(after_seq),
        ];
        count_binds.extend(filter_binds);
        let total: i64 = count_query(
            &conn,
            &format!(
                "SELECT COUNT(*) FROM messages WHERE alias=? AND (({live})
                 OR (NOT ({live}) AND created >= ? AND created >= ? AND seq > ?)){filter}"
            ),
            &count_binds,
        )?;
        let omitted = (total as usize).saturating_sub(rows.len());
        Ok(MessageIndexPage { rows, omitted })
    }

    /// Current work and unresolved outcomes for overview probes. Terminal
    /// history stays available through `messages`; unknowns remain visible
    /// even after a clock step or alias re-registration, as there too.
    pub(crate) fn active_messages(&self, alias: &str) -> Result<Vec<Message>> {
        let conn = self.conn();
        self.agent_in(&conn, alias)?;
        let mut stmt = conn.prepare(
            "SELECT * FROM messages WHERE alias=?1
             AND state IN ('queued','submitting','running','unknown')
             AND source != 'topic_notice' ORDER BY seq",
        )?;
        let rows = stmt.query_map([alias], row_message)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// CAD-1353: one grouped read for the fleet status view — how many
    /// deliveries each alias is waiting to start and how long the oldest
    /// has waited. A running turn is already delivering, so it is not
    /// counted as queued; the stall view reports it separately.
    pub fn open_delivery_summary(&self) -> Result<HashMap<String, (i64, Option<f64>)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT alias, COUNT(*), MIN(created) FROM messages
             WHERE alias != '' AND state IN ('queued','submitting','unknown')
             AND source != 'topic_notice' GROUP BY alias",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get::<_, i64>(1)?, row.get::<_, Option<f64>>(2)?),
            ))
        })?;
        let mut summary = HashMap::new();
        for row in rows {
            let (alias, counts) = row?;
            summary.insert(alias, counts);
        }
        Ok(summary)
    }

    /// Running rows only, via the queue index: recurring summaries must
    /// not deserialize completed bodies and results on every board tick.
    pub(crate) fn running_messages(&self, alias: &str) -> Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT * FROM messages WHERE alias=?1 AND state='running' ORDER BY seq")?;
        let rows = stmt.query_map([alias], row_message)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The board's historical render-miss count, without fetching message
    /// bodies. Retain `messages`' registration boundary and non-terminal
    /// clock-step behavior. Old malformed result JSON counts as no result,
    /// as it does in `row_message`, rather than failing the whole snapshot.
    pub(crate) fn parked_message_count(&self, alias: &str) -> Result<i64> {
        let conn = self.conn();
        let agent = self.agent_in(&conn, alias)?;
        Ok(conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM messages WHERE alias=?1 AND state!='running'
             AND (created >= ?2 OR (state NOT IN
                  ('completed','failed','interrupted','cancelled') AND NOT {NOTICE_ENDED_SQL}))
             AND CASE WHEN json_valid(result)
                      THEN json_extract(result,'$.via') = 'pty_render_miss'
                      ELSE 0 END"
            ),
            params![alias, agent.created],
            |row| row.get(0),
        )?)
    }

    pub fn message(&self, id: &str) -> Result<Option<Message>> {
        let conn = self.conn();
        self.message_in(&conn, id)
    }

    /// CAD-1353: the recorded budget for a dispatch, falling back to the
    /// job default. The agent's own `stall_secs` remains the final
    /// fallback in the watcher.
    pub fn message_stall_budget(&self, message: &Message) -> Option<i64> {
        if let Some(secs) = message.stall_secs {
            return Some(secs);
        }
        let task_id = message.task_id.as_deref()?;
        let task = self.task(task_id).ok()?;
        self.job(&task.job_id).ok()?.stall_secs
    }

    /// Whether a message row with this id exists. A bare existence probe
    /// for send paths that must not decode the full row.
    pub fn message_exists(&self, id: &str) -> Result<bool> {
        let conn = self.conn();
        match conn.query_row(
            "SELECT 1 FROM messages WHERE id=? LIMIT 1",
            [id],
            |_| Ok(()),
        ) {
            Ok(()) => Ok(true),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Whether an agent caller may read this message body. The row must
    /// name the alias, and a known recipient UID must be the alias's
    /// CURRENT UID; a legacy NULL UID counts only from the current
    /// registration on (`created >= agent.created`). Unknown alias: no.
    pub fn agent_reads_message(&self, msg: &Message, alias: &str) -> Result<bool> {
        let Some(agent) = self.agent_opt(alias)? else {
            return Ok(false);
        };
        if msg.alias != agent.alias {
            return Ok(false);
        }
        Ok(match &msg.recipient_uid {
            Some(uid) => *uid == agent.uid,
            None => msg.created >= agent.created,
        })
    }

    /// CAD-880: message ids starting with `prefix`, ordered — the pool a
    /// unique id prefix resolves against. The prefix is matched literally
    /// (`%`, `_` and `\` are escaped, never wildcards); at most 11 rows
    /// are read — enough to tell unique from ambiguous.
    pub fn message_id_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut pattern = String::with_capacity(prefix.len() + 1);
        for c in prefix.chars() {
            if matches!(c, '\\' | '%' | '_') {
                pattern.push('\\');
            }
            pattern.push(c);
        }
        pattern.push('%');
        let mut stmt = conn
            .prepare("SELECT id FROM messages WHERE id LIKE ?1 ESCAPE '\\' ORDER BY id LIMIT 11")?;
        let rows = stmt.query_map([pattern], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
    }

    /// CAD-880 (F1): `message_id_prefix` scoped to one alias — an agent
    /// caller's prefix enumeration never names another agent's ids. The
    /// operator's explicit path uses the global pool above.
    pub fn message_id_prefix_for(&self, prefix: &str, alias: &str) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut pattern = String::with_capacity(prefix.len() + 1);
        for c in prefix.chars() {
            if matches!(c, '\\' | '%' | '_') {
                pattern.push('\\');
            }
            pattern.push(c);
        }
        pattern.push('%');
        // The same predicate as `agent_reads_message`: a known recipient
        // UID must be the alias's current UID; a NULL (legacy) UID counts
        // only from the current registration on. An unknown alias matches
        // nothing (NULL comparisons are false).
        let mut stmt = conn.prepare(
            "SELECT id FROM messages WHERE id LIKE ?1 ESCAPE '\\' AND alias = ?2 AND \
             (recipient_uid = (SELECT uid FROM agents WHERE alias = ?2) OR \
              (recipient_uid IS NULL AND created >= (SELECT created FROM agents WHERE alias = ?2))) \
             ORDER BY id LIMIT 11",
        )?;
        let rows = stmt.query_map((pattern, alias), |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
    }

    /// The agent's in-flight turn, if any — the actor loop is serial and
    /// holds one report-owing turn at a time (CAD-250); a routed
    /// notification is `running` only for its paste. The newest row wins
    /// when legacy rows overlap. The stall watch and the view surfaces
    /// both read this.
    pub fn running_message(&self, alias: &str) -> Result<Option<Message>> {
        let conn = self.conn();
        match conn.query_row(
            "SELECT * FROM messages WHERE alias=? AND state='running'
             ORDER BY seq DESC LIMIT 1",
            [alias],
            row_message,
        ) {
            Ok(m) => Ok(Some(m)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The lane's latest owned turn on this issue: the original
    /// dispatch kickoff or a daemon-issued native review follow-up
    /// (a `source='review'` row with no caller-derived sender — only
    /// `send_issue_followup` writes one, tagged server-side). Only rows
    /// carrying the lane's `issue`/`worktree` tags count: an untagged
    /// `review` row is ordinary mail, never this worker's turn.
    pub(crate) fn worker_issue_turn(
        &self,
        issue: &str,
        worker: &str,
    ) -> Result<Option<WorkerTurn>> {
        self.read_worker_turn(
            "SELECT id,state,created,CASE WHEN state='completed' THEN result END \
             FROM messages WHERE alias=?1 AND issue=?2 AND worktree IS NOT NULL \
             AND (source='dispatch' OR (source='review' AND sender_uid IS NULL)) \
             ORDER BY seq DESC LIMIT 1",
            params![worker, issue],
        )
    }

    /// The worktree the lane's latest dispatch kickoff was tagged
    /// with — the current authorized lane after a manual retry or
    /// worktree repair re-sent it. Follow-ups re-derive the lane tag
    /// from that native queue record; they never take a caller's or a
    /// follow-up's own claim, so `source='dispatch'` rows only — never
    /// a `source='review'` row a follow-up itself wrote.
    pub(crate) fn worker_issue_lane(&self, issue: &str, worker: &str) -> Result<Option<String>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT worktree FROM messages WHERE alias=?1 AND issue=?2 \
             AND source='dispatch' AND worktree IS NOT NULL \
             ORDER BY seq DESC LIMIT 1",
            params![worker, issue],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    fn read_worker_turn(
        &self,
        query: &str,
        parameters: impl rusqlite::Params,
    ) -> Result<Option<WorkerTurn>> {
        let row: Option<(String, String, f64, Option<String>)> = {
            let conn = self.conn();
            conn.query_row(query, parameters, |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .optional()?
        };
        let Some((id, state, created, raw)) = row else {
            return Ok(None);
        };
        let sha = if let Some(raw) = raw {
            let result: Value = serde_json::from_str(&raw)
                .map_err(|_| Error::rejected("the worker's durable result is malformed"))?;
            match result.get("sha") {
                Some(Value::String(sha)) => Some(super::check_commit_sha(sha)?),
                Some(Value::Null) | None => ["text", "note"].iter().find_map(|field| {
                    result
                        .get(field)
                        .and_then(Value::as_str)
                        .and_then(super::last_sha_line)
                }),
                Some(_) => {
                    return Err(Error::rejected(
                        "the worker's durable result revision is not a string",
                    ))
                }
            }
        } else {
            None
        };
        Ok(Some(WorkerTurn {
            id,
            state,
            created,
            sha,
        }))
    }

    /// CAD-250: the agent's delivered, unreported pty turns, oldest
    /// first. One at most once `take_queued` holds the line; more only
    /// for rows that accumulated before it did (adopted on a hot
    /// restart), which the report bound then retires.
    pub fn awaiting_reports(&self, alias: &str) -> Result<Vec<Message>> {
        Ok(self
            .running_messages(alias)?
            .into_iter()
            .filter(Message::awaiting_report)
            .collect())
    }

    /// CAD-250: every row holding the alias's turn ([`Message::holds_turn`]),
    /// oldest first — marked `awaiting_report` or not. The report bound
    /// walks these.
    pub fn held_turns(&self, alias: &str) -> Result<Vec<Message>> {
        Ok(self
            .running_messages(alias)?
            .into_iter()
            .filter(Message::holds_turn)
            .collect())
    }

    /// CAD-1266: every in-flight message (`running` or `submitted`) as
    /// `(alias, id, state, since)` in one query, `since` being `started`
    /// else `created`. Replaces loading each agent's whole history to
    /// filter it in memory; the terminal history is never read.
    pub fn inflight_messages(&self) -> Result<Vec<(String, String, String, f64)>> {
        self.read_tx(|conn| {
            Ok(conn.query_vec(
                "SELECT alias, id, state, COALESCE(started, created) FROM messages
                 WHERE state IN ('running','submitted')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?)
        })
    }

    /// CAD-375: every running message's turn token and its agent —
    /// `(alias, turn_id)`. The daemon withholds each from every
    /// connection but the owner's, current or not. Driven from `agents`
    /// so each probe is an index seek on `msg_queue(alias,state,…)`,
    /// never a scan of the history.
    pub fn running_turn_tokens(&self) -> Result<Vec<(String, String)>> {
        self.read_tx(|conn| {
            let stmt_sql = "SELECT a.alias, m.turn_id
                         FROM agents a JOIN messages m ON m.alias = a.alias AND m.state = 'running'
                         WHERE m.turn_id IS NOT NULL AND m.turn_id != ''";
            let rows = conn
                .query_vec(stmt_sql, [], |r| Ok((r.get(0)?, r.get(1)?)))
                .map(|rows| rows.into_iter().map(Ok::<_, rusqlite::Error>))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// Queued deliveries that are turns of their own — what an
    /// unreported turn holds back (routed notifications still pass).
    pub fn queued_turns(&self, alias: &str) -> Result<i64> {
        self.read_tx(|conn| {
            Ok(conn.query_row(
                &format!(
                    "SELECT COUNT(*) FROM messages WHERE alias=? AND state='queued'
                             AND source NOT IN {TURNLESS_SOURCES_SQL}"
                ),
                [alias],
                |r| r.get(0),
            )?)
        })
    }

    /// Providers with at least one *live* in-flight turn — the WAL
    /// watcher refuses to checkpoint a store whose provider is mid-turn.
    /// Live (CAD-250) means the alias has an actor (`live`, the daemon's
    /// owned set) and, for a delivered pty turn awaiting its report, the
    /// report bound has not run out at `now`: a stale row on a dead actor
    /// or an overdue one never defers a checkpoint.
    pub fn busy_providers(&self, live: &HashSet<String>, now: f64) -> Result<HashSet<String>> {
        self.read_tx(|conn| {
            let stmt_sql = "SELECT a.provider AS agent_provider, a.params AS agent_params,
                                a.endpoint_kind AS agent_kind, m.*
                         FROM agents a JOIN messages m ON m.alias = a.alias
                         WHERE m.state IN ('submitting','running')";
            let rows = conn
                .query_vec(stmt_sql, [], |r| {
                    let params: Option<String> = r.get("agent_params")?;
                    Ok((
                        r.get::<_, String>("agent_provider")?,
                        params.and_then(|p| serde_json::from_str::<Value>(&p).ok()),
                        r.get::<_, String>("agent_kind")? == "pty",
                        row_message(r)?,
                    ))
                })
                .map(|rows| rows.into_iter().map(Ok::<_, rusqlite::Error>))?;
            let mut busy = HashSet::new();
            for row in rows {
                let (provider, params, pty, message) = row?;
                if !live.contains(&message.alias) {
                    continue;
                }
                // The report bound is a pty rule: a managed turn is live
                // while its provider call runs, however long.
                if pty && message.report_overdue(report_timeout_secs(params.as_ref()), now) {
                    continue;
                }
                busy.insert(provider);
            }
            Ok(busy)
        })
    }

    /// Bound a non-agent event stream to its newest `keep` rows — the
    /// daemon's `wal_checkpointed` stream has no agents row, so the
    /// agent-removal `DELETE` never reaches it.
    ///
    /// Daemon `assist_intent` rows sit outside the `keep` window and are
    /// dropped only when terminal (`delivered`/`refused`/`unresolved`), with a
    /// `job_id` whose canonical `jobs` row is gone; pending, NULL, unknown or
    /// malformed status and a missing job association are kept. On the daemon
    /// stream an open mutation intent is not pruned: a `prepared` row survives
    /// until its matching `finished` row exists (same request, correlation,
    /// company, action and resource, schema 1, a valid outcome for its kind).
    /// A `pm_mutation_prepared` row has no SQLite finish, so it is always kept.
    /// Malformed or unmatched identity keeps the row. Protected rows are kept
    /// in addition to the newest `keep`; ordinary rows follow the cap as before.
    pub fn prune_stream(&self, alias: &str, keep: i64) -> Result<()> {
        self.write_tx(|conn| {
            conn.execute(
                "DELETE FROM events WHERE alias=?1 AND (
                   (kind='assist_intent' AND alias='daemon' AND job_id IS NOT NULL
                    AND CASE WHEN json_valid(payload)
                          THEN json_extract(payload,'$.status') END
                        IN ('delivered','refused','unresolved')
                    AND NOT EXISTS (SELECT 1 FROM jobs WHERE jobs.id=events.job_id))
                   OR (NOT (kind='assist_intent' AND alias='daemon') AND seq NOT IN (
                        SELECT seq FROM events
                        WHERE alias=?1 AND NOT (kind='assist_intent' AND alias='daemon')
                        ORDER BY seq DESC LIMIT ?2)
                 AND NOT (alias=?3 AND (
                     kind='pm_mutation_prepared'
                     OR (kind IN ('app_workspace_mutation_prepared','board_session_mutation_prepared')
                         AND NOT EXISTS (SELECT 1 FROM events f
                             WHERE f.alias=events.alias
                             AND f.kind=CASE events.kind
                                 WHEN 'app_workspace_mutation_prepared'
                                 THEN 'app_workspace_mutation_finished'
                                 ELSE 'board_session_mutation_finished' END
                             AND CASE f.kind
                                 WHEN 'app_workspace_mutation_finished'
                                 THEN json_extract(f.payload,'$.outcome') IN
                                     ('installed','removed','already_removed','restored',
                                      'already_live_consent_recorded','already_live','refused','uncertain')
                                 ELSE json_extract(f.payload,'$.outcome') IN ('completed','uncertain') END
                             AND json_extract(f.payload,'$.schema_version')=1
                             AND json_extract(events.payload,'$.schema_version')=1
                             AND json_extract(f.payload,'$.request_id')=json_extract(events.payload,'$.request_id')
                             AND json_extract(f.payload,'$.correlation_id')=json_extract(events.payload,'$.correlation_id')
                             AND json_extract(f.payload,'$.company') IS json_extract(events.payload,'$.company')
                             AND json_extract(f.payload,'$.action')=json_extract(events.payload,'$.action')
                             AND json_extract(f.payload,'$.resource')=json_extract(events.payload,'$.resource'))))))
                 )",
                params![alias, keep, Self::DAEMON_STREAM],
            )?;
            Ok(())
        })
    }

    /// The next still-waiting message for the agent — `queued` or
    /// mid-gate `submitting` — in `take_queued`'s order. The stall watch
    /// tracks it so a pane menu blocking delivery is visible before any
    /// turn starts.
    pub fn queued_head(&self, alias: &str) -> Result<Option<Message>> {
        let conn = self.conn();
        // A mid-gate `submitting` row is the one being delivered, so it
        // leads whatever its rank.
        match conn.query_row(
            "SELECT * FROM messages WHERE alias=? AND state IN
             ('queued','submitting')
             ORDER BY state='submitting' DESC, priority DESC, seq LIMIT 1",
            [alias],
            row_message,
        ) {
            Ok(m) => Ok(Some(m)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// CAD-1700: the complete queue's routing metadata in delivery order
    /// (`submitting` leads, then priority, then arrival): each message id
    /// and the conversation its enqueue note landed in. One narrow
    /// projection — no message bodies, no per-row lookup — so a pane can
    /// find its own message and its predecessor at any queue position.
    /// The first entry per message is the grouped `MIN(e.seq)`: `seq` is a
    /// global AUTOINCREMENT key, so SQLite's bare-column-with-min rule
    /// makes the row's bare `thread_id` exactly that entry — no window
    /// function, which the store's authorizer does not allow.
    pub fn queued_metadata(&self, alias: &str) -> Result<Vec<(String, Option<String>)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "WITH queue AS (
                 SELECT id, state, priority, seq FROM messages
                 WHERE alias=?1 AND state IN ('queued','submitting')
             ), first_entry AS (
                 SELECT e.message_id, e.thread_id, MIN(e.seq) AS first_seq
                 FROM thread_entries e JOIN threads t ON t.id=e.thread_id
                 WHERE e.message_id IN (SELECT id FROM queue)
                   AND (t.alias=?1 OR t.archived_alias=?1)
                 GROUP BY e.message_id
             )
             SELECT q.id, f.thread_id
             FROM queue q
             LEFT JOIN first_entry f ON f.message_id=q.id
             ORDER BY q.state='submitting' DESC, q.priority DESC, q.seq",
        )?;
        let rows = stmt.query_map([alias], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// CAD-413: per `stopped` agent with work waiting, its oldest queued
    /// message id and the queued count — the candidates an auto-resume
    /// sweep considers. Nudges are never queued for a stopped agent and
    /// are not work to resume for. One grouped pass on the queue index.
    pub fn queued_for_stopped(&self) -> Result<Vec<(String, String, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT m.alias, m.id FROM messages m JOIN agents a ON a.alias = m.alias
             WHERE a.state = 'stopped' AND a.archived_at IS NULL
               AND m.state = 'queued' AND m.source NOT IN (?, ?)
             ORDER BY m.alias, m.seq",
        )?;
        let rows = stmt.query_map([NUDGE_SOURCE, crate::proto::TOPIC_NOTICE_SOURCE], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out: Vec<(String, String, i64)> = Vec::new();
        for row in rows {
            let (alias, id) = row?;
            match out.last_mut() {
                Some(last) if last.0 == alias => last.2 += 1,
                _ => out.push((alias, id, 1)),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod index_tests {
    use super::*;
    use crate::store::agents::NewAgent;

    fn open() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("cadence.sqlite3")).unwrap();
        (dir, store)
    }

    fn register(store: &Store, alias: &str, dir: &std::path::Path) {
        store
            .register_agent(&NewAgent {
                alias,
                provider: "fake",
                endpoint_kind: "managed",
                role: "worker",
                cwd: dir.to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
    }

    fn index<'a>(
        store: &Store,
        alias: &str,
        states: &'a [String],
        from: Option<&'a str>,
    ) -> MessageIndexPage {
        store
            .message_index(
                alias,
                &MessageIndexQuery {
                    states,
                    from,
                    source: None,
                    since: None,
                    limit: 20,
                    include_app: false,
                },
            )
            .unwrap()
    }

    #[test]
    fn index_omits_body_result_and_turn_token() {
        let (dir, store) = open();
        register(&store, "worker", dir.path());
        register(&store, "pm", dir.path());
        let secret = "turn-token-secret-body";
        store
            .enqueue("worker", secret, None, "m-run", "user")
            .unwrap();
        store.mark_running("m-run", "pty-secret-token").unwrap();
        store
            .enqueue_sent(
                "worker",
                "from-pm-body",
                None,
                "m-pm",
                "user",
                None,
                &Sender::Agent("pm".into()),
            )
            .unwrap();
        let page = index(&store, "worker", &[], None);
        assert_eq!(page.rows.len(), 2);
        let running = page.rows.iter().find(|row| row.id == "m-run").unwrap();
        assert!(running.awaiting_report || running.state == "running");
        assert_eq!(running.body_chars, secret.chars().count() as i64);
        let encoded = serde_json::to_string(&running.to_json()).unwrap();
        for leaked in [secret, "pty-secret-token", "from-pm-body"] {
            assert!(!encoded.contains(leaked), "{encoded}");
        }
        for key in ["body", "turn_id", "result", "error", "sender_uid"] {
            assert!(running.to_json().get(key).is_none(), "{key}");
        }
        let from_pm = index(&store, "worker", &[], Some("pm"));
        assert_eq!(from_pm.rows.len(), 1);
        assert_eq!(from_pm.rows[0].id, "m-pm");
        assert_eq!(from_pm.rows[0].to_json()["from"], "pm");
        let queued_states = ["queued".to_string()];
        let queued = index(&store, "worker", &queued_states, None);
        assert_eq!(queued.rows.len(), 1);
        assert_eq!(queued.rows[0].id, "m-pm");
    }
}

/// CAD-1700: the master push stamp and the ordered queue it reports.
#[cfg(test)]
mod master_stamp_tests {
    use super::*;
    use crate::store::agents::NewAgent;
    use crate::store::{NewEntry, KIND_MESSAGE, ROLE_SYSTEM};

    fn open() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("cadence.sqlite3")).unwrap();
        store
            .register_agent(&NewAgent {
                alias: "master",
                provider: "fake",
                endpoint_kind: "managed",
                role: "worker",
                cwd: dir.path().to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        (dir, store)
    }

    #[test]
    fn master_stamp_moves_on_enqueue_run_finish_and_thread_entry() {
        let (_dir, store) = open();
        let quiet = store.master_stamp("master").unwrap();
        assert_eq!(
            store.master_stamp("master").unwrap(),
            quiet,
            "an idle master is stable"
        );

        store
            .enqueue("master", "one", None, "m-1", "operator")
            .unwrap();
        let queued = store.master_stamp("master").unwrap();
        assert_ne!(queued, quiet, "enqueue moves the stamp");
        assert_eq!(queued.2, 1, "one queued message is counted");

        store.mark_running("m-1", "tok").unwrap();
        let running = store.master_stamp("master").unwrap();
        assert_ne!(running, queued, "starting the turn moves the stamp");
        assert_eq!((running.1, running.2), (1, 0), "one running, none queued");

        store.ensure_thread("master").unwrap();
        store
            .thread_append(
                "master",
                NewEntry {
                    role: ROLE_SYSTEM,
                    kind: KIND_MESSAGE,
                    text: "a step",
                    payload: None,
                    message_id: Some("m-1"),
                },
            )
            .unwrap();
        let entry = store.master_stamp("master").unwrap();
        assert!(
            entry.3 > running.3,
            "a thread entry moves the newest entry seq"
        );

        store
            .finish_running("m-1", "completed", &json!({"text": "done"}), None)
            .unwrap()
            .unwrap();
        assert_ne!(
            store.master_stamp("master").unwrap(),
            entry,
            "finishing the turn moves the stamp"
        );
    }

    /// CAD-1700: the pane's queue projection is complete rather than a
    /// 20-row page, in delivery order, and carries each message's
    /// conversation from the one narrow join. An enqueue writes the
    /// message's operator note when the alias has a home thread, so every
    /// master row resolves home; an alias with no home thread drops the
    /// note (the documented CLI/unattributed path), so its row is NULL.
    #[test]
    fn queued_metadata_is_complete_and_carries_conversations() {
        let (_dir, store) = open();
        let home = store.ensure_thread("master").unwrap().id;
        for i in 1..=25 {
            let id = format!("m{i}");
            store.enqueue("master", &id, None, &id, "operator").unwrap();
        }
        let queued = store.queued_metadata("master").unwrap();
        assert_eq!(
            queued.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            (1..=25).map(|i| format!("m{i}")).collect::<Vec<_>>(),
            "the projection is complete and in delivery order"
        );
        assert_eq!(queued[19].0, "m20", "the predecessor is represented");
        assert_eq!(queued[20].0, "m21", "the 21st message is represented");
        assert!(
            queued
                .iter()
                .all(|(_, conversation)| conversation.as_deref() == Some(home.as_str())),
            "every enqueued master message resolves its home conversation"
        );
        // A canonical message whose note was dropped: w1 has an agent but
        // no home thread, so its enqueue note lands nowhere.
        store
            .register_agent(&NewAgent {
                alias: "w1",
                provider: "fake",
                endpoint_kind: "managed",
                role: "worker",
                cwd: _dir.path().to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        store
            .enqueue("w1", "unthreaded", None, "w-note", "operator")
            .unwrap();
        assert_eq!(
            store.queued_metadata("w1").unwrap(),
            vec![("w-note".to_string(), None::<String>)],
            "a queued message with no thread note has no conversation"
        );
    }
}
