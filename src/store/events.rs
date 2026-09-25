//! Event log, approval and verdict streams, gate records.

use crate::error::{Error, Result};
use crate::proto::identifier;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::Path;

use super::{now, Store};

/// Operator approval evidence (CAD-217) rides its own event stream.
/// The name is not a valid agent identifier (it holds a `:`), so no
/// `agent rm` can delete it, and no retention prune names it — unlike
/// the `daemon` stream, which the WAL watcher trims to its newest rows.
/// `cadence audit` reads it read-only; nothing else consults it.
pub const APPROVAL_STREAM: &str = "audit:approvals";

/// CAD-449: every verdict `report_verdict` recorded — the daemon's own
/// statement of who judged which head, written from the identity it
/// derived from the reviewer's connection. Like [`APPROVAL_STREAM`] the
/// name is no agent identifier, so no `agent rm` deletes it and no
/// retention prune names it; no RPC writes it but `report_verdict`.
pub const VERDICT_STREAM: &str = "audit:verdicts";

pub const VERDICT_RECORDED_EVENT: &str = "review_verdict";

/// CAD-405: a project's work gate keys approved by the operator.
pub const WORK_APPROVED_EVENT: &str = "project_work_approved";

/// CAD-487: a workflow's gate keys (`agent`, `depends_on`, …)
/// approved by the operator — `plan propose --workflow` matches the
/// file's digest against the latest record.
pub const WORKFLOW_APPROVED_EVENT: &str = "workflow_approved";

/// CAD-547: an installed app's structural digest (manifest envelope +
/// bindings + per-workflow gate keys + rubric/template content)
/// approved by the operator — `plan propose --workflow <app>/<wf>`
/// matches the installed digest against the latest record.
pub const APP_APPROVED_EVENT: &str = "app_approved";

/// An operator approved `action` on one exact head — see [`NewApproval`].
pub const APPROVAL_RECORDED_EVENT: &str = "approval_recorded";

/// An operator withdrew an earlier approval id. Message delivery state
/// (a cancelled or superseded queue message) never writes this.
pub const APPROVAL_REVOKED_EVENT: &str = "approval_revoked";

/// One operator approval as `Store::record_approval` persists it: the
/// operator approved `action` on the exact `head_sha` of PR `pr` in
/// `repo`, as told by `source` (who approved and where — a claim the
/// record carries, never authority by itself). The time is the event's.
pub struct NewApproval<'a> {
    /// `None` picks [`default_approval_id`], counting up past revoked
    /// or different records so a fresh approval gets a fresh id.
    pub id: Option<&'a str>,
    pub source: &'a str,
    pub action: &'a str,
    pub head_sha: &'a str,
    pub repo: &'a str,
    pub pr: u64,
}

/// `<action>-pr<N>-<head[..12]>` — the base id an approval records
/// under when the operator names none. A fresh approval after a revoke
/// becomes `<base>-2`, `<base>-3`, … (see `Store::record_approval`).
pub fn default_approval_id(action: &str, pr: u64, head: &str) -> String {
    format!("{action}-pr{pr}-{}", &head[..head.len().min(12)])
}

/// `owner/name` — the scope a merge approval is bound to.
fn approval_repo(repo: &str) -> Result<()> {
    let ok = repo.split_once('/').is_some_and(|(owner, name)| {
        let part = |p: &str| {
            !p.is_empty()
                && p.len() <= 100
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        part(owner) && part(name)
    });
    if ok {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "Approval repo '{repo}' must be owner/name"
        )))
    }
}

fn approval_text(value: &str, what: &str, max: usize) -> Result<()> {
    if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(Error::rejected(format!(
            "{what} must contain 1-{max} non-control characters"
        )));
    }
    Ok(())
}

fn approval_source(source: &str) -> Result<()> {
    approval_text(source, "Approval source", 200)?;
    // `user` is the default daemon message source, not an authenticated
    // human identity. `daemon` is the event stream identity. Neither is
    // accepted as the source of an explicit approval record.
    if ["user", "daemon"]
        .iter()
        .any(|s| source.trim().eq_ignore_ascii_case(s))
    {
        return Err(Error::rejected(
            "Approval source must identify an explicit operator; `user` and `daemon` are not proof of human approval",
        ));
    }
    Ok(())
}

