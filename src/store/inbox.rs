//! Inbox mailboxes: unread delivery rows per agent.

use crate::adapter::registry;
use crate::error::{Error, Result};
use rusqlite::params;
use serde_json::{json, Value};

use super::agents::Agent;
use super::messages::{row_message, Message};
use super::StoreConn;
use super::{now, Store};

impl Store {
    /// The alias must be a passive inbox — every `cadence inbox` verb
    /// (drain, peek, ack) shares this refusal for process endpoints.
    fn inbox_agent_in(&self, conn: &impl super::StoreConn, alias: &str) -> Result<Agent> {
        let agent = self.agent_in(conn, alias)?;
        if agent.provider != "inbox" || agent.endpoint_kind != "inbox" {
            return Err(Error::rejected(format!(
                "Agent '{alias}' is endpoint kind '{}' — `cadence inbox` only \
                 reads inbox agents",
                agent.endpoint_kind
            )));
        }
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
        self.write_tx(|conn| {
            let tx = &mut *conn;
            self.inbox_agent_in(&tx, alias)?;
            let pending = tx.query_vec(
                "SELECT * FROM messages WHERE alias=? AND state='queued' AND seq>?
                         ORDER BY seq",
                params![alias, after],
                row_message,
            )?;
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
            Ok(pending)
        })
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
        self.read_tx(|conn| {
            self.inbox_agent_in(&conn, alias)?;
            let stmt_sql = "SELECT * FROM messages WHERE alias=? AND state='queued' AND seq>?
                         ORDER BY seq";
            let pending = conn
                .query_vec(stmt_sql, params![alias, after], row_message)
                .map(|rows| rows.into_iter().map(Ok::<_, rusqlite::Error>))?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            let parked = self.inbox_parked_in(&conn, alias, reader)?;
            Ok(pending
                .into_iter()
                .filter(|m| !parked.contains(&m.id))
                .collect())
        })
    }

