//! CAD-506 / ADR 0006 §5.2, §5.4: the durable pending-effect record and
//! the "ran without you" draft log.
//!
//! `platform_effects` is one staged send-class call, keyed by
//! `effect_id` and (uniquely) by its brokered-request handle — one
//! effect = one request = one execution (§5.4). The row is the
//! authority: the brokered `pending` entry is only its live
//! projection, so a caller's deadline or a daemon restart never ends
//! the lifecycle.
//!
//! `platform_drafts` is the durable draft log (§5.2, Q3): a `draft`
//! executes at call time and leaves an information-only row — no
//! pending effect, no press — so the board can show "ran without you".
//!
//! No column holds credential bytes: `input`/`preview`/`input_summary`
//! are secret-guarded at stage time, and every write is paired with a
//! `refuse_leak` check against the enrolled credential before it
//! lands.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use crate::error::{Error, Result};

use super::{now, Store};

/// §5.5 effect event names — all on [`super::platform::PLATFORM_STREAM`]
/// (carrying fingerprints and handles, never secrets).
pub const EFFECT_REQUESTED_EVENT: &str = "effect_requested";
pub const EFFECT_DECIDED_EVENT: &str = "effect_decided";
pub const EFFECT_EXECUTED_EVENT: &str = "effect_executed";
pub const EFFECT_FAILED_EVENT: &str = "effect_failed";
pub const EFFECT_CANCELLED_EVENT: &str = "effect_cancelled";
/// `verified:false` — the Needs-you raise (§5.4 step 6). Also written
/// for rows left `reconcile` by a restart (§5.4 step 8): an accepted
/// send whose platform outcome is unproven is exactly the ambiguity a
/// human must inspect.
pub const EFFECT_NEEDS_YOU_EVENT: &str = "effect_needs_you";

/// The v18 schema objects. `IF NOT EXISTS` so a half-applied
/// migration converges on reopen.
pub(super) const SCHEMA_V18: &str = "CREATE TABLE IF NOT EXISTS platform_effects(
        effect_id TEXT PRIMARY KEY,
        request TEXT NOT NULL UNIQUE,
        agent TEXT NOT NULL,
        platform TEXT NOT NULL,
        account TEXT NOT NULL,
        tool TEXT NOT NULL,
        label TEXT,
        input TEXT NOT NULL,
        input_summary TEXT NOT NULL,
        preview TEXT NOT NULL,
        source_name TEXT,
        source_hash TEXT,
        scopes TEXT NOT NULL,
        task TEXT,
        state TEXT NOT NULL,
        close_reason TEXT,
        decision TEXT,
        outcome TEXT,
        needs_you INTEGER NOT NULL DEFAULT 0,
        staged_at REAL NOT NULL,
        updated_at REAL NOT NULL);
     CREATE TABLE IF NOT EXISTS platform_drafts(
        id INTEGER PRIMARY KEY,
        agent TEXT NOT NULL,
        platform TEXT NOT NULL,
        account TEXT NOT NULL,
        tool TEXT NOT NULL,
        label TEXT,
        input_summary TEXT NOT NULL,
        artifact TEXT,
        ran_at REAL NOT NULL);";

/// A `platform_effects` row — the durable pending-effect record of
/// §5.4. `to_record` renders exactly the shared schema's shape
/// (`contracts/connected-platform/v1/pending-effect.schema.json`).
#[derive(Debug, Clone)]
pub struct EffectRow {
    pub effect_id: String,
    /// The brokered-request handle — `request` in the record.
    pub request: String,
    pub agent: String,
    pub platform: String,
    pub account: String,
    pub tool: String,
    pub label: Option<String>,
    /// The exact staged call arguments (JSON).
    pub input: Value,
    pub input_summary: String,
    pub preview: String,
    /// The reviewed source artifact `input.source` names, if any.
    pub source_name: Option<String>,
    /// Its hash at stage time — Execute re-verifies (§5.4 step 3).
    pub source_hash: Option<String>,
    /// The declaration's scopes, frozen at stage time — the execute-time
    /// grant re-check runs against these, not the live table.
    pub scopes: Vec<String>,
    /// The task the call belongs to, when the caller named one.
    pub task: Option<String>,
    pub state: String,
    pub close_reason: Option<String>,
    /// `{by:{member,role,rule}, at, reason?}` once decided.
    pub decision: Option<Value>,
    /// `{result|error, verified}` once the effect ran.
    pub outcome: Option<Value>,
    /// `verified:false` raised a Needs-you item; a `reconcile` row is
    /// one too until a human closes it.
    pub needs_you: bool,
    pub staged_at: f64,
    pub updated_at: f64,
}

