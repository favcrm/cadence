//! Worktree target resolution and ownership checks for `issue finish`.

use super::*;

pub(crate) struct Target {
    pub(crate) front: Front,
    pub(crate) dir: PathBuf,
    pub(crate) wt_dir: Option<PathBuf>,
    pub(crate) wt_name: Option<String>,
    pub(crate) branch: String,
    pub(crate) root: PathBuf,
    pub(crate) msg_refs: std::collections::HashSet<String>,
    pub(crate) cargo_target: Option<String>,
}

pub(crate) fn verify_checkout_ownership(
    root: &Path,
    path: &Path,
    issue: &str,
    expected_branch: Option<&str>,
) -> Result<Option<crate::worktree::lifecycle::Checkout>> {
    let record = crate::worktree::lifecycle::managed_record(root, path)?;
    if let Some(record) = &record {
        if record.purpose != "development"
            || record.issue.as_deref() != Some(issue)
            || expected_branch.is_some_and(|branch| record.branch.as_deref() != Some(branch))
            || record.state != "active"
        {
            return Err(Error::rejected(format!(
                "checkout ownership for {} does not authorize issue {issue}, branch {:?}, purpose {}, state {} — refusing deletion",
                path.display(), record.branch, record.purpose, record.state
            )));
        }
    }
    // Issue refs remain the development-lane authority for legacy checkouts;
    // when lifecycle metadata exists it must agree and be active. In all cases
    // resolve has already bound the candidate to a declared repo and layout.
    let branch = expected_branch.or_else(|| record.as_ref().and_then(|r| r.branch.as_deref()));
    if path.is_dir() {
        let actual = checked_out_branch(path)?.ok_or_else(|| {
            Error::rejected(format!(
                "checkout {} has no registered branch; refusing deletion",
                path.display()
            ))
        })?;
        if branch.is_some_and(|branch| branch != actual) {
            return Err(Error::rejected(format!(
                "checkout {} is on branch {actual}, not the issue-owned branch {}; refusing deletion",
                path.display(), branch.unwrap_or_default()
            )));
        }
        crate::worktree::validate_registered_branch(root, path, &actual)?;
    } else if let Some(branch) = branch {
        let live = registered_branch_path(root, branch)?.filter(|live| !same_path(live, path));
        if let Some(live) = live {
            return Err(Error::rejected(format!(
                "issue-owned branch {branch} is checked out at {}, not {}; refusing deletion",
                live.display(),
                path.display()
            )));
        }
    }
    Ok(record)
}

pub(crate) fn verify_refs_only_ownership(
    root: &Path,
    path: &Path,
    issue: &str,
    expected_branch: Option<&str>,
) -> Result<()> {
    let Some(record) = crate::worktree::lifecycle::managed_record(root, path)? else {
        return Ok(());
    };
    if record.purpose != "development"
        || record.issue.as_deref() != Some(issue)
        || expected_branch.is_some_and(|branch| record.branch.as_deref() != Some(branch))
    {
        return Err(Error::rejected(format!(
            "refs-only finish ownership for {} does not match issue {issue}, branch {:?}, purpose {}",
            path.display(), record.branch, record.purpose
        )));
    }
    if !matches!(record.state.as_str(), "active" | "released") {
        let reason = record
            .retention_reason
            .as_deref()
            .or(record.release_reason.as_deref())
            .unwrap_or("no recorded lifecycle reason");
        return Err(Error::rejected(format!(
            "refs-only finish refuses {} recorded as {} ({reason})",
            path.display(),
            record.state
        )));
    }
    Ok(())
}

/// The branch a live worktree has checked out; `None` only when it is absent
/// or detached. Git and filesystem inspection errors remain errors.
pub(crate) fn checked_out_branch(wt: &Path) -> Result<Option<String>> {
    match std::fs::symlink_metadata(wt) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(
            Error::rejected(format!("{} is not a real worktree directory", wt.display())),
        ),
        Ok(_) => git_branch(wt),
    }
}

/// The three ref states an issue can be in for finishing.
pub(crate) enum Resolve {
    /// No worktree/branch refs at all — `finish` errors.
    Nothing,
    /// Refs exist but every worktree/branch ref is closed.
    Finished,
    /// An open worktree and/or branch ref to clean up.
    Target(Box<Target>),
}

/// The recorded paths of `lanes`, one per line, for refusals.
pub(crate) fn lane_list(lanes: &[&Ref]) -> String {
    lanes
        .iter()
        .filter_map(|r| r.path.as_deref())
        .collect::<Vec<_>>()
        .join("\n  ")
}

