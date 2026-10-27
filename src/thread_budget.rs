//! One process-wide thread budget for the protected daemon.
//!
//! Root's custody recheck refuses the owned runtime when its `/proc/<pid>/status`
//! `Threads:` leaves `1..=128` (`installer_bundle/constructor/runtime_kernel.rs`).
//! The daemon accepted connections without any bound, so enough held sockets
//! could push it past that cap and get the runtime killed.
//!
//! Every daemon thread except the main thread now takes one unit of this budget
//! for its whole life. A unit is released when the thread returns, panics or
//! fails to spawn, so the accounting cannot drift. `RESERVED_OUTSIDE_BUDGET` is
//! the only thread outside it, and it is a source fact rather than a guess: the
//! process starts with exactly one thread, and `runtime_child::entry` budgets
//! its own accept forwarders.
//!
//! Refusal is a typed error, never a panic and never an unbudgeted fallback.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;

/// Root's custody cap on this process, kept in one place for both sides.
pub(crate) const PROTECTED_THREAD_CAP: usize = 128;

/// The main thread, which exists before this module is reachable.
pub(crate) const RESERVED_OUTSIDE_BUDGET: usize = 1;

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// One held unit. Dropping it releases the unit exactly once.
pub(crate) struct Permit(());

impl Drop for Permit {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The most units that may be held at once.
pub(crate) fn limit() -> usize {
    PROTECTED_THREAD_CAP - RESERVED_OUTSIDE_BUDGET
}

/// Units currently held, for tests and refusal messages.
pub(crate) fn active() -> usize {
    ACTIVE.load(Ordering::SeqCst)
}

/// Take one unit, or `None` when the budget is exhausted.
pub(crate) fn try_acquire() -> Option<Permit> {
    let limit = limit();
    let mut now = ACTIVE.load(Ordering::SeqCst);
    loop {
        if now >= limit {
            return None;
        }
        match ACTIVE.compare_exchange_weak(now, now + 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return Some(Permit(())),
            Err(observed) => now = observed,
        }
    }
}

fn exhausted() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        format!(
            "protected daemon thread budget exhausted ({}/{} in use)",
            active(),
            limit()
        ),
    )
}

/// Spawn a daemon thread that holds one unit until it exits.
///
/// A failed spawn drops the closure and therefore releases the unit; the caller
/// sees the OS error and must handle it explicitly.
pub(crate) fn spawn<F, T>(f: F) -> std::io::Result<JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let permit = try_acquire().ok_or_else(exhausted)?;
    std::thread::Builder::new().spawn(move || {
        let _permit = permit;
        f()
    })
}

/// Named variant for the threads whose name is observable.
pub(crate) fn spawn_named<F, T>(name: String, f: F) -> std::io::Result<JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let permit = try_acquire().ok_or_else(exhausted)?;
    std::thread::Builder::new().name(name).spawn(move || {
        let _permit = permit;
        f()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_are_bounded_and_released() {
        let base = active();
        let mut held = Vec::new();
        while let Some(permit) = try_acquire() {
            held.push(permit);
        }
        assert_eq!(active(), limit());
        assert!(try_acquire().is_none(), "exhausted budget still admitted");
        held.clear();
        assert_eq!(active(), base, "dropping permits did not release units");
        assert!(try_acquire().is_some(), "released unit was not reusable");
    }

    #[test]
    fn a_thread_releases_its_unit_when_it_exits() {
        let base = active();
        let handle = spawn(|| {}).unwrap();
        handle.join().unwrap();
        assert_eq!(active(), base);
    }
}
