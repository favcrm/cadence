//! The checkout actions of the issue CLI.

use super::*;
use std::path::Path;

pub(crate) fn run(action: &IssueAction, state_dir: &Path) -> Result<i32> {
    match action {
        IssueAction::Checkout { action } => match action {
            CheckoutAction::Inventory { repo } => {
                let pm = open_pm()?;
                let mut out = crate::worktree::lifecycle::inventory(repo)?;
                out["cleanup_plan"] =
                    reclaim::plan_for_repo(&pm, state_dir, reclaim::idle_secs(&pm.dir), repo)?;
                print_json(&out);
                Ok(0)
            }
            CheckoutAction::Adopt {
                repo,
                path,
                purpose,
                tool,
                owner,
                pinned_sha,
                branch,
                release_artifacts,
                rollback_artifacts,
            } => {
                let mut record = crate::worktree::lifecycle::new_record(
                    crate::worktree::lifecycle::CheckoutSpec {
                        repo,
                        purpose,
                        tool,
                        owner,
                        path,
                        branch: branch.as_deref(),
                        pinned_sha,
                        issue: None,
                    },
                );
                record.release_artifacts = release_artifacts.clone();
                record.rollback_artifacts = rollback_artifacts.clone();
                crate::worktree::lifecycle::adopt(repo, record)?;
                print_json(
                    &json!({"adopted": true, "repo": repo, "path": path, "purpose": purpose,
                    "owner": owner, "pinned_sha": pinned_sha, "deletion": "not-performed"}),
                );
                Ok(0)
            }
            CheckoutAction::Release { repo, path, reason } => {
                crate::worktree::lifecycle::release(repo, path, reason)?;
                print_json(&json!({"path": path, "state": "released", "reason": reason,
                    "deletion": "not-performed"}));
                Ok(0)
            }
            CheckoutAction::Resume {
                repo,
                path,
                pinned_sha,
                reason,
            } => {
                crate::worktree::lifecycle::resume(repo, path, pinned_sha, reason)?;
                print_json(
                    &json!({"path": path, "state": "active", "pinned_sha": pinned_sha,
                    "reason": reason, "deletion": "not-performed"}),
                );
                Ok(0)
            }
            CheckoutAction::Retain { repo, path, reason } => {
                crate::worktree::lifecycle::transition(repo, path, "retained", Some(reason))?;
                print_json(&json!({"path": path, "state": "retained", "reason": reason,
                    "deletion": "not-performed"}));
                Ok(0)
            }
        },
        _ => unreachable!("checkout"),
    }
}
