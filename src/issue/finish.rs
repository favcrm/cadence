//! `cadence issue finish <ID>` — remove an issue's recorded worktree
//! and branch once the work has landed. The guard is per worktree,
//! not per agent: it refuses only while the worktree is actually in
//! use — a live message whose dispatch recorded this worktree (the
//! issue's `message` refs, or a job task's recorded worktree), an
//! unreconciled `unknown` message bound the same way or held by a
//! registered agent whose cwd is on the worktree, a pane
//! process tree with cwd inside it, any process standing in it, or
//! any non-ignored file modified within `ACTIVE_WINDOW` (CAD-275) —
//! plus a dirty worktree (ignored paths don't count), a branch that
//! has not started (no commits beyond where it was cut — never
//! "merged", CAD-275) and a branch whose work survives nowhere —
//! merged into the repo's default branch by ancestry,
//! patch-equivalent commits, a squash merge, or a merged PR, or
//! pushed. A busy owner working ELSEWHERE is not a reason. `--force`
//! overrides each and is recorded as a `Forced:` trailer on the
//! finish commit. Refs are kept as history, marked `closed: true`
//! (a branch finish leaves standing keeps its ref open, CAD-145);
//! the issue's status is untouched — status follows the job or the
//! PM. An issue with several open worktree refs needs `--worktree
//! <path>` to name one (CAD-274); a named directory already gone only
//! closes its refs. `issue finish --merged` sweeps every open
//! worktree ref whose branch is merged and whose guard passes; an
//! unstarted branch is `skipped(not started)`. A recorded directory
//! that is already missing, or a branch checked out at a different
//! live path, is skipped for reconciliation instead of finished — the
//! sweep does not close those refs or delete those branches.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

#[cfg(target_os = "linux")]
use std::ffi::CString;
#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::io::{self, Read};
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "linux")]
use std::os::unix::fs::{FileTypeExt, MetadataExt};

use serde_json::{json, Value};
use tempfile::TempDir;

use crate::adapter::pty;
use crate::client;
use crate::error::{Error, Result};
use crate::issue::model::{self, Front, Ref};
use crate::issue::{board, project, start, write, Pm};
use crate::proc::{run_bounded, BoundedError};
use crate::worktree::layout;

/// Every git/gh probe in this file runs through the bounded runner —
/// user repos can be slow or locked and finish must not stall.
const GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// `git -C dir <args>`; non-zero exit is a rejected error carrying
/// stderr, like `issue::git`. `pub(crate)`: `issue reconcile` runs
/// the same bounded probes for its merge classification.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String> {
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

