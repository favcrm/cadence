//! Review-owned checkout creation, recovery, restoration and guarded release.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::proc::run_bounded;
use crate::worktree;

use super::{git, git_status};

/// Tool-owned detached checkout. Ownership, exact SHA and lifecycle state
/// live in the repository ledger so an interrupted review is inventoryable.
const REVIEW_TOOL: &str = "cadence review";

pub(super) struct MergeResultState {
    pub(super) base_sha: String,
    pub(super) merge_head: Option<String>,
    pub(super) tree_sha: String,
}

pub(super) struct ReviewTree {
    root: PathBuf,
    pub(super) dir: PathBuf,
    owner: String,
    pinned_sha: String,
    keep: bool,
    retention_reason: Option<String>,
    pub(super) merge_result: Option<MergeResultState>,
    release_guard: Option<worktree::lifecycle::ReleaseGuard>,
    git_secs: u64,
}

impl ReviewTree {
    /// Keep the release decision and its recovery explanation together.
    /// Clearing retention does not bypass the guard and refusal checks in Drop.
    pub(super) fn set_retention(&mut self, reason: Option<String>) {
        self.keep = reason.is_some();
        self.retention_reason = reason;
    }

    /// Create a detached, exact-SHA checkout. Existing paths are inventory-only
    /// until explicitly adopted; this command never reuses an unknown tree.
    pub(super) fn checkout(
        root: &Path,
        name: &str,
        sha: &str,
        base_sha: Option<&str>,
        keep: bool,
        git_secs: u64,
    ) -> Result<Self> {
        let root = worktree::main_root(root)?;
        let dir = worktree::layout::worktree_dir(&root, name);
        let owner = std::env::var("CADENCE_ALIAS").unwrap_or_else(|_| "cadence-review".into());
        let recovery = worktree::lifecycle::recoverable_record(
            &root,
            &dir,
            "review",
            REVIEW_TOOL,
            None,
            None,
        )?;
        if let Some(record) = &recovery {
            if record.owner != owner {
                return Err(Error::rejected(format!(
                    "review checkout {} is owned by '{}', not '{}'; inventory it before recovery",
                    dir.display(),
                    record.owner,
                    owner
                )));
            }
            if record.pinned_sha != sha {
                return Err(Error::rejected(format!(
                    "review checkout {} is owned at pinned SHA {}, not {}; inventory and retain the existing checkout",
                    dir.display(), record.pinned_sha, sha
                )));
            }
            if dir.exists() {
                Self::verify_recovery(&root, &dir, sha, git_secs)?;
            } else if !matches!(record.state.as_str(), "preparing" | "setup-failed") {
                return Err(Error::rejected(format!(
                    "review checkout {} is recorded as {} but is missing; inventory it before retrying",
                    dir.display(), record.state
                )));
            }
        } else if dir.exists() {
            return Err(Error::rejected(format!(
                "review checkout {} already exists without a recoverable lifecycle record — inventory it and explicitly adopt it before reuse",
                dir.display()
            )));
        }
        worktree::ensure_cadence_ignored(&root)?;
        let mut record = worktree::lifecycle::new_record(worktree::lifecycle::CheckoutSpec {
            repo: &root,
            purpose: "review",
            tool: REVIEW_TOOL,
            owner: &owner,
            path: &dir,
            branch: None,
            pinned_sha: sha,
            issue: None,
        });
        record.base_sha = base_sha.map(str::to_string);
        worktree::lifecycle::begin(&root, record.clone())?;
        if !dir.exists() {
            if let Err(e) = git(
                &root,
                &["worktree", "add", "--detach", &dir.to_string_lossy(), sha],
                git_secs,
            ) {
                let _ = worktree::lifecycle::transition(
                    &root,
                    &dir,
                    "setup-failed",
                    Some(&e.to_string()),
                );
                return Err(e);
            }
        }
        // Activation failure leaves the linked checkout and pending record
        // intact; recovery must not infer permission to remove it.
        worktree::lifecycle::activate(&root, record)?;
        Ok(Self {
            root,
            dir,
            owner,
            pinned_sha: sha.to_string(),
            // Errors before the report is durable preserve the tree for
            // recovery. Successful completion opts into release explicitly.
            keep: true,
            retention_reason: Some(if keep {
                "review --keep requested".into()
            } else {
                "review interrupted or failed; preserve for recovery".into()
            }),
            merge_result: None,
            release_guard: None,
            git_secs,
        })
    }

