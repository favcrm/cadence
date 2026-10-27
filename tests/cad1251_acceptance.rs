//! CAD-1251 independent acceptance check (written by the reviewer, not the
//! implementer; AGENTS.md "Gates and security work").
//!
//! CAD-1207's guarantee must survive the longer start budget: a board that
//! never proves its start is stopped for real when its budget runs out.
//! No orphan board keeps serving the port, so the next start (or the
//! operator's `cadence ui start`) can bind it.
#![cfg(all(unix, feature = "test-seam"))]

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_cadence");

fn start(root: &std::path::Path, port: &str, envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.arg("--state-dir")
        .arg(root.join("state"))
        .args(["ui", "start", "--port", port])
        .env("HOME", root.join("home"))
        .env("XDG_STATE_HOME", root.join("xdg"))
        .env("TMPDIR", root.join("tmp"))
        .env("CADENCE_SUITE_LOCK", root.join("locks/suite.lock"))
        .env("CADENCE_TEST_PORT_LOCK_DIR", root.join("locks"))
        .env_remove("CADENCE_STATE_DIR")
        .env_remove("CADENCE_PM_DIR")
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_UI_START_BUDGET_SECS")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cadence_agent::reaper::output(&mut cmd).unwrap()
}

fn free_port() -> u16 {
    (3180..3200)
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("free port")
}

#[test]
fn cad1251_an_unproven_board_is_stopped_and_frees_its_port() {
    let root = tempfile::Builder::new()
        .prefix("c1251acc-")
        .tempdir_in("/tmp")
        .unwrap();
    for d in ["home", "xdg", "tmp", "locks", "state"] {
        std::fs::create_dir_all(root.path().join(d)).unwrap();
    }
    let port = free_port();
    let began = Instant::now();
    let out = start(
        root.path(),
        &port.to_string(),
        &[
            ("CADENCE_TEST_UI_WITHHOLD_READY", "1"),
            ("CADENCE_UI_START_BUDGET_SECS", "3"),
        ],
    );
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "an unproven board must fail the start: {text}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(20),
        "the start must give up at its budget, not hang: {:?}",
        began.elapsed()
    );
    // The reaped board must not keep serving the port.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut freed = false;
    while Instant::now() < deadline {
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            freed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        freed,
        "port {port} is still held after the unproven board was reported stopped: {text}"
    );
}