/// Load the issue and resolve its open worktree/branch refs + repo
/// root — shared by `run` and the `--merged` sweep. `pick` names the
/// worktree ref to finish (`--worktree`, and each sweep row); without
/// it the issue must have at most one open worktree ref — several
/// refuse, listed, rather than guess (CAD-274). A pick matching only
/// a closed ref is already finished. The chosen OPEN worktree ref
/// pairs with the open branch ref of the SAME name — a re-start under
/// `--name` leaves older pairs behind, and first-of-kind matching
/// could fuse halves of different pairs. A lone open branch ref
/// (hand-edited history) is still finishable on its own.
pub(crate) fn resolve(pm: &Pm, id: &str, pick: Option<&Path>) -> Result<Resolve> {
    let (_project, dir) = write::issue_dir(pm, id)?;
    let (front, _body) = write::load_front(&dir)?;
    let open_wts: Vec<&Ref> = front
        .refs
        .iter()
        .filter(|r| r.kind == "worktree" && r.closed != Some(true))
        .collect();
    let open_wt_ref = match pick {
        Some(want) => {
            let same = |r: &Ref| {
                r.kind == "worktree"
                    && r.path
                        .as_deref()
                        .is_some_and(|p| same_path(Path::new(p), want))
            };
            match open_wts.iter().copied().find(|r| same(r)) {
                Some(r) => Some(r),
                None if front.refs.iter().any(same) => return Ok(Resolve::Finished),
                None => {
                    return Err(Error::rejected(format!(
                        "{id} records no open worktree {} — open: {}",
                        want.display(),
                        if open_wts.is_empty() {
                            "(none)".to_string()
                        } else {
                            format!("\n  {}", lane_list(&open_wts))
                        }
                    )))
                }
            }
        }
        None if open_wts.len() > 1 => {
            return Err(Error::rejected(format!(
                "{id} has {} open worktree refs — refusing to guess which \
                 to finish; pass --worktree <path>:\n  {}",
                open_wts.len(),
                lane_list(&open_wts)
            )))
        }
        None => open_wts.first().copied(),
    };
    let open_wt = open_wt_ref.and_then(|r| r.path.clone()).map(PathBuf::from);
    let cargo_target = open_wt_ref.and_then(|r| r.cargo_target.clone());
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
    // `issue start` names the dir and branch alike, so `cadence/<dir>`
    // pairs them; a worktree whose dir and branch differ (hand-recorded
    // or adopted) pairs by the branch it actually has checked out, when
    // that value is recorded as an open branch ref (CAD-166).
    let branch = match &wt_name {
        Some(n) => {
            if let Some(branch) = open_branch(Some(&layout::branch(n))) {
                branch
            } else {
                let head = match open_wt.as_deref() {
                    Some(path) => checked_out_branch(path)?,
                    None => None,
                };
                head.as_deref()
                    .and_then(|head| open_branch(Some(head)))
                    .unwrap_or_default()
            }
        }
        None => open_branch(None).unwrap_or_default(),
    };
    // Tracker values reach git below — refuse one it would read as an
    // option, however it got into the file (CAD-144).
    model::check_ref_value(&branch)?;
    if let Some(d) = &open_wt {
        model::check_ref_value(&d.to_string_lossy())?;
    }
    if open_wt.is_none() && branch.is_empty() {
        if front
            .refs
            .iter()
            .any(|r| matches!(r.kind.as_str(), "worktree" | "branch"))
        {
            return Ok(Resolve::Finished);
        }
        return Ok(Resolve::Nothing);
    }
    // The repo root: through the live worktree when it exists, else
    // the chosen `<root>/.cadence/wt/<name>` path walked upward, else
    // the first recorded one (closed refs still name the repo).
    let wt_dir = open_wt;
    let walk = layout::assumed_root;
    let root = wt_dir
        .as_deref()
        .filter(|d| d.is_dir())
        .and_then(|d| project::repo_identity(d).map(|(r, _)| r))
        .or_else(|| wt_dir.as_deref().and_then(walk))
        .or_else(|| {
            front
                .refs
                .iter()
                .find(|r| r.kind == "worktree")
                .and_then(|r| r.path.as_deref())
                .and_then(|d| walk(Path::new(d)))
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
    let declared = start::declared_repos(&_project);
    if !declared.contains(&root) {
        return Err(Error::rejected(format!(
            "Worktree {} has foreign ownership: repo {} is undeclared by project {} (declares: {}) — refusing deletion",
            wt_dir
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            root.display(),
            _project.key,
            if declared.is_empty() {
                "no local repo paths".to_string()
            } else {
                declared
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )));
    }
    if let Some(path) = &wt_dir {
        if same_path(path, &root) {
            return Err(Error::rejected(format!(
                "Worktree {} is not a linked worktree (the main checkout is never reclaimed)",
                path.display()
            )));
        }
        if layout::root_of(path).as_deref() != Some(root.as_path()) {
            return Err(Error::rejected(format!(
                "Worktree {} is outside the managed layout for repo {} — inventory and explicitly adopt unknown checkouts; issue finish will not delete them",
                path.display(), root.display()
            )));
        }
        if path.is_dir() {
            let meta = std::fs::symlink_metadata(path)?;
            if !meta.is_dir()
                || meta.file_type().is_symlink()
                || path.canonicalize()? != lexical_path(path)
            {
                return Err(Error::rejected(format!(
                    "Worktree {} is symlinked or differs from its canonical managed path — refusing cleanup",
                    path.display()
                )));
            }
        }
    }
    // Message refs bind the worktree they were dispatched against: a
    // ref carrying a `worktree` field scopes to that pair only, so a
    // re-start under `--name` doesn't inherit the earlier kickoff's
    // binding. Unscoped refs (hand-added, pre-CAD-94) bind any target.
    // A closed ref — a dispatch whose send failed — bound nothing and
    // counts for no check.
    let msg_refs = bound_msg_refs(&front, wt_name.as_deref(), wt_dir.as_deref());
    Ok(Resolve::Target(Box::new(Target {
        front,
        dir,
        wt_dir,
        wt_name,
        branch,
        root,
        msg_refs,
        cargo_target,
    })))
}
