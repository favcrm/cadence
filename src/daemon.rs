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

// CAD-534: split root — the RPC handlers live in src/daemon/<area>.rs;
// the item→file map is src/daemon/split-map.toml
// (scripts/split-daemon regenerates both). The core stays here:
// the `Shared` struct, the actor loop / wake / lifecycle, the
// constants, the RPC dispatch and the test suites.

mod agents_rpc;
mod answer_rpc;
mod approvals_rpc;
mod area_rpc;
mod caller_rule;
mod checkup;
mod delivery_rpc;
mod dispatch_rpc;
mod effect_rpc;
mod identity;
mod jobs_rpc;
mod lane_rpc;
mod master_rpc;
mod master_session_rpc;
mod master_wake;
mod memory_rpc;
mod messages_rpc;
mod models_rpc;
mod monitors_rpc;
mod needs_rpc;
mod next_action;
mod operator_rpc;
mod plans_rpc;
mod platform_rpc;
mod requests_rpc;
mod serve;
mod slots_rpc;
mod threads_rpc;
mod timers;
mod watch;
mod wiki_rpc;

use crate::adapter;
use crate::adapter::registry;
use crate::adapter::AdapterHooks;
use crate::adapter::ProviderAdapter;
use crate::adapter::ProviderEnv;
use crate::adapter::ProviderRequest;
use crate::adapter::SettledPoll;
use crate::adapter::TurnResult;
use crate::client;
use crate::error::Error;
use crate::error::Result;
use crate::peer::unmatched_caller;
use crate::peer::AgentCaller;
use crate::peer::AgentMutation;
use crate::proto;
use crate::slots::SlotConfig;
use crate::slots::Slots;
use crate::store;
use crate::store::Agent;
use crate::store::Message;
use crate::store::Store;
use crate::store::Take;
use serde_json::json;
use serde_json::Value;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::BufRead;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::Condvar;
use std::sync::Mutex;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;
use uuid::Uuid;

/// CAD-339: the daemon methods a master connection may call.
pub use master_rpc::MASTER_ALLOWED;

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

/// How long an idle actor waits on an empty queue before it looks
/// again. Enqueues wake it at once; this is the backstop.
const IDLE_POLL: Duration = Duration::from_secs(5);

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

/// The daemon's own event stream — `wal_checkpointed` lands here.
/// Readable via `cadence events daemon`; not a sendable alias.
const DAEMON_ALIAS: &str = Store::DAEMON_STREAM;

/// How an approval-evidence writer was authorized — the daemon's own
/// statement, stamped on every record (CAD-217).
const APPROVAL_RECORDED_VIA: &str = "operator-connection";

/// Who a model-defaults change is recorded as: the only caller
/// `model_defaults_set` accepts (CAD-337).
const MODEL_DEFAULTS_ATTRIBUTION: &str = "operator";

/// The lane an operator-launched runner is accounted to — not a valid
/// alias, so it never collides with an agent's.
const OPERATOR_LANE: &str = "(operator)";

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
    /// CAD-561: the pending update while one drains — `None` when no
    /// update is in progress. Set by `update_drain`, adopted from
    /// `<state>/update.json` (proved against the rollout lease — see
    /// [`Shared::pending_update`]), cleared by `update_drain off`.
    draining: Mutex<Option<crate::update::PendingUpdate>>,
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
    /// CAD-339: a report was filed — the report router scans at once.
    reports_dirty: AtomicBool,
    /// CAD-339: the report router's scan period; `None` is off.
    router_every: Option<Duration>,
    /// CAD-477: the checkup's pass period; `None` is off.
    checkup_every: Option<Duration>,
    /// CAD-484: test seam for the idle lane's dispatch — `None` runs
    /// the real `issue::dispatch::run`.
    checkup_dispatch: Option<Arc<CheckupDispatch>>,
    /// CAD-339: reports due to the master but held back by the per-pass
    /// cap at the router's last pass.
    router_backlog: std::sync::atomic::AtomicUsize,
    /// CAD-339: serializes writers of the escalation record.
    escalation_lock: Mutex<()>,
    /// CAD-339: serializes `master_dispatch` — the ticket's `ready`
    /// check and its dispatch are one step, so concurrent calls for a
    /// ticket dispatch it once.
    dispatch_lock: Mutex<()>,
    /// CAD-431: serializes every transition of the worker loop's
    /// record (`delivery.json`).
    delivery_lock: Mutex<()>,
    /// The actor's empty-queue poll — the backstop behind its wake.
    idle_poll: Duration,
    /// CAD-445: serialises `<state>/master-wakes.json` (blocker epochs,
    /// tickets a plan-approved wake already named).
    wake_lock: Mutex<()>,
    /// CAD-152: `agent recover-submit` runs one at a time, from its
    /// message checks through the recorded outcome — a racing second
    /// recovery (a PM and the operator on the same stuck draft) sees
    /// the first one's `submit_recovered` and refuses, never a second
    /// Enter.
    recover_lock: Mutex<()>,
    /// CAD-324: agents whose next delivered turn carries a continuity
    /// pack because an actor opened a new session or reopened one whose
    /// last turn was lost; taken by the actor at its next turn. A
    /// compaction is not kept here but as a thread note
    /// ([`Store::compaction_pending`]), so it survives a restart.
    continuity_due: Mutex<HashMap<String, crate::continuity::Reason>>,
    /// CAD-313: login links and board sessions ([`crate::operator_auth`]).
    operator_auth: Mutex<crate::operator_auth::Auth>,
    /// CAD-313: the clock links and sessions expire by (epoch seconds) —
    /// the wall clock in production, injectable in tests.
    operator_clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// CAD-526: the platform JWKS the board-identity assertions verify
    /// against — short-lived cache, refetched on an unknown `kid`
    /// ([`crate::board_identity::JwksCache`]).
    board_jwks: Mutex<crate::board_identity::JwksCache>,
    /// CAD-366: where enrolled platform credential bytes live — the
    /// host's keychain when usable, else the daemon-owned `0600`
    /// store under the state dir (ADR 0006 §5.3).
    platform_custody: crate::platform::Custody,
    /// CAD-366: serializes a custody write against the record write it
    /// pairs with — concurrent enrolls of one account, or a revoke
    /// racing a put, must never leave a record's fingerprint and the
    /// custody bytes disagreeing. One lock for every custody mutation:
    /// these verbs are operator-paced and rare, so a per-key map buys
    /// nothing here.
    platform_custody_lock: Mutex<()>,
    /// CAD-506: the registered platform adapters the effect gate drives
    /// (`platform` name → adapter). A platform with none fails closed —
    /// no reviewed table means no classification, so no call.
    platforms: effect_rpc::PlatformMap,
    effect_execute_gate: Option<effect_rpc::EffectExecuteGate>,
    /// CAD-546: the `local` platform's outbox root — what
    /// `platform_outbox` lists. Set by `platform::local::register`
    /// alongside the adapter so the read serves what the write lands.
    outbox_dir: Option<PathBuf>,
    /// CAD-538: the hosted lease this daemon holds when `hosted.lease`
    /// is configured. The heartbeat renews it; its fence is shared with
    /// `store` (every `write_conn`) and with each [`Self::pm`] handle.
    lease: Option<Arc<crate::lease::LeaseCtl>>,
    /// CAD-482: the test-only caller seam's armed credential — `Some`
    /// only when a fixture asked for it ([`ServeOptions::test_seam`])
    /// on a `test-seam` build. Request frames carrying `test_caller`
    /// are honored against it; without it they are refused.
    seam: Option<crate::test_seam::Seam>,
    /// CAD-575: the `devin models list` outcome `master_models` reads
    /// cost tiers from — the pi-devin cache file wins fresh every call;
    /// only the spawned CLI path memoizes ([`crate::devin_catalog`]).
    devin_catalog: crate::devin_catalog::CatalogCache,
}

impl Shared {
    pub fn new(state_dir: &Path, opts: &ServeOptions) -> Result<Arc<Self>> {
        Self::new_hot(state_dir, opts, HotStart::fresh())
    }

    /// `new` with the consumed hot-restart context: the adoption
    /// candidates the marker carried plus this run's instance id.
    pub fn new_hot(state_dir: &Path, opts: &ServeOptions, hot: HotStart) -> Result<Arc<Self>> {
        // CAD-482: the seam check runs before the lease is taken or
        // the store opens — a fixture that arms on the production dir
        // or outside the temp root refuses here, before anything is
        // written. A state dir that still carries a minted token
        // re-arms: `daemon restart` spawns this process without the
        // arming env.
        let seam = crate::test_seam::arm_if_requested(
            state_dir,
            opts.test_seam || crate::test_seam::armed(state_dir),
        )?;
        // CAD-538: a configured hosted lease must be held before the
        // store opens — `recover` writes at open. A daemon that cannot
        // take the lease refuses here having written nothing.
        let lease = crate::lease::acquire(state_dir, &hosted_config(opts)?)?;
        Self::new_leased(state_dir, opts, hot, lease, seam)
    }

