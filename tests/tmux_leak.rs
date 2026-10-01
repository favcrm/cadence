//! CAD-955: a test's tmux server dies with the test on every exit path,
//! and no leftover process keeps the suite lock. Each case runs a child
//! (this binary, `leak_child`) under a plain `flock` — the way a suite
//! lock reaches every process it starts — and inspects what remains once
//! the child has exited. Every process this file scans for or kills is
//! matched by its own unique socket name; nothing else is touched.

#![allow(clippy::disallowed_methods)]

use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::process::{Command, Stdio};

#[path = "common/tmux_guard.rs"]
mod tmux_guard;

use tmux_guard::{tmux_pids_on_socket, TmuxServerGuard};

const MODE: &str = "CADENCE_TMUX_LEAK_MODE";
const SOCKET: &str = "CADENCE_TMUX_LEAK_SOCKET";

/// Helper, not a check: does nothing unless a parent test selected a
/// mode. Starts a real tmux server on the parent's socket, then leaves
/// by returning or by panicking, with or without a guard.
#[test]
fn leak_child() {
    let (Ok(mode), Ok(socket)) = (std::env::var(MODE), std::env::var(SOCKET)) else {
        return;
    };
    let guard = (mode != "unguarded").then(|| TmuxServerGuard::new("tmux", socket.clone()));
    let status = Command::new("tmux")
        .args(["-L", &socket, "new-session", "-d", "sleep 120"])
        .status()
        .expect("tmux must be installed");
    assert!(status.success(), "tmux new-session failed");
    assert!(
        !tmux_pids_on_socket(&socket).is_empty(),
        "the helper's tmux server is not running"
    );
    if mode == "panic" {
        panic!("intentional failure after the tmux server started");
    }
    drop(guard);
}

fn suite_lock_is_free(lock: &Path) -> bool {
    let file = std::fs::OpenOptions::new().write(true).open(lock).unwrap();
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    rc == 0
}

struct Outcome {
    socket: String,
    lock_free: bool,
    leaked: Vec<u32>,
    child_ok: bool,
}

/// Run the helper in `mode` under `flock <lock>` (no `-o`: the lock fd is
/// inherited, as it is by anything the suite starts), then report.
fn run(mode: &str) -> Outcome {
    let dir = tempfile::Builder::new()
        .prefix("c955")
        .tempdir_in("/tmp")
        .unwrap();
    let tmux_tmp = dir.path().join("t");
    std::fs::create_dir_all(&tmux_tmp).unwrap();
    let lock = dir.path().join("suite.lock");
    std::fs::write(&lock, "").unwrap();
    let socket = format!(
        "cadence-c955-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    // The parent's own guard: whatever the assertions below do, this
    // socket's server is gone before the test returns.
    let _cleanup = TmuxServerGuard::new("tmux", socket.clone()).tmpdir(&tmux_tmp);
    let status = Command::new("flock")
        .arg(&lock)
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "leak_child", "--test-threads", "1"])
        .env(MODE, mode)
        .env(SOCKET, &socket)
        .env("TMUX_TMPDIR", &tmux_tmp)
        .env_remove("CADENCE_SUITE_LOCK")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("flock must be installed");
    // The server needs a moment to exit after kill-server on the guarded
    // paths; the guard waits for it, so this is only the unguarded case.
    let leaked = tmux_pids_on_socket(&socket);
    let lock_free = suite_lock_is_free(&lock);
    Outcome {
        socket,
        lock_free,
        leaked,
        child_ok: status.success(),
    }
}

#[test]
fn unguarded_tmux_server_survives_and_holds_the_suite_lock() {
    // The control: without the guard the leak is real, so the other two
    // cases are able to fail.
    let o = run("unguarded");
    assert!(o.child_ok);
    assert!(
        !o.leaked.is_empty(),
        "expected the unguarded tmux server of {} to survive",
        o.socket
    );
    assert!(!o.lock_free, "the leaked server should hold the lock fd");
}

#[test]
fn guarded_tmux_server_is_killed_when_the_test_passes() {
    let o = run("pass");
    assert!(o.child_ok);
    assert!(o.leaked.is_empty(), "{} left {:?}", o.socket, o.leaked);
    assert!(o.lock_free, "{} left the suite lock held", o.socket);
}

#[test]
fn guarded_tmux_server_is_killed_when_the_test_panics() {
    let o = run("panic");
    assert!(!o.child_ok, "the helper was meant to fail");
    assert!(o.leaked.is_empty(), "{} left {:?}", o.socket, o.leaked);
    assert!(o.lock_free, "{} left the suite lock held", o.socket);
}
