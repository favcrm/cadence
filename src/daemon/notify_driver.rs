//! CAD-1423: the background Web Push initiator. One unfenced daemon thread
//! (native placement only) asks the platform to deliver the candidate
//! `(intent, device)` pairs that the native store marks due. It sends only
//! opaque references. The platform's prepare and finish callbacks, which
//! carry their own signed proofs, are the sole path that claims, settles or
//! revokes a delivery. This thread never marks a delivery accepted, read, or
//! sent from the invoke reply.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::*;
use crate::notify_host;

/// The poll cadence and the bounded pause after a failed host call.
const TICK: Duration = Duration::from_secs(2);
const RETRY: Duration = Duration::from_secs(10);
/// Candidates handled per tick, so one tick never floods the platform.
const BATCH: i64 = 20;

/// Sleep in `closing`-aware steps so a stop lands within ~50ms.
fn sleep_until(closing: &AtomicBool, deadline: Instant) {
    while !closing.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

impl Shared {
    /// Whether this daemon may run the initiator: native placement only. A
    /// standalone or unavailable membership mode never sends.
    pub(super) fn notify_driver_enabled(&self) -> bool {
        matches!(
            self.membership_mode,
            public_membership::MembershipMode::Native(_)
        )
    }

    /// The loop body. Candidates are read from the store as a snapshot and
    /// the store is released before any HTTP call, so no store lock or RPC
    /// lock is held across the network.
    pub(super) fn run_notify_driver(self: &Arc<Self>) {
        while !self.closing.load(Ordering::SeqCst) {
            let pause = if self.notify_driver_tick() {
                TICK
            } else {
                RETRY
            };
            sleep_until(&self.closing, Instant::now() + pause);
        }
    }

    /// One bounded batch. A failed invoke does not abort the rest of the batch:
    /// every candidate is tried once, and the failures are counted and logged
    /// as one line. Returns whether the tick was clean, so a failed batch waits
    /// the bounded retry before the next poll.
    fn notify_driver_tick(&self) -> bool {
        let refs = match self.store.push_due_refs(BATCH) {
            Ok(refs) => refs,
            Err(e) => {
                eprintln!("cadence: notification initiator: {e}");
                return false;
            }
        };
        if refs.is_empty() {
            return true;
        }
        // Fail closed: no invoke unless the platform reports push available.
        match notify_host::push_config() {
            Ok(cfg) if cfg.available => {}
            _ => return false,
        }
        let mut failed = 0usize;
        for (intent, device) in refs {
            if self.closing.load(Ordering::SeqCst) {
                break;
            }
            if notify_host::push_delivery_invoke(intent, &device).is_err() {
                failed += 1;
            }
        }
        if failed > 0 {
            eprintln!(
                "cadence: notification initiator: {failed} invoke(s) failed; retrying after pause"
            );
        }
        failed == 0
    }
}
