//! Inbox mailboxes: unread delivery rows per agent.

use crate::adapter::registry;
use crate::error::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use super::agents::Agent;
use super::messages::{row_message, Message};
use super::{now, Store};

impl Store {
    /// The alias must be a passive inbox — every `cadence inbox` verb
    /// (drain, peek, ack) shares this refusal for process endpoints.
    fn inbox_agent_in(&self, conn: &Connection, alias: &str) -> Result<Agent> {
        let agent = self.agent_in(conn, alias)?;
        if registry::has_actor(&agent.provider, &agent.endpoint_kind) {
            return Err(Error::rejected(format!(
                "Agent '{alias}' is endpoint kind '{}' — `cadence inbox` only \
                 reads inbox agents",
                agent.endpoint_kind
            )));
        }
        Ok(agent)
    }

    /// Drain an inbox agent's queue: every `queued` message with
    /// `seq > after`, oldest first, is completed `via=inbox_read` in one
    /// transaction. The receipt is local mailbox history; `route_result`
    /// deliberately does not turn it into a synthetic worker notification.
    pub fn inbox_drain(&self, alias: &str, after: i64) -> Result<Vec<Message>> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        self.inbox_agent_in(&tx, alias)?;
        let mut stmt = tx.prepare(
            "SELECT * FROM messages WHERE alias=? AND state='queued' AND seq>?
             ORDER BY seq",
        )?;
        let pending = stmt
            .query_map(params![alias, after], row_message)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        for m in &pending {
            let result = json!({"status": "completed", "via": "inbox_read"});
            tx.execute(
                "UPDATE messages SET state='completed',result=?,completed=?
                 WHERE id=? AND state='queued'",
                params![result.to_string(), now(), m.id],
            )?;
            Self::event(&tx, alias, "inbox_read", json!({"message": m.id}))?;
            self.route_result(&tx, m, &result)?;
        }
        tx.commit()?;
        Ok(pending)
    }

    /// Peek at an inbox agent's queue (CAD-480): every `queued` message
    /// with `seq > after`, oldest first — the same set `inbox_drain`
    /// would consume, but nothing changes state. A reader that crashes,
    /// truncates or mis-parses after a peek loses nothing: the messages
    /// stay queued for the next reader. Messages `reader` parked
    /// (`inbox_park`) are skipped for that reader only — they stay
    /// queued and countable but stop blocking the follower's head of
    /// line.
    pub fn inbox_peek(&self, alias: &str, after: i64, reader: &str) -> Result<Vec<Message>> {
        let conn = self.conn();
        self.inbox_agent_in(&conn, alias)?;
        let mut stmt = conn.prepare(
            "SELECT * FROM messages WHERE alias=? AND state='queued' AND seq>?
             ORDER BY seq",
        )?;
        let pending = stmt
            .query_map(params![alias, after], row_message)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let parked = self.inbox_parked_in(&conn, alias, reader)?;
        Ok(pending
            .into_iter()
            .filter(|m| !parked.contains(&m.id))
            .collect())
    }

    /// The message ids `reader` parked on this inbox (`inbox_park`
    /// events), as a set the peek filters out.
    fn inbox_parked_in(
        &self,
        conn: &Connection,
        alias: &str,
        reader: &str,
    ) -> Result<std::collections::HashSet<String>> {
        let mut stmt =
            conn.prepare("SELECT payload FROM events WHERE alias=? AND kind='inbox_park'")?;
        let rows = stmt
            .query_map([alias], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .iter()
            .filter_map(|p| serde_json::from_str::<Value>(p).ok())
            .filter(|p| p["reader"].as_str().unwrap_or("default") == reader)
            .filter_map(|p| p["message"].as_str().map(str::to_string))
            .collect())
    }

    /// Acknowledge an inbox's messages (CAD-480): every `queued` message
    /// with `seq <= through` is completed `via=inbox_ack` in one
    /// transaction — a consume watermark. `reader` names the consuming
    /// cursor (one per reader, recorded on the `inbox_ack` event) and
    /// `by` the derived caller. An already-consumed or nonexistent seq
    /// below the watermark is not an error: the ack asserts "consumed
    /// through N", and completing only rows still `queued` keeps the
    /// ack idempotent so concurrent readers never double-complete.
    /// `through` is clamped to the inbox's greatest existing seq before
    /// it is stored — a claim past the tail completes the tail but must
    /// not record a cursor that blinds the reader to every later
    /// arrival. The receipt is local mailbox history like `inbox_read`
    /// — `route_result` never synthesizes a worker notification for it.
    /// Returns the seqs this call completed and the remaining unread
    /// count.
    pub fn inbox_ack(&self, alias: &str, through: i64, reader: &str, by: &str) -> Result<Value> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        self.inbox_agent_in(&tx, alias)?;
        let tail: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE alias=?",
            [alias],
            |r| r.get(0),
        )?;
        let through = through.min(tail);
        let mut stmt = tx.prepare(
            "SELECT * FROM messages WHERE alias=? AND state='queued' AND seq<=?
             ORDER BY seq",
        )?;
        let pending = stmt
            .query_map(params![alias, through], row_message)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        // Messages this reader parked stay queued under its own
        // watermark too — the ack asserts consumption, and a parked
        // message is precisely what the reader could not consume. The
        // cursor still moves past their seqs; they stay queued and
        // unread for the operator and other readers.
        let parked = self.inbox_parked_in(&tx, alias, reader)?;
        let pending: Vec<Message> = pending
            .into_iter()
            .filter(|m| !parked.contains(&m.id))
            .collect();
        let mut acked = Vec::with_capacity(pending.len());
        for m in &pending {
            let result =
                json!({"status": "completed", "via": "inbox_ack", "by": by, "reader": reader});
            tx.execute(
                "UPDATE messages SET state='completed',result=?,completed=?
                 WHERE id=? AND state='queued'",
                params![result.to_string(), now(), m.id],
            )?;
            acked.push(m.seq);
            self.route_result(&tx, m, &result)?;
        }
        // The watermark event is the durable per-reader cursor — a
        // restart reads `inbox_readers` to resume after the last ack.
        // It is recorded even when nothing was queued, so a reader's
        // claim past the current tail is still durable.
        Self::event(
            &tx,
            alias,
            "inbox_ack",
            json!({"reader": reader, "through": through, "seqs": acked, "by": by}),
        )?;
        let unread: i64 = tx.query_row(
            "SELECT COUNT(*) FROM messages WHERE alias=? AND state='queued'",
            [alias],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(json!({"acked": acked, "through": through, "reader": reader,
                  "unread": unread}))
    }

    /// Park one still-queued message for `reader` (CAD-480): the
    /// `inbox_park` event makes that reader's peeks skip it, so a
    /// message that keeps failing `--exec` stops blocking the follower's
    /// head of line. The row stays `queued` — it still counts in
    /// `unread`, a drain still consumes it, and an ack at or past its
    /// seq still completes it.
    pub fn inbox_park(
        &self,
        alias: &str,
        reader: &str,
        message: &str,
        by: &str,
        reason: &str,
    ) -> Result<Value> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        self.inbox_agent_in(&tx, alias)?;
        let state: Option<String> = tx
            .query_row(
                "SELECT state FROM messages WHERE alias=? AND id=?",
                params![alias, message],
                |r| r.get(0),
            )
            .optional()?;
        match state.as_deref() {
            Some("queued") => {}
            Some(s) => {
                return Err(Error::rejected(format!(
                    "cannot park '{message}' on '{alias}': it is {s}, not queued"
                )))
            }
            None => {
                return Err(Error::rejected(format!(
                    "cannot park '{message}': no such message on '{alias}'"
                )))
            }
        }
        Self::event(
            &tx,
            alias,
            "inbox_park",
            json!({"reader": reader, "message": message, "by": by,
                   "reason": reason}),
        )?;
        let unread: i64 = tx.query_row(
            "SELECT COUNT(*) FROM messages WHERE alias=? AND state='queued'",
            [alias],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(json!({"parked": message, "reader": reader, "unread": unread}))
    }

    /// Reset one reader's cursor (CAD-480): the `inbox_ack_reset` event
    /// drops the reader's watermark, so its next peek without `after`
    /// resumes from 0 and re-delivers every still-queued message. Only
    /// the cursor moves — completed messages stay completed, so nothing
    /// is lost; queued ones come back.
    pub fn inbox_ack_reset(&self, alias: &str, reader: &str, by: &str) -> Result<Value> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        self.inbox_agent_in(&tx, alias)?;
        Self::event(
            &tx,
            alias,
            "inbox_ack_reset",
            json!({"reader": reader, "by": by}),
        )?;
        tx.commit()?;
        Ok(json!({"reset": reader}))
    }

    /// Per-reader consume watermarks for an inbox (CAD-480), derived
    /// from its durable `inbox_ack` events: each reader's greatest
    /// `through` claim and when it last acked. An `inbox_ack_reset`
    /// event drops the reader's entry — only later acks rebuild it. An
    /// agent row's event log is the durable record, so the cursors
    /// survive a daemon restart and reset only with the history itself
    /// — a reset re-delivers still-queued messages, never loses them.
    pub fn inbox_readers(&self, alias: &str) -> Result<Value> {
        let conn = self.conn();
        self.inbox_readers_in(&conn, alias)
    }

    /// [`Self::inbox_readers`] on a caller-held connection.
    fn inbox_readers_in(&self, conn: &Connection, alias: &str) -> Result<Value> {
        let mut stmt = conn.prepare(
            "SELECT kind, payload, at FROM events WHERE alias=?
             AND kind IN ('inbox_ack','inbox_ack_reset') ORDER BY seq",
        )?;
        let rows = stmt
            .query_map([alias], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, f64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut readers = serde_json::Map::new();
        for (kind, payload, at) in rows {
            let payload: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
            let reader = payload["reader"].as_str().unwrap_or("default");
            if kind == "inbox_ack_reset" {
                // Events arrive in seq order: later acks re-create the
                // entry, so a reset drops exactly the pre-reset history.
                readers.remove(reader);
                continue;
            }
            let through = payload["through"].as_i64().unwrap_or(0);
            let entry = readers
                .entry(reader.to_string())
                .or_insert_with(|| json!({"through": 0, "last_ack_at": Value::Null}));
            if through > entry["through"].as_i64().unwrap_or(0) {
                entry["through"] = json!(through);
            }
            entry["last_ack_at"] = json!(at);
        }
        Ok(Value::Object(readers))
    }

    /// One reader's ack watermark — the seq it has consumed through.
    /// `None` when the reader never acked (its cursor starts at 0).
    pub fn inbox_reader_cursor(&self, alias: &str, reader: &str) -> Result<i64> {
        Ok(self.inbox_readers(alias)?[reader]["through"]
            .as_i64()
            .unwrap_or(0))
    }

    /// Durable mailbox evidence for a passive inbox.  Reading this view does
    /// not claim, drain, or complete any message: `inbox_read` remains an
    /// explicit consumer receipt and therefore cannot be mistaken for a
    /// semantic PM decision.
    pub fn inbox_status(&self, alias: &str) -> Result<Option<Value>> {
        let conn = self.conn();
        let agent = self.agent_in(&conn, alias)?;
        if registry::has_actor(&agent.provider, &agent.endpoint_kind) {
            return Ok(None);
        }
        let mut states = serde_json::Map::new();
        let mut stmt = conn.prepare(
            "SELECT state,COUNT(*) FROM messages WHERE alias=? GROUP BY state ORDER BY state",
        )?;
        let rows = stmt.query_map([alias], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (state, count) = row?;
            states.insert(state, json!(count));
        }
        let queued = states.get("queued").and_then(Value::as_i64).unwrap_or(0);
        let (oldest_created, last_received_at, last_progress_at): (
            Option<f64>,
            Option<f64>,
            Option<f64>,
        ) = conn.query_row(
            "SELECT MIN(CASE WHEN state='queued' THEN created END),
                    MAX(created),
                    MAX(CASE WHEN completed IS NOT NULL THEN completed
                             WHEN started IS NOT NULL THEN started END)
             FROM messages WHERE alias=?",
            [alias],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        Ok(Some(json!({
            "kind": "passive",
            "state": if queued > 0 { "backlog" } else { "idle" },
            "queued": queued,
            "states": states,
            "oldest_created_at": oldest_created,
            "oldest_age_secs": oldest_created.map(|created| (now() - created).max(0.0)),
            "last_received_at": last_received_at,
            "last_progress_at": last_progress_at,
            // CAD-480: per-reader ack watermarks — who has consumed
            // through what.
            "readers": self.inbox_readers_in(&conn, alias)?,
            "semantic_completion": "external_consumer_required",
            "receipt_only": true,
            "next_action": if queued > 0 {
                "A persistent mailbox consumer must read and act on these messages"
            } else {
                "Await the next durable message"
            },
        })))
    }

    /// CAD-251 consumer evidence for a mailbox: unread count, oldest
    /// unread, newest arrival, and the last `inbox_read`/`inbox_ack`
    /// completion — the inputs to [`crate::inbox::health`].
    pub fn inbox_consumer(&self, alias: &str) -> Result<Value> {
        let conn = self.conn();
        let (unread, oldest, newest, last_read): (i64, Option<f64>, Option<f64>, Option<f64>) =
            conn.query_row(
                "SELECT COALESCE(SUM(state='queued'),0),
                        MIN(CASE WHEN state='queued' THEN created END),
                        MAX(created),
                        MAX(CASE WHEN state='completed'
                                  AND (result LIKE '%\"via\":\"inbox_read\"%'
                                       OR result LIKE '%\"via\":\"inbox_ack\"%')
                                 THEN completed END)
                 FROM messages WHERE alias=?",
                [alias],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        Ok(json!({
            "unread": unread,
            "oldest_unread_at": oldest,
            "last_received_at": newest,
            "last_read_at": last_read,
        }))
    }
}
