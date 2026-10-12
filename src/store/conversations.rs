//! CAD-819: the cross-agent work conversation view — read-only.
//!
//! A pair conversation is the messages between two durable participant
//! uids (`messages.sender_uid` / `recipient_uid`), in either direction.
//! Its id is derived from the SORTED participant keys, so a client never
//! builds a pair and the daemon never trusts one. Reads never change a
//! message's state: a queued row stays queued (no drain).
//!
//! App installation conversations (`threads.install_id`) are a separate
//! namespace and are excluded here: this view is the agent work view only.

use rusqlite::params;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::messages::{row_message, Message};
use super::{Store, StoreConn};
use crate::error::Result;

/// The participant key for a uid, as the conversation id sees it.
fn key(uid: &str) -> String {
    format!("agent:{uid}")
}

/// The conversation id for two participant keys, order-independent.
pub fn conversation_id(a: &str, b: &str) -> String {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let digest = Sha256::digest(format!("{lo}|{hi}").as_bytes());
    let hex: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
    format!("conv-{hex}")
}

/// A pair's `(state -> count)` rows and its oldest queued message's `created`.
pub type PairCounts = (Vec<(String, i64)>, Option<f64>);

/// One pair conversation row (the list view).
#[derive(Debug, Clone)]
pub struct PairSummary {
    pub id: String,
    pub a_uid: String,
    pub b_uid: String,
    pub last_at: f64,
    pub last_seq: i64,
}

/// Daemon-written and routed/dispatch sources: system traffic. A message
/// is system when its id is daemon-minted (`sys-`) or its stored source is
/// one of these. Operator and agent sends are never system.
pub const SYSTEM_SOURCES: &[&str] = &[
    "nudge",
    "wake",
    "answer",
    "effect",
    "permission",
    "topic_notice",
    "worker_result",
    "worker_notice",
    "job_event",
    "job_dispatch",
    "kickoff",
    "app_run_dispatch",
];

/// One read/list filter context, resolved by the daemon. The client never
/// supplies a uid or a repo: `agent_uid` and the project sets come from
/// the daemon's own agent rows and registered project repos.
#[derive(Default)]
pub struct Scope<'a> {
    pub include_system: bool,
    /// The agent's uid when an agent filter is set.
    pub agent_uid: Option<&'a str>,
    /// Agent uids whose registered checkout is in the project.
    pub project_uids: Option<&'a [String]>,
    /// The project's repos as lowercase `owner/name` (task/job fallback).
    pub project_repos: Option<&'a [String]>,
}

/// The shared `WHERE` clause for a message scope. Binds `?1..` for the
/// pair/agent uids; returns the SQL fragment and its bind values.
fn scope_sql(scope: &Scope, args: &mut Vec<String>, recipient_only: bool) -> String {
    let mut clauses = Vec::new();
    if !scope.include_system {
        let sources = SYSTEM_SOURCES
            .iter()
            .map(|s| format!("'{s}'"))
            .collect::<Vec<_>>()
            .join(",");
        clauses.push(format!("id NOT LIKE 'sys-%' AND source NOT IN ({sources})"));
    }
    if let Some(uid) = scope.agent_uid {
        args.push(uid.to_string());
        let n = args.len();
        clauses.push(if recipient_only {
            format!("recipient_uid=?{n}")
        } else {
            format!("(sender_uid=?{n} OR recipient_uid=?{n})")
        });
    }
    if let Some(uids) = scope.project_uids {
        let mut ins = Vec::new();
        for uid in uids {
            args.push(uid.clone());
            ins.push(format!("?{}", args.len()));
        }
        let mut repo_hit = String::from("0");
        if let Some(repos) = scope.project_repos.filter(|r| !r.is_empty()) {
            let mut marks = Vec::new();
            for repo in repos {
                args.push(repo.clone());
                marks.push(format!("?{}", args.len()));
            }
            repo_hit = format!(
                "EXISTS (SELECT 1 FROM tasks t JOIN jobs j ON j.id=t.job_id
                          WHERE t.id=messages.task_id AND lower(j.repo) IN ({}))",
                marks.join(",")
            );
        }
        if ins.is_empty() {
            clauses.push(format!("({repo_hit})"));
        } else {
            let who = if recipient_only {
                format!("recipient_uid IN ({})", ins.join(","))
            } else {
                format!(
                    "(sender_uid IN ({i}) OR recipient_uid IN ({i}))",
                    i = ins.join(",")
                )
            };
            clauses.push(format!("({who} OR {repo_hit})"));
        }
    }
    if clauses.is_empty() {
        "1=1".to_string()
    } else {
        clauses.join(" AND ")
    }
}

