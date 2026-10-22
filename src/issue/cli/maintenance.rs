//! The maintenance actions of the issue CLI.

use super::*;
use std::path::Path;

pub(crate) fn run(action: &IssueAction, state_dir: &Path) -> Result<i32> {
    match action {
        IssueAction::Lint { project, staged } => {
            let pm = open_pm()?;
            let report = if *staged {
                lint::run_staged(&pm)?
            } else {
                let approvals = work::fetch_approvals(state_dir);
                let delivery = crate::issue::delivery_policy::fetch_approvals(state_dir);
                lint::run_with(&pm, project.as_deref(), Some(&approvals), Some(&delivery))?
            };
            if report["ok"].as_bool() == Some(true) {
                print_json(&report);
                Ok(0)
            } else {
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&report).unwrap_or_default()
                );
                Ok(1)
            }
        }
        IssueAction::Reconcile {
            project,
            issue,
            dry_run,
            limit,
            json,
        } => {
            let pm = open_pm()?;
            // CAD-1377: validate the selector before any probe — an
            // unknown id or one outside `--project` refuses here, at
            // read time, never after git/gh work started.
            if let Some(id) = issue.as_deref() {
                let found = board::find_issue(&pm.dir, id)?;
                if let Some(want) = project.as_deref() {
                    if found.project != want {
                        return Err(Error::rejected(format!(
                            "{id} belongs to project '{}' not '{want}'",
                            found.project
                        )));
                    }
                }
            }
            let out = match issue.as_deref() {
                Some(id) => reconcile::run_selected(
                    &pm,
                    id,
                    project.as_deref(),
                    *dry_run,
                    "",
                    state_dir,
                    *limit,
                )?,
                None => reconcile::run(&pm, project.as_deref(), *dry_run, "", state_dir, *limit)?,
            };
            if *json {
                print_json(&out);
            } else {
                // A failed history walk parks every note-derived
                // candidate without producing a row — say so up
                // front or the sweep reads as a quiet no-op (CAD-878).
                if let Some(e) = reconcile::history_failure(&out) {
                    println!(
                        "history: tracker status-line walk failed ({e}); \
                         note-driven candidates stay parked this sweep"
                    );
                }
                for row in out["rows"].as_array().into_iter().flatten() {
                    let mut line = format!(
                        "{}: {}",
                        row["issue"].as_str().unwrap_or("?"),
                        row["outcome"].as_str().unwrap_or("?")
                    );
                    if let Some(r) = row["reason"].as_str() {
                        line.push_str(&format!("({})", r.lines().next().unwrap_or(r)));
                    }
                    if let Some(e) = row["evidence"].as_str() {
                        line.push_str(&format!(" — {e}"));
                    }
                    println!("{line}");
                }
                // CAD-1377: cleanup rows and refusals must be visible
                // on the human path too — a targeted run that could
                // not clean a lane is partial work, not silence.
                for row in out["finish"]["rows"].as_array().into_iter().flatten() {
                    let mut line = format!(
                        "finish {}: {}",
                        row["issue"].as_str().unwrap_or("?"),
                        row["outcome"].as_str().unwrap_or("?")
                    );
                    if let Some(r) = row["reason"].as_str() {
                        line.push_str(&format!("({})", r.lines().next().unwrap_or(r)));
                    }
                    if let Some(wt) = row["worktree"].as_str() {
                        line.push_str(&format!(" — {wt}"));
                    }
                    println!("{line}");
                }
                let deferred = out["deferred"].as_array().map(|d| d.len()).unwrap_or(0)
                    + out["cleanup_deferred"]
                        .as_array()
                        .map(|d| d.len())
                        .unwrap_or(0);
                if deferred > 0 || out["finish_refused"].as_u64().unwrap_or(0) > 0 {
                    println!(
                        "partial: {} deferred, {} cleanup refusals",
                        deferred,
                        out["finish_refused"].as_u64().unwrap_or(0)
                    );
                }
            }
            // A reconcile whose own writes failed still answers — the
            // row says why. `errors` counts unwritable closes.
            Ok(if out["errors"].as_array().is_some_and(|e| !e.is_empty()) {
                1
            } else {
                0
            })
        }
        IssueAction::Sync {
            no_push,
            dry_run,
            resolve,
        } => {
            let pm = open_pm()?;
            let side = resolve.as_deref().map(|s| match s {
                "ours" => sync::Resolve::Ours,
                _ => sync::Resolve::Theirs,
            });
            let report = sync::run(&pm, !no_push, *dry_run, side)?;
            if report["ok"].as_bool() != Some(true) {
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&report).unwrap_or_default()
                );
                return Ok(1);
            }
            print_json(&report);
            // CAD-754: a fresh upstream is the right time to sweep —
            // status catches up with merges the push just published.
            // Failure never fails the sync that succeeded.
            if !*dry_run {
                match reconcile::run(&pm, None, false, "", state_dir, 0) {
                    Ok(rec) => {
                        if let Some(e) = reconcile::history_failure(&rec) {
                            eprintln!(
                                "reconcile: status-line history failed ({e}); \
                                 note-driven candidates stayed parked"
                            );
                        }
                        let done = rec["done"].as_array().map(|d| d.len()).unwrap_or(0);
                        let held = rec["held"].as_array().map(|h| h.len()).unwrap_or(0);
                        eprintln!("reconcile: {done} closed, {held} held");
                    }
                    Err(e) => eprintln!("reconcile: {e}"),
                }
            }
            Ok(0)
        }
        IssueAction::Sweep { dry_run, grace } => {
            let pm = open_pm()?;
            let out = blocked::sweep(
                &pm,
                grace.unwrap_or(blocked::PARK_GRACE_SECS),
                *dry_run,
                Some(state_dir),
                "",
            )?;
            print_json(&out);
            Ok(0)
        }
        IssueAction::Groom {
            dry_run,
            project,
            json: _,
            grace,
        } => {
            let pm = open_pm()?;
            let out = groom::groom(
                &pm,
                project.as_deref(),
                grace.unwrap_or(groom::GROOM_GRACE_SECS),
                *dry_run,
                "",
            )?;
            print_json(&out);
            Ok(0)
        }
        IssueAction::Sprint { action } => {
            let pm = open_pm()?;
            let out = match action {
                SprintAction::Close {
                    tag,
                    next,
                    drop,
                    dry_run,
                } => sprint::close(
                    &pm,
                    tag,
                    next.as_deref(),
                    *drop,
                    *dry_run,
                    Some(state_dir),
                    "",
                )?,
                SprintAction::Open { tag, cap, dry_run } => {
                    sprint::open(&pm, tag, *cap, *dry_run, Some(state_dir), "")?
                }
            };
            print_json(&out);
            Ok(0)
        }
        _ => unreachable!("maintenance"),
    }
}
