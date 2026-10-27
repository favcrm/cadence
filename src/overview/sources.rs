//! Bounded overview source acquisition, including borrowed board reuse.
//! Row assembly and its contextual reads live separately in assembly.rs.

use super::github_cache::github_throttled;
use super::{degraded, github_repos_cached, monitoring, now_epoch, Options};
use crate::adapter::registry;
use crate::client;
use crate::issue::{self, board, project};
use serde_json::{json, Value};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Concurrent per-agent probes.
const PROBE_WORKERS: usize = 8;

/// What one agent's probes returned. A mailbox (no actor) needs none —
/// its `agent_list` row carries the backlog and health.
#[derive(Clone, Default)]
pub(super) struct AgentProbe {
    pub(super) show: Option<Value>,
    pub(super) requests: Vec<Value>,
    /// A running/submitted turn, a busy pane, or a probe that could not
    /// tell — any of them holds the drift restart back.
    pub(super) holds_drift: bool,
    /// The daemon predates `agent_probe`.
    pub(super) probe_unknown: bool,
    /// The probe budget ran out before this agent was reached.
    pub(super) unprobed: bool,
    pub(super) degraded: Vec<Value>,
}

/// One daemon RPC under the overview's bounds: the per-call timeout,
/// clipped to what is left of the pass's deadline.
fn bounded_rpc(
    state_dir: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
    deadline: Instant,
) -> Result<Value, String> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left < Duration::from_millis(50) {
        return Err("overview probe budget spent".to_string());
    }
    let bound = left.min(timeout);
    client::rpc_timeout(state_dir, method, params, bound).map_err(|e| {
        let text = e.to_string();
        // A read timeout surfaces as EAGAIN/"timed out" — name the bound.
        if text.contains("os error 11") || text.contains("timed out") {
            format!("no answer within {}ms", bound.as_millis())
        } else {
            text
        }
    })
}

/// Show, pending requests and (for an idle pty pane) the pane probe of
/// one agent, each bounded.
pub(super) fn probe_agent(
    state_dir: &Path,
    a: &Value,
    timeout: Duration,
    deadline: Instant,
) -> AgentProbe {
    let alias = a["alias"].as_str().unwrap_or_default();
    let provider = a["provider"].as_str().unwrap_or_default();
    let kind = a["endpoint_kind"].as_str().unwrap_or_default();
    let mut p = AgentProbe::default();
    // A mailbox has no pane and no requests: its `agent_list` row
    // carries the backlog. Only rows that predate the `inbox` block
    // (an older daemon) need the show read for the queued count.
    let mailbox = !registry::has_actor(provider, kind);
    if mailbox && a["inbox"]["queued"].is_i64() {
        return p;
    }
    if deadline.saturating_duration_since(Instant::now()) < Duration::from_millis(50) {
        p.unprobed = true;
        // A mailbox never holds a restart back.
        p.holds_drift = !mailbox;
        return p;
    }
    let rpc = |method: &str| {
        let params = if method == "agent_show" {
            json!({"alias": alias, "active_only": true})
        } else {
            json!({"alias": alias})
        };
        bounded_rpc(state_dir, method, params, timeout, deadline)
    };
    match rpc("agent_show") {
        Ok(v) => p.show = Some(v),
        Err(e) => p.degraded.push(degraded("agent_show", alias, e)),
    }
    if mailbox {
        return p;
    }
    match rpc("agent_requests") {
        Ok(v) => p.requests = v["requests"].as_array().cloned().unwrap_or_default(),
        Err(e) if e.contains("Unknown method") => {}
        Err(e) => p.degraded.push(degraded("agent_requests", alias, e)),
    }
    // Drift is only actionable when everything is idle: any running or
    // submitted message, or any busy pty pane, holds it back — and so
    // does a show that never answered.
    let busy_turn = p.show.as_ref().is_none_or(|show| {
        show["messages"].as_array().is_some_and(|ms| {
            ms.iter().any(|m| {
                matches!(
                    m["state"].as_str().unwrap_or_default(),
                    "running" | "submitted"
                )
            })
        })
    });
    if busy_turn {
        p.holds_drift = true;
    } else if kind == "pty" && a["endpoint"].is_string() {
        match rpc("agent_probe") {
            Ok(v) if v["idle"].as_bool().unwrap_or(false) => {}
            Ok(_) => p.holds_drift = true,
            // "Unknown method" — a daemon that predates the RPC; any
            // other failure reads as busy, conservatively.
            Err(e) if e.contains("Unknown method") => p.probe_unknown = true,
            Err(e) => {
                p.holds_drift = true;
                p.degraded.push(degraded("agent_probe", alias, e));
            }
        }
    }
    p
}

