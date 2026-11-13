//! Hosted lane release (CAD-2178): `issue finish` on a hosted org removes a
//! lane this machine started, through the same evidence and in-use guard as
//! the native finish and with no `--force`. There is no tracker here — the
//! lane was bound from the coordinator ledger by the caller — so only the
//! checkout and its branch are touched.

use super::*;

#[cfg(test)]
thread_local! {
    pub(crate) static BEFORE_FINAL_LOCK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

/// Release `path` (branch `branch`) once its exact tip survives somewhere
/// durable: an ancestor of `origin/<default>`, or the head of a merged PR on
/// origin's own GitHub repository (see [`hosted_evidence`]). Nothing is
/// inferred from content, and "pushed" alone, which native finish accepts, is
/// not enough: a hosted lane has no tracker ref to keep a branch findable.
pub(crate) fn release_hosted_lane(
    issue: &str,
    root: &Path,
    path: &Path,
    branch: &str,
    state_dir: &Path,
    prs: &dyn Fn(&str, &str) -> Option<Value>,
    lock_and_validate: &dyn Fn() -> Result<std::fs::File>,
) -> Result<Value> {
    if !path.is_dir() {
        return Err(Error::rejected(format!(
            "Hosted lane {} is already gone; nothing was removed",
            path.display()
        )));
    }
    verify_hosted_checkout_ownership(root, path, issue, Some(branch))?;
    let t = Target {
        front: Front::new(issue, "hosted lane", "now"),
        dir: PathBuf::new(),
        wt_dir: Some(path.to_path_buf()),
        wt_name: path.file_name().map(|n| n.to_string_lossy().into_owned()),
        branch: branch.to_string(),
        root: root.to_path_buf(),
        msg_refs: std::collections::HashSet::new(),
        cargo_target: None,
    };
    // Without `origin/HEAD`, native falls back to the main checkout's current
    // branch, so "merged" could mean a feature branch. Hosted refuses that.
    if git(
        root,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    )
    .is_err()
    {
        return Err(refuse(
            "origin/HEAD is not set, so the default branch is unknown (run `git remote set-head \
             origin -a`)",
        ));
    }
    let view = daemon_view(state_dir);
    // Our own evidence, never native `merge_rule`: no content inference.
    let into = default_ref(root);
    let tip_now = branch_tip(root, branch);
    let verdict = match &tip_now {
        Some(tip) => hosted_evidence(root, branch, tip, into.as_deref(), prs),
        None => Err("the branch is already gone".to_string()),
    };
    let state = match (&tip_now, &verdict) {
        (None, _) => Branch::Gone,
        (Some(tip), _) if not_started(root, branch, tip) => Branch::NotStarted { tip: tip.clone() },
        (Some(tip), Ok(how)) => Branch::Merged {
            how,
            tip: tip.clone(),
        },
        (Some(tip), Err(_)) => Branch::Open { tip: tip.clone() },
    };
    let ev = Evidence {
        state,
        into_sha: into
            .as_deref()
            .and_then(|d| git(root, &["rev-parse", "--verify", "--quiet", d]).ok()),
        into,
        remote_tip: tracking_tip(root, branch),
        remote_cmp: false,
        remote_note: None,
    };
    // Removing the worktree deletes ignored files silently (this repo ignores
    // `docs/*`, `.env`, `claudedocs/`). Only top-level build output may go.
    let mut blocks = inspect(&view, state_dir, &t, &ev).blocks;
    blocks.retain(|b| b.tag != "unmerged-unpushed");
    if let Some(block) = blocks.into_iter().next() {
        return Err(refuse(&block.reason));
    }
    if let Some(sample) = unexpected_ignored(path)? {
        return Err(refuse(&format!(
            "Worktree {} holds ignored files that removal would delete: {sample}",
            path.display()
        )));
    }
    let (how, tip) = match (&ev.state, verdict, tip_now) {
        (Branch::Merged { .. }, Ok(how), Some(tip)) => (how, tip),
        (_, Err(why), _) => return Err(refuse(&format!("the branch is not shown merged: {why}"))),
        _ => return Err(refuse("the branch is not shown merged")),
    };
    // These checks run after both coordination and lifecycle locks are held,
    // immediately before the lifecycle record enters `releasing`.
    let final_checks = || {
        if let Some(reason) = stale_evidence_reason(root, branch, &ev) {
            return Err(Error::rejected(reason));
        }
        let view = daemon_view(state_dir);
        let mut blocks = inspect(&view, state_dir, &t, &ev).blocks;
        blocks.retain(|block| block.tag != "unmerged-unpushed");
        if let Some(block) = blocks.into_iter().next() {
            return Err(refuse(&block.reason));
        }
        if let Some(sample) = unexpected_ignored(path)? {
            return Err(refuse(&format!(
                "Worktree {} holds ignored files that removal would delete: {sample}",
                path.display()
            )));
        }
        match process_use_under(path) {
            Err(error) => Err(refuse(&format!(
                "Cannot revalidate process use of {} ({error})",
                path.display()
            ))),
            Ok(uses)
                if uses.enumeration_error.is_some()
                    || !uses.cwd.is_empty()
                    || !uses.fd.is_empty() =>
            {
                Err(refuse(&format!("A process is using {}", path.display())))
            }
            Ok(_) => Ok(()),
        }
    };
    #[cfg(test)]
    BEFORE_FINAL_LOCK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
    let _assignment_ledger = lock_and_validate()?;
    verify_hosted_checkout_ownership(root, path, issue, Some(branch))?;
    let mut lifecycle = crate::worktree::lifecycle::begin_hosted_release(
        root,
        path,
        issue,
        Some(branch),
        &format!("hosted issue finish {issue} for branch {branch}"),
        &final_checks,
    )?;
    // No `--force`: git itself refuses a dirty or locked worktree.
    let target = path.to_string_lossy().into_owned();
    if let Err(e) = git(root, &["worktree", "remove", "--", &target]) {
        let reason = format!("git worktree remove {} failed: {e}", path.display());
        let _ = lifecycle.cleanup_failed(&reason);
        return Err(Error::rejected(reason));
    }
    let deleted_branch = delete_branch_at(root, branch, &tip);
    let warning = lifecycle
        .released(&format!(
            "hosted lane released; branch {branch} merged by {how}"
        ))
        .err()
        .map(|e| e.to_string());
    Ok(json!({
        "issue": issue,
        "finished": true,
        "hosted": true,
        "worktree": path,
        "branch": branch,
        "removed_worktree": true,
        "deleted_branch": deleted_branch,
        "branch_note": (!deleted_branch).then_some(
            "branch tip moved during release - kept; its evidence covered the old tip"
        ),
        "merged_by": how,
        "warning": warning,
    }))
}

/// Why the lane's exact tip survives after deletion, or why not. Only two
/// things count: the tip is an ancestor of `origin/<default>` (with
/// `origin/HEAD` set), or a merged PR on origin's own GitHub repository has
/// `headRefOid` exactly equal to the tip, `mergedAt` set and `baseRefName`
/// the default branch (GitHub keeps `refs/pull/N/head` after a squash). A
/// PR whose head is merely an ancestor or descendant of the tip is not.
pub(crate) fn hosted_evidence(
    root: &Path,
    branch: &str,
    tip: &str,
    into: Option<&str>,
    prs: &dyn Fn(&str, &str) -> Option<Value>,
) -> std::result::Result<&'static str, String> {
    let into = into.ok_or("the default branch is unknown")?;
    if ancestor(root, tip, into) {
        return Ok("ancestry");
    }
    let url = git(root, &["remote", "get-url", "origin"])
        .map_err(|_| "the repository has no origin remote".to_string())?;
    let slug = github_slug(&url).ok_or_else(|| {
        "origin is not a github.com repository, so no merged PR can be checked".to_string()
    })?;
    let rows = prs(&slug, branch)
        .ok_or_else(|| format!("could not ask GitHub (`gh`) for merged PRs of {slug}"))?;
    let base = into.strip_prefix("origin/").unwrap_or(into);
    let found = rows.as_array().is_some_and(|rows| {
        rows.iter().any(|pr| {
            pr["headRefOid"].as_str() == Some(tip)
                && pr["mergedAt"].as_str().is_some_and(|t| !t.is_empty())
                && pr["baseRefName"].as_str() == Some(base)
        })
    });
    if found {
        Ok("pr")
    } else {
        Err(format!(
            "branch '{branch}' is neither an ancestor of {into} nor the exact head of a merged \
             PR in {slug}"
        ))
    }
}

