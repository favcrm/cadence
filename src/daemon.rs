//! Persistent local controller: Unix-socket server + one actor per agent.
//!
//! The socket lives in a 0700 state directory and accepts only same-UID
//! peers (`SO_PEERCRED`). That establishes same-user access — it is not a
//! hostile same-user isolation boundary.
//!
//! Each registered agent gets one actor thread that owns its provider
//! adapter and serializes turns. The daemon relaunches enabled actors on
//! start — except actors fenced by an `unknown` in-flight attempt, which
//! stay in `attention` until a human reconciles them.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use uuid::Uuid;

use crate::adapter::{
    self, registry, AdapterHooks, ProviderAdapter, ProviderEnv, ProviderRequest, TurnResult,
};
use crate::client;
use crate::error::{Error, Result};
use crate::proto;
use crate::store::{self, Agent, Message, Store, Take};

/// A `(Mutex, Condvar)` pair used for queue/event wakeups.
pub struct Notify {
    lock: Mutex<()>,
    cv: Condvar,
}

impl Default for Notify {
    fn default() -> Self {
        Self::new()
    }
}

impl Notify {
    pub fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            cv: Condvar::new(),
        }
    }
    pub fn notify_all(&self) {
        let _guard = self.lock.lock().unwrap();
        self.cv.notify_all();
    }
    /// Wait until `deadline`; returns false if it expired.
    pub fn wait_until(&self, deadline: Instant) -> bool {
        let guard = self.lock.lock().unwrap();
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let _ = self.cv.wait_timeout(guard, remaining).unwrap();
        Instant::now() < deadline
    }
}

/// Grace period for a cooperative stop before the transport is force-closed.
const STOP_GRACE: Duration = Duration::from_secs(3);
/// Stall watch cadence — `silent_secs` stays live without a store read
/// per agent becoming pressure.
const STALL_TICK: Duration = Duration::from_secs(2);
/// `stall_secs` when neither the job nor the agent sets one.
const DEFAULT_STALL_SECS: u64 = 1800;
/// PTY screens are sampled at most this often while a turn runs — the
/// bound is one capture per running pty agent per minute.
const SCREEN_SAMPLE: Duration = Duration::from_secs(60);

/// Screen sampling interval: this daemon's `ServeOptions` value when
/// set (tests shrink it per daemon), else `CADENCE_STALL_SAMPLE_SECS`,
/// else the kickoff's one-minute bound.
fn screen_sample(own: &AtomicU64) -> Duration {
    let secs = match own.load(Ordering::Relaxed) {
        0 => std::env::var("CADENCE_STALL_SAMPLE_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0),
        n => n,
    };
    if secs > 0 {
        Duration::from_secs(secs)
    } else {
        SCREEN_SAMPLE
    }
}

/// Unix epoch seconds — for `last_activity` in stall events.
fn epoch_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Human silence duration for stall notices — `42s`, `12m 3s`, `1h 4m`.
fn fmt_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

struct PendingRequest {
    alias: String,
    id: Value,
    method: String,
    params: Value,
}

/// Per-agent control surface shared by dispatch and the actor thread.
struct AgentCtl {
    /// The actor's live adapter, published before `open` so a stop can
    /// force-close it even while initialization is still in flight.
    adapter: Mutex<Option<Arc<dyn ProviderAdapter>>>,
    wake: Notify,
    thread: Mutex<Option<JoinHandle<()>>>,
    /// Stall-watch state for the in-flight turn — updated by provider
    /// events, the turn-start hook and the watch's own sampling.
    stall: Mutex<StallWatch>,
}

/// What the stall watch knows about the agent's in-flight turn.
/// `activity` is the freshest proof of life the daemon has folded in:
/// the turn start, a provider event, a pty screen change, or an open
/// brokered request. `stalled_at` marks a `turn_stalled` episode —
/// `turn_resumed` requires strictly newer activity than it.
struct StallWatch {
    /// Running message this state covers; `None` when idle.
    message: Option<String>,
    /// Freshest activity instant observed for `message`.
    activity: Instant,
    /// The last-activity instant recorded when `turn_stalled` fired —
    /// a resume needs activity strictly newer than this.
    stalled_at: Option<Instant>,
    /// When the last screen sample landed — the capture throttle.
    sample_at: Option<Instant>,
    /// The settled screen hash. Screen activity is debounced — a new
    /// hash counts as activity only once confirmed: the next sample
    /// shows the same new hash, or the screen has differed from both
    /// of the last two settled hashes for two consecutive samples
    /// (a scrolling pane churns through distinct hashes and still
    /// confirms). A hash seen once and reverted — a capture taken
    /// mid-repaint — can neither resume a stalled turn nor reset the
    /// silence clock. Confirmation costs one extra sample interval
    /// before `turn_resumed`; captures stay capped at one per
    /// interval.
    settled: Option<String>,
    /// The settled value before `settled` — history for the novelty
    /// check: a hash equal to a recent settled value must confirm by
    /// repetition, not by merely differing again.
    previous: Option<String>,
    /// A hash differing from `settled` seen once, awaiting a second
    /// consecutive sighting to confirm by repetition.
    candidate: Option<String>,
    /// Consecutive samples that differed from `settled` — `>= 2` with
    /// a novel hash confirms by persistence.
    changed: u8,
    /// A capture in flight on its own thread — at most one per agent,
    /// so a wedged pane leaks one thread and never stalls the ticker.
    sample_rx: Option<std::sync::mpsc::Receiver<String>>,
    /// Stall episodes seen for `message`; each mints a distinct notice
    /// dedupe so a resume + re-stall notifies again.
    episodes: u64,
}

impl Default for StallWatch {
    fn default() -> Self {
        Self {
            message: None,
            activity: Instant::now(),
            stalled_at: None,
            sample_at: None,
            settled: None,
            previous: None,
            candidate: None,
            changed: 0,
            sample_rx: None,
            episodes: 0,
        }
    }
}

impl AgentCtl {
    /// Fold a fresh proof of life into the watch's clock.
    fn bump_activity(&self) {
        self.stall.lock().unwrap().activity = Instant::now();
    }
}

/// Alias ownership: an alias is owned while an actor ctl exists OR while
/// a stop reservation is held. The reservation covers the whole stop —
/// interrupt through the final state write — so a resume can never start
/// a new actor generation underneath a finishing stop.
#[derive(Default)]
struct Lifecycle {
    agents: HashMap<String, Arc<AgentCtl>>,
    stopping: HashSet<String>,
}

impl Lifecycle {
    fn owned(&self, alias: &str) -> bool {
        self.agents.contains_key(alias) || self.stopping.contains(alias)
    }
}

/// Drops the caller's stop reservation on scope exit. Only the stop
/// that inserted the reservation ever holds this guard — overlapping
/// stops are rejected before mutation.
struct StopReservation<'a> {
    lifecycle: &'a Mutex<Lifecycle>,
    alias: &'a str,
}

impl Drop for StopReservation<'_> {
    fn drop(&mut self) {
        self.lifecycle.lock().unwrap().stopping.remove(self.alias);
    }
}

pub struct Shared {
    pub store: Store,
    /// Broadcast on any queue/event change.
    changed: Notify,
    pending: Mutex<HashMap<String, PendingRequest>>,
    /// Brokered requests answered by `agent respond` but not yet
    /// collected by their `request_wait` caller: handle → (alias,
    /// answer). In-memory like `pending` — a daemon restart drops both
    /// and the waiter hears `closed`, by design.
    answered: Mutex<HashMap<String, (String, Value)>>,
    lifecycle: Mutex<Lifecycle>,
    closing: AtomicBool,
    provider_log_dir: PathBuf,
    state_dir: PathBuf,
    /// How each agent's endpoint last came up (`"adopted"` /
    /// `"respawned"` — attachable kinds only), recorded before the
    /// identity write so a resume report can say which happened.
    open_attach: Mutex<HashMap<String, &'static str>>,
    /// Provider launch overrides for this daemon instance.
    provider_env: ProviderEnv,
    /// Unix epoch seconds when this daemon process came up — the
    /// `started_at` half of `daemon_info`'s build/uptime report.
    started_at: f64,
    /// Stall screen-sample seconds for this daemon (0 = unset).
    stall_sample_secs: Arc<AtomicU64>,
    /// This run's instance id — recorded at start and stamped on the
    /// shutdown marker, so the next daemon can prove a marker belongs
    /// to the immediately preceding run (CAD-89).
    instance: String,
}

impl Shared {
    pub fn new(state_dir: &Path, opts: &ServeOptions) -> Result<Arc<Self>> {
        Self::new_hot(state_dir, opts, HotStart::fresh())
    }

    /// `new` with the consumed hot-restart context: the adoption
    /// candidates the marker carried plus this run's instance id.
    pub fn new_hot(state_dir: &Path, opts: &ServeOptions, hot: HotStart) -> Result<Arc<Self>> {
        let HotStart { instance, marker } = hot;
        let store = Store::open_adopting(&state_dir.join("cadence.sqlite3"), marker)?;
        let provider_log_dir = state_dir.join("agents");
        std::fs::create_dir_all(&provider_log_dir)?;
        Ok(Arc::new(Self {
            store,
            changed: Notify::new(),
            pending: Mutex::new(HashMap::new()),
            answered: Mutex::new(HashMap::new()),
            lifecycle: Mutex::new(Lifecycle::default()),
            closing: AtomicBool::new(false),
            provider_log_dir,
            state_dir: state_dir.to_path_buf(),
            open_attach: Mutex::new(HashMap::new()),
            provider_env: opts.provider_env.clone(),
            started_at: epoch_secs(),
            stall_sample_secs: Arc::clone(&opts.stall_sample_secs),
            instance,
        }))
    }

    fn wake(&self) {
        self.changed.notify_all();
    }

    /// Spawn the actor for `alias` unless it is already owned (running or
    /// stopping) or fenced by an unknown outcome.
    pub fn launch_actor(self: &Arc<Self>, alias: &str) -> Result<()> {
        let mut lc = self.lifecycle.lock().unwrap();
        if lc.owned(alias) {
            return Err(Error::rejected("Agent is still running or stopping"));
        }
        self.start_actor_locked(&mut lc, alias, false)?;
        Ok(())
    }

    /// Fence check, enable, state write, spawn and insert — all while the
    /// lifecycle lock is held, so a concurrent stop/resume cannot
    /// interleave. The map entry is the ownership record: it is inserted
    /// before the actor becomes visible and removed only by the actor
    /// itself after full termination, so a present entry always means
    /// "still owned". Returns false when the alias is fenced.
    fn start_actor_locked(
        self: &Arc<Self>,
        lc: &mut Lifecycle,
        alias: &str,
        enable: bool,
    ) -> Result<bool> {
        // A mailbox never gets an actor — regardless of who asked.
        let a = self.store.agent(alias)?;
        if !registry::has_actor(&a.provider, &a.endpoint_kind) {
            return Err(Error::rejected(format!(
                "Agent '{alias}' is an inbox — it has no actor to start"
            )));
        }
        if self.store.has_unknown(alias)? {
            // One write: the fence lands with the runtime fields
            // cleared — no `attention` + live endpoint window.
            self.store.set_state_detached(
                alias,
                "attention",
                Some(&self.uncertain_fence_text(alias)),
            )?;
            let _ = self.store.event_public(
                alias,
                "attention",
                json!({"reason": "uncertain_turn_preserved"}),
            );
            self.wake();
            return Ok(false);
        }
        if enable {
            self.store.set_enabled(alias, true)?;
        }
        self.store.set_agent_state(alias, "starting", None)?;
        let ctl = Arc::new(AgentCtl {
            adapter: Mutex::new(None),
            wake: Notify::new(),
            thread: Mutex::new(None),
            stall: Mutex::new(StallWatch::default()),
        });
        let shared = Arc::clone(self);
        let owned = alias.to_string();
        let spawned = Arc::clone(&ctl);
        let handle = thread::spawn(move || shared.run_actor(&owned, spawned));
        *ctl.thread.lock().unwrap() = Some(handle);
        lc.agents.insert(alias.to_string(), ctl);
        Ok(true)
    }

    fn on_provider_event(&self, alias: &str, method: &str, params: Value) {
        // Every adapter event is proof of life for the stall watch —
        // provider lifecycle traffic and the explicit `cadence/*`
        // channel (ack, tool_use, message_report) alike.
        self.bump_activity(alias);
        // `cadence/<kind>` is the adapter's own bookkeeping channel —
        // recorded verbatim, not provider traffic.
        if let Some(kind) = method.strip_prefix("cadence/") {
            if kind == "claude_init" {
                if let Some(model) = params.get("model").and_then(Value::as_str) {
                    let _ = self.store.set_model_reported(alias, model);
                }
            }
            if kind == "session_minted" {
                // A pty profile minted its native session id before
                // spawning — persist it so the next open resumes it
                // even if this launch dies before session proof.
                if let Some(session) = params.get("session").and_then(Value::as_str) {
                    let _ = self.store.set_params(alias, &json!({"session": session}));
                }
            }
            let _ = self.store.event_public(alias, kind, params);
            self.wake();
            return;
        }
        // Token streams and tool details stay in the provider transcript;
        // we record the lifecycle envelope only.
        let _ = self.store.event_public(
            alias,
            "provider_event",
            json!({
                "method": method, "data": params,
            }),
        );
        if method == "serverRequest/resolved" {
            self.resolve_external(alias, &params);
        }
        self.wake();
    }

    /// The provider resolved a request outside Cadence — e.g. an
    /// attached official TUI answered the approval. Drop the matching
    /// pending handle so a late `agent respond` is rejected rather than
    /// double-answering; other pending requests survive, and the agent
    /// stays `waiting_input` while any remain.
    fn resolve_external(&self, alias: &str, params: &Value) {
        let request_id = params.get("requestId").cloned().unwrap_or(Value::Null);
        let mut pending = self.pending.lock().unwrap();
        let resolved: Vec<String> = pending
            .iter()
            .filter(|(_, req)| req.alias == alias && req.id == request_id)
            .map(|(handle, _)| handle.clone())
            .collect();
        if resolved.is_empty() {
            return;
        }
        for handle in &resolved {
            pending.remove(handle);
            let _ = self
                .store
                .event_public(alias, "input_resolved", json!({"request": handle}));
        }
        drop(pending);
        self.relax_waiting(alias);
    }