/// Probe every agent on [`PROBE_WORKERS`] threads — the daemon answers
/// each connection on its own thread, so the pass costs the slowest
/// probes, not their sum. No probe starts past `opts.probe_budget`.
fn probe_agents(state_dir: &Path, agents: &[Value], opts: &Options) -> Vec<AgentProbe> {
    let deadline = Instant::now() + opts.probe_budget;
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<AgentProbe>>> = agents.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..PROBE_WORKERS.min(agents.len()) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(a) = agents.get(i) else {
                    break;
                };
                let p = probe_agent(state_dir, a, opts.probe_timeout, deadline);
                *slots[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
            });
        }
    });
    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .unwrap_or_else(|e| e.into_inner())
                .unwrap_or_default()
        })
        .collect()
}

/// The daemon's half of the screen: reachability, build identity, the
/// agent rows and their probes.
#[derive(Clone)]
pub(super) struct DaemonView {
    pub(super) reachable: bool,
    pub(super) info: Option<Value>,
    pub(super) agents: Vec<Value>,
    pub(super) probes: Vec<AgentProbe>,
    pub(super) degraded: Vec<Value>,
}

fn daemon_view(state_dir: &Path, opts: &Options) -> DaemonView {
    let t = opts.probe_timeout;
    let mut view = DaemonView {
        // Reachability comes from `health` — a pre-daemon_info daemon
        // answers it, so an old build never reads as "unreachable"
        // while `agent list` works. `daemon_info` only carries the id.
        reachable: client::rpc_timeout(state_dir, "health", json!({}), t).is_ok(),
        info: None,
        agents: Vec::new(),
        probes: Vec::new(),
        degraded: Vec::new(),
    };
    if !view.reachable {
        return view;
    }
    view.info = client::rpc_timeout(state_dir, "daemon_info", json!({}), t).ok();
    match client::rpc_timeout(state_dir, "agent_list", json!({}), t) {
        Ok(v) => view.agents = v["agents"].as_array().cloned().unwrap_or_default(),
        Err(e) => view.degraded.push(degraded(
            "agent_list",
            "",
            format!("agent rows missing — {e}"),
        )),
    }
    view.probes = probe_agents(state_dir, &view.agents, opts);
    let unprobed: Vec<&str> = view
        .agents
        .iter()
        .zip(&view.probes)
        .filter(|(_, p)| p.unprobed)
        .filter_map(|(a, _)| a["alias"].as_str())
        .collect();
    if !unprobed.is_empty() {
        view.degraded.push(degraded(
            "agent_probe_budget",
            "",
            format!(
                "{} agent(s) not probed within {:.1}s: {}",
                unprobed.len(),
                opts.probe_budget.as_secs_f64(),
                unprobed.join(", ")
            ),
        ));
    }
    for p in &mut view.probes {
        view.degraded.append(&mut p.degraded);
    }
    view
}

/// The daemon-side sources of one build — the agent probes and the
/// monitoring block. The board's read model (CAD-325) keeps the last
/// pass and rebuilds the screen from it when only the tracker moved.
#[derive(Clone)]
pub struct DaemonSources {
    pub(super) daemon: DaemonView,
    pub(super) monitoring: Value,
}

/// Probe the daemon and read the monitors, concurrently.
pub fn daemon_sources(state_dir: &Path, opts: &Options) -> DaemonSources {
    std::thread::scope(|s| {
        let daemon = s.spawn(|| daemon_view(state_dir, opts));
        let monitoring = s.spawn(|| monitoring(state_dir));
        DaemonSources {
            daemon: daemon.join().expect("overview daemon probe panicked"),
            monitoring: monitoring
                .join()
                .expect("overview monitoring read panicked"),
        }
    })
}

/// What a long-lived caller hands a build instead of re-reading it: the
/// indexed tracker's views and a recent daemon pass. Status and claim
/// times need no hand-off — [`crate::issue::line_times::LineTimes`] is cached per tracker HEAD
/// (CAD-403).
pub struct Reuse<'a> {
    pub views: &'a [board::View],
    pub sources: &'a DaemonSources,
}

