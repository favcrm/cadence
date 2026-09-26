//! CAD-534: `cadence daemon` timers RPC handlers — moved verbatim from src/daemon.rs; the
//! item→file map is src/daemon/split-map.toml
//! (scripts/split-daemon regenerates it).

use super::*;

/// A configured age below this is raised to it, with a warning — an
/// automatic sweep never reaches an agent idle for less than a week.
pub const AGENT_GC_FLOOR_SECS: u64 = 7 * 86_400;

/// The timer sweeps at most this often.
pub(super) const AGENT_GC_EVERY: Duration = Duration::from_secs(3600);

/// How often the timer re-reads `[host]` — enabling, retuning or
/// disabling it applies without a daemon restart.
const AGENT_GC_RECHECK: Duration = Duration::from_secs(60);

/// The daemon's agent-gc timer: where its setting comes from and what
/// it last did — `health` (`cadence daemon status`) reports both.
pub(super) struct AgentGcTimer {
    /// `Some` is verbatim (tests); `None` re-reads `[host]` each check.
    pub(super) pinned: Option<AgentGcSetting>,
    pub(super) state: Mutex<AgentGcState>,
}

#[derive(Default)]
pub(super) struct AgentGcState {
    pub(super) setting: AgentGcSetting,
    pub(super) next_check: Option<Instant>,
    pub(super) last_sweep: Option<Instant>,
    pub(super) last_check_at: Option<f64>,
    pub(super) last_sweep_at: Option<f64>,
    pub(super) last_removed: Option<usize>,
}

impl AgentGcTimer {
    pub(super) fn new(pinned: Option<AgentGcSetting>) -> Self {
        let timer = Self {
            pinned,
            state: Mutex::new(AgentGcState::default()),
        };
        timer.state.lock().unwrap().setting = timer.resolve();
        timer
    }

    pub(super) fn resolve(&self) -> AgentGcSetting {
        self.pinned.clone().unwrap_or_else(|| {
            AgentGcSetting::from_pm_dir(crate::issue::default_dir().ok().as_deref())
        })
    }

    pub(super) fn status(&self) -> Value {
        let st = self.state.lock().unwrap();
        let effective = st.setting.effective_secs();
        json!({
            "enabled": effective.is_some(),
            "older_than_secs": effective,
            "configured_secs": st.setting.configured_secs,
            "floor_secs": AGENT_GC_FLOOR_SECS,
            "every_secs": AGENT_GC_EVERY.as_secs(),
            "setting": "pm.yaml [host] agent_gc_older_than_secs (unset = off)",
            "warning": st.setting.warning(),
            "last_check_at": st.last_check_at,
            "last_sweep_at": st.last_sweep_at,
            "last_removed": st.last_removed,
            "note": AGENT_GC_RECORDS_ONLY,
        })
    }
}

impl Shared {
    /// One stall-watch tick of the agent-gc timer: a no-op until a
    /// check is due (every `AGENT_GC_RECHECK`), and a sweep at most once
    /// per `AGENT_GC_EVERY`, only while the setting is on. Runs on the
    /// stall-watch thread, never on an actor loop.
    pub(super) fn agent_gc_tick(&self) {
        let now = Instant::now();
        let due = self
            .agent_gc
            .state
            .lock()
            .unwrap()
            .next_check
            .is_none_or(|at| now >= at);
        if !due {
            return;
        }
        // pm.yaml is read outside the lock `health` takes.
        let setting = self.agent_gc.resolve();
        let older_than = {
            let mut st = self.agent_gc.state.lock().unwrap();
            if st.last_check_at.is_none() || st.setting != setting {
                if let Some(warning) = setting.warning() {
                    eprintln!("agent-gc timer: {warning}");
                }
            }
            st.next_check = Some(now + AGENT_GC_RECHECK);
            st.last_check_at = Some(epoch_secs());
            st.setting = setting;
            let Some(older_than) = st.setting.effective_secs() else {
                return;
            };
            if st
                .last_sweep
                .is_some_and(|at| now.duration_since(at) < AGENT_GC_EVERY)
            {
                return;
            }
            st.last_sweep = Some(now);
            older_than
        };
        let removed = self.agent_gc_sweep(older_than as f64);
        {
            let mut st = self.agent_gc.state.lock().unwrap();
            st.last_sweep_at = Some(epoch_secs());
            st.last_removed = Some(removed.len());
        }
        if !removed.is_empty() {
            eprintln!(
                "agent-gc timer: removed {} dead agent registry row(s) idle over {older_than}s \
                 ({}) — {AGENT_GC_RECORDS_ONLY}",
                removed.len(),
                removed.join(", ")
            );
            self.wake();
        }
    }

