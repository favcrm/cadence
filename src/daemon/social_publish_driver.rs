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
//! - `CADENCE_SOCIAL_PUBLISH_DRIVER=off` (or the `publish_driver_off`
//!   option) leaves the sender attached for manual RPCs while the
//!   driver stays inert — the canary kill switch.
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
/// The driver on/off env knob (`off` disables; anything else is on
/// while a sender is attached).
pub const DRIVER_ENV: &str = "CADENCE_SOCIAL_PUBLISH_DRIVER";
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
    /// Kill switch — `true` parks the loop with `status:"off"`.
    pub off: bool,
    /// Lateness bound for claim-vs-hold.
    pub max_lateness_secs: i64,
    /// The driver's clock (epoch seconds) — wall unless a test pins it.
    pub clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// One driver thread per daemon — the spawn guard.
    started: AtomicBool,
    /// `test-seam` hook between a committed claim and its send — the
    /// window a lease-loss test parks in. `None` in production builds
    /// (the field does not exist there).
    #[cfg(feature = "test-seam")]
    claimed_gate: Option<Arc<dyn Fn() + Send + Sync>>,
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
    /// seam `publish_driver_ms` bypasses the seconds clamp so driver
    /// tests run the loop hot; production env config stays second-bound.
    pub(super) fn new(opts: &ServeOptions) -> Self {
        let interval = match opts.publish_driver_ms {
            Some(ms) => Duration::from_millis(ms.max(1)),
            None => Duration::from_secs(
                std::env::var(INTERVAL_ENV)
                    .ok()
                    .and_then(|raw| raw.parse::<u64>().ok())
                    .unwrap_or(DEFAULT_INTERVAL_SECS)
                    .clamp(MIN_INTERVAL_SECS, MAX_INTERVAL_SECS),
            ),
        };
        let off = opts
            .publish_driver_off
            .unwrap_or_else(|| std::env::var(DRIVER_ENV).ok().as_deref() == Some("off"));
        let max_lateness_secs = opts.publish_driver_max_lateness_secs.unwrap_or_else(|| {
            std::env::var(LATENESS_ENV)
                .ok()
                .and_then(|raw| raw.parse::<i64>().ok())
                .unwrap_or(DEFAULT_MAX_LATENESS_SECS)
                .max(0)
        });
        Self {
            interval,
            off,
            max_lateness_secs,
            clock: opts
                .publish_driver_clock
                .clone()
                .unwrap_or_else(|| Arc::new(crate::issue::time::now_epoch)),
            started: AtomicBool::new(false),
            #[cfg(feature = "test-seam")]
            claimed_gate: opts.publish_driver_claimed_gate.clone(),
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
            // Bound: evict the oldest-keyed entry — the map is
            // best-effort diagnostics, never a queue.
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
        if !self.publish_driver.start() {
            eprintln!("cadence: social publish driver already running — second spawn refused");
            return;
        }
        let driver = &self.publish_driver;
        let mut reconcile_after: Option<String> = None;
        let mut backoff: HashMap<String, i64> = HashMap::new();
        while !self.closing.load(Ordering::SeqCst) {
            let now = (driver.clock)();
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
            self.reconcile_processing(&mut reconcile_after, &mut backoff);
            self.claim_due(now, &mut backoff);
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
    ) {
        let Some(sender) = self.social_publish_sender.clone() else {
            return;
        };
        let now = (self.publish_driver.clock)();
        let rows = match self
            .store
            .social_publish_processing(RECONCILE_CAP, after.as_deref())
        {
            Ok(rows) => rows,
            Err(e) => {
                self.publish_driver.fail(&e.to_string());
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
            if self.closing.load(Ordering::SeqCst) {
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
                .is_some_and(|until| (self.publish_driver.clock)() < *until)
            {
                continue;
            }
            match sender.status(&key) {
                Ok(outcome) => {
                    self.publish_driver.clear_error(&id);
                    backoff.remove(&id);
                    match outcome.state {
                        PublishState::Posted | PublishState::Processing => {
                            let _ = self
                                .store
                                .social_publish_note_evidence(&id, &outcome.evidence_json());
                            // A posted verdict closes the row — the
                            // report replays the stored byte-exact
                            // upstream evidence through the strict gate.
                            if outcome.state == PublishState::Posted {
                                let _ = self.report_posted_from_upstream(&id);
                            }
                        }
                        // The door demands a new grant — a definitive
                        // end, reported refused without noting evidence
                        // (the note path rejects `reconnect_needed` and
                        // would spin the tick on errors).
                        // The door demands a new grant — a definitive
                        // end, reported refused without noting evidence
                        // (the note path rejects `reconnect_needed` and
                        // would spin the tick on errors).
                        PublishState::Refused | PublishState::ReconnectNeeded => {
                            let _ = self.store.social_publish_report(
                                &id,
                                "refused",
                                &json!({"error": format!("upstream status {}", outcome.state.as_str())}),
                            );
                        }
                    }
                }
                Err(refusal) if refusal.code == "unknown_key" => {
                    // The door never saw this key — a crash between claim
                    // and send. Past the bound the row can never
                    // reconcile: escalate to `held` for a human decision
                    // rather than spinning forever.
                    if now as f64 - updated >= UNKNOWN_KEY_HOLD_SECS as f64 {
                        backoff.remove(&id);
                        self.publish_driver.clear_error(&id);
                        let _ = self.store.social_publish_report(
                            &id,
                            "held",
                            &json!({"reason": "the door has no record of this publish — check before rescheduling"}),
                        );
                    } else {
                        self.publish_driver.note_error(&id, &refusal);
                    }
                }
                Err(refusal) => {
                    // Transient status failure: record and per-intent
                    // backoff — the row stays `processing`.
                    self.publish_driver.note_error(&id, &refusal);
                    let delay = backoff
                        .get(&id)
                        .map(|_| BACKOFF_MAX_SECS)
                        .unwrap_or(BACKOFF_MIN_SECS);
                    backoff.insert(id, (self.publish_driver.clock)() + delay as i64);
                }
            }
        }
    }

    /// Claim and dispatch up to `PER_TICK_CAP` due intents. Preflight
    /// runs BEFORE the claim: ambiguity leaves the row queued under
    /// per-intent backoff; definitive refusal claims and reports. A row
    /// due past `max_lateness` is claimed and held ("missed publish
    /// window") — never sent.
    fn claim_due(self: &Arc<Self>, now: i64, backoff: &mut HashMap<String, i64>) {
        let driver = &self.publish_driver;
        let sender = self.social_publish_sender.clone();
        let Some(sender) = sender else { return };
        for _ in 0..PER_TICK_CAP {
            if self.closing.load(Ordering::SeqCst) {
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
            // Peek the next due row (read-only) to preflight its binding
            // before claiming — an ambiguous door leaves it queued.
            let Some(due) = (match self.store.social_publish_peek_due(now) {
                Ok(due) => due,
                Err(e) => {
                    driver.fail(&e.to_string());
                    return;
                }
            }) else {
                return;
            };
            let intent = &due["intent"];
            let id = intent["intent_id"].as_str().unwrap_or("").to_owned();
            if backoff
                .get(&id)
                .is_some_and(|until| (driver.clock)() < *until)
            {
                // This head row is backing off; a later row may still be
                // dispatchable — but `claim_due` only ever takes the
                // oldest due row, so a backing-off head does block the
                // queue this tick. Acceptable: the row recovers inside
                // BACKOFF_MIN_SECS and fairness keeps order.
                return;
            }
            let frozen = &intent["frozen"];
            let due_epoch = frozen["due_epoch"].as_i64().unwrap_or(0);
            // Stale backlog: claim and hold, never send.
            if now - due_epoch > driver.max_lateness_secs {
                match self.store.social_publish_claim_due(now, |_, _| Ok(true)) {
                    Ok(Some(claimed)) => {
                        let cid = claimed["intent"]["intent_id"]
                            .as_str()
                            .unwrap_or("")
                            .to_owned();
                        let _ = self.store.social_publish_report(
                            &cid,
                            "held",
                            &json!({"reason": "missed publish window"}),
                        );
                        driver.clear_error(&cid);
                        backoff.remove(&cid);
                    }
                    Ok(None) => return,
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
                match self.store.social_publish_claim_due(now, |_, _| Ok(true)) {
                    Ok(Some(claimed)) => {
                        let cid = claimed["intent"]["intent_id"]
                            .as_str()
                            .unwrap_or("")
                            .to_owned();
                        let _ = self.store.social_publish_report(
                            &cid,
                            "held",
                            &json!({"reason": "explicit-mode publish needs operator dispatch"}),
                        );
                    }
                    Ok(None) => return,
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
                match self.store.social_publish_claim_due(now, |_, _| Ok(true)) {
                    Ok(Some(claimed)) => {
                        let cid = claimed["intent"]["intent_id"]
                            .as_str()
                            .unwrap_or("")
                            .to_owned();
                        let _ = self.store.social_publish_report(
                            &cid,
                            "held",
                            &json!({"reason": "frozen binding does not parse for dispatch"}),
                        );
                    }
                    Ok(None) => return,
                    Err(e) => {
                        driver.fail(&e.to_string());
                        return;
                    }
                }
                continue;
            };
            match sender.preflight(&binding) {
                Preflight::Uncertain(refusal) => {
                    driver.note_error(&id, &refusal);
                    backoff.insert(id, (driver.clock)() + BACKOFF_MIN_SECS as i64);
                    return;
                }
                Preflight::Refused(refusal) => {
                    // Definitive door refusal: claim and report refused —
                    // the row ends, it does not spin.
                    match self.store.social_publish_claim_due(now, |_, _| Ok(true)) {
                        Ok(Some(claimed)) => {
                            let cid = claimed["intent"]["intent_id"]
                                .as_str()
                                .unwrap_or("")
                                .to_owned();
                            let _ = self.store.social_publish_report(
                                &cid,
                                "refused",
                                &json!({"error": refusal.to_string()}),
                            );
                            driver.clear_error(&cid);
                            backoff.remove(&cid);
                        }
                        Ok(None) => return,
                        Err(e) => {
                            driver.fail(&e.to_string());
                            return;
                        }
                    }
                    continue;
                }
                Preflight::Approved => {}
            }
            // Claim, then re-prove approved material and dispatch through
            // the one shared path.
            let claimed = match self.store.social_publish_claim_due(now, |_, _| Ok(true)) {
                Ok(Some(claimed)) => claimed,
                Ok(None) => return,
                Err(e) => {
                    driver.fail(&e.to_string());
                    return;
                }
            };
            let cid = claimed["intent"]["intent_id"]
                .as_str()
                .unwrap_or("")
                .to_owned();
            // test-seam: the window between a committed claim and its
            // send — a lease-loss test parks here to trip the fence.
            #[cfg(feature = "test-seam")]
            if let Some(gate) = &driver.claimed_gate {
                gate();
            }
            // Between claim and send — the fence can trip while the
            // claim committed; the send must not follow it.
            if let Some(reason) = self.lease.as_ref().and_then(|l| l.fence().check()) {
                driver.fail(&format!("lease fenced mid-tick: {reason}"));
                return;
            }
            if !self
                .store
                .social_publish_material_current(&cid)
                .unwrap_or(false)
            {
                let _ = self.store.social_publish_report(
                    &cid,
                    "held",
                    &json!({"reason": "approved material changed since freeze"}),
                );
                continue;
            }
            match self.dispatch_claimed(&cid, claimed) {
                Ok(envelope) => {
                    driver.clear_error(&cid);
                    backoff.remove(&cid);
                    // The dispatch path persists evidence but leaves the
                    // row `processing` (the operator RPC reports next);
                    // the driver finishes the job — a posted upstream
                    // verdict is reported posted from the stored
                    // byte-exact evidence, never the caller's words.
                    let _ = self.report_posted_from_upstream(&cid);
                    let _ = envelope;
                }
                Err(e) => {
                    driver.note_error(&cid, &Refusal::new("refused", e.to_string()));
                    backoff.insert(cid, (driver.clock)() + BACKOFF_MIN_SECS as i64);
                }
            }
        }
    }

    /// Report a `processing` intent `posted` when its stored upstream
    /// evidence already carries a posted verdict — the strict
    /// upstream-equality gate in `social_publish_report` enforces
    /// byte-exactness against frozen, so only daemon-observed evidence
    /// can close the row. No-op when the row isn't processing or the
    /// evidence isn't posted.
    fn report_posted_from_upstream(&self, intent_id: &str) -> Result<()> {
        let shown = self.store.social_publish_show(intent_id)?;
        let upstream = &shown["intent"]["upstream"];
        if upstream["state"].as_str() != Some("posted") {
            return Ok(());
        }
        self.store
            .social_publish_report(
                intent_id,
                "posted",
                &json!({
                    "permalink": upstream["permalink"],
                    "destination_id": upstream["destination_id"],
                    "caption_digest": upstream["caption_digest"],
                    "image_digest": upstream["image_digest"],
                    "provider_ids": upstream["provider_ids"],
                    "provider_payload": upstream["provider_payload"],
                }),
            )
            .map(|_| ())
    }
}