    /// Relax `waiting_input` → `busy` only if no pending requests remain
    /// for the alias AND the agent is still waiting. The pending mutex
    /// is held across the conditional update, so a request arriving in
    /// between cannot have its `waiting_input` clobbered, and the SQL
    /// `WHERE state='waiting_input'` can never overwrite a concurrently
    /// finished/fenced/stopped state. Lock order is pending → store
    /// conn everywhere; nothing takes conn → pending.
    fn relax_waiting(&self, alias: &str) {
        let pending = self.pending.lock().unwrap();
        if pending.values().any(|req| req.alias == alias) {
            return;
        }
        let _ = self
            .store
            .set_agent_state_if(alias, "busy", "waiting_input");
    }

    fn on_provider_request(self: &Arc<Self>, alias: &str, request: ProviderRequest) {
        let handle = Uuid::new_v4().simple().to_string();
        self.pending.lock().unwrap().insert(
            handle.clone(),
            PendingRequest {
                alias: alias.to_string(),
                id: request.id,
                method: request.method.clone(),
                params: request.params.clone(),
            },
        );
        // Requests only arrive mid-turn; relax/stop may have moved the
        // agent on already — never clobber a non-busy state.
        let _ = self
            .store
            .set_agent_state_if(alias, "waiting_input", "busy");
        let _ = self.store.event_public(
            alias,
            "input_required",
            json!({"request": handle, "method": request.method, "params": request.params}),
        );
        self.wake();
    }

    /// The actor loop: own the adapter, serialize turns, preserve unknown
    /// outcomes, and stop cleanly on disable/shutdown.
    fn run_actor(self: &Arc<Self>, alias: &str, ctl: Arc<AgentCtl>) {
        let outcome = self.actor_inner(alias, &ctl);
        // Cleanup always runs: release the adapter, clear ctl, final
        // state. Detach — never kill — on daemon shutdown and on any
        // error exit (a fence): owned endpoints like a tmux pane
        // outlive the controller so the operator can inspect the screen
        // a failure left behind and `agent resume` re-adopt it. Explicit
        // stops keep `close()`. `detach` defaults to `close` for
        // adapters that own their provider process, so managed
        // endpoints are still reaped on every exit.
        if let Some(adapter) = ctl.adapter.lock().unwrap().take() {
            if self.closing.load(Ordering::SeqCst) || outcome.is_err() {
                adapter.detach();
            } else {
                adapter.close();
            }
        }
        {
            let mut pending = self.pending.lock().unwrap();
            pending.retain(|_, req| req.alias != alias);
            // Uncollected brokered answers die with the actor too — a
            // `request_wait` still blocked sees the handle gone and
            // reports `closed` to its caller.
            self.answered.lock().unwrap().retain(|_, (a, _)| a != alias);
        }
        self.wake();
        let closing = self.closing.load(Ordering::SeqCst);
        match outcome {
            Err(ref error) => {
                // When unreconciled unknowns outlive the actor, the
                // recorded reason names the reconcile-first recovery —
                // resume alone is rejected while the fence stands.
                let reason = if self.store.has_unknown(alias).unwrap_or(false) {
                    format!(
                        "{error} — reconcile: `cadence agent unfence {alias} \
                         --status interrupted`, then `cadence agent resume {alias}`"
                    )
                } else {
                    error.to_string()
                };
                // One write: `attention` must never be observable with
                // the dead actor's endpoint still attached.
                let _ = self
                    .store
                    .set_state_detached(alias, "attention", Some(&reason));
                let _ = self
                    .store
                    .event_public(alias, "attention", json!({"reason": reason}));
            }
            Ok(()) => {
                let agent = self.store.agent(alias);
                let enabled = agent.as_ref().map(|a| a.enabled).unwrap_or(false);
                let state = if !enabled {
                    "stopped"
                } else if closing {
                    "offline"
                } else {
                    "idle"
                };
                // One write: the terminal state lands together with the
                // cleared endpoint fields.
                let _ = self.store.set_state_detached(alias, state, None);
            }
        }
        // Release the alias only after cleanup and the final state write:
        // until this removal, lifecycle callers still see the agent owned.
        let mut lc = self.lifecycle.lock().unwrap();
        if lc.agents.get(alias).is_some_and(|c| Arc::ptr_eq(c, &ctl)) {
            lc.agents.remove(alias);
        }
        drop(lc);
        self.wake();
    }

    fn actor_inner(self: &Arc<Self>, alias: &str, ctl: &Arc<AgentCtl>) -> Result<()> {
        let agent = self.store.agent(alias)?;
        let log_path = self.provider_log_dir.join(format!("{alias}.provider.log"));
        let shared = Arc::clone(self);
        let owned = alias.to_string();
        let hooks = AdapterHooks {
            on_event: Box::new(move |method, params| {
                shared.on_provider_event(&owned, method, params)
            }),
            on_request: {
                let shared = Arc::clone(self);
                let owned = alias.to_string();
                Box::new(move |request| shared.on_provider_request(&owned, request))
            },
        };
        let adapter = adapter::build(&agent, hooks, &log_path, &self.provider_env)?;
        let adapter: Arc<dyn ProviderAdapter> = Arc::from(adapter);
        // Publish before `open` so stop/shutdown can force-close the
        // transport while initialization RPCs are still in flight.
        *ctl.adapter.lock().unwrap() = Some(Arc::clone(&adapter));
        // A failed open returns Err — run_actor's cleanup detaches on
        // any error, which still closes owned provider processes
        // (detach defaults to close) while leaving a pty pane visible
        // for inspection.
        //
        // Hot restart: a candidate the shutdown marker recorded is
        // opened in adopt mode — the pane is re-validated against the
        // record (same pid, same native session) and the recorded
        // generation is reused so the turn's token stays valid. A
        // refused adoption falls back to the plain fence for this
        // agent only: the message goes `unknown`, the actor exits into
        // `attention`, and `turn_adopt_refused` names the check that
        // failed.
        let adoption = self.store.take_adoption(alias);
        let identity = match &adoption {
            Some(entry) => match adapter.open_adopted(&agent, entry) {
                Ok(identity) => identity,
                Err(error) => {
                    let reason = error.to_string();
                    let _ = self
                        .store
                        .orphan_running(alias, &format!("hot-restart adoption refused: {reason}"));
                    let _ = self.store.event_public(
                        alias,
                        "turn_adopt_refused",
                        json!({"message": entry.message_id,
                               "turn_id": entry.turn_id,
                               "reason": reason}),
                    );
                    return Err(error);
                }
            },
            None => adapter.open(&agent)?,
        };
        // Record how the endpoint came up *before* set_identity makes
        // it visible — a resume report polling the agent row must find
        // the outcome already written.
        if let Some(attach) = identity.attach {
            self.open_attach
                .lock()
                .unwrap()
                .insert(alias.to_string(), attach);
        }
        match &adoption {
            Some(entry) => self.store.set_identity_adopted(alias, &identity, entry)?,
            None => self.store.set_identity(alias, &identity)?,
        }
        self.wake();
        let mut gate_notice: Option<String> = None;
        let mut gate_waits: u32 = 0;
        // Proven paste misses per message — a TUI that looks idle but
        // keeps dropping pastes must not be re-fed forever.
        let mut unrendered: u32 = 0;
        loop {
            if self.closing.load(Ordering::SeqCst) {
                return Ok(());
            }
            match self.store.take_queued(alias)? {
                Take::Stop => return Ok(()),
                Take::Empty => {
                    if adapter.disconnected() {
                        // Submitted PTY messages may have reached the
                        // provider; fence them rather than replay.
                        let _ = self
                            .store
                            .orphan_running(alias, "endpoint lost after submission");
                        return Err(Error::unknown("Provider process disconnected while idle"));
                    }
                    ctl.wake.wait_until(Instant::now() + Duration::from_secs(5));
                }
                Take::Message(message) => {
                    let started_id = message.id.clone();
                    let shared = Arc::clone(self);
                    let watch = Arc::clone(ctl);
                    let outcome = adapter.run_turn(&message.body, &message.id, &move |turn| {
                        let _ = shared.store.mark_running(&started_id, turn);
                        watch.bump_activity();
                        shared.wake();
                    });
                    match outcome {
                        Ok(result) => {
                            if let Err(error) = self.complete(&message, result) {
                                // The provider reported an outcome we could
                                // not persist or classify — ambiguous
                                // post-submission, fence rather than replay.
                                return self.unknown(
                                    alias,
                                    &message,
                                    &format!("reported outcome could not be applied: {error}"),
                                );
                            }
                            gate_notice = None;
                            gate_waits = 0;
                            unrendered = 0;
                        }
                        // The paste did not render within the deadline:
                        // evidence of a dropped or unsubmitted delivery.
                        // Routed notifications are informational — requeue
                        // for at-least-once delivery, bounded; on exhaustion
                        // the delivery is *parked*, not fenced: a
                        // notification must never kill the recipient's pane
                        // and in-flight work. The worker's result stays
                        // durable on the worker's own message, so nothing
                        // is lost. Task messages keep the uncertainty
                        // discipline: `unknown` + fence, never a blind
                        // replay.
                        Err(Error::NotRendered(miss)) => {
                            unrendered += 1;
                            let routed = message.is_routed();
                            let retry = routed && unrendered <= 3;
                            // The miss carries the pane's own evidence:
                            // screen tails before the paste and after
                            // the deadline plus the probe verdict that
                            // admitted the send — what "idle" looked
                            // like when delivery failed.
                            let crate::error::RenderMiss {
                                reason,
                                before_tail,
                                after_tail,
                                claim_probe,
                            } = miss;
                            let _ = self.store.event_public(
                                alias,
                                "paste_not_rendered",
                                json!({"message": message.id,
                                       "reason": reason,
                                       "attempt": unrendered,
                                       "retry": retry,
                                       "before": before_tail,
                                       "after": after_tail,
                                       "claim_probe": claim_probe}),
                            );
                            if retry {
                                let _ = self.store.requeue(&message.id);
                                let _ = self.store.set_agent_state_if(alias, "idle", "busy");
                                gate_notice = None;
                                ctl.wake.wait_until(Instant::now() + Duration::from_secs(5));
                            } else if routed {
                                let _ = self.store.event_public(
                                    alias,
                                    "delivery_parked",
                                    json!({"message": message.id,
                                           "reason": reason,
                                           "attempts": unrendered}),
                                );
                                self.store.finish(
                                    &message,
                                    "failed",
                                    &json!({"status": "failed",
                                            "via": "pty_render_miss",
                                            "error": reason}),
                                    Some(&reason),
                                )?;
                                let _ = self.store.set_agent_state_if(alias, "idle", "busy");
                                unrendered = 0;
                                self.wake();
                            } else {
                                return self.unknown(
                                    alias,
                                    &message,
                                    "submission accepted but never rendered on the endpoint",
                                );
                            }
                        }
                        // The submission gate refused before any paste:
                        // safe to retry — back to the queue with a
                        // bounded backoff, never a silent drop.
                        Err(Error::GateRefused(reason)) => {
                            let _ = self.store.requeue(&message.id);
                            let _ = self.store.set_agent_state_if(alias, "idle", "busy");
                            if gate_notice.as_deref() != Some(reason.as_str()) {
                                let _ = self.store.event_public(
                                    alias,
                                    "gate_wait",
                                    json!({"message": message.id, "reason": reason}),
                                );
                                gate_notice = Some(reason);
                            }
                            // 5s → 10 → 20 → 30s cap: claims and inbox
                            // arrivals wake the wait early, so the poll
                            // is only the fallback for a busy pane.
                            let wait = Duration::from_secs((5u64 << gate_waits.min(3)).min(30));
                            gate_waits = gate_waits.saturating_add(1);
                            unrendered = 0;
                            ctl.wake.wait_until(Instant::now() + wait);
                        }
                        Err(Error::OutcomeUnknown(error)) => {
                            return self.unknown(alias, &message, &error);
                        }
                        // Deterministic pre-submission rejection: zero
                        // bytes reached the provider, so nothing is
                        // unknown — fail the message, keep the endpoint
                        // live and keep draining the queue.
                        Err(Error::PreWrite(reason)) => {
                            self.store.finish(
                                &message,
                                "failed",
                                &json!({"status": "failed", "text": "",
                                        "error": reason}),
                                Some(&reason),
                            )?;
                            gate_notice = None;
                            self.wake();
                        }
                        // A provider/adapter error is actor-fatal: record
                        // the failed attempt, then land in `attention`.
                        Err(error) => {
                            self.store.finish(
                                &message,
                                "failed",
                                &json!({"status": "failed", "text": "", "error": error.to_string()}),
                                Some(&error.to_string()),
                            )?;
                            // Other submitted PTY messages are now
                            // orphaned by the dead endpoint.
                            let _ = self
                                .store
                                .orphan_running(alias, "endpoint lost after submission");
                            self.wake();
                            return Err(error);
                        }
                    }
                }
            }
        }
    }

    fn complete(&self, message: &Message, result: TurnResult) -> Result<()> {
        // PTY endpoints report "submitted": the paste reached the
        // terminal, but only an explicit ack/result report may finish
        // the message — it stays `running` meanwhile.
        if result.status == "submitted" {
            self.store.mark_submitted(message)?;
            // A routed notification's delivery IS its completion — the
            // receiving PM is not expected to report a result on it.
            if message.is_routed() {
                self.store.finish(
                    message,
                    "completed",
                    &json!({"status": "completed", "via": "pty_deliver",
                            "turn_id": result.turn_id}),
                    None,
                )?;
            }
            self.wake();
            return Ok(());
        }
        let status = match result.status.as_str() {
            "completed" | "failed" | "interrupted" => result.status.clone(),
            other => {
                return Err(Error::unknown(format!(
                    "Unexpected provider completion status: {other}"
                )))
            }
        };
        self.store.finish(
            message,
            &status,
            &json!({
                "turn_id": result.turn_id,
                "status": status,
                "text": result.text,
                "stop_reason": result.stop_reason,
                "error": result.error,
            }),
            result.error.as_deref(),
        )?;
        self.wake();
        Ok(())
    }

