//! Guarded release and target-reclaim transactions for managed worktree checkouts.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::fs;
use std::path::{Path, PathBuf};

use super::{
    canonical_root, path_key, read_ledger, record_path_matches, validate_record, write_ledger,
    Checkout, LedgerLock,
};
use crate::error::{Error, Result};
use crate::issue::{finish, time};
use crate::worktree;

/// Holds the repository lifecycle lock through destructive cleanup and its
/// final state. A legacy checkout without a record still holds the lock to fence
/// adoption; concurrent retention and release cannot cross the deletion boundary.
pub struct ReleaseGuard {
    root: PathBuf,
    dir: PathBuf,
    key: Option<String>,
    _lock: LedgerLock,
}

impl ReleaseGuard {
    fn finish(&mut self, state: &str, reason: &str) -> Result<()> {
        let Some(key) = self.key.as_deref() else {
            return Ok(());
        };
        if reason.trim().is_empty() {
            return Err(Error::rejected(format!(
                "checkout state '{state}' requires a reason"
            )));
        }
        let mut ledger = read_ledger(&self.root)?;
        let record = ledger
            .records
            .iter_mut()
            .find(|record| record.path == key)
            .ok_or_else(|| Error::rejected(format!("{key} has no managed ownership record")))?;
        if record.state != "releasing" {
            return Err(Error::rejected(format!(
                "{key} changed lifecycle state during release; refusing to record {state}"
            )));
        }
        record.state = state.to_string();
        record.updated_at = time::now_epoch();
        if state == "retained" {
            record.retention_reason = Some(reason.to_string());
        } else {
            record.release_reason = Some(reason.to_string());
        }
        write_ledger(&self.dir, &ledger)
    }

    pub fn released(&mut self, reason: &str) -> Result<()> {
        self.finish("released", reason)
    }

    pub fn retained(&mut self, reason: &str) -> Result<()> {
        self.finish("retained", reason)
    }

    pub fn cleanup_failed(&mut self, reason: &str) -> Result<()> {
        self.finish("cleanup-failed", reason)
    }
}

fn validate_refs_only_finish_target(root: &Path, path: &Path, branch: Option<&str>) -> Result<()> {
    let lexical = finish::lexical_path(path);
    if !path.is_absolute() || path != lexical.as_path() {
        return Err(Error::rejected(format!(
            "refs-only finish path {} is not canonical; refusing closure",
            path.display()
        )));
    }
    let parent = path.parent().ok_or_else(|| {
        Error::rejected(format!(
            "refs-only finish path {} has no parent; refusing closure",
            path.display()
        ))
    })?;
    let canonical_parent = parent.canonicalize().map_err(|error| {
        Error::rejected(format!(
            "cannot verify refs-only finish parent {}: {error}",
            parent.display()
        ))
    })?;
    if canonical_parent != parent {
        return Err(Error::rejected(format!(
            "refs-only finish path {} has a symlinked parent alias; refusing closure",
            path.display()
        )));
    }
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(Error::rejected(format!(
                "refs-only finish path {} reappeared; refusing closure",
                path.display()
            )))
        }
        Err(error) => {
            return Err(Error::rejected(format!(
                "cannot verify refs-only finish path {} is absent: {error}",
                path.display()
            )))
        }
    }
    if let Some(branch) = branch.filter(|branch| !branch.is_empty()) {
        if let Some(live) = finish::registered_branch_path(root, branch)?
            .filter(|live| !finish::same_path(live, path))
        {
            return Err(Error::rejected(format!(
                "issue-owned branch {branch} is checked out at {}, not {}; refusing refs-only finish",
                live.display(),
                path.display()
            )));
        }
    }
    Ok(())
}

struct ReleaseOptions<'a> {
    allow_legacy_absence: bool,
    allow_already_released: bool,
    bind_issue: Option<&'a str>,
}