    /// The manual `agent gc` candidate rule, narrowed: never an enabled
    /// agent, a lifecycle-owned alias, a pty row whose pane is still up,
    /// or a row with an open or `unknown` message. The store re-checks
    /// the row rules in the removing transaction and records one
    /// `agent_gc_removed` event per row. Unlike `agent gc`, nothing is
    /// killed — registry rows only.
    fn agent_gc_sweep(&self, older_than: f64) -> Vec<String> {
        let candidates = match self.store.gc_candidates(Some(older_than)) {
            Ok(candidates) => candidates,
            Err(error) => {
                eprintln!("agent-gc timer: candidate read failed: {error}");
                return Vec::new();
            }
        };
        let mut removed = Vec::new();
        for agent in candidates {
            if agent.enabled {
                continue;
            }
            if agent.endpoint_kind == "pty"
                && adapter::pty::pane_alive(&self.state_dir, &agent.alias, &self.provider_env)
            {
                continue;
            }
            // Held per row, as `agent gc` holds it: a resume cannot
            // start an actor between the ownership check and the delete.
            let lc = self.lifecycle.lock().unwrap();
            if lc.owned(&agent.alias) {
                continue;
            }
            match self.store.timer_gc_remove(&agent.alias, older_than) {
                Ok(Some(_)) => {
                    self.open_attach.lock().unwrap().remove(&agent.alias);
                    removed.push(agent.alias);
                }
                Ok(None) => {}
                Err(error) => eprintln!("agent-gc timer: {}: {error}", agent.alias),
            }
        }
        removed
    }
}

// ---- Idle auto-stop (CAD-96): default ON, resumable ----
//
// An agent whose actor has had nothing to do for the bound — no message
// in any non-terminal state and no delivery, report or turn activity on
// its durable streams — is stopped through the normal `agent stop` path
// (pane-session reaping and slot-enrollment revocation included) and
// stays resumable. PMs/group roots, inboxes, agents with an attached
// terminal client and opted-out agents are never stopped.

/// The idle bound when pm.yaml says nothing: one hour, every provider.
pub const AUTO_STOP_DEFAULT_SECS: u64 = 3600;

/// A configured bound below this is raised to it, with a warning — a
/// typo'd `60` must not stop agents between two turns of one task.
pub const AUTO_STOP_FLOOR_SECS: u64 = 600;

/// The timer checks at most this often (seconds of its clock).
const AUTO_STOP_EVERY_SECS: f64 = 60.0;

/// The event an auto-stop records on the agent's own stream.
pub const AUTO_STOP_EVENT: &str = "agent_auto_stopped";

/// CAD-413: work queued for an auto-stopped agent resumes it — recorded
/// before the resume starts, so it supersedes the auto-stop marker and
/// the sweep never retries the same stop.
pub const AUTO_RESUME_EVENT: &str = "agent_auto_resumed";

/// CAD-413: that resume failed (a refused start, or an open that never
/// reached `ready`). The marker holds — no retry — until an operator
/// resume or stop supersedes it, and it raises a needs-me row.
pub const AUTO_RESUME_FAILED_EVENT: &str = "agent_auto_resume_failed";

/// Event kinds that are bookkeeping, not delivery/report/turn work:
/// they never reset an agent's idle clock. Everything else does — an
/// unknown new kind errs toward keeping the agent.
pub(super) const AUTO_STOP_PASSIVE_KINDS: &[&str] = &[
    AUTO_STOP_EVENT,
    AUTO_RESUME_EVENT,
    AUTO_RESUME_FAILED_EVENT,
    "stop_requested",
    "params_updated",
    "quota_updated",
    "quota_update_ignored",
    "inbox_unconsumed",
    "pane_tree_reap_intent",
    "pane_tree_reaped",
    "pane_tree_reap_refused",
    "pane_tree_unowned",
];

