//! Hosted lane release (CAD-2178): `issue finish` on a hosted org removes a
//! lane this machine started, through the same evidence and in-use guard as
//! the native finish and with no `--force`. There is no tracker here — the
//! lane was bound from the coordinator ledger by the caller — so only the
//! checkout and its branch are touched.

use super::*;

/// Release `path` (branch `branch`) once its work is merged, it is clean and
/// idle. Merge evidence must be `Branch::Merged` (ancestry, patch-equivalent
/// commits, a squash, or a merged PR with this tip); "pushed" alone, which
/// the native finish accepts, is not enough: a hosted lane has no tracker
/// ref to keep a surviving branch findable.
pub(crate) fn release_hosted_lane(
    issue: &str,
    root: &Path,
    path: &Path,
    branch: &str,
    state_dir: &Path,
) -> Result<Value> {
    if !path.is_dir() {
        return Err(Error::rejected(format!(
            "Hosted lane {} is already gone; nothing was removed",
            path.display()
        )));
    }
    verify_checkout_ownership(root, path, issue, Some(branch))?;
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
    let view = daemon_view(state_dir);
    let ev = evidence(&t, false);
    let (_, tip, survivability_block) = survivability(&ev, &t);
    let mut blocks = inspect(&view, state_dir, &t, &ev).blocks;
    blocks.extend(survivability_block);
    if let Some(block) = blocks.into_iter().next() {
        return Err(refuse(&block.reason));
    }
    // Removing the worktree deletes ignored files silently (this repo ignores
    // `docs/*`, `.env`, `claudedocs/`). Only build output and lane state may go.
    if let Some(sample) = unexpected_ignored(path)? {
        return Err(refuse(&format!(
            "Worktree {} holds ignored files that removal would delete: {sample}",
            path.display()
        )));
    }
    let (how, tip) = match (&ev.state, tip) {
        (Branch::Merged { how, .. }, Some(tip)) => (*how, tip),
        _ => {
            return Err(refuse(&format!(
                "Branch '{branch}' is not shown merged into the default branch, so its work \
                 might be lost"
            )))
        }
    };
    // Re-read the inputs the evidence depended on at the destructive boundary.
    if let Some(reason) = stale_evidence_reason(root, branch, &ev) {
        return Err(Error::rejected(reason));
    }
    match process_use_under(path) {
        Err(e) => {
            return Err(refuse(&format!(
                "Cannot revalidate process use of {} ({e})",
                path.display()
            )))
        }
        Ok(uses)
            if uses.enumeration_error.is_some() || !uses.cwd.is_empty() || !uses.fd.is_empty() =>
        {
            return Err(refuse(&format!("A process is using {}", path.display())))
        }
        Ok(_) => {}
    }
    verify_checkout_ownership(root, path, issue, Some(branch))?;
    let mut lifecycle = crate::worktree::lifecycle::begin_release(
        root,
        path,
        issue,
        Some(branch),
        &format!("hosted issue finish {issue} for branch {branch}"),
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

/// Ignored paths a hosted release may delete without asking: build output and
/// the lane's own `.cadence/` state. Anything else ignored (`.env`, `docs/*`,
/// `claudedocs/`) is the operator's.
const ALLOWED_IGNORED: [&str; 3] = ["target/", "node_modules/", ".cadence/"];

/// A short sample of ignored paths outside [`ALLOWED_IGNORED`], or `None`.
fn unexpected_ignored(path: &Path) -> Result<Option<String>> {
    let status = git(path, &["status", "--porcelain", "--ignored"])?;
    let rest: Vec<&str> = status
        .lines()
        .filter_map(|l| l.strip_prefix("!! "))
        .filter(|p| {
            !ALLOWED_IGNORED
                .iter()
                .any(|a| p.starts_with(a) || p.contains(&format!("/{a}")))
        })
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
         --force; resolve this (pushing the branch alone does not release it) and run \
         `issue finish` again. Nothing was removed"
    ))
}
