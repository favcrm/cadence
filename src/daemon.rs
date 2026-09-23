//! Persistent local controller: Unix-socket server + one actor per agent.
//!
//! The socket lives in a 0700 state directory and accepts only same-UID
//! peers (`SO_PEERCRED`). That establishes same-user access — it is not a
//! hostile same-user isolation boundary. Slot RPCs additionally bind
//! caller identity to the connection (CAD-113): the peer pid's /proc
//! ancestry must reach a registered pane, or the call is refused.
//!
//! Each registered agent gets one actor thread that owns its provider
//! adapter and serializes turns. The daemon relaunches enabled actors on
//! start — except actors fenced by an `unknown` in-flight attempt, which
//! stay in `attention` until a human reconciles them.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use uuid::Uuid;

use crate::adapter::{
    self, registry, AdapterHooks, Probe, ProviderAdapter, ProviderEnv, ProviderRequest,
    SettledPoll, TurnResult,
};
use crate::client;
use crate::error::{Error, Result};
use crate::memory::{self, IdentityProof, NativeIdentity};
use crate::peer::{unmatched_caller, PeerTies};
use crate::proto;
use crate::slots::{SlotConfig, SlotKind, Slots};
use crate::store::{self, Agent, Message, Store, Take};

/// A `(Mutex, Condvar)` pair used for queue/event wakeups.
///
/// `notify_all` bumps a generation. A waiter that samples
/// [`ticket`](Self::ticket) *before* its condition check, then
/// [`wait_if_unchanged`](Self::wait_if_unchanged), does not lose a
/// notify that lands in the gap — this condvar stores no permit on
/// its own.
pub struct Notify {
    lock: Mutex<u64>,
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
            lock: Mutex::new(0),
            cv: Condvar::new(),
        }
    }
    pub fn notify_all(&self) {
        let mut gen = self.lock.lock().unwrap();
        *gen = gen.wrapping_add(1);
        self.cv.notify_all();
    }
    /// Generation to sample before checking a condition that
    /// `notify_all` is meant to republish.
    pub fn ticket(&self) -> u64 {
        *self.lock.lock().unwrap()
    }
    /// Wait until `deadline`; returns false if it expired.
    /// A notify that arrives before this call is not remembered —
    /// sample [`ticket`](Self::ticket) first when that gap matters.
    pub fn wait_until(&self, deadline: Instant) -> bool {
        let guard = self.lock.lock().unwrap();
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let _ = self.cv.wait_timeout(guard, remaining).unwrap();
        Instant::now() < deadline
    }
    /// Wait until `deadline` or until `notify_all` has run since
    /// `ticket`. Returns false if the deadline expired with the
    /// generation unchanged.
    pub fn wait_if_unchanged(&self, ticket: u64, deadline: Instant) -> bool {
        let mut gen = self.lock.lock().unwrap();
        loop {
            if *gen != ticket {
                return true;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (guard, result) = self.cv.wait_timeout(gen, remaining).unwrap();
            gen = guard;
            if result.timed_out() && *gen == ticket {
                return false;
            }
        }
    }
}

/// Grace period for a cooperative stop before the transport is force-closed.
const STOP_GRACE: Duration = Duration::from_secs(3);
/// Stall watch cadence — `silent_secs` stays live without a store read
/// per agent becoming pressure.
const STALL_TICK: Duration = Duration::from_secs(2);
/// Provider WAL watch cadence — at the ~2 MiB/s a runaway devin WAL
/// wrote, a one-minute tick bounds overshoot past `wal_max_bytes` to
/// ~128 MiB.
const WAL_TICK: Duration = Duration::from_secs(60);
/// How long a pty actor waits before retrying a requeued delivery: a
/// render-miss requeue waits this long, a gate refusal backs off from it
/// (×1, ×2, ×4, capped at ×6 — 5 → 10 → 20 → 30s). Claims and inbox
/// arrivals still wake either wait early. `CADENCE_PTY_RETRY_SECS`
/// overrides it per daemon — tests shrink it (see [`parse_pty_retry_base`]).
const PTY_RETRY_BASE: Duration = Duration::from_secs(5);
/// `CADENCE_PTY_RETRY_SECS` bounds. The floor keeps a busy pane's gate
/// from becoming a tight probe loop; the ceiling keeps a typo from
/// parking deliveries for hours.
const PTY_RETRY_MIN_SECS: f64 = 0.1;
const PTY_RETRY_MAX_SECS: f64 = 3600.0;

/// The pty retry base from `CADENCE_PTY_RETRY_SECS`: unset is the 5s
/// default; a number of seconds in [0.1, 3600] is used as is; anything
/// else (0, negative, NaN, inf, out of range, not a number) is refused
/// with the reason, and the caller falls back to the default.
fn parse_pty_retry_base(raw: Option<&str>) -> std::result::Result<Duration, String> {
    let Some(raw) = raw else {
        return Ok(PTY_RETRY_BASE);
    };
    let secs: f64 = raw
        .trim()
        .parse()
        .map_err(|_| format!("CADENCE_PTY_RETRY_SECS={raw:?} is not a number of seconds"))?;
    if !(PTY_RETRY_MIN_SECS..=PTY_RETRY_MAX_SECS).contains(&secs) {
        return Err(format!(
            "CADENCE_PTY_RETRY_SECS={raw:?} is outside [{PTY_RETRY_MIN_SECS}, {PTY_RETRY_MAX_SECS}]s"
        ));
    }
    Ok(Duration::from_secs_f64(secs))
}

/// Gate-refusal back-off before retry number `waits` (0-based): base ×1,
/// ×2, ×4, then capped at ×6 — 5 → 10 → 20 → 30 → 30s at the default.
fn gate_backoff(base: Duration, waits: u32) -> Duration {
    (base * (1u32 << waits.min(3))).min(base * 6)
}
/// Persistent monitor reconciliation cadence. Individual registrations
/// carry their own interval; this tick only bounds how soon a due check
/// starts after its deadline.
const MONITOR_TICK: Duration = Duration::from_secs(1);
/// The daemon's own event stream — `wal_checkpointed` lands here.
/// Readable via `cadence events daemon`; not a sendable alias.
const DAEMON_ALIAS: &str = Store::DAEMON_STREAM;
/// How an approval-evidence writer was authorized — the daemon's own
/// statement, stamped on every record (CAD-217).
const APPROVAL_RECORDED_VIA: &str = "operator-connection";
/// CAD-250 N3: how long a nudge may wait in the queue (a busy or
/// menu-blocked pane) before it is cancelled as stale steering.
const NUDGE_TTL_SECS: u64 = 900;
/// CAD-250 N4: nudges are short steering, not tasks.
const NUDGE_MAX_CHARS: usize = 500;
/// `stall_secs` when neither the job nor the agent sets one.
const DEFAULT_STALL_SECS: u64 = 1800;
/// `silent_end_secs` when the agent doesn't set one: ten minutes of
/// probe-verified idle pane on a running message before
/// `turn_silent_end` fires — long enough that a between-tools quiet
/// spell never trips it.
const DEFAULT_SILENT_END_SECS: u64 = 600;
/// PTY screens are sampled at most this often while the pane is live —
/// a running turn for activity/silent-end bookkeeping, an idle pane for
/// menu/draft surfacing. The bound is one capture per pty agent per
/// minute.
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

/// Wait until runner root `pid` (a child of this daemon, leader of its
/// own process group) has exited WITHOUT reaping it, then SIGKILL what
/// is left of its group. While the zombie is unreaped the kernel cannot
/// hand its pid — the group id — to another process, so the kill can
/// only reach the runner's own stragglers (CAD-230b).
fn end_process_group(pid: u32) {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if rc == 0 {
            break;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            // Not our child any more (already reaped): the group id may
            // be reused — never signal it.
            return;
        }
    }
    unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
}

/// How often the daemon re-reads strict holders (CAD-230b).
const SLOT_WATCH_TICK: Duration = Duration::from_secs(1);

/// The lane an operator-launched runner is accounted to — not a valid
/// alias, so it never collides with an agent's.
const OPERATOR_LANE: &str = "(operator)";

/// A launched runner queues this long for its slot unless asked
/// otherwise (`wait_secs`), and never longer than the cap.
const RUNNER_WAIT_SECS: u64 = 600;
const RUNNER_MAX_WAIT_SECS: u64 = 86_400;

/// Monotonic seconds since an arbitrary process-local epoch — the
/// slot clock. NTP steps and wall-clock jumps cannot age a waiter or
/// expire a hold; the wall epoch rides alongside only for restart
/// persistence.
fn mono_secs() -> f64 {
    static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    T0.get_or_init(std::time::Instant::now)
        .elapsed()
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
    /// A Devin cloud turn is held: its message is `unknown` while the
    /// session may still be working. Any exit then detaches instead of
    /// closing, so a stop does not archive the session the operator's
    /// reconcile has to inspect.
    cloud_held: AtomicBool,
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
    /// The sample carries the probe verdict beside the hash: a menu
    /// frame is a human wait (never activity), an idle frame builds
    /// the silent-end streak.
    sample_rx: Option<std::sync::mpsc::Receiver<(String, Probe)>>,
    /// Consecutive landed samples whose probe read the pane idle — a
    /// silent end is proven by a streak, never one capture.
    idle_samples: u8,
    /// When the current idle streak began — `silent_end_secs`
    /// measures from here.
    idle_since: Option<Instant>,
    /// The menu line while the sampled probe shows an approval menu —
    /// `Some` is the menu-open flag: the rising edge fires
    /// `approval_menu` once per menu, and the views read the line.
    menu_line: Option<String>,
    /// The menu lines `approval_menu` has already fired for — a small
    /// bounded history, not just the last one: menus that alternate
    /// subjects across non-menu samples must not re-fire on every
    /// re-detection.
    menu_evented: std::collections::VecDeque<String>,
    /// `turn_silent_end` already fired for this message — the event
    /// is once per message, never twice.
    silent_end_sent: bool,
    /// The last landed probe verdict — rides `turn_silent_end`'s
    /// payload as evidence.
    last_probe: Option<Probe>,
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
            idle_samples: 0,
            idle_since: None,
            menu_line: None,
            menu_evented: std::collections::VecDeque::new(),
            silent_end_sent: false,
            last_probe: None,
            episodes: 0,
        }
    }
}

/// What `stall_view` hands a view: the silence age, the open stall
/// episode, and the sampled pane verdict (menu line, idle-streak age,
/// once-fired silent-end flag).
struct StallView {
    silent_secs: u64,
    stalled: bool,
    menu: Option<String>,
    ended_secs: Option<u64>,
    silent_ended: bool,
}

