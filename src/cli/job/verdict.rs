//! Verdict worktree verification and GitHub PR status lookup helpers for `cadence job`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use cadence_agent::error::{Error, Result};
use cadence_agent::proc::BoundedError;
use serde_json::{json, Value};

/// Spawn `prog args` in `cwd`, capture output, kill after 10s — the
/// short timeout every verdict check gets. Ok(stdout) on exit 0; the
/// Err string carries stderr/exit/spawn/timeout.
fn run_capped(prog: &str, args: &[String], cwd: &Path) -> std::result::Result<String, String> {
    let mut cmd = Command::new(prog);
    cmd.args(args).current_dir(cwd);
    let out = match cadence_agent::proc::run_bounded(&mut cmd, Duration::from_secs(10)) {
        Ok(out) => out,
        Err(BoundedError::TimedOut { .. }) => return Err(format!("{prog} timed out after 10s")),
        Err(e) => return Err(format!("{prog}: {e}")),
    };
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(format!(
            "{prog} exited {}: {}",
            out.status.code().unwrap_or(-1),
            stderr
        ))
    }
}

fn run_git(cwd: &Path, args: &[&str]) -> std::result::Result<String, String> {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    run_capped("git", &args, cwd)
}

/// Every `gh` invocation in the verdict bridge goes through here so
/// tests can put a fake `gh` first on PATH.
fn gh(cwd: &Path, args: &[String]) -> std::result::Result<String, String> {
    run_capped("gh", args, cwd)
}

/// `owner/name` from a GitHub remote URL (`git@github.com:o/n.git`,
/// `https://github.com/o/n`, `ssh://git@github.com/o/n`) — None for
/// any other host.
fn github_slug(url: &str) -> Option<String> {
    let rest = url.split_once("github.com")?.1;
    let rest = rest.strip_prefix([':', '/'])?;
    let slug = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = slug.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(owner), Some(name), None) if !owner.is_empty() && !name.is_empty() => {
            Some(format!("{owner}/{name}"))
        }
        _ => None,
    }
}

/// The repo the verdict checks run in: the job's recorded `repo`, or —
/// when the job lost it — the main repo resolved from the task's
/// worktree (`<repo>/.cadence/wt/<name>` shares the object store).
/// Neither existing is a rejection, not a skip: verification was on by
/// default and could not run.
pub(super) fn verdict_repo(job: &Value, task: &Value) -> Result<PathBuf> {
    if let Some(repo) = job["repo"].as_str() {
        return Ok(PathBuf::from(repo));
    }
    if let Some(wt) = task["worktree"].as_str().map(PathBuf::from) {
        if wt.is_dir() {
            if let Ok(common) = run_git(
                &wt,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            ) {
                if let Some(root) = Path::new(common.trim()).parent() {
                    return Ok(root.to_path_buf());
                }
            }
        }
        return Err(Error::rejected(format!(
            "job '{}' has no repo and the worktree {} cannot resolve \
             one — cannot verify the worktree \
             (`--no-verify-worktree` bypasses)",
            task["job"].as_str().unwrap_or("?"),
            wt.display()
        )));
    }
    Err(Error::rejected(format!(
        "job '{}' has no repo — cannot verify the worktree \
         (`--no-verify-worktree` bypasses)",
        task["job"].as_str().unwrap_or("?")
    )))
}

/// `job verdict` worktree verification — bind the judged sha to the
/// task's worktree and branch: it resolves to a commit, it is the tip
/// of the branch, the task's base is an ancestor, the worktree is
/// clean, and `origin/<branch>` equals it (the commit is pushed).
/// Each failed check rejects naming the check and both values; checks
/// that cannot apply land in `skipped` with the reason.
pub(super) fn verify_worktree(repo: &Path, task: &Value, sha: &str) -> Result<Value> {
    let branch = task["branch"].as_str().unwrap_or_default();
    let mut checked: Vec<&str> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let fail = |check: &str, detail: String| {
        Error::rejected(format!("worktree verify — {check}: {detail}"))
    };

    let resolved = run_git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{sha}^{{commit}}"),
        ],
    )
    .map_err(|e| {
        fail(
            "commit",
            format!(
                "{sha} does not resolve to a commit in {} ({e})",
                repo.display()
            ),
        )
    })?
    .trim()
    .to_string();
    checked.push("commit");

    let tip = run_git(
        repo,
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
    )
    .map_err(|e| {
        fail(
            "branch tip",
            format!("branch {branch} does not resolve ({e})"),
        )
    })?
    .trim()
    .to_string();
    if tip != resolved {
        return Err(fail(
            "branch tip",
            format!("branch {branch} is at {tip}, judged sha is {resolved}"),
        ));
    }
    checked.push("branch tip");

    match task["base_sha"].as_str() {
        Some(base) => {
            run_git(repo, &["merge-base", "--is-ancestor", base, &resolved]).map_err(|_| {
                fail(
                    "base ancestor",
                    format!("{base} is not an ancestor of {resolved}"),
                )
            })?;
            checked.push("base ancestor");
        }
        None => skipped.push(json!({"check": "base ancestor",
            "reason": "task has no base_sha"})),
    }

    // `task.worktree` is the scope claim `.cadence/wt/<name>` —
    // `issue start` stores the bare name; an absolute path is honored
    // as recorded.
    let wt = task["worktree"].as_str().map(PathBuf::from).map(|p| {
        if p.is_absolute() {
            p
        } else {
            cadence_agent::worktree::layout::worktree_dir(repo, p)
        }
    });
    match wt {
        Some(dir) if dir.is_dir() => {
            let dirty = run_git(&dir, &["status", "--porcelain"]).map_err(|e| {
                fail(
                    "worktree clean",
                    format!("git status in {}: {e}", dir.display()),
                )
            })?;
            if !dirty.trim().is_empty() {
                return Err(fail(
                    "worktree clean",
                    format!(
                        "{} has uncommitted changes: {}",
                        dir.display(),
                        dirty.lines().take(3).collect::<Vec<_>>().join("; ")
                    ),
                ));
            }
            checked.push("worktree clean");
        }
        Some(dir) => skipped.push(json!({"check": "worktree clean",
            "reason": format!("worktree directory {} is absent", dir.display())})),
        None => skipped.push(json!({"check": "worktree clean",
            "reason": "task has no worktree"})),
    }

    match run_git(repo, &["remote", "get-url", "origin"]) {
        Err(_) => skipped.push(json!({"check": "pushed",
            "reason": "repo has no origin"})),
        Ok(_) => {
            let remote = run_git(
                repo,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("refs/remotes/origin/{branch}"),
                ],
            )
            .map_err(|_| {
                fail(
                    "pushed",
                    format!("origin/{branch} does not resolve — {resolved} is not pushed"),
                )
            })?
            .trim()
            .to_string();
            if remote != resolved {
                return Err(fail(
                    "pushed",
                    format!("origin/{branch} is {remote}, judged sha is {resolved}"),
                ));
            }
            checked.push("pushed");
        }
    }

    Ok(json!({"checked": checked, "skipped": skipped}))
}

