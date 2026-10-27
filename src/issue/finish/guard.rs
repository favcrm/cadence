//! In-use guards for `issue finish`.

use super::*;

/// Which agent the daemon reports holding `alias` — folded into the
/// shared probe-error rules.
pub(crate) enum AgentLookup {
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
pub(crate) enum ProbeError {
    Absent,
    Inconclusive(Error),
    Unreachable(Error),
}

/// Classify a daemon rpc error: `Rejected` is an answer, but only the
/// absence the caller asked about counts as `Absent` — any other
/// rejection is inconclusive rather than silently "not there".
/// `Internal` is a transport failure.
pub(crate) fn classify_probe_error(e: Error, absent_marker: &str) -> ProbeError {
    match e {
        e @ Error::Internal(_) => ProbeError::Unreachable(e),
        Error::Rejected(m) if m.contains(absent_marker) => ProbeError::Absent,
        e => ProbeError::Inconclusive(e),
    }
}

pub(crate) fn agent_lookup(state_dir: &Path, alias: &str) -> AgentLookup {
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
pub(crate) fn push_once(blocks: &mut Vec<Block>, seen: &mut bool, tag: &str, reason: String) {
    if !*seen {
        *seen = true;
        blocks.push(Block {
            tag: tag.to_string(),
            reason,
        });
    }
}

pub(crate) fn push_alias(aliases: &mut Vec<String>, a: &str) {
    if !a.is_empty() && !aliases.iter().any(|x| x == a) {
        aliases.push(a.to_string());
    }
}

/// Whether `cwd` is the worktree directory or a child of it. The
/// worktree path is canonical when that directory exists and lexical
/// otherwise. A sibling whose name only shares a prefix does not match.
pub(crate) fn cwd_on_worktree(wt_dir: &Path, cwd: &Path) -> bool {
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
pub(crate) fn inspect(view: &DaemonView, state_dir: &Path, t: &Target, ev: &Evidence) -> Check {
    let (blocks, deferred) = in_use_blocks(view, state_dir, t);
    inspect_rest(blocks, deferred, t, ev)
}

/// The in-use half of [`inspect`]: live or unknown messages, task
/// bindings, pane trees and processes bound to this worktree. Returns
/// the blocks and the deferred enumeration (meta) failures.
pub(crate) fn in_use_blocks(
    view: &DaemonView,
    state_dir: &Path,
    t: &Target,
) -> (Vec<Block>, Vec<Block>) {
    in_use_blocks_with_process_probe(view, state_dir, t, &process_use_under)
}

pub(crate) fn in_use_blocks_with_process_probe(
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

pub(crate) fn inspect_rest(
    mut blocks: Vec<Block>,
    deferred: Vec<Block>,
    t: &Target,
    ev: &Evidence,
) -> Check {
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