/// Resolve HEAD's branch; symbolic-ref exit 1 is Git's detached-HEAD result.
pub(crate) fn git_branch(dir: &Path) -> Result<Option<String>> {
    let args = ["symbolic-ref", "--quiet", "--short", "HEAD"];
    let out = git_out(dir, &args, &[])?;
    match out.status.code() {
        Some(0) => {
            let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if branch.is_empty() {
                Err(Error::rejected(format!(
                    "git {} returned an empty branch in {}",
                    args.join(" "),
                    dir.display()
                )))
            } else {
                Ok(Some(branch))
            }
        }
        Some(1) => Ok(None),
        code => Err(Error::rejected(format!(
            "git {} failed in {} with status {:?}: {}",
            args.join(" "),
            dir.display(),
            code,
            String::from_utf8_lossy(&out.stderr).trim()
        ))),
    }
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
pub(crate) fn default_ref(root: &Path) -> Option<String> {
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

/// Why `tip` counts as merged into `into`, checked in order: plain
/// ancestry; every commit up to `tip` already applied upstream
/// (`git cherry` — the rebase/cherry-pick case); the combined diff
/// up to `tip` reverse-applying onto `into` (a squash merge); or a
/// merged GitHub PR whose recorded head covers `tip`. All four are
/// pinned to the SHA, never re-resolved by name — the evidence and
/// the commit a `branch -D` deletes cannot disagree. `pr_head` is
/// the head branch name the `pr` arm filters on (a remote-tracking
/// tip still names the local head it was pushed from). The first
/// match wins and is reported as `merged_by`.
pub(crate) fn merge_rule(
    root: &Path,
    pr_head: &str,
    tip: &str,
    into: &str,
) -> Option<&'static str> {
    if ancestor(root, tip, into) {
        return Some("ancestry");
    }
    // `+` = a commit with no patch-equivalent upstream; a listing
    // with none means everything up to `tip` already landed.
    if let Ok(marks) = git(root, &["cherry", into, tip]) {
        if !marks.lines().any(|l| l.starts_with('+')) {
            return Some("cherry");
        }
    }
    if patch_applied(root, tip, into) {
        return Some("patch");
    }
    if pr_merged(root, pr_head, tip, into) {
        return Some("pr");
    }
    None
}

/// Squash-merge test: if the branch's combined diff against its
/// merge-base reverse-applies cleanly onto `into`'s tree, `into`
/// already holds that state. Runs against a temporary index — no
/// worktree is ever touched.
fn patch_applied(root: &Path, tip: &str, into: &str) -> bool {
    let Ok(base) = git(root, &["merge-base", into, tip]) else {
        return false;
    };
    // Raw stdout bytes, not `git()`'s trimmed string — `git apply`
    // rejects a patch whose final newline was stripped as corrupt.
    let diff = match git_out(root, &["diff", "--binary", &base, tip], &[]) {
        Ok(o) if o.status.success() => o.stdout,
        _ => return false,
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

/// A merged GitHub PR counts as merged only when its recorded head
/// commit covers `tip` — the name alone is not evidence: a branch
/// name reused after its PR merged would otherwise report new
/// unmerged commits as merged and the sweep would `branch -D` them
/// away. The head SHA must equal the tip, or the tip must be an
/// ancestor of it (a local branch behind the merged head). When the
/// PR's head commit is not a local object only equality can prove
/// coverage — anything else falls through to the other evidence.
/// The PR's `baseRefName` must be the default ref — a PR merged into
/// a stacked base is not merged into the default branch. `gh` needs
/// a GitHub origin and a successful answer; any failure falls
/// through: gh is a hint, never the only authority.
fn pr_merged(root: &Path, branch: &str, tip: &str, into: &str) -> bool {
    let Ok(url) = git(root, &["remote", "get-url", "origin"]) else {
        return false;
    };
    if !url.contains("github.com") {
        return false;
    }
    // `into` may be a remote-tracking name ("origin/main") — the PR
    // base is a bare branch name.
    let base_name = into.strip_prefix("origin/").unwrap_or(into);
    let mut cmd = Command::new("gh");
    cmd.args([
        "pr",
        "list",
        "--head",
        branch,
        "--state",
        "merged",
        "--json",
        "number,headRefOid,baseRefName",
        "--limit",
        "10",
    ])
    .current_dir(root);
    let Ok(out) = run_bounded(&mut cmd, Duration::from_secs(10)) else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let Ok(prs) = serde_json::from_slice::<Value>(&out.stdout) else {
        return false;
    };
    prs.as_array().is_some_and(|prs| {
        prs.iter()
            .filter(|pr| pr["baseRefName"].as_str() == Some(base_name))
            .filter_map(|pr| pr["headRefOid"].as_str())
            .any(|oid| oid == tip || object_has(root, oid) && ancestor(root, tip, oid))
    })
}

/// Is `oid` present in the local object store? Ancestry against a
/// commit we do not have cannot be checked.
fn object_has(root: &Path, oid: &str) -> bool {
    git_out(root, &["cat-file", "-e", oid], &[])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

thread_local! {
    /// A per-thread bound on the daemon probes below. Unset keeps the
    /// CLI's long default; the daemon's own checkup sets it so a wedged
    /// self-RPC cannot stall the stall-watch thread (CAD-1021).
    static PROBE_TIMEOUT: std::cell::Cell<Option<std::time::Duration>> =
        const { std::cell::Cell::new(None) };
}

/// Run `f` with every finish daemon probe on this thread bounded by `d`.
pub(crate) fn with_probe_timeout<T>(d: std::time::Duration, f: impl FnOnce() -> T) -> T {
    let prev = PROBE_TIMEOUT.with(|t| t.replace(Some(d)));
    let out = f();
    PROBE_TIMEOUT.with(|t| t.set(prev));
    out
}

fn probe_rpc(state_dir: &Path, method: &str, params: Value) -> Result<Value> {
    match PROBE_TIMEOUT.with(|t| t.get()) {
        Some(d) => client::rpc_timeout(state_dir, method, params, d),
        None => client::rpc(state_dir, method, params),
    }
}

/// A resolved finish target: the issue's open worktree/branch refs
/// plus the repo root the git probes run against. `msg_refs` are the
/// issue's `message` ref targets — the dispatches recorded against
/// this worktree.
struct Target {
    front: Front,
    body: String,
    dir: PathBuf,
    wt_dir: Option<PathBuf>,
    wt_name: Option<String>,
    branch: String,
    root: PathBuf,
    msg_refs: std::collections::HashSet<String>,
    cargo_target: Option<String>,
}

fn verify_checkout_ownership(
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

fn verify_refs_only_ownership(
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
fn checked_out_branch(wt: &Path) -> Result<Option<String>> {
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
enum Resolve {
    /// No worktree/branch refs at all — `finish` errors.
    Nothing,
    /// Refs exist but every worktree/branch ref is closed.
    Finished,
    /// An open worktree and/or branch ref to clean up.
    Target(Box<Target>),
}

/// The recorded paths of `lanes`, one per line, for refusals.
fn lane_list(lanes: &[&Ref]) -> String {
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
fn resolve(pm: &Pm, id: &str, pick: Option<&Path>) -> Result<Resolve> {
    let (_project, dir) = write::issue_dir(pm, id)?;
    let (front, body) = write::load_front(&dir)?;
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
        body,
        dir,
        wt_dir,
        wt_name,
        branch,
        root,
        msg_refs,
        cargo_target,
    })))
}

/// The `message` ref targets bound to this worktree: a ref carrying a
/// `worktree` field scopes to that pair only; unscoped refs bind any
/// target; a closed ref (a failed send) binds nothing.
fn bound_msg_refs(
    front: &Front,
    wt_name: Option<&str>,
    wt_dir: Option<&Path>,
) -> std::collections::HashSet<String> {
    front
        .refs
        .iter()
        .filter(|r| r.kind == "message" && r.closed != Some(true))
        .filter(|r| match r.worktree.as_deref() {
            Some(w) => wt_name == Some(w) || wt_dir.is_some_and(|d| d.to_string_lossy() == w),
            None => true,
        })
        .filter_map(|r| r.path.clone())
        .collect()
}

/// One condition blocking a finish. `reason` names the pid or
/// message id so the operator knows what to wait for; `tag` is the
/// short `overrode` entry `--force` records.
struct Block {
    tag: String,
    reason: String,
}

/// Everything the read-only probes learned about a target — shared by
/// `run` (which refuses on the first block) and the `--merged` sweep
/// (which reports them as rows). Survivability is re-derived by the
/// caller via `survivability`/`branch_state`, so it is not carried
/// here.
struct Check {
    blocks: Vec<Block>,
}

#[cfg(test)]
thread_local! {
    static PROCESS_PROC_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct ProcessProcRootGuard(Option<PathBuf>);

#[cfg(test)]
impl Drop for ProcessProcRootGuard {
    fn drop(&mut self) {
        PROCESS_PROC_ROOT.with(|root| *root.borrow_mut() = self.0.take());
    }
}

#[cfg(test)]
pub(crate) fn use_process_proc_root_for_test(root: PathBuf) -> ProcessProcRootGuard {
    let previous = PROCESS_PROC_ROOT.with(|current| current.borrow_mut().replace(root));
    ProcessProcRootGuard(previous)
}

fn process_proc_root() -> PathBuf {
    #[cfg(test)]
    if let Some(root) = PROCESS_PROC_ROOT.with(|root| root.borrow().clone()) {
        return root;
    }
    // Release builds reject `test-seam`; integration checks can isolate proc
    // enumeration without weakening the production `/proc` path.
    #[cfg(feature = "test-seam")]
    if let Some(root) = std::env::var_os("CADENCE_TEST_PROC_ROOT") {
        return PathBuf::from(root);
    }
    PathBuf::from("/proc")
}

#[cfg(target_os = "linux")]
fn decode_mountinfo_path(value: &str) -> Option<PathBuf> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            decoded.push(bytes[i]);
            i += 1;
            continue;
        }
        let digits = bytes.get(i + 1..i + 4)?;
        let escaped = match digits {
            b"040" => b' ',
            b"011" => b'\t',
            b"012" => b'\n',
            b"134" => b'\\',
            _ => return None,
        };
        decoded.push(escaped);
        i += 4;
    }
    if decoded.contains(&0) {
        return None;
    }
    String::from_utf8(decoded).ok().map(PathBuf::from)
}

#[cfg(target_os = "linux")]
fn validate_proc_mount_options(options: &[&str]) -> std::result::Result<(), String> {
    let mut hidepid_seen = false;
    for option in options.iter().flat_map(|options| options.split(',')) {
        if option.is_empty() {
            return Err("mountinfo contains an empty mount option".into());
        }
        if option == "hidepid" {
            return Err("proc mount has an unparseable hidepid option".into());
        }
        if let Some(value) = option.strip_prefix("hidepid=") {
            if hidepid_seen {
                return Err("proc mount has ambiguous duplicate hidepid options".into());
            }
            hidepid_seen = true;
            if value != "0" {
                return Err(format!(
                    "proc mount restricts process visibility with hidepid={value}"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
struct ProcView {
    root: File,
    canonical: PathBuf,
    scan_root: PathBuf,
    synthetic: bool,
}

#[cfg(target_os = "linux")]
fn proc_view_error(path: &Path, reason: impl std::fmt::Display) -> Error {
    Error::rejected(format!(
        "cannot prove complete procfs visibility for {}: {reason}",
        path.display()
    ))
}

#[cfg(target_os = "linux")]
impl ProcView {
    fn open(proc_root: &Path) -> Result<Self> {
        let canonical = proc_root.canonicalize().map_err(|error| {
            proc_view_error(proc_root, format!("cannot resolve proc root: {error}"))
        })?;
        let c_path = CString::new(canonical.as_os_str().as_bytes())
            .map_err(|_| proc_view_error(&canonical, "proc root contains NUL"))?;
        let fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(proc_view_error(
                &canonical,
                format!("cannot open proc root: {}", io::Error::last_os_error()),
            ));
        }
        let root = unsafe { File::from_raw_fd(fd) };
        let root_meta = root.metadata().map_err(|error| {
            proc_view_error(&canonical, format!("cannot inspect opened root: {error}"))
        })?;
        if !root_meta.is_dir() {
            return Err(proc_view_error(
                &canonical,
                "opened root is not a directory",
            ));
        }
        let scan_root = PathBuf::from(format!("/proc/self/fd/{}", root.as_raw_fd()));
        let anchor_meta = std::fs::metadata(&scan_root).map_err(|error| {
            proc_view_error(
                &canonical,
                format!(
                    "cannot resolve held root through {}: {error}",
                    scan_root.display()
                ),
            )
        })?;
        if !anchor_meta.is_dir()
            || anchor_meta.dev() != root_meta.dev()
            || anchor_meta.ino() != root_meta.ino()
        {
            return Err(proc_view_error(
                &canonical,
                "anchored scan path does not identify the opened root directory",
            ));
        }
        let root_is_nonprocfs =
            fstatfs_type(&root).is_ok_and(|kind| kind != libc::PROC_SUPER_MAGIC);
        let synthetic = root_is_nonprocfs
            && canonical != Path::new("/proc")
            && (cfg!(test) || cfg!(feature = "test-seam"));
        Ok(Self {
            root,
            canonical,
            scan_root,
            synthetic,
        })
    }

    fn open_at(&self, relative: &str, flags: libc::c_int) -> io::Result<File> {
        let relative = CString::new(relative)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "proc path contains NUL"))?;
        let fd = unsafe { libc::openat(self.root.as_raw_fd(), relative.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn verify_proof_fd(
        &self,
        file: &File,
        root_dev: u64,
        root_mount_id: u64,
        label: &str,
        require_regular: bool,
    ) -> Result<()> {
        let metadata = file.metadata().map_err(|error| {
            proc_view_error(&self.canonical, format!("cannot inspect {label}: {error}"))
        })?;
        if require_regular && !metadata.is_file() {
            return Err(proc_view_error(
                &self.canonical,
                format!("{label} is not a regular file"),
            ));
        }
        if !require_regular && !metadata.file_type().is_symlink() {
            return Err(proc_view_error(
                &self.canonical,
                format!("{label} is not a namespace symlink"),
            ));
        }
        if metadata.dev() != root_dev {
            return Err(proc_view_error(
                &self.canonical,
                format!("{label} is on a different filesystem from the held proc root"),
            ));
        }
        let fs_type = fstatfs_type(file).map_err(|error| {
            proc_view_error(
                &self.canonical,
                format!("cannot inspect {label} filesystem: {error}"),
            )
        })?;
        if !self.synthetic && fs_type != libc::PROC_SUPER_MAGIC {
            return Err(proc_view_error(
                &self.canonical,
                format!("{label} is not on procfs"),
            ));
        }
        let mount_id = statx_mount_id(file).map_err(|error| {
            proc_view_error(
                &self.canonical,
                format!("cannot prove {label} mount identity: {error}"),
            )
        })?;
        if mount_id != root_mount_id {
            return Err(proc_view_error(
                &self.canonical,
                format!("{label} is not on the held proc-root mount"),
            ));
        }
        Ok(())
    }

    fn open_proof_file(&self, relative: &str, root_dev: u64, root_mount_id: u64) -> Result<File> {
        let file = self
            .open_at(
                relative,
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
            .map_err(|error| {
                proc_view_error(
                    &self.canonical,
                    format!("cannot open {relative} relative to held root: {error}"),
                )
            })?;
        self.verify_proof_fd(&file, root_dev, root_mount_id, relative, true)?;
        Ok(file)
    }

    fn read_namespace_link(
        &self,
        relative: &str,
        root_dev: u64,
        root_mount_id: u64,
    ) -> Result<u64> {
        let file = self
            .open_at(relative, libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .map_err(|error| {
                proc_view_error(
                    &self.canonical,
                    format!("cannot open {relative} relative to held root: {error}"),
                )
            })?;
        self.verify_proof_fd(&file, root_dev, root_mount_id, relative, false)?;
        let empty = b"\0";
        let mut bytes = [0u8; 64];
        let count = unsafe {
            libc::readlinkat(
                file.as_raw_fd(),
                empty.as_ptr().cast(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if count < 0 {
            return Err(proc_view_error(
                &self.canonical,
                format!(
                    "cannot read {relative} through held link: {}",
                    io::Error::last_os_error()
                ),
            ));
        }
        let count = count as usize;
        if count == bytes.len() {
            return Err(proc_view_error(
                &self.canonical,
                format!("{relative} namespace link was truncated"),
            ));
        }
        let target = std::str::from_utf8(&bytes[..count]).map_err(|error| {
            proc_view_error(
                &self.canonical,
                format!("{relative} has a non-UTF-8 target: {error}"),
            )
        })?;
        parse_pid_namespace_link(target).ok_or_else(|| {
            proc_view_error(
                &self.canonical,
                format!("{relative} is not a canonical pid namespace link"),
            )
        })
    }

    fn read_pid_status(&self, pid: u32) -> io::Result<String> {
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if self.synthetic { 0 } else { libc::O_NONBLOCK };
        let file = self.open_at(&format!("{pid}/status"), flags)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() && !(self.synthetic && metadata.file_type().is_fifo()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process status is not a regular file",
            ));
        }
        let mut status = String::new();
        let mut file = file;
        file.read_to_string(&mut status)?;
        Ok(status)
    }

    fn validate_completeness(&self) -> Result<()> {
        let root_meta = self.root.metadata().map_err(|error| {
            proc_view_error(
                &self.canonical,
                format!("cannot inspect held root: {error}"),
            )
        })?;
        if !root_meta.is_dir() {
            return Err(proc_view_error(
                &self.canonical,
                "held root is not a directory",
            ));
        }
        let root_fs_type = fstatfs_type(&self.root).map_err(|error| {
            proc_view_error(
                &self.canonical,
                format!("cannot inspect held root filesystem: {error}"),
            )
        })?;
        if (!self.synthetic && root_fs_type != libc::PROC_SUPER_MAGIC)
            || (self.synthetic && root_fs_type == libc::PROC_SUPER_MAGIC)
        {
            return Err(proc_view_error(
                &self.canonical,
                "held root filesystem does not match its proc-view adapter",
            ));
        }
        let root_mount_id = statx_mount_id(&self.root).map_err(|error| {
            proc_view_error(
                &self.canonical,
                format!("cannot prove held root mount identity: {error}"),
            )
        })?;

        let mut mountinfo =
            self.open_proof_file("self/mountinfo", root_meta.dev(), root_mount_id)?;
        let mut mountinfo_text = String::new();
        mountinfo
            .read_to_string(&mut mountinfo_text)
            .map_err(|error| {
                proc_view_error(
                    &self.canonical,
                    format!("cannot read held self/mountinfo: {error}"),
                )
            })?;
        validate_proc_mountinfo(
            &mountinfo_text,
            &self.canonical,
            root_mount_id,
            self.synthetic,
        )?;

        let release = if self.synthetic {
            let mut file =
                self.open_proof_file("self/kernel-release", root_meta.dev(), root_mount_id)?;
            let mut release = String::new();
            file.read_to_string(&mut release).map_err(|error| {
                proc_view_error(
                    &self.canonical,
                    format!("cannot read synthetic kernel release: {error}"),
                )
            })?;
            release
        } else {
            kernel_release().map_err(|error| {
                proc_view_error(
                    &self.canonical,
                    format!("cannot read kernel release: {error}"),
                )
            })?
        };
        if !supported_kernel_abi(&release) {
            return Err(proc_view_error(
                &self.canonical,
                format!("kernel release {release:?} has no frozen proc proof ABI"),
            ));
        }

        let mut stat = self.open_proof_file("2/stat", root_meta.dev(), root_mount_id)?;
        let mut stat_text = String::new();
        stat.read_to_string(&mut stat_text).map_err(|error| {
            proc_view_error(
                &self.canonical,
                format!("cannot read held PID 2 stat: {error}"),
            )
        })?;
        if !is_initial_kernel_task(&stat_text) {
            return Err(proc_view_error(
                &self.canonical,
                "PID 2 is not a live kthreadd kernel thread in the initial PID namespace",
            ));
        }
        let self_pid_ns =
            self.read_namespace_link("self/ns/pid", root_meta.dev(), root_mount_id)?;
        let task_pid_ns = self.read_namespace_link("2/ns/pid", root_meta.dev(), root_mount_id)?;
        if self_pid_ns != task_pid_ns {
            return Err(proc_view_error(
                &self.canonical,
                "caller and PID 2 are not in the same PID namespace",
            ));
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn fstatfs_type(file: &File) -> io::Result<libc::c_long> {
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(file.as_raw_fd(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stat.f_type as libc::c_long)
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn statx_mount_id(file: &File) -> io::Result<u64> {
    let empty = b"\0";
    let mut stat: libc::statx = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::statx(
            file.as_raw_fd(),
            empty.as_ptr().cast(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_MNT_ID,
            &mut stat,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if stat.stx_mask & libc::STATX_MNT_ID == 0 || stat.stx_mnt_id == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "statx did not report STATX_MNT_ID",
        ));
    }
    Ok(stat.stx_mnt_id)
}

#[cfg(all(target_os = "linux", not(target_env = "gnu")))]
fn statx_mount_id(_file: &File) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this libc target has no verified statx mount-id ABI",
    ))
}

#[cfg(target_os = "linux")]
fn parse_pid_namespace_link(target: &str) -> Option<u64> {
    let value = target.strip_prefix("pid:[")?.strip_suffix(']')?;
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let inode = value.parse::<u64>().ok()?;
    (inode != 0 && inode.to_string() == value).then_some(inode)
}

#[cfg(target_os = "linux")]
fn kernel_release() -> io::Result<String> {
    let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut uts) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let bytes: Vec<u8> = uts
        .release
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect();
    if bytes.len() == uts.release.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "uname release is not NUL-terminated",
        ));
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(target_os = "linux")]
fn supported_kernel_abi(release: &str) -> bool {
    const SUPPORTED: &[(u32, u32)] = &[
        (5, 8),
        (5, 10),
        (5, 15),
        (6, 1),
        (6, 6),
        (6, 8),
        (6, 11),
        (6, 12),
        (6, 14),
        (6, 17),
        (7, 0),
    ];
    let Some((major, rest)) = release.split_once('.') else {
        return false;
    };
    if major.is_empty() || !major.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let Some(minor) = rest.split(['.', '-']).next() else {
        return false;
    };
    if minor.is_empty() || !minor.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let (Ok(major), Ok(minor)) = (major.parse::<u32>(), minor.parse::<u32>()) else {
        return false;
    };
    SUPPORTED.contains(&(major, minor))
}

#[cfg(target_os = "linux")]
fn is_initial_kernel_task(stat: &str) -> bool {
    const PF_KTHREAD: u64 = 0x0020_0000;
    let Some((pid, record)) = stat.split_once(' ') else {
        return false;
    };
    if pid.parse::<u32>().ok() != Some(2) {
        return false;
    }
    let Some(record) = record.strip_prefix('(') else {
        return false;
    };
    let Some(close) = record.rfind(')') else {
        return false;
    };
    if &record[..close] != "kthreadd" {
        return false;
    }
    let fields: Vec<&str> = record[close + 1..].split_whitespace().collect();
    if fields.len() < 7 || fields[0].len() != 1 {
        return false;
    }
    let Some(state) = fields[0].chars().next() else {
        return false;
    };
    let Some(parent) = fields[1].parse::<u32>().ok() else {
        return false;
    };
    let Some(flags) = fields[6].parse::<u64>().ok() else {
        return false;
    };
    valid_proc_state(state)
        && !matches!(state, 'Z' | 'X' | 'x')
        && parent == 0
        && flags & PF_KTHREAD != 0
}

#[cfg(target_os = "linux")]
fn sensitive_proc_submount(root: &Path, mount_point: &Path) -> bool {
    let Ok(relative) = mount_point.strip_prefix(root) else {
        return false;
    };
    let Some(std::path::Component::Normal(first)) = relative.components().next() else {
        return false;
    };
    let first = first.as_bytes();
    first == b"self"
        || first == b"thread-self"
        || (!first.is_empty() && first.iter().all(u8::is_ascii_digit))
}

#[cfg(target_os = "linux")]
fn validate_proc_mountinfo(
    text: &str,
    canonical: &Path,
    root_mount_id: u64,
    synthetic: bool,
) -> Result<()> {
    let mut exact_mounts = 0usize;
    let mut mount_ids = std::collections::HashSet::new();
    for (line_index, line) in text.lines().enumerate() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let separators: Vec<usize> = fields
            .iter()
            .enumerate()
            .filter_map(|(index, field)| (*field == "-").then_some(index))
            .collect();
        let Some(mount_id) = fields.first().and_then(|field| field.parse::<u64>().ok()) else {
            return Err(proc_view_error(
                canonical,
                format!("malformed mountinfo line {}", line_index + 1),
            ));
        };
        if line.is_empty()
            || fields.len() < 10
            || mount_id == 0
            || fields[1].parse::<u64>().is_err()
            || separators.len() != 1
        {
            return Err(proc_view_error(
                canonical,
                format!("malformed mountinfo line {}", line_index + 1),
            ));
        }
        let separator = separators[0];
        let device = fields[2].split_once(':');
        if separator < 6
            || fields.len() < separator + 4
            || !mount_ids.insert(mount_id)
            || device.is_none_or(|(major, minor)| {
                major.parse::<u64>().is_err() || minor.parse::<u64>().is_err()
            })
        {
            return Err(proc_view_error(
                canonical,
                format!("malformed mountinfo line {}", line_index + 1),
            ));
        }
        let mount_root = decode_mountinfo_path(fields[3]);
        let mount_point = decode_mountinfo_path(fields[4]);
        let (Some(mount_root), Some(mount_point)) = (mount_root, mount_point) else {
            return Err(proc_view_error(
                canonical,
                format!(
                    "malformed escaped path on mountinfo line {}",
                    line_index + 1
                ),
            ));
        };
        if !mount_point.is_absolute()
            || (fields[separator + 1] != "nsfs" && !mount_root.is_absolute())
        {
            return Err(proc_view_error(
                canonical,
                format!("non-absolute mount path on line {}", line_index + 1),
            ));
        }
        if fields[5].split(',').any(str::is_empty)
            || fields[separator + 3].split(',').any(str::is_empty)
        {
            return Err(proc_view_error(
                canonical,
                format!("malformed mount options on line {}", line_index + 1),
            ));
        }
        if sensitive_proc_submount(canonical, &mount_point) {
            return Err(proc_view_error(
                canonical,
                format!(
                    "sensitive proc subtree is overmounted at {}",
                    mount_point.display()
                ),
            ));
        }
        if mount_point == canonical {
            exact_mounts += 1;
            if exact_mounts != 1 {
                return Err(proc_view_error(canonical, "ambiguous stacked proc mounts"));
            }
            let expected_mount_id = if synthetic { 1 } else { root_mount_id };
            if mount_id != expected_mount_id {
                return Err(proc_view_error(
                    canonical,
                    "mountinfo root ID does not match the held root mount",
                ));
            }
            if mount_root != Path::new("/") || fields[separator + 1] != "proc" {
                return Err(proc_view_error(
                    canonical,
                    "mount is not a full procfs root",
                ));
            }
            validate_proc_mount_options(&[fields[5], fields[separator + 3]])
                .map_err(|reason| proc_view_error(canonical, reason))?;
        }
    }
    if exact_mounts != 1 {
        return Err(proc_view_error(
            canonical,
            "mountinfo has no unique full procfs root",
        ));
    }
    Ok(())
}

/// Cwd and open-descriptor holders under a managed checkout. Any process that
/// cannot be fully inspected leaves the scan incomplete; cleanup never infers
/// that a process lacks an inherited or transferred checkout descriptor from
/// its UID or path permissions.
pub(crate) struct ProcessUse {
    pub cwd: Vec<u32>,
    pub fd: Vec<u32>,
    pub enumeration_error: Option<String>,
}

pub(crate) type ProcessUseProbe = dyn Fn(&Path) -> Result<ProcessUse>;

#[cfg(target_os = "linux")]
fn proc_id_values(status: &str, key: &str, count: usize) -> Option<Vec<u32>> {
    let line = status.lines().find_map(|line| line.strip_prefix(key))?;
    line.split_whitespace()
        .take(count)
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .ok()
        .filter(|values| values.len() == count)
}

#[cfg(target_os = "linux")]
fn valid_proc_state(state: char) -> bool {
    matches!(
        state,
        'R' | 'S' | 'D' | 'T' | 't' | 'Z' | 'X' | 'x' | 'K' | 'W' | 'P' | 'I'
    )
}

#[cfg(target_os = "linux")]
fn proc_status_state(status: &str) -> Option<char> {
    let mut states = status
        .lines()
        .filter_map(|line| line.strip_prefix("State:"));
    let state = states.next()?.split_whitespace().next()?;
    if states.next().is_some() || state.len() != 1 {
        return None;
    }
    let state = state.chars().next()?;
    valid_proc_state(state).then_some(state)
}

// Validate the PID entry only; credentials are never used to skip cwd or FD
// inspection or to infer that a process could not hold a checkout descriptor.
#[cfg(target_os = "linux")]
fn proc_status_is_complete(status: &str) -> bool {
    let credentials_complete = proc_id_values(status, "Uid:", 4).is_some()
        && proc_id_values(status, "Gid:", 4).is_some()
        && status
            .lines()
            .find_map(|line| line.strip_prefix("Groups:"))
            .is_some_and(|groups| {
                groups
                    .split_whitespace()
                    .all(|group| group.parse::<u32>().is_ok())
            });
    let capabilities_complete = status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:"))
        .is_some_and(|caps| u64::from_str_radix(caps.trim(), 16).is_ok());
    proc_status_state(status).is_some() && credentials_complete && capabilities_complete
}

#[cfg(target_os = "linux")]
fn inspect_process_status(status: &str) -> io::Result<bool> {
    let state = proc_status_state(status).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "process status has a missing or malformed State field",
        )
    })?;
    if !proc_status_is_complete(status) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "process status has incomplete credentials",
        ));
    }
    Ok(!matches!(state, 'Z' | 'X'))
}

#[cfg(target_os = "linux")]
struct PendingPidError {
    kind: io::ErrorKind,
    only_gone_resolves: bool,
    operation: &'static str,
    message: String,
}

#[cfg(target_os = "linux")]
fn remember_pid_error(
    pending: &mut Option<PendingPidError>,
    what: &'static str,
    error: io::Error,
    allow_zombie_resolution: bool,
) {
    let kind = error.kind();
    let only_gone_resolves = kind == io::ErrorKind::NotFound && !allow_zombie_resolution;
    match pending {
        None => {
            *pending = Some(PendingPidError {
                kind,
                only_gone_resolves,
                operation: what,
                message: error.to_string(),
            });
        }
        Some(previous)
            if previous.kind == io::ErrorKind::NotFound && kind != io::ErrorKind::NotFound =>
        {
            *previous = PendingPidError {
                kind,
                only_gone_resolves,
                operation: what,
                message: error.to_string(),
            };
        }
        Some(previous)
            if previous.kind == io::ErrorKind::NotFound && kind == io::ErrorKind::NotFound =>
        {
            previous.only_gone_resolves |= only_gone_resolves;
        }
        Some(_) => {}
    }
}

#[cfg(target_os = "linux")]
fn process_status(view: &ProcView, pid: u32) -> io::Result<bool> {
    inspect_process_status(&view.read_pid_status(pid)?)
}

#[cfg(target_os = "linux")]
fn pid_is_gone_or_dead(view: &ProcView, pid: u32, allow_zombie: bool) -> io::Result<bool> {
    let proc_dir = view.scan_root.join(pid.to_string());
    match std::fs::symlink_metadata(&proc_dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
        Ok(metadata) if !metadata.is_dir() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "numeric proc entry is not a directory",
            ));
        }
        Ok(_) => {}
    }
    if allow_zombie {
        Ok(!process_status(view, pid)?)
    } else {
        Ok(false)
    }
}

