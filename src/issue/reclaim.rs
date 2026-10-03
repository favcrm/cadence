//! Automatic reclaim of lane build output (CAD-1021 slice 4, operator
//! scope 2026-10-02 ~15:50Z). Two pieces on the daemon checkup's cadence:
//!
//! - **Scheduled merged sweep** — `issue finish --merged` runs as the
//!   library call, same guards (in use, dirty, recent writes) as the
//!   manual command. The gap was that the sweep ran only on `issue sync`
//!   or by hand; lanes whose PRs merged sat forever.
//!
//! - **Idle `target/` reclaim** — a lane that has gone quiet still holds
//!   tens of GB of `target/`. When a lane worktree has no process with a
//!   cwd or open fd inside it, no live message or pane bound to it, and
//!   no write under it for `idle_secs` (default 6 h, `pm.yaml [host]
//!   reclaim_target_idle_secs`), only `<worktree>/target` is deleted —
//!   never the worktree, branch or source. `target/` is a regenerable
//!   cache, so this loses no work. The freed bytes ride the issue's
//!   comment log.
//!
//! Safety invariants (each test below goes red without its guard):
//! - I-A: a running build (a process cwd'd or with an open fd inside the
//!   lane), a live message/pane bound to the lane, or a write inside the
//!   idle window is never reclaimed.
//! - I-B: only `<worktree>/target` (or the lane's recorded
//!   `cargo_target` when it lives inside the lane) is deleted — a
//!   symlinked `target/`, a path escaping the lane, the shared
//!   `.cadence/target/shared`, the main checkout and every other lane's
//!   target are all refused, never removed.
//! - I-C: a reclaim racing a build that is just starting never deletes
//!   files out from under it — the fd/cwd scans are the race window
//!   (cargo holds its target dir open the moment it starts), and the
//!   whole check runs under the tracker lock so two reclaim passes can
//!   never interleave a delete with a stale verdict.
//!
//! The shared `.cadence/target/shared` cache is never a candidate — it
//! belongs to the repo, not the lane, and holds every lane's deps.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::{board, finish, write, Pm};

/// Default idle window for `target/` reclaim — `[host]
/// reclaim_target_idle_secs` overrides.
pub(crate) const RECLAIM_IDLE_SECS: u64 = 6 * 3600;

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