impl StallView {
    /// Write the view fields onto an agent/task JSON row — the same
    /// keys `agent_list`/`agent_show`/`job show` consumers read.
    fn apply(&self, j: &mut Value) {
        j["silent_secs"] = json!(self.silent_secs);
        j["stalled"] = json!(self.stalled);
        if let Some(line) = &self.menu {
            j["pane_menu"] = json!(line);
        }
        if let Some(secs) = self.ended_secs {
            j["ended_secs"] = json!(secs);
        }
        if self.silent_ended {
            j["silent_ended"] = json!(true);
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

/// generation, pane pid, native session — one row per live PTY alias.
type ShutdownFacts = HashMap<String, (String, u32, String)>;

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
    /// PTY endpoint facts captured by [`Shared::begin_closing`] before
    /// any actor is woken. Idle actors detach on that wake and clear
    /// `pid`/`generation`; reading the rows later loses the adoption
    /// record. `None` until shutdown is requested.
    shutdown_facts: Mutex<Option<ShutdownFacts>>,
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
    /// CAD-113 build-slot registry — holds persist to slots.json and
    /// are revalidated at boot; the queue itself is in-memory (its
    /// callers re-poll anyway).
    slots: Mutex<Slots>,
    /// The slot clock — `mono_secs` in production, injectable so the
    /// integration suite advances starvation/age without sleeping.
    slot_clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    /// CAD-199: the opt-in, records-only agent-gc timer.
    agent_gc: AgentGcTimer,
    /// CAD-96: the idle auto-stop timer (default ON, resumable).
    auto_stop: AutoStopTimer,
}

impl Shared {
    pub fn new(state_dir: &Path, opts: &ServeOptions) -> Result<Arc<Self>> {
        Self::new_hot(state_dir, opts, HotStart::fresh())
    }

    /// `new` with the consumed hot-restart context: the adoption
    /// candidates the marker carried plus this run's instance id.
    pub fn new_hot(state_dir: &Path, opts: &ServeOptions, hot: HotStart) -> Result<Arc<Self>> {
        let HotStart { instance, marker } = hot;
        let daemon_id = instance.clone();
        let db_path = state_dir.join("cadence.sqlite3");
        // Authorise the holder before the store opens the file
        // read-write and migrates. A direct `daemon run` whose identity
        // does not hold the lease refuses here and leaves the database
        // unchanged. `open_adopting` repeats the same check.
        crate::rollout::authorize_migration(&db_path)?;
        let store = Store::open_adopting(&db_path, marker)?;
        // Same-build crash restart is allowed with no lease. A different
        // build must already hold one — `daemon start` checks before
        // spawn, and this is the backstop for a direct `daemon run`. A
        // sandbox's own state dir never needs it (CAD-310).
        if !crate::rollout::sandbox_exempt(state_dir) {
            store.enforce_running_build()?;
        }
        // `daemon start` passes the holder on argv, not the environment.
        // Drop a parent-exported copy too, so panes do not inherit it.
        std::env::remove_var("CADENCE_ROLLOUT_AS");
        store.record_running_build(crate::overview::BUILD_COMMIT)?;
        store.ingest_rollout_gate(state_dir)?;
        let provider_log_dir = state_dir.join("agents");
        std::fs::create_dir_all(&provider_log_dir)?;
        // CAD-113: slot holds persist under the state dir; restore
        // revalidates them against live processes BEFORE the socket
        // opens, so a restart never forgets or double-grants a hold.
        let slot_clock = opts
            .slot_clock
            .clone()
            .unwrap_or_else(|| Arc::new(mono_secs));
        let mut slots = Slots::new(resolve_slot_config(opts));
        slots.persist_to(state_dir.join("slots.json"));
        let boot_events = slots.restore(crate::slots::SlotClock::at(slot_clock(), epoch_secs()));
        let shared = Arc::new(Self {
            store,
            changed: Notify::new(),
            pending: Mutex::new(HashMap::new()),
            answered: Mutex::new(HashMap::new()),
            lifecycle: Mutex::new(Lifecycle::default()),
            closing: AtomicBool::new(false),
            shutdown_facts: Mutex::new(None),
            provider_log_dir,
            state_dir: state_dir.to_path_buf(),
            open_attach: Mutex::new(HashMap::new()),
            provider_env: opts.provider_env.clone(),
            started_at: epoch_secs(),
            stall_sample_secs: Arc::clone(&opts.stall_sample_secs),
            instance,
            slots: Mutex::new(slots),
            slot_clock,
            agent_gc: AgentGcTimer::new(opts.agent_gc.clone()),
            auto_stop: AutoStopTimer::new(opts.auto_stop.clone(), opts.auto_stop_clock.clone()),
        });
        // Holds dropped by boot-time revalidation get their release
        // events now that the store-backed emitter exists.
        shared.emit_slot_events(boot_events);
        // Cloud sessions tag themselves `cadence:<id>` so a restart can
        // see which daemon opened them. The value is an instance id,
        // not a credential, and it stays in this daemon's provider env.
        shared.provider_env.set("CADENCE_DAEMON_ID", daemon_id);
        // CAD-230b: a runner in flight when the last daemon stopped is
        // `unknown` and incomplete — reported, never relaunched.
        for r in crate::runner::recover(state_dir, epoch_secs()) {
            let _ = shared.store.event_public(
                &r.requester.lane,
                "runner_unknown",
                json!({"runner_id": r.runner_id, "recipe": r.recipe,
                       "last_state": r.last_state, "reason": r.reason}),
            );
        }
        Ok(shared)
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
            cloud_held: AtomicBool::new(false),
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
        self.thread_on_provider_event(alias, method, &params);
        if method == "cadence/codex_quota" {
            let thread_id = params.get("thread_id").and_then(Value::as_str);
            if let Some(thread_id) = thread_id {
                match self
                    .store
                    .update_provider_quota(alias, "codex", thread_id, &params)
                {
                    Ok(true) => {}
                    Ok(false) => {
                        let _ = self.store.event_public(
                            alias,
                            "quota_update_ignored",
                            json!({"reason": "provider thread no longer current"}),
                        );
                    }
                    Err(error) => {
                        eprintln!("codex quota update for '{alias}' failed: {error}");
                    }
                }
            }
            self.wake();
            return;
        }
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
                // even if this launch dies before session proof. A
                // lost persist must be loud: without it the next open
                // silently re-mints, churning a new session per retry.
                if let Some(session) = params.get("session").and_then(Value::as_str) {
                    if let Err(e) = self.store.set_params(alias, &json!({"session": session})) {
                        eprintln!("session_minted: persist failed for '{alias}': {e}");
                        let _ = self.store.event_public(
                            alias,
                            "session_persist_failed",
                            json!({"session": session, "error": e.to_string()}),
                        );
                    }
                }
            }
            if kind == "session_resume_failed" {
                // The stored native session failed to prove after the
                // pane ran — the chat/session is unresumable. Clearing
                // it lets the next open mint a fresh one instead of
                // wedging the alias on the same dead id forever. The
                // adapter only emits for profiles that opt in, and the
                // daemon refuses independently for every endpoint that
                // did not declare disposable sessions — a misbehaving
                // profile must never drop an operator's Claude/Devin
                // session on a transient proof failure.
                let disposable = self
                    .store
                    .agent(alias)
                    .ok()
                    .and_then(|a| {
                        registry::spec_opt(&a.provider, &a.endpoint_kind)
                            .map(|s| s.session_disposable)
                    })
                    .unwrap_or(false);
                if disposable {
                    // params.session AND thread_id both feed
                    // desired_session — clear both in one write or the
                    // dead id keeps resuming through the fallback.
                    if let Err(e) = self.store.clear_native_session(alias) {
                        eprintln!("session_resume_failed: clear failed for '{alias}': {e}");
                        let _ = self.store.event_public(
                            alias,
                            "session_persist_failed",
                            json!({"error": e.to_string()}),
                        );
                    }
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

    /// Payload events into a threaded agent's chat (CAD-319): Codex
    /// `agentMessage` items as they persist, managed Claude tool uses
    /// (name + redacted summary). The turn result lands with the
    /// message's finish, in the store. A lost append is logged, never
    /// fatal to the turn — the provider transcript still has it.
    fn thread_on_provider_event(&self, alias: &str, method: &str, params: &Value) {
        let (kind, text, payload) = match method {
            "item/completed" => {
                let item = &params["item"];
                if item.get("type").and_then(Value::as_str) != Some("agentMessage") {
                    return;
                }
                let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                (
                    store::KIND_ASSISTANT_TEXT,
                    text.to_string(),
                    json!({"provider_item": item.get("id"), "phase": item.get("phase")}),
                )
            }
            "cadence/tool_use" => {
                let tool = params.get("tool").and_then(Value::as_str).unwrap_or("tool");
                // The adapter already summarized and redacted; the store
                // redacts again. A summary-less event records the name.
                let summary = params
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| tool.to_string());
                (
                    store::KIND_TOOL_CALL,
                    summary,
                    json!({"tool": tool, "tool_use_id": params.get("tool_use_id")}),
                )
            }
            _ => return,
        };
        if let Err(e) =
            self.store
                .thread_append_running(alias, store::ROLE_AGENT, kind, &text, Some(payload))
        {
            eprintln!("thread append for '{alias}' failed: {e}");
        }
    }

    /// `thread_read` — a page of an agent's thread after `after`,
    /// optionally long-polling up to `wait` seconds (≤ 30) for the next
    /// entry, the way `agent_events` does. Read-only.
    fn rpc_thread_read(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let after = optional_i64(params, "after").unwrap_or(0);
        if after < 0 {
            return Err(Error::rejected("Thread cursor must be nonnegative"));
        }
        let limit = optional_i64(params, "limit").unwrap_or(100);
        if !(1..=store::THREAD_PAGE_MAX).contains(&limit) {
            return Err(Error::rejected(format!(
                "Thread page limit must be 1-{}",
                store::THREAD_PAGE_MAX
            )));
        }
        let wait = optional_u64(params, "wait").unwrap_or(0).min(30);
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            let thread = self.store.thread(&alias)?;
            let entries = self.store.thread_entries(&alias, after, limit)?;
            if !entries.is_empty()
                || Instant::now() >= deadline
                || self.closing.load(Ordering::SeqCst)
            {
                let cursor = entries.last().map(|e| e.seq).unwrap_or(after);
                return Ok(json!({
                    "alias": alias,
                    "thread": thread.as_ref().map(store::Thread::to_json),
                    "entries": entries.iter().map(store::ThreadEntry::to_json).collect::<Vec<_>>(),
                    "cursor": cursor,
                }));
            }
            let step = deadline.min(Instant::now() + Duration::from_secs(1));
            self.changed.wait_until(step);
        }
    }

    /// `thread_send` — the operator's chat message to an agent: starts
    /// the alias's thread on first use and queues the text exactly like
    /// `agent_send`, recorded as an `operator` entry.
    ///
    /// It instructs an agent, so an agent must never reach it: a
    /// connection the daemon attributes to a pane or managed endpoint is
    /// refused, and so is one whose identity cannot be derived (fail
    /// closed). Tied to no agent is accepted as the operator — a default,
    /// not positive proof: the board relays browser writes from its own
    /// process, so the browser's identity is the board's to establish
    /// (CAD-313). Identity-shaped and routing fields are refused rather
    /// than read.
    fn rpc_thread_send(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        if let Some(obj) = params.as_object() {
            if let Some(field) = obj
                .keys()
                .find(|k| !matches!(k.as_str(), "alias" | "text" | "message"))
            {
                return Err(Error::rejected(format!(
                    "thread send takes alias, text and message only; field '{field}' \
                     is not accepted"
                )));
            }
        }
        match self.caller_identity(peer_pid) {
            Ok(Caller::NoAgentIdentity) => {}
            Ok(Caller::Agent(v)) => {
                return Err(Error::rejected(format!(
                    "thread send is the operator's chat — this connection is agent \
                     '{}'; agents message each other with `cadence send`",
                    v.agent.alias
                )))
            }
            Err(e) => {
                return Err(Error::rejected(format!(
                    "thread send refused: caller identity underivable — {e}"
                )))
            }
        }
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let mut send = params.clone();
        send["alias"] = json!(alias);
        send["source"] = json!("operator");
        // The thread starts inside the enqueue transaction: a refused
        // message leaves no thread and no `thread_created` event.
        let mut receipt = self.send_as(&send, &|_| store::Sender::OperatorChat)?;
        receipt["thread"] = self
            .store
            .thread(&alias)?
            .as_ref()
            .map(store::Thread::to_json)
            .unwrap_or(Value::Null);
        Ok(receipt)
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
        // stops keep `close()`, except while a Devin cloud turn is held.
        // `detach` defaults to `close` for adapters that own their
        // provider process, so managed endpoints are still reaped on
        // every exit.
        if let Some(adapter) = ctl.adapter.lock().unwrap().take() {
            if self.closing.load(Ordering::SeqCst)
                || outcome.is_err()
                || ctl.cloud_held.load(Ordering::SeqCst)
            {
                adapter.detach();
            } else {
                adapter.close();
            }
        }
        // The endpoint is gone: its build-slot enrollment admits no
        // more work (holds stay accounted until released or dead).
        self.revoke_endpoint(alias, "endpoint closed");
        {
            let mut pending = self.pending.lock().unwrap();
            pending.retain(|_, req| req.alias != alias);
            // Uncollected brokered answers die with the actor too — a
            // `request_wait` still blocked sees the handle gone and
            // reports `closed` to its caller.
            self.answered.lock().unwrap().retain(|_, (a, _)| a != alias);
        }
        // CAD-250 N2: a nudge never outlives the actor it was aimed at —
        // whatever ended it (stop, fence, shutdown), queued nudges close
        // here with `nudge_cancelled`, never pasted into a later pane.
        let nudge_reason = if self.closing.load(Ordering::SeqCst) {
            "shutdown"
        } else if outcome.is_err() {
            "fence"
        } else {
            "stop"
        };
        let _ = self.store.cancel_nudges_for(alias, nudge_reason);
        self.wake();
        let closing = self.closing.load(Ordering::SeqCst);
        match outcome {
            Err(ref error) => {
                // When unreconciled unknowns outlive the actor, keep the
                // provider's own account. The returned error is often the
                // generic review sentence; restamping from that alone would
                // hide the reason the operator has to inspect.
                let reason = if self.store.has_unknown(alias).unwrap_or(false) {
                    format_unknown_fence(&self.preserved_unknown_detail(alias, &error.to_string()))
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
        // A lost poll on a live Devin Cloud session is not a dead
        // process. Finish the message unknown and keep the actor and
        // endpoint; do not fence.
        let hold_cloud = agent.provider == "devin" && agent.endpoint_kind == "cloud";
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
            // One pane proof covers the whole list — every entry for
            // an alias shares the marker's generation/pane/session.
            Some(entries) => match adapter.open_adopted(&agent, &entries[0]) {
                Ok(identity) => identity,
                Err(error) => {
                    let reason = error.to_string();
                    let _ = self
                        .store
                        .orphan_running(alias, &format!("hot-restart adoption refused: {reason}"));
                    for e in entries {
                        let _ = self.store.event_public(
                            alias,
                            "turn_adopt_refused",
                            json!({"message": e.message_id,
                                   "turn_id": e.turn_id,
                                   "reason": reason}),
                        );
                    }
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
            Some(entries) => self.store.set_identity_adopted_with_quota(
                alias,
                &identity,
                entries,
                adapter.quota_snapshot(),
            )?,
            None => {
                self.store
                    .set_identity_with_quota(alias, &identity, adapter.quota_snapshot())?
            }
        }
        // CAD-230: a managed provider's process is enrolled for build
        // slots from the pid just recorded — the daemon's own record,
        // never a caller's claim.
        self.enroll_endpoint(alias);
        self.wake();
        let retry_base = self.pty_retry_base();
        let mut gate_notice: Option<String> = None;
        let mut gate_waits: u32 = 0;
        // Proven paste misses per message — a TUI that looks idle but
        // keeps dropping pastes must not be re-fed forever.
        let mut unrendered: u32 = 0;
        let mut unrendered_message: Option<String> = None;
        let mut cloud_held: Option<Box<store::Message>> = None;
        let mut recover_delay = Duration::from_secs(1);
        let mut recover_started: Option<Instant> = None;
        let mut recover_escalated = false;
        let mut recover_failures: u32 = 0;
        // A hold lives only in this actor. A daemon restart or a stop
        // during a hold leaves the message `unknown`, and start, resume
        // and relaunch fence the agent on it: the operator inspects the
        // Devin session and reconciles. Nothing rebuilds the turn.
        loop {
            if self.closing.load(Ordering::SeqCst) {
                return Ok(());
            }
            // CAD-250: a delivered turn whose report never came is
            // bounded here, on the actor that owns the endpoint — it goes
            // `unknown` and fences exactly like any other uncertain
            // outcome. Checked every pass: the queue wait below is
            // bounded (5s idle, 30s gate backoff), so the bound fires
            // within seconds of running out.
            if let Some(awaiting) = self.report_overdue(alias) {
                if self.report_timeout(alias, awaiting)? {
                    return Err(Error::unknown(UNKNOWN_GENERIC_REASON));
                }
            }
            if let Some(pending) = cloud_held.clone() {
                if !self.store.agent(alias)?.enabled {
                    return Ok(());
                }
                // An operator reconcile settled the held message: stop
                // polling and take the next queued message.
                if self
                    .store
                    .message(&pending.id)?
                    .is_none_or(|message| message.state != "unknown")
                {
                    cloud_held = None;
                    ctl.cloud_held.store(false, Ordering::SeqCst);
                    recover_started = None;
                    recover_escalated = false;
                    recover_failures = 0;
                    continue;
                }
                // After the budget, one escalation and no further polls.
                // The agent stays enabled and the held message is not replayed.
                if recover_escalated {
                    ctl.wake.wait_if_unchanged(
                        ctl.wake.ticket(),
                        Instant::now() + Duration::from_secs(3600),
                    );
                    continue;
                }
                let transient = match adapter.poll_settled() {
                    // A stop or shutdown released the adapter, which then
                    // reports interrupted. That is not the session's
                    // outcome: leave the message for the reconcile.
                    Ok(SettledPoll::Ready(_))
                        if self.closing.load(Ordering::SeqCst)
                            || !self.store.agent(alias)?.enabled =>
                    {
                        return Ok(());
                    }
                    Ok(SettledPoll::Ready(turn)) => {
                        let status = match turn.status.as_str() {
                            "failed" => "failed",
                            "interrupted" => "interrupted",
                            _ => "completed",
                        };
                        let note = if turn.text.is_empty() {
                            None
                        } else {
                            Some(turn.text.as_str())
                        };
                        if let Err(error) =
                            self.store
                                .reconcile(&pending.id, status, note, "cloud_poll", None)
                        {
                            // The operator reconciled it first; theirs stands.
                            let settled = self
                                .store
                                .message(&pending.id)?
                                .is_none_or(|message| message.state != "unknown");
                            if !settled {
                                return Err(error);
                            }
                        }
                        cloud_held = None;
                        ctl.cloud_held.store(false, Ordering::SeqCst);
                        recover_started = None;
                        recover_escalated = false;
                        recover_failures = 0;
                        self.wake();
                        continue;
                    }
                    Ok(SettledPoll::Pending { transient }) => transient,
                    Err(_) => true,
                };
                let interval = adapter.poll_interval().max(Duration::from_millis(1));
                if transient {
                    recover_failures = recover_failures.saturating_add(1);
                    recover_delay = recover_delay
                        .saturating_mul(2)
                        .clamp(interval, Duration::from_secs(60));
                } else {
                    recover_delay = interval;
                }
                let started = recover_started.get_or_insert_with(Instant::now);
                if recover_failures >= 8 || started.elapsed() >= adapter.recover_budget() {
                    self.store.escalate_cloud_hold(
                        &pending,
                        "devin cloud recovery stopped after repeated poll failures",
                    )?;
                    recover_escalated = true;
                    self.wake();
                    continue;
                }
                let ready_at = Instant::now() + recover_delay;
                while Instant::now() < ready_at {
                    if self.closing.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    ctl.wake.wait_if_unchanged(ctl.wake.ticket(), ready_at);
                }
                continue;
            }
            // Sample before the empty check. `disconnected` probes the
            // pane before the wait; a `notify_agent` in that gap must
            // still wake the actor or a routed reply sits until the
            // 5s poll.
            let ticket = ctl.wake.ticket();
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
                    ctl.wake
                        .wait_if_unchanged(ticket, Instant::now() + Duration::from_secs(5));
                }
                Take::Message(message) => {
                    // A gate refusal requeues the same message, so its
                    // proven render-miss budget must survive the wait. If a
                    // queued row was cancelled while waiting, start the next
                    // row with a fresh budget instead of inheriting its count.
                    if unrendered_message.as_deref() != Some(message.id.as_str()) {
                        unrendered = 0;
                        unrendered_message = Some(message.id.clone());
                    }
                    let started_id = message.id.clone();
                    let shared = Arc::clone(self);
                    let watch = Arc::clone(ctl);
                    // Routed mail and nudges may pass the pty claim gate
                    // while a turn runs. Clear on return, including errors,
                    // so a later user message cannot inherit the flag.
                    let nudge = message.is_nudge();
                    adapter.set_unclaimed_ok(message.is_routed() || nudge);
                    let outcome = adapter.run_turn(&message.body, &message.id, &move |turn| {
                        // CAD-250: a nudge owns no turn — it never becomes
                        // `running`, and its paste is not the held turn's
                        // proof of life.
                        if !nudge {
                            let _ = shared.store.mark_running(&started_id, turn);
                            watch.bump_activity();
                        }
                        shared.wake();
                    });
                    adapter.set_unclaimed_ok(false);
                    // CAD-250: an unconfirmed nudge paste ends `unknown`,
                    // but a nudge belongs to no turn — it never fences the
                    // agent, never retries, never touches the held turn.
                    let outcome = match outcome {
                        Err(Error::NotRendered(miss)) if nudge => {
                            let _ = self.store.event_public(
                                alias,
                                "paste_not_rendered",
                                json!({"message": message.id,
                                       "reason": miss.reason,
                                       "attempt": 1,
                                       "retry": false,
                                       "before": miss.before_tail,
                                       "after": miss.after_tail,
                                       "claim_probe": miss.claim_probe}),
                            );
                            self.nudge_unconfirmed(&message, &miss.reason)?;
                            continue;
                        }
                        Err(Error::OutcomeUnknown(reason)) if nudge => {
                            self.nudge_unconfirmed(&message, &reason)?;
                            continue;
                        }
                        other => other,
                    };
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
                            unrendered_message = None;
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
                                let retry_ticket = ctl.wake.ticket();
                                let _ = self.store.requeue(&message.id);
                                let _ = self.store.set_agent_state_if(alias, "idle", "busy");
                                gate_notice = None;
                                ctl.wake
                                    .wait_if_unchanged(retry_ticket, Instant::now() + retry_base);
                            } else if routed {
                                let _ = self.store.event_public(
                                    alias,
                                    "delivery_parked",
                                    json!({"message": message.id,
                                           "reason": reason,
                                           "attempts": unrendered}),
                                );
                                let parked = json!({"status": "failed",
                                            "via": "pty_render_miss",
                                            "error": reason});
                                self.store
                                    .finish(&message, "failed", &parked, Some(&reason))?;
                                self.notify_routed_target(&message, &parked);
                                let _ = self.store.set_agent_state_if(alias, "idle", "busy");
                                unrendered = 0;
                                unrendered_message = None;
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
                            let retry_ticket = ctl.wake.ticket();
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
                            // 5s → 10 → 20 → 30s cap (`gate_backoff`):
                            // claims and inbox arrivals wake the wait
                            // early, so the poll is only the fallback for
                            // a busy pane.
                            let wait = gate_backoff(retry_base, gate_waits);
                            gate_waits = gate_waits.saturating_add(1);
                            ctl.wake
                                .wait_if_unchanged(retry_ticket, Instant::now() + wait);
                        }
                        Err(Error::OutcomeUnknown(error)) => {
                            if hold_cloud {
                                let held = json!({
                                    "status": "unknown",
                                    "text": "",
                                    "error": error,
                                    "held": true,
                                });
                                ctl.cloud_held.store(true, Ordering::SeqCst);
                                self.store
                                    .finish(&message, "unknown", &held, Some(&error))?;
                                let _ = self.store.event_public(
                                    alias,
                                    "cloud_hold",
                                    json!({"reason": error, "fenced": false}),
                                );
                                cloud_held = Some(message);
                                recover_delay =
                                    adapter.poll_interval().max(Duration::from_millis(1));
                                recover_started = Some(Instant::now());
                                recover_escalated = false;
                                recover_failures = 0;
                                self.wake();
                            } else {
                                return self.unknown(alias, &message, &error);
                            }
                        }
                        // Deterministic pre-submission rejection: zero
                        // bytes reached the provider, so nothing is
                        // unknown — fail the message, keep the endpoint
                        // live and keep draining the queue.
                        Err(Error::PreWrite(reason)) => {
                            let failed = json!({"status": "failed", "text": "",
                                        "error": reason});
                            self.store
                                .finish(&message, "failed", &failed, Some(&reason))?;
                            self.notify_routed_target(&message, &failed);
                            gate_notice = None;
                            self.wake();
                        }
                        // A provider/adapter error is actor-fatal: record
                        // the failed attempt, then land in `attention`.
                        Err(error) => {
                            let failed =
                                json!({"status": "failed", "text": "", "error": error.to_string()});
                            self.store.finish(
                                &message,
                                "failed",
                                &failed,
                                Some(&error.to_string()),
                            )?;
                            self.notify_routed_target(&message, &failed);
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

    /// CAD-250: a nudge whose paste could not be confirmed — `unknown`,
    /// recorded with a `nudge_unconfirmed` event, and nothing else: no
    /// fence, no retry, no notice (a nudge has no `reply_to`).
    fn nudge_unconfirmed(&self, message: &Message, reason: &str) -> Result<()> {
        let stored = json!({"status": "unknown", "text": "", "error": reason,
                            "via": "pty_nudge"});
        self.store
            .finish(message, "unknown", &stored, Some(reason))?;
        let _ = self.store.event_public(
            &message.alias,
            "nudge_unconfirmed",
            json!({"message": message.id, "reason": reason}),
        );
        // `finish` idles the agent; a turn still held keeps it busy.
        if !self.store.held_turns(&message.alias)?.is_empty() {
            let _ = self
                .store
                .set_agent_state_if(&message.alias, "busy", "idle");
        }
        self.wake();
        Ok(())
    }

    fn complete(&self, message: &Message, result: TurnResult) -> Result<()> {
        // CAD-250: a nudge completes at its confirmed paste — no report
        // is owed, and the held turn (if any) is untouched.
        if message.is_nudge() {
            let delivered = json!({"status": "completed", "via": "pty_nudge",
                        "turn_id": result.turn_id});
            self.store.finish(message, "completed", &delivered, None)?;
            if !self.store.held_turns(&message.alias)?.is_empty() {
                let _ = self
                    .store
                    .set_agent_state_if(&message.alias, "busy", "idle");
            }
            self.wake();
            return Ok(());
        }
        // PTY endpoints report "submitted": the paste reached the
        // terminal, but only an explicit ack/result report may finish
        // the message — it stays `running` meanwhile.
        if result.status == "submitted" {
            self.store.mark_submitted(message)?;
            // A routed notification's delivery IS its completion — the
            // receiving PM is not expected to report a result on it.
            if message.is_routed() {
                let delivered = json!({"status": "completed", "via": "pty_deliver",
                            "turn_id": result.turn_id});
                self.store.finish(message, "completed", &delivered, None)?;
                self.notify_routed_target(message, &delivered);
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
        let stored = json!({
            "turn_id": result.turn_id,
            "status": status,
            "text": result.text,
            "stop_reason": result.stop_reason,
            "error": result.error,
        });
        self.store
            .finish(message, &status, &stored, result.error.as_deref())?;
        self.notify_routed_target(message, &stored);
        self.wake();
        Ok(())
    }

    /// CAD-250: the agent's delivered-unreported turns and their bound
    /// when at least one has run out — `None` while every one is within
    /// it (or the bound is `0`, disabled).
    fn report_overdue(&self, alias: &str) -> Option<(Vec<Message>, u64)> {
        // Every turn-holding row, marked `awaiting_report` or not (F2): the
        // hold and the bound share one predicate.
        let awaiting = self.store.held_turns(alias).ok()?;
        if awaiting.is_empty() {
            return None;
        }
        let agent = self.store.agent(alias).ok()?;
        if agent.endpoint_kind != "pty" {
            return None;
        }
        let bound = store::report_timeout_secs(agent.params.as_ref());
        let now = epoch_secs();
        awaiting
            .iter()
            .any(|m| m.report_overdue(bound, now))
            .then_some((awaiting, bound))
    }

    /// CAD-250: retire overdue unreported turns to `unknown` and fence
    /// the actor — the standard uncertain-outcome path, never a
    /// completion and never a replay. Each overdue row gets one
    /// `report_timeout` event and, through the `unknown` finish, exactly
    /// one `worker_notice` to its `reply_to` (a job kickoff's is the PM;
    /// `send` defaults it to the upstream). Siblings still within the
    /// bound — only rows that accumulated before one-turn-per-actor —
    /// go `unknown` with it, since the endpoint they ran on is being
    /// detached. Every write is guarded: a report that lands first wins.
    /// `Ok(true)` when anything went `unknown` (the caller exits fenced).
    fn report_timeout(&self, alias: &str, (awaiting, bound): (Vec<Message>, u64)) -> Result<bool> {
        let now = epoch_secs();
        let (overdue, siblings): (Vec<&Message>, Vec<&Message>) =
            awaiting.iter().partition(|m| m.report_overdue(bound, now));
        let mut expired: Vec<String> = Vec::new();
        for m in overdue {
            let waited = m.report_clock().map_or(0, |c| (now - c).max(0.0) as u64);
            let reason = format!(
                "no report within report_timeout_secs={bound} ({}) of delivery \
                 — outcome unknown; not completed, not replayed",
                fmt_duration(bound)
            );
            if self
                .store
                .expire_awaiting_report(&m.id, Some((bound, now)), &reason)?
            {
                let (job_id, task_id) = self.message_scope(m);
                let _ = self.store.event_public_scoped(
                    alias,
                    "report_timeout",
                    json!({"message": m.id, "turn_id": m.turn_id,
                           "waited_secs": waited, "report_timeout_secs": bound}),
                    job_id.as_deref(),
                    task_id,
                );
                self.notify_routed_target(m, &Value::Null);
                expired.push(m.id.clone());
            }
        }
        let Some(first) = expired.first().cloned() else {
            return Ok(false);
        };
        for m in siblings {
            let reason = format!(
                "fenced with {first}, whose report bound ran out — outcome \
                 unknown; not completed, not replayed"
            );
            if self.store.expire_awaiting_report(&m.id, None, &reason)? {
                self.notify_routed_target(m, &Value::Null);
                expired.push(m.id.clone());
            }
        }
        let reason = format!(
            "no report within report_timeout_secs={bound} ({}) — {} turn(s) went unknown: {}",
            fmt_duration(bound),
            expired.len(),
            expired.join(", ")
        );
        // One write, as in `unknown`: the fence lands with the cleared
        // endpoint.
        self.store
            .set_state_detached(alias, "attention", Some(&format_unknown_fence(&reason)))?;
        let _ = self
            .store
            .event_public(alias, "attention", json!({"reason": reason}));
        self.wake();
        Ok(true)
    }

    /// CAD-250: the agent's `awaiting_report` view — the oldest
    /// delivered-unreported turn, how long it has waited, the bound and
    /// what is queued behind it. `None` when no turn awaits a report.
    fn awaiting_report_view(&self, agent: &Agent) -> Option<Value> {
        if agent.endpoint_kind != "pty" {
            return None;
        }
        let awaiting = self.store.held_turns(&agent.alias).ok()?;
        let head = awaiting.first()?;
        let bound = store::report_timeout_secs(agent.params.as_ref());
        let waited = head
            .report_clock()
            .map_or(0, |c| (epoch_secs() - c).max(0.0) as u64);
        let acked = head
            .result
            .as_ref()
            .is_some_and(|r| r.get("ack").is_some_and(|a| !a.is_null()));
        Some(json!({
            "message": head.id,
            "turn_id": head.turn_id,
            "task_id": head.task_id,
            "since_secs": waited,
            "acked": acked,
            "report_timeout_secs": bound,
            // `null` when the bound is disabled (`0`).
            "remaining_secs": (bound > 0).then(|| bound.saturating_sub(waited)),
            "count": awaiting.len(),
            "queued_behind": self.store.queued_turns(&agent.alias).unwrap_or(0),
        }))
    }

    /// The attention text for an unknown-outcome fence. A re-stamp on
    /// actor exit or relaunch-skip keeps the provider account already
    /// stored on the unknown row (or, failing that, the previous
    /// `agent.error` detail). It does not replace that account with
    /// only the generic review sentence.
    fn uncertain_fence_text(&self, alias: &str) -> String {
        format_unknown_fence(&self.preserved_unknown_detail(alias, ""))
    }

    fn preserved_unknown_detail(&self, alias: &str, actor_error: &str) -> String {
        if let Some(detail) = self.store.preferred_unknown_error(alias).ok().flatten() {
            let bounded = bound_unknown_detail(&detail);
            if !bounded.is_empty() && bounded != UNKNOWN_GENERIC_REASON {
                return bounded;
            }
        }
        let from_actor = bound_unknown_detail(actor_error);
        if !from_actor.is_empty() && from_actor != UNKNOWN_GENERIC_REASON {
            return from_actor;
        }
        if let Some(stored) = self.store.agent(alias).ok().and_then(|agent| agent.error) {
            let bounded = bound_unknown_detail(unknown_fence_detail(&stored));
            if !bounded.is_empty() {
                return bounded;
            }
        }
        UNKNOWN_GENERIC_REASON.to_string()
    }

    /// An `OutcomeUnknown` never becomes a retry: mark the attempt and
    /// fence the actor for review. `reason` is the provider's own account
    /// of the uncertainty (idle window, EOF, cap exceeded, …) — the
    /// operator needs it to reconcile.
    fn unknown(&self, alias: &str, message: &Message, reason: &str) -> Result<()> {
        let stored = json!({"status": "unknown", "text": "", "error": reason});
        self.store
            .finish(message, "unknown", &stored, Some(reason))?;
        self.notify_routed_target(message, &stored);
        // One write: the fence is visible immediately, so the cleared
        // endpoint must land with it — a reader in between must never
        // see `attention` plus a live endpoint.
        self.store
            .set_state_detached(alias, "attention", Some(&format_unknown_fence(reason)))?;
        let _ = self
            .store
            .event_public(alias, "attention", json!({"reason": reason}));
        self.wake();
        Err(Error::unknown(UNKNOWN_GENERIC_REASON))
    }

    // ---- dispatch ----

    /// The socket peer's `SO_PEERCRED` pid binds slot and approval-answer
    /// caller identity; clients cannot supply this identity.
    pub fn dispatch(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        match method {
            // `pid` is the singleton-lock holder: `daemon start` tells
            // the child it spawned from a daemon that already ran.
            "health" => Ok({
                let (adopted_live, adopted_oldest, adopted_reaped_total) =
                    crate::reaper::adopted_report(5);
                json!({
                "state": "ready",
                "pid": std::process::id(),
                "sandbox": crate::sandbox::profile(),
                "protocol": proto::PROTOCOL_VERSION,
                "capabilities": proto::capabilities(),
                "agent_gc_timer": self.agent_gc.status(),
                "agent_auto_stop": self.auto_stop.status(),
                // CAD-308: whether orphans of what this daemon launched
                // re-parent to it (true for `daemon run`).
                "child_subreaper": crate::reaper::is_subreaper(),
                // Adopted orphans still running (never killed) and the
                // adopted children reaped so far — CAD-308.
                "adopted_live": adopted_live,
                "adopted_oldest": adopted_oldest
                    .iter()
                    .map(|a| json!({"pid": a.pid, "comm": a.comm, "age_secs": a.age_secs}))
                    .collect::<Vec<_>>(),
                "adopted_reaped_total": adopted_reaped_total,
                })
            }),
            // Build identity + process start — the deploy-drift check
            // measures merged commits against *this* binary's commit.
            "daemon_info" => Ok(json!({
                "build_commit": crate::overview::BUILD_COMMIT,
                "build_time": crate::overview::BUILD_TIME,
                "started_at": self.started_at,
            })),
            "shutdown" => {
                self.begin_closing();
                Ok(json!({"state": "stopping"}))
            }
            "agent_register" => self.rpc_register(params),
            "model_defaults_get" => self.rpc_model_defaults_get(),
            "model_defaults_set" => self.rpc_model_defaults_set(params),
            "agent_list" => {
                let mut agents = Vec::new();
                // CAD-96: one grouped read tells auto-stopped rows apart.
                let markers = self
                    .store
                    .last_events_of_all(AUTO_STOP_MARKER_KINDS)
                    .unwrap_or_default();
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
                    if let Some(inbox) = self.store.inbox_status(&agent.alias)? {
                        j["inbox"] = inbox;
                        // CAD-251: stale-consumer evidence + owner.
                        j["inbox_health"] = self.inbox_health(&agent).unwrap_or(Value::Null);
                    }
                    if let Some(view) = self.stall_view(&agent.alias) {
                        view.apply(&mut j);
                    }
                    apply_auto_stop_view(&mut j, &agent, markers.get(&agent.alias));
                    if let Some(awaiting) = self.awaiting_report_view(&agent) {
                        j["awaiting_report"] = awaiting;
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
                if let Some(view) = self.stall_view(&alias) {
                    view.apply(&mut agent_json);
                }
                let marker = self.store.last_event_of(&alias, AUTO_STOP_MARKER_KINDS)?;
                apply_auto_stop_view(&mut agent_json, &agent, marker.as_ref());
                if let Some(awaiting) = self.awaiting_report_view(&agent) {
                    agent_json["awaiting_report"] = awaiting;
                }
                if agent.endpoint_kind == "pty" {
                    self.pty_lane_facts(&agent, &mut agent_json);
                }
                // The briefing lives under the state dir — actors read
                // it there, never inside their cwd repository. The path
                // is advertised only while the file exists; a missing
                // one is named as missing, never as a live path.
                if registry::has_actor(&agent.provider, &agent.endpoint_kind) {
                    let file = client::briefing_path(
                        &self.state_dir,
                        agent.params.as_ref().unwrap_or(&Value::Null),
                        &agent.alias,
                    );
                    if file.is_file() {
                        agent_json["briefing"] = json!(file);
                    } else {
                        agent_json["briefing"] = Value::Null;
                        agent_json["briefing_missing"] = json!(file);
                    }
                }
                Ok(json!({
                    "agent": agent_json,
                    "messages": messages.iter().map(Message::to_json).collect::<Vec<_>>(),
                    "event_cursor": self.store.event_cursor(&alias)?,
                    // Inbound backlog — what `cadence inbox` would drain
                    // for a mailbox, what the actor will still take for
                    // a live endpoint.
                    "queued": self.store.queued_count(&alias)?,
                    "inbox": self.store.inbox_status(&alias)?,
                    // Unreconciled `unknown` count — nonzero means the
                    // agent is fenced and `message reconcile` /
                    // `agent unfence` is the only exit.
                    "unknown": self.store.unknown_messages(&alias)?.len(),
                }))
            }
            "agent_send" => self.rpc_send_from(params, peer_pid),
            "agent_ask" => self.rpc_ask(params, peer_pid),
            "thread_read" => self.rpc_thread_read(params),
            "thread_send" => self.rpc_thread_send(params, peer_pid),
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
            "agent_answer" => self.rpc_answer(params, peer_pid),
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
            "memory_propose" => self.rpc_memory_propose(params, peer_pid),
            "memory_review" => self.rpc_memory_review(params, peer_pid),
            "memory_finalize" => self.rpc_memory_finalize(params, peer_pid),
            "monitor_register" => self.rpc_monitor_register(params),
            "monitor_list" => self.rpc_monitor_list(),
            "monitor_show" => self.rpc_monitor_show(params),
            "monitor_heartbeat" => self.rpc_monitor_heartbeat(params),
            "monitor_alerts" => self.rpc_monitor_alerts(params),
            "monitor_alert_ack" => self.rpc_monitor_alert_ack(params),
            "monitor_stop" => self.rpc_monitor_stop(params),
            "monitor_dispatch" => self.rpc_monitor_dispatch(params),
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
                    // Re-checks endpoint/state and refuses open work
                    // (unless `force`) inside its transaction — before
                    // any kill, so a refusal leaves the pane alone.
                    let force = params
                        .get("force")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    self.store.remove_agent(&alias, force)?;
                    // A fenced pty pane may still be alive — remove is
                    // the explicit kill; never leave an orphan session
                    // on the private socket behind a dropped row.
                    if agent.endpoint_kind == "pty" {
                        adapter::pty::kill_pane(&self.state_dir, &alias, &self.provider_env);
                    }
                    self.open_attach.lock().unwrap().remove(&alias);
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
                        // Open work refuses (CAD-284): skip, never force.
                        if self.store.remove_agent(&agent.alias, false).is_ok() {
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
            "slot_acquire" => self.rpc_slot_acquire(params, peer_pid),
            "slot_release" => self.rpc_slot_release(params, peer_pid),
            "slot_status" => self.rpc_slot_status(params, peer_pid),
            "slot_reconcile" => self.rpc_slot_reconcile(params, peer_pid),
            "slot_launch" => self.rpc_slot_launch(params, peer_pid),
            "slot_runner" => self.rpc_slot_runner(params, peer_pid),
            "approval_record" => self.rpc_approval_record(params, peer_pid),
            "approval_revoke" => self.rpc_approval_revoke(params, peer_pid),
            "plan_propose" => self.rpc_plan_propose(params, peer_pid),
            "plan_approve" => self.rpc_plan_decide(params, peer_pid, true),
            "plan_reject" => self.rpc_plan_decide(params, peer_pid, false),
            other => Err(Error::rejected(format!("Unknown method '{other}'"))),
        }
    }

    /// Slot lifecycle events ride the durable event stream addressed
    /// to the requesting lane — an agent sees why its build waited in
    /// its own `agent events` view.
    fn emit_slot_events(&self, events: Vec<crate::slots::SlotEvent>) {
        if events.is_empty() {
            return;
        }
        for (lane, kind, payload) in events {
            let _ = self.store.event_public(&lane, kind, payload);
        }
        self.wake();
    }

    /// The slot caller's connection-bound identity. The peer's pid
    /// comes from `SO_PEERCRED` and its `/proc` ancestry is walked;
    /// the NEAREST identity node on that chain decides, so a caller's
    /// own pane or endpoint beats any outer one and resolution never
    /// depends on map order:
    ///
    /// - a registered pty pane → the legacy binding (CAD-113, unchanged):
    ///   `lane` is the pane's alias and every pid on the chain may bind
    ///   a hold (`acquire --pid $$` claims the invoking shell);
    /// - the root of a strict enrollment (CAD-230: a managed provider
    ///   the daemon launched) → the strict binding: the peer must be
    ///   that exact process or reach it through a complete ancestry
    ///   verified hop by hop (pid + starttime + uid), and only that
    ///   verified segment may bind a hold. A failed verification
    ///   refuses — it never falls through to an outer pane.
    ///
    /// Fail-closed: an unreadable ancestry or no match refuses the
    /// call — there is no `operator` fallback; a caller detached from
    /// every pane and endpoint holds no lane at all. `Ok(None)` is the
    /// clean "no identity" answer — for [`Self::rpc_slot_reconcile`] a
    /// precondition of operator authority, never proof of it.
    fn slot_identity(&self, peer_pid: u32) -> Result<Option<SlotWho>> {
        let chain = adapter::pty::caller_chain(peer_pid).ok_or_else(|| {
            Error::rejected(format!(
                "Slot caller pid {peer_pid}: /proc ancestry unreadable — \
                 caller identity underivable"
            ))
        })?;
        let panes: HashMap<u32, String> = self
            .store
            .pty_endpoint_facts()
            .unwrap_or_default()
            .into_iter()
            .map(|(alias, (_, pane_pid, _))| (pane_pid, alias))
            .collect();
        let pane_at = chain.iter().position(|pid| panes.contains_key(pid));
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let root_at = slots.nearest_enrolled_root(&chain);
        match (pane_at, root_at) {
            (Some(p), Some(r)) if p == r => Err(Error::rejected(format!(
                "Slot caller pid {peer_pid}: pid {} is both a registered pane and \
                 an enrolled endpoint — caller identity ambiguous",
                chain[p]
            ))),
            (pane, Some(r)) if pane.is_none_or(|p| r < p) => Ok(Some(SlotWho::Strict(
                slots.strict_caller(peer_pid, chain[r])?,
            ))),
            (Some(p), _) => {
                let lane = adapter::pty::nearest_pane(&chain[p..], &panes)
                    .cloned()
                    .unwrap_or_default();
                Ok(Some(SlotWho::Pane { lane, chain }))
            }
            _ => Ok(None),
        }
    }

    /// [`Self::slot_identity`] for the slot verbs: no identity refuses.
    fn slot_caller(&self, peer_pid: u32) -> Result<SlotWho> {
        self.slot_identity(peer_pid)?.ok_or_else(|| {
            Error::rejected(format!(
                "Slot caller pid {peer_pid} descends from no registered \
                 pane and no enrolled managed endpoint — caller identity \
                 underivable"
            ))
        })
    }

    /// Revalidate every active strict enrollment against its owner row
    /// before a slot call: a missing row, a closed endpoint or a changed
    /// owner generation revokes (CAD-230). A store that cannot answer
    /// refuses the call instead — it proves no drift, so it neither
    /// revokes nor lets an unrevalidated enrollment admit.
    fn revalidate_enrollments(&self) -> Result<()> {
        let owners = self
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .enrolled_owners();
        if owners.is_empty() {
            return Ok(());
        }
        let mut current: HashMap<String, Option<String>> = HashMap::new();
        for alias in owners {
            let row = self.store.agent_opt(&alias)?;
            current.insert(alias, row.as_ref().and_then(owner_generation));
        }
        let events = self
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .revalidate_owners(&current);
        self.emit_slot_events(events);
        Ok(())
    }

    /// Mint (or renew) the strict build-slot enrollment for a managed
    /// endpoint that just opened — from the pid the adapter recorded
    /// and the owner row as now written. Admission is a side benefit
    /// of the endpoint: a refusal is recorded, never fatal.
    fn enroll_endpoint(&self, alias: &str) {
        let Ok(agent) = self.store.agent(alias) else {
            return;
        };
        if !registry::enrolls_build_slots(&agent.provider, &agent.endpoint_kind) {
            return;
        }
        let (Some(generation), Some(pid)) = (
            owner_generation(&agent),
            agent.pid.and_then(|p| u32::try_from(p).ok()),
        ) else {
            return;
        };
        let clk = crate::slots::SlotClock::at((self.slot_clock)(), epoch_secs());
        let outcome = self.slots.lock().unwrap_or_else(|e| e.into_inner()).enroll(
            alias,
            &generation,
            pid,
            clk,
        );
        match outcome {
            Ok((_, events)) => self.emit_slot_events(events),
            Err(e) => {
                let _ = self.store.event_public(
                    alias,
                    "slot_enrollment_refused",
                    json!({"pid": pid, "reason": e.to_string()}),
                );
            }
        }
    }

    /// The endpoint closed: its enrollment authorizes nothing more.
    fn revoke_endpoint(&self, alias: &str, reason: &str) {
        let events = self
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .revoke_owner(alias, reason);
        self.emit_slot_events(events);
    }

    /// The one caller-identity verifier (CAD-381, decision #3): who is
    /// on the other end of this Unix connection, answered only from the
    /// daemon's own launch records — never from request fields, env
    /// (`CADENCE_ALIAS`), upstream grouping or an operator default.
    /// Memory resolves through here; reviews and reports are meant to.
    ///
    /// Every agent endpoint the daemon records is an identity node:
    ///
    /// - a pty endpoint's pane process (the agent row's pid), proven by
    ///   the adapter's native ownership check ([`Self::verify_pane_agent`]);
    /// - a managed endpoint's enrolled provider root (CAD-230: pid +
    ///   starttime + uid, bound to the owner generation), proven by the
    ///   strict enrollment verifier ([`Self::verify_enrolled_agent`]) —
    ///   so headless claude/codex authenticate exactly like panes.
    ///
    /// Exactly one node on the peer's ancestry is the agent. None is
    /// [`Caller::NoAgentIdentity`] — which never takes an agent's
    /// identity and is NOT operator proof (see that variant). Two or
    /// more (or one pid that is both a pane and an enrolled root) is
    /// ambiguous and refused, as is any node whose proof fails: fail
    /// closed, never fall through to another node.
    fn caller_identity(&self, peer_pid: u32) -> Result<Caller> {
        // Drifted or closed owners lose their enrollment before it can
        // vouch for anyone.
        self.revalidate_enrollments()?;
        // /proc is still needed here, and only here: the peer's
        // ancestry is how a tool subprocess reaches its endpoint's
        // process. Linux-only; the macOS port is CAD-315.
        let chain = adapter::pty::caller_chain(peer_pid).ok_or_else(|| {
            Error::rejected(format!(
                "Caller pid {peer_pid}: /proc ancestry unreadable — caller \
                 identity underivable"
            ))
        })?;
        let panes: HashMap<u32, String> = self
            .store
            .pty_endpoint_facts()?
            .into_iter()
            .map(|(alias, (_, pane_pid, _))| (pane_pid, alias))
            .collect();
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let roots = slots.enrolled_roots_on(&chain);
        let mut nodes: Vec<String> = chain
            .iter()
            .filter_map(|pid| panes.get(pid).map(|alias| format!("pane '{alias}'")))
            .collect();
        for &r in &roots {
            if panes.contains_key(&chain[r]) {
                return Err(Error::rejected(format!(
                    "Caller pid {peer_pid}: pid {} is both a registered pane and an \
                     enrolled endpoint — caller identity ambiguous",
                    chain[r]
                )));
            }
            nodes.push(format!("enrolled root pid {}", chain[r]));
        }
        match nodes.len() {
            0 => return Ok(Caller::NoAgentIdentity),
            1 => {}
            n => {
                return Err(Error::rejected(format!(
                    "Caller pid {peer_pid} descends from {n} agent endpoints ({}) — \
                     caller identity ambiguous",
                    nodes.join(", ")
                )))
            }
        }
        if let Some(&r) = roots.first() {
            let strict = slots.strict_caller(peer_pid, chain[r])?;
            let enrollment = slots
                .endpoint_enrollment(&strict.enrollment_id)
                .cloned()
                .ok_or_else(|| {
                    Error::rejected(format!(
                        "Caller pid {peer_pid}: enrollment {} of '{}' is revoked (or a \
                         build runner's) — it vouches for no agent",
                        strict.enrollment_id, strict.lane
                    ))
                })?;
            drop(slots);
            return self
                .verify_enrolled_agent(enrollment)
                .map(|v| Caller::Agent(Box::new(v)));
        }
        drop(slots);
        let alias = adapter::pty::nearest_pane(&chain, &panes)
            .cloned()
            .expect("one pane node");
        self.verify_pane_agent(&alias)
            .map(|v| Caller::Agent(Box::new(v)))
    }

    /// A pty endpoint named by its pane: the row must be a live,
    /// generation-stamped endpoint and the adapter must still own that
    /// exact native session and pane process (unchanged CAD-191 proof).
    fn verify_pane_agent(&self, alias: &str) -> Result<VerifiedAgent> {
        let agent = self.store.agent(alias)?;
        require_live_endpoint(&agent)?;
        let generation = agent
            .generation
            .clone()
            .filter(|g| !g.is_empty() && agent.endpoint.is_some())
            .ok_or_else(|| {
                Error::rejected(format!("pty endpoint '{alias}' has no live generation"))
            })?;
        let pid = agent
            .pid
            .and_then(|p| u32::try_from(p).ok())
            .ok_or_else(|| Error::rejected("native endpoint pid disappeared"))?;
        // /proc: a pane's process start is read live — the pane record
        // carries no start time of its own.
        let process_start = process_start_identity(pid)?;
        let adapter = self.adapter_for(alias)?;
        adapter.verify_owned_endpoint(pid, &generation, agent.session_id.as_deref())?;
        if process_start != process_start_identity(pid)? {
            return Err(Error::rejected(
                "native endpoint process changed while resolving caller identity",
            ));
        }
        Ok(VerifiedAgent {
            agent,
            generation,
            process_start,
        })
    }

    /// A managed endpoint named by its active enrollment: the owner
    /// row, read now, must still be that endpoint — same owner
    /// generation (registration + endpoint generation + pid), same
    /// provider pid — and live. The process identity is the
    /// enrollment's own record (already re-verified hop by hop by
    /// [`crate::slots::Slots::strict_caller`]), so no further /proc
    /// read is needed.
    fn verify_enrolled_agent(&self, e: crate::slots::Enrollment) -> Result<VerifiedAgent> {
        let agent = self.store.agent_opt(&e.owner_actor)?.ok_or_else(|| {
            Error::rejected(format!(
                "enrollment {} names '{}', which is no longer registered",
                e.id, e.owner_actor
            ))
        })?;
        if owner_generation(&agent).as_deref() != Some(e.owner_generation.as_str())
            || agent.pid != Some(i64::from(e.root.pid))
        {
            return Err(Error::rejected(format!(
                "enrollment {} of '{}' no longer matches its endpoint (owner \
                 generation or provider pid changed)",
                e.id, e.owner_actor
            )));
        }
        require_live_endpoint(&agent)?;
        Ok(VerifiedAgent {
            agent,
            generation: e.owner_generation,
            process_start: e.root.starttime,
        })
    }

    /// Resolve a memory actor through [`Self::caller_identity`]: only a
    /// verified pm or worker endpoint may author, review or finalize.
    fn memory_actor(&self, peer_pid: u32) -> Result<NativeIdentity> {
        let verified = match self.caller_identity(peer_pid)? {
            Caller::Agent(v) => *v,
            Caller::NoAgentIdentity => {
                return Err(Error::rejected(format!(
                    "Memory caller pid {peer_pid} has no agent identity — it descends \
                     from no registered pane and no enrolled managed endpoint; memory \
                     actions are agent-authenticated and such a caller is never given \
                     an agent's identity"
                )))
            }
        };
        if !matches!(verified.agent.role.as_str(), "pm" | "worker") {
            return Err(Error::rejected(format!(
                "Memory caller '{}' has role '{}' — only pm and worker endpoints \
                 author or review memory",
                verified.agent.alias, verified.agent.role
            )));
        }
        Ok(NativeIdentity {
            proof: IdentityProof {
                alias: verified.agent.alias,
                registration: verified.agent.created.to_bits(),
                generation: verified.generation,
                process_start: verified.process_start,
                role: verified.agent.role,
            },
        })
    }

    /// The tracker memory RPCs read and write — [`Self::pm_dir`], so an
    /// in-process test daemon pins its own and never races another
    /// test over the process-wide `CADENCE_PM_DIR`.
    fn memory_pm(&self) -> Result<crate::issue::Pm> {
        crate::issue::Pm::at(&self.pm_dir()?)
    }

    fn reject_memory_identity_claims(params: &Value) -> Result<()> {
        for field in [
            "actor",
            "alias",
            "by",
            "generation",
            "identity",
            "pane",
            "pid",
            "process_start",
            "reviewer",
        ] {
            if params.get(field).is_some() {
                return Err(Error::rejected(format!(
                    "memory identity is connection-bound; request field '{field}' is not accepted"
                )));
            }
        }
        Ok(())
    }

    fn rpc_memory_propose(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_memory_identity_claims(params)?;
        let actor = self.memory_actor(peer_pid)?;
        let pm = self.memory_pm()?;
        let key = required_str(params, "project")?;
        let kind = required_str(params, "kind")?;
        let scope = params
            .get("scope")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| Error::rejected(format!("invalid memory scope: {e}")))?
            .unwrap_or_default();
        memory::propose_native(
            &pm,
            key,
            kind,
            &scope,
            optional_str(params, "source"),
            optional_str(params, "confidence"),
            optional_str(params, "from"),
            optional_str(params, "text"),
            optional_str(params, "id"),
            &actor,
        )
    }

    fn rpc_memory_review(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_memory_identity_claims(params)?;
        let actor = self.memory_actor(peer_pid)?;
        let pm = self.memory_pm()?;
        let request = memory::ReviewRequest {
            operation: required_str(params, "operation")?,
            verdict: required_str(params, "verdict")?,
            evidence: required_str(params, "evidence")?,
            expected_digest: required_str(params, "digest")?,
        };
        memory::submit_review(
            &pm,
            optional_str(params, "project"),
            required_str(params, "slug")?,
            &request,
            &actor,
        )
    }

    fn rpc_memory_finalize(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        Self::reject_memory_identity_claims(params)?;
        if params.get("body").is_some() || params.get("edit").is_some() {
            return Err(Error::rejected(
                "editing memory content during finalization is refused; submit a new proposal",
            ));
        }
        let actor = self.memory_actor(peer_pid)?;
        let pm = self.memory_pm()?;
        let operation = required_str(params, "operation")?;
        match operation {
            "reject" => memory::reject_native(
                &pm,
                optional_str(params, "project"),
                required_str(params, "slug")?,
                &actor,
            ),
            "supersede" => memory::supersede_native(
                &pm,
                optional_str(params, "project"),
                required_str(params, "old")?,
                required_str(params, "new")?,
                &actor,
            ),
            "accept" | "verify" => memory::finalize_native(
                &pm,
                optional_str(params, "project"),
                required_str(params, "slug")?,
                operation,
                required_str(params, "digest")?,
                &actor,
            ),
            _ => Err(Error::rejected("unsupported memory finalization operation")),
        }
    }

    /// The pid a slot request may bind: the socket peer itself or one
    /// of its /proc ancestors — anything else is a foreign pid and the
    /// request is refused, not rebound. `pid` absent means the peer.
    fn claimed_slot_pid(params: &Value, chain: &[u32], peer_pid: u32) -> Result<u32> {
        let pid = optional_u64(params, "pid")
            .map(|p| p as u32)
            .unwrap_or(peer_pid);
        if pid == 0 || !chain.contains(&pid) {
            return Err(Error::rejected(format!(
                "Slot caller pid {peer_pid} cannot claim pid {pid} — it is \
                 not the connection peer or one of its ancestors"
            )));
        }
        Ok(pid)
    }

    /// Non-blocking slot acquire (CAD-113) — the caller polls with a
    /// stable `request_id`; each answer is granted-or-queue-position.
    /// `lane`/`pid` are never taken from the request: identity is the
    /// connection's, and a `pid` claim off the peer's own ancestry (or,
    /// for a strict caller, off its verified segment) is refused.
    fn rpc_slot_acquire(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.revalidate_enrollments()?;
        let who = self.slot_caller(peer_pid)?;
        let kind = SlotKind::parse(required_str(params, "kind")?)?;
        let request_id = required_str(params, "request_id")?;
        if request_id.len() > 128 {
            return Err(Error::rejected("Slot request_id must be <= 128 bytes"));
        }
        let pid = Self::claimed_slot_pid(params, who.chain(), peer_pid)?;
        // `probe` is the read-only fast-fail: it answers granted or
        // position without leaving a waiter in the queue.
        let probe = params["probe"].as_bool().unwrap_or(false);
        // `exec` (CAD-230b, `build-slot run`): the requesting peer IS the
        // process that execs into the command, so it must claim itself —
        // never an ancestor. A strict caller's hold is then exec-bound;
        // a pane caller's legacy hold is otherwise unchanged.
        let exec = params["exec"].as_bool().unwrap_or(false);
        if exec && pid != peer_pid {
            return Err(crate::slots::exec_not_peer(pid));
        }
        let clk = crate::slots::SlotClock::at((self.slot_clock)(), epoch_secs());
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let (result, events) = match &who {
            SlotWho::Pane { lane, .. } => slots.acquire(kind, lane, pid, request_id, probe, clk)?,
            SlotWho::Strict(caller) => {
                slots.acquire_strict_bound(kind, caller, pid, request_id, probe, clk, exec)?
            }
        };
        drop(slots);
        self.emit_slot_events(events);
        Ok(result)
    }

    /// `slot_release` — the release must name the holding (lane,
    /// pid): a token alone is not authority to free another
    /// caller's slot. Both come from the connection: the lane is the
    /// peer's derived pane (or enrollment owner) and the pid must be on
    /// the peer's own ancestry, so a caller can only ever name its own
    /// lineage.
    fn rpc_slot_release(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.revalidate_enrollments()?;
        let who = self.slot_caller(peer_pid)?;
        let token = required_str(params, "token")?;
        let pid = Self::claimed_slot_pid(params, who.chain(), peer_pid)?;
        let now = (self.slot_clock)();
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let (result, events) = match &who {
            SlotWho::Pane { lane, .. } => slots.release(token, lane, pid, now)?,
            SlotWho::Strict(caller) => slots.release_strict(token, caller, pid, now)?,
        };
        drop(slots);
        self.emit_slot_events(events);
        Ok(result)
    }

    /// `slot_status` — the pools and queue are public, but a hold's
    /// token shows only to its owner: the caller whose derived lane
    /// matches the hold and whose own ancestry includes the hold's
    /// pid. A `lane` param is ignored — identity is the connection's.
    fn rpc_slot_status(&self, _params: &Value, peer_pid: u32) -> Result<Value> {
        self.revalidate_enrollments()?;
        let who = self.slot_caller(peer_pid)?;
        let (status, events) = self.slots.lock().unwrap_or_else(|e| e.into_inner()).status(
            crate::slots::SlotCaller {
                lane: who.lane(),
                pids: who.chain(),
            },
            (self.slot_clock)(),
        );
        self.emit_slot_events(events);
        Ok(status)
    }

    /// Operator authority on POSITIVE proof only (CAD-276) — for
    /// `slot_reconcile` and the approval-evidence verbs (CAD-217), named
    /// by `verb` in refusals: deriving no slot identity is not enough —
    /// a detached child of a pane or managed tool derives none. See
    /// [`crate::peer::operator_proof`] for the checks; anything
    /// unreadable or ambiguous refuses.
    fn proven_operator(&self, verb: &str, peer_pid: u32) -> Result<()> {
        self.operator_evidence(peer_pid).map_err(|why| {
            Error::rejected(format!(
                "{verb} is an operator action — this connection is not \
                 provably the operator: {why}; run it from an attached operator \
                 shell outside every pane and managed endpoint"
            ))
        })
    }

    /// [`crate::peer::operator_proof`] against the live panes and
    /// enrollments — `Err` names the first check that failed.
    fn operator_evidence(&self, peer_pid: u32) -> std::result::Result<(), String> {
        let panes: HashMap<u32, String> = self
            .store
            .pty_endpoint_facts()
            .map_err(|e| {
                format!(
                    "the registered panes cannot be read to prove this connection is not one ({e})"
                )
            })?
            .into_iter()
            .map(|(alias, (_, pane_pid, _))| (pane_pid, alias))
            .collect();
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        crate::peer::operator_proof(
            peer_pid,
            unsafe { libc::geteuid() },
            std::process::id(),
            &panes,
            |pid| slots.nearest_enrolled_root(&[pid]).is_some(),
        )
    }

    /// `slot_reconcile` — the one mutating operator path over a strict
    /// hold (CAD-230). Operator authority is the connection's: a caller
    /// that derives ANY slot identity (a pane or an enrolled endpoint)
    /// is an agent and is refused, and so is one that is not PROVABLY
    /// the operator ([`Self::proven_operator`], CAD-276);
    /// identity-shaped request fields are refused rather than read. The
    /// daemon frees the hold only on its own proof of the holder's
    /// death; see [`Slots::reconcile`].
    fn rpc_slot_reconcile(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        for field in ["by", "operator", "actor", "alias", "lane", "pid"] {
            if params.get(field).is_some() {
                return Err(Error::rejected(format!(
                    "slot reconcile authority is connection-bound; request field \
                     '{field}' is not accepted"
                )));
            }
        }
        if let Some(who) = self.slot_identity(peer_pid)? {
            return Err(Error::rejected(format!(
                "slot reconcile is an operator action — this connection is agent \
                 '{}'; run it outside every pane and managed endpoint",
                who.lane()
            )));
        }
        self.proven_operator("slot reconcile", peer_pid)?;
        let enrollment = required_str(params, "enrollment_id")?;
        let token = required_str(params, "token")?;
        let evidence = params
            .get("evidence")
            .filter(|e| e.is_object())
            .ok_or_else(|| Error::rejected("slot reconcile needs an evidence object"))?;
        let (result, events) = self
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reconcile(enrollment, token, evidence, (self.slot_clock)())?;
        self.emit_slot_events(events);
        Ok(result)
    }

    /// Who may ask the daemon to launch a runner (CAD-230b), from the
    /// connection alone: a pane agent (the legacy derivation), an ACTIVE
    /// enrolled managed endpoint (phase a), or — deriving neither — the
    /// proven operator ([`crate::peer::operator_proof`]). A runner's own
    /// process tree, a revoked or expired endpoint, a failed strict
    /// verification and anything unproven are refused, naming the rule.
    fn launch_requester(&self, peer_pid: u32) -> Result<crate::runner::Requester> {
        let requester = |kind: &str, lane: String| crate::runner::Requester {
            kind: kind.to_string(),
            lane,
        };
        match self.slot_identity(peer_pid)? {
            Some(SlotWho::Pane { lane, .. }) => Ok(requester("pane", lane)),
            Some(SlotWho::Strict(caller)) => {
                let lane = self
                    .slots
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .launch_lane(&caller, (self.slot_clock)())?;
                Ok(requester("managed", lane))
            }
            // `(operator)` can never be an agent alias, so the operator's
            // runners never share a lane (or its events) with an agent.
            None => match self.operator_evidence(peer_pid) {
                Ok(()) => Ok(requester("operator", OPERATOR_LANE.to_string())),
                Err(why) => Err(Error::rejected(format!(
                    "build-slot launch needs a pane agent, an enrolled managed \
                     endpoint or the proven operator — this connection derives no \
                     slot identity and is not provably the operator: {why}. Launch \
                     from an agent's pane or managed endpoint, or from an attached \
                     operator shell"
                ))),
            },
        }
    }

    /// The tracker dir recipes and memory are read from: this daemon's own
    /// `CADENCE_PM_DIR` (its per-instance env — tests), else the
    /// process default.
    fn pm_dir(&self) -> Result<PathBuf> {
        match self.provider_env.var("CADENCE_PM_DIR") {
            Some(dir) if !dir.is_empty() => Ok(PathBuf::from(dir)),
            _ => crate::issue::default_dir(),
        }
    }

    /// `slot_launch` (CAD-230b) — run one of a project's recipes as a
    /// daemon-launched runner. The request names only the recipe, the
    /// project, optionally a checkout of one of its registered repos and
    /// how long to queue; anything command- or identity-shaped is
    /// refused. The daemon resolves the launch intent from project
    /// config, writes its digest bound to a fresh runner id BEFORE
    /// spawning, spawns the gated process (its own process group, output
    /// to `<state>/runners/<id>.log`), enrolls it as root = worker and
    /// hands the queue wait, the grant, the gate and the exit receipt to
    /// a runner thread. Answers at once with the runner id; the CLI
    /// polls `slot_runner`.
    fn rpc_slot_launch(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        const FIELDS: [&str; 4] = ["recipe", "project", "worktree", "wait_secs"];
        if let Some(extra) = params
            .as_object()
            .and_then(|o| o.keys().find(|k| !FIELDS.contains(&k.as_str())))
        {
            return Err(Error::rejected(format!(
                "build-slot launch takes only recipe, project, worktree and \
                 wait_secs — '{extra}' is refused: a recipe's argv, cwd and env \
                 come only from project config, and who is asking only from the \
                 connection"
            )));
        }
        self.revalidate_enrollments()?;
        let requester = self.launch_requester(peer_pid)?;
        let recipe = required_str(params, "recipe")?;
        let project = required_str(params, "project")?;
        let worktree = optional_text(params, "worktree")?.map(Path::new);
        let wait_secs = match params.get("wait_secs") {
            None | Some(Value::Null) => RUNNER_WAIT_SECS,
            Some(v) => v
                .as_u64()
                .ok_or_else(|| Error::rejected("wait_secs must be a whole number of seconds"))?
                .min(RUNNER_MAX_WAIT_SECS),
        };
        let intent = crate::runner::resolve(&self.pm_dir()?, project, recipe, worktree)?;
        let log = crate::runner::log_path(&self.state_dir, &intent.runner_id);
        let mut receipt =
            crate::runner::Receipt::pending(&intent, requester.clone(), &log, epoch_secs());
        // The digest is bound to the runner id durably before anything
        // is spawned.
        crate::runner::write_receipt(&self.state_dir, &receipt)?;
        let env: Vec<(String, String)> = intent
            .env
            .iter()
            .filter_map(|name| self.provider_env.var(name).map(|v| (name.clone(), v)))
            .collect();
        let child = match crate::runner::spawn_gated(&intent, &env, &log) {
            Ok(child) => child,
            Err(e) => {
                receipt.finish("refused", Some(e.to_string()), epoch_secs());
                let _ = crate::runner::write_receipt(&self.state_dir, &receipt);
                return Err(e);
            }
        };
        // The gate holds the recipe until the go line; any refusal from
        // here on closes it, so nothing ever runs unenrolled or unslotted.
        let refuse = |mut child: std::process::Child,
                      mut receipt: crate::runner::Receipt,
                      e: Error|
         -> Result<Value> {
            drop(child.stdin.take());
            let _ = child.wait();
            receipt.finish("refused", Some(e.to_string()), epoch_secs());
            let _ = crate::runner::write_receipt(&self.state_dir, &receipt);
            Err(e)
        };
        let clk = crate::slots::SlotClock::at((self.slot_clock)(), epoch_secs());
        let enrolled = self
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .enroll_runner(
                &requester.lane,
                &intent.runner_id,
                &intent.digest,
                child.id(),
                clk,
            );
        let (enrollment_id, root, events) = match enrolled {
            Ok(enrolled) => enrolled,
            Err(e) => return refuse(child, receipt, e),
        };
        self.emit_slot_events(events);
        receipt.state = "queued".into();
        receipt.pid = Some(root.pid);
        receipt.starttime = Some(root.starttime);
        receipt.enrollment_id = Some(enrollment_id.clone());
        if let Err(e) = crate::runner::write_receipt(&self.state_dir, &receipt) {
            let events = self
                .slots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .end_runner(
                    &enrollment_id,
                    "runner receipt unwritable",
                    (self.slot_clock)(),
                );
            self.emit_slot_events(events);
            return refuse(child, receipt, e);
        }
        let answer = json!({
            "runner_id": intent.runner_id, "state": "queued",
            "project": intent.project, "recipe": intent.recipe,
            "kind": intent.kind.as_str(), "digest": intent.digest,
            "head_sha": intent.head_sha, "log_path": receipt.log_path,
            "lane": requester.lane, "requester": requester.kind,
        });
        let shared = Arc::clone(self);
        let kind = intent.kind;
        thread::spawn(move || shared.run_runner(child, receipt, kind, wait_secs));
        Ok(answer)
    }

    /// One runner's life after launch (CAD-230b): queue for its slot as
    /// the exact enrolled process, and only once granted record
    /// `running` and open the gate — a crash after that write reads as
    /// `unknown`, never as "never ran". Then wait for the exit (the
    /// daemon is the parent, so it reaps), record the exit receipt, let
    /// the tri-state reaper free the hold on the now-dead holder, and
    /// revoke the enrollment. A refusal, a queue timeout or a closing
    /// daemon closes the gate instead: the recipe never starts.
    fn run_runner(
        self: Arc<Self>,
        mut child: std::process::Child,
        mut receipt: crate::runner::Receipt,
        kind: SlotKind,
        wait_secs: u64,
    ) {
        let enrollment = receipt.enrollment_id.clone().unwrap_or_default();
        let deadline = Instant::now() + Duration::from_secs(wait_secs);
        let mut gate = child.stdin.take();
        let queued: std::result::Result<(), (&str, String)> = loop {
            if self.closing.load(Ordering::SeqCst) {
                break Err((
                    "cancelled",
                    "the daemon is shutting down — the gate never opened".into(),
                ));
            }
            let clk = crate::slots::SlotClock::at((self.slot_clock)(), epoch_secs());
            let polled = self
                .slots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .acquire_runner(&enrollment, kind, clk);
            match polled {
                Ok((answer, events)) => {
                    self.emit_slot_events(events);
                    if answer["granted"].as_bool() == Some(true) {
                        break Ok(());
                    }
                }
                Err(e) => break Err(("refused", e.to_string())),
            }
            if let Ok(Some(status)) = child.try_wait() {
                break Err((
                    "refused",
                    format!("the runner process ended before its slot was granted ({status})"),
                ));
            }
            if Instant::now() >= deadline {
                break Err((
                    "timed_out",
                    format!(
                        "no {} slot within {wait_secs}s — the gate never opened",
                        kind.as_str()
                    ),
                ));
            }
            thread::sleep(Duration::from_millis(250));
        };
        match queued {
            Err((state, why)) => {
                drop(gate.take());
                let _ = child.wait();
                receipt.finish(state, Some(why), epoch_secs());
            }
            Ok(()) => {
                // The digest names a source HEAD; a checkout that moved
                // while this runner queued is not that source.
                let head = crate::runner::head_of(Path::new(&receipt.worktree));
                if head.as_deref() != Some(receipt.head_sha.as_str()) {
                    drop(gate.take());
                    let _ = child.wait();
                    receipt.finish(
                        "refused",
                        Some(format!(
                            "the checkout's HEAD moved while queued ({} → {}) — the \
                             gate never opened; launch again to bind the new source",
                            receipt.head_sha,
                            head.as_deref().unwrap_or("unreadable")
                        )),
                        epoch_secs(),
                    );
                    return self.finish_runner(&enrollment, receipt);
                }
                // A closing daemon opens no gate, even on a grant that
                // raced its shutdown.
                if self.closing.load(Ordering::SeqCst) {
                    drop(gate.take());
                    let _ = child.wait();
                    return;
                }
                receipt.dirty |= crate::runner::is_dirty(Path::new(&receipt.worktree));
                receipt.state = "running".into();
                receipt.started = Some(epoch_secs());
                let opened = crate::runner::write_receipt(&self.state_dir, &receipt).is_ok()
                    && gate.as_mut().is_some_and(|g| {
                        g.write_all(crate::runner::go_line(&receipt.runner_id).as_bytes())
                            .and_then(|_| g.flush())
                            .is_ok()
                    });
                drop(gate.take());
                // The recipe's process group ends with it: once the root
                // has exited — observed WITHOUT reaping it, so its pid
                // (the group id) cannot be reused yet — any straggler
                // left in the group (a backgrounded job, the rustc of a
                // killed cargo) is killed, so nothing keeps building
                // outside the slot that is about to free.
                end_process_group(child.id());
                let status = child.wait();
                match (opened, status) {
                    (false, _) => receipt.finish(
                        "refused",
                        Some(
                            "the running receipt could not be written — the gate stayed closed"
                                .into(),
                        ),
                        epoch_secs(),
                    ),
                    (true, Ok(status)) => {
                        use std::os::unix::process::ExitStatusExt;
                        receipt.exit_code = status.code();
                        receipt.signal = status.signal();
                        receipt.finish("exited", None, epoch_secs());
                    }
                    (true, Err(e)) => receipt.finish(
                        "unknown",
                        Some(format!("waiting for the runner failed: {e}")),
                        epoch_secs(),
                    ),
                }
            }
        }
        self.finish_runner(&enrollment, receipt);
    }

    /// Close a runner: free its hold (proof of death only), revoke its
    /// enrollment, write the exit receipt and tell the requester's lane.
    fn finish_runner(&self, enrollment: &str, receipt: crate::runner::Receipt) {
        // A daemon that is closing owns no state any more — its successor
        // does, once the singleton frees. It writes nothing: the next boot
        // reports this runner `unknown` rather than race that daemon's
        // `slots.json` and receipt.
        if self.closing.load(Ordering::SeqCst) {
            return;
        }
        let reason = format!("runner {} {}", receipt.runner_id, receipt.state);
        let events = self
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .end_runner(enrollment, &reason, (self.slot_clock)());
        self.emit_slot_events(events);
        if let Err(e) = crate::runner::write_receipt(&self.state_dir, &receipt) {
            eprintln!(
                "runner {}: exit receipt write failed: {e}",
                receipt.runner_id
            );
        }
        let _ = self.store.event_public(
            &receipt.requester.lane,
            "runner_finished",
            json!({"runner_id": receipt.runner_id, "recipe": receipt.recipe,
                   "project": receipt.project, "state": receipt.state,
                   "exit_code": receipt.exit_code, "signal": receipt.signal,
                   "digest": receipt.digest, "head_sha": receipt.head_sha}),
        );
        self.wake();
    }

    /// `slot_runner` — one runner's receipt. Readable by whoever may ask
    /// about slots at all: any connection with a slot identity, or the
    /// proven operator. Receipts carry no credential (never a token).
    fn rpc_slot_runner(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        if self.slot_identity(peer_pid)?.is_none() {
            self.proven_operator("slot runner", peer_pid)?;
        }
        let id = required_str(params, "runner_id")?;
        let receipt = crate::runner::read_receipt(&self.state_dir, id)?;
        serde_json::to_value(&receipt).map_err(|e| Error::internal(e.to_string()))
    }

    /// CAD-230b: a strict hold ends with its exact holder, observed by
    /// the daemon itself — no release call and no other client's slot
    /// call needed. Legacy holds keep their reap-on-call rule.
    fn run_slot_watch(self: &Arc<Self>) {
        while !self.closing.load(Ordering::SeqCst) {
            let events = self
                .slots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .reap_strict_holds((self.slot_clock)());
            self.emit_slot_events(events);
            let deadline = Instant::now() + SLOT_WATCH_TICK;
            while !self.closing.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    /// Operator authority for the approval-evidence verbs (CAD-217):
    /// exactly the connection-bound rule `slot_reconcile` applies. A
    /// caller whose `SO_PEERCRED` ancestry reaches a registered pane or
    /// an enrolled managed endpoint is an agent and is refused, and so
    /// is one that is not provably the operator
    /// ([`Self::proven_operator`]). Identity-shaped request fields are
    /// refused rather than read — a worker's output or message can
    /// never name who recorded the evidence. Residual (CAD-276's, see
    /// docs/AUDIT.md; CAD-280 replaces the rule): a same-uid process
    /// that leaves every agent's ancestry without orphaning its session
    /// and scrubs its env and stdio still passes.
    fn approval_operator(&self, verb: &str, params: &Value, peer_pid: u32) -> Result<()> {
        for field in [
            "by",
            "operator",
            "actor",
            "alias",
            "lane",
            "pid",
            "pane",
            "recorded_via",
        ] {
            if params.get(field).is_some() {
                return Err(Error::rejected(format!(
                    "{verb} authority is connection-bound; request field \
                     '{field}' is not accepted"
                )));
            }
        }
        if let Some(who) = self.slot_identity(peer_pid)? {
            return Err(Error::rejected(format!(
                "{verb} is an operator action — this connection is agent \
                 '{}'; run it outside every pane and managed endpoint",
                who.lane()
            )));
        }
        self.proven_operator(verb, peer_pid)
    }

    /// `approval_record` — persist an operator's merge approval for one
    /// exact head as audit evidence (`id` optional: the store picks a
    /// fresh default, see `Store::record_approval`). It grants nothing: dispatch and
    /// merge never read it; `cadence audit` binds it to the landed head.
    fn rpc_approval_record(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.approval_operator("approval record", params, peer_pid)?;
        let pr = params
            .get("pr")
            .and_then(Value::as_u64)
            .ok_or_else(|| Error::rejected("Missing or non-numeric 'pr'"))?;
        let approval = store::NewApproval {
            id: optional_str(params, "id"),
            source: required_str(params, "source")?,
            action: optional_str(params, "action").unwrap_or("merge"),
            head_sha: required_str(params, "head")?,
            repo: required_str(params, "repo")?,
            pr,
        };
        let (new, id) = self
            .store
            .record_approval(&approval, APPROVAL_RECORDED_VIA)?;
        Ok(json!({
            "state": "recorded",
            "duplicate": !new,
            "approval_id": id,
            "source": approval.source,
            "action": approval.action,
            "head_sha": approval.head_sha,
            "scope": {"repo": approval.repo, "pr": approval.pr},
            "recorded_via": APPROVAL_RECORDED_VIA,
        }))
    }

    /// `approval_revoke` — the only way an approval is withdrawn. A
    /// cancelled or superseded message never reaches this.
    fn rpc_approval_revoke(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.approval_operator("approval revoke", params, peer_pid)?;
        let id = required_str(params, "id")?;
        let source = required_str(params, "source")?;
        let reason = required_str(params, "reason")?;
        let new = self
            .store
            .revoke_approval(id, source, reason, APPROVAL_RECORDED_VIA)?;
        Ok(json!({
            "state": "revoked",
            "duplicate": !new,
            "approval_id": id,
            "source": source,
            "reason": reason,
            "recorded_via": APPROVAL_RECORDED_VIA,
        }))
    }

    /// CAD-360: the plan gate for every daemon dispatch of a task —
    /// `task_dispatch` and both monitor paths. A task whose job is bound
    /// to a tracker issue dispatches only if [`crate::issue::plan::gate_id`]
    /// passes. Fail closed: a task or job that cannot be read, or a
    /// tracker dir that cannot be resolved, refuses; only a job with no
    /// issue, or an issue no project holds, passes unchecked.
    fn plan_gate_task(&self, task_id: &str) -> Result<()> {
        let task = self.store.task(task_id)?;
        let job = self.store.job(&task.job_id)?;
        let Some(issue) = job.issue_id else {
            return Ok(());
        };
        let pm_dir = self.pm_dir().map_err(|e| {
            Error::invalid(
                "plan_unreadable",
                format!("tracker dir for {issue} cannot be resolved ({e}) — refused"),
            )
        })?;
        crate::issue::plan::gate_id(&pm_dir, &issue)
    }

    /// CAD-359 `plan_propose` — write a plan (epic + tickets, one
    /// tracker commit) and emit `plan_proposed` on the daemon stream for
    /// a UI's plan card. The proposer is the connection's: a pane or
    /// managed endpoint's lane, else the proven operator; anything
    /// unattributable is refused, as is an identity-shaped field.
    fn rpc_plan_propose(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        for field in [
            "by",
            "actor",
            "alias",
            "lane",
            "pane",
            "pid",
            "operator",
            "proposed_by",
        ] {
            if params.get(field).is_some() {
                return Err(Error::rejected(format!(
                    "plan propose attribution is connection-bound; request field \
                     '{field}' is not accepted"
                )));
            }
        }
        let actor = match self.slot_identity(peer_pid)? {
            Some(who) => who.lane().to_string(),
            None => match self.operator_evidence(peer_pid) {
                Ok(()) => "operator".to_string(),
                Err(why) => {
                    return Err(Error::rejected(format!(
                        "plan propose needs an attributable caller — a pane agent, an \
                         enrolled managed endpoint or the proven operator: {why}"
                    )))
                }
            },
        };
        let project = required_str(params, "project")?;
        let text = required_str(params, "text")?;
        let pm = crate::issue::Pm::at(&self.pm_dir()?)?;
        let allow = crate::secret::Allowlist::load(&self.state_dir)?;
        let out = crate::issue::plan::propose(&pm, project, text, &allow, &actor)?;
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "plan_proposed",
            json!({
                "epic": out["epic"],
                "project": out["project"],
                "title": out["title"],
                "tickets": out["tickets"],
                "ticket_count": out["tickets"].as_array().map_or(0, Vec::len),
                "proposed_by": out["proposed_by"],
            }),
        );
        self.wake();
        Ok(out)
    }

    /// CAD-360 `plan_approve` / `plan_reject` — operator only, exactly
    /// the connection-bound rule of the approval-evidence verbs
    /// ([`Self::approval_operator`]): an agent caller is refused, and a
    /// caller with no agent identity must be the proven operator
    /// (CAD-276). The decision is a tracker commit; approval moves the
    /// plan's backlog tickets to ready.
    fn rpc_plan_decide(&self, params: &Value, peer_pid: u32, approve: bool) -> Result<Value> {
        let verb = if approve {
            "plan approve"
        } else {
            "plan reject"
        };
        self.approval_operator(verb, params, peer_pid)?;
        let epic = required_str(params, "epic")?;
        let pm = crate::issue::Pm::at(&self.pm_dir()?)?;
        let out = crate::issue::write::decide_plan(
            &pm,
            epic,
            approve,
            "operator",
            optional_str(params, "reason"),
        )?;
        let kind = if approve {
            "plan_approved"
        } else {
            "plan_rejected"
        };
        let _ = self.store.event_public(DAEMON_ALIAS, kind, out.clone());
        self.wake();
        Ok(out)
    }

    fn rpc_register(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = required_str(params, "alias")?;
        let provider = required_str(params, "provider")?;
        let endpoint =
            optional_str(params, "endpoint_kind").unwrap_or(registry::DEFAULT_ENDPOINT_KIND);
        let role = optional_str(params, "role").unwrap_or("worker");
        let team_role = optional_text(params, "team_role")?;
        let model_policy = optional_text(params, "model_policy")?;
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
            team_role,
            model_policy,
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

    fn rpc_model_defaults_get(self: &Arc<Self>) -> Result<Value> {
        self.model_defaults_snapshot()
    }

    fn rpc_model_defaults_set(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let document = match params.get("document") {
            Some(Value::String(raw)) => raw.as_str(),
            Some(_) => {
                return Err(Error::invalid(
                    "invalid_request",
                    "'document' must be a string",
                ))
            }
            None => {
                return Err(Error::invalid(
                    "invalid_request",
                    "Missing required parameter 'document'",
                ))
            }
        };
        let attribution = optional_text(params, "attribution")?;
        self.store.replace_model_defaults(document, attribution)?;
        self.model_defaults_snapshot()
    }

    fn model_defaults_snapshot(self: &Arc<Self>) -> Result<Value> {
        let snapshot = self.store.model_defaults()?;
        let agents = self.store.agents()?;
        let observed: Vec<crate::model_defaults::ObservedModel> = agents
            .iter()
            .map(|agent| crate::model_defaults::ObservedModel {
                provider: agent.provider.as_str(),
                configured: agent
                    .params
                    .as_ref()
                    .and_then(|params| params.get("model"))
                    .and_then(Value::as_str),
                reported: agent.model.as_deref(),
            })
            .collect();
        Ok(crate::model_defaults::snapshot_json(
            snapshot.revision,
            &snapshot.config,
            &observed,
        ))
    }

    /// `agent_send` without a connection to attribute (unit tests):
    /// a threaded agent records the message as unattributed.
    #[cfg(test)]
    fn rpc_send(self: &Arc<Self>, params: &Value) -> Result<Value> {
        self.send_as(params, &|_| store::Sender::Unattributed)
    }

    /// `agent_send` over the socket: a threaded agent's chat records
    /// who queued it, derived from the connection (CAD-319).
    fn rpc_send_from(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        self.send_as(params, &|alias| self.thread_sender(alias, peer_pid))
    }

    /// Who queued a message for `alias`'s thread. Only computed for an
    /// alias that has one — the identity walk is not free. An agent
    /// connection is that agent; no agent is the operator by default
    /// (not proof — CAD-313); an underivable caller is unattributed.
    fn thread_sender(&self, alias: &str, peer_pid: u32) -> store::Sender {
        match self.store.thread(alias) {
            Ok(Some(_)) => {}
            _ => return store::Sender::Unattributed,
        }
        match self.caller_identity(peer_pid) {
            Ok(Caller::Agent(v)) => store::Sender::Agent(v.agent.alias.clone()),
            Ok(Caller::NoAgentIdentity) => store::Sender::Operator,
            Err(_) => store::Sender::Unattributed,
        }
    }

    fn send_as(
        self: &Arc<Self>,
        params: &Value,
        sender_of: &dyn Fn(&str) -> store::Sender,
    ) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let text = required_str(params, "text")?;
        let target = self.store.agent_opt(&alias)?;
        // A pty endpoint pastes literally and fails a body with control
        // characters at delivery; refuse it here so `send` never answers
        // `queued` for a message that cannot be delivered (CAD-218).
        let pty = target.as_ref().is_some_and(|a| a.endpoint_kind == "pty");
        // CAD-250: `--nudge` — turnless steering for a live pty pane. The
        // flag is the only way in: a caller-supplied `source: "nudge"`
        // takes the same checks rather than bypassing them.
        let nudge = params
            .get("nudge")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || optional_str(params, "source") == Some(store::NUDGE_SOURCE);
        if nudge {
            if optional_str(params, "task").is_some() {
                return Err(Error::rejected(
                    "--nudge is steering, not task work — it takes no --task",
                ));
            }
            if text.chars().count() > NUDGE_MAX_CHARS {
                return Err(Error::rejected(format!(
                    "a nudge is at most {NUDGE_MAX_CHARS} characters — send longer \
                     guidance as a normal message or a file path"
                )));
            }
            if let Some(agent) = target.as_ref().filter(|a| a.endpoint_kind != "pty") {
                return Err(Error::rejected(format!(
                    "--nudge only applies to pty endpoints — '{alias}' is {}/{}; \
                     send it a normal message instead",
                    agent.provider, agent.endpoint_kind
                )));
            }
            if optional_str(params, "reply_to").is_some() {
                return Err(Error::rejected(
                    "--nudge owes no report, so it takes no reply_to",
                ));
            }
            // N1: a nudge is for a pane that exists now — never queued for
            // a stopped or fenced agent to receive later.
            let live = self.lifecycle.lock().unwrap().agents.contains_key(&alias)
                && target.as_ref().is_some_and(|a| {
                    a.endpoint.is_some() && matches!(a.state.as_str(), "idle" | "busy")
                });
            if !live {
                return Err(Error::rejected(format!("agent {alias} has no live pane")));
            }
        }
        if pty && crate::adapter::pty::has_control_chars(text) {
            return Err(Error::rejected(
                "PTY messages must be a single line without control characters \
                 — put a long body in a file and send its path",
            ));
        }
        // `send --task` attaches the delivery to a task — ad-hoc
        // PM↔worker follow-up inside a job's delivery record. CAD-160:
        // an open task's message is composed to restate its objective
        // and outstanding criteria, fitted to the endpoint's ceiling.
        let task = optional_str(params, "task");
        let composed = match task {
            Some(task) => {
                let ceiling = if pty {
                    crate::adapter::pty::MAX_BODY
                } else {
                    store::ENQUEUE_BYTES
                };
                Some(
                    self.store
                        .compose_task_message(task, &alias, text, ceiling)?,
                )
            }
            None => None,
        };
        let text = composed.as_deref().unwrap_or(text);
        // An explicit reply_to always wins; absent one, a worker joined
        // to a group (params.upstream) reports results to its PM by
        // default. `enqueue` still validates the target.
        // A nudge owes no report: no upstream default either.
        let reply_to = optional_str(params, "reply_to")
            .map(str::to_string)
            .or_else(|| (!nudge).then(|| self.upstream_of(&alias)).flatten());
        let message = optional_str(params, "message")
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        // Caller-supplied provenance (`bootstrap` from join, etc.).
        // Identifier-charset only — internal sources like
        // `worker_result` contain characters this rejects, so the
        // internal routing contract cannot be forged through agent_send.
        let source = if nudge {
            store::NUDGE_SOURCE
        } else {
            optional_str(params, "source").unwrap_or("user")
        };
        proto::identifier(source, "Message source")?;
        let sender = sender_of(&alias);
        let (duplicate, state) = self.store.enqueue_sent(
            &alias,
            text,
            reply_to.as_deref(),
            &message,
            source,
            task,
            &sender,
        )?;
        self.notify_agent(&alias);
        self.wake();
        let mut receipt = json!({"message": message, "state": state, "duplicate": duplicate});
        // CAD-251: an undrained mailbox warns the sender — never refuses.
        if let Some(warning) = self.inbox_warning(&alias) {
            receipt["warning"] = json!(warning);
        }
        Ok(receipt)
    }

    /// Send and wait for the message's terminal state, bounded by `wait`.
    fn rpc_ask(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let wait = optional_u64(params, "wait").unwrap_or(120).min(600);
        let result = self.rpc_send_from(params, peer_pid)?;
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
        let raw = required_str(params, "alias")?;
        // The daemon stream has no agents row — read-only, so a plain
        // match suffices; every other RPC keeps resolving to an agent.
        let alias = if raw == DAEMON_ALIAS {
            raw.to_string()
        } else {
            self.resolve_alias(raw)?
        };
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
                "devin/user_input" => {
                    let from_answers = answers.as_ref().and_then(|value| {
                        value
                            .get("message")
                            .or_else(|| value.get("text"))
                            .and_then(Value::as_str)
                    });
                    let text = from_answers
                        .filter(|text| !text.trim().is_empty())
                        .or_else(|| decision.filter(|text| !text.trim().is_empty()))
                        .ok_or_else(|| {
                            Error::rejected(
                                "Respond with answers.message, answers.text, or --decision text for the Devin session",
                            )
                        })?;
                    json!({"message": text})
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

    /// The caller's derived identity for pane-attention verbs
    /// (`answer`), from three signals: `/proc` ancestry from the
    /// `SO_PEERCRED` pid (a pid that descends from an agent's pane root
    /// IS that agent — the one unforgeable signal), the pane's own
    /// `CADENCE_ALIAS` env the peer still carries (a `setsid` detach
    /// keeps it), and a shared pty via fd targets (detach keeps stdio).
    /// The last two are caller-choosable, which is safe here because
    /// they only narrow (see `src/peer.rs`, CAD-276). The signals are
    /// [`PeerTies`] — the one rule the
    /// board's write identity shares (CAD-263). A pane must never act
    /// on its own pane state: a worker that can reach the socket could
    /// otherwise self-sanction the very decision the menu exists to
    /// gate.
    ///
    /// Deterministic: the target's pane is checked first — self-refusal
    /// never loses to map order — then the others sorted by alias.
    /// Fails closed: the fleet map itself must load (a store error is
    /// a refusal, never an empty map that skips the self-check), and a
    /// caller whose ancestry cannot be fully walked is never stamped
    /// `operator` — while the target's pane is alive the ambiguity is
    /// a refusal, after it is gone the stamp is `unknown`. `operator`
    /// requires positive terminal evidence — the peer holding a pty
    /// that is no pane's; a fully detached caller (no ancestry hit, no
    /// env alias, no tty) matches nothing and is honestly `unknown`.
    /// Returns `(by, by_kind)`; callers record `claimed_by` separately
    /// when the supplied `by` disagrees.
    fn derived_caller(
        &self,
        alias: &str,
        peer_pid: u32,
        verb: &str,
    ) -> Result<(String, &'static str)> {
        let facts = self.store.pty_endpoint_facts()?;
        let peer = PeerTies::probe(peer_pid);
        if let Some((_, pane_pid, _)) = facts.get(alias) {
            if peer.tied_to(alias, *pane_pid) {
                return Err(Error::rejected(format!(
                    "a pane cannot {verb} its own pane — the caller is tied \
                     to the target's pane process",
                )));
            }
        }
        let others = facts
            .iter()
            .filter(|(a, _)| a.as_str() != alias)
            .map(|(a, (_, pane_pid, _))| (a.as_str(), *pane_pid));
        if let Some(agent) = peer.agents(others).into_iter().next() {
            return Ok((agent, "agent"));
        }
        // `/proc/<pid>` is a directory — `read_link` on it is always
        // EINVAL, so liveness is a `metadata` existence check.
        let target_alive = facts
            .get(alias)
            .is_some_and(|(_, pid, _)| std::fs::metadata(format!("/proc/{pid}")).is_ok());
        unmatched_caller(peer.walked(), target_alive, peer.on_tty(), verb)
    }

    /// `agent answer`: one menu-choice keystroke to a pane currently
    /// probing `approval_menu` — the adapter re-probes and refuses
    /// anything else, so the key can never land in a prompt or a
    /// running turn. Records `approval_answered` with who answered
    /// and the menu line the answer went to.
    fn rpc_answer(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let choice = required_str(params, "choice")?;
        let claimed_by = optional_str(params, "by");
        let note = optional_str(params, "note");
        // The answerer's identity is DERIVED, never claimed — see
        // `derived_caller` for the signals and the fail-closed rule.
        let (by, by_kind) = self.derived_caller(&alias, peer_pid, "answer")?;
        let probe = self.adapter_for(&alias)?.answer_approval(choice)?;
        let mut detail = json!({
            "by": by,
            "by_kind": by_kind,
            "caller_pid": peer_pid,
            "choice": choice,
            "line": probe.reason.clone(),
            "probe": probe.to_json(),
        });
        // A supplied `by` that disagrees with the derived identity is
        // preserved as a claim, not an attribution.
        if let Some(c) = claimed_by {
            if c != by {
                detail["claimed_by"] = json!(c);
            }
        }
        if let Some(n) = note {
            detail["note"] = json!(n);
        }
        let _ = self.store.event_public(&alias, "approval_answered", detail);
        self.wake();
        // An answered menu may be exactly what a queued head waits
        // behind — wake the delivery loop rather than leaving it to
        // sit out the gate backoff.
        self.notify_agent(&alias);
        Ok(json!({"alias": alias, "state": "answered", "choice": choice}))
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

    /// Explicit ack/result report for a running message. The `token` is
    /// the `turn_id` minted at submission; it embeds the endpoint
    /// generation under the endpoint's own scheme (CAD-162:
    /// `registry::turn_token_current`), so a report aimed at a previous
    /// endpoint life — or carrying another endpoint kind's token — is
    /// rejected as stale, and an endpoint with no checkable scheme
    /// refuses every report. Callers are identified by possession of
    /// the token, which is self-asserted — not an authentication.
    ///
    /// On an endpoint whose adapter turn result completes the message
    /// (managed), only `ack` is accepted: the turn result is the one
    /// writer of the outcome, so a reported `result` would race it.
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
        if !registry::turn_token_current(
            &agent.provider,
            &agent.endpoint_kind,
            agent.generation.as_deref(),
            token,
        ) {
            return Err(Error::rejected(
                "Submission token belongs to a stale endpoint generation",
            ));
        }
        if kind == "result" && registry::reports_turn_result(&agent.provider, &agent.endpoint_kind)
        {
            return Err(Error::rejected(
                "This endpoint's turn result completes the message — report `ack` only",
            ));
        }
        // CAD-341: `check` runs the token/generation/state gates of a
        // result report and changes nothing — `message result --report`
        // files its report only once the daemon would take the result.
        // It also names the board issue the message's task is bound to.
        if params
            .get("check")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            if kind != "result" {
                return Err(Error::rejected("`check` applies to result reports only"));
            }
            if !matches!(message.state.as_str(), "running" | "completed") {
                return Err(Error::rejected(format!(
                    "Message is not awaiting a report (state {})",
                    message.state
                )));
            }
            let issue = message
                .task_id
                .as_deref()
                .and_then(|t| self.store.task(t).ok())
                .and_then(|t| self.store.job(&t.job_id).ok())
                .and_then(|j| j.issue_id);
            let result_text = message.result.as_ref().and_then(|r| r.get("text")).cloned();
            return Ok(json!({"state": message.state, "task": message.task_id,
                             "issue": issue, "result_text": result_text}));
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
                // CAD-250 F1: the `running` check and the finish are one
                // transaction — a report racing the report bound (or a
                // reconcile) either wins outright or is judged against
                // the row as it now stands, never both.
                let message = if message.state == "running" {
                    let stored = json!({
                        "status": "completed", "text": text,
                        "turn_id": token, "via": "pty_report",
                        "sha": sha,
                    });
                    match self
                        .store
                        .finish_running(&message.id, "completed", &stored, None)?
                    {
                        Ok(finished) => {
                            self.notify_routed_target(&finished, &stored);
                            // The report frees the actor's one turn — wake
                            // it so the next queued delivery is claimed
                            // now, not on the idle poll.
                            self.notify_agent(&finished.alias);
                            self.wake();
                            return Ok(json!({"state": "reported", "kind": kind}));
                        }
                        Err(current) => {
                            current.ok_or_else(|| Error::rejected("Unknown message"))?
                        }
                    }
                } else {
                    message
                };
                if message.state == "completed" {
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
        if let Some(result) = message.result.clone() {
            self.notify_routed_target(&message, &result);
        }
        // A live actor holding this cloud turn stops polling it now.
        self.notify_agent(&message.alias);
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
        if let Some(result) = message.result.clone() {
            self.notify_routed_target(&message, &result);
        }
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
                 message — resume refused. {} {}",
                unknown_inspect_lead(),
                unknown_recovery_note()
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
            let message = self.store.reconcile(id, status, note, by, None)?;
            if let Some(result) = message.result.clone() {
                self.notify_routed_target(&message, &result);
            }
            reconciled.push(id.clone());
        }
        self.notify_agent(&alias);
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
            optional_str(params, "task_acceptance"),
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
                            if let Some(view) = self.stall_view(assignee) {
                                view.apply(&mut j);
                            }
                        }
                    }
                    if matches!(task.state.as_str(), "dispatched" | "running")
                        && is_terminal(&m.state)
                        && m.state != "completed"
                    {
                        let assignee = task.assignee.as_deref().unwrap_or("?");
                        j["attention"] = if m.state == "unknown" {
                            json!(unknown_kickoff_attention(mid, assignee))
                        } else {
                            json!(ordinary_terminal_kickoff_attention(
                                mid,
                                &m.state,
                                &task.id,
                                task.revision + 1,
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
        let verdicts = self.store.verdicts_for_task(&task.id)?;
        if let Some(v) = store::current_verdict(task.revision, &verdicts) {
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
        self.plan_gate_task(required_str(params, "task")?)?;
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

    fn rpc_monitor_register(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let tasks = params
            .get("tasks")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::rejected("Monitor registration requires a tasks array"))?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| Error::rejected("Monitor task ids must be strings"))
            })
            .collect::<Result<Vec<_>>>()?;
        let owner = optional_str(params, "owner").unwrap_or("operator");
        let interval = optional_u64(params, "interval_secs").unwrap_or(60);
        let dispatch_enabled = params
            .get("dispatch_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let auto_dispatch_enabled = params
            .get("auto_dispatch_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (monitor, duplicate) = self.store.register_monitor(
            required_str(params, "monitor")?,
            required_str(params, "project")?,
            owner,
            interval,
            &tasks,
            dispatch_enabled,
            auto_dispatch_enabled,
        )?;
        let (monitor, coverage, open, total) = self.store.monitor_view(&monitor.id)?;
        self.wake();
        Ok(json!({
            "monitor": monitor.to_json(&coverage, open, total),
            "duplicate": duplicate,
        }))
    }

    fn rpc_monitor_list(self: &Arc<Self>) -> Result<Value> {
        let mut monitors = Vec::new();
        for monitor in self.store.monitors()? {
            let (monitor, coverage, open, total) = self.store.monitor_view(&monitor.id)?;
            monitors.push(monitor.to_json(&coverage, open, total));
        }
        Ok(json!({"monitors": monitors}))
    }

    fn rpc_monitor_show(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let id = required_str(params, "monitor")?;
        let (monitor, coverage, open, total) = self.store.monitor_view(id)?;
        Ok(json!({"monitor": monitor.to_json(&coverage, open, total)}))
    }

    fn rpc_monitor_heartbeat(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let id = required_str(params, "monitor")?;
        let monitor = self.store.monitor_heartbeat(id)?;
        let (monitor, coverage, open, total) = self.store.monitor_view(&monitor.id)?;
        self.wake();
        Ok(json!({"monitor": monitor.to_json(&coverage, open, total)}))
    }

    fn rpc_monitor_alerts(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let id = required_str(params, "monitor")?;
        let after = optional_i64(params, "after").unwrap_or(0);
        let open_only = params.get("open").and_then(Value::as_bool).unwrap_or(false);
        let limit = optional_i64(params, "limit").unwrap_or(100);
        let alerts = self.store.monitor_alerts(id, after, open_only, limit)?;
        let cursor = alerts.last().map(|a| a.seq).unwrap_or(after);
        Ok(json!({
            "monitor": id,
            "alerts": alerts.iter().map(store::MonitorAlert::to_json).collect::<Vec<_>>(),
            "cursor": cursor,
        }))
    }

    fn rpc_monitor_alert_ack(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let id = required_str(params, "monitor")?;
        let seq = optional_i64(params, "alert")
            .ok_or_else(|| Error::rejected("Monitor alert acknowledgement requires --alert"))?;
        let by = optional_str(params, "by").unwrap_or("operator");
        let alert = self.store.ack_monitor_alert(id, seq, by)?;
        self.wake();
        Ok(json!({"alert": alert.to_json()}))
    }

    fn rpc_monitor_stop(self: &Arc<Self>, params: &Value) -> Result<Value> {
        if optional_str(params, "pane").is_some() {
            return Err(Error::rejected(
                "monitor stop is an operator action — run it outside a cadence pane",
            ));
        }
        let id = required_str(params, "monitor")?;
        let monitor = self.store.stop_monitor(id)?;
        let (monitor, coverage, open, total) = self.store.monitor_view(&monitor.id)?;
        self.wake();
        Ok(json!({"monitor": monitor.to_json(&coverage, open, total)}))
    }

    /// One guarded handoff into the existing job-dispatch transaction. The
    /// public RPC remains an explicit operator action; the monitor watcher
    /// may call the same helper only for a separately persisted automatic
    /// opt-in.
    fn rpc_monitor_dispatch(self: &Arc<Self>, params: &Value) -> Result<Value> {
        if optional_str(params, "pane").is_some() {
            return Err(Error::rejected(
                "monitor dispatch is an operator action — run it outside a cadence pane",
            ));
        }
        let monitor_id = required_str(params, "monitor")?;
        let task_id = required_str(params, "task")?;
        self.monitor_dispatch_task(monitor_id, task_id, false)
    }

    fn monitor_dispatch_task(
        self: &Arc<Self>,
        monitor_id: &str,
        task_id: &str,
        automatic: bool,
    ) -> Result<Value> {
        if automatic {
            self.plan_gate_task(task_id)?;
            // Hold the pending-request mutex across the store transaction.
            // The snapshot contains every alias, while the transaction
            // re-reads the task's current assignee before applying it, so an
            // approval arriving concurrently cannot be missed or bypassed.
            let pending = self.pending.lock().unwrap();
            let pending_aliases: HashSet<String> = pending
                .values()
                .map(|request| request.alias.clone())
                .collect();
            let (task, message, duplicate, behind_dead) =
                self.store.dispatch_automatic_monitor_task(
                    monitor_id,
                    task_id,
                    &pending_aliases,
                    &format!("monitor:{monitor_id}"),
                )?;
            drop(pending);
            let assignee = task.assignee.clone().ok_or_else(|| {
                Error::internal("automatic dispatch returned a task without an assignee")
            })?;
            let _ = self.store.event_public(
                store::Store::DAEMON_STREAM,
                "monitor_dispatch",
                json!({"monitor": monitor_id, "task": task_id,
                       "message": message, "duplicate": duplicate,
                       "queued_behind_dead": behind_dead,
                       "automatic": true}),
            );
            self.notify_agent(&assignee);
            self.wake();
            return Ok(json!({
                "monitor": monitor_id,
                "task": task.to_json(),
                "message": message,
                "duplicate": duplicate,
                "queued_behind_dead": behind_dead,
            }));
        }
        let monitor = self.store.monitor(monitor_id)?;
        if monitor.state != "active" {
            return Err(Error::rejected(format!(
                "Monitor '{monitor_id}' is {} — dispatch requires an active check",
                monitor.state
            )));
        }
        if !monitor.dispatch_enabled {
            return Err(Error::rejected(format!(
                "Monitor '{monitor_id}' has dispatch disabled — enable it explicitly at registration"
            )));
        }
        if !self.store.monitor_is_covered(monitor_id, task_id)? {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is outside monitor '{monitor_id}' coverage"
            )));
        }
        let task = self.store.task(task_id)?;
        let job = self.store.job(&task.job_id)?;
        self.plan_gate_task(task_id)?;
        if job.state != "open" || job.repo.as_deref() != Some(monitor.project.as_str()) {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is not in monitor project '{}' with an open job",
                monitor.project
            )));
        }
        // A retry of a kickoff already claimed by the worker must reuse the
        // existing message atomically, even though the worker is now busy.
        // The duplicate-only store branch never mints a new revision.
        if matches!(task.state.as_str(), "dispatched" | "running") {
            if let Some((task, message, duplicate, behind_dead)) =
                self.store.duplicate_task_dispatch(task_id)?
            {
                self.store.resolve_monitor_dispatch_blocked(
                    monitor_id,
                    task_id,
                    epoch_secs(),
                    "monitor_dispatch",
                )?;
                // The durable job-dispatch row is already the idempotency
                // evidence for an automatic retry. Manual RPC callers keep
                // their historical event for every explicit invocation;
                // the watcher must not emit one event per interval.
                if !automatic {
                    let _ = self.store.event_public(
                        store::Store::DAEMON_STREAM,
                        "monitor_dispatch",
                        json!({"monitor": monitor_id, "task": task_id,
                               "message": message, "duplicate": duplicate,
                               "queued_behind_dead": behind_dead,
                               "automatic": false}),
                    );
                }
                self.wake();
                return Ok(json!({
                    "monitor": monitor_id,
                    "task": task.to_json(),
                    "message": message,
                    "duplicate": duplicate,
                    "queued_behind_dead": behind_dead,
                }));
            }
            return Err(Error::rejected(format!(
                "Task '{task_id}' has no live kickoff — only draft or revising tasks are eligible"
            )));
        }
        if !matches!(task.state.as_str(), "draft" | "revising") {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is '{}' — only draft or revising tasks are eligible",
                task.state
            )));
        }
        if task
            .acceptance
            .as_deref()
            .is_none_or(|s| s.trim().is_empty())
        {
            return Err(Error::rejected(format!(
                "Task '{task_id}' has no acceptance criteria — dispatch is refused"
            )));
        }
        let assignee = task
            .assignee
            .as_deref()
            .ok_or_else(|| Error::rejected(format!("Task '{task_id}' has no explicit assignee")))?;
        let agent = self.store.agent(assignee)?;
        if !registry::has_actor(&agent.provider, &agent.endpoint_kind) {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' is a mailbox, not a dispatchable worker"
            )));
        }
        // The fake provider is an in-process fixture and deliberately has no
        // transport endpoint. Every real actor publishes one when open.
        let live_endpoint =
            agent.endpoint.is_some() || (agent.provider == "fake" && agent.endpoint_kind == "fake");
        if !agent.enabled || !live_endpoint || agent.state != "idle" {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' is not demonstrably idle and live (state {}, endpoint {})",
                agent.state, live_endpoint
            )));
        }
        if registry::ready_gate(&agent.provider, &agent.endpoint_kind)
            && agent
                .params
                .as_ref()
                .and_then(|p| p.get("auto_ready"))
                .and_then(Value::as_str)
                != Some("verified")
        {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' requires an explicit readiness claim; automatic dispatch is refused"
            )));
        }
        if self
            .pending
            .lock()
            .unwrap()
            .values()
            .any(|request| request.alias == assignee)
        {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' is waiting on an approval request"
            )));
        }
        if self.store.queued_count(assignee)? > 0 {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' has queued work; dispatch is refused"
            )));
        }
        let unfinished = self.store.tasks_for_assignee(assignee)?;
        if unfinished.iter().any(|other| other.id != task_id) {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' already has unfinished task work"
            )));
        }
        let (task, message, duplicate, behind_dead) =
            self.store
                .dispatch_task(task_id, None, None, &format!("monitor:{monitor_id}"))?;
        self.store.resolve_monitor_dispatch_blocked(
            monitor_id,
            task_id,
            epoch_secs(),
            "monitor_dispatch",
        )?;
        let _ = self.store.event_public(
            store::Store::DAEMON_STREAM,
            "monitor_dispatch",
            json!({"monitor": monitor_id, "task": task_id,
                   "message": message, "duplicate": duplicate,
                   "queued_behind_dead": behind_dead,
                   "automatic": automatic}),
        );
        self.notify_agent(assignee);
        self.wake();
        Ok(json!({
            "monitor": monitor_id,
            "task": task.to_json(),
            "message": message,
            "duplicate": duplicate,
            "queued_behind_dead": behind_dead,
        }))
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
        // CAD-201: the pane root identity is read while the agent row
        // still names its live generation — the actor's exit clears it.
        let pane_tree =
            (agent.endpoint_kind == "pty").then(|| self.owned_pane_root(&alias, &agent));
        if let Some(ctl) = ctl {
            self.stop_ctls(&[ctl]);
        }
        // A fenced pty agent's pane survived the fence for inspection —
        // `agent stop` is the explicit kill. For a live agent the
        // actor's own close() already ran, so this is a no-op for it.
        if agent.endpoint_kind == "pty" {
            adapter::pty::kill_pane(&self.state_dir, &alias, &self.provider_env);
        }
        match pane_tree {
            Some(Ok(Some(root))) => {
                // The drain is bounded but long (60s by default) — it
                // runs on its own thread, never in this RPC or an actor.
                let shared = Arc::clone(self);
                let owned = alias.clone();
                thread::spawn(move || shared.reap_pane_tree(&owned, &root));
            }
            Some(Err(reason)) => {
                let _ = self.store.event_public(
                    &alias,
                    "pane_tree_unowned",
                    json!({"reason": reason,
                           "note": "no process was signalled beyond the pane itself"}),
                );
            }
            // Not pty, or this tree was already reaped.
            Some(Ok(None)) | None => {}
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

    /// CAD-201: the pane-root identity `agent stop` may reap by —
    /// the newest `pane_root` record, provided it belongs to the
    /// endpoint generation the agent row still names (when it names
    /// one). `Ok(None)`: the newest record is already a reap result,
    /// so a repeated stop signals nothing. `Err`: the tree is unowned
    /// — nothing recorded (an agent opened before CAD-201), an
    /// unreadable root, or a stale generation — and nothing is
    /// signalled.
    fn owned_pane_root(
        &self,
        alias: &str,
        agent: &Agent,
    ) -> std::result::Result<Option<adapter::pty::lane::PaneRoot>, String> {
        let latest = self
            .store
            .last_event_of(alias, PANE_TREE_KINDS)
            .map_err(|e| format!("pane root record unreadable: {e}"))?;
        let Some(event) = latest else {
            return Err("no pane root identity was recorded for this agent \
                        (its pane was opened before CAD-201)"
                .to_string());
        };
        match event.kind.as_str() {
            "pane_root" => {}
            "pane_root_unrecorded" => {
                return Err("the pane root's identity was unreadable at open".to_string())
            }
            _ => return Ok(None),
        }
        let root = adapter::pty::lane::PaneRoot::from_json(&event.payload)
            .ok_or_else(|| "the recorded pane root identity is malformed".to_string())?;
        if let Some(current) = agent.generation.as_deref() {
            if current != root.generation {
                return Err(format!(
                    "the recorded pane root belongs to generation {}, the endpoint \
                     is at {current}",
                    root.generation
                ));
            }
        }
        Ok(Some(root))
    }

    /// This daemon's pty retry base: its provider env (a test) or the
    /// environment's `CADENCE_PTY_RETRY_SECS`, else the default. An
    /// invalid value warns on stderr and keeps the default.
    fn pty_retry_base(&self) -> Duration {
        let raw = self.provider_env.var("CADENCE_PTY_RETRY_SECS");
        parse_pty_retry_base(raw.as_deref()).unwrap_or_else(|reason| {
            eprintln!("pty retry: {reason}; using the default {PTY_RETRY_BASE:?}");
            PTY_RETRY_BASE
        })
    }

    /// CAD-201: reap what is left of a stopped pane's session — off
    /// the actor loop and the RPC thread. Intent, result and residue
    /// land as `pane_tree_reap_intent` / `pane_tree_reaped` (or
    /// `pane_tree_reap_refused`) events on the agent's stream. The
    /// action-time check re-reads the newest pane record: a reopen
    /// whose root sits on the recorded session id stops the reap.
    fn reap_pane_tree(&self, alias: &str, root: &adapter::pty::lane::PaneRoot) {
        use adapter::pty::lane;
        let drain = self
            .provider_env
            .var("CADENCE_PTY_DRAIN_SECS")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|s| s.is_finite() && *s >= 0.0)
            .map(Duration::from_secs_f64)
            .unwrap_or(lane::DEFAULT_DRAIN);
        let opts = lane::ReapOptions {
            drain,
            ..lane::ReapOptions::default()
        };
        let still_ours = || -> std::result::Result<(), String> {
            let latest = self
                .store
                .last_event_of(alias, &["pane_root"])
                .map_err(|e| format!("pane root record unreadable at action time: {e}"))?;
            match latest.and_then(|e| lane::PaneRoot::from_json(&e.payload)) {
                Some(newer) if newer != *root && newer.sid == root.sid => Err(format!(
                    "a newer endpoint (generation {}) recorded a pane root on the \
                     same session id {}",
                    newer.generation, root.sid
                )),
                _ => Ok(()),
            }
        };
        let on_intent = |members: &[lane::Member]| {
            let members: Vec<Value> = members
                .iter()
                .map(|m| json!({"pid": m.pid, "start_time": m.start_time}))
                .collect();
            let _ = self.store.event_public(
                alias,
                "pane_tree_reap_intent",
                json!({"root": root.to_json(), "members": members,
                       "drain_secs": opts.drain.as_secs_f64()}),
            );
        };
        let report = lane::reap_session(root, &opts, &still_ours, &on_intent);
        let kind = if report.refused.is_some() {
            "pane_tree_reap_refused"
        } else {
            "pane_tree_reaped"
        };
        let mut payload = report.to_json();
        payload["root"] = root.to_json();
        let _ = self.store.event_public(alias, kind, payload);
    }

    /// CAD-201/CAD-202 facts for `agent show`: the recorded pane root
    /// (and whether it is the live generation's), and — while the
    /// endpoint is live — the pane's cwd with `cwd_deleted`. The cwd
    /// is read only when the row's pid is still the recorded root
    /// process (same start time), never from a reused pid.
    fn pty_lane_facts(&self, agent: &Agent, j: &mut Value) {
        use adapter::pty::lane;
        let root = self
            .store
            .last_event_of(&agent.alias, &["pane_root"])
            .ok()
            .flatten()
            .and_then(|e| lane::PaneRoot::from_json(&e.payload));
        j["pane_root"] = match &root {
            Some(r) => {
                let mut v = r.to_json();
                v["current"] = json!(agent.generation.as_deref() == Some(r.generation.as_str()));
                v
            }
            None => Value::Null,
        };
        let live_pid = agent
            .pid
            .filter(|_| agent.endpoint.is_some())
            .and_then(|p| u32::try_from(p).ok());
        let cwd = live_pid.and_then(|pid| {
            let proven = match &root {
                Some(r) if r.pid == pid => r.check() == lane::RootState::Same,
                // No record for this pid (older open): the row's pid is
                // the live pane the actor verified — read-only use.
                _ => true,
            };
            proven.then(|| lane::pane_cwd(pid)).flatten()
        });
        j["cwd_deleted"] = json!(cwd.as_ref().is_some_and(|c| c.deleted));
        j["pane_cwd"] = cwd.map(|c| c.to_json()).unwrap_or(Value::Null);
    }

    /// Interrupt every actor, wait one bounded grace, force-close the
    /// stragglers, then join all threads. A forced close makes any
    /// outstanding attempt `OutcomeUnknown` — fenced, never replayed.
    fn stop_ctls(&self, ctls: &[Arc<AgentCtl>]) {
        for ctl in ctls {
            if let Some(adapter) = ctl.adapter.lock().unwrap().clone() {
                adapter.release_for_stop();
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
                    // is meant to re-adopt. A held Devin cloud turn
                    // detaches too, so its session is not archived.
                    // `detach` defaults to `close` for adapters that
                    // own their provider process, so managed endpoints
                    // are still reaped.
                    if self.closing.load(Ordering::SeqCst) || ctl.cloud_held.load(Ordering::SeqCst)
                    {
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

    // ---- Unconsumed inboxes: warn, never refuse or drop (CAD-251) ----

    /// The mailbox's health block ([`crate::inbox::health`]) — `None`
    /// for an agent with an actor (it consumes its own queue) or when
    /// the store read fails.
    fn inbox_health(&self, agent: &Agent) -> Option<Value> {
        if registry::has_actor(&agent.provider, &agent.endpoint_kind) {
            return None;
        }
        let consumer = self.store.inbox_consumer(&agent.alias).ok()?;
        let policy = crate::inbox::Policy::from_params(agent.params.as_ref());
        let owner = crate::inbox::owner_of(&agent.alias, |a| self.upstream_of(a));
        Some(crate::inbox::health(
            &agent.alias,
            &consumer,
            policy,
            &owner,
            epoch_secs(),
        ))
    }

    /// The sender-facing warning for a delivery into `alias`, when it
    /// is a stale mailbox.
    fn inbox_warning(&self, alias: &str) -> Option<String> {
        let agent = self.store.agent(alias).ok()?;
        let health = self.inbox_health(&agent)?;
        health["warning"].as_str().map(str::to_string)
    }

    /// Routed deliveries (reply_to results, upstream notices) have no
    /// CLI caller to warn, so a stale mailbox that received anything
    /// since its last warning gets one `inbox_unconsumed` event — at
    /// most once per idle window, never per message.
    fn inbox_sweep(&self) {
        let Ok(agents) = self.store.agents() else {
            return;
        };
        let now = epoch_secs();
        for agent in &agents {
            let Some(health) = self.inbox_health(agent) else {
                continue;
            };
            if health["stale"] != json!(true) {
                continue;
            }
            let received = health["last_received_at"].as_f64().unwrap_or(0.0);
            let window = health["threshold"]["idle_secs"].as_f64().unwrap_or(0.0);
            let warned = self
                .store
                .last_event_at(&agent.alias, "inbox_unconsumed")
                .ok()
                .flatten();
            if let Some(at) = warned {
                if received <= at || now - at < window {
                    continue;
                }
            }
            let _ = self
                .store
                .event_public(&agent.alias, "inbox_unconsumed", health);
        }
    }

    // ---- Stall watch: report silent turns, never touch them (CAD-52) ----

    /// Sample owned agents on a slow cadence until shutdown. The watch
    /// only ever emits events and notices — it never interrupts,
    /// re-dispatches or fences anything it observes.
    fn run_stall_watch(self: &Arc<Self>) {
        let mut inbox_swept: Option<Instant> = None;
        let mut nudges_swept: Option<Instant> = None;
        while !self.closing.load(Ordering::SeqCst) {
            self.stall_tick();
            // CAD-250 N3: a nudge still queued past its TTL is stale
            // steering — cancelled, never pasted late.
            if nudges_swept.is_none_or(|at| at.elapsed() >= Duration::from_secs(10)) {
                let _ = self
                    .store
                    .expire_queued_nudges(epoch_secs(), NUDGE_TTL_SECS as f64);
                nudges_swept = Some(Instant::now());
            }
            // CAD-251: the unconsumed-inbox sweep rides the screen-sample
            // cadence (one minute by default) — a store read per mailbox.
            if inbox_swept.is_none_or(|at| at.elapsed() >= screen_sample(&self.stall_sample_secs)) {
                self.inbox_sweep();
                inbox_swept = Some(Instant::now());
            }
            // CAD-199: off unless configured; sweeps at most hourly.
            self.agent_gc_tick();
            // CAD-96: idle auto-stop — checks at most once a minute.
            self.auto_stop_tick();
            std::thread::sleep(STALL_TICK);
        }
    }

    /// Reconcile daemon-owned monitor registrations without waking a
    /// provider. Each check advances its durable cursor together with any
    /// deduplicated local alerts; failures remain visible as `degraded`.
    /// Opted-in registrations then run the same guarded dispatch helper as
    /// the explicit RPC. A guard refusal becomes one durable task alert and
    /// is retried only on the next monitor interval.
    fn run_monitor_watch(self: &Arc<Self>) {
        while !self.closing.load(Ordering::SeqCst) {
            let at = epoch_secs();
            if let Ok(monitors) = self.store.due_monitors(at) {
                for monitor in monitors {
                    let result = self.store.check_monitor(&monitor.id, at);
                    if let Err(error) = result {
                        let _ = self
                            .store
                            .fail_monitor_check(&monitor.id, at, &error.to_string());
                    } else if monitor.auto_dispatch_enabled {
                        self.reconcile_monitor_dispatch(&monitor.id, at);
                    }
                    self.wake();
                }
            }
            let deadline = Instant::now() + MONITOR_TICK;
            while !self.closing.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    fn reconcile_monitor_dispatch(self: &Arc<Self>, monitor_id: &str, at: f64) {
        let task_ids = match self.store.monitor_tasks(monitor_id) {
            Ok(tasks) => tasks,
            Err(error) => {
                let _ = self.store.fail_monitor_check(
                    monitor_id,
                    at,
                    &format!("automatic coverage reconciliation failed: {error}"),
                );
                return;
            }
        };
        for task_id in task_ids {
            let task = match self.store.task_opt(&task_id) {
                Ok(Some(task)) => task,
                Ok(None) => {
                    let _ = self.store.record_monitor_dispatch_blocked(
                        monitor_id,
                        &task_id,
                        at,
                        "covered task no longer exists",
                    );
                    continue;
                }
                Err(error) => {
                    let _ = self.store.record_monitor_dispatch_blocked(
                        monitor_id,
                        &task_id,
                        at,
                        &format!("covered task lookup failed: {error}"),
                    );
                    continue;
                }
            };
            if !matches!(
                task.state.as_str(),
                "draft" | "revising" | "dispatched" | "running"
            ) {
                // Review, verified, blocked, cancelled, and done work has no
                // eligible automatic action. Its durable task state remains
                // the source of truth; do not manufacture an alert.
                continue;
            }
            if let Err(error) = self.monitor_dispatch_task(monitor_id, &task_id, true) {
                let _ = self.store.record_monitor_dispatch_blocked(
                    monitor_id,
                    &task_id,
                    at,
                    &error.to_string(),
                );
            }
        }
    }

    /// CAD-132: a provider WAL once grew 0 → 30 GiB in four hours and
    /// twice took the disk under 4 GiB. The daemon now checkpoints
    /// known provider stores itself — PASSIVE then TRUNCATE — whenever
    /// the WAL passes `[host] wal_max_bytes`, has been quiet for
    /// `WAL_QUIET_SECS`, and the owning provider has no in-flight
    /// cadence turn. A deferred checkpoint is retried next tick. The
    /// sleep is sub-stepped so `closing` lands within ~200ms, not
    /// after a whole tick.
    fn run_wal_watch(self: &Arc<Self>) {
        let mut watch = WalWatch::default();
        while !self.closing.load(Ordering::SeqCst) {
            self.wal_tick(&mut watch);
            let deadline = Instant::now() + WAL_TICK;
            while !self.closing.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }

    /// One pass: thresholds re-read each tick so a `pm.yaml` edit
    /// applies without a restart; the busy-provider set comes from
    /// the live store. `[host] wal_checkpoint: false` opts the whole
    /// watcher out; `wal_dry_run: true` — or a sandbox profile, since
    /// provider stores are the host's — records intent, never writes.
    fn wal_tick(&self, watch: &mut WalWatch) {
        let Some(home) = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|h| !h.as_os_str().is_empty())
        else {
            // HOME unset → provider roots would resolve against the
            // daemon's cwd; there is nothing to watch.
            return;
        };
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        let roots = crate::doctor::host::wal_roots(&home, &data_home);
        let pm_dir = crate::issue::default_dir()
            .ok()
            .filter(|d| d.join("pm.yaml").is_file());
        let t = crate::doctor::host::host_thresholds(pm_dir.as_deref());
        if !t.wal_checkpoint {
            return;
        }
        // CAD-250: only live turns defer — an alias with an actor, and
        // a delivered pty turn still inside its report bound.
        let live: HashSet<String> = self
            .lifecycle
            .lock()
            .unwrap()
            .agents
            .keys()
            .cloned()
            .collect();
        let Ok(busy) = self.store.busy_providers(&live, epoch_secs()) else {
            return;
        };
        wal_pass(
            &roots,
            &busy,
            t.wal_max_bytes,
            WAL_QUIET_SECS,
            wal_observe_only(t.wal_dry_run, crate::sandbox::profile().as_deref()),
            &self.store,
            watch,
        );
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
        // The tracked head: the running turn when one is in flight,
        // else the oldest still-waiting message — a pane menu that
        // blocks its delivery must surface before any turn starts.
        // And with nothing tracked at all a pty pane STILL samples:
        // an approval raised on an idle pane is a needs-me signal the
        // views and the `approval_menu` event must carry — menu
        // detection only; stall/silent-end bookkeeping and message
        // attribution need a started turn.
        let running = match self.store.running_message(alias) {
            Ok(m) => m,
            Err(_) => return,
        };
        let tracked: Option<Message> = match running {
            Some(ref m) => Some(m.clone()),
            None => match self.store.queued_head(alias) {
                Ok(m) => m,
                Err(_) => return,
            },
        };
        let Ok(agent) = self.store.agent(alias) else {
            return;
        };
        if tracked.is_none() && agent.endpoint_kind != "pty" {
            // Nothing to watch on a non-pty endpoint — clear any
            // stale watch so a later message starts clean.
            let mut w = ctl.stall.lock().unwrap();
            if w.message.is_some() {
                *w = StallWatch::default();
            }
            return;
        }
        // Store reads stay outside the stall lock — `stall_budget`
        // takes the conn mutex and no other path holds it in reverse.
        let budget = running
            .as_ref()
            .map(|m| self.stall_budget(&agent, m))
            .unwrap_or(0);
        let ad = ctl.adapter.lock().unwrap().clone();
        let mut w = ctl.stall.lock().unwrap();
        if w.message.as_deref() != tracked.as_ref().map(|m| m.id.as_str()) {
            // The tracked subject changed (or drained) — the watch
            // resets, but the fired-menu history is pane state, not
            // message state: keeping it stops a menu that survives a
            // message transition from re-firing its event.
            let menu_evented = std::mem::take(&mut w.menu_evented);
            *w = StallWatch {
                message: tracked.as_ref().map(|m| m.id.clone()),
                menu_evented,
                ..StallWatch::default()
            };
        }
        // An open brokered request means the provider is silent by
        // design — a human is thinking. That wait is activity.
        let pending_req = running.is_some()
            && self
                .pending
                .lock()
                .unwrap()
                .values()
                .any(|req| req.alias == alias);
        if pending_req {
            w.activity = Instant::now();
        }
        // The adapter's own clock when it keeps one — managed
        // transcripts stamp every provider notification.
        if running.is_some() {
            if let Some(at) = ad.as_ref().and_then(|a| a.activity_at()) {
                if at > w.activity {
                    w.activity = at;
                }
            }
        }
        // PTY screens have no transport clock: captures run on their
        // own threads, one in flight per agent at most, so a slow or
        // wedged pane can never block the ticker (or any view that
        // touches this lock). A finished sample lands here carrying
        // the probe verdict beside the hash — a menu frame is a human
        // wait (never activity), an idle frame builds the silent-end
        // streak.
        let mut menu_rise: Option<String> = None;
        let mut end_fire: Option<(u64, f64, Probe)> = None;
        if agent.endpoint_kind == "pty" {
            let mut landed = None;
            match w.sample_rx.as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(sample)) => {
                    landed = Some(sample);
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
            if let Some((hash, probe)) = landed {
                // The verdict first: an open menu clears the idle
                // streak and fires `approval_menu` on the rising edge
                // only — and only once per distinct menu line, so a
                // detection flicker cannot re-fire the same wait; an
                // idle frame extends the streak; anything else (busy,
                // draft) resets both clocks.
                if probe.approval_menu {
                    w.idle_samples = 0;
                    w.idle_since = None;
                    if w.menu_line.is_none() && !w.menu_evented.iter().any(|l| l == &probe.reason) {
                        menu_rise = Some(probe.reason.clone());
                        w.menu_evented.push_back(probe.reason.clone());
                        // Bounded: the pane only ever renders a small
                        // vocabulary of menu lines — 8 is far past
                        // any alternating-subject cycle.
                        while w.menu_evented.len() > 8 {
                            w.menu_evented.pop_front();
                        }
                    }
                    w.menu_line = Some(probe.reason.clone());
                } else {
                    // The menu closed — a re-request of the same
                    // subject is a NEW wait and must re-fire, so the
                    // fired-subject history resets with the menu. A
                    // one-sample flicker between menu frames re-fires
                    // a duplicate — noise is recoverable, a silently
                    // missed approval is not.
                    if w.menu_line.is_some() {
                        w.menu_evented.clear();
                    }
                    w.menu_line = None;
                    if probe.idle {
                        w.idle_samples = w.idle_samples.saturating_add(1);
                        if w.idle_since.is_none() {
                            w.idle_since = Some(Instant::now());
                        }
                    } else {
                        w.idle_samples = 0;
                        w.idle_since = None;
                    }
                }
                if running.is_none() {
                    // Queued-head tracking is menu detection only —
                    // the screen-hash churn below measures a turn's
                    // activity and means nothing before it starts.
                } else if w.settled.as_deref() == Some(hash.as_str()) {
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
                        // settle only forms the baseline. A menu frame
                        // never counts as turn activity: the wait is a
                        // human's, and the silence clock must see it.
                        if w.settled.is_some() && !probe.approval_menu {
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
                w.last_probe = Some(probe);
                w.sample_at = Some(Instant::now());
            }
            if w.sample_rx.is_none()
                && w.sample_at
                    .is_none_or(|at| at.elapsed() >= screen_sample(&self.stall_sample_secs))
            {
                if let Some(ad) = ad {
                    let (tx, rx) = std::sync::mpsc::channel();
                    thread::spawn(move || {
                        if let Ok(sample) = ad.sample_screen() {
                            let _ = tx.send(sample);
                        }
                    });
                    w.sample_rx = Some(rx);
                }
            }
            // The silent end: the durable message still runs but the
            // pane probes idle — verified for `silent_end_secs` over
            // at least three consecutive samples, never one capture,
            // never while a menu or a brokered request explains the
            // wait. The event fires once per message and flags it —
            // the message itself is never auto-resolved.
            let end_budget = if running.is_some() {
                self.silent_end_budget(&agent)
            } else {
                0
            };
            if !w.silent_end_sent
                && end_budget > 0
                && !pending_req
                && w.menu_line.is_none()
                && w.idle_samples >= 3
                && w.idle_since
                    .is_some_and(|t| t.elapsed() >= Duration::from_secs(end_budget))
            {
                w.silent_end_sent = true;
                if let Some(p) = w.last_probe.clone() {
                    let age = w.idle_since.map(|t| t.elapsed().as_secs()).unwrap_or(0);
                    let last_activity = epoch_secs() - w.activity.elapsed().as_secs_f64();
                    end_fire = Some((age, last_activity, p));
                }
            }
        }
        let silent = w.activity.elapsed();
        // Everything the lock decided, applied after it's dropped —
        // event writes take the store mutex and never run under `w`.
        enum After {
            Resume(Instant, u64),
            Stall(u64, Duration),
            Emit,
        }
        let after = if running.is_none() {
            After::Emit
        } else if let Some(stalled_at) = w.stalled_at {
            if w.activity > stalled_at {
                let episode = w.episodes;
                w.stalled_at = None;
                After::Resume(stalled_at, episode)
            } else {
                After::Emit
            }
        } else if budget > 0 && silent >= Duration::from_secs(budget) {
            w.episodes += 1;
            w.stalled_at = Some(w.activity);
            After::Stall(w.episodes, silent)
        } else {
            After::Emit
        };
        drop(w);
        if let Some(line) = menu_rise {
            self.approval_menu_fired(&agent, tracked.as_ref(), &line, running.is_none());
        }
        if let Some((age, last_activity, probe)) = end_fire {
            if let Some(m) = running.as_ref() {
                self.silent_end_fired(&agent, m, age, last_activity, &probe);
            }
        }
        match after {
            After::Resume(at, episode) => {
                self.stall_resumed(&agent, running.as_ref().unwrap(), at.elapsed(), episode)
            }
            After::Stall(episode, silent) => {
                self.stall_fired(&agent, running.as_ref().unwrap(), silent, episode)
            }
            After::Emit => {}
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

    /// The idle-pane budget for `turn_silent_end`: the agent's
    /// `silent_end_secs` param, else the default. `0` disables — a
    /// still-running message on an idle pane is then only ever a
    /// stall observation, never a silent-end verdict.
    fn silent_end_budget(&self, agent: &Agent) -> u64 {
        agent
            .params
            .as_ref()
            .and_then(|p| p.get("silent_end_secs"))
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(DEFAULT_SILENT_END_SECS)
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

    /// `approval_menu`: the sampled pane just showed an approval menu
    /// — recorded once per distinct menu line, carrying the line so
    /// the row names what's being asked. `tracked` is the message it
    /// blocks: `queued` marks it still-waiting rather than mid-turn,
    /// and `None` means the pane is idle — the event fires as
    /// `idle: true` and never attributes a message that does not
    /// exist.
    fn approval_menu_fired(
        &self,
        agent: &Agent,
        tracked: Option<&Message>,
        line: &str,
        queued: bool,
    ) {
        let mut payload = json!({"line": line});
        let (mut job_id, mut task_id) = (None, None);
        if let Some(m) = tracked {
            payload["message"] = json!(m.id);
            let (j, t) = self.message_scope(m);
            job_id = j;
            task_id = t;
            if queued {
                payload["queued"] = json!(true);
            }
            if let Some(t) = task_id {
                payload["task"] = json!(t);
            }
        } else {
            payload["idle"] = json!(true);
        }
        let _ = self.store.event_public_scoped(
            &agent.alias,
            "approval_menu",
            payload,
            job_id.as_deref(),
            task_id,
        );
        self.wake();
    }

    /// `turn_silent_end`: the durable message still runs but the pane
    /// has probed idle for `silent_end_secs` — the provider ended
    /// without reporting. Records the age and the admitting probe as
    /// evidence; the message is flagged, never auto-resolved.
    fn silent_end_fired(
        &self,
        agent: &Agent,
        message: &Message,
        age_secs: u64,
        last_activity: f64,
        probe: &Probe,
    ) {
        let (job_id, task_id) = self.message_scope(message);
        let mut payload = json!({
            "message": message.id,
            "age_secs": age_secs,
            "last_activity": last_activity,
            "probe": probe.to_json(),
        });
        if let Some(t) = task_id {
            payload["task"] = json!(t);
        }
        let _ = self.store.event_public_scoped(
            &agent.alias,
            "turn_silent_end",
            payload,
            job_id.as_deref(),
            task_id,
        );
        self.wake();
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

    /// The live stall-watch facts a view renders while a message runs:
    /// silence age, the open `turn_stalled` episode, and the sampled
    /// pane verdict — the menu line while one's open, the idle
    /// streak's age, and the once-fired silent-end flag. With no
    /// in-flight turn the view only carries a menu line the queued
    /// head is blocked behind; `None` when nothing is tracked at all.
    fn stall_view(&self, alias: &str) -> Option<StallView> {
        let running = self.store.running_message(alias).ok()?;
        let ctl = self.lifecycle.lock().unwrap().agents.get(alias)?.clone();
        let w = ctl.stall.lock().unwrap();
        let Some(running) = running else {
            // A queued head behind an open menu: only the menu line
            // is meaningful — nothing has started or ended.
            return w.menu_line.clone().map(|line| StallView {
                silent_secs: 0,
                stalled: false,
                menu: Some(line),
                ended_secs: None,
                silent_ended: false,
            });
        };
        if w.message.as_deref() == Some(running.id.as_str()) {
            return Some(StallView {
                silent_secs: w.activity.elapsed().as_secs(),
                stalled: w.stalled_at.is_some(),
                menu: w.menu_line.clone(),
                ended_secs: w.idle_since.map(|t| t.elapsed().as_secs()),
                silent_ended: w.silent_end_sent,
            });
        }
        // The watch hasn't ticked over this message yet — report
        // silence from its recorded start.
        let silent = running
            .started
            .map(|s| (epoch_secs() - s).max(0.0) as u64)
            .unwrap_or(0);
        Some(StallView {
            silent_secs: silent,
            stalled: false,
            menu: None,
            ended_secs: None,
            silent_ended: false,
        })
    }

    fn notify_agent(&self, alias: &str) {
        if let Some(ctl) = self.lifecycle.lock().unwrap().agents.get(alias) {
            ctl.wake.notify_all();
        }
    }

    /// Wake `reply_to` after a route of `worker_result` or
    /// `worker_notice`. `wake()` still releases socket long-polls;
    /// this is the actor parked in the empty-queue wait. An
    /// `inbox_read` receipt does not route. A missing `reply_to`, or
    /// an alias with no actor, does nothing.
    fn notify_routed_target(&self, message: &Message, result: &Value) {
        if result.get("via").and_then(Value::as_str) == Some("inbox_read") {
            return;
        }
        if let Some(reply_to) = message.reply_to.as_deref() {
            self.notify_agent(reply_to);
        }
    }

    /// Provider-side proof of life for the stall watch — any adapter
    /// event or request channel activity refreshes the agent's clock.
    fn bump_activity(&self, alias: &str) {
        if let Some(ctl) = self.lifecycle.lock().unwrap().agents.get(alias) {
            ctl.bump_activity();
        }
    }

    /// Request shutdown. Capture PTY adoption facts first, then set
    /// `closing` and wake actors. Both the shutdown RPC and the signal
    /// handler come through here: an idle actor parked in `wait_until`
    /// returns as soon as it is woken and `set_state_detached` clears
    /// `pid`/`generation` before `serve` reaches [`Shared::shutdown`].
    /// Snapshotting inside `shutdown` loses that race and
    /// `shutdown_entries` then omits the alias.
    fn begin_closing(&self) {
        {
            let mut slot = self.shutdown_facts.lock().unwrap();
            if slot.is_none() {
                *slot = Some(self.store.pty_endpoint_facts().unwrap_or_default());
            }
        }
        self.closing.store(true, Ordering::SeqCst);
        self.wake();
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
        // Facts come from `begin_closing`, taken before the wake that
        // lets an idle actor detach. Re-reading the agent rows here is
        // the former snapshot and is empty once that detach has run.
        // The marker's message rows are still read last, after the drain.
        let captured = self.shutdown_facts.lock().unwrap().clone();
        let facts = match captured {
            Some(facts) => facts,
            None => self.store.pty_endpoint_facts().unwrap_or_default(),
        };
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

// ---------- provider WAL auto-checkpoint (CAD-132) ----------

/// What a TRUNCATE attempt learned. Contention under
/// `busy_timeout(0)` arrives as an error, and a non-WAL database
/// reports `(0, -1, -1)` — both mean "leave it for the next tick",
/// so they share one variant.
enum Checkpoint {
    /// The WAL checkpointed and truncated — record the event.
    Done,
    /// Busy, locked, not a WAL store, or open failed — all deferred.
    Deferred,
}

/// `PRAGMA wal_checkpoint(PASSIVE)` then `(TRUNCATE)` on `db` — a
/// second connection to the provider's own store. READ_WRITE without
/// CREATE: `find_wals` derives the db path from a `-wal` stem, and an
/// orphan `foo.db-wal` must never *create* `foo.db` in a provider's
/// directory. No busy timeout — a contended file defers instead of
/// stalling the watch. PASSIVE moves frames out while readers run;
/// TRUNCATE then frees the file unless someone is mid-snapshot.
fn checkpoint_wal(db: &Path) -> Checkpoint {
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return Checkpoint::Deferred;
    };
    let _ = conn.busy_timeout(Duration::ZERO);
    // PASSIVE's own result is advisory — TRUNCATE does the real work.
    let _ = conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(()));
    match conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
    }) {
        // (busy=0, log>=0): a real WAL store checkpointed. (0,-1,-1)
        // is a journal_mode=DELETE db answering the pragma — a stale
        // leftover -wal, not something we shrank: no event is owed.
        Ok((0, log)) if log >= 0 => Checkpoint::Done,
        Ok(_) | Err(_) => Checkpoint::Deferred,
    }
}

/// Seconds a WAL must go unwritten before the watcher calls the
/// store idle — the gate against writers cadence cannot see
/// (interactive terminals, another daemon, provider background jobs).
const WAL_QUIET_SECS: u64 = 60;

/// The watcher's dry-run switch: `[host] wal_dry_run`, forced on under
/// a sandbox profile — a sandbox never checkpoints the host's provider
/// stores (CAD-310).
fn wal_observe_only(configured: bool, sandbox: Option<&str>) -> bool {
    configured || sandbox.is_some()
}

/// Keep this many `daemon` events — the stream has no agents row, so
/// agent-removal pruning never reaches it; unbounded growth in a
/// feature whose purpose is bounding growth would be embarrassing.
const DAEMON_EVENTS_KEEP: i64 = 200;

/// Cross-tick watch state: `pending` dedupes dry-run events (one per
/// db per crossing, cleared when it drops under the limit), and
/// `truncated_seen` keeps a stuck scan from re-eventing every tick.
#[derive(Default)]
struct WalWatch {
    pending: HashSet<PathBuf>,
    truncated_seen: bool,
}

/// One watch pass over `roots`: each `*-wal` over `max_bytes`, quiet
/// for `quiet_secs`, owned by this uid and not a symlink, whose
/// provider has no in-flight *cadence* turn gets checkpointed — the
/// busy gate is a courtesy over cadence's own store; SQLite's locking
/// and the quiet-window are what protect data. `dry_run` records
/// `wal_checkpoint_pending` instead of touching the db.
fn wal_pass(
    roots: &[crate::doctor::host::WalRoot],
    busy: &HashSet<String>,
    max_bytes: u64,
    quiet_secs: u64,
    dry_run: bool,
    store: &Store,
    watch: &mut WalWatch,
) {
    let uid = unsafe { libc::geteuid() };
    let mut truncated = false;
    for root in roots {
        if busy.contains(root.provider) {
            continue;
        }
        let scan = crate::doctor::host::find_wals(&root.root);
        truncated |= scan.truncated;
        for db in scan.dbs {
            let Some(wal) = crate::doctor::host::wal_sibling(&db) else {
                continue;
            };
            // symlink_metadata + uid: a root-run daemon checkpointing
            // a user's db would leave root-owned -wal/-shm the provider
            // then cannot open; a symlinked store is never ours to
            // write through.
            let Ok(wal_meta) = std::fs::symlink_metadata(&wal) else {
                watch.pending.remove(&db);
                continue;
            };
            let Ok(db_meta) = std::fs::symlink_metadata(&db) else {
                // Orphan -wal with no db beside it — never create one.
                watch.pending.remove(&db);
                continue;
            };
            if wal_meta.is_symlink()
                || db_meta.is_symlink()
                || wal_meta.uid() != uid
                || db_meta.uid() != uid
            {
                continue;
            }
            let before = wal_meta.len();
            if before <= max_bytes {
                watch.pending.remove(&db);
                continue;
            }
            // Recently-written WAL: some writer is live — defer.
            let quiet = wal_meta
                .modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .is_some_and(|ago| ago >= Duration::from_secs(quiet_secs));
            if !quiet {
                continue;
            }
            if dry_run {
                if watch.pending.insert(db.clone()) {
                    let _ = store.event_public(
                        DAEMON_ALIAS,
                        "wal_checkpoint_pending",
                        json!({
                            "provider": root.provider,
                            "store": root.label,
                            "db": db,
                            "wal_bytes_before": before,
                            "dry_run": true,
                        }),
                    );
                }
                continue;
            }
            match checkpoint_wal(&db) {
                Checkpoint::Done => {
                    watch.pending.remove(&db);
                    let after = std::fs::metadata(&wal).ok().map(|m| m.len()).unwrap_or(0);
                    let _ = store.event_public(
                        DAEMON_ALIAS,
                        "wal_checkpointed",
                        json!({
                            "provider": root.provider,
                            "store": root.label,
                            "db": db,
                            "wal_bytes_before": before,
                            "wal_bytes_after": after,
                        }),
                    );
                }
                Checkpoint::Deferred => {}
            }
        }
    }
    if truncated {
        if !watch.truncated_seen {
            let _ = store.event_public(
                DAEMON_ALIAS,
                "wal_scan_truncated",
                json!({"reason": "find_wals hit its cap — a runaway WAL may be unwatched"}),
            );
        }
        watch.truncated_seen = true;
    } else {
        watch.truncated_seen = false;
    }
    let _ = store.prune_stream(DAEMON_ALIAS, DAEMON_EVENTS_KEEP);
}

/// Event kinds that settle a pty lane's tree ownership (CAD-201): the
/// newest of these decides what `agent stop` may reap.
const PANE_TREE_KINDS: &[&str] = &[
    "pane_root",
    "pane_root_unrecorded",
    "pane_tree_reaped",
    "pane_tree_reap_refused",
];

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

/// A present string field. JSON null and omission are both absent; any
/// other type is a rejection rather than a silent skip.
fn optional_text<'a>(params: &'a Value, field: &str) -> Result<Option<&'a str>> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.as_str())),
        Some(_) => Err(Error::invalid(
            "invalid_request",
            format!("'{field}' must be a string"),
        )),
    }
}

fn optional_u64(params: &Value, field: &str) -> Option<u64> {
    params.get(field).and_then(Value::as_u64)
}

fn optional_i64(params: &Value, field: &str) -> Option<i64> {
    params.get(field).and_then(Value::as_i64)
}

/// Who is on the other end of a connection — see
/// [`Shared::caller_identity`] (CAD-381).
enum Caller {
    /// No agent endpoint on the caller's ancestry: it never resolves
    /// to, or borrows, an agent's identity. This is NOT operator proof —
    /// an agent's own process escapes every agent tree by `setsid` +
    /// double fork, `systemd-run` or a new tmux session. Operator
    /// authority needs its own positive proof
    /// ([`Shared::proven_operator`], CAD-276; web operator auth CAD-313).
    NoAgentIdentity,
    /// Exactly one live agent endpoint, proven from the daemon's record.
    Agent(Box<VerifiedAgent>),
}

/// An agent endpoint the daemon verified for this connection.
struct VerifiedAgent {
    agent: Agent,
    /// The endpoint's proof generation: the pty adapter generation, or
    /// a managed endpoint's enrolled owner generation.
    generation: String,
    /// The endpoint process's start time (`/proc` starttime).
    process_start: u64,
}

/// A verified identity needs an endpoint that is up right now.
fn require_live_endpoint(agent: &Agent) -> Result<()> {
    if matches!(
        agent.state.as_str(),
        "idle" | "busy" | "running" | "waiting_input"
    ) {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "agent '{}' is {} — only a live endpoint has a caller identity",
            agent.alias, agent.state
        )))
    }
}

/// Who a slot call runs as — see [`Shared::slot_identity`].
enum SlotWho {
    /// Legacy binding: the nearest registered pty pane (CAD-113).
    Pane { lane: String, chain: Vec<u32> },
    /// Strict binding: a verified caller of a managed endpoint's
    /// enrollment (CAD-230).
    Strict(crate::slots::StrictCaller),
}

impl SlotWho {
    fn lane(&self) -> &str {
        match self {
            SlotWho::Pane { lane, .. } => lane,
            SlotWho::Strict(c) => &c.lane,
        }
    }

    /// Every pid the caller may bind a hold to: the whole ancestry for
    /// a pane caller, the verified peer-to-root segment for a strict one.
    fn chain(&self) -> &[u32] {
        match self {
            SlotWho::Pane { chain, .. } => chain,
            SlotWho::Strict(c) => &c.segment,
        }
    }
}

/// An enrollment's owner generation, read from the owner row: the
/// registration instant, the adapter's endpoint generation and the
/// recorded provider pid. A re-registration, a reopen or a closed
/// endpoint (pid cleared) all change or erase it — `None` means the
/// owner has no live endpoint to enroll.
fn owner_generation(agent: &Agent) -> Option<String> {
    let pid = agent.pid?;
    Some(format!(
        "{:016x}:{}:{pid}",
        agent.created.to_bits(),
        agent.generation.as_deref().unwrap_or("-")
    ))
}

/// Reject peers that are not the same Unix user; return the peer PID
/// used to derive slot and approval-answer caller identity.
#[cfg(target_os = "linux")]
fn check_peer(stream: &UnixStream) -> Result<u32> {
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
    Ok(cred.pid as u32)
}

/// Off Linux there is no `SO_PEERCRED`: refuse every peer (fail closed)
/// until the macOS port (CAD-315) brings a verified equivalent. The
/// daemon does not start there anyway — see `reaper::enable`.
#[cfg(not(target_os = "linux"))]
fn check_peer(_stream: &UnixStream) -> Result<u32> {
    Err(Error::rejected(
        "socket peer credentials are only checked on Linux; the daemon is Linux-only \
         until the macOS port (CAD-315)",
    ))
}

/// Linux process-start identity for an endpoint pid. The daemon stores the
/// registration discriminator separately; this request-time provenance
/// catches a stale or reused pid before issuing a receipt.
fn process_start_identity(pid: u32) -> Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|e| {
        Error::rejected(format!(
            "Cannot read native endpoint process start for pid {pid}: {e}"
        ))
    })?;
    let end = stat
        .rfind(')')
        .ok_or_else(|| Error::rejected(format!("Malformed /proc/{pid}/stat")))?;
    let fields: Vec<&str> = stat[end + 1..].split_whitespace().collect();
    fields
        .get(19)
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| Error::rejected(format!("Missing process start for pid {pid}")))
}