#[cfg(target_os = "linux")]
fn aggregate_pid_error(use_: &mut ProcessUse, pid: u32, pending: Option<PendingPidError>) {
    if let Some(error) = pending {
        use_.enumeration_error.get_or_insert_with(|| {
            format!(
                "cannot {} for process {pid} during cleanup: {}",
                error.operation, error.message
            )
        });
    }
}

#[cfg(target_os = "linux")]
fn finish_pid_scan(
    view: &ProcView,
    pid: u32,
    use_: &mut ProcessUse,
    mut pending: Option<PendingPidError>,
) {
    if let Some(allow_zombie) = pending
        .as_ref()
        .filter(|error| error.kind == io::ErrorKind::NotFound)
        .map(|error| !error.only_gone_resolves)
    {
        match pid_is_gone_or_dead(view, pid, allow_zombie) {
            Ok(true) => {
                pending = None;
                use_.cwd.retain(|holder| *holder != pid);
                use_.fd.retain(|holder| *holder != pid);
            }
            Ok(false) => {}
            Err(error) => {
                remember_pid_error(&mut pending, "confirm process disappearance", error, true)
            }
        }
    }
    aggregate_pid_error(use_, pid, pending);
}

#[cfg(target_os = "linux")]
fn inspect_pid(view: &ProcView, dir: &Path, pid: u32, use_: &mut ProcessUse) {
    let proc_dir = view.scan_root.join(pid.to_string());
    let mut pending = None;
    match std::fs::symlink_metadata(&proc_dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            remember_pid_error(&mut pending, "inspect proc entry", error, false);
            finish_pid_scan(view, pid, use_, pending);
            return;
        }
        Ok(metadata) if !metadata.is_dir() => {
            remember_pid_error(
                &mut pending,
                "inspect proc entry",
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "numeric proc entry is not a directory",
                ),
                false,
            );
            finish_pid_scan(view, pid, use_, pending);
            return;
        }
        Ok(_) => {}
    }
    match process_status(view, pid) {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            remember_pid_error(&mut pending, "establish process identity", error, true);
            finish_pid_scan(view, pid, use_, pending);
            return;
        }
    }

    match std::fs::read_link(proc_dir.join("cwd")) {
        Ok(cwd) if cwd.starts_with(dir) => use_.cwd.push(pid),
        Ok(_) => {}
        Err(error) => remember_pid_error(&mut pending, "inspect cwd", error, true),
    }
    match std::fs::read_dir(proc_dir.join("fd")) {
        Ok(fds) => {
            let mut holds = false;
            for fd in fds {
                let fd = match fd {
                    Ok(fd) => fd,
                    Err(error) => {
                        remember_pid_error(
                            &mut pending,
                            "enumerate file descriptors",
                            error,
                            false,
                        );
                        continue;
                    }
                };
                let path = fd.path();
                match std::fs::read_link(&path) {
                    Ok(target) if target.starts_with(dir) => {
                        holds = true;
                        break;
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        match std::fs::symlink_metadata(&path) {
                            Err(metadata_error)
                                if metadata_error.kind() == io::ErrorKind::NotFound => {}
                            Err(metadata_error) => remember_pid_error(
                                &mut pending,
                                "confirm file descriptor entry disappearance",
                                metadata_error,
                                false,
                            ),
                            Ok(_) => remember_pid_error(
                                &mut pending,
                                "inspect file descriptor",
                                error,
                                false,
                            ),
                        }
                    }
                    Err(error) => {
                        remember_pid_error(&mut pending, "inspect file descriptor", error, false)
                    }
                }
            }
            if holds {
                use_.fd.push(pid);
            }
        }
        Err(error) => remember_pid_error(&mut pending, "enumerate file descriptors", error, true),
    }
    finish_pid_scan(view, pid, use_, pending);
}

pub(crate) fn process_use_under(dir: &Path) -> Result<ProcessUse> {
    process_use_under_from_proc_root(dir, &process_proc_root())
}

#[cfg(target_os = "linux")]
pub(crate) fn process_use_under_from_proc_root(dir: &Path, proc_root: &Path) -> Result<ProcessUse> {
    let dir = dir.canonicalize().map_err(|error| {
        Error::rejected(format!(
            "cannot resolve live-use path {}: {error}",
            dir.display()
        ))
    })?;
    let view = ProcView::open(proc_root)?;
    let completeness_error = view
        .validate_completeness()
        .err()
        .map(|error| error.to_string());
    let complete = completeness_error.is_none();
    let mut use_ = ProcessUse {
        cwd: Vec::new(),
        fd: Vec::new(),
        enumeration_error: completeness_error,
    };
    let procs = match std::fs::read_dir(&view.scan_root) {
        Ok(procs) => procs,
        Err(error) => {
            use_.enumeration_error
                .get_or_insert_with(|| format!("cannot enumerate anchored proc entries: {error}"));
            return Ok(use_);
        }
    };
    let me = std::process::id();
    for entry in procs {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                use_.enumeration_error.get_or_insert_with(|| {
                    format!("cannot enumerate anchored proc entries: {error}")
                });
                continue;
            }
        };
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if complete && pid == me {
            continue;
        }
        inspect_pid(&view, &dir, pid, &mut use_);
    }
    Ok(use_)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn process_use_under_from_proc_root(dir: &Path, proc_root: &Path) -> Result<ProcessUse> {
    let _ = (dir, proc_root);
    Err(Error::rejected(
        "process cwd/open-fd enumeration requires a verified Linux proc view; refusing deletion",
    ))
}

/// CAD-275: how long a worktree must sit untouched before `finish`
/// treats it as idle. Every non-ignored file counts, so a fresh
/// checkout is "active" for this long after `issue start`.
pub(crate) const ACTIVE_WINDOW: Duration = Duration::from_secs(30 * 60);