    /// The attention text for an unknown-outcome fence — keeps the
    /// provider's own reason when `unknown()` already recorded it
    /// (everything before the `— reconcile:` tail), so a re-stamp on
    /// relaunch-skip doesn't erase the detail the operator needs.
    fn uncertain_fence_text(&self, alias: &str) -> String {
        let detail = self
            .store
            .agent(alias)
            .ok()
            .and_then(|a| a.error)
            .and_then(|e| e.split(" — reconcile:").next().map(str::to_string))
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "Uncertain provider outcome requires review".to_string());
        format!(
            "{detail} — reconcile: \
             `cadence agent unfence {alias} --status interrupted`, then \
             `cadence agent resume {alias}`"
        )
    }

    /// An `OutcomeUnknown` never becomes a retry: mark the attempt and
    /// fence the actor for review. `reason` is the provider's own account
    /// of the uncertainty (idle window, EOF, cap exceeded, …) — the
    /// operator needs it to reconcile.
    fn unknown(&self, alias: &str, message: &Message, reason: &str) -> Result<()> {
        self.store.finish(
            message,
            "unknown",
            &json!({"status": "unknown", "text": "", "error": reason}),
            Some(reason),
        )?;
        // One write: the fence is visible immediately, so the cleared
        // endpoint must land with it — a reader in between must never
        // see `attention` plus a live endpoint.
        self.store.set_state_detached(
            alias,
            "attention",
            Some(&format!(
                "{reason} — reconcile: \
                 `cadence agent unfence {alias} --status interrupted`, then \
                 `cadence agent resume {alias}`"
            )),
        )?;
        let _ = self
            .store
            .event_public(alias, "attention", json!({"reason": reason}));
        self.wake();
        Err(Error::unknown("Uncertain provider outcome requires review"))
    }

    // ---- dispatch ----

    pub fn dispatch(self: &Arc<Self>, method: &str, params: &Value) -> Result<Value> {
        match method {
            "health" => Ok(json!({
                "state": "ready",
                "protocol": proto::PROTOCOL_VERSION,
                "capabilities": proto::capabilities(),
            })),
            // Build identity + process start — the deploy-drift check
            // measures merged commits against *this* binary's commit.
            "daemon_info" => Ok(json!({
                "build_commit": crate::overview::BUILD_COMMIT,
                "build_time": crate::overview::BUILD_TIME,
                "started_at": self.started_at,
            })),
            "shutdown" => {
                self.closing.store(true, Ordering::SeqCst);
                self.wake();
                Ok(json!({"state": "stopping"}))
            }
            "agent_register" => self.rpc_register(params),
            "agent_list" => {
                let mut agents = Vec::new();
                for agent in self.store.agents()? {
                    let mut j = agent.to_json();
                    // The alias's current non-terminal task assignments —
                    // derived from tasks.assignee, never stored.
                    let tasks: Vec<String> = self
                        .store
                        .tasks_for_assignee(&agent.alias)?
                        .iter()
                        .map(|t| t.id.clone())
                        .collect();
                    j["tasks"] = json!(tasks);
                    j["capabilities"] =
                        registry::capabilities_json(&agent.provider, &agent.endpoint_kind);
                    let (dead, resumable) = self.agent_liveness(&agent);
                    j["dead"] = json!(dead);
                    j["resumable"] = json!(resumable);
                    if let Some((silent_secs, stalled)) = self.stall_view(&agent.alias) {
                        j["silent_secs"] = json!(silent_secs);
                        j["stalled"] = json!(stalled);
                    }
                    agents.push(j);
                }
                Ok(json!({"agents": agents}))
            }
            "agent_show" => {
                let alias = self.resolve_alias(required_str(params, "alias")?)?;
                let agent = self.store.agent(&alias)?;
                let messages = self.store.messages(&alias)?;
                let mut agent_json = agent.to_json();
                agent_json["capabilities"] =
                    registry::capabilities_json(&agent.provider, &agent.endpoint_kind);
                let (dead, resumable) = self.agent_liveness(&agent);
                agent_json["dead"] = json!(dead);
                agent_json["resumable"] = json!(resumable);
                if let Some((silent_secs, stalled)) = self.stall_view(&alias) {
                    agent_json["silent_secs"] = json!(silent_secs);
                    agent_json["stalled"] = json!(stalled);
                }
                // The briefing lives under the state dir — actors read
                // it there, never inside their cwd repository.
                if registry::has_actor(&agent.provider, &agent.endpoint_kind) {
                    agent_json["briefing"] = json!(client::briefing_path(
                        &self.state_dir,
                        agent.params.as_ref().unwrap_or(&Value::Null),
                        &agent.alias,
                    ));
                }
                Ok(json!({
                    "agent": agent_json,
                    "messages": messages.iter().map(Message::to_json).collect::<Vec<_>>(),
                    "event_cursor": self.store.event_cursor(&alias)?,
                    // Inbound backlog — what `cadence inbox` would drain
                    // for a mailbox, what the actor will still take for
                    // a live endpoint.
                    "queued": self.store.queued_count(&alias)?,
                    // Unreconciled `unknown` count — nonzero means the
                    // agent is fenced and `message reconcile` /
                    // `agent unfence` is the only exit.
                    "unknown": self.store.unknown_messages(&alias)?.len(),
                }))
            }
            "agent_send" => self.rpc_send(params),
            "agent_ask" => self.rpc_ask(params),
            "agent_events" => self.rpc_events(params),
            "agent_requests" => {
                let alias = self.resolve_alias(required_str(params, "alias")?)?;
                let requests = self
                    .pending
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(_, req)| req.alias == alias)
                    .map(|(handle, req)| {
                        json!({"request": handle, "method": req.method, "params": req.params})
                    })
                    .collect::<Vec<_>>();
                Ok(json!({"requests": requests}))
            }
            "agent_respond" => self.rpc_respond(params),
            "request_open" => self.rpc_request_open(params),
            "request_wait" => self.rpc_request_wait(params),
            "request_close" => self.rpc_request_close(params),
            "agent_ready" => self.rpc_ready(params),
            "agent_capture" => self.rpc_capture(params),
            "agent_probe" => self.rpc_probe(params),
            "agent_set" => self.rpc_set(params),
            "agent_inbox" => self.rpc_inbox(params),
            "message_report" => self.rpc_message_report(params),
            "message_reconcile" => self.rpc_reconcile(params),
            "message_cancel" => self.rpc_cancel(params),
            "job_new" => self.rpc_job_new(params),
            "job_list" => self.rpc_job_list(params),
            "job_show" => self.rpc_job_show(params),
            "job_events" => self.rpc_job_events(params),
            "job_cancel" => self.rpc_job_cancel(params),
            "job_close" => self.rpc_job_close(params),
            "task_new" => self.rpc_task_new(params),
            "task_show" => self.rpc_task_show(params),
            "task_dispatch" => self.rpc_task_dispatch(params),
            "task_verdict" => self.rpc_task_verdict(params),
            "task_accept" => self.rpc_task_accept(params),
            "task_sha" => self.rpc_task_sha(params),
            "task_fail" => self.rpc_task_fail(params),
            "task_reopen" => self.rpc_task_reopen(params),
            "task_cancel" => self.rpc_task_cancel(params),
            "agent_unfence" => self.rpc_unfence(params),
            "agent_stop" => self.rpc_stop(params),
            "agent_remove" => {
                let alias = self.resolve_alias(required_str(params, "alias")?)?;
                let agent = self.store.agent(&alias)?;
                // A live endpoint means an actor is serving it — the
                // operator must stop it first. Endpoint is checked
                // before ownership so the error suggests the remedy.
                // Inbox pseudo-endpoints are permanent mailboxes, not
                // processes — removal is the only lifecycle they have.
                if agent.endpoint.is_some()
                    && registry::has_actor(&agent.provider, &agent.endpoint_kind)
                {
                    return Err(Error::rejected(format!(
                        "Agent '{alias}' still has a live endpoint — \
                         run `cadence agent stop {alias}` first"
                    )));
                }
                {
                    let lc = self.lifecycle.lock().unwrap();
                    if lc.owned(&alias) {
                        return Err(Error::rejected(format!(
                            "Agent '{alias}' is still owned by a live actor — \
                             run `cadence agent stop {alias}` first"
                        )));
                    }
                    // A fenced pty pane may still be alive — remove is
                    // the explicit kill; never leave an orphan session
                    // on the private socket behind a dropped row.
                    if agent.endpoint_kind == "pty" {
                        adapter::pty::kill_pane(&self.state_dir, &alias, &self.provider_env);
                    }
                    self.open_attach.lock().unwrap().remove(&alias);
                    // Re-checks endpoint/state inside its transaction.
                    self.store.remove_agent(&alias)?;
                }
                self.wake();
                Ok(json!({"alias": alias, "state": "removed"}))
            }
            "agent_gc" => {
                let older_than = params.get("older_than").and_then(Value::as_f64);
                let candidates = self.store.gc_candidates(older_than)?;
                let mut removed = Vec::new();
                {
                    let lc = self.lifecycle.lock().unwrap();
                    for agent in candidates {
                        // Skip an alias owned mid-transition rather than
                        // failing the whole sweep.
                        if lc.owned(&agent.alias) {
                            continue;
                        }
                        // A fenced pty pane may still be alive — gc is
                        // the explicit kill; no orphan sessions behind
                        // dropped rows.
                        if agent.endpoint_kind == "pty" {
                            adapter::pty::kill_pane(
                                &self.state_dir,
                                &agent.alias,
                                &self.provider_env,
                            );
                        }
                        if self.store.remove_agent(&agent.alias).is_ok() {
                            self.open_attach.lock().unwrap().remove(&agent.alias);
                            removed.push(agent.alias);
                        }
                    }
                }
                self.wake();
                Ok(json!({"removed": removed}))
            }
            "agent_resume" => {
                let alias = self.resolve_alias(required_str(params, "alias")?)?;
                let started = self.try_resume(&alias)?;
                let state = if started { "starting" } else { "attention" };
                Ok(json!({"alias": alias, "state": state}))
            }
            other => Err(Error::rejected(format!("Unknown method '{other}'"))),
        }
    }

    fn rpc_register(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = required_str(params, "alias")?;
        let provider = required_str(params, "provider")?;
        let endpoint =
            optional_str(params, "endpoint_kind").unwrap_or(registry::DEFAULT_ENDPOINT_KIND);
        let role = optional_str(params, "role").unwrap_or("worker");
        let cwd = optional_str(params, "cwd");
        let sandbox = optional_str(params, "sandbox").unwrap_or("read-only");
        let instructions = optional_str(params, "instructions");
        let agent_params = optional_str(params, "params");
        if registry::is_inbox_kind(endpoint) != registry::is_inbox_provider(provider) {
            return Err(Error::rejected(
                "Provider 'inbox' and endpoint kind 'inbox' must be used together",
            ));
        }
        // A mailbox never runs a process — its cwd is bookkeeping only,
        // so direct socket callers may omit it (the CLI defaults cwd).
        let cwd = match (cwd, endpoint) {
            (Some(cwd), _) => std::fs::canonicalize(cwd)
                .map_err(|_| Error::rejected("Working directory must exist"))?,
            (None, e) if !registry::has_actor(provider, e) => self.state_dir.clone(),
            (None, _) => return Err(Error::rejected("Missing 'cwd'")),
        };
        // Enumerated launch params are validated at the door — a bad
        // value rejected here never lands on the agent row to be
        // replayed into a provider argv on every resume.
        if let Some(raw) = agent_params {
            let parsed: Value = serde_json::from_str(raw)
                .map_err(|_| Error::rejected("'params' must be a JSON object"))?;
            registry::validate_launch_params(provider, endpoint, &parsed)?;
        }
        self.store.register_agent(&crate::store::NewAgent {
            alias,
            provider,
            endpoint_kind: endpoint,
            role,
            cwd: &cwd.to_string_lossy(),
            sandbox,
            instructions,
            params: agent_params,
        })?;
        // A mailbox has no actor — it is `idle` with its pseudo-endpoint
        // from registration and simply accrues queued messages.
        if !registry::has_actor(provider, endpoint) {
            return Ok(json!({
                "alias": alias, "state": "idle", "provider": provider,
                "endpoint": format!("inbox://{alias}"),
            }));
        }
        self.launch_actor(alias)?;
        Ok(json!({"alias": alias, "state": "starting", "provider": provider}))
    }

    fn rpc_send(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let text = required_str(params, "text")?;
        // An explicit reply_to always wins; absent one, a worker joined
        // to a group (params.upstream) reports results to its PM by
        // default. `enqueue` still validates the target.
        let reply_to = optional_str(params, "reply_to")
            .map(str::to_string)
            .or_else(|| self.upstream_of(&alias));
        let message = optional_str(params, "message")
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        // Caller-supplied provenance (`bootstrap` from join, etc.).
        // Identifier-charset only — internal sources like
        // `worker_result` contain characters this rejects, so the
        // internal routing contract cannot be forged through agent_send.
        let source = optional_str(params, "source").unwrap_or("user");
        proto::identifier(source, "Message source")?;
        // `send --task` attaches the delivery to a task — ad-hoc
        // PM↔worker follow-up inside a job's delivery record.
        let task = optional_str(params, "task");
        let (duplicate, state) =
            self.store
                .enqueue_task(&alias, text, reply_to.as_deref(), &message, source, task)?;
        self.notify_agent(&alias);
        self.wake();
        Ok(json!({"message": message, "state": state, "duplicate": duplicate}))
    }

    /// Send and wait for the message's terminal state, bounded by `wait`.
    fn rpc_ask(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let wait = optional_u64(params, "wait").unwrap_or(120).min(600);
        let result = self.rpc_send(params)?;
        let message = result["message"].as_str().unwrap_or_default().to_string();
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            let stored = self
                .store
                .message(&message)?
                .ok_or_else(|| Error::internal("Message vanished"))?;
            if is_terminal(&stored.state) || Instant::now() >= deadline {
                return Ok(stored.to_json());
            }
            self.changed
                .wait_until(Instant::now() + Duration::from_millis(250));
        }
    }

    fn rpc_events(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let after = optional_i64(params, "after").unwrap_or(0);
        if after < 0 {
            return Err(Error::rejected("Event cursor must be nonnegative"));
        }
        let wait = optional_u64(params, "wait").unwrap_or(0).min(30);
        // `tail` is the newest-first default `cadence events` uses
        // when no --after cursor is given: one bounded page ending at
        // the latest seq, oldest first within it, plus the forward
        // cursor and whether older history exists below the page.
        if params.get("tail").and_then(Value::as_bool).unwrap_or(false) {
            let mut events = self.store.events_tail(&alias, 51)?;
            let has_older = events.len() > 50;
            events.truncate(50);
            return Ok(json!({
                "events": events.iter().map(crate::store::Event::to_json).collect::<Vec<_>>(),
                "cursor": events.last().map(|e| e.seq).unwrap_or(0),
                "has_older": has_older,
            }));
        }
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            let events = self.store.events(&alias, after, 100)?;
            if !events.is_empty() || self.closing.load(Ordering::SeqCst) {
                let cursor = events.last().map(|e| e.seq).unwrap_or(after);
                return Ok(json!({
                    "events": events.iter().map(crate::store::Event::to_json).collect::<Vec<_>>(),
                    "cursor": cursor,
                }));
            }
            if Instant::now() >= deadline {
                return Ok(json!({"events": [], "cursor": after}));
            }
            let step = deadline.min(Instant::now() + Duration::from_secs(1));
            self.changed.wait_until(step);
        }
    }

    fn rpc_respond(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let handle = required_str(params, "request")?;
        let decision = optional_str(params, "decision");
        // Explicit JSON null means "not provided".
        let answers = params.get("answers").filter(|a| !a.is_null()).cloned();
        // Operator note carried on a brokered decline — the MCP server
        // hands it to the provider as the denial message.
        let reason = optional_str(params, "reason").map(str::to_string);
        // Claim the handle atomically: whichever path removes it first
        // — this respond, an external `serverRequest/resolved`, or the
        // actor's exit sweep — owns the answer, and every other path
        // sees "no longer pending". Validation runs under the same
        // lock so a malformed respond leaves the request pending
        // instead of consuming it. No I/O happens while the lock is
        // held. A miss falls to the spec's rejection hint where one is
        // configured (managed claude without --broker-approvals).
        enum Claim {
            /// A provider-originated request: answer over the adapter.
            Provider(Value, Value),
            /// A `request_open` brokered request: the answer is parked
            /// for the blocked `request_wait` caller instead.
            Brokered,
        }
        let claim = {
            let mut map = self.pending.lock().unwrap();
            let req = map.get(handle).filter(|req| req.alias == alias);
            let Some(req) = req else {
                drop(map);
                let agent = self.store.agent(&alias)?;
                return Err(Error::rejected(
                    registry::respond_rejection(&agent.provider, &agent.endpoint_kind)
                        .unwrap_or("Request is no longer pending for this agent"),
                ));
            };
            let method = req.method.clone();
            if method.starts_with("cadence/") {
                let answer = match decision {
                    Some("accept") if answers.is_none() => json!({"decision": "accept"}),
                    Some("decline") if answers.is_none() => {
                        json!({"decision": "decline", "reason": reason})
                    }
                    _ => return Err(Error::rejected("Respond with decision accept or decline")),
                };
                // Park the answer before dropping the pending entry —
                // `request_wait` treats a missing handle as closed, so
                // the mailbox must be filled first or an accept could
                // surface as a denial.
                self.answered
                    .lock()
                    .unwrap()
                    .insert(handle.to_string(), (alias.clone(), answer));
                map.remove(handle);
                Claim::Brokered
            } else {
                let request_id = req.id.clone();
                let request_params = req.params.clone();
                let response = match method.as_str() {
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                    match decision {
                        Some("accept") | Some("decline") if answers.is_none() => {
                            json!({"decision": decision.unwrap()})
                        }
                        _ => return Err(Error::rejected("Respond with decision accept or decline")),
                    }
                }
                "item/tool/requestUserInput" => {
                    if decision.is_some() || !answers.as_ref().is_some_and(Value::is_object) {
                        return Err(Error::rejected("Respond with an answers object"));
                    }
                    json!({"answers": answers.unwrap()})
                }
                "session/request_permission" => match decision {
                    Some("decline") if answers.is_none() => {
                        json!({"outcome": {"outcome": "cancelled"}})
                    }
                    Some("accept") if answers.is_none() => {
                        let option = request_params
                            .get("options")
                            .and_then(Value::as_array)
                            .and_then(|options| {
                                options.iter().find(|o| {
                                    o.get("kind").and_then(Value::as_str) == Some("allow_once")
                                })
                            })
                            .ok_or_else(|| {
                                Error::rejected("Provider did not offer an allow-once option")
                            })?;
                        json!({"outcome": {"outcome": "selected", "optionId": option["optionId"]}})
                    }
                    _ => return Err(Error::rejected("Respond with decision accept or decline")),
                },
                _ => return Err(Error::rejected(
                    "This request type is not supported; stop the agent or use the provider directly",
                )),
                };
                map.remove(handle);
                Claim::Provider(request_id, response)
            }
        };
        match claim {
            Claim::Brokered => {}
            Claim::Provider(request_id, response) => {
                let adapter = self
                    .lifecycle
                    .lock()
                    .unwrap()
                    .agents
                    .get(&alias)
                    .and_then(|ctl| ctl.adapter.lock().unwrap().clone());
                let adapter = adapter.ok_or_else(|| {
                    Error::internal("Agent adapter is not available for this request")
                })?;
                adapter.respond(&request_id, response)?;
            }
        }
        self.relax_waiting(&alias);
        let _ = self
            .store
            .event_public(&alias, "input_answered", json!({"request": handle}));
        self.wake();
        Ok(json!({"state": "answered"}))
    }

    /// `request_open` — a brokered request raised by an external
    /// requester (the `cadence mcp-permission` server a brokered
    /// claude launches) rather than by the provider adapter itself.
    /// Same model as `on_provider_request`: durable through the event
    /// log, visible via `agent_requests`, and holding the agent in
    /// `waiting_input` until `agent respond` answers it.
    fn rpc_request_open(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let agent = self.store.agent(&alias)?;
        let brokered = agent
            .params
            .as_ref()
            .and_then(|p| p.get("broker_approvals"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !brokered {
            return Err(Error::rejected(format!(
                "Agent '{alias}' was not launched with --broker-approvals — \
                 its permission prompts are not brokered"
            )));
        }
        let kind = optional_str(params, "kind").unwrap_or("approval");
        proto::identifier(kind, "Request kind")?;
        let tool = required_str(params, "tool")?;
        let input_summary = optional_str(params, "input_summary").unwrap_or_default();
        let input = params.get("input").cloned().unwrap_or(Value::Null);
        // The caller may name the handle (the mcp-permission server
        // mints one per tool call): a retry after a lost response then
        // re-opens the SAME request — no duplicate pending entry, no
        // second event, no second PM notice.
        let handle = match optional_str(params, "request") {
            Some(h) => proto::identifier(h, "Request handle")?,
            None => Uuid::new_v4().simple().to_string(),
        };
        {
            let pending = self.pending.lock().unwrap();
            if let Some(req) = pending.get(&handle) {
                if req.alias == alias {
                    return Ok(json!({"request": handle, "state": "waiting_input",
                                     "existing": true}));
                }
                return Err(Error::rejected(
                    "Request handle is already pending for another agent",
                ));
            }
        }
        self.pending.lock().unwrap().insert(
            handle.clone(),
            PendingRequest {
                alias: alias.clone(),
                // No provider request id — the answer parks in
                // `answered` for `request_wait`, never `adapter.respond`.
                id: Value::Null,
                method: format!("cadence/{kind}"),
                params: json!({"kind": kind, "tool": tool,
                               "input_summary": input_summary, "input": input}),
            },
        );
        // Requests only arrive mid-turn; relax/stop may have moved the
        // agent on already — never clobber a non-busy state.
        let _ = self
            .store
            .set_agent_state_if(&alias, "waiting_input", "busy");
        let _ = self.store.event_public(
            &alias,
            "request_opened",
            json!({"request": handle, "kind": kind, "tool": tool,
                   "input_summary": input_summary}),
        );
        // An upstream PM gets exactly one notice naming the agent and
        // the respond command — deterministic id, so a retried open
        // can never double-notify.
        if let Some(pm) = self.upstream_of(&alias) {
            let delivery = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("cadence-notice:request:{handle}").as_bytes(),
            )
            .simple()
            .to_string();
            let payload = json!({"request": handle, "worker": alias,
                                 "tool": tool, "input_summary": input_summary});
            let body = format!(
                "A managed worker is waiting on a tool-permission decision. This is an \
                 informational notice, not a result; do not treat it as worker output. \
                 Answer it with `cadence agent respond {alias} --request {handle} \
                 --decision accept|decline [--reason \"why\"]` — `cadence agent requests \
                 {alias}` shows the full input. {payload}"
            );
            if self
                .store
                .enqueue_task(&pm, &body, None, &delivery, "worker_notice", None)
                .is_ok()
            {
                self.notify_agent(&pm);
            }
        }
        // An open request counts as provider activity — a worker
        // waiting on a human is not idle.
        if let Ok(adapter) = self.adapter_for(&alias) {
            adapter.note_activity();
        }
        self.wake();
        Ok(json!({"request": handle, "state": "waiting_input"}))
    }

    /// `request_wait` — block until a brokered request is answered,
    /// closed, or the caller's slice expires. `request_wait` callers
    /// re-issue until their own deadline; each pass stamps provider
    /// activity so a human's thinking time is never an idle fence.
    fn rpc_request_wait(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let handle = required_str(params, "request")?;
        let wait = optional_u64(params, "wait").unwrap_or(60).min(120);
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            let alias = self
                .pending
                .lock()
                .unwrap()
                .get(handle)
                .map(|req| req.alias.clone());
            let Some(alias) = alias else {
                // The handle is gone — `agent respond` parks the
                // answer before dropping it, so the mailbox is
                // authoritative here; an actor exit sweep or a daemon
                // restart leaves it empty, which reads as closed.
                if let Some((_, answer)) = self.answered.lock().unwrap().remove(handle) {
                    return Ok(json!({"state": "answered", "answer": answer}));
                }
                return Ok(json!({"state": "closed",
                                 "reason": "request is not pending"}));
            };
            if let Ok(adapter) = self.adapter_for(&alias) {
                adapter.note_activity();
            }
            if self.closing.load(Ordering::SeqCst) {
                return Ok(json!({"state": "closed", "reason": "daemon shutting down"}));
            }
            if Instant::now() >= deadline {
                return Ok(json!({"state": "waiting"}));
            }
            self.changed
                .wait_until(Instant::now() + Duration::from_millis(250));
        }
    }

    /// `request_close` — the requester's local deadline fired: retire
    /// the pending handle so the agent leaves `waiting_input` and
    /// `agent_requests` drains. An answer parked at the boundary still
    /// lands — `agent respond` fills the mailbox before dropping the
    /// pending entry, so a close that finds it returns `answered`.
    fn rpc_request_close(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let handle = required_str(params, "request")?;
        let alias = {
            let mut pending = self.pending.lock().unwrap();
            // Same lock order as respond (pending → answered): a
            // respond mid-flight holds pending through its mailbox
            // insert, so whichever we observe here is final.
            if let Some((alias, answer)) = self.answered.lock().unwrap().remove(handle) {
                pending.remove(handle);
                drop(pending);
                self.relax_waiting(&alias);
                return Ok(json!({"state": "answered", "answer": answer}));
            }
            pending.remove(handle).map(|req| req.alias)
        };
        if let Some(alias) = alias {
            self.relax_waiting(&alias);
            let _ = self
                .store
                .event_public(&alias, "request_closed", json!({"request": handle}));
        }
        self.wake();
        Ok(json!({"state": "closed"}))
    }

    /// The live adapter for an alias, when an actor owns one.
    fn adapter_for(&self, alias: &str) -> Result<Arc<dyn ProviderAdapter>> {
        self.lifecycle
            .lock()
            .unwrap()
            .agents
            .get(alias)
            .and_then(|ctl| ctl.adapter.lock().unwrap().clone())
            .ok_or_else(|| {
                // A mailbox never has an adapter — name its real verb.
                if self
                    .store
                    .agent(alias)
                    .map(|a| !registry::has_actor(&a.provider, &a.endpoint_kind))
                    .unwrap_or(false)
                {
                    Error::rejected(format!(
                        "Agent '{alias}' is an inbox — no live endpoint; \
                         `cadence inbox {alias}` drains the queue"
                    ))
                } else {
                    Error::rejected("Agent has no live endpoint (not running?)")
                }
            })
    }

    /// Operator readiness claim for gated endpoints (pty): asserts the
    /// terminal was inspected and is idle with an empty input. Single
    /// use, short TTL — see the adapter for semantics. `by` carries the
    /// claimer's `CADENCE_ALIAS` when the call came from inside a pane —
    /// recorded for audit (G5 policy stays open; the record exists).
    fn rpc_ready(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let by = optional_str(params, "by").map(str::to_string);
        let force = params
            .get("force")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // The claim itself runs the idle probe and refuses a busy
        // pane — `force` is the operator's explicit override and is
        // recorded as such on the event.
        let probe = self.adapter_for(&alias)?.claim_ready(by.clone(), force)?;
        let mut detail = json!({
            "by": by.unwrap_or_else(|| "operator".to_string()),
            "probe": probe.to_json(),
        });
        if force {
            detail["forced"] = json!(true);
        }
        let _ = self.store.event_public(&alias, "ready_claimed", detail);
        // Wake the actor's gate wait — a claim should release the head
        // message immediately, not on the next poll tick.
        self.notify_agent(&alias);
        self.wake();
        Ok(json!({"alias": alias, "state": "ready-claimed"}))
    }

    /// Screen contents of a PTY endpoint for operator inspection.
    fn rpc_capture(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let text = self.adapter_for(&alias)?.capture()?;
        Ok(json!({"alias": alias, "capture": text}))
    }

    /// Screen probe for a PTY endpoint — the same reduction the
    /// verified auto-claim gate uses.
    fn rpc_probe(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let probe = self.adapter_for(&alias)?.probe()?;
        let mut out = probe.to_json();
        out["alias"] = json!(alias);
        Ok(out)
    }

    /// Merge key=value pairs into an agent's stored params — how an
    /// existing agent opts into `auto_ready=verified` post-launch.
    fn rpc_set(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let patch = params
            .get("patch")
            .filter(|p| p.is_object())
            .cloned()
            .ok_or_else(|| Error::rejected("Missing 'patch' object"))?;
        // Live-mutable params are an explicit allowlist — arbitrary keys
        // like `upstream` or `session` would silently rewire routing and
        // session binding, so they are rejected rather than merged.
        let agent = self.store.agent(&alias)?;
        // `next_launch`: launch params (model, effort) stored for the
        // next open only — the live process is left exactly as it is.
        let next_launch = params
            .get("next_launch")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        for (key, value) in patch.as_object().unwrap() {
            proto::param_key(key)?;
            if next_launch {
                registry::validate_next_launch_param(
                    &agent.provider,
                    &agent.endpoint_kind,
                    key,
                    value,
                )?;
            } else {
                registry::validate_live_param(&agent.provider, &agent.endpoint_kind, key, value)?;
            }
        }
        self.store.set_params(&alias, &patch)?;
        if next_launch {
            return Ok(json!({"alias": alias, "state": "updated", "applies": "next launch"}));
        }
        // Push the merged params into the live adapter so cached
        // endpoint options (auto_ready) take effect without a restart.
        if let Ok(adapter) = self.adapter_for(&alias) {
            if let Some(params) = self.store.agent(&alias)?.params {
                adapter.update_params(&params);
            }
        }
        // Wake a gate wait — new params may be exactly what it needs.
        self.notify_agent(&alias);
        self.wake();
        Ok(json!({"alias": alias, "state": "updated"}))
    }

    /// Drain an inbox agent's durable queue — messages complete
    /// `via=inbox_read` as they are returned. `wait` long-polls on the
    /// daemon's change signal, the same mechanism `events` uses.
    fn rpc_inbox(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let after = optional_i64(params, "after").unwrap_or(0);
        let wait = optional_u64(params, "wait").unwrap_or(0).min(30);
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            let messages = self.store.inbox_drain(&alias, after)?;
            if !messages.is_empty() || self.closing.load(Ordering::SeqCst) {
                // Consuming a message with a return address routed its
                // result — the target actor must not wait out its poll.
                for m in &messages {
                    if let Some(target) = &m.reply_to {
                        self.notify_agent(target);
                    }
                }
                self.wake();
                let cursor = messages.last().map(|m| m.seq).unwrap_or(after);
                return Ok(json!({
                    "messages": messages.iter().map(Message::to_json).collect::<Vec<_>>(),
                    "cursor": cursor,
                }));
            }
            if Instant::now() >= deadline {
                return Ok(json!({"messages": [], "cursor": after}));
            }
            let step = deadline.min(Instant::now() + Duration::from_secs(1));
            self.changed.wait_until(step);
        }
    }

    /// Explicit ack/result report for a submitted PTY message. The
    /// `token` is the `turn_id` minted at submission; it embeds the
    /// endpoint generation, so a report aimed at a previous pane life
    /// is rejected as stale. Callers are identified by possession of
    /// the token, which is self-asserted — not an authentication.
    fn rpc_message_report(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let id = required_str(params, "message")?;
        let token = required_str(params, "token")?;
        let kind = required_str(params, "kind")?;
        let text = optional_str(params, "text");
        let message = self
            .store
            .message(id)?
            .ok_or_else(|| Error::rejected("Unknown message"))?;
        let agent = self.store.agent(&message.alias)?;
        if message.turn_id.as_deref() != Some(token) {
            return Err(Error::rejected(
                "Token does not match the message's submission token",
            ));
        }
        let stale = match &agent.generation {
            Some(gen) => !token.starts_with(&format!("pty-{gen}-")),
            None => true,
        };
        if stale {
            return Err(Error::rejected(
                "Submission token belongs to a stale endpoint generation",
            ));
        }
        // A valid ack/result report is explicit agent activity — it
        // feeds the stall clock for every endpoint kind.
        self.bump_activity(&message.alias);
        match kind {
            "ack" => {
                self.store.mark_ack(&message, text)?;
            }
            "result" => {
                let text = text.ok_or_else(|| Error::rejected("A result report requires text"))?;
                // `--sha` binds the report to an exact commit — the
                // verdict protocol requires it on task-attached work.
                let sha = optional_str(params, "sha")
                    .map(store::check_commit_sha)
                    .transpose()?;
                if message.state == "running" {
                    self.store.finish(
                        &message,
                        "completed",
                        &json!({
                            "status": "completed", "text": text,
                            "turn_id": token, "via": "pty_report",
                            "sha": sha,
                        }),
                        None,
                    )?;
                } else if message.state == "completed" {
                    // Idempotent retry vs conflicting duplicate.
                    let same = message
                        .result
                        .as_ref()
                        .and_then(|r| r.get("text"))
                        .and_then(Value::as_str)
                        == Some(text);
                    if !same {
                        return Err(Error::rejected(
                            "Message already completed with a different result",
                        ));
                    }
                    return Ok(json!({"state": "completed", "duplicate": true}));
                } else {
                    return Err(Error::rejected(format!(
                        "Message is not awaiting a report (state {})",
                        message.state
                    )));
                }
            }
            other => {
                return Err(Error::rejected(format!(
                    "Report kind must be ack or result, not '{other}'"
                )))
            }
        }
        self.wake();
        Ok(json!({"state": "reported", "kind": kind}))
    }

    /// Operator reconcile of an `unknown` message — no turn token, the
    /// token is stale by definition when a message is `unknown`. The
    /// store transaction enforces unknown-only; `completed`/`failed`
    /// route `reply_to`, `interrupted` routes nothing.
    fn rpc_reconcile(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let message_id = required_str(params, "message")?;
        let status = required_str(params, "status")?;
        let note = optional_str(params, "note");
        let by = optional_str(params, "by").unwrap_or("operator");
        // An operator-stated SHA on a `completed` reconcile is bound
        // like a worker's `--sha` — explicit, never inferred.
        let sha = optional_str(params, "sha");
        let message = self.store.reconcile(message_id, status, note, by, sha)?;
        self.wake();
        Ok(json!({"state": "reconciled", "message": message.to_json()}))
    }

    /// Cancel a still-`queued` message — never delivered. The store's
    /// state-guarded UPDATE makes the cancel atomic against an actor's
    /// `take_queued` claim; a `reply_to` gets one `worker_notice` so a
    /// waiter isn't left hanging. `wake()` so a pending ask waiter sees
    /// the terminal state promptly.
    fn rpc_cancel(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let message_id = required_str(params, "message")?;
        let by = optional_str(params, "by").unwrap_or("operator");
        let reason = optional_str(params, "reason");
        let message = self.store.cancel(message_id, by, reason)?;
        self.wake();
        Ok(json!({"state": "cancelled", "message": message.to_json()}))
    }

    /// The resume path shared by `agent_resume` and `agent_unfence`:
    /// inbox guard, ownership/fence checks, then start the actor.
    /// Returns whether the actor started — a failed start records
    /// `attention` instead.
    fn try_resume(self: &Arc<Self>, alias: &str) -> Result<bool> {
        let agent = self.store.agent(alias)?;
        if !registry::has_actor(&agent.provider, &agent.endpoint_kind) {
            return Err(Error::rejected(format!(
                "Agent '{alias}' is an inbox — nothing to resume; \
                 `cadence inbox {alias}` drains it"
            )));
        }
        let mut lc = self.lifecycle.lock().unwrap();
        if lc.owned(alias) {
            // Distinguish the two owned cases for the operator: a live
            // actor means "attach" (fake/managed actors may carry no
            // endpoint address, so state is the signal), a
            // starting/stopping one means "retry".
            let live = agent.endpoint.is_some()
                || matches!(agent.state.as_str(), "idle" | "running" | "waiting_input");
            return if live {
                Err(Error::rejected(format!(
                    "Agent '{alias}' is already live — attach with \
                     `cadence attach {alias}`"
                )))
            } else {
                Err(Error::rejected(format!(
                    "Agent '{alias}' is still starting or stopping — \
                     retry shortly"
                )))
            };
        }
        // An unreconciled `unknown` fences the agent — the exit is an
        // explicit operator reconcile, not another resume (which would
        // fail closed anyway inside start_actor).
        if self.store.has_unknown(alias)? {
            return Err(Error::rejected(format!(
                "Agent '{alias}' is fenced by an unreconciled unknown \
                 message — reconcile it first: `cadence agent unfence \
                 {alias} --status interrupted`, then `cadence agent \
                 resume {alias}`"
            )));
        }
        // Enable only after the ownership/fence checks pass — a
        // rejected resume must leave no side effects behind.
        self.start_actor_locked(&mut lc, alias, true)
    }

    /// Bounded wait for a just-started actor's `open`: live when an
    /// endpoint is published or a live state lands, over when the
    /// actor gives up (`attention`/`stopped`/`offline`). Same 30s
    /// bound the CLI's resume polling uses. Returns `(live, state)`.
    fn wait_open_outcome(&self, alias: &str) -> (bool, String) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let agent = match self.store.agent(alias) {
                Ok(a) => a,
                Err(_) => return (false, "gone".to_string()),
            };
            let live = agent.endpoint.is_some()
                || matches!(agent.state.as_str(), "idle" | "running" | "waiting_input");
            if live {
                return (true, agent.state);
            }
            if matches!(agent.state.as_str(), "attention" | "stopped" | "offline") {
                return (false, agent.state);
            }
            if Instant::now() >= deadline {
                return (false, agent.state);
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// `dead` is per endpoint kind, not "no endpoint string":
    /// attachable kinds (pty, managed-ws) are dead when registered,
    /// not operator-stopped, and holding no live endpoint; managed
    /// kinds are dead when fenced or enabled-but-unattended; inbox and
    /// fake never die. `resumable` is the question `dead` was being
    /// asked: stopped-or-dead with a saved native thread and no
    /// unreconciled unknowns fencing it.
    fn agent_liveness(&self, agent: &Agent) -> (bool, bool) {
        let dead = if registry::attachable(&agent.provider, &agent.endpoint_kind) {
            agent.endpoint.is_none() && agent.state != "stopped"
        } else if agent.endpoint_kind == "managed" {
            agent.state == "attention"
                || (agent.enabled && !self.lifecycle.lock().unwrap().owned(&agent.alias))
        } else {
            false
        };
        let resumable = (agent.state == "stopped" || dead)
            && agent.thread_id.as_deref().is_some_and(|t| !t.is_empty())
            && !self.store.has_unknown(&agent.alias).unwrap_or(false);
        (dead, resumable)
    }

    fn rpc_unfence(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let status = required_str(params, "status")?;
        let note = optional_str(params, "note");
        let by = optional_str(params, "by").unwrap_or("operator");
        // Resolve the agent before any reconcile so a bad alias fails
        // without side effects.
        let agent = self.store.agent(&alias)?;
        let ids = self.store.unknown_messages(&alias)?;
        if ids.is_empty() {
            return Err(Error::rejected(format!(
                "Agent '{alias}' has no unknown messages to reconcile \
                 — use `cadence agent resume {alias}`"
            )));
        }
        let mut reconciled = Vec::new();
        for id in &ids {
            self.store.reconcile(id, status, note, by, None)?;
            reconciled.push(id.clone());
        }
        // `resume` is opt-in over the socket — the CLI's `agent unfence`
        // passes it unless `--no-resume`, keeping the bare-RPC call a
        // reconcile-only operation.
        let resume = params
            .get("resume")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let pty = agent.endpoint_kind == "pty";
        let mut out = json!({"alias": alias, "reconciled": reconciled});
        if !resume {
            out["resumed"] = json!(false);
            if pty {
                out["pane"] = json!("none");
            }
            out["state"] = json!(self.store.agent(&alias)?.state);
            self.wake();
            return Ok(out);
        }
        // The settled default: reconcile then bring the agent back,
        // and say what the endpoint actually did — adopted the
        // surviving pane, respawned on the recorded session, or
        // nothing because the resume failed. A resume rejection is
        // reported, not thrown: the reconcile already committed.
        let (resumed, state) = match self.try_resume(&alias) {
            Ok(true) => self.wait_open_outcome(&alias),
            Ok(false) => (false, self.store.agent(&alias)?.state),
            Err(e) => {
                out["resumed"] = json!(false);
                if pty {
                    out["pane"] = json!("none");
                }
                out["state"] = json!(self.store.agent(&alias)?.state);
                out["error"] = json!(e.to_string());
                self.wake();
                return Ok(out);
            }
        };
        out["resumed"] = json!(resumed);
        if pty {
            let pane = if resumed {
                self.open_attach
                    .lock()
                    .unwrap()
                    .get(&alias)
                    .copied()
                    .unwrap_or("respawned")
            } else {
                "none"
            };
            out["pane"] = json!(pane);
        }
        out["state"] = json!(state);
        self.wake();
        Ok(out)
    }

    // ---- Jobs: the work axis (docs/JOBS.md) ----

    /// `job new` — bookkeeping, not spawning. Requires a registered PM
    /// (an inbox alias is a legitimate PM — notifications drain through
    /// `cadence inbox`) and a readable spec the caller already hashed.
    fn rpc_job_new(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let pm = self.resolve_alias(required_str(params, "pm")?)?;
        let spec = required_str(params, "spec")?;
        let spec_sha256 = required_str(params, "spec_sha256")?;
        let id = optional_str(params, "job")
            .map(str::to_string)
            .unwrap_or_else(|| format!("job-{}", &Uuid::new_v4().simple().to_string()[..8]));
        let (duplicate, job) = self.store.create_job(
            &id,
            optional_str(params, "title"),
            spec,
            spec_sha256,
            &pm,
            optional_str(params, "issue"),
            optional_str(params, "repo"),
            optional_str(params, "base_ref"),
            optional_i64(params, "max_revisions").unwrap_or(2),
            optional_i64(params, "stall_secs"),
            optional_str(params, "task_title"),
            optional_str(params, "task_worktree"),
            optional_str(params, "task_branch"),
            optional_str(params, "task_base_sha"),
            optional_str(params, "task_assignee"),
        )?;
        self.wake();
        Ok(json!({"job": job.to_json(), "duplicate": duplicate}))
    }

    fn rpc_job_list(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let jobs = self.store.jobs(
            optional_str(params, "state"),
            params.get("all").and_then(Value::as_bool).unwrap_or(false),
        )?;
        let mut out = Vec::new();
        for job in jobs {
            let mut j = job.to_json();
            let mut counts: std::collections::BTreeMap<String, i64> =
                std::collections::BTreeMap::new();
            for task in self.store.tasks_for_job(&job.id)? {
                *counts.entry(task.state.clone()).or_insert(0) += 1;
            }
            j["tasks"] = json!(counts);
            out.push(j);
        }
        Ok(json!({"jobs": out}))
    }

    /// `job show` — job + tasks with live kickoff state, latest verdict
    /// and lazily-computed drift. No startup reconciliation runs: the
    /// read side flags a task whose kickoff ended without completing,
    /// a dead assignee, a missing SHA, or a spec that drifted since
    /// `job new` hashed it.
    fn rpc_job_show(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let id = required_str(params, "job")?;
        let job = self.store.job(id)?;
        let mut jj = job.to_json();
        if let Some(sha) = &job.spec_sha256 {
            match sha256_file(std::path::Path::new(&job.spec_path)) {
                Some(now) if now != *sha => {
                    jj["attention"] = json!(
                        "spec changed since `job new` — \
                        re-create the job if the drift is real work"
                    );
                }
                None => {
                    jj["attention"] = json!(format!("spec {} is unreadable", job.spec_path));
                }
                _ => {}
            }
        }
        let mut tasks = Vec::new();
        for task in self.store.tasks_for_job(id)? {
            tasks.push(self.task_json(&task)?);
        }
        jj["tasks"] = json!(tasks);
        Ok(json!({"job": jj}))
    }

    /// One task rendered for show/list: the row plus live kickoff
    /// state, drift flags and the latest verdict.
    fn task_json(self: &Arc<Self>, task: &store::Task) -> Result<Value> {
        let mut j = task.to_json();
        if let Some(mid) = &task.dispatch_message {
            match self.store.message(mid)? {
                Some(m) => {
                    j["kickoff"] = json!({"id": m.id, "state": m.state, "turn_id": m.turn_id});
                    if m.state == "running" {
                        if let Some(assignee) = &task.assignee {
                            if let Some((silent_secs, stalled)) = self.stall_view(assignee) {
                                j["silent_secs"] = json!(silent_secs);
                                j["stalled"] = json!(stalled);
                            }
                        }
                    }
                    if matches!(task.state.as_str(), "dispatched" | "running")
                        && is_terminal(&m.state)
                        && m.state != "completed"
                    {
                        let assignee = task.assignee.as_deref().unwrap_or("?");
                        j["attention"] = if m.state == "unknown" {
                            json!(format!(
                                "kickoff {mid} went unknown — the worker is fenced. \
                                 `cadence agent unfence {assignee} --status interrupted`, \
                                 `cadence agent resume {assignee}`, then `cadence job \
                                 dispatch {}` starts the next revision",
                                task.id
                            ))
                        } else {
                            json!(format!(
                                "kickoff {mid} ended '{}' — `cadence job dispatch {}` \
                                 starts revision {}",
                                m.state,
                                task.id,
                                task.revision + 1
                            ))
                        };
                    }
                }
                None => {
                    j["attention"] = json!(format!(
                        "dispatch message {mid} is gone — history is incomplete"
                    ));
                }
            }
        }
        if let Some(assignee) = &task.assignee {
            if let Ok(agent) = self.store.agent(assignee) {
                if agent.endpoint.is_none()
                    && registry::has_actor(&agent.provider, &agent.endpoint_kind)
                    && !is_task_terminal(&task.state)
                {
                    j["assignee_dead"] = json!(format!(
                        "assignee {assignee} has no live endpoint — \
                         `cadence job dispatch {} --to <worker>` reassigns",
                        task.id
                    ));
                }
            }
        }
        if task.state == "review" && task.head_sha.is_none() {
            j["attention"] = json!(format!(
                "kickoff reported no SHA — `cadence job task sha {} <sha>` \
                 records it before a verdict can land",
                task.id
            ));
        }
        if let Some(v) = self.store.verdicts_for_task(&task.id)?.last() {
            j["latest_verdict"] = v.to_json();
        }
        Ok(j)
    }

    fn rpc_task_show(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let task = self.store.task(required_str(params, "task")?)?;
        let mut out = self.task_json(&task)?;
        out["messages"] = json!(self
            .store
            .messages_for_task(&task.id)?
            .iter()
            .map(Message::to_json)
            .collect::<Vec<_>>());
        out["verdicts"] = json!(self
            .store
            .verdicts_for_task(&task.id)?
            .iter()
            .map(store::Verdict::to_json)
            .collect::<Vec<_>>());
        Ok(json!({"task": out}))
    }

    fn rpc_job_events(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let job = self.store.job(required_str(params, "job")?)?;
        let after = params.get("after").and_then(Value::as_i64).unwrap_or(0);
        if after < 0 {
            return Err(Error::rejected("Event cursor must be nonnegative"));
        }
        let wait = optional_u64(params, "wait").unwrap_or(0).min(30);
        // Same `tail` contract as agent_events — the job view's
        // default page is the newest too.
        if params.get("tail").and_then(Value::as_bool).unwrap_or(false) {
            let mut events = self.store.job_events_tail(&job.id, 51)?;
            let has_older = events.len() > 50;
            events.truncate(50);
            return Ok(json!({
                "events": events.iter().map(store::Event::to_json).collect::<Vec<_>>(),
                "cursor": events.last().map(|e| e.seq).unwrap_or(0),
                "has_older": has_older,
            }));
        }
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            let events = self.store.job_events(&job.id, after, 200)?;
            if !events.is_empty() || self.closing.load(Ordering::SeqCst) {
                return Ok(json!({
                    "events": events.iter().map(store::Event::to_json).collect::<Vec<_>>(),
                    "cursor": events.last().map(|e| e.seq).unwrap_or(after),
                }));
            }
            if Instant::now() >= deadline {
                return Ok(json!({"events": [], "cursor": after}));
            }
            let step = deadline.min(Instant::now() + Duration::from_secs(1));
            self.changed.wait_until(step);
        }
    }

    fn rpc_task_new(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let job = self.store.job(required_str(params, "job")?)?;
        let id = match optional_str(params, "task") {
            Some(id) => id.to_string(),
            None => {
                // Default `<job>-t<n>` — first free index keeps ids
                // readable and collision-free.
                let existing = self.store.tasks_for_job(&job.id)?;
                let mut n = existing.len() + 1;
                loop {
                    let candidate = format!("{}-t{n}", job.id);
                    if self.store.task_opt(&candidate)?.is_none() {
                        break candidate;
                    }
                    n += 1;
                }
            }
        };
        let assignee = optional_str(params, "assignee")
            .map(|a| self.resolve_alias(a))
            .transpose()?;
        let task = self.store.create_task(
            &job.id,
            &id,
            optional_str(params, "title"),
            assignee.as_deref(),
            optional_str(params, "spec"),
            optional_str(params, "acceptance"),
            optional_str(params, "worktree"),
            optional_str(params, "branch"),
            optional_str(params, "base_sha"),
        )?;
        self.wake();
        Ok(json!({"task": task.to_json()}))
    }

    /// `job dispatch` — bookkeeping plus the kickoff enqueue in one
    /// store transaction. Does not bypass the ready gate: `--ready` is
    /// claimed client-side exactly like `send --ready`.
    fn rpc_task_dispatch(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let to = optional_str(params, "to")
            .map(|a| self.resolve_alias(a))
            .transpose()?;
        let by = optional_str(params, "by").unwrap_or("operator");
        let (task, message, duplicate, behind_dead) = self.store.dispatch_task(
            required_str(params, "task")?,
            to.as_deref(),
            optional_str(params, "message"),
            by,
        )?;
        if let Some(assignee) = &task.assignee {
            self.notify_agent(assignee);
        }
        self.wake();
        Ok(json!({"task": task.to_json(), "message": message,
                  "duplicate": duplicate, "queued_behind_dead": behind_dead}))
    }

    /// `job verdict` — reviewer identity is self-asserted on this
    /// same-host socket: inside a cadence pane the reviewer IS
    /// `CADENCE_ALIAS` (`--reviewer` and `operator` are refused there);
    /// outside, `--reviewer` is required. The store binds the verdict
    /// to `head_sha` + current revision.
    fn rpc_task_verdict(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let task_id = required_str(params, "task")?;
        let sha = required_str(params, "sha")?;
        let verdict = required_str(params, "verdict")?;
        let pane = optional_str(params, "pane");
        let claimed = optional_str(params, "reviewer");
        let reviewer = match pane {
            Some(alias) => {
                if claimed.is_some() {
                    return Err(Error::rejected(
                        "--reviewer cannot be asserted inside a cadence pane — \
                         the pane alias is the reviewer",
                    ));
                }
                if alias == "operator" {
                    return Err(Error::rejected(
                        "'operator' cannot be claimed inside a cadence pane — \
                         verdicts from the human run outside panes",
                    ));
                }
                alias.to_string()
            }
            None => claimed.map(str::to_string).ok_or_else(|| {
                Error::rejected("Outside a cadence pane, --reviewer <alias|operator> is required")
            })?,
        };
        let verify = params
            .get("verify")
            .filter(|v| !v.is_null())
            .map(|v| v.to_string());
        let (task, v) = self.store.record_verdict(
            task_id,
            sha,
            verdict,
            &reviewer,
            pane,
            optional_str(params, "evidence"),
            optional_str(params, "message"),
            optional_i64(params, "revision"),
            verify.as_deref(),
        )?;
        let job = self.store.job(&task.job_id)?;
        self.notify_agent(&job.pm_alias);
        self.wake();
        Ok(json!({"task": task.to_json(), "verdict": v.to_json()}))
    }

    fn rpc_task_accept(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let by = optional_str(params, "by").unwrap_or("operator");
        let task = self.store.accept_task(
            required_str(params, "task")?,
            optional_str(params, "merged_sha"),
            by,
        )?;
        let job = self.store.job(&task.job_id)?;
        self.notify_agent(&job.pm_alias);
        self.wake();
        Ok(json!({"task": task.to_json()}))
    }

    /// `job task sha` — the repair path for a `review` task whose
    /// kickoff reported no SHA (A3): the PM/operator records it
    /// explicitly; never inferred.
    fn rpc_task_sha(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let by = optional_str(params, "by").unwrap_or("operator");
        let task = self.store.set_task_sha(
            required_str(params, "task")?,
            required_str(params, "sha")?,
            by,
        )?;
        self.wake();
        Ok(json!({"task": task.to_json()}))
    }

    fn rpc_task_fail(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let by = optional_str(params, "by").unwrap_or("operator");
        let task = self.store.fail_task(
            required_str(params, "task")?,
            required_str(params, "reason")?,
            by,
        )?;
        self.wake();
        Ok(json!({"task": task.to_json()}))
    }

    /// `job task reopen` — operator only: a pane alias means an agent
    /// is asking, and re-scoping blocked work is the human's call.
    fn rpc_task_reopen(self: &Arc<Self>, params: &Value) -> Result<Value> {
        if optional_str(params, "pane").is_some() {
            return Err(Error::rejected(
                "job task reopen is an operator action — run it outside a cadence pane",
            ));
        }
        let task = self
            .store
            .reopen_task(required_str(params, "task")?, "operator")?;
        self.wake();
        Ok(json!({"task": task.to_json()}))
    }

    fn rpc_task_cancel(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let by = optional_str(params, "by").unwrap_or("operator");
        let task = self.store.cancel_task(required_str(params, "task")?, by)?;
        self.wake();
        Ok(json!({"task": task.to_json()}))
    }

    fn rpc_job_cancel(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let by = optional_str(params, "by").unwrap_or("operator");
        let job = self.store.cancel_job(required_str(params, "job")?, by)?;
        self.wake();
        Ok(json!({"job": job.to_json()}))
    }

    fn rpc_job_close(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let by = optional_str(params, "by").unwrap_or("operator");
        let job = self.store.close_job(required_str(params, "job")?, by)?;
        self.wake();
        Ok(json!({"job": job.to_json()}))
    }

    fn rpc_stop(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let agent = self.store.agent(&alias)?;
        if !registry::has_actor(&agent.provider, &agent.endpoint_kind) {
            return Err(Error::rejected(format!(
                "Agent '{alias}' is an inbox — no actor to stop; \
                 `cadence agent remove {alias}` deletes the mailbox"
            )));
        }
        // Reserve the alias for the whole stop — through the final state
        // write — so a resume cannot start a new actor in the gap where
        // the old actor already released ownership.
        let ctl = {
            let mut lc = self.lifecycle.lock().unwrap();
            // A stop already in flight owns the alias through its final
            // write; reject before any mutation rather than letting a
            // second operation write stale state over a newer actor.
            if !lc.stopping.insert(alias.clone()) {
                return Err(Error::rejected("Agent is already stopping"));
            }
            lc.agents.get(&alias).cloned()
        };
        let _reservation = StopReservation {
            lifecycle: &self.lifecycle,
            alias: &alias,
        };
        self.store.set_enabled(&alias, false)?;
        let _ = self.store.event_public(&alias, "stop_requested", json!({}));
        // A fenced agent keeps its attention state and reason; the stop
        // only disables it.
        if self.store.agent(&alias)?.state != "attention" {
            self.store.set_agent_state(&alias, "stopping", None)?;
        }
        if let Some(ctl) = ctl {
            self.stop_ctls(&[ctl]);
        }
        // A fenced pty agent's pane survived the fence for inspection —
        // `agent stop` is the explicit kill. For a live agent the
        // actor's own close() already ran, so this is a no-op for it.
        if agent.endpoint_kind == "pty" {
            adapter::pty::kill_pane(&self.state_dir, &alias, &self.provider_env);
        }
        // The actor writes its own terminal state on exit; do not mask a
        // fence it may have raised while finishing.
        let state = if self.store.agent(&alias)?.state == "attention" {
            "attention"
        } else {
            // One write: `stopped` lands with the runtime fields
            // cleared — the actor may still be finishing its own exit.
            self.store.set_state_detached(&alias, "stopped", None)?;
            "stopped"
        };
        self.wake();
        Ok(json!({"alias": alias, "state": state}))
    }

    /// Interrupt every actor, wait one bounded grace, force-close the
    /// stragglers, then join all threads. A forced close makes any
    /// outstanding attempt `OutcomeUnknown` — fenced, never replayed.
    fn stop_ctls(&self, ctls: &[Arc<AgentCtl>]) {
        for ctl in ctls {
            if let Some(adapter) = ctl.adapter.lock().unwrap().clone() {
                adapter.interrupt();
            }
            ctl.wake.notify_all();
        }
        let deadline = Instant::now() + STOP_GRACE;
        while ctls.iter().any(|c| !ctl_finished(c)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        for ctl in ctls {
            if !ctl_finished(ctl) {
                if let Some(adapter) = ctl.adapter.lock().unwrap().clone() {
                    // A forced close makes any outstanding attempt
                    // OutcomeUnknown — fenced, never replayed. During
                    // daemon shutdown the endpoint must detach, not
                    // die: owned endpoints (a pty pane) outlive the
                    // controller and are revalidated on the next open —
                    // killing one here orphans the session the restart
                    // is meant to re-adopt. `detach` defaults to
                    // `close` for adapters that own their provider
                    // process, so managed endpoints are still reaped.
                    if self.closing.load(Ordering::SeqCst) {
                        adapter.detach();
                    } else {
                        adapter.close();
                    }
                }
                ctl.wake.notify_all();
            }
        }
        for ctl in ctls {
            if let Some(handle) = ctl.thread.lock().unwrap().take() {
                let _ = handle.join();
            }
        }
    }

    /// Resolve a user-facing agent name to the canonical alias. Accepts
    /// an alias or a provider-native id — a Devin session slug or Codex
    /// thread id — so agents stay addressable by their native handle.
    /// Exact aliases always win over native ids.
    fn resolve_alias(&self, name: &str) -> Result<String> {
        if let Some(agent) = self.store.agent_opt(name)? {
            return Ok(agent.alias);
        }
        self.store
            .agent_by_native(name)?
            .map(|agent| agent.alias)
            .ok_or_else(|| Error::rejected("Unknown managed agent"))
    }

    /// The agent's registered upstream (`params.upstream`), if any —
    /// used as the default `reply_to` for its sends.
    fn upstream_of(&self, alias: &str) -> Option<String> {
        let agent = self.store.agent(alias).ok()?;
        agent
            .params
            .as_ref()?
            .get("upstream")?
            .as_str()
            .map(str::to_string)
    }

    // ---- Stall watch: report silent turns, never touch them (CAD-52) ----

    /// Sample owned agents on a slow cadence until shutdown. The watch
    /// only ever emits events and notices — it never interrupts,
    /// re-dispatches or fences anything it observes.
    fn run_stall_watch(self: &Arc<Self>) {
        while !self.closing.load(Ordering::SeqCst) {
            self.stall_tick();
            std::thread::sleep(STALL_TICK);
        }
    }

    /// One watch pass: refresh every owned agent's activity evidence,
    /// then compare its silence against the resolved budget.
    fn stall_tick(&self) {
        let agents: Vec<(String, Arc<AgentCtl>)> = self
            .lifecycle
            .lock()
            .unwrap()
            .agents
            .iter()
            .map(|(a, c)| (a.clone(), Arc::clone(c)))
            .collect();
        for (alias, ctl) in agents {
            self.stall_check(&alias, &ctl);
        }
    }

    /// Refresh one agent's activity evidence and fire the episode
    /// transition, if any. A turn that ends while stalled just ends —
    /// no recovery event is owed for a message that stopped running.
    fn stall_check(&self, alias: &str, ctl: &Arc<AgentCtl>) {
        let running = match self.store.running_message(alias) {
            Ok(Some(m)) => m,
            Ok(None) => {
                let mut w = ctl.stall.lock().unwrap();
                w.message = None;
                w.stalled_at = None;
                w.sample_at = None;
                w.settled = None;
                w.previous = None;
                w.candidate = None;
                w.changed = 0;
                return;
            }
            Err(_) => return,
        };
        let Ok(agent) = self.store.agent(alias) else {
            return;
        };
        // Store reads stay outside the stall lock — `stall_budget`
        // takes the conn mutex and no other path holds it in reverse.
        let budget = self.stall_budget(&agent, &running);
        let ad = ctl.adapter.lock().unwrap().clone();
        let mut w = ctl.stall.lock().unwrap();
        if w.message.as_deref() != Some(running.id.as_str()) {
            *w = StallWatch {
                message: Some(running.id.clone()),
                ..StallWatch::default()
            };
        }
        // An open brokered request means the provider is silent by
        // design — a human is thinking. That wait is activity.
        if self
            .pending
            .lock()
            .unwrap()
            .values()
            .any(|req| req.alias == alias)
        {
            w.activity = Instant::now();
        }
        // The adapter's own clock when it keeps one — managed
        // transcripts stamp every provider notification.
        if let Some(at) = ad.as_ref().and_then(|a| a.activity_at()) {
            if at > w.activity {
                w.activity = at;
            }
        }
        // PTY screens have no transport clock: captures run on their
        // own threads, one in flight per agent at most, so a slow or
        // wedged pane can never block the ticker (or any view that
        // touches this lock). A finished sample lands here.
        if agent.endpoint_kind == "pty" {
            let mut landed = None;
            match w.sample_rx.as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(hash)) => {
                    landed = Some(hash);
                    w.sample_rx = None;
                }
                // The sender is gone (capture failed) — release the
                // slot so the next due tick spawns another. `Empty`
                // leaves the slot held: a sample is still in flight.
                Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                    w.sample_rx = None;
                }
                _ => {}
            }
            if let Some(hash) = landed {
                if w.settled.as_deref() == Some(hash.as_str()) {
                    // Still the settled screen — a candidate reverted
                    // without ever confirming; drop it.
                    w.candidate = None;
                    w.changed = 0;
                } else {
                    w.changed = w.changed.saturating_add(1);
                    let confirmed = w.candidate.as_deref() == Some(hash.as_str())
                        || (w.changed >= 2 && w.previous.as_deref() != Some(hash.as_str()));
                    if confirmed {
                        // Second consecutive sighting, or a novel hash
                        // after the screen stayed changed for two
                        // samples — the change is real. A first-ever
                        // settle only forms the baseline.
                        if w.settled.is_some() {
                            w.activity = Instant::now();
                        }
                        w.previous = w.settled.take();
                        w.settled = Some(hash);
                        w.candidate = None;
                        w.changed = 0;
                    } else {
                        // First sighting of a different screen — hold
                        // as a candidate; a lone sample proves nothing.
                        w.candidate = Some(hash);
                    }
                }
                w.sample_at = Some(Instant::now());
            }
            if w.sample_rx.is_none()
                && w.sample_at
                    .is_none_or(|at| at.elapsed() >= screen_sample(&self.stall_sample_secs))
            {
                if let Some(ad) = ad {
                    let (tx, rx) = std::sync::mpsc::channel();
                    thread::spawn(move || {
                        if let Ok(screen) = ad.capture() {
                            let _ = tx.send(adapter::pty::activity_hash(&screen));
                        }
                    });
                    w.sample_rx = Some(rx);
                }
            }
        }
        let silent = w.activity.elapsed();
        if let Some(stalled_at) = w.stalled_at {
            if w.activity > stalled_at {
                let episode = w.episodes;
                w.stalled_at = None;
                drop(w);
                self.stall_resumed(&agent, &running, stalled_at.elapsed(), episode);
            }
            return;
        }
        if budget > 0 && silent >= Duration::from_secs(budget) {
            w.episodes += 1;
            w.stalled_at = Some(w.activity);
            let episode = w.episodes;
            drop(w);
            self.stall_fired(&agent, &running, silent, episode);
        }
    }

    /// The silence budget for a running message: the job's
    /// `stall_secs` for task-attached deliveries, else the agent's
    /// `stall_secs` param, else the default. `0` disables the episode
    /// (silence is still measured for the views).
    fn stall_budget(&self, agent: &Agent, message: &Message) -> u64 {
        if let Some(task_id) = &message.task_id {
            if let Ok(task) = self.store.task(task_id) {
                if let Ok(job) = self.store.job(&task.job_id) {
                    if let Some(s) = job.stall_secs {
                        return s.max(0) as u64;
                    }
                }
            }
        }
        agent
            .params
            .as_ref()
            .and_then(|p| p.get("stall_secs"))
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(DEFAULT_STALL_SECS)
    }

    /// `(job_id, task_id)` scope for a message's stall events — the
    /// `job events` view is one query over those columns.
    fn message_scope<'m>(&self, message: &'m Message) -> (Option<String>, Option<&'m str>) {
        let task_id = message.task_id.as_deref();
        let job_id = task_id.and_then(|t| self.store.task(t).ok().map(|t| t.job_id));
        (job_id, task_id)
    }

    /// `turn_stalled`: record the event and send the one notice this
    /// episode gets. Fires once per silence episode.
    fn stall_fired(&self, agent: &Agent, message: &Message, silent: Duration, episode: u64) {
        let silent_secs = silent.as_secs();
        let (job_id, task_id) = self.message_scope(message);
        let mut payload = json!({
            "message": message.id,
            "silent_secs": silent_secs,
            "last_activity": epoch_secs() - silent_secs as f64,
        });
        if let Some(t) = task_id {
            payload["task"] = json!(t);
        }
        let _ = self.store.event_public_scoped(
            &agent.alias,
            "turn_stalled",
            payload,
            job_id.as_deref(),
            task_id,
        );
        self.wake();
        self.stall_notice(agent, message, episode, true, silent_secs);
    }

    /// `turn_resumed`: the first activity after a stall closes the
    /// episode — same event/notice pair to the same recipients.
    fn stall_resumed(&self, agent: &Agent, message: &Message, silent: Duration, episode: u64) {
        let silent_secs = silent.as_secs();
        let (job_id, task_id) = self.message_scope(message);
        let mut payload = json!({"message": message.id, "silent_secs": silent_secs});
        if let Some(t) = task_id {
            payload["task"] = json!(t);
        }
        let _ = self.store.event_public_scoped(
            &agent.alias,
            "turn_resumed",
            payload,
            job_id.as_deref(),
            task_id,
        );
        self.wake();
        self.stall_notice(agent, message, episode, false, silent_secs);
    }

    /// The one notice an episode sends: a `job_event` to the PM for a
    /// job kickoff, a `worker_notice` to `reply_to` for any other
    /// delivery, nothing when the turn answers to nobody.
    fn stall_notice(
        &self,
        agent: &Agent,
        message: &Message,
        episode: u64,
        stalled: bool,
        silent_secs: u64,
    ) {
        let what = message.task_id.as_deref().unwrap_or(&message.id);
        let body = if stalled {
            let cursor = self.store.event_cursor(&agent.alias).unwrap_or(0);
            format!(
                "Worker {} has shown no activity for {} on {} — this is \
                 informational; nothing was changed or interrupted. \
                 Look with: `cadence agent capture {}`, \
                 `cadence agent probe {}`, `cadence events {} --after {}`.",
                agent.alias,
                fmt_duration(silent_secs),
                what,
                agent.alias,
                agent.alias,
                agent.alias,
                cursor
            )
        } else {
            format!(
                "Worker {} is active again on {} after {} of silence — \
                 the earlier stall notice is resolved; nothing was changed.",
                agent.alias,
                what,
                fmt_duration(silent_secs)
            )
        };
        let kind = if stalled { "stalled" } else { "resumed" };
        if message.source == "job_dispatch" && message.task_id.is_some() {
            let task_id = message.task_id.as_deref().unwrap();
            let state = if stalled { "stalled" } else { "running" };
            if self
                .store
                .job_notice(
                    task_id,
                    state,
                    &format!("turn_{kind}:{}:{episode}", message.id),
                    &body,
                )
                .is_ok()
            {
                if let Ok(task) = self.store.task(task_id) {
                    if let Ok(job) = self.store.job(&task.job_id) {
                        self.notify_agent(&job.pm_alias);
                    }
                }
            }
            return;
        }
        if let Some(reply_to) = &message.reply_to {
            let delivery = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("cadence-notice:{kind}:{}:{episode}", message.id).as_bytes(),
            )
            .simple()
            .to_string();
            if self
                .store
                .enqueue_task(
                    reply_to,
                    &body,
                    None,
                    &delivery,
                    "worker_notice",
                    message.task_id.as_deref(),
                )
                .is_ok()
            {
                self.notify_agent(reply_to);
            }
        }
    }

    /// The view-side snapshot for `agent show`/`agent list`/`job show`:
    /// `(silent_secs, stalled)` while a message is running — `None`
    /// when the agent has no in-flight turn.
    fn stall_view(&self, alias: &str) -> Option<(u64, bool)> {
        let running = self.store.running_message(alias).ok()??;
        let ctl = self.lifecycle.lock().unwrap().agents.get(alias)?.clone();
        let w = ctl.stall.lock().unwrap();
        if w.message.as_deref() == Some(running.id.as_str()) {
            return Some((w.activity.elapsed().as_secs(), w.stalled_at.is_some()));
        }
        // The watch hasn't ticked over this message yet — report
        // silence from its recorded start.
        let silent = running
            .started
            .map(|s| (epoch_secs() - s).max(0.0) as u64)
            .unwrap_or(0);
        Some((silent, false))
    }

    fn notify_agent(&self, alias: &str) {
        if let Some(ctl) = self.lifecycle.lock().unwrap().agents.get(alias) {
            ctl.wake.notify_all();
        }
    }

    /// Provider-side proof of life for the stall watch — any adapter
    /// event or request channel activity refreshes the agent's clock.
    fn bump_activity(&self, alias: &str) {
        if let Some(ctl) = self.lifecycle.lock().unwrap().agents.get(alias) {
            ctl.bump_activity();
        }
    }

    /// Graceful daemon stop: the only path that may write the
    /// hot-restart marker. Pty actors are never interrupted here —
    /// `interrupt()` sends C-c into the pane, which could cancel the
    /// provider work a clean restart is meant to re-adopt; waking the
    /// idle loop is enough. A mid-`submitting` paste finishes its
    /// render check inside the actor's own deadline and the join waits
    /// it out — a rendered paste is recorded `running` (adoptable), an
    /// unrendered one falls back to `queued`/`unknown`, never
    /// `running` without proof. Managed endpoints keep the
    /// interrupt-and-grace path: their provider process dies with the
    /// daemon either way.
    fn shutdown(&self) {
        // Endpoint facts must be read while the panes are still live on
        // the agent rows — detach clears `pid`/`generation`/`endpoint`.
        // The marker's message rows are read last, after the drain.
        let facts = self.store.pty_endpoint_facts().unwrap_or_default();
        let owned: Vec<(String, Arc<AgentCtl>)> = self
            .lifecycle
            .lock()
            .unwrap()
            .agents
            .iter()
            .map(|(alias, ctl)| (alias.clone(), Arc::clone(ctl)))
            .collect();
        let (pty, rest): (Vec<_>, Vec<_>) = owned.into_iter().partition(|(alias, _)| {
            self.store
                .agent(alias)
                .map(|a| a.endpoint_kind == "pty")
                .unwrap_or(false)
        });
        for (_, ctl) in &pty {
            ctl.wake.notify_all();
        }
        let rest: Vec<Arc<AgentCtl>> = rest.into_iter().map(|(_, ctl)| ctl).collect();
        self.stop_ctls(&rest);
        // The pty join is the bounded `submitting` wait (decision 4):
        // the render check's own deadline caps it — no extra timer.
        for (_, ctl) in pty {
            if let Some(handle) = ctl.thread.lock().unwrap().take() {
                let _ = handle.join();
            }
        }
        // LAST: every actor has detached and written its final state,
        // so the message rows are settled — and a marker written here
        // can only ever describe a clean stop.
        if let Ok(entries) = self.store.shutdown_entries(&facts) {
            write_shutdown_marker(&self.state_dir, &self.instance, entries);
        }
    }
}