/// The newest of these is the agent's durable stop reason: an
/// auto-stop, a manual stop (`stop_requested`), an open (`ready`), or
/// an auto-resume in flight or failed. Events outlive a daemon restart,
/// so the timer's stop is told apart from an operator's across one.
pub(super) const AUTO_STOP_MARKER_KINDS: &[&str] = &[
    AUTO_STOP_EVENT,
    "stop_requested",
    "ready",
    AUTO_RESUME_EVENT,
    AUTO_RESUME_FAILED_EVENT,
];

/// What the attached-client exemption can and cannot see.
pub const AUTO_STOP_ATTACH_NOTE: &str = "attached-terminal exemption: pty panes via tmux \
     list-clients; a managed-ws codex TUI client (`codex resume --remote`) is not \
     detectable and does not exempt its agent";

/// An idle duration as an operator reads it: `72m`, `3h`, `5h20m`.
fn fmt_idle(idle_secs: f64) -> String {
    let mins = (idle_secs.max(0.0) / 60.0).round() as u64;
    if mins < 120 {
        format!("{mins}m")
    } else if mins.is_multiple_of(60) {
        format!("{}h", mins / 60)
    } else {
        format!("{}h{}m", mins / 60, mins % 60)
    }
}

/// `stopped (auto, idle 72m)` — how every surface names an auto-stop.
pub fn auto_stop_label(idle_secs: f64) -> String {
    format!("stopped (auto, idle {})", fmt_idle(idle_secs))
}

/// The `auto_stopped` view of a stopped agent whose newest marker is an
/// auto-stop — `None` for a live, manually stopped or resumed agent.
pub(super) fn auto_stop_view(agent: &Agent, marker: Option<&store::Event>) -> Option<Value> {
    let event = marker.filter(|e| e.kind == AUTO_STOP_EVENT)?;
    if agent.state != "stopped" {
        return None;
    }
    let idle = event.payload["idle_secs"].as_f64().unwrap_or(0.0);
    Some(json!({
        "at": event.at,
        "idle_secs": idle,
        "bound_secs": event.payload["bound_secs"],
        "reason": event.payload["reason"],
        "label": auto_stop_label(idle),
        "resume": format!("cadence agent resume {}", agent.alias),
    }))
}

/// CAD-413: the `auto_resume_failed` view of an agent whose newest
/// marker is a failed auto-resume and that is still not live — what the
/// needs-me row names: the agent, the message left waiting, and why.
fn auto_resume_failed_view(agent: &Agent, marker: Option<&store::Event>) -> Option<Value> {
    let event = marker.filter(|e| e.kind == AUTO_RESUME_FAILED_EVENT)?;
    if !matches!(agent.state.as_str(), "stopped" | "attention" | "offline") {
        return None;
    }
    Some(json!({
        "at": event.at,
        "message": event.payload["message"],
        "queued": event.payload["queued"],
        "reason": event.payload["reason"],
        "resume": format!("cadence agent resume {}", agent.alias),
    }))
}

/// Stamp `auto_stopped` + `state_label` (or `auto_resume_failed`) onto
/// an agent JSON row.
pub(super) fn apply_auto_stop_view(j: &mut Value, agent: &Agent, marker: Option<&store::Event>) {
    if let Some(view) = auto_stop_view(agent, marker) {
        j["state_label"] = view["label"].clone();
        j["auto_stopped"] = view;
    }
    if let Some(view) = auto_resume_failed_view(agent, marker) {
        j["auto_resume_failed"] = view;
    }
}

/// One agent's verdict on a timer check.
#[derive(Debug, PartialEq)]
pub(super) enum AutoStopVerdict {
    Keep(String),
    Stop {
        idle_secs: f64,
        idle_since: f64,
        bound_secs: u64,
        source: String,
    },
}