fn handle_conn(shared: Arc<Shared>, stream: UnixStream) {
    let Ok(peer_pid) = check_peer(&stream) else {
        return;
    };
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
                shared.dispatch(method, &params, peer_pid)
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

// ---- Agent-gc timer (CAD-199): opt-in, registry records only ----
//
// `agent gc` removes stopped agents that are otherwise resumable, so the
// timer is OFF unless the operator sets `[host] agent_gc_older_than_secs`
// in pm.yaml. It never kills a process and never touches a pane: it
// deletes registry rows (and their message/event history) and nothing
// else, so it frees no memory and no disk.

/// What every agent-gc timer output says, verbatim.
pub const AGENT_GC_RECORDS_ONLY: &str = "records only: frees no memory and no disk; \
     a removed agent can no longer be resumed";
/// A configured age below this is raised to it, with a warning — an
/// automatic sweep never reaches an agent idle for less than a week.
pub const AGENT_GC_FLOOR_SECS: u64 = 7 * 86_400;
/// The timer sweeps at most this often.
const AGENT_GC_EVERY: Duration = Duration::from_secs(3600);
/// How often the timer re-reads `[host]` — enabling, retuning or
/// disabling it applies without a daemon restart.
const AGENT_GC_RECHECK: Duration = Duration::from_secs(60);

/// The agent-gc timer's setting: `[host] agent_gc_older_than_secs`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentGcSetting {
    /// The configured age in seconds; `None` (the default) is OFF.
    pub configured_secs: Option<u64>,
    /// Why `[host]` could not be read — the timer is then off.
    pub config_error: Option<String>,
}

