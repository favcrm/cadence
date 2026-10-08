//! CAD-1204 independent acceptance: hold a real CLI report until the real
//! daemon has re-adopted a live pty pane. All processes and state are isolated.
#![cfg(all(unix, feature = "test-seam"))]

use cadence_agent::{
    reaper,
    store::{Message, Store},
};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Output},
    thread,
    time::{Duration, Instant},
};
use tempfile::{Builder, TempDir};

const BIN: &str = env!("CARGO_BIN_EXE_cadence");
const TMUX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake_tmux.py");
const TUI: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake_tui.py");
const ALIAS: &str = "cad1204-pane";
const SESSION: &str = "cad1204-native";

struct Fx {
    _temp: Option<TempDir>,
    root: PathBuf,
    state: PathBuf,
    daemon: Option<Child>,
}
impl Fx {
    fn new() -> Self {
        let temp = Builder::new().prefix("c1204-").tempdir_in("/tmp").unwrap();
        let root = temp.path().to_path_buf();
        let state = root.join("state");
        for d in [
            "home",
            "xdg",
            "xdg/config",
            "tmp",
            "locks",
            "state",
            "stub-locks",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        Self {
            _temp: Some(temp),
            root,
            state,
            daemon: None,
        }
    }
    fn command(root: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.arg("--state-dir")
            .arg(root.join("state"))
            .args(args)
            .env("HOME", root.join("home"))
            .env("XDG_STATE_HOME", root.join("xdg"))
            .env("XDG_CONFIG_HOME", root.join("xdg/config"))
            .env("TMPDIR", root.join("tmp"))
            .env("CADENCE_SUITE_LOCK", root.join("locks/suite.lock"))
            .env("CADENCE_TEST_PORT_LOCK_DIR", root.join("locks"))
            .env("CADENCE_TMUX_COMMAND", TMUX)
            .env("CADENCE_FAKE_TMUX_STATE", root.join("tmux/state.json"))
            .env(
                "CADENCE_FAKE_TMUX_BARRIER_ARMED",
                root.join("barrier/armed"),
            )
            .env(
                "CADENCE_FAKE_TMUX_BARRIER_ENTERED",
                root.join("barrier/entered"),
            )
            .env(
                "CADENCE_FAKE_TMUX_BARRIER_RELEASE",
                root.join("barrier/release"),
            )
            .env("CADENCE_FAKE_TMUX_BARRIER_MAX_SECS", "20")
            .env("CADENCE_STUB_LOCKS", root.join("stub-locks"))
            .env("CADENCE_STUB_COMMAND", format!("python3 {TUI}"))
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("CADENCE_PM_DIR")
            .env_remove("CADENCE_ALIAS")
            .env_remove("CADENCE_ROLLOUT_AS")
            .env_remove("CADENCE_PROFILE")
            .env_remove("XDG_DATA_HOME");
        for name in cadence_agent::adapter::PROVIDER_COMMAND_VARS {
            if name != "CADENCE_TMUX_COMMAND" {
                c.env(name, cadence_agent::adapter::REFUSED_COMMAND);
            }
        }
        for name in ["CARGO_HOME", "RUSTUP_HOME"] {
            if let Some(v) = std::env::var_os(name) {
                c.env(name, v);
            }
        }
        c
    }
    fn run(root: &Path, args: &[&str]) -> Output {
        let mut c = Self::command(root, args);
        reaper::output(&mut c).expect("run real cadence CLI")
    }
    fn start(&mut self) {
        let mut c = Self::command(&self.root, &["daemon", "run"]);
        c.stdout(std::fs::File::create(self.root.join("daemon.stdout")).unwrap())
            .stderr(std::fs::File::create(self.root.join("daemon.stderr")).unwrap());
        self.daemon = Some(reaper::spawn(&mut c).expect("spawn isolated daemon"));
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(
                self.daemon.as_mut().unwrap().try_wait().unwrap().is_none(),
                "daemon exited"
            );
            if Self::run(&self.root, &["daemon", "status"])
                .status
                .success()
            {
                break;
            }
            assert!(Instant::now() < until, "isolated daemon did not start");
            thread::sleep(Duration::from_millis(25));
        }
    }
    fn stop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let out = Self::run(&self.root, &["daemon", "stop"]);
            assert!(out.status.success(), "daemon stop: {}", text(&out));
            let until = Instant::now() + Duration::from_secs(15);
            while child.try_wait().unwrap().is_none() {
                assert!(Instant::now() < until, "daemon failed to stop");
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
    fn store(&self) -> Store {
        Store::open_side(&self.state.join("cadence.sqlite3")).unwrap()
    }
    fn register_and_run_turn(&mut self) -> Message {
        self.start();
        let registered = Self::run(
            &self.root,
            &[
                "agent",
                "register",
                ALIAS,
                "--provider",
                "tui-stub",
                "--endpoint",
                "pty",
                "--cwd",
                "/tmp",
                "--param",
                &format!("session={SESSION}"),
            ],
        );
        assert!(
            registered.status.success(),
            "register: {}",
            text(&registered)
        );
        let sent = Self::run(
            &self.root,
            &[
                "send",
                ALIAS,
                "--text",
                "CAD-1204 live pane acceptance turn",
            ],
        );
        assert!(sent.status.success(), "send: {}", text(&sent));
        let pane_state = self.root.join("tmux/state.json");
        let ownership_lock = self.root.join(format!("stub-locks/{SESSION}.lock"));
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            let identity = self.store().agent(ALIAS).unwrap();
            if pane_state.exists()
                && ownership_lock.exists()
                && identity.generation.is_some()
                && identity.endpoint.is_some()
            {
                break;
            }
            assert!(
                Instant::now() < until,
                "pty agent did not finish open with a persisted endpoint identity"
            );
            thread::sleep(Duration::from_millis(25));
        }
        let ready = Self::run(&self.root, &["agent", "ready", ALIAS]);
        assert!(ready.status.success(), "ready: {}", text(&ready));
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(m) = self.store().running_message(ALIAS).unwrap() {
                return m;
            }
            if Instant::now() >= until {
                let shown = Self::run(&self.root, &["agent", "show", ALIAS]);
                let row = self.store().messages(ALIAS).unwrap();
                let daemon_exit = self.daemon.as_mut().unwrap().try_wait().unwrap();
                let retained = self._temp.take().unwrap().keep();
                panic!("pty agent did not start a running turn; retained={}; daemon_exit={daemon_exit:?}; show={} messages={row:?} pane={} tmux={} stderr={}", retained.display(), text(&shown), std::fs::read_to_string(self.root.join("tmux/pane.out")).unwrap_or_default(), std::fs::read_to_string(self.root.join("tmux/state.json")).unwrap_or_default(), std::fs::read_to_string(self.root.join("daemon.stderr")).unwrap_or_default());
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
    fn barrier(&self) -> (PathBuf, PathBuf) {
        let entered = self.root.join("barrier/entered");
        let release = self.root.join("barrier/release");
        std::fs::create_dir_all(entered.parent().unwrap()).unwrap();
        std::fs::write(self.root.join("barrier/armed"), "armed\n").unwrap();
        (entered, release)
    }
    fn release(&self, release: &Path) {
        std::fs::write(release, "release\n").unwrap();
    }
}
impl Drop for Fx {
    fn drop(&mut self) {
        self.stop();
        let mut kill = Command::new(TMUX);
        kill.args(["-L", "isolated", "kill-server"])
            .env("CADENCE_FAKE_TMUX_STATE", self.root.join("tmux/state.json"));
        let _ = reaper::output(&mut kill);
    }
}
fn text(out: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}
fn result(root: &Path, id: &str, token: &str) -> Output {
    let mut c = Fx::command(
        root,
        &[
            "message",
            "result",
            id,
            "--token",
            token,
            "--text",
            "acceptance report",
        ],
    );
    reaper::output(&mut c).unwrap()
}
fn forged_alias_report(root: &Path, id: &str, token: &str) -> serde_json::Value {
    let socket = root.join("state/cadence.sock");
    let stream = UnixStream::connect(socket).expect("connect directly to isolated daemon");
    writeln!(
        &stream,
        "{}",
        serde_json::json!({
            "method": "message_report",
            "params": {
                "kind": "result",
                "message": id,
                "token": token,
                "text": "acceptance report",
                "alias": "forged-alias"
            }
        })
    )
    .expect("write forged-alias RPC frame");
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .expect("read forged-alias RPC response");
    serde_json::from_str(&response).expect("parse forged-alias RPC response")
}
fn wait_file(path: &Path) {
    let until = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < until,
            "barrier not reached: {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn pending_report_is_held_then_completed_once_after_real_readoption() {
    let mut fx = Fx::new();
    let before = fx.register_and_run_turn();
    assert_eq!(before.state, "running");
    let token = before.turn_id.clone().expect("running turn token");
    fx.stop();
    let (entered, release) = fx.barrier();
    fx.start();
    wait_file(&entered);
    let while_pending = fx.store().message(&before.id).unwrap().unwrap();
    assert_eq!(
        while_pending.state, "running",
        "message mutated before adoption proof"
    );
    let root = fx.root.clone();
    let id = before.id.clone();
    let tok = token.clone();
    let report = thread::spawn(move || result(&root, &id, &tok));
    thread::sleep(Duration::from_millis(100));
    assert!(
        !report.is_finished(),
        "report answered while adoption proof was parked"
    );
    assert_eq!(
        fx.store().message(&before.id).unwrap().unwrap().state,
        "running",
        "pending report mutated durable state"
    );
    fx.release(&release);
    let out = report.join().unwrap();
    assert!(
        out.status.success(),
        "genuine pending report refused: {}",
        text(&out)
    );
    let done = fx.store().message(&before.id).unwrap().unwrap();
    assert_eq!(done.state, "completed");
    let resend = result(&fx.root, &before.id, &token);
    let after = fx.store().message(&before.id).unwrap().unwrap();
    assert_eq!(after.state, "completed");
    assert!(
        after.result == done.result,
        "duplicate report changed result"
    );
    assert!(
        !resend.status.success() || text(&resend).to_lowercase().contains("duplicate"),
        "resend not classified as duplicate/refusal: {}",
        text(&resend)
    );
    fx.stop();
}

#[test]
fn invalid_identity_and_dead_pane_fail_closed_during_pending_adoption() {
    let mut fx = Fx::new();
    let message = fx.register_and_run_turn();
    let token = message.turn_id.clone().expect("running token");
    fx.stop();
    let (entered, release) = fx.barrier();
    fx.start();
    wait_file(&entered);
    let cases = [
        (message.id.clone(), "forged-token".to_owned()),
        ("different-message".to_owned(), token.clone()),
    ];
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let invalid_reports: Vec<_> = cases
        .into_iter()
        .map(|(id, tok)| {
            let root = fx.root.clone();
            let started = started_tx.clone();
            thread::spawn(move || {
                started.send(()).unwrap();
                result(&root, &id, &tok)
            })
        })
        .collect();
    let alias_root = fx.root.clone();
    let alias_id = message.id.clone();
    let alias_token = token.clone();
    let (alias_started_tx, alias_started_rx) = std::sync::mpsc::channel();
    let forged_alias = thread::spawn(move || {
        alias_started_tx.send(()).unwrap();
        forged_alias_report(&alias_root, &alias_id, &alias_token)
    });
    alias_started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("forged-alias RPC caller did not start during pending adoption");
    drop(started_tx);
    for _ in 0..invalid_reports.len() {
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("invalid report caller did not start during pending adoption");
    }
    thread::sleep(Duration::from_millis(100));
    assert!(
        forged_alias.is_finished(),
        "forged-alias RPC was held instead of rejected during pending adoption"
    );
    let alias_response = forged_alias.join().unwrap();
    assert_eq!(
        alias_response["ok"], false,
        "forged alias was not refused: {alias_response}"
    );
    let before_release = fx.store().message(&message.id).unwrap().unwrap();
    assert_eq!(before_release.state, "running");
    assert_eq!(before_release.turn_id, message.turn_id);
    assert_eq!(
        before_release.result, message.result,
        "invalid reports changed the result while adoption was pending"
    );
    fx.release(&release);
    for pending in invalid_reports {
        let out = pending.join().unwrap();
        assert!(
            !out.status.success(),
            "invalid report accepted: {}",
            text(&out)
        );
        assert!(
            !text(&out).to_lowercase().contains("completed"),
            "invalid report received a completion response: {}",
            text(&out)
        );
    }
    let still_running = fx.store().message(&message.id).unwrap().unwrap();
    assert_eq!(
        still_running.state, "running",
        "invalid report changed the message state"
    );
    assert_eq!(
        still_running.result, message.result,
        "invalid report changed the result"
    );

    // After settlement the genuine turn remains intact and reportable; its
    // acceptance is covered by the separate positive test (and is the known
    // main-branch RED there).
    let after_settle = fx.store().message(&message.id).unwrap().unwrap();
    assert_eq!(after_settle.state, "running");
    assert_eq!(after_settle.turn_id, message.turn_id);
    assert_eq!(after_settle.result, message.result);
    // The same forged identities remain refused after the pending window.
    let forged = result(&fx.root, &message.id, "forged-token-after");
    let mismatch = result(&fx.root, "different-message-after", &token);
    let alias = forged_alias_report(&fx.root, &message.id, &token);
    assert!(
        !forged.status.success(),
        "post-window forged token accepted"
    );
    assert!(
        !mismatch.status.success(),
        "post-window mismatched message accepted"
    );
    assert_eq!(
        alias["ok"], false,
        "post-window forged alias accepted: {alias}"
    );
    let after_invalid = fx.store().message(&message.id).unwrap().unwrap();
    assert_eq!(after_invalid.state, "running");
    assert_eq!(after_invalid.turn_id, message.turn_id);
    assert_eq!(after_invalid.result, message.result);
    fx.stop();
}

#[test]
fn dead_pane_at_adoption_refuses_pending_report_without_completion() {
    let mut fx = Fx::new();
    let message = fx.register_and_run_turn();
    let token = message.turn_id.clone().expect("running token");
    fx.stop();
    let (entered, release) = fx.barrier();
    fx.start();
    wait_file(&entered);
    let root = fx.root.clone();
    let id = message.id.clone();
    let tok = token.clone();
    let pending = thread::spawn(move || result(&root, &id, &tok));
    thread::sleep(Duration::from_millis(50));
    assert!(!pending.is_finished());
    let state = fx.root.join("tmux/state.json");
    let mut kill = Command::new(TMUX);
    kill.args(["-L", "unused", "kill-session"])
        .env("CADENCE_FAKE_TMUX_STATE", state);
    reaper::output(&mut kill).unwrap();
    fx.release(&release);
    let out = pending.join().unwrap();
    assert!(
        !out.status.success(),
        "report accepted after pane death: {}",
        text(&out)
    );
    let row = fx.store().message(&message.id).unwrap().unwrap();
    assert_ne!(row.state, "completed", "dead-pane report was completed");
    fx.stop();
}
