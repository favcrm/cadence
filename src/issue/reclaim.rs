//! Automatic reclaim of lane build output (CAD-1021 slice 4, operator
//! scope 2026-10-02 ~15:50Z). Two pieces on the daemon checkup's cadence
//! (one daemon-wide throttle, not per pm dir):
//!
//! - **Scheduled merged sweep** — `issue finish --merged` runs as the
//!   library call, same guards (in use, dirty, recent writes) as the
//!   manual command. The gap was that the sweep ran only on `issue sync`
//!   or by hand; lanes whose PRs merged sat forever.
//!
//! - **Idle `target/` reclaim** — a lane that has gone quiet still holds
//!   tens of GB of `target/`. When a linked lane worktree has no process
//!   with a cwd or open fd inside it, no live message, task binding or
//!   registered agent cwd bound to it, and no write to its source for
//!   `idle_secs` (default 6 h, `pm.yaml [host] reclaim_target_idle_secs`),
//!   only `<worktree>/target` is deleted — never the worktree, branch or
//!   source. `target/` is a regenerable cache, so this loses no work. The
//!   freed bytes ride the issue's comment log.
//!
//! Safety invariants (the guard checks at the bottom go red without them):
//! - I-A: never reclaimed while in use. In use means a process cwd'd or
//!   holding an fd inside the lane, a registered agent cwd on it, or a
//!   live/unknown message or task bound to it (finish's own in-use
//!   evaluation, `finish::lane_in_use`). A daemon that is up but could not
//!   enumerate its agents blocks (that evaluation's deferred failure). The idle window
//!   is an mtime walk over the lane's source (`target/` is skipped — its
//!   own writes are covered by the fd/cwd scan); a walk that hits its
//!   entry cap counts the lane as recent, never idle.
//! - I-B: exactly one path is ever deleted: `<canonical lane>/target`,
//!   re-derived at delete time. The lane must be a linked worktree (git
//!   dir != common dir), not the repo root, and sit under
//!   `<root>/.cadence/wt`. A recorded `cargo_target` is never trusted: it
//!   must canonicalize to that same path or the lane is skipped. A
//!   symlinked `target/` and the shared `.cadence/target/shared` cache
//!   are refused.
//! - I-C: the check-to-delete gap is narrowed, not closed, and there is no
//!   cross-process lock. Immediately before `remove_dir_all` the daemon
//!   snapshot is re-fetched and the live-use scan re-run (a build takes
//!   the lane as cwd and holds `target/` open the moment it starts); the
//!   size measurement happens before that re-scan, and the path is
//!   re-derived and symlink-checked right after it. A process that
//!   starts after the re-scan can still lose its `target/`; cargo
//!   recreates it.
//!
//! Self-RPC: the pass probes the daemon it runs inside (`agent_list`,
//! `agent_show`, `task_show`), so every probe is bounded by
//! [`PROBE_TIMEOUT`]; a timeout is an unreachable daemon and blocks.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::{board, finish, write, Pm};

/// Default idle window for `target/` reclaim — `[host]
/// reclaim_target_idle_secs` overrides.
pub(crate) const RECLAIM_IDLE_SECS: u64 = 6 * 3600;

/// Bound on each daemon probe the pass makes against its own daemon
/// (as `blocked::sweep` bounds its notices).
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Entries the idle walk visits before it gives up and counts the lane
/// as recent.
const WALK_CAP: usize = 50_000;

/// The `[host]` idle window for `target/` reclaim.
pub(crate) fn idle_secs(pm_dir: &Path) -> u64 {
    crate::doctor::host::read_host_overrides(pm_dir)
        .ok()
        .flatten()
        .and_then(|o| o.reclaim_target_idle_secs)
        .unwrap_or(RECLAIM_IDLE_SECS)
}

/// Pids holding an open file descriptor under `dir` — a build in
/// flight keeps `target/` open even when its cwd sits elsewhere. A
/// vanished pid or a denied read just doesn't report; the cadence
/// process itself is excluded. /proc races are benign: a pid that goes
/// away mid-scan is simply not held.
fn pids_fd_under(dir: &Path) -> Vec<u32> {
    let mut out = Vec::new();
    let Ok(dir) = dir.canonicalize() else {
        return out;
    };
    let me = std::process::id();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in procs.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if let Ok(target) = std::fs::read_link(fd.path()) {
                if target.starts_with(&dir) {
                    out.push(pid);
                    break;
                }
            }
        }
    }
    out
}

