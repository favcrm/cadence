//! Process-owned suite admission for shared integration fixtures.
//!
//! Public helpers remain available through the common harness reexports.

use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::time::Instant;

/// Host-wide suite slot (CAD-71). With `CADENCE_SUITE_LOCK` set, an
/// unfiltered run of this binary — the full suite — holds that
/// exclusive `flock` for the process lifetime, so concurrent full
/// suites on one host take turns instead of starving each other into
/// load flakes. A filtered run (one test, one group) never queues. The
/// first daemon a test starts acquires it; the kernel releases it at
/// exit. The wait is bounded (`CADENCE_SUITE_LOCK_WAIT_SECS`, default
/// 3600) and every test panics with the reason if it runs out.
pub static SUITE_SLOT: std::sync::OnceLock<Result<Option<std::fs::File>, String>> =
    std::sync::OnceLock::new();

pub fn suite_slot() {
    if let Err(msg) = SUITE_SLOT.get_or_init(acquire_suite_slot) {
        panic!("{msg}");
    }
}

/// libtest's positional arguments are test-name filters; these flags
/// take a separate value that is not one.
pub fn is_filtered_run() -> bool {
    const VALUED: &[&str] = &[
        "--test-threads",
        "--skip",
        "--logfile",
        "--color",
        "--format",
        "--shuffle-seed",
        "-Z",
    ];
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if VALUED.contains(&a.as_str()) {
            args.next();
        } else if !a.starts_with('-') {
            return true;
        }
    }
    false
}

/// Nextest launches every test in a process-per-test child and exposes
/// `NEXTEST=1` plus `NEXTEST_EXECUTION_MODE`. A filtered child cannot
/// safely acquire the host slot itself. The outer review process owns
/// the flock and explicitly clears the child path instead.
pub fn is_nextest_run() -> bool {
    std::env::var("NEXTEST").ok().as_deref() == Some("1")
        || std::env::var("NEXTEST_EXECUTION_MODE").is_ok()
}

pub fn nextest_outer_lock_required(
    nextest: bool,
    lock_path: Option<&str>,
    review_held: bool,
) -> Result<(), &'static str> {
    if !nextest {
        return Ok(());
    }
    if review_held && lock_path.is_none() {
        return Ok(());
    }
    Err(
        "nextest requires the external CADENCE_SUITE_LOCK; run `cadence review` or use the pinned outer wrapper",
    )
}

pub fn acquire_suite_slot() -> Result<Option<std::fs::File>, String> {
    use std::io::Write;
    use std::os::unix::io::AsRawFd;
    let path = std::env::var("CADENCE_SUITE_LOCK")
        .ok()
        .filter(|p| !p.is_empty());
    let review_held = std::env::var("CADENCE_REVIEW_SUITE_LOCK_HELD")
        .ok()
        .as_deref()
        == Some("1");
    nextest_outer_lock_required(is_nextest_run(), path.as_deref(), review_held)
        .map_err(str::to_string)?;
    let Some(path) = path else { return Ok(None) };
    if is_filtered_run() {
        return Ok(None);
    }
    let wait_secs: u64 = std::env::var("CADENCE_SUITE_LOCK_WAIT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3600);
    let path = PathBuf::from(path);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| {
            format!(
                "cannot create the directory of the host suite slot {} \
                 (CADENCE_SUITE_LOCK): {e} — fix the path or unset the variable",
                path.display()
            )
        })?;
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| {
            format!(
                "cannot open the host suite slot {} (CADENCE_SUITE_LOCK): {e} \
                 — fix the path or unset the variable",
                path.display()
            )
        })?;
    let epoch = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    };
    // Raw stderr, not eprintln!: libtest captures the macros per test,
    // and the queueing must be visible while it happens.
    let say = |msg: String| {
        let _ = std::io::stderr().write_all(format!("{msg}\n").as_bytes());
    };
    let start = Instant::now();
    let mut announced = false;
    loop {
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            say(format!(
                "suite slot {} acquired at epoch {} after {}s (pid {})",
                path.display(),
                epoch(),
                start.elapsed().as_secs(),
                std::process::id()
            ));
            return Ok(Some(file));
        }
        if !announced {
            say(format!(
                "suite slot {} busy — another full suite runs on this host; \
                 waiting up to {wait_secs}s (epoch {})",
                path.display(),
                epoch()
            ));
            announced = true;
        }
        if start.elapsed() >= Duration::from_secs(wait_secs) {
            return Err(format!(
                "timed out after {wait_secs}s waiting for the host suite slot {} \
                 (CADENCE_SUITE_LOCK) — another full suite still holds it; \
                 unset the variable to run unserialized",
                path.display()
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}
