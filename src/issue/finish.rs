//! `cadence issue finish <ID>` — remove an issue's recorded worktree
//! and branch once the work has landed. The guard is per worktree,
//! not per agent: it refuses only while the worktree is actually in
//! use — a live message whose dispatch recorded this worktree (the
//! issue's `message` refs, or a job task's recorded worktree), a pane
//! process tree with cwd inside it, or any process standing in it —
//! plus a dirty worktree (ignored paths don't count) and a branch
//! whose work survives nowhere — merged into the repo's default
//! branch by ancestry, patch-equivalent commits, a squash merge, or
//! a merged PR, or pushed. A busy owner working ELSEWHERE is not a
//! reason. `--force` overrides each and is recorded as a `Forced:`
//! trailer on the finish commit. Refs are kept as history, marked
//! `closed: true`; the issue's status is untouched — status follows
//! the job or the PM. `issue finish --merged` sweeps every open
//! worktree ref whose branch is merged and whose guard passes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use crate::adapter::pty;
use crate::client;
use crate::error::{Error, Result};
use crate::issue::{board, model::Front, project, write, Pm};
use crate::proc::{run_bounded, BoundedError};

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
fn merge_rule(root: &Path, pr_head: &str, tip: &str, into: &str) -> Option<&'static str> {
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

/// The three ref states an issue can be in for finishing.
enum Resolve {
    /// No worktree/branch refs at all — `finish` errors.
    Nothing,
    /// Refs exist but every worktree/branch ref is closed.
    Finished,
    /// An open worktree and/or branch ref to clean up.
    Target(Box<Target>),
}

/// Load the issue and resolve its open worktree/branch refs + repo
/// root — shared by `run` and the `--merged` sweep. The first OPEN
/// worktree ref pairs with the open branch ref of the SAME name — a
/// re-start under `--name` leaves older pairs behind, and
/// first-of-kind matching could fuse halves of different pairs. A
/// lone open branch ref (hand-edited history) is still finishable on
/// its own.
fn resolve(pm: &Pm, id: &str) -> Result<Resolve> {
    let (_project, dir) = write::issue_dir(pm, id)?;
    let (front, body) = write::load_front(&dir)?;
    let open_wt_ref = front
        .refs
        .iter()
        .find(|r| r.kind == "worktree" && r.closed != Some(true));
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
            return Ok(Resolve::Finished);
        }
        return Ok(Resolve::Nothing);
    }
    // The repo root: through the live worktree when it exists, else
    // any recorded `<root>/.cadence/wt/<name>` path walked upward
    // (closed refs still name the repo).
    let wt_dir = open_wt;
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
    // Message refs bind the worktree they were dispatched against: a
    // ref carrying a `worktree` field scopes to that pair only, so a
    // re-start under `--name` doesn't inherit the earlier kickoff's
    // binding. Unscoped refs (hand-added, pre-CAD-94) bind any target.
    // A closed ref — a dispatch whose send failed — bound nothing and
    // counts for no check.
    let msg_refs = front
        .refs
        .iter()
        .filter(|r| r.kind == "message" && r.closed != Some(true))
        .filter(|r| match r.worktree.as_deref() {
            Some(w) => {
                wt_name.as_deref() == Some(w)
                    || wt_dir.as_deref().is_some_and(|d| d.to_string_lossy() == w)
            }
            None => true,
        })
        .filter_map(|r| r.path.clone())
        .collect();
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

/// Pids whose `/proc/<pid>/cwd` resolves under `dir` — the "open
/// shell in the worktree" check. /proc races are fine: a vanished pid
/// or a denied read just doesn't report. The cadence process itself
/// is excluded; a parent shell standing in the worktree is not — that
/// is exactly the open-shell case.
fn pids_cwd_under(dir: &Path) -> Vec<u32> {
    let mut out = Vec::new();
    let Ok(dir) = dir.canonicalize() else {
        return out;
    };
    let me = std::process::id();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in procs.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Ok(cwd) = std::fs::read_link(format!("/proc/{pid}/cwd")) else {
            continue;
        };
        if cwd.starts_with(&dir) {
            out.push(pid);
        }
    }
    out
}