    /// `new_hot` over an already-resolved lease and seam — `serve`
    /// acquires both before `hot_restart_begin` so a refused daemon
    /// leaves even the marker files untouched.
    fn new_leased(
        state_dir: &Path,
        opts: &ServeOptions,
        hot: HotStart,
        lease: Option<Arc<crate::lease::LeaseCtl>>,
        seam: Option<crate::test_seam::Seam>,
    ) -> Result<Arc<Self>> {
        let HotStart { instance, marker } = hot;
        let daemon_id = instance.clone();
        let db_path = state_dir.join("cadence.sqlite3");
        // Authorise the holder before the store opens the file
        // read-write and migrates. A direct `daemon run` whose identity
        // does not hold the lease refuses here and leaves the database
        // unchanged. `open_adopting` repeats the same check.
        crate::rollout::authorize_migration(&db_path)?;
        let store = Store::open_adopting(&db_path, marker)?;
        // CAD-538: the store's write path now shares the lease fence —
        // one trip refuses every later write.
        if let Some(lease) = &lease {
            store.install_write_fence(lease.fence());
        }
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
            // CAD-561: a pending update recorded by an update that is
            // still running (a restart in the middle of one) keeps the
            // fleet drained across the restart. The marker is adopted on
            // first read, proved against the live rollout lease — the
            // file alone is not authority (any same-uid process can
            // write it). The file's mtime is the update's heartbeat: a
            // marker that stopped being rewritten (an update that died)
            // goes stale in minutes and never wedges the fleet.
            draining: Mutex::new(None),
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
            reports_dirty: AtomicBool::new(false),
            router_every: match opts.report_router {
                None => Some(Duration::from_secs(30)),
                Some(0) => None,
                Some(secs) => Some(Duration::from_secs(secs)),
            },
            checkup_every: match opts.checkup {
                None => Some(Duration::from_secs(checkup::DEFAULT_CHECKUP_SECS)),
                Some(0) => None,
                Some(secs) => Some(Duration::from_secs(secs)),
            },
            checkup_dispatch: opts.checkup_dispatch.clone(),
            router_backlog: std::sync::atomic::AtomicUsize::new(0),
            escalation_lock: Mutex::new(()),
            dispatch_lock: Mutex::new(()),
            delivery_lock: Mutex::new(()),
            wake_lock: Mutex::new(()),
            continuity_due: Mutex::new(HashMap::new()),
            auto_stop: AutoStopTimer::new(opts.auto_stop.clone(), opts.auto_stop_clock.clone()),
            idle_poll: opts.idle_poll.unwrap_or(IDLE_POLL),
            recover_lock: Mutex::new(()),
            operator_auth: Mutex::new(crate::operator_auth::Auth::load(state_dir)),
            operator_clock: opts
                .operator_clock
                .clone()
                .unwrap_or_else(|| Arc::new(crate::issue::time::now_epoch)),
            board_jwks: Mutex::new(crate::board_identity::JwksCache::default()),
            platform_custody: crate::platform::Custody::open(state_dir)?,
            platform_custody_lock: Mutex::new(()),
            platforms: opts.platforms.clone(),
            effect_execute_gate: opts.effect_execute_gate.clone(),
            outbox_dir: opts.outbox_dir.clone(),
            lease,
            seam,
            devin_catalog: crate::devin_catalog::CatalogCache::default(),
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
        // CAD-324: the provider compacted the session — its next turn
        // carries a continuity pack. The due-ness is a thread note, so it
        // survives a daemon restart; the event itself is recorded below
        // like every `cadence/<kind>`.
        if method == "cadence/session_compacted" {
            if let Err(e) = self.store.thread_append(
                alias,
                store::NewEntry {
                    role: store::ROLE_SYSTEM,
                    kind: store::KIND_MESSAGE,
                    text: "The provider compacted this session's context; the next turn \
                           carries a continuity pack.",
                    payload: Some(json!({"event": crate::continuity::COMPACTED_EVENT,
                                         "trigger": params.get("trigger")})),
                    message_id: None,
                },
            ) {
                eprintln!("compaction note for '{alias}' failed: {e}");
            }
        }
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
        // Assistant prose belongs to the thread (above), never the
        // event log — `events` stays a lifecycle envelope (CAD-320).
        if method == "cadence/assistant_text" {
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
    /// `agentMessage` items as they persist, managed Claude text blocks,
    /// tool uses (name + redacted summary) and tool results (redacted
    /// summary + `is_error`, CAD-320). The turn result lands with the
    /// message's finish, in the store — so the final answer is never
    /// recorded twice: Codex `final_answer` items (and unphased ones,
    /// which Codex joins into the result) are held until the finish,
    /// which keeps only those the result does not carry, and the Claude
    /// adapter drops the text block its `result` repeats. A lost append
    /// is logged, never fatal to the turn — the provider transcript
    /// still has it.
    /// CAD-324: the prompt for `message` — its body, preceded by a
    /// continuity pack when one is due for `alias` and the endpoint takes
    /// one. Due-ness is consumed here, delivered or not: a pack goes with
    /// the first turn of a new or lost session, and with the first turn
    /// after a compaction. The pack is assembled by the daemon from the
    /// store, the tracker and USER.md; the thread records that it went
    /// (counts and digest, never the content). A pack that cannot be
    /// built never holds the turn back: the message goes alone and the
    /// failure is an event.
    fn continuity_prompt(&self, alias: &str, endpoint_kind: &str, message: &Message) -> String {
        // A new or lost session is decided at open (in memory: the next
        // open decides again); a compaction is a thread note, pending
        // until a pack note follows it.
        let due = self
            .continuity_due
            .lock()
            .unwrap()
            .remove(alias)
            .or_else(|| {
                self.store
                    .compaction_pending(alias)
                    .unwrap_or(false)
                    .then_some(crate::continuity::Reason::Compacted)
            });
        let Some(reason) = due else {
            return message.body.clone();
        };
        if !crate::continuity::endpoint_takes_packs(endpoint_kind) {
            return message.body.clone();
        }
        let pm_dir = self.pm_dir().ok().filter(|d| d.is_dir());
        let built =
            crate::continuity::assemble(&self.store, pm_dir.as_deref(), alias, reason, &message.id);
        let pack = match built {
            Ok(Some(pack)) => pack,
            Ok(None) => {
                // Nothing to carry. A pending compaction is settled so
                // later turns do not rebuild it.
                if reason == crate::continuity::Reason::Compacted {
                    self.continuity_settle(alias, reason, &message.id, "skipped", None);
                }
                return message.body.clone();
            }
            Err(e) => {
                // One failure per trigger: the note settles it, so a
                // pack that cannot be built is not rebuilt (and its
                // failure not re-reported) on every later turn.
                let error = e.to_string();
                let _ = self.store.event_public(
                    alias,
                    "continuity_pack_failed",
                    json!({"reason": reason.as_str(), "message": message.id,
                           "error": error}),
                );
                self.continuity_settle(alias, reason, &message.id, "failed", Some(&error));
                return message.body.clone();
            }
        };
        let payload = pack.payload(&message.id);
        if let Err(e) = self.store.thread_append(
            alias,
            store::NewEntry {
                role: store::ROLE_SYSTEM,
                kind: store::KIND_MESSAGE,
                text: &pack.note(),
                payload: Some(payload.clone()),
                message_id: None,
            },
        ) {
            eprintln!("continuity note for '{alias}' failed: {e}");
        }
        let _ = self
            .store
            .event_public(alias, crate::continuity::PACK_EVENT, payload);
        self.wake();
        pack.wrap(&message.body)
    }

    /// CAD-324: record in the thread that a due pack was not delivered
    /// (`outcome`: `skipped` — nothing to carry — or `failed`). The note
    /// is a pack note, so it settles a pending compaction.
    fn continuity_settle(
        &self,
        alias: &str,
        reason: crate::continuity::Reason,
        message: &str,
        outcome: &str,
        error: Option<&str>,
    ) {
        let text = match error {
            Some(e) => format!("Continuity pack not delivered ({}): {e}", reason.as_str()),
            None => format!(
                "Continuity pack not delivered ({}): nothing to carry.",
                reason.as_str()
            ),
        };
        if let Err(e) = self.store.thread_append(
            alias,
            store::NewEntry {
                role: store::ROLE_SYSTEM,
                kind: store::KIND_MESSAGE,
                text: &text,
                payload: Some(json!({"event": crate::continuity::PACK_EVENT,
                                     "reason": reason.as_str(), "message": message,
                                     "outcome": outcome, "error": error})),
                message_id: None,
            },
        ) {
            eprintln!("continuity note for '{alias}' failed: {e}");
        }
    }

    fn thread_on_provider_event(&self, alias: &str, method: &str, params: &Value) {
        // CAD-551: a permission denial is a refused step, not a failed
        // one — one `tool_result` entry per denied call, paired with its
        // `tool_use` by `tool_use_id`. The denial arrives on the result
        // event, after the call's own entries, so the board shows the
        // refusal where the step already rendered.
        if method == "cadence/permission_denied" {
            for denial in params
                .get("denials")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let summary = denial
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("tool call");
                if let Err(e) = self.store.thread_append_running(
                    alias,
                    store::ROLE_AGENT,
                    store::KIND_TOOL_RESULT,
                    summary,
                    Some(json!({
                        "is_error": true,
                        "refused": true,
                        "tool_use_id": denial.get("tool_use_id"),
                    })),
                ) {
                    eprintln!("refused-step note for '{alias}' failed: {e}");
                }
            }
            return;
        }
        let (kind, text, payload) = match method {
            "item/completed" => {
                let item = &params["item"];
                if item.get("type").and_then(Value::as_str) != Some("agentMessage") {
                    return;
                }
                let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                let payload = json!({"provider_item": item.get("id"), "phase": item.get("phase")});
                let phase = item.get("phase").and_then(Value::as_str);
                if matches!(phase, None | Some("final_answer")) {
                    // Codex builds the turn result from these; the
                    // finish keeps whichever the result does not carry.
                    if let Err(e) = self.store.thread_hold_running(alias, text, payload) {
                        eprintln!("thread hold for '{alias}' failed: {e}");
                    }
                    return;
                }
                (store::KIND_ASSISTANT_TEXT, text.to_string(), payload)
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
            "cadence/assistant_text" => (
                store::KIND_ASSISTANT_TEXT,
                params["text"].as_str().unwrap_or("").to_string(),
                // Never the final answer — that is the turn result.
                json!({"phase": "commentary"}),
            ),
            "cadence/tool_result" => (
                store::KIND_TOOL_RESULT,
                params["summary"].as_str().unwrap_or("").to_string(),
                json!({
                    "is_error": params["is_error"].as_bool().unwrap_or(false),
                    // The adapter asserts `refused` only when the provider's
                    // own refusal channel named it (the master's guard, a
                    // permission denial) — an erroring tool stays a failure.
                    "refused": params["refused"].as_bool().unwrap_or(false),
                    "tool_use_id": params.get("tool_use_id"),
                }),
            ),
            _ => return,
        };
        if let Err(e) =
            self.store
                .thread_append_running(alias, store::ROLE_AGENT, kind, &text, Some(payload))
        {
            eprintln!("thread append for '{alias}' failed: {e}");
        }
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
        // Same lane, same rule (CAD-542): `params` is the provider's
        // input verbatim — a codex `requestApproval` carries the
        // command — so the event keeps routing fields only. The
        // pending row retains the params `agent_requests` discloses to
        // the authorised answerer.
        let _ = self.store.event_public(
            alias,
            "input_required",
            json!({"request": handle, "method": request.method}),
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
                // CAD-413: an auto-resume whose open never reached
                // `ready` is still the newest marker — name it.
                let marker = self.store.last_event_of(alias, AUTO_STOP_MARKER_KINDS);
                if marker.is_ok_and(|m| m.is_some_and(|e| e.kind == AUTO_RESUME_EVENT)) {
                    self.auto_resume_failed(alias, &reason);
                }
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
                    .set_identity_with_quota(alias, &identity, adapter.quota_snapshot())?;
                // CAD-324: a session the provider did not carry over — a
                // new one, or a reopen whose last turn was lost — starts
                // its next turn with a continuity pack. An adopted
                // endpoint is the same live session: nothing is due.
                let reason = if agent.thread_id.as_deref() != Some(identity.thread_id.as_str()) {
                    Some(crate::continuity::Reason::New)
                } else if self.store.last_turn_lost(alias)? {
                    Some(crate::continuity::Reason::Lost)
                } else {
                    None
                };
                if let Some(reason) = reason {
                    self.continuity_due
                        .lock()
                        .unwrap()
                        .insert(alias.to_string(), reason);
                }
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
            // CAD-561: while an update drains, no actor starts a new
            // turn — queued deliveries stay in the inbox and are claimed
            // after the restart (or when the drain is lifted). The
            // in-flight turn this actor already runs is untouched.
            if self.draining() {
                if !self.store.agent(alias)?.enabled {
                    return Ok(());
                }
                ctl.wake
                    .wait_if_unchanged(ticket, Instant::now() + self.idle_poll);
                continue;
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
                    ctl.wake
                        .wait_if_unchanged(ticket, Instant::now() + self.idle_poll);
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
                    // CAD-324: a nudge owns no turn and carries no pack.
                    let prompt = if nudge {
                        message.body.clone()
                    } else {
                        self.continuity_prompt(alias, &agent.endpoint_kind, &message)
                    };
                    adapter.set_unclaimed_ok(message.is_routed() || nudge);
                    // CAD-520: a nudge may also enter through a busy
                    // pane's steering input (Devin's guide box). Cleared
                    // with `unclaimed_ok` so a later message cannot
                    // inherit either flag.
                    adapter.set_steer_ok(nudge);
                    let outcome = adapter.run_turn(&prompt, &message.id, &move |turn| {
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
                    adapter.set_steer_ok(false);
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
                                       "claim_probe": miss.claim_probe,
                                       "reprobe": miss.reprobe}),
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
                                reprobe,
                            } = *miss;
                            let _ = self.store.event_public(
                                alias,
                                "paste_not_rendered",
                                json!({"message": message.id,
                                       "reason": reason,
                                       "attempt": unrendered,
                                       "retry": retry,
                                       "before": before_tail,
                                       "after": after_tail,
                                       "claim_probe": claim_probe,
                                       "reprobe": reprobe}),
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
    /// caller identity; clients cannot supply this identity. Every
    /// answer leaves through [`Self::withhold_turn_tokens`] (CAD-375).
    pub fn dispatch(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        // CAD-339: what the master may never do is refused here, before
        // any method runs — a refusal leaves no write.
        self.master_policy(method, params, peer_pid)?;
        let answer = self.dispatch_method(method, params, peer_pid)?;
        Ok(self.withhold_turn_tokens(answer, peer_pid))
    }

    /// CAD-375: a running turn's token is the one credential
    /// `message_report` checks, so no answer carries it to anyone but
    /// the agent that owns the turn — the connection whose `/proc`
    /// ancestry derives that agent ([`Self::slot_identity`]). The
    /// operator, the board and every other agent read `null` where a
    /// `turn_id` held it (and a redaction marker where prose quoted
    /// it), on every read path at once: `agent_show`/`agent_list`
    /// (`awaiting_report`, message rows), events, job and task views,
    /// threads, pane captures.
    ///
    /// EVERY running message's token is withheld, whatever its
    /// currency: a hot restart clears the generation while adopted
    /// turns keep running and then restores it, so a token that is not
    /// current now can be current again in a moment (review R1). The
    /// owner derivation reads panes and enrollments only; it can miss
    /// the owner (a pane whose facts are not published yet) and then
    /// withholds from the owner too — never the other way round. Fail
    /// closed: a caller whose identity cannot be derived owns nothing,
    /// and unreadable running turns withhold every `turn_id`.
    fn withhold_turn_tokens(&self, answer: Value, peer_pid: u32) -> Value {
        let live = match self.store.running_turn_tokens() {
            Ok(rows) => rows,
            Err(_) => return withhold_all_turn_ids(answer),
        };
        if live.is_empty() {
            return answer;
        }
        let text = answer.to_string();
        let present: Vec<&(String, String)> = live
            .iter()
            .filter(|(_, token)| text.contains(token.as_str()))
            .collect();
        if present.is_empty() {
            return answer;
        }
        let owner = self
            .revalidate_enrollments()
            .and_then(|()| self.slot_identity(peer_pid))
            .ok()
            .flatten()
            .map(|who| who.lane().to_string())
            .filter(|lane| !lane.is_empty());
        let foreign: Vec<&str> = present
            .into_iter()
            .filter(|(alias, _)| owner.as_deref() != Some(alias.as_str()))
            .map(|(_, token)| token.as_str())
            .collect();
        if foreign.is_empty() {
            return answer;
        }
        let mut answer = answer;
        redact_tokens(&mut answer, &foreign);
        answer
    }

    fn dispatch_method(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        // CAD-384: the one caller rule, before any method runs — a
        // refusal leaves no write. An admitted request may come back
        // with its attribution field stamped to the caller.
        let stamped = self.caller_gate(method, params, peer_pid)?;
        let params = stamped.as_ref().unwrap_or(params);
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
                // CAD-538: the hosted lease, when held — provider, epoch,
                // expiry and the fence reason after a loss.
                "lease": self.lease.as_ref().map(|l| l.status_json()),
                // CAD-561: a pending update and what it waits on, so
                // `cadence daemon status` and the board's banner show it.
                "pending_update": self.pending_update().map(|p| p.to_json()),
                "update_waiting": self.inflight_turns().unwrap_or_default(),
                })
            }),
            // Build identity + process start — the deploy-drift check
            // measures merged commits against *this* binary's commit.
            "daemon_info" => Ok({
                // Registered connection (platform adapter) names — an
                // app slot's binding is checked against these (CAD-547).
                let mut connections: Vec<&str> =
                    self.platforms.keys().map(String::as_str).collect();
                connections.sort_unstable();
                json!({
                    "build_commit": crate::overview::BUILD_COMMIT,
                    "build_time": crate::overview::BUILD_TIME,
                    "started_at": self.started_at,
                    "connections": connections,
                })
            }),
            "shutdown" => {
                self.begin_closing();
                Ok(json!({"state": "stopping"}))
            }
            // CAD-561: the update's drain gate. Operator-only, like
            // every other action that stops the fleet's work: `on`
            // records the pending update (also on disk, so a restart
            // mid-update stays drained) and stops actors claiming new
            // turns; `off` lifts both.
            "update_drain" => {
                self.operator_connection("update_drain", params, peer_pid)?;
                if params["on"].as_bool() == Some(true) {
                    // `label` names the update's owner for the report and
                    // the banner. It is a label, not authority: the
                    // connection is the authority (and this verb only
                    // admits the operator), so no caller can become
                    // another by writing it.
                    let label = optional_str(params, "label").unwrap_or("operator");
                    if label.is_empty()
                        || label.len() > 200
                        || label
                            .chars()
                            .any(|c| c.is_control() || c == '\n' || c == '\r')
                    {
                        return Err(Error::rejected(
                            "update_drain: label must be 1..=200 characters with no control \
                             characters",
                        ));
                    }
                    let pending = crate::update::PendingUpdate {
                        phase: optional_str(params, "phase")
                            .unwrap_or("draining")
                            .to_string(),
                        target: required_str(params, "target")?.to_string(),
                        from: optional_str(params, "from").map(str::to_string),
                        by: label.to_string(),
                        since: params["since"]
                            .as_f64()
                            .unwrap_or_else(crate::rollout::unix_now),
                    };
                    // CAD-561 r2: the drain is the lease holder's. The
                    // label is caller-chosen, so without this a drain
                    // could name anyone — and the fleet would stop for a
                    // run that holds nothing.
                    if !self.marker_is_the_lease_holders(&pending) {
                        return Err(Error::rejected(format!(
                            "update_drain: the rollout lease is not held by '{label}' — \
                             the drain belongs to the live lease holder; claim the \
                             lease, then drain"
                        )));
                    }
                    crate::update::write_pending(&self.state_dir, &pending)?;
                    *self.draining.lock().unwrap() = Some(pending);
                } else {
                    crate::update::clear_pending(&self.state_dir);
                    *self.draining.lock().unwrap() = None;
                }
                self.wake();
                Ok(json!({
                    "draining": self.draining(),
                    "pending_update": self.pending_update().map(|p| p.to_json()),
                }))
            }
            "update_status" => {
                let pending = self.pending_update();
                let waiting = self.inflight_turns()?;
                Ok(json!({
                    "pending_update": pending.as_ref().map(|p| p.to_json()),
                    "waiting": waiting,
                    "waiting_count": waiting.len(),
                }))
            }
            "agent_register" => self.rpc_register(params, peer_pid),
            "model_defaults_get" => self.rpc_model_defaults_get(),
            "model_defaults_set" => self.rpc_model_defaults_set(params, peer_pid),
            "agent_list" => {
                // CAD-437: repeatable any-of filters, daemon-side.
                let states = optional_strs(params, "states")?;
                let providers = optional_strs(params, "providers")?;
                let kinds = optional_strs(params, "kinds")?;
                check_values("states", &states, crate::store::AGENT_STATES)?;
                check_values("providers", &providers, &registry::provider_ids())?;
                check_values("kinds", &kinds, &registry::endpoint_kind_ids())?;
                let mut agents = Vec::new();
                // CAD-96: one grouped read tells auto-stopped rows apart.
                let markers = self
                    .store
                    .last_events_of_all(AUTO_STOP_MARKER_KINDS)
                    .unwrap_or_default();
                for agent in self.store.agents()? {
                    if !states.is_empty() && !states.contains(&agent.state) {
                        continue;
                    }
                    if !providers.is_empty() && !providers.contains(&agent.provider) {
                        continue;
                    }
                    if !kinds.is_empty() && !kinds.contains(&agent.endpoint_kind) {
                        continue;
                    }
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
                    // CAD-325: `board: true` folds what the board read
                    // per agent through `agent_show` into this one pass.
                    if params.get("board").and_then(Value::as_bool) == Some(true)
                        && registry::has_actor(&agent.provider, &agent.endpoint_kind)
                    {
                        j["board"] = self.board_view(&agent.alias)?;
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
                // CAD-556: the emitted Landlock policy for a confined
                // pi worker — recomputed from the same inputs `open`
                // uses, like `master confinement` prints the master's.
                // `confined` reports the launch intent on every
                // pi/managed row so `agent show` answers "is it
                // sandboxed" without parsing the argv.
                if agent.provider == "pi" && agent.endpoint_kind == "managed" {
                    if crate::master::is_master(&agent.alias) {
                        agent_json["confined"] = json!(crate::master::is_confined(
                            agent.params.as_ref(),
                            crate::confine::available().is_ok()
                        ));
                    } else {
                        let confined = adapter::pi::worker_confined(&agent);
                        agent_json["confined"] = json!(confined);
                        if confined {
                            let (_exe, policy) = adapter::pi::pi_worker_confinement(
                                &self.provider_env,
                                &self.state_dir,
                                &agent,
                            );
                            agent_json["confinement"] =
                                json!({"read": policy.read, "write": policy.write});
                        }
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
                // CAD-506: a pending row carries the caller-declared
                // input — it discloses to the operator, to the owning
                // agent, and to the owner's PM (CAD-370's authorised
                // reviewer; the open notice sends it here for the full
                // input). A peer agent or an unproven caller is refused;
                // before this, Rule::Read exposed every agent's pending
                // input to any caller (the CAD-366 review flag).
                let may_see = match self.agent_caller(peer_pid, "agent requests")? {
                    AgentCaller::Operator => true,
                    AgentCaller::Agent(ref a) if *a == alias => true,
                    AgentCaller::Agent(ref a) => {
                        let target = self.store.agent(&alias)?;
                        self.effective_pm(&target)?.as_deref() == Some(a.as_str())
                    }
                };
                if !may_see {
                    return Err(Error::rejected(format!(
                        "agent requests refused: '{alias}'s pending rows disclose \
                         only to the operator, '{alias}' itself and its PM \
                         (caller rule, CAD-506)"
                    )));
                }
                let mut requests: Vec<Value> = self
                    .pending
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(_, req)| req.alias == alias)
                    .map(|(handle, req)| {
                        json!({"request": handle, "method": req.method, "params": req.params})
                    })
                    .collect();
                // A staged send is a brokered `kind:"effect"` request
                // whose authority is the durable row — it joins the
                // listing from the table, so a restart never drops an
                // unanswered press.
                for row in self.store.platform_effects(Some(&alias))? {
                    if row.state == "waiting" {
                        requests.push(json!({"request": row.request,
                            "method": "cadence/effect",
                            "params": row.to_record()}));
                    }
                }
                Ok(json!({"requests": requests}))
            }
            "agent_respond" => self.rpc_respond(params, peer_pid),
            "request_open" => self.rpc_request_open(params, peer_pid),
            "request_wait" => self.rpc_request_wait(params, peer_pid),
            "request_close" => self.rpc_request_close(params, peer_pid),
            "agent_ready" => self.rpc_ready(params),
            "agent_capture" => self.rpc_capture(params),
            "agent_probe" => self.rpc_probe(params),
            "agent_answer" => self.rpc_answer(params, peer_pid),
            "agent_recover_submit" => self.rpc_recover_submit(params, peer_pid),
            "agent_set" => self.rpc_set(params, peer_pid),
            "agent_inbox" => self.rpc_inbox(params, peer_pid),
            "agent_inbox_ack" => self.rpc_inbox_ack(params, peer_pid),
            "message_report" => self.rpc_message_report(params),
            "message_reconcile" => self.rpc_reconcile(params, peer_pid),
            "message_cancel" => self.rpc_cancel(params),
            "interrupt" => self.rpc_interrupt(params, peer_pid),
            "job_new" => self.rpc_job_new(params),
            "job_list" => self.rpc_job_list(params),
            "job_show" => self.rpc_job_show(params),
            "job_events" => self.rpc_job_events(params),
            "job_cancel" => self.rpc_job_cancel(params),
            "job_close" => self.rpc_job_close(params),
            "task_new" => self.rpc_task_new(params),
            "task_show" => self.rpc_task_show(params),
            "task_dispatch" => self.rpc_task_dispatch(params),
            "task_verdict" => self.rpc_task_verdict(params, peer_pid),
            "task_accept" => self.rpc_task_accept(params),
            "task_sha" => self.rpc_task_sha(params),
            "task_fail" => self.rpc_task_fail(params),
            "task_reopen" => self.rpc_task_reopen(params, peer_pid),
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
            "monitor_stop" => self.rpc_monitor_stop(params, peer_pid),
            "monitor_dispatch" => self.rpc_monitor_dispatch(params, peer_pid),
            "agent_unfence" => self.rpc_unfence(params, peer_pid),
            "agent_stop" => self.rpc_stop(params),
            "agent_remove" => {
                let alias = self.resolve_alias(required_str(params, "alias")?)?;
                let agent = self.store.agent(&alias)?;
                // CAD-304 S3: removal deletes the agent's history — the
                // operator's or its own PM's call, never a peer's or its
                // own (see `authorize_agent_mutation`). Checked before
                // anything else so a refused caller learns nothing more.
                let caller = self.authorize_agent_mutation(
                    params,
                    peer_pid,
                    "agent remove",
                    &agent,
                    AgentMutation::Controlled,
                )?;
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
                let notify = {
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
                    let notify = self
                        .store
                        .remove_agent(&alias, force, &caller_audit(&caller))?;
                    // A fenced pty pane may still be alive — remove is
                    // the explicit kill; never leave an orphan session
                    // on the private socket behind a dropped row.
                    if agent.endpoint_kind == "pty" {
                        adapter::pty::kill_pane(&self.state_dir, &alias, &self.provider_env);
                    }
                    self.open_attach.lock().unwrap().remove(&alias);
                    notify
                };
                // A forced finish routed notices: wake their actors.
                for target in &notify {
                    self.notify_agent(target);
                }
                self.wake();
                Ok(json!({"alias": alias, "state": "removed"}))
            }
            "agent_gc" => {
                // CAD-304 S3: a sweep removes agents, so each candidate
                // passes the same caller rule as `agent remove` — the
                // operator sweeps everything, an agent only the agents
                // it is PM of; the rest are listed as not permitted.
                reject_identity_fields(params, "agent gc")?;
                let caller = self.agent_caller(peer_pid, "agent gc")?;
                let audit = caller_audit(&caller);
                let older_than = params.get("older_than").and_then(Value::as_f64);
                let (candidates, not_permitted) = self.gc_partition(&caller, older_than)?;
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
                        if self.store.remove_agent(&agent.alias, false, &audit).is_ok() {
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
                let mut out = json!({"removed": removed});
                if !not_permitted.is_empty() {
                    out["not_permitted"] = json!(not_permitted);
                }
                Ok(out)
            }
            "agent_gc_plan" => {
                // Read-only: what `agent gc` would sweep for THIS caller
                // and what the caller rule keeps it from sweeping —
                // `session end --dry-run` renders it.
                reject_identity_fields(params, "agent gc")?;
                let caller = self.agent_caller(peer_pid, "agent gc")?;
                let older_than = params.get("older_than").and_then(Value::as_f64);
                let (candidates, not_permitted) = self.gc_partition(&caller, older_than)?;
                let candidates: Vec<String> = candidates.into_iter().map(|a| a.alias).collect();
                Ok(json!({"candidates": candidates, "not_permitted": not_permitted}))
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
            "epic_stage" => self.rpc_epic_stage(params, peer_pid),
            "project_work_approve" => self.rpc_project_work_approve(params, peer_pid),
            "project_new" => self.rpc_project_new(params, peer_pid),
            "area_ack" => self.rpc_area_ack(params, peer_pid),
            "dispatch_record" => self.rpc_dispatch_record(params, peer_pid),
            "dispatch_send" => self.rpc_dispatch_send(params, peer_pid),
            // CAD-606: operator-only board kickoff — join a worker, then
            // dispatch. Options is the form catalog for the same gate.
            "issue_kickoff" => self.rpc_issue_kickoff(params, peer_pid),
            "issue_kickoff_options" => self.rpc_issue_kickoff_options(params, peer_pid),
            "lane_show" => self.rpc_lane_show(params),
            "lane_ask" => self.rpc_lane_ask(params, peer_pid),
            "lane_instruct" => self.rpc_lane_instruct(params, peer_pid),
            "lane_interrupt" => self.rpc_lane_interrupt(params, peer_pid),
            "lane_stop" => self.rpc_lane_stop(params, peer_pid),
            "lane_unfence" => self.rpc_lane_unfence(params, peer_pid),
            "lane_reassign" => self.rpc_lane_reassign(params, peer_pid),
            "rollout_grant" => self.rpc_rollout_grant(params, peer_pid),
            "rollout_revoke" => self.rpc_rollout_revoke(params, peer_pid),
            "project_work_approvals" => Ok(json!({
                "approvals": self.store.work_approvals()?,
            })),
            "workflow_approve" => self.rpc_workflow_approve(params, peer_pid),
            "app_approve" => self.rpc_app_approve(params, peer_pid),
            "master_dispatch" => self.rpc_master_dispatch(params, peer_pid),
            "question_escalate" => self.rpc_question_escalate(params, peer_pid),
            "agent_file_write" => self.rpc_agent_file_write(params, peer_pid),
            "master_start" => self.rpc_master_start(params, peer_pid),
            "master_summary" => self.rpc_master_summary(params, peer_pid),
            "master_state" => self.rpc_master_state(params),
            "master_models" => self.rpc_master_models(params, peer_pid),
            "master_command" => self.rpc_master_command(params, peer_pid),
            // CAD-574: the operator's Needs-you snooze/dismiss — a row
            // suppression is the operator's call alone.
            "needs_dismiss" => self.rpc_needs_dismiss(params, peer_pid),
            "reports_changed" => self.rpc_reports_changed(peer_pid),
            "report_verdict" => self.rpc_report_verdict(params, peer_pid),
            "answer_route" => self.rpc_answer_route(params, peer_pid),
            "delivery_list" => self.rpc_delivery_list(params),
            "delivery_observe" => self.rpc_delivery_observe(params, peer_pid),
            "delivery_merge" => self.rpc_delivery_merge(params, peer_pid),
            "delivery_decline" => self.rpc_delivery_decline(params, peer_pid),
            "operator_link_mint" => self.rpc_operator_link_mint(params, peer_pid),
            "operator_session_open" => self.rpc_operator_session_open(params, peer_pid),
            "operator_session_check" => self.rpc_operator_session_check(params),
            "board_session_open" => self.rpc_board_session_open(params, peer_pid),
            "board_session_check" => self.rpc_board_session_check(params),
            "operator_session_logout" => self.rpc_operator_session_logout(params),
            "operator_session_stolen" => self.rpc_operator_session_stolen(params),
            "operator_sessions" => self.rpc_operator_sessions(params, peer_pid),
            "operator_secret_rotate" => self.rpc_operator_secret_rotate(params, peer_pid),
            "platform_enroll" => self.rpc_platform_enroll(params, peer_pid),
            "platform_rotate" => self.rpc_platform_rotate(params, peer_pid),
            "platform_revoke" => self.rpc_platform_revoke(params, peer_pid),
            "platform_accounts" => self.rpc_platform_accounts(params),
            "platform_grant" => self.rpc_platform_grant(params, peer_pid),
            "platform_ungrant" => self.rpc_platform_ungrant(params, peer_pid),
            "platform_grants" => self.rpc_platform_grants(params, peer_pid),
            "platform_check" => self.rpc_platform_check(params, peer_pid),
            "platform_defaults" => self.rpc_platform_defaults(params),
            "platform_default_set" => self.rpc_platform_default_set(params, peer_pid),
            "platform_call" => self.rpc_platform_call(params, peer_pid),
            "platform_effects" => self.rpc_platform_effects(params, peer_pid),
            "platform_effect_close" => self.rpc_platform_effect_close(params, peer_pid),
            "platform_outbox" => self.rpc_platform_outbox(params, peer_pid),
            // CAD-580: the wiki v1 store — caller derived by
            // `wiki_caller`; `wiki_as` binds only on an operator
            // connection.
            "wiki_ls" => self.rpc_wiki_ls(params, peer_pid),
            "wiki_read" => self.rpc_wiki_read(params, peer_pid),
            "wiki_write" => self.rpc_wiki_write(params, peer_pid),
            "wiki_put_blob" => self.rpc_wiki_put_blob(params, peer_pid),
            "wiki_mkdir" => self.rpc_wiki_mkdir(params, peer_pid),
            "wiki_mv" => self.rpc_wiki_mv(params, peer_pid),
            "wiki_rm" => self.rpc_wiki_rm(params, peer_pid),
            "wiki_search" => self.rpc_wiki_search(params, peer_pid),
            "wiki_history" => self.rpc_wiki_history(params, peer_pid),
            other => Err(Error::rejected(format!("Unknown method '{other}'"))),
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

    /// The one way daemon code opens the tracker — `Pm::at` over this
    /// daemon's pm_dir plus the lease when `hosted.lease` is on, so a
    /// fenced daemon's tracker writes refuse and a leased daemon's
    /// commits carry `Lease-Epoch`. Every RPC `Pm::at(&self.pm_dir())`
    /// goes through here.
    fn pm(&self) -> Result<crate::issue::Pm> {
        self.pm_at(&self.pm_dir()?)
    }

    /// [`Self::pm`] at an explicit dir — for seams whose signature
    /// already carries the tracker path (checkup's dispatch seam,
    /// `route_answer`'s test calls).
    fn pm_at(&self, pm_dir: &Path) -> Result<crate::issue::Pm> {
        let mut pm = crate::issue::Pm::at(pm_dir)?;
        if let Some(lease) = &self.lease {
            pm.attach_lease(lease.pm_lease());
        }
        Ok(pm)
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

    /// CAD-561: is a `cadence update` draining the fleet right now?
    /// Every actor consults this before claiming its next turn. The
    /// marker file is the truth (its mtime is the heartbeat), so a
    /// marker that disappeared, or stopped being rewritten, lifts the
    /// gate without anyone calling `update_drain off`.
    fn draining(&self) -> bool {
        self.pending_update().is_some()
    }

    /// The pending update, refreshed from disk so a marker written by
    /// the update itself is seen; a file that disappeared or went stale
    /// clears the gate. A fresh marker is only adopted while its writer
    /// is the live rollout lease holder ([`Self::marker_is_the_lease_holders`]):
    /// the file is a plain same-uid file anyone can write, so on its own
    /// it would be a way to quiet the whole fleet (CAD-561 r2).
    fn pending_update(&self) -> Option<crate::update::PendingUpdate> {
        let on_disk = crate::update::pending_update(&self.state_dir)
            .filter(|pending| self.marker_is_the_lease_holders(pending));
        let mut held = self.draining.lock().unwrap();
        *held = on_disk.clone();
        on_disk
    }

    /// Is this marker the live lease holder's? The update claims the
    /// lease before it drains and releases it when it is done, so the
    /// lease row is the proof the marker file cannot carry.
    fn marker_is_the_lease_holders(&self, pending: &crate::update::PendingUpdate) -> bool {
        match crate::rollout::status(&self.state_dir) {
            Ok(status) => {
                status["held"].as_bool() == Some(true)
                    && status["expired"].as_bool() != Some(true)
                    && status["holder"].as_str() == Some(pending.by.as_str())
            }
            Err(_) => false,
        }
    }

    /// The turns an update waits on: every `running`/`submitted`
    /// message on a live actor, with its age. The message row is the
    /// authoritative in-flight signal (the pane probe can read idle
    /// between paste and render).
    fn inflight_turns(&self) -> Result<Vec<Value>> {
        let now = crate::rollout::unix_now();
        let mut rows = Vec::new();
        for agent in self.store.agents()? {
            if !registry::has_actor(&agent.provider, &agent.endpoint_kind) {
                continue;
            }
            let messages = self.store.messages(&agent.alias)?;
            for m in messages {
                if !matches!(m.state.as_str(), "running" | "submitted") {
                    continue;
                }
                let since = m.started.unwrap_or(m.created);
                rows.push(json!({
                    "alias": agent.alias,
                    "message": m.id,
                    "state": m.state,
                    "age_secs": (now - since).max(0.0).round() as u64,
                }));
            }
        }
        rows.sort_by(|a, b| {
            b["age_secs"]
                .as_u64()
                .cmp(&a["age_secs"].as_u64())
                .then_with(|| a["alias"].as_str().cmp(&b["alias"].as_str()))
        });
        Ok(rows)
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

/// A repeated-value param (CAD-437): a string or an array of strings.
/// `None`/absent → empty; anything else is a rejection, never a silent
/// skip.
fn optional_strs(params: &Value, field: &str) -> Result<Vec<String>> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::String(s)) => Ok(vec![s.clone()]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str().map(str::to_string).ok_or_else(|| {
                    Error::invalid(
                        "invalid_request",
                        format!("'{field}' items must be strings"),
                    )
                })
            })
            .collect(),
        Some(_) => Err(Error::invalid(
            "invalid_request",
            format!("'{field}' must be a string or an array of strings"),
        )),
    }
}

/// CAD-574: `thread_send`'s `refs` — at most eight `{kind,id}` subjects
/// of needs-me rows the operator's message cites. The array is
/// normalized to `{kind,id}` pairs only — an extra key refuses the
/// whole call, like the verb's field allowlist. The stored entry
/// carries them so the board can render the citation and the retry
/// check can compare them.
fn thread_refs(value: &Value) -> Result<Value> {
    let arr = value.as_array().ok_or_else(|| {
        Error::rejected("refs must be an array of {\"kind\":…, \"id\":…} subjects")
    })?;
    if arr.is_empty() || arr.len() > 8 {
        return Err(Error::rejected("refs takes 1-8 entries"));
    }
    let mut out = Vec::with_capacity(arr.len());
    for r in arr {
        let Some(obj) = r.as_object() else {
            return Err(Error::rejected(
                "a ref must be a {\"kind\":…, \"id\":…} object",
            ));
        };
        if let Some(key) = obj.keys().find(|k| !matches!(k.as_str(), "kind" | "id")) {
            return Err(Error::rejected(format!(
                "a ref takes kind and id only; field '{key}' is not accepted"
            )));
        }
        let kind = obj.get("kind").and_then(Value::as_str).unwrap_or_default();
        if kind.is_empty()
            || kind.len() > 40
            || !kind
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            return Err(Error::rejected(format!(
                "bad ref kind '{kind}' — [A-Za-z0-9._-], 1-40 chars"
            )));
        }
        let id = obj.get("id").and_then(Value::as_str).unwrap_or_default();
        if id.is_empty() || id.len() > 240 || id.chars().any(char::is_control) {
            return Err(Error::rejected(
                "bad ref id — 1-240 chars, no control characters",
            ));
        }
        out.push(json!({"kind": kind, "id": id}));
    }
    Ok(Value::Array(out))
}

/// Every `values` member in `valid` — a wire peer is untrusted, so the
/// daemon re-checks the vocabulary the CLI already checked.
fn check_values(field: &str, values: &[String], valid: &[&str]) -> Result<()> {
    for v in values {
        if !valid.contains(&v.as_str()) {
            return Err(Error::invalid(
                "invalid_request",
                format!("'{field}' value '{v}' — one of {}", valid.join(" ")),
            ));
        }
    }
    Ok(())
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

/// Every alias some agent names as its upstream — a group root even
/// when it was registered as a worker.
fn upstream_roots(agents: &[Agent]) -> HashSet<String> {
    agents
        .iter()
        .filter_map(|a| agent_upstream(a).map(str::to_string))
        .collect()
}

/// A token long enough to mask inside prose without corrupting other
/// text: every scheme a report accepts (`pty-<gen>-<nonce>`,
/// `claude-<gen>-<nonce>`) is far longer; a short schemeless token
/// (`fake-turn-1`, which `fake-turn-10` contains) is withheld only
/// where it is a whole value.
fn quotable(token: &str) -> bool {
    token.len() >= 16
}

/// Fields that usually name a record: a string under one of them that
/// EXACTLY equals a token's text is an id collision (a caller-chosen
/// message id may be anything) and is left alone. Any other string
/// there is prose and is masked like everywhere else.
const ID_FIELDS: &[&str] = &[
    "id",
    "message",
    "message_id",
    "dispatch_message",
    "request",
    "task",
    "task_id",
    "job",
    "alias",
];

/// Withhold `tokens` from `value` (CAD-375). Only token-bearing places
/// change: a `turn_id` field holding one becomes `null`, and prose
/// quoting a [`quotable`] token has it masked — in any field, except an
/// [`ID_FIELDS`] value that is exactly the token (an id collision). A short schemeless token (a codex
/// `t-1`, a fake `fake-turn-1`) is withheld only as a `turn_id` value —
/// elsewhere the same text is someone else's data.
fn redact_tokens(value: &mut Value, tokens: &[&str]) {
    match value {
        Value::String(text) => {
            if tokens.iter().any(|t| quotable(t) && text.contains(t)) {
                let mut masked = text.clone();
                for token in tokens.iter().filter(|t| quotable(t)) {
                    masked = masked.replace(token, "[turn token withheld]");
                }
                *value = Value::String(masked);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| redact_tokens(v, tokens)),
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if key == "turn_id" {
                    if v.as_str().is_some_and(|t| tokens.contains(&t)) {
                        *v = Value::Null;
                    }
                } else if !(ID_FIELDS.contains(&key.as_str())
                    && v.as_str().is_some_and(|t| tokens.contains(&t)))
                {
                    // Under an id key only an EXACT token text is an id
                    // collision left alone; prose there (a Devin event's
                    // `message`) is masked like anywhere else.
                    redact_tokens(v, tokens);
                }
            }
        }
        _ => {}
    }
}

/// The fail-closed form of [`redact_tokens`] when the running turns
/// cannot be read: every `turn_id` value is withheld.
fn withhold_all_turn_ids(mut value: Value) -> Value {
    fn walk(value: &mut Value) {
        match value {
            Value::Array(items) => items.iter_mut().for_each(walk),
            Value::Object(map) => {
                for (key, v) in map.iter_mut() {
                    if key == "turn_id" {
                        *v = Value::Null;
                    } else {
                        walk(v);
                    }
                }
            }
            _ => {}
        }
    }
    walk(&mut value);
    value
}

/// Request fields that claim an identity — refused on the
/// agent-mutating verbs (CAD-149): the caller is the connection's,
/// never a name the request carries.
const IDENTITY_FIELDS: &[&str] = &[
    "by", "as", "actor", "caller", "operator", "reviewer", "pane", "lane", "pid", "owner",
];

/// Request fields an operator-connection verb refuses rather than reads
/// ([`Shared::operator_connection`]).
const OPERATOR_FIELDS: &[&str] = &[
    "by",
    "operator",
    "actor",
    "alias",
    "lane",
    "pid",
    "pane",
    "recorded_via",
    "attribution",
];

fn reject_operator_fields(verb: &str, params: &Value) -> Result<()> {
    for field in OPERATOR_FIELDS {
        if params.get(field).is_some() {
            return Err(Error::rejected(format!(
                "{verb} authority is connection-bound; request field \
                 '{field}' is not accepted"
            )));
        }
    }
    Ok(())
}

/// The `CADENCE_ALIAS` in `pid`'s environment: `Ok(None)` when unset,
/// `Err(())` when the environment cannot be read — the caller decides
/// (the sandbox-outsider check fails closed on it).
fn proc_env_alias(pid: u32) -> std::result::Result<Option<String>, ()> {
    let env = std::fs::read(format!("/proc/{pid}/environ")).map_err(|_| ())?;
    Ok(env
        .split(|b| *b == 0)
        .filter_map(|kv| std::str::from_utf8(kv).ok())
        .find_map(|kv| kv.strip_prefix("CADENCE_ALIAS="))
        .filter(|a| !a.is_empty())
        .map(str::to_string))
}

/// An inbox reader name (CAD-480): the alias grammar — `[A-Za-z0-9._-]`,
/// 1-80 chars — so a cursor key can never carry path or query syntax
/// into the event log it is recorded on.
fn inbox_reader(params: &Value) -> Result<&str> {
    let reader = optional_str(params, "reader").unwrap_or("default");
    if reader.is_empty()
        || reader.len() > 80
        || !reader
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(Error::rejected(format!(
            "invalid reader name '{reader}' — [A-Za-z0-9._-], 1-80 chars"
        )));
    }
    Ok(reader)
}

fn reject_identity_fields(params: &Value, verb: &str) -> Result<()> {
    for field in IDENTITY_FIELDS {
        if params.get(field).is_some() {
            return Err(Error::rejected(format!(
                "{verb}: caller identity is connection-bound; request field \
                 '{field}' is not accepted"
            )));
        }
    }
    Ok(())
}

/// `{"by", "by_kind"}` for an agent-mutation audit stamp.
fn caller_audit(caller: &AgentCaller) -> Value {
    let (by, by_kind) = caller.audit();
    json!({"by": by, "by_kind": by_kind})
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
    /// CAD-339 report router scan period in seconds: `None` (production)
    /// is 30; `Some(0)` turns it off — test daemons stay hermetic, no
    /// tracker scan.
    pub report_router: Option<u64>,
    /// CAD-477 checkup period in seconds: `None` (production) is
    /// [`checkup::DEFAULT_CHECKUP_SECS`]; `Some(0)` turns it off — test
    /// daemons stay hermetic, no unattended lane judgements.
    pub checkup: Option<u64>,
    /// CAD-484: the idle lane's dispatch seam — `None` (production)
    /// runs `issue::dispatch::run`; a test injects a recorder so the
    /// pick-and-dispatch logic runs without a provider.
    pub checkup_dispatch: Option<Arc<CheckupDispatch>>,
    /// The actor's empty-queue poll — `None` is five seconds. A test
    /// that must prove a delivery came from the wake, not the poll,
    /// sets it past its own wait bound (CAD-391).
    pub idle_poll: Option<Duration>,
    /// Test seam (CAD-471): the in-process owner's stop. Setting it
    /// stops `serve` as the `shutdown` RPC does, without a connection.
    /// A test daemon on a thread of the test runner cannot stop itself
    /// over its socket when the suite runs in an agent pane: the caller
    /// rule refuses `shutdown` from that ancestry (CAD-384), correctly.
    /// Only code in this process holding the flag can set it, so the
    /// gate is untouched. Production leaves it unset.
    pub stop: Option<Arc<AtomicBool>>,
    /// CAD-313: the operator-auth clock (epoch seconds) — `None` is the
    /// wall clock; tests inject one they advance past a link's TTL.
    pub operator_clock: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    /// CAD-506: the platform adapters this daemon proxies through —
    /// `platform` name → adapter. CAD-367/501 register real ones;
    /// tests register the shared-fixture `FakePlatform`.
    pub platforms: effect_rpc::PlatformMap,
    /// CAD-506 test seam: consulted once per accepted effect between
    /// the durable `decided` write and execution. `false` models the
    /// daemon dying inside §5.4 step 5's window — the decision is
    /// recorded, the run never starts, and a restart reconciles the
    /// row. Production leaves it unset (always executes).
    pub effect_execute_gate: Option<effect_rpc::EffectExecuteGate>,
    /// CAD-546: the `local` platform's outbox root —
    /// `platform_outbox` lists it. `platform::local::register` sets it
    /// with the adapter; a daemon without the `local` platform leaves
    /// it `None` and the read refuses.
    pub outbox_dir: Option<PathBuf>,
    /// CAD-538: the hosted lifecycle — `Some` is verbatim (a `Hosted`
    /// with `lease` unset is explicitly off, which is how tests pin
    /// it); `None` reads the tracker's `hosted:` table in pm.yaml.
    pub lease: Option<crate::lease::Hosted>,
    /// CAD-482: arm the test-only caller seam. Honored only in
    /// `test-seam` builds; a daemon asked for it on any other build
    /// refuses to start rather than fall back to ambient identity.
    /// Arming mints `<state>/seam/token` — the credential asserting
    /// callers present — and is refused for the production state dir
    /// or a dir outside the temp root.
    pub test_seam: bool,
}

/// Run the daemon in the foreground until `shutdown` or a signal.
pub fn serve(state_dir: &Path) -> Result<()> {
    // CAD-482: a spawned fixture daemon (`daemon run`/`daemon start`
    // under the test suite) arms the seam from its environment —
    // `env_armed` reads CADENCE_TEST_SEAM and is `false` in every
    // build without the feature, so production never consults it.
    let mut opts = ServeOptions {
        test_seam: crate::test_seam::env_armed(),
        ..ServeOptions::default()
    };
    // CAD-546: the built-in `local` platform rides the production
    // daemon — no network, no credential; its `publish` send still
    // stages and presses like every other adapter's.
    crate::platform::local::register(state_dir, &mut opts);
    serve_with(state_dir, opts)
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
    // CAD-482: the seam confines a fixture before the lease or the
    // store writes anything — a refused arm leaves only the singleton
    // lock behind. A state dir still carrying a minted token re-arms:
    // `daemon restart` respawns this process without the arming env.
    let seam = crate::test_seam::arm_if_requested(
        state_dir,
        opts.test_seam || crate::test_seam::armed(state_dir),
    )?;
    // CAD-538: a configured hosted lease is taken before the marker is
    // consumed and before the store opens — a daemon that cannot hold
    // it refuses here having written nothing but the singleton lock.
    let hosted = hosted_config(&opts)?;
    let lease = crate::lease::acquire(state_dir, &hosted)?;
    let hot = hot_restart_begin(state_dir);
    let shared = Shared::new_leased(state_dir, &opts, hot, lease, seam)?;
    // CAD-313: the operator secret exists from the first start, so an
    // upgrade needs no manual step. An existing file is never touched —
    // a wrong mode is refused at use, naming the fix — and a failure
    // here only disables board logins; it never stops the daemon.
    if let Err(e) = crate::operator_auth::ensure_secret(state_dir) {
        eprintln!("warning: operator secret unavailable, board logins refused: {e}");
    }
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
    // Joined before `serve` returns (CAD-408): the watcher can dispatch,
    // so a stopped daemon must not leave a tick in flight against the
    // store.
    let monitor_watch = {
        let shared = Arc::clone(&shared);
        thread::spawn(move || shared.run_monitor_watch())
    };
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
    // Report router (CAD-339): workers' reports and unanswered
    // questions reach the master's thread. Idle without a master.
    {
        let shared = Arc::clone(&shared);
        thread::spawn(move || shared.run_report_router());
    }
    // CAD-538: the hosted lease heartbeat — joined in the shutdown
    // tail so no renew can race the flush and release.
    let lease_heartbeat = shared.lease.clone().map(|lease| {
        let shared = Arc::clone(&shared);
        thread::spawn(move || shared.run_lease_heartbeat(&lease))
    });
    while !shared.closing.load(Ordering::SeqCst) {
        if opts.stop.as_ref().is_some_and(|s| s.load(Ordering::SeqCst)) {
            shared.begin_closing();
            continue;
        }
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
    // `closing` is set, so the watcher exits within its 100ms sub-step
    // or at the end of the tick it is in. Joining before the actors stop
    // lets a kickoff from that last tick settle into the shutdown marker.
    let _ = monitor_watch.join();
    // CAD-538: the heartbeat must be quiet before the flush and the
    // release — a late renew would rewrite the lease file a release
    // just removed, and a renewal's writes are post-marker state.
    if let Some(heartbeat) = lease_heartbeat {
        let _ = heartbeat.join();
    }
    shared.shutdown();
    // CAD-538: flush before exit — WAL fold + the tracker's staged
    // index — then release the lease LAST: a successor may start the
    // moment it is gone, and this process must have no writes left.
    lease_flush(&shared, flush_budget(&hosted, shared.lease.as_deref()));
    if let Some(lease) = &shared.lease {
        if let Err(e) = lease.release() {
            eprintln!("cadence: lease release failed (expiry covers it): {e}");
        }
    }
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

    /// CAD-324: a pending compaction whose pack cannot be sent is
    /// settled once in the thread — never rebuilt on every later turn.
    #[test]
    fn an_undeliverable_compaction_pack_is_settled_once() {
        let dir = tempfile::tempdir().unwrap();
        let opts = ServeOptions::default();
        let no_pm = dir.path().join("no-pm");
        opts.provider_env
            .set("CADENCE_PM_DIR", no_pm.to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let cwd = dir.path().to_str().unwrap().to_string();
        shared
            .store
            .register_agent(&NewAgent {
                alias: "lead",
                provider: "fake",
                endpoint_kind: "fake",
                role: "worker",
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        shared.store.ensure_thread("lead").unwrap();
        // Only the compaction note: nothing for a pack to carry.
        shared
            .store
            .thread_append(
                "lead",
                store::NewEntry {
                    role: store::ROLE_SYSTEM,
                    kind: store::KIND_MESSAGE,
                    text: "compacted",
                    payload: Some(json!({"event": crate::continuity::COMPACTED_EVENT})),
                    message_id: None,
                },
            )
            .unwrap();
        assert!(shared.store.compaction_pending("lead").unwrap());
        for id in ["k1", "k2"] {
            shared
                .store
                .enqueue("lead", "an ask", None, id, "user")
                .unwrap();
            let Take::Message(m) = shared.store.take_queued("lead").unwrap() else {
                panic!("nothing queued");
            };
            assert_eq!(shared.continuity_prompt("lead", "fake", &m), "an ask");
            shared
                .store
                .finish(&m, "completed", &json!({"text": "ok"}), None)
                .unwrap();
        }
        assert!(!shared.store.compaction_pending("lead").unwrap());
        let settled = shared
            .store
            .thread_entries("lead", 0, 100)
            .unwrap()
            .into_iter()
            .filter(|e| {
                e.payload.as_ref().and_then(|p| p.get("outcome")) == Some(&json!("skipped"))
            })
            .count();
        assert_eq!(settled, 1, "settled once, not per turn");
    }

    /// CAD-324: a terminal pane (the pack would be a paste) and a cloud
    /// session never get a continuity pack — not with a thread, an
    /// earlier turn to carry, a new session due and a compaction
    /// pending. A structured endpoint in the same position does.
    #[test]
    fn continuity_packs_skip_pty_and_cloud_endpoints() {
        let dir = tempfile::tempdir().unwrap();
        let opts = ServeOptions::default();
        // A tracker path that does not exist: never the host's ~/pm.
        let no_pm = dir.path().join("no-pm");
        opts.provider_env
            .set("CADENCE_PM_DIR", no_pm.to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let cwd = dir.path().to_str().unwrap().to_string();
        for (alias, provider, kind, packs) in [
            ("p-claude", "claude", "pty", false),
            ("p-devin", "devin", "pty", false),
            ("p-cursor", "cursor", "pty", false),
            ("c-devin", "devin", "cloud", false),
            ("m-claude", "claude", "managed", true),
            ("w-codex", "codex", "managed-ws", true),
        ] {
            shared
                .store
                .register_agent(&NewAgent {
                    alias,
                    provider,
                    endpoint_kind: kind,
                    role: "worker",
                    cwd: &cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params: None,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
            shared.store.ensure_thread(alias).unwrap();
            shared
                .store
                .enqueue(alias, "an earlier ask", None, &format!("{alias}-0"), "user")
                .unwrap();
            let Take::Message(first) = shared.store.take_queued(alias).unwrap() else {
                panic!("nothing queued for {alias}");
            };
            shared
                .store
                .finish(
                    &first,
                    "completed",
                    &json!({"text": "an earlier answer"}),
                    None,
                )
                .unwrap();
            shared
                .continuity_due
                .lock()
                .unwrap()
                .insert(alias.to_string(), crate::continuity::Reason::New);
            shared
                .store
                .thread_append(
                    alias,
                    store::NewEntry {
                        role: store::ROLE_SYSTEM,
                        kind: store::KIND_MESSAGE,
                        text: "compacted",
                        payload: Some(json!({"event": crate::continuity::COMPACTED_EVENT})),
                        message_id: None,
                    },
                )
                .unwrap();
            shared
                .store
                .enqueue(alias, "the ask", None, &format!("{alias}-1"), "user")
                .unwrap();
            let Take::Message(message) = shared.store.take_queued(alias).unwrap() else {
                panic!("nothing queued for {alias}");
            };
            let prompt = shared.continuity_prompt(alias, kind, &message);
            let delivered = shared
                .store
                .events_tail(alias, 50)
                .unwrap()
                .iter()
                .any(|e| e.kind == crate::continuity::PACK_EVENT);
            if packs {
                assert!(
                    prompt.starts_with(crate::continuity::PACK_BEGIN),
                    "{alias}: {prompt}"
                );
                assert!(prompt.ends_with("the ask"), "{alias}");
                assert!(prompt.contains("an earlier answer"), "{alias}");
                assert!(delivered, "{alias}");
            } else {
                assert_eq!(prompt, "the ask", "{alias}");
                assert!(!delivered, "{alias}");
            }
        }
    }

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

    /// A live lease held by `name` — the update's own claim, taken the
    /// way a fixture asserts the operator (CAD-482).
    fn hold_lease(state: &Path, name: &str) {
        let seam = state.join("seam");
        std::fs::create_dir_all(&seam).unwrap();
        std::fs::write(seam.join("token"), "test-token").unwrap();
        let target = "b".repeat(40);
        let caller = crate::rollout::Caller {
            identity: name.to_string(),
            source: "as",
        };
        crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
            crate::rollout::claim(
                state,
                &crate::rollout::ClaimRequest {
                    caller: &caller,
                    reason: "cadence update",
                    target: Some(&target),
                    ttl: Duration::from_secs(3600),
                    takeover: false,
                    now: crate::rollout::unix_now(),
                },
            )
            .unwrap();
        });
    }

    /// CAD-561 r2: the drain gate. A daemon that comes up while an
    /// update is in flight is drained from boot (the restart in the
    /// middle of an update must not start turns) — but only while the
    /// marker's writer is the LIVE LEASE HOLDER: the file is a plain
    /// same-uid file, so on its own it would be a way to quiet the
    /// whole fleet. The marker's mtime is the heartbeat: an update that
    /// stopped rewriting it — one that died — stops draining within the
    /// bound instead of wedging the fleet.
    #[test]
    fn cad561_a_pending_update_drains_only_while_the_lease_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        // Create the store first, so the marker is read on a real dir.
        Shared::new(state, &ServeOptions::default()).unwrap();
        let pending = crate::update::PendingUpdate {
            phase: "draining".into(),
            target: "b".repeat(40),
            from: Some("a".repeat(40)),
            by: "operator:ada".into(),
            since: crate::rollout::unix_now(),
        };
        // A fresh marker no live lease backs — anyone with state-dir
        // access can write this file — is not authority.
        crate::update::write_pending(state, &pending).unwrap();
        let shared = Shared::new(state, &ServeOptions::default()).unwrap();
        assert!(!shared.draining(), "an unbacked marker never drains");
        // The update claims the lease, then drains: a restart mid-update
        // stays drained from boot.
        hold_lease(state, "operator:ada");
        assert!(shared.draining(), "the lease holder's marker drains");
        assert_eq!(
            shared.pending_update().map(|p| p.target),
            Some("b".repeat(40))
        );
        // Another writer's fresh marker is still not authority.
        let mut forged = pending.clone();
        forged.by = "operator:mallory".into();
        crate::update::write_pending(state, &forged).unwrap();
        assert!(!shared.draining(), "only the lease holder's marker drains");
        crate::update::write_pending(state, &pending).unwrap();
        assert!(shared.draining());
        // The update finishing (marker removed) lifts it.
        crate::update::clear_pending(state);
        assert!(shared.pending_update().is_none());
        assert!(!shared.draining());
        // A marker whose heartbeat stopped (an update that died) is
        // ignored: the file's mtime, not `since`, decides.
        crate::update::write_pending(state, &pending).unwrap();
        let old = std::time::SystemTime::now()
            - Duration::from_secs_f64(crate::update::UPDATE_STALE_SECS + 60.0);
        let file = std::fs::File::options()
            .write(true)
            .open(crate::update::update_file(state))
            .unwrap();
        file.set_modified(old).unwrap();
        drop(file);
        assert!(crate::update::pending_update(state).is_none());
        let shared = Shared::new(state, &ServeOptions::default()).unwrap();
        assert!(!shared.draining(), "a stale marker does not drain");
        // A live marker drains again.
        crate::update::write_pending(state, &pending).unwrap();
        assert!(shared.draining());
        assert_eq!(
            shared.pending_update().map(|p| p.phase),
            Some("draining".into())
        );
    }

    /// CAD-561: the RPC that gates the fleet is the operator's, proved
    /// by the handler itself — a table-level `Handler` rule never admits
    /// an agent, and the handler runs `operator_connection`. Pinned at
    /// the source, the same way the CAD-339 table parse pins methods.
    #[test]
    fn cad561_update_drain_is_operator_gated_in_the_dispatch() {
        let src = include_str!("daemon.rs");
        let arm = src
            .find("\"update_drain\" => {")
            .expect("the update_drain arm");
        let body = &src[arm..arm + 400];
        assert!(
            body.contains("operator_connection(\"update_drain\""),
            "update_drain must prove the operator connection: {body}"
        );
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

    /// CAD-375: a withheld token is `null` where it is the whole value
    /// and masked where prose quotes it; other strings are untouched.
    #[test]
    fn redact_tokens_withholds_values_and_quotes() {
        let tok = "pty-g1-0123456789abcdef";
        let short = "fake-turn-1";
        let mut v = json!({
            "agent": {"awaiting_report": {"turn_id": tok, "message": "m1"}},
            "events": [{"payload": {"turn_id": tok}}, {"text": format!("token {tok} here")}],
            "other": "pty-g1-0123456789abcdeX",
            "fake": short,
            "later": "fake-turn-10",
        });
        redact_tokens(&mut v, &[tok, short]);
        assert!(v["agent"]["awaiting_report"]["turn_id"].is_null(), "{v}");
        assert_eq!(v["agent"]["awaiting_report"]["message"], "m1");
        assert!(v["events"][0]["payload"]["turn_id"].is_null(), "{v}");
        assert_eq!(v["events"][1]["text"], "token [turn token withheld] here");
        assert_eq!(v["other"], "pty-g1-0123456789abcdeX");
        // A short token is withheld only as a `turn_id` value — the
        // same text elsewhere is someone else's data.
        assert_eq!(v["fake"], short, "{v}");
        assert_eq!(v["later"], "fake-turn-10");
        let mut row = json!({"id": "t-1", "turn_id": "t-1"});
        redact_tokens(&mut row, &["t-1"]);
        assert_eq!(row, json!({"id": "t-1", "turn_id": null}));
        // An id equal to a (long) token's text is still an id: only the
        // token-bearing field and prose change (review, CI 35936820152).
        let mut row = json!({"id": tok, "message": tok, "message_id": tok,
                             "entries": [{"message": tok, "text": format!("see {tok}")}],
                             "turn_id": tok});
        redact_tokens(&mut row, &[tok]);
        assert_eq!(row["id"], tok);
        assert_eq!(row["message"], tok);
        assert_eq!(row["message_id"], tok);
        assert_eq!(row["entries"][0]["message"], tok);
        assert_eq!(row["entries"][0]["text"], "see [turn token withheld]");
        assert!(row["turn_id"].is_null(), "{row}");
        // Prose under an id-named key is still masked (a Devin cloud
        // event carries provider text as `message`).
        let mut ev = json!({"event_id": "e1", "message": format!("done, token is {tok}")});
        redact_tokens(&mut ev, &[tok]);
        assert_eq!(ev["message"], "done, token is [turn token withheld]");
        let all = withhold_all_turn_ids(json!({"m": [{"turn_id": "x", "id": "m1"}]}));
        assert!(all["m"][0]["turn_id"].is_null() && all["m"][0]["id"] == "m1");
    }

    /// The notice's live exit works: a reconcile while the actor holds
    /// the turn ends the hold and the next message runs.
    #[test]
    fn cloud_reconcile_during_a_hold_releases_the_actor() {
        let (base, calls, server) = cloud_hold_server();
        let (dir, shared) = shared();
        hold_cloud_turn(&shared, dir.path(), &base, "held-1");
        shared
            .reconcile_message(&json!({"message": "held-1", "status": "interrupted"}))
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

    /// CAD-158 acceptance 7: an urgent message that supersedes a stale
    /// task-bound instruction is still composed (CAD-160) — the new
    /// text, then the objective, then every outstanding criterion.
    #[test]
    fn urgent_superseding_task_message_restates_objective_and_criteria() {
        let (_dir, shared) =
            task_bound_fixture(r#"1) [ ] "sends via V2"; 2) [x] "tests green"; 3) [ ] "docs""#);
        shared
            .rpc_send(
                &json!({"alias": "w1", "text": "old scope", "task": "j1-t1", "message": "m1"}),
            )
            .unwrap();
        shared
            .rpc_send(
                &json!({"alias": "w1", "text": "current scope", "task": "j1-t1",
                               "message": "m2", "priority": "urgent", "supersedes": ["m1"]}),
            )
            .unwrap();
        let m2 = shared.store.message("m2").unwrap().unwrap();
        assert_eq!(m2.priority, store::Priority::Urgent);
        assert_eq!(
            m2.body,
            "current scope — Task j1-t1 (job j1) is still open; this message amends it and \
             does not replace it. Objective: Wire the email provider. Spec: /specs/j1.md. \
             Outstanding criteria: 1) [ ] \"sends via V2\"; 2) [ ] \"docs\"."
        );
        let m1 = shared.store.message("m1").unwrap().unwrap();
        assert_eq!(m1.state, "cancelled");
        assert_eq!(m1.result.unwrap()["superseded_by"], "m2");
        // A nudge is pasted at once — it has no queue to steer.
        let err = shared
            .rpc_send(&json!({"alias": "w1", "text": "x", "nudge": true, "priority": "urgent"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("takes no --priority or --supersedes"), "{err}");
        // A caller-supplied identity is refused, not read.
        let err = shared
            .rpc_send(&json!({"alias": "w1", "text": "x", "priority": "urgent", "by": "pm"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("'by' is not accepted"), "{err}");
        // PR #252 QA N1: a priority that is not a string — the stored
        // rank, a bool, an array — is refused, never queued as normal.
        for (id, bad) in [
            ("p1", json!(1)),
            ("p2", json!(true)),
            ("p3", json!(["urgent"])),
        ] {
            let err = shared
                .rpc_send(&json!({"alias": "w1", "text": "x", "message": id, "priority": bad}))
                .unwrap_err()
                .to_string();
            assert!(err.contains("'priority' must be a string"), "{err}");
            assert!(shared.store.message(id).unwrap().is_none(), "{id} queued");
        }
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

    /// A real process tree for the CAD-385 tests: `sh` (the process a
    /// pane row records) and its `sleep` child (the caller, whose
    /// ancestry reaches `sh`). One process group, killed on drop.
    struct PaneTree {
        sh: std::process::Child,
        caller: u32,
    }

    impl PaneTree {
        fn spawn() -> Self {
            use std::os::unix::process::CommandExt;
            // `; :` keeps sh from exec-ing into sleep: sh stays the parent.
            let sh = std::process::Command::new("sh")
                .args(["-c", "sleep 30; :"])
                .process_group(0)
                .spawn()
                .unwrap();
            let children = format!("/proc/{0}/task/{0}/children", sh.id());
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let caller = loop {
                let found = std::fs::read_to_string(&children)
                    .ok()
                    .and_then(|t| t.split_whitespace().next()?.parse().ok());
                if let Some(pid) = found {
                    break pid;
                }
                assert!(std::time::Instant::now() < deadline, "sleep never started");
                std::thread::sleep(Duration::from_millis(10));
            };
            Self { sh, caller }
        }

        fn pane_pid(&self) -> u32 {
            self.sh.id()
        }
    }

    impl Drop for PaneTree {
        fn drop(&mut self) {
            // Our own group: sh and the sleep it started, nothing else.
            unsafe { libc::kill(-(self.sh.id() as libc::pid_t), libc::SIGKILL) };
            let _ = self.sh.wait();
        }
    }

    /// Record `alias` as a live pty pane at `pid` with `pid_start` as
    /// its recorded process start time (`None`: a row from before v14).
    fn record_pane(shared: &Shared, dir: &Path, alias: &str, pid: u32, pid_start: Option<u64>) {
        register(shared, dir, alias);
        let conn = rusqlite::Connection::open(dir.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "UPDATE agents SET endpoint_kind='pty', generation='g1', pid=?1, pid_start=?2 \
             WHERE alias=?3",
            rusqlite::params![pid, pid_start.map(|s| s as i64), alias],
        )
        .unwrap();
    }

    /// Every pid → alias mapping the daemon's Unix socket uses, for one
    /// caller, rendered comparably.
    fn identity_answers(shared: &Shared, peer: u32) -> Vec<String> {
        vec![
            format!(
                "slot_identity: {:?}",
                shared
                    .slot_identity(peer)
                    .map(|w| w.map(|w| w.lane().to_string()))
            ),
            format!(
                "caller_identity: {:?}",
                shared.caller_identity(peer).map(|c| match c {
                    Caller::NoAgentIdentity => "none".to_string(),
                    Caller::Agent(v) => v.agent.alias.clone(),
                })
            ),
            format!("operator_evidence: {:?}", shared.operator_evidence(peer)),
            format!(
                "derived_caller: {:?}",
                shared.derived_caller("elsewhere", peer, "answer")
            ),
        ]
    }

    /// CAD-385 acceptance 2: a pane row whose pid is now held by an
    /// unrelated process — a real one, recorded with an EARLIER start
    /// time than the process holding the pid now — maps the caller to
    /// no alias in any derivation, answering exactly as when the row is
    /// not registered at all. The same row with the matching start
    /// time does name the caller's pane (the check is not blanket).
    #[test]
    fn cad385_reused_pane_pid_maps_no_alias_like_an_unregistered_process() {
        let tree = PaneTree::spawn();
        let start = crate::peer::proc_starttime(tree.pane_pid()).unwrap();

        let (bare_dir, bare) = shared();
        register(&bare, bare_dir.path(), "elsewhere");
        let unregistered = identity_answers(&bare, tree.caller);
        assert!(unregistered[0].contains("Ok(None)"), "{unregistered:?}");

        let (dir, stale) = shared();
        register(&stale, dir.path(), "elsewhere");
        record_pane(&stale, dir.path(), "pm", tree.pane_pid(), Some(start - 1));
        assert_eq!(identity_answers(&stale, tree.caller), unregistered);

        let (live_dir, live) = shared();
        register(&live, live_dir.path(), "elsewhere");
        record_pane(&live, live_dir.path(), "pm", tree.pane_pid(), Some(start));
        assert!(matches!(
            live.slot_identity(tree.caller).unwrap(),
            Some(SlotWho::Pane { ref lane, .. }) if lane == "pm"
        ));
        assert_eq!(
            live.derived_caller("elsewhere", tree.caller, "answer")
                .unwrap(),
            ("pm".to_string(), "agent")
        );
    }

    /// CAD-385 acceptance 3: a pane row with a pid but no recorded start
    /// time (written before schema v14) fails closed — no derivation
    /// names its alias or lets the caller pass as unregistered: slot and
    /// caller identity refuse naming the remedy, and operator proof
    /// still counts the row as a pane.
    #[test]
    fn cad385_pane_row_without_a_start_time_fails_closed_naming_the_remedy() {
        let tree = PaneTree::spawn();
        let (dir, shared) = shared();
        register(&shared, dir.path(), "elsewhere");
        record_pane(&shared, dir.path(), "pm", tree.pane_pid(), None);

        let slot = shared.slot_identity(tree.caller).err().unwrap().to_string();
        let caller = match shared.caller_identity(tree.caller) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a legacy row must not resolve a caller"),
        };
        let answer = shared
            .derived_caller("elsewhere", tree.caller, "answer")
            .unwrap_err()
            .to_string();
        for refusal in [&slot, &caller, &answer] {
            assert!(refusal.contains("'pm'"), "{refusal}");
            assert!(refusal.contains("process start time"), "{refusal}");
            assert!(refusal.contains("cadence daemon restart"), "{refusal}");
            assert!(refusal.contains("doctor --host"), "{refusal}");
        }
        let proof = shared.operator_evidence(tree.caller).unwrap_err();
        assert!(proof.contains("registered pane 'pm'"), "{proof}");
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

    /// Park `alias` the way the timer leaves it — `stopped`, disabled,
    /// newest marker `agent_auto_stopped`, one message queued — and
    /// return the marker the sweep reads.
    fn auto_stopped_with_mail(shared: &Arc<Shared>, dir: &Path, alias: &str) -> store::Event {
        worker(shared, dir, alias, json!({}));
        shared.store.set_enabled(alias, false).unwrap();
        shared
            .store
            .set_state_detached(alias, "stopped", None)
            .unwrap();
        shared
            .store
            .event_public(alias, AUTO_STOP_EVENT, json!({"idle_secs": 7200.0}))
            .unwrap();
        shared
            .store
            .enqueue(alias, "work", None, &format!("m-{alias}"), "user")
            .unwrap();
        shared
            .store
            .last_event_of(alias, AUTO_STOP_MARKER_KINDS)
            .unwrap()
            .unwrap()
    }

    /// Still parked: stopped, disabled, no actor, and no auto-resume
    /// record of either kind — nothing for a needs-me row to show.
    fn assert_parked_quietly(shared: &Arc<Shared>, alias: &str) {
        let agent = shared.store.agent(alias).unwrap();
        assert_eq!(agent.state, "stopped", "{alias}");
        assert!(!agent.enabled, "{alias}");
        assert!(!shared.lifecycle.lock().unwrap().owned(alias), "{alias}");
        let resume = shared
            .store
            .last_event_of(alias, &[AUTO_RESUME_EVENT, AUTO_RESUME_FAILED_EVENT])
            .unwrap();
        assert!(resume.is_none(), "{alias}: {resume:?}");
    }

    /// CAD-413 (qa-1): an operator stop landing between the sweep's
    /// marker read and the resume wins — finished or still in flight —
    /// and leaves no `agent_auto_resume_failed` behind. The control
    /// proves the same call starts an agent whose marker is unchanged.
    #[test]
    fn auto_resume_loses_to_an_operator_stop_between_check_and_start() {
        let dir = tempfile::tempdir().unwrap();
        let shared = pinned(dir.path(), AutoStopSetting::off());

        // The stop finished after the sweep read `agent_auto_stopped`.
        let seen = auto_stopped_with_mail(&shared, dir.path(), "w-done");
        shared.rpc_stop(&json!({"alias": "w-done"})).unwrap();
        shared.auto_resume("w-done", "m-w-done", 1, &seen);
        assert_parked_quietly(&shared, "w-done");

        // The stop is still in flight: its reservation holds the alias.
        let seen = auto_stopped_with_mail(&shared, dir.path(), "w-flight");
        shared
            .lifecycle
            .lock()
            .unwrap()
            .stopping
            .insert("w-flight".to_string());
        shared.auto_resume("w-flight", "m-w-flight", 1, &seen);
        shared.lifecycle.lock().unwrap().stopping.remove("w-flight");
        assert_parked_quietly(&shared, "w-flight");

        // Control: nothing raced, so the resume records and starts.
        let seen = auto_stopped_with_mail(&shared, dir.path(), "w-ctl");
        shared.auto_resume("w-ctl", "m-w-ctl", 1, &seen);
        assert!(shared.lifecycle.lock().unwrap().owned("w-ctl"));
        let marker = shared
            .store
            .last_event_of("w-ctl", AUTO_STOP_MARKER_KINDS)
            .unwrap()
            .unwrap();
        assert_ne!(marker.kind, AUTO_STOP_EVENT, "{marker:?}");
        assert!(shared
            .store
            .last_event_of("w-ctl", &[AUTO_RESUME_EVENT])
            .unwrap()
            .is_some());
        shared.rpc_stop(&json!({"alias": "w-ctl"})).unwrap();
    }

    /// A resume recorded but never started (the daemon died between the
    /// two) is surfaced as failed on the next sweep, not left waiting
    /// silently — and only once.
    #[test]
    fn auto_resume_recorded_but_never_started_is_reported_failed() {
        let dir = tempfile::tempdir().unwrap();
        let shared = pinned(dir.path(), AutoStopSetting::off());
        auto_stopped_with_mail(&shared, dir.path(), "w1");
        shared
            .store
            .event_public(
                "w1",
                AUTO_RESUME_EVENT,
                json!({"message": "m-w1", "queued": 1}),
            )
            .unwrap();
        shared.auto_resume_tick();
        let failed = shared
            .store
            .last_event_of("w1", AUTO_STOP_MARKER_KINDS)
            .unwrap()
            .unwrap();
        assert_eq!(failed.kind, AUTO_RESUME_FAILED_EVENT, "{failed:?}");
        assert_eq!(failed.payload["message"], "m-w1", "{failed:?}");
        assert!(!shared.lifecycle.lock().unwrap().owned("w1"));
        shared.auto_resume_tick();
        let after = shared
            .store
            .last_event_of("w1", AUTO_STOP_MARKER_KINDS)
            .unwrap()
            .unwrap();
        assert_eq!(after.seq, failed.seq, "reported once: {after:?}");
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

// Re-exports — every name the areas moved keeps resolving
// at `crate::daemon::<Item>`; `pub(super)` lines re-bind moved
// helpers at module scope (CAD-534).
#[allow(unused_imports)]
use identity::{Caller, SlotWho, VerifiedAgent};
pub use serve::HotStart;
#[allow(unused_imports)]
use serve::{
    acquire_singleton, flush_budget, handle_conn, hosted_config, hot_restart_begin, lease_flush,
    process_start_identity, relaunch_agents, resolve_slot_config, write_shutdown_marker,
    CheckupDispatch, SHUTDOWN_FILE,
};
#[allow(unused_imports)]
use timers::{
    apply_auto_stop_view, auto_stop_view, AgentGcState, AgentGcTimer, AutoStopState, AutoStopTimer,
    AutoStopVerdict, AGENT_GC_EVERY, AUTO_STOP_MARKER_KINDS, AUTO_STOP_PASSIVE_KINDS,
};
pub use timers::{
    auto_stop_label, AGENT_GC_FLOOR_SECS, AUTO_RESUME_EVENT, AUTO_RESUME_FAILED_EVENT,
    AUTO_STOP_ATTACH_NOTE, AUTO_STOP_DEFAULT_SECS, AUTO_STOP_EVENT, AUTO_STOP_FLOOR_SECS,
};
#[allow(unused_imports)]
use watch::{
    checkpoint_wal, epoch_secs, fmt_duration, mono_secs, wal_observe_only, wal_pass, Checkpoint,
    StallView, StallWatch, WalWatch, NUDGE_MAX_CHARS, PANE_TREE_KINDS,
};
