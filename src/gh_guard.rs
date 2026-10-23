//! The rate-limit back-off every polling `gh` caller shares (CAD-1751).
//!
//! When `gh` reports a rate limit, the board and the daemon stop
//! spawning `gh` until GitHub's reset instead of retrying. The pause is
//! one `gh-rate-limit.json` in the state dir, so both processes see it.
//! It is read from `gh`'s exit status and stderr — never from a cached
//! body — and `gh api rate_limit`, which GitHub does not count, names
//! the reset. A pause only makes `gh` look unreachable, which every
//! caller already treats as "no evidence".

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::Value;

use crate::proc::run_bounded;

/// Pause when the reset time cannot be read (a secondary limit).
const FALLBACK_PAUSE_SECS: i64 = 15 * 60;
/// No pause is shorter or longer than this: GitHub's window is an hour.
const MIN_PAUSE_SECS: i64 = 60;
const MAX_PAUSE_SECS: i64 = 65 * 60;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn pause_file(state_dir: &Path) -> PathBuf {
    state_dir.join("gh-rate-limit.json")
}

/// The epoch second `gh` is paused until, if still in the future. A
/// corrupt or far-future file never holds `gh` past the longest pause
/// this module would write.
pub(crate) fn paused_until(state_dir: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(pause_file(state_dir)).ok()?;
    let until = serde_json::from_str::<Value>(&text).ok()?["until"].as_i64()?;
    let at = now();
    (until > at).then_some(until.min(at + MAX_PAUSE_SECS))
}

/// The note shown while `gh` is paused.
pub(crate) fn paused_note(until: i64) -> String {
    format!(
        "github API rate limit exceeded — gh calls paused until {}",
        crate::issue::time::iso(until)
    )
}

/// Does this `gh` failure say the account is rate limited?
pub(crate) fn is_rate_limited(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    (s.contains("rate limit") && s.contains("exceeded"))
        || s.contains("abuse detection")
        || s.contains("(http 429)")
}

/// The latest reset among exhausted resources in a `rate_limit` reply.
fn reset_from(body: &str) -> Option<i64> {
    let v: Value = serde_json::from_str(body).ok()?;
    v["resources"]
        .as_object()?
        .values()
        .filter(|r| r["remaining"].as_i64() == Some(0))
        .filter_map(|r| r["reset"].as_i64())
        .max()
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
/// Longest a pause write waits for another writer before recording anyway.
const LOCK_WAIT: Duration = Duration::from_secs(2);

/// An exclusive `flock` on a sidecar file, so the read -> max -> rename
/// below is one transaction across processes. Bounded: on timeout or
/// error the caller records the pause unlocked rather than drop it.
fn lock_pause(state_dir: &Path) -> Option<std::fs::File> {
    use std::os::fd::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state_dir.join("gh-rate-limit.json.lock"))
        .ok()?;
    let deadline = std::time::Instant::now() + LOCK_WAIT;
    loop {
        // SAFETY: flock on an fd this function owns.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Some(file);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn record_pause(state_dir: &Path, reset: Option<i64>) -> i64 {
    let at = now();
    let until = reset
        .unwrap_or(at + FALLBACK_PAUSE_SECS)
        .clamp(at + MIN_PAUSE_SECS, at + MAX_PAUSE_SECS);
    // Two processes can hit the limit at once. The lock is held from the
    // read of the recorded pause to the rename, so a pause is never
    // shortened by a writer that read older state; the temp name is
    // unique per call. Without the lock (timeout) the pause is still
    // written, never dropped.
    let _lock = lock_pause(state_dir);
    let until = paused_until(state_dir).map_or(until, |held| held.max(until));
    let file = pause_file(state_dir);
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = file.with_extension(format!("{}.{seq}.tmp", std::process::id()));
    if std::fs::write(&tmp, serde_json::json!({"until": until}).to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &file);
    }
    until
}

/// Run `gh` unless paused; a rate-limit failure starts the pause. The
/// error is the refusal text; a `gh` failure is returned as `Ok` for
/// the caller to read as it always did.
pub(crate) fn run(cmd: &mut Command, timeout: Duration) -> Result<Output, String> {
    if !take_call() {
        return Err("gh call cap for this pass is spent".to_string());
    }
    let dir = BOUND.lock().unwrap_or_else(|e| e.into_inner()).clone();
    run_in(dir.as_deref(), cmd, timeout)
}