/// The daemon's idle auto-stop timer: its setting, its clock (epoch
/// seconds; tests inject one) and what it last did — `health` (`cadence
/// daemon status`) reports all of it.
pub(super) struct AutoStopTimer {
    /// `Some` is verbatim (tests); `None` re-reads `[host]` each check.
    pub(super) pinned: Option<AutoStopSetting>,
    pub(super) clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    pub(super) state: Mutex<AutoStopState>,
}

#[derive(Default)]
pub(super) struct AutoStopState {
    pub(super) setting: AutoStopSetting,
    pub(super) last_check_at: Option<f64>,
    /// When the last sweep finished — `last_kept` belongs to it.
    pub(super) last_sweep_at: Option<f64>,
    pub(super) last_stop_at: Option<f64>,
    pub(super) last_stopped: Vec<String>,
    pub(super) stopped_total: u64,
    /// Why each live agent was kept on the last check.
    pub(super) last_kept: std::collections::BTreeMap<String, String>,
    /// CAD-413: stopped agents with work waiting whose stop was not the
    /// timer's — their marker is read once, not every tick. An entry
    /// leaves when the agent stops being a candidate (resumed, queue
    /// drained); only an auto-stop, which needs a live agent with an
    /// empty queue, could make it one again.
    pub(super) resume_declined: HashSet<String>,
}

impl AutoStopTimer {
    pub(super) fn new(
        pinned: Option<AutoStopSetting>,
        clock: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
    ) -> Self {
        let timer = Self {
            pinned,
            clock: clock.unwrap_or_else(|| Arc::new(epoch_secs)),
            state: Mutex::new(AutoStopState::default()),
        };
        timer.state.lock().unwrap().setting = timer.resolve();
        timer
    }

    pub(super) fn resolve(&self) -> AutoStopSetting {
        self.pinned.clone().unwrap_or_else(|| {
            AutoStopSetting::from_pm_dir(crate::issue::default_dir().ok().as_deref())
        })
    }

    pub(super) fn status(&self) -> Value {
        let st = self.state.lock().unwrap();
        let bound = st.setting.default_bound();
        json!({
            "enabled": bound.is_some(),
            "idle_secs": bound,
            "configured_secs": st.setting.idle_secs,
            "by_provider": st.setting.by_provider,
            "default_secs": AUTO_STOP_DEFAULT_SECS,
            "floor_secs": AUTO_STOP_FLOOR_SECS,
            "every_secs": AUTO_STOP_EVERY_SECS as u64,
            "setting": "pm.yaml [host] auto_stop_idle_secs (unset = 3600, 0 = off) and \
                        auto_stop_idle_secs_by_provider; per agent: `cadence agent set \
                        <alias> auto_stop=off` or `auto_stop_idle_secs=<n>`",
            "exempt": "group roots (role pm, no upstream, or named as an upstream), \
                       inboxes, agents with an attached terminal client, auto_stop=off",
            "attach_detection": AUTO_STOP_ATTACH_NOTE,
            "warning": st.setting.warning(),
            "last_check_at": st.last_check_at,
            "last_sweep_at": st.last_sweep_at,
            "last_stop_at": st.last_stop_at,
            "last_stopped": st.last_stopped,
            "stopped_total": st.stopped_total,
            "last_kept": st.last_kept,
            "resume": "cadence agent resume <alias> (or `cadence resume <group>`)",
        })
    }
}

impl Shared {
    /// One stall-watch tick of the idle auto-stop timer: a no-op until a
    /// check is due (every [`AUTO_STOP_EVERY_SECS`] of its clock). Runs
    /// on the stall-watch thread, never on an actor loop.
    pub(super) fn auto_stop_tick(self: &Arc<Self>) {
        let now = (self.auto_stop.clock)();
        let due = self
            .auto_stop
            .state
            .lock()
            .unwrap()
            .last_check_at
            // A clock stepped backwards re-arms rather than pausing.
            .is_none_or(|at| now - at >= AUTO_STOP_EVERY_SECS || now < at);
        if !due {
            return;
        }
        // pm.yaml is read outside the lock `health` takes.
        let setting = self.auto_stop.resolve();
        {
            let mut st = self.auto_stop.state.lock().unwrap();
            if st.last_check_at.is_none() || st.setting != setting {
                if let Some(warning) = setting.warning() {
                    eprintln!("idle auto-stop: {warning}");
                }
            }
            st.last_check_at = Some(now);
            st.setting = setting.clone();
        }
        if setting.config_error.is_some() {
            return;
        }
        let (stopped, kept) = self.auto_stop_sweep(&setting, now);
        let mut st = self.auto_stop.state.lock().unwrap();
        st.last_sweep_at = Some(now);
        st.last_kept = kept;
        if !stopped.is_empty() {
            st.last_stop_at = Some(now);
            st.stopped_total += stopped.len() as u64;
            st.last_stopped = stopped;
        }
    }

