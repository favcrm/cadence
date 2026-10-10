//! CAD-1020: daemon-owned driver for scheduled social publishes.
//!
//! One daemon thread — spawned beside `run_monitor_watch` in `serve`,
//! joined before `Shared::shutdown` — claims due frozen intents, sends
//! them through the registered [`PublishSender`] and reconciles
//! `processing` rows through the provider's status door. Exactly-once
//! comes from the store (`social_publish_claim_due`'s single-tx
//! `queued→processing` CAS) plus the door's AOS-94 key dedupe; the
//! driver adds no second path.
//!
//! Guards (the contract in PR #693 names each one's mutation test):
//! - lease-holder-only: the driver checks the hosted-lease fence at the
//!   top of every tick and between claim→send and send→report — the
//!   pre-send check stops the HTTP call the store fence cannot cover.
//!   Unleased daemons skip the check (`Option`-conditional).
//! - `sender_not_configured`: without a sender the tick records the
//!   status and sleeps; nothing claims.
//! - MAX_LATENESS: a due row older than the bound is claimed and held
//!   ("missed publish window"), never sent — the first canary start
//!   does not dump a stale backlog.
//! - Preflight before claim: `PublishSender::preflight` stages the
//!   binding at the door first. `Uncertain` leaves the row queued
//!   (per-item backoff); `Refused` claims and reports; only
//!   `Approved` claims and executes.
//! - `processing` rows recover through `status`, never `execute`.
//!   `unknown_key` past `UNKNOWN_KEY_HOLD_SECS` escalates to `held`.
//!   `reconnect_needed` reports `refused` directly (noting it would be
//!   rejected evidence and spin the tick).
//! - Per-intent transient errors land in a bounded in-memory map
//!   surfaced as `intent.driver.last_error`; terminal reasons persist
//!   in the row's receipt (`refused`/`held`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::*;
use crate::platform::agenticos_external::publish::{Preflight, PublishState, Refusal};

/// Default tick interval — `CADENCE_SOCIAL_PUBLISH_INTERVAL_SECS`
/// overrides it, clamped to [`MIN_INTERVAL_SECS`]..[`MAX_INTERVAL_SECS`].
pub(super) const DEFAULT_INTERVAL_SECS: u64 = 30;
pub(super) const MIN_INTERVAL_SECS: u64 = 5;
pub(super) const MAX_INTERVAL_SECS: u64 = 60;
/// At most this many claims execute per tick — a backlog drains over
/// ticks, never in one unbounded sweep.
pub(super) const PER_TICK_CAP: usize = 8;
/// Processing intents reconciled per tick (bounded separately).
pub(super) const RECONCILE_CAP: usize = 16;
/// Per-intent backoff bounds on provider-ambiguous outcomes.
pub(super) const BACKOFF_MIN_SECS: u64 = 10;
pub(super) const BACKOFF_MAX_SECS: u64 = 300;
/// Default lateness bound: a due row older than this is held, never
/// sent — a first start must not dump the stale backlog.
pub(super) const DEFAULT_MAX_LATENESS_SECS: i64 = 900;
/// A `processing` row whose status keeps answering `unknown_key` is
/// escalated to `held` once the row's age since `updated` passes this
/// bound (≥ 10 × DOOR_TIMEOUT — the door never saw the key, so no
/// status reconcile can ever succeed).
pub(super) const UNKNOWN_KEY_HOLD_SECS: i64 = 320;
/// The transient-error map's bound — one entry per in-flight intent.
const LAST_ERROR_CAP: usize = 256;
/// A tick's wall-time bound: `closing` and the fence are checked before
/// every claim, but a string of in-flight door calls could still stretch
/// a tick — the sweep + claim work is cut off at this deadline so one
/// tick can never wedge the loop (the remaining rows run next tick).
pub(super) const TICK_DEADLINE_SECS: u64 = 5 * 60;
/// Interval env (seconds, clamped).
pub const INTERVAL_ENV: &str = "CADENCE_SOCIAL_PUBLISH_INTERVAL_SECS";
/// Lateness env (seconds).
pub const LATENESS_ENV: &str = "CADENCE_SOCIAL_PUBLISH_MAX_LATENESS_SECS";

