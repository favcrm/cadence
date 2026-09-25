//! Jobs, tasks and verdicts.

use crate::adapter::registry;
use crate::error::{Error, Result};
use crate::proto::identifier;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use uuid::Uuid;

use super::agents::Agent;
use super::kickoff::{check_commit_sha, criteria_too_long, flatten_controls, kickoff_body};
use super::messages::{row_message, Message, Priority};
use super::{is_task_terminal, is_terminal, now, take_bytes, Sender, Store};

/// The `jobs.state` vocabulary for `job list --state`.
pub const JOB_STATES: &[&str] = &["draft", "open", "done", "failed", "cancelled"];

/// A job: the rollup container binding a spec, a PM (group root) and a
/// set of tasks. `state`: `draft|open|done|failed|cancelled`.
#[derive(Debug, Clone)]
pub struct Job {
    pub id: String,
    pub title: Option<String>,
    pub spec_path: String,
    pub spec_sha256: Option<String>,
    pub pm_alias: String,
    /// Board issue (`<PREFIX>-<n>`), grammar-validated only — the
    /// daemon never reads the board filesystem.
    pub issue_id: Option<String>,
    pub repo: Option<String>,
    pub base_ref: Option<String>,
    pub state: String,
    pub max_revisions: i64,
    /// Silence budget for turns this job's kickoffs start: over it the
    /// turn is reported stalled. NULL → the assignee's `stall_secs`
    /// param, then the daemon default.
    pub stall_secs: Option<i64>,
    pub error: Option<String>,
    pub created: f64,
    pub updated: f64,
}

/// A task: the dispatch/QA unit inside a job. `revision` counts
/// attempts; each attempt is one kickoff message (`dispatch_message`).
#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub job_id: String,
    pub title: Option<String>,
    pub role: String,
    pub assignee: Option<String>,
    pub spec_path: Option<String>,
    pub acceptance: Option<String>,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub base_sha: Option<String>,
    /// The worker's reported commit for the current revision — the
    /// only SHA a verdict may name. NULL until reported.
    pub head_sha: Option<String>,
    pub state: String,
    pub revision: i64,
    pub dispatch_message: Option<String>,
    pub error: Option<String>,
    pub created: f64,
    pub updated: f64,
}

/// A verdict: the QA record bound to one exact `(task, revision, sha)`.
/// Rows are append-only — re-dispatch starts a new revision and old
/// verdicts remain as the audit trail.
#[derive(Debug, Clone)]
pub struct Verdict {
    pub seq: i64,
    pub task_id: String,
    pub revision: i64,
    pub sha: String,
    pub verdict: String,
    pub reviewer: String,
    pub evidence: Option<Value>,
    pub message: Option<String>,
    /// The CLI's worktree-verification result (`{checked, skipped}`)
    /// bound to this verdict — NULL on unscoped tasks and on verdicts
    /// recorded before CAD-51.
    pub verify: Option<Value>,
    pub created: f64,
}

/// The verdict that applies to `revision`: the highest `seq` among rows
/// for that revision. A later reopen can leave higher revision numbers
/// in the history; those are not the current verdict.
pub fn current_verdict(revision: i64, verdicts: &[Verdict]) -> Option<&Verdict> {
    verdicts
        .iter()
        .filter(|v| v.revision == revision)
        .max_by_key(|v| v.seq)
}

fn row_job(row: &rusqlite::Row) -> rusqlite::Result<Job> {
    Ok(Job {
        id: row.get("id")?,
        title: row.get("title")?,
        spec_path: row.get("spec_path")?,
        spec_sha256: row.get("spec_sha256")?,
        pm_alias: row.get("pm_alias")?,
        issue_id: row.get("issue_id")?,
        repo: row.get("repo")?,
        base_ref: row.get("base_ref")?,
        state: row.get("state")?,
        max_revisions: row.get("max_revisions")?,
        stall_secs: row.get("stall_secs")?,
        error: row.get("error")?,
        created: row.get("created")?,
        updated: row.get("updated")?,
    })
}

fn row_task(row: &rusqlite::Row) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get("id")?,
        job_id: row.get("job_id")?,
        title: row.get("title")?,
        role: row.get("role")?,
        assignee: row.get("assignee")?,
        spec_path: row.get("spec_path")?,
        acceptance: row.get("acceptance")?,
        worktree: row.get("worktree")?,
        branch: row.get("branch")?,
        base_sha: row.get("base_sha")?,
        head_sha: row.get("head_sha")?,
        state: row.get("state")?,
        revision: row.get("revision")?,
        dispatch_message: row.get("dispatch_message")?,
        error: row.get("error")?,
        created: row.get("created")?,
        updated: row.get("updated")?,
    })
}