/// The most recently modified non-ignored file under `wt` — tracked
/// or untracked, via `git ls-files` — with its age, when younger than
/// `ACTIVE_WINDOW`. A file dated in the future counts as just now.
fn recent_activity(wt: &Path) -> Result<Option<(String, Duration)>> {
    let out = git_out(
        wt,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
        &[],
    )?;
    if !out.status.success() {
        return Err(Error::rejected(format!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let now = std::time::SystemTime::now();
    let mut newest: Option<(String, Duration)> = None;
    for rel in out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let rel = String::from_utf8_lossy(rel);
        // A tracked file deleted in the tree is the dirty check's.
        let Ok(modified) = std::fs::symlink_metadata(wt.join(&*rel)).and_then(|m| m.modified())
        else {
            continue;
        };
        let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
        if age < ACTIVE_WINDOW && newest.as_ref().is_none_or(|(_, a)| age < *a) {
            newest = Some((rel.into_owned(), age));
        }
    }
    Ok(newest)
}

/// `45s`, `12m03s`, `30m` — refusal wording for ages and windows.
fn fmt_age(d: Duration) -> String {
    let secs = d.as_secs();
    match (secs / 60, secs % 60) {
        (0, s) => format!("{s}s"),
        (m, 0) => format!("{m}m"),
        (m, s) => format!("{m}m{s:02}s"),
    }
}

/// Best-effort process name for a refusal message.
pub(crate) fn comm_of(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "?".to_string())
}

/// Which kind of daemon answer `message_bound` could not get — the
/// `ProbeError` without its `Error` payload so the shared view can
/// store it per task.
#[derive(Clone, Copy)]
enum ProbeKind {
    Unreachable,
    Inconclusive,
}

/// Is this live message bound to the target's worktree? Binding is
/// recorded, never parsed: the issue's `message` refs name the
/// dispatches sent against it, and a job task's `worktree` names the
/// worktree its kickoff runs in. A task the shared enumeration could
/// not inspect — or a `task_show` that fails — is `Unknown`, never a
/// silent "not bound": the caller surfaces it as a block.
enum Bound {
    Yes,
    No,
    Unknown(ProbeKind),
}

fn task_worktree_matches(t: &Target, wt: Option<&str>) -> bool {
    let Some(wt) = wt else {
        return false;
    };
    t.wt_name.as_deref() == Some(wt)
        || t.wt_dir
            .as_deref()
            .is_some_and(|d| d.to_string_lossy() == wt)
}

fn message_bound(view: &DaemonView, state_dir: &Path, msg: &Value, t: &Target) -> Bound {
    let id = msg["id"].as_str().unwrap_or_default();
    if t.msg_refs.contains(id) {
        return Bound::Yes;
    }
    // A branch-only target has no worktree a task could be bound to —
    // skip the lookup.
    if t.wt_name.is_none() && t.wt_dir.is_none() {
        return Bound::No;
    }
    let Some(task) = msg["task_id"].as_str() else {
        return Bound::No;
    };
    // The shared enumeration first: a task it saw answers locally; a
    // task it could not inspect is an Unknown the caller surfaces.
    if let Some(task_obj) = view.tasks.get(task) {
        return if task_worktree_matches(t, task_obj["worktree"].as_str()) {
            Bound::Yes
        } else {
            Bound::No
        };
    }
    if let Some(kind) = view.task_errors.get(task) {
        return Bound::Unknown(*kind);
    }
    // A task no agent listed — ask directly; the error is routed
    // through the same classification, never swallowed.
    match probe_rpc(state_dir, "task_show", json!({"task": task})) {
        Ok(ts) => {
            if task_worktree_matches(t, ts["task"]["worktree"].as_str()) {
                Bound::Yes
            } else {
                Bound::No
            }
        }
        Err(e) => match classify_probe_error(e, "No such task") {
            ProbeError::Absent => Bound::No,
            ProbeError::Unreachable(_) => Bound::Unknown(ProbeKind::Unreachable),
            ProbeError::Inconclusive(_) => Bound::Unknown(ProbeKind::Inconclusive),
        },
    }
}

/// One daemon-contact snapshot per finish probe — or per sweep, so N
/// issues do not each fan out `agent_list` + `task_show`×N. `up` is
/// false only when the socket does not exist: a cleanly stopped
/// daemon removes it, so "not running" means "no agents" and the
/// /proc + pane scans carry the check. A socket that exists but does
/// not answer is a daemon that was there and stopped answering — its
/// checks still block.
pub(crate) struct DaemonView {
    pub(crate) up: bool,
    /// task id → its `task` payload for every task any agent lists —
    /// the map `inspect` and `message_bound` share.
    tasks: std::collections::HashMap<String, Value>,
    /// task ids whose `task_show` failed — a hole in the enumeration
    /// a bound check must not silently treat as absent.
    task_errors: std::collections::HashMap<String, ProbeKind>,
    /// First enumeration failure per kind (the `agent_list` call or a
    /// `task_show`) — reported as the deferred meta-block's reason.
    enum_unreachable: Option<String>,
    enum_inconclusive: Option<String>,
    /// Agent rows from `agent_list` (alias, cwd, and the rest of the
    /// list payload). Empty when the daemon is down or the list failed
    /// — that failure is `enum_unreachable` / `enum_inconclusive`, not
    /// a guess that no cwd is on the worktree.
    agents: Vec<Value>,
}

impl DaemonView {
    /// Test: the daemon is up but the agent enumeration failed.
    #[cfg(test)]
    pub(crate) fn enumeration_failed(mut self) -> Self {
        self.enum_unreachable = Some("agent_list timed out".to_string());
        self
    }

    /// A test view: `up` plus the agent rows `agent_list` would have
    /// returned (each `{"alias", "cwd"}`), so a reclaim/finish guard's
    /// live-agent arm can be driven without a running daemon.
    #[cfg(test)]
    pub(crate) fn with_agents(up: bool, agents: Vec<Value>) -> Self {
        Self {
            up,
            agents,
            tasks: std::collections::HashMap::new(),
            task_errors: std::collections::HashMap::new(),
            enum_unreachable: None,
            enum_inconclusive: None,
        }
    }
}

/// Why the live-use scan (messages, task bindings, panes, processes)
/// blocks `lane`, or `None` when nothing is bound to it. Reclaim reuses
/// finish's in-use evaluation; any unresolvable or unknown answer blocks.
pub(crate) fn lane_in_use(
    view: &DaemonView,
    state_dir: &Path,
    front: &Front,
    lane: &Path,
    process_use_probe: &ProcessUseProbe,
) -> Option<String> {
    let wt_name = lane.file_name().map(|n| n.to_string_lossy().into_owned());
    let t = Target {
        front: front.clone(),
        body: String::new(),
        dir: PathBuf::new(),
        wt_dir: Some(lane.to_path_buf()),
        msg_refs: bound_msg_refs(front, wt_name.as_deref(), Some(lane)),
        wt_name,
        branch: String::new(),
        root: PathBuf::new(),
        cargo_target: None,
    };
    let (mut blocks, deferred) =
        in_use_blocks_with_process_probe(view, state_dir, &t, process_use_probe);
    blocks.extend(deferred);
    blocks.first().map(|b| b.reason.clone())
}

pub(crate) fn daemon_view(state_dir: &Path) -> DaemonView {
    let mut v = DaemonView {
        up: client::socket_path(state_dir).exists(),
        tasks: std::collections::HashMap::new(),
        task_errors: std::collections::HashMap::new(),
        enum_unreachable: None,
        enum_inconclusive: None,
        agents: Vec::new(),
    };
    if !v.up {
        return v;
    }
    match probe_rpc(state_dir, "agent_list", json!({})) {
        Ok(list) => {
            for a in list["agents"].as_array().into_iter().flatten() {
                v.agents.push(a.clone());
                for tid in a["tasks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    match probe_rpc(state_dir, "task_show", json!({"task": tid})) {
                        Ok(ts) => {
                            if let Some(task) = ts.get("task") {
                                v.tasks.insert(tid.to_string(), task.clone());
                            }
                        }
                        Err(e) => match classify_probe_error(e, "No such task") {
                            // The task is gone — not a hole.
                            ProbeError::Absent => {}
                            ProbeError::Unreachable(e) => {
                                v.task_errors
                                    .insert(tid.to_string(), ProbeKind::Unreachable);
                                v.enum_unreachable.get_or_insert(e.to_string());
                            }
                            ProbeError::Inconclusive(e) => {
                                v.task_errors
                                    .insert(tid.to_string(), ProbeKind::Inconclusive);
                                v.enum_inconclusive.get_or_insert(e.to_string());
                            }
                        },
                    }
                }
            }
        }
        // agent_list takes no alias — no rejection can mean "absent";
        // anything else the daemon answered is inconclusive.
        Err(e @ Error::Internal(_)) => v.enum_unreachable = Some(e.to_string()),
        Err(e) => v.enum_inconclusive = Some(e.to_string()),
    }
    v
}

/// Where the target's branch stands for survivability — one
/// `rev-parse` + `merge_rule` pair feeds both the finish guard and
/// the `--merged` sweep's candidate filter. The variants carry the
/// tip they were computed against so a `branch -D` can be bound to
/// exactly the commit the evidence covered.
enum Branch {
    /// No branch ref recorded, or the recorded ref no longer exists.
    Gone,
    /// CAD-275: zero commits beyond the commit the branch was cut
    /// from — the lane has not started, so nothing on it is merged
    /// (its tip sitting on the default branch proves nothing). Never
    /// a sweep candidate; an explicit finish needs `--force`. `tip`
    /// is deletable: the branch holds no work.
    NotStarted { tip: String },
    /// The ref exists but `merge_rule` does not land — the tip's
    /// commits are covered by no evidence and the branch is never
    /// deleted.
    Open { tip: String },
    /// Merged into the repo's default branch — `merged_by` names
    /// how, `tip` is the commit the evidence was verified against.
    Merged { how: &'static str, tip: String },
}

/// Everything the merge/push evidence was computed AGAINST, recorded
/// in the unlocked probe so the locked phase can prove nothing moved
/// without re-running `gh` or `fetch` while holding writers out:
/// the branch tip, the default ref (name AND commit — either can
/// change mid-probe), and the remote-tracking tip. `remote_tip` is
/// fetch-fresh under `--remote`, else the tracking ref as it stands.
/// `remote_cmp` is false only when a `--remote` fetch could not
/// prove the remote ref — there is no freshness to defend, the note
/// explains, and the delete is skipped. `remote_note` is that note.
struct Evidence {
    state: Branch,
    into: Option<String>,
    into_sha: Option<String>,
    remote_tip: Option<String>,
    remote_cmp: bool,
    remote_note: Option<String>,
}

/// The branch's tip once: `rev-parse refs/heads/<branch>`.
pub(crate) fn branch_tip(root: &Path, branch: &str) -> Option<String> {
    if branch.is_empty() {
        return None;
    }
    git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .ok()
}

/// Where `branch` was cut: the oldest entry of its reflog — `branch:
/// Created from …` for every lane `issue start` mints. `None` when
/// the reflog is gone (expired or disabled): the merge rules decide.
fn fork_point(root: &Path, branch: &str) -> Option<String> {
    let log = git(
        root,
        &[
            "reflog",
            "show",
            "--format=%H",
            &format!("refs/heads/{branch}"),
        ],
    )
    .ok()?;
    log.lines()
        .last()
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// CAD-275: zero commits beyond `git merge-base <tip> <fork point>`
/// — the branch still sits where it was cut (or behind it). Measured
/// against the fork point, not the default branch: a lane merged by
/// fast-forward also has no commits beyond the default branch, yet it
/// did start.
pub(crate) fn not_started(root: &Path, branch: &str, tip: &str) -> bool {
    let Some(fork) = fork_point(root, branch) else {
        return false;
    };
    git(root, &["rev-list", "--count", &format!("{fork}..{tip}")]).is_ok_and(|n| n == "0")
}

/// The tracking tip once: `rev-parse refs/remotes/origin/<branch>`.
fn tracking_tip(root: &Path, branch: &str) -> Option<String> {
    git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/origin/{branch}"),
        ],
    )
    .ok()
}

