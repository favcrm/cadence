//! CAD-1251: the board's start proof gets a budget that survives a slow
//! cold start, and a board that never reports itself is still stopped.
#![cfg(all(unix, feature = "test-seam"))]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use tempfile::{Builder, TempDir};

const BIN: &str = env!("CARGO_BIN_EXE_cadence");

struct Fixture {
    _root: TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = Builder::new().prefix("c1251-").tempdir_in("/tmp").unwrap();
        let p = root.path().to_path_buf();
        for d in ["home", "xdg", "tmp", "locks", "state"] {
            std::fs::create_dir_all(p.join(d)).unwrap();
        }
        Self {
            _root: root,
            root: p,
        }
    }

    fn ui(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.arg("--state-dir")
            .arg(self.root.join("state"))
            .args(args)
            .env("HOME", self.root.join("home"))
            .env("XDG_STATE_HOME", self.root.join("xdg"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("CADENCE_SUITE_LOCK", self.root.join("locks/suite.lock"))
            .env("CADENCE_TEST_PORT_LOCK_DIR", self.root.join("locks"))
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

    fn pid(&self) -> Option<i32> {
        std::fs::read_to_string(self.root.join("state/ui.pid"))
            .ok()
            .and_then(|t| t.trim().parse().ok())
    }

    fn stop(&self) {
        let _ = self.ui(&["ui", "stop"], &[]);
        if let Some(pid) = self.pid() {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

fn port(base: u16) -> String {
    (base..3200)
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("free port")
        .to_string()
}

fn text(o: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// A ready report later than the proof budget fails and stops the board;
/// the same delay inside a larger budget (what a restart or update now
/// passes) succeeds.
#[test]
fn slow_ready_fails_on_a_short_budget_and_succeeds_on_a_restart_budget() {
    let f = Fixture::new();
    let port = port(3150);
    let slow = [("CADENCE_TEST_UI_READY_DELAY_MS", "4000")];
    let short = f.ui(
        &["ui", "start", "--port", &port],
        &[slow[0], ("CADENCE_UI_START_BUDGET_SECS", "1")],
    );
    assert!(!short.status.success(), "{}", text(&short));
    assert!(text(&short).contains("within 1s"), "{}", text(&short));
    assert!(f.pid().is_none(), "the unproven board left ui.pid");

    let long = f.ui(
        &["ui", "start", "--port", &port],
        &[slow[0], ("CADENCE_UI_START_BUDGET_SECS", "60")],
    );
    let out = text(&long);
    f.stop();
    assert!(long.status.success(), "{out}");
    assert!(out.contains("board proved its start in"), "{out}");
}

/// A board that never reports is still stopped, however large the budget.
#[test]
fn a_board_that_never_reports_is_still_stopped() {
    let f = Fixture::new();
    let port = port(3170);
    let out = f.ui(
        &["ui", "start", "--port", &port],
        &[
            ("CADENCE_TEST_UI_WITHHOLD_READY", "1"),
            ("CADENCE_UI_START_BUDGET_SECS", "3"),
        ],
    );
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("was stopped"), "{}", text(&out));
    assert!(f.pid().is_none());
    f.stop();
}