    fn verify_recovery(root: &Path, dir: &Path, sha: &str, git_secs: u64) -> Result<()> {
        let meta = std::fs::symlink_metadata(dir)?;
        if !meta.is_dir()
            || meta.file_type().is_symlink()
            || dir.canonicalize()? != crate::issue::finish::lexical_path(dir)
        {
            return Err(Error::rejected(format!(
                "review checkout {} is not a real canonical directory",
                dir.display()
            )));
        }
        if worktree::main_root(dir)?.canonicalize()? != root.canonicalize()? {
            return Err(Error::rejected(format!(
                "review checkout {} belongs to another Git common directory",
                dir.display()
            )));
        }
        let listed = git(root, &["worktree", "list", "--porcelain"], git_secs)?;
        if !listed
            .lines()
            .any(|line| line == format!("worktree {}", dir.display()))
        {
            return Err(Error::rejected(format!(
                "review checkout {} is not registered by git worktree list",
                dir.display()
            )));
        }
        let head = git(dir, &["rev-parse", "--verify", "HEAD^{commit}"], git_secs)?;
        let branch = git_status(
            dir,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            git_secs,
        )?;
        if head != sha || branch.timed_out || branch.status != Some(1) {
            return Err(Error::rejected(format!(
                "review checkout {} does not match detached pinned SHA {sha}: {}",
                dir.display(),
                branch.stderr.trim()
            )));
        }
        let status = git(
            dir,
            &["status", "--porcelain", "--untracked-files=all"],
            git_secs,
        )?;
        if !status.is_empty() {
            return Err(Error::rejected(format!(
                "review checkout {} is dirty — retain it for recovery",
                dir.display()
            )));
        }
        Ok(())
    }

    pub(super) fn begin_release_guard(&mut self) -> Result<()> {
        if self.release_guard.is_none() {
            self.release_guard = Some(worktree::lifecycle::begin_review_release(
                &self.root,
                &self.dir,
                REVIEW_TOOL,
                &self.owner,
                &self.pinned_sha,
                "review completed",
            )?);
        }
        Ok(())
    }