/// `owner/repo` of an `origin` url on exactly `github.com` (or
/// `ssh.github.com` over ssh): `git@github.com:o/r`, `ssh://git@github.com[:port]/o/r`,
/// `https://[cred@]github.com/o/r`, with or without `.git`. Anything else is
/// `None`: `notgithub.com`, `github.com.evil.example`, `git.corp.test/github.com/o/r`.
pub(crate) fn github_slug(url: &str) -> Option<String> {
    let url = url.trim();
    let path = if let Some(rest) = url.strip_prefix("git@github.com:") {
        rest
    } else {
        let (scheme, rest) = url.split_once("://")?;
        let (authority, path) = rest.split_once('/')?;
        let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let (host, port) = hostport
            .split_once(':')
            .map_or((hostport, None), |(h, p)| (h, Some(p)));
        if port.is_some_and(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
            return None;
        }
        let allowed = match scheme {
            "https" => host.eq_ignore_ascii_case("github.com"),
            "ssh" => {
                host.eq_ignore_ascii_case("github.com")
                    || host.eq_ignore_ascii_case("ssh.github.com")
            }
            _ => false,
        };
        if !allowed {
            return None;
        }
        path
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    let ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    (parts.next().is_none() && ok(owner) && ok(repo)).then(|| format!("{owner}/{repo}"))
}

/// The real lookup: `gh pr list -R <slug> --head <branch> --state merged`.
/// `None` on any failure (no `gh`, not signed in, a timeout, bad JSON).
pub(crate) fn gh_merged_prs(slug: &str, branch: &str) -> Option<Value> {
    let mut cmd = Command::new("gh");
    cmd.args([
        "-R",
        slug,
        "pr",
        "list",
        "--head",
        branch,
        "--state",
        "merged",
        "--json",
        "number,headRefOid,baseRefName,mergedAt",
        "--limit",
        "10",
    ]);
    let out = crate::gh_guard::run(&mut cmd, Duration::from_secs(10)).ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// Top-level ignored build output a hosted release may delete without asking.
/// `.cadence/` is not listed: nothing the hosted lane writes there was found,
/// and it could hold operator files. Git reports an ignored directory
/// collapsed (`!! target/`), so a file an operator keeps inside `target/` or a
/// cache directory below is deleted with it; that is accepted.
const ALLOWED_IGNORED: [&str; 1] = ["target/"];

/// Dependency and bytecode caches a lane regenerates, deletable at any depth
/// (CAD-2290): an ignored path with one of these as a whole segment.
const CACHE_DIRS: [&str; 2] = ["node_modules", "__pycache__"];

/// Whether an ignored path a hosted release may delete. Everything else
/// ignored (`docs/*`, `claudedocs/`, `docs/target/`, an `.env` lane setup did
/// not write) is the operator's. The lane's `.env` goes only while it is
/// exactly what setup last recorded for this worktree (CAD-2290).
fn removable_ignored(lane: &Path, entry: &str) -> bool {
    ALLOWED_IGNORED.iter().any(|a| entry.starts_with(a))
        || entry
            .trim_end_matches('/')
            .split('/')
            .any(|segment| CACHE_DIRS.contains(&segment))
        || (entry == ".env" && crate::worktree::lane_env_is_setups(lane))
}

/// A short sample of ignored paths a hosted release may not delete, or `None`.
fn unexpected_ignored(path: &Path) -> Result<Option<String>> {
    let status = git(
        path,
        &[
            "status",
            "--porcelain",
            "--ignored",
            "--untracked-files=normal",
        ],
    )?;
    let rest: Vec<&str> = status
        .lines()
        .filter_map(|l| l.strip_prefix("!! "))
        .filter(|p| !removable_ignored(path, p))
        .collect();
    if rest.is_empty() {
        return Ok(None);
    }
    let more = rest.len().saturating_sub(5);
    let mut sample = rest.iter().take(5).copied().collect::<Vec<_>>().join(", ");
    if more > 0 {
        sample.push_str(&format!(" (and {more} more)"));
    }
    Ok(Some(sample))
}

/// A native block reason ends by offering `--force`; hosted release has none.
fn refuse(reason: &str) -> Error {
    let reason = reason
        .replace(
            "reconcile it before finishing this worktree",
            "an operator must resolve the unknown message on this machine's daemon",
        )
        .replace(", or pass --force", "")
        .replace(" or pass --force", "")
        .replace("; pass --force", "")
        .replace(", pass --force", "")
        .replace(
            "Pass --force to finish an abandoned lane",
            "A lane with no work is not released",
        );
    Error::rejected(format!(
        "Hosted issue finish did not release the lane: {reason}. Hosted release has no \
         --force. Resolve this and run `issue finish` again if it can change (pushing the \
         branch alone does not release it); if the lane will never qualify, check it by hand \
         and use `git worktree remove <path>` (this deletes its ignored files) and \
         `git branch -d <branch>` (`-D` for a squash-merged branch, only after verifying \
         the merge). Nothing was removed"
    ))
}