    /// Evaluate every agent with a live actor; stop the idle ones.
    /// Returns (stopped aliases, kept alias → reason).
    fn auto_stop_sweep(
        self: &Arc<Self>,
        setting: &AutoStopSetting,
        now: f64,
    ) -> (Vec<String>, std::collections::BTreeMap<String, String>) {
        let mut kept = std::collections::BTreeMap::new();
        let live: HashSet<String> = {
            let lc = self.lifecycle.lock().unwrap();
            lc.agents
                .keys()
                .filter(|a| !lc.stopping.contains(*a))
                .cloned()
                .collect()
        };
        if live.is_empty() {
            return (Vec::new(), kept);
        }
        let (agents, activity) = match (
            self.store.agents(),
            self.store.auto_stop_activity(AUTO_STOP_PASSIVE_KINDS),
        ) {
            (Ok(agents), Ok(activity)) => (agents, activity),
            (Err(error), _) | (_, Err(error)) => {
                eprintln!("idle auto-stop: store read failed: {error}");
                return (Vec::new(), kept);
            }
        };
        let roots = upstream_roots(&agents);
        let mut stopped = Vec::new();
        for agent in agents.iter().filter(|a| live.contains(&a.alias)) {
            match self.auto_stop_verdict(agent, setting, now, &roots, activity.get(&agent.alias)) {
                AutoStopVerdict::Keep(reason) => {
                    kept.insert(agent.alias.clone(), reason);
                }
                AutoStopVerdict::Stop {
                    idle_secs,
                    idle_since,
                    bound_secs,
                    source,
                } => {
                    match self.auto_stop_agent(agent, idle_secs, idle_since, bound_secs, &source) {
                        Ok(()) => stopped.push(agent.alias.clone()),
                        Err(reason) => {
                            kept.insert(agent.alias.clone(), reason);
                        }
                    }
                }
            }
        }
        if !stopped.is_empty() {
            eprintln!(
                "idle auto-stop: stopped {} idle agent(s) ({}) — resumable with \
                 `cadence agent resume <alias>`",
                stopped.len(),
                stopped.join(", ")
            );
        }
        (stopped, kept)
    }

    /// Whether `agent` is due for an auto-stop at `now`, and if not why.
    /// Cheap row checks first; tmux is only asked about an agent that
    /// is otherwise due.
    pub(super) fn auto_stop_verdict(
        &self,
        agent: &Agent,
        setting: &AutoStopSetting,
        now: f64,
        roots: &HashSet<String>,
        activity: Option<&(Option<f64>, i64)>,
    ) -> AutoStopVerdict {
        use AutoStopVerdict::Keep;
        if !registry::has_actor(&agent.provider, &agent.endpoint_kind) {
            return Keep("inbox".to_string());
        }
        if let Some(why) = group_root_reason(agent, roots) {
            return Keep(format!("group root ({why})"));
        }
        let (bound, source) = setting.bound_for(agent);
        let Some(bound_secs) = bound else {
            return Keep(format!("auto-stop off ({source})"));
        };
        if !agent.enabled || agent.state != "idle" {
            return Keep(format!("state {}", agent.state));
        }
        if agent.thread_id.as_deref().unwrap_or_default().is_empty() {
            return Keep("not resumable: no native thread/session recorded".to_string());
        }
        let (last, open) = activity.copied().unwrap_or((None, 0));
        if open > 0 {
            return Keep(format!("busy: {open} message(s) not in a terminal state"));
        }
        let Some(idle_since) = last else {
            return Keep("no durable activity record".to_string());
        };
        let idle_secs = now - idle_since;
        if idle_secs < bound_secs as f64 {
            return Keep(format!(
                "idle {:.0}s of {bound_secs}s ({source})",
                idle_secs.max(0.0)
            ));
        }
        if agent.endpoint_kind == "pty" {
            match adapter::pty::pane_clients(&self.state_dir, &agent.alias, &self.provider_env) {
                Some(0) => {}
                Some(n) => return Keep(format!("{n} terminal client(s) attached")),
                None => return Keep("attached clients unreadable (tmux)".to_string()),
            }
            match self.adapter_for(&agent.alias).and_then(|a| a.probe()) {
                Ok(probe) if probe.idle => {}
                Ok(probe) => return Keep(format!("pane not idle: {}", probe.reason)),
                Err(error) => return Keep(format!("pane probe failed: {error}")),
            }
        }
        AutoStopVerdict::Stop {
            idle_secs,
            idle_since,
            bound_secs,
            source,
        }
    }