/// Inputs collected once for one build. Reused daemon/tracker results stay borrowed.
pub(super) struct Snapshot<'a> {
    pub(super) now: i64,
    pub(super) pm: Option<issue::Pm>,
    pub(super) projects: Vec<project::Project>,
    pub(super) repo_paths: Vec<(PathBuf, String)>,
    pub(super) slug_project: HashMap<String, String>,
    pub(super) slug_clone: HashMap<String, PathBuf>,
    pub(super) has_github: bool,
    pub(super) sources: Cow<'a, DaemonSources>,
    pub(super) views: Option<Cow<'a, [board::View]>>,
    pub(super) gh_repos: HashMap<String, Value>,
    pub(super) gh_state: Value,
}

pub(super) fn collect<'a>(
    state_dir: &Path,
    pm_dir: &Path,
    opts: &Options,
    reuse: Option<Reuse<'a>>,
) -> Result<Snapshot<'a>, String> {
    let now = now_epoch();
    let pm = issue::Pm::at(pm_dir).ok();
    let projects = pm
        .as_ref()
        .map(|pm| project::list(&pm.dir).unwrap_or_default())
        .unwrap_or_default();
    if let Some(key) = opts.scope.project.as_deref() {
        if !projects.iter().any(|p| p.key == key) {
            let known: Vec<&str> = projects.iter().map(|p| p.key.as_str()).collect();
            return Err(if known.is_empty() {
                format!(
                    "unknown project '{key}' — no tracker projects under {}",
                    pm_dir.display()
                )
            } else {
                format!("unknown project '{key}' — known: {}", known.join(", "))
            });
        }
    }
    let mut slugs = Vec::new();
    let mut slug_project: HashMap<String, String> = HashMap::new();
    let mut repo_paths: Vec<(PathBuf, String)> = Vec::new();
    // Slug → the declared local clone its first-parent log comes from.
    let mut slug_clone: HashMap<String, PathBuf> = HashMap::new();
    for p in &projects {
        for r in &p.repos {
            let path = r.path.as_deref().map(|path| {
                let path = project::expand_home(path);
                path.canonicalize().unwrap_or(path)
            });
            if let Some(remote) = &r.remote {
                let norm = project::normalize_remote(remote);
                if let Some(slug) = norm.strip_prefix("github.com/") {
                    slug_project.insert(slug.to_string(), p.key.clone());
                    slugs.push(slug.to_string());
                    if let Some(path) = &path {
                        slug_clone
                            .entry(slug.to_string())
                            .or_insert_with(|| path.clone());
                    }
                }
            }
            if let Some(path) = path {
                repo_paths.push((path, p.key.clone()));
            }
        }
    }
    slugs.sort();
    slugs.dedup();

    // ---- the three sources, concurrently (what `reuse` lacks) ----
    let reused = reuse.as_ref();
    let (fresh_sources, (gh_repos, gh_state), fresh_views) = std::thread::scope(|s| {
        let sources = reused
            .is_none()
            .then(|| s.spawn(|| daemon_sources(state_dir, opts)));
        let gh = s.spawn(|| {
            if opts.cache_only {
                github_repos_cached(state_dir, &slugs)
            } else {
                github_throttled(state_dir, &slugs, opts.gh_wait, opts.gh_cache_secs)
            }
        });
        // Local files only — the notes index keeps it one pass.
        let tracker = reused.is_none().then(|| {
            s.spawn(|| {
                pm.as_ref().map(|pm| {
                    let issues = board::load_all(&pm.dir, None).unwrap_or_default();
                    board::views(&pm.config.notes_dir(), issues)
                })
            })
        });
        (
            sources.map(|h| h.join().expect("overview daemon sources panicked")),
            gh.join().expect("overview gh refresh panicked"),
            tracker.and_then(|h| h.join().expect("overview tracker read panicked")),
        )
    });
    let sources = match (reused, fresh_sources) {
        (Some(r), _) => Cow::Borrowed(r.sources),
        (None, Some(fresh)) => Cow::Owned(fresh),
        (None, None) => unreachable!("sources are gathered unless reused"),
    };
    let views = match reused {
        Some(r) => pm.as_ref().map(|_| Cow::Borrowed(r.views)),
        None => fresh_views.map(Cow::Owned),
    };
    Ok(Snapshot {
        now,
        pm,
        projects,
        repo_paths,
        slug_project,
        slug_clone,
        has_github: !slugs.is_empty(),
        sources,
        views,
        gh_repos,
        gh_state,
    })
}