fn ctl_finished(ctl: &AgentCtl) -> bool {
    ctl.thread
        .lock()
        .unwrap()
        .as_ref()
        .is_none_or(|h| h.is_finished())
}

fn is_terminal(state: &str) -> bool {
    matches!(
        state,
        "completed" | "failed" | "interrupted" | "unknown" | "cancelled"
    )
}

fn is_task_terminal(state: &str) -> bool {
    matches!(state, "verified" | "done" | "cancelled" | "failed")
}

/// Content hash for spec-drift detection — the same value the CLI
/// computes at `job new`. Returns None when the file is unreadable
/// (the show path surfaces that as its own flag).
fn sha256_file(path: &std::path::Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).ok()?;
    Some(format!("{:x}", Sha256::digest(&bytes)))
}

fn required_str<'a>(params: &'a Value, field: &str) -> Result<&'a str> {
    params
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::rejected(format!("Missing required parameter '{field}'")))
}

fn optional_str<'a>(params: &'a Value, field: &str) -> Option<&'a str> {
    params.get(field).and_then(Value::as_str)
}

fn optional_u64(params: &Value, field: &str) -> Option<u64> {
    params.get(field).and_then(Value::as_u64)
}

fn optional_i64(params: &Value, field: &str) -> Option<i64> {
    params.get(field).and_then(Value::as_i64)
}