/// The driver's live knobs and observable status. Lives on `Shared`;
/// read by `health`/`daemon_info` and the list/show envelopes.
pub(super) struct Driver {
    /// Resolved tick interval (test seam overrides in ms, bypassing
    /// the seconds clamp like `crm_send_interval_ms`).
    pub interval: Duration,
    /// Test seam: parks the loop (see `ServeOptions::social_publish_driver_off`).
    pub off: bool,
    /// Lateness bound for claim-vs-hold.
    pub max_lateness_secs: i64,
    /// The driver's clock (epoch seconds) — wall unless a test pins it.
    pub clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// One driver thread per daemon — the spawn guard.
    started: AtomicBool,
    /// Lib tests only: runs between a committed claim and its send.
    #[cfg(test)]
    after_claim: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Observable status block for `health`/envelopes.
    state: Mutex<DriverState>,
    /// Transient per-intent errors — in-memory, bounded, cleared when
    /// the row leaves `queued`/`processing` (terminal reasons persist
    /// on the row's receipt instead).
    last_errors: Mutex<HashMap<String, String>>,
}

#[derive(Default)]
struct DriverState {
    status: &'static str,
    last_tick: Option<f64>,
    next_tick: Option<f64>,
    /// Latest tick-level error (fence trip, store failure) — Refusal
    /// code+detail only, never a URL, credential path or bearer.
    last_error: Option<String>,
}

impl Driver {
    /// Resolve the driver's knobs from `ServeOptions` + env. The test
    /// seam `social_publish_driver_ms` bypasses the seconds clamp so driver
    /// tests run the loop hot; production env config stays second-bound.
    pub(super) fn new(opts: &ServeOptions) -> Self {
        let interval = match opts.social_publish_driver_ms {
            Some(ms) => Duration::from_millis(ms.max(1)),
            None => Duration::from_secs(
                std::env::var(INTERVAL_ENV)
                    .ok()
                    .and_then(|raw| raw.parse::<u64>().ok())
                    .unwrap_or(DEFAULT_INTERVAL_SECS)
                    .clamp(MIN_INTERVAL_SECS, MAX_INTERVAL_SECS),
            ),
        };
        let max_lateness_secs = std::env::var(LATENESS_ENV)
            .ok()
            .and_then(|raw| raw.parse::<i64>().ok())
            .unwrap_or(DEFAULT_MAX_LATENESS_SECS)
            .max(0);
        Self {
            interval,
            off: opts.social_publish_driver_off,
            max_lateness_secs,
            clock: opts
                .social_publish_driver_clock
                .clone()
                .unwrap_or_else(|| Arc::new(crate::issue::time::now_epoch)),
            started: AtomicBool::new(false),
            #[cfg(test)]
            after_claim: opts.social_publish_driver_after_claim.clone(),
            state: Mutex::new(DriverState::default()),
            last_errors: Mutex::new(HashMap::new()),
        }
    }

    /// Spawn-once guard — a second caller is refused, so two driver
    /// loops can never race inside one daemon.
    pub(super) fn start(&self) -> bool {
        !self.started.swap(true, Ordering::SeqCst)
    }

    /// Status block for `health`/`daemon_info` and the list envelope.
    pub(super) fn status_json(&self) -> Value {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        json!({
            "status": if state.status.is_empty() { "idle" } else { state.status },
            "off": self.off,
            "interval_secs": self.interval.as_secs_f64(),
            "max_lateness_secs": self.max_lateness_secs,
            "last_tick": state.last_tick,
            "next_tick": state.next_tick,
            "last_error": state.last_error,
        })
    }

    fn set_status(&self, status: &'static str, next_tick: Option<f64>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.status = status;
        state.last_tick = Some(epoch_secs());
        state.next_tick = next_tick;
    }

    fn fail(&self, what: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.status = "error";
        state.last_error = Some(what.to_owned());
        state.last_tick = Some(epoch_secs());
    }

    /// Record a transient per-intent error (`code: detail` only).
    fn note_error(&self, intent_id: &str, refusal: &Refusal) {
        let mut map = self.last_errors.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= LAST_ERROR_CAP {
            // Bound: evict an arbitrary entry (HashMap order) — the map
            // is best-effort diagnostics, never a queue.
            if let Some(first) = map.keys().next().cloned() {
                map.remove(&first);
            }
        }
        map.insert(
            intent_id.to_owned(),
            format!("{}: {}", refusal.code, refusal.detail),
        );
    }

