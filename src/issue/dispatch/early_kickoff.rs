//! Early accepted-kickoff gate for `cadence dispatch` (CAD-1846).
//!
//! Answers duplicate or busy-lane kickoffs from the daemon's own rows before
//! source capture, contract rendering, lane setup, or job creation.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{acceptance_listing, LIVE_MESSAGE_STATES};
use crate::client;
use crate::error::{Error, Result};
use crate::issue::model::Front;
use crate::issue::parse::AcceptanceItem;

// ── Early accepted-kickoff gate (CAD-1846) ───────────────────────
//
// An accepted kickoff is answered from the daemon's own rows before a second
// source capture, contract render, lane setup or job creation. A tracker ref
// only locates a candidate; the row itself proves identity and kind.

/// The two kickoff kinds. They have different duplicate identities: a plain
/// kickoff carries no task, a job kickoff is its task's `job_dispatch` row.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DispatchMode {
    Plain,
    Job,
}

impl DispatchMode {
    pub(super) fn label(self) -> &'static str {
        match self {
            DispatchMode::Plain => "plain",
            DispatchMode::Job => "job",
        }
    }
}

/// The kickoff kind a stored message row actually is. `None` for anything
/// else, so an arbitrary user-authored row can never count as a kickoff.
pub(super) fn row_mode(row: &Value) -> Option<DispatchMode> {
    match (row["source"].as_str(), row["task_id"].as_str()) {
        (Some("dispatch"), None) => Some(DispatchMode::Plain),
        (Some("job_dispatch"), Some(_)) => Some(DispatchMode::Job),
        _ => None,
    }
}

/// The issue's open `message` refs as `(id, recorded owner)` — a locator
/// only; every caller proves the daemon's own row.
pub(super) fn message_refs(front: &Front, worker: &str) -> Vec<(String, String)> {
    front
        .refs
        .iter()
        .filter(|r| r.kind == "message" && r.closed != Some(true))
        .filter_map(|r| {
            let mid = r.path.clone()?;
            let owner = r
                .agent
                .clone()
                .or_else(|| {
                    r.label
                        .as_deref()
                        .and_then(|label| label.strip_prefix("dispatch → "))
                        .map(str::to_string)
                })
                .unwrap_or_else(|| worker.to_string());
            Some((mid, owner))
        })
        .collect()
}

fn find_row(show: &Value, mid: &str) -> Option<Value> {
    show["messages"]
        .as_array()?
        .iter()
        .find(|row| row["id"].as_str() == Some(mid))
        .cloned()
}

/// The daemon's own row for `mid` on `owner`'s history.
pub(super) fn message_row(
    state_dir: &Path,
    owner: &str,
    worker: &str,
    show: &Value,
    mid: &str,
) -> Option<Value> {
    if owner == worker {
        return find_row(show, mid);
    }
    client::rpc(state_dir, "agent_show", json!({"alias": owner}))
        .ok()
        .and_then(|show| find_row(&show, mid))
}

fn lexical(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Canonical when both sides resolve, else the absolute lexical path — a
/// recorded string and a live path compare without inventing a root.
pub(super) fn same_path(recorded: &Path, expected: &Path) -> bool {
    let left = recorded
        .canonicalize()
        .unwrap_or_else(|_| lexical(recorded));
    let right = expected
        .canonicalize()
        .unwrap_or_else(|_| lexical(expected));
    left == right
}

/// A task's recorded worktree against the lane this dispatch resolved: a bare
/// name names the lane, a path must resolve to it.
fn same_lane(recorded: &str, lane: &Path) -> bool {
    let recorded = Path::new(recorded);
    if recorded.is_absolute() || recorded.components().count() != 1 {
        return same_path(recorded, lane);
    }
    match (recorded.file_name(), lane.file_name()) {
        (Some(recorded), Some(lane)) => recorded == lane,
        _ => false,
    }
}

/// The approved spec locator as given: canonical when it resolves, else the
/// absolute lexical path. Never read, never hashed.
pub(super) fn spec_locator(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| lexical(path))
}

fn same_locator(recorded: &str, locator: &Path) -> bool {
    spec_locator(Path::new(recorded)) == spec_locator(locator)
}

