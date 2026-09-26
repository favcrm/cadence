//! Monitor rows: registration, checks, alerts, dispatch.

use crate::adapter::registry;
use crate::error::{Error, Result};
use crate::proto::identifier;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashSet;
use uuid::Uuid;

use super::events::row_event;
use super::kickoff::kickoff_body;
use super::messages::Priority;
use super::plans::Task;
use super::quota::automatic_quota_error;
use super::{is_terminal, now, Sender, Store};

/// A daemon-owned supervision registration. `state` is the monitor
/// lifecycle (`degraded` until the first successful check, then `active`,
/// or `off` after an explicit stop); it never describes worker health.
#[derive(Debug, Clone)]
pub struct Monitor {
    pub id: String,
    pub project: String,
    pub owner: String,
    pub interval_secs: i64,
    pub state: String,
    pub heartbeat_at: Option<f64>,
    pub last_check_at: Option<f64>,
    pub last_success_at: Option<f64>,
    pub next_check_at: Option<f64>,
    pub event_cursor: i64,
    pub delivery_configured: bool,
    pub delivery_state: String,
    /// The legacy dispatch bit only permits an explicit `monitor dispatch`
    /// call.  Background coordination needs a separate durable opt-in so a
    /// pre-existing manual registration cannot silently start dispatching.
    pub dispatch_enabled: bool,
    pub auto_dispatch_enabled: bool,
    pub error: Option<String>,
    pub created: f64,
    pub updated: f64,
}

/// A durable local alert produced by an observed, task-scoped event.
#[derive(Debug, Clone)]
pub struct MonitorAlert {
    pub seq: i64,
    pub monitor_id: String,
    pub task_id: String,
    pub event_seq: i64,
    pub fingerprint: String,
    pub kind: String,
    pub payload: Value,
    pub state: String,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub created: f64,
    pub updated: f64,
}

/// Result of one daemon-owned monitor pass.
#[derive(Debug, Clone, Default)]
pub struct MonitorCheck {
    pub monitor_id: String,
    pub scanned: i64,
    pub alerts_created: i64,
    pub cursor: i64,
}

fn row_monitor(row: &rusqlite::Row) -> rusqlite::Result<Monitor> {
    Ok(Monitor {
        id: row.get("id")?,
        project: row.get("project")?,
        owner: row.get("owner")?,
        interval_secs: row.get("interval_secs")?,
        state: row.get("state")?,
        heartbeat_at: row.get("heartbeat_at")?,
        last_check_at: row.get("last_check_at")?,
        last_success_at: row.get("last_success_at")?,
        next_check_at: row.get("next_check_at")?,
        event_cursor: row.get("event_cursor")?,
        delivery_configured: row.get::<_, i64>("delivery_configured")? != 0,
        delivery_state: row.get("delivery_state")?,
        dispatch_enabled: row.get::<_, i64>("dispatch_enabled")? != 0,
        auto_dispatch_enabled: row.get::<_, i64>("auto_dispatch_enabled")? != 0,
        error: row.get("error")?,
        created: row.get("created")?,
        updated: row.get("updated")?,
    })
}

fn row_monitor_alert(row: &rusqlite::Row) -> rusqlite::Result<MonitorAlert> {
    let payload: String = row.get("payload")?;
    Ok(MonitorAlert {
        seq: row.get("seq")?,
        monitor_id: row.get("monitor_id")?,
        task_id: row.get("task_id")?,
        event_seq: row.get("event_seq")?,
        fingerprint: row.get("fingerprint")?,
        kind: row.get("kind")?,
        payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
        state: row.get("state")?,
        attempts: row.get("attempts")?,
        last_error: row.get("last_error")?,
        created: row.get("created")?,
        updated: row.get("updated")?,
    })
}

