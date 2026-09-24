//! Durable conversation threads (CAD-319).
//!
//! A thread is an agent's conversation with the operator — the master
//! chat — kept in the runtime store so it outlives every provider
//! session: a new, lost or compacted session changes nothing here. One
//! thread per agent alias for now; the thread has its own id so more
//! than one per agent can come later without a schema change. Removing
//! the agent archives its thread (`alias` NULL, `archived_alias` kept,
//! a `system` entry marks it): a new agent under a reused alias starts
//! fresh (CAD-304 S4).
//!
//! Entries are append-only and ordered by `seq` (one global
//! autoincrement, so a cursor is a plain integer):
//!
//! | role       | kind             | written by                                  |
//! |------------|------------------|---------------------------------------------|
//! | `operator` | `message`        | a message the operator queued to the agent  |
//! | `system`   | `message`        | any other queued message (routed result, kickoff, agent peer) |
//! | `agent`    | `assistant_text` | Codex commentary `agentMessage` items and managed Claude text blocks, as they persist |
//! | `agent`    | `tool_call`      | managed Claude `tool_use` (name + redacted summary) |
//! | `agent`    | `tool_result`    | managed Claude `tool_result` (redacted ≤160-char summary + `is_error`) |
//! | `agent`    | `turn_result`    | the turn's final result, any provider       |
//!
//! The final answer is stored once, as the `turn_result`: neither
//! adapter records it as `assistant_text` too (CAD-320).
//!
//! Everything stored is secret-redacted first ([`crate::secret::redact_text`])
//! and bounded, so a thread can never be the reason an export refuses. A
//! turn token is never copied into a payload; one quoted in prose is
//! still caught by the export's generic turn-token redaction.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{now, take_bytes, Store};
use crate::error::{Error, Result};

pub const ROLE_OPERATOR: &str = "operator";
pub const ROLE_AGENT: &str = "agent";
pub const ROLE_SYSTEM: &str = "system";

pub const KIND_MESSAGE: &str = "message";
pub const KIND_ASSISTANT_TEXT: &str = "assistant_text";
pub const KIND_TOOL_CALL: &str = "tool_call";
pub const KIND_TOOL_RESULT: &str = "tool_result";
pub const KIND_TURN_RESULT: &str = "turn_result";

const ROLES: &[&str] = &[ROLE_OPERATOR, ROLE_AGENT, ROLE_SYSTEM];
const KINDS: &[&str] = &[
    KIND_MESSAGE,
    KIND_ASSISTANT_TEXT,
    KIND_TOOL_CALL,
    KIND_TOOL_RESULT,
    KIND_TURN_RESULT,
];

/// Entry text cap. Queued messages are at most 48 000 bytes; a longer
/// provider text is cut with a marker rather than refused.
pub const TEXT_CAP: usize = 64_000;
/// A tool call is stored as a one-line summary, never its full input.
pub const TOOL_SUMMARY_CAP: usize = 160;
/// Largest page one read returns.
pub const PAGE_MAX: i64 = 500;

/// Who queued a message, for its thread entry. Only the daemon decides
/// this, from the connection — never a request field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sender {
    /// A caller tied to no agent. This is the operator by default, not
    /// by positive proof (CAD-313 / CAD-335 phase 2).
    Operator,
    /// The operator's chat (`thread_send`): an [`Sender::Operator`] that
    /// also starts the alias's thread — inside the enqueue transaction,
    /// so a refused message never leaves a thread behind.
    OperatorChat,
    /// A caller the daemon attributed to a registered agent.
    Agent(String),
    /// Internal enqueues (routed results, kickoffs, monitor dispatch)
    /// and callers whose identity was not derived.
    Unattributed,
}

#[derive(Debug, Clone)]
pub struct Thread {
    pub id: String,
    /// The live alias; archived threads are never returned by alias.
    pub alias: String,
    pub created: f64,
    pub updated: f64,
}

impl Thread {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "alias": self.alias,
            "created": self.created,
            "updated": self.updated,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ThreadEntry {
    pub seq: i64,
    pub thread_id: String,
    pub role: String,
    pub kind: String,
    pub text: String,
    pub payload: Option<Value>,
    pub message_id: Option<String>,
    pub created: f64,
}

impl ThreadEntry {
    pub fn to_json(&self) -> Value {
        json!({
            "seq": self.seq,
            "thread": self.thread_id,
            "role": self.role,
            "kind": self.kind,
            "text": self.text,
            "payload": self.payload,
            "message": self.message_id,
            "created": self.created,
        })
    }
}

/// Provider text held back until its message finishes: kept only if
/// the turn result does not carry it (CAD-320).
#[derive(Debug, Clone)]
pub struct HeldText {
    text: String,
    payload: Value,
}

/// One entry to append. `text` and every string in `payload` are
/// redacted and bounded on the way in.
#[derive(Debug, Clone)]
pub struct NewEntry<'a> {
    pub role: &'a str,
    pub kind: &'a str,
    pub text: &'a str,
    pub payload: Option<Value>,
    pub message_id: Option<&'a str>,
}

