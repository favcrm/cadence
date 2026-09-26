//! `cadence` — CLI client + daemon entrypoint.
//!
//! Commands that are not implemented in this milestone fail loudly rather
//! than pretending to work.

// Test code spawns freely: no test process runs the CAD-308 reaper
// (only `daemon run` does), so the spawn registry need not see it.
#![cfg_attr(test, allow(clippy::disallowed_methods))]

use serde_json::json;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::Ordering;

mod cli;

/// The exit code the process has already committed to. The hook
/// below exits with THIS, not a hard-coded 0: a failing verb whose
/// own error print hits a closed pipe (`issue show NOSUCH 2>&1 |
/// head -c 0`) must still exit 1, or `set -e` and `if cadence …`
/// callers would read the failure as success.
static INTENDED_EXIT: AtomicI32 = AtomicI32::new(0);

/// A downstream reader that closes early (`cadence … | head`) makes
/// every further stdout write fail with EPIPE — Rust ignores SIGPIPE,
/// so `println!` panics with "failed printing to stdout: Broken pipe"
/// (wording verified against std on rustc 1.98.1 — a std release that
/// rewords it turns this fix off silently) where a unix tool would
/// exit quietly. Catch exactly that panic in the hook and exit with
/// `INTENDED_EXIT` instead; every other panic still reports through
/// the default hook. `process::exit` skips unwinding — `Drop`
/// impls (a held pm lock, a temp dir) never run — which is safe for
/// today's read-only verbs but is an assumption the next verb that
/// takes a lock before printing will inherit silently. Set before
/// `run`, so it covers `daemon run`/`ui run` too — their stdout is
/// a log file or terminal, where the panic can never fire.
fn install_broken_pipe_exit() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let msg = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&'static str>().copied())
            .unwrap_or_default();
        if msg.starts_with("failed printing to std") && msg.contains("Broken pipe") {
            std::process::exit(INTENDED_EXIT.load(Ordering::Relaxed));
        }
        default_hook(info);
    }));
}

fn main() {
    install_broken_pipe_exit();
    let code = match cli::run() {
        Ok(code) => code,
        Err(error) => {
            // Record the failure BEFORE the error print — if the
            // eprintln itself hits a closed pipe, the hook must
            // exit with the real code, not success.
            INTENDED_EXIT.store(1, Ordering::Relaxed);
            // `code` is the stable refusal name (e.g.
            // `workflow_unapproved`) — the wire carries it, so the
            // CLI's error print does too.
            let mut err = json!({"error": error.to_string(), "kind": error.kind()});
            if let Some(code) = error.code() {
                err["code"] = json!(code);
            }
            eprintln!("{}", serde_json::to_string_pretty(&err).unwrap_or_default());
            1
        }
    };
    INTENDED_EXIT.store(code, Ordering::Relaxed);
    std::process::exit(code);
}

/// The debug Clap builder for this CLI exceeds the 2 MiB libtest worker stack.
/// Raise it before libtest reads `RUST_MIN_STACK`, unless the operator already set one.
#[cfg(all(test, target_os = "linux"))]
#[used]
#[link_section = ".init_array"]
static CADENCE_TEST_STACK: unsafe extern "C" fn() = cadence_raise_test_stack;

#[cfg(all(test, target_os = "linux"))]
unsafe extern "C" fn cadence_raise_test_stack() {
    let key = c"RUST_MIN_STACK".as_ptr();
    if libc::getenv(key).is_null() {
        libc::setenv(key, c"8388608".as_ptr(), 0);
    }
}
