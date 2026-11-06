//! CAD-2207 generic daemon restart-proof acceptance.
//!
//! The public contract is observable only through the real `cadence` binary:
//! an ordinary-only fleet restarts cleanly; a previously live personal
//! assistant that returns live still produces a clean generic verdict without
//! its alias/UID appearing anywhere in the public report; and missing, old or
//! malformed shutdown evidence with a bound private assistant never yields a
//! clean verdict. The fixture seeds the real store before the daemon starts
//! and corrupts only the real persisted shutdown marker after `daemon stop`.
//!
//! Attention/disappearance/UID-change and concurrent start-vs-begin_closing
//! are deliberately not mocked here: see the module report for the missing
//! public daemon/CLI seam.

use cadence_agent::issue::Pm;
use cadence_agent::store::{NewAgent, Store};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ORDINARY_ALIAS: &str = "cad2207-restart-ordinary";
const PRIVATE_ALIAS: &str = "assist-agent-cad2207-restart";
const ORG: &str = "org-cad2207";
const SUB: &str = "sub-cad2207";
const ASSISTANT_ID: &str = "assist_cad2207";
const RESTART_IDENTITY: &str = "operator:cad2207-restart";

struct Rig {
    _root: tempfile::TempDir,
    state: PathBuf,
    pm: PathBuf,
    home: PathBuf,
    private_uid: Option<String>,
}