impl Monitor {
    pub fn to_json(&self, coverage: &[String], open_alerts: i64, total_alerts: i64) -> Value {
        json!({
            "id": self.id,
            "project": self.project,
            "owner": self.owner,
            "interval_secs": self.interval_secs,
            "monitoring": self.state,
            "heartbeat_at": self.heartbeat_at,
            "last_check_at": self.last_check_at,
            "last_success_at": self.last_success_at,
            "next_check_at": self.next_check_at,
            "event_cursor": self.event_cursor,
            "coverage": coverage,
            "delivery": {
                "configured": self.delivery_configured,
                "state": self.delivery_state,
            },
            "dispatch_enabled": self.dispatch_enabled,
            "auto_dispatch_enabled": self.auto_dispatch_enabled,
            "open_alerts": open_alerts,
            "total_alerts": total_alerts,
            "error": self.error,
            "created": self.created,
            "updated": self.updated,
        })
    }
}

impl MonitorAlert {
    pub fn to_json(&self) -> Value {
        json!({
            "seq": self.seq,
            "monitor": self.monitor_id,
            "task": self.task_id,
            "event_seq": self.event_seq,
            "fingerprint": self.fingerprint,
            "kind": self.kind,
            "payload": self.payload,
            "state": self.state,
            "attempts": self.attempts,
            "last_error": self.last_error,
            "created": self.created,
            "updated": self.updated,
        })
    }
}