/// Resolve every input the finish decision needs — all in the
/// unlocked probe. Under `--remote` the tracking ref is fetched
/// first: the remote gate and the delete's lease must evaluate the
/// server's CURRENT tip, never a stale view. A fetch that cannot
/// prove the remote ref (unreachable, or the branch gone there)
/// yields no tip — the delete is skipped and `remote_note` says why;
/// a stale tracking ref is never read as evidence.
fn evidence(t: &Target, remote: bool) -> Evidence {
    let tip = branch_tip(&t.root, &t.branch);
    let into = default_ref(&t.root);
    let into_sha = into
        .as_deref()
        .and_then(|d| git(&t.root, &["rev-parse", "--verify", "--quiet", d]).ok());
    let state = match tip {
        None => Branch::Gone,
        Some(tip) if not_started(&t.root, &t.branch, &tip) => Branch::NotStarted { tip },
        Some(tip) => match into
            .as_deref()
            .and_then(|d| merge_rule(&t.root, &t.branch, &tip, d))
        {
            Some(how) => Branch::Merged { how, tip },
            None => Branch::Open { tip },
        },
    };
    let (remote_tip, remote_cmp, remote_note) = if t.branch.is_empty() {
        (None, false, None)
    } else if remote {
        // An explicit refspec, not a bare branch name: the tracking ref
        // updates even when `remote.origin.fetch` does not map it (a
        // single-branch clone), so the gate and the lease never read a
        // stale tip. `--` ends option parsing before any tracker value.
        let refspec = format!("+refs/heads/{0}:refs/remotes/origin/{0}", t.branch);
        match git_out(&t.root, &["fetch", "--", "origin", &refspec], &[]) {
            Ok(o) if o.status.success() => (tracking_tip(&t.root, &t.branch), true, None),
            Ok(o) if String::from_utf8_lossy(&o.stderr).contains("couldn't find remote ref") => (
                None,
                false,
                Some(format!(
                    "no 'origin/{}' on the remote — nothing deleted",
                    t.branch
                )),
            ),
            _ => (
                None,
                false,
                Some(format!(
                    "cannot reach origin to refresh 'origin/{}' — remote kept",
                    t.branch
                )),
            ),
        }
    } else {
        (tracking_tip(&t.root, &t.branch), true, None)
    };
    Evidence {
        state,
        into,
        into_sha,
        remote_tip,
        remote_cmp,
        remote_note,
    }
}

/// Survivability for `t`'s branch from recorded `evidence`:
/// `merged_by` names the merge evidence; `tip` is the commit a
/// `branch -D` may delete — `Some` only when evidence (merge rule or
/// a pushed remote ref) covers it. A tip with uncovered commits
/// yields the `unmerged-unpushed` block and NO deletable tip — the
/// branch is never deleted.
fn survivability(
    ev: &Evidence,
    t: &Target,
) -> (Option<&'static str>, Option<String>, Option<Block>) {
    match &ev.state {
        Branch::Merged { how, tip } => (Some(*how), Some(tip.clone()), None),
        // No work on the branch — deleting it loses nothing. Whether
        // the lane may be finished at all is `inspect`'s `not-started`
        // block, not survivability.
        Branch::NotStarted { tip } => (None, Some(tip.clone()), None),
        Branch::Open { tip } => {
            let pushed = ev
                .remote_tip
                .as_deref()
                .is_some_and(|r| ancestor(&t.root, tip, r));
            if pushed {
                // Every commit on the branch is on the remote —
                // deleting the local ref loses nothing.
                (None, Some(tip.clone()), None)
            } else {
                (
                    None,
                    None,
                    Some(Block {
                        tag: "unmerged-unpushed".to_string(),
                        reason: format!(
                            "Branch '{}' is neither merged into the default \
                             branch nor pushed — its work would be lost. Merge or \
                             push it, or pass --force",
                            t.branch
                        ),
                    }),
                )
            }
        }
        Branch::Gone => (None, None, None),
    }
}

/// Phase-2 staleness for the probe's evidence — all local
/// `rev-parse`s, no fetch/gh/rpc under the lock. Any input the
/// probe's evidence depended on moving mid-probe makes the probe
/// stale: refuse with "retry" rather than re-run network checks
/// while holding writers out. A remote-side move the tracking ref
/// cannot see is covered by the leased delete instead.
fn stale_evidence_reason(root: &Path, branch: &str, ev: &Evidence) -> Option<String> {
    if !branch.is_empty() {
        let tip_now = branch_tip(root, branch);
        let ev_tip = match &ev.state {
            Branch::Merged { tip, .. } | Branch::Open { tip } | Branch::NotStarted { tip } => {
                Some(tip.clone())
            }
            Branch::Gone => None,
        };
        if tip_now != ev_tip {
            return Some(format!("branch '{branch}' tip moved during finish — retry"));
        }
        if ev.remote_cmp && tracking_tip(root, branch) != ev.remote_tip {
            return Some(format!("'origin/{branch}' moved during finish — retry"));
        }
    }
    let into_now = default_ref(root);
    if into_now != ev.into {
        return Some("the default branch changed during finish — retry".to_string());
    }
    let into_sha_now = into_now
        .as_deref()
        .and_then(|d| git(root, &["rev-parse", "--verify", "--quiet", d]).ok());
    if into_sha_now != ev.into_sha {
        return Some("the default branch moved during finish — retry".to_string());
    }
    None
}

/// Atomically delete `branch` iff its tip is exactly `tip` —
/// `update-ref -d <ref> <tip>` is the compare-and-delete: a tip that
/// moved (or a ref that vanished) fails and keeps its commits.
fn delete_branch_at(root: &Path, branch: &str, tip: &str) -> bool {
    git(
        root,
        &["update-ref", "-d", &format!("refs/heads/{branch}"), tip],
    )
    .is_ok()
}

/// Delete `origin/<branch>` leased on `expect` — the tip the probe
/// proved covered. `--force-with-lease=<ref>:<expect>` makes the
/// server refuse when its ref moved since the fetch: an origin-ahead
/// branch is never deleted by a stale view. The empty-src refspec
/// deletes the ref.
fn remote_delete(root: &Path, branch: &str, expect: &str) -> Result<()> {
    git(
        root,
        &[
            "push",
            &format!("--force-with-lease=refs/heads/{branch}:{expect}"),
            "--",
            "origin",
            &format!(":refs/heads/{branch}"),
        ],
    )
    .map(|_| ())
}

/// Phase-2 staleness: a retargeted pair or a newly recorded in-scope
/// message ref means the unlocked probe is stale — even --force does
/// not retarget a probe. Returns the retry reason when stale.
fn stale_probe_reason(probe: &Target, t: &Target) -> Option<String> {
    if t.wt_dir != probe.wt_dir || t.branch != probe.branch {
        Some(format!(
            "{}'s worktree/branch refs changed during finish — retry",
            t.front.id
        ))
    } else if !t.msg_refs.is_subset(&probe.msg_refs) {
        Some("A dispatch was recorded during finish — retry".to_string())
    } else {
        None
    }
}

/// Which agent the daemon reports holding `alias` — folded into the
/// shared probe-error rules.
enum AgentLookup {
    Shown(Value),
    /// The daemon answered that the thing is absent — it provably
    /// holds nothing in flight.
    Absent,
    /// A daemon-answered error that is neither a clean absence nor a
    /// transport failure (Provider, OutcomeUnknown, a Rejected we did
    /// not anticipate) — the check could not be made.
    Inconclusive(Error),
    /// The daemon could not be reached at all.
    Unreachable(Error),
}

/// The error half of `AgentLookup`, reusable for the enumeration
/// calls that have no `Shown` payload.
enum ProbeError {
    Absent,
    Inconclusive(Error),
    Unreachable(Error),
}

/// Classify a daemon rpc error: `Rejected` is an answer, but only the
/// absence the caller asked about counts as `Absent` — any other
/// rejection is inconclusive rather than silently "not there".
/// `Internal` is a transport failure.
fn classify_probe_error(e: Error, absent_marker: &str) -> ProbeError {
    match e {
        e @ Error::Internal(_) => ProbeError::Unreachable(e),
        Error::Rejected(m) if m.contains(absent_marker) => ProbeError::Absent,
        e => ProbeError::Inconclusive(e),
    }
}

fn agent_lookup(state_dir: &Path, alias: &str) -> AgentLookup {
    match probe_rpc(state_dir, "agent_show", json!({"alias": alias})) {
        Ok(show) => AgentLookup::Shown(show),
        Err(e) => match classify_probe_error(e, "Unknown managed agent") {
            ProbeError::Absent => AgentLookup::Absent,
            ProbeError::Inconclusive(e) => AgentLookup::Inconclusive(e),
            ProbeError::Unreachable(e) => AgentLookup::Unreachable(e),
        },
    }
}

/// One block of a kind no matter how many lookups raise it.
fn push_once(blocks: &mut Vec<Block>, seen: &mut bool, tag: &str, reason: String) {
    if !*seen {
        *seen = true;
        blocks.push(Block {
            tag: tag.to_string(),
            reason,
        });
    }
}

fn push_alias(aliases: &mut Vec<String>, a: &str) {
    if !a.is_empty() && !aliases.iter().any(|x| x == a) {
        aliases.push(a.to_string());
    }
}

/// Whether `cwd` is the worktree directory or a child of it. The
/// worktree path is canonical when that directory exists and lexical
/// otherwise. A sibling whose name only shares a prefix does not match.
fn cwd_on_worktree(wt_dir: &Path, cwd: &Path) -> bool {
    if cwd.as_os_str().is_empty() {
        return false;
    }
    let base = if wt_dir.exists() {
        wt_dir
            .canonicalize()
            .unwrap_or_else(|_| wt_dir.to_path_buf())
    } else {
        wt_dir.to_path_buf()
    };
    let candidate = if cwd.exists() {
        cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf())
    } else {
        cwd.to_path_buf()
    };
    candidate == base || candidate.starts_with(&base)
}

/// Aliases from `agent_list` whose cwd is this worktree. A missing
/// list is not "nobody" — the caller already records that enumeration
/// failure. Historical cwds are not retained here.
pub(crate) fn cwd_holder_aliases(view: &DaemonView, wt_dir: Option<&Path>) -> Vec<String> {
    let Some(dir) = wt_dir else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for agent in &view.agents {
        let Some(alias) = agent["alias"].as_str() else {
            continue;
        };
        let Some(cwd) = agent["cwd"].as_str().filter(|c| !c.is_empty()) else {
            continue;
        };
        if cwd_on_worktree(dir, Path::new(cwd)) {
            push_alias(&mut out, alias);
        }
    }
    out
}

/// The per-worktree guard plus the unchanged dirty/survivability
/// checks, evaluated read-only against a shared daemon `view` and
/// merge `ev`idence. In-use means THIS worktree: a live message
/// recorded against it, an unreconciled `unknown` message bound to it
/// or held by a registered agent whose cwd is on it, a pane tree with
/// cwd inside it, or any process standing in it. A reconciled agent
/// whose cwd is still this path does not block. An owner busy
/// elsewhere does not block.
/// Agent checks run only when the daemon socket exists (`view.up`):
/// a stopped daemon means "no agents" and the /proc and pane scans
/// carry the check. When it answers, only a TRANSPORT failure blocks
/// — a `Rejected` answer (unknown or absent agent) is the daemon
/// proving that agent holds nothing, while a Provider/OutcomeUnknown
/// answer is inconclusive and blocks with its own wording. The /proc
/// scans run regardless.
fn inspect(view: &DaemonView, state_dir: &Path, t: &Target, ev: &Evidence) -> Check {
    let (blocks, deferred) = in_use_blocks(view, state_dir, t);
    inspect_rest(blocks, deferred, t, ev)
}

/// The in-use half of [`inspect`]: live or unknown messages, task
/// bindings, pane trees and processes bound to this worktree. Returns
/// the blocks and the deferred enumeration (meta) failures.
fn in_use_blocks(view: &DaemonView, state_dir: &Path, t: &Target) -> (Vec<Block>, Vec<Block>) {
    in_use_blocks_with_process_probe(view, state_dir, t, &process_use_under)
}