/// The open PR to post on: `--pr` names it, else the open PR whose
/// head branch is the task's. Returns `(number, head_sha)` — the head
/// check against the judged sha happens in the caller.
fn lookup_pr(
    repo: &Path,
    slug: &str,
    branch: &str,
    pr: Option<u64>,
) -> std::result::Result<(i64, String), String> {
    match pr {
        Some(n) => {
            let out = gh(
                repo,
                &[
                    "pr".to_string(),
                    "view".to_string(),
                    n.to_string(),
                    "--repo".to_string(),
                    slug.to_string(),
                    "--json".to_string(),
                    "number,headRefOid".to_string(),
                ],
            )?;
            let v: Value = serde_json::from_str(&out)
                .map_err(|e| format!("gh pr view {n}: unreadable response ({e})"))?;
            let head = v["headRefOid"]
                .as_str()
                .ok_or_else(|| format!("gh pr view {n}: no headRefOid in response"))?
                .to_string();
            Ok((n as i64, head))
        }
        None => {
            let out = gh(
                repo,
                &[
                    "pr".to_string(),
                    "list".to_string(),
                    "--repo".to_string(),
                    slug.to_string(),
                    "--head".to_string(),
                    branch.to_string(),
                    "--state".to_string(),
                    "open".to_string(),
                    "--json".to_string(),
                    "number,headRefOid".to_string(),
                ],
            )?;
            let v: Value = serde_json::from_str(&out)
                .map_err(|e| format!("gh pr list: unreadable response ({e})"))?;
            match v.as_array().and_then(|prs| prs.first()) {
                Some(pr) => Ok((
                    pr["number"].as_i64().unwrap_or(0),
                    pr["headRefOid"].as_str().unwrap_or_default().to_string(),
                )),
                None => Err(format!("no open PR for branch {branch}")),
            }
        }
    }
}

/// The qa-verdict bridge — post the `qa-verdict` commit status on the
/// PR head that equals the judged sha (the same API call and context
/// as `scripts/qa-verdict.sh`; the script stays the manual path).
/// Posting never decides the verdict: a missing `gh`, no PR, a moved
/// head, or an API error is reported `{posted: false, reason}` while
/// the committed verdict stands.
pub(super) fn verdict_status_post(
    job: &Value,
    task: &Value,
    sha: &str,
    verdict: &str,
    revision: i64,
    pr: Option<u64>,
) -> Value {
    let reason = |r: String| json!({"posted": false, "reason": r});
    let branch = match task["branch"].as_str() {
        Some(b) => b.to_string(),
        None => return reason("task has no branch".to_string()),
    };
    let repo = match job["repo"].as_str() {
        Some(r) => PathBuf::from(r),
        None => return reason("job has no repo".to_string()),
    };
    let origin = match run_git(&repo, &["remote", "get-url", "origin"]) {
        Ok(u) => u.trim().to_string(),
        Err(_) => return reason("repo has no origin".to_string()),
    };
    let slug = match github_slug(&origin) {
        Some(s) => s,
        None => return reason(format!("origin '{origin}' is not a GitHub remote")),
    };
    let (number, head) = match lookup_pr(&repo, &slug, &branch, pr) {
        Ok(found) => found,
        Err(r) => return reason(r),
    };
    if head != sha {
        return reason(format!("pr head {head} is not the judged sha {sha}"));
    }
    let state = if verdict == "pass" {
        "success"
    } else {
        "failure"
    };
    let task_id = task["id"].as_str().unwrap_or("?");
    let description: String = format!("{verdict} — {task_id} r{revision}")
        .chars()
        .take(140)
        .collect();
    match gh(
        &repo,
        &[
            "api".to_string(),
            "--method".to_string(),
            "POST".to_string(),
            format!("repos/{slug}/statuses/{sha}"),
            "-f".to_string(),
            "context=qa-verdict".to_string(),
            "-f".to_string(),
            format!("state={state}"),
            "-f".to_string(),
            format!("description={description}"),
        ],
    ) {
        Ok(_) => json!({"posted": true, "pr": number, "sha": sha}),
        Err(e) => reason(format!("gh post failed: {e}")),
    }
}