impl AgentGcSetting {
    /// The timer on, sweeping rows idle longer than `secs`.
    pub fn older_than(secs: u64) -> Self {
        Self {
            configured_secs: Some(secs),
            config_error: None,
        }
    }

    /// `[host]` in `<pm_dir>/pm.yaml`; no file or no key is off, and an
    /// unusable table is off with its error — the sweep fails closed.
    pub fn from_pm_dir(pm_dir: Option<&Path>) -> Self {
        match pm_dir
            .map(crate::doctor::host::read_host_overrides)
            .transpose()
        {
            Ok(overrides) => Self {
                configured_secs: overrides.flatten().and_then(|o| o.agent_gc_older_than_secs),
                config_error: None,
            },
            Err(error) => Self {
                configured_secs: None,
                config_error: Some(error),
            },
        }
    }

    /// The age a sweep uses: the configured one raised to the floor.
    /// `None` is off.
    pub fn effective_secs(&self) -> Option<u64> {
        self.configured_secs.map(|s| s.max(AGENT_GC_FLOOR_SECS))
    }

    /// The operator-facing warning, if any: an unreadable `[host]`, or
    /// an age below the floor that the timer raised.
    pub fn warning(&self) -> Option<String> {
        if let Some(error) = &self.config_error {
            return Some(format!("agent-gc timer off: {error}"));
        }
        match self.configured_secs {
            Some(secs) if secs < AGENT_GC_FLOOR_SECS => Some(format!(
                "[host] agent_gc_older_than_secs {secs} is below the 7-day floor; \
                 the timer uses {AGENT_GC_FLOOR_SECS}s"
            )),
            _ => None,
        }
    }
}

