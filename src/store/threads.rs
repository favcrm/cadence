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

use super::StoreConn;
use super::{now, take_bytes, Store};
use crate::error::{Error, Result};

mod model;
pub use self::model::*;

fn row_thread(row: &rusqlite::Row) -> rusqlite::Result<Thread> {
    Ok(Thread {
        id: row.get("id")?,
        alias: row
            .get::<_, Option<String>>("alias")?
            .or(row.get::<_, Option<String>>("archived_alias")?)
            .unwrap_or_default(),
        created: row.get("created")?,
        updated: row.get("updated")?,
        install_id: row.get("install_id")?,
        context_id: row.get("context_id")?,
        subject: row.get("subject")?,
        is_general: row.get::<_, i64>("is_general")? != 0,
        archived: row.get::<_, i64>("archived")? != 0,
        title: row.get("title")?,
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

/// CAD-1098 v32: the conversation columns and partial unique indexes.
/// Runs inside the caller's transaction. A store whose `threads` already
/// has `install_id` (a half-applied earlier attempt) skips the rebuild
/// and only (re)creates the indexes.
pub(super) fn migrate_v32(tx: &Connection) -> Result<()> {
    let rebuilt = tx
        .prepare("PRAGMA table_info(threads)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(std::result::Result::ok)
        .any(|name| name == "install_id");
    if !rebuilt {
        tx.execute_batch(
            "CREATE TABLE threads_v32(
                id TEXT PRIMARY KEY,
                alias TEXT,
                archived_alias TEXT,
                created REAL NOT NULL,
                updated REAL NOT NULL,
                install_id TEXT,
                context_id TEXT,
                subject TEXT,
                is_general INTEGER NOT NULL DEFAULT 0 CHECK(is_general IN (0,1)),
                archived INTEGER NOT NULL DEFAULT 0 CHECK(archived IN (0,1)),
                title TEXT,
                CHECK(install_id IS NOT NULL
                      OR (subject IS NULL AND context_id IS NULL AND is_general=0)),
                CHECK(subject IS NULL OR is_general=0));
             INSERT INTO threads_v32(id,alias,archived_alias,created,updated)
                SELECT id,alias,archived_alias,created,updated FROM threads;
             DROP TABLE threads;
             ALTER TABLE threads_v32 RENAME TO threads;",
        )?;
    }
    tx.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS threads_home_alias
            ON threads(alias) WHERE install_id IS NULL;
         CREATE UNIQUE INDEX IF NOT EXISTS threads_general
            ON threads(alias, install_id) WHERE is_general=1;
         CREATE UNIQUE INDEX IF NOT EXISTS threads_subject
            ON threads(alias, install_id, subject) WHERE subject IS NOT NULL;
         CREATE INDEX IF NOT EXISTS threads_conv
            ON threads(alias, install_id, created);",
    )?;
    Ok(())
}

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

/// The thread entry for a message queued to a threaded agent. `refs`
/// is the operator chat's cited needs-me rows (CAD-574) — daemon-side
/// validation keeps it `{kind,id}` pairs; it rides the entry's payload
/// so the board can render the citation, never the message text.
#[allow(clippy::too_many_arguments)]
fn message_entry<'a>(
    sender: &Sender,
    source: &str,
    body: &'a str,
    id: &'a str,
    refs: Option<&Value>,
    app: Option<&Value>,
    attachments: Option<&Value>,
    human: Option<&Value>,
    media_refs: Option<&Value>,
) -> NewEntry<'a> {
    let (role, mut payload) = match sender {
        Sender::Operator | Sender::OperatorChat => (ROLE_OPERATOR, json!({"source": source})),
        Sender::Agent(alias) => (ROLE_SYSTEM, json!({"source": source, "from": alias})),
        // A member's own message reads as the member (their user id), in
        // the assistant alias's private thread — never "operator".
        Sender::Member(sub) => (
            ROLE_MEMBER,
            json!({"source": source, "from": format!("users/{sub}"), "member": sub}),
        ),
        // CAD-1568: a remote CLI send reads as its verified principal —
        // a member-role entry, like a member assistant's chat, but the
        // stamp is `cli_authority` so member-authority readers never
        // mistake it for a bound assistant's stamp.
        Sender::Cli(auth) => (
            ROLE_MEMBER,
            json!({"source": source, "from": format!("cli-{}", auth.handle),
                   "member": auth.sub,
                   "cli_authority": {"org": auth.org, "sub": auth.sub,
                       "actor": auth.actor, "credential_id": auth.credential_id}}),
        ),
        Sender::VerifiedMember(auth) => (
            ROLE_MEMBER,
            json!({"source": source, "from": format!("users/{}", auth.sub), "member": auth.sub,
                   "member_authority": {"org": auth.org, "sub": auth.sub,
                       "assistant_id": auth.assistant_id, "credential_id": auth.credential_id}}),
        ),
        Sender::Unattributed => (ROLE_SYSTEM, json!({"source": source})),
    };
    if let Some(refs) = refs {
        payload["refs"] = refs.clone();
    }
    // CAD-802: the daemon-stamped App binding — verified server-side
    // at send time, read back by the board and the master turn.
    if let Some(app) = app {
        payload["app"] = app.clone();
    }
    // CAD-1168: the daemon-resolved attachment metadata — the ids the
    // send named are verified against `chat_files` before this rides
    // the payload, so a stored row is always a real retained file.
    if let Some(attachments) = attachments {
        payload["attachments"] = attachments.clone();
    }
    // CAD-1374: the verified human behind an owner chat send, daemon-derived
    // and recorded only when present.
    if let Some(human) = human {
        payload["human"] = human.clone();
    }
    if let Some(media_refs) = media_refs {
        payload["media_refs"] = media_refs.clone();
    }
    NewEntry {
        role,
        kind: KIND_MESSAGE,
        text: body,
        payload: Some(payload),
        message_id: Some(id),
    }
}