impl EffectRow {
    fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        let input: String = row.get("input")?;
        let scopes: String = row.get("scopes")?;
        let decision: Option<String> = row.get("decision")?;
        let outcome: Option<String> = row.get("outcome")?;
        Ok(Self {
            effect_id: row.get("effect_id")?,
            request: row.get("request")?,
            agent: row.get("agent")?,
            platform: row.get("platform")?,
            account: row.get("account")?,
            tool: row.get("tool")?,
            label: row.get("label")?,
            input: serde_json::from_str(&input).unwrap_or(Value::Null),
            input_summary: row.get("input_summary")?,
            preview: row.get("preview")?,
            source_name: row.get("source_name")?,
            source_hash: row.get("source_hash")?,
            scopes: serde_json::from_str(&scopes).unwrap_or_default(),
            task: row.get("task")?,
            state: row.get("state")?,
            close_reason: row.get("close_reason")?,
            decision: decision.and_then(|d| serde_json::from_str(&d).ok()),
            outcome: outcome.and_then(|o| serde_json::from_str(&o).ok()),
            needs_you: row.get::<_, i64>("needs_you")? != 0,
            staged_at: row.get("staged_at")?,
            updated_at: row.get("updated_at")?,
        })
    }

    /// The §5.4 record — the shape the schema validates and the press
    /// reviews. Only record fields appear; `scopes`, `task` and the
    /// timestamps are the row's bookkeeping, not the record.
    pub fn to_record(&self) -> Value {
        let mut rec = json!({
            "request": self.request,
            "kind": "effect",
            "agent": self.agent,
            "platform": self.platform,
            "account": self.account,
            "tool": self.tool,
            "effect": "send",
            "input_summary": self.input_summary,
            "input": self.input,
            "preview": self.preview,
            "effect_id": self.effect_id,
            "state": self.state,
        });
        if let Some(v) = &self.source_hash {
            rec["source_hash"] = json!(v);
        }
        if let Some(v) = &self.label {
            rec["label"] = json!(v);
        }
        if let Some(v) = &self.close_reason {
            rec["close_reason"] = json!(v);
        }
        if let Some(v) = &self.decision {
            rec["decision"] = v.clone();
        }
        if let Some(v) = &self.outcome {
            rec["outcome"] = v.clone();
        }
        // `needs_you` is the store's bookkeeping — the shared schema's
        // `additionalProperties:false` keeps it out of the record.
        rec
    }
}

/// A `platform_drafts` row — the information-only "ran without you"
/// record of a `draft` call (§5.2, Q3).
#[derive(Debug, Clone)]
pub struct DraftRow {
    pub id: i64,
    pub agent: String,
    pub platform: String,
    pub account: String,
    pub tool: String,
    pub label: Option<String>,
    pub input_summary: String,
    /// What the draft produced, bounded (a platform ref or artifact id).
    pub artifact: Option<String>,
    pub ran_at: f64,
}

impl DraftRow {
    fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            agent: row.get("agent")?,
            platform: row.get("platform")?,
            account: row.get("account")?,
            tool: row.get("tool")?,
            label: row.get("label")?,
            input_summary: row.get("input_summary")?,
            artifact: row.get("artifact")?,
            ran_at: row.get("ran_at")?,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "agent": self.agent,
            "platform": self.platform,
            "account": self.account,
            "tool": self.tool,
            "label": self.label,
            "input_summary": self.input_summary,
            "artifact": self.artifact,
            "ran_at": self.ran_at,
        })
    }
}

/// What a press records — `{member, role, rule}`.
pub fn presser_json(member: &str, role: &str, rule: &str) -> Value {
    json!({"member": member, "role": role, "rule": rule})
}