impl Store {
    fn monitor_in(&self, conn: &Connection, id: &str) -> Result<Monitor> {
        conn.query_row("SELECT * FROM monitors WHERE id=?", [id], row_monitor)
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::rejected(format!("No such monitor '{id}'"))
                }
                other => other.into(),
            })
    }

    fn monitor_alert_in(&self, conn: &Connection, seq: i64) -> Result<MonitorAlert> {
        conn.query_row(
            "SELECT * FROM monitor_alerts WHERE seq=?",
            [seq],
            row_monitor_alert,
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                Error::rejected(format!("No such monitor alert '{seq}'"))
            }
            other => other.into(),
        })
    }

    fn monitor_coverage_in(&self, conn: &Connection, id: &str) -> Result<Vec<String>> {
        let mut stmt =
            conn.prepare("SELECT task_id FROM monitor_tasks WHERE monitor_id=? ORDER BY task_id")?;
        let rows = stmt.query_map([id], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn monitor_counts_in(&self, conn: &Connection, id: &str) -> Result<(i64, i64)> {
        let open: i64 = conn.query_row(
            "SELECT COUNT(*) FROM monitor_alerts WHERE monitor_id=? AND state='open'",
            [id],
            |r| r.get(0),
        )?;
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM monitor_alerts WHERE monitor_id=?",
            [id],
            |r| r.get(0),
        )?;
        Ok((open, total))
    }

    pub fn monitor_view(&self, id: &str) -> Result<(Monitor, Vec<String>, i64, i64)> {
        let conn = self.conn();
        let monitor = self.monitor_in(&conn, id)?;
        let coverage = self.monitor_coverage_in(&conn, id)?;
        let (open, total) = self.monitor_counts_in(&conn, id)?;
        Ok((monitor, coverage, open, total))
    }

    pub fn monitors(&self) -> Result<Vec<Monitor>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT * FROM monitors ORDER BY id")?;
        let rows = stmt.query_map([], row_monitor)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Register a monitor with a fixed task set. The project key must match
    /// each covered job's explicit repo binding; scope is never inferred.
    #[allow(clippy::too_many_arguments)]
    pub fn register_monitor(
        &self,
        id: &str,
        project: &str,
        owner: &str,
        interval_secs: u64,
        task_ids: &[String],
        dispatch_enabled: bool,
        auto_dispatch_enabled: bool,
    ) -> Result<(Monitor, bool)> {
        identifier(id, "Monitor id")?;
        identifier(owner, "Monitor owner")?;
        if project.trim().is_empty() || project.len() > 512 {
            return Err(Error::rejected(
                "Monitor project must contain 1-512 characters",
            ));
        }
        if !(1..=86_400).contains(&interval_secs) {
            return Err(Error::rejected(
                "Monitor interval must be between 1 and 86400 seconds",
            ));
        }
        if auto_dispatch_enabled && !dispatch_enabled {
            return Err(Error::rejected(
                "Automatic monitor dispatch requires the separate manual dispatch permission",
            ));
        }
        if task_ids.is_empty() || task_ids.len() > 256 {
            return Err(Error::rejected(
                "Monitor coverage must contain 1-256 task ids",
            ));
        }
        let mut unique = task_ids.to_vec();
        unique.sort();
        unique.dedup();
        if unique.len() != task_ids.len() {
            return Err(Error::rejected(
                "Monitor coverage must not contain duplicate task ids",
            ));
        }
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        for task_id in &unique {
            let task = self.task_in(&tx, task_id)?;
            let job = self.job_in(&tx, &task.job_id)?;
            if job.repo.as_deref() != Some(project) {
                return Err(Error::rejected(format!(
                    "Task '{task_id}' is not bound to monitor project '{project}'"
                )));
            }
        }
        if let Ok(existing) = self.monitor_in(&tx, id) {
            let existing_coverage = self.monitor_coverage_in(&tx, id)?;
            let same = existing.project == project
                && existing.owner == owner
                && existing.interval_secs == interval_secs as i64
                && existing.dispatch_enabled == dispatch_enabled
                && existing.auto_dispatch_enabled == auto_dispatch_enabled
                && existing_coverage == unique;
            if !same {
                return Err(Error::rejected(format!(
                    "Monitor '{id}' already exists with different scope or settings"
                )));
            }
            tx.commit()?;
            return Ok((existing, true));
        }
        let t = now();
        let cursor: i64 =
            tx.query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |r| r.get(0))?;
        tx.execute(
            "INSERT INTO monitors(
                id,project,owner,interval_secs,state,next_check_at,event_cursor,
                delivery_configured,delivery_state,dispatch_enabled,
                auto_dispatch_enabled,created,updated)
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                id,
                project,
                owner,
                interval_secs as i64,
                "degraded",
                t,
                cursor,
                0i64,
                "unconfigured",
                dispatch_enabled as i64,
                auto_dispatch_enabled as i64,
                t,
                t
            ],
        )?;
        for task_id in &unique {
            tx.execute(
                "INSERT INTO monitor_tasks(monitor_id,task_id) VALUES(?,?)",
                params![id, task_id],
            )?;
        }
        Self::event(
            &tx,
            Self::DAEMON_STREAM,
            "monitor_registered",
            json!({"monitor": id, "project": project,
                   "owner": owner, "coverage": unique,
                   "dispatch_enabled": dispatch_enabled,
                   "auto_dispatch_enabled": auto_dispatch_enabled}),
        )?;
        tx.commit()?;
        let monitor = self.monitor_in(&conn, id)?;
        Ok((monitor, false))
    }

    pub fn monitor(&self, id: &str) -> Result<Monitor> {
        let conn = self.conn();
        self.monitor_in(&conn, id)
    }

    pub fn monitor_is_covered(&self, id: &str, task_id: &str) -> Result<bool> {
        let conn = self.conn();
        self.monitor_in(&conn, id)?;
        Ok(conn
            .query_row(
                "SELECT 1 FROM monitor_tasks WHERE monitor_id=? AND task_id=?",
                params![id, task_id],
                |_| Ok(()),
            )
            .is_ok())
    }

    /// The fixed coverage set for a monitor.  Callers use this list for
    /// reconciliation; membership is always the stored task set and is
    /// never inferred from a project or job name.
    pub fn monitor_tasks(&self, id: &str) -> Result<Vec<String>> {
        let conn = self.conn();
        self.monitor_in(&conn, id)?;
        self.monitor_coverage_in(&conn, id)
    }

    /// Return an existing live kickoff without minting a new revision. This
    /// is the atomic duplicate-only branch used by the monitor handoff when
    /// a caller retries after the worker has claimed the first request.
    pub fn duplicate_task_dispatch(
        &self,
        task_id: &str,
    ) -> Result<Option<(Task, String, bool, bool)>> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let task = self.task_in(&tx, task_id)?;
        if !matches!(task.state.as_str(), "dispatched" | "running") {
            tx.commit()?;
            return Ok(None);
        }
        let job = self.job_in(&tx, &task.job_id)?;
        if job.state != "open" {
            tx.commit()?;
            return Ok(None);
        }
        let assignee = task.assignee.as_deref().ok_or_else(|| {
            Error::rejected(format!(
                "Task '{task_id}' has no assignee — cannot reuse its kickoff"
            ))
        })?;
        let worker = self.agent_in(&tx, assignee)?;
        self.check_group_member(&job, &worker)?;
        let live_id = task
            .dispatch_message
            .as_deref()
            .and_then(|message| self.message_in(&tx, message).ok().flatten())
            .filter(|message| !is_terminal(&message.state))
            .map(|message| message.id);
        tx.commit()?;
        Ok(live_id.map(|message| (task, message, true, false)))
    }

    /// Dispatch a covered task from the automatic monitor path. Every
    /// durable admission fact that can race another monitor tick — monitor
    /// scope, task/job state, worker identity, provider allowance evidence,
    /// pending approval snapshot, queue occupancy, and unfinished work — is
    /// read under the same SQLite transaction that enqueues the kickoff.
    ///
    /// `pending_aliases` is collected while the daemon holds its pending
    /// request mutex. The lock order is therefore pending map -> store
    /// connection, matching the other approval paths and making the
    /// approval guard part of this admission boundary.
    pub fn dispatch_automatic_monitor_task(
        &self,
        monitor_id: &str,
        task_id: &str,
        pending_aliases: &HashSet<String>,
        by: &str,
    ) -> Result<(Task, String, bool, bool)> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let monitor = self.monitor_in(&tx, monitor_id)?;
        if monitor.state != "active" {
            return Err(Error::rejected(format!(
                "Monitor '{monitor_id}' is {} — dispatch requires an active check",
                monitor.state
            )));
        }
        if !monitor.dispatch_enabled {
            return Err(Error::rejected(format!(
                "Monitor '{monitor_id}' has dispatch disabled — enable it explicitly at registration"
            )));
        }
        if !monitor.auto_dispatch_enabled {
            return Err(Error::rejected(format!(
                "Monitor '{monitor_id}' has automatic dispatch disabled — opt in explicitly at registration"
            )));
        }
        if !self
            .monitor_coverage_in(&tx, monitor_id)?
            .iter()
            .any(|covered| covered == task_id)
        {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is outside monitor '{monitor_id}' coverage"
            )));
        }

        let task = self.task_in(&tx, task_id)?;
        let job = self.job_in(&tx, &task.job_id)?;
        if job.state != "open" || job.repo.as_deref() != Some(monitor.project.as_str()) {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is not in monitor project '{}' with an open job",
                monitor.project
            )));
        }
        let assignee = task
            .assignee
            .as_deref()
            .ok_or_else(|| Error::rejected(format!("Task '{task_id}' has no explicit assignee")))?;
        let worker = self.agent_in(&tx, assignee)?;
        self.check_group_member(&job, &worker)?;

        // A retry of a still-live kickoff is idempotent and must remain
        // possible even if the worker is now busy or has a pending prompt.
        if matches!(task.state.as_str(), "dispatched" | "running") {
            let live_id = task
                .dispatch_message
                .as_deref()
                .and_then(|message| self.message_in(&tx, message).ok().flatten())
                .filter(|message| !is_terminal(&message.state))
                .map(|message| message.id);
            if let Some(live_id) = live_id {
                self.resolve_monitor_dispatch_blocked_tx(&tx, monitor_id, task_id, now(), by)?;
                tx.commit()?;
                return Ok((task, live_id, true, false));
            }
            return Err(Error::rejected(format!(
                "Task '{task_id}' has no live kickoff — only draft or revising tasks are eligible"
            )));
        }
        if !matches!(task.state.as_str(), "draft" | "revising") {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is '{}' — only draft or revising tasks are eligible",
                task.state
            )));
        }
        if task
            .acceptance
            .as_deref()
            .is_none_or(|s| s.trim().is_empty())
        {
            return Err(Error::rejected(format!(
                "Task '{task_id}' has no acceptance criteria — dispatch is refused"
            )));
        }
        if !registry::has_actor(&worker.provider, &worker.endpoint_kind) {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' is a mailbox, not a dispatchable worker"
            )));
        }
        if let Some(reason) = automatic_quota_error(&worker) {
            return Err(Error::rejected(reason));
        }
        // The fake provider is an in-process fixture and deliberately has no
        // transport endpoint. Real actors publish one when open.
        let live_endpoint = worker.endpoint.is_some()
            || (worker.provider == "fake" && worker.endpoint_kind == "fake");
        if !worker.enabled || !live_endpoint || worker.state != "idle" {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' is not demonstrably idle and live (state {}, endpoint {})",
                worker.state, live_endpoint
            )));
        }
        if registry::ready_gate(&worker.provider, &worker.endpoint_kind)
            && worker
                .params
                .as_ref()
                .and_then(|params| params.get("auto_ready"))
                .and_then(Value::as_str)
                != Some("verified")
        {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' requires an explicit readiness claim; automatic dispatch is refused"
            )));
        }
        if pending_aliases.contains(assignee) {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' is waiting on an approval request"
            )));
        }
        let queued: i64 = tx.query_row(
            "SELECT COUNT(*) FROM messages WHERE alias=? AND state='queued'",
            [assignee],
            |row| row.get(0),
        )?;
        if queued > 0 {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' has queued work; dispatch is refused"
            )));
        }
        let unfinished: Option<String> = tx
            .query_row(
                "SELECT id FROM tasks
                 WHERE assignee=? AND id<>?
                   AND state NOT IN ('verified','done','cancelled','failed')
                 ORDER BY updated LIMIT 1",
                params![assignee, task_id],
                |row| row.get(0),
            )
            .optional()?;
        if unfinished.is_some() {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' already has unfinished task work"
            )));
        }

        let revision = task.revision + 1;
        let attempt: i64 = tx.query_row(
            "SELECT COUNT(*) FROM messages WHERE task_id=? AND source='job_dispatch'",
            [task_id],
            |row| row.get(0),
        )?;
        let kickoff = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("cadence-dispatch:{task_id}:r{revision}:a{attempt}").as_bytes(),
        )
        .simple()
        .to_string();
        let body = kickoff_body(&job, &task, revision, &kickoff, &worker)?;
        let reply_to = (assignee != job.pm_alias).then_some(job.pm_alias.as_str());
        let (duplicate, _state) = self.enqueue_tx(
            &tx,
            assignee,
            &body,
            reply_to,
            &kickoff,
            "job_dispatch",
            Some(task_id),
            job.issue_id.as_deref(),
            task.worktree.as_deref(),
            &Sender::Unattributed,
            Priority::Normal,
            None,
        )?;
        if duplicate {
            tx.commit()?;
            return Ok((task, kickoff, true, false));
        }
        let at = now();
        tx.execute(
            "UPDATE tasks SET state='dispatched',revision=?,assignee=?,
             dispatch_message=?,head_sha=NULL,error=NULL,updated=?
             WHERE id=?",
            params![revision, assignee, kickoff, at, task_id],
        )?;
        Self::event_scoped(
            &tx,
            &job.pm_alias,
            "task_dispatched",
            json!({"task": task_id, "job": job.id, "assignee": assignee,
                   "revision": revision, "message": kickoff, "by": by,
                   "automatic": true}),
            Some(&job.id),
            Some(task_id),
        )?;
        self.resolve_monitor_dispatch_blocked_tx(&tx, monitor_id, task_id, at, by)?;
        let dispatched = self.task_in(&tx, task_id)?;
        tx.commit()?;
        let behind_dead = worker.endpoint.is_none()
            && registry::has_actor(&worker.provider, &worker.endpoint_kind);
        Ok((dispatched, kickoff, false, behind_dead))
    }

    pub fn monitor_heartbeat(&self, id: &str) -> Result<Monitor> {
        let conn = self.write_conn()?;
        let t = now();
        conn.execute(
            "UPDATE monitors SET heartbeat_at=?,updated=? WHERE id=? AND state<>'off'",
            params![t, t, id],
        )?;
        self.monitor_in(&conn, id)
    }

    pub fn due_monitors(&self, at: f64) -> Result<Vec<Monitor>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT * FROM monitors
             WHERE state IN ('active','degraded') AND next_check_at IS NOT NULL
               AND next_check_at<=?
             ORDER BY next_check_at,id",
        )?;
        let rows = stmt.query_map([at], row_monitor)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub(super) fn monitor_alert_kind(kind: &str) -> bool {
        matches!(
            kind,
            "turn_stalled"
                | "turn_unknown"
                | "attention"
                | "paste_not_rendered"
                | "delivery_parked"
                | "delivery_stalled"
                | "turn_silent_end"
                | "approval_menu"
                | "draft_pending"
                | "cloud_hold"
                | "cloud_recover_escalated"
        )
    }

    /// Observe one due monitor. Alert insertion and cursor advancement are
    /// one SQLite transaction: a crash can repeat a read, never a durable
    /// alert, because `(monitor_id,fingerprint)` is unique.
    pub fn check_monitor(&self, id: &str, at: f64) -> Result<MonitorCheck> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let monitor = self.monitor_in(&tx, id)?;
        if monitor.state == "off" {
            tx.commit()?;
            return Ok(MonitorCheck {
                monitor_id: id.to_string(),
                cursor: monitor.event_cursor,
                ..MonitorCheck::default()
            });
        }
        let coverage: HashSet<String> = self.monitor_coverage_in(&tx, id)?.into_iter().collect();
        let events = {
            let mut stmt = tx.prepare(
                "SELECT seq,alias,kind,payload,job_id,task_id,at FROM events
                 WHERE seq>? ORDER BY seq LIMIT 500",
            )?;
            let rows = stmt.query_map([monitor.event_cursor], row_event)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut cursor = monitor.event_cursor;
        let mut alerts_created = 0;
        for event in &events {
            cursor = cursor.max(event.seq);
            let Some(task_id) = event.task_id.as_deref() else {
                continue;
            };
            if !coverage.contains(task_id) || !Self::monitor_alert_kind(&event.kind) {
                continue;
            }
            let fingerprint = format!("event:{}", event.seq);
            let payload = json!({
                "event_seq": event.seq,
                "alias": event.alias,
                "kind": event.kind,
                "payload": event.payload,
                "job_id": event.job_id,
                "task_id": task_id,
                "at": event.at,
            });
            tx.execute(
                "INSERT OR IGNORE INTO monitor_alerts(
                    monitor_id,task_id,event_seq,fingerprint,kind,payload,
                    state,attempts,created,updated)
                 VALUES(?,?,?,?,?,?, 'open',0,?,?)",
                params![
                    id,
                    task_id,
                    event.seq,
                    fingerprint,
                    event.kind,
                    payload.to_string(),
                    at,
                    at
                ],
            )?;
            if tx.changes() == 1 {
                alerts_created += 1;
                Self::event(
                    &tx,
                    Self::DAEMON_STREAM,
                    "monitor_alert",
                    json!({"monitor": id, "task": task_id,
                           "event_seq": event.seq, "kind": event.kind,
                           "fingerprint": format!("event:{}", event.seq)}),
                )?;
            }
        }
        let next = at + monitor.interval_secs as f64;
        tx.execute(
            "UPDATE monitors SET state='active',heartbeat_at=?,last_check_at=?,
                last_success_at=?,next_check_at=?,event_cursor=?,error=NULL,updated=?
             WHERE id=? AND state<>'off'",
            params![at, at, at, next, cursor, at, id],
        )?;
        tx.commit()?;
        Ok(MonitorCheck {
            monitor_id: id.to_string(),
            scanned: events.len() as i64,
            alerts_created,
            cursor,
        })
    }

    /// Persist a failed pass without manufacturing a healthy result. The
    /// first occurrence of a reason emits one daemon event; repeated ticks
    /// update status and retry time without an event storm.
    pub fn fail_monitor_check(&self, id: &str, at: f64, error: &str) -> Result<Monitor> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let monitor = self.monitor_in(&tx, id)?;
        let changed = monitor.state != "degraded" || monitor.error.as_deref() != Some(error);
        let next = at + monitor.interval_secs as f64;
        tx.execute(
            "UPDATE monitors SET state='degraded',heartbeat_at=?,last_check_at=?,
                next_check_at=?,error=?,updated=? WHERE id=? AND state<>'off'",
            params![at, at, next, error, at, id],
        )?;
        if changed {
            Self::event(
                &tx,
                Self::DAEMON_STREAM,
                "monitor_degraded",
                json!({"monitor": id, "error": error}),
            )?;
        }
        tx.commit()?;
        self.monitor_in(&conn, id)
    }

    /// Record a guarded automatic-dispatch refusal without treating the
    /// refusal as a monitor-health failure.  One alert per `(monitor,task)`
    /// is retained and updated across reconciliation ticks; this prevents a
    /// busy/approval/quota guard from creating an alert storm while keeping
    /// the latest actionable reason durable across restart.
    pub fn record_monitor_dispatch_blocked(
        &self,
        id: &str,
        task_id: &str,
        at: f64,
        reason: &str,
    ) -> Result<MonitorAlert> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let monitor = self.monitor_in(&tx, id)?;
        let owner = monitor.owner.clone();
        if !self
            .monitor_coverage_in(&tx, id)?
            .iter()
            .any(|covered| covered == task_id)
        {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is outside monitor '{id}' coverage"
            )));
        }
        let fingerprint = format!("dispatch-blocked:{task_id}");
        let previous: Option<(i64, i64, String, Option<String>)> = tx
            .query_row(
                "SELECT seq,event_seq,state,last_error FROM monitor_alerts
                 WHERE monitor_id=? AND fingerprint=?",
                params![id, fingerprint],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let action =
            "Inspect the guard reason, resolve the explicit task/worker prerequisite, then retry";
        let seq = if let Some((seq, event_seq, state, previous_reason)) = previous {
            let next_state = match state.as_str() {
                // A later refusal is a new episode after a successful
                // dispatch; make the durable alert visible again.
                "resolved" => "open",
                "acknowledged" if previous_reason.as_deref() != Some(reason) => "open",
                state => state,
            };
            let payload = json!({
                "monitor": id,
                "task_id": task_id,
                "event_seq": event_seq,
                "reason": reason,
                "next_action": action,
                "owner": owner.clone(),
                "authority": "operator",
                "automatic": true,
                "observed_at": at,
            });
            tx.execute(
                "UPDATE monitor_alerts SET payload=?,state=?,attempts=attempts+1,
                    last_error=?,updated=? WHERE seq=?",
                params![payload.to_string(), next_state, reason, at, seq],
            )?;
            seq
        } else {
            Self::event_scoped(
                &tx,
                Self::DAEMON_STREAM,
                "monitor_dispatch_blocked",
                json!({"monitor": id, "task": task_id,
                       "reason": reason, "next_action": action,
                       "owner": owner.clone(), "automatic": true}),
                None,
                Some(task_id),
            )?;
            let event_seq = tx.last_insert_rowid();
            let payload = json!({
                "monitor": id,
                "task_id": task_id,
                "event_seq": event_seq,
                "reason": reason,
                "next_action": action,
                "owner": owner,
                "authority": "operator",
                "automatic": true,
                "observed_at": at,
            });
            tx.execute(
                "INSERT INTO monitor_alerts(
                    monitor_id,task_id,event_seq,fingerprint,kind,payload,
                    state,attempts,last_error,created,updated)
                 VALUES(?,?,?,?,?,?,'open',1,?,?,?)",
                params![
                    id,
                    task_id,
                    event_seq,
                    fingerprint,
                    "dispatch_blocked",
                    payload.to_string(),
                    reason,
                    at,
                    at
                ],
            )?;
            tx.last_insert_rowid()
        };
        tx.commit()?;
        self.monitor_alert_in(&conn, seq)
    }

    /// Close a dispatch-blocked alert once the same covered task has a
    /// durable kickoff. This is a state transition backed by the dispatch
    /// transaction for automatic work; the public method lets an explicit
    /// operator dispatch repair the same stale alert as well.
    fn resolve_monitor_dispatch_blocked_tx(
        &self,
        tx: &Connection,
        id: &str,
        task_id: &str,
        at: f64,
        by: &str,
    ) -> Result<()> {
        let fingerprint = format!("dispatch-blocked:{task_id}");
        let Some((seq, state, payload)) = tx
            .query_row(
                "SELECT seq,state,payload FROM monitor_alerts
                 WHERE monitor_id=? AND fingerprint=?",
                params![id, fingerprint],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(());
        };
        if state == "resolved" {
            return Ok(());
        }
        let mut payload = serde_json::from_str::<Value>(&payload).unwrap_or_else(|_| json!({}));
        if let Some(object) = payload.as_object_mut() {
            object.insert("resolved_at".to_string(), json!(at));
            object.insert(
                "resolution".to_string(),
                json!("automatic dispatch succeeded"),
            );
            object.insert("resolved_by".to_string(), json!(by));
        } else {
            payload = json!({
                "monitor": id,
                "task_id": task_id,
                "resolved_at": at,
                "resolution": "automatic dispatch succeeded",
                "resolved_by": by,
            });
        }
        tx.execute(
            "UPDATE monitor_alerts SET payload=?,state='resolved',last_error=NULL,
             updated=? WHERE seq=?",
            params![payload.to_string(), at, seq],
        )?;
        Self::event_scoped(
            tx,
            Self::DAEMON_STREAM,
            "monitor_dispatch_resolved",
            json!({"monitor": id, "task": task_id, "alert": seq,
                   "by": by, "automatic": by.starts_with("monitor:")}),
            None,
            Some(task_id),
        )?;
        Ok(())
    }

    pub fn resolve_monitor_dispatch_blocked(
        &self,
        id: &str,
        task_id: &str,
        at: f64,
        by: &str,
    ) -> Result<()> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        self.monitor_in(&tx, id)?;
        self.resolve_monitor_dispatch_blocked_tx(&tx, id, task_id, at, by)?;
        tx.commit()?;
        Ok(())
    }

    pub fn stop_monitor(&self, id: &str) -> Result<Monitor> {
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        self.monitor_in(&tx, id)?;
        let t = now();
        tx.execute(
            "UPDATE monitors SET state='off',next_check_at=NULL,error=NULL,updated=?
             WHERE id=?",
            params![t, id],
        )?;
        Self::event(
            &tx,
            Self::DAEMON_STREAM,
            "monitor_off",
            json!({"monitor": id}),
        )?;
        tx.commit()?;
        self.monitor_in(&conn, id)
    }

    pub fn monitor_alerts(
        &self,
        id: &str,
        after: i64,
        open_only: bool,
        limit: i64,
    ) -> Result<Vec<MonitorAlert>> {
        let conn = self.conn();
        self.monitor_in(&conn, id)?;
        let sql = if open_only {
            "SELECT * FROM monitor_alerts WHERE monitor_id=? AND seq>? AND state='open'
             ORDER BY seq LIMIT ?"
        } else {
            "SELECT * FROM monitor_alerts WHERE monitor_id=? AND seq>?
             ORDER BY seq LIMIT ?"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params![id, after, limit.clamp(1, 500)], row_monitor_alert)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn ack_monitor_alert(&self, id: &str, seq: i64, by: &str) -> Result<MonitorAlert> {
        identifier(by, "Alert acknowledger")?;
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        let alert = self.monitor_alert_in(&tx, seq)?;
        if alert.monitor_id != id {
            return Err(Error::rejected(format!(
                "Alert '{seq}' does not belong to monitor '{id}'"
            )));
        }
        if alert.state == "open" {
            tx.execute(
                "UPDATE monitor_alerts SET state='acknowledged',updated=? WHERE seq=?",
                params![now(), seq],
            )?;
            Self::event(
                &tx,
                Self::DAEMON_STREAM,
                "monitor_alert_ack",
                json!({"monitor": id, "alert": seq, "by": by}),
            )?;
        }
        tx.commit()?;
        self.monitor_alert_in(&conn, seq)
    }

    // ---- Jobs, tasks, verdicts (the work axis) ----
}
