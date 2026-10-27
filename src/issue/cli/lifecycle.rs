//! The lifecycle actions of the issue CLI.

use super::*;
use std::path::Path;

pub(crate) fn run(action: &IssueAction, state_dir: &Path) -> Result<i32> {
    match action {
        IssueAction::Start {
            id,
            repo,
            name,
            base,
            owner,
            job,
            pm,
            spec,
            assignee,
            force,
            by,
            take_over,
        } => {
            let pm_dir = open_pm()?;
            let args = start::StartArgs {
                by: by.clone(),
                take_over: take_over.clone(),
                repo: repo.clone(),
                name: name.clone(),
                base: base.clone(),
                owner: owner.clone(),
                job: if *job {
                    Some(start::JobArgs {
                        id: None,
                        pm: pm.clone().unwrap_or_default(),
                        spec: spec.clone().unwrap_or_default(),
                        assignee: assignee.clone(),
                        force: *force,
                    })
                } else {
                    None
                },
            };
            let out = start::run(&pm_dir, id, &args, "", state_dir)?;
            if let Some(w) = out["claim"]["warning"].as_str() {
                eprintln!("warning: {w}");
            }
            // CAD-378: advisory lease warnings.
            for line in crate::issue::areas::warning_lines(&out["leases"]) {
                eprintln!("warning: {line}");
            }
            print_json(&out);
            Ok(0)
        }
        IssueAction::Claim {
            id,
            by,
            note,
            take_over,
        } => {
            let pm = open_pm()?;
            let out = claim::claim(
                &pm,
                id,
                by.as_deref(),
                note.as_deref(),
                take_over.as_deref(),
                "",
            )?;
            if let Some(w) = out["warning"].as_str() {
                eprintln!("warning: {w}");
            }
            print_json(&out);
            Ok(0)
        }
        IssueAction::Ack { id, area, note } => {
            model::check_id(id)?;
            print_json(&crate::client::rpc(
                state_dir,
                "area_ack",
                json!({"issue": id, "area": area, "note": note}),
            )?);
            Ok(0)
        }
        IssueAction::Release { id, by, note } => {
            let pm = open_pm()?;
            print_json(&claim::release(
                &pm,
                id,
                by.as_deref(),
                note.as_deref(),
                "",
            )?);
            Ok(0)
        }
        IssueAction::Reclaim { apply, idle_secs } => {
            let pm = open_pm()?;
            let idle = idle_secs.unwrap_or_else(|| reclaim::idle_secs(&pm.dir));
            let out = if *apply {
                reclaim::run_with_idle(&pm, state_dir, "", idle)?
            } else {
                reclaim::plan(&pm, state_dir, idle)?
            };
            print_json(&out);
            Ok(0)
        }
        IssueAction::Finish {
            id,
            worktree,
            force,
            keep_branch,
            remote,
            merged,
            project,
            dry_run,
            json,
        } => {
            let pm_dir = open_pm()?;
            if *merged {
                let out = finish::sweep(
                    &pm_dir,
                    project.as_deref(),
                    *remote,
                    *dry_run,
                    "",
                    state_dir,
                )?;
                if *json {
                    print_json(&out);
                } else {
                    for row in out["rows"].as_array().into_iter().flatten() {
                        let mut line = format!(
                            "{}: {}",
                            row["issue"].as_str().unwrap_or("?"),
                            row["outcome"].as_str().unwrap_or("?")
                        );
                        if let Some(r) = row["reason"].as_str() {
                            line.push_str(&format!("({})", r.lines().next().unwrap_or(r)));
                        }
                        if let Some(wt) = row["worktree"].as_str() {
                            line.push_str(&format!(" — {wt}"));
                        }
                        if let Some(how) = row["merged_by"].as_str() {
                            line.push_str(&format!(" [{how}]"));
                        }
                        println!("{line}");
                    }
                }
                return Ok(if out["refused"].as_u64().unwrap_or(0) > 0 {
                    1
                } else {
                    0
                });
            }
            let Some(id) = id.as_deref() else {
                return Err(Error::rejected(
                    "issue finish needs an id — or --merged to sweep",
                ));
            };
            // CAD-800: fail closed — clap's `requires = "merged"` alone
            // does not reject `finish <ID> --dry-run` when the positional
            // ID is present (the installed build silently ignored the flag
            // and ran a real finish). Sweep-only flags are parser-rejected
            // via `conflicts_with = "id"` above; this guard covers any
            // parser bypass (argument order, future flags). `--json` is
            // deliberately NOT guarded: single-ID output is always JSON,
            // so it is an accepted no-op there (see
            // `finish_guard_per_worktree`).
            if *dry_run || project.is_some() {
                return Err(Error::rejected(
                    "issue finish --dry-run and --project need --merged — a single-ID finish has no preview; omit them or use `issue finish --merged --dry-run` to preview the sweep",
                ));
            }
            let args = finish::FinishArgs {
                force: *force,
                keep_branch: *keep_branch,
                remote: *remote,
                worktree: worktree.as_deref(),
                close_if_gone: worktree.is_some(),
            };
            print_json(&finish::run(&pm_dir, id, &args, "", state_dir, None)?);
            Ok(0)
        }
        _ => unreachable!("lifecycle"),
    }
}
