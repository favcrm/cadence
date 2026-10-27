//! One process-wide thread budget for the protected daemon.
//!
//! Root's custody recheck refuses the owned runtime when its `/proc/<pid>/status`
//! `Threads:` leaves `1..=128` (`installer_bundle/constructor/runtime_kernel.rs`).
//! The daemon accepted connections without any bound, so enough held sockets
//! could push it past that cap and get the runtime killed.
//!
//! Every daemon thread except the main thread takes one unit of this budget.
//! The unit is released only once the OS thread has actually terminated, which
//! Rust only guarantees after `join()`: a thread function returning is not the
//! same as the kernel thread being gone, so releasing on closure exit would let
//! a finishing handler overlap its replacement and breach the cap. A unit is
//! therefore returned by [`Budgeted::join`], or by [`reap_finished`] for detached
//! threads — that path joins first and only then releases. A failed spawn drops
//! the closure before any thread exists and releases immediately.
//!
//! `RESERVED_OUTSIDE_BUDGET` is the only thread outside the budget, and it is a
//! source fact rather than a guess: the process starts with exactly one thread.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;

/// Root's custody cap on this process, kept in one place for both sides.
pub(crate) const PROTECTED_THREAD_CAP: usize = 128;

/// The main thread, which exists before this module is reachable.
pub(crate) const RESERVED_OUTSIDE_BUDGET: usize = 1;

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Detached threads whose unit is released only when their join succeeds.
type Detached = Mutex<Vec<(JoinHandle<()>, Permit)>>;

fn detached() -> &'static Detached {
    static DETACHED: OnceLock<Detached> = OnceLock::new();
    DETACHED.get_or_init(|| Mutex::new(Vec::new()))
}

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

/// A budgeted thread. The unit lives until [`Self::join`] proves termination.
pub(crate) struct Budgeted<T> {
    handle: Option<JoinHandle<T>>,
    permit: Option<Permit>,
}

impl<T> Budgeted<T> {
    /// Join the thread and then release its unit — the only release that
    /// follows real OS termination.
    pub(crate) fn join(mut self) -> std::thread::Result<T> {
        let handle = self.handle.take().expect("budgeted thread joined once");
        let result = handle.join();
        self.permit.take();
        result
    }

    /// Whether the thread *function* finished. Only a hint: a finished
    /// function is not yet a terminated thread, so this never releases.
    pub(crate) fn is_finished(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| h.is_finished())
    }
}

impl Budgeted<()> {
    /// Detach: the unit is released by [`reap_finished`] after it joins.
    pub(crate) fn detach(mut self) {
        if let (Some(handle), Some(permit)) = (self.handle.take(), self.permit.take()) {
            detached()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((handle, permit));
        }
    }
}

impl<T> Drop for Budgeted<T> {
    fn drop(&mut self) {
        // A handle dropped without a join cannot prove the OS thread is gone.
        // Releasing here would let a finishing thread overlap its replacement
        // and breach the cap, so the unit is intentionally leaked instead:
        // fail closed on capacity rather than over-admit.
        if let Some(permit) = self.permit.take() {
            std::mem::forget(permit);
        }
    }
}

/// Join every detached thread that has finished its function, releasing each
/// unit only after the join proves the OS thread is gone.
pub(crate) fn reap_finished() {
    let mut pending = detached().lock().unwrap_or_else(|e| e.into_inner());
    let mut index = 0;
    while index < pending.len() {
        if pending[index].0.is_finished() {
            let (handle, permit) = pending.swap_remove(index);
            let _ = handle.join();
            drop(permit);
        } else {
            index += 1;
        }
    }
}

/// Spawn a daemon thread that holds one unit until it is joined.
pub(crate) fn spawn<F, T>(f: F) -> std::io::Result<Budgeted<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let permit = try_acquire().ok_or_else(exhausted)?;
    match std::thread::Builder::new().spawn(f) {
        Ok(handle) => Ok(Budgeted {
            handle: Some(handle),
            permit: Some(permit),
        }),
        Err(e) => Err(e),
    }
}

/// Named variant for the threads whose name is observable.
pub(crate) fn spawn_named<F, T>(name: String, f: F) -> std::io::Result<Budgeted<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let permit = try_acquire().ok_or_else(exhausted)?;
    match std::thread::Builder::new().name(name).spawn(f) {
        Ok(handle) => Ok(Budgeted {
            handle: Some(handle),
            permit: Some(permit),
        }),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These tests share the process-wide counter, so they must not overlap.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static SERIAL: Mutex<()> = Mutex::new(());
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn permits_are_bounded_and_released() {
        let _serial = serial();
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
    fn joining_releases_the_unit() {
        let _serial = serial();
        let base = active();
        let thread = spawn(|| {}).unwrap();
        assert_eq!(active(), base + 1, "unit was not held while running");
        thread.join().unwrap();
        assert_eq!(active(), base, "unit survived a successful join");
    }

    #[test]
    fn detaching_holds_the_unit_until_the_reaper_joins() {
        let _serial = serial();
        let base = active();
        spawn(|| {}).unwrap().detach();
        assert_eq!(active(), base + 1, "detach released before termination");
        for _ in 0..200 {
            reap_finished();
            if active() == base {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(active(), base, "reaper did not release the joined unit");
    }
}
