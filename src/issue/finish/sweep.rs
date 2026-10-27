//! Merged-worktree sweep and path preview for `issue finish`.

use super::*;

/// Where a sweep candidate's recorded worktree and branch actually
/// are. Read-only: a missing directory or a branch checked out at
/// another path is a reconciliation, never a finish.
pub(crate) struct PathPreview {
    /// `present` when the recorded directory exists, else `missing`.
    pub(crate) path_state: &'static str,
    /// `present`, `missing`, or `elsewhere` (the branch ref exists
    /// and `git worktree list` has it at a different path).
    pub(crate) branch_state: &'static str,
    /// Path `git worktree list` reports for this branch, if any.
    pub(crate) live_path: Option<PathBuf>,
    /// Stable skip reason when the sweep must not finish this row.
    pub(crate) reconcile: Option<&'static str>,
}

/// Collapse `.` and `..` without requiring the path to exist, so a
/// missing recorded directory still compares with the path git prints.
pub(crate) fn lexical_path(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    if let (Ok(a), Ok(b)) = (a.canonicalize(), b.canonicalize()) {
        return a == b;
    }
    lexical_path(a) == lexical_path(b)
}

/// The worktree path that currently has `branch` checked out, from
/// `git worktree list --porcelain`. Enumeration errors are returned to
/// destructive callers so they fail closed.
pub(crate) fn registered_branch_path(root: &Path, branch: &str) -> Result<Option<PathBuf>> {
    if branch.is_empty() {
        return Ok(None);
    }
    let text = git(root, &["worktree", "list", "--porcelain"])?;
    let want = format!("branch refs/heads/{branch}");
    let mut current: Option<PathBuf> = None;
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(PathBuf::from(path));
        } else if line == want {
            return Ok(current);
        }
    }
    Ok(None)
}

pub(crate) fn branch_checkout_path(root: &Path, branch: &str) -> Option<PathBuf> {
    registered_branch_path(root, branch).ok().flatten()
}

pub(crate) fn path_preview(t: &Target) -> PathPreview {
    let recorded = t.wt_dir.as_deref();
    let path_present = recorded.is_some_and(|d| d.is_dir());
    let live = branch_checkout_path(&t.root, &t.branch);
    let branch_missing = t.branch.is_empty() || branch_tip(&t.root, &t.branch).is_none();
    let elsewhere = !branch_missing
        && match (recorded, live.as_deref()) {
            (Some(recorded), Some(live)) => !same_path(recorded, live),
            _ => false,
        };
    let branch_state = if branch_missing {
        "missing"
    } else if elsewhere {
        "elsewhere"
    } else {
        "present"
    };
    // A live checkout at another path wins over "the recorded
    // directory is gone": that lane is the retained worktree the
    // missing ref must not be finished out from under.
    let reconcile = if elsewhere {
        Some("reconcile: path-branch-mismatch")
    } else if !path_present && branch_missing {
        Some("reconcile: missing-worktree, branch-missing")
    } else if !path_present {
        Some("reconcile: missing-worktree, branch-present")
    } else {
        None
    };
    PathPreview {
        path_state: if path_present { "present" } else { "missing" },
        branch_state,
        live_path: live,
        reconcile,
    }
}

pub(crate) fn annotate_preview(row: &mut Value, preview: &PathPreview) {
    row["path_state"] = json!(preview.path_state);
    row["branch_state"] = json!(preview.branch_state);
    row["live_path"] = preview
        .live_path
        .as_ref()
        .map(|p| json!(p.display().to_string()))
        .unwrap_or(Value::Null);
}