fn row_verdict(row: &rusqlite::Row) -> rusqlite::Result<Verdict> {
    let evidence: Option<String> = row.get("evidence")?;
    let verify: Option<String> = row.get("verify")?;
    Ok(Verdict {
        seq: row.get("seq")?,
        task_id: row.get("task_id")?,
        revision: row.get("revision")?,
        sha: row.get("sha")?,
        verdict: row.get("verdict")?,
        reviewer: row.get("reviewer")?,
        evidence: evidence.and_then(|e| serde_json::from_str(&e).ok()),
        message: row.get("message")?,
        verify: verify.and_then(|v| serde_json::from_str(&v).ok()),
        created: row.get("created")?,
    })
}

impl Job {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "title": self.title, "spec_path": self.spec_path,
            "spec_sha256": self.spec_sha256, "pm": self.pm_alias,
            "issue": self.issue_id, "repo": self.repo, "base_ref": self.base_ref,
            "state": self.state, "max_revisions": self.max_revisions,
            "stall_secs": self.stall_secs,
            "error": self.error, "created": self.created, "updated": self.updated,
        })
    }
}

impl Task {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "job": self.job_id, "title": self.title,
            "role": self.role, "assignee": self.assignee,
            "spec_path": self.spec_path, "acceptance": self.acceptance,
            "worktree": self.worktree, "branch": self.branch,
            "base_sha": self.base_sha, "head_sha": self.head_sha,
            "state": self.state, "revision": self.revision,
            "dispatch_message": self.dispatch_message, "error": self.error,
            "created": self.created, "updated": self.updated,
        })
    }
}

impl Verdict {
    pub fn to_json(&self) -> Value {
        json!({
            "seq": self.seq, "task": self.task_id, "revision": self.revision,
            "sha": self.sha, "verdict": self.verdict,
            "reviewer": self.reviewer, "evidence": self.evidence,
            "message": self.message, "verify": self.verify,
            "created": self.created,
        })
    }
}