    /// The message ids `reader` parked on this inbox (`inbox_park`
    /// events), as a set the peek filters out.
    fn inbox_parked_in(
        &self,
        conn: &impl super::StoreConn,
        alias: &str,
        reader: &str,
    ) -> Result<std::collections::HashSet<String>> {
        let rows = conn.query_vec(
            "SELECT payload FROM events WHERE alias=? AND kind='inbox_park'",
            [alias],
            |row| row.get::<_, String>(0),
        )?;
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
        self.write_tx(|conn| {
            let tx = &mut *conn;
            self.inbox_agent_in(&tx, alias)?;
            let tail: i64 = tx.query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE alias=?",
                [alias],
                |r| r.get(0),
            )?;
            let through = through.min(tail);
            let pending = tx.query_vec(
                "SELECT * FROM messages WHERE alias=? AND state='queued' AND seq<=?
                         ORDER BY seq",
                params![alias, through],
                row_message,
            )?;
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
            Ok(json!({"acked": acked, "through": through, "reader": reader,
                              "unread": unread}))
        })
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
        self.write_tx(|conn| {
            let tx = &mut *conn;
            self.inbox_agent_in(&tx, alias)?;
            let state: Option<String> = tx.query_opt(
                "SELECT state FROM messages WHERE alias=? AND id=?",
                params![alias, message],
                |r| r.get(0),
            )?;
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
            Ok(json!({"parked": message, "reader": reader, "unread": unread}))
        })
    }

    /// Reset one reader's cursor (CAD-480): the `inbox_ack_reset` event
    /// drops the reader's watermark, so its next peek without `after`
    /// resumes from 0 and re-delivers every still-queued message. Only
    /// the cursor moves — completed messages stay completed, so nothing
    /// is lost; queued ones come back.
    pub fn inbox_ack_reset(&self, alias: &str, reader: &str, by: &str) -> Result<Value> {
        self.write_tx(|conn| {
            let tx = &mut *conn;
            self.inbox_agent_in(&tx, alias)?;
            Self::event(
                &tx,
                alias,
                "inbox_ack_reset",
                json!({"reader": reader, "by": by}),
            )?;
            Ok(json!({"reset": reader}))
        })
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
    fn inbox_readers_in(&self, conn: &impl super::StoreConn, alias: &str) -> Result<Value> {
        let rows = conn.query_vec(
            "SELECT kind, payload, at FROM events WHERE alias=?
             AND kind IN ('inbox_ack','inbox_ack_reset') ORDER BY seq",
            [alias],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, f64>(2)?,
                ))
            },
        )?;
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
        if registry::placement(&agent.provider, &agent.endpoint_kind) != registry::Placement::Queue
        {
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

    /// CAD-2476: persist lightweight inbox metadata (`kind`, optional
    /// `artifact`, `auto_answered`) as a durable event tied to `message_id`
    /// without altering the `messages` table schema or polluting `body`.
    pub fn record_inbox_envelope(
        &self,
        alias: &str,
        message_id: &str,
        kind: crate::inbox::InboxKind,
        artifact: Option<&str>,
        auto_answered: bool,
    ) -> Result<()> {
        self.write_tx(|conn| {
            let mut payload = json!({
                "message": message_id,
                "kind": kind.as_str(),
            });
            if let Some(a) = artifact {
                payload["artifact"] = json!(a);
            }
            if auto_answered {
                payload["auto_answered"] = json!(true);
            }
            Self::event(&conn, alias, "inbox_envelope", payload)?;
            Ok(())
        })
    }

    /// CAD-2476: record that an inbox message was explicitly promoted to a
    /// tracked board issue (opt-in only; ordinary check-ins/handoffs stay
    /// ticket-free).
    pub fn record_inbox_promotion(
        &self,
        alias: &str,
        message_id: &str,
        issue_id: &str,
        by: &str,
    ) -> Result<()> {
        self.write_tx(|conn| {
            Self::event(
                &conn,
                alias,
                "inbox_promoted",
                json!({
                    "message": message_id,
                    "issue": issue_id,
                    "by": by,
                }),
            )?;
            Ok(())
        })
    }

    /// CAD-2476: record an operator's explicit decision (`approved` or
    /// `declined`) on a specialist `decision` proposal in the inbox.
    pub fn record_inbox_decision(
        &self,
        alias: &str,
        message_id: &str,
        decision: &str,
        reason: Option<&str>,
        by: &str,
    ) -> Result<()> {
        if !matches!(decision, "approved" | "declined") {
            return Err(Error::rejected(
                "inbox decision must be 'approved' or 'declined'",
            ));
        }
        self.write_tx(|conn| {
            let mut payload = json!({
                "message": message_id,
                "decision": decision,
                "by": by,
            });
            if let Some(r) = reason.map(str::trim).filter(|s| !s.is_empty()) {
                payload["reason"] = json!(r);
            }
            Self::event(&conn, alias, "inbox_decision", payload)?;
            Ok(())
        })
    }

    /// CAD-2476: durable Agent Inbox feed for ANY registered agent or
    /// assistant (active or passive). Returns newest-first bounded messages
    /// enriched with lightweight `kind` (`checkin`, `handoff`, `decision`,
    /// `reply`, `message`), optional `artifact`, daemon-derived sender
    /// provenance, optional promoted issue link, unread state against
    /// `reader`'s `inbox_ack` watermark, and the workspace directory of
    /// reachable colleagues' assistants and specialist agents.
    pub fn agent_inbox_feed(
        &self,
        alias: &str,
        after: Option<i64>,
        limit: usize,
        unread_only: bool,
        reader: &str,
    ) -> Result<Value> {
        let limit = limit.clamp(1, 100);
        self.read_tx(|conn| {
            let agent = self.agent_in(&conn, alias)?;
            let is_passive = agent.provider == "inbox" && agent.endpoint_kind == "inbox";
            let readers = self.inbox_readers_in(&conn, alias)?;
            let cursor = readers[reader]["through"].as_i64().unwrap_or(0);
            let parked = self.inbox_parked_in(&conn, alias, reader)?;

            // Load lightweight envelope metadata, optional ticket promotions, and decisions.
            let mut envelopes: std::collections::HashMap<String, Value> =
                std::collections::HashMap::new();
            let mut promoted: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            let mut decisions: std::collections::HashMap<String, Value> =
                std::collections::HashMap::new();
            let meta_rows = conn.query_vec(
                "SELECT kind, payload FROM events WHERE alias=?
                 AND kind IN ('inbox_envelope','inbox_promoted','inbox_decision') ORDER BY seq",
                [alias],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?;
            for (kind, raw) in meta_rows {
                let Ok(v) = serde_json::from_str::<Value>(&raw) else {
                    continue;
                };
                let Some(mid) = v["message"].as_str() else {
                    continue;
                };
                if kind == "inbox_envelope" {
                    envelopes.insert(mid.to_string(), v);
                } else if kind == "inbox_promoted" {
                    if let Some(issue) = v["issue"].as_str() {
                        promoted.insert(mid.to_string(), issue.to_string());
                    }
                } else if kind == "inbox_decision" {
                    decisions.insert(mid.to_string(), v);
                }
            }

            // Identify bound member assistants so we can label colleagues vs specialists.
            let mut colleague_aliases: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            if let Ok(rows) = conn.query_vec(
                "SELECT alias FROM assistants WHERE state='active'",
                [],
                |row| row.get::<_, String>(0),
            ) {
                for a in rows {
                    colleague_aliases.insert(a);
                }
            }

            let bound = after.unwrap_or(0);
            let rows = conn.query_vec(
                "SELECT * FROM messages WHERE alias=? AND seq>?
                 ORDER BY seq DESC LIMIT ?",
                params![alias, bound, (limit * 2) as i64],
                row_message,
            )?;

            let mut unread_total: i64 = 0;
            let mut tail_seq: i64 = cursor;
            let mut items: Vec<Value> = Vec::new();
            for m in rows {
                if m.is_nudge() || m.is_topic_notice() {
                    continue;
                }
                if m.seq > tail_seq {
                    tail_seq = m.seq;
                }
                if parked.contains(&m.id) {
                    continue;
                }
                let env = envelopes.get(&m.id);
                let auto_answered = env
                    .and_then(|e| e["auto_answered"].as_bool())
                    .unwrap_or(false);
                let is_unread = if auto_answered {
                    false
                } else if is_passive {
                    m.state == "queued" && m.seq > cursor
                } else {
                    m.seq > cursor
                };
                if is_unread {
                    unread_total += 1;
                }
                if unread_only && !is_unread {
                    continue;
                }
                if items.len() >= limit {
                    continue;
                }
                let kind = env
                    .and_then(|e| e["kind"].as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        if m.reply_to.is_some() || m.origin.is_some() {
                            "reply".to_string()
                        } else {
                            "message".to_string()
                        }
                    });
                let artifact = env.and_then(|e| e["artifact"].as_str()).map(str::to_string);
                let sender = m.sender.clone().unwrap_or_else(|| "unknown".to_string());
                let sender_kind = if sender == "operator" {
                    "operator"
                } else if colleague_aliases.contains(&sender)
                    || sender.starts_with("assist-")
                    || sender.starts_with("assist_")
                    || sender.starts_with("users/")
                {
                    "colleague"
                } else {
                    "specialist"
                };
                let promoted_issue = promoted.get(&m.id).cloned().or_else(|| m.issue.clone());
                let body = if m.source == "app_run_dispatch" {
                    "[app-owned material: use authorized run surfaces]".to_string()
                } else {
                    m.body.clone()
                };
                let decision_ev = decisions.get(&m.id);
                let decision_state = if let Some(ev) = decision_ev {
                    ev["decision"].as_str().map(str::to_string)
                } else if kind == "decision" {
                    Some("pending".to_string())
                } else {
                    None
                };
                let decided_by = decision_ev
                    .and_then(|ev| ev["by"].as_str())
                    .map(str::to_string);
                let decision_reason = decision_ev
                    .and_then(|ev| ev["reason"].as_str())
                    .map(str::to_string);
                items.push(json!({
                    "seq": m.seq,
                    "id": m.id,
                    "alias": m.alias,
                    "kind": kind,
                    "sender": sender,
                    "sender_uid": m.sender_uid,
                    "sender_kind": sender_kind,
                    "recipient": m.recipient.clone().unwrap_or_else(|| m.alias.clone()),
                    "reply_to": m.reply_to,
                    "origin": m.origin,
                    "body": body,
                    "artifact": artifact,
                    "state": m.state,
                    "unread": is_unread,
                    "auto_answered": auto_answered,
                    "decision_state": decision_state,
                    "decided_by": decided_by,
                    "decision_reason": decision_reason,
                    "issue": promoted_issue,
                    "task_id": m.task_id,
                    "created": m.created,
                }));
            }

            let directory = self.agent_inbox_directory_in(&conn, alias, &colleague_aliases)?;

            Ok(json!({
                "alias": alias,
                "cursor": cursor,
                "tail_seq": tail_seq,
                "unread": unread_total,
                "reader": reader,
                "messages": items,
                "directory": directory,
            }))
        })
    }

    /// Reachable colleagues' assistants and company specialist agents for
    /// read-only directory discovery.
    fn agent_inbox_directory_in(
        &self,
        conn: &impl super::StoreConn,
        self_alias: &str,
        colleague_aliases: &std::collections::HashSet<String>,
    ) -> Result<Value> {
        let rows = conn.query_vec(
            "SELECT alias, role, team_role, provider, endpoint_kind, state, params
             FROM agents ORDER BY alias",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )?;
        let mut colleagues = Vec::new();
        let mut specialists = Vec::new();
        for (alias, role, team_role, provider, endpoint_kind, state, raw_params) in rows {
            if alias == self_alias {
                continue;
            }
            let checkin_auto_reply = raw_params
                .as_deref()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .and_then(|v| {
                    v.pointer("/inbox/checkin_auto_reply")
                        .and_then(Value::as_bool)
                })
                .unwrap_or(false);
            let is_colleague = colleague_aliases.contains(&alias)
                || alias.starts_with("assist-")
                || alias.starts_with("assist_");
            let entry = json!({
                "alias": alias,
                "role": team_role.as_deref().unwrap_or(&role),
                "provider": provider,
                "endpoint_kind": endpoint_kind,
                "state": state,
                "checkin_auto_reply": checkin_auto_reply,
                "kind": if is_colleague {
                    "colleague"
                } else {
                    "specialist"
                },
            });
            if is_colleague {
                colleagues.push(entry);
            } else {
                specialists.push(entry);
            }
        }
        Ok(json!({
            "colleagues": colleagues,
            "specialists": specialists,
        }))
    }

    /// CAD-2476: acknowledge / mark read through `through` for ANY registered
    /// agent or assistant. Passive `inbox` mailboxes delegate to `inbox_ack`
    /// (completing queued rows); active agents record the durable `inbox_ack`
    /// watermark without disturbing live turn states.
    pub fn agent_inbox_mark(
        &self,
        alias: &str,
        through: i64,
        reader: &str,
        by: &str,
    ) -> Result<Value> {
        let is_passive = self.read_tx(|conn| {
            let agent = self.agent_in(&conn, alias)?;
            Ok(agent.provider == "inbox" && agent.endpoint_kind == "inbox")
        })?;
        if is_passive {
            return self.inbox_ack(alias, through, reader, by);
        }
        self.write_tx(|conn| {
            let tx = &mut *conn;
            self.agent_in(&tx, alias)?;
            let tail: i64 = tx.query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE alias=?",
                [alias],
                |r| r.get(0),
            )?;
            let clamped = through.clamp(0, tail);
            Self::event(
                &tx,
                alias,
                "inbox_ack",
                json!({"reader": reader, "through": clamped, "seqs": [], "by": by}),
            )?;
            let unread: i64 = tx.query_row(
                "SELECT COUNT(*) FROM messages WHERE alias=? AND seq>? AND source NOT IN ('nudge','topic_notice')",
                params![alias, clamped],
                |r| r.get(0),
            )?;
            Ok(json!({
                "alias": alias,
                "through": clamped,
                "reader": reader,
                "unread": unread,
            }))
        })
    }
}