/// One sweep row for `id`'s open worktree ref `lane` — `None` when
/// the ref no longer resolves to a worktree target. The bool is true
/// when the row is `refused`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn sweep_row(
    pm: &Pm,
    id: &str,
    lane: &Path,
    remote: bool,
    dry_run: bool,
    actor: &str,
    state_dir: &Path,
    view: &DaemonView,
) -> (Option<Value>, bool) {
    let mut row = json!({
        "issue": id,
        "worktree": lane,
        "branch": Value::Null,
        "merged_by": Value::Null,
        "outcome": Value::Null,
        "reason": Value::Null,
        "path_state": Value::Null,
        "branch_state": Value::Null,
        "live_path": Value::Null,
    });
    let t = match resolve(pm, id, Some(lane)) {
        Ok(Resolve::Target(t)) if t.wt_dir.is_some() => t,
        Ok(_) => return (None, false),
        Err(e) => {
            row["outcome"] = json!("refused");
            row["reason"] = json!(e.to_string());
            return (Some(row), true);
        }
    };
    row["branch"] = json!(t.branch);
    // Missing path / branch-at-another-path is not a finish
    // candidate. Classify before the guard so a gone directory
    // cannot read as `would-finish`, and skip `run` so a real
    // sweep cannot close the ref or delete the branch. Explicit
    // `issue finish <ID>` is unchanged.
    let preview = path_preview(&t);
    annotate_preview(&mut row, &preview);
    // Candidates are merged branches — unmerged or unstarted work is
    // skipped, never refused (the point of the verb). The sweep's own
    // evidence never fetches: a real row's `run` fetches for its own
    // gate at finish time.
    let ev = evidence(&t, false);
    if let Some(reason) = preview.reconcile {
        if let Branch::Merged { how, .. } = &ev.state {
            row["merged_by"] = json!(how);
        }
        row["outcome"] = json!("skipped");
        row["reason"] = json!(reason);
        return (Some(row), false);
    }
    let skipped = match &ev.state {
        Branch::Gone if t.branch.is_empty() => Some("no branch ref"),
        Branch::Gone => Some("branch missing"),
        Branch::NotStarted { .. } => Some("not started"),
        Branch::Open { .. } => Some("unmerged"),
        Branch::Merged { .. } => None,
    };
    if let Some(reason) = skipped {
        row["outcome"] = json!("skipped");
        row["reason"] = json!(reason);
        return (Some(row), false);
    }
    if let Branch::Merged { how, .. } = &ev.state {
        row["merged_by"] = json!(how);
    }
    if let Some(path) = t.wt_dir.as_deref() {
        if let Err(e) = verify_checkout_ownership(
            &t.root,
            path,
            id,
            (!t.branch.is_empty()).then_some(t.branch.as_str()),
        ) {
            row["outcome"] = json!("refused");
            row["reason"] = json!(e.to_string());
            return (Some(row), true);
        }
    }
    if dry_run {
        return match inspect(view, state_dir, &t, &ev).blocks.first() {
            Some(b) => {
                row["outcome"] = json!("refused");
                row["reason"] = json!(b.reason);
                (Some(row), true)
            }
            None => {
                row["outcome"] = json!("would-finish");
                (Some(row), false)
            }
        };
    }
    let args = FinishArgs {
        force: false,
        keep_branch: false,
        remote,
        worktree: Some(lane),
        close_if_gone: false,
    };
    match run(pm, id, &args, actor, state_dir, Some(view)) {
        Ok(out) => {
            row["outcome"] = json!("finished");
            row["removed_worktree"] = out["removed_worktree"].clone();
            row["deleted_branch"] = out["deleted_branch"].clone();
            row["branch_note"] = out["branch_note"].clone();
            row["remote_deleted"] = out["remote_deleted"].clone();
            row["remote_note"] = out["remote_note"].clone();
            (Some(row), false)
        }
        Err(e) => {
            row["outcome"] = json!("refused");
            row["reason"] = json!(e.to_string());
            (Some(row), true)
        }
    }
}

/// `issue finish --merged [--project P] [--remote] [--dry-run]` —
/// sweep every open worktree ref in scope whose branch is merged into
/// the repo's default branch and whose per-worktree guard passes.
/// One row per worktree: `finished` (or `would-finish` under
/// `--dry-run`), `skipped(<reason>)` when it is not a merged
/// candidate, `refused(<reason>)` when a guard blocks. A missing
/// recorded directory, or a branch checked out at a different live
/// path, is `skipped` with a reconcile reason on both the dry run
/// and a real sweep — those refs stay open and those branches stay.
/// Every candidate row also carries `path_state` (`present`|
/// `missing`), `branch_state` (`present`|`missing`|`elsewhere`), and
/// `live_path`. The sweep never forces. `refused` counts the refused
/// rows — the CLI maps nonzero to exit 1.
pub fn sweep(
    pm: &Pm,
    project: Option<&str>,
    remote: bool,
    dry_run: bool,
    actor: &str,
    state_dir: &Path,
) -> Result<Value> {
    let issues = board::load_all(&pm.dir, project)?;
    sweep_issues(pm, issues, remote, dry_run, actor, state_dir, project)
}

/// The sweep over an already-loaded issue set — shared by `sweep`
/// (every issue in scope) and `sweep_selected` (named issues only).
pub(crate) fn sweep_issues(
    pm: &Pm,
    issues: Vec<board::Issue>,
    remote: bool,
    dry_run: bool,
    actor: &str,
    state_dir: &Path,
    project: Option<&str>,
) -> Result<Value> {
    // One enumeration for the whole sweep — the task→worktree map
    // answers for every row instead of re-issuing `agent_list` +
    // `task_show`×N per issue (and twice per issue on a real sweep).
    // A binding added mid-sweep still lands in a row's probe as a
    // stale-ref refusal.
    let view = daemon_view(state_dir);
    let mut rows = Vec::new();
    let mut refused = 0usize;
    for issue in issues {
        let id = issue.front.id.clone();
        // One row per open WORKTREE ref — a lone open branch ref is
        // finished by name, not swept; an issue with several lanes
        // gets a row for each, every one finished by its own path.
        for lane in start::open_worktrees(&issue.front) {
            let (row, is_refused) =
                sweep_row(pm, &id, &lane, remote, dry_run, actor, state_dir, &view);
            if let Some(row) = row {
                refused += usize::from(is_refused);
                rows.push(row);
            }
        }
    }
    Ok(json!({
        "rows": rows,
        "refused": refused,
        "dry_run": dry_run,
        "project": project,
    }))
}

/// `sweep` over only the named issues — the cleanup half of a
/// targeted `issue reconcile --issue` (CAD-1377): each row runs the
/// same resolve + guard as the project sweep and nothing outside
/// `ids` is resolved, probed or touched. An unknown id refuses
/// before any probe.
pub(crate) fn sweep_selected(
    pm: &Pm,
    ids: &[&str],
    remote: bool,
    dry_run: bool,
    actor: &str,
    state_dir: &Path,
) -> Result<Value> {
    let mut issues = Vec::with_capacity(ids.len());
    for id in ids {
        issues.push(board::find_issue(&pm.dir, id)?);
    }
    sweep_issues(pm, issues, remote, dry_run, actor, state_dir, None)
}
