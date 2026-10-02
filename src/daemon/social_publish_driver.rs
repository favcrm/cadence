//! CAD-1020: daemon-owned driver for scheduled social publishes.
//!
//! Skeleton only — the contract is under review (see the draft PR body
//! and the CAD-1020 ticket). The driver loop, claim/send/reconcile
//! wiring and the adversarial suite land after the contract passes.
//! This file deliberately exports nothing usable yet.

#![allow(dead_code)]

/// Default tick interval — the bound `social_publish_driver_interval_secs`
/// overrides between `MIN` and `MAX`.
pub(super) const DEFAULT_INTERVAL_SECS: u64 = 30;
pub(super) const MIN_INTERVAL_SECS: u64 = 5;
pub(super) const MAX_INTERVAL_SECS: u64 = 60;
/// At most this many claims execute per tick — a backlog drains over
/// ticks, never in one unbounded sweep.
pub(super) const PER_TICK_CAP: usize = 8;
/// Processing intents reconciled per tick (bounded separately from claims).
pub(super) const RECONCILE_CAP: usize = 16;
/// Backoff bounds on consecutive provider-ambiguous ticks.
pub(super) const BACKOFF_MIN_SECS: u64 = 10;
pub(super) const BACKOFF_MAX_SECS: u64 = 300;