/// What the idle walk over a lane's source saw.
#[derive(Debug, PartialEq)]
enum Walk {
    /// Nothing written within the window.
    Idle,
    /// A write this long ago, inside the window.
    Recent(Duration),
    /// The entry cap was hit first — the rest is unknown, so not idle.
    Truncated,
}

/// The most recent write under the lane's source within `window`. The
/// lane's top-level `target/` is skipped: its writes belong to a build,
/// which the cwd/fd scan already catches, and walking it would spend the
/// whole budget before the edited source is seen. Cheap `mtime` walk;
/// reads no contents.
fn walk_writes(lane: &Path, window: Duration, cap: usize) -> Walk {
    let now = SystemTime::now();
    let mut newest: Option<Duration> = None;
    let mut stack = vec![lane.to_path_buf()];
    let mut visited = 0usize;
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for ent in entries.flatten() {
            if d == lane && ent.file_name() == "target" {
                continue;
            }
            visited += 1;
            if visited > cap {
                return Walk::Truncated;
            }
            let Ok(meta) = ent.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(ent.path());
            }
            if let Ok(m) = meta.modified() {
                let age = now.duration_since(m).unwrap_or(Duration::ZERO);
                if age < window && newest.is_none_or(|a| age < a) {
                    newest = Some(age);
                }
            }
        }
    }
    newest.map_or(Walk::Idle, Walk::Recent)
}

/// True for the shared dep cache `<root>/.cadence/target/shared` or
/// anything under it — repo property, never a lane's to delete.
fn is_shared_cache(cand: &Path, root: &Path) -> bool {
    let shared = crate::worktree::shared_target_dir(root);
    let shared = shared.canonicalize().unwrap_or(shared);
    cand == shared || cand.starts_with(&shared)
}

/// `git rev-parse <flag>` in `dir`, canonicalized.
fn git_path(dir: &Path, flag: &str) -> Option<PathBuf> {
    let out = crate::reaper::output(
        std::process::Command::new("git")
            .args(["rev-parse", "--path-format=absolute", flag])
            .current_dir(dir),
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    p.canonicalize().ok()
}

/// The one path a reclaim may delete for `lane`: `<canonical lane>/target`,
/// derived fresh from the lane. `Some` only when it is a real directory.
/// Refused (`Err`): a lane that is not a linked worktree under
/// `<root>/.cadence/wt`, a symlinked `target/`, the shared cache.
/// Skipped (`None`): a recorded `cargo_target` that is not exactly that
/// path — the recorded string is never trusted as a delete target.
fn reclaim_target(lane: &Path, cargo_target: Option<&str>) -> Result<Option<PathBuf>> {
    let refuse = |why: String| Err(Error::rejected(format!("{}: {why}", lane.display())));
    match std::fs::symlink_metadata(lane) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
        Ok(_) => return refuse("lane path is not a real directory".into()),
        Err(e) => return refuse(format!("cannot inspect lane path ({e})")),
    }
    let lane_c = lane
        .canonicalize()
        .map_err(|e| Error::rejected(format!("{}: cannot resolve ({e})", lane.display())))?;
    if finish::lexical_path(lane) != lane_c {
        return refuse(
            "lane path traverses a symlink or differs from its canonical identity".into(),
        );
    }
    let root = crate::worktree::main_root(&lane_c)
        .map_err(|e| Error::rejected(format!("{}: no main root ({e})", lane.display())))?;
    let root_c = root.canonicalize().unwrap_or(root);
    let (Some(git_dir), Some(common)) = (
        git_path(&lane_c, "--git-dir"),
        git_path(&lane_c, "--git-common-dir"),
    ) else {
        return refuse("not a git worktree".to_string());
    };
    if git_dir == common || lane_c == root_c {
        return refuse("not a linked worktree (the main checkout is never reclaimed)".to_string());
    }
    let wt_parent = crate::worktree::layout::worktrees_dir(&root_c);
    if lane_c == wt_parent || !lane_c.starts_with(&wt_parent) {
        return refuse(format!("not under {}", wt_parent.display()));
    }
    let registered = finish::git(&root_c, &["worktree", "list", "--porcelain"]).map_err(|e| {
        Error::rejected(format!(
            "{}: cannot enumerate registered checkouts ({e})",
            lane.display()
        ))
    })?;
    if !registered
        .lines()
        .any(|line| line == format!("worktree {}", lane_c.display()))
    {
        return refuse("path is not a registered worktree of its claimed repo".into());
    }
    let target = lane_c.join("target");
    if let Some(ct) = cargo_target {
        let same = Path::new(ct)
            .canonicalize()
            .is_ok_and(|c| c == target && !target.is_symlink());
        if !same {
            // A target dir elsewhere (the shared cache, a lane-local
            // `build.target-dir`) or a forged path: skipped, not deleted.
            return Ok(None);
        }
    }
    match std::fs::symlink_metadata(&target) {
        Ok(m) if m.file_type().is_symlink() => {
            return refuse("target/ is a symlink — a reclaimed target must be a real dir".into())
        }
        Ok(m) if m.is_dir() => {}
        _ => return Ok(None),
    }
    if is_shared_cache(&target, &root_c) {
        return refuse("target/ is inside the shared dep cache".to_string());
    }
    Ok(Some(target))
}