    fn clear_error(&self, intent_id: &str) {
        self.last_errors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(intent_id);
    }

    /// Per-intent transient error, surfaced as `driver.last_error` in
    /// the show/list envelopes.
    pub(super) fn last_error_for(&self, intent_id: &str) -> Option<String> {
        self.last_errors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(intent_id)
            .cloned()
    }
}

/// Sleep in `closing`-aware sub-steps — a stop lands within ~200ms,
/// never after a whole tick.
fn sleep_until(closing: &AtomicBool, deadline: Instant) {
    while !closing.load(Ordering::SeqCst) && Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(left.min(Duration::from_millis(200)));
    }
}

impl Shared {
    /// The driver loop: tick → reconcile processing → claim due → sleep.
    /// Spawned once in `serve`; a second spawn is refused by
    /// [`Driver::start`]. Joined before `Shared::shutdown` so a tick
    /// never outlives the daemon — a SIGKILL in-flight is safe by
    /// construction (the row is left `processing` for reconcile).
    pub(super) fn run_social_publish_driver(self: &Arc<Self>) {
        if !self.social_publish_driver.start() {
            eprintln!("cadence: social publish driver already running — second spawn refused");
            return;
        }
        let driver = &self.social_publish_driver;
        let mut reconcile_after: Option<String> = None;
        let mut backoff: HashMap<String, i64> = HashMap::new();
        while !self.closing.load(Ordering::SeqCst) {
            // Lease-holder-only: an unleased daemon skips the check; a
            // fenced one parks the tick before any claim or send.
            if let Some(reason) = self.lease.as_ref().and_then(|l| l.fence().check()) {
                driver.fail(&format!("lease fenced: {reason}"));
                sleep_until(
                    &self.closing,
                    Instant::now() + driver.interval.min(Duration::from_secs(5)),
                );
                continue;
            }
            if self.social_publish_sender.is_none() {
                driver.set_status("sender_not_configured", None);
                sleep_until(&self.closing, Instant::now() + driver.interval);
                continue;
            }
            if driver.off {
                driver.set_status("off", None);
                sleep_until(&self.closing, Instant::now() + driver.interval);
                continue;
            }
            // One wall bound covers the whole tick: reconcile's status
            // reads and the claims after them.
            let deadline = Instant::now() + Duration::from_secs(TICK_DEADLINE_SECS);
            self.reconcile_processing(&mut reconcile_after, &mut backoff, deadline);
            self.claim_due(&mut backoff, deadline);
            driver.set_status("ok", Some(epoch_secs() + driver.interval.as_secs_f64()));
            sleep_until(&self.closing, Instant::now() + driver.interval);
        }
    }

