//! Durable message queue: enqueue, supersede and per-alias reads.

use crate::adapter::registry;
use crate::error::{Error, Result};
use crate::proto::identifier;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashSet;

use super::agents::Agent;
use super::schema::Take;
use super::{now, Sender, Store};

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
}

pub(super) fn row_message(row: &rusqlite::Row) -> rusqlite::Result<Message> {
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
const TURNLESS_SOURCES_SQL: &str = "('worker_result','worker_notice','job_event','nudge')";

/// The `unknown` rows that fence an agent: every one except a nudge's,
/// whose unconfirmed paste belongs to no turn (CAD-250).
pub(super) const FENCING_UNKNOWN_SQL: &str = "state='unknown' AND source != 'nudge'";

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
    ) -> Result<(bool, String)> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
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
        let out = self.enqueue_tx(
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
            refs,
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
        tx.commit()?;
        Ok(out)
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
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
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
        )?;
        tx.commit()?;
        Ok(out)
    }

    /// The ids a steering send superseded, oldest first.
    fn superseded_by_in(tx: &Connection, id: &str) -> Result<Vec<String>> {
        let mut stmt = tx.prepare(
            "SELECT id FROM messages WHERE state='cancelled'
             AND json_extract(result,'$.superseded_by')=? ORDER BY seq",
        )?;
        let ids = stmt
            .query_map([id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    /// CAD-158 fail-closed gate: `id` may be superseded by a send to
    /// `alias` only while it is `alias`'s own still-`queued` instruction.
    /// The refusal names the id and what it is.
    fn supersedable_in(&self, tx: &Connection, alias: &str, id: &str) -> Result<Message> {
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
        tx: &Connection,
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
        tx: &Connection,
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

    /// Transactional enqueue — validation, idempotent dedupe, insert,
    /// `queued` event — usable inside a caller's `BEGIN IMMEDIATE`.
    /// Every caller-reachable path (`agent_send`, `thread_send`,
    /// `task_dispatch`, dispatch kickoffs) comes through here, and the
    /// daemon's reserved ids and sources are refused (CAD-445).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn enqueue_tx(
        &self,
        tx: &Connection,
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
    ) -> Result<(bool, String)> {
        self.enqueue_tx_as(
            tx, alias, body, reply_to, id, source, task_id, issue, worktree, sender, priority,
            false, refs,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn enqueue_tx_as(
        &self,
        tx: &Connection,
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
    ) -> Result<(bool, String)> {
        if !daemon {
            crate::proto::caller_message(id, source)?;
        }
        if body.is_empty() || body.len() > 48_000 {
            return Err(Error::rejected("Prompt must contain 1-48000 characters"));
        }
        identifier(id, "Message id")?;
        self.agent_in(tx, alias)?;
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
            self.task_in(tx, task)?;
        }
        if let Some(old) = self.message_in(tx, id)? {
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
                && Self::entry_refs_in(tx, id)? == refs.cloned();
            if !same {
                return Err(Error::rejected(
                    "Message id was already used with different content",
                ));
            }
            return Ok((true, old.state));
        }
        tx.execute(
            "INSERT INTO messages(id,alias,body,reply_to,source,task_id,issue,worktree,priority,created)
             VALUES(?,?,?,?,?,?,?,?,?,?)",
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
                now()
            ],
        )?;
        Self::event_scoped(
            tx,
            alias,
            "queued",
            json!({
                "message": id,
                "source": source,
                "reply_to": reply_to,
                "sender": sender.label(),
                "recipient_identity": reply_recipient
                    .as_ref()
                    .map(Self::agent_identity),
            }),
            None,
            task_id,
        )?;
        Self::thread_note_enqueued(tx, alias, sender, source, body, id, refs)?;
        Ok((false, "queued".to_string()))
    }

    pub(super) fn message_in(&self, conn: &Connection, id: &str) -> Result<Option<Message>> {
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
        tx: &Connection,
        alias: &str,
        message_id: &str,
        source: &str,
    ) -> Result<Option<Value>> {
        let mut stmt = tx.prepare(
            "SELECT payload FROM events
             WHERE alias=? AND kind='queued' ORDER BY seq",
        )?;
        let mut rows = stmt.query([alias])?;
        while let Some(row) = rows.next()? {
            let payload: String = row.get(0)?;
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
        tx: &Connection,
        source_alias: &str,
        source_message: &str,
        source: &str,
        recipient: &str,
    ) -> Result<(Option<Value>, Option<Agent>, Option<&'static str>)> {
        let expected = self.queued_recipient_identity(tx, source_alias, source_message, source)?;
        let current = self.agent_opt_in(tx, recipient)?;
        let reason = match current.as_ref() {
            None => Some("recipient_missing"),
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
        tx: &Connection,
        delivery: &str,
    ) -> Result<bool> {
        let mut stmt = tx.prepare(
            "SELECT payload FROM events
             WHERE alias=? AND kind='handoff_unresolved' ORDER BY seq",
        )?;
        let mut rows = stmt.query([Self::DAEMON_STREAM])?;
        while let Some(row) = rows.next()? {
            let payload: String = row.get(0)?;
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
        tx: &Connection,
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
        tx: &Connection,
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
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
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
        let running_turn: Option<String> = tx
            .query_row(
                &format!(
                    "SELECT turn_id FROM messages WHERE alias=? AND state='running'
                     AND source NOT IN {TURNLESS_SOURCES_SQL} ORDER BY seq LIMIT 1"
                ),
                [alias],
                |r| r.get(0),
            )
            .optional()?;
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
            let Some(message) = next else {
                // A prior routed row in this same transaction may have
                // been failed as unresolved before the queue became empty.
                // Commit that durable evidence even though no message is
                // returned to the actor.
                tx.commit()?;
                return Ok(Take::Empty);
            };
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
                    self.fail_unresolved_routed(&tx, &message, &agent, expected.as_ref(), reason)?;
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
            tx.commit()?;
            return Ok(Take::Message(Box::new(message)));
        }
    }

    /// Record that the provider acknowledged a turn start.
    pub fn mark_running(&self, message_id: &str, turn_id: &str) -> Result<()> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
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
        tx.commit()?;
        Ok(())
    }

    /// Return a `submitting` message to `queued` — the submission gate
    /// refused before any paste, so retry is safe.
    pub fn requeue(&self, message_id: &str) -> Result<()> {
        let conn = self.write_conn()?;
        conn.execute(
            "UPDATE messages SET state='queued',started=NULL
             WHERE id=? AND state='submitting'",
            [message_id],
        )?;
        Ok(())
    }

    /// PTY submission was accepted by the terminal: the message stays
    /// `running` (turn_id already recorded) with a durable `submitted`
    /// marker until an explicit ack/result report lands.
    pub fn mark_submitted(&self, message: &Message) -> Result<()> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
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
        tx.commit()?;
        Ok(())
    }

    /// Explicit acknowledgement for a running message; it stays
    /// `running` until a result completes it — a pty result report, or
    /// (CAD-162) the adapter's turn result on a managed endpoint. Only a
    /// pty-style (explicitly reported) turn carries the `submitted`
    /// marker, so a managed ack never makes its turn `awaiting_report`.
    pub fn mark_ack(&self, message: &Message, text: Option<&str>) -> Result<()> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let turn_result = tx
            .query_row(
                "SELECT provider, endpoint_kind FROM agents WHERE alias=?",
                [&message.alias],
                |r| {
                    Ok(registry::reports_turn_result(
                        &r.get::<_, String>(0)?,
                        &r.get::<_, String>(1)?,
                    ))
                },
            )
            .optional()?
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
        tx.commit()?;
        Ok(())
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
        let mut stmt = conn.prepare(
            "SELECT * FROM messages WHERE alias=?1
             AND (created >= ?2 OR state NOT IN
                  ('completed','failed','interrupted','cancelled'))
             ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![alias, agent.created], row_message)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn message(&self, id: &str) -> Result<Option<Message>> {
        let conn = self.conn();
        self.message_in(&conn, id)
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

    /// CAD-250: the agent's delivered, unreported pty turns, oldest
    /// first. One at most once `take_queued` holds the line; more only
    /// for rows that accumulated before it did (adopted on a hot
    /// restart), which the report bound then retires.
    pub fn awaiting_reports(&self, alias: &str) -> Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT * FROM messages WHERE alias=? AND state='running' ORDER BY seq")?;
        let rows = stmt.query_map([alias], row_message)?;
        let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.into_iter().filter(Message::awaiting_report).collect())
    }

    /// CAD-250: every row holding the alias's turn ([`Message::holds_turn`]),
    /// oldest first — marked `awaiting_report` or not. The report bound
    /// walks these.
    pub fn held_turns(&self, alias: &str) -> Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT * FROM messages WHERE alias=? AND state='running' ORDER BY seq")?;
        let rows = stmt.query_map([alias], row_message)?;
        let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.into_iter().filter(Message::holds_turn).collect())
    }

    /// CAD-375: every running message's turn token and its agent —
    /// `(alias, turn_id)`. The daemon withholds each from every
    /// connection but the owner's, current or not. Driven from `agents`
    /// so each probe is an index seek on `msg_queue(alias,state,…)`,
    /// never a scan of the history.
    pub fn running_turn_tokens(&self) -> Result<Vec<(String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT a.alias, m.turn_id
             FROM agents a JOIN messages m ON m.alias = a.alias AND m.state = 'running'
             WHERE m.turn_id IS NOT NULL AND m.turn_id != ''",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Queued deliveries that are turns of their own — what an
    /// unreported turn holds back (routed notifications still pass).
    pub fn queued_turns(&self, alias: &str) -> Result<i64> {
        let conn = self.conn();
        Ok(conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM messages WHERE alias=? AND state='queued'
                 AND source NOT IN {TURNLESS_SOURCES_SQL}"
            ),
            [alias],
            |r| r.get(0),
        )?)
    }

    /// Providers with at least one *live* in-flight turn — the WAL
    /// watcher refuses to checkpoint a store whose provider is mid-turn.
    /// Live (CAD-250) means the alias has an actor (`live`, the daemon's
    /// owned set) and, for a delivered pty turn awaiting its report, the
    /// report bound has not run out at `now`: a stale row on a dead actor
    /// or an overdue one never defers a checkpoint.
    pub fn busy_providers(&self, live: &HashSet<String>, now: f64) -> Result<HashSet<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT a.provider AS agent_provider, a.params AS agent_params,
                    a.endpoint_kind AS agent_kind, m.*
             FROM agents a JOIN messages m ON m.alias = a.alias
             WHERE m.state IN ('submitting','running')",
        )?;
        let rows = stmt.query_map([], |r| {
            let params: Option<String> = r.get("agent_params")?;
            Ok((
                r.get::<_, String>("agent_provider")?,
                params.and_then(|p| serde_json::from_str::<Value>(&p).ok()),
                r.get::<_, String>("agent_kind")? == "pty",
                row_message(r)?,
            ))
        })?;
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
    }

    /// Bound a non-agent event stream to its newest `keep` rows — the
    /// daemon's `wal_checkpointed` stream has no agents row, so the
    /// agent-removal `DELETE` never reaches it.
    pub fn prune_stream(&self, alias: &str, keep: i64) -> Result<()> {
        let conn = self.write_conn()?;
        conn.execute(
            "DELETE FROM events WHERE alias=?1 AND seq NOT IN (
                 SELECT seq FROM events WHERE alias=?1
                 ORDER BY seq DESC LIMIT ?2)",
            params![alias, keep],
        )?;
        Ok(())
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

    /// CAD-413: per `stopped` agent with work waiting, its oldest queued
    /// message id and the queued count — the candidates an auto-resume
    /// sweep considers. Nudges are never queued for a stopped agent and
    /// are not work to resume for. One grouped pass on the queue index.
    pub fn queued_for_stopped(&self) -> Result<Vec<(String, String, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT m.alias, m.id FROM messages m JOIN agents a ON a.alias = m.alias
             WHERE a.state = 'stopped' AND m.state = 'queued' AND m.source != ?
             ORDER BY m.alias, m.seq",
        )?;
        let rows = stmt.query_map([NUDGE_SOURCE], |r| {
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