impl Rig {
    fn new(with_private: bool) -> Self {
        let root = tempfile::Builder::new()
            .prefix("cad2207-restart-")
            .tempdir()
            .unwrap();
        let state = root.path().join("state");
        let pm = root.path().join("pm");
        let home = root.path().join("home");
        std::fs::create_dir(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(&home).unwrap();
        Pm::init(&pm).unwrap();

        let private_uid = {
            let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
            register_agent(&store, ORDINARY_ALIAS, &state);
            let private_uid = if with_private {
                // Bind first: the real store refuses a binding whose alias is
                // already an ordinary registered agent.
                store
                    .assistant_bind(ORG, SUB, ASSISTANT_ID, PRIVATE_ALIAS, Some("rev0"))
                    .unwrap();
                let cwd = principal_cwd(&state, ORG, SUB);
                std::fs::create_dir_all(&cwd).unwrap();
                register_agent(&store, PRIVATE_ALIAS, &cwd);
                let uid = store.agent(PRIVATE_ALIAS).unwrap().uid;
                assert!(
                    !uid.is_empty(),
                    "the private assistant has no immutable UID"
                );
                Some(uid)
            } else {
                None
            };
            private_uid
        };

        Rig {
            _root: root,
            state,
            pm,
            home,
            private_uid,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cadence"));
        command
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("TMPDIR", "/tmp")
            .env("CADENCE_STATE_DIR", &self.state)
            .env("CADENCE_PM_DIR", &self.pm)
            .env("CADENCE_TEST_SEAM", "1")
            .args(args);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = self.command(args);
        cadence_agent::reaper::output(&mut command).unwrap()
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.state.join("daemon.log")).unwrap_or_default()
    }

    /// Poll the real store row until the fake provider's actor has opened:
    /// idle state, a native thread/session and a live pid. This is the
    /// observable proof that the private/ordinary runtime is not merely a
    /// state label before the restart claim is made.
    fn wait_agent_live(&self, alias: &str, phase: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut last = None;
        while std::time::Instant::now() < deadline {
            if let Some(row) = observed_live(&self.state, alias) {
                if row.0 == "idle"
                    && row.1.as_deref().is_some_and(|v| !v.is_empty())
                    && row.2.as_deref().is_some_and(|v| !v.is_empty())
                    && row.3.is_some_and(|pid| pid > 0)
                {
                    return;
                }
                last = Some(row);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!(
            "{phase}: {alias} never reached a live actor (last row {last:?})\ndaemon.log:\n{}",
            self.log()
        );
    }

    fn start(&self) {
        let out = self.run(&["daemon", "start"]);
        assert!(
            out.status.success(),
            "daemon start failed\nstdout:\n{}\nstderr:\n{}\ndaemon.log:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            self.log()
        );
        let answer: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
        assert_eq!(
            answer["state"].as_str(),
            Some("started"),
            "daemon start did not return a started receipt: {answer}"
        );
    }

    #[cfg(feature = "test-seam")]
    fn start_with_begin_closing_gate(&self, gate: &Path) {
        let mut command = self.command(&["daemon", "start"]);
        command.env("CADENCE_TEST_CAD2207_BEGIN_CLOSING_GATE", gate);
        let out = cadence_agent::reaper::output(&mut command).unwrap();
        assert!(
            out.status.success(),
            "daemon start with begin-closing gate failed\nstdout:\n{}\nstderr:\n{}\ndaemon.log:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            self.log()
        );
        let answer: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
        assert_eq!(
            answer["state"].as_str(),
            Some("started"),
            "daemon start with gate did not return a started receipt: {answer}"
        );
    }

    #[cfg(feature = "test-seam")]
    fn spawn_with_env(&self, args: &[&str], envs: &[(&str, &Path)]) -> std::process::Child {
        let mut command = self.command(args);
        for (key, value) in envs {
            command.env(key, value);
        }
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        cadence_agent::reaper::spawn(&mut command).unwrap()
    }

    fn claim_restart_lease(&self) {
        let out = self.run(&[
            "rollout",
            "claim",
            "--reason",
            "CAD-2207 restart-proof acceptance",
            "--as",
            RESTART_IDENTITY,
        ]);
        assert!(
            out.status.success(),
            "could not claim the restart lease\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn stop(&self) {
        let _ = self.run(&["daemon", "stop"]);
    }

    fn marker_path(&self) -> PathBuf {
        self.state.join("shutdown.json")
    }

    fn assert_no_private_identity(&self, out: &Output) {
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !text.contains(PRIVATE_ALIAS),
            "the public restart verdict exposed the private alias:\n{text}"
        );
        if let Some(uid) = &self.private_uid {
            assert!(
                !text.contains(uid.as_str()),
                "the public restart verdict exposed the private UID:\n{text}"
            );
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.stop();
    }
}

fn principal_cwd(state: &Path, org: &str, sub: &str) -> PathBuf {
    let canonical = format!("{}:{org}|{}:{sub}", org.len(), sub.len());
    state
        .join("assistants")
        .join(cadence_agent::operator_auth::digest(&canonical))
}

type ObservedLive = (String, Option<String>, Option<String>, Option<i64>);

fn observed_live(state: &Path, alias: &str) -> Option<ObservedLive> {
    let conn = rusqlite::Connection::open(state.join("cadence.sqlite3")).ok()?;
    conn.busy_timeout(std::time::Duration::from_millis(250))
        .ok()?;
    conn.query_row(
        "SELECT state, thread_id, session_id, pid FROM agents WHERE alias=?",
        [alias],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )
    .ok()
}

#[cfg(feature = "test-seam")]
fn wait_gate(gate: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !gate.join("entered").is_file() {
        assert!(
            std::time::Instant::now() < deadline,
            "daemon never entered gate {gate:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(feature = "test-seam")]
fn release_gate(gate: &Path) {
    std::fs::create_dir_all(gate).unwrap();
    std::fs::write(gate.join("release"), b"").unwrap();
}

#[cfg(feature = "test-seam")]
fn mutate_after_settle(state: &Path, alias: &str, mode: &str) {
    let conn = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    conn.busy_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    match mode {
        "attention" => {
            conn.execute(
                "UPDATE agents SET state='attention', error='cad-2207 settle-gate mutation' WHERE alias=?",
                rusqlite::params![alias],
            )
            .unwrap();
        }
        "missing" => {
            conn.execute("DELETE FROM agents WHERE alias=?", rusqlite::params![alias])
                .unwrap();
        }
        "uid" => {
            conn.execute(
                "UPDATE agents SET uid='cad2207-mutated-uid' WHERE alias=?",
                rusqlite::params![alias],
            )
            .unwrap();
        }
        other => panic!("unknown settle-gate mutation {other}"),
    }
}

fn register_agent(store: &Store, alias: &str, cwd: &Path) {
    store
        .register_agent(&NewAgent {
            alias,
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: cwd.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
}

/// Ordinary-only healthy restart is a real success; no hidden row exists.
#[test]
fn ordinary_only_healthy_restart_is_clean() {
    let rig = Rig::new(false);
    rig.start();
    rig.wait_agent_live(ORDINARY_ALIAS, "ordinary before restart");
    rig.claim_restart_lease();
    let out = rig.run(&["daemon", "restart", "--as", RESTART_IDENTITY]);
    assert!(
        out.status.success(),
        "ordinary-only restart was not clean\nstdout:\n{}\nstderr:\n{}\ndaemon.log:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        rig.log()
    );
    rig.wait_agent_live(ORDINARY_ALIAS, "ordinary after restart");
}

/// A previously live personal assistant that returns live is one generic
/// settled fleet: the restart is clean, but the public report never names
/// the private alias or UID.
#[test]
fn private_assistant_returns_live_without_public_identity() {
    let rig = Rig::new(true);
    rig.start();
    rig.wait_agent_live(ORDINARY_ALIAS, "ordinary before private restart");
    rig.wait_agent_live(PRIVATE_ALIAS, "private before restart");
    rig.claim_restart_lease();
    let out = rig.run(&["daemon", "restart", "--as", RESTART_IDENTITY]);
    assert!(
        out.status.success(),
        "a live private assistant made the restart dirty\nstdout:\n{}\nstderr:\n{}\ndaemon.log:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        rig.log()
    );
    rig.assert_no_private_identity(&out);
    rig.wait_agent_live(ORDINARY_ALIAS, "ordinary after private restart");
    rig.wait_agent_live(PRIVATE_ALIAS, "private after restart");
}

/// Missing, old or malformed shutdown evidence must never be read as a clean
/// stop when a bound private assistant (hidden work) exists. The evidence is
/// the real file `daemon stop` wrote; only its persisted form is corrupted.
#[test]
fn missing_old_or_malformed_shutdown_evidence_never_clean_with_hidden_work() {
    for kind in ["missing", "old", "malformed"] {
        let rig = Rig::new(true);
        rig.start();
        rig.wait_agent_live(PRIVATE_ALIAS, "private before evidence stop");
        let stopped = rig.run(&["daemon", "stop"]);
        assert!(
            stopped.status.success(),
            "{kind}: daemon stop failed before evidence corruption\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&stopped.stdout),
            String::from_utf8_lossy(&stopped.stderr)
        );
        assert!(
            rig.marker_path().exists(),
            "{kind}: daemon stop wrote no shutdown evidence to corrupt"
        );
        rig.claim_restart_lease();
        match kind {
            "missing" => std::fs::remove_file(rig.marker_path()).unwrap(),
            "old" => {
                let instance = std::fs::read_to_string(rig.state.join("daemon-instance"))
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                assert!(
                    !instance.is_empty(),
                    "old evidence cannot name the run that wrote it"
                );
                std::fs::write(
                    rig.marker_path(),
                    json!({"instance": instance, "at": 1.0, "entries": []}).to_string(),
                )
                .unwrap();
            }
            "malformed" => std::fs::write(rig.marker_path(), b"{\"version\":2").unwrap(),
            _ => unreachable!(),
        }
        let out = rig.run(&["daemon", "restart", "--as", RESTART_IDENTITY]);
        assert!(
            !out.status.success(),
            "{kind}: restart returned a clean verdict over unusable shutdown evidence with hidden work\nstdout:\n{}\nstderr:\n{}\ndaemon.log:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            rig.log()
        );
        rig.assert_no_private_identity(&out);
    }
}

/// CAD-2207 settle gate: after the v2 marker is consumed and the successor
/// store is open, a previously live actor that becomes attention, disappears,
/// or changes its immutable uid must not settle cleanly. The test mutates the
/// successor's real rows while the production gate holds it, then releases and
/// observes the real restart verdict.
#[cfg(feature = "test-seam")]
#[test]
fn settle_gate_removed_changed_or_attention_actor_is_not_clean() {
    for mode in ["attention", "missing", "uid"] {
        let rig = Rig::new(true);
        rig.start();
        rig.wait_agent_live(PRIVATE_ALIAS, "private before settle-gate restart");
        rig.claim_restart_lease();
        let gate = rig._root.path().join(format!("settle-{mode}"));
        std::fs::create_dir_all(&gate).unwrap();
        let child = rig.spawn_with_env(
            &["daemon", "restart", "--as", RESTART_IDENTITY],
            &[("CADENCE_TEST_CAD2207_SETTLE_GATE", &gate)],
        );
        wait_gate(&gate);
        mutate_after_settle(&rig.state, PRIVATE_ALIAS, mode);
        release_gate(&gate);
        let out = child.wait_with_output().unwrap();
        assert!(
            !out.status.success(),
            "{mode}: a previously live actor settled cleanly after mutation\nstdout:\n{}\nstderr:\n{}\ndaemon.log:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            rig.log()
        );
        rig.assert_no_private_identity(&out);
    }
}

/// CAD-2207 begin-closing gate: a competing socket actor start blocked on the
/// lifecycle lock must be refused after begin_closing sets closing, or its
/// effect must make the restart verdict non-clean. It must never be silently
/// missed. The candidate is a disabled/stopped ordinary actor, so only the
/// competing start could make it live.
#[cfg(feature = "test-seam")]
#[test]
fn concurrent_resume_during_begin_closing_is_refused_or_captured() {
    const CANDIDATE: &str = "cad2207-race-candidate";
    let rig = Rig::new(false);
    {
        let store = Store::open(&rig.state.join("cadence.sqlite3")).unwrap();
        register_agent(&store, CANDIDATE, &rig.state);
        store.set_agent_state(CANDIDATE, "stopped", None).unwrap();
        store.set_enabled(CANDIDATE, false).unwrap();
    }
    let gate = rig._root.path().join("begin-closing");
    std::fs::create_dir_all(&gate).unwrap();
    rig.start_with_begin_closing_gate(&gate);
    rig.wait_agent_live(ORDINARY_ALIAS, "ordinary before close-race");
    rig.claim_restart_lease();
    let child = rig.spawn_with_env(&["daemon", "restart", "--as", RESTART_IDENTITY], &[]);
    wait_gate(&gate);
    let state = rig.state.clone();
    let competitor = std::thread::spawn(move || {
        cadence_agent::test_seam::scoped(cadence_agent::test_seam::Asserted::Operator, || {
            cadence_agent::client::rpc(&state, "agent_resume", json!({"alias": CANDIDATE}))
        })
    });
    // Let the competing dispatch reach the lifecycle lock, then let
    // begin_closing set `closing` and release ownership.
    std::thread::sleep(std::time::Duration::from_millis(100));
    release_gate(&gate);
    let competing = competitor.join().unwrap();
    let out = child.wait_with_output().unwrap();
    let refused = competing.is_err();
    let captured = !out.status.success();
    assert!(
        refused || captured,
        "a competing actor start was silently missed\ncompetitor: {competing:?}\nstdout:\n{}\nstderr:\n{}\ndaemon.log:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        rig.log()
    );
}