    /// Reconcile up to `RECONCILE_CAP` `processing` rows through the
    /// status door — never `execute`. A rotating `after` cursor walks
    /// the full set over ticks so a stuck head can't starve the tail.
    fn reconcile_processing(
        self: &Arc<Self>,
        after: &mut Option<String>,
        backoff: &mut HashMap<String, i64>,
        deadline: Instant,
    ) {
        let Some(sender) = self.social_publish_sender.clone() else {
            return;
        };
        let rows = match self
            .store
            .social_publish_processing(RECONCILE_CAP, after.as_deref())
        {
            Ok(rows) => rows,
            Err(e) => {
                self.social_publish_driver.fail(&e.to_string());
                return;
            }
        };
        if rows.is_empty() {
            *after = None;
            return;
        }
        if rows.len() >= RECONCILE_CAP {
            *after = rows.last().map(|(id, _, _)| id.clone());
        } else {
            *after = None;
        }
        for (id, key, updated) in rows {
            if self.closing.load(Ordering::SeqCst) || Instant::now() >= deadline {
                return;
            }
            // Lease-holder-only between items — a mid-tick loss stops
            // the next HTTP read.
            if self
                .lease
                .as_ref()
                .and_then(|l| l.fence().check())
                .is_some()
            {
                return;
            }
            // Per-intent backoff: ambiguous outcomes skip this row until
            // its own delay lapses — one bad intent can't stall others.
            if backoff
                .get(&id)
                .is_some_and(|until| (self.social_publish_driver.clock)() < *until)
            {
                continue;
            }
            match sender.status(&key) {
                Ok(outcome) => {
                    self.social_publish_driver.clear_error(&id);
                    backoff.remove(&id);
                    match outcome.state {
                        PublishState::Posted | PublishState::Processing => {
                            if let Err(e) = self
                                .store
                                .social_publish_note_evidence(&id, &outcome.evidence_json())
                            {
                                self.social_publish_driver
                                    .note_error(&id, &Refusal::new("refused", e.to_string()));
                            } else if outcome.state == PublishState::Posted {
                                // A posted verdict closes the row —
                                // report replays the stored byte-exact
                                // upstream evidence through the gate.
                                self.end_report(&id);
                            }
                        }
                        // The door demands a new grant — a definitive
                        // end, reported refused without noting evidence
                        // (the note path rejects `reconnect_needed` and
                        // would spin the tick on errors).
                        PublishState::Refused | PublishState::ReconnectNeeded => {
                            if let Err(e) = self.store.social_publish_report(
                                &id,
                                "refused",
                                &json!({"error": format!("upstream status {}", outcome.state.as_str())}),
                            ) {
                                self.social_publish_driver
                                    .note_error(&id, &Refusal::new("refused", e.to_string()));
                            }
                        }
                    }
                }
                Err(refusal) if refusal.code == "unknown_key" => {
                    // The door never saw this key — a crash between claim
                    // and send. Past the bound the row can never
                    // reconcile: escalate to `held` for a human decision
                    // rather than spinning forever.
                    let now = (self.social_publish_driver.clock)();
                    if now as f64 - updated >= UNKNOWN_KEY_HOLD_SECS as f64 {
                        backoff.remove(&id);
                        self.social_publish_driver.clear_error(&id);
                        self.end_report_at(
                            &id,
                            "held",
                            &json!({"reason": "the door has no record of this publish — check before rescheduling"}),
                        );
                    } else {
                        self.social_publish_driver.note_error(&id, &refusal);
                    }
                }
                Err(refusal) => {
                    // Transient status failure: record and per-intent
                    // backoff — the row stays `processing`. Each repeat
                    // failure doubles the delay toward the cap so a
                    // flapping row backs off, never hot-loops.
                    self.social_publish_driver.note_error(&id, &refusal);
                    let delay = backoff
                        .get(&id)
                        .map(|until| {
                            let prev = (*until - (self.social_publish_driver.clock)())
                                .max(BACKOFF_MIN_SECS as i64)
                                as u64;
                            (prev * 2).min(BACKOFF_MAX_SECS)
                        })
                        .unwrap_or(BACKOFF_MIN_SECS);
                    backoff.insert(id, (self.social_publish_driver.clock)() + delay as i64);
                }
            }
        }
    }

