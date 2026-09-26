//! CAD-534: `cadence daemon` watch RPC handlers — moved verbatim from src/daemon.rs.

use super::*;

use crate::adapter::Probe;
use std::os::unix::fs::MetadataExt;

/// Stall watch cadence — `silent_secs` stays live without a store read
/// per agent becoming pressure.
const STALL_TICK: Duration = Duration::from_secs(2);

/// Provider WAL watch cadence — at the ~2 MiB/s a runaway devin WAL
/// wrote, a one-minute tick bounds overshoot past `wal_max_bytes` to
/// ~128 MiB.
const WAL_TICK: Duration = Duration::from_secs(60);

/// Persistent monitor reconciliation cadence. Individual registrations
/// carry their own interval; this tick only bounds how soon a due check
/// starts after its deadline.
const MONITOR_TICK: Duration = Duration::from_secs(1);

/// CAD-250 N3: how long a nudge may wait in the queue (a busy or
/// menu-blocked pane) before it is cancelled as stale steering.
const NUDGE_TTL_SECS: u64 = 900;

/// CAD-250 N4: nudges are short steering, not tasks.
pub(super) const NUDGE_MAX_CHARS: usize = 500;

/// `stall_secs` when neither the job nor the agent sets one.
const DEFAULT_STALL_SECS: u64 = 1800;

/// `silent_end_secs` when the agent doesn't set one: ten minutes of
/// probe-verified idle pane on a running message before
/// `turn_silent_end` fires — long enough that a between-tools quiet
/// spell never trips it.
const DEFAULT_SILENT_END_SECS: u64 = 600;

/// `delivery_watch_secs` when the agent doesn't set one: a queued head
/// that has waited this long while the pane probes idle is wedged —
/// delivery should have landed in seconds. The brief's default is 5
/// minutes: at the 60s screen cadence that is ~5 consecutive ready
/// samples, well past any gate retry's own backoff noise.
const DEFAULT_DELIVERY_WATCH_SECS: u64 = 300;

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
pub(super) fn epoch_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Monotonic seconds since an arbitrary process-local epoch — the
/// slot clock. NTP steps and wall-clock jumps cannot age a waiter or
/// expire a hold; the wall epoch rides alongside only for restart
/// persistence.
pub(super) fn mono_secs() -> f64 {
    static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    T0.get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
}

/// Human silence duration for stall notices — `42s`, `12m 3s`, `1h 4m`.
pub(super) fn fmt_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// What the stall watch knows about the agent's in-flight turn.
/// `activity` is the freshest proof of life the daemon has folded in:
/// the turn start, a provider event, a pty screen change, or an open
/// brokered request. `stalled_at` marks a `turn_stalled` episode —
/// `turn_resumed` requires strictly newer activity than it.
pub(super) struct StallWatch {
    /// Running message this state covers; `None` when idle.
    pub(super) message: Option<String>,
    /// Freshest activity instant observed for `message`.
    pub(super) activity: Instant,
    /// The last-activity instant recorded when `turn_stalled` fired —
    /// a resume needs activity strictly newer than this.
    pub(super) stalled_at: Option<Instant>,
    /// When the last screen sample landed — the capture throttle.
    pub(super) sample_at: Option<Instant>,
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
    pub(super) settled: Option<String>,
    /// The settled value before `settled` — history for the novelty
    /// check: a hash equal to a recent settled value must confirm by
    /// repetition, not by merely differing again.
    pub(super) previous: Option<String>,
    /// A hash differing from `settled` seen once, awaiting a second
    /// consecutive sighting to confirm by repetition.
    pub(super) candidate: Option<String>,
    /// Consecutive samples that differed from `settled` — `>= 2` with
    /// a novel hash confirms by persistence.
    pub(super) changed: u8,
    /// A capture in flight on its own thread — at most one per agent,
    /// so a wedged pane leaks one thread and never stalls the ticker.
    /// The sample carries the probe verdict beside the hash: a menu
    /// frame is a human wait (never activity), an idle frame builds
    /// the silent-end streak.
    pub(super) sample_rx: Option<std::sync::mpsc::Receiver<(String, Probe)>>,
    /// Consecutive landed samples whose probe read the pane idle — a
    /// silent end is proven by a streak, never one capture.
    pub(super) idle_samples: u8,
    /// When the current idle streak began — `silent_end_secs`
    /// measures from here.
    pub(super) idle_since: Option<Instant>,
    /// The menu line while the sampled probe shows an approval menu —
    /// `Some` is the menu-open flag: the rising edge fires
    /// `approval_menu` once per menu, and the views read the line.
    pub(super) menu_line: Option<String>,
    /// The menu lines `approval_menu` has already fired for — a small
    /// bounded history, not just the last one: menus that alternate
    /// subjects across non-menu samples must not re-fire on every
    /// re-detection.
    pub(super) menu_evented: std::collections::VecDeque<String>,
    /// `turn_silent_end` already fired for this message — the event
    /// is once per message, never twice.
    pub(super) silent_end_sent: bool,
    /// `delivery_stalled` already fired for the tracked queued head —
    /// once per message, like `silent_end_sent` (CAD-520).
    pub(super) delivery_stalled_sent: bool,
    /// The last landed probe verdict — rides `turn_silent_end`'s
    /// payload as evidence.
    pub(super) last_probe: Option<Probe>,
    /// Stall episodes seen for `message`; each mints a distinct notice
    /// dedupe so a resume + re-stall notifies again.
    pub(super) episodes: u64,
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
            delivery_stalled_sent: false,
            last_probe: None,
            episodes: 0,
        }
    }
}