/// Reject peers that are not the same Unix user.
fn check_peer(stream: &UnixStream) -> Result<()> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return Err(Error::internal("Cannot determine socket peer credentials"));
    }
    if cred.uid != unsafe { libc::geteuid() } {
        return Err(Error::rejected("Socket peer is not the same user"));
    }
    Ok(())
}

fn handle_conn(shared: Arc<Shared>, stream: UnixStream) {
    if check_peer(&stream).is_err() {
        return;
    }
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let response = serde_json::from_str::<Value>(&line)
            .map_err(|_| Error::rejected("Request must be one JSON object per line"))
            .and_then(|frame| {
                let method = frame
                    .get("method")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::rejected("Missing 'method'"))?;
                let params = frame.get("params").cloned().unwrap_or(json!({}));
                shared.dispatch(method, &params)
            });
        let frame = match response {
            Ok(result) => proto::ok(result),
            Err(error) => proto::err(&error),
        };
        if writeln!(writer, "{frame}").is_err() {
            break;
        }
    }
}

/// Exclusive lifetime ownership of the state directory. The lock file is
/// held for the whole `serve` call; a second daemon fails here before it
/// can touch the store, the socket, or any actor.
fn acquire_singleton(state_dir: &Path) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(state_dir.join("cadence.lock"))?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Err(Error::rejected(
            "Another cadence daemon already owns this state directory",
        ));
    }
    Ok(file)
}