impl Store {
    /// Stage a send: insert the `waiting` row and its `effect_requested`
    /// audit event in one transaction. `request` is UNIQUE — a retried
    /// open of the same handle lands here and must return the existing
    /// row (`existing`), never a second effect. Answers the row and
    /// whether it already existed (the dedupe), so the caller omits a
    /// second `request_opened` event.
    pub fn effect_stage(&self, row: &EffectRow) -> Result<(EffectRow, bool)> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let existing: Option<EffectRow> = tx
            .query_row(
                "SELECT * FROM platform_effects WHERE request=?1",
                params![row.request],
                EffectRow::from_row,
            )
            .optional()?;
        if let Some(existing) = existing {
            // The dedupe must be genuine: a same-named handle carrying a
            // different call is a conflict, not a retry — refuse it so a
            // caller cannot squat a handle and disguise a second effect.
            let same = existing.agent == row.agent
                && existing.platform == row.platform
                && existing.account == row.account
                && existing.tool == row.tool
                && existing.input == row.input;
            if !same {
                return Err(Error::rejected(format!(
                    "effect request '{}' already names a different staged call",
                    row.request
                )));
            }
            return Ok((existing, true));
        }
        tx.execute(
            "INSERT INTO platform_effects
             (effect_id, request, agent, platform, account, tool, label,
              input, input_summary, preview, source_name, source_hash,
              scopes, task, state, staged_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,'waiting',?15,?15)",
            params![
                row.effect_id,
                row.request,
                row.agent,
                row.platform,
                row.account,
                row.tool,
                row.label,
                serde_json::to_string(&row.input)?,
                row.input_summary,
                row.preview,
                row.source_name,
                row.source_hash,
                serde_json::to_string(&row.scopes)?,
                row.task,
                now(),
            ],
        )?;
        Self::event(
            &tx,
            super::platform::PLATFORM_STREAM,
            EFFECT_REQUESTED_EVENT,
            json!({"request": row.request, "effect_id": row.effect_id,
                   "agent": row.agent, "platform": row.platform,
                   "account": row.account, "tool": row.tool,
                   "input_summary": row.input_summary}),
        )?;
        tx.commit()?;
        Ok((row.clone(), false))
    }

    /// One row by brokered handle.
    pub fn effect_by_request(&self, request: &str) -> Result<Option<EffectRow>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT * FROM platform_effects WHERE request=?1",
            params![request],
            EffectRow::from_row,
        )
        .optional()
        .map_err(Into::into)
    }

    /// One row by durable id.
    pub fn effect_by_id(&self, effect_id: &str) -> Result<Option<EffectRow>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT * FROM platform_effects WHERE effect_id=?1",
            params![effect_id],
            EffectRow::from_row,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Pending-effect records — all of them, or one agent's.
    pub fn platform_effects(&self, agent: Option<&str>) -> Result<Vec<EffectRow>> {
        let conn = self.conn();
        let mut out = Vec::new();
        match agent {
            Some(agent) => {
                let mut stmt = conn.prepare(
                    "SELECT * FROM platform_effects WHERE agent=?1 \
                     ORDER BY staged_at, effect_id",
                )?;
                for r in stmt.query_map(params![agent], EffectRow::from_row)? {
                    out.push(r?);
                }
            }
            None => {
                let mut stmt =
                    conn.prepare("SELECT * FROM platform_effects ORDER BY staged_at, effect_id")?;
                for r in stmt.query_map([], EffectRow::from_row)? {
                    out.push(r?);
                }
            }
        }
        Ok(out)
    }

    /// The draft log — the information-only "ran without you" rows.
    /// `limit` bounds the listing; newest first.
    pub fn platform_drafts(&self, agent: Option<&str>, limit: usize) -> Result<Vec<DraftRow>> {
        let conn = self.conn();
        let mut out = Vec::new();
        let sql = match agent {
            Some(_) => {
                "SELECT * FROM platform_drafts WHERE agent=?1 \
                        ORDER BY id DESC LIMIT ?2"
            }
            None => "SELECT * FROM platform_drafts ORDER BY id DESC LIMIT ?2",
        };
        let mut stmt = conn.prepare(sql)?;
        for r in stmt.query_map(
            params![agent.unwrap_or_default(), limit as i64],
            DraftRow::from_row,
        )? {
            out.push(r?);
        }
        Ok(out)
    }

    /// Record one `draft` execution — row and `effect_executed` audit in
    /// one transaction (§5.5 audits drafts too; there is no pending row).
    pub fn draft_record(&self, row: &DraftRow, verified_ok: &Value) -> Result<()> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO platform_drafts
             (agent, platform, account, tool, label, input_summary, artifact, ran_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                row.agent,
                row.platform,
                row.account,
                row.tool,
                row.label,
                row.input_summary,
                row.artifact,
                row.ran_at,
            ],
        )?;
        Self::event(
            &tx,
            super::platform::PLATFORM_STREAM,
            EFFECT_EXECUTED_EVENT,
            json!({"effect": "draft", "agent": row.agent,
                   "platform": row.platform, "account": row.account,
                   "tool": row.tool, "label": row.label,
                   "input_summary": row.input_summary,
                   "result": verified_ok, "verified": true}),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The press — `accept` or `decline`. One atomic guarded update is
    /// the concurrent-press guarantee (§5.4 step 4): whichever
    /// transaction lands first owns the decision; every later press —
    /// same decision or not — finds the row no longer `waiting` and is
    /// refused. `decision` is `{by, at, reason?}`; the durable record
    /// exists BEFORE any execution starts (step 5).
    ///
    /// Answers the updated row, or `None` when the row left `waiting`
    /// (already decided, declined, closed or reconciled).
    pub fn effect_decide(
        &self,
        request: &str,
        accept: bool,
        decision: &Value,
    ) -> Result<Option<EffectRow>> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let state = if accept { "decided" } else { "declined" };
        let n = tx.execute(
            "UPDATE platform_effects SET state=?2, decision=?3, updated_at=?4
             WHERE request=?1 AND state='waiting'",
            params![request, state, serde_json::to_string(decision)?, now()],
        )?;
        if n == 0 {
            return Ok(None);
        }
        Self::event(
            &tx,
            super::platform::PLATFORM_STREAM,
            EFFECT_DECIDED_EVENT,
            json!({"request": request,
                   "decision": if accept { "accept" } else { "decline" },
                   "by": decision["by"], "reason": decision["reason"]}),
        )?;
        let row = tx.query_row(
            "SELECT * FROM platform_effects WHERE request=?1",
            params![request],
            EffectRow::from_row,
        )?;
        tx.commit()?;
        Ok(Some(row))
    }

    /// `decided` → `executing`, immediately before the platform call.
    /// A separate commit keeps "accept recorded, execution unproven"
    /// distinguishable from "executing" at restart — both reconcile,
    /// but the record says exactly where the run stopped.
    pub fn effect_executing(&self, effect_id: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE platform_effects SET state='executing', updated_at=?2
             WHERE effect_id=?1 AND state='decided'",
            params![effect_id, now()],
        )?;
        Ok(())
    }

    /// The platform outcome: `executing` → `done`|`failed` with the
    /// recorded `{result|error, verified}` and its §5.5 event, in one
    /// transaction. `needs_you` is set when `verified` is `false`.
    pub fn effect_outcome(
        &self,
        effect_id: &str,
        ok: bool,
        outcome: &Value,
        summary: &str,
    ) -> Result<EffectRow> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let needs_you = outcome["verified"] == json!(false);
        tx.execute(
            "UPDATE platform_effects SET state=?2, outcome=?3, needs_you=?4,
             updated_at=?5 WHERE effect_id=?1 AND state='executing'",
            params![
                effect_id,
                if ok { "done" } else { "failed" },
                serde_json::to_string(outcome)?,
                needs_you as i64,
                now()
            ],
        )?;
        Self::event(
            &tx,
            super::platform::PLATFORM_STREAM,
            if ok {
                EFFECT_EXECUTED_EVENT
            } else {
                EFFECT_FAILED_EVENT
            },
            json!({"effect_id": effect_id,
                   "result": summary, "verified": outcome["verified"]}),
        )?;
        if needs_you {
            Self::event(
                &tx,
                super::platform::PLATFORM_STREAM,
                EFFECT_NEEDS_YOU_EVENT,
                json!({"effect_id": effect_id,
                       "reason": "read-back does not match the approved input"}),
            )?;
        }
        let row = tx.query_row(
            "SELECT * FROM platform_effects WHERE effect_id=?1",
            params![effect_id],
            EffectRow::from_row,
        )?;
        tx.commit()?;
        Ok(row)
    }

    /// Cancel a `waiting` (or `reconcile`) row: `closed` with the
    /// reason named, plus `effect_cancelled`. Waiting and reconcile are
    /// the only states a close may take — anything past the durable
    /// decision is terminal or in flight. A `reconcile` row's decision
    /// is dropped from the projection on close: the record's invariants
    /// keep `closed` decision-free, and the `effect_decided` event
    /// still carries the press the row was reconciling.
    ///
    /// Answers the closed row, `None` when no closeable row matched.
    pub fn effect_close(&self, key: EffectKey, reason: &str) -> Result<Option<EffectRow>> {
        self.effect_close_in(key, reason, &["waiting", "reconcile"])
    }

    /// The Execute-time cancel (§5.4 step 3's race clause): a `decided`
    /// row — accept recorded, not yet executing — closes with the named
    /// reason when its source pin no longer verifies or its grant is
    /// gone. The same projection rule applies: `closed` carries no
    /// decision; the `effect_decided` event keeps the press.
    pub fn effect_close_decided(&self, effect_id: &str, reason: &str) -> Result<Option<EffectRow>> {
        self.effect_close_in(EffectKey::Id(effect_id.to_string()), reason, &["decided"])
    }

    fn effect_close_in(
        &self,
        key: EffectKey,
        reason: &str,
        states: &[&'static str],
    ) -> Result<Option<EffectRow>> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let (where_by, arg): (&str, String) = match key {
            EffectKey::Request(r) => ("request", r),
            EffectKey::Id(id) => ("effect_id", id),
        };
        let list = states
            .iter()
            .map(|s| format!("'{s}'"))
            .collect::<Vec<_>>()
            .join(",");
        let row: Option<EffectRow> = tx
            .query_row(
                &format!(
                    "SELECT * FROM platform_effects WHERE {where_by}=?1 \
                     AND state IN ({list})"
                ),
                params![arg],
                EffectRow::from_row,
            )
            .optional()?;
        let Some(row) = row else {
            return Ok(None);
        };
        tx.execute(
            "UPDATE platform_effects SET state='closed', close_reason=?2,
             decision=NULL, outcome=NULL, needs_you=0, updated_at=?3
             WHERE effect_id=?1",
            params![row.effect_id, reason, now()],
        )?;
        Self::event(
            &tx,
            super::platform::PLATFORM_STREAM,
            EFFECT_CANCELLED_EVENT,
            json!({"effect_id": row.effect_id, "request": row.request,
                   "reason": reason}),
        )?;
        tx.commit()?;
        Ok(Some(row))
    }

    /// §5.4 step 8 — restart reconciliation, inside `recover`'s
    /// transaction: a row the daemon proved `decided` (accept) or
    /// `executing` without an outcome may already have fired on the
    /// platform, so it becomes `reconcile` for a human — never
    /// re-fired. `waiting` rows survive untouched; the request listing
    /// reads them from this table, so nothing needs re-parking.
    pub fn reconcile_effects_in(&self, tx: &rusqlite::Connection) -> Result<Vec<EffectRow>> {
        let mut stmt =
            tx.prepare("SELECT * FROM platform_effects WHERE state IN ('decided','executing')")?;
        let rows: Vec<EffectRow> = stmt
            .query_map([], EffectRow::from_row)?
            .collect::<std::result::Result<_, _>>()?;
        if rows.is_empty() {
            return Ok(rows);
        }
        tx.execute(
            "UPDATE platform_effects SET state='reconcile', needs_you=1,
             updated_at=?1 WHERE state IN ('decided','executing')",
            params![now()],
        )?;
        for row in &rows {
            Self::event(
                tx,
                super::platform::PLATFORM_STREAM,
                EFFECT_NEEDS_YOU_EVENT,
                json!({"effect_id": row.effect_id, "request": row.request,
                       "reason": "accepted effect lacks a proven outcome — reconcile",
                       "from_state": row.state}),
            )?;
        }
        Ok(rows)
    }

    /// Acknowledge a terminal row's Needs-you flag — the operator has
    /// seen the `verified:false` outcome and dismissed the item. The
    /// record is untouched (`done`/`failed` rows never leave their
    /// state); only the row bookkeeping clears. Answers `None` when
    /// the row is not a flagged terminal.
    pub fn effect_ack(&self, effect_id: &str) -> Result<Option<EffectRow>> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let n = tx.execute(
            "UPDATE platform_effects SET needs_you=0, updated_at=?2 \
             WHERE effect_id=?1 AND state IN ('done','failed') AND needs_you=1",
            params![effect_id, now()],
        )?;
        if n == 0 {
            tx.commit()?;
            return Ok(None);
        }
        Self::event(
            &tx,
            super::platform::PLATFORM_STREAM,
            "effect_acknowledged",
            json!({"effect_id": effect_id}),
        )?;
        let row = tx.query_row(
            "SELECT * FROM platform_effects WHERE effect_id=?1",
            params![effect_id],
            EffectRow::from_row,
        )?;
        tx.commit()?;
        Ok(Some(row))
    }

    /// `waiting` rows re-park as live pending entries after restart.
    pub fn waiting_effects(&self) -> Result<Vec<EffectRow>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT * FROM platform_effects WHERE state='waiting' ORDER BY staged_at")?;
        let rows = stmt.query_map([], EffectRow::from_row)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

/// Which handle [`Store::effect_close`] names.
pub enum EffectKey {
    Request(String),
    Id(String),
}
