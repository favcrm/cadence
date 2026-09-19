//! `cadence issue finish <ID>` — remove an issue's recorded worktree
//! and branch once the work has landed. Three guards make the unsafe
//! cleanup impossible by default: a busy owner agent (running message
//! or a pty pane that probes busy), a dirty worktree, and a branch
//! whose work survives nowhere (unmerged into the repo's default
//! branch AND unpushed). `--force` overrides each and is recorded as
//! a `Forced:` trailer on the finish commit. Refs are kept as
//! history, marked `closed: true`; the issue's status is untouched —
//! status follows the job or the PM.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::client;
use crate::error::{Error, Result};
use crate::issue::{git, project, write, Pm};

/// Message states that mean the owner is or will be working — a
/// queued kickoff points at the worktree even before it starts.
const LIVE_MESSAGE_STATES: &[&str] = &["queued", "submitting", "running"];

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

    // 2. Dirty worktree — list the dirty paths. A status that cannot
    //    be read refuses too: an unreadable tree is not a clean one.
    if let Some(d) = wt_dir.as_deref().filter(|d| d.is_dir()) {
        match git(d, &["status", "--porcelain"]) {
            Err(e) if force => overridden.push(format!("dirty-check-failed: {e}")),
            Err(e) => {
                return Err(Error::rejected(format!(
                    "Cannot check {} for uncommitted changes ({e}) — \
                     refusing to guess; pass --force",
                    d.display()
                )))
            }
            Ok(dirty) if !dirty.is_empty() => {
                if force {
                    overridden.push("dirty-worktree".to_string());
                } else {
                    let list: Vec<&str> = dirty.lines().take(10).collect();
                    return Err(Error::rejected(format!(
                        "Worktree {} has uncommitted changes:\n  {}\nCommit, \
                         stash or pass --force",
                        d.display(),
                        list.join("\n  ")
                    )));
                }
            }
            Ok(_) => {}
        }
    }

    // 3. Survivability: refuse when the branch is neither merged into
    //    the repo's default branch nor pushed — the work would be lost.
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
            let merged = default_ref(&root).is_some_and(|d| ancestor(&root, &branch, &d));
            let remote_ref = format!("refs/remotes/origin/{branch}");
            let pushed = git(&root, &["rev-parse", "--verify", "--quiet", &remote_ref]).is_ok()
                && ancestor(&root, &branch, &format!("origin/{branch}"));
            if !merged && !pushed {
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
        "status": front.status,
    }))
}