fn in_use_blocks_with_process_probe(
    view: &DaemonView,
    state_dir: &Path,
    t: &Target,
    process_use_probe: &ProcessUseProbe,
) -> (Vec<Block>, Vec<Block>) {
    let mut blocks = Vec::new();
    let mut pane_pid = None;
    let mut unreachable = false;
    let mut inconclusive = false;
    // Enumeration failures are META failures — "the check itself
    // could not run". They report after the specific findings (a
    // named owner, message, pid, dirty path) so the actionable
    // reason always outranks the generic one.
    let mut deferred = Vec::new();
    let mut enum_unreachable = false;
    let mut enum_inconclusive = false;
    if view.up {
        // Every agent that could hold a message bound to this
        // worktree: the owner (whose pane tree also gets the cwd
        // scan), each recipient an in-scope message ref names (the
        // structured `agent` field, falling back to the
        // `dispatch → <alias>` label for refs written before it — a
        // re-assigned issue leaves the earlier dispatchee's bound
        // messages live), and the assignee of any task bound to the
        // worktree.
        let mut aliases: Vec<String> = Vec::new();
        if let Some(owner) = t.front.owner.as_deref() {
            push_alias(&mut aliases, owner);
        }
        for r in t
            .front
            .refs
            .iter()
            .filter(|r| r.kind == "message")
            .filter(|r| r.path.as_deref().is_some_and(|p| t.msg_refs.contains(p)))
        {
            if let Some(a) = r.agent.as_deref().or_else(|| {
                r.label
                    .as_deref()
                    .and_then(|l| l.strip_prefix("dispatch → "))
            }) {
                push_alias(&mut aliases, a);
            }
        }
        // Task-bound coverage: a task kickoff lives on its assignee
        // even when no issue ref recorded it — read off the shared
        // enumeration, which never fails open: a task it could not
        // inspect already raised its deferred meta-block.
        for task in view.tasks.values() {
            if task_worktree_matches(t, task["worktree"].as_str()) {
                if let Some(asg) = task["assignee"].as_str() {
                    push_alias(&mut aliases, asg);
                }
            }
        }
        // Current registry cwd only — not every historical path the
        // row ever had. A child of the worktree counts; a different
        // worktree does not. An unknown on one of these aliases blocks
        // even when the message itself is not bound here.
        let cwd_holders = cwd_holder_aliases(view, t.wt_dir.as_deref());
        for alias in &cwd_holders {
            push_alias(&mut aliases, alias);
        }
        if let Some(e) = &view.enum_unreachable {
            push_once(
                &mut deferred,
                &mut enum_unreachable,
                "daemon-unreachable",
                format!(
                    "Daemon unreachable while enumerating agents/tasks ({e}) — \
                     a task-bound kickoff could be missed; rerun when the \
                     daemon is up or pass --force"
                ),
            );
        }
        if let Some(e) = &view.enum_inconclusive {
            push_once(
                &mut deferred,
                &mut enum_inconclusive,
                "agent-check-inconclusive",
                format!(
                    "Cannot enumerate agents/tasks ({e}) — a task-bound kickoff \
                     could be missed; pass --force"
                ),
            );
        }
        for alias in &aliases {
            match agent_lookup(state_dir, alias) {
                AgentLookup::Shown(show) => {
                    // A queued message blocks only while someone can start
                    // it: an inbox queue is durable backlog that drains
                    // only on `cadence inbox` (CAD-64), and a dead agent's
                    // queue can never begin. running/submitting are live
                    // work for every owner kind.
                    let dead = show["agent"]["dead"].as_bool() == Some(true);
                    let inbox = show["agent"]["endpoint_kind"].as_str() == Some("inbox");
                    let cwd_holder = cwd_holders.iter().any(|a| a == alias);
                    let mut bound_msg = None;
                    let mut unknown_msg = None;
                    for m in show["messages"].as_array().into_iter().flatten() {
                        let state = m["state"].as_str().unwrap_or_default();
                        let unknown = state == "unknown";
                        // `unknown` is in-use whenever it is bound to this
                        // worktree. A cwd holder blocks on any unknown,
                        // bound or not. Queued still waits for a live
                        // non-inbox owner; a stale fence error with zero
                        // unknowns is not a hold.
                        let live = match state {
                            "running" | "submitting" => true,
                            "queued" => !dead && !inbox,
                            "unknown" => true,
                            _ => false,
                        };
                        if !live {
                            continue;
                        }
                        if unknown && cwd_holder {
                            unknown_msg = Some(m);
                            break;
                        }
                        match message_bound(view, state_dir, m, t) {
                            Bound::Yes => {
                                if unknown {
                                    unknown_msg = Some(m);
                                } else {
                                    bound_msg = Some(m);
                                }
                                break;
                            }
                            Bound::No => {}
                            Bound::Unknown(ProbeKind::Unreachable) => push_once(
                                &mut blocks,
                                &mut unreachable,
                                "daemon-unreachable",
                                format!(
                                    "Daemon unreachable while checking a message's \
                                     task binding on '{alias}' — refusing to guess; \
                                     rerun when the daemon is up or pass --force"
                                ),
                            ),
                            Bound::Unknown(ProbeKind::Inconclusive) => push_once(
                                &mut blocks,
                                &mut inconclusive,
                                "agent-check-inconclusive",
                                format!(
                                    "Cannot check a message's task binding on \
                                     '{alias}' — refusing to guess; pass --force"
                                ),
                            ),
                        }
                    }
                    if let Some(msg) = unknown_msg {
                        blocks.push(Block {
                            tag: "unreconciled-unknown".to_string(),
                            reason: format!(
                                "Agent '{alias}' has unreconciled unknown message {} — \
                                 reconcile it before finishing this worktree, or pass --force",
                                msg["id"].as_str().unwrap_or_default()
                            ),
                        });
                    } else if let Some(msg) = bound_msg {
                        let body: String = msg["body"]
                            .as_str()
                            .unwrap_or_default()
                            .chars()
                            .take(60)
                            .collect();
                        blocks.push(Block {
                            tag: "bound-message".to_string(),
                            reason: format!(
                                "Agent '{alias}' has a {} message {} recorded \
                                 against this worktree: \"{body}\" — wait for \
                                 it or pass --force",
                                msg["state"].as_str().unwrap_or_default(),
                                msg["id"].as_str().unwrap_or_default()
                            ),
                        });
                    }
                    // Only the owner's pane tree gets the descendant cwd
                    // scan — other panes are caught by the /proc scan
                    // below like any process.
                    if Some(alias.as_str()) == t.front.owner.as_deref()
                        && show["agent"]["endpoint_kind"].as_str() == Some("pty")
                        && !dead
                    {
                        pane_pid = show["agent"]["pid"].as_u64().map(|p| p as u32);
                    }
                }
                AgentLookup::Absent => {}
                AgentLookup::Unreachable(e) => push_once(
                    &mut blocks,
                    &mut unreachable,
                    "daemon-unreachable",
                    format!(
                        "Daemon unreachable while checking '{alias}' ({e}) — \
                         the agent checks cannot run; rerun when the daemon is \
                         up or pass --force"
                    ),
                ),
                AgentLookup::Inconclusive(e) => push_once(
                    &mut blocks,
                    &mut inconclusive,
                    "agent-check-inconclusive",
                    format!(
                        "Cannot check agent '{alias}' ({e}) — the daemon \
                         answered but the result is inconclusive; refusing \
                         to guess, pass --force"
                    ),
                ),
            }
        }
    }
    // Any process cwd or open descriptor in the worktree blocks removal.
    // Enumeration failure is itself a refusal: an unreadable /proc entry
    // cannot be interpreted as no live user.
    if let Some(d) = t.wt_dir.as_deref().filter(|d| d.is_dir()) {
        match process_use_probe(d) {
            Err(e) => blocks.push(Block {
                tag: "process-enumeration-failed".to_string(),
                reason: format!(
                    "Cannot fully enumerate process cwd/open-fd use for {} ({e}) — refusing deletion",
                    d.display()
                ),
            }),
            Ok(uses) => {
                if let Some(error) = &uses.enumeration_error {
                    let live = uses
                        .fd
                        .first()
                        .map(|pid| {
                            format!(
                                "; process {pid} ({}) holds an open file descriptor inside {}",
                                comm_of(*pid),
                                d.display()
                            )
                        })
                        .or_else(|| {
                            uses.cwd.first().map(|pid| {
                                format!(
                                    "; process {pid} ({}) has cwd inside {}",
                                    comm_of(*pid),
                                    d.display()
                                )
                            })
                        })
                        .unwrap_or_default();
                    blocks.push(Block {
                        tag: "process-enumeration-failed".to_string(),
                        reason: format!(
                            "Cannot fully enumerate process cwd/open-fd use ({error}){live} — refusing deletion"
                        ),
                    });
                }
                let mut pane_hit = None;
                let mut proc_hit = None;
                for pid in uses.cwd {
                    if pane_pid.is_some_and(|pp| pty::descends_from(pid, pp)) {
                        pane_hit.get_or_insert(pid);
                    } else {
                        proc_hit.get_or_insert(pid);
                    }
                }
                if let Some(pid) = pane_hit {
                    blocks.push(Block {
                        tag: "pane-cwd".to_string(),
                        reason: format!(
                            "Owner '{}' pane descendant pid {pid} ({}) has cwd \
                             inside {} — wait for it or pass --force",
                            t.front.owner.as_deref().unwrap_or("?"),
                            comm_of(pid),
                            d.display()
                        ),
                    });
                }
                if let Some(pid) = proc_hit {
                    blocks.push(Block {
                        tag: "proc-cwd".to_string(),
                        reason: format!(
                            "Process {pid} ({}) has cwd inside {} — close it or \
                             cd out of the worktree, or pass --force",
                            comm_of(pid),
                            d.display()
                        ),
                    });
                }
                if let Some(pid) = uses.fd.first() {
                    blocks.push(Block {
                        tag: "proc-open-fd".to_string(),
                        reason: format!(
                            "Process {pid} ({}) holds an open file descriptor inside {} — \
                             close it before removing the worktree",
                            comm_of(*pid),
                            d.display()
                        ),
                    });
                }
            }
        }
    }
    (blocks, deferred)
}

fn inspect_rest(mut blocks: Vec<Block>, deferred: Vec<Block>, t: &Target, ev: &Evidence) -> Check {
    // Dirty worktree — `--ignored` marks ignored paths `!!` so a build
    // artifact (like the ui/node_modules symlink) never blocks; only
    // real changes and non-ignored untracked files do. A status that
    // cannot be read refuses too: an unreadable tree is not a clean one.
    if let Some(d) = t.wt_dir.as_deref().filter(|d| d.is_dir()) {
        match git(d, &["status", "--porcelain", "--ignored"]) {
            Err(e) => blocks.push(Block {
                tag: format!("dirty-check-failed: {e}"),
                reason: format!(
                    "Cannot check {} for uncommitted changes ({e}) — \
                     refusing to guess; pass --force",
                    d.display()
                ),
            }),
            Ok(status) => {
                let dirty: Vec<&str> = status.lines().filter(|l| !l.starts_with("!!")).collect();
                if !dirty.is_empty() {
                    let list: Vec<&str> = dirty.iter().take(10).copied().collect();
                    blocks.push(Block {
                        tag: "dirty-worktree".to_string(),
                        reason: format!(
                            "Worktree {} has uncommitted changes:\n  {}\nCommit, \
                             stash or pass --force",
                            d.display(),
                            list.join("\n  ")
                        ),
                    });
                }
            }
        }
    }
    // CAD-275: recent activity. A worker cadence cannot see (a
    // subagent with no registered pane, message or process cwd in the
    // lane) still leaves fresh files behind — the newest non-ignored
    // file, tracked or untracked, is the evidence. A tree that cannot
    // be listed refuses too: unreadable is not idle.
    if let Some(d) = t.wt_dir.as_deref().filter(|d| d.is_dir()) {
        match recent_activity(d) {
            Err(e) => blocks.push(Block {
                tag: "activity-check-failed".to_string(),
                reason: format!(
                    "Cannot check {} for recent activity ({e}) — refusing \
                     to guess; pass --force",
                    d.display()
                ),
            }),
            Ok(Some((path, age))) => blocks.push(Block {
                tag: "recent-activity".to_string(),
                reason: format!(
                    "Worktree {} was modified {} ago ({path}) — it may be in \
                     use; finish once it has been idle {}, or pass --force",
                    d.display(),
                    fmt_age(age),
                    fmt_age(ACTIVE_WINDOW)
                ),
            }),
            Ok(None) => {}
        }
    }
    // CAD-275: a branch that has not started is never merged — finish
    // would remove a lane someone may be about to use.
    if matches!(ev.state, Branch::NotStarted { .. }) {
        blocks.push(Block {
            tag: "not-started".to_string(),
            reason: format!(
                "Branch '{}' has no commits beyond where it was cut — the \
                 lane has not started, so nothing is merged. Pass --force \
                 to finish an abandoned lane",
                t.branch
            ),
        });
    }
    // Survivability: the branch's work must survive somewhere — merged
    // into the repo's default branch or pushed.
    if let (_, _, Some(b)) = survivability(ev, t) {
        blocks.push(b);
    }
    // Meta-failures last: a named owner, message, pid, dirty path or
    // unmerged branch is the actionable reason — "the enumeration
    // itself could not run" reports only when nothing else did. A
    // duplicate tag already reported is not pushed twice.
    for b in deferred {
        if !blocks.iter().any(|x| x.tag == b.tag) {
            blocks.push(b);
        }
    }
    Check { blocks }
}

/// What one `issue finish <ID>` does — the CLI flags, and each sweep
/// row's (which never forces).
pub(crate) struct FinishArgs<'a> {
    pub force: bool,
    pub keep_branch: bool,
    pub remote: bool,
    /// Finish exactly this open worktree ref — `--worktree`, and each
    /// sweep row. Required when the issue has several.
    pub worktree: Option<&'a Path>,
    /// `--worktree` only (CAD-274): a recorded directory that is
    /// already gone closes its worktree/branch refs in one tracker
    /// commit and touches no git state — the branch, if any, is kept.
    /// The sweep never sets it: its missing-dir rows stay open.
    pub close_if_gone: bool,
}