impl Store {
    pub(super) fn job_in(&self, conn: &Connection, id: &str) -> Result<Job> {
        conn.query_row("SELECT * FROM jobs WHERE id=?", [id], row_job)
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::rejected(format!("No such job '{id}'"))
                }
                other => other.into(),
            })
    }

    pub(super) fn task_in(&self, conn: &Connection, id: &str) -> Result<Task> {
        conn.query_row("SELECT * FROM tasks WHERE id=?", [id], row_task)
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::rejected(format!("No such task '{id}'"))
                }
                other => other.into(),
            })
    }

    pub fn job(&self, id: &str) -> Result<Job> {
        let conn = self.conn();
        self.job_in(&conn, id)
    }

    pub fn task(&self, id: &str) -> Result<Task> {
        let conn = self.conn();
        self.task_in(&conn, id)
    }

    /// CAD-160 (ADR-0002 phase 1): the body a task-bound message (`send
    /// --task`, `ask --task`) is delivered as. For an open task it is
    /// the sender's text, then the task's objective, then every
    /// still-unchecked acceptance criterion — a steering message
    /// restates what the worker is still on the hook for, so it cannot
    /// read as a replacement. Only the task's assignee is on the hook:
    /// a message to anyone else (a worker's note to its PM), or bound
    /// to an unassigned or terminal task, is `text` unchanged. A
    /// composed message needs non-blank text — the amendment comes
    /// first. Criteria are never cut: the restated objective gives way
    /// first, and a body that still exceeds `ceiling` refuses naming
    /// the ceiling and the spec file.
    pub fn compose_task_message(
        &self,
        task_id: &str,
        recipient: &str,
        text: &str,
        ceiling: usize,
    ) -> Result<String> {
        let task = self.task(task_id)?;
        if is_task_terminal(&task.state) || task.assignee.as_deref() != Some(recipient) {
            return Ok(text.to_string());
        }
        if text.trim().is_empty() {
            return Err(Error::rejected(
                "Prompt must contain 1-48000 characters — a message to an open task \
                 needs non-blank text before its restated objective and criteria",
            ));
        }
        let job = self.job(&task.job_id)?;
        let spec = task.spec_path.as_deref().unwrap_or(&job.spec_path);
        let outstanding = crate::issue::dispatch::outstanding_items(task.acceptance.as_deref());
        let criteria = if outstanding.is_empty() {
            "none".to_string()
        } else {
            crate::issue::dispatch::inline_listing(&outstanding)
        };
        let objective = flatten_controls(
            task.title
                .as_deref()
                .or(job.title.as_deref())
                .unwrap_or("implement per spec"),
        );
        let build = |objective: &str| {
            format!(
                "{text} — Task {} (job {}) is still open; this message amends it and does \
                 not replace it. Objective: {objective}. Spec: {}. Outstanding criteria: \
                 {criteria}.",
                task.id,
                job.id,
                flatten_controls(spec)
            )
        };
        let full = build(&objective);
        let over = full.len().saturating_sub(ceiling);
        if over == 0 {
            return Ok(full);
        }
        const CUT: &str = "…";
        if let Some(keep) = objective.len().checked_sub(over + CUT.len()) {
            return Ok(build(&format!("{}{CUT}", take_bytes(&objective, keep))));
        }
        let total = full.len() - objective.len();
        Err(criteria_too_long(
            &task.id,
            "message",
            total,
            criteria.len(),
            ceiling,
            spec,
        ))
    }

    pub fn task_opt(&self, id: &str) -> Result<Option<Task>> {
        let conn = self.conn();
        match conn.query_row("SELECT * FROM tasks WHERE id=?", [id], row_task) {
            Ok(t) => Ok(Some(t)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Jobs for `job list` — non-terminal by default, `all` includes
    /// done/cancelled/failed; `state` filters exactly.
    pub fn jobs(&self, state: Option<&str>, all: bool) -> Result<Vec<Job>> {
        let conn = self.conn();
        let sql = if state.is_some() {
            "SELECT * FROM jobs WHERE state=? ORDER BY created"
        } else if all {
            "SELECT * FROM jobs ORDER BY created"
        } else {
            "SELECT * FROM jobs WHERE state NOT IN ('done','cancelled','failed')
             ORDER BY created"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = if let Some(s) = state {
            stmt.query_map([s], row_job)?
        } else {
            stmt.query_map([], row_job)?
        };
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn tasks_for_job(&self, job_id: &str) -> Result<Vec<Task>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT * FROM tasks WHERE job_id=? ORDER BY created")?;
        let rows = stmt.query_map([job_id], row_task)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// An alias's non-terminal task assignments — derived, never stored.
    pub fn tasks_for_assignee(&self, alias: &str) -> Result<Vec<Task>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT * FROM tasks WHERE assignee=?
             AND state NOT IN ('verified','done','cancelled','failed')
             ORDER BY updated",
        )?;
        let rows = stmt.query_map([alias], row_task)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn verdicts_for_task(&self, task_id: &str) -> Result<Vec<Verdict>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT * FROM verdicts WHERE task_id=? ORDER BY revision, seq")?;
        let rows = stmt.query_map([task_id], row_verdict)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every message attached to a task (kickoffs + `--task` sends),
    /// oldest first — `job task show`'s delivery view.
    pub fn messages_for_task(&self, task_id: &str) -> Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT * FROM messages WHERE task_id=? ORDER BY seq")?;
        let rows = stmt.query_map([task_id], row_message)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// `job new`: bookkeeping, not spawning. One transaction writes the
    /// job (`open`) plus its default task `<job>-t1` covering the spec.
    /// Idempotent on the client key: same id + same spec hash +
    /// same PM + same issue → `duplicate:true`; any difference →
    /// `rejected`, the same rule `enqueue` uses for message ids.
    /// One leaf issue maps to at most one non-terminal job.
    #[allow(clippy::too_many_arguments)]
    pub fn create_job(
        &self,
        id: &str,
        title: Option<&str>,
        spec_path: &str,
        spec_sha256: &str,
        pm_alias: &str,
        issue_id: Option<&str>,
        repo: Option<&str>,
        base_ref: Option<&str>,
        max_revisions: i64,
        stall_secs: Option<i64>,
        task_title: Option<&str>,
        task_worktree: Option<&str>,
        task_branch: Option<&str>,
        task_base_sha: Option<&str>,
        task_assignee: Option<&str>,
        task_acceptance: Option<&str>,
    ) -> Result<(bool, Job)> {
        identifier(id, "Job id")?;
        if let Some(issue) = issue_id {
            crate::issue::model::check_id(issue)?;
        }
        if max_revisions < 0 {
            return Err(Error::rejected("--max-revisions must be >= 0"));
        }
        if stall_secs.is_some_and(|s| s < 0) {
            return Err(Error::rejected("--stall-secs must be >= 0"));
        }
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        self.agent_in(&tx, pm_alias)?;
        if let Ok(existing) = self.job_in(&tx, id) {
            let same = existing.pm_alias == pm_alias
                && existing.spec_path == spec_path
                && existing.spec_sha256.as_deref() == Some(spec_sha256)
                && existing.issue_id.as_deref() == issue_id;
            if !same {
                return Err(Error::rejected(
                    "Job id was already used with different content",
                ));
            }
            return Ok((true, existing));
        }
        if let Some(issue) = issue_id {
            let holder: Option<String> = tx
                .query_row(
                    "SELECT id FROM jobs WHERE issue_id=?
                     AND state NOT IN ('done','cancelled','failed')",
                    [issue],
                    |r| r.get(0),
                )
                .ok();
            if let Some(holder) = holder {
                return Err(Error::rejected(format!(
                    "Issue {issue} already maps to job '{holder}' — \
                     one leaf issue maps to one job"
                )));
            }
        }
        let t = now();
        tx.execute(
            "INSERT INTO jobs(id,title,spec_path,spec_sha256,pm_alias,issue_id,
             repo,base_ref,state,max_revisions,stall_secs,created,updated)
             VALUES(?,?,?,?,?,?,?,?,'open',?,?,?,?)",
            params![
                id,
                title,
                spec_path,
                spec_sha256,
                pm_alias,
                issue_id,
                repo,
                base_ref,
                max_revisions,
                stall_secs,
                t,
                t
            ],
        )?;
        Self::event_scoped(
            &tx,
            pm_alias,
            "job_created",
            json!({"job": id, "spec": spec_path, "issue": issue_id}),
            Some(id),
            None,
        )?;
        if let Some(a) = task_assignee {
            let job = self.job_in(&tx, id)?;
            let worker = self.agent_in(&tx, a)?;
            self.check_group_member(&job, &worker)?;
        }
        let task_id = format!("{id}-t1");
        tx.execute(
            "INSERT INTO tasks(id,job_id,title,assignee,acceptance,worktree,
             branch,base_sha,state,created,updated)
             VALUES(?,?,?,?,?,?,?,?,'draft',?,?)",
            params![
                task_id,
                id,
                task_title.or(title),
                task_assignee,
                task_acceptance,
                task_worktree,
                task_branch,
                task_base_sha,
                t,
                t
            ],
        )?;
        Self::event_scoped(
            &tx,
            pm_alias,
            "task_created",
            json!({"task": task_id, "job": id}),
            Some(id),
            Some(&task_id),
        )?;
        tx.commit()?;
        Ok((false, self.job_in(&conn, id)?))
    }

    /// `job task add`: a draft task in an open job. `--assignee` is
    /// validated against the job's group immediately — eligible workers
    /// are the PM itself or agents whose `params.upstream` names it.
    #[allow(clippy::too_many_arguments)]
    pub fn create_task(
        &self,
        job_id: &str,
        id: &str,
        title: Option<&str>,
        assignee: Option<&str>,
        spec_path: Option<&str>,
        acceptance: Option<&str>,
        worktree: Option<&str>,
        branch: Option<&str>,
        base_sha: Option<&str>,
    ) -> Result<Task> {
        identifier(id, "Task id")?;
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let job = self.job_in(&tx, job_id)?;
        if job.state != "open" {
            return Err(Error::rejected(format!(
                "Job '{job_id}' is '{}' — tasks can only be added to an open job",
                job.state
            )));
        }
        if self.task_opt_in(&tx, id)? {
            return Err(Error::rejected(format!(
                "Task id '{id}' is already used — task ids are global"
            )));
        }
        if let Some(w) = assignee {
            let worker = self.agent_in(&tx, w)?;
            self.check_group_member(&job, &worker)?;
        }
        let t = now();
        tx.execute(
            "INSERT INTO tasks(id,job_id,title,assignee,spec_path,acceptance,
             worktree,branch,base_sha,state,created,updated)
             VALUES(?,?,?,?,?,?,?,?,?,'draft',?,?)",
            params![
                id, job_id, title, assignee, spec_path, acceptance, worktree, branch, base_sha, t,
                t
            ],
        )?;
        Self::event_scoped(
            &tx,
            &job.pm_alias,
            "task_created",
            json!({"task": id, "job": job_id, "assignee": assignee}),
            Some(job_id),
            Some(id),
        )?;
        tx.commit()?;
        self.task_in(&conn, id)
    }

    fn task_opt_in(&self, conn: &Connection, id: &str) -> Result<bool> {
        Ok(conn
            .query_row("SELECT 1 FROM tasks WHERE id=?", [id], |_| Ok(()))
            .is_ok())
    }

    /// Eligible workers for a job: the PM itself or a group member
    /// (`params.upstream == pm`). Dispatch can never bind outside the
    /// job's group — routed results land on the PM via that same wire.
    pub(super) fn check_group_member(&self, job: &Job, worker: &Agent) -> Result<()> {
        let member = worker.alias == job.pm_alias
            || worker
                .params
                .as_ref()
                .and_then(|p| p.get("upstream"))
                .and_then(Value::as_str)
                == Some(job.pm_alias.as_str());
        if !member {
            return Err(Error::rejected(format!(
                "'{}' is not in job '{}'s group — join it first: \
                 `cadence join {} <provider>`",
                worker.alias, job.id, job.pm_alias
            )));
        }
        Ok(())
    }

    /// `job dispatch`: one transaction — task `dispatched` at a new (or
    /// retried) revision, the kickoff message enqueued, events scoped.
    ///
    /// Dispatch is legal from `draft`, `revising`, and — once the live
    /// kickoff is terminal for any reason other than normal completion
    /// — `dispatched`/`running`. A live kickoff makes re-dispatch an
    /// idempotent retry of the SAME revision (deterministic
    /// `cadence-dispatch:<task>:r<n>` id → `duplicate`, never a second
    /// paste). `blocked` requires `job task reopen`, unless `--to`
    /// reassigns — a new assignee is a fresh QA chain. `verified`,
    /// `done`, `failed`, `cancelled` reject.
    ///
    /// Returns `(task, kickoff_message_id, duplicate, queued_behind_dead)`.
    pub fn dispatch_task(
        &self,
        task_id: &str,
        to: Option<&str>,
        message_id: Option<&str>,
        by: &str,
    ) -> Result<(Task, String, bool, bool)> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let task = self.task_in(&tx, task_id)?;
        let job = self.job_in(&tx, &task.job_id)?;
        if job.state != "open" {
            return Err(Error::rejected(format!(
                "Job '{}' is '{}' — dispatch needs an open job",
                job.id, job.state
            )));
        }
        let assignee = to.or(task.assignee.as_deref()).ok_or_else(|| {
            Error::rejected(format!(
                "Task '{task_id}' has no assignee — \
                 `cadence job dispatch {task_id} --to <worker>`"
            ))
        })?;
        let worker = self.agent_in(&tx, assignee)?;
        self.check_group_member(&job, &worker)?;

        // Same-revision retry vs new revision.
        let mut revision = task.revision;
        match task.state.as_str() {
            "draft" | "revising" => revision += 1,
            "dispatched" | "running" => {
                let live_id = task
                    .dispatch_message
                    .as_deref()
                    .and_then(|m| self.message_in(&tx, m).ok().flatten())
                    .filter(|m| !is_terminal(&m.state))
                    .map(|m| m.id);
                if let Some(live_id) = live_id {
                    // Kickoff still in flight — a second dispatch is a
                    // retry of this revision, not a new attempt: the
                    // live kickoff id IS the dedupe key. Reassigning
                    // under a live kickoff is refused — the pane may
                    // already hold the paste.
                    if to.is_some() && to != task.assignee.as_deref() {
                        return Err(Error::rejected(format!(
                            "Task '{task_id}' has a live kickoff — reassign \
                             after it finishes or is reconciled"
                        )));
                    }
                    tx.commit()?;
                    return Ok((task, live_id, true, false));
                }
                revision += 1;
            }
            "blocked" => {
                if to.is_some() && to != task.assignee.as_deref() {
                    revision += 1;
                } else {
                    return Err(Error::rejected(format!(
                        "Task '{task_id}' is blocked — `cadence job task reopen \
                         {task_id}` re-scopes it, or `job dispatch {task_id} \
                         --to <worker>` reassigns"
                    )));
                }
            }
            state => {
                return Err(Error::rejected(format!(
                    "Task '{task_id}' is '{state}' — dispatch is legal from \
                     draft, revising, or after the live kickoff ended"
                )))
            }
        }

        // Deterministic kickoff id for a fresh mint: (task, revision,
        // attempt). Live-kickoff retries never reach here — they return
        // the live id above — so this only needs uniqueness across
        // re-scopes: `job task reopen` resets revision to 0, and
        // `attempt` (kickoffs already written) keeps the id fresh.
        let attempt: i64 = tx.query_row(
            "SELECT COUNT(*) FROM messages WHERE task_id=? AND source='job_dispatch'",
            [task_id],
            |r| r.get(0),
        )?;
        let kickoff = message_id.map(str::to_string).unwrap_or_else(|| {
            Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("cadence-dispatch:{task_id}:r{revision}:a{attempt}").as_bytes(),
            )
            .simple()
            .to_string()
        });
        let body = kickoff_body(&job, &task, revision, &kickoff, &worker)?;
        // PM self-task: the PM's own turn IS the report path — a
        // reply_to to itself would fail enqueue's self-reply rule.
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
        )?;
        if duplicate {
            return Ok((task, kickoff, true, false));
        }
        tx.execute(
            "UPDATE tasks SET state='dispatched',revision=?,assignee=?,
             dispatch_message=?,head_sha=NULL,error=NULL,updated=?
             WHERE id=?",
            params![revision, assignee, kickoff, now(), task_id],
        )?;
        Self::event_scoped(
            &tx,
            &job.pm_alias,
            "task_dispatched",
            json!({"task": task_id, "job": job.id, "assignee": assignee,
                   "revision": revision, "message": kickoff, "by": by}),
            Some(&job.id),
            Some(task_id),
        )?;
        tx.commit()?;
        let behind_dead = worker.endpoint.is_none()
            && registry::has_actor(&worker.provider, &worker.endpoint_kind);
        Ok((self.task_in(&conn, task_id)?, kickoff, false, behind_dead))
    }

    /// `job verdict`: validate + record + transition + notify in one
    /// transaction. The verdict names the exact reported commit —
    /// `sha == tasks.head_sha` and `state == 'review'` or it is
    /// rejected. `revise` past `max_revisions` records the verdict and
    /// escalates to `blocked` instead of looping.
    ///
    /// `reviewer` is the verified caller the RPC layer derived from the
    /// connection (an agent alias, or `operator` — CAD-372); `pane` is
    /// the agent's alias when the caller is one. Reviewer independence
    /// is enforced again here: reviewer == assignee is rejected.
    ///
    /// `verify` is the CLI's worktree-verification result as JSON text
    /// (`{checked, skipped}`) — stored verbatim with the verdict and
    /// echoed on the `verdict_recorded` event.
    #[allow(clippy::too_many_arguments)]
    pub fn record_verdict(
        &self,
        task_id: &str,
        sha: &str,
        verdict: &str,
        reviewer: &str,
        pane: Option<&str>,
        evidence: Option<&str>,
        message: Option<&str>,
        expect_revision: Option<i64>,
        verify: Option<&str>,
    ) -> Result<(Task, Verdict)> {
        if !matches!(verdict, "pass" | "revise" | "blocked") {
            return Err(Error::rejected(format!(
                "Verdict must be pass|revise|blocked, not '{verdict}'"
            )));
        }
        let sha = check_commit_sha(sha)?;
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let task = self.task_in(&tx, task_id)?;
        let job = self.job_in(&tx, &task.job_id)?;
        if task.state != "review" {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is '{}', not 'review' — a verdict lands \
                 only on a reported revision",
                task.state
            )));
        }
        if let Some(r) = expect_revision {
            if r != task.revision {
                return Err(Error::rejected(format!(
                    "Verdict names revision {r} but task '{task_id}' is at \
                     revision {} — stale verdict",
                    task.revision
                )));
            }
        }
        let head = task.head_sha.clone().ok_or_else(|| {
            Error::rejected(format!(
                "Task '{task_id}' reported no SHA — record it first with \
                 `cadence job task sha {task_id} <sha>`"
            ))
        })?;
        if sha != head {
            return Err(Error::rejected(format!(
                "Verdict SHA {sha} does not match the task's reported \
                 head_sha {head} — verify the exact reported commit"
            )));
        }
        if task.assignee.as_deref() == Some(reviewer) {
            return Err(Error::rejected(format!(
                "Reviewer '{reviewer}' is the task's assignee — a worker \
                 cannot verdict its own revision"
            )));
        }
        // An unassigned task (its assignee was force-removed, CAD-304)
        // still names its author through the kickoff that produced the
        // revision under review.
        if task.assignee.is_none() {
            if let Some(kickoff) = task.dispatch_message.as_deref() {
                if let Some(m) = self.message_in(&tx, kickoff)? {
                    if m.alias == reviewer {
                        return Err(Error::rejected(format!(
                            "Reviewer '{reviewer}' ran this revision's kickoff — \
                             a worker cannot verdict its own revision"
                        )));
                    }
                }
            }
        }
        let evidence_json: Option<String> = evidence.map(|e| {
            serde_json::from_str::<Value>(e)
                .map(|v| v.to_string())
                .unwrap_or_else(|_| json!({"text": e}).to_string())
        });
        let verify_json: Option<String> = verify.map(|v| {
            serde_json::from_str::<Value>(v)
                .map(|j| j.to_string())
                .unwrap_or_else(|_| json!({"text": v}).to_string())
        });
        let verify_val = verify_json
            .as_deref()
            .and_then(|v| serde_json::from_str::<Value>(v).ok());
        tx.execute(
            "INSERT INTO verdicts(task_id,revision,sha,verdict,reviewer,evidence,
             message,verify,created) VALUES(?,?,?,?,?,?,?,?,?)",
            params![
                task_id,
                task.revision,
                sha,
                verdict,
                reviewer,
                evidence_json,
                message,
                verify_json,
                now()
            ],
        )?;
        let seq = tx.last_insert_rowid();
        let next = match verdict {
            "pass" => "verified",
            "revise" if task.revision < job.max_revisions => "revising",
            _ => "blocked",
        };
        let error = match (verdict, next) {
            ("revise", "blocked") => Some("revision cap reached".to_string()),
            ("blocked", _) => Some("verdict: blocked".to_string()),
            _ => None,
        };
        tx.execute(
            "UPDATE tasks SET state=?,error=?,updated=? WHERE id=?",
            params![next, error, now(), task_id],
        )?;
        Self::event_scoped(
            &tx,
            &job.pm_alias,
            "verdict_recorded",
            json!({"task": task_id, "job": job.id, "revision": task.revision,
                   "sha": sha, "verdict": verdict, "reviewer": reviewer,
                   "pane": pane, "state": next, "verify": verify_val}),
            Some(&job.id),
            Some(task_id),
        )?;
        if next != "verified" {
            Self::event_scoped(
                &tx,
                &job.pm_alias,
                if next == "blocked" {
                    "task_blocked"
                } else {
                    "task_revising"
                },
                json!({"task": task_id, "revision": task.revision,
                       "verdict": verdict}),
                Some(&job.id),
                Some(task_id),
            )?;
        }
        let note = match next {
            "verified" => format!(
                "verdict pass on task {task_id} r{} — verified (sha {sha}, \
                 reviewer {reviewer}). Accept: `cadence job accept {task_id}`.",
                task.revision
            ),
            "revising" => format!(
                "verdict revise on task {task_id} r{} — re-dispatch: \
                 `cadence job dispatch {task_id}`.",
                task.revision
            ),
            _ => format!(
                "task {task_id} blocked at r{} (verdict {verdict}, \
                 reviewer {reviewer}) — `cadence job task reopen {task_id}` \
                 re-scopes it.",
                task.revision
            ),
        };
        self.route_job_event(&tx, &job, &task, next, &format!("verdict:{seq}"), &note)?;
        tx.commit()?;
        let verdict_row = self.verdict_in(&conn, seq)?;
        Ok((self.task_in(&conn, task_id)?, verdict_row))
    }

    /// `job accept`: verified → done, the acceptance edge. `--merged-sha`
    /// is recorded as evidence on the event/notification — cadence never
    /// runs git merges itself.
    pub fn accept_task(&self, task_id: &str, merged_sha: Option<&str>, by: &str) -> Result<Task> {
        if let Some(s) = merged_sha {
            check_commit_sha(s)?;
        }
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let task = self.task_in(&tx, task_id)?;
        let job = self.job_in(&tx, &task.job_id)?;
        if task.state != "verified" {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is '{}', not 'verified' — \
                 `cadence job verdict {task_id} --sha <sha> --pass` first",
                task.state
            )));
        }
        tx.execute(
            "UPDATE tasks SET state='done',updated=? WHERE id=?",
            params![now(), task_id],
        )?;
        Self::event_scoped(
            &tx,
            &job.pm_alias,
            "task_done",
            json!({"task": task_id, "job": job.id, "revision": task.revision,
                   "merged_sha": merged_sha, "by": by}),
            Some(&job.id),
            Some(task_id),
        )?;
        self.route_job_event(
            &tx,
            &job,
            &task,
            "done",
            "accept",
            &format!(
                "task {task_id} done at r{} — job '{}'.",
                task.revision, job.id
            ),
        )?;
        tx.commit()?;
        self.task_in(&conn, task_id)
    }

    /// `job task reopen`: blocked/verified/failed → draft, revision
    /// resets to 0 — a re-scope, not a continuation. Operator intent.
    pub fn reopen_task(&self, task_id: &str, by: &str) -> Result<Task> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let task = self.task_in(&tx, task_id)?;
        if !matches!(task.state.as_str(), "blocked" | "verified" | "failed") {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is '{}' — reopen is legal from \
                 blocked, verified or failed",
                task.state
            )));
        }
        tx.execute(
            "UPDATE tasks SET state='draft',revision=0,head_sha=NULL,
             error=NULL,updated=? WHERE id=?",
            params![now(), task_id],
        )?;
        Self::event_scoped(
            &tx,
            &self.job_in(&tx, &task.job_id)?.pm_alias,
            "task_reopened",
            json!({"task": task_id, "by": by}),
            Some(&task.job_id),
            Some(task_id),
        )?;
        tx.commit()?;
        self.task_in(&conn, task_id)
    }

    /// `job task fail`: PM marks a task unrecoverable. Terminal.
    pub fn fail_task(&self, task_id: &str, reason: &str, by: &str) -> Result<Task> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let task = self.task_in(&tx, task_id)?;
        if is_task_terminal(&task.state) {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is already '{}'",
                task.state
            )));
        }
        tx.execute(
            "UPDATE tasks SET state='failed',error=?,updated=? WHERE id=?",
            params![reason, now(), task_id],
        )?;
        Self::event_scoped(
            &tx,
            &self.job_in(&tx, &task.job_id)?.pm_alias,
            "task_failed",
            json!({"task": task_id, "reason": reason, "by": by}),
            Some(&task.job_id),
            Some(task_id),
        )?;
        tx.commit()?;
        self.task_in(&conn, task_id)
    }

    /// `job task cancel`: task → cancelled; its kickoff is cancelled in
    /// the same transaction when still `queued`/`submitting` — a
    /// `running` kickoff cannot be unpasted and finishes on its own.
    /// Agents are never stopped by a job.
    pub fn cancel_task(&self, task_id: &str, by: &str) -> Result<Task> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let task = self.task_in(&tx, task_id)?;
        if is_task_terminal(&task.state) {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is already '{}'",
                task.state
            )));
        }
        self.cancel_task_tx(&tx, &task, by)?;
        tx.commit()?;
        self.task_in(&conn, task_id)
    }

    fn cancel_task_tx(&self, tx: &Connection, task: &Task, by: &str) -> Result<()> {
        if let Some(kickoff) = &task.dispatch_message {
            tx.execute(
                "UPDATE messages SET state='cancelled',completed=?
                 WHERE id=? AND state IN ('queued','submitting')",
                params![now(), kickoff],
            )?;
        }
        tx.execute(
            "UPDATE tasks SET state='cancelled',updated=? WHERE id=?",
            params![now(), task.id],
        )?;
        Self::event_scoped(
            tx,
            &self.job_in(tx, &task.job_id)?.pm_alias,
            "task_cancelled",
            json!({"task": task.id, "by": by}),
            Some(&task.job_id),
            Some(&task.id),
        )?;
        Ok(())
    }

    /// `job cancel`: job → cancelled, every non-terminal task cancelled
    /// in one transaction (queued kickoffs included). Running kickoffs
    /// are left alone — agents are never stopped by a job.
    pub fn cancel_job(&self, job_id: &str, by: &str) -> Result<Job> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let job = self.job_in(&tx, job_id)?;
        if matches!(job.state.as_str(), "done" | "cancelled" | "failed") {
            return Err(Error::rejected(format!(
                "Job '{job_id}' is already '{}'",
                job.state
            )));
        }
        let tasks: Vec<Task> = {
            let mut stmt = tx.prepare(
                "SELECT * FROM tasks WHERE job_id=? AND state NOT IN
                 ('verified','done','cancelled','failed')",
            )?;
            let rows = stmt.query_map([job_id], row_task)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for task in &tasks {
            self.cancel_task_tx(&tx, task, by)?;
        }
        tx.execute(
            "UPDATE jobs SET state='cancelled',updated=? WHERE id=?",
            params![now(), job_id],
        )?;
        Self::event_scoped(
            &tx,
            &job.pm_alias,
            "job_cancelled",
            json!({"job": job_id, "tasks": tasks.len(), "by": by}),
            Some(job_id),
            None,
        )?;
        tx.commit()?;
        self.job_in(&conn, job_id)
    }

    /// `job close`: legal only when every task is `done`.
    pub fn close_job(&self, job_id: &str, by: &str) -> Result<Job> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let job = self.job_in(&tx, job_id)?;
        if job.state != "open" {
            return Err(Error::rejected(format!(
                "Job '{job_id}' is '{}' — only an open job closes",
                job.state
            )));
        }
        let open: Vec<String> = {
            let mut stmt = tx.prepare("SELECT id FROM tasks WHERE job_id=? AND state != 'done'")?;
            let rows = stmt.query_map([job_id], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if !open.is_empty() {
            return Err(Error::rejected(format!(
                "Job '{job_id}' has tasks not done: {} — \
                 `cadence job cancel {job_id}` abandons the job instead",
                open.join(", ")
            )));
        }
        tx.execute(
            "UPDATE jobs SET state='done',updated=? WHERE id=?",
            params![now(), job_id],
        )?;
        Self::event_scoped(
            &tx,
            &job.pm_alias,
            "job_closed",
            json!({"job": job_id, "by": by}),
            Some(job_id),
            None,
        )?;
        tx.commit()?;
        self.job_in(&conn, job_id)
    }

    /// `job task sha`: record the reported commit manually — the repair
    /// path when a kickoff completed without `SHA:` or `--sha`. Never
    /// infers: the caller names the SHA explicitly. Recording is an
    /// event; overwriting a different SHA is rejected.
    pub fn set_task_sha(&self, task_id: &str, sha: &str, by: &str) -> Result<Task> {
        let sha = check_commit_sha(sha)?;
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let task = self.task_in(&tx, task_id)?;
        if task.state != "review" {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is '{}' — `job task sha` repairs a \
                 reported revision awaiting verdict",
                task.state
            )));
        }
        if let Some(head) = &task.head_sha {
            if *head == sha {
                return Ok(task);
            }
            return Err(Error::rejected(format!(
                "Task '{task_id}' already reports head_sha {head} — \
                 SHA is bound at report time, not edited"
            )));
        }
        tx.execute(
            "UPDATE tasks SET head_sha=?,updated=? WHERE id=?",
            params![sha, now(), task_id],
        )?;
        Self::event_scoped(
            &tx,
            &self.job_in(&tx, &task.job_id)?.pm_alias,
            "task_sha_recorded",
            json!({"task": task_id, "sha": sha, "by": by}),
            Some(&task.job_id),
            Some(task_id),
        )?;
        tx.commit()?;
        self.task_in(&conn, task_id)
    }

    fn verdict_in(&self, conn: &Connection, seq: i64) -> Result<Verdict> {
        conn.query_row("SELECT * FROM verdicts WHERE seq=?", [seq], row_verdict)
            .map_err(Into::into)
    }
}
