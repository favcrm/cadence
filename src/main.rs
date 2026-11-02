//! `cadence` — CLI client + daemon entrypoint.
//!
//! Commands that are not implemented in this milestone fail loudly rather
//! than pretending to work.

// Test code spawns freely: no test process runs the CAD-308 reaper
// (only `daemon run` does), so the spawn registry need not see it.
#![cfg_attr(test, allow(clippy::disallowed_methods))]

use serde_json::json;

mod cli;

/// Whether this panic is the one `println!`/`print!`/`eprintln!` raise
/// when the reader of stdout (or stderr) closed early. Rust ignores
/// SIGPIPE, so the write fails with EPIPE and the macro panics with
/// "failed printing to stdout: Broken pipe" (wording verified against
/// std on rustc 1.98.1 — a std release that rewords it turns this
/// backstop off, leaving a stray panic message on stderr; the CLI's own
/// writers in `cli` absorb EPIPE themselves and never take this path).
fn closed_consumer_panic(payload: &(dyn std::any::Any + Send)) -> bool {
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&'static str>().copied())
        .unwrap_or_default();
    message.starts_with("failed printing to std") && message.contains("Broken pipe")
}

fn main() {
    // A closed consumer is not a panic worth reporting: silence exactly
    // that message, keep the default hook for every other panic, and let
    // the panic unwind to `catch_unwind` below. Unwinding is the point —
    // a hook that called `process::exit` would skip every `Drop` between
    // the failed write and `main` (a held PM lock, a temp spool). Set
    // before `run`, so it covers `daemon run`/`ui run` too: their stdout
    // is a log file or a terminal, where EPIPE cannot fire.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !closed_consumer_panic(info.payload()) {
            default_hook(info);
        }
    }));
    let code = match std::panic::catch_unwind(cli::run) {
        Ok(Ok(code)) => code,
        Ok(Err(error)) => {
            let code = cadence_agent::error::exit_code_for_kind(error.kind());
            // `code` is the stable refusal name (e.g.
            // `workflow_unapproved`) — the wire carries it, so the
            // CLI's error print does too. Stderr is outside the stdout
            // JSON policy, and a closed stderr keeps this code: the
            // error print absorbs EPIPE instead of panicking.
            let mut err = json!({"error": error.to_string(), "kind": error.kind()});
            if let Some(code) = error.code() {
                err["code"] = json!(code);
            }
            cli::eprint_line(&serde_json::to_string_pretty(&err).unwrap_or_default());
            code
        }
        // The consumer stopped reading before the verb returned a
        // failure, so the run stands: nothing said otherwise. A verb
        // whose failure verdict follows its report must print through
        // the `cli` writers, which absorb EPIPE and return that verdict.
        Err(payload) if closed_consumer_panic(payload.as_ref()) => 0,
        // Every other panic is a real fault: the default hook already
        // reported it, and resuming keeps the default exit status.
        Err(payload) => std::panic::resume_unwind(payload),
    };
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