struct Binding<'a> {
    install: &'a str,
    context: Option<&'a str>,
    conversation: Option<&'a str>,
}

impl Store {
    /// The alias's thread, created on first use. The agent must exist.
    pub fn ensure_thread(&self, alias: &str) -> Result<Thread> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            self.agent_in(&tx, alias)?;
            let thread = Self::ensure_thread_in(&tx, alias)?;
            Ok(thread)
        })
    }

    /// [`Self::ensure_thread`] inside the caller's transaction; the
    /// caller has already proved the agent exists.
    pub(super) fn ensure_thread_in(tx: &impl super::StoreConn, alias: &str) -> Result<Thread> {
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
            install_id: None,
            context_id: None,
            subject: None,
            is_general: false,
            archived: false,
            title: None,
        })
    }

    /// Detach the alias's thread when its agent row is removed (CAD-304
    /// S4: a reused alias starts fresh). The rows stay, keyed by thread
    /// id; the alias moves to `archived_alias` and a `system` entry marks
    /// the removal. A later registration under the alias gets a new
    /// thread.
    pub(super) fn thread_detach_in(tx: &impl super::StoreConn, alias: &str) -> Result<()> {
        let Some(thread) = Self::thread_in(tx, alias)? else {
            return Self::conversations_detach_in(tx, alias);
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
        Self::conversations_detach_in(tx, alias)?;
        Ok(())
    }

    /// The agent's app conversations go with it: archived (read-only,
    /// kept), their alias moved to `archived_alias` so a reused alias
    /// starts fresh, like the home thread.
    fn conversations_detach_in(tx: &impl super::StoreConn, alias: &str) -> Result<()> {
        tx.execute(
            "UPDATE threads SET alias=NULL, archived_alias=?1, archived=1
             WHERE alias=?1 AND install_id IS NOT NULL",
            [alias],
        )?;
        Ok(())
    }

    /// The alias's thread, if one was ever started.
    pub fn thread(&self, alias: &str) -> Result<Option<Thread>> {
        self.read_tx(|conn| Self::thread_in(&conn, alias))
    }

    /// CAD-819 operator rows: every live home thread (the operator's own
    /// chat with an agent, `install_id` NULL). App installation threads are
    /// excluded by that filter, so the work view never shows app chats.
    pub fn home_threads(&self) -> Result<Vec<Thread>> {
        self.read_tx(|conn| {
            Ok(conn.query_vec(
                "SELECT * FROM threads WHERE alias IS NOT NULL AND install_id IS NULL
                  ORDER BY updated DESC",
                [],
                row_thread,
            )?)
        })
    }

    fn thread_in(conn: &impl super::StoreConn, alias: &str) -> Result<Option<Thread>> {
        Ok(conn
            .query_row(
                "SELECT * FROM threads WHERE alias=? AND install_id IS NULL",
                [alias],
                row_thread,
            )
            .optional()?)
    }

    fn thread_by_id_in(conn: &impl super::StoreConn, id: &str) -> Result<Option<Thread>> {
        Ok(conn
            .query_row("SELECT * FROM threads WHERE id=?", [id], row_thread)
            .optional()?)
    }

    /// One thread (home or conversation) by id; `None` when unknown.
    pub fn thread_by_id(&self, id: &str) -> Result<Option<Thread>> {
        let conn = self.conn();
        Self::thread_by_id_in(&conn, id)
    }

    /// The message `alias` delivered to its provider session most
    /// recently before `current` (nudges aside) — the session the next
    /// turn would otherwise reuse. `None` when it has delivered none.
    pub fn last_delivered_message(&self, alias: &str, current: &str) -> Result<Option<String>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                &format!(
                    "SELECT id FROM messages WHERE alias=?1 AND id != ?2
                     AND source != 'nudge' AND started IS NOT NULL
                     AND state IN {DELIVERED_STATES}
                     ORDER BY started DESC, seq DESC LIMIT 1"
                ),
                params![alias, current],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// The thread a message's entries live in: where its enqueue note
    /// landed, `None` when the message left none (daemon traffic to an
    /// unthreaded alias). Every later entry for the message — assistant
    /// text, tools, turn result — follows it (CAD-1098 I2). `alias`
    /// bounds the lookup so a message id of another agent never steers
    /// an entry out of its own threads.
    fn message_thread_in(
        conn: &impl super::StoreConn,
        alias: &str,
        id: &str,
    ) -> Result<Option<Thread>> {
        Ok(conn
            .query_row(
                "SELECT t.* FROM thread_entries e JOIN threads t ON t.id=e.thread_id
                 WHERE e.message_id=?1 AND (t.alias=?2 OR t.archived_alias=?2)
                 ORDER BY e.seq LIMIT 1",
                params![id, alias],
                row_thread,
            )
            .optional()?)
    }

    /// The conversation `message_id` belongs to: `Some(thread)` (home
    /// included) when its enqueue note exists, `None` otherwise.
    pub fn message_conversation(&self, alias: &str, message_id: &str) -> Result<Option<Thread>> {
        let conn = self.conn();
        Self::message_thread_in(&conn, alias, message_id)
    }

    /// Is `install` one this store knows? An installation exists for the
    /// store once it holds a context (the same proof `thread_app` uses
    /// for the app binding).
    pub fn app_install_known(&self, install: &str) -> Result<bool> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                "SELECT 1 FROM app_contexts WHERE install_id=? LIMIT 1",
                [install],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Every conversation of `alias` in `install`, General first, then
    /// by creation. Reads only; creates nothing.
    pub fn conversation_list(&self, alias: &str, install: &str) -> Result<Vec<Thread>> {
        let conn = self.conn();
        let rows = conn.query_vec(
            "SELECT * FROM threads WHERE alias=? AND install_id=?
             ORDER BY is_general DESC, created, id LIMIT ?",
            params![alias, install, CONVERSATION_LIMIT + 1],
            row_thread,
        )?;
        Ok(rows)
    }

    /// Conversation-list wire rows with the newest visible assistant reply.
    /// Creation, operator/system entries and tool progress are not replies.
    /// The watermark is computed from existing entries; no read state is stored.
    pub fn conversation_summaries(&self, alias: &str, install: &str) -> Result<Vec<Value>> {
        let conn = self.conn();
        Ok(conn.query_vec(
            "SELECT t.*, COALESCE((
                SELECT e.seq FROM thread_entries e
                WHERE e.thread_id=t.id AND e.role=? AND e.kind IN (?,?)
                  AND length(trim(e.text, ?)) > 0
                ORDER BY e.seq DESC LIMIT 1
             ), 0) AS reply_seq
             FROM threads t WHERE t.alias=? AND t.install_id=?
             ORDER BY t.is_general DESC, t.created, t.id LIMIT ?",
            params![
                ROLE_AGENT,
                KIND_ASSISTANT_TEXT,
                KIND_TURN_RESULT,
                REPLY_WHITESPACE,
                alias,
                install,
                CONVERSATION_LIMIT + 1
            ],
            |row| {
                let mut value = row_thread(row)?.to_json();
                value["reply_seq"] = json!(row.get::<_, i64>("reply_seq")?);
                Ok(value)
            },
        )?)
    }

    /// CAD-1700: the newest visible assistant reply's seq in one thread,
    /// or 0 when it has none. The same predicate as
    /// [`Self::conversation_summaries`] — an agent `assistant_text` or
    /// `turn_result` whose text is not ECMAScript whitespace — computed
    /// from the complete entries, never a read page or read state.
    pub fn thread_reply_seq(&self, thread_id: &str) -> Result<i64> {
        let conn = self.conn();
        Ok(conn.query_row(
            "SELECT COALESCE((
                SELECT e.seq FROM thread_entries e
                WHERE e.thread_id=?1 AND e.role=?2 AND e.kind IN (?,?)
                  AND length(trim(e.text, ?)) > 0
                ORDER BY e.seq DESC LIMIT 1
             ), 0)",
            params![
                thread_id,
                ROLE_AGENT,
                KIND_ASSISTANT_TEXT,
                KIND_TURN_RESULT,
                REPLY_WHITESPACE
            ],
            |row| row.get(0),
        )?)
    }

    /// Make or find a conversation (CAD-1098). The caller has already
    /// proven the installation, the context and (for a subject) the
    /// campaign. Idempotent kinds race the partial unique indexes plus
    /// `INSERT OR IGNORE`: two concurrent first creates yield one row.
    /// `Ok((thread, created))`.
    pub fn conversation_create(
        &self,
        alias: &str,
        install: &str,
        context: Option<&str>,
        kind: ConversationKind,
    ) -> Result<(Thread, bool)> {
        self.conversation_create_as(alias, install, context, kind, None)
    }

    /// CAD-1374: [`Self::conversation_create`] with the verified human
    /// behind the write. A created conversation records one canonical
    /// receipt in the same transaction; `None` (legacy/autonomous) records
    /// none, so no human is invented.
    pub fn conversation_create_as(
        &self,
        alias: &str,
        install: &str,
        context: Option<&str>,
        kind: ConversationKind,
        human: Option<&Value>,
    ) -> Result<(Thread, bool)> {
        self.write_tx(|tx| {
            self.agent_in(tx, alias)?;
            Self::conversation_make_in(tx, alias, install, context, kind, human)
        })
    }

    /// The app's General conversation, made if absent (idempotent; the
    /// installation is already proven by the caller). The board's list
    /// always shows it, so it exists before the first message.
    pub fn conversation_ensure_general(&self, alias: &str, install: &str) -> Result<Thread> {
        self.write_tx(|tx| {
            self.agent_in(tx, alias)?;
            let (thread, _) = Self::conversation_make_in(
                tx,
                alias,
                install,
                None,
                ConversationKind::General,
                None,
            )?;
            Ok(thread)
        })
    }

    fn conversation_make_in(
        tx: &impl super::StoreConn,
        alias: &str,
        install: &str,
        context: Option<&str>,
        kind: ConversationKind,
        human: Option<&Value>,
    ) -> Result<(Thread, bool)> {
        let (general, subject) = match kind {
            ConversationKind::General => (1_i64, None),
            ConversationKind::Fresh => (0, None),
            ConversationKind::Subject(subject) => (0, Some(subject)),
        };
        let at = now();
        let id = Uuid::new_v4().simple().to_string();
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM threads WHERE alias=? AND install_id=?",
            params![alias, install],
            |r| r.get(0),
        )?;
        let inserted = if kind == ConversationKind::Fresh {
            if count >= CONVERSATION_LIMIT {
                return Err(Error::rejected(format!(
                    "this app already has {CONVERSATION_LIMIT} conversations"
                )));
            }
            tx.execute(
                "INSERT INTO threads(id,alias,created,updated,install_id,context_id,subject,is_general)
                 VALUES(?,?,?,?,?,?,NULL,0)",
                params![id, alias, at, at, install, context],
            )?
        } else {
            tx.execute(
                "INSERT OR IGNORE INTO threads(id,alias,created,updated,install_id,context_id,subject,is_general)
                 VALUES(?,?,?,?,?,?,?,?)",
                params![id, alias, at, at, install, context, subject, general],
            )?
        };
        let thread = match kind {
            ConversationKind::General => tx.query_row(
                "SELECT * FROM threads WHERE alias=? AND install_id=? AND is_general=1",
                params![alias, install],
                row_thread,
            )?,
            ConversationKind::Subject(subject) => tx.query_row(
                "SELECT * FROM threads WHERE alias=? AND install_id=? AND subject=?",
                params![alias, install, subject],
                row_thread,
            )?,
            ConversationKind::Fresh => {
                tx.query_row("SELECT * FROM threads WHERE id=?", [&id], row_thread)?
            }
        };
        if inserted == 1 {
            let mut payload = json!({"thread": thread.id, "install_id": install,
                                    "subject": subject, "general": general == 1});
            if let Some(human) = human {
                let company = human
                    .get("company")
                    .filter(|c| c.is_string())
                    .ok_or_else(|| Error::internal("conversation receipt has no company"))?;
                payload["schema_version"] = json!(1);
                payload["action"] = json!("conversation_created");
                payload["outcome"] = json!("created");
                payload["reason"] = json!("conversation_created");
                payload["company"] = company.clone();
                payload["human"] = human.clone();
                payload["request_id"] = json!(id);
                payload["correlation_id"] = json!(id);
                payload["causation_id"] = Value::Null;
                payload["revision_or_digest"] = Value::Null;
            }
            Self::event(tx, alias, "conversation_created", payload)?;
        }
        Ok((thread, inserted == 1))
    }

    /// The conversation a verified-app send lands in, inside the enqueue
    /// transaction. `selector` is only a selector: it must name an
    /// existing, unarchived conversation of this alias and installation,
    /// and a subject conversation serves only the context it was proven
    /// under. No selector means the app's General conversation (made on
    /// first use). Nothing here trusts a client field for scope.
    pub(super) fn conversation_resolve_in(
        tx: &impl super::StoreConn,
        alias: &str,
        install: &str,
        context: Option<&str>,
        selector: Option<&str>,
    ) -> Result<Thread> {
        let thread = match selector {
            None => {
                Self::conversation_make_in(
                    tx,
                    alias,
                    install,
                    context,
                    ConversationKind::General,
                    None,
                )?
                .0
            }
            Some(id) => Self::thread_by_id_in(tx, id)?
                .filter(|t| t.alias == alias && t.install_id.as_deref() == Some(install))
                .ok_or_else(|| Error::rejected("Unknown conversation for this app installation"))?,
        };
        if thread.archived {
            return Err(Error::rejected(
                "this conversation is archived and read-only",
            ));
        }
        if thread.subject.is_some()
            && (context.is_none() || thread.context_id.as_deref() != context)
        {
            return Err(Error::rejected(
                "this campaign conversation serves only the context it was created in",
            ));
        }
        Ok(thread)
    }

    /// Append to the alias's thread. `Ok(None)` when it has none —
    /// agents without a chat are untouched.
    pub fn thread_append(&self, alias: &str, entry: NewEntry) -> Result<Option<i64>> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let seq = Self::thread_append_in(&tx, alias, entry)?;
            Ok(seq)
        })
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
        self.write_tx(|conn| {
            let tx = &mut *conn;
            // Follow the running message's conversation (CAD-1098 I2).
            // With no running message, thread_append_in resolves home.
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
            Ok(seq)
        })
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
        self.write_tx(|conn| {
            let tx = &mut *conn;
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
        })
    }

    /// The id of the alias's in-flight turn (`submitting` or `running`),
    /// as [`Self::thread_append_running`] links provider output to it.
    pub fn active_message_id(&self, alias: &str) -> Result<Option<String>> {
        let conn = self.conn();
        Self::running_message_in(&conn, alias)
    }

    /// The alias's in-flight turn. `submitting` counts: a provider can
    /// persist items before `on_started` marks the message `running`
    /// (Codex emits them right behind the `turn/start` reply).
    fn running_message_in(tx: &impl super::StoreConn, alias: &str) -> Result<Option<String>> {
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
        tx: &impl super::StoreConn,
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
        // CAD-1098: an entry tied to a message lands in that message's
        // conversation; anything else is home.
        let thread = match entry.message_id {
            Some(id) => match Self::message_thread_in(tx, alias, id)? {
                Some(thread) => Some(thread),
                None => Self::thread_in(tx, alias)?,
            },
            None => Self::thread_in(tx, alias)?,
        };
        let Some(thread) = thread else {
            return Ok(None);
        };
        Self::thread_write_in(tx, &thread, entry).map(Some)
    }

    /// Append to one specific thread (a conversation's pack or session
    /// notes — entries tied to no message that must not land in home).
    pub fn thread_append_to(&self, thread_id: &str, entry: NewEntry) -> Result<Option<i64>> {
        self.write_tx(|tx| {
            let Some(thread) = Self::thread_by_id_in(tx, thread_id)? else {
                return Ok(None);
            };
            Self::thread_write_in(tx, &thread, entry).map(Some)
        })
    }

    /// CAD-1752: record the master's hire proposal on its conversation.
    /// One entry per (turn, digest): proposing the same hire twice on one
    /// turn is refused, so a replayed call cannot stack cards. It writes
    /// only this thread entry - never an agent row.
    pub fn hire_proposal_put(
        &self,
        thread_id: &str,
        message_id: &str,
        text: &str,
        payload: Value,
    ) -> Result<i64> {
        let digest = payload["digest"].as_str().unwrap_or_default().to_string();
        let role = payload["role"].as_str().unwrap_or_default().to_string();
        let name = payload["name"].as_str().unwrap_or_default().to_string();
        self.write_tx(|conn| {
            let tx = &mut *conn;
            let dup: Option<i64> = tx
                .query_row(
                    "SELECT seq FROM thread_entries WHERE thread_id=? AND kind=? AND message_id=?
                       AND json_extract(payload,'$.digest')=? AND json_extract(payload,'$.role')=?
                       AND COALESCE(json_extract(payload,'$.name'),'')=?",
                    params![
                        thread_id,
                        KIND_HIRE_PROPOSAL,
                        message_id,
                        digest,
                        role,
                        name
                    ],
                    |r| r.get(0),
                )
                .optional()?;
            if dup.is_some() {
                return Err(Error::rejected(
                    "this turn already proposed exactly this hire",
                ));
            }
            Self::thread_write_by_id_in(
                tx,
                thread_id,
                NewEntry {
                    role: ROLE_AGENT,
                    kind: KIND_HIRE_PROPOSAL,
                    text,
                    payload: Some(payload),
                    message_id: Some(message_id),
                },
            )
        })
    }

    /// Append inside a caller's transaction to one thread by id.
    pub(super) fn thread_write_by_id_in(
        tx: &impl super::StoreConn,
        thread_id: &str,
        entry: NewEntry,
    ) -> Result<i64> {
        let thread = Self::thread_by_id_in(tx, thread_id)?
            .ok_or_else(|| Error::rejected("Unknown conversation"))?;
        Self::thread_write_in(tx, &thread, entry)
    }

    fn thread_write_in(
        tx: &impl super::StoreConn,
        thread: &Thread,
        entry: NewEntry,
    ) -> Result<i64> {
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
        Ok(seq)
    }

    /// The operator/system entry for a freshly queued message.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn thread_note_enqueued(
        tx: &impl super::StoreConn,
        alias: &str,
        sender: &Sender,
        source: &str,
        body: &str,
        id: &str,
        refs: Option<&Value>,
        app: Option<&Value>,
        attachments: Option<&Value>,
        human: Option<&Value>,
        conversation: Option<&str>,
        media_refs: Option<&Value>,
    ) -> Result<()> {
        let mut entry = message_entry(
            sender,
            source,
            body,
            id,
            refs,
            app,
            attachments,
            human,
            media_refs,
        );
        if let Some(conversation) = conversation {
            entry
                .payload
                .as_mut()
                .ok_or_else(|| Error::internal("thread message entry is missing its payload"))?
                ["conversation"] = json!(conversation);
        }
        // CAD-1098: a verified App binding lands the message in the
        // conversation the server resolves (the client's selector is
        // checked, never trusted). The thread is fixed here, in the
        // enqueue transaction, together with the entry.
        if *sender == Sender::OperatorChat {
            if let Some(binding) = app.and_then(Self::verified_binding) {
                let thread = Self::conversation_resolve_in(
                    tx,
                    alias,
                    binding.install,
                    binding.context,
                    binding.conversation,
                )?;
                Self::thread_write_in(tx, &thread, entry)?;
                return Ok(());
            }
            if let Some(id) = conversation {
                let thread = Self::thread_by_id_in(tx, id)?
                    .filter(|t| t.alias == alias && t.runs_as_home() && !t.archived)
                    .ok_or_else(|| Error::rejected("Unknown or unavailable Home conversation"))?;
                Self::thread_write_in(tx, &thread, entry)?;
                return Ok(());
            }
            Self::ensure_thread_in(tx, alias)?;
        }
        // A CLI send has no conversation selector; without a home thread
        // `thread_append_in` returns Ok(None) and the entry is dropped.
        if matches!(sender, Sender::Cli(_)) {
            Self::ensure_thread_in(tx, alias)?;
        }
        Self::thread_append_in(tx, alias, entry)?;
        Ok(())
    }

    /// The `{install, context, conversation selector}` of a daemon-
    /// normalized App binding; `None` unless the daemon stamped it
    /// verified. The selector is the optional `conversation` the send
    /// named — a selector only.
    fn verified_binding(app: &Value) -> Option<Binding<'_>> {
        let obj = app.as_object()?;
        if obj.get("verified") != Some(&Value::Bool(true)) {
            return None;
        }
        Some(Binding {
            install: obj.get("install_id")?.as_str()?,
            // Absent for an installation-only binding: the context is a
            // per-turn hint, never part of a conversation's key.
            context: obj.get("context_id").and_then(Value::as_str),
            conversation: obj.get("conversation").and_then(Value::as_str),
        })
    }

    /// The verified App binding the enqueue note for `id` recorded
    /// (CAD-802), `None` when its payload carries none — the stored
    /// side of the retry's content comparison.
    pub(super) fn entry_app_in(tx: &impl super::StoreConn, id: &str) -> Result<Option<Value>> {
        Self::entry_payload_field_in(tx, id, "app")
    }

    /// The verified human the enqueue note for `id` recorded (CAD-1374),
    /// `None` when its payload carries none — the stored side of the
    /// retry's comparison, so a reused id under another human conflicts.
    pub(super) fn entry_human_in(tx: &impl super::StoreConn, id: &str) -> Result<Option<Value>> {
        Self::entry_payload_field_in(tx, id, "human")
    }

    /// The delivery-time App hint for `id` (CAD-802): the send-time
    /// stamp re-proved against the live store. An archived context, a
    /// revised or re-digested one, or any malformed stamp drops the
    /// hint — fail closed on the hint, never on the message, which
    /// delivers exactly as queued. Returns install, context, label
    /// and revision for the provider envelope; never profiles,
    /// secrets or digests.
    pub fn message_app(&self, message_id: &str) -> Result<Option<Value>> {
        // The store holds one mutex-guarded connection: scope the
        // guard so the re-proof below (which locks again) cannot
        // self-deadlock and wedge the daemon.
        let stamped = {
            let conn = self.conn();
            Self::entry_app_in(&conn, message_id)?
        };
        let Some(stamped) = stamped else {
            return Ok(None);
        };
        let obj = match stamped.as_object() {
            Some(obj) => obj,
            None => return Ok(None),
        };
        if obj.get("verified") != Some(&Value::Bool(true)) {
            return Ok(None);
        }
        let install = obj
            .get("install_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let context = obj
            .get("context_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let stamp_revision = obj.get("context_revision").and_then(Value::as_i64);
        let stamp_digest = obj.get("context_digest").and_then(Value::as_str);
        if install.is_empty()
            || context.is_empty()
            || stamp_revision.is_none()
            || stamp_digest.is_none()
        {
            return Ok(None);
        }
        let (config, proof) = match self.app_context_proof(install, context) {
            Ok(proven) => proven,
            Err(_) => return Ok(None),
        };
        if proof.revision != stamp_revision.unwrap() || proof.digest != stamp_digest.unwrap() {
            return Ok(None);
        }
        Ok(Some(json!({
            "install_id": proof.install_id,
            "context_id": proof.id,
            "label": config.label,
            "revision": proof.revision,
        })))
    }

    /// The refs the enqueue note for `id` recorded (CAD-574), `None`
    /// when its payload carries none — the stored side of the retry's
    /// content comparison.
    pub(super) fn entry_refs_in(tx: &impl super::StoreConn, id: &str) -> Result<Option<Value>> {
        Self::entry_payload_field_in(tx, id, "refs")
    }

    /// The stored `payload.attachments` of `message_id`'s enqueue
    /// entry — metadata rows only ({id,name,size,mime,sha256}), what
    /// `delivery_body` renders into the attachment envelope. `None`
    /// when the entry carries none.
    pub fn message_attachments(&self, message_id: &str) -> Result<Option<Value>> {
        let conn = self.conn();
        Self::entry_attachments_in(&conn, message_id)
    }

    /// The attachments the enqueue note for `id` recorded (CAD-1168),
    /// `None` when its payload carries none — the stored side of the
    /// retry's content comparison.
    pub(super) fn entry_attachments_in(
        tx: &impl super::StoreConn,
        id: &str,
    ) -> Result<Option<Value>> {
        Self::entry_payload_field_in(tx, id, "attachments")
    }

    /// CAD-1481: the library-image selectors pinned on the enqueue note.
    pub fn message_media_refs(&self, message_id: &str) -> Result<Option<Value>> {
        let conn = self.conn();
        Self::entry_payload_field_in(&conn, message_id, "media_refs")
    }

    pub(super) fn entry_media_refs_in(
        tx: &impl super::StoreConn,
        id: &str,
    ) -> Result<Option<Value>> {
        Self::entry_payload_field_in(tx, id, "media_refs")
    }

    pub(super) fn entry_conversation_in(
        tx: &impl super::StoreConn,
        id: &str,
    ) -> Result<Option<String>> {
        Ok(Self::entry_payload_field_in(tx, id, "conversation")?
            .and_then(|value| value.as_str().map(str::to_string)))
    }

    /// One named field of the enqueue note's payload for `id`, `None`
    /// when the payload carries none — the stored side of the retry's
    /// content comparison.
    fn entry_payload_field_in(
        tx: &impl super::StoreConn,
        id: &str,
        field: &str,
    ) -> Result<Option<Value>> {
        let first: Option<Option<String>> = tx
            .query_row(
                "SELECT payload FROM thread_entries WHERE message_id=? \
                 ORDER BY seq LIMIT 1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(first
            .flatten()
            .and_then(|p| serde_json::from_str::<Value>(&p).ok())
            .and_then(|v| v.get(field).filter(|r| !r.is_null()).cloned()))
    }

    /// The `turn_result` entry for a finished message. The payload is
    /// built from named fields only — the stored result carries the
    /// turn token, which must never be copied here. Text held for the
    /// message ([`Self::thread_hold_running`]) that the result does not
    /// carry lands first, as `assistant_text`.
    pub(super) fn thread_note_finished(
        &self,
        tx: &impl super::StoreConn,
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

    /// The sender the daemon attributed when `message_id` was queued,
    /// recovered from the recipient's thread entry: `(role, from)` —
    /// `from` is the agent alias an agent caller was proven to be
    /// (`role` is `system`), `None` for operator and unattributed
    /// sends. `None` overall when the message produced no thread entry
    /// (an unthreaded recipient keeps none). This is the
    /// connection-derived record, not a request field: `dispatch_record`
    /// binds a lane's PM from it (CAD-378).
    pub fn message_sender(&self, message_id: &str) -> Result<Option<(String, Option<String>)>> {
        let conn = self.conn();
        match conn.query_row(
            "SELECT role,payload FROM thread_entries WHERE message_id=? \
             ORDER BY seq LIMIT 1",
            [message_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        ) {
            Ok((role, payload)) => {
                let from = payload
                    .and_then(|p| serde_json::from_str::<Value>(&p).ok())
                    .and_then(|v| v.get("from").and_then(Value::as_str).map(str::to_string));
                Ok(Some((role, from)))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// CAD-1380 R1: the member authority the ORIGINAL enqueue of `message_id`
    /// recorded. Only a member-role message entry in the message's own alias
    /// thread counts; a malformed stamp is an error, never a guess.
    /// CAD-1568: a `cli_authority`-stamped member entry is a remote CLI
    /// principal's message, not a bound assistant's — it answers `None`,
    /// not a malformed member stamp.
    pub fn message_member_authority(&self, message_id: &str) -> Result<Option<MemberAuthority>> {
        let conn = self.conn();
        let row = conn.query_row(
            "SELECT e.role,e.kind,e.payload FROM thread_entries e \
             JOIN threads t ON t.id=e.thread_id \
             JOIN messages m ON m.id=e.message_id AND m.alias=t.alias \
             WHERE e.message_id=? ORDER BY e.seq LIMIT 1",
            [message_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        );
        let (role, kind, payload) = match row {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if role != ROLE_MEMBER || kind != KIND_MESSAGE {
            return Ok(None);
        }
        let payload = payload.and_then(|p| serde_json::from_str::<Value>(&p).ok());
        // A `cli_authority` stamp is a remote CLI principal's message
        // (CAD-1568) — member-role for display, but not a bound
        // assistant's authority, so it is `None` here, never an error.
        if payload
            .as_ref()
            .and_then(|v| v.get("cli_authority"))
            .is_some()
        {
            return Ok(None);
        }
        let stamp = payload.and_then(|v| v.get("member_authority").cloned());
        let field = |k: &str| {
            stamp
                .as_ref()
                .and_then(|s| s.get(k))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        match (
            field("org"),
            field("sub"),
            field("assistant_id"),
            field("credential_id"),
        ) {
            (Some(org), Some(sub), Some(assistant_id), Some(credential_id)) => {
                Ok(Some(MemberAuthority {
                    org,
                    sub,
                    assistant_id,
                    credential_id,
                }))
            }
            _ => Err(Error::rejected(format!(
                "member message '{message_id}' carries no verified member authority"
            ))),
        }
    }

    /// Entries after `after`, oldest first, at most `limit` (bounded by
    /// [`PAGE_MAX`]). An alias with no thread reads empty.
    pub fn thread_entries(&self, alias: &str, after: i64, limit: i64) -> Result<Vec<ThreadEntry>> {
        let conn = self.conn();
        let Some(thread) = Self::thread_in(&conn, alias)? else {
            return Ok(Vec::new());
        };
        Self::entries_after_in(&conn, &thread.id, after, limit)
    }

    /// [`Self::thread_entries`] for one thread by id (home or an app
    /// conversation).
    pub fn thread_entries_of(
        &self,
        thread_id: &str,
        after: i64,
        limit: i64,
    ) -> Result<Vec<ThreadEntry>> {
        let conn = self.conn();
        Self::entries_after_in(&conn, thread_id, after, limit)
    }

    fn entries_after_in(
        conn: &impl super::StoreConn,
        thread_id: &str,
        after: i64,
        limit: i64,
    ) -> Result<Vec<ThreadEntry>> {
        let limit = limit.clamp(1, PAGE_MAX);
        let rows = conn.query_vec(
            "SELECT * FROM thread_entries WHERE thread_id=? AND seq>?
             ORDER BY seq LIMIT ?",
            params![thread_id, after, limit],
            row_entry,
        )?;
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
        Self::entries_before_in(&conn, &thread.id, before, limit)
    }

    /// [`Self::thread_entries_before`] for one thread by id.
    pub fn thread_entries_before_of(
        &self,
        thread_id: &str,
        before: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<ThreadEntry>, bool)> {
        let conn = self.conn();
        Self::entries_before_in(&conn, thread_id, before, limit)
    }

    fn entries_before_in(
        conn: &impl super::StoreConn,
        thread_id: &str,
        before: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<ThreadEntry>, bool)> {
        let limit = limit.clamp(1, PAGE_MAX);
        let mut rows = conn.query_vec(
            "SELECT * FROM thread_entries WHERE thread_id=? AND seq<?
             ORDER BY seq DESC LIMIT ?",
            params![thread_id, before.unwrap_or(i64::MAX), limit + 1],
            row_entry,
        )?;
        let more = rows.len() as i64 > limit;
        rows.truncate(limit as usize);
        rows.reverse();
        Ok((rows, more))
    }

    /// The thread as a continuity pack may carry it (CAD-324): the
    /// newest `limit` entries, oldest first, plus how many older ones
    /// qualify beyond them. Only what reached the agent qualifies — an
    /// entry tied to a message counts only once that message was
    /// delivered ([`DELIVERED_STATES`], an allowlist): a message still
    /// queued, or one the operator cancelled, never reaches a pack, and
    /// neither does `current`, the message the pack travels with.
    /// Entries tied to no message (system notes) qualify. An alias with
    /// no thread reads empty.
    pub fn continuity_entries(
        &self,
        alias: &str,
        current: &str,
        limit: i64,
    ) -> Result<(Vec<ThreadEntry>, i64)> {
        let conn = self.conn();
        let Some(thread) = Self::thread_in(&conn, alias)? else {
            return Ok((Vec::new(), 0));
        };
        Self::continuity_in(&conn, &thread.id, current, limit)
    }

    /// [`Self::continuity_entries`] for one conversation's thread: the
    /// pack of a session that serves it carries only these entries
    /// (CAD-1098 I7).
    pub fn continuity_entries_of(
        &self,
        thread_id: &str,
        current: &str,
        limit: i64,
    ) -> Result<(Vec<ThreadEntry>, i64)> {
        let conn = self.conn();
        Self::continuity_in(&conn, thread_id, current, limit)
    }

    fn continuity_in(
        conn: &impl super::StoreConn,
        thread_id: &str,
        current: &str,
        limit: i64,
    ) -> Result<(Vec<ThreadEntry>, i64)> {
        let filter = format!(
            "e.thread_id=?1 AND (e.message_id IS NULL OR (e.message_id != ?2 AND
             EXISTS(SELECT 1 FROM messages m WHERE m.id=e.message_id
                    AND m.state IN {DELIVERED_STATES})))"
        );
        let total: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM thread_entries e WHERE {filter}"),
            params![thread_id, current],
            |r| r.get(0),
        )?;
        let limit = limit.clamp(1, PAGE_MAX);
        let mut rows = conn.query_vec(
            &format!(
                "SELECT e.* FROM thread_entries e WHERE {filter} ORDER BY e.seq DESC LIMIT ?3"
            ),
            params![thread_id, current, limit],
            row_entry,
        )?;
        rows.reverse();
        let older = total - rows.len() as i64;
        Ok((rows, older.max(0)))
    }

    /// Is a compaction still waiting for its continuity pack (CAD-324)?
    /// True when the alias's thread records a `session_compacted` note
    /// newer than its last `continuity_pack` note. Both are thread rows,
    /// so the answer survives a daemon restart.
    pub fn compaction_pending(&self, alias: &str) -> Result<bool> {
        let conn = self.conn();
        let Some(thread) = Self::thread_in(&conn, alias)? else {
            return Ok(false);
        };
        Self::compaction_pending_in(&conn, &thread.id)
    }

    /// [`Self::compaction_pending`] for one thread by id.
    pub fn compaction_pending_of(&self, thread_id: &str) -> Result<bool> {
        let conn = self.conn();
        Self::compaction_pending_in(&conn, thread_id)
    }

    fn compaction_pending_in(conn: &impl super::StoreConn, thread_id: &str) -> Result<bool> {
        let last = |event: &str| -> Result<i64> {
            Ok(conn.query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM thread_entries
                 WHERE thread_id=? AND role='system' AND message_id IS NULL
                 AND json_extract(payload,'$.event')=?",
                params![thread_id, event],
                |r| r.get(0),
            )?)
        };
        Ok(last(COMPACTED_EVENT)? > last(PACK_EVENT)?)
    }

    /// Did the alias's last finished turn lose its outcome (CAD-324)?
    /// True when its most recently finished message — nudges and
    /// cancellations aside — is `unknown`, or was reconciled from
    /// `unknown` by the operator: the provider session that ran it was
    /// lost mid-turn, so the next session starts with a continuity pack.
    pub fn last_turn_lost(&self, alias: &str) -> Result<bool> {
        let conn = self.conn();
        let row: Option<(String, Option<String>)> = conn
            .query_row(
                "SELECT state, json_extract(result,'$.via') FROM messages
                 WHERE alias=? AND source != 'nudge' AND completed IS NOT NULL
                 AND state NOT IN ('queued','cancelled')
                 ORDER BY completed DESC, seq DESC LIMIT 1",
                [alias],
                |r| Ok((r.get(0)?, r.get(1).ok().flatten())),
            )
            .optional()?;
        Ok(row.is_some_and(|(state, via)| {
            state == "unknown" || via.as_deref() == Some("operator_reconcile")
        }))
    }
}

/// `payload.event` of the thread note recording a delivered continuity
/// pack (CAD-324).
pub const PACK_EVENT: &str = "continuity_pack";
/// `payload.event` of the thread note recording a provider compaction.
pub const COMPACTED_EVENT: &str = "session_compacted";

/// Message states whose thread entries a continuity pack may carry: the
/// message reached the agent (or was submitted to it). `queued` and
/// `cancelled` are left out by not being listed.
const DELIVERED_STATES: &str =
    "('submitting','running','completed','failed','interrupted','unknown')";

#[cfg(test)]
mod tests;