#[cfg(test)]
mod cad2476_inbox_tests {
    use super::*;
    use crate::inbox::InboxKind;
    use crate::store::{NewAgent, Sender};

    fn register(
        store: &Store,
        dir: &std::path::Path,
        alias: &str,
        team_role: &str,
        provider: &str,
    ) {
        store
            .register_agent(&NewAgent {
                alias,
                provider,
                endpoint_kind: if provider == "inbox" {
                    "inbox"
                } else {
                    "managed"
                },
                role: "worker",
                cwd: dir.to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: Some(team_role),
                model_policy: None,
            })
            .unwrap();
    }

    #[test]
    fn agent_inbox_feed_supports_colleague_handoff_checkin_ack_and_opt_in_promote() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("cadence.sqlite3")).unwrap();
        register(&store, dir.path(), "master", "pm", "fake");
        register(&store, dir.path(), "assist-maya", "worker", "fake");
        register(&store, dir.path(), "finance-agent", "research", "fake");

        // 1. Colleague handoff with artifact ref — no ticket created.
        store
            .enqueue_sent(
                "master",
                "Q3 forecast ready for review",
                Some("assist-maya"),
                "msg-handoff-1",
                "cli",
                None,
                &Sender::Agent("assist-maya".into()),
            )
            .unwrap();
        let m1 = store.message("msg-handoff-1").unwrap().unwrap();
        store
            .record_inbox_envelope(
                "master",
                &m1.id,
                InboxKind::Handoff,
                Some("docs/q3-forecast.md"),
                false,
            )
            .unwrap();

        // 2. Non-ticket daily check-in from specialist.
        store
            .enqueue_sent(
                "master",
                "Invoice #4821 drafted, awaiting operator approval before booking.",
                Some("finance-agent"),
                "msg-checkin-2",
                "cli",
                None,
                &Sender::Agent("finance-agent".into()),
            )
            .unwrap();
        let m2 = store.message("msg-checkin-2").unwrap().unwrap();
        store
            .record_inbox_envelope("master", &m2.id, InboxKind::Checkin, None, false)
            .unwrap();

        let feed = store
            .agent_inbox_feed("master", None, 20, false, "default")
            .unwrap();
        assert_eq!(feed["alias"], "master");
        assert_eq!(feed["unread"], 2);
        assert_eq!(feed["tail_seq"], m2.seq);
        let msgs = feed["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        // Newest first
        assert_eq!(msgs[0]["id"], "msg-checkin-2");
        assert_eq!(msgs[0]["kind"], "checkin");
        assert_eq!(msgs[0]["sender_kind"], "specialist");
        assert!(msgs[0]["issue"].is_null());
        assert_eq!(msgs[1]["id"], "msg-handoff-1");
        assert_eq!(msgs[1]["kind"], "handoff");
        assert_eq!(msgs[1]["sender_kind"], "colleague");
        assert_eq!(msgs[1]["artifact"], "docs/q3-forecast.md");
        assert!(msgs[1]["issue"].is_null());

        // Workspace directory splits colleagues vs specialists and excludes self.
        let colleagues = feed["directory"]["colleagues"].as_array().unwrap();
        let specialists = feed["directory"]["specialists"].as_array().unwrap();
        assert_eq!(colleagues.len(), 1);
        assert_eq!(colleagues[0]["alias"], "assist-maya");
        assert_eq!(specialists.len(), 1);
        assert_eq!(specialists[0]["alias"], "finance-agent");

        // Mark first message read — unread drops from 2 to 1.
        let acked = store
            .agent_inbox_mark("master", m1.seq, "default", "operator")
            .unwrap();
        assert_eq!(acked["through"], m1.seq);
        assert_eq!(acked["unread"], 1);
        let unread_only = store
            .agent_inbox_feed("master", None, 20, true, "default")
            .unwrap();
        assert_eq!(unread_only["messages"].as_array().unwrap().len(), 1);
        assert_eq!(unread_only["messages"][0]["id"], "msg-checkin-2");

        // Opt-in promotion attaches a board issue only to the promoted message.
        store
            .record_inbox_promotion("master", &m1.id, "CAD-999", "operator")
            .unwrap();
        let after_promote = store
            .agent_inbox_feed("master", None, 20, false, "default")
            .unwrap();
        let promoted_row = after_promote["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "msg-handoff-1")
            .unwrap();
        assert_eq!(promoted_row["issue"], "CAD-999");
        let unpromoted_row = after_promote["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "msg-checkin-2")
            .unwrap();
        assert!(unpromoted_row["issue"].is_null());

        // Legacy passive-inbox drain still refuses non-passive agents.
        assert!(store.inbox_drain("master", 0).is_err());
    }
}