/// The issue's single OPEN native job and its canonical default task. A
/// terminal job is never a retry candidate — the store's one-open-job guard
/// then governs a fresh job.
struct OpenJob {
    job: Value,
    task: Value,
}

fn open_job(state_dir: &Path, front: &Front) -> Result<Option<OpenJob>> {
    let list = client::rpc(state_dir, "job_list", json!({"states": ["open"]}))?;
    let jobs: Vec<Value> = list["jobs"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|job| {
                    job["state"].as_str() == Some("open")
                        && job["issue"].as_str() == Some(front.id.as_str())
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let job = match jobs.as_slice() {
        [] => return Ok(None),
        [job] => job.clone(),
        many => {
            return Err(Error::rejected(format!(
                "{} maps to {} open jobs ({}) — refusing to guess which native \
                 binding a dispatch retries; close the stale ones first",
                front.id,
                many.len(),
                many.iter()
                    .filter_map(|job| job["id"].as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            )))
        }
    };
    let id = job["id"].as_str().unwrap_or_default().to_string();
    let task_id = format!("{id}-t1");
    // `job_show` computes file drift and opens the spec. A retry must read
    // only stored identity, even if its source is now huge or unavailable.
    let show = client::rpc(state_dir, "task_show", json!({"task": task_id}))?;
    let task = show["task"].clone();
    if task["id"].as_str() != Some(task_id.as_str()) || task["job"].as_str() != Some(id.as_str()) {
        return Err(Error::rejected(format!(
            "open job {id} does not carry its canonical task {task_id} — refusing to \
             infer a native binding from it"
        )));
    }
    Ok(Some(OpenJob { job, task }))
}

/// The read-only facts an early duplicate gate needs about this dispatch.
pub(super) struct EarlyDispatch<'a> {
    pub(super) state_dir: &'a Path,
    pub(super) front: &'a Front,
    pub(super) mode: DispatchMode,
    pub(super) worker: &'a str,
    pub(super) reply_to: &'a str,
    pub(super) show: &'a Value,
    pub(super) repo_root: &'a Path,
    pub(super) lane: &'a Path,
    pub(super) branch: &'a str,
    pub(super) job_locator: Option<&'a Path>,
    pub(super) acceptance: &'a [AcceptanceItem],
}

/// What the daemon's own rows already say about this dispatch.
pub(super) enum EarlyKickoff {
    /// Nothing accepted yet: the normal flow runs.
    None,
    /// This request's own accepted kickoff — return it, create nothing.
    Duplicate {
        row: Value,
        job: Option<Value>,
        task: Option<Value>,
        kind: &'static str,
    },
    /// The issue's open job exists but was never accepted: reuse its native
    /// binding instead of minting another job.
    ReuseJob { job: Value },
}

fn busy_error(issue: &str, lane: &Path, mode: DispatchMode, row: &Value) -> Error {
    Error::rejected(format!(
        "{issue}'s lane {} already has a live {} kickoff {} ({}) — wait for it to finish \
         or reconcile it; nothing was created or queued",
        lane.display(),
        mode.label(),
        row["id"].as_str().unwrap_or("?"),
        row["state"].as_str().unwrap_or("?"),
    ))
}

/// Which of this request's material inputs the open job's own scope disagrees
/// with. Job identity, PM, repository, assignee, lane and spec locator are
/// the binding a retry may never re-point.
fn binding_mismatches(binding: &OpenJob, d: &EarlyDispatch<'_>) -> Vec<&'static str> {
    let job = &binding.job;
    let task = &binding.task;
    let mut wrong = Vec::new();
    if job["issue"].as_str() != Some(d.front.id.as_str()) {
        wrong.push("issue");
    }
    if job["pm"].as_str() != Some(d.reply_to) {
        wrong.push("pm");
    }
    if !job["repo"]
        .as_str()
        .is_some_and(|repo| same_path(Path::new(repo), d.repo_root))
    {
        wrong.push("repo");
    }
    if task["job"].as_str() != job["id"].as_str() {
        wrong.push("task job");
    }
    if task["assignee"].as_str() != Some(d.worker) {
        wrong.push("assignee");
    }
    if !task["worktree"]
        .as_str()
        .is_some_and(|worktree| same_lane(worktree, d.lane))
    {
        wrong.push("worktree");
    }
    if task["branch"].as_str() != Some(d.branch) {
        wrong.push("branch");
    }
    let locator_matches = d.job_locator.is_some_and(|locator| {
        task["spec_path"]
            .as_str()
            .or_else(|| job["spec_path"].as_str())
            .is_some_and(|recorded| same_locator(recorded, locator))
    });
    if !locator_matches {
        wrong.push("spec");
    }
    wrong
}

/// The material inputs a never-accepted draft must still agree on before its
/// native binding is reused. The lane's own recorded base is the authority:
/// the default branch moves after the lane was cut, and a legitimate frozen
/// lane must not be refused because `main` advanced.
fn draft_mismatches(binding: &OpenJob, d: &EarlyDispatch<'_>) -> Result<Vec<&'static str>> {
    let job = &binding.job;
    let task = &binding.task;
    let mut wrong = binding_mismatches(binding, d);
    if task["state"].as_str() != Some("draft") {
        wrong.push("task state");
    }
    let recorded = crate::worktree::lifecycle::recoverable_record(
        d.repo_root,
        d.lane,
        "development",
        "cadence issue start",
        Some(d.branch),
        Some(d.front.id.as_str()),
    )?;
    if let Some(base) = recorded.and_then(|record| record.base_sha) {
        if job["base_ref"].as_str() != Some(base.as_str()) {
            wrong.push("job base");
        }
        if task["base_sha"].as_str() != Some(base.as_str()) {
            wrong.push("task base");
        }
    } else if job["base_ref"].as_str() != task["base_sha"].as_str() {
        wrong.push("base");
    }
    let acceptance = acceptance_listing(d.acceptance);
    if task["acceptance"].as_str() != acceptance.as_deref()
        && !task["acceptance"]
            .as_str()
            .is_some_and(|saved| super::same_checklist(saved, d.acceptance))
    {
        wrong.push("acceptance");
    }
    Ok(wrong)
}

/// The task's own kickoff row, from the native task view: an id-targeted read,
/// so a bounded agent history can never hide an accepted row.
fn task_message_row(state_dir: &Path, task_id: &str, mid: &str) -> Result<Option<Value>> {
    let show = client::rpc(state_dir, "task_show", json!({"task": task_id}))?;
    Ok(show["task"]["messages"].as_array().and_then(|rows| {
        rows.iter()
            .find(|row| row["id"].as_str() == Some(mid))
            .cloned()
    }))
}

fn accepted_row_ok(row: &Value, d: &EarlyDispatch<'_>, task_id: &str) -> bool {
    row["alias"].as_str() == Some(d.worker)
        && row["issue"].as_str() == Some(d.front.id.as_str())
        && row["worktree"]
            .as_str()
            .is_some_and(|worktree| same_lane(worktree, d.lane))
        && row["task_id"].as_str() == Some(task_id)
        && row_mode(row) == Some(DispatchMode::Job)
}

/// Decide the early outcome from the daemon's rows.
pub(super) fn early_kickoff(d: &EarlyDispatch<'_>) -> Result<EarlyKickoff> {
    // 1. Live rows the issue's open `message` refs name.
    let mut live_plain = None;
    let mut live_job = None;
    for (mid, owner) in message_refs(d.front, d.worker) {
        let Some(row) = message_row(d.state_dir, &owner, d.worker, d.show, &mid) else {
            continue;
        };
        if !LIVE_MESSAGE_STATES.contains(&row["state"].as_str().unwrap_or_default()) {
            continue;
        }
        if row["alias"].as_str() != Some(d.worker)
            || row["issue"].as_str() != Some(d.front.id.as_str())
            || !row["worktree"]
                .as_str()
                .is_some_and(|worktree| same_path(Path::new(worktree), d.lane))
        {
            continue;
        }
        match row_mode(&row) {
            Some(DispatchMode::Plain) => live_plain = Some(row),
            Some(DispatchMode::Job) => live_job = Some(row),
            None => {}
        }
    }
    if let Some(row) = live_plain {
        if d.mode == DispatchMode::Plain {
            return Ok(EarlyKickoff::Duplicate {
                row,
                job: None,
                task: None,
                kind: "live",
            });
        }
        return Err(busy_error(&d.front.id, d.lane, DispatchMode::Plain, &row));
    }

    // 2. The issue's one open native job and its canonical default task.
    let Some(binding) = open_job(d.state_dir, d.front)? else {
        return match live_job {
            Some(row) => Err(busy_error(&d.front.id, d.lane, DispatchMode::Job, &row)),
            None => Ok(EarlyKickoff::None),
        };
    };
    let job_id = binding.job["id"].as_str().unwrap_or_default().to_string();
    let task_id = binding.task["id"].as_str().unwrap_or_default().to_string();
    let accepted = binding.task["dispatch_message"]
        .as_str()
        .map(str::to_string);
    let on_lane = binding.task["worktree"]
        .as_str()
        .is_some_and(|worktree| same_lane(worktree, d.lane))
        && binding.task["branch"].as_str() == Some(d.branch);

    if d.mode == DispatchMode::Plain {
        // A live job kickoff on this lane is a busy lane, never a duplicate.
        // A recorded kickoff that cannot be proven as this issue's job row is
        // reconciled, never treated as absent.
        if let Some(mid) = &accepted {
            if on_lane {
                let stored = task_message_row(d.state_dir, &task_id, mid)?;
                match stored {
                    Some(row) if accepted_row_ok(&row, d, &task_id) => {
                        if LIVE_MESSAGE_STATES.contains(&row["state"].as_str().unwrap_or_default())
                        {
                            return Err(busy_error(&d.front.id, d.lane, DispatchMode::Job, &row));
                        }
                    }
                    _ => {
                        return Err(Error::rejected(format!(
                            "{} maps to open job {job_id} whose task {task_id} records \
                             kickoff {mid} that the daemon cannot prove as this issue's \
                             job row — reconcile the task before dispatching again; \
                             nothing was created",
                            d.front.id
                        )))
                    }
                }
            }
        }
        // A ref may still name a live job row of another (closed) job on this
        // lane; the lane is busy either way.
        if let Some(row) = live_job {
            return Err(busy_error(&d.front.id, d.lane, DispatchMode::Job, &row));
        }
        return Ok(EarlyKickoff::None);
    }

    // Job mode: the binding must be this request's own scope.
    let wrong = binding_mismatches(&binding, d);
    if !wrong.is_empty() {
        return Err(Error::rejected(format!(
            "{} already maps to open job {job_id} but its native binding differs from this \
             request ({}); use native reopen/revise or cancel the job — nothing was created",
            d.front.id,
            wrong.join(", "),
        )));
    }
    if let Some(mid) = &accepted {
        let stored = task_message_row(d.state_dir, &task_id, mid)?;
        let row = stored.filter(|row| accepted_row_ok(row, d, &task_id));
        let Some(row) = row else {
            return Err(Error::rejected(format!(
                "{} maps to open job {job_id} whose task {task_id} records kickoff {mid} \
                 that the daemon cannot prove as this issue's accepted job row — reconcile \
                 the task before dispatching again; nothing was created",
                d.front.id
            )));
        };
        let state = row["state"].as_str().unwrap_or_default().to_string();
        return match state.as_str() {
            "completed" => Ok(EarlyKickoff::Duplicate {
                row,
                job: Some(binding.job),
                task: Some(binding.task),
                kind: "reported",
            }),
            live if LIVE_MESSAGE_STATES.contains(&live) => Ok(EarlyKickoff::Duplicate {
                row,
                job: Some(binding.job),
                task: Some(binding.task),
                kind: "live",
            }),
            _ => Err(Error::rejected(format!(
                "{} maps to open job {job_id} whose task {task_id} kickoff {mid} ended \
                 '{}' without being accepted — reconcile the task before dispatching \
                 again; nothing was created",
                d.front.id,
                row["state"].as_str().unwrap_or("unknown"),
            ))),
        };
    }
    if let Some(row) = live_job {
        return Err(busy_error(&d.front.id, d.lane, DispatchMode::Job, &row));
    }
    let wrong = draft_mismatches(&binding, d)?;
    if !wrong.is_empty() {
        return Err(Error::rejected(format!(
            "{} already maps to open job {job_id} whose draft binding differs from this \
             request ({}); use native reopen/revise or cancel the job — nothing was created",
            d.front.id,
            wrong.join(", "),
        )));
    }
    Ok(EarlyKickoff::ReuseJob { job: binding.job })
}