/// Per-instance daemon configuration.
#[derive(Clone, Default)]
pub struct ServeOptions {
    /// Provider launch overrides (`CADENCE_CLAUDE_COMMAND`, …) for this
    /// daemon only; unset names fall back to the environment.
    pub provider_env: ProviderEnv,
    /// Stall screen-sample interval in seconds for this daemon; 0 falls
    /// back to `CADENCE_STALL_SAMPLE_SECS`, then one minute. Shared so
    /// an in-process test can shrink it after start.
    pub stall_sample_secs: Arc<AtomicU64>,
}

// ---- Hot restart (CAD-89): clean-stop marker + instance files ----
//
// A provably clean shutdown is the ONLY path that writes
// `shutdown.json`: it is the daemon's last act, after every actor has
// detached. The marker names the daemon run that wrote it
// (`daemon-instance`, recorded at serve start) and each pty turn still
// `running`. On the next start the marker is consumed exactly once —
// it is valid only against the immediately preceding recorded run and
// only within MARKER_TTL; anything else takes the historical fence
// path for the recorded agents.

/// The last recorded serve() run's instance id.
const INSTANCE_FILE: &str = "daemon-instance";
/// The clean-shutdown marker: running pty turns awaiting re-adoption.
const SHUTDOWN_FILE: &str = "shutdown.json";
/// How long a shutdown marker stays adoptable — a bound on pane
/// longevity, not on restart speed. Past it the recorded checks would
/// read stale pane state as fresh; the turns fence instead.
const MARKER_TTL_SECS: f64 = 900.0;