fn begin_release_transaction(
    repo: &Path,
    path: &Path,
    reason: &str,
    refused: String,
    options: ReleaseOptions<'_>,
    expected: impl FnOnce(Option<&Checkout>) -> Result<bool>,
) -> Result<ReleaseGuard> {
    if reason.trim().is_empty() {
        return Err(Error::rejected("checkout release requires a reason"));
    }
    let root = canonical_root(repo)?;
    let key = path_key(path);
    let (dir, lock) = LedgerLock::acquire(&root)?;
    let mut ledger = read_ledger(&root)?;
    let matches: Vec<usize> = ledger
        .records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| record_path_matches(record, &key).then_some(index))
        .collect();
    if matches.len() > 1 {
        return Err(Error::rejected(format!(
            "multiple lifecycle ownership records name {}; refusing release",
            path.display()
        )));
    }
    let Some(index) = matches.first().copied() else {
        if !options.allow_legacy_absence {
            return Err(Error::rejected(format!(
                "{} has no managed ownership record; refusing deletion",
                path.display()
            )));
        }
        if !expected(None)? {
            return Err(Error::rejected(refused));
        }
        return Ok(ReleaseGuard {
            root,
            dir,
            key: None,
            _lock: lock,
        });
    };
    let record = &ledger.records[index];
    validate_record(&root, record)?;
    if record.path != key {
        return Err(Error::rejected(format!(
            "checkout path {} differs from recorded path {}; refusing moved or noncanonical ownership",
            path.display(),
            record.path
        )));
    }
    if record.state == "released" && options.allow_already_released {
        if !expected(Some(record))? {
            return Err(Error::rejected(format!(
                "{refused} (lifecycle state released)"
            )));
        }
        return Ok(ReleaseGuard {
            root,
            dir,
            key: None,
            _lock: lock,
        });
    }
    if record.state != "active" {
        return Err(Error::rejected(format!(
            "{refused} (lifecycle state {})",
            record.state
        )));
    }
    if !expected(Some(record))? {
        return Err(Error::rejected(refused));
    }
    let record = &mut ledger.records[index];
    if record.issue.is_none() {
        record.issue = options.bind_issue.map(str::to_owned);
    }
    record.state = "releasing".into();
    record.release_reason = Some(reason.to_string());
    record.updated_at = time::now_epoch();
    write_ledger(&dir, &ledger)?;
    Ok(ReleaseGuard {
        root,
        dir,
        key: Some(key),
        _lock: lock,
    })
}

/// Authorize a development release and hold the ledger lock across deletion.
/// Missing records are allowed only for legacy issue-ref-authorized checkouts;
/// the held lock serializes any concurrent explicit adoption.
pub fn begin_release(
    repo: &Path,
    path: &Path,
    issue: &str,
    branch: Option<&str>,
    reason: &str,
) -> Result<ReleaseGuard> {
    begin_release_transaction(
        repo,
        path,
        reason,
        format!(
            "checkout ownership for {} changed before release of issue {issue}; refusing deletion",
            path.display()
        ),
        ReleaseOptions {
            allow_legacy_absence: true,
            allow_already_released: false,
            bind_issue: None,
        },
        |record| {
            Ok(record.is_none_or(|record| {
                record.purpose == "development"
                    && record.issue.as_deref() == Some(issue)
                    && record.branch.as_deref() == branch
            }))
        },
    )
}

/// Authorize a hosted lane bound by the assignment ledger. Older hosted lanes
/// were recorded by the generic agent-worktree creator without an issue.
pub(crate) fn begin_hosted_release(
    repo: &Path,
    path: &Path,
    issue: &str,
    branch: Option<&str>,
    reason: &str,
    before_release: &dyn Fn() -> Result<()>,
) -> Result<ReleaseGuard> {
    begin_release_transaction(
        repo,
        path,
        reason,
        format!(
            "checkout ownership for {} changed before hosted release of issue {issue}; refusing deletion",
            path.display()
        ),
        ReleaseOptions {
            allow_legacy_absence: true,
            allow_already_released: false,
            bind_issue: Some(issue),
        },
        |record| {
            let allowed = record.is_none_or(|record| {
                record.purpose == "development"
                    && record.branch.as_deref() == branch
                    && (record.issue.as_deref() == Some(issue)
                        || (record.issue.is_none() && record.tool == "cadence agent worktree"))
            });
            if allowed {
                before_release()?;
            }
            Ok(allowed)
        },
    )
}

pub fn begin_refs_only_finish(
    repo: &Path,
    path: &Path,
    issue: &str,
    branch: Option<&str>,
    reason: &str,
) -> Result<ReleaseGuard> {
    let root = canonical_root(repo)?;
    begin_release_transaction(
        &root,
        path,
        reason,
        format!(
            "refs-only finish ownership for {} changed before closing issue {issue}; refusing closure",
            path.display()
        ),
        ReleaseOptions {
            allow_legacy_absence: true,
            allow_already_released: true,
            bind_issue: None,
        },
        |record| {
            validate_refs_only_finish_target(&root, path, branch)?;
            Ok(record.is_none_or(|record| {
                record.purpose == "development"
                    && record.issue.as_deref() == Some(issue)
                    && record.branch.as_deref() == branch
            }))
        },
    )
}