fn row_thread(row: &rusqlite::Row) -> rusqlite::Result<Thread> {
    Ok(Thread {
        id: row.get("id")?,
        alias: row.get("alias")?,
        created: row.get("created")?,
        updated: row.get("updated")?,
    })
}

fn row_entry(row: &rusqlite::Row) -> rusqlite::Result<ThreadEntry> {
    let payload: Option<String> = row.get("payload")?;
    Ok(ThreadEntry {
        seq: row.get("seq")?,
        thread_id: row.get("thread_id")?,
        role: row.get("role")?,
        kind: row.get("kind")?,
        text: row.get("text")?,
        payload: payload.and_then(|p| serde_json::from_str(&p).ok()),
        message_id: row.get("message_id")?,
        created: row.get("created")?,
    })
}

/// The v13 schema objects. `IF NOT EXISTS` so a half-applied migration
/// converges on reopen.
pub(super) const SCHEMA_V13: &str = "CREATE TABLE IF NOT EXISTS threads(
        id TEXT PRIMARY KEY,
        alias TEXT UNIQUE,
        archived_alias TEXT,
        created REAL NOT NULL,
        updated REAL NOT NULL);
     CREATE TABLE IF NOT EXISTS thread_entries(
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        thread_id TEXT NOT NULL REFERENCES threads(id),
        role TEXT NOT NULL,
        kind TEXT NOT NULL,
        text TEXT NOT NULL,
        payload TEXT,
        message_id TEXT,
        created REAL NOT NULL);
     CREATE INDEX IF NOT EXISTS thread_entries_thread ON thread_entries(thread_id, seq);";

/// Redact then bound one stored text. A scan that cannot run withholds
/// the text: the entry still lands, the value never does.
pub fn clean_text(text: &str, cap: usize) -> String {
    let redacted = match crate::secret::redact_text(text) {
        Ok(redacted) => redacted,
        Err(_) => return "[withheld: secret scan unavailable]".to_string(),
    };
    if redacted.len() <= cap {
        return redacted;
    }
    let mut out = take_bytes(&redacted, cap.saturating_sub(16));
    out.push_str(" …[truncated]");
    out
}

/// [`clean_text`] over every string in a payload.
fn clean_payload(value: Value) -> Value {
    match value {
        Value::String(s) => Value::String(clean_text(&s, TEXT_CAP)),
        Value::Array(items) => Value::Array(items.into_iter().map(clean_payload).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, clean_payload(v)))
                .collect::<Map<_, _>>(),
        ),
        other => other,
    }
}

/// A one-line, redacted summary of a tool call's input — what a thread
/// stores instead of the input itself. The first well-known field wins
/// (`command`, `file_path`, `path`, `pattern`, `url`, `query`,
/// `description`, `prompt`); otherwise the sorted key names, never
/// their values. Commands go through the CAD-108 argv scrubber, then
/// everything through the secret scan.
pub fn tool_summary(name: &str, input: &Value) -> String {
    const FIELDS: &[&str] = &[
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "query",
        "description",
        "prompt",
    ];
    let detail = FIELDS
        .iter()
        .find_map(|field| {
            input
                .get(*field)
                .and_then(Value::as_str)
                .map(|value| (*field, value))
        })
        .map(|(field, value)| {
            let flat: String = value
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            if field == "command" {
                crate::doctor::host::redact_argv(&[flat.as_str()])
            } else {
                flat
            }
        })
        .or_else(|| {
            input.as_object().filter(|o| !o.is_empty()).map(|o| {
                let mut keys: Vec<&str> = o.keys().map(String::as_str).collect();
                keys.sort_unstable();
                format!("{{{}}}", keys.join(", "))
            })
        });
    let line = match detail {
        Some(detail) => format!("{name}: {detail}"),
        None => name.to_string(),
    };
    clean_text(&line, TOOL_SUMMARY_CAP)
}