/// What `serve()` carries into `Shared`: this run's instance id plus
/// the consumed marker (entries and a staleness reason when the marker
/// itself failed validation).
pub struct HotStart {
    pub instance: String,
    marker: Option<store::ConsumedMarker>,
}

impl HotStart {
    /// No marker — tests and every non-serve `Shared::new`.
    pub fn fresh() -> Self {
        Self {
            instance: Uuid::new_v4().simple().to_string(),
            marker: None,
        }
    }
}

/// Small-string atomic write: tmp file in the same directory, then
/// rename — a reader never sees a torn marker.
fn write_file_atomic(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn read_instance(state_dir: &Path) -> Option<String> {
    std::fs::read_to_string(state_dir.join(INSTANCE_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Read, validate and DELETE the shutdown marker — consume-once: a
/// daemon that crashes after this point leaves nothing to adopt, which
/// is exactly the crash path. The new run's instance id is recorded
/// immediately after, so `marker.instance` must equal the PREVIOUS
/// recorded start to count as provably clean.
fn hot_restart_begin(state_dir: &Path) -> HotStart {
    let previous = read_instance(state_dir);
    let path = state_dir.join(SHUTDOWN_FILE);
    let raw = std::fs::read_to_string(&path).ok();
    // Consume-once regardless of what the marker says — a stale or
    // unreadable marker must never be adopted twice.
    let _ = std::fs::remove_file(&path);
    let marker = raw.and_then(|raw| match serde_json::from_str::<Value>(&raw) {
        Ok(v) => {
            let instance = v["instance"].as_str().unwrap_or_default().to_string();
            let at = v["at"].as_f64().unwrap_or(0.0);
            let entries = v["entries"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|r| {
                            Some(store::AdoptEntry {
                                alias: r["alias"].as_str()?.to_string(),
                                message_id: r["message_id"].as_str()?.to_string(),
                                turn_id: r["turn_id"].as_str()?.to_string(),
                                generation: r["generation"].as_str()?.to_string(),
                                pane_pid: r["pane_pid"].as_u64()? as u32,
                                native_session: r["native_session"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let stale = if instance.is_empty() || Some(instance.as_str()) != previous.as_deref() {
                Some("shutdown marker does not match the last recorded daemon run".to_string())
            } else if epoch_secs() - at > MARKER_TTL_SECS {
                Some(format!(
                    "shutdown marker expired ({:.0}s old, bound {:.0}s)",
                    epoch_secs() - at,
                    MARKER_TTL_SECS
                ))
            } else {
                None
            };
            Some(store::ConsumedMarker { entries, stale })
        }
        Err(_) => {
            eprintln!("hot-restart: unreadable shutdown marker discarded");
            None
        }
    });
    let instance = Uuid::new_v4().simple().to_string();
    if let Err(e) = write_file_atomic(&state_dir.join(INSTANCE_FILE), &instance) {
        eprintln!("hot-restart: could not record daemon instance: {e}");
    }
    HotStart { instance, marker }
}

/// The last write of a clean shutdown — after this the daemon exits.
/// Never fails the stop itself: a marker that can't be written is a
/// crash-equivalent state dir, which the next start already handles.
fn write_shutdown_marker(state_dir: &Path, instance: &str, entries: Vec<store::AdoptEntry>) {
    let marker = json!({
        "instance": instance,
        "at": epoch_secs(),
        "entries": entries.iter().map(|e| json!({
            "alias": e.alias,
            "message_id": e.message_id,
            "turn_id": e.turn_id,
            "generation": e.generation,
            "pane_pid": e.pane_pid,
            "native_session": e.native_session,
        })).collect::<Vec<_>>(),
    });
    if let Err(e) = write_file_atomic(&state_dir.join(SHUTDOWN_FILE), &marker.to_string()) {
        eprintln!("hot-restart: could not write shutdown marker: {e}");
    }
}

/// Run the daemon in the foreground until `shutdown` or a signal.
pub fn serve(state_dir: &Path) -> Result<()> {
    serve_with(state_dir, ServeOptions::default())
}

/// `serve` with per-instance options — in-process test daemons pass
/// their mock commands here instead of through the shared environment.
pub fn serve_with(state_dir: &Path, opts: ServeOptions) -> Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let _singleton = acquire_singleton(state_dir)?;
    // Consume the shutdown marker and record this run's instance BEFORE
    // the store opens — recover() protects the candidate entries as it
    // sweeps, and a crash between here and open simply leaves nothing
    // to adopt.
    let hot = hot_restart_begin(state_dir);
    let shared = Shared::new_hot(state_dir, &opts, hot)?;
    let socket_path = state_dir.join("cadence.sock");
    if socket_path.exists() {
        // Safe while the singleton is held: no live owner can exist.
        std::fs::remove_file(&socket_path)?;
    }
    let listener = UnixListener::bind(&socket_path)?;
    listener.set_nonblocking(true)?;
    // Relaunch enabled actors; fenced ones land in `attention` instead.
    // Inbox rows are durable mailboxes — enabled or not, they own no
    // actor and keep their pseudo-endpoint across restarts.
    for agent in shared.store.agents()? {
        if !agent.enabled || !registry::has_actor(&agent.provider, &agent.endpoint_kind) {
            continue;
        }
        // A fenced agent stays registered but must never churn on a
        // daemon restart — no actor, no provider process. `attention`
        // or an unreconciled `unknown` both mean the operator must
        // reconcile before it runs again. The fenced state and its
        // recovery hint are preserved/restored, then `relaunch_skipped`
        // is emitted instead of a launch.
        let unknown = shared.store.has_unknown(&agent.alias)?;
        if agent.state == "attention" || unknown {
            let (reason, error) = if unknown {
                (
                    "unknown messages await reconcile",
                    shared.uncertain_fence_text(&agent.alias),
                )
            } else {
                // Other fences (session mismatch, failed open) keep
                // their recorded error verbatim.
                (
                    "agent is in attention",
                    agent.error.clone().unwrap_or_default(),
                )
            };
            shared
                .store
                .set_state_detached(&agent.alias, "attention", Some(&error))?;
            eprintln!("start: skipping fenced agent '{}' ({reason})", agent.alias);
            let _ = shared.store.event_public(
                &agent.alias,
                "relaunch_skipped",
                json!({"reason": reason}),
            );
            continue;
        }
        shared.launch_actor(&agent.alias)?;
    }
    // Signal-driven shutdown: set the same flag as the rpc.
    {
        let shared = Arc::clone(&shared);
        let mut signals = signal_hook::iterator::Signals::new([
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGINT,
        ])
        .map_err(|e| Error::internal(format!("signal hook: {e}")))?;
        thread::spawn(move || {
            for _ in signals.forever() {
                shared.closing.store(true, Ordering::SeqCst);
                shared.wake();
            }
        });
    }
    // Stall watch: a running turn that goes silent is reported to
    // whoever waits on it — never interrupted, never replayed.
    {
        let shared = Arc::clone(&shared);
        thread::spawn(move || shared.run_stall_watch());
    }
    while !shared.closing.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let shared = Arc::clone(&shared);
                thread::spawn(move || handle_conn(shared, stream));
            }
            // WouldBlock is the idle nonblocking poll; ConnectionAborted
            // is the listener race — a queued connection reset before
            // accept (a client exiting mid-handshake) must not kill the
            // daemon.
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::ConnectionAborted
                ) =>
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e.into()),
        }
    }
    shared.shutdown();
    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewAgent;

    fn shared() -> (tempfile::TempDir, Arc<Shared>) {
        let dir = tempfile::tempdir().unwrap();
        let shared = Shared::new(dir.path(), &ServeOptions::default()).unwrap();
        (dir, shared)
    }

    fn register(shared: &Shared, dir: &Path, alias: &str) {
        shared
            .store
            .register_agent(&NewAgent {
                alias,
                provider: "fake",
                endpoint_kind: "fake",
                role: "worker",
                cwd: dir.to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
            })
            .unwrap();
    }

    /// Deterministic boundary for the stop/resume gap: while a stop
    /// reservation is held — the state an alias is in between the old
    /// actor's map removal and the stop's final write — a launch is
    /// rejected; releasing the reservation unblocks it.
    #[test]
    fn stopping_reservation_blocks_relaunch() {
        let (dir, shared) = shared();
        register(&shared, dir.path(), "w1");
        shared
            .lifecycle
            .lock()
            .unwrap()
            .stopping
            .insert("w1".to_string());
        let err = shared.launch_actor("w1").unwrap_err();
        assert!(err.to_string().contains("running or stopping"));
        // Reservation dropped (stop finalized): launch succeeds again.
        shared.lifecycle.lock().unwrap().stopping.remove("w1");
        shared.launch_actor("w1").unwrap();
        // Let the spawned actor exit instead of leaking it into other tests.
        shared.store.set_enabled("w1", false).unwrap();
        let lc = shared.lifecycle.lock().unwrap();
        if let Some(ctl) = lc.agents.get("w1") {
            ctl.wake.notify_all();
        }
    }

    /// While a stop reservation is in flight (its owner has not yet
    /// finished the final state write), a second stop is rejected and
    /// mutates nothing — so no stale stop can outlive the reservation
    /// and write over a newer actor generation.
    #[test]
    fn overlapping_stop_is_rejected_without_mutation() {
        let (dir, shared) = shared();
        register(&shared, dir.path(), "w1");
        shared
            .lifecycle
            .lock()
            .unwrap()
            .stopping
            .insert("w1".to_string());
        let before = shared.store.agent("w1").unwrap().state;
        let err = shared.rpc_stop(&json!({"alias": "w1"})).unwrap_err();
        assert!(err.to_string().contains("already stopping"));
        // Zero mutations: agent still enabled and in its prior state.
        let agent = shared.store.agent("w1").unwrap();
        assert_eq!(agent.state, before);
        assert!(agent.enabled);
        // Once the owning stop finalizes and releases, a sequential
        // stop proceeds — repeat stop stays idempotent.
        shared.lifecycle.lock().unwrap().stopping.remove("w1");
        let stopped = shared.rpc_stop(&json!({"alias": "w1"})).unwrap();
        assert_eq!(stopped["state"], "stopped");
        let again = shared.rpc_stop(&json!({"alias": "w1"})).unwrap();
        assert_eq!(again["state"], "stopped");
    }
}