/// Spend one guarded start from this thread's cap; false when spent.
fn take_call() -> bool {
    CALLS_LEFT.with(|c| match c.get() {
        Some(0) => false,
        Some(n) => {
            c.set(Some(n - 1));
            true
        }
        None => true,
    })
}

thread_local! {
    static CALLS_LEFT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Run `f` with at most `cap` guarded `gh` starts on this thread; past
/// it they fail like any failed read. Bounds a periodic pass whose own
/// size is not bounded (the daemon's reconcile). Nested, the smaller
/// cap holds and the inner calls are spent from the outer one too.
pub(crate) fn with_call_cap<T>(cap: usize, f: impl FnOnce() -> T) -> T {
    struct Reset {
        outer: Option<usize>,
        start: usize,
    }
    impl Drop for Reset {
        fn drop(&mut self) {
            let left = CALLS_LEFT.with(|c| c.get()).unwrap_or(0);
            let spent = self.start.saturating_sub(left);
            CALLS_LEFT.with(|c| c.set(self.outer.map(|o| o.saturating_sub(spent))));
        }
    }
    let outer = CALLS_LEFT.with(|c| c.get());
    let start = outer.map_or(cap, |o| o.min(cap));
    CALLS_LEFT.with(|c| c.set(Some(start)));
    let _reset = Reset { outer, start };
    f()
}

/// The state dir this process's board or daemon runs on. The pause file
/// lives there and nowhere else — never resolved from the environment,
/// which an explicit `--state-dir` does not set. A process that never
/// binds (a one-shot CLI) neither writes nor obeys a pause.
static BOUND: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

pub(crate) fn bind(state_dir: &Path) {
    *BOUND.lock().unwrap_or_else(|e| e.into_inner()) = Some(state_dir.to_path_buf());
}

pub(crate) fn run_in(
    state_dir: Option<&Path>,
    cmd: &mut Command,
    timeout: Duration,
) -> Result<Output, String> {
    if let Some(until) = state_dir.and_then(paused_until) {
        return Err(paused_note(until));
    }
    let out = run_bounded(cmd, timeout).map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() && is_rate_limited(&String::from_utf8_lossy(&out.stderr)) {
        if let Some(dir) = state_dir {
            // The lookup is a `gh` start like any other: past the cap
            // it is skipped and the fallback pause applies.
            let reset = take_call()
                .then(|| {
                    run_bounded(
                        Command::new(cmd.get_program()).args(["api", "rate_limit"]),
                        timeout,
                    )
                    .ok()
                })
                .flatten()
                .filter(|o| o.status.success())
                .and_then(|o| reset_from(&String::from_utf8_lossy(&o.stdout)));
            record_pause(dir, reset);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod cad1751_call_cap_refusal_acceptance;
#[cfg(test)]
mod cad1759_guard_hardening_refusal_acceptance;
#[cfg(test)]
mod cad1759_pause_race_refusal_acceptance;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_cap_takes_minimum_and_spends_outer() {
        with_call_cap(5, || {
            with_call_cap(10, || {
                assert!(take_call() && take_call());
            });
            assert!(take_call() && take_call() && take_call());
            assert!(!take_call(), "inner calls were spent from the outer cap");
        });
        assert!(take_call(), "no cap outside the scope");
    }

    #[test]
    fn pause_keeps_longest_and_clamps_on_read() {
        let dir = tempfile::tempdir().unwrap();
        let long = record_pause(dir.path(), Some(now() + 3000));
        let shorter = record_pause(dir.path(), Some(now() + 100));
        assert_eq!(shorter, long);
        assert_eq!(paused_until(dir.path()), Some(long));
        std::fs::write(pause_file(dir.path()), r#"{"until":99999999999}"#).unwrap();
        assert!(paused_until(dir.path()).unwrap() <= now() + MAX_PAUSE_SECS);
        assert!(is_rate_limited(
            "You have triggered an abuse detection mechanism"
        ));
    }

    #[test]
    fn concurrent_writers_never_shorten_the_pause() {
        let dir = tempfile::tempdir().unwrap();
        let long = now() + 3000;
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let d = dir.path().to_path_buf();
                std::thread::spawn(move || {
                    record_pause(&d, Some(if i == 3 { long } else { now() + 100 + i }))
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert!(paused_until(dir.path()).unwrap() >= long);
    }
}
