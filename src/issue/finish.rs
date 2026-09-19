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

/// Why `branch` counts as merged into `into`, checked in order:
/// plain ancestry; every branch commit already applied upstream
/// (`git cherry` — the rebase/cherry-pick case); the branch's whole
/// diff reverse-applying onto `into` (a squash merge); or a merged
/// GitHub PR whose recorded head covers `tip`. The first match wins
/// and is reported as `merged_by`.
fn merge_rule(root: &Path, branch: &str, tip: &str, into: &str) -> Option<&'static str> {
    if ancestor(root, branch, into) {
        return Some("ancestry");
    }
    // `+` = a commit with no patch-equivalent upstream; a listing
    // with none means everything on the branch already landed.
    if let Ok(marks) = git(root, &["cherry", into, branch]) {
        if !marks.lines().any(|l| l.starts_with('+')) {
            return Some("cherry");
        }
    }
    if patch_applied(root, branch, into) {
        return Some("patch");
    }
    if pr_merged(root, branch, tip) {
        return Some("pr");
    }
    None
}

/// Squash-merge test: if the branch's combined diff against its
/// merge-base reverse-applies cleanly onto `into`'s tree, `into`
/// already holds that state. Runs against a temporary index — no
/// worktree is ever touched.
fn patch_applied(root: &Path, branch: &str, into: &str) -> bool {
    let Ok(base) = git(root, &["merge-base", into, branch]) else {
        return false;
    };
    // Raw stdout bytes, not `git()`'s trimmed string — `git apply`
    // rejects a patch whose final newline was stripped as corrupt.
    let diff = match git_out(root, &["diff", "--binary", &base, branch], &[]) {
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
/// `gh` needs a GitHub origin and a successful answer; any failure
/// falls through: gh is a hint, never the only authority.
fn pr_merged(root: &Path, branch: &str, tip: &str) -> bool {
    let Ok(url) = git(root, &["remote", "get-url", "origin"]) else {
        return false;
    };
    if !url.contains("github.com") {
        return false;
    }
    let mut cmd = Command::new("gh");
    cmd.args([
        "pr",
        "list",
        "--head",
        branch,
        "--state",
        "merged",
        "--json",
        "number,headRefOid",
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
    let msg_refs = front
        .refs
        .iter()
        .filter(|r| r.kind == "message")
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

/// Is this live message bound to the target's worktree? Binding is
/// recorded, never parsed: the issue's `message` refs name the
/// dispatches sent against it, and a job task's `worktree` names the
/// worktree its kickoff runs in. A `task_show` that fails proves
/// nothing — the pane/proc scans still catch real use.
fn message_bound(state_dir: &Path, msg: &Value, t: &Target) -> bool {
    let id = msg["id"].as_str().unwrap_or_default();
    if t.msg_refs.contains(id) {
        return true;
    }
    // A branch-only target has no worktree a task could be bound to —
    // skip the rpc.
    if t.wt_name.is_none() && t.wt_dir.is_none() {
        return false;
    }
    let Some(task) = msg["task_id"].as_str() else {
        return false;
    };
    let Ok(show) = client::rpc(state_dir, "task_show", json!({"task": task})) else {
        return false;
    };
    let Some(wt) = show["task"]["worktree"].as_str() else {
        return false;
    };
    t.wt_name.as_deref() == Some(wt)
        || t.wt_dir
            .as_deref()
            .is_some_and(|d| d.to_string_lossy() == wt)
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

fn branch_state(t: &Target) -> Branch {
    if t.branch.is_empty() {
        return Branch::Gone;
    }
    let Ok(tip) = git(
        &t.root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{}", t.branch),
        ],
    ) else {
        return Branch::Gone;
    };
    match default_ref(&t.root).and_then(|d| merge_rule(&t.root, &t.branch, &tip, &d)) {
        Some(how) => Branch::Merged { how, tip },
        None => Branch::Open { tip },
    }
}

/// Which agent the daemon reports holding `alias` — folded into the
/// shared probe-error rules.
enum AgentLookup {
    Shown(Value),
    /// The daemon answered that the alias is unknown or absent — the
    /// agent provably holds nothing in flight.
    Absent,
    /// A daemon-answered error that is neither a clean absence nor a
    /// transport failure (Provider, OutcomeUnknown) — the check could
    /// not be made.
    Inconclusive(Error),
    /// The daemon could not be reached at all.
    Unreachable(Error),
}

fn agent_lookup(state_dir: &Path, alias: &str) -> AgentLookup {
    match client::rpc(state_dir, "agent_show", json!({"alias": alias})) {
        Ok(show) => AgentLookup::Shown(show),
        Err(e @ Error::Internal(_)) => AgentLookup::Unreachable(e),
        Err(Error::Rejected(_)) => AgentLookup::Absent,
        Err(e) => AgentLookup::Inconclusive(e),
    }
}

/// Survivability for `t`'s branch: `merged_by` names the merge
/// evidence; `tip` is the commit a `branch -D` may delete — `Some`
/// only when evidence (merge rule or a pushed remote ref) covers it.
/// A tip with uncovered commits yields the `unmerged-unpushed` block
/// and NO deletable tip — the branch is never deleted.
fn survivability(t: &Target) -> (Option<&'static str>, Option<String>, Option<Block>) {
    match branch_state(t) {
        Branch::Merged { how, tip } => (Some(how), Some(tip), None),
        Branch::Open { tip } => {
            let remote_ref = format!("refs/remotes/origin/{}", t.branch);
            let pushed = git(&t.root, &["rev-parse", "--verify", "--quiet", &remote_ref]).is_ok()
                && ancestor(&t.root, &tip, &format!("origin/{}", t.branch));
            if pushed {
                // Every commit on the branch is on the remote —
                // deleting the local ref loses nothing.
                (None, Some(tip), None)
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

/// One `daemon-unreachable` block no matter how many lookups fail.
fn push_unreachable(blocks: &mut Vec<Block>, unreachable: &mut bool, reason: String) {
    if !*unreachable {
        *unreachable = true;
        blocks.push(Block {
            tag: "daemon-unreachable".to_string(),
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
/// checks, evaluated read-only. In-use means THIS worktree: a live
/// message recorded against it, a pane tree with cwd inside it, or
/// any process standing in it. An owner busy elsewhere does not
/// block. Only a daemon TRANSPORT failure blocks the checks — a
/// `Rejected` answer (unknown or absent agent) is the daemon proving
/// that agent holds nothing, while a Provider/OutcomeUnknown answer
/// is inconclusive and blocks with its own wording. The /proc scans
/// run regardless.
fn inspect(state_dir: &Path, t: &Target) -> Check {
    let mut blocks = Vec::new();
    let mut pane_pid = None;
    let mut unreachable = false;
    // Every agent that could hold a message bound to this worktree:
    // the owner (whose pane tree also gets the cwd scan), each
    // recipient an in-scope message ref names (`dispatch → <alias>` —
    // a re-assigned issue leaves the earlier dispatchee's bound
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
        if let Some(a) = r
            .label
            .as_deref()
            .and_then(|l| l.strip_prefix("dispatch → "))
        {
            push_alias(&mut aliases, a);
        }
    }
    // Task-bound coverage: a task kickoff lives on its assignee even
    // when no issue ref recorded it. Enumeration is best-effort — a
    // daemon that cannot answer holds no live messages we could miss,
    // and the owner/recipient checks above already surface its health.
    if let Ok(list) = client::rpc(state_dir, "agent_list", json!({})) {
        for a in list["agents"].as_array().into_iter().flatten() {
            for tid in a["tasks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                let Ok(ts) = client::rpc(state_dir, "task_show", json!({"task": tid})) else {
                    continue;
                };
                let wt = ts["task"]["worktree"].as_str().unwrap_or_default();
                let bound = t.wt_name.as_deref() == Some(wt)
                    || t.wt_dir
                        .as_deref()
                        .is_some_and(|d| d.to_string_lossy() == wt);
                if bound {
                    if let Some(asg) = ts["task"]["assignee"].as_str() {
                        push_alias(&mut aliases, asg);
                    }
                }
            }
        }
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
                if let Some(msg) = show["messages"].as_array().and_then(|ms| {
                    ms.iter().find(|m| {
                        let live = match m["state"].as_str().unwrap_or_default() {
                            "running" | "submitting" => true,
                            "queued" => !dead && !inbox,
                            _ => false,
                        };
                        live && message_bound(state_dir, m, t)
                    })
                }) {
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
            AgentLookup::Unreachable(e) => push_unreachable(
                &mut blocks,
                &mut unreachable,
                format!(
                    "Daemon unreachable while checking '{alias}' ({e}) — \
                     the agent checks cannot run; rerun when the daemon is \
                     up or pass --force"
                ),
            ),
            AgentLookup::Inconclusive(e) => blocks.push(Block {
                tag: "agent-check-inconclusive".to_string(),
                reason: format!(
                    "Cannot check agent '{alias}' ({e}) — the daemon \
                     answered but the result is inconclusive; refusing \
                     to guess, pass --force"
                ),
            }),
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
    if let (_, _, Some(b)) = survivability(t) {
        blocks.push(b);
    }
    Check { blocks }
}

/// `issue finish <ID> [--force] [--keep-branch] [--remote]` — JSON like
/// the other issue verbs. Runs in two phases so the pm lock is never
/// held across a daemon RPC: the resolve + probe (agent shows, task
/// lookups, /proc scans, git and gh evidence) happen unlocked, then
/// the lock covers only the commit phase — a fresh resolve for
/// staleness, the branch evidence re-verified against the CURRENT
/// tip, the removals, and the tracker write.
pub fn run(
    pm: &Pm,
    id: &str,
    force: bool,
    keep_branch: bool,
    remote: bool,
    actor: &str,
    state_dir: &Path,
) -> Result<Value> {
    // Phase 1 — the unlocked probe. Nothing here holds the pm lock.
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
    // The per-worktree guard + dirty + survivability, evaluated once.
    // `--force` records every block it bypasses; without it the first
    // block refuses, naming its pid or message id.
    let check = inspect(state_dir, &probe);
    let mut overridden = Vec::new();
    for b in check.blocks {
        if force {
            overridden.push(b.tag);
        } else {
            return Err(Error::rejected(b.reason));
        }
    }

    // Phase 2 — the commit phase, under the pm lock. Re-resolve so a
    // tracker write that landed mid-probe is seen, re-verify the
    // branch evidence against the tip that exists NOW, then delete.
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
    if t.wt_dir != probe.wt_dir || t.branch != probe.branch {
        return Err(Error::rejected(format!(
            "{id}'s worktree/branch refs changed during finish — retry"
        )));
    }
    if !t.msg_refs.is_subset(&probe.msg_refs) {
        return Err(Error::rejected(
            "A dispatch was recorded during finish — retry".to_string(),
        ));
    }
    // Re-verify survivability against the current tip: a commit that
    // landed mid-probe could uncover work the probe counted as
    // covered — or cover work it refused.
    let (merged_how, tip, block) = survivability(&t);
    if let Some(b) = block {
        if force {
            overridden.push(b.tag);
        } else {
            return Err(Error::rejected(b.reason));
        }
    }
    let merged_by = merged_how.map_or(Value::Null, |h| json!(h));
    let dir = t.dir.clone();
    let wt_dir = t.wt_dir.clone();
    let wt_name = t.wt_name.clone();
    let branch = t.branch.clone();
    let root = t.root.clone();
    let cargo_target = t.cargo_target.clone();

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
        match tip {
            Some(tip) => {
                let now = git(
                    &root,
                    &[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("refs/heads/{branch}"),
                    ],
                )
                .ok();
                if now.as_deref() == Some(tip.as_str()) {
                    deleted_branch = git(&root, &["branch", "-D", &branch]).is_ok();
                } else {
                    branch_note = json!(
                        "branch tip moved during finish — kept (its evidence \
                         covered {tip})"
                    );
                }
            }
            None => {
                // tip=None: either the branch is already gone (nothing
                // to say) or its tip has commits no evidence covers —
                // never -D, even under --force.
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
        // never refused (the point of the verb).
        match branch_state(&t) {
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
            Branch::Merged { how, .. } => {
                row["merged_by"] = json!(how);
                if dry_run {
                    match inspect(state_dir, &t).blocks.first() {
                        Some(b) => {
                            row["outcome"] = json!("refused");
                            row["reason"] = json!(b.reason);
                            refused += 1;
                        }
                        None => row["outcome"] = json!("would-finish"),
                    }
                } else {
                    match run(pm, &id, false, false, remote, actor, state_dir) {
                        Ok(out) => {
                            row["outcome"] = json!("finished");
                            row["removed_worktree"] = out["removed_worktree"].clone();
                            row["deleted_branch"] = out["deleted_branch"].clone();
                            row["branch_note"] = out["branch_note"].clone();
                            row["remote_deleted"] = out["remote_deleted"].clone();
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