    /// Stop one idle agent through the normal `agent stop` path and
    /// record `agent_auto_stopped` on its stream. The durable facts are
    /// re-read first: a message queued or a turn begun since the sweep
    /// read them keeps the agent.
    fn auto_stop_agent(
        self: &Arc<Self>,
        agent: &Agent,
        idle_secs: f64,
        idle_since: f64,
        bound_secs: u64,
        source: &str,
    ) -> std::result::Result<(), String> {
        let alias = agent.alias.as_str();
        let fresh = self
            .store
            .auto_stop_activity(AUTO_STOP_PASSIVE_KINDS)
            .map_err(|e| format!("re-check failed: {e}"))?;
        let (last, open) = fresh.get(alias).copied().unwrap_or((None, 0));
        let state = self.store.agent(alias).map(|a| a.state).unwrap_or_default();
        if open > 0 || last.is_some_and(|at| at > idle_since) || state != "idle" {
            return Err("activity since the check read it".to_string());
        }
        let reason = format!(
            "idle {} with no queued, running, awaiting-report or unknown message \
             (bound {}s from {source})",
            fmt_idle(idle_secs),
            bound_secs
        );
        let out = self
            .rpc_stop(&json!({"alias": alias}))
            .map_err(|e| format!("stop failed: {e}"))?;
        let _ = self.store.event_public(
            alias,
            AUTO_STOP_EVENT,
            json!({
                "idle_secs": idle_secs.floor(),
                "idle_since": idle_since,
                "bound_secs": bound_secs,
                "bound_source": source,
                "reason": reason,
                "state": out["state"],
                "resumable": true,
                "resume": format!("cadence agent resume {alias}"),
            }),
        );
        self.wake();
        Ok(())
    }
}

// ---- Auto-resume on queued work (CAD-413) ----
//
// An agent the idle timer stopped is parked, not dismissed: a message
// queued for it — by any path, including one left queued across a
// daemon restart — resumes it through the normal `agent resume` path
// and the actor delivers as usual. An operator/PM stop, a fence, or a
// failed auto-resume keeps the agent stopped and the message waits;
// the failure raises a needs-me row instead of passing silently.

