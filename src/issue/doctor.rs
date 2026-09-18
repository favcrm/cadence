//! `issue doctor` — a read-only health report for the tracker: root,
//! git repo, remote, hook state, lint, push lag and the failure-log
//! tail. Nothing here writes; the exit code is 1 when any check fails.

use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

use crate::error::Result;
use crate::issue::{hooks, lint, Pm};

/// A git probe that never fails the report — `None` means "absent",
/// matching doctor semantics (a missing repo is data, not an error).
fn probe(pm_dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(pm_dir)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Some(text)
}

/// The last `n` non-empty lines of `<gitdir>/push-failures.log`, which
/// the post-commit hook appends to when a background push fails.
fn push_failures(git_dir: &Path) -> Value {
    let path = git_dir.join("push-failures.log");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Value::Null;
    };
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let tail: Vec<&String> = lines
        .iter()
        .filter(|l| !l.is_empty())
        .rev()
        .take(10)
        .collect();
    json!({
        "path": path,
        "lines": lines.len(),
        "tail": tail.into_iter().rev().cloned().collect::<Vec<_>>(),
    })
}

/// Push state against `origin/<branch>`: ahead/behind counts and
/// whether the last push left nothing outstanding. `None` without a
/// remote; `upstream: false` when the branch was never pushed.
fn push_state(pm_dir: &Path) -> Option<Value> {
    let url = probe(pm_dir, &["remote", "get-url", "origin"])?;
    let branch = probe(pm_dir, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let upstream = branch.as_deref().map(|b| format!("origin/{b}"));
    let has_upstream = upstream
        .as_deref()
        .is_some_and(|u| probe(pm_dir, &["rev-parse", "--verify", "--quiet", u]).is_some());
    let (ahead, behind) = if has_upstream {
        let range = format!("{}...{}", upstream.as_deref().unwrap_or_default(), "HEAD");
        let counts = probe(pm_dir, &["rev-list", "--left-right", "--count", &range]);
        let mut parts = counts.as_deref().unwrap_or("0\t0").split_whitespace();
        // Left of `...` counts upstream-only commits (behind), right
        // counts HEAD-only ones (ahead).
        let behind = parts
            .next()
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0);
        let ahead = parts
            .next()
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0);
        (ahead, behind)
    } else {
        (0, 0)
    };
    let mut push = json!({
        "remote": url,
        "branch": branch,
        "upstream": has_upstream,
        "ahead": ahead,
        "behind": behind,
        "up_to_date": has_upstream && ahead == 0,
    });
    // Behind the remote → `issue sync` is the path back level.
    if behind > 0 {
        push["sync"] = json!("cadence issue sync");
    }
    Some(push)
}

/// Share of the last 50 commits carrying the CAD-42 `Actor:`/`Issue:`
/// trailers — informational only; history works without them and lint
/// does not require them.
fn trailer_share(pm_dir: &Path) -> Value {
    let Some(log) = probe(
        pm_dir,
        &[
            "log",
            "-50",
            "--format=%x1f%(trailers:key=Actor,valueonly,separator=%x2C)",
        ],
    ) else {
        return Value::Null;
    };
    let mut checked = 0u64;
    let mut with_actor = 0u64;
    for line in log.lines() {
        let Some((_, actor)) = line.split_once('\x1f') else {
            continue;
        };
        checked += 1;
        if !actor.trim().is_empty() {
            with_actor += 1;
        }
    }
    json!({"window": checked, "with_trailers": with_actor})
}

pub fn run(pm: &Pm) -> Result<Value> {
    let git_dir = hooks::git_dir(&pm.dir);
    let hooks_report = hooks::report(&pm.dir);
    let hooks_ok = ["pre-commit", "post-commit"].iter().all(|name| {
        let h = &hooks_report[name];
        h["present"] == true && h["executable"] == true && h["owner"] == "cadence"
    });
    let lint_report = lint::run(pm, None)?;
    let lint_ok = lint_report["ok"] == true;
    let push = push_state(&pm.dir);
    // No remote is a legal shape (post-commit no-ops); with a remote,
    // unpushed commits or a missing upstream count as not-ok.
    let push_ok = push
        .as_ref()
        .map(|p| p["up_to_date"] == true)
        .unwrap_or(true);
    let ok = git_dir.is_some() && hooks_ok && lint_ok && push_ok;
    Ok(json!({
        "ok": ok,
        "pm_dir": pm.dir,
        "git": git_dir.is_some(),
        "remote": probe(&pm.dir, &["remote", "get-url", "origin"]),
        "hooks": hooks_report,
        "lint": {
            "ok": lint_ok,
            "errors": lint_report["errors"].as_array().map(|e| e.len()).unwrap_or(0),
            "warnings": lint_report["warnings"].as_array().map(|w| w.len()).unwrap_or(0),
        },
        "push": push,
        "push_failures": git_dir.as_deref().map(push_failures).unwrap_or(Value::Null),
        "trailers": trailer_share(&pm.dir),
    }))
}