/// `issue finish <ID> [--worktree P] [--force] [--keep-branch] [--remote]` — JSON like
/// the other issue verbs. Runs in two phases so the pm lock is never
/// held across a daemon RPC, a `fetch` or a `gh` call: the resolve +
/// probe (daemon view, /proc scans, git and gh evidence, the remote
/// fetch) happen unlocked, then the lock covers only the commit
/// phase — a fresh resolve for staleness, LOCAL `rev-parse`s proving
/// the evidence's inputs did not move, the removals, and the tracker
/// write. `shared` lets a sweep reuse one enumeration for every
/// issue instead of re-issuing `agent_list` + `task_show`×N per row;
/// a binding added mid-sweep still lands as a stale probe.
pub(crate) fn run(
    pm: &Pm,
    id: &str,
    args: &FinishArgs,
    actor: &str,
    state_dir: &Path,
    shared: Option<&DaemonView>,
) -> Result<Value> {
    let force = args.force;
    // Phase 1 — the unlocked probe. Nothing here holds the pm lock:
    // one enumeration + the merge/remote evidence (incl. the
    // `--remote` fetch and `gh`) all resolve before any lock.
    let owned;
    let view = match shared {
        Some(v) => v,
        None => {
            owned = daemon_view(state_dir);
            &owned
        }
    };
    let probe = match resolve(pm, id, args.worktree)? {
        Resolve::Nothing => {
            return Err(Error::rejected(format!(
                "{id}: no worktree/branch refs recorded — nothing to finish"
            )))
        }
        Resolve::Finished => {
            return Ok(json!({"issue": id, "finished": false,
                             "reason": "worktree already finished"}))
        }
        Resolve::Target(t) => t,
    };
    if probe.wt_dir.is_none() && !probe.branch.is_empty() && args.keep_branch && !args.remote {
        return Ok(json!({
            "issue": id,
            "finished": false,
            "branch": probe.branch,
            "kept_branch": true,
            "deleted_branch": false,
            "removed_worktree": false,
            "reason": "no managed checkout path; branch/ref retained; use issue start with recorded lane/name to reattach before finishing"
        }));
    }
    // A gone directory under `--worktree` only closes refs: nothing
    // is removed or deleted, so no branch or remote is touched and
    // survivability has nothing to protect.
    let refs_only = args.close_if_gone && probe.wt_dir.as_deref().is_some_and(|d| !d.is_dir());
    let keep_branch = args.keep_branch || refs_only;
    let remote = args.remote && !refs_only;
    if refs_only {
        let path = probe.wt_dir.as_deref().ok_or_else(|| {
            Error::rejected(format!("issue {id} has no path for refs-only finish"))
        })?;
        verify_refs_only_ownership(
            &probe.root,
            path,
            id,
            (!probe.branch.is_empty()).then_some(probe.branch.as_str()),
        )?;
    } else if let Some(path) = probe.wt_dir.as_deref() {
        let _ = verify_checkout_ownership(
            &probe.root,
            path,
            id,
            (!probe.branch.is_empty()).then_some(probe.branch.as_str()),
        )?;
    } else if !probe.branch.is_empty() && (!keep_branch || remote) {
        return Err(Error::rejected(format!(
            "issue {id} has no managed checkout path for branch {}; refusing deletion",
            probe.branch
        )));
    }
    let ev = evidence(&probe, remote);
    let (merged_how, tip, _) = survivability(&ev, &probe);
    // The --remote coverage decision is probe data too — `gh` runs
    // unlocked. Merge evidence must cover the FETCHED remote tip;
    // survivability's "pushed" is not enough (origin can hold commits
    // no evidence covers — another agent pushing ahead — and an
    // unmerged-but-pushed branch's last copy IS that remote).
    let remote_covered = remote
        && ev.remote_cmp
        && merged_how.is_some()
        && ev
            .remote_tip
            .as_deref()
            .zip(ev.into.as_deref())
            .is_some_and(|(r, into)| merge_rule(&probe.root, &probe.branch, r, into).is_some());
    // The per-worktree guard + dirty + survivability, evaluated once.
    // `--force` records every block it bypasses; without it the first
    // block refuses, naming its pid or message id.
    let check = inspect(view, state_dir, &probe, &ev);
    let mut overridden = Vec::new();
    for b in check.blocks {
        if refs_only && matches!(b.tag.as_str(), "not-started" | "unmerged-unpushed") {
            continue;
        }
        let safety_probe_failed = matches!(
            b.tag.as_str(),
            "process-enumeration-failed"
                | "daemon-unreachable"
                | "agent-check-inconclusive"
                | "activity-check-failed"
        ) || b.tag.starts_with("dirty-check-failed:");
        if safety_probe_failed && !refs_only {
            return Err(Error::rejected(format!(
                "{} — failed safety enumeration cannot be overridden with --force",
                b.reason
            )));
        }
        if force {
            overridden.push(b.tag);
        } else {
            return Err(Error::rejected(b.reason));
        }
    }

    // Phase 2 — the commit phase, under the pm lock. Re-resolve so a
    // tracker write that landed mid-probe is seen, then prove every
    // input the probe's evidence depended on is unmoved — all local
    // `rev-parse`s; nothing networked runs while holding the lock.
    let _lock = pm.lock()?;
    let mut t = match resolve(pm, id, args.worktree)? {
        // A concurrent finish closed the pair mid-probe — the same
        // idempotent answer the unlocked probe would have given.
        Resolve::Finished => {
            return Ok(json!({"issue": id, "finished": false,
                             "reason": "worktree already finished"}))
        }
        Resolve::Nothing => {
            return Err(Error::rejected(format!(
                "{id}: worktree/branch refs vanished during finish — retry"
            )))
        }
        Resolve::Target(t) => t,
    };
    // A retargeted pair or a newly recorded message ref means the
    // probe is stale — even --force does not retarget a probe.
    if let Some(reason) = stale_probe_reason(&probe, &t) {
        return Err(Error::rejected(reason));
    }
    // Any input the evidence was computed against moving mid-probe —
    // the branch tip, the default ref, or the tracking ref a
    // concurrent fetch updated — also makes it stale. A remote-side
    // move the tracking ref cannot see is covered by the leased
    // delete below, not this check.
    if let Some(reason) = stale_evidence_reason(&t.root, &t.branch, &ev) {
        return Err(Error::rejected(reason));
    }
    // Local live-use is rechecked at the destructive boundary. Daemon and
    // tracker bindings were probed unlocked above; new tracker refs make the
    // stale-probe check fail, while process cwd/FD use can change without a
    // tracker write.
    if !refs_only {
        if let Some(d) = t.wt_dir.as_deref().filter(|d| d.is_dir()) {
            match process_use_under(d) {
                Err(e) => {
                    return Err(Error::rejected(format!(
                        "Cannot revalidate process cwd/open-fd use for {} ({e}) — refusing deletion; --force cannot override failed enumeration",
                        d.display()
                    )))
                }
                Ok(uses) if uses.enumeration_error.is_some() => {
                    return Err(Error::rejected(format!(
                        "Cannot fully revalidate process cwd/open-fd use for {} ({}) — refusing deletion; --force cannot override failed enumeration",
                        d.display(), uses.enumeration_error.as_deref().unwrap_or("unknown process enumeration failure")
                    )))
                }
                Ok(uses) if !uses.cwd.is_empty() || !uses.fd.is_empty() => {
                    let pid = uses.cwd.first().or_else(|| uses.fd.first()).copied().unwrap_or(0);
                    let tag = if uses.cwd.contains(&pid) { "proc-cwd" } else { "proc-open-fd" };
                    let reason = format!(
                        "Process {pid} ({}) began using {} during finish — close it before deletion",
                        comm_of(pid), d.display()
                    );
                    if force {
                        if !overridden.iter().any(|old| old == tag) {
                            overridden.push(tag.to_string());
                        }
                    } else {
                        return Err(Error::rejected(reason));
                    }
                }
                Ok(_) => {}
            }
        }
    }
    let merged_by = merged_how.map_or(Value::Null, |h| json!(h));
    let dir = t.dir.clone();
    let wt_dir = t.wt_dir.clone();
    let wt_name = t.wt_name.clone();
    let branch = t.branch.clone();
    let root = t.root.clone();
    let cargo_target = t.cargo_target.clone();

    // "Pushed" was the local branch's only evidence — under --remote,
    // keeping the uncovered remote is what makes deleting the local
    // safe; if the remote is kept AND the local's commits are its
    // only other copy story, keep both (the row explains; --force
    // overrides). A plain finish never deletes the remote, so the
    // pushed evidence stands on its own and the local still goes.
    let keep_for_remote = remote
        && ev.remote_tip.is_some()
        && !remote_covered
        && !force
        && merged_how.is_none()
        && tip.is_some();
    let will_remove_worktree = wt_dir.as_deref().is_some_and(|d| d.is_dir());
    let will_delete_branch =
        !keep_branch && !branch.is_empty() && !keep_for_remote && tip.is_some();
    let will_delete_remote = remote
        && !branch.is_empty()
        && ev.remote_note.is_none()
        && ev.remote_tip.is_some()
        && (remote_covered || force);
    let mut lifecycle_release = if refs_only {
        let path = wt_dir.as_deref().ok_or_else(|| {
            Error::rejected(format!("issue {id} has no path for refs-only finish"))
        })?;
        Some(crate::worktree::lifecycle::begin_refs_only_finish(
            &root,
            path,
            id,
            (!branch.is_empty()).then_some(branch.as_str()),
            &format!("issue finish {id} refs-only closure"),
        )?)
    } else if will_remove_worktree || will_delete_branch || will_delete_remote {
        let path = wt_dir.as_deref().ok_or_else(|| {
            Error::rejected(format!(
                "issue {id} has no managed checkout path for branch {branch}; refusing deletion"
            ))
        })?;
        verify_checkout_ownership(
            &root,
            path,
            id,
            (!branch.is_empty()).then_some(branch.as_str()),
        )?;
        Some(crate::worktree::lifecycle::begin_release(
            &root,
            path,
            id,
            (!branch.is_empty()).then_some(branch.as_str()),
            &format!("issue finish {id} for branch {branch}"),
        )?)
    } else {
        None
    };

    // Removal: the worktree first (frees the branch), then the branch
    // — and only the exact tip the evidence covered: a tip that moved
    // (or was never covered) keeps its commits, so an uncovered branch
    // is never `branch -D`'d even under --force.
    let mut removed_worktree = false;
    if !refs_only {
        if let Some(d) = wt_dir.as_deref().filter(|d| d.is_dir()) {
            let target = d.to_string_lossy().into_owned();
            let mut args = vec!["worktree", "remove"];
            if force {
                args.push("--force");
            }
            args.push("--");
            args.push(&target);
            if let Err(e) = git(&root, &args) {
                let reason = format!("git worktree remove {} failed: {e}", d.display());
                if let Some(release) = lifecycle_release.as_mut() {
                    let _ = release.cleanup_failed(&reason);
                }
                return Err(Error::rejected(reason));
            }
            removed_worktree = true;
        }
    }
    let mut deleted_branch = false;
    let mut branch_note = Value::Null;
    if refs_only && branch_tip(&root, &branch).is_some() {
        branch_note = json!("kept: the worktree dir was already gone — refs closed only");
    }
    if !keep_branch && !branch.is_empty() {
        if keep_for_remote {
            branch_note = json!(format!(
                "kept: 'origin/{branch}' is not covered by merge evidence — \
                 deleting the local branch would leave one stray copy"
            ));
        } else {
            match tip {
                Some(tip) => {
                    deleted_branch = delete_branch_at(&root, &branch, &tip);
                    if !deleted_branch {
                        branch_note = json!(format!(
                            "branch tip moved during finish — kept (its \
                             evidence covered {tip})"
                        ));
                    }
                }
                None => {
                    // tip=None: either the branch is already gone
                    // (nothing to say) or its tip has commits no
                    // evidence covers — never deleted, even --force.
                    if git(
                        &root,
                        &[
                            "rev-parse",
                            "--verify",
                            "--quiet",
                            &format!("refs/heads/{branch}"),
                        ],
                    )
                    .is_ok()
                    {
                        branch_note =
                            json!("branch tip has commits no merge/push evidence covers — kept");
                    }
                }
            }
        }
    }
    // One tracker commit marks the refs closed — kept as history. A
    // branch this finish left standing (`--keep-branch`, kept for its
    // remote, an uncovered or moved tip) keeps its ref open, so the
    // surviving work stays on the board and finishable (CAD-145). A
    // refs-only close of a missing checkout closes only the worktree ref;
    // any surviving branch ref remains open for recovery.
    let branch_kept = !deleted_branch && !branch.is_empty() && branch_tip(&root, &branch).is_some();
    let disposition = if branch_kept {
        format!("checkout released; branch {branch} retained")
    } else {
        format!("checkout released; branch {branch} removed or already missing")
    };
    // The lifecycle transaction ends at the destructive boundary, before any
    // tracker write can fail after the checkout has already been removed.
    let release_warning = if refs_only {
        None
    } else {
        lifecycle_release
            .take()
            .and_then(|mut release| release.released(&disposition).err())
            .map(|e| e.to_string())
    };
    for r in &mut t.front.refs {
        if (r.kind == "worktree"
            && wt_dir
                .as_deref()
                .is_some_and(|d| r.path.as_deref() == Some(d.to_string_lossy().as_ref())))
            || (r.kind == "branch"
                && !branch.is_empty()
                && !branch_kept
                && r.path.as_deref() == Some(&branch))
        {
            r.closed = Some(true);
        }
    }
    // CAD-454: the front write is committed only if the tracker commit
    // lands — a refused commit puts the file back so the refs stay
    // open on disk, not just in memory.
    let front_file = dir.join("issue.md");
    let front_prev = std::fs::read(&front_file).ok();
    write::save_front(&dir, &t.front, &t.body)?;
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
    if let Err(e) = pm.commit(
        std::slice::from_ref(&front_file),
        &format!("{subject}\n\n{trailers}"),
    ) {
        match &front_prev {
            Some(bytes) => {
                let _ = std::fs::write(&front_file, bytes);
            }
            None => {
                let _ = std::fs::remove_file(&front_file);
            }
        }
        return Err(e);
    }
    let lifecycle_warning = if refs_only {
        lifecycle_release
            .take()
            .and_then(|mut release| release.released(&disposition).err())
            .map(|e| e.to_string())
    } else {
        release_warning
    };
    // The commit phase is done — the pm lock goes back before the
    // remote delete: a `push` can take seconds and the lock's spin
    // deadline is 15s. The lease below, not lock ordering, is what
    // makes the delete safe.
    drop(_lock);

    let mut remote_deleted = false;
    let mut remote_note = Value::Null;
    if remote {
        if branch.is_empty() {
            remote_note = json!("no branch ref — nothing deleted");
        } else if let Some(note) = &ev.remote_note {
            // The fetch could not prove the remote ref — keep it and
            // say why; a stale tracking ref never stands in.
            remote_note = json!(note);
        } else {
            match ev.remote_tip.as_deref() {
                None => {
                    remote_note = json!(format!("no 'origin/{branch}' — nothing deleted"));
                }
                Some(expect) => {
                    if !remote_covered && !force {
                        remote_note = json!(
                            "remote branch kept: its tip is not covered by merge \
                             evidence — pass --force to delete it anyway"
                        );
                    } else {
                        // Leased on the fetched tip: a remote-side move
                        // since the fetch refuses the push instead of
                        // deleting commits this host never saw.
                        match remote_delete(&root, &branch, expect) {
                            Ok(_) => {
                                remote_deleted = true;
                                if !remote_covered {
                                    overridden.push("remote-delete-uncovered".to_string());
                                }
                            }
                            Err(e) => remote_note = json!(e.to_string()),
                        }
                    }
                }
            }
        }
    }

    // Accounting only — `git worktree remove` takes the worktree dir
    // and nothing else. `cargo_target_exists` is a literal check on
    // the recorded path after the removal, emitted only when a target
    // was recorded: a target inside the worktree reports false while
    // the shared dep cache it linked into survives untouched (rm
    // unlinks symlinks; it never follows them).
    let mut out = json!({
        "issue": t.front.id,
        "finished": true,
        "worktree": wt_dir,
        "branch": branch,
        "cargo_target": cargo_target,
        "removed_worktree": removed_worktree,
        "deleted_branch": deleted_branch,
        "branch_note": branch_note,
        "kept_branch": keep_branch,
        "remote_deleted": remote_deleted,
        "remote_note": remote_note,
        "forced": force,
        "overrode": overridden,
        "merged_by": merged_by,
        "not_started": matches!(ev.state, Branch::NotStarted { .. }),
        "refs_only": refs_only,
        "checkout_disposition": disposition,
        "lifecycle_warning": lifecycle_warning,
        "status": t.front.status,
    });
    if let Some(target) = &cargo_target {
        // `symlink_metadata` — a recorded path that is itself a
        // symlink reports its own presence, not its target's.
        out["cargo_target_exists"] = json!(Path::new(target).symlink_metadata().is_ok());
    }
    Ok(out)
}

