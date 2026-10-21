//! Daemon-view and process-use probes for `issue finish`.

use super::*;

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

pub(crate) fn probe_rpc(state_dir: &Path, method: &str, params: Value) -> Result<Value> {
    match PROBE_TIMEOUT.with(|t| t.get()) {
        Some(d) => client::rpc_timeout(state_dir, method, params, d),
        None => client::rpc(state_dir, method, params),
    }
}

/// A resolved finish target: the issue's open worktree/branch refs
/// plus the repo root the git probes run against. `msg_refs` are the
/// issue's `message` ref targets — the dispatches recorded against
/// this worktree.
/// The `message` ref targets bound to this worktree: a ref carrying a
/// `worktree` field scopes to that pair only; unscoped refs bind any
/// target; a closed ref (a failed send) binds nothing.
pub(crate) fn bound_msg_refs(
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
pub(crate) struct Block {
    pub(crate) tag: String,
    pub(crate) reason: String,
}

/// Everything the read-only probes learned about a target — shared by
/// `run` (which refuses on the first block) and the `--merged` sweep
/// (which reports them as rows). Survivability is re-derived by the
/// caller via `survivability`/`branch_state`, so it is not carried
/// here.
pub(crate) struct Check {
    pub(crate) blocks: Vec<Block>,
}

/// CAD-275: how long a worktree must sit untouched before `finish`
/// treats it as idle. Every non-ignored file counts, so a fresh
/// checkout is "active" for this long after `issue start`.
pub(crate) const ACTIVE_WINDOW: Duration = Duration::from_secs(30 * 60);

/// The most recently modified non-ignored file under `wt` — tracked
/// or untracked, via `git ls-files` — with its age, when younger than
/// `ACTIVE_WINDOW`. A file dated in the future counts as just now.
pub(crate) fn recent_activity(wt: &Path) -> Result<Option<(String, Duration)>> {
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
pub(crate) fn fmt_age(d: Duration) -> String {
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
pub(crate) enum ProbeKind {
    Unreachable,
    Inconclusive,
}

/// Is this live message bound to the target's worktree? Binding is
/// recorded, never parsed: the issue's `message` refs name the
/// dispatches sent against it, and a job task's `worktree` names the
/// worktree its kickoff runs in. A task the shared enumeration could
/// not inspect — or a `task_show` that fails — is `Unknown`, never a
/// silent "not bound": the caller surfaces it as a block.
pub(crate) enum Bound {
    Yes,
    No,
    Unknown(ProbeKind),
}

pub(crate) fn task_worktree_matches(t: &Target, wt: Option<&str>) -> bool {
    let Some(wt) = wt else {
        return false;
    };
    t.wt_name.as_deref() == Some(wt)
        || t.wt_dir
            .as_deref()
            .is_some_and(|d| d.to_string_lossy() == wt)
}

pub(crate) fn message_bound(view: &DaemonView, state_dir: &Path, msg: &Value, t: &Target) -> Bound {
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
    pub(crate) tasks: std::collections::HashMap<String, Value>,
    /// task ids whose `task_show` failed — a hole in the enumeration
    /// a bound check must not silently treat as absent.
    pub(crate) task_errors: std::collections::HashMap<String, ProbeKind>,
    /// First enumeration failure per kind (the `agent_list` call or a
    /// `task_show`) — reported as the deferred meta-block's reason.
    pub(crate) enum_unreachable: Option<String>,
    pub(crate) enum_inconclusive: Option<String>,
    /// Agent rows from `agent_list` (alias, cwd, and the rest of the
    /// list payload). Empty when the daemon is down or the list failed
    /// — that failure is `enum_unreachable` / `enum_inconclusive`, not
    /// a guess that no cwd is on the worktree.
    pub(crate) agents: Vec<Value>,
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