/// The live-use scan — every block a just-started build can produce:
/// a process cwd'd in the lane, an open fd inside it, an incomplete
/// daemon enumeration, a registered agent whose cwd is bound to it, or
/// a live/unknown message or task bound to it (finish's in-use
/// evaluation). Re-run against a fresh `view` right before the delete.
fn live_reason(
    view: &finish::DaemonView,
    state_dir: &Path,
    front: &crate::issue::model::Front,
    lane: &Path,
) -> Option<String> {
    if let Some(pid) = finish::pids_cwd_under(lane).first() {
        return Some(format!(
            "process {pid} ({}) cwd inside",
            finish::comm_of(*pid)
        ));
    }
    if let Some(pid) = pids_fd_under(lane).first() {
        return Some(format!(
            "process {pid} ({}) holds an open fd inside",
            finish::comm_of(*pid)
        ));
    }
    if view.up && !finish::cwd_holder_aliases(view, Some(lane)).is_empty() {
        return Some("a registered agent's cwd is on the lane".to_string());
    }
    finish::lane_in_use(view, state_dir, front, lane)
}

/// The reason a lane's `target/` is NOT reclaimable, or `None` when the
/// lane is idle and safe to reclaim.
fn blocked_reason(
    view: &finish::DaemonView,
    state_dir: &Path,
    front: &crate::issue::model::Front,
    lane: &Path,
    idle: Duration,
    cap: usize,
) -> Option<String> {
    if let Some(r) = live_reason(view, state_dir, front, lane) {
        return Some(r);
    }
    match walk_writes(lane, idle, cap) {
        Walk::Idle => None,
        Walk::Recent(_) => Some(format!(
            "written within the {}h idle window",
            idle.as_secs() / 3600
        )),
        Walk::Truncated => Some("idle walk truncated — treating as recent".to_string()),
    }
}

/// Part 1+2 on the checkup cadence. Sweeps every open worktree whose
/// branch is merged (`finish --merged`, same guards), then reclaims the
/// `target/` of every still-open lane that has gone idle. Best-effort per
/// lane: one lane's failure never blocks the next, and every decision —
/// sweep, reclaim, refuse — is logged so the ticket records what ran.
///
/// Returns a summary `{swept, reclaimed, refused}` for the event stream.
pub fn run(pm: &Pm, state_dir: &Path, actor: &str) -> Result<Value> {
    run_with_idle(pm, state_dir, actor, idle_secs(&pm.dir))
}

/// `run` with an explicit idle window — the daemon's configured value
/// (`pm.yaml [host] reclaim_target_idle_secs`, default 6 h). Tests pass a
/// short window instead of aging a fixture for hours.
pub fn run_with_idle(pm: &Pm, state_dir: &Path, actor: &str, idle_secs: u64) -> Result<Value> {
    finish::with_probe_timeout(PROBE_TIMEOUT, || {
        run_bounded(pm, state_dir, actor, idle_secs)
    })
}