    /// Restore a successfully gated no-commit merge to the pinned PR head
    /// before normal release. The held release guard has already validated
    /// exact active ownership and keeps retention/removal serialized. Only abort
    /// the merge when the index still equals the recorded merge tree, the
    /// worktree has no extra changes, and no live process can be using it.
    pub(super) fn restore_merge_result(&mut self) -> Result<()> {
        let Some(merge) = self.merge_result.as_ref() else {
            return Ok(());
        };
        if self.release_guard.is_none() {
            return Err(Error::rejected(
                "review merge-result restoration requires its held release guard",
            ));
        }
        let meta = std::fs::symlink_metadata(&self.dir)?;
        if !meta.is_dir()
            || meta.file_type().is_symlink()
            || self.dir.canonicalize()? != crate::issue::finish::lexical_path(&self.dir)
            || worktree::main_root(&self.dir)?.canonicalize()? != self.root
        {
            return Err(Error::rejected(
                "review checkout identity changed before merge-result restoration",
            ));
        }
        let registered =
            crate::issue::finish::git(&self.root, &["worktree", "list", "--porcelain"])?;
        if !registered
            .lines()
            .any(|line| line == format!("worktree {}", self.dir.display()))
        {
            return Err(Error::rejected(
                "review checkout is no longer registered before merge-result restoration",
            ));
        }
        let head =
            crate::issue::finish::git(&self.dir, &["rev-parse", "--verify", "HEAD^{commit}"])?;
        if head != merge.base_sha {
            return Err(Error::rejected(format!(
                "review merge-result HEAD moved from base {} to {head}",
                merge.base_sha
            )));
        }
        let merge_head = git_status(
            &self.dir,
            &["rev-parse", "--quiet", "--verify", "MERGE_HEAD^{commit}"],
            self.git_secs,
        )?;
        match (
            &merge.merge_head,
            merge_head.status,
            merge_head.stdout.trim(),
        ) {
            (Some(expected), Some(0), actual) if expected == actual => {}
            (None, Some(1), _) => {}
            (Some(expected), _, actual) => {
                return Err(Error::rejected(format!(
                    "review MERGE_HEAD changed from recorded {expected} to {actual}"
                )))
            }
            (None, status, _) => {
                return Err(Error::rejected(format!(
                    "review unexpectedly has MERGE_HEAD (git status {status:?})"
                )))
            }
        }
        let index_tree = crate::issue::finish::git(&self.dir, &["write-tree"])?;
        if index_tree != merge.tree_sha {
            return Err(Error::rejected(
                "review merge-result index changed; retaining checkout",
            ));
        }
        let unstaged = git_status(&self.dir, &["diff", "--quiet", "--"], self.git_secs)?;
        if unstaged.timed_out || unstaged.status != Some(0) {
            return Err(Error::rejected(
                "review merge-result worktree changed; retaining checkout",
            ));
        }
        let untracked =
            crate::issue::finish::git(&self.dir, &["ls-files", "--others", "--exclude-standard"])?;
        if !untracked.is_empty() {
            return Err(Error::rejected(
                "review merge-result has untracked files; retaining checkout",
            ));
        }
        match crate::issue::process_use::process_use_under(&self.dir) {
            Ok(uses)
                if uses.enumeration_error.is_none()
                    && uses.cwd.is_empty()
                    && uses.fd.is_empty() => {}
            Ok(uses) => {
                return Err(Error::rejected(format!(
                    "review checkout has live or unknown process use; retaining checkout ({:?})",
                    uses.enumeration_error
                )))
            }
            Err(error) => {
                return Err(Error::rejected(format!(
                    "cannot enumerate review checkout process use; retaining checkout ({error})"
                )))
            }
        }
        if merge.merge_head.is_some() {
            crate::issue::finish::git(&self.dir, &["merge", "--abort"])?;
        }
        let head =
            crate::issue::finish::git(&self.dir, &["rev-parse", "--verify", "HEAD^{commit}"])?;
        let status = crate::issue::finish::git(
            &self.dir,
            &["status", "--porcelain", "--untracked-files=all"],
        )?;
        if head != merge.base_sha || !status.is_empty() {
            return Err(Error::rejected(
                "review merge-result did not restore a clean base; retaining checkout",
            ));
        }
        crate::issue::finish::git(&self.dir, &["checkout", "--detach", &self.pinned_sha])?;
        let head =
            crate::issue::finish::git(&self.dir, &["rev-parse", "--verify", "HEAD^{commit}"])?;
        let status = crate::issue::finish::git(
            &self.dir,
            &["status", "--porcelain", "--untracked-files=all"],
        )?;
        let branch = git_status(
            &self.dir,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            self.git_secs,
        )?;
        if head != self.pinned_sha || !status.is_empty() || branch.status != Some(1) {
            return Err(Error::rejected(
                "review checkout did not return to its clean detached pinned head; retaining checkout",
            ));
        }
        self.merge_result = None;
        Ok(())
    }

    fn owns(&self) -> bool {
        worktree::lifecycle::owns(
            &self.root,
            &self.dir,
            REVIEW_TOOL,
            &self.owner,
            &self.pinned_sha,
        )
        .unwrap_or(false)
    }

