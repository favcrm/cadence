//! CAD-508 independent acceptance: real CLI processes against an isolated
//! daemon/socket. The turn-adoption contract needs a live pane provider; this
//! target covers the transport boundary and records that limitation below.
#![cfg(all(unix, feature = "test-seam"))]

use cadence_agent::reaper;
use cadence_agent::store::{NewAgent, Store};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::{Builder, TempDir};

const BIN: &str = env!("CARGO_BIN_EXE_cadence");

struct Fixture {
    _root: TempDir,
    root: PathBuf,
    state: PathBuf,
    daemon: Option<Child>,
}

impl Fixture {
    fn new() -> Self {
        let root = Builder::new().prefix("c508-").tempdir_in("/tmp").unwrap();
        let root_path = root.path().to_path_buf();
        let state = root_path.join("state");
        for name in ["home", "xdg", "xdg/config", "tmp", "locks", "state"] {
            std::fs::create_dir_all(root_path.join(name)).unwrap();
        }
        // Store a real durable message before launching the real daemon.
        let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
        store
            .register_agent(&NewAgent {
                alias: "acceptance-agent",
                provider: "inbox",
                endpoint_kind: "inbox",
                role: "worker",
                cwd: root_path.to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        store
            .enqueue(
                "acceptance-agent",
                "restart-window-body",
                None,
                "cad508-read-once",
                "operator",
            )
            .unwrap();
        Self {
            _root: root,
            root: root_path,
            state,
            daemon: None,
        }
    }

    fn command(root: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.arg("--state-dir")
            .arg(root.join("state"))
            .args(args)
            .env("HOME", root.join("home"))
            .env("XDG_STATE_HOME", root.join("xdg"))
            .env("XDG_CONFIG_HOME", root.join("xdg/config"))
            .env("TMPDIR", root.join("tmp"))
            .env("CADENCE_SUITE_LOCK", root.join("locks/suite.lock"))
            .env("CADENCE_TEST_PORT_LOCK_DIR", root.join("locks"))
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("CADENCE_PM_DIR")
            .env_remove("CADENCE_ALIAS")
            .env_remove("CADENCE_ROLLOUT_AS")
            .env_remove("CADENCE_PROFILE")
            .env_remove("XDG_DATA_HOME");
        for name in cadence_agent::adapter::PROVIDER_COMMAND_VARS {
            cmd.env(name, cadence_agent::adapter::REFUSED_COMMAND);
        }
        for name in ["CARGO_HOME", "RUSTUP_HOME"] {
            if let Some(value) = std::env::var_os(name) {
                cmd.env(name, value);
            }
        }
        cmd
    }

    fn run(root: &Path, args: &[&str]) -> Output {
        let mut cmd = Self::command(root, args);
        reaper::output(&mut cmd).expect("real cadence CLI invocation")
    }

    fn start(&mut self) {
        let mut cmd = Self::command(&self.root, &["daemon", "run"]);
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        self.daemon = Some(reaper::spawn(&mut cmd).expect("spawn isolated real daemon"));
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(child) = self.daemon.as_mut() {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "daemon exited during startup"
                );
            }
            let status = Self::run(&self.root, &["daemon", "status"]);
            if status.status.success() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "isolated daemon did not open its socket: {status:?}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let stopped = Self::run(&self.root, &["daemon", "stop"]);
            assert!(stopped.status.success(), "daemon stop failed: {stopped:?}");
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                match child.try_wait().expect("wait for owned daemon") {
                    Some(_) => break,
                    None if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
                    None => {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("owned fixture daemon failed to stop");
                    }
                }
            }
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self.daemon.is_some() {
            self.stop();
        }
    }
}

fn output_text(out: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn durable_read_waits_across_socket_gap_but_probe_does_not_retry() {
    let mut fx = Fixture::new();
    fx.start();
    fx.stop();

    // A health/status probe is intentionally not a durable operation: it
    // must promptly report the absent daemon instead of entering the relay.
    let before = Instant::now();
    let probe = Fixture::run(&fx.root, &["daemon", "status"]);
    assert!(
        !probe.status.success(),
        "status unexpectedly succeeded without a daemon"
    );
    assert!(
        before.elapsed() < Duration::from_secs(3),
        "lifecycle/probe call was silently retried"
    );

    // Spawn the real message-read CLI during the socket-down interval. The
    // message already exists in SQLite and the returning daemon can answer.
    let root = fx.root.clone();
    let pending =
        thread::spawn(move || Fixture::run(&root, &["message", "read", "cad508-read-once"]));
    thread::sleep(Duration::from_millis(500));
    let returned_early = pending.is_finished();
    fx.start();
    let read = pending.join().expect("read worker joined");
    assert!(
        !returned_early,
        "durable CLI command failed immediately while the daemon socket was down: {}",
        output_text(&read)
    );
    assert!(
        read.status.success(),
        "durable message read did not succeed after daemon return: {}",
        output_text(&read)
    );
    let body = output_text(&read);
    assert!(
        body.contains("restart-window-body"),
        "read returned the wrong durable body: {body}"
    );
    fx.stop();
}

#[test]
fn invalid_running_turn_reports_do_not_complete_messages() {
    // Server-side fail-closed checks, exercised through the real binary and
    // a real daemon. This fixture does not claim to prove adoption of a live
    // provider pane; no current retained provider fixture exposes that seam.
    let mut fx = Fixture::new();
    fx.start();
    // Insert after startup so daemon recovery is not itself under test here.
    let store = Store::open(&fx.state.join("cadence.sqlite3")).unwrap();
    store
        .enqueue(
            "acceptance-agent",
            "report-body",
            None,
            "cad508-report-turn",
            "operator",
        )
        .unwrap();
    store
        .mark_running("cad508-report-turn", "persisted-turn-token")
        .unwrap();
    store
        .enqueue(
            "acceptance-agent",
            "other-report-body",
            None,
            "cad508-other-turn",
            "operator",
        )
        .unwrap();
    store
        .mark_running("cad508-other-turn", "other-persisted-token")
        .unwrap();

    let forged = Fixture::run(
        &fx.root,
        &[
            "message",
            "result",
            "cad508-report-turn",
            "--token",
            "forged-turn-token",
            "--text",
            "forged completion",
        ],
    );
    assert!(
        !forged.status.success(),
        "forged turn token was accepted: {}",
        output_text(&forged)
    );
    let mismatched = Fixture::run(
        &fx.root,
        &[
            "message",
            "result",
            "cad508-other-turn",
            "--token",
            "persisted-turn-token",
            "--text",
            "wrong message completion",
        ],
    );
    assert!(
        !mismatched.status.success(),
        "message/token mismatch was accepted: {}",
        output_text(&mismatched)
    );
    for id in ["cad508-report-turn", "cad508-other-turn"] {
        let row = store
            .message(id)
            .unwrap()
            .expect("seeded message remains present");
        assert_ne!(row.state, "completed", "refused report completed {id}");
        assert_ne!(
            row.result
                .as_ref()
                .and_then(|r| r.get("status"))
                .and_then(serde_json::Value::as_str),
            Some("completed"),
            "refused report recorded a completed result for {id}: {:?}",
            row.result
        );
    }
    fx.stop();
}