/// The daemon's agent-gc timer: where its setting comes from and what
/// it last did — `health` (`cadence daemon status`) reports both.
struct AgentGcTimer {
    /// `Some` is verbatim (tests); `None` re-reads `[host]` each check.
    pinned: Option<AgentGcSetting>,
    state: Mutex<AgentGcState>,
}

#[derive(Default)]
struct AgentGcState {
    setting: AgentGcSetting,
    next_check: Option<Instant>,
    last_sweep: Option<Instant>,
    last_check_at: Option<f64>,
    last_sweep_at: Option<f64>,
    last_removed: Option<usize>,
}

impl AgentGcTimer {
    fn new(pinned: Option<AgentGcSetting>) -> Self {
        let timer = Self {
            pinned,
            state: Mutex::new(AgentGcState::default()),
        };
        timer.state.lock().unwrap().setting = timer.resolve();
        timer
    }

    fn resolve(&self) -> AgentGcSetting {
        self.pinned.clone().unwrap_or_else(|| {
            AgentGcSetting::from_pm_dir(crate::issue::default_dir().ok().as_deref())
        })
    }

    fn status(&self) -> Value {
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
    fn agent_gc_tick(&self) {
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
/// Event kinds that are bookkeeping, not delivery/report/turn work:
/// they never reset an agent's idle clock. Everything else does — an
/// unknown new kind errs toward keeping the agent.
const AUTO_STOP_PASSIVE_KINDS: &[&str] = &[
    AUTO_STOP_EVENT,
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
/// The newest of these decides whether a stopped agent was stopped by
/// the timer: a later manual stop or a resume supersedes the marker.
const AUTO_STOP_MARKER_KINDS: &[&str] = &[AUTO_STOP_EVENT, "stop_requested", "ready"];
/// What the attached-client exemption can and cannot see.
pub const AUTO_STOP_ATTACH_NOTE: &str = "attached-terminal exemption: pty panes via tmux \
     list-clients; a managed-ws codex TUI client (`codex resume --remote`) is not \
     detectable and does not exempt its agent";

/// The idle auto-stop setting: `[host] auto_stop_idle_secs` and
/// `auto_stop_idle_secs_by_provider` in pm.yaml. Per-agent params
/// (`auto_stop=off`, `auto_stop_idle_secs`) override both.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutoStopSetting {
    /// `None` is the built-in [`AUTO_STOP_DEFAULT_SECS`]; `0` is off.
    pub idle_secs: Option<u64>,
    /// Provider → bound; `0` turns the timer off for that provider.
    pub by_provider: std::collections::BTreeMap<String, u64>,
    /// Why `[host]` could not be read — the timer then stops nothing.
    pub config_error: Option<String>,
}

impl AutoStopSetting {
    /// Off for every provider (per-agent `auto_stop_idle_secs` can still
    /// turn one agent on). Test daemons pin this.
    pub fn off() -> Self {
        Self::idle_after(0)
    }

    /// On for every provider at `secs` (raised to the floor).
    pub fn idle_after(secs: u64) -> Self {
        Self {
            idle_secs: Some(secs),
            ..Self::default()
        }
    }

    /// `[host]` in `<pm_dir>/pm.yaml`; no file or no key is the default
    /// (ON, one hour). An unusable table stops nothing, with its error.
    pub fn from_pm_dir(pm_dir: Option<&Path>) -> Self {
        match pm_dir
            .map(crate::doctor::host::read_host_overrides)
            .transpose()
        {
            Ok(overrides) => {
                let o = overrides.flatten();
                Self {
                    idle_secs: o.as_ref().and_then(|o| o.auto_stop_idle_secs),
                    by_provider: o
                        .and_then(|o| o.auto_stop_idle_secs_by_provider)
                        .unwrap_or_default(),
                    config_error: None,
                }
            }
            Err(error) => Self {
                config_error: Some(error),
                ..Self::default()
            },
        }
    }

    fn floored(secs: u64) -> Option<u64> {
        (secs > 0).then(|| secs.max(AUTO_STOP_FLOOR_SECS))
    }

    /// The host-wide bound (`None` = off).
    pub fn default_bound(&self) -> Option<u64> {
        if self.config_error.is_some() {
            return None;
        }
        match self.idle_secs {
            Some(secs) => Self::floored(secs),
            None => Some(AUTO_STOP_DEFAULT_SECS),
        }
    }

    /// `agent`'s idle bound and where it came from; `None` is off.
    /// Precedence: agent `auto_stop=off`, agent `auto_stop_idle_secs`,
    /// `[host]` by provider, `[host]` default, built-in default.
    pub fn bound_for(&self, agent: &Agent) -> (Option<u64>, String) {
        if self.config_error.is_some() {
            return (None, "pm.yaml [host] unreadable".to_string());
        }
        let param = |key: &str| agent.params.as_ref().and_then(|p| p.get(key)).cloned();
        if param("auto_stop").and_then(|v| v.as_str().map(str::to_string)) == Some("off".into()) {
            return (None, "agent auto_stop=off".to_string());
        }
        let agent_secs = param("auto_stop_idle_secs").and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        });
        if let Some(secs) = agent_secs {
            return (Self::floored(secs), "agent auto_stop_idle_secs".to_string());
        }
        if let Some(&secs) = self.by_provider.get(&agent.provider) {
            return (
                Self::floored(secs),
                format!("[host] auto_stop_idle_secs_by_provider.{}", agent.provider),
            );
        }
        match self.idle_secs {
            Some(secs) => (
                Self::floored(secs),
                "[host] auto_stop_idle_secs".to_string(),
            ),
            None => (Some(AUTO_STOP_DEFAULT_SECS), "default".to_string()),
        }
    }

    /// The operator-facing warning: an unreadable `[host]`, or a bound
    /// below the floor that the timer raised.
    pub fn warning(&self) -> Option<String> {
        if let Some(error) = &self.config_error {
            return Some(format!("idle auto-stop off: {error}"));
        }
        let mut low: Vec<String> = Vec::new();
        if let Some(secs) = self
            .idle_secs
            .filter(|s| (1..AUTO_STOP_FLOOR_SECS).contains(s))
        {
            low.push(format!("auto_stop_idle_secs {secs}"));
        }
        for (provider, secs) in &self.by_provider {
            if (1..AUTO_STOP_FLOOR_SECS).contains(secs) {
                low.push(format!("auto_stop_idle_secs_by_provider.{provider} {secs}"));
            }
        }
        (!low.is_empty()).then(|| {
            format!(
                "[host] {} below the {AUTO_STOP_FLOOR_SECS}s floor; the timer uses \
                 {AUTO_STOP_FLOOR_SECS}s",
                low.join(", ")
            )
        })
    }
}

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
fn auto_stop_view(agent: &Agent, marker: Option<&store::Event>) -> Option<Value> {
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

/// Stamp `auto_stopped` + `state_label` onto an agent JSON row.
fn apply_auto_stop_view(j: &mut Value, agent: &Agent, marker: Option<&store::Event>) {
    if let Some(view) = auto_stop_view(agent, marker) {
        j["state_label"] = view["label"].clone();
        j["auto_stopped"] = view;
    }
}

/// One agent's verdict on a timer check.
#[derive(Debug, PartialEq)]
enum AutoStopVerdict {
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
struct AutoStopTimer {
    /// `Some` is verbatim (tests); `None` re-reads `[host]` each check.
    pinned: Option<AutoStopSetting>,
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    state: Mutex<AutoStopState>,
}

#[derive(Default)]
struct AutoStopState {
    setting: AutoStopSetting,
    last_check_at: Option<f64>,
    /// When the last sweep finished — `last_kept` belongs to it.
    last_sweep_at: Option<f64>,
    last_stop_at: Option<f64>,
    last_stopped: Vec<String>,
    stopped_total: u64,
    /// Why each live agent was kept on the last check.
    last_kept: std::collections::BTreeMap<String, String>,
}

impl AutoStopTimer {
    fn new(
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

    fn resolve(&self) -> AutoStopSetting {
        self.pinned.clone().unwrap_or_else(|| {
            AutoStopSetting::from_pm_dir(crate::issue::default_dir().ok().as_deref())
        })
    }

    fn status(&self) -> Value {
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
    fn auto_stop_tick(self: &Arc<Self>) {
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
    fn auto_stop_verdict(
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

/// Every alias some agent names as its upstream — a group root even
/// when it was registered as a worker.
fn upstream_roots(agents: &[Agent]) -> HashSet<String> {
    agents
        .iter()
        .filter_map(|a| agent_upstream(a).map(str::to_string))
        .collect()
}

fn agent_upstream(agent: &Agent) -> Option<&str> {
    agent
        .params
        .as_ref()
        .and_then(|p| p.get("upstream"))
        .and_then(Value::as_str)
        .filter(|u| !u.is_empty())
}

/// Why `agent` is a group root, if it is one: role/team role `pm`, no
/// upstream (the codebase's `group_root` rule), or named as another
/// agent's upstream.
fn group_root_reason(agent: &Agent, roots: &HashSet<String>) -> Option<&'static str> {
    if agent.role == "pm" || agent.team_role.as_deref() == Some("pm") {
        Some("role pm")
    } else if roots.contains(&agent.alias) {
        Some("has members")
    } else if agent_upstream(agent).is_none() {
        Some("no upstream")
    } else {
        None
    }
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
    /// CAD-113 slot configuration: `Some` is verbatim (tests);
    /// `None` resolves `[host]` in pm.yaml, falling back to defaults.
    pub slots: Option<SlotConfig>,
    /// The slot clock — `None` is `mono_secs`; tests inject a
    /// counter they advance on demand instead of sleeping.
    pub slot_clock: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
    /// Test seam (CAD-241). When set, `serve` waits here after
    /// shutdown is requested and before `Shared::shutdown` — where
    /// endpoint facts used to be read. Production leaves it unset. The
    /// waiter observes that idle actors have already detached, then
    /// waits on the same barrier so the marker is written after that
    /// detach.
    pub release_shutdown_snapshot: Option<Arc<Barrier>>,
    /// CAD-199 agent-gc timer: `Some` is verbatim (tests keep daemons
    /// hermetic this way); `None` reads `[host]
    /// agent_gc_older_than_secs` from pm.yaml each check — unset is off.
    pub agent_gc: Option<AgentGcSetting>,
    /// CAD-96 idle auto-stop: `Some` is verbatim (test daemons pin
    /// [`AutoStopSetting::off`]); `None` reads `[host]` from pm.yaml
    /// each check — unset is ON at one hour.
    pub auto_stop: Option<AutoStopSetting>,
    /// The auto-stop clock (epoch seconds) — `None` is the wall clock;
    /// tests inject one they advance instead of sleeping.
    pub auto_stop_clock: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
}

/// Slot configuration precedence: explicit `ServeOptions.slots`, then
/// `[host]` in the repo's pm.yaml, then the built-in defaults.
fn resolve_slot_config(opts: &ServeOptions) -> SlotConfig {
    if let Some(c) = &opts.slots {
        return c.clone();
    }
    let mut c = SlotConfig::default();
    let overrides = crate::issue::default_dir()
        .ok()
        .as_deref()
        .and_then(crate::doctor::host::host_overrides);
    if let Some(o) = overrides {
        if let Some(v) = o.build_slots {
            c.build_slots = v as usize;
        }
        if let Some(v) = o.suite_slots {
            c.suite_slots = v as usize;
        }
        if let Some(v) = o.jobs_per_lane {
            c.jobs_per_lane = v as usize;
        }
        if let Some(v) = o.starve_secs {
            c.starve_secs = v;
        }
        if let Some(v) = o.priority_lanes {
            c.priority_lanes = v;
        }
        if let Some(v) = o.max_hold_secs {
            c.max_hold_secs = v;
        }
    }
    c
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

/// Relaunch enabled actors at daemon start; fenced ones land in
/// `attention` instead.
fn relaunch_agents(shared: &Arc<Shared>) -> Result<()> {
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
            // A kept-for-adoption `running` message whose agent still
            // fences — a sibling in-flight message swept it into
            // `unknown` — has no actor left to prove the pane.
            // Unverified `running` is just `unknown`: fence it too.
            // Discard its adoption candidates as well — a resume after
            // unfence must be an ordinary open (fresh generation), not
            // an adopt of entries whose panes were never re-proven.
            let _ = shared
                .store
                .orphan_running(&agent.alias, "agent fenced at restart; turn never verified");
            if let Some(entries) = shared.store.take_adoption(&agent.alias) {
                for e in entries {
                    let _ = shared.store.event_public(
                        &agent.alias,
                        "turn_adopt_refused",
                        json!({"message": e.message_id,
                               "turn_id": e.turn_id,
                               "reason": "agent fenced at restart"}),
                    );
                }
            }
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
    Ok(())
}

/// Run the daemon in the foreground until `shutdown` or a signal.
pub fn serve(state_dir: &Path) -> Result<()> {
    serve_with(state_dir, ServeOptions::default())
}

/// `serve` with per-instance options — in-process test daemons pass
/// their mock commands here instead of through the shared environment.
pub fn serve_with(state_dir: &Path, opts: ServeOptions) -> Result<()> {
    std::fs::create_dir_all(state_dir)?;
    // Before the marker is consumed and before recover() writes. A
    // direct `daemon run` of a different build by a non-holder must
    // leave shutdown.json and the database byte-identical. `daemon
    // start` already refuses before spawn; this is the hand-run path.
    crate::rollout::authorize_direct_run(state_dir)?;
    let _singleton = acquire_singleton(state_dir)?;
    // CAD-407: before the marker is consumed and before the store opens
    // (which would create an empty one): an interrupted restore's aside
    // files may be the only copy of the previous store. Every start path
    // — `daemon start`, `run`, `restart` — comes through here.
    crate::backup::refuse_interrupted_restore(state_dir)?;
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
    relaunch_agents(&shared)?;
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
                shared.begin_closing();
            }
        });
    }
    // Stall watch: a running turn that goes silent is reported to
    // whoever waits on it — never interrupted, never replayed.
    {
        let shared = Arc::clone(&shared);
        thread::spawn(move || shared.run_stall_watch());
    }
    // Persistent monitor reconciliation: registrations survive a daemon
    // restart and are checked without an LLM turn or provider invocation.
    {
        let shared = Arc::clone(&shared);
        thread::spawn(move || shared.run_monitor_watch());
    }
    // WAL watch: provider stores checkpointed while their provider
    // idles — CAD-132, the 30 GiB sessions.db-wal that ate the disk.
    {
        let shared = Arc::clone(&shared);
        thread::spawn(move || shared.run_wal_watch());
    }
    // Slot watch (CAD-230b): strict holds end with their holder.
    {
        let shared = Arc::clone(&shared);
        thread::spawn(move || shared.run_slot_watch());
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
    // Former facts snapshot lived in `shutdown`. A test holds this
    // barrier until it has observed idle actors detach, which is the
    // interleaving that used to erase adoption facts.
    if let Some(gate) = &opts.release_shutdown_snapshot {
        gate.wait();
    }
    shared.shutdown();
    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

/// Fallback detail when an unknown fence has no provider account.
pub const UNKNOWN_GENERIC_REASON: &str = "Uncertain provider outcome requires review";

/// Separates a preserved unknown reason from the operator hint. Older
/// rows used ` — reconcile:`; both markers are stripped on restamp.
const UNKNOWN_FENCE_MARK: &str = " — inspect:";

/// Same character cap `current_message_summary` uses for operator-facing text.
const UNKNOWN_DETAIL_CHARS: usize = 512;

/// First sentence of an unknown-outcome hint: look before reconciling.
pub fn unknown_inspect_lead() -> &'static str {
    "Inspect the uncertain message and its side effects before reconciling. \
     A missed render observation does not prove the delivery did not happen."
}

/// What reconciliation is, and how CLI unfence behaves. No command chain:
/// CLI unfence already resumes, and dispatch is not authorized here.
pub fn unknown_recovery_note() -> &'static str {
    "Reconciliation is an explicit operator decision, not an automatic \
     interrupted or completed result. The CLI unfence command resumes by \
     default; do not follow it with a second resume. Pass --no-resume to \
     reconcile without resuming."
}

/// `job show` attention while the kickoff is still `unknown`. Dispatch is
/// described as a later paste, not as the next command.
pub fn unknown_kickoff_attention(message_id: &str, assignee: &str) -> String {
    format!(
        "kickoff {message_id} went unknown — the worker {assignee} is fenced. \
         {} {} Dispatch is a new paste and another revision only after that \
         reconciliation and a decision that continuation is safe.",
        unknown_inspect_lead(),
        unknown_recovery_note()
    )
}

/// Attention for an ordinary interrupted or failed kickoff. Dispatch stays
/// the named next revision; unknown kickoffs do not use this sentence.
pub fn ordinary_terminal_kickoff_attention(
    message_id: &str,
    state: &str,
    task_id: &str,
    next_revision: i64,
) -> String {
    format!(
        "kickoff {message_id} ended '{state}' — `cadence job dispatch {task_id}` \
         starts revision {next_revision}"
    )
}

/// Detail stored ahead of the hint. Accepts the current ` — inspect:`
/// form and the previous ` — reconcile:` form so a restamp does not
/// swallow the provider account into the hint.
pub fn unknown_fence_detail(stored: &str) -> &str {
    let head = stored.split(" — reconcile:").next().unwrap_or(stored);
    head.split(UNKNOWN_FENCE_MARK).next().unwrap_or(head).trim()
}

/// Operator-facing fence text. The detail is bounded and passed through
/// the argv scrubber; screen tails and raw argv are never appended.
pub fn format_unknown_fence(detail: &str) -> String {
    let bounded = bound_unknown_detail(detail);
    let detail = if bounded.is_empty() {
        UNKNOWN_GENERIC_REASON
    } else {
        bounded.as_str()
    };
    format!(
        "{detail}{UNKNOWN_FENCE_MARK} {} {}",
        unknown_inspect_lead(),
        unknown_recovery_note()
    )
}

fn bound_unknown_detail(detail: &str) -> String {
    let flat: String = detail
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let words: Vec<String> = flat.split_whitespace().map(str::to_string).collect();
    if words.is_empty() {
        return String::new();
    }
    let safe = crate::doctor::host::redact_argv(&words);
    let count = safe.chars().count();
    if count <= UNKNOWN_DETAIL_CHARS {
        return safe;
    }
    let mut out: String = safe.chars().take(UNKNOWN_DETAIL_CHARS).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod pty_retry_tests {
    use super::*;

    #[test]
    fn retry_base_defaults_to_five_seconds() {
        assert_eq!(parse_pty_retry_base(None), Ok(Duration::from_secs(5)));
        assert_eq!(PTY_RETRY_BASE, Duration::from_secs(5));
    }

    #[test]
    fn retry_base_accepts_seconds_inside_the_bounds() {
        assert_eq!(parse_pty_retry_base(Some("1")), Ok(Duration::from_secs(1)));
        assert_eq!(
            parse_pty_retry_base(Some(" 2.5 ")),
            Ok(Duration::from_millis(2500))
        );
        assert_eq!(
            parse_pty_retry_base(Some("0.1")),
            Ok(Duration::from_millis(100))
        );
        assert_eq!(
            parse_pty_retry_base(Some("3600")),
            Ok(Duration::from_secs(3600))
        );
    }

    #[test]
    fn retry_base_refuses_values_outside_the_bounds_or_not_numbers() {
        for raw in [
            "0", "-1", "0.05", "0.099", "3600.5", "1e9", "NaN", "nan", "inf", "-inf", "", "five",
            "5s",
        ] {
            let got = parse_pty_retry_base(Some(raw));
            assert!(got.is_err(), "{raw:?} must be refused, got {got:?}");
            assert!(
                got.unwrap_err().contains("CADENCE_PTY_RETRY_SECS"),
                "reason names the knob for {raw:?}"
            );
        }
    }

    #[test]
    fn gate_backoff_doubles_to_a_six_times_cap() {
        let five = Duration::from_secs(5);
        let schedule: Vec<u64> = (0..6).map(|n| gate_backoff(five, n).as_secs()).collect();
        assert_eq!(schedule, vec![5, 10, 20, 30, 30, 30]);
        // The cap scales with the base, and a huge count cannot overflow.
        let one = Duration::from_secs(1);
        assert_eq!(gate_backoff(one, 0), one);
        assert_eq!(gate_backoff(one, 2), Duration::from_secs(4));
        assert_eq!(gate_backoff(one, 3), Duration::from_secs(6));
        assert_eq!(gate_backoff(one, u32::MAX), Duration::from_secs(6));
        let floor = Duration::from_millis(100);
        assert_eq!(gate_backoff(floor, 9), Duration::from_millis(600));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewAgent;

    /// CAD-407: `serve` — so `daemon start`, `run` and `restart` — refuses
    /// while an interrupted restore's aside files exist, before the
    /// shutdown marker is consumed or a store is created or opened. The
    /// error names the leftover and the `mv` that puts it back.
    #[test]
    fn serve_refuses_after_an_interrupted_restore_without_touching_state() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        let aside = state.join("cadence.sqlite3.replaced-20260923T000000Z-deadbeef");
        std::fs::write(&aside, b"previous store").unwrap();
        let marker = state.join(SHUTDOWN_FILE);
        std::fs::write(&marker, b"{}").unwrap();

        // On a thread: a serve that wrongly starts must fail the test,
        // not hang it.
        let owned = state.to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(serve_with(&owned, ServeOptions::default()));
        });
        let err = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("serve started over an interrupted restore")
            .unwrap_err()
            .to_string();

        assert!(err.contains("interrupted restore"), "{err}");
        assert!(err.contains(&aside.display().to_string()), "{err}");
        let put_back = format!(
            "mv '{}' '{}'",
            aside.display(),
            state.join("cadence.sqlite3").display()
        );
        assert!(err.contains(&put_back), "{err}");
        assert!(!state.join("cadence.sqlite3").exists());
        assert!(!state.join("cadence.sock").exists());
        assert_eq!(std::fs::read(&marker).unwrap(), b"{}");
        assert_eq!(std::fs::read(&aside).unwrap(), b"previous store");

        // Once it is moved away the check passes.
        std::fs::remove_file(&aside).unwrap();
        crate::backup::refuse_interrupted_restore(state).unwrap();
    }

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
                team_role: None,
                model_policy: None,
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

    #[test]
    fn cloud_hold_notifies_then_a_later_poll_recovers_the_sha() {
        use std::sync::atomic::AtomicUsize;

        const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
        let gets = Arc::new(AtomicUsize::new(0));
        let posts = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicBool::new(false));
        let early_post = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let gets_t = Arc::clone(&gets);
        let posts_t = Arc::clone(&posts);
        let finished_t = Arc::clone(&finished);
        let early_t = Arc::clone(&early_post);
        let stop_t = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !stop_t.load(Ordering::SeqCst) {
                let mut req = match server.recv_timeout(Duration::from_millis(50)) {
                    Ok(Some(req)) => req,
                    _ => continue,
                };
                let path = req.url().to_string();
                let method = req.method().as_str().to_string();
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let (status, payload): (u16, String) = if path.contains("/repositories") {
                    (
                        200,
                        json!({"repositories": [{"name": "cadence", "owner": "favcrm"}]})
                            .to_string(),
                    )
                } else if method == "POST" && path.ends_with("/sessions") {
                    (
                        200,
                        json!({
                            "session_id": "devin-created",
                            "status": "running",
                            "status_detail": "working",
                            "url": "https://app.devin.ai/sessions/devin-created"
                        })
                        .to_string(),
                    )
                } else if method == "POST" && path.contains("/messages") {
                    let n = posts_t.fetch_add(1, Ordering::SeqCst);
                    if n >= 1 && !finished_t.load(Ordering::SeqCst) {
                        early_t.store(true, Ordering::SeqCst);
                    }
                    (200, json!({"ok": true}).to_string())
                } else if method == "GET" && path.contains("/messages") {
                    let items = if posts_t.load(Ordering::SeqCst) == 0 {
                        json!([])
                    } else if posts_t.load(Ordering::SeqCst) >= 2 {
                        json!([
                            {"event_id": "evt-1", "source": "devin", "message": format!("done\nSHA: {SHA}"), "created_at": 1},
                            {"event_id": "evt-2", "source": "devin", "message": "follow-up done", "created_at": 2}
                        ])
                    } else {
                        json!([{"event_id": "evt-1", "source": "devin", "message": format!("done\nSHA: {SHA}"), "created_at": 1}])
                    };
                    let total = items.as_array().map(|rows| rows.len()).unwrap_or(0);
                    (
                        200,
                        json!({"items": items, "has_next_page": false, "end_cursor": null, "total": total})
                            .to_string(),
                    )
                } else if method == "GET" && path.contains("/sessions/") {
                    if posts_t.load(Ordering::SeqCst) == 0 {
                        (
                            200,
                            json!({
                                "session_id": "devin-created",
                                "status": "running",
                                "status_detail": "working",
                                "url": "https://app.devin.ai/sessions/devin-created",
                                "pull_requests": [],
                            })
                            .to_string(),
                        )
                    } else {
                        let n = gets_t.fetch_add(1, Ordering::SeqCst);
                        if n < 3 {
                            (500, json!({"error": "transient"}).to_string())
                        } else {
                            finished_t.store(true, Ordering::SeqCst);
                            (
                                200,
                                json!({
                                    "session_id": "devin-created",
                                    "status": "running",
                                    "status_detail": "finished",
                                    "url": "https://app.devin.ai/sessions/devin-created",
                                    "pull_requests": [],
                                })
                                .to_string(),
                            )
                        }
                    }
                } else {
                    (200, json!({"ok": true}).to_string())
                };
                let _ =
                    req.respond(tiny_http::Response::from_string(payload).with_status_code(status));
            }
        });
        let (dir, shared) = shared();
        let base = format!("http://{addr}");
        shared.provider_env.set("CADENCE_DEVIN_API_BASE", &base);
        shared
            .provider_env
            .set("CADENCE_DEVIN_API_KEY", "cog_test_secret_value");
        shared.provider_env.set("CADENCE_DEVIN_ORG_ID", "org-test");
        shared
            .provider_env
            .set("CADENCE_DEVIN_POLL_INTERVAL_MS", "20");
        shared
            .provider_env
            .set("CADENCE_DEVIN_POLL_BUDGET_MS", "120");
        let cwd = dir.path().to_str().unwrap();
        let spec = dir.path().join("spec.md");
        std::fs::write(&spec, "Do the cloud work.").unwrap();
        shared
            .store
            .register_agent(&NewAgent {
                alias: "pm",
                provider: "fake",
                endpoint_kind: "managed",
                role: "pm",
                cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        let params = r#"{"repos":["favcrm/cadence"],"upstream":"pm"}"#;
        shared
            .store
            .register_agent(&NewAgent {
                alias: "cloud-1",
                provider: "devin",
                endpoint_kind: "cloud",
                role: "worker",
                cwd,
                sandbox: "read-only",
                instructions: None,
                params: Some(params),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        shared
            .store
            .create_job(
                "j1",
                None,
                spec.to_str().unwrap(),
                &"0".repeat(64),
                "pm",
                None,
                None,
                None,
                2,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        shared
            .store
            .create_task(
                "j1",
                "t1",
                None,
                Some("cloud-1"),
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        let (_task, kickoff, dup, _) = shared
            .store
            .dispatch_task("t1", None, None, "test")
            .unwrap();
        assert!(!dup);
        shared
            .store
            .enqueue(
                "cloud-1",
                "follow-up while the session works",
                Some("pm"),
                "follow-up",
                "user",
            )
            .unwrap();
        shared.launch_actor("cloud-1").unwrap();
        let started = Instant::now();
        loop {
            if started.elapsed() > Duration::from_secs(8) {
                panic!(
                    "held poll did not recover: task={:?} kickoff={:?} events={:?}",
                    shared.store.task("t1").ok(),
                    shared.store.message(&kickoff).ok(),
                    shared.store.events("cloud-1", 0, 40).ok()
                );
            }
            if shared.store.task("t1").unwrap().head_sha.as_deref() == Some(SHA) {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !early_post.load(Ordering::SeqCst),
            "posted the next message while the session was still held"
        );
        assert!(shared
            .store
            .events("cloud-1", 0, 40)
            .unwrap()
            .iter()
            .any(|event| event.kind == "cloud_hold"));
        assert!(shared.store.messages("pm").unwrap().iter().any(|message| {
            message.body.contains("not fenced") && message.body.contains("held")
        }));
        let follow_started = Instant::now();
        loop {
            if follow_started.elapsed() > Duration::from_secs(8) {
                panic!(
                    "follow-up stayed {:?}",
                    shared.store.message("follow-up").unwrap()
                );
            }
            let state = shared.store.message("follow-up").unwrap().unwrap().state;
            if state == "completed" || state == "failed" {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!early_post.load(Ordering::SeqCst));
        shared.store.set_enabled("cloud-1", false).unwrap();
        shared.begin_closing();
        let handle = {
            let lc = shared.lifecycle.lock().unwrap();
            lc.agents
                .get("cloud-1")
                .and_then(|ctl| ctl.thread.lock().unwrap().take())
        };
        if let Some(ctl) = shared.lifecycle.lock().unwrap().agents.get("cloud-1") {
            ctl.wake.notify_all();
        }
        if let Some(handle) = handle {
            let _ = handle.join();
        }
        stop.store(true, Ordering::SeqCst);
        let _ = thread.join();
    }

    fn stop_cloud(shared: &Shared, stop: &AtomicBool, thread: thread::JoinHandle<()>) {
        shared.store.set_enabled("cloud-1", false).unwrap();
        shared.begin_closing();
        let handle = {
            let lc = shared.lifecycle.lock().unwrap();
            lc.agents
                .get("cloud-1")
                .and_then(|ctl| ctl.thread.lock().unwrap().take())
        };
        if let Some(ctl) = shared.lifecycle.lock().unwrap().agents.get("cloud-1") {
            ctl.wake.notify_all();
        }
        if let Some(handle) = handle {
            let _ = handle.join();
        }
        stop.store(true, Ordering::SeqCst);
        let _ = thread.join();
    }

    #[test]
    fn cloud_prewrite_fails_the_message_without_recovery() {
        use std::sync::atomic::AtomicUsize;

        let posts = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let posts_t = Arc::clone(&posts);
        let stop_t = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !stop_t.load(Ordering::SeqCst) {
                let mut req = match server.recv_timeout(Duration::from_millis(50)) {
                    Ok(Some(req)) => req,
                    _ => continue,
                };
                let path = req.url().to_string();
                let method = req.method().as_str().to_string();
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let (status, payload): (u16, String) = if path.contains("/repositories") {
                    (
                        200,
                        json!({"repositories": [{"name": "cadence", "owner": "favcrm"}]})
                            .to_string(),
                    )
                } else if method == "POST" && path.ends_with("/sessions") {
                    (
                        200,
                        json!({
                            "session_id": "devin-created",
                            "status": "running",
                            "status_detail": "working",
                            "url": "https://app.devin.ai/sessions/devin-created"
                        })
                        .to_string(),
                    )
                } else if method == "POST" && path.contains("/messages") {
                    posts_t.fetch_add(1, Ordering::SeqCst);
                    (200, json!({"ok": true}).to_string())
                } else if method == "GET" && path.contains("/messages") {
                    (429, json!({"error": "slow"}).to_string())
                } else if method == "GET" && path.contains("/sessions/") {
                    (
                        200,
                        json!({
                            "session_id": "devin-created",
                            "status": "running",
                            "status_detail": "working",
                            "url": "https://app.devin.ai/sessions/devin-created",
                            "pull_requests": [],
                        })
                        .to_string(),
                    )
                } else {
                    (200, json!({"ok": true}).to_string())
                };
                let _ =
                    req.respond(tiny_http::Response::from_string(payload).with_status_code(status));
            }
        });
        let (dir, shared) = shared();
        let base = format!("http://{addr}");
        shared.provider_env.set("CADENCE_DEVIN_API_BASE", &base);
        shared
            .provider_env
            .set("CADENCE_DEVIN_API_KEY", "cog_test_secret_value");
        shared.provider_env.set("CADENCE_DEVIN_ORG_ID", "org-test");
        shared
            .provider_env
            .set("CADENCE_DEVIN_POLL_INTERVAL_MS", "20");
        shared
            .provider_env
            .set("CADENCE_DEVIN_POLL_BUDGET_MS", "200");
        let cwd = dir.path().to_str().unwrap();
        shared
            .store
            .register_agent(&NewAgent {
                alias: "cloud-1",
                provider: "devin",
                endpoint_kind: "cloud",
                role: "worker",
                cwd,
                sandbox: "read-only",
                instructions: None,
                params: Some(r#"{"repos":["favcrm/cadence"]}"#),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        shared
            .store
            .enqueue("cloud-1", "do the task", None, "prewrite-1", "user")
            .unwrap();
        shared.launch_actor("cloud-1").unwrap();
        let started = Instant::now();
        loop {
            if started.elapsed() > Duration::from_secs(8) {
                panic!(
                    "prewrite did not fail the message: {:?}",
                    shared.store.message("prewrite-1").ok()
                );
            }
            if shared.store.message("prewrite-1").unwrap().unwrap().state == "failed" {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let message = shared.store.message("prewrite-1").unwrap().unwrap();
        assert_eq!(message.state, "failed");
        let blob = format!(
            "{} {}",
            message.result.unwrap_or(json!({})),
            message.error.unwrap_or_default()
        );
        assert!(!blob.contains("unknown"), "{blob}");
        assert_eq!(posts.load(Ordering::SeqCst), 0);
        let events = shared.store.events("cloud-1", 0, 40).unwrap();
        assert!(events.iter().all(|event| event.kind != "cloud_hold"));
        assert!(events
            .iter()
            .all(|event| event.kind != "cloud_recover_escalated"));
        stop_cloud(&shared, &stop, thread);
    }

    /// Calls a held-turn mock records. `healthy` ends the lost polls.
    #[derive(Default)]
    struct HoldCalls {
        posts: std::sync::atomic::AtomicUsize,
        archives: std::sync::atomic::AtomicUsize,
        deletes: std::sync::atomic::AtomicUsize,
        healthy: AtomicBool,
        stop: AtomicBool,
    }

    /// A Devin mock whose session GET fails with 503 after the first
    /// message post, so that turn is held until `healthy` is set.
    fn cloud_hold_server() -> (String, Arc<HoldCalls>, thread::JoinHandle<()>) {
        let calls = Arc::new(HoldCalls::default());
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let seen = Arc::clone(&calls);
        let thread = thread::spawn(move || {
            while !seen.stop.load(Ordering::SeqCst) {
                let mut req = match server.recv_timeout(Duration::from_millis(50)) {
                    Ok(Some(req)) => req,
                    _ => continue,
                };
                let path = req.url().to_string();
                let method = req.method().as_str().to_string();
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let posts = seen.posts.load(Ordering::SeqCst);
                let session = |detail: &str| {
                    json!({
                        "session_id": "devin-created",
                        "status": "running",
                        "status_detail": detail,
                        "url": "https://app.devin.ai/sessions/devin-created",
                        "pull_requests": [],
                    })
                    .to_string()
                };
                let (status, payload): (u16, String) = if path.contains("/repositories") {
                    (
                        200,
                        json!({"repositories": [{"name": "cadence", "owner": "favcrm"}]})
                            .to_string(),
                    )
                } else if method == "POST" && path.ends_with("/sessions") {
                    (200, session("working"))
                } else if method == "POST" && path.ends_with("/archive") {
                    seen.archives.fetch_add(1, Ordering::SeqCst);
                    (200, json!({"ok": true}).to_string())
                } else if method == "DELETE" {
                    seen.deletes.fetch_add(1, Ordering::SeqCst);
                    (200, json!({"ok": true}).to_string())
                } else if method == "POST" && path.contains("/messages") {
                    seen.posts.fetch_add(1, Ordering::SeqCst);
                    (200, json!({"ok": true}).to_string())
                } else if method == "GET" && path.contains("/messages") {
                    let items: Vec<Value> = (2..=posts)
                        .map(|n| {
                            json!({"event_id": format!("evt-{n}"), "source": "devin",
                                   "message": format!("reply {n}"), "created_at": n})
                        })
                        .collect();
                    let total = items.len();
                    (
                        200,
                        json!({"items": items, "has_next_page": false,
                               "end_cursor": null, "total": total})
                        .to_string(),
                    )
                } else if method == "GET" && path.contains("/sessions/") {
                    if posts == 0 {
                        (200, session("working"))
                    } else if seen.healthy.load(Ordering::SeqCst) {
                        (200, session("finished"))
                    } else {
                        (503, json!({"error": "unavailable"}).to_string())
                    }
                } else {
                    (200, json!({"ok": true}).to_string())
                };
                let _ =
                    req.respond(tiny_http::Response::from_string(payload).with_status_code(status));
            }
        });
        (format!("http://{addr}"), calls, thread)
    }

    fn devin_env(shared: &Shared, base: &str) {
        shared.provider_env.set("CADENCE_DEVIN_API_BASE", base);
        shared
            .provider_env
            .set("CADENCE_DEVIN_API_KEY", "cog_test_secret_value");
        shared.provider_env.set("CADENCE_DEVIN_ORG_ID", "org-test");
        shared
            .provider_env
            .set("CADENCE_DEVIN_POLL_INTERVAL_MS", "20");
        shared
            .provider_env
            .set("CADENCE_DEVIN_POLL_BUDGET_MS", "150");
    }

    /// Register `cloud-1`, send it one message and wait until the actor
    /// holds that turn.
    fn hold_cloud_turn(shared: &Arc<Shared>, dir: &Path, base: &str, id: &str) {
        devin_env(shared, base);
        shared
            .store
            .register_agent(&NewAgent {
                alias: "cloud-1",
                provider: "devin",
                endpoint_kind: "cloud",
                role: "worker",
                cwd: dir.to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: Some(r#"{"repos":["favcrm/cadence"]}"#),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        shared
            .store
            .enqueue("cloud-1", "do the task", None, id, "user")
            .unwrap();
        shared.launch_actor("cloud-1").unwrap();
        let started = Instant::now();
        loop {
            let held = shared
                .lifecycle
                .lock()
                .unwrap()
                .agents
                .get("cloud-1")
                .is_some_and(|ctl| ctl.cloud_held.load(Ordering::SeqCst));
            if held && shared.store.message(id).unwrap().unwrap().state == "unknown" {
                return;
            }
            if started.elapsed() > Duration::from_secs(8) {
                panic!(
                    "turn was not held: {:?} {:?}",
                    shared.store.message(id).ok(),
                    shared.store.events("cloud-1", 0, 40).ok()
                );
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn join_actor(shared: &Shared, alias: &str) {
        let handle = {
            let lc = shared.lifecycle.lock().unwrap();
            lc.agents
                .get(alias)
                .and_then(|ctl| ctl.thread.lock().unwrap().take())
        };
        if let Some(ctl) = shared.lifecycle.lock().unwrap().agents.get(alias) {
            ctl.wake.notify_all();
        }
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    /// A daemon restart during a hold does not resume the turn: the
    /// held message stays unknown and fences the agent for an operator
    /// reconcile. No actor starts, nothing is posted or archived.
    #[test]
    fn cloud_restart_during_a_hold_fences_for_reconcile() {
        let (base, calls, server) = cloud_hold_server();
        let (dir, shared) = shared();
        hold_cloud_turn(&shared, dir.path(), &base, "held-1");
        assert_eq!(calls.posts.load(Ordering::SeqCst), 1);
        // Shutdown: the actor detaches and the agent stays enabled.
        shared.begin_closing();
        join_actor(&shared, "cloud-1");
        drop(shared);

        let restarted = Shared::new(dir.path(), &ServeOptions::default()).unwrap();
        devin_env(&restarted, &base);
        relaunch_agents(&restarted).unwrap();
        thread::sleep(Duration::from_millis(300));

        let agent = restarted.store.agent("cloud-1").unwrap();
        assert_eq!(agent.state, "attention", "{:?}", agent.error);
        assert!(agent.enabled);
        assert!(
            !restarted.lifecycle.lock().unwrap().owned("cloud-1"),
            "an actor started for a fenced agent"
        );
        assert_eq!(
            restarted.store.message("held-1").unwrap().unwrap().state,
            "unknown"
        );
        assert!(restarted
            .store
            .events("cloud-1", 0, 80)
            .unwrap()
            .iter()
            .any(|event| event.kind == "relaunch_skipped"));
        assert_eq!(calls.posts.load(Ordering::SeqCst), 1, "restart posted");
        assert_eq!(calls.archives.load(Ordering::SeqCst), 0, "archived");
        assert_eq!(calls.deletes.load(Ordering::SeqCst), 0, "deleted");
        calls.stop.store(true, Ordering::SeqCst);
        let _ = server.join();
    }

    /// `agent stop` during a hold leaves the session for the reconcile:
    /// no archive, no delete, and the message stays unknown.
    #[test]
    fn cloud_stop_during_a_hold_does_not_archive_the_session() {
        let (base, calls, server) = cloud_hold_server();
        let (dir, shared) = shared();
        hold_cloud_turn(&shared, dir.path(), &base, "held-1");
        shared.rpc_stop(&json!({"alias": "cloud-1"})).unwrap();

        assert_eq!(calls.archives.load(Ordering::SeqCst), 0, "archived");
        assert_eq!(calls.deletes.load(Ordering::SeqCst), 0, "deleted");
        assert_eq!(calls.posts.load(Ordering::SeqCst), 1);
        let message = shared.store.message("held-1").unwrap().unwrap();
        assert_eq!(message.state, "unknown", "{:?}", message.result);
        assert_eq!(shared.store.agent("cloud-1").unwrap().state, "stopped");
        calls.stop.store(true, Ordering::SeqCst);
        let _ = server.join();
    }

    /// The notice's live exit works: a reconcile while the actor holds
    /// the turn ends the hold and the next message runs.
    #[test]
    fn cloud_reconcile_during_a_hold_releases_the_actor() {
        let (base, calls, server) = cloud_hold_server();
        let (dir, shared) = shared();
        hold_cloud_turn(&shared, dir.path(), &base, "held-1");
        shared
            .rpc_reconcile(&json!({"message": "held-1", "status": "interrupted"}))
            .unwrap();
        calls.healthy.store(true, Ordering::SeqCst);
        shared
            .store
            .enqueue("cloud-1", "next task", None, "next-1", "user")
            .unwrap();
        shared.notify_agent("cloud-1");
        let started = Instant::now();
        loop {
            let state = shared.store.message("next-1").unwrap().unwrap().state;
            if state == "completed" {
                break;
            }
            if started.elapsed() > Duration::from_secs(8) {
                panic!(
                    "next message stayed {state}: {:?}",
                    shared.store.events("cloud-1", 0, 80).ok()
                );
            }
            thread::sleep(Duration::from_millis(20));
        }
        let held = shared.store.message("held-1").unwrap().unwrap();
        assert_eq!(held.state, "interrupted");
        assert_eq!(
            held.result.unwrap()["via"],
            json!("operator_reconcile"),
            "the actor overwrote the operator's reconcile"
        );
        assert_eq!(calls.posts.load(Ordering::SeqCst), 2);
        assert_ne!(shared.store.agent("cloud-1").unwrap().state, "attention");
        shared.store.set_enabled("cloud-1", false).unwrap();
        shared.begin_closing();
        join_actor(&shared, "cloud-1");
        calls.stop.store(true, Ordering::SeqCst);
        let _ = server.join();
    }

    #[test]
    fn cloud_hold_recovery_backs_off_and_escalates_once() {
        use std::sync::atomic::AtomicUsize;

        let gets = Arc::new(AtomicUsize::new(0));
        let posts = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let gets_t = Arc::clone(&gets);
        let posts_t = Arc::clone(&posts);
        let stop_t = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !stop_t.load(Ordering::SeqCst) {
                let mut req = match server.recv_timeout(Duration::from_millis(50)) {
                    Ok(Some(req)) => req,
                    _ => continue,
                };
                let path = req.url().to_string();
                let method = req.method().as_str().to_string();
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let (status, payload): (u16, String) = if path.contains("/repositories") {
                    (
                        200,
                        json!({"repositories": [{"name": "cadence", "owner": "favcrm"}]})
                            .to_string(),
                    )
                } else if method == "POST" && path.ends_with("/sessions") {
                    (
                        200,
                        json!({
                            "session_id": "devin-created",
                            "status": "running",
                            "status_detail": "working",
                            "url": "https://app.devin.ai/sessions/devin-created"
                        })
                        .to_string(),
                    )
                } else if method == "POST" && path.contains("/messages") {
                    posts_t.fetch_add(1, Ordering::SeqCst);
                    (200, json!({"ok": true}).to_string())
                } else if method == "GET" && path.contains("/messages") {
                    gets_t.fetch_add(1, Ordering::SeqCst);
                    if posts_t.load(Ordering::SeqCst) == 0 {
                        (
                            200,
                            json!({"items": [], "has_next_page": false, "end_cursor": null, "total": 0})
                                .to_string(),
                        )
                    } else {
                        (429, json!({"error": "slow down"}).to_string())
                    }
                } else if method == "GET" && path.contains("/sessions/") {
                    gets_t.fetch_add(1, Ordering::SeqCst);
                    if posts_t.load(Ordering::SeqCst) == 0 {
                        (
                            200,
                            json!({
                                "session_id": "devin-created",
                                "status": "running",
                                "status_detail": "working",
                                "url": "https://app.devin.ai/sessions/devin-created",
                                "pull_requests": [],
                            })
                            .to_string(),
                        )
                    } else {
                        (429, json!({"error": "slow down"}).to_string())
                    }
                } else {
                    (200, json!({"ok": true}).to_string())
                };
                let _ =
                    req.respond(tiny_http::Response::from_string(payload).with_status_code(status));
            }
        });
        let (dir, shared) = shared();
        let base = format!("http://{addr}");
        shared.provider_env.set("CADENCE_DEVIN_API_BASE", &base);
        shared
            .provider_env
            .set("CADENCE_DEVIN_API_KEY", "cog_test_secret_value");
        shared.provider_env.set("CADENCE_DEVIN_ORG_ID", "org-test");
        shared
            .provider_env
            .set("CADENCE_DEVIN_POLL_INTERVAL_MS", "20");
        shared
            .provider_env
            .set("CADENCE_DEVIN_POLL_BUDGET_MS", "80");
        shared
            .provider_env
            .set("CADENCE_DEVIN_RECOVER_BUDGET_MS", "80");
        let cwd = dir.path().to_str().unwrap();
        shared
            .store
            .register_agent(&NewAgent {
                alias: "pm",
                provider: "fake",
                endpoint_kind: "managed",
                role: "pm",
                cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        let params = r#"{"repos":["favcrm/cadence"],"upstream":"pm"}"#;
        shared
            .store
            .register_agent(&NewAgent {
                alias: "cloud-1",
                provider: "devin",
                endpoint_kind: "cloud",
                role: "worker",
                cwd,
                sandbox: "read-only",
                instructions: None,
                params: Some(params),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        shared
            .store
            .enqueue("cloud-1", "do the held task", Some("pm"), "turn-1", "user")
            .unwrap();
        shared
            .store
            .enqueue(
                "cloud-1",
                "follow-up while the session works",
                Some("pm"),
                "follow-up",
                "user",
            )
            .unwrap();
        shared.launch_actor("cloud-1").unwrap();
        let started = Instant::now();
        loop {
            if started.elapsed() > Duration::from_secs(5) {
                panic!(
                    "recovery did not escalate: gets={} posts={} events={:?}",
                    gets.load(Ordering::SeqCst),
                    posts.load(Ordering::SeqCst),
                    shared.store.events("cloud-1", 0, 40).ok()
                );
            }
            let n = shared
                .store
                .events("cloud-1", 0, 40)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "cloud_recover_escalated")
                .count();
            if n >= 1 {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let during = gets.load(Ordering::SeqCst);
        assert!(
            during <= 12,
            "held recovery made {during} session GETs; expected at most 12"
        );
        thread::sleep(Duration::from_millis(300));
        let after = gets.load(Ordering::SeqCst);
        assert_eq!(
            after, during,
            "recovery kept polling after escalation ({during} -> {after})"
        );
        let escalations = shared
            .store
            .events("cloud-1", 0, 40)
            .unwrap()
            .iter()
            .filter(|event| event.kind == "cloud_recover_escalated")
            .count();
        assert_eq!(escalations, 1, "expected one escalation, saw {escalations}");
        let notices: Vec<_> = shared
            .store
            .messages("pm")
            .unwrap()
            .into_iter()
            .filter(|message| message.body.contains("stopped polling"))
            .collect();
        assert_eq!(notices.len(), 1, "expected one escalation notice");
        assert!(notices[0].body.contains("not fenced"));
        assert!(notices[0].body.contains("not replayed"));
        assert!(notices[0].body.contains(
            "`cadence message reconcile turn-1 --status <completed|failed|interrupted>`"
        ));
        assert!(notices[0]
            .body
            .contains("https://app.devin.ai/sessions/devin-created"));
        assert!(!notices[0].body.contains("cadence agent stop"));
        let agent = shared.store.agent("cloud-1").unwrap();
        assert_ne!(agent.state, "attention");
        assert!(agent.enabled);
        assert_eq!(
            shared.store.message("turn-1").unwrap().unwrap().state,
            "unknown"
        );
        assert_eq!(
            shared.store.message("follow-up").unwrap().unwrap().state,
            "queued"
        );
        assert_eq!(
            posts.load(Ordering::SeqCst),
            1,
            "replayed work into the session"
        );
        shared.store.set_enabled("cloud-1", false).unwrap();
        shared.begin_closing();
        let handle = {
            let lc = shared.lifecycle.lock().unwrap();
            lc.agents
                .get("cloud-1")
                .and_then(|ctl| ctl.thread.lock().unwrap().take())
        };
        if let Some(ctl) = shared.lifecycle.lock().unwrap().agents.get("cloud-1") {
            ctl.wake.notify_all();
        }
        if let Some(handle) = handle {
            let _ = handle.join();
        }
        stop.store(true, Ordering::SeqCst);
        let _ = thread.join();
    }

    /// A `session_minted` whose `set_params` fails must not vanish:
    /// the mint is still recorded and a `session_persist_failed` sits
    /// beside it — a lost persist would otherwise silently re-mint a
    /// new session on every retry. The deterministic failure is an
    /// alias the store doesn't know; events key on alias text, so
    /// registering the alias afterwards still surfaces both rows.
    #[test]
    fn session_minted_persist_failure_is_evented() {
        let (dir, shared) = shared();
        shared.on_provider_event(
            "ghost",
            "cadence/session_minted",
            json!({"session": "chat-1"}),
        );
        register(&shared, dir.path(), "ghost");
        let kinds: Vec<String> = shared
            .store
            .events("ghost", 0, 50)
            .unwrap()
            .iter()
            .map(|e| e.kind.clone())
            .collect();
        assert!(kinds.iter().any(|k| k == "session_minted"), "{kinds:?}");
        assert!(
            kinds.iter().any(|k| k == "session_persist_failed"),
            "{kinds:?}"
        );
    }

    /// The daemon refuses to clear for an endpoint that never opted
    /// in: `session_resume_failed` fired at a non-disposable provider
    /// is recorded for visibility but `params.session` survives — the
    /// spec gate is the second boundary so a misbehaving profile can
    /// never drop an operator's session.
    #[test]
    fn session_resume_failed_never_clears_nondisposable() {
        let (dir, shared) = shared();
        shared
            .store
            .register_agent(&NewAgent {
                alias: "cl1",
                provider: "claude",
                endpoint_kind: "pty",
                role: "worker",
                cwd: dir.path().to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        shared
            .store
            .set_params("cl1", &json!({"session": "claude-session-1"}))
            .unwrap();
        shared.on_provider_event(
            "cl1",
            "cadence/session_resume_failed",
            json!({"session": "claude-session-1", "reason": "timed out"}),
        );
        let agent = shared.store.agent("cl1").unwrap();
        assert_eq!(
            agent
                .params
                .as_ref()
                .and_then(|p| p.get("session"))
                .and_then(Value::as_str),
            Some("claude-session-1"),
            "non-disposable endpoint must keep its stored session"
        );
        let kinds: Vec<String> = shared
            .store
            .events("cl1", 0, 50)
            .unwrap()
            .iter()
            .map(|e| e.kind.clone())
            .collect();
        assert!(
            kinds.iter().any(|k| k == "session_resume_failed"),
            "{kinds:?}"
        );
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

    // ---------- CAD-160: task-bound messages restate the task ----------

    /// A pm, a pty worker in its group, and job `j1` whose default task
    /// `j1-t1` carries a title, a spec and a CAD-300 acceptance listing.
    fn task_bound_fixture(acceptance: &str) -> (tempfile::TempDir, Arc<Shared>) {
        let (dir, shared) = shared();
        register(&shared, dir.path(), "pm");
        shared
            .store
            .register_agent(&NewAgent {
                alias: "w1",
                provider: "devin",
                endpoint_kind: "pty",
                role: "worker",
                cwd: dir.path().to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: Some(&json!({"upstream": "pm"}).to_string()),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        shared
            .store
            .create_job(
                "j1",
                None,
                "/specs/j1.md",
                &"0".repeat(64),
                "pm",
                None,
                None,
                None,
                2,
                None,
                Some("Wire the email provider"),
                None,
                None,
                None,
                Some("w1"),
                Some(acceptance),
            )
            .unwrap();
        (dir, shared)
    }

    fn stored_body(shared: &Shared, id: &str) -> String {
        shared.store.message(id).unwrap().unwrap().body
    }

    /// The body stored at enqueue is the exact text the pty adapter
    /// pastes: the sender's text, then the objective, then every
    /// still-unchecked criterion, in that order, on one line.
    #[test]
    fn task_bound_send_restates_objective_and_outstanding_criteria() {
        let (_dir, shared) =
            task_bound_fixture(r#"1) [ ] "sends via V2"; 2) [x] "tests green"; 3) [ ] "docs""#);
        let text = "use the V1 email provider";
        shared
            .rpc_send(&json!({"alias": "w1", "text": text, "task": "j1-t1", "message": "m1"}))
            .unwrap();
        let body = stored_body(&shared, "m1");
        assert_eq!(
            body,
            "use the V1 email provider — Task j1-t1 (job j1) is still open; this message \
             amends it and does not replace it. Objective: Wire the email provider. Spec: \
             /specs/j1.md. Outstanding criteria: 1) [ ] \"sends via V2\"; 2) [ ] \"docs\"."
        );
        let objective = body.find("Objective:").unwrap();
        let criteria = body.find("Outstanding criteria:").unwrap();
        assert!(body.starts_with(text) && objective < criteria, "{body}");
        assert!(
            !body.contains("tests green"),
            "checked item restated: {body}"
        );
        assert!(!crate::adapter::pty::has_control_chars(&body), "{body}");
        let message = shared.store.message("m1").unwrap().unwrap();
        assert_eq!(message.task_id.as_deref(), Some("j1-t1"));
        assert_eq!(message.reply_to.as_deref(), Some("pm"));
    }

    /// No task, or a terminal task: the body is the sender's text,
    /// byte for byte.
    #[test]
    fn untasked_and_terminal_task_sends_are_byte_identical() {
        let (_dir, shared) = task_bound_fixture(r#"1) [ ] "sends via V2""#);
        let text = "  plain chat — no task. ";
        shared
            .rpc_send(&json!({"alias": "w1", "text": text, "message": "m1"}))
            .unwrap();
        assert_eq!(stored_body(&shared, "m1"), text);
        shared.store.cancel_task("j1-t1", "test").unwrap();
        shared
            .rpc_send(&json!({"alias": "w1", "text": text, "task": "j1-t1", "message": "m2"}))
            .unwrap();
        assert_eq!(stored_body(&shared, "m2"), text);
    }

    /// QA N2: a worker's `--task` note to its PM is not steering —
    /// the PM is not the one on the hook — so it goes out unchanged.
    #[test]
    fn task_bound_send_to_a_non_assignee_is_byte_identical() {
        let (_dir, shared) = task_bound_fixture(r#"1) [ ] "sends via V2""#);
        let text = "PR is up";
        shared
            .rpc_send(&json!({"alias": "pm", "text": text, "task": "j1-t1", "message": "r1"}))
            .unwrap();
        assert_eq!(stored_body(&shared, "r1"), text);
    }

    /// QA N3: blank text bound to an open task is refused before it
    /// becomes a bare restatement; nothing is queued.
    #[test]
    fn blank_task_bound_send_is_refused() {
        let (_dir, shared) = task_bound_fixture(r#"1) [ ] "sends via V2""#);
        for (id, text) in [("e1", ""), ("e2", "   ")] {
            let err = shared
                .rpc_send(&json!({"alias": "w1", "text": text, "task": "j1-t1", "message": id}))
                .unwrap_err()
                .to_string();
            assert!(err.contains("Prompt must contain"), "{err}");
            assert!(shared.store.message(id).unwrap().is_none());
        }
        assert_eq!(shared.store.queued_count("w1").unwrap(), 0);
    }

    /// Criteria past the pty ceiling refuse the send, naming the
    /// ceiling and the spec file, and nothing is queued; long prose
    /// beside fitting criteria is cut instead.
    #[test]
    fn task_bound_send_past_the_pty_ceiling_refuses_and_queues_nothing() {
        let long = format!(r#"1) [ ] "{}""#, "c".repeat(4100));
        let (_dir, shared) = task_bound_fixture(&long);
        let err = shared
            .rpc_send(&json!({"alias": "w1", "text": "steer", "task": "j1-t1", "message": "m1"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("4000-char"), "{err}");
        assert!(err.contains("spec file /specs/j1.md"), "{err}");
        assert!(shared.store.message("m1").unwrap().is_none());
        assert_eq!(shared.store.queued_count("w1").unwrap(), 0);

        let criteria = "c".repeat(800);
        let (_dir, shared) = task_bound_fixture(&format!(r#"1) [ ] "{criteria}""#));
        shared
            .store
            .create_task(
                "j1",
                "j1-t2",
                Some(&"T".repeat(3000)),
                Some("w1"),
                None,
                Some(&format!(r#"1) [ ] "{criteria}""#)),
                None,
                None,
                None,
            )
            .unwrap();
        let text = "p".repeat(500);
        shared
            .rpc_send(&json!({"alias": "w1", "text": text, "task": "j1-t2", "message": "m2"}))
            .unwrap();
        let body = stored_body(&shared, "m2");
        assert!(
            body.len() <= crate::adapter::pty::MAX_BODY,
            "{} bytes",
            body.len()
        );
        assert!(body.starts_with(&text), "{body}");
        assert!(body.contains("T…. Spec:"), "{body}");
        assert!(
            body.ends_with(&format!("Outstanding criteria: 1) [ ] \"{criteria}\".")),
            "{body}"
        );
    }

    // ---------- CAD-132: provider WAL watch ----------

    /// A WAL-mode db whose writer connection stays open — closing the
    /// last connection auto-checkpoints, which would erase the fixture.
    fn wal_db(dir: &Path, name: &str) -> (PathBuf, rusqlite::Connection) {
        let db = dir.join(name);
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(
            "CREATE TABLE t(x BLOB);
             INSERT INTO t VALUES (randomblob(262144));",
        )
        .unwrap();
        (db, conn)
    }

    fn wal_size(db: &Path) -> u64 {
        crate::doctor::host::wal_sibling(db)
            .and_then(|w| std::fs::metadata(w).ok())
            .map(|m| m.len())
            .unwrap_or(0)
    }

    /// `quiet_secs = 0` makes a just-written WAL "quiet" — tests then
    /// exercise the checkpoint itself; the live daemon passes
    /// `WAL_QUIET_SECS`.
    fn pass(
        roots: &[crate::doctor::host::WalRoot],
        busy: &HashSet<String>,
        max_bytes: u64,
        shared: &Shared,
        watch: &mut WalWatch,
    ) {
        wal_pass(roots, busy, max_bytes, 0, false, &shared.store, watch);
    }

    /// Idle provider, WAL over the limit: PASSIVE+TRUNCATE frees the
    /// file and a `wal_checkpointed` event carries before/after bytes.
    #[test]
    fn wal_pass_checkpoints_idle_provider() {
        let (dir, shared) = shared();
        let root = dir.path().join("codex");
        std::fs::create_dir_all(&root).unwrap();
        let (db, _writer) = wal_db(&root, "state_1.sqlite");
        let before = wal_size(&db);
        assert!(before > 1, "fixture must produce a real WAL");
        let roots = [crate::doctor::host::WalRoot {
            provider: "codex",
            label: "codex store",
            root,
        }];
        let mut watch = WalWatch::default();
        pass(&roots, &HashSet::new(), 1, &shared, &mut watch);
        assert_eq!(wal_size(&db), 0, "TRUNCATE frees the wal");
        let events = shared.store.events_tail(DAEMON_ALIAS, 10).unwrap();
        let ev = events
            .iter()
            .find(|e| e.kind == "wal_checkpointed")
            .expect("wal_checkpointed event");
        assert_eq!(ev.payload["provider"], "codex");
        assert_eq!(ev.payload["store"], "codex store");
        assert_eq!(ev.payload["wal_bytes_before"], before);
        assert_eq!(ev.payload["wal_bytes_after"], 0);
    }

    /// A provider with a `submitting`/`running` turn is never
    /// checkpointed — the WAL keeps growing until the turn ends.
    /// The busy set here comes from the real store query, so the
    /// enqueue→submitting path is covered end to end.
    #[test]
    fn wal_pass_skips_busy_provider() {
        let (dir, shared) = shared();
        shared
            .store
            .register_agent(&NewAgent {
                alias: "op1",
                provider: "codex",
                endpoint_kind: "pty",
                role: "worker",
                cwd: dir.path().to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        shared
            .store
            .enqueue("op1", "do work", None, "m-wal-1", "test")
            .unwrap();
        let Take::Message(msg) = shared.store.take_queued("op1").unwrap() else {
            panic!("queued message must be taken");
        };
        shared.store.mark_running(&msg.id, "pty-1-wal").unwrap();
        // The alias has an actor — the daemon's owned set (CAD-250).
        let live: HashSet<String> = ["op1".to_string()].into();
        let busy = shared.store.busy_providers(&live, epoch_secs()).unwrap();
        assert!(busy.contains("codex"), "{busy:?}");

        let root = dir.path().join("codex");
        std::fs::create_dir_all(&root).unwrap();
        let (db, _writer) = wal_db(&root, "state_1.sqlite");
        let before = wal_size(&db);
        let roots = [crate::doctor::host::WalRoot {
            provider: "codex",
            label: "codex store",
            root,
        }];
        let mut watch = WalWatch::default();
        pass(&roots, &busy, 1, &shared, &mut watch);
        assert_eq!(wal_size(&db), before, "busy provider is untouched");
        assert!(shared
            .store
            .events_tail(DAEMON_ALIAS, 10)
            .unwrap()
            .iter()
            .all(|e| e.kind != "wal_checkpointed"));
        // Once the turn ends the next pass checkpoints.
        shared
            .store
            .finish(&msg, "completed", &json!({"status": "completed"}), None)
            .unwrap();
        let busy = shared.store.busy_providers(&live, epoch_secs()).unwrap();
        assert!(!busy.contains("codex"), "{busy:?}");
        pass(&roots, &busy, 1, &shared, &mut watch);
        assert_eq!(wal_size(&db), 0);
    }

    /// CAD-250: only a *live* turn defers a checkpoint. A delivered pty
    /// turn awaiting its report is busy while its actor lives and the
    /// report bound has not run out; the same row on an alias with no
    /// actor, or past its bound, is stale — the pass checkpoints anyway.
    #[test]
    fn wal_pass_ignores_stale_awaiting_report_rows() {
        let (dir, shared) = shared();
        shared
            .store
            .register_agent(&NewAgent {
                alias: "dv1",
                provider: "devin",
                endpoint_kind: "pty",
                role: "worker",
                cwd: dir.path().to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: Some(r#"{"report_timeout_secs": 3600}"#),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        shared
            .store
            .enqueue("dv1", "do work", None, "m-stale-1", "test")
            .unwrap();
        let Take::Message(msg) = shared.store.take_queued("dv1").unwrap() else {
            panic!("queued message must be taken");
        };
        shared.store.mark_running(&msg.id, "pty-1-stale").unwrap();
        let msg = shared.store.message(&msg.id).unwrap().unwrap();
        shared.store.mark_submitted(&msg).unwrap();
        let msg = shared.store.message(&msg.id).unwrap().unwrap();
        assert!(msg.awaiting_report(), "{msg:?}");
        let delivered = msg.started.unwrap();

        let live: HashSet<String> = ["dv1".to_string()].into();
        let none = HashSet::new();
        let within = delivered + 60.0;
        let past = delivered + 3601.0;
        // Live actor, inside the bound: busy.
        let busy = shared.store.busy_providers(&live, within).unwrap();
        assert!(busy.contains("devin"), "{busy:?}");
        // No actor owns the alias: the row is stale, not busy.
        let busy = shared.store.busy_providers(&none, within).unwrap();
        assert!(!busy.contains("devin"), "{busy:?}");
        // Past the report bound: stale even with an actor.
        let busy = shared.store.busy_providers(&live, past).unwrap();
        assert!(!busy.contains("devin"), "{busy:?}");

        // And the checkpoint decision follows: a stale row never defers.
        let root = dir.path().join("devin");
        std::fs::create_dir_all(&root).unwrap();
        let (db, _writer) = wal_db(&root, "sessions.db");
        assert!(wal_size(&db) > 0);
        let roots = [crate::doctor::host::WalRoot {
            provider: "devin",
            label: "devin sessions",
            root,
        }];
        let mut watch = WalWatch::default();
        let live_busy = shared.store.busy_providers(&live, within).unwrap();
        pass(&roots, &live_busy, 1, &shared, &mut watch);
        assert!(wal_size(&db) > 0, "a live turn defers the checkpoint");
        let stale_busy = shared.store.busy_providers(&live, past).unwrap();
        pass(&roots, &stale_busy, 1, &shared, &mut watch);
        assert_eq!(wal_size(&db), 0, "an overdue row does not defer it");
    }

    /// A reader mid-snapshot makes TRUNCATE return busy — the pass
    /// defers quietly and the next tick (after the reader) completes.
    #[test]
    fn wal_pass_busy_reader_retries_next_tick() {
        let (dir, shared) = shared();
        let root = dir.path().join("devin");
        std::fs::create_dir_all(&root).unwrap();
        let (db, _writer) = wal_db(&root, "sessions.db");
        let before = wal_size(&db);
        // A second connection holding an open read transaction pins
        // the WAL — TRUNCATE reports busy.
        let reader = rusqlite::Connection::open(&db).unwrap();
        reader
            .execute_batch("BEGIN; SELECT count(*) FROM t;")
            .unwrap();
        let roots = [crate::doctor::host::WalRoot {
            provider: "devin",
            label: "devin store",
            root,
        }];
        let mut watch = WalWatch::default();
        pass(&roots, &HashSet::new(), 1, &shared, &mut watch);
        assert_eq!(wal_size(&db), before, "busy wal is left alone");
        assert!(shared
            .store
            .events_tail(DAEMON_ALIAS, 10)
            .unwrap()
            .iter()
            .all(|e| e.kind != "wal_checkpointed"));
        // Reader finishes — the retry completes the checkpoint.
        reader.execute_batch("END").unwrap();
        drop(reader);
        pass(&roots, &HashSet::new(), 1, &shared, &mut watch);
        assert_eq!(wal_size(&db), 0);
    }

    /// Under the limit a WAL is left alone — no checkpoint, no event.
    #[test]
    fn wal_pass_ignores_small_wals() {
        let (dir, shared) = shared();
        let root = dir.path().join("claude");
        std::fs::create_dir_all(&root).unwrap();
        let (db, _writer) = wal_db(&root, "sess.db");
        let before = wal_size(&db);
        let roots = [crate::doctor::host::WalRoot {
            provider: "claude",
            label: "claude store",
            root,
        }];
        let mut watch = WalWatch::default();
        pass(&roots, &HashSet::new(), u64::MAX, &shared, &mut watch);
        assert_eq!(wal_size(&db), before);
        assert!(shared
            .store
            .events_tail(DAEMON_ALIAS, 10)
            .unwrap()
            .is_empty());
    }

    /// A WAL written moments ago means a writer cadence cannot see is
    /// live — the quiet-window defers, no matter what busy_providers
    /// says.
    #[test]
    fn wal_pass_defers_to_quiet_window() {
        let (dir, shared) = shared();
        let root = dir.path().join("devin");
        std::fs::create_dir_all(&root).unwrap();
        let (db, _writer) = wal_db(&root, "sessions.db");
        let before = wal_size(&db);
        let roots = [crate::doctor::host::WalRoot {
            provider: "devin",
            label: "devin store",
            root,
        }];
        let mut watch = WalWatch::default();
        // quiet_secs=60, wal mtime=now → deferred, untouched, no event.
        wal_pass(
            &roots,
            &HashSet::new(),
            1,
            60,
            false,
            &shared.store,
            &mut watch,
        );
        assert_eq!(wal_size(&db), before);
        assert!(shared
            .store
            .events_tail(DAEMON_ALIAS, 10)
            .unwrap()
            .iter()
            .all(|e| e.kind != "wal_checkpointed"));
    }

    /// An orphan `foo.db-wal` must never create `foo.db` — the
    /// checkpoint connection opens READ_WRITE without CREATE.
    #[test]
    fn wal_pass_never_creates_a_db() {
        let (dir, shared) = shared();
        let root = dir.path().join("codex");
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("ghost.sqlite");
        std::fs::write(root.join("ghost.sqlite-wal"), vec![0u8; 4096]).unwrap();
        let roots = [crate::doctor::host::WalRoot {
            provider: "codex",
            label: "codex store",
            root,
        }];
        let mut watch = WalWatch::default();
        pass(&roots, &HashSet::new(), 1, &shared, &mut watch);
        assert!(!db.exists(), "orphan -wal must not conjure a db");
    }

    /// A `journal_mode=DELETE` db with a stale leftover `-wal`: sqlite's
    /// open-time recovery sees the wal, prunes it, and the pragma
    /// returns (0,0,0) — the file IS freed, so Done+event is honest.
    /// (The (0,-1,-1) non-WAL result is only reachable if the wal
    /// vanished between scan and open — the guard stays for that.)
    #[test]
    fn wal_pass_stale_wal_is_reclaimed() {
        let (dir, shared) = shared();
        let root = dir.path().join("devin");
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("sessions.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1);")
            .unwrap();
        drop(conn);
        std::fs::write(root.join("sessions.db-wal"), vec![0u8; 8192]).unwrap();
        let roots = [crate::doctor::host::WalRoot {
            provider: "devin",
            label: "devin store",
            root,
        }];
        let mut watch = WalWatch::default();
        pass(&roots, &HashSet::new(), 1, &shared, &mut watch);
        assert_eq!(wal_size(&db), 0, "stale wal is reclaimed");
        let events = shared.store.events_tail(DAEMON_ALIAS, 10).unwrap();
        let ev = events
            .iter()
            .find(|e| e.kind == "wal_checkpointed")
            .expect("stale wal reclaim is a real freeing event");
        assert_eq!(ev.payload["wal_bytes_before"], 8192);
        assert_eq!(ev.payload["wal_bytes_after"], 0);
    }

    /// A symlinked WAL is never ours to write through — the guard
    /// leaves it alone even when the target is a real hot WAL.
    #[test]
    fn wal_pass_skips_symlinked_wal() {
        let (dir, shared) = shared();
        let root = dir.path().join("codex");
        std::fs::create_dir_all(&root).unwrap();
        let (db, _writer) = wal_db(&root, "state_1.sqlite");
        let real_wal = crate::doctor::host::wal_sibling(&db).unwrap();
        let staged = dir.path().join("real-wal-copy");
        std::fs::rename(&real_wal, &staged).unwrap();
        std::os::unix::fs::symlink(&staged, &real_wal).unwrap();
        let roots = [crate::doctor::host::WalRoot {
            provider: "codex",
            label: "codex store",
            root,
        }];
        let mut watch = WalWatch::default();
        pass(&roots, &HashSet::new(), 1, &shared, &mut watch);
        assert!(
            std::fs::symlink_metadata(&real_wal).unwrap().is_symlink(),
            "symlink wal must be left in place"
        );
        assert!(shared
            .store
            .events_tail(DAEMON_ALIAS, 10)
            .unwrap()
            .iter()
            .all(|e| e.kind != "wal_checkpointed"));
    }

    /// `wal_dry_run` records `wal_checkpoint_pending` once per db per
    /// crossing — a second pass over the same WAL does not re-emit.
    #[test]
    fn wal_pass_dry_run_events_once() {
        let (dir, shared) = shared();
        let root = dir.path().join("devin");
        std::fs::create_dir_all(&root).unwrap();
        let (db, _writer) = wal_db(&root, "sessions.db");
        let before = wal_size(&db);
        let roots = [crate::doctor::host::WalRoot {
            provider: "devin",
            label: "devin store",
            root,
        }];
        let mut watch = WalWatch::default();
        wal_pass(
            &roots,
            &HashSet::new(),
            1,
            0,
            true,
            &shared.store,
            &mut watch,
        );
        wal_pass(
            &roots,
            &HashSet::new(),
            1,
            0,
            true,
            &shared.store,
            &mut watch,
        );
        assert_eq!(wal_size(&db), before, "dry run never writes");
        let events = shared.store.events_tail(DAEMON_ALIAS, 10).unwrap();
        let pending: Vec<_> = events
            .iter()
            .filter(|e| e.kind == "wal_checkpoint_pending")
            .collect();
        assert_eq!(pending.len(), 1, "one pending event, not one per tick");
        assert_eq!(pending[0].payload["wal_bytes_before"], before);
        // Back under the limit clears the dedupe — a later crossing
        // reports again.
        checkpoint_wal(&db);
        wal_pass(
            &roots,
            &HashSet::new(),
            1,
            0,
            true,
            &shared.store,
            &mut watch,
        );
        let n: usize = shared
            .store
            .events_tail(DAEMON_ALIAS, 10)
            .unwrap()
            .iter()
            .filter(|e| e.kind == "wal_checkpoint_pending")
            .count();
        assert_eq!(n, 1, "still one — under the limit emitted nothing");
    }

    /// CAD-310: a sandbox profile forces the watcher observe-only even
    /// with `wal_dry_run` off — intent recorded, the WAL never touched.
    #[test]
    fn wal_pass_under_a_sandbox_profile_is_observe_only() {
        assert!(!wal_observe_only(false, None));
        assert!(wal_observe_only(true, None));
        let (dir, shared) = shared();
        let root = dir.path().join("devin");
        std::fs::create_dir_all(&root).unwrap();
        let (db, _writer) = wal_db(&root, "sessions.db");
        let before = wal_size(&db);
        let roots = [crate::doctor::host::WalRoot {
            provider: "devin",
            label: "devin store",
            root,
        }];
        let mut watch = WalWatch::default();
        wal_pass(
            &roots,
            &HashSet::new(),
            1,
            0,
            wal_observe_only(false, Some("x")),
            &shared.store,
            &mut watch,
        );
        assert_eq!(wal_size(&db), before, "a sandbox never checkpoints");
        let events = shared.store.events_tail(DAEMON_ALIAS, 10).unwrap();
        assert!(events.iter().any(|e| e.kind == "wal_checkpoint_pending"));
        assert!(events.iter().all(|e| e.kind != "wal_checkpointed"));
    }

    /// The daemon stream is bounded: more than DAEMON_EVENTS_KEEP
    /// events leaves exactly `keep` newest rows.
    #[test]
    fn daemon_stream_is_pruned() {
        let (_dir, shared) = shared();
        for i in 0..5 {
            let _ = shared
                .store
                .event_public(DAEMON_ALIAS, "wal_checkpointed", json!({"i": i}));
        }
        shared.store.prune_stream(DAEMON_ALIAS, 3).unwrap();
        let events = shared.store.events_tail(DAEMON_ALIAS, 10).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events.last().unwrap().payload["i"], 4);
        assert_eq!(events.first().unwrap().payload["i"], 2);
    }

    /// CAD-102 r5: a caller whose `/proc` ancestry cannot be walked —
    /// here a peer pid that no longer exists — must be REFUSED while
    /// the target pane is alive. The r4 guard checked liveness with
    /// `read_link("/proc/<pid>")`, which always fails EINVAL on a
    /// directory, so the refusal never fired and the answer proceeded
    /// stamped `unknown`.
    #[test]
    fn broken_walk_with_live_target_pane_is_refused() {
        let (dir, shared) = shared();
        register(&shared, dir.path(), "tgt");
        // Give the agent a pty endpoint with a LIVE pane pid — this
        // test process — so `pty_endpoint_facts` finds the target.
        let conn = rusqlite::Connection::open(dir.path().join("cadence.sqlite3")).unwrap();
        conn.execute(
            "UPDATE agents SET endpoint_kind='pty', generation=1, pid=?1 \
             WHERE alias='tgt'",
            rusqlite::params![std::process::id() as i64],
        )
        .unwrap();
        drop(conn);
        // A dead peer pid: the first /proc read fails → chain is None.
        let dead = u32::MAX - 42;
        assert!(std::fs::metadata(format!("/proc/{dead}")).is_err());
        let err = shared.derived_caller("tgt", dead, "answer").unwrap_err();
        assert!(err.to_string().contains("could not be walked"), "{err}");
    }

    /// The unmatched-caller tail: a broken walk refuses only while the
    /// target lives; a clean walk stamps `operator` solely on positive
    /// terminal evidence — a detached caller is `unknown`, never
    /// `operator`.
    #[test]
    fn unmatched_caller_is_fail_closed() {
        // Broken walk: refused while the target pane lives.
        assert!(unmatched_caller(false, true, false, "answer").is_err());
        assert!(unmatched_caller(false, true, true, "answer").is_err());
        // …and `unknown` once it is gone.
        assert_eq!(
            unmatched_caller(false, false, false, "answer").unwrap(),
            ("unknown".to_string(), "unknown")
        );
        // Clean walk + foreign pty → operator.
        assert_eq!(
            unmatched_caller(true, true, true, "answer").unwrap(),
            ("operator".to_string(), "operator")
        );
        // Clean walk + no terminal (a full setsid detach) → unknown.
        assert_eq!(
            unmatched_caller(true, true, false, "answer").unwrap(),
            ("unknown".to_string(), "unknown")
        );
        assert_eq!(
            unmatched_caller(true, false, false, "answer").unwrap(),
            ("unknown".to_string(), "unknown")
        );
    }

    // ---- CAD-162: turn-token staleness keyed on the endpoint's scheme ----

    const GEN: &str = "0123456789abcdef0123456789abcdef";
    const OLD_GEN: &str = "fedcba9876543210fedcba9876543210";

    /// An agent of `(provider, kind)` whose live endpoint is at
    /// `generation`, holding one `running` message minted `token`.
    /// Returns the message id. No actor runs — the RPC is judged
    /// against the store alone, exactly as the daemon does.
    fn running_turn(
        shared: &Arc<Shared>,
        dir: &Path,
        alias: &str,
        (provider, kind): (&str, &str),
        generation: Option<&str>,
        token: &str,
    ) -> String {
        shared
            .store
            .register_agent(&NewAgent {
                alias,
                provider,
                endpoint_kind: kind,
                role: "worker",
                cwd: dir.to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        shared
            .store
            .set_identity(
                alias,
                &adapter::Identity {
                    thread_id: format!("thread-{alias}"),
                    session_id: format!("session-{alias}"),
                    model: None,
                    effort: None,
                    pid: 0,
                    endpoint: None,
                    generation: generation.map(str::to_string),
                    attach: None,
                },
            )
            .unwrap();
        let id = format!("m-{alias}");
        shared
            .store
            .enqueue(alias, "work", None, &id, "test")
            .unwrap();
        let Take::Message(msg) = shared.store.take_queued(alias).unwrap() else {
            panic!("{alias}: queued message must be taken");
        };
        shared.store.mark_running(&msg.id, token).unwrap();
        id
    }

    fn report(shared: &Arc<Shared>, id: &str, token: &str, kind: &str) -> Result<Value> {
        shared.dispatch(
            "message_report",
            &json!({"message": id, "token": token, "kind": kind, "text": "reported"}),
            0,
        )
    }

    /// A refused report changed nothing: the message is still `running`
    /// under the same token and carries no ack or result.
    fn assert_refused_untouched(shared: &Arc<Shared>, id: &str, token: &str, what: &str) {
        for kind in ["ack", "result"] {
            let err = report(shared, id, token, kind)
                .expect_err(&format!("{what}: {kind} accepted as current"))
                .to_string();
            assert!(err.contains("stale endpoint generation"), "{what}: {err}");
        }
        let m = shared.store.message(id).unwrap().unwrap();
        assert_eq!(m.state, "running", "{what}");
        assert_eq!(m.turn_id.as_deref(), Some(token), "{what}");
        assert!(m.result.is_none(), "{what}: {:?}", m.result);
    }

    /// Endpoints whose turn tokens embed their generation, with each
    /// one's own token shape and another kind's shape.
    fn schemed_endpoints() -> Vec<((&'static str, &'static str), &'static str, &'static str)> {
        vec![
            (("claude", "pty"), "pty", "claude"),
            (("devin", "pty"), "pty", "claude"),
            (("cursor", "pty"), "pty", "claude"),
            (("tui-stub", "pty"), "pty", "claude"),
            (("claude", "managed"), "claude", "pty"),
        ]
    }

    /// CAD-162 acceptance 2/3: on every endpoint kind with a scheme, the
    /// token minted under the live generation is current; the same
    /// shape from an EARLIER generation, another endpoint kind's token
    /// carrying the LIVE generation, and any token while the generation
    /// is unproven (cleared at store open) are each refused and change
    /// nothing.
    #[test]
    fn message_report_judges_tokens_by_the_endpoints_own_scheme() {
        let (dir, shared) = shared();
        for (i, (pair, own, foreign)) in schemed_endpoints().into_iter().enumerate() {
            let what = format!("{}/{}", pair.0, pair.1);
            // Current: accepted, and the ack keeps the message running.
            let token = format!("{own}-{GEN}-nonce{i}");
            let id = running_turn(
                &shared,
                dir.path(),
                &format!("cur{i}"),
                pair,
                Some(GEN),
                &token,
            );
            report(&shared, &id, &token, "ack")
                .unwrap_or_else(|e| panic!("{what}: current token refused: {e}"));
            let m = shared.store.message(&id).unwrap().unwrap();
            assert_eq!(m.state, "running", "{what}");
            assert_eq!(
                m.result.as_ref().unwrap()["ack"]["text"],
                "reported",
                "{what}"
            );

            // Earlier generation, same scheme.
            let stale = format!("{own}-{OLD_GEN}-nonce{i}");
            let id = running_turn(
                &shared,
                dir.path(),
                &format!("old{i}"),
                pair,
                Some(GEN),
                &stale,
            );
            assert_refused_untouched(&shared, &id, &stale, &format!("{what} stale"));

            // Another endpoint kind's token under the LIVE generation.
            let cross = format!("{foreign}-{GEN}-nonce{i}");
            let id = running_turn(
                &shared,
                dir.path(),
                &format!("xk{i}"),
                pair,
                Some(GEN),
                &cross,
            );
            assert_refused_untouched(&shared, &id, &cross, &format!("{what} cross-kind"));

            // Generation not proven: nothing is current.
            let id = running_turn(&shared, dir.path(), &format!("ng{i}"), pair, None, &token);
            assert_refused_untouched(&shared, &id, &token, &format!("{what} no generation"));
        }
    }

    /// CAD-162 acceptance 2/3, fail closed: an endpoint whose token
    /// carries nothing cadence can check against its generation (codex:
    /// provider turn ids; devin cloud: the message id; fake; mailbox)
    /// accepts NO report — not its own token, not a pty- or claude-
    /// shaped token forged under its live generation.
    #[test]
    fn message_report_refuses_every_token_on_endpoints_without_a_scheme() {
        let (dir, shared) = shared();
        let pairs = [
            ("codex", "managed"),
            ("codex", "managed-ws"),
            ("devin", "cloud"),
            ("fake", "fake"),
            ("inbox", "inbox"),
        ];
        for (i, pair) in pairs.into_iter().enumerate() {
            let alias = format!("ns{i}");
            let id = format!("m-{alias}");
            let tokens = [
                // What each adapter mints today: codex a provider turn
                // id, devin cloud the message id, fake a counter.
                "turn-0001".to_string(),
                id.clone(),
                "fake-turn-1".to_string(),
                // Forgeries under the live generation.
                format!("pty-{GEN}-n"),
                format!("claude-{GEN}-n"),
                format!("{}-{GEN}-n", pair.0),
                format!("{}-{GEN}-n", pair.1),
            ];
            let first = running_turn(&shared, dir.path(), &alias, pair, Some(GEN), &tokens[0]);
            assert_eq!(first, id);
            for token in &tokens {
                shared.store.mark_running(&id, token).unwrap();
                assert_refused_untouched(
                    &shared,
                    &id,
                    token,
                    &format!("{}/{} {token}", pair.0, pair.1),
                );
            }
        }
    }

    /// CAD-162 acceptance 4: an ack on a managed endpoint keeps the
    /// message running without the pty `submitted` marker (so it is
    /// never `awaiting_report`), a reported `result` is refused — the
    /// adapter's turn result is the one writer of the outcome — and that
    /// later turn result completes the message.
    #[test]
    fn managed_ack_keeps_running_and_the_turn_result_completes() {
        let (dir, shared) = shared();
        let token = format!("claude-{GEN}-n");
        let id = running_turn(
            &shared,
            dir.path(),
            "mc1",
            ("claude", "managed"),
            Some(GEN),
            &token,
        );
        report(&shared, &id, &token, "ack").unwrap();
        let m = shared.store.message(&id).unwrap().unwrap();
        assert_eq!(m.state, "running");
        let result = m.result.clone().unwrap();
        assert_eq!(result["status"], "acknowledged", "{result}");
        assert_eq!(result["ack"]["text"], "reported", "{result}");
        assert!(!m.awaiting_report(), "a managed ack owes no report");
        assert!(m.holds_turn());

        let err = report(&shared, &id, &token, "result")
            .unwrap_err()
            .to_string();
        assert!(err.contains("report `ack` only"), "{err}");
        let m = shared.store.message(&id).unwrap().unwrap();
        assert_eq!(m.state, "running");
        assert_eq!(m.result.as_ref().unwrap()["status"], "acknowledged");

        shared
            .complete(
                &m,
                TurnResult {
                    turn_id: token.clone(),
                    status: "completed".into(),
                    text: "done".into(),
                    stop_reason: Some("end_turn".into()),
                    error: None,
                },
            )
            .unwrap();
        let m = shared.store.message(&id).unwrap().unwrap();
        assert_eq!(m.state, "completed");
        assert_eq!(m.result.as_ref().unwrap()["text"], "done");
        // An ack after completion is refused like on pty.
        assert!(report(&shared, &id, &token, "ack").is_err());
    }
}

#[cfg(test)]
mod unknown_fence_guidance {
    use super::{
        format_unknown_fence, ordinary_terminal_kickoff_attention, unknown_fence_detail,
        unknown_kickoff_attention, UNKNOWN_GENERIC_REASON,
    };

    const RENDER_MISS: &str = "submission accepted but never rendered on the endpoint";

    #[test]
    fn render_miss_reason_survives_restamp() {
        let once = format_unknown_fence(RENDER_MISS);
        let twice = format_unknown_fence(unknown_fence_detail(&once));
        assert_eq!(once, twice);
        assert!(once.starts_with(RENDER_MISS), "{once}");
        assert!(once.contains("does not prove"), "{once}");
        assert!(once.contains("--no-resume"), "{once}");
        assert!(!once.contains("then `cadence agent resume"), "{once}");
        assert!(!once.contains("job dispatch"), "{once}");
        assert!(!once.contains("cadence agent unfence"), "{once}");
        assert_ne!(
            unknown_fence_detail(&once),
            UNKNOWN_GENERIC_REASON,
            "{once}"
        );
    }

    #[test]
    fn old_reconcile_marker_keeps_provider_account() {
        let old = "Connection lost during turn; provider outcome is unknown — reconcile: \
                   `cadence agent unfence w1 --status interrupted`, then \
                   `cadence agent resume w1`";
        let detail = unknown_fence_detail(old);
        assert_eq!(
            detail,
            "Connection lost during turn; provider outcome is unknown"
        );
        let restamped = format_unknown_fence(detail);
        assert!(
            restamped.contains("Connection lost during turn"),
            "{restamped}"
        );
        assert!(restamped.contains("does not prove"), "{restamped}");
        assert!(
            !restamped.contains("then `cadence agent resume"),
            "{restamped}"
        );
    }

    #[test]
    fn empty_detail_stays_generic_without_a_command_chain() {
        let text = format_unknown_fence("  \n");
        assert!(text.starts_with(UNKNOWN_GENERIC_REASON), "{text}");
        assert!(text.contains(" — inspect:"), "{text}");
        assert!(!text.contains("then `cadence agent resume"), "{text}");
    }

    #[test]
    fn detail_is_bounded_and_secret_shaped_argv_is_scrubbed() {
        let noisy = format!(
            "before\nsecret --api-key=supersecretvalue {}",
            "x".repeat(600)
        );
        let text = format_unknown_fence(&noisy);
        let detail = unknown_fence_detail(&text);
        assert!(!detail.contains('\n'), "{detail}");
        assert!(!detail.contains("supersecretvalue"), "{detail}");
        assert!(detail.contains("[REDACTED]"), "{detail}");
        assert!(detail.chars().count() <= 513, "{}", detail.chars().count());
        assert!(!text.contains("before_tail"), "{text}");
    }

    #[test]
    fn unknown_kickoff_does_not_name_dispatch_ordinary_terminal_does() {
        let unknown = unknown_kickoff_attention("m1", "w1");
        assert!(unknown.contains("does not prove"), "{unknown}");
        assert!(unknown.contains("continuation is safe"), "{unknown}");
        assert!(!unknown.contains("job dispatch"), "{unknown}");
        assert!(!unknown.contains("cadence agent unfence"), "{unknown}");
        let failed = ordinary_terminal_kickoff_attention("m1", "failed", "task-a", 3);
        assert!(
            failed.contains("`cadence job dispatch task-a` starts revision 3"),
            "{failed}"
        );
        let interrupted = ordinary_terminal_kickoff_attention("m1", "interrupted", "task-a", 2);
        assert!(
            interrupted.contains("`cadence job dispatch task-a` starts revision 2"),
            "{interrupted}"
        );
    }
}

#[cfg(test)]
mod agent_gc_timer {
    use super::*;
    use crate::store::NewAgent;

    const DAY: f64 = 86_400.0;

    fn pinned(dir: &Path, setting: AgentGcSetting) -> Arc<Shared> {
        let opts = ServeOptions {
            agent_gc: Some(setting),
            ..ServeOptions::default()
        };
        Shared::new(dir, &opts).unwrap()
    }

    /// Register a stopped, disabled fake agent last updated `age_days`
    /// ago — pre-aged through a side connection, no clock or sleep.
    fn stopped_row(shared: &Shared, dir: &Path, alias: &str, age_days: f64) {
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
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        rusqlite::Connection::open(dir.join("cadence.sqlite3"))
            .unwrap()
            .execute(
                "UPDATE agents SET state='stopped', enabled=0, endpoint=NULL,
                 updated=? WHERE alias=?",
                rusqlite::params![epoch_secs() - age_days * DAY, alias],
            )
            .unwrap();
    }

    fn removed_events(shared: &Shared) -> Vec<Value> {
        shared
            .store
            .events(Store::DAEMON_STREAM, 0, 100)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "agent_gc_removed")
            .map(|e| e.payload)
            .collect()
    }

    #[test]
    fn setting_is_off_unless_host_sets_it_and_floors_at_seven_days() {
        let dir = tempfile::tempdir().unwrap();
        let pm = dir.path();
        // No pm.yaml, and a [host] table without the key: off.
        assert_eq!(
            AgentGcSetting::from_pm_dir(Some(pm)),
            AgentGcSetting::default()
        );
        assert_eq!(AgentGcSetting::from_pm_dir(None).effective_secs(), None);
        std::fs::write(
            pm.join("pm.yaml"),
            "schema: 1\nhost:\n  wal_max_bytes: 4096\n",
        )
        .unwrap();
        let off = AgentGcSetting::from_pm_dir(Some(pm));
        assert_eq!(off.effective_secs(), None);
        assert_eq!(off.warning(), None);
        // Set at or above the floor: used verbatim, no warning.
        std::fs::write(
            pm.join("pm.yaml"),
            "schema: 1\nhost:\n  agent_gc_older_than_secs: 1209600\n",
        )
        .unwrap();
        let on = AgentGcSetting::from_pm_dir(Some(pm));
        assert_eq!(on.effective_secs(), Some(1_209_600));
        assert_eq!(on.warning(), None);
        // Below the floor: raised to 7 days, with a warning.
        std::fs::write(
            pm.join("pm.yaml"),
            "schema: 1\nhost:\n  agent_gc_older_than_secs: 3600\n",
        )
        .unwrap();
        let low = AgentGcSetting::from_pm_dir(Some(pm));
        assert_eq!(low.configured_secs, Some(3600));
        assert_eq!(low.effective_secs(), Some(AGENT_GC_FLOOR_SECS));
        assert!(low.warning().unwrap().contains("below the 7-day floor"));
        // An unusable [host] table fails closed: off, with the error.
        std::fs::write(
            pm.join("pm.yaml"),
            "schema: 1\nhost:\n  agent_gc_older_than_secs: \"7d\"\n",
        )
        .unwrap();
        let bad = AgentGcSetting::from_pm_dir(Some(pm));
        assert_eq!(bad.effective_secs(), None);
        assert!(
            bad.warning().unwrap().contains("agent_gc_older_than_secs"),
            "{bad:?}"
        );
    }

    #[test]
    fn unconfigured_tick_never_removes() {
        let dir = tempfile::tempdir().unwrap();
        let shared = pinned(dir.path(), AgentGcSetting::default());
        stopped_row(&shared, dir.path(), "ancient", 365.0);
        shared.agent_gc_tick();
        assert!(shared.store.agent_opt("ancient").unwrap().is_some());
        assert!(removed_events(&shared).is_empty());
        let status = shared.agent_gc.status();
        assert_eq!(status["enabled"], false, "{status}");
        assert!(status["last_check_at"].is_f64(), "{status}");
        assert!(status["last_sweep_at"].is_null(), "{status}");
    }

    #[test]
    fn configured_tick_sweeps_eligible_rows_at_most_hourly() {
        let dir = tempfile::tempdir().unwrap();
        // One hour configured: below the floor, so the timer uses 7 days.
        let shared = pinned(dir.path(), AgentGcSetting::older_than(3600));
        stopped_row(&shared, dir.path(), "old", 30.0);
        stopped_row(&shared, dir.path(), "under-floor", 2.0);
        stopped_row(&shared, dir.path(), "unknown", 30.0);
        shared
            .store
            .enqueue("unknown", "work", None, "m-u", "user")
            .unwrap();
        rusqlite::Connection::open(dir.path().join("cadence.sqlite3"))
            .unwrap()
            .execute("UPDATE messages SET state='unknown' WHERE id='m-u'", [])
            .unwrap();

        shared.agent_gc_tick();
        assert!(shared.store.agent_opt("old").unwrap().is_none());
        assert!(shared.store.agent_opt("under-floor").unwrap().is_some());
        assert!(shared.store.agent_opt("unknown").unwrap().is_some());
        let events = removed_events(&shared);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["alias"], "old");
        assert_eq!(events[0]["older_than_secs"], AGENT_GC_FLOOR_SECS as f64);
        let status = shared.agent_gc.status();
        assert_eq!(status["enabled"], true, "{status}");
        assert_eq!(status["older_than_secs"], AGENT_GC_FLOOR_SECS, "{status}");
        assert_eq!(status["configured_secs"], 3600, "{status}");
        assert!(status["warning"].as_str().unwrap().contains("floor"));
        assert_eq!(status["last_removed"], 1, "{status}");
        assert!(status["note"]
            .as_str()
            .unwrap()
            .contains("frees no memory and no disk"));

        // A fresh eligible row within the hour waits: a forced re-check
        // reads the setting but does not sweep again.
        stopped_row(&shared, dir.path(), "later", 30.0);
        shared.agent_gc.state.lock().unwrap().next_check = None;
        shared.agent_gc_tick();
        assert!(shared.store.agent_opt("later").unwrap().is_some());
        // An hour after the last sweep, the next tick takes it.
        {
            let mut st = shared.agent_gc.state.lock().unwrap();
            st.next_check = None;
            st.last_sweep = Instant::now().checked_sub(AGENT_GC_EVERY);
        }
        shared.agent_gc_tick();
        assert!(shared.store.agent_opt("later").unwrap().is_none());
        assert_eq!(removed_events(&shared).len(), 2);
    }
}

#[cfg(test)]
mod auto_stop_timer {
    use super::*;
    use crate::store::NewAgent;

    const HOUR: f64 = 3600.0;

    fn pinned(dir: &Path, setting: AutoStopSetting) -> Arc<Shared> {
        let opts = ServeOptions {
            auto_stop: Some(setting),
            ..ServeOptions::default()
        };
        Shared::new(dir, &opts).unwrap()
    }

    /// Register `alias` and make its row look like a live, idle actor
    /// with a saved thread — no actor runs; the verdict reads rows.
    fn idle_row(
        shared: &Shared,
        dir: &Path,
        alias: &str,
        provider: &str,
        kind: &str,
        role: &str,
        params: Value,
    ) -> Agent {
        let params = params.to_string();
        shared
            .store
            .register_agent(&NewAgent {
                alias,
                provider,
                endpoint_kind: kind,
                role,
                cwd: dir.to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: Some(&params),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        rusqlite::Connection::open(dir.join("cadence.sqlite3"))
            .unwrap()
            .execute(
                "UPDATE agents SET state='idle', enabled=1, thread_id=? WHERE alias=?",
                rusqlite::params![format!("t-{alias}"), alias],
            )
            .unwrap();
        shared.store.agent(alias).unwrap()
    }

    fn worker(shared: &Shared, dir: &Path, alias: &str, params: Value) -> Agent {
        let mut p = json!({"upstream": "pm"});
        if let (Some(p), Some(extra)) = (p.as_object_mut(), params.as_object()) {
            p.extend(extra.clone());
        }
        idle_row(shared, dir, alias, "fake", "fake", "worker", p)
    }

    fn verdict(
        shared: &Shared,
        setting: &AutoStopSetting,
        agent: &Agent,
        at: f64,
    ) -> AutoStopVerdict {
        let agents = shared.store.agents().unwrap();
        let activity = shared
            .store
            .auto_stop_activity(AUTO_STOP_PASSIVE_KINDS)
            .unwrap();
        let agent = shared.store.agent(&agent.alias).unwrap();
        shared.auto_stop_verdict(
            &agent,
            setting,
            at,
            &upstream_roots(&agents),
            activity.get(&agent.alias),
        )
    }

    fn kept(v: &AutoStopVerdict) -> &str {
        match v {
            AutoStopVerdict::Keep(reason) => reason,
            other => panic!("expected keep, got {other:?}"),
        }
    }

    #[test]
    fn setting_defaults_on_at_an_hour_with_provider_override_and_floor() {
        let dir = tempfile::tempdir().unwrap();
        let pm = dir.path();
        // No pm.yaml / no key: ON at the built-in hour, no warning.
        let on = AutoStopSetting::from_pm_dir(Some(pm));
        assert_eq!(on, AutoStopSetting::default());
        assert_eq!(on.default_bound(), Some(AUTO_STOP_DEFAULT_SECS));
        assert_eq!(on.warning(), None);
        assert_eq!(
            AutoStopSetting::from_pm_dir(None).default_bound(),
            Some(3600)
        );
        std::fs::write(
            pm.join("pm.yaml"),
            "schema: 1\nhost:\n  auto_stop_idle_secs: 0\n  \
             auto_stop_idle_secs_by_provider:\n    claude: 7200\n    codex: 60\n",
        )
        .unwrap();
        let s = AutoStopSetting::from_pm_dir(Some(pm));
        assert_eq!(s.default_bound(), None, "0 turns the host default off");
        assert_eq!(s.by_provider.get("claude"), Some(&7200));
        let w = s.warning().unwrap();
        assert!(
            w.contains("auto_stop_idle_secs_by_provider.codex 60"),
            "{w}"
        );
        assert!(w.contains("600s floor"), "{w}");
        // An unusable [host] table stops nothing, and says why.
        std::fs::write(
            pm.join("pm.yaml"),
            "schema: 1\nhost:\n  auto_stop_idle_secs: \"1h\"\n",
        )
        .unwrap();
        let bad = AutoStopSetting::from_pm_dir(Some(pm));
        assert_eq!(bad.default_bound(), None);
        assert!(
            bad.warning().unwrap().contains("idle auto-stop off"),
            "{bad:?}"
        );
    }

    #[test]
    fn bound_precedence_agent_then_provider_then_host() {
        let dir = tempfile::tempdir().unwrap();
        let mut setting = AutoStopSetting::idle_after(5400);
        setting.by_provider.insert("fake".into(), 7200);
        let shared = pinned(dir.path(), setting.clone());
        let plain = worker(&shared, dir.path(), "plain", json!({}));
        assert_eq!(
            setting.bound_for(&plain),
            (
                Some(7200),
                "[host] auto_stop_idle_secs_by_provider.fake".into()
            )
        );
        let own = worker(
            &shared,
            dir.path(),
            "own",
            json!({"auto_stop_idle_secs": "900"}),
        );
        assert_eq!(setting.bound_for(&own).0, Some(900));
        let low = worker(
            &shared,
            dir.path(),
            "low",
            json!({"auto_stop_idle_secs": 5}),
        );
        assert_eq!(setting.bound_for(&low).0, Some(AUTO_STOP_FLOOR_SECS));
        let zero = worker(
            &shared,
            dir.path(),
            "zero",
            json!({"auto_stop_idle_secs": 0}),
        );
        assert_eq!(setting.bound_for(&zero).0, None);
        let off = worker(
            &shared,
            dir.path(),
            "off",
            json!({"auto_stop": "off", "auto_stop_idle_secs": 900}),
        );
        assert_eq!(
            setting.bound_for(&off),
            (None, "agent auto_stop=off".into())
        );
        // Another provider falls through to the host default.
        let host = idle_row(
            &shared,
            dir.path(),
            "host",
            "claude",
            "managed",
            "worker",
            json!({"upstream": "pm"}),
        );
        assert_eq!(setting.bound_for(&host).0, Some(5400));
        assert_eq!(AutoStopSetting::off().bound_for(&host).0, None);
        assert_eq!(AutoStopSetting::default().bound_for(&host).0, Some(3600));
    }

    #[test]
    fn idle_worker_is_due_only_after_the_bound() {
        let dir = tempfile::tempdir().unwrap();
        let setting = AutoStopSetting::default();
        let shared = pinned(dir.path(), setting.clone());
        let w = worker(&shared, dir.path(), "w1", json!({}));
        let now = epoch_secs();
        // Registration is its newest activity: 59 minutes on, kept.
        let young = verdict(&shared, &setting, &w, now + 59.0 * 60.0);
        assert!(kept(&young).contains("of 3600s"), "{young:?}");
        // Pre-age its whole stream by 72 minutes: due now.
        let age = |secs: f64| {
            rusqlite::Connection::open(dir.path().join("cadence.sqlite3"))
                .unwrap()
                .execute(
                    "UPDATE events SET at=at-? WHERE alias='w1'",
                    rusqlite::params![secs],
                )
                .unwrap();
        };
        age(72.0 * 60.0);
        match verdict(&shared, &setting, &w, epoch_secs()) {
            AutoStopVerdict::Stop {
                idle_secs,
                bound_secs,
                source,
                ..
            } => {
                assert!(idle_secs >= 71.0 * 60.0, "{idle_secs}");
                assert_eq!(bound_secs, 3600);
                assert_eq!(source, "default");
            }
            other => panic!("expected stop, got {other:?}"),
        }
        // Bookkeeping events never reset the idle clock; turn work does.
        for passive in ["quota_updated", "params_updated", "stop_requested"] {
            shared.store.event_public("w1", passive, json!({})).unwrap();
        }
        assert!(matches!(
            verdict(&shared, &setting, &w, epoch_secs()),
            AutoStopVerdict::Stop { .. }
        ));
        shared
            .store
            .event_public("w1", "turn_finished", json!({}))
            .unwrap();
        let fresh = verdict(&shared, &setting, &w, epoch_secs());
        assert!(kept(&fresh).starts_with("idle"), "{fresh:?}");
        // A resume's `ready` restarts the clock the same way.
        age(72.0 * 60.0);
        shared.store.event_public("w1", "ready", json!({})).unwrap();
        let resumed = verdict(&shared, &setting, &w, epoch_secs());
        assert!(kept(&resumed).starts_with("idle"), "{resumed:?}");
    }

    #[test]
    fn pm_roots_inbox_opted_out_and_unresumable_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let setting = AutoStopSetting::default();
        let shared = pinned(dir.path(), setting.clone());
        let at = epoch_secs() + 10.0 * HOUR;
        let pm = idle_row(&shared, dir.path(), "pm", "fake", "fake", "pm", json!({}));
        assert_eq!(
            kept(&verdict(&shared, &setting, &pm, at)),
            "group root (role pm)"
        );
        // No upstream: its own group root, like `group_root` everywhere.
        let solo = idle_row(
            &shared,
            dir.path(),
            "solo",
            "fake",
            "fake",
            "worker",
            json!({}),
        );
        assert_eq!(
            kept(&verdict(&shared, &setting, &solo, at)),
            "group root (no upstream)"
        );
        // A worker others report to is a sub-PM.
        let lead = worker(&shared, dir.path(), "lead", json!({}));
        worker(&shared, dir.path(), "member", json!({"upstream": "lead"}));
        assert_eq!(
            kept(&verdict(&shared, &setting, &lead, at)),
            "group root (has members)"
        );
        let inbox = idle_row(
            &shared,
            dir.path(),
            "box",
            "inbox",
            "inbox",
            "worker",
            json!({"upstream": "pm"}),
        );
        assert_eq!(kept(&verdict(&shared, &setting, &inbox, at)), "inbox");
        let off = worker(&shared, dir.path(), "off", json!({"auto_stop": "off"}));
        assert!(kept(&verdict(&shared, &setting, &off, at)).contains("auto_stop=off"));
        let nothread = worker(&shared, dir.path(), "nothread", json!({}));
        rusqlite::Connection::open(dir.path().join("cadence.sqlite3"))
            .unwrap()
            .execute(
                "UPDATE agents SET thread_id=NULL WHERE alias='nothread'",
                [],
            )
            .unwrap();
        assert!(kept(&verdict(&shared, &setting, &nothread, at)).contains("not resumable"));
        // The control: an ordinary member idle as long is due.
        let member = shared.store.agent("member").unwrap();
        assert!(matches!(
            verdict(&shared, &setting, &member, at),
            AutoStopVerdict::Stop { .. }
        ));
    }

    #[test]
    fn every_non_terminal_message_state_keeps_the_agent() {
        let dir = tempfile::tempdir().unwrap();
        let setting = AutoStopSetting::default();
        let shared = pinned(dir.path(), setting.clone());
        let at = epoch_secs() + 10.0 * HOUR;
        let conn = rusqlite::Connection::open(dir.path().join("cadence.sqlite3")).unwrap();
        for state in [
            "queued",
            "submitting",
            "running",
            "submitted",
            "awaiting_report",
            "unknown",
            "some_future_state",
        ] {
            let alias = format!("busy-{}", state.replace('_', "-"));
            let w = worker(&shared, dir.path(), &alias, json!({}));
            let id = format!("m-{alias}");
            shared
                .store
                .enqueue(&alias, "work", None, &id, "user")
                .unwrap();
            conn.execute(
                "UPDATE messages SET state=?, created=created-36000 WHERE id=?",
                rusqlite::params![state, id],
            )
            .unwrap();
            let v = verdict(&shared, &setting, &w, at);
            assert!(kept(&v).starts_with("busy: 1 message"), "{state}: {v:?}");
        }
        // Terminal states do not hold an agent.
        for state in ["completed", "failed", "interrupted", "cancelled"] {
            let alias = format!("done-{state}");
            let w = worker(&shared, dir.path(), &alias, json!({}));
            let id = format!("m-{state}");
            shared
                .store
                .enqueue(&alias, "work", None, &id, "user")
                .unwrap();
            conn.execute(
                "UPDATE messages SET state=? WHERE id=?",
                rusqlite::params![state, id],
            )
            .unwrap();
            assert!(
                matches!(
                    verdict(&shared, &setting, &w, at),
                    AutoStopVerdict::Stop { .. }
                ),
                "{state}"
            );
        }
    }

    #[test]
    fn per_provider_override_is_respected() {
        let dir = tempfile::tempdir().unwrap();
        let mut setting = AutoStopSetting::default();
        setting.by_provider.insert("fake".into(), 3 * 3600);
        setting.by_provider.insert("claude".into(), 0);
        let shared = pinned(dir.path(), setting.clone());
        let now = epoch_secs();
        let w = worker(&shared, dir.path(), "w1", json!({}));
        let early = verdict(&shared, &setting, &w, now + 2.0 * HOUR);
        assert!(kept(&early).contains("by_provider.fake"), "{early:?}");
        assert!(matches!(
            verdict(&shared, &setting, &w, now + 3.5 * HOUR),
            AutoStopVerdict::Stop {
                bound_secs: 10800,
                ..
            }
        ));
        let c = idle_row(
            &shared,
            dir.path(),
            "c1",
            "claude",
            "managed",
            "worker",
            json!({"upstream": "pm"}),
        );
        let off = verdict(&shared, &setting, &c, now + 30.0 * HOUR);
        assert!(kept(&off).contains("auto-stop off"), "{off:?}");
    }

    #[test]
    fn label_and_view_name_the_auto_stop_until_superseded() {
        assert_eq!(auto_stop_label(72.0 * 60.0), "stopped (auto, idle 72m)");
        assert_eq!(auto_stop_label(3.0 * HOUR), "stopped (auto, idle 3h)");
        assert_eq!(
            auto_stop_label(5.0 * HOUR + 1200.0),
            "stopped (auto, idle 5h20m)"
        );
        let dir = tempfile::tempdir().unwrap();
        let shared = pinned(dir.path(), AutoStopSetting::off());
        worker(&shared, dir.path(), "w1", json!({}));
        shared
            .store
            .set_state_detached("w1", "stopped", None)
            .unwrap();
        shared
            .store
            .event_public("w1", "stop_requested", json!({}))
            .unwrap();
        shared
            .store
            .event_public(
                "w1",
                AUTO_STOP_EVENT,
                json!({"idle_secs": 4320.0, "bound_secs": 3600}),
            )
            .unwrap();
        let agent = shared.store.agent("w1").unwrap();
        let markers = shared
            .store
            .last_events_of_all(AUTO_STOP_MARKER_KINDS)
            .unwrap();
        let view = auto_stop_view(&agent, markers.get("w1")).unwrap();
        assert_eq!(view["label"], "stopped (auto, idle 72m)");
        assert_eq!(view["resume"], "cadence agent resume w1");
        // A later manual stop supersedes the marker.
        shared
            .store
            .event_public("w1", "stop_requested", json!({}))
            .unwrap();
        let marker = shared
            .store
            .last_event_of("w1", AUTO_STOP_MARKER_KINDS)
            .unwrap();
        assert!(auto_stop_view(&agent, marker.as_ref()).is_none());
    }
}
