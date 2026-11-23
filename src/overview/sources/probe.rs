//! Bounded daemon and per-agent RPC probes for overview source collection.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::super::{degraded, Options};
use crate::adapter::registry;
use crate::client;

/// Concurrent per-agent probes.
const PROBE_WORKERS: usize = 8;

/// What one agent's probes returned. A mailbox (no actor) needs none —
/// its `agent_list` row carries the backlog and health.
#[derive(Clone, Default)]
pub(in super::super) struct AgentProbe {
    pub(in super::super) show: Option<Value>,
    pub(in super::super) requests: Vec<Value>,
    /// A running/submitted turn, a busy pane, or a probe that could not
    /// tell — any of them holds the drift restart back.
    pub(in super::super) holds_drift: bool,
    /// The daemon predates `agent_probe`.
    pub(in super::super) probe_unknown: bool,
    /// The probe budget ran out before this agent was reached.
    pub(in super::super) unprobed: bool,
    pub(in super::super) degraded: Vec<Value>,
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
pub(in super::super) fn probe_agent(
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
pub(in super::super) struct DaemonView {
    pub(in super::super) reachable: bool,
    pub(in super::super) info: Option<Value>,
    pub(in super::super) agents: Vec<Value>,
    pub(in super::super) probes: Vec<AgentProbe>,
    pub(in super::super) degraded: Vec<Value>,
}

pub(super) fn daemon_view(state_dir: &Path, opts: &Options) -> DaemonView {
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