impl Shared {
    /// Resume every auto-stopped agent with work queued. Runs on the
    /// stall-watch thread each tick: one grouped queue read, then a
    /// marker read only for the stopped agents that have work waiting.
    pub(super) fn auto_resume_tick(self: &Arc<Self>) {
        let waiting = match self.store.queued_for_stopped() {
            Ok(waiting) => waiting,
            Err(error) => {
                eprintln!("auto-resume: store read failed: {error}");
                return;
            }
        };
        let declined = {
            let mut st = self.auto_stop.state.lock().unwrap();
            st.resume_declined
                .retain(|alias| waiting.iter().any(|(a, _, _)| a == alias));
            st.resume_declined.clone()
        };
        for (alias, message, queued) in waiting {
            if declined.contains(&alias) || self.lifecycle.lock().unwrap().owned(&alias) {
                continue;
            }
            // Only the timer's own stop qualifies — the newest marker
            // must still be the auto-stop, never a manual stop or an
            // earlier auto-resume (in flight or failed).
            let marker = match self.store.last_event_of(&alias, AUTO_STOP_MARKER_KINDS) {
                Ok(marker) => marker,
                Err(error) => {
                    eprintln!("auto-resume: {alias}: stop reason unreadable: {error}");
                    continue;
                }
            };
            match marker {
                Some(stopped) if stopped.kind == AUTO_STOP_EVENT => {
                    self.auto_resume(&alias, &message, queued, &stopped);
                }
                // A resume recorded but never started: only a daemon
                // that died between the record and the start leaves a
                // `stopped`, unowned agent behind it (a start moves it
                // to `starting` under the same lock). Surface it rather
                // than let the message wait silently.
                Some(resumed) if resumed.kind == AUTO_RESUME_EVENT => {
                    self.auto_resume_failed(
                        &alias,
                        "the daemon stopped before this auto-resume started the agent",
                    );
                }
                _ => {
                    self.auto_stop
                        .state
                        .lock()
                        .unwrap()
                        .resume_declined
                        .insert(alias);
                }
            }
        }
    }

    /// Resume `alias` for its queued work, if the stop the sweep saw
    /// (`seen`) is still its stop. The check, the record and the start
    /// happen under the `lifecycle` lock that `agent stop` takes to
    /// reserve the alias before it writes `stop_requested`, so a
    /// racing operator stop always wins: in flight, the alias is owned;
    /// finished, a newer marker has replaced `seen`. Either way the
    /// resume declines quietly — no event, no needs-me row. A stop that
    /// begins after the lock is released stops the new actor. The
    /// record lands before the start and supersedes the auto-stop
    /// marker, so a failed start is never retried by the next tick.
    pub(super) fn auto_resume(
        self: &Arc<Self>,
        alias: &str,
        message: &str,
        queued: i64,
        seen: &store::Event,
    ) {
        let mut lc = self.lifecycle.lock().unwrap();
        if lc.owned(alias) {
            return;
        }
        let still_seen = self
            .store
            .last_event_of(alias, AUTO_STOP_MARKER_KINDS)
            .is_ok_and(|m| m.is_some_and(|m| m.seq == seen.seq));
        let still_stopped = self.store.agent(alias).is_ok_and(|a| a.state == "stopped");
        if !still_seen || !still_stopped {
            return;
        }
        let recorded = self.store.event_public(
            alias,
            AUTO_RESUME_EVENT,
            json!({
                "message": message,
                "queued": queued,
                "auto_stopped_at": seen.at,
                "reason": format!("message {message} queued for an agent the idle timer stopped"),
            }),
        );
        if let Err(error) = recorded {
            eprintln!("auto-resume: {alias}: cannot record the resume, not starting: {error}");
            return;
        }
        let failure = match self.start_actor_locked(&mut lc, alias, true) {
            Ok(true) => None,
            Ok(false) => Some("fenced by an unreconciled unknown message".to_string()),
            Err(error) => Some(error.to_string()),
        };
        drop(lc);
        match failure {
            None => eprintln!("auto-resume: resuming {alias} for queued message {message}"),
            Some(reason) => self.auto_resume_failed(alias, &reason),
        }
        self.wake();
    }

    /// The auto-resume of `alias` failed — refused at start, or its
    /// open never reached `ready`. Names the waiting message from the
    /// resume record so the needs-me row can.
    pub(super) fn auto_resume_failed(&self, alias: &str, reason: &str) {
        let resumed = self
            .store
            .last_event_of(alias, &[AUTO_RESUME_EVENT])
            .ok()
            .flatten()
            .map(|e| e.payload)
            .unwrap_or(Value::Null);
        let _ = self.store.event_public(
            alias,
            AUTO_RESUME_FAILED_EVENT,
            json!({
                "message": resumed["message"],
                "queued": resumed["queued"],
                "reason": reason,
                "resume": format!("cadence agent resume {alias}"),
            }),
        );
        eprintln!("auto-resume: {alias} failed to resume: {reason}");
        self.wake();
    }
}