/// Where a sweep candidate's recorded worktree and branch actually
/// are. Read-only: a missing directory or a branch checked out at
/// another path is a reconciliation, never a finish.
struct PathPreview {
    /// `present` when the recorded directory exists, else `missing`.
    path_state: &'static str,
    /// `present`, `missing`, or `elsewhere` (the branch ref exists
    /// and `git worktree list` has it at a different path).
    branch_state: &'static str,
    /// Path `git worktree list` reports for this branch, if any.
    live_path: Option<PathBuf>,
    /// Stable skip reason when the sweep must not finish this row.
    reconcile: Option<&'static str>,
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

fn branch_checkout_path(root: &Path, branch: &str) -> Option<PathBuf> {
    registered_branch_path(root, branch).ok().flatten()
}

fn path_preview(t: &Target) -> PathPreview {
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

fn annotate_preview(row: &mut Value, preview: &PathPreview) {
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
fn sweep_row(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn repo() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let r = tmp.path();
        git(r, &["init", "-b", "main"]).unwrap();
        git(r, &["config", "user.email", "t@t"]).unwrap();
        git(r, &["config", "user.name", "t"]).unwrap();
        std::fs::write(r.join("f"), "one").unwrap();
        git(r, &["add", "f"]).unwrap();
        git(r, &["commit", "-m", "one"]).unwrap();
        tmp
    }

    fn target(id: &str) -> Target {
        Target {
            front: Front::new(id, "t", "now"),
            body: String::new(),
            dir: PathBuf::new(),
            wt_dir: None,
            wt_name: None,
            branch: "side".to_string(),
            root: PathBuf::new(),
            msg_refs: HashSet::new(),
            cargo_target: None,
        }
    }

    /// CAD-275: "not started" is measured from where the branch was
    /// cut, not from the default branch — a lane fast-forwarded into
    /// main also has no commits beyond main, yet it did start.
    #[test]
    fn not_started_measures_from_the_fork_point() {
        let tmp = repo();
        let r = tmp.path();
        git(r, &["branch", "lane"]).unwrap();
        let tip = git(r, &["rev-parse", "lane"]).unwrap();
        assert!(not_started(r, "lane", &tip), "cut and untouched");
        git(r, &["checkout", "-q", "lane"]).unwrap();
        std::fs::write(r.join("f"), "two").unwrap();
        git(r, &["commit", "-qam", "two"]).unwrap();
        git(r, &["checkout", "-q", "main"]).unwrap();
        git(r, &["merge", "-q", "--ff-only", "lane"]).unwrap();
        let tip = git(r, &["rev-parse", "lane"]).unwrap();
        assert!(ancestor(r, &tip, "main"));
        assert!(!not_started(r, "lane", &tip), "fast-forwarded work started");
        // No reflog (expired or disabled): no fork point, and the
        // merge rules decide — never a guess of "not started".
        git(r, &["-c", "core.logAllRefUpdates=false", "branch", "bare"]).unwrap();
        assert!(fork_point(r, "bare").is_none());
        assert!(!not_started(r, "bare", &tip));
    }

    #[test]
    fn fmt_age_reads_like_a_person() {
        assert_eq!(fmt_age(Duration::from_secs(0)), "0s");
        assert_eq!(fmt_age(Duration::from_secs(45)), "45s");
        assert_eq!(fmt_age(Duration::from_secs(12 * 60 + 3)), "12m03s");
        assert_eq!(fmt_age(ACTIVE_WINDOW), "30m");
    }

    #[test]
    fn delete_branch_at_is_compare_and_delete() {
        let tmp = repo();
        let r = tmp.path();
        git(r, &["branch", "side"]).unwrap();
        let tip = git(r, &["rev-parse", "side"]).unwrap();
        // A second commit the evidence pretended to cover — deleting
        // against IT must refuse and keep the real tip's commits.
        std::fs::write(r.join("f"), "two").unwrap();
        git(r, &["commit", "-am", "two"]).unwrap();
        let other = git(r, &["rev-parse", "main"]).unwrap();
        assert_ne!(tip, other);
        assert!(!delete_branch_at(r, "side", &other));
        assert_eq!(git(r, &["rev-parse", "side"]).unwrap(), tip);
        // The exact covered tip deletes; the ref is then gone.
        assert!(delete_branch_at(r, "side", &tip));
        assert!(git(r, &["rev-parse", "--verify", "--quiet", "side"]).is_err());
        // Deleting an absent ref is a no-op, not a crash.
        assert!(!delete_branch_at(r, "side", &tip));
    }

    /// A repo with a bare `origin` remote — the fixture for the
    /// leased-delete unit test.
    fn repo_with_origin() -> (TempDir, TempDir) {
        let tmp = repo();
        let r = tmp.path();
        git(r, &["branch", "side"]).unwrap();
        let bare = TempDir::new().unwrap();
        git(bare.path(), &["init", "--bare", "-b", "main"]).unwrap();
        git(
            r,
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        )
        .unwrap();
        git(r, &["push", "-q", "origin", "main", "side"]).unwrap();
        git(r, &["fetch", "-q", "origin"]).unwrap();
        (tmp, bare)
    }

    #[test]
    fn remote_delete_is_leased_on_the_expected_tip() {
        let (tmp, bare) = repo_with_origin();
        let r = tmp.path();
        let tip = git(r, &["rev-parse", "side"]).unwrap();
        // The expected tip deletes the remote ref.
        assert!(remote_delete(r, "side", &tip).is_ok());
        assert!(
            git(bare.path(), &["rev-parse", "--verify", "--quiet", "side"]).is_err(),
            "remote branch must be gone"
        );
        // Re-push, then advance the remote PAST the proven tip (the
        // mid-finish move — another agent pushing ahead): the leased
        // push must refuse and the new commits must survive.
        git(r, &["push", "-q", "origin", "side"]).unwrap();
        std::fs::write(r.join("f"), "more").unwrap();
        git(r, &["commit", "-qam", "remote ahead"]).unwrap();
        let ahead = git(r, &["rev-parse", "main"]).unwrap();
        git(r, &["push", "-q", "origin", "main:side"]).unwrap();
        assert_eq!(
            git(bare.path(), &["rev-parse", "side"]).unwrap(),
            ahead,
            "fixture: the remote must hold the advanced tip"
        );
        // The stale tracking view the probe had proven covered.
        git(r, &["update-ref", "refs/remotes/origin/side", &tip]).unwrap();
        assert!(
            remote_delete(r, "side", &tip).is_err(),
            "a remote that moved past the proven tip must refuse"
        );
        assert_eq!(
            git(bare.path(), &["rev-parse", "side"]).unwrap(),
            ahead,
            "the unseen commits must still be on the remote"
        );
    }

    /// CAD-144: a single-branch clone's configured refspec does not map
    /// the lane, so a bare `fetch origin <b>` leaves the tracking ref
    /// stale. The explicit refspec must refresh it — the coverage gate
    /// and the lease read the server's current tip.
    #[test]
    fn remote_fetch_refreshes_tracking_on_a_single_branch_clone() {
        let (tmp, _bare) = repo_with_origin();
        let r = tmp.path();
        let stale = git(r, &["rev-parse", "side"]).unwrap();
        std::fs::write(r.join("f"), "ahead").unwrap();
        git(r, &["commit", "-qam", "ahead"]).unwrap();
        let ahead = git(r, &["rev-parse", "main"]).unwrap();
        git(r, &["push", "-q", "origin", "main:side"]).unwrap();
        git(
            r,
            &[
                "config",
                "remote.origin.fetch",
                "+refs/heads/main:refs/remotes/origin/main",
            ],
        )
        .unwrap();
        git(r, &["update-ref", "refs/remotes/origin/side", &stale]).unwrap();
        let mut t = target("CAD-1");
        t.root = r.to_path_buf();
        let ev = evidence(&t, true);
        assert!(ev.remote_cmp, "{:?}", ev.remote_note);
        assert_eq!(ev.remote_tip.as_deref(), Some(ahead.as_str()));
    }

    /// CAD-144: a branch value shaped like an option never reaches git
    /// as one — even past `resolve`'s refusal, the fetch passes it only
    /// inside a refspec after `--`.
    #[test]
    fn remote_fetch_never_passes_the_branch_as_an_option() {
        let (tmp, _bare) = repo_with_origin();
        let r = tmp.path();
        let marker = r.join("upload-pack-ran");
        let mut t = target("CAD-1");
        t.root = r.to_path_buf();
        t.branch = format!("--upload-pack=touch {}", marker.display());
        let ev = evidence(&t, true);
        assert!(!marker.exists(), "--upload-pack ran: {:?}", ev.remote_note);
        assert!(!ev.remote_cmp && ev.remote_tip.is_none());
    }

    #[test]
    fn stale_evidence_flags_only_mid_probe_moves() {
        let tmp = repo();
        let r = tmp.path();
        git(r, &["branch", "side"]).unwrap();
        let mut t = target("CAD-1");
        t.root = r.to_path_buf();
        // No remote configured — tracking tip is absent, compared.
        let ev = evidence(&t, false);
        assert!(stale_evidence_reason(r, "side", &ev).is_none());
        // Branch tip moved mid-probe — stale.
        std::fs::write(r.join("f"), "two").unwrap();
        git(r, &["commit", "-qam", "two"]).unwrap();
        git(r, &["branch", "-f", "side", "HEAD"]).unwrap();
        let moved = stale_evidence_reason(r, "side", &ev).unwrap();
        assert!(moved.contains("tip moved"), "{moved}");
        // A tracking-ref move is stale only when remote_cmp says the
        // probe proved it — a fetch that could not prove the remote
        // skips the compare.
        git(r, &["update-ref", "refs/remotes/origin/side", "HEAD"]).unwrap();
        assert!(stale_evidence_reason(r, "side", &ev).is_some());
        // Re-probe fresh (everything matches now), then clear
        // remote_cmp — the same tracking state must not refuse.
        let mut ev_no_cmp = evidence(&t, false);
        assert!(stale_evidence_reason(r, "side", &ev_no_cmp).is_none());
        ev_no_cmp.remote_cmp = false;
        ev_no_cmp.remote_tip = None;
        assert!(
            stale_evidence_reason(r, "side", &ev_no_cmp).is_none(),
            "an unproven remote must not produce a stale-tracking refusal"
        );
    }

    #[test]
    fn stale_probe_flags_only_mid_probe_changes() {
        let probe = target("CAD-1");
        // Identical re-resolve — fresh.
        assert!(stale_probe_reason(&probe, &target("CAD-1")).is_none());
        // Retargeted worktree — stale.
        let mut t = target("CAD-1");
        t.wt_dir = Some(PathBuf::from("/elsewhere"));
        assert!(stale_probe_reason(&probe, &t)
            .unwrap()
            .contains("changed during finish"));
        // Retargeted branch — stale.
        let mut t = target("CAD-1");
        t.branch = "other".to_string();
        assert!(stale_probe_reason(&probe, &t).is_some());
        // A message ref recorded mid-probe — stale.
        let mut t = target("CAD-1");
        t.msg_refs.insert("new-msg".to_string());
        assert!(stale_probe_reason(&probe, &t)
            .unwrap()
            .contains("dispatch was recorded"));
        // A ref that vanished mid-probe is NOT stale — the probe saw
        // strictly more than exists now, so it erred conservative.
        let mut probe2 = target("CAD-1");
        probe2.msg_refs.insert("gone".to_string());
        assert!(stale_probe_reason(&probe2, &target("CAD-1")).is_none());
    }
}