/// Best-effort process name for a refusal message.
fn comm_of(pid: u32) -> String {
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
    match client::rpc(state_dir, "task_show", json!({"task": task})) {
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
    up: bool,
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
}

fn daemon_view(state_dir: &Path) -> DaemonView {
    let mut v = DaemonView {
        up: client::socket_path(state_dir).exists(),
        tasks: std::collections::HashMap::new(),
        task_errors: std::collections::HashMap::new(),
        enum_unreachable: None,
        enum_inconclusive: None,
    };
    if !v.up {
        return v;
    }
    match client::rpc(state_dir, "agent_list", json!({})) {
        Ok(list) => {
            for a in list["agents"].as_array().into_iter().flatten() {
                for tid in a["tasks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    match client::rpc(state_dir, "task_show", json!({"task": tid})) {
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
fn branch_tip(root: &Path, branch: &str) -> Option<String> {
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
        match git_out(&t.root, &["fetch", "origin", &t.branch], &[]) {
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
            Branch::Merged { tip, .. } | Branch::Open { tip } => Some(tip.clone()),
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
    match client::rpc(state_dir, "agent_show", json!({"alias": alias})) {
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

/// The per-worktree guard plus the unchanged dirty/survivability
/// checks, evaluated read-only against a shared daemon `view` and
/// merge `ev`idence. In-use means THIS worktree: a live message
/// recorded against it, a pane tree with cwd inside it, or any
/// process standing in it. An owner busy elsewhere does not block.
/// Agent checks run only when the daemon socket exists (`view.up`):
/// a stopped daemon means "no agents" and the /proc and pane scans
/// carry the check. When it answers, only a TRANSPORT failure blocks
/// — a `Rejected` answer (unknown or absent agent) is the daemon
/// proving that agent holds nothing, while a Provider/OutcomeUnknown
/// answer is inconclusive and blocks with its own wording. The /proc
/// scans run regardless.
fn inspect(view: &DaemonView, state_dir: &Path, t: &Target, ev: &Evidence) -> Check {
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
                    let mut bound_msg = None;
                    for m in show["messages"].as_array().into_iter().flatten() {
                        let live = match m["state"].as_str().unwrap_or_default() {
                            "running" | "submitting" => true,
                            "queued" => !dead && !inbox,
                            _ => false,
                        };
                        if !live {
                            continue;
                        }
                        match message_bound(view, state_dir, m, t) {
                            Bound::Yes => {
                                bound_msg = Some(m);
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
                    if let Some(msg) = bound_msg {
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
    // Any process standing in the worktree blocks it — an owner pane's
    // descendants are named as such, everything else as a plain pid.
    if let Some(d) = t.wt_dir.as_deref().filter(|d| d.is_dir()) {
        let mut pane_hit = None;
        let mut proc_hit = None;
        for pid in pids_cwd_under(d) {
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
    }
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

/// `issue finish <ID> [--force] [--keep-branch] [--remote]` — JSON like
/// the other issue verbs. Runs in two phases so the pm lock is never
/// held across a daemon RPC, a `fetch` or a `gh` call: the resolve +
/// probe (daemon view, /proc scans, git and gh evidence, the remote
/// fetch) happen unlocked, then the lock covers only the commit
/// phase — a fresh resolve for staleness, LOCAL `rev-parse`s proving
/// the evidence's inputs did not move, the removals, and the tracker
/// write. `shared` lets a sweep reuse one enumeration for every
/// issue instead of re-issuing `agent_list` + `task_show`×N per row;
/// a binding added mid-sweep still lands as a stale probe.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run(
    pm: &Pm,
    id: &str,
    force: bool,
    keep_branch: bool,
    remote: bool,
    actor: &str,
    state_dir: &Path,
    shared: Option<&DaemonView>,
) -> Result<Value> {
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
    let probe = match resolve(pm, id)? {
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
    let mut t = match resolve(pm, id)? {
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

    // Removal: the worktree first (frees the branch), then the branch
    // — and only the exact tip the evidence covered: a tip that moved
    // (or was never covered) keeps its commits, so an uncovered branch
    // is never `branch -D`'d even under --force.
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
    let mut branch_note = Value::Null;
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
    // One tracker commit marks both refs closed — kept as history.
    for r in &mut t.front.refs {
        if (r.kind == "worktree"
            && wt_dir
                .as_deref()
                .is_some_and(|d| r.path.as_deref() == Some(d.to_string_lossy().as_ref())))
            || (r.kind == "branch" && !branch.is_empty() && r.path.as_deref() == Some(&branch))
        {
            r.closed = Some(true);
        }
    }
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
    pm.commit(&format!("{subject}\n\n{trailers}"))?;
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
        "status": t.front.status,
    });
    if let Some(target) = &cargo_target {
        // `symlink_metadata` — a recorded path that is itself a
        // symlink reports its own presence, not its target's.
        out["cargo_target_exists"] = json!(Path::new(target).symlink_metadata().is_ok());
    }
    Ok(out)
}

/// `issue finish --merged [--project P] [--remote] [--dry-run]` —
/// sweep every open worktree ref in scope whose branch is merged into
/// the repo's default branch and whose per-worktree guard passes.
/// One row per worktree: `finished` (or `would-finish` under
/// `--dry-run`), `skipped(<reason>)` when it is not a merged
/// candidate, `refused(<reason>)` when a guard blocks. The sweep never
/// forces. `refused` counts the refused rows — the CLI maps nonzero
/// to exit 1.
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
        let mut row = json!({
            "issue": issue.front.id,
            "worktree": issue.front.refs.iter()
                .find(|r| r.kind == "worktree" && r.closed != Some(true))
                .and_then(|r| r.path.clone()),
            "branch": Value::Null,
            "merged_by": Value::Null,
            "outcome": Value::Null,
            "reason": Value::Null,
        });
        let id = issue.front.id.clone();
        // Only open WORKTREE refs are sweep candidates — a lone open
        // branch ref is finished by name, not swept.
        let t = match resolve(pm, &id) {
            Ok(Resolve::Target(t)) if t.wt_dir.is_some() => t,
            Ok(_) => continue,
            Err(e) => {
                row["outcome"] = json!("refused");
                row["reason"] = json!(e.to_string());
                refused += 1;
                rows.push(row);
                continue;
            }
        };
        row["branch"] = json!(t.branch);
        // Candidates are merged branches — unmerged work is skipped,
        // never refused (the point of the verb). The sweep's own
        // evidence never fetches: a real row's `run` fetches for its
        // own gate at finish time.
        let ev = evidence(&t, false);
        match &ev.state {
            Branch::Gone => {
                row["outcome"] = json!("skipped");
                row["reason"] = json!(if t.branch.is_empty() {
                    "no branch ref"
                } else {
                    "branch missing"
                });
            }
            Branch::Open { .. } => {
                row["outcome"] = json!("skipped");
                row["reason"] = json!("unmerged");
            }
            Branch::Merged { ref how, .. } => {
                row["merged_by"] = json!(how);
                if dry_run {
                    match inspect(&view, state_dir, &t, &ev).blocks.first() {
                        Some(b) => {
                            row["outcome"] = json!("refused");
                            row["reason"] = json!(b.reason);
                            refused += 1;
                        }
                        None => row["outcome"] = json!("would-finish"),
                    }
                } else {
                    match run(pm, &id, false, false, remote, actor, state_dir, Some(&view)) {
                        Ok(out) => {
                            row["outcome"] = json!("finished");
                            row["removed_worktree"] = out["removed_worktree"].clone();
                            row["deleted_branch"] = out["deleted_branch"].clone();
                            row["branch_note"] = out["branch_note"].clone();
                            row["remote_deleted"] = out["remote_deleted"].clone();
                            row["remote_note"] = out["remote_note"].clone();
                        }
                        Err(e) => {
                            row["outcome"] = json!("refused");
                            row["reason"] = json!(e.to_string());
                            refused += 1;
                        }
                    }
                }
            }
        }
        rows.push(row);
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