/// The most recent write anywhere under `dir`, or `None` when nothing
/// was written within `window`. Unlike `finish::recent_activity` (a
/// tracked-files `ls-files` probe for the *dirty* question) this counts
/// every path — a build's `target/` writes are exactly what prove a lane
/// is alive. Cheap `mtime` walk; reads nothing's contents.
fn newest_write_under(dir: &Path, window: Duration) -> Option<Duration> {
    let now = SystemTime::now();
    let mut newest: Option<Duration> = None;
    let mut stack = vec![dir.to_path_buf()];
    let mut visited = 0usize;
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for ent in entries.flatten() {
            visited += 1;
            if visited > 50_000 {
                return newest; // bounded walk — never a hang on a huge tree
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
    newest
}

/// The `<worktree>/target` (or recorded `cargo_target`) candidate for a
/// lane — `Some` only when it is a real directory, strictly inside the
/// worktree, and not the shared cache or the main checkout's target.
/// A symlink or an escaping path is a hard `Err` (refuse, never follow).
fn reclaim_target(lane: &Path, cargo_target: Option<&str>, root: &Path) -> Result<Option<PathBuf>> {
    let lane_c = lane.canonicalize().unwrap_or_else(|_| lane.to_path_buf());
    // The candidate: the recorded cargo_target when present, else the
    // conventional <wt>/target.
    let candidate = cargo_target
        .map(PathBuf::from)
        .unwrap_or_else(|| lane.join("target"));
    // Never reclaim outside the lane — the recorded path must resolve
    // strictly inside the worktree.
    if candidate.is_symlink() {
        return Err(Error::rejected(format!(
            "{} is a symlink — a reclaimed target must be a real dir",
            candidate.display()
        )));
    }
    let cand_c = candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.clone());
    if !cand_c.starts_with(&lane_c) {
        return Err(Error::rejected(format!(
            "{} escapes the lane {} — refusing",
            candidate.display(),
            lane.display()
        )));
    }
    // The shared dep cache is repo property — never a lane candidate.
    let shared = crate::worktree::shared_target_dir(root)
        .canonicalize()
        .unwrap_or_else(|_| crate::worktree::shared_target_dir(root));
    if cand_c == shared || cand_c.starts_with(&shared) {
        return Err(Error::rejected(format!(
            "{} is inside the shared dep cache — refusing",
            candidate.display()
        )));
    }
    if candidate.is_dir() {
        Ok(Some(candidate))
    } else {
        Ok(None)
    }
}

/// The reason a lane's `target/` is NOT reclaimable, or `None` when the
/// lane is idle and safe to reclaim. `state_dir` may name a stopped
/// daemon — a down daemon just means no live panes/messages.
fn blocked_reason(view: &finish::DaemonView, lane: &Path, idle: Duration) -> Option<String> {
    // A process standing in the lane, or holding an fd inside it, is a
    // live build or a live shell — never reclaim under it.
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
    // A registered agent whose cwd is on the lane, or a live message or
    // pane bound to it, is work in flight even with no on-disk write yet.
    if view.up && !finish::cwd_holder_aliases(view, Some(lane)).is_empty() {
        return Some("a registered agent's cwd is on the lane".to_string());
    }
    if newest_write_under(lane, idle).is_some() {
        return Some(format!(
            "written within the {}h idle window",
            idle.as_secs() / 3600
        ));
    }
    None
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
    let mut out = json!({"swept": 0, "reclaimed": [], "skipped": []});
    // Part 1: the merged sweep. dry_run=false — this runs the real
    // `finish` guards; a candidate that fails them is `skipped`/`refused`,
    // never removed.
    match finish::sweep(pm, None, false, false, actor, state_dir) {
        Ok(s) => out["swept"] = json!(s["rows"].as_array().map(|r| r.len()).unwrap_or(0)),
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
            let res = reclaim_lane(&view, pm, &issue, &lane, idle, actor);
            match res {
                Ok(Some(bytes)) => out["reclaimed"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"issue": id, "lane": lane, "bytes_freed": bytes})),
                Ok(None) => {}
                Err(e) => out["skipped"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"issue": id, "lane": lane, "reason": e.to_string()})),
            }
        }
    }
    Ok(out)
}

/// One lane's reclaim decision + delete. `Ok(Some(bytes))` reclaimed,
/// `Ok(None)` eligible-but-absent (no target dir), `Err` refused.
fn reclaim_lane(
    view: &finish::DaemonView,
    pm: &Pm,
    issue: &board::Issue,
    lane: &Path,
    idle: Duration,
    actor: &str,
) -> Result<Option<u64>> {
    if !lane.is_dir() {
        return Ok(None);
    }
    // The recorded cargo_target for this lane's open ref, if any.
    let cargo_target = issue
        .front
        .refs
        .iter()
        .filter(|r| r.kind == "worktree" && r.closed != Some(true))
        .filter(|r| finish::same_path(Path::new(r.path.as_deref().unwrap_or_default()), lane))
        .find_map(|r| r.cargo_target.clone());
    let root = crate::worktree::main_root(lane)
        .map_err(|e| Error::rejected(format!("lane {} has no main root: {e}", lane.display())))?;
    let candidate = reclaim_target(lane, cargo_target.as_deref(), &root)?;
    let Some(target) = candidate else {
        return Ok(None);
    };
    if blocked_reason(view, lane, idle).is_some() {
        // Eligible lane but currently in use — skip silently (the daemon
        // being down is covered by the /proc scans still running).
        return Ok(None);
    }
    // Re-verify the path is still a real dir under the lane immediately
    // before the delete — a TOCTOU symlink swap between the check and
    // the rm must never follow the link. `remove_dir_all` itself does
    // not traverse a symlink at the root.
    let meta = std::fs::symlink_metadata(&target)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(Error::rejected(format!(
            "{} is no longer a real dir — refusing the delete",
            target.display()
        )));
    }
    let (bytes, _trunc) = crate::doctor::host::dir_size(&target);
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
    Ok(Some(bytes))
}