fn run_bounded(pm: &Pm, state_dir: &Path, actor: &str, idle_secs: u64) -> Result<Value> {
    let mut out = json!({"swept": 0, "reclaimed": [], "skipped": []});
    // Part 1: the merged sweep. dry_run=false — this runs the real
    // `finish` guards; a candidate that fails them is `skipped`/`refused`,
    // never removed.
    match finish::sweep(pm, None, false, false, actor, state_dir) {
        Ok(s) => {
            let rows = s["rows"].as_array().cloned().unwrap_or_default();
            out["swept"] = json!(rows.len());
            for row in rows.into_iter().filter(|row| row["outcome"] == "refused") {
                out["skipped"].as_array_mut().unwrap().push(json!({
                    "issue": row["issue"],
                    "lane": row["path"],
                    "reason_code": "merged-checkout-refused",
                    "reason": row["reason"],
                }));
            }
        }
        Err(e) => {
            tracing::warn!(event = "reclaim_sweep_failed", error = e.to_string());
        }
    }
    // Part 2: idle target/ reclaim. One enumeration shared across every
    // lane; a daemon that is down means "no live panes/messages" but the
    // /proc scans still carry the guard.
    let view = finish::daemon_view(state_dir);
    let idle = Duration::from_secs(idle_secs);
    let issues = board::load_all(&pm.dir, None)?;
    for issue in issues {
        let id = issue.front.id.clone();
        for lane in crate::issue::start::open_worktrees(&issue.front) {
            let res = reclaim_lane_detailed(&view, state_dir, pm, &issue, &lane, idle, actor);
            match res {
                Ok(ReclaimAttempt::Reclaimed(bytes)) => out["reclaimed"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"issue": id, "lane": lane, "bytes_freed": bytes})),
                Ok(ReclaimAttempt::Absent) => {}
                Ok(ReclaimAttempt::Skipped(reason)) => out["skipped"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"issue": id, "lane": lane, "reason_code": "retained-by-safety-policy", "reason": reason})),
                Err(e) => out["skipped"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"issue": id, "lane": lane, "reason_code": "cleanup-check-failed", "reason": e.to_string()})),
            }
        }
    }
    Ok(out)
}

/// Read-only manual plan. The same `reclaim_target` confinement and
/// `blocked_reason` policy is used by the scheduled actor below; dry-run does
/// not delete, write tracker comments or notify owners.
pub fn plan(pm: &Pm, state_dir: &Path, idle_secs: u64) -> Result<Value> {
    finish::with_probe_timeout(PROBE_TIMEOUT, || plan_bounded(pm, state_dir, idle_secs))
}

pub fn plan_for_repo(pm: &Pm, state_dir: &Path, idle_secs: u64, repo: &Path) -> Result<Value> {
    let root = crate::worktree::main_root(repo)?.canonicalize()?;
    let mut plan = plan(pm, state_dir, idle_secs)?;
    if let Some(rows) = plan["merged_checkouts"].as_array_mut() {
        rows.retain(|row| {
            row["worktree"]
                .as_str()
                .and_then(|path| lane_repo(Path::new(path)))
                .as_deref()
                == Some(root.as_path())
        });
    }
    if let Some(rows) = plan["cache_resources"].as_array_mut() {
        rows.retain(|row| {
            row["resource"]["path"]
                .as_str()
                .and_then(|path| lane_repo(Path::new(path)))
                .as_deref()
                == Some(root.as_path())
        });
    }
    plan["repo"] = json!(root);
    plan["scope"] = json!("repository");
    Ok(plan)
}

fn lane_repo(lane: &Path) -> Option<PathBuf> {
    crate::worktree::main_root(lane)
        .ok()
        .and_then(|root| root.canonicalize().ok())
        .or_else(|| crate::worktree::layout::assumed_root(lane))
}

fn plan_bounded(pm: &Pm, state_dir: &Path, idle_secs: u64) -> Result<Value> {
    let merged = finish::sweep(pm, None, false, true, "", state_dir)?;
    let view = finish::daemon_view(state_dir);
    let idle = Duration::from_secs(idle_secs);
    let issues = board::load_all(&pm.dir, None)?;
    let mut resources = Vec::new();
    for issue in &issues {
        for lane in crate::issue::start::open_worktrees(&issue.front) {
            resources.push(target_plan(&view, state_dir, issue, &lane, idle));
        }
    }
    Ok(json!({
        "schema": "cadence.reclaim-plan/1",
        "dry_run": true,
        "cleanup_mode": "report-only-for-checkout-removal",
        "merged_checkouts": merged["rows"],
        "cache_resources": resources,
    }))
}

