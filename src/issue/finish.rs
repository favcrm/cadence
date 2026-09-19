//! `cadence issue finish <ID>` — remove an issue's recorded worktree
//! and branch once the work has landed. Three guards make the unsafe
//! cleanup impossible by default: a busy owner agent (live message or
//! a pty pane that probes busy — an `inbox` owner is a mailbox, never
//! busy), a dirty worktree (ignored paths don't count), and a branch
//! whose work survives nowhere — merged into the repo's default
//! branch by ancestry, patch-equivalent commits, a squash merge, or
//! a merged PR, or pushed. `--force` overrides each and is recorded
//! as a `Forced:` trailer on the finish commit. Refs are kept as
//! history, marked `closed: true`; the issue's status is untouched —
//! status follows the job or the PM.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use crate::client;
use crate::error::{Error, Result};
use crate::issue::{project, write, Pm};
use crate::proc::{run_bounded, BoundedError};

/// Message states that mean the owner is or will be working — a
/// queued kickoff points at the worktree even before it starts.
const LIVE_MESSAGE_STATES: &[&str] = &["queued", "submitting", "running"];

/// Every git/gh probe in this file runs through the bounded runner —
/// user repos can be slow or locked and finish must not stall.
const GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// `git -C dir <args>`; non-zero exit is a rejected error carrying
/// stderr, like `issue::git`.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = git_out(dir, args, &[])?;
    if !out.status.success() {
        return Err(Error::rejected(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Raw bounded git output for probes that need stdout on any exit.
fn git_out(dir: &Path, args: &[&str], env: &[(&str, &Path)]) -> Result<Output> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    run_bounded(&mut cmd, GIT_TIMEOUT).map_err(|e| match e {
        BoundedError::Spawn(_) => Error::rejected("`git` is required and was not found on PATH"),
        e => Error::rejected(format!("git {} in {}: {e}", args.join(" "), dir.display())),
    })
}

/// The repo's default ref for merge checks: `origin/HEAD` when set,
/// else the main checkout's current branch.
fn default_ref(root: &Path) -> Option<String> {
    if let Ok(origin_head) = git(
        root,
        &["symbolic-ref", "refs/remotes/origin/HEAD", "--short"],
    ) {
        return Some(origin_head);
    }
    git(root, &["symbolic-ref", "--short", "HEAD"]).ok()
}

fn ancestor(root: &Path, branch: &str, into: &str) -> bool {
    git(root, &["merge-base", "--is-ancestor", branch, into]).is_ok()
}

/// Why `branch` counts as merged into `into`, checked in order:
/// plain ancestry; every branch commit already applied upstream
/// (`git cherry` — the rebase/cherry-pick case); the branch's whole
/// diff reverse-applying onto `into` (a squash merge); or a merged
/// GitHub PR naming this head branch. The first match wins and is
/// reported as `merged_by`.
fn merge_rule(root: &Path, branch: &str, into: &str) -> Option<&'static str> {
    if ancestor(root, branch, into) {
        return Some("ancestry");
    }
    // `+` = a commit with no patch-equivalent upstream; a listing
    // with none means everything on the branch already landed.
    if let Ok(marks) = git(root, &["cherry", into, branch]) {
        if !marks.lines().any(|l| l.starts_with('+')) {
            return Some("cherry");
        }
    }
    if patch_applied(root, branch, into) {
        return Some("patch");
    }
    if pr_merged(root, branch) {
        return Some("pr");
    }
    None
}

/// Squash-merge test: if the branch's combined diff against its
/// merge-base reverse-applies cleanly onto `into`'s tree, `into`
/// already holds that state. Runs against a temporary index — no
/// worktree is ever touched.
fn patch_applied(root: &Path, branch: &str, into: &str) -> bool {
    let Ok(base) = git(root, &["merge-base", into, branch]) else {
        return false;
    };
    let Ok(diff) = git(root, &["diff", "--binary", &base, branch]) else {
        return false;
    };
    // An empty diff means the branch contributes nothing — deleting
    // it loses no work.
    if diff.is_empty() {
        return true;
    }
    let Ok(tmp) = TempDir::new() else {
        return false;
    };
    let index = tmp.path().join("index");
    let env = [("GIT_INDEX_FILE", index.as_path())];
    // git_out is Ok on any exit status — the check is the exit code.
    let seeded = git_out(root, &["read-tree", into], &env)
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !seeded {
        return false;
    }
    let patch = tmp.path().join("patch");
    if std::fs::write(&patch, &diff).is_err() {
        return false;
    }
    git_out(
        root,
        &[
            "apply",
            "--check",
            "-R",
            "--cached",
            patch.to_str().unwrap(),
        ],
        &env,
    )
    .map(|o| o.status.success())
    .unwrap_or(false)
}

/// A merged GitHub PR with this head branch counts as merged — but
/// only when the repo actually has a GitHub origin and `gh` answers.
/// Any failure falls through: gh is a hint, never the only authority.
fn pr_merged(root: &Path, branch: &str) -> bool {
    let Ok(url) = git(root, &["remote", "get-url", "origin"]) else {
        return false;
    };
    if !url.contains("github.com") {
        return false;
    }
    let mut cmd = Command::new("gh");
    cmd.args([
        "pr", "list", "--head", branch, "--state", "merged", "--json", "number", "--limit", "1",
    ])
    .current_dir(root);
    let Ok(out) = run_bounded(&mut cmd, Duration::from_secs(10)) else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    serde_json::from_slice::<Value>(&out.stdout)
        .ok()
        .and_then(|v| v.as_array().map(|a| !a.is_empty()))
        .unwrap_or(false)
}

/// Refuse while the owner has a live message or a pty pane probing
/// busy — naming the agent and what it is doing. Daemon-down refuses
/// rather than guesses (a pane the daemon can't see may still be
/// mid-run). Under `--force` the check still runs so `overrode`
/// records what was bypassed truthfully; an unreachable daemon is
/// recorded as such rather than treated as idle.
fn check_owner_idle(
    state_dir: &Path,
    owner: &str,
    force: bool,
    overridden: &mut Vec<String>,
) -> Result<()> {
    let show = client::rpc(state_dir, "agent_show", json!({"alias": owner}));
    let show = match (show, force) {
        (Ok(show), _) => show,
        (Err(_), true) => {
            overridden.push("owner-check-unreachable".to_string());
            return Ok(());
        }
        (Err(e), false) => {
            return Err(Error::rejected(format!(
                "Cannot check owner '{owner}' ({e}) — the daemon must be \
                 reachable to finish safely; rerun when it is up or pass \
                 --force"
            )));
        }
    };
    // An inbox is a durable mailbox, not an actor — nothing it owns
    // can be touching the worktree, so queued messages there never
    // block. The daemon lookup above still runs: kind is only known
    // once the daemon answers.
    if show["agent"]["endpoint_kind"].as_str() == Some("inbox") {
        return Ok(());
    }
    if let Some(msg) = show["messages"].as_array().and_then(|ms| {
        ms.iter()
            .find(|m| LIVE_MESSAGE_STATES.contains(&m["state"].as_str().unwrap_or_default()))
    }) {
        if force {
            overridden.push("owner-busy".to_string());
        } else {
            let body: String = msg["body"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(60)
                .collect();
            return Err(Error::rejected(format!(
                "Owner '{owner}' has a {} message {}: \"{body}\" — wait for \
                 it or pass --force",
                msg["state"].as_str().unwrap_or_default(),
                msg["id"].as_str().unwrap_or_default()
            )));
        }
    }
    if show["agent"]["endpoint_kind"].as_str() == Some("pty")
        && show["agent"]["dead"].as_bool() != Some(true)
    {
        if let Ok(probe) = client::rpc(state_dir, "agent_probe", json!({"alias": owner})) {
            if probe["idle"].as_bool() == Some(false) {
                if force {
                    overridden.push("pane-busy".to_string());
                } else {
                    return Err(Error::rejected(format!(
                        "Owner '{owner}'s pane is busy ({}) — wait for it or \
                         pass --force",
                        probe["reason"].as_str().unwrap_or("not idle")
                    )));
                }
            }
        }
    }
    Ok(())
}

/// `issue finish <ID> [--force] [--keep-branch] [--remote]` — JSON like
/// the other issue verbs.
pub fn run(
    pm: &Pm,
    id: &str,
    force: bool,
    keep_branch: bool,
    remote: bool,
    actor: &str,
    state_dir: &Path,
) -> Result<Value> {
    let (_project, dir) = write::issue_dir(pm, id)?;
    let _lock = pm.lock()?;
    let (mut front, body) = write::load_front(&dir)?;

    // The first OPEN worktree ref, paired with the open branch ref of
    // the SAME name — a re-start under `--name` leaves older pairs
    // behind, and first-of-kind matching could fuse halves of
    // different pairs. A lone open branch ref (hand-edited history)
    // is still finishable on its own.
    let open_wt = front
        .refs
        .iter()
        .find(|r| r.kind == "worktree" && r.closed != Some(true))
        .and_then(|r| r.path.clone())
        .map(PathBuf::from);
    let wt_name = open_wt
        .as_deref()
        .and_then(|d| d.file_name())
        .map(|n| n.to_string_lossy().into_owned());
    let open_branch = |expected: Option<&str>| {
        front
            .refs
            .iter()
            .find(|r| {
                r.kind == "branch"
                    && r.closed != Some(true)
                    && expected.is_none_or(|e| r.path.as_deref() == Some(e))
            })
            .and_then(|r| r.path.clone())
    };
    let branch = match &wt_name {
        Some(n) => open_branch(Some(&format!("cadence/{n}"))).unwrap_or_default(),
        None => open_branch(None).unwrap_or_default(),
    };
    if open_wt.is_none() && branch.is_empty() {
        if front
            .refs
            .iter()
            .any(|r| matches!(r.kind.as_str(), "worktree" | "branch"))
        {
            return Ok(json!({"issue": front.id, "finished": false,
                             "reason": "worktree already finished"}));
        }
        return Err(Error::rejected(format!(
            "{id}: no worktree/branch refs recorded — nothing to finish"
        )));
    }
    let wt_dir = open_wt.clone();

    // The repo root: through the live worktree when it exists, else
    // any recorded `<root>/.cadence/wt/<name>` path walked upward
    // (closed refs still name the repo).
    let root = wt_dir
        .as_deref()
        .filter(|d| d.is_dir())
        .and_then(|d| project::repo_identity(d).map(|(r, _)| r))
        .or_else(|| {
            front
                .refs
                .iter()
                .find(|r| r.kind == "worktree")
                .and_then(|r| r.path.as_deref())
                .and_then(|d| {
                    Path::new(d)
                        .parent()?
                        .parent()?
                        .parent()?
                        .canonicalize()
                        .ok()
                })
        })
        .ok_or_else(|| {
            Error::rejected(format!(
                "Cannot locate the repo for worktree {}",
                wt_dir
                    .as_deref()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default()
            ))
        })?;

    let mut overridden = Vec::new();

    // 1. Owner-busy (daemon) — skipped entirely with no owner.
    if let Some(owner) = front.owner.clone() {
        check_owner_idle(state_dir, &owner, force, &mut overridden)?;
    }

    // 2. Dirty worktree — `--ignored` marks ignored paths `!!` so a
    //    build artifact (like the ui/node_modules symlink) never
    //    blocks; only real changes and non-ignored untracked files do.
    //    A status that cannot be read refuses too: an unreadable tree
    //    is not a clean one.
    if let Some(d) = wt_dir.as_deref().filter(|d| d.is_dir()) {
        match git(d, &["status", "--porcelain", "--ignored"]) {
            Err(e) if force => overridden.push(format!("dirty-check-failed: {e}")),
            Err(e) => {
                return Err(Error::rejected(format!(
                    "Cannot check {} for uncommitted changes ({e}) — \
                     refusing to guess; pass --force",
                    d.display()
                )))
            }
            Ok(status) => {
                let dirty: Vec<&str> = status.lines().filter(|l| !l.starts_with("!!")).collect();
                if !dirty.is_empty() {
                    if force {
                        overridden.push("dirty-worktree".to_string());
                    } else {
                        let list: Vec<&str> = dirty.iter().take(10).copied().collect();
                        return Err(Error::rejected(format!(
                            "Worktree {} has uncommitted changes:\n  {}\nCommit, \
                             stash or pass --force",
                            d.display(),
                            list.join("\n  ")
                        )));
                    }
                }
            }
        }
    }

    // 3. Survivability: refuse when the branch's work survives nowhere
    //    — not merged into the repo's default branch (by ancestry,
    //    patch-equivalent commits, a squash merge, or a merged PR) and
    //    not pushed.
    let mut merged_by = Value::Null;
    if !branch.is_empty() {
        let branch_exists = git(
            &root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )
        .is_ok();
        if branch_exists {
            if let Some(default) = default_ref(&root) {
                if let Some(how) = merge_rule(&root, &branch, &default) {
                    merged_by = json!(how);
                }
            }
            let remote_ref = format!("refs/remotes/origin/{branch}");
            let pushed = git(&root, &["rev-parse", "--verify", "--quiet", &remote_ref]).is_ok()
                && ancestor(&root, &branch, &format!("origin/{branch}"));
            if merged_by.is_null() && !pushed {
                if force {
                    overridden.push("unmerged-unpushed".to_string());
                } else {
                    return Err(Error::rejected(format!(
                        "Branch '{branch}' is neither merged into the default \
                         branch nor pushed — its work would be lost. Merge or \
                         push it, or pass --force"
                    )));
                }
            }
        }
    }

    // Removal: the worktree first (frees the branch), then the branch.
    let _ = git(&root, &["worktree", "prune"]);
    let mut removed_worktree = false;
    if let Some(d) = wt_dir.as_deref().filter(|d| d.is_dir()) {
        let target = d.to_string_lossy().into_owned();
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(&target);
        git(&root, &args).map_err(|e| {
            Error::rejected(format!("git worktree remove {} failed: {e}", d.display()))
        })?;
        removed_worktree = true;
    }
    let mut deleted_branch = false;
    if !keep_branch && !branch.is_empty() {
        deleted_branch = git(&root, &["branch", "-D", &branch]).is_ok();
    }
    let mut remote_deleted = false;
    let mut remote_note = Value::Null;
    if remote && !branch.is_empty() {
        if git(&root, &["remote"])
            .unwrap_or_default()
            .lines()
            .any(|r| r == "origin")
        {
            match git(&root, &["push", "origin", "--delete", &branch]) {
                Ok(_) => remote_deleted = true,
                Err(e) => remote_note = json!(e.to_string()),
            }
        } else {
            remote_note = json!("no 'origin' remote — nothing deleted");
        }
    }

    // One tracker commit marks both refs closed — kept as history.
    for r in &mut front.refs {
        if (r.kind == "worktree"
            && wt_dir
                .as_deref()
                .is_some_and(|d| r.path.as_deref() == Some(d.to_string_lossy().as_ref())))
            || (r.kind == "branch" && !branch.is_empty() && r.path.as_deref() == Some(&branch))
        {
            r.closed = Some(true);
        }
    }
    write::save_front(&dir, &front, &body)?;
    let what = if branch.is_empty() {
        wt_name.as_deref().unwrap_or("worktree").to_string()
    } else {
        branch.clone()
    };
    let subject = if actor.is_empty() {
        format!("{id}: finish {what}")
    } else {
        format!("{id}: finish {what} ({actor})")
    };
    let mut trailers = format!("Issue: {id}\nActor: {}\n", write::actor_who(actor, None));
    if force {
        trailers.push_str("Forced: true\n");
    }
    pm.commit(&format!("{subject}\n\n{trailers}"))?;

    Ok(json!({
        "issue": front.id,
        "finished": true,
        "worktree": wt_dir,
        "branch": branch,
        "removed_worktree": removed_worktree,
        "deleted_branch": deleted_branch,
        "kept_branch": keep_branch,
        "remote_deleted": remote_deleted,
        "remote_note": remote_note,
        "forced": force,
        "overrode": overridden,
        "merged_by": merged_by,
        "status": front.status,
    }))
}