/// What `stall_view` hands a view: the silence age, the open stall
/// episode, and the sampled pane verdict (menu line, idle-streak age,
/// once-fired silent-end flag).
pub(super) struct StallView {
    pub(super) silent_secs: u64,
    pub(super) stalled: bool,
    pub(super) menu: Option<String>,
    pub(super) ended_secs: Option<u64>,
    pub(super) silent_ended: bool,
    /// CAD-520: a queued head has outlived `delivery_watch_secs` while
    /// the pane probes ready — delivery is wedged, needs a human. The
    /// wedged message id and the probe's verdict ride the view so the
    /// needs-me row names what stalled and what the pane showed.
    pub(super) delivery_stalled: Option<(String, String)>,
}

impl StallView {
    /// Write the view fields onto an agent/task JSON row — the same
    /// keys `agent_list`/`agent_show`/`job show` consumers read.
    pub(super) fn apply(&self, j: &mut Value) {
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
        if let Some((message, verdict)) = &self.delivery_stalled {
            j["delivery_stalled"] = json!({"message": message, "verdict": verdict});
        }
    }
}

impl Shared {
    // ---- Stall watch: report silent turns, never touch them (CAD-52) ----

    /// Sample owned agents on a slow cadence until shutdown. The stall
    /// watch itself only ever emits events and notices — it never
    /// interrupts, re-dispatches or fences anything it observes. The
    /// PM checkup rides the same loop on its own slower cadence; it
    /// alone records a lane outcome and may nudge or escalate.
    pub(super) fn run_stall_watch(self: &Arc<Self>) {
        let mut inbox_swept: Option<Instant> = None;
        let mut nudges_swept: Option<Instant> = None;
        let mut checkup_at: Option<Instant> = None;
        let mut events_rolled: Option<Instant> = None;
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
            // CAD-316: delivery bookkeeping past a week folds into
            // per-alias counts — hourly, allowlisted kinds only. A full
            // batch means backlog remains: the next tick takes the next
            // chunk, so the store lock is released between chunks.
            if events_rolled.is_none_or(|at| at.elapsed() >= EVENT_ROLLUP_EVERY) {
                let cutoff = epoch_secs() - crate::store::EVENT_ROLLUP_AGE_SECS;
                let batch = crate::store::EVENT_ROLLUP_BATCH;
                events_rolled = match self.store.roll_up_delivery_events(cutoff, batch) {
                    Ok(folded) if folded >= batch => None,
                    Ok(_) => Some(Instant::now()),
                    Err(e) => {
                        eprintln!("event rollup: {e}");
                        Some(Instant::now())
                    }
                };
            }
            // CAD-199: off unless configured; sweeps at most hourly.
            self.agent_gc_tick();
            // CAD-96: idle auto-stop — checks at most once a minute.
            self.auto_stop_tick();
            // CAD-413: work queued for an auto-stopped agent resumes it.
            self.auto_resume_tick();
            // CAD-477: the PM checkup — visits each worker holding a
            // running or queued turn and records one outcome.
            if let Some(every) = self.checkup_every {
                if checkup_at.is_none_or(|at| at.elapsed() >= every) {
                    self.checkup_tick();
                    checkup_at = Some(Instant::now());
                }
            }
            std::thread::sleep(STALL_TICK);
        }
    }

    /// Reconcile daemon-owned monitor registrations without waking a
    /// provider. Each check advances its durable cursor together with any
    /// deduplicated local alerts; failures remain visible as `degraded`.
    /// Opted-in registrations then run the same guarded dispatch helper as
    /// the explicit RPC. A guard refusal becomes one durable task alert and
    /// is retried only on the next monitor interval.
    pub(super) fn run_monitor_watch(self: &Arc<Self>) {
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
    pub(super) fn run_wal_watch(self: &Arc<Self>) {
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
        let mut delivery_fire: Option<(String, u64, Probe)> = None;
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
                // CAD-520: the delivery watchdog. The tracked head is a
                // still-queued message while the pane probes *idle* —
                // delivery should already have happened, so past
                // `delivery_watch_secs` that is a wedge: one
                // `delivery_stalled` event per tracked message, never a
                // refire while it sits. A busy pane or an open menu
                // never trips it — those waits are real.
                if running.is_none() && !w.delivery_stalled_sent && probe.idle {
                    if let Some(m) = tracked.as_ref() {
                        let bound = self.delivery_watch_budget(&agent);
                        let queued_secs = epoch_secs() - m.created;
                        if bound > 0 && queued_secs >= bound as f64 {
                            w.delivery_stalled_sent = true;
                            delivery_fire = Some((m.id.clone(), queued_secs as u64, probe.clone()));
                        }
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
                // CAD-468: the edge is also the reminder — the pane is
                // provably idle while the turn still runs, so prompt the
                // worker in band before the report bound forces the
                // unknown.
                self.report_reminder(&agent, m);
            }
        }
        if let Some((msg_id, queued_secs, probe)) = delivery_fire {
            self.delivery_stalled_fired(&agent, tracked.as_ref(), &msg_id, queued_secs, &probe);
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
    pub(super) fn silent_end_budget(&self, agent: &Agent) -> u64 {
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

    /// CAD-520: the bound for `delivery_stalled` — how long a queued
    /// head may sit while the pane probes idle before the watchdog
    /// calls it wedged. The agent's `delivery_watch_secs` param, else
    /// the default; `0` disables.
    fn delivery_watch_budget(&self, agent: &Agent) -> u64 {
        agent
            .params
            .as_ref()
            .and_then(|p| p.get("delivery_watch_secs"))
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(DEFAULT_DELIVERY_WATCH_SECS)
    }

    /// `(job_id, task_id)` scope for a message's stall events — the
    /// `job events` view is one query over those columns.
    pub(super) fn message_scope<'m>(
        &self,
        message: &'m Message,
    ) -> (Option<String>, Option<&'m str>) {
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

    /// `delivery_stalled` (CAD-520): the queued head has outlived
    /// `delivery_watch_secs` while the pane keeps probing ready —
    /// evidence the delivery path wedged (a dead actor, a gate that
    /// can never admit). One event per tracked message; the probe
    /// verdict rides along as the "ready" proof.
    fn delivery_stalled_fired(
        &self,
        agent: &Agent,
        tracked: Option<&Message>,
        msg_id: &str,
        queued_secs: u64,
        probe: &Probe,
    ) {
        let (job_id, task_id) = tracked
            .map(|m| self.message_scope(m))
            .unwrap_or((None, None));
        let _ = self.store.event_public_scoped(
            &agent.alias,
            "delivery_stalled",
            json!({
                "message": msg_id,
                "queued_secs": queued_secs,
                "bound_secs": self.delivery_watch_budget(agent),
                "probe": probe.to_json(),
            }),
            job_id.as_deref(),
            task_id,
        );
        self.wake();
    }

    /// CAD-468: at the silent-end edge, prompt the worker in band — one
    /// daemon-originated nudge per turn carrying the exact
    /// `cadence message result` command. It is turnless like any nudge;
    /// CAD-565 binds it to this turn at claim, so it delivers only if
    /// the pane regains a steerable input while the turn still lives —
    /// on an idle pane it gate-waits and is skipped when the turn ends,
    /// never landing as a later turn's input. The
    /// `(nudge, "report-reminder:<message>:<turn>")` id dedupes a
    /// restart, a re-probe or a second edge: once per turn, never twice.
    /// A report that already landed skips it; the reminder itself never
    /// resolves the turn and never fences — the unchanged
    /// `report_timeout_secs` bound still decides `unknown`.
    ///
    /// The reminder never carries the live turn token: the body lands
    /// in pane scrollback and the durable row, readable by any same-uid
    /// peer — a pasted token would let one forge the report. The worker
    /// fetches it itself: `cadence self` prints id and token.
    pub(super) fn report_reminder(&self, agent: &Agent, message: &Message) {
        // Re-read: a report can land between the probe's verdict and
        // this write — only a still-running turn is reminded.
        let Ok(Some(m)) = self.store.message(&message.id) else {
            return;
        };
        if m.state != "running" {
            return;
        }
        let bound = store::report_timeout_secs(agent.params.as_ref());
        let mut text = format!(
            "Pane idle with a turn still open — report it: \
             `cadence message result {} --token <token from `cadence self`> \
             --text '<summary>'`.",
            m.id
        );
        if bound > 0 {
            text.push_str(&format!(
                " No report inside report_timeout_secs={bound} leaves the \
                 turn `unknown` for the operator to judge."
            ));
        }
        // The token still keys the dedupe (hashed into the `sys-nudge-`
        // id, never pasted): one reminder per TURN, so a turn that ends
        // and a new one that stalls each get their own.
        let token = m.turn_id.as_deref().unwrap_or("no-turn");
        let key = format!("report-reminder:{}:{token}", m.id);
        if let Err(e) = self.daemon_message(&agent.alias, store::NUDGE_SOURCE, &key, &text) {
            tracing::warn!(
                event = "report_reminder_failed",
                alias = agent.alias.as_str(),
                message = m.id.as_str(),
                error = e.to_string()
            );
        }
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
    pub(super) fn stall_view(&self, alias: &str) -> Option<StallView> {
        let running = self.store.running_message(alias).ok()?;
        let ctl = self.lifecycle.lock().unwrap().agents.get(alias)?.clone();
        let w = ctl.stall.lock().unwrap();
        let Some(running) = running else {
            // A queued head behind an open menu or past the delivery
            // watchdog bound: only those two facts are meaningful —
            // nothing has started or ended. The stall flag is
            // delivered-pane state too: once the probe reads busy the
            // row clears — something is moving.
            let stalled = (w.delivery_stalled_sent
                && w.last_probe.as_ref().is_some_and(|p| p.idle))
            .then(|| {
                (
                    w.message.clone().unwrap_or_default(),
                    w.last_probe
                        .as_ref()
                        .map(|p| p.reason.clone())
                        .unwrap_or_default(),
                )
            });
            if w.menu_line.is_none() && stalled.is_none() {
                return None;
            }
            return Some(StallView {
                silent_secs: 0,
                stalled: false,
                menu: w.menu_line.clone(),
                ended_secs: None,
                silent_ended: false,
                delivery_stalled: stalled,
            });
        };
        if w.message.as_deref() == Some(running.id.as_str()) {
            return Some(StallView {
                silent_secs: w.activity.elapsed().as_secs(),
                stalled: w.stalled_at.is_some(),
                menu: w.menu_line.clone(),
                ended_secs: w.idle_since.map(|t| t.elapsed().as_secs()),
                silent_ended: w.silent_end_sent,
                delivery_stalled: None,
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
            delivery_stalled: None,
        })
    }
}

// ---------- provider WAL auto-checkpoint (CAD-132) ----------

/// What a TRUNCATE attempt learned. Contention under
/// `busy_timeout(0)` arrives as an error, and a non-WAL database
/// reports `(0, -1, -1)` — both mean "leave it for the next tick",
/// so they share one variant.
pub(super) enum Checkpoint {
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
pub(super) fn checkpoint_wal(db: &Path) -> Checkpoint {
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
pub(super) fn wal_observe_only(configured: bool, sandbox: Option<&str>) -> bool {
    configured || sandbox.is_some()
}

/// Keep this many `daemon` events — the stream has no agents row, so
/// agent-removal pruning never reaches it; unbounded growth in a
/// feature whose purpose is bounding growth would be embarrassing.
const DAEMON_EVENTS_KEEP: i64 = 200;

/// How often the stall watch folds week-old delivery events (CAD-316).
const EVENT_ROLLUP_EVERY: Duration = Duration::from_secs(3600);

/// Cross-tick watch state: `pending` dedupes dry-run events (one per
/// db per crossing, cleared when it drops under the limit), and
/// `truncated_seen` keeps a stuck scan from re-eventing every tick.
#[derive(Default)]
pub(super) struct WalWatch {
    pub(super) pending: HashSet<PathBuf>,
    pub(super) truncated_seen: bool,
}

/// One watch pass over `roots`: each `*-wal` over `max_bytes`, quiet
/// for `quiet_secs`, owned by this uid and not a symlink, whose
/// provider has no in-flight *cadence* turn gets checkpointed — the
/// busy gate is a courtesy over cadence's own store; SQLite's locking
/// and the quiet-window are what protect data. `dry_run` records
/// `wal_checkpoint_pending` instead of touching the db.
pub(super) fn wal_pass(
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
pub(super) const PANE_TREE_KINDS: &[&str] = &[
    "pane_root",
    "pane_root_unrecorded",
    "pane_tree_reaped",
    "pane_tree_reap_refused",
];
