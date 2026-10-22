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

/// The epoch second `gh` is paused until, if still in the future.
pub(crate) fn paused_until(state_dir: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(pause_file(state_dir)).ok()?;
    let until = serde_json::from_str::<Value>(&text).ok()?["until"].as_i64()?;
    (until > now()).then_some(until)
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
    (s.contains("rate limit") && s.contains("exceeded")) || s.contains("(http 429)")
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

fn record_pause(state_dir: &Path, reset: Option<i64>) -> i64 {
    let at = now();
    let until = reset
        .unwrap_or(at + FALLBACK_PAUSE_SECS)
        .clamp(at + MIN_PAUSE_SECS, at + MAX_PAUSE_SECS);
    let file = pause_file(state_dir);
    let tmp = file.with_extension("tmp");
    if std::fs::write(&tmp, serde_json::json!({"until": until}).to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &file);
    }
    until
}

/// Run `gh` unless paused; a rate-limit failure starts the pause. The
/// error is the refusal text; a `gh` failure is returned as `Ok` for
/// the caller to read as it always did.
pub(crate) fn run(cmd: &mut Command, timeout: Duration) -> Result<Output, String> {
    if CALLS_LEFT.with(|c| match c.get() {
        Some(0) => true,
        Some(n) => {
            c.set(Some(n - 1));
            false
        }
        None => false,
    }) {
        return Err("gh call cap for this pass is spent".to_string());
    }
    let dir = BOUND.lock().unwrap_or_else(|e| e.into_inner()).clone();
    run_in(dir.as_deref(), cmd, timeout)
}

thread_local! {
    static CALLS_LEFT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Run `f` with at most `cap` guarded `gh` starts on this thread; past
/// it they fail like any failed read. Bounds a periodic pass whose own
/// size is not bounded (the daemon's reconcile).
pub(crate) fn with_call_cap<T>(cap: usize, f: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            CALLS_LEFT.with(|c| c.set(None));
        }
    }
    CALLS_LEFT.with(|c| c.set(Some(cap)));
    let _reset = Reset;
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
            let reset = run_bounded(
                Command::new(cmd.get_program()).args(["api", "rate_limit"]),
                timeout,
            )
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| reset_from(&String::from_utf8_lossy(&o.stdout)));
            record_pause(dir, reset);
        }
    }
    Ok(out)
}