fn target_plan(
    view: &finish::DaemonView,
    state_dir: &Path,
    issue: &board::Issue,
    lane: &Path,
    idle: Duration,
) -> Value {
    let branch = lane
        .file_name()
        .map(|name| crate::worktree::layout::branch(&name.to_string_lossy()));
    let base = json!({"issue": issue.front.id, "path": lane, "branch": branch.clone()});
    if !lane.is_dir() {
        return json!({"resource": base, "status": "missing", "reason_code": "checkout-missing",
            "reason": "checkout is missing; branch and tracker refs are preserved", "reclaimable_bytes": 0});
    }
    let cargo_target = issue
        .front
        .refs
        .iter()
        .filter(|r| r.kind == "worktree" && r.closed != Some(true))
        .filter(|r| finish::same_path(Path::new(r.path.as_deref().unwrap_or_default()), lane))
        .find_map(|r| r.cargo_target.clone());
    let target = match reclaim_target(lane, cargo_target.as_deref()) {
        Ok(Some(target)) => target,
        Ok(None) => {
            return json!({"resource": base, "status": "retained",
            "reason_code": "target-not-lane-local", "reason": "recorded cargo target is not the lane-local target; no deletion", "reclaimable_bytes": null})
        }
        Err(e) => {
            return json!({"resource": base, "status": "refused",
            "reason_code": "path-confinement-or-symlink-refused", "reason": e.to_string(), "reclaimable_bytes": null})
        }
    };
    if let Some(reason) = blocked_reason(view, state_dir, &issue.front, lane, idle, WALK_CAP) {
        let code = if reason.contains("written within") {
            "source-recent"
        } else if reason.contains("truncated")
            || reason.contains("cannot")
            || reason.contains("enumerat")
        {
            "live-use-or-idle-probe-unknown"
        } else {
            "live-use-blocked"
        };
        return json!({"resource": base, "status": "retained", "target": target,
            "reason_code": code, "reason": reason, "reclaimable_bytes": null});
    }
    let (size, truncated) = crate::doctor::host::dir_size(&target);
    json!({
        "resource": base,
        "status": "reclaimable-cache-only",
        "target": target,
        "reason_code": if truncated { "size-estimate-unknown" } else { "idle-target-cache" },
        "reason": "only the confined lane-local target cache may be reclaimed; source and branch are retained",
        "reclaimable_bytes": if truncated { Value::Null } else { json!(size) },
        "retained_branch": branch,
        "retained_artifacts": [],
    })
}

enum ReclaimAttempt {
    Reclaimed(u64),
    Absent,
    Skipped(String),
}

/// Compatibility wrapper used by focused guard checks.
#[cfg(test)]
fn reclaim_lane(
    view: &finish::DaemonView,
    state_dir: &Path,
    pm: &Pm,
    issue: &board::Issue,
    lane: &Path,
    idle: Duration,
    actor: &str,
) -> Result<Option<u64>> {
    match reclaim_lane_detailed(view, state_dir, pm, issue, lane, idle, actor)? {
        ReclaimAttempt::Reclaimed(bytes) => Ok(Some(bytes)),
        ReclaimAttempt::Absent | ReclaimAttempt::Skipped(_) => Ok(None),
    }
}

/// One lane's shared reclaim decision + delete. Refusals are returned to the
/// scheduled report rather than silently disappearing.
fn reclaim_lane_detailed(
    view: &finish::DaemonView,
    state_dir: &Path,
    pm: &Pm,
    issue: &board::Issue,
    lane: &Path,
    idle: Duration,
    actor: &str,
) -> Result<ReclaimAttempt> {
    if !lane.is_dir() {
        return Ok(ReclaimAttempt::Absent);
    }
    // The recorded cargo_target for this lane's open ref, if any — only
    // ever compared against the derived path, never deleted.
    let cargo_target = issue
        .front
        .refs
        .iter()
        .filter(|r| r.kind == "worktree" && r.closed != Some(true))
        .filter(|r| finish::same_path(Path::new(r.path.as_deref().unwrap_or_default()), lane))
        .find_map(|r| r.cargo_target.clone());
    let Some(target) = reclaim_target(lane, cargo_target.as_deref())? else {
        return Ok(ReclaimAttempt::Absent);
    };
    if let Some(reason) = blocked_reason(view, state_dir, &issue.front, lane, idle, WALK_CAP) {
        return Ok(ReclaimAttempt::Skipped(reason));
    }
    // Measure before the re-scan so nothing slow sits between the
    // re-scan and the delete.
    let (bytes, _trunc) = crate::doctor::host::dir_size(&target);
    // Test seam: let a test inject a just-started build into the real
    // check→delete gap, proving the re-scan below catches it.
    #[cfg(test)]
    tests::before_rescan(lane);
    // I-C: re-fetch the daemon snapshot and re-run the live-use scan.
    let fresh = finish::daemon_view(state_dir);
    if let Some(appeared) = live_reason(&fresh, state_dir, &issue.front, lane) {
        tracing::info!(
            event = "reclaim_rescan_skip",
            lane = %lane.display(),
            reason = %appeared
        );
        return Ok(ReclaimAttempt::Skipped(appeared));
    }
    // Re-derive the one deletable path and re-verify it is still a real
    // dir immediately before the delete — a swap to a symlink must never
    // be followed.
    let Some(target) = reclaim_target(lane, cargo_target.as_deref())? else {
        return Ok(ReclaimAttempt::Absent);
    };
    std::fs::remove_dir_all(&target)?;
    // Log the freed bytes on the issue — a reclaim is recorded, never
    // silent.
    let text = format!(
        "reclaim: removed idle {} on {} — freed {} bytes",
        target.display(),
        lane.display(),
        bytes
    );
    let _ = write::add_comment(
        pm,
        &issue.front.id,
        &text,
        None,
        Some("reclaim"),
        None,
        actor,
    );
    Ok(ReclaimAttempt::Reclaimed(bytes))
}

