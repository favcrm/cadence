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
use std::time::Duration;

use serde_json::{json, Value};

use super::*;
use crate::platform::agenticos_external::publish::Refusal;

/// Default tick interval — `CADENCE_SOCIAL_PUBLISH_INTERVAL_SECS`
/// overrides it, clamped to [`MIN_INTERVAL_SECS`]..[`MAX_INTERVAL_SECS`].
pub(super) const DEFAULT_INTERVAL_SECS: u64 = 30;
pub(super) const MIN_INTERVAL_SECS: u64 = 5;
pub(super) const MAX_INTERVAL_SECS: u64 = 60;
/// At most this many claims execute per tick — a backlog drains over
/// ticks, never in one unbounded sweep.
#[allow(dead_code)] // loop lands in the stacked PR
pub(super) const PER_TICK_CAP: usize = 8;
/// Processing intents reconciled per tick (bounded separately).
#[allow(dead_code)] // loop lands in the stacked PR
pub(super) const RECONCILE_CAP: usize = 16;
/// Per-intent backoff bounds on provider-ambiguous outcomes.
#[allow(dead_code)] // loop lands in the stacked PR
pub(super) const BACKOFF_MIN_SECS: u64 = 10;
#[allow(dead_code)] // loop lands in the stacked PR
pub(super) const BACKOFF_MAX_SECS: u64 = 300;
/// Default lateness bound: a due row older than this is held, never
/// sent — a first start must not dump the stale backlog.
pub(super) const DEFAULT_MAX_LATENESS_SECS: i64 = 900;
/// A `processing` row whose status keeps answering `unknown_key` is
/// escalated to `held` once the row's age since `updated` passes this
/// bound (≥ 10 × DOOR_TIMEOUT — the door never saw the key, so no
/// status reconcile can ever succeed).
#[allow(dead_code)] // loop lands in the stacked PR
pub(super) const UNKNOWN_KEY_HOLD_SECS: i64 = 320;
/// The transient-error map's bound — one entry per in-flight intent.
#[allow(dead_code)] // loop lands in the stacked PR
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
    #[allow(dead_code)] // read by the loop in the stacked PR
    pub clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// One driver thread per daemon — the spawn guard.
    started: AtomicBool,
    /// `test-seam` hook between a committed claim and its send — the
    /// window a lease-loss test parks in. `None` in production builds
    /// (the field does not exist there).
    #[cfg(feature = "test-seam")]
    #[allow(dead_code)] // read by the loop in the stacked PR
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
    #[allow(dead_code)] // consumed by the loop in the stacked PR
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

    #[allow(dead_code)] // consumed by the loop in the stacked PR
    fn set_status(&self, status: &'static str, next_tick: Option<f64>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.status = status;
        state.last_tick = Some(epoch_secs());
        state.next_tick = next_tick;
    }

    #[allow(dead_code)] // consumed by the loop in the stacked PR
    fn fail(&self, what: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.status = "error";
        state.last_error = Some(what.to_owned());
        state.last_tick = Some(epoch_secs());
    }

    /// Record a transient per-intent error (`code: detail` only).
    #[allow(dead_code)] // consumed by the loop in the stacked PR
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

    #[allow(dead_code)] // consumed by the loop in the stacked PR
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