    /// Attempt up to `PER_TICK_CAP` due intents, oldest first. Rows that
    /// are backing off are skipped and do not count, so ambiguous rows
    /// at the head never stall the due rows behind them
    /// (head-of-line blocking, spec finding 3). Preflight runs BEFORE
    /// each claim: ambiguity leaves the row queued under per-intent
    /// backoff (grown 10s→300s); definitive refusal claims and reports.
    /// A row due past `max_lateness` is claimed and held ("missed
    /// publish window") — never sent.
    fn claim_due(self: &Arc<Self>, backoff: &mut HashMap<String, i64>, deadline: Instant) {
        let driver = &self.social_publish_driver;
        let sender = self.social_publish_sender.clone();
        let Some(sender) = sender else { return };
        // Page through every due row: backing-off rows are skipped
        // without counting, so no number of them blocks the rows behind.
        let mut attempts = 0;
        let mut after: Option<(i64, String)> = None;
        loop {
            let at = after.as_ref().map(|(due, id)| (*due, id.as_str()));
            let page = match self
                .store
                .social_publish_due_batch((driver.clock)(), PER_TICK_CAP, at)
            {
                Ok(page) => page,
                Err(e) => {
                    driver.fail(&e.to_string());
                    return;
                }
            };
            let Some(last) = page.last() else { return };
            after = Some((
                last["intent"]["due_epoch"].as_i64().unwrap_or(0),
                last["intent"]["intent_id"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned(),
            ));
            for due in page {
                if self.closing.load(Ordering::SeqCst) || Instant::now() >= deadline {
                    return;
                }
                // Mid-tick lease check — before every claim, so a lost lease
                // stops the next claim AND the send inside it.
                if self
                    .lease
                    .as_ref()
                    .and_then(|l| l.fence().check())
                    .is_some()
                {
                    driver.fail("lease fenced mid-tick");
                    return;
                }
                let intent = &due["intent"];
                let id = intent["intent_id"].as_str().unwrap_or("").to_owned();
                // Per-intent backoff: a backing-off row is skipped, not
                // returned-on — later due rows still dispatch this tick.
                if backoff
                    .get(&id)
                    .is_some_and(|until| (driver.clock)() < *until)
                {
                    continue;
                }
                // The cap counts claim attempts, not rows looked at.
                if attempts == PER_TICK_CAP {
                    return;
                }
                attempts += 1;
                let frozen = &intent["frozen"];
                // The lateness bound reads the COLUMN's `due_epoch` (the
                // claim SQL's own source) — a forged column can't hide
                // behind the still-frozen `frozen.due_epoch`.
                let due_epoch = intent["due_epoch"]
                    .as_i64()
                    .or_else(|| frozen["due_epoch"].as_i64())
                    .unwrap_or(0);
                // Stale backlog: claim the peeked row and hold, never send.
                // Lateness is judged on the clock now, after any slow door
                // calls earlier in this tick, never on the tick's start.
                let now = (driver.clock)();
                if now - due_epoch > driver.max_lateness_secs {
                    match self.claim_and_end(
                        &due,
                        "held",
                        &json!({"reason": "missed publish window"}),
                    ) {
                        Ok(true) => {}
                        Ok(false) => continue,
                        Err(e) => {
                            driver.fail(&e.to_string());
                            return;
                        }
                    }
                    continue;
                }
                // Explicit-mode intents (no artifact freeze) are held for the
                // operator path — `material_current` can't re-prove them.
                if frozen["artifact_id"].is_null() {
                    match self.claim_and_end(
                        &due,
                        "held",
                        &json!({"reason": "explicit-mode publish needs operator dispatch"}),
                    ) {
                        Ok(true) | Ok(false) => {}
                        Err(e) => {
                            driver.fail(&e.to_string());
                            return;
                        }
                    }
                    continue;
                }
                // Preflight before claim — binding built from frozen alone.
                let Some(binding) =
                    super::social_publish_rpc::sender_binding(frozen, &intent["request"])
                else {
                    // Malformed frozen can't ever send: claim and hold.
                    match self.claim_and_end(
                        &due,
                        "held",
                        &json!({"reason": "frozen binding does not parse for dispatch"}),
                    ) {
                        Ok(true) | Ok(false) => {}
                        Err(e) => {
                            driver.fail(&e.to_string());
                            return;
                        }
                    }
                    continue;
                };
                match sender.preflight(&binding) {
                    Preflight::Uncertain(refusal) => {
                        // Ambiguous door: leave queued and BACK OFF this row
                        // only — the batch continues so a stuck head never
                        // stalls the rows behind it.
                        driver.note_error(&id, &refusal);
                        let delay = backoff
                            .get(&id)
                            .map(|until| {
                                let prev =
                                    (*until - (driver.clock)()).max(BACKOFF_MIN_SECS as i64) as u64;
                                (prev * 2).min(BACKOFF_MAX_SECS)
                            })
                            .unwrap_or(BACKOFF_MIN_SECS);
                        backoff.insert(id, (driver.clock)() + delay as i64);
                        continue;
                    }
                    Preflight::Refused(refusal) => {
                        // Definitive door refusal: claim and report refused —
                        // the row ends, it does not spin.
                        match self.claim_and_end(
                            &due,
                            "refused",
                            &json!({"error": refusal.to_string()}),
                        ) {
                            Ok(true) => {}
                            Ok(false) => continue,
                            Err(e) => {
                                driver.fail(&e.to_string());
                                return;
                            }
                        }
                        continue;
                    }
                    Preflight::Approved => {}
                }
                // Claim THE PEEKED ROW by identity; `None` means another
                // claimant or a cancel took it first.
                let claimed = match self.claim_peeked(&due) {
                    Ok(Some(claimed)) => claimed,
                    Ok(None) => continue,
                    Err(e) => {
                        driver.fail(&e.to_string());
                        return;
                    }
                };
                let cid = id.clone();
                #[cfg(test)]
                if let Some(hook) = &driver.after_claim {
                    hook();
                }
                // Between claim and send — the fence can trip while the
                // claim committed; the send must not follow it.
                if let Some(reason) = self.lease.as_ref().and_then(|l| l.fence().check()) {
                    driver.fail(&format!("lease fenced mid-tick: {reason}"));
                    return;
                }
                // A failed read is not a material change: record it and back
                // off, never send. The unsent row stays `processing` and
                // reconcile ends it (send-now's `?` leaves it the same way).
                match self.store.social_publish_material_current(&cid) {
                    Ok(true) => {}
                    Ok(false) => {
                        self.end_report_at(
                            &cid,
                            "held",
                            &json!({"reason": "approved material changed since freeze"}),
                        );
                        continue;
                    }
                    Err(e) => {
                        driver.note_error(&cid, &Refusal::new("store_error", e.to_string()));
                        backoff.insert(cid, (driver.clock)() + BACKOFF_MIN_SECS as i64);
                        continue;
                    }
                }
                match self.dispatch_claimed(&cid, claimed) {
                    Ok(_envelope) => {
                        driver.clear_error(&cid);
                        backoff.remove(&cid);
                        // The dispatch path persists evidence but leaves the
                        // row `processing` (the operator RPC reports next);
                        // the driver finishes the job — a posted upstream
                        // verdict is reported posted from the stored
                        // byte-exact evidence, never the caller's words.
                        self.end_report(&cid);
                    }
                    Err(e) => {
                        driver.note_error(&cid, &Refusal::new("refused", e.to_string()));
                        backoff.insert(cid, (driver.clock)() + BACKOFF_MIN_SECS as i64);
                    }
                }
            }
        }
    }

    /// Close a dispatched row from its stored upstream evidence through
    /// the path send-now uses ([`Shared::settle_dispatched`]); a store
    /// error lands in `last_error` rather than being swallowed.
    fn end_report(&self, intent_id: &str) {
        if let Err(e) = self.settle_dispatched(intent_id) {
            self.social_publish_driver
                .note_error(intent_id, &Refusal::new("refused", e.to_string()));
        }
    }

    /// Report a terminal state with a literal receipt, routing the
    /// store error to `last_error` rather than discarding it.
    fn end_report_at(&self, intent_id: &str, state: &str, receipt: &Value) {
        if let Err(e) = self.store.social_publish_report(intent_id, state, receipt) {
            self.social_publish_driver.note_error(
                intent_id,
                &Refusal::new("refused", format!("{state} report failed: {e}")),
            );
        }
    }

    /// Claim the EXACT row the driver peeked, by identity in its own
    /// install and context — the CAS send-now uses. The due check ran on
    /// the peek; a row a concurrent claimant or a cancel moved out of
    /// `queued` is `None`, never a send of a row the driver did not check.
    /// A peek of the oldest due row would let one backing-off head block
    /// every row behind it.
    fn claim_peeked(&self, due: &Value) -> Result<Option<Value>> {
        let intent = &due["intent"];
        self.store.social_publish_claim_id(
            intent["intent_id"].as_str().unwrap_or(""),
            intent["frozen"]["install_id"].as_str().unwrap_or(""),
            intent["frozen"]["context_id"].as_str(),
        )
    }

    /// Claim the peeked row and report it terminal (`held`/`refused`) in
    /// one step — the shared shape for the stale-backlog, explicit-mode,
    /// malformed-frozen and definitive-refusal branches. `Ok(false)`
    /// means the row already left `queued`. Errors reach the caller so a
    /// store failure isn't swallowed into `last_error` silence.
    fn claim_and_end(&self, due: &Value, state: &str, receipt: &Value) -> Result<bool> {
        match self.claim_peeked(due)? {
            None => Ok(false),
            Some(claimed) => {
                let cid = claimed["intent"]["intent_id"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned();
                if let Err(e) = self.store.social_publish_report(&cid, state, receipt) {
                    self.social_publish_driver.note_error(
                        &cid,
                        &Refusal::new("refused", format!("{state} report failed: {e}")),
                    );
                } else {
                    self.social_publish_driver.clear_error(&cid);
                }
                Ok(true)
            }
        }
    }
}

#[cfg(test)]
mod tests;