/// Whether one stored message is system traffic: the same rule the SQL
/// scope applies (a daemon `sys-` id or a system source).
pub fn is_system(m: &Message) -> bool {
    m.id.starts_with(crate::proto::DAEMON_MESSAGE_PREFIX)
        || SYSTEM_SOURCES.contains(&m.source.as_str())
}

/// The pair predicate over `?1`/`?2` (either direction).
fn pair_clause() -> &'static str {
    "((sender_uid=?1 AND recipient_uid=?2) OR (sender_uid=?2 AND recipient_uid=?1))"
}

/// The legacy/system bucket predicate: no recorded sender, scoped by the
/// shared filter. Under an agent or project scope only recipient matches
/// count, so an unattributed row is never shown as its sender.
fn unknown_clause(scope: &Scope, args: &mut Vec<String>) -> String {
    let base = "sender_uid IS NULL AND sender IS NULL AND origin IS NULL";
    format!("{base} AND {}", scope_sql(scope, args, true))
}

impl Store {
    /// Routed copies (`origin` set) among the messages of a pair, keyed by
    /// the original message they answer. Read-only.
    pub fn work_conversation_routed_of(
        &self,
        a_uid: &str,
        b_uid: &str,
        origins: &[String],
    ) -> Result<Vec<Value>> {
        if origins.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; origins.len()].join(",");
        let sql = format!(
            "SELECT * FROM messages WHERE origin IN ({marks})
               AND ((sender_uid=? AND recipient_uid=?) OR (sender_uid=? AND recipient_uid=?))
             ORDER BY seq"
        );
        self.read_tx(|conn| {
            let mut args: Vec<String> = origins.to_vec();
            args.extend([
                a_uid.to_string(),
                b_uid.to_string(),
                b_uid.to_string(),
                a_uid.to_string(),
            ]);
            let rows: Vec<Message> =
                conn.query_vec(&sql, rusqlite::params_from_iter(args.iter()), row_message)?;
            Ok(rows
                .iter()
                .map(|m| {
                    json!({
                        "id": m.id,
                        "seq": m.seq,
                        "origin": m.origin,
                        "source": m.source,
                        "state": m.state,
                        "result": m.result,
                        "error": m.error,
                        "created": m.created,
                    })
                })
                .collect())
        })
    }

    /// The participant JSON for a uid: its live alias and `removed: false`,
    /// or — once the agent is gone — the alias it had at its last recorded
    /// message and `removed: true`. Never reattributed to a re-registered
    /// alias: the snapshot is taken from the uid's own message rows.
    pub fn participant(&self, uid: &str) -> Result<Value> {
        let live = self.alias_of_uid(uid)?;
        let alias = match live.clone() {
            Some(a) => Some(a),
            None => self.read_tx(|conn| {
                // The newest stored label for this uid, from the message
                // that carried it (never another uid's label).
                Ok(conn.query_opt(
                    "SELECT label FROM (
                         SELECT seq, sender AS label FROM messages
                           WHERE sender_uid=? AND sender IS NOT NULL
                         UNION ALL
                         SELECT seq, recipient AS label FROM messages
                           WHERE recipient_uid=? AND recipient IS NOT NULL)
                       ORDER BY seq DESC LIMIT 1",
                    params![uid, uid],
                    |r| r.get::<_, String>(0),
                )?)
            })?,
        };
        Ok(json!({
            "uid": uid,
            "alias": alias,
            "kind": "agent",
            "removed": live.is_none(),
        }))
    }

    /// The live alias of a participant uid, `None` when that agent was
    /// removed (its history stays keyed by the uid).
    pub fn alias_of_uid(&self, uid: &str) -> Result<Option<String>> {
        self.read_tx(|conn| {
            Ok(conn.query_opt("SELECT alias FROM agents WHERE uid=?", [uid], |r| r.get(0))?)
        })
    }

    /// Every pair conversation between two agent uids, most recent first.
    /// Routed copies (origin set) are part of their pair, not separate.
    /// Unknown-sender legacy rows are excluded: they have no pair.
    pub fn work_conversation_pairs(&self) -> Result<Vec<PairSummary>> {
        self.read_tx(|conn| {
            let rows: Vec<(String, String, f64, i64)> = conn.query_vec(
                "SELECT sender_uid, recipient_uid, MAX(created), MAX(seq)
                 FROM messages
                 WHERE sender_uid IS NOT NULL AND recipient_uid IS NOT NULL
                   AND sender_uid != recipient_uid
                 GROUP BY MIN(sender_uid, recipient_uid), MAX(sender_uid, recipient_uid)
                 ORDER BY MAX(seq) DESC",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            Ok(rows
                .into_iter()
                .map(|(s, r, last_at, last_seq)| {
                    let (lo, hi) = if s <= r { (s, r) } else { (r, s) };
                    PairSummary {
                        id: conversation_id(&key(&lo), &key(&hi)),
                        a_uid: lo,
                        b_uid: hi,
                        last_at,
                        last_seq,
                    }
                })
                .collect())
        })
    }

    /// The legacy and system bucket: rows with no recorded sender. Under an
    /// agent or project scope it is recipient-scoped (`recipient_uid` in the
    /// scope), so a system notice reaches its recipient's view, never an
    /// invented sender. Returns `(count, latest seq, latest at)`.
    pub fn work_unknown_summary(&self, scope: &Scope) -> Result<(i64, Option<i64>, Option<f64>)> {
        let mut args = Vec::new();
        let filter = unknown_clause(scope, &mut args);
        self.read_tx(|conn| {
            Ok(conn.query_row(
                &format!("SELECT COUNT(*), MAX(seq), MAX(created) FROM messages WHERE {filter}"),
                rusqlite::params_from_iter(args.iter()),
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
    }

    /// A page of the unknown bucket, the same filter as its summary. The
    /// shared paging rule of [`Self::work_conversation_messages`].
    pub fn work_unknown_messages(
        &self,
        scope: &Scope,
        before: Option<i64>,
        after: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Message>> {
        let mut args = Vec::new();
        let filter = unknown_clause(scope, &mut args);
        self.page(&filter, args, before, after, limit)
    }

    /// The pair's state counts over every message in scope (SQL GROUP BY,
    /// never a capped page), plus the oldest queued message's age.
    pub fn work_conversation_counts(
        &self,
        a_uid: &str,
        b_uid: &str,
        scope: &Scope,
    ) -> Result<PairCounts> {
        let mut args = vec![a_uid.to_string(), b_uid.to_string()];
        let filter = format!(
            "{} AND {}",
            pair_clause(),
            scope_sql(scope, &mut args, false)
        );
        self.read_tx(|conn| {
            let counts: Vec<(String, i64)> = conn.query_vec(
                &format!("SELECT state, COUNT(*) FROM messages WHERE {filter} GROUP BY state"),
                rusqlite::params_from_iter(args.iter()),
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let oldest: Option<Option<f64>> = conn.query_opt(
                &format!("SELECT MIN(created) FROM messages WHERE {filter} AND state='queued'"),
                rusqlite::params_from_iter(args.iter()),
                |r| r.get(0),
            )?;
            Ok((counts, oldest.flatten()))
        })
    }

    /// The pair's newest in-scope message, for the bounded preview.
    pub fn work_conversation_latest(
        &self,
        a_uid: &str,
        b_uid: &str,
        scope: &Scope,
    ) -> Result<Option<Message>> {
        let mut args = vec![a_uid.to_string(), b_uid.to_string()];
        let filter = format!(
            "{} AND {}",
            pair_clause(),
            scope_sql(scope, &mut args, false)
        );
        self.read_tx(|conn| {
            Ok(conn.query_opt(
                &format!("SELECT * FROM messages WHERE {filter} ORDER BY seq DESC LIMIT 1"),
                rusqlite::params_from_iter(args.iter()),
                row_message,
            )?)
        })
    }

    /// Whether a pair holds any in-scope message at all: a pair the scope
    /// excludes (another project, system-only with the toggle off) is not
    /// a row.
    pub fn work_conversation_in_scope(
        &self,
        a_uid: &str,
        b_uid: &str,
        scope: &Scope,
    ) -> Result<bool> {
        let mut args = vec![a_uid.to_string(), b_uid.to_string()];
        let filter = format!(
            "{} AND {}",
            pair_clause(),
            scope_sql(scope, &mut args, false)
        );
        self.read_tx(|conn| {
            Ok(conn
                .query_opt(
                    &format!("SELECT 1 FROM messages WHERE {filter} LIMIT 1"),
                    rusqlite::params_from_iter(args.iter()),
                    |_| Ok(()),
                )?
                .is_some())
        })
    }

    /// Whether the pair holds any in-scope message older than `before`.
    pub fn work_conversation_has_before(
        &self,
        a_uid: &str,
        b_uid: &str,
        before: i64,
        scope: &Scope,
    ) -> Result<bool> {
        let mut args = vec![a_uid.to_string(), b_uid.to_string()];
        let filter = format!(
            "{} AND {}",
            pair_clause(),
            scope_sql(scope, &mut args, false)
        );
        args.push(before.to_string());
        let n = args.len();
        self.read_tx(|conn| {
            Ok(conn
                .query_opt(
                    &format!("SELECT 1 FROM messages WHERE {filter} AND seq<?{n} LIMIT 1"),
                    rusqlite::params_from_iter(args.iter()),
                    |_| Ok(()),
                )?
                .is_some())
        })
    }

    /// A page of one pair's in-scope messages (`seq` ascending in the page).
    pub fn work_conversation_messages(
        &self,
        a_uid: &str,
        b_uid: &str,
        scope: &Scope,
        before: Option<i64>,
        after: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Message>> {
        let mut args = vec![a_uid.to_string(), b_uid.to_string()];
        let filter = format!(
            "{} AND {}",
            pair_clause(),
            scope_sql(scope, &mut args, false)
        );
        self.page(&filter, args, before, after, limit)
    }

    /// Shared paging over a filter: newest-first when reading backwards,
    /// presented ascending. Read-only.
    fn page(
        &self,
        filter: &str,
        mut args: Vec<String>,
        before: Option<i64>,
        after: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Message>> {
        let (cmp, order, cursor) = match (before, after) {
            (Some(b), _) => ("<", "DESC", b),
            (None, Some(a)) => (">", "ASC", a),
            (None, None) => (">", "DESC", 0),
        };
        args.push(cursor.to_string());
        let c = args.len();
        args.push(limit.to_string());
        let l = args.len();
        let sql = format!(
            "SELECT * FROM messages WHERE ({filter}) AND seq {cmp} ?{c} ORDER BY seq {order} LIMIT ?{l}"
        );
        self.read_tx(|conn| {
            let mut rows: Vec<Message> =
                conn.query_vec(&sql, rusqlite::params_from_iter(args.iter()), row_message)?;
            if order == "DESC" {
                rows.reverse();
            }
            Ok(rows)
        })
    }
}

/// The bounded preview of a message body, with the existing secret
/// redaction applied (the list row). Never returns a turn token.
pub fn preview(text: &str) -> Result<String> {
    Ok(super::threads::clean_text(text, 160))
}

/// The full message body for the read: redacted and bounded by the
/// queued-message cap, never a preview truncation. A secret scan that is
/// unavailable answers the existing fail-closed marker.
pub fn body(text: &str) -> String {
    super::threads::clean_text(text, super::messages::ENQUEUE_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::agents::NewAgent;
    use crate::store::Sender;
    use tempfile::TempDir;

    fn store() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let s = Store::open(&dir.path().join("t.sqlite3")).unwrap();
        (dir, s)
    }

    fn reg(s: &Store, alias: &str, cwd: &std::path::Path) {
        s.register_agent(&NewAgent {
            alias,
            provider: "inbox",
            endpoint_kind: "inbox",
            role: "worker",
            cwd: cwd.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    }

    #[test]
    fn sender_is_the_daemon_derived_agent_and_history_survives_removal() {
        let (dir, s) = store();
        reg(&s, "w1", dir.path());
        reg(&s, "w2", dir.path());
        let w1 = s.agent("w1").unwrap().uid;
        let w2 = s.agent("w2").unwrap().uid;

        // The connection says w1 sent it; the body names w2 as a forged
        // claim. Only the daemon-derived Sender is stored.
        s.enqueue_sent(
            "w2",
            "hello",
            None,
            "m1",
            "agent",
            None,
            &Sender::Agent("w1".into()),
        )
        .unwrap();
        let m = s.message("m1").unwrap().unwrap();
        assert_eq!(m.sender_uid.as_deref(), Some(w1.as_str()));
        assert_eq!(m.sender.as_deref(), Some("w1"));

        // A pair listing and read never change queued state (no drain).
        let pairs = s.work_conversation_pairs().unwrap();
        assert_eq!(pairs.len(), 1);
        let page = s
            .work_conversation_messages(&w1, &w2, &Scope::default(), None, None, 10)
            .unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].state, "queued");
        assert_eq!(s.message("m1").unwrap().unwrap().state, "queued");

        // Removing w1 keeps the message; its uid still keys the pair.
        s.remove_agent("w1", false, &serde_json::json!({})).unwrap();
        assert!(s.alias_of_uid(&w1).unwrap().is_none());
        let kept = s
            .work_conversation_messages(&w1, &w2, &Scope::default(), None, None, 10)
            .unwrap();
        assert_eq!(kept.len(), 1, "agent rm keeps history");
    }

    #[test]
    fn topic_subscribers_gate_reads_and_a_retry_never_duplicates_the_post() {
        let (dir, s) = store();
        reg(&s, "pm", dir.path());
        reg(&s, "w1", dir.path());
        reg(&s, "outsider", dir.path());
        let pm = s.agent("pm").unwrap().uid;
        let w1 = s.agent("w1").unwrap().uid;
        let outsider = s.agent("outsider").unwrap().uid;

        let (topic, created) = s.topic_ensure("fleet", "all").unwrap();
        assert!(created);
        assert!(
            !s.topic_ensure("fleet", "all").unwrap().1,
            "idempotent create"
        );
        s.topic_subscribe(&topic.id, &pm).unwrap();
        s.topic_subscribe(&topic.id, &w1).unwrap();

        // Only current subscribers are on the allowlist.
        assert!(s.topic_is_subscriber(&topic.id, &w1).unwrap());
        assert!(!s.topic_is_subscriber(&topic.id, &outsider).unwrap());

        // One transcript row; a retry of the same envelope is not a second row.
        let (post, fresh) = s
            .topic_post_insert(&topic.id, "p1", None, "operator", "hello all")
            .unwrap();
        assert!(fresh);
        let (same, again) = s
            .topic_post_insert(&topic.id, "p1", None, "operator", "hello all")
            .unwrap();
        assert_eq!(same, post);
        assert!(!again);
        assert_eq!(s.topic_posts(&topic.id, None, None, 10).unwrap().len(), 1);
        // A conflicting retry is refused, never a second row.
        assert!(s
            .topic_post_insert(&topic.id, "p1", None, "operator", "different")
            .is_err());

        // Unsubscribe removes the member; the transcript stays intact.
        s.topic_unsubscribe(&topic.id, &w1).unwrap();
        assert!(!s.topic_is_subscriber(&topic.id, &w1).unwrap());
        assert_eq!(s.topic_posts(&topic.id, None, None, 10).unwrap().len(), 1);
    }

    #[test]
    fn the_threads_stamp_moves_on_a_topic_write_and_on_a_delivery_state_change() {
        let (dir, s) = store();
        reg(&s, "w1", dir.path());
        let before = s.threads_stamp().unwrap();
        let (topic, _) = s.topic_ensure("fleet", "all").unwrap();
        let after_topic = s.threads_stamp().unwrap();
        assert!(
            after_topic.0 > before.0,
            "topic create moves the topic stamp"
        );

        s.enqueue_sent("w1", "hi", None, "m1", "agent", None, &Sender::Operator)
            .unwrap();
        let after_enqueue = s.threads_stamp().unwrap();
        assert!(
            after_enqueue.1 > after_topic.1,
            "a new message moves the message stamp"
        );

        // A state transition on an existing row moves the transition stamp
        // even though no new row is inserted.
        let _ = s.inbox_drain("w1", 0);
        let after_state = s.threads_stamp().unwrap();
        assert_eq!(after_state.1, after_enqueue.1, "no new row");
        assert!(after_state.2 >= after_enqueue.2);
        let _ = topic;
    }

    #[test]
    fn a_topic_notice_is_available_without_a_turn_and_a_forged_source_is_refused() {
        let (dir, s) = store();
        reg(&s, "w1", dir.path());
        reg(&s, "w2", dir.path());
        let w1 = s.agent("w1").unwrap().uid;
        let (topic, _) = s.topic_ensure("fleet", "all").unwrap();
        s.topic_subscribe(&topic.id, &w1).unwrap();

        // An ordinary send can never carry the notice source (daemon-only).
        let forged = s.enqueue(
            "w1",
            "x",
            None,
            "forged-1",
            crate::proto::TOPIC_NOTICE_SOURCE,
        );
        assert!(forged.is_err(), "forged topic_notice source is refused");

        // A real publish queues one notice and no turn starts.
        let (post, fresh) = s
            .topic_publish(&topic.id, "p1", None, "operator", "hello")
            .unwrap();
        assert!(fresh);
        let deliveries = s.topic_deliveries(&post).unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].state, "queued");

        // Claiming for w1 consumes the notice in the store: nothing is handed
        // to the actor, and the notice is `available`, never `completed`.
        if let crate::store::Take::Message(m) = s.take_queued("w1").unwrap() {
            panic!("a topic notice reached the actor: {}", m.id);
        }
        let notice = s
            .message(deliveries[0].message.as_deref().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            notice.state, "available",
            "available, not seen or completed"
        );
        assert!(!notice.holds_turn());

        // A retry of the same publish creates no second notice.
        let (_, again) = s
            .topic_publish(&topic.id, "p1", None, "operator", "hello")
            .unwrap();
        assert!(!again);
        assert_eq!(s.topic_deliveries(&post).unwrap().len(), 1);
    }

    #[test]
    fn a_job_transition_fans_a_turnless_notice_to_the_job_topic_once() {
        let (dir, s) = store();
        reg(&s, "pm", dir.path());
        // A job member is a group worker whose `upstream` is the PM.
        s.register_agent(&NewAgent {
            alias: "w1",
            provider: "inbox",
            endpoint_kind: "inbox",
            role: "worker",
            cwd: dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: Some(r#"{"upstream":"pm"}"#),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
        s.create_job(
            "j1",
            None,
            "/s.md",
            &"0".repeat(64),
            "pm",
            None,
            None,
            None,
            2,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        s.create_task("j1", "t1", None, Some("w1"), None, None, None, None, None)
            .unwrap();
        let pm = s.agent("pm").unwrap().uid;
        let w1 = s.agent("w1").unwrap().uid;
        let (topic, _) = s.topic_ensure("job", "j1").unwrap();
        s.topic_subscribe(&topic.id, &pm).unwrap();
        s.topic_subscribe(&topic.id, &w1).unwrap();

        // A real task transition: the PM's job_event is written as before.
        s.job_notice("t1", "started", "d1", "started").unwrap();

        // Each topic member gets exactly one turnless notice, not a turn.
        let posts = s.topic_posts(&topic.id, None, None, 10).unwrap();
        assert_eq!(posts.len(), 1, "one transcript post for the transition");
        let deliveries = s.topic_deliveries(&posts[0].id).unwrap();
        assert_eq!(deliveries.len(), 2, "one delivery per member");
        assert!(deliveries.iter().all(|d| d.state == "queued"));

        // A repeat of the same transition writes no second post or notice.
        s.job_notice("t1", "started", "d1", "started").unwrap();
        assert_eq!(s.topic_posts(&topic.id, None, None, 10).unwrap().len(), 1);
    }
}