/// Authorize a review-tree release while binding it to the exact tool owner
/// and pinned revision, then hold the ledger lock through removal.
pub fn begin_review_release(
    repo: &Path,
    path: &Path,
    tool: &str,
    owner: &str,
    pinned_sha: &str,
    reason: &str,
) -> Result<ReleaseGuard> {
    begin_release_transaction(
        repo,
        path,
        reason,
        format!(
            "review checkout ownership for {} changed before release; refusing deletion",
            path.display()
        ),
        ReleaseOptions {
            allow_legacy_absence: false,
            allow_already_released: false,
            bind_issue: None,
        },
        |record| {
            Ok(record.is_some_and(|record| {
                record.purpose == "review"
                    && record.tool == tool
                    && record.owner == owner
                    && record.pinned_sha == pinned_sha
            }))
        },
    )
}

pub struct ReclaimGuard {
    _lock: LedgerLock,
}

fn validate_target_reclaim_at_root(
    root: &Path,
    path: &Path,
    issue: &str,
    branch: &str,
) -> Result<()> {
    if !path.is_absolute() {
        return Err(Error::rejected(format!(
            "target reclaim path {} is not absolute",
            path.display()
        )));
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        Error::rejected(format!(
            "cannot inspect target reclaim checkout {}: {error}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::rejected(format!(
            "target reclaim checkout {} is not a real directory",
            path.display()
        )));
    }
    let canonical = path.canonicalize()?;
    if canonical.as_os_str() != path.as_os_str()
        || canonical == root
        || worktree::main_root(&canonical)?.canonicalize()? != root
    {
        return Err(Error::rejected(format!(
            "target reclaim checkout {} is not its canonical linked path in {}",
            path.display(),
            root.display()
        )));
    }
    if finish::git_branch(&canonical)?.as_deref() != Some(branch) {
        return Err(Error::rejected(format!(
            "target reclaim checkout {} is not on issue-owned branch {branch}",
            path.display()
        )));
    }
    worktree::validate_registered_branch(root, &canonical, branch)?;

    let key = canonical.to_string_lossy().into_owned();
    let ledger = read_ledger(root)?;
    let matches: Vec<&Checkout> = ledger
        .records
        .iter()
        .filter(|record| record_path_matches(record, &key))
        .collect();
    if matches.len() > 1 {
        return Err(Error::rejected(format!(
            "multiple lifecycle ownership records name {}; refusing target reclaim",
            path.display()
        )));
    }
    if let Some(record) = matches.first().copied() {
        validate_record(root, record)?;
        if record.path != key {
            return Err(Error::rejected(format!(
                "target reclaim path {} differs from recorded path {}; refusing moved or noncanonical ownership",
                path.display(),
                record.path
            )));
        }
        if record.purpose != "development"
            || record.issue.as_deref() != Some(issue)
            || record.branch.as_deref() != Some(branch)
        {
            return Err(Error::rejected(format!(
                "target reclaim lifecycle ownership for {} does not match development issue {issue}, branch {branch}",
                path.display()
            )));
        }
        if record.state != "active" {
            let reason = record
                .retention_reason
                .as_deref()
                .or(record.release_reason.as_deref())
                .unwrap_or("no recorded lifecycle reason");
            return Err(Error::rejected(format!(
                "target reclaim refused: {} is recorded as {} ({reason})",
                path.display(),
                record.state
            )));
        }
    }
    Ok(())
}

pub fn validate_target_reclaim(repo: &Path, path: &Path, issue: &str, branch: &str) -> Result<()> {
    let root = canonical_root(repo)?;
    validate_target_reclaim_at_root(&root, path, issue, branch)
}

pub fn begin_target_reclaim(
    repo: &Path,
    path: &Path,
    issue: &str,
    branch: &str,
) -> Result<ReclaimGuard> {
    let root = canonical_root(repo)?;
    let (_dir, lock) = LedgerLock::acquire(&root)?;
    validate_target_reclaim_at_root(&root, path, issue, branch)?;
    Ok(ReclaimGuard { _lock: lock })
}