/// A one-line, redacted summary of a tool's output — what a thread
/// stores instead of the output itself (CAD-320). `output` is the
/// provider's `tool_result.content`: a string, or blocks whose `text` is
/// joined and whose other types are only named (`[image]`). At most
/// [`TEXT_CAP`] bytes are scanned, cut back to whitespace so no partial
/// secret escapes the scan; the scan runs before flattening, so
/// multi-line rules (private keys) still match.
pub fn tool_result_summary(output: &Value) -> String {
    let text = match output {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => b
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                Some(other) => format!("[{other}]"),
                None => String::new(),
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    };
    let mut head = take_bytes(&text, TEXT_CAP);
    if head.len() < text.len() {
        let cut = head.rfind(char::is_whitespace).unwrap_or(0);
        head.truncate(cut);
    }
    let redacted = clean_text(&head, TEXT_CAP);
    let flat = redacted
        .replace(|c: char| c.is_control(), " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    // One byte over the cap keeps `clean_text`'s truncation marker.
    clean_text(&take_bytes(&flat, TOOL_SUMMARY_CAP + 1), TOOL_SUMMARY_CAP)
}

/// The thread entry for a message queued to a threaded agent.
fn message_entry<'a>(sender: &Sender, source: &str, body: &'a str, id: &'a str) -> NewEntry<'a> {
    let (role, payload) = match sender {
        Sender::Operator | Sender::OperatorChat => (ROLE_OPERATOR, json!({"source": source})),
        Sender::Agent(alias) => (ROLE_SYSTEM, json!({"source": source, "from": alias})),
        Sender::Unattributed => (ROLE_SYSTEM, json!({"source": source})),
    };
    NewEntry {
        role,
        kind: KIND_MESSAGE,
        text: body,
        payload: Some(payload),
        message_id: Some(id),
    }
}

impl Store {
    /// The alias's thread, created on first use. The agent must exist.
    pub fn ensure_thread(&self, alias: &str) -> Result<Thread> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        self.agent_in(&tx, alias)?;
        let thread = Self::ensure_thread_in(&tx, alias)?;
        tx.commit()?;
        Ok(thread)
    }

    /// [`Self::ensure_thread`] inside the caller's transaction; the
    /// caller has already proved the agent exists.
    fn ensure_thread_in(tx: &Connection, alias: &str) -> Result<Thread> {
        if let Some(thread) = Self::thread_in(tx, alias)? {
            return Ok(thread);
        }
        let at = now();
        let id = Uuid::new_v4().simple().to_string();
        tx.execute(
            "INSERT INTO threads(id,alias,created,updated) VALUES(?,?,?,?)",
            params![id, alias, at, at],
        )?;
        Self::event(tx, alias, "thread_created", json!({"thread": id}))?;
        Ok(Thread {
            id,
            alias: alias.to_string(),
            created: at,
            updated: at,
        })
    }

    /// Detach the alias's thread when its agent row is removed (CAD-304
    /// S4: a reused alias starts fresh). The rows stay, keyed by thread
    /// id; the alias moves to `archived_alias` and a `system` entry marks
    /// the removal. A later registration under the alias gets a new
    /// thread.
    pub(super) fn thread_detach_in(tx: &Connection, alias: &str) -> Result<()> {
        let Some(thread) = Self::thread_in(tx, alias)? else {
            return Ok(());
        };
        Self::thread_append_in(
            tx,
            alias,
            NewEntry {
                role: ROLE_SYSTEM,
                kind: KIND_MESSAGE,
                text: &format!("agent '{alias}' was removed; this thread is archived"),
                payload: Some(json!({"event": "agent_removed"})),
                message_id: None,
            },
        )?;
        // No event under the alias: the alias's events were just pruned,
        // and a new registration must not inherit one.
        tx.execute(
            "UPDATE threads SET alias=NULL, archived_alias=? WHERE id=?",
            params![alias, thread.id],
        )?;
        Ok(())
    }

    /// The alias's thread, if one was ever started.
    pub fn thread(&self, alias: &str) -> Result<Option<Thread>> {
        let conn = self.conn();
        Self::thread_in(&conn, alias)
    }

    fn thread_in(conn: &Connection, alias: &str) -> Result<Option<Thread>> {
        Ok(conn
            .query_row("SELECT * FROM threads WHERE alias=?", [alias], row_thread)
            .optional()?)
    }

    /// Append to the alias's thread. `Ok(None)` when it has none —
    /// agents without a chat are untouched.
    pub fn thread_append(&self, alias: &str, entry: NewEntry) -> Result<Option<i64>> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let seq = Self::thread_append_in(&tx, alias, entry)?;
        tx.commit()?;
        Ok(seq)
    }

    /// [`Self::thread_append`] for managed provider output: the entry
    /// links to the alias's running message, if any.
    pub fn thread_append_running(
        &self,
        alias: &str,
        role: &str,
        kind: &str,
        text: &str,
        payload: Option<Value>,
    ) -> Result<Option<i64>> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        if Self::thread_in(&tx, alias)?.is_none() {
            return Ok(None);
        }
        let running = Self::running_message_in(&tx, alias)?;
        let seq = Self::thread_append_in(
            &tx,
            alias,
            NewEntry {
                role,
                kind,
                text,
                payload,
                message_id: running.as_deref(),
            },
        )?;
        tx.commit()?;
        Ok(seq)
    }

    /// Agent text that the turn result may repeat — Codex `final_answer`
    /// and unphased `agentMessage` items (CAD-320). Held in memory
    /// against the alias's running message; its finish appends each
    /// held text the result does not contain as `assistant_text`, ahead
    /// of the `turn_result` and in the same transaction. So an `unknown`
    /// or failed turn, or a result built from other items, loses none of
    /// it, and a result that carries it never shows it twice. With no
    /// running message there is nothing to dedupe against: it is
    /// appended now. A daemon restart drops what is held — the provider
    /// transcript still has it.
    pub fn thread_hold_running(&self, alias: &str, text: &str, payload: Value) -> Result<()> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        if Self::thread_in(&tx, alias)?.is_none() {
            return Ok(());
        }
        let Some(running) = Self::running_message_in(&tx, alias)? else {
            Self::thread_append_in(
                &tx,
                alias,
                NewEntry {
                    role: ROLE_AGENT,
                    kind: KIND_ASSISTANT_TEXT,
                    text,
                    payload: Some(payload),
                    message_id: None,
                },
            )?;
            tx.commit()?;
            return Ok(());
        };
        self.thread_held
            .lock()
            .unwrap()
            .entry(running)
            .or_default()
            .push(HeldText {
                text: text.to_string(),
                payload,
            });
        Ok(())
    }

    /// The alias's in-flight turn. `submitting` counts: a provider can
    /// persist items before `on_started` marks the message `running`
    /// (Codex emits them right behind the `turn/start` reply).
    fn running_message_in(tx: &Connection, alias: &str) -> Result<Option<String>> {
        Ok(tx
            .query_row(
                "SELECT id FROM messages WHERE alias=?
                 AND state IN ('submitting','running') AND source != 'nudge'
                 ORDER BY seq DESC LIMIT 1",
                [alias],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Transactional append — used inside enqueue and finish so an entry
    /// lands with the state change it records, or not at all.
    pub(super) fn thread_append_in(
        tx: &Connection,
        alias: &str,
        entry: NewEntry,
    ) -> Result<Option<i64>> {
        if !ROLES.contains(&entry.role) {
            return Err(Error::internal(format!(
                "unknown thread role '{}'",
                entry.role
            )));
        }
        if !KINDS.contains(&entry.kind) {
            return Err(Error::internal(format!(
                "unknown thread entry kind '{}'",
                entry.kind
            )));
        }
        let Some(thread) = Self::thread_in(tx, alias)? else {
            return Ok(None);
        };
        let text = clean_text(entry.text, TEXT_CAP);
        let payload = entry.payload.map(|p| clean_payload(p).to_string());
        let at = now();
        tx.execute(
            "INSERT INTO thread_entries(thread_id,role,kind,text,payload,message_id,created)
             VALUES(?,?,?,?,?,?,?)",
            params![
                thread.id,
                entry.role,
                entry.kind,
                text,
                payload,
                entry.message_id,
                at
            ],
        )?;
        let seq = tx.last_insert_rowid();
        tx.execute(
            "UPDATE threads SET updated=? WHERE id=?",
            params![at, thread.id],
        )?;
        Ok(Some(seq))
    }

    /// The operator/system entry for a freshly queued message.
    pub(super) fn thread_note_enqueued(
        tx: &Connection,
        alias: &str,
        sender: &Sender,
        source: &str,
        body: &str,
        id: &str,
    ) -> Result<()> {
        if *sender == Sender::OperatorChat {
            Self::ensure_thread_in(tx, alias)?;
        }
        Self::thread_append_in(tx, alias, message_entry(sender, source, body, id))?;
        Ok(())
    }

    /// The `turn_result` entry for a finished message. The payload is
    /// built from named fields only — the stored result carries the
    /// turn token, which must never be copied here. Text held for the
    /// message ([`Self::thread_hold_running`]) that the result does not
    /// carry lands first, as `assistant_text`.
    pub(super) fn thread_note_finished(
        &self,
        tx: &Connection,
        message: &super::Message,
        status: &str,
        result: &Value,
        error: Option<&str>,
    ) -> Result<()> {
        let text = result.get("text").and_then(Value::as_str).unwrap_or("");
        let held = self
            .thread_held
            .lock()
            .unwrap()
            .remove(&message.id)
            .unwrap_or_default();
        for item in held {
            let body = item.text.trim();
            if body.is_empty() || text.contains(body) {
                continue;
            }
            Self::thread_append_in(
                tx,
                &message.alias,
                NewEntry {
                    role: ROLE_AGENT,
                    kind: KIND_ASSISTANT_TEXT,
                    text: &item.text,
                    payload: Some(item.payload),
                    message_id: Some(&message.id),
                },
            )?;
        }
        let mut payload = json!({"status": status});
        if let Some(error) = error.filter(|e| !e.is_empty()) {
            payload["error"] = json!(error);
        }
        if let Some(stop) = result.get("stop_reason").and_then(Value::as_str) {
            payload["stop_reason"] = json!(stop);
        }
        Self::thread_append_in(
            tx,
            &message.alias,
            NewEntry {
                role: ROLE_AGENT,
                kind: KIND_TURN_RESULT,
                text,
                payload: Some(payload),
                message_id: Some(&message.id),
            },
        )?;
        Ok(())
    }

    /// Entries after `after`, oldest first, at most `limit` (bounded by
    /// [`PAGE_MAX`]). An alias with no thread reads empty.
    pub fn thread_entries(&self, alias: &str, after: i64, limit: i64) -> Result<Vec<ThreadEntry>> {
        let conn = self.conn();
        let Some(thread) = Self::thread_in(&conn, alias)? else {
            return Ok(Vec::new());
        };
        let limit = limit.clamp(1, PAGE_MAX);
        let mut stmt = conn.prepare(
            "SELECT * FROM thread_entries WHERE thread_id=? AND seq>?
             ORDER BY seq LIMIT ?",
        )?;
        let rows = stmt
            .query_map(params![thread.id, after, limit], row_entry)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The newest `limit` entries with `seq < before` (`None`: the
    /// newest of all), oldest first, and whether older entries remain —
    /// a chat view opens on its latest page and pages backwards
    /// (CAD-328) instead of replaying the whole thread from the start.
    pub fn thread_entries_before(
        &self,
        alias: &str,
        before: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<ThreadEntry>, bool)> {
        let conn = self.conn();
        let Some(thread) = Self::thread_in(&conn, alias)? else {
            return Ok((Vec::new(), false));
        };
        let limit = limit.clamp(1, PAGE_MAX);
        let mut stmt = conn.prepare(
            "SELECT * FROM thread_entries WHERE thread_id=? AND seq<?
             ORDER BY seq DESC LIMIT ?",
        )?;
        let mut rows = stmt
            .query_map(
                params![thread.id, before.unwrap_or(i64::MAX), limit + 1],
                row_entry,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows.len() as i64 > limit;
        rows.truncate(limit as usize);
        rows.reverse();
        Ok((rows, more))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use rusqlite::Connection;
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;

    use super::*;
    use crate::store::NewAgent;

    /// Seeded alphanumeric noise — secret fixtures are built at runtime
    /// so no literal in this file has a credential shape.
    fn noise(seed: &str, n: usize) -> String {
        const ALPHANUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        let mut out = String::new();
        let mut counter = 0u32;
        while out.len() < n {
            for b in Sha256::digest(format!("{seed}:{counter}").as_bytes()) {
                if out.len() < n {
                    out.push(ALPHANUM[b as usize % ALPHANUM.len()] as char);
                }
            }
            counter += 1;
        }
        out
    }

    /// A GitHub classic PAT shape: `ghp_` plus 36 characters.
    fn github_token(seed: &str) -> String {
        [&["gh", "p_"].concat(), noise(seed, 36).as_str()].concat()
    }

    fn store() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let s = Store::open(&dir.path().join("t.sqlite3")).unwrap();
        (dir, s)
    }

    fn reg(s: &Store, alias: &str, cwd: &Path) {
        s.register_agent(&NewAgent {
            alias,
            provider: "fake",
            endpoint_kind: "managed",
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

    fn version(db: &Path) -> i64 {
        Connection::open(db)
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap()
    }

    fn has_table(db: &Path, name: &str) -> bool {
        Connection::open(db)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?",
                [name],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            == 1
    }

    #[test]
    fn migration_v12_to_v13_adds_threads_and_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("t.sqlite3");
        {
            let s = Store::open(&db).unwrap();
            reg(&s, "m1", dir.path());
            s.enqueue("m1", "before the migration", None, "x1", "user")
                .unwrap();
        }
        // A genuine v12: no thread objects.
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "DROP TABLE thread_entries; DROP TABLE threads;
                 UPDATE schema_version SET version=12;",
            )
            .unwrap();
        assert!(!has_table(&db, "threads"));
        {
            let s = Store::open_for_schema_tests(&db).unwrap();
            assert_eq!(
                s.message("x1").unwrap().unwrap().body,
                "before the migration"
            );
            s.ensure_thread("m1").unwrap();
            s.enqueue("m1", "after", None, "x2", "user").unwrap();
            assert_eq!(s.thread_entries("m1", 0, 10).unwrap().len(), 1);
        }
        assert_eq!(version(&db), 13);
        assert!(has_table(&db, "threads") && has_table(&db, "thread_entries"));
        // Half-applied: objects present, version rolled back — converges
        // and keeps the rows.
        Connection::open(&db)
            .unwrap()
            .execute("UPDATE schema_version SET version=12", [])
            .unwrap();
        {
            let s = Store::open_for_schema_tests(&db).unwrap();
            assert_eq!(s.thread_entries("m1", 0, 10).unwrap().len(), 1);
        }
        assert_eq!(version(&db), 13);
        // Reopening a current store is a no-op.
        let s = Store::open(&db).unwrap();
        assert_eq!(s.thread_entries("m1", 0, 10).unwrap().len(), 1);
        assert_eq!(version(&db), crate::rollout::SCHEMA_VERSION);
    }

    #[test]
    fn enqueue_records_the_sender_only_for_a_threaded_agent() {
        let (dir, s) = store();
        reg(&s, "master", dir.path());
        reg(&s, "w1", dir.path());
        // No thread yet: nothing is recorded, and reads are empty.
        s.enqueue("master", "early", None, "e0", "user").unwrap();
        assert!(s.thread("master").unwrap().is_none());
        assert!(s.thread_entries("master", 0, 10).unwrap().is_empty());

        let thread = s.ensure_thread("master").unwrap();
        assert_eq!(
            s.ensure_thread("master").unwrap().id,
            thread.id,
            "one per alias"
        );
        s.enqueue_sent(
            "master",
            "hello master",
            None,
            "e1",
            "operator",
            None,
            &Sender::Operator,
        )
        .unwrap();
        // A retry of the same envelope is a duplicate — no second entry.
        s.enqueue_sent(
            "master",
            "hello master",
            None,
            "e1",
            "operator",
            None,
            &Sender::Operator,
        )
        .unwrap();
        s.enqueue_sent(
            "master",
            "from a peer",
            None,
            "e2",
            "user",
            None,
            &Sender::Agent("w1".into()),
        )
        .unwrap();
        s.enqueue("master", "internal", None, "e3", "worker_result")
            .unwrap();
        // Other agents stay untouched.
        s.enqueue("w1", "not threaded", None, "e4", "user").unwrap();

        let entries = s.thread_entries("master", 0, 10).unwrap();
        let shape: Vec<(&str, &str, &str, Option<&str>)> = entries
            .iter()
            .map(|e| {
                (
                    e.role.as_str(),
                    e.kind.as_str(),
                    e.text.as_str(),
                    e.message_id.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                ("operator", "message", "hello master", Some("e1")),
                ("system", "message", "from a peer", Some("e2")),
                ("system", "message", "internal", Some("e3")),
            ]
        );
        assert_eq!(entries[1].payload.as_ref().unwrap()["from"], "w1");
        assert_eq!(
            entries[2].payload.as_ref().unwrap()["source"],
            "worker_result"
        );
        assert!(s.thread_entries("w1", 0, 10).unwrap().is_empty());
        assert!(s.thread("w1").unwrap().is_none());
    }

    #[test]
    fn finish_records_the_turn_result_without_the_turn_token() {
        let (dir, s) = store();
        reg(&s, "master", dir.path());
        s.ensure_thread("master").unwrap();
        s.enqueue("master", "do it", None, "m1", "user").unwrap();
        let token = format!("claude-{}-{}", noise("gen", 32), noise("id", 32));
        s.mark_running("m1", &token).unwrap();
        s.thread_append_running("master", ROLE_AGENT, KIND_TOOL_CALL, "Bash: ls", None)
            .unwrap();
        let message = s.message("m1").unwrap().unwrap();
        s.finish(
            &message,
            "completed",
            &json!({"turn_id": token, "status": "completed", "text": "all done",
                    "stop_reason": "end_turn", "error": null}),
            None,
        )
        .unwrap();
        let entries = s.thread_entries("master", 0, 10).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[1].kind, "tool_call");
        assert_eq!(
            entries[1].message_id.as_deref(),
            Some("m1"),
            "linked to the running turn"
        );
        let result = &entries[2];
        assert_eq!(
            (result.role.as_str(), result.kind.as_str()),
            ("agent", "turn_result")
        );
        assert_eq!(result.text, "all done");
        assert_eq!(result.message_id.as_deref(), Some("m1"));
        assert_eq!(
            result.payload,
            Some(json!({"status": "completed", "stop_reason": "end_turn"}))
        );
        for e in &entries {
            assert!(!e.to_json().to_string().contains(&token), "{e:?}");
        }
    }

    /// A provider item that lands between `take_queued` (`submitting`)
    /// and `mark_running` still links to its turn (the CI race behind
    /// `cad319_thread_records_codex_agent_messages`).
    #[test]
    fn running_append_links_a_submitting_turn() {
        let (dir, s) = store();
        reg(&s, "master", dir.path());
        s.ensure_thread("master").unwrap();
        s.enqueue("master", "do it", None, "m1", "user").unwrap();
        assert!(matches!(
            s.take_queued("master").unwrap(),
            crate::store::Take::Message(_)
        ));
        assert_eq!(s.message("m1").unwrap().unwrap().state, "submitting");
        s.thread_append_running("master", ROLE_AGENT, KIND_ASSISTANT_TEXT, "looking", None)
            .unwrap();
        let entries = s.thread_entries("master", 0, 10).unwrap();
        assert_eq!(entries[1].message_id.as_deref(), Some("m1"), "{entries:?}");
    }

    #[test]
    fn entries_page_by_after_and_limit() {
        let (dir, s) = store();
        reg(&s, "master", dir.path());
        s.ensure_thread("master").unwrap();
        for i in 0..7 {
            s.thread_append(
                "master",
                NewEntry {
                    role: ROLE_AGENT,
                    kind: KIND_ASSISTANT_TEXT,
                    text: &format!("line {i}"),
                    payload: None,
                    message_id: None,
                },
            )
            .unwrap()
            .unwrap();
        }
        let first = s.thread_entries("master", 0, 3).unwrap();
        assert_eq!(
            first.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            ["line 0", "line 1", "line 2"]
        );
        let next = s
            .thread_entries("master", first.last().unwrap().seq, 3)
            .unwrap();
        assert_eq!(
            next.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            ["line 3", "line 4", "line 5"]
        );
        let rest = s
            .thread_entries("master", next.last().unwrap().seq, 3)
            .unwrap();
        assert_eq!(rest.len(), 1);
        assert!(s
            .thread_entries("master", rest[0].seq, 3)
            .unwrap()
            .is_empty());
        // Limits clamp to the page bounds.
        assert_eq!(s.thread_entries("master", 0, 0).unwrap().len(), 1);
        assert_eq!(s.thread_entries("master", 0, 10_000).unwrap().len(), 7);
        // Unknown roles and kinds are refused, never stored.
        for (role, kind) in [("boss", KIND_MESSAGE), (ROLE_AGENT, "shout")] {
            assert!(s
                .thread_append(
                    "master",
                    NewEntry {
                        role,
                        kind,
                        text: "x",
                        payload: None,
                        message_id: None,
                    },
                )
                .is_err());
        }
    }

    /// CAD-304 S4 applied to chats: removing an agent archives its
    /// thread (rows kept by thread id, a `system` entry marks it); a new
    /// agent under the reused alias starts a fresh thread and inherits
    /// nothing. The timer gc path archives the same way.
    #[test]
    fn a_removed_alias_archives_its_thread_and_a_reuse_starts_fresh() {
        let (dir, s) = store();
        reg(&s, "master", dir.path());
        let old = s.ensure_thread("master").unwrap();
        s.enqueue("master", "remember me", None, "m1", "user")
            .unwrap();
        let m = s.message("m1").unwrap().unwrap();
        s.finish(&m, "completed", &json!({"text": "ok"}), None)
            .unwrap();
        s.set_agent_state("master", "stopped", None).unwrap();
        s.remove_agent(
            "master",
            false,
            &json!({"by": "operator", "by_kind": "operator"}),
        )
        .unwrap();
        assert!(s.thread("master").unwrap().is_none());
        assert!(s.thread_entries("master", 0, 10).unwrap().is_empty());
        // The archived rows stay under the old thread id.
        let (archived_alias, alias): (Option<String>, Option<String>) = s
            .conn()
            .query_row(
                "SELECT archived_alias, alias FROM threads WHERE id=?",
                [&old.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(archived_alias.as_deref(), Some("master"));
        assert_eq!(alias, None);
        let kept: Vec<(String, String)> = s
            .conn()
            .prepare("SELECT role, text FROM thread_entries WHERE thread_id=? ORDER BY seq")
            .unwrap()
            .query_map([&old.id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(kept.len(), 3, "{kept:?}");
        assert_eq!(kept[2].0, "system");
        assert!(kept[2].1.contains("removed"), "{kept:?}");
        let left: i64 = s
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE alias='master' AND job_id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0, "no event is left for a reused alias to inherit");

        // A new agent under the same alias: no thread until one starts,
        // and then a different one with none of the old entries.
        reg(&s, "master", dir.path());
        s.enqueue("master", "unthreaded", None, "m2", "user")
            .unwrap();
        assert!(s.thread("master").unwrap().is_none());
        let fresh = s.ensure_thread("master").unwrap();
        assert_ne!(fresh.id, old.id);
        s.enqueue("master", "hello again", None, "m3", "user")
            .unwrap();
        let entries = s.thread_entries("master", 0, 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "hello again");

        // Removing again archives the second thread beside the first.
        for id in ["m2", "m3"] {
            let m = s.message(id).unwrap().unwrap();
            s.finish(&m, "completed", &json!({"text": "ok"}), None)
                .unwrap();
        }
        s.set_agent_state("master", "stopped", None).unwrap();
        s.remove_agent(
            "master",
            false,
            &json!({"by": "operator", "by_kind": "operator"}),
        )
        .unwrap();
        let archived: i64 = s
            .conn()
            .query_row(
                "SELECT count(*) FROM threads WHERE archived_alias='master' AND alias IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(archived, 2);
    }

    /// A refused chat message starts no thread: the thread is created
    /// inside the enqueue transaction, after validation.
    #[test]
    fn a_refused_operator_chat_message_leaves_no_thread() {
        let (dir, s) = store();
        reg(&s, "master", dir.path());
        let too_long = "x".repeat(48_001);
        for body in ["", too_long.as_str()] {
            assert!(s
                .enqueue_sent(
                    "master",
                    body,
                    None,
                    "c1",
                    "operator",
                    None,
                    &Sender::OperatorChat,
                )
                .is_err());
        }
        assert!(s.thread("master").unwrap().is_none());
        assert!(s
            .events("master", 0, 100)
            .unwrap()
            .iter()
            .all(|e| e.kind != "thread_created"));
        s.enqueue_sent(
            "master",
            "ok now",
            None,
            "c2",
            "operator",
            None,
            &Sender::OperatorChat,
        )
        .unwrap();
        let entries = s.thread_entries("master", 0, 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].role, "operator");
    }

    #[test]
    fn tool_input_secrets_never_reach_the_summary_or_the_store() {
        let token = github_token("thread-tool");
        let plain = noise("plain-password", 12);
        let cmd = format!("curl -H 'Authorization: token {token}' --password {plain} https://x");
        let summary = tool_summary("Bash", &json!({"command": cmd}));
        assert!(summary.starts_with("Bash: curl"), "{summary}");
        assert!(!summary.contains(&token), "{summary}");
        assert!(
            !summary.contains(&plain),
            "argv rule redacts a secret flag: {summary}"
        );
        assert!(summary.len() <= TOOL_SUMMARY_CAP, "{summary}");

        // A field outside the known ones is named, never valued.
        let other = tool_summary("Mystery", &json!({"b": token, "a": 1}));
        assert_eq!(other, "Mystery: {a, b}");
        assert_eq!(tool_summary("Noop", &Value::Null), "Noop");
        let path = tool_summary("Read", &json!({"file_path": "/repo/src/lib.rs"}));
        assert_eq!(path, "Read: /repo/src/lib.rs");
        // Control characters cannot break the one-line summary.
        assert_eq!(
            tool_summary("Bash", &json!({"command": "echo a\necho b"})),
            "Bash: echo a echo b"
        );

        // Whatever an adapter hands over, the store redacts again: text
        // and every payload string.
        let (dir, s) = store();
        reg(&s, "master", dir.path());
        s.ensure_thread("master").unwrap();
        s.thread_append_running(
            "master",
            ROLE_AGENT,
            KIND_TOOL_CALL,
            &format!("Bash: echo {token}"),
            Some(json!({"tool": "Bash", "nested": [{"raw": token}]})),
        )
        .unwrap();
        s.enqueue("master", &format!("pasted {token}"), None, "m1", "user")
            .unwrap();
        let raw: Vec<String> = s
            .conn()
            .prepare("SELECT text || coalesce(payload,'') FROM thread_entries")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(raw.len(), 2);
        for cell in &raw {
            assert!(!cell.contains(&token), "{cell}");
            assert!(cell.contains("[redacted:"), "{cell}");
        }
    }

    /// CAD-410: a private key whose END marker never reaches the scan —
    /// a head-limited read, or a body cut at the [`TEXT_CAP`] scan limit
    /// the way a tool-result summary cuts it — keeps none of its body
    /// in a summary or a stored entry.
    #[test]
    fn a_private_key_cut_before_its_end_never_reaches_the_store() {
        let header = ["-----BEGIN RSA ", "PRIVATE", " KEY-----"].concat();
        let footer = ["-----END RSA ", "PRIVATE", " KEY-----"].concat();
        let body: Vec<String> = (0..TEXT_CAP / 64 + 50)
            .map(|i| noise(&format!("thread-pem:{i}"), 64))
            .collect();
        let whole = format!("{header}\n{}\n{footer}\n", body.join("\n"));
        assert!(whole.len() > TEXT_CAP);
        let mut head = take_bytes(&whole, TEXT_CAP);
        head.truncate(head.rfind(char::is_whitespace).unwrap());
        assert!(!head.contains(&footer), "the END marker is past the cut");
        let leaked = |cell: &str| {
            body.iter()
                .find(|line| cell.contains(line.as_str()))
                .cloned()
        };

        let cleaned = clean_text(&head, TEXT_CAP);
        assert_eq!(cleaned, "[redacted:private-key]");
        let first = &head[..head.find('\n').unwrap() + 3 * 65];
        let summary = tool_summary("Bash", &json!({"command": format!("printf '{first}'")}));
        assert!(summary.starts_with("Bash: printf "), "{summary}");
        assert!(summary.ends_with("[redacted:private-key]"), "{summary}");
        assert_eq!(leaked(&summary), None, "{summary}");

        let (dir, s) = store();
        reg(&s, "master", dir.path());
        s.ensure_thread("master").unwrap();
        s.thread_append_running(
            "master",
            ROLE_AGENT,
            KIND_ASSISTANT_TEXT,
            &head,
            Some(json!({"raw": first})),
        )
        .unwrap();
        s.enqueue("master", first, None, "m1", "user").unwrap();
        let raw: Vec<String> = s
            .conn()
            .prepare("SELECT text || coalesce(payload,'') FROM thread_entries")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(raw.len(), 2);
        for cell in &raw {
            assert_eq!(leaked(cell), None, "key body stored");
            assert!(cell.contains("[redacted:private-key]"), "{cell}");
        }
    }

    #[test]
    fn tool_output_is_a_redacted_one_line_summary() {
        let token = github_token("thread-tool-output");
        let summary = tool_result_summary(&json!(format!("TOKEN={token}\nok\u{1b}[0m done")));
        assert!(summary.starts_with("TOKEN=[redacted:"), "{summary}");
        assert!(!summary.contains(&token), "{summary}");
        assert!(summary.ends_with("ok [0m done"), "one line: {summary}");

        // Blocks: text joined, other types only named.
        let blocks = json!([{"type": "text", "text": "a"}, {"type": "image", "source": {}}]);
        assert_eq!(tool_result_summary(&blocks), "a [image]");
        assert_eq!(tool_result_summary(&Value::Null), "");

        // Long output: bounded with the marker; the tail never survives.
        let long = format!("{}TAIL", "y ".repeat(400));
        let summary = tool_result_summary(&json!(long));
        assert!(summary.len() <= TOOL_SUMMARY_CAP, "{}", summary.len());
        assert!(summary.ends_with("…[truncated]"), "{summary}");
        assert!(!summary.contains("TAIL"), "{summary}");
    }

    #[test]
    fn long_text_is_bounded_with_a_marker() {
        let long = "a".repeat(TEXT_CAP + 500);
        let cleaned = clean_text(&long, TEXT_CAP);
        assert!(cleaned.len() <= TEXT_CAP, "{}", cleaned.len());
        assert!(cleaned.ends_with("…[truncated]"));
    }
}