    fn release_refusal(&self) -> Option<String> {
        match std::fs::symlink_metadata(&self.dir) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => return Some("review checkout path is not a real directory".into()),
            Err(e) => return Some(format!("cannot inspect review checkout path: {e}")),
        }
        let checkout_root = match worktree::main_root(&self.dir) {
            Ok(root) => match root.canonicalize() {
                Ok(root) => root,
                Err(e) => return Some(format!("cannot resolve review Git common directory: {e}")),
            },
            Err(e) => return Some(format!("cannot verify review checkout identity: {e}")),
        };
        if checkout_root != self.root {
            return Some("review checkout belongs to a different Git common directory".into());
        }
        let listed =
            match crate::issue::finish::git(&self.root, &["worktree", "list", "--porcelain"]) {
                Ok(listed) => listed,
                Err(e) => return Some(format!("cannot enumerate registered worktrees: {e}")),
            };
        if !listed
            .lines()
            .any(|line| line == format!("worktree {}", self.dir.display()))
        {
            return Some("review checkout is no longer registered at its owned path".into());
        }
        let head =
            match crate::issue::finish::git(&self.dir, &["rev-parse", "--verify", "HEAD^{commit}"])
            {
                Ok(head) => head,
                Err(e) => return Some(format!("cannot verify pinned review HEAD: {e}")),
            };
        if head != self.pinned_sha {
            return Some(format!(
                "HEAD moved from pinned {} to {head}",
                self.pinned_sha
            ));
        }
        match crate::issue::finish::git(
            &self.dir,
            &["status", "--porcelain", "--untracked-files=all"],
        ) {
            Ok(status) if !status.is_empty() => {
                return Some("review checkout has uncommitted changes".into())
            }
            Err(e) => return Some(format!("cannot inspect review checkout dirt: {e}")),
            _ => {}
        }
        match crate::issue::process_use::process_use_under(&self.dir) {
            Ok(uses) if uses.enumeration_error.is_some() => Some(format!(
                "cannot fully enumerate live process use: {}",
                uses.enumeration_error
                    .as_deref()
                    .unwrap_or("unknown enumeration failure")
            )),
            Ok(uses) if !uses.cwd.is_empty() || !uses.fd.is_empty() => Some(format!(
                "live process use remains (cwd pids: {:?}, open-fd pids: {:?})",
                uses.cwd, uses.fd
            )),
            Err(e) => Some(format!("cannot enumerate live process use: {e}")),
            _ => None,
        }
    }
}

impl Drop for ReviewTree {
    fn drop(&mut self) {
        if self.keep {
            let reason = self
                .retention_reason
                .as_deref()
                .unwrap_or("review explicitly retained");
            if let Some(mut release) = self.release_guard.take() {
                if let Err(e) = release.retained(reason) {
                    tracing::warn!(event = "review_checkout_retention_record_failed", path = %self.dir.display(), error = %e);
                }
            } else if self.owns() {
                if let Err(e) =
                    worktree::lifecycle::transition(&self.root, &self.dir, "retained", Some(reason))
                {
                    tracing::warn!(event = "review_checkout_retention_record_failed", path = %self.dir.display(), error = %e);
                }
            }
            return;
        }
        let Some(mut release) = self.release_guard.take() else {
            let reason =
                "review completed without its pre-acquired release guard; preserve checkout";
            if self.owns() {
                let _ = worktree::lifecycle::transition(
                    &self.root,
                    &self.dir,
                    "retained",
                    Some(reason),
                );
            }
            tracing::warn!(event = "review_checkout_release_record_failed", path = %self.dir.display(), reason);
            return;
        };
        if let Some(reason) = self.release_refusal() {
            let _ = release.retained(&reason);
            tracing::warn!(event = "review_checkout_release_refused", path = %self.dir.display(), reason = %reason);
            return;
        }
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.root)
            .args(["worktree", "remove"])
            .arg(&self.dir);
        let result = run_bounded(&mut cmd, Duration::from_secs(self.git_secs));
        let failure = match result {
            Ok(out) if out.status.success() && !self.dir.exists() => {
                if let Err(e) = release.released("review completed") {
                    tracing::warn!(event = "review_checkout_release_record_failed", path = %self.dir.display(), error = %e);
                }
                None
            }
            Ok(out) => Some(format!(
                "git worktree remove failed: {}{}",
                String::from_utf8_lossy(&out.stderr).trim(),
                if self.dir.exists() {
                    " (checkout still exists)"
                } else {
                    ""
                }
            )),
            Err(e) => Some(format!("git worktree remove failed: {e}")),
        };
        if let Some(reason) = failure {
            let _ = release.cleanup_failed(&reason);
            tracing::warn!(event = "review_checkout_release_failed", path = %self.dir.display(), reason = %reason);
        }
    }
}