fn approval_head(head_sha: &str) -> Result<()> {
    if head_sha.len() != 40
        || !head_sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::rejected(
            "Approval head must be the full 40-character lowercase hexadecimal SHA",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Event {
    pub seq: i64,
    pub alias: String,
    pub kind: String,
    pub payload: Value,
    /// Job/task the event was caused by, when it was a job operation.
    /// `job events` is one indexed query across alias rows.
    pub job_id: Option<String>,
    pub task_id: Option<String>,
    pub at: f64,
}

pub(super) fn row_event(row: &rusqlite::Row) -> rusqlite::Result<Event> {
    let payload: String = row.get("payload")?;
    Ok(Event {
        seq: row.get("seq")?,
        alias: row.get("alias")?,
        kind: row.get("kind")?,
        payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
        job_id: row.get("job_id")?,
        task_id: row.get("task_id")?,
        at: row.get("at")?,
    })
}

impl Event {
    pub fn to_json(&self) -> Value {
        json!({
            "seq": self.seq, "alias": self.alias, "kind": self.kind,
            "payload": self.payload, "job_id": self.job_id,
            "task_id": self.task_id, "at": self.at,
        })
    }
}

impl Store {
    pub(super) fn event(conn: &Connection, alias: &str, kind: &str, payload: Value) -> Result<()> {
        Self::event_scoped(conn, alias, kind, payload, None, None)
    }

    /// Event insert that additionally records the job/task the event
    /// was caused by — the `job events` view is one indexed query
    /// across these rows.
    pub(super) fn event_scoped(
        conn: &Connection,
        alias: &str,
        kind: &str,
        payload: Value,
        job_id: Option<&str>,
        task_id: Option<&str>,
    ) -> Result<()> {
        conn.execute(
            "INSERT INTO events(alias,kind,payload,job_id,task_id,at)
             VALUES(?,?,?,?,?,?)",
            params![alias, kind, payload.to_string(), job_id, task_id, now()],
        )?;
        Ok(())
    }

    /// Does `alias`'s stream hold a `kind` event naming `message_id`
    /// (`payload.message`) — e.g. the daemon's `master_dispatched`
    /// record of a master kickoff (CAD-323).
    pub fn event_names_message(&self, alias: &str, kind: &str, message_id: &str) -> Result<bool> {
        let conn = self.conn();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE alias=?1 AND kind=?2 \
             AND json_extract(payload,'$.message')=?3",
            params![alias, kind, message_id],
            |row| row.get(0),
        )?;
        Ok(n > 0)
    }

    /// CAD-405: record the operator's approval of a project's work gate
    /// keys (`project`, `digest`, `by`, `at`, …) on [`APPROVAL_STREAM`]
    /// — never pruned, and not a mailbox anything can cancel.
    pub fn record_work_approval(&self, payload: Value) -> Result<()> {
        let conn = self.write_conn()?;
        Self::event(&conn, APPROVAL_STREAM, WORK_APPROVED_EVENT, payload)
    }

    /// CAD-487: record the operator's approval of a workflow's gate
    /// keys (`project`, `name`, `digest`, `by`, `at`) on
    /// [`APPROVAL_STREAM`] — beside the work-gate approvals, keyed
    /// `"<project>/<name>"` so a project approval and a workflow
    /// approval never share a row.
    pub fn record_workflow_approval(&self, payload: Value) -> Result<()> {
        let conn = self.write_conn()?;
        Self::event(&conn, APPROVAL_STREAM, WORKFLOW_APPROVED_EVENT, payload)
    }

    /// CAD-487: the latest workflow approval per `"<project>/<name>"`.
    pub fn workflow_approvals(&self) -> Result<std::collections::HashMap<String, Value>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT payload FROM events WHERE alias=? AND kind=? ORDER BY seq")?;
        let mut rows = stmt.query(params![APPROVAL_STREAM, WORKFLOW_APPROVED_EVENT])?;
        let mut out = std::collections::HashMap::new();
        while let Some(row) = rows.next()? {
            let raw: String = row.get(0)?;
            let Ok(payload) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            match (payload["project"].as_str(), payload["name"].as_str()) {
                (Some(p), Some(n)) => {
                    out.insert(format!("{p}/{n}"), payload.clone());
                }
                _ => continue,
            }
        }
        Ok(out)
    }

    /// CAD-547: record the operator's approval of an app's structural
    /// digest (`project`, `name`, `digest`, `by`, `at`) on
    /// [`APPROVAL_STREAM`], keyed `"<project>/<name>"` beside the
    /// workflow approvals — a separate event kind, so a workflow named
    /// `a` and an app named `a` never share a row.
    pub fn record_app_approval(&self, payload: Value) -> Result<()> {
        let conn = self.write_conn()?;
        Self::event(&conn, APPROVAL_STREAM, APP_APPROVED_EVENT, payload)
    }

    /// CAD-547: the latest app approval per `"<project>/<name>"`.
    pub fn app_approvals(&self) -> Result<std::collections::HashMap<String, Value>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT payload FROM events WHERE alias=? AND kind=? ORDER BY seq")?;
        let mut rows = stmt.query(params![APPROVAL_STREAM, APP_APPROVED_EVENT])?;
        let mut out = std::collections::HashMap::new();
        while let Some(row) = rows.next()? {
            let raw: String = row.get(0)?;
            let Ok(payload) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            match (payload["project"].as_str(), payload["name"].as_str()) {
                (Some(p), Some(n)) => {
                    out.insert(format!("{p}/{n}"), payload.clone());
                }
                _ => continue,
            }
        }
        Ok(out)
    }

    /// CAD-449: record a verdict `report_verdict` accepted (`issue`,
    /// `verdict`, `sha`, `reviewer`, `report`) on [`VERDICT_STREAM`].
    pub fn record_review_verdict(&self, payload: Value) -> Result<()> {
        let conn = self.write_conn()?;
        Self::event(&conn, VERDICT_STREAM, VERDICT_RECORDED_EVENT, payload)
    }

    /// CAD-449: did `report_verdict` record exactly this verdict — the
    /// same issue, verdict, sha, reviewer and report?
    pub fn verdict_recorded(
        &self,
        issue: &str,
        verdict: &str,
        sha: &str,
        reviewer: &str,
        report: &str,
    ) -> Result<bool> {
        let conn = self.conn();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE alias=?1 AND kind=?2 \
             AND json_extract(payload,'$.issue')=?3 \
             AND json_extract(payload,'$.verdict')=?4 \
             AND json_extract(payload,'$.sha')=?5 \
             AND json_extract(payload,'$.reviewer')=?6 \
             AND json_extract(payload,'$.report')=?7",
            params![
                VERDICT_STREAM,
                VERDICT_RECORDED_EVENT,
                issue,
                verdict,
                sha,
                reviewer,
                report
            ],
            |row| row.get(0),
        )?;
        Ok(n > 0)
    }

    /// CAD-405: the latest work-gate approval per project.
    pub fn work_approvals(&self) -> Result<std::collections::HashMap<String, Value>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT payload FROM events WHERE alias=? AND kind=? ORDER BY seq")?;
        let mut rows = stmt.query(params![APPROVAL_STREAM, WORK_APPROVED_EVENT])?;
        let mut out = std::collections::HashMap::new();
        while let Some(row) = rows.next()? {
            let raw: String = row.get(0)?;
            let Ok(payload) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            if let Some(project) = payload["project"].as_str() {
                out.insert(project.to_string(), payload.clone());
            }
        }
        Ok(out)
    }

    /// Standalone event insert for runtime/daemon bookkeeping.
    pub fn event_public(&self, alias: &str, kind: &str, payload: Value) -> Result<()> {
        let conn = self.write_conn()?;
        Self::event(&conn, alias, kind, payload)
    }

    /// The first approval event of `kind` naming approval `id` on
    /// [`APPROVAL_STREAM`]. The stream is not a mailbox, so deleting or
    /// cancelling a message can neither remove nor revoke an approval.
    fn approval_event(conn: &Connection, kind: &str, id: &str) -> Result<Option<Value>> {
        let mut stmt =
            conn.prepare("SELECT payload FROM events WHERE alias=? AND kind=? ORDER BY seq")?;
        let mut rows = stmt.query(params![APPROVAL_STREAM, kind])?;
        while let Some(row) = rows.next()? {
            let raw: String = row.get(0)?;
            let Ok(payload) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            if payload["approval_id"].as_str() == Some(id) {
                return Ok(Some(payload));
            }
        }
        Ok(None)
    }

    /// Persist an explicit operator approval as audit evidence
    /// (CAD-217). Evidence only: dispatch, task acceptance and merge
    /// policy never consult it. `recorded_via` is the daemon's own
    /// statement of how the writer was authorized — never caller input.
    ///
    /// Answers `(new, id)`. An identical re-send of a live (unrevoked)
    /// approval dedupes (`new == false`). Ids are never reused: an
    /// explicit id that names different evidence, or that was revoked,
    /// is refused; with no explicit id the default base counts up
    /// (`<base>-2`, …) past revoked or different records, so
    /// re-approving after a revoke records a fresh approval.
    pub fn record_approval(&self, a: &NewApproval, recorded_via: &str) -> Result<(bool, String)> {
        approval_source(a.source)?;
        identifier(a.action, "Approval action")?;
        approval_head(a.head_sha)?;
        approval_repo(a.repo)?;
        if a.pr == 0 {
            return Err(Error::rejected("Approval PR number must be positive"));
        }
        let base = match a.id {
            Some(id) => id.to_string(),
            None => default_approval_id(a.action, a.pr, a.head_sha),
        };
        identifier(&base, "Approval id")?;
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        for n in 1..=1000u32 {
            let id = if n == 1 {
                base.clone()
            } else {
                format!("{base}-{n}")
            };
            identifier(&id, "Approval id")?;
            let evidence = json!({
                "approval_id": id,
                "source": a.source,
                "action": a.action,
                "head_sha": a.head_sha,
                "scope": {"repo": a.repo, "pr": a.pr},
            });
            let Some(old) = Self::approval_event(&tx, APPROVAL_RECORDED_EVENT, &id)? else {
                let mut payload = evidence;
                payload["recorded_via"] = json!(recorded_via);
                Self::event(&tx, APPROVAL_STREAM, APPROVAL_RECORDED_EVENT, payload)?;
                tx.commit()?;
                return Ok((true, id));
            };
            let same = ["source", "action", "head_sha", "scope"]
                .iter()
                .all(|k| old[k] == evidence[k]);
            let revoked = Self::approval_event(&tx, APPROVAL_REVOKED_EVENT, &id)?.is_some();
            match (a.id.is_some(), revoked, same) {
                (_, false, true) => return Ok((false, id)),
                (true, true, _) => {
                    return Err(Error::rejected(format!(
                        "Approval id '{id}' was revoked — an approval id is never \
                         reused; pass a new --id, or omit --id for a fresh default id"
                    )))
                }
                (true, false, false) => {
                    return Err(Error::rejected(format!(
                        "Approval id '{id}' already names different evidence"
                    )))
                }
                (false, _, _) => continue,
            }
        }
        Err(Error::rejected(format!(
            "Approval id '{base}': no free default id — pass --id"
        )))
    }

    /// Persist an explicit operator revocation of approval `id`. It must
    /// name a recorded approval; identical retries dedupe and a second,
    /// different revocation of the same id is refused. Message delivery
    /// state never reaches this — cancelling a message is not revoking.
    pub fn revoke_approval(
        &self,
        id: &str,
        source: &str,
        reason: &str,
        recorded_via: &str,
    ) -> Result<bool> {
        identifier(id, "Approval id")?;
        approval_source(source)?;
        approval_text(reason, "Approval revocation reason", 256)?;
        let evidence = json!({"approval_id": id, "source": source, "reason": reason});
        let conn = self.write_conn()?;
        let tx = conn.unchecked_transaction()?;
        if Self::approval_event(&tx, APPROVAL_RECORDED_EVENT, id)?.is_none() {
            return Err(Error::rejected(format!(
                "Approval id '{id}' has no recorded approval to revoke"
            )));
        }
        if let Some(old) = Self::approval_event(&tx, APPROVAL_REVOKED_EVENT, id)? {
            let same = ["approval_id", "source", "reason"]
                .iter()
                .all(|k| old[k] == evidence[k]);
            if same {
                return Ok(false);
            }
            return Err(Error::rejected(format!(
                "Approval id '{id}' already names different revocation evidence"
            )));
        }
        let mut payload = evidence;
        payload["recorded_via"] = json!(recorded_via);
        Self::event(&tx, APPROVAL_STREAM, APPROVAL_REVOKED_EVENT, payload)?;
        tx.commit()?;
        Ok(true)
    }

    /// `event_public` with job/task scope — the stall watch uses it so
    /// `turn_stalled`/`turn_resumed` surface in `job events`, not only
    /// the agent's own stream.
    pub fn event_public_scoped(
        &self,
        alias: &str,
        kind: &str,
        payload: Value,
        job_id: Option<&str>,
        task_id: Option<&str>,
    ) -> Result<()> {
        let conn = self.write_conn()?;
        Self::event_scoped(&conn, alias, kind, payload, job_id, task_id)
    }

    /// Record the build commit this daemon process is running.
    pub fn record_running_build(&self, commit: &str) -> Result<()> {
        let conn = self.write_conn()?;
        crate::rollout::upsert_daemon_build(&conn, commit, crate::rollout::unix_now())
    }

    /// Refuse to keep running when this binary's commit is not the one
    /// the daemon last recorded, unless the caller holds the lease.
    pub fn enforce_running_build(&self) -> Result<()> {
        let conn = self.write_conn()?;
        crate::rollout::enforce_running_build(&conn)
    }

    /// Fold a refused migration's side log into the daemon event stream.
    /// The refusal itself cannot be inserted into the database it is
    /// refusing to modify.
    pub fn ingest_rollout_gate(&self, state_dir: &Path) -> Result<()> {
        let conn = self.write_conn()?;
        crate::rollout::ingest_gate_log(state_dir, &conn)?;
        Ok(())
    }

    /// Event log page for the `events` API; cursor is the last seq seen.
    pub fn events(&self, alias: &str, after: i64, limit: i64) -> Result<Vec<Event>> {
        let conn = self.conn();
        self.events_alias_in(&conn, alias)?;
        let mut stmt = conn.prepare(
            "SELECT seq,alias,kind,payload,job_id,task_id,at FROM events
             WHERE alias=? AND seq>? ORDER BY seq LIMIT ?",
        )?;
        let rows = stmt.query_map(params![alias, after, limit], row_event)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Event streams that exist without an `agents` row — the daemon
    /// writes `wal_checkpointed` here. Only the events read path
    /// accepts it; sends still require a registered alias.
    pub const DAEMON_STREAM: &str = "daemon";

    fn events_alias_in(&self, conn: &Connection, alias: &str) -> Result<()> {
        if alias == Self::DAEMON_STREAM {
            return Ok(());
        }
        self.agent_in(conn, alias).map(|_| ())
    }

    /// The `job events` view: every scoped event for the job across
    /// alias rows, ordered. One indexed query — no separate stream.
    pub fn job_events(&self, job_id: &str, after: i64, limit: i64) -> Result<Vec<Event>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT seq,alias,kind,payload,job_id,task_id,at FROM events
             WHERE job_id=? AND seq>? ORDER BY seq LIMIT ?",
        )?;
        let rows = stmt.query_map(params![job_id, after, limit], row_event)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The newest `limit` events for an alias, oldest first — the
    /// default `cadence events` page. The DESC scan is what the seq
    /// index gives for free; reversing costs one Vec pass.
    pub fn events_tail(&self, alias: &str, limit: i64) -> Result<Vec<Event>> {
        let conn = self.conn();
        self.events_alias_in(&conn, alias)?;
        let mut stmt = conn.prepare(
            "SELECT seq,alias,kind,payload,job_id,task_id,at FROM events
             WHERE alias=? ORDER BY seq DESC LIMIT ?",
        )?;
        let rows = stmt.query_map(params![alias, limit], row_event)?;
        let mut events: Vec<Event> = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        events.reverse();
        Ok(events)
    }

    /// The newest event of any of `kinds` for `alias` — the pty lane
    /// reads its recorded pane-root identity and whether that tree was
    /// already reaped this way (CAD-201). Events outlive the endpoint
    /// fields `set_state_detached` clears, so a fenced pane's identity
    /// is still here when `agent stop` comes.
    pub fn last_event_of(&self, alias: &str, kinds: &[&str]) -> Result<Option<Event>> {
        let conn = self.conn();
        let placeholders = kinds.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT seq,alias,kind,payload,job_id,task_id,at FROM events
             WHERE alias=? AND kind IN ({placeholders}) ORDER BY seq DESC LIMIT 1"
        );
        let mut args: Vec<&dyn rusqlite::ToSql> = vec![&alias];
        args.extend(kinds.iter().map(|k| k as &dyn rusqlite::ToSql));
        match conn.query_row(&sql, args.as_slice(), row_event) {
            Ok(event) => Ok(Some(event)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(other) => Err(other.into()),
        }
    }

    /// The newest `limit` events in the job view, oldest first —
    /// `events_tail` for the `job events` stream.
    pub fn job_events_tail(&self, job_id: &str, limit: i64) -> Result<Vec<Event>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT seq,alias,kind,payload,job_id,task_id,at FROM events
             WHERE job_id=? ORDER BY seq DESC LIMIT ?",
        )?;
        let rows = stmt.query_map(params![job_id, limit], row_event)?;
        let mut events: Vec<Event> = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        events.reverse();
        Ok(events)
    }

    /// Latest event seq for `agent_show`'s cursor.
    pub fn event_cursor(&self, alias: &str) -> Result<i64> {
        let conn = self.conn();
        let cursor: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq),0) FROM events WHERE alias=?",
            [alias],
            |r| r.get(0),
        )?;
        Ok(cursor)
    }

    // ---- Daemon-owned monitors and local alerts (CAD-176) ----

    /// When `kind` last fired on `alias`'s event stream, if ever.
    pub fn last_event_at(&self, alias: &str, kind: &str) -> Result<Option<f64>> {
        let conn = self.conn();
        Ok(conn.query_row(
            "SELECT MAX(at) FROM events WHERE alias=? AND kind=?",
            params![alias, kind],
            |row| row.get(0),
        )?)
    }

    /// CAD-96: per alias, the newest event of any of `kinds` — one
    /// grouped pass, so `agent_list` can tell an auto-stopped agent from
    /// a manually stopped one without a scan per row.
    pub fn last_events_of_all(
        &self,
        kinds: &[&str],
    ) -> Result<std::collections::HashMap<String, Event>> {
        let conn = self.conn();
        let placeholders = kinds.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT e.seq,e.alias,e.kind,e.payload,e.job_id,e.task_id,e.at FROM events e
             JOIN (SELECT MAX(seq) AS seq FROM events WHERE kind IN ({placeholders})
                   GROUP BY alias) m ON e.seq = m.seq"
        );
        let args: Vec<&dyn rusqlite::ToSql> =
            kinds.iter().map(|k| k as &dyn rusqlite::ToSql).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(args.as_slice(), row_event)?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let event = row?;
            out.insert(event.alias.clone(), event);
        }
        Ok(out)
    }
}