#[cfg(test)]
mod tests {
    //! Destructive-guard checks only: this module deletes directories.
    use super::*;
    use std::process::Command;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "reclaim-{tag}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// A real repo plus a `git worktree add` lane at `<root>/.cadence/wt/d-1`.
    fn repo_with_lane(tmp: &Tmp) -> (PathBuf, PathBuf) {
        let repo = tmp.0.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(git(&repo, &["init", "-q"]));
        std::fs::write(repo.join("f"), b"x").unwrap();
        assert!(git(&repo, &["add", "f"]));
        assert!(git(&repo, &["commit", "-qm", "c"]));
        let lane = repo.join(".cadence").join("wt").join("d-1");
        assert!(git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                lane.to_str().unwrap(),
                "-b",
                "x-lane"
            ]
        ));
        (repo, lane)
    }

    fn lane_target(lane: &Path) -> PathBuf {
        let t = lane.join("target");
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(t.join("blob"), vec![b'x'; 4096]).unwrap();
        t
    }

    fn no_daemon() -> PathBuf {
        std::env::temp_dir().join("reclaim-no-daemon")
    }

    fn down_view() -> finish::DaemonView {
        finish::daemon_view(&no_daemon())
    }

    fn process_enumeration_uncertain(reason: &str) -> bool {
        reason.contains("Cannot fully enumerate process cwd/open-fd use")
    }

    fn age(dir: &Path, secs: u64) {
        let epoch = (SystemTime::now() - Duration::from_secs(secs))
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let _ = Command::new("find")
            .arg(dir)
            .args(["-exec", "touch", "-h", "-d"])
            .arg(format!("@{epoch}"))
            .arg("{}")
            .arg("+")
            .status();
    }

    fn pm_stub(tmp: &Tmp) -> Pm {
        let pm_dir = tmp.0.join("pm");
        std::fs::create_dir_all(&pm_dir).unwrap();
        std::fs::write(pm_dir.join("pm.yaml"), "schema: 1\n").unwrap();
        Pm::at(&pm_dir).unwrap()
    }

    fn issue_with(lane: &Path, cargo_target: Option<&str>) -> board::Issue {
        let mut i = board::Issue {
            project: "demo".to_string(),
            dir: lane.to_path_buf(),
            front: crate::issue::model::Front::new("D-1", "T", "2026-01-01T00:00:00Z"),
            body: String::new(),
            comments: Vec::new(),
            artifacts: Vec::new(),
        };
        i.front.refs = vec![crate::issue::model::Ref {
            kind: "worktree".to_string(),
            url: None,
            path: Some(lane.display().to_string()),
            label: None,
            closed: None,
            worktree: None,
            cargo_target: cargo_target.map(str::to_string),
            agent: None,
        }];
        i
    }

    fn reclaim(tmp: &Tmp, lane: &Path, cargo_target: Option<&str>) -> Result<Option<u64>> {
        reclaim_lane(
            &down_view(),
            &no_daemon(),
            &pm_stub(tmp),
            &issue_with(lane, cargo_target),
            lane,
            Duration::from_secs(6 * 3600),
            "test",
        )
    }

    thread_local! {
        static PLANT: std::cell::RefCell<Option<std::process::Child>> =
            const { std::cell::RefCell::new(None) };
        static PLANT_ARMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// Test seam called by `reclaim_lane` between the idleness check and
    /// the re-scan: when armed, a `sleep` cwd'd in the lane appears, the
    /// "build just started" condition.
    pub(crate) fn before_rescan(dir: &Path) {
        if !PLANT_ARMED.with(|a| a.get()) {
            return;
        }
        let child = Command::new("sleep")
            .arg("60")
            .current_dir(dir)
            .spawn()
            .expect("plant a build");
        let want = dir.canonicalize().unwrap();
        let link = format!("/proc/{}/cwd", child.id());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline
            && std::fs::read_link(&link).map(|c| c != want).unwrap_or(true)
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        PLANT.with(|p| *p.borrow_mut() = Some(child));
    }

    /// Critical 1: a recorded `cargo_target` that is the lane itself, or a
    /// path inside it, is never deleted — and refuses the lane's real
    /// `target/` too (control: reclaimed when the live-use scan completes).
    #[test]
    fn a_forged_cargo_target_deletes_nothing() {
        let tmp = Tmp::new("forged");
        let (_repo, lane) = repo_with_lane(&tmp);
        let target = lane_target(&lane);
        std::fs::create_dir_all(lane.join("src")).unwrap();
        std::fs::write(lane.join("src/keep.rs"), b"work").unwrap();
        age(&lane, 7 * 3600);
        for forged in [lane.clone(), lane.join("src")] {
            let r = reclaim(&tmp, &lane, forged.to_str());
            assert!(!matches!(r, Ok(Some(_))), "{} reclaimed", forged.display());
            assert!(lane.join("src/keep.rs").is_file(), "source deleted");
            assert!(target.is_dir(), "forged {} was accepted", forged.display());
        }
        match blocked_reason(
            &down_view(),
            &no_daemon(),
            &issue_with(&lane, None).front,
            &lane,
            Duration::from_secs(6 * 3600),
            WALK_CAP,
        ) {
            Some(reason) => {
                assert!(
                    process_enumeration_uncertain(&reason),
                    "unexpected blocker: {reason}"
                );
                assert!(target.is_dir(), "uncertain enumeration must retain target");
            }
            None => {
                assert!(reclaim(&tmp, &lane, None).unwrap().is_some(), "control");
                assert!(lane.join("src/keep.rs").is_file() && !target.exists());
            }
        }
    }

    /// Important 2: the main checkout, and a linked worktree outside
    /// `.cadence/wt`, are refused.
    #[test]
    fn only_linked_lanes_under_the_lane_parent() {
        let tmp = Tmp::new("linked");
        let (repo, lane) = repo_with_lane(&tmp);
        let main_target = lane_target(&repo);
        age(&repo, 7 * 3600);
        assert!(reclaim(&tmp, &repo, None).is_err());
        assert!(main_target.is_dir(), "main checkout target deleted");
        let outside = tmp.0.join("outside");
        assert!(git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                outside.to_str().unwrap(),
                "-b",
                "o-lane"
            ]
        ));
        let outside_target = lane_target(&outside);
        age(&outside, 7 * 3600);
        assert!(reclaim(&tmp, &outside, None).is_err());
        assert!(outside_target.is_dir(), "lane outside .cadence/wt deleted");
        let _ = lane;
    }

    /// Important 2: the shared dep cache, and anything under it, is a
    /// shared-cache path; a lane's own target is not.
    #[test]
    fn shared_dep_cache_is_recognised() {
        let tmp = Tmp::new("shared");
        let (repo, lane) = repo_with_lane(&tmp);
        let shared = crate::worktree::shared_target_dir(&repo);
        std::fs::create_dir_all(shared.join("debug")).unwrap();
        let repo_c = repo.canonicalize().unwrap();
        let shared_c = shared.canonicalize().unwrap();
        assert!(is_shared_cache(&shared_c, &repo_c));
        assert!(is_shared_cache(&shared_c.join("debug"), &repo_c));
        assert!(!is_shared_cache(
            &lane.canonicalize().unwrap().join("target"),
            &repo_c
        ));
    }

    /// Important 3: a truncated idle walk is never read as idle, even when
    /// every entry it did see is old.
    #[test]
    fn a_truncated_walk_is_blocked() {
        let tmp = Tmp::new("walk");
        let (_repo, lane) = repo_with_lane(&tmp);
        for n in 0..10 {
            std::fs::write(lane.join(format!("f{n}")), b"x").unwrap();
        }
        age(&lane, 7 * 3600);
        let idle = Duration::from_secs(6 * 3600);
        assert_eq!(walk_writes(&lane, idle, 3), Walk::Truncated);
        let front = issue_with(&lane, None).front;
        assert!(blocked_reason(&down_view(), &no_daemon(), &front, &lane, idle, 3).is_some());
        if let Some(reason) = blocked_reason(&down_view(), &no_daemon(), &front, &lane, idle, 1000)
        {
            assert!(
                process_enumeration_uncertain(&reason),
                "unexpected blocker: {reason}"
            );
        }
    }

    /// Important 4: a daemon that is up but could not enumerate agents
    /// blocks; it is not read as "no agents".
    #[test]
    fn an_unenumerable_daemon_blocks() {
        let tmp = Tmp::new("enum");
        let (_repo, lane) = repo_with_lane(&tmp);
        age(&lane, 7 * 3600);
        let front = issue_with(&lane, None).front;
        let idle = Duration::from_secs(6 * 3600);
        let view = finish::DaemonView::with_agents(true, vec![]).enumeration_failed();
        assert!(blocked_reason(&view, &no_daemon(), &front, &lane, idle, WALK_CAP).is_some());
    }

    /// A one-shot fake daemon on `<state>/cadence.sock` answering the
    /// first `agent_show` with `show`. The accept is bounded: with the
    /// guard under test disabled no client connects, and the test must
    /// fail on its assertion rather than hang.
    fn fake_daemon(state: &Path, show: Value) -> std::thread::JoinHandle<()> {
        use std::io::{BufRead, Write};
        std::fs::create_dir_all(state).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(state.join("cadence.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut sock = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(_) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    Err(_) => return,
                }
            };
            sock.set_nonblocking(false).unwrap();
            let mut line = String::new();
            std::io::BufReader::new(&sock).read_line(&mut line).unwrap();
            writeln!(sock, "{}", crate::proto::ok(show)).unwrap();
        })
    }

    /// Important 4: a running message recorded against the lane blocks,
    /// through finish's in-use evaluation, with no process or cwd trace.
    #[test]
    fn a_live_message_bound_to_the_lane_blocks() {
        let tmp = Tmp::new("msg");
        let (_repo, lane) = repo_with_lane(&tmp);
        age(&lane, 7 * 3600);
        let state = tmp.0.join("state");
        let server = fake_daemon(
            &state,
            json!({"agent": {"dead": false, "endpoint_kind": "inbox"},
                "messages": [{"id": "m1", "state": "running", "body": "build"}]}),
        );
        let mut front = issue_with(&lane, None).front;
        front.owner = Some("w1".to_string());
        front.refs.push(crate::issue::model::Ref {
            kind: "message".to_string(),
            url: None,
            path: Some("m1".to_string()),
            label: None,
            closed: None,
            worktree: None,
            cargo_target: None,
            agent: Some("w1".to_string()),
        });
        let view = finish::DaemonView::with_agents(true, vec![]);
        let idle = Duration::from_secs(6 * 3600);
        let r = blocked_reason(&view, &state, &front, &lane, idle, WALK_CAP);
        server.join().unwrap();
        assert!(
            r.is_some_and(|r| r.contains("recorded")),
            "message must block"
        );
    }

    /// A registered agent whose cwd is the lane blocks even when the
    /// daemon reports it holds no message, with no /proc trace and no
    /// in-window write.
    #[test]
    fn live_agent_on_the_lane_is_refused() {
        let tmp = Tmp::new("agent");
        let (_repo, lane) = repo_with_lane(&tmp);
        let _t = lane_target(&lane);
        age(&lane, 7 * 3600);
        let idle = Duration::from_secs(6 * 3600);
        let front = issue_with(&lane, None).front;
        if let Some(reason) =
            blocked_reason(&down_view(), &no_daemon(), &front, &lane, idle, WALK_CAP)
        {
            assert!(
                process_enumeration_uncertain(&reason),
                "unexpected control blocker: {reason}"
            );
        }
        let state = tmp.0.join("state");
        let server = fake_daemon(
            &state,
            json!({"agent": {"dead": false, "endpoint_kind": "pty"}, "messages": []}),
        );
        let view = finish::DaemonView::with_agents(
            true,
            vec![json!({"alias": "w1", "cwd": lane.display().to_string()})],
        );
        let r = blocked_reason(&view, &state, &front, &lane, idle, WALK_CAP);
        server.join().unwrap();
        assert!(
            r.is_some_and(|r| r.contains("agent")),
            "agent cwd must block"
        );
    }

    /// A build that appears between the check and the delete is caught by
    /// the just-before-delete re-scan.
    #[test]
    fn a_build_started_in_the_gap_is_caught_by_the_rescan() {
        let tmp = Tmp::new("race");
        let (_repo, lane) = repo_with_lane(&tmp);
        let target = lane_target(&lane);
        age(&lane, 7 * 3600);
        PLANT_ARMED.with(|a| a.set(true));
        let bytes = reclaim(&tmp, &lane, None);
        PLANT_ARMED.with(|a| a.set(false));
        if let Some(mut c) = PLANT.with(|p| p.borrow_mut().take()) {
            let _ = c.kill();
            let _ = c.wait();
        }
        assert_eq!(bytes.unwrap(), None, "the re-scan must skip a racing lane");
        assert!(target.is_dir(), "the racing lane's target survived");
    }
}
