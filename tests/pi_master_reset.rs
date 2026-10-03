//! CAD-1076 result checks (CAD-1090): the master keeps answering after
//! the gateway refuses its provider session's history. Each test runs
//! a real daemon, starts the real Pi master over
//! `tests/fixtures/pi-gateway.py` (Pi's RPC lines, gateway 400 on cue)
//! and talks to it the way the operator does: `thread_send`, then the
//! message state and the thread.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cadence_agent::adapter::ProviderEnv;
use cadence_agent::{client, daemon, master, reaper, slots::SlotConfig};
use serde_json::{json, Value};
use tempfile::{Builder, TempDir};

struct Master {
    _root: TempDir,
    state: PathBuf,
    stop: Arc<AtomicBool>,
    serve: Option<JoinHandle<cadence_agent::Result<()>>>,
}

impl Master {
    /// A daemon over its own tracker (with the operator's `[pi]` policy)
    /// whose Pi master has answered its bootstrap.
    fn start() -> Self {
        let root = Builder::new().prefix("c1076-").tempdir_in("/tmp").unwrap();
        let [state, pm, home] = ["s", "pm", "h"].map(|d| root.path().join(d));
        let mut init = Command::new(env!("CARGO_BIN_EXE_cadence"));
        init.args(["issue", "init"])
            .env("CADENCE_PM_DIR", &pm)
            .env("HOME", &home)
            .env_remove("CADENCE_ALIAS");
        let out = reaper::output(&mut init).unwrap();
        assert!(out.status.success(), "issue init: {out:?}");
        let yaml = pm.join("pm.yaml");
        let mut text = std::fs::read_to_string(&yaml).unwrap();
        text.push_str("\npi:\n  models:\n    allow: [\"fake/model-1\"]\n    default: {master: \"fake/model-1\", worker: \"fake/model-1\"}\n");
        std::fs::write(&yaml, text).unwrap();
        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi-gateway.py");
        let env = ProviderEnv::refusing_providers();
        env.set("CADENCE_PI_COMMAND", format!("python3 {}", fake.display()));
        env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
        env.set("HOME", home.to_str().unwrap());
        // This host may lack Landlock; the master then needs --unconfined.
        env.set(master::TEST_NO_LANDLOCK, "1");
        let stop = Arc::new(AtomicBool::new(false));
        let opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(stop.clone()),
            slots: Some(SlotConfig::default()),
            ..Default::default()
        };
        let dir = state.clone();
        let serve = Some(thread::spawn(move || daemon::serve_with(&dir, opts)));
        let m = Self {
            _root: root,
            state,
            stop,
            serve,
        };
        m.until("daemon health", || {
            client::rpc_timeout(&m.state, "health", json!({}), Duration::from_secs(1)).is_ok()
        });
        let unconfined = json!({"provider": "pi", "unconfined": true});
        m.rpc("master_start", unconfined);
        let boot = m.rpc("agent_show", json!({"alias": "master"}))["messages"][0]["id"].clone();
        let boot = m.settled(boot.as_str().unwrap());
        assert_eq!(boot["state"], "completed", "bootstrap: {boot}");
        m
    }

    fn rpc(&self, method: &str, params: Value) -> Value {
        client::rpc(&self.state, method, params).unwrap_or_else(|e| panic!("{method}: {e}"))
    }

    fn until(&self, what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done() {
            assert!(
                Instant::now() < deadline,
                "{what}: timed out; pi saw {:?}",
                self.journal()
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// The message once it is no longer queued or running.
    fn settled(&self, id: &str) -> Value {
        let mut found = Value::Null;
        self.until(&format!("message {id} settles"), || {
            let show = self.rpc("agent_show", json!({"alias": "master"}));
            found = show["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["id"] == id)
                .cloned()
                .unwrap_or_default();
            matches!(found["state"].as_str(), Some("completed" | "failed"))
        });
        found
    }

    /// The operator's chat message to the master, once settled.
    fn ask(&self, text: &str) -> Value {
        let sent = self.rpc("thread_send", json!({"alias": "master", "text": text}));
        self.settled(sent["message"].as_str().unwrap_or_else(|| panic!("{sent}")))
    }

    fn thread(&self) -> Vec<Value> {
        let read = self.rpc("thread_read", json!({"alias": "master", "limit": 500}));
        read["entries"].as_array().unwrap().clone()
    }

    /// Every request the fake Pi received, in order.
    fn journal(&self) -> Vec<Value> {
        let file = master::workdir(&self.state).join("pi-rpc.jsonl");
        let text = std::fs::read_to_string(file).unwrap_or_default();
        text.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn count(&self, rpc: &str, marker: Option<&str>) -> usize {
        self.journal()
            .iter()
            .filter(|r| r["rpc"] == rpc && marker.is_none_or(|m| r["marker"] == m))
            .count()
    }

    fn resets_for(&self, message: &Value) -> usize {
        self.thread()
            .iter()
            .filter(|e| {
                e["payload"]["event"] == "provider_session_reset"
                    && e["payload"]["message"] == message["id"]
            })
            .count()
    }
}

impl Drop for Master {
    fn drop(&mut self) {
        let _ = client::rpc_timeout(&self.state, "shutdown", json!({}), Duration::from_secs(5));
        self.stop.store(true, Ordering::SeqCst);
        let serve = self.serve.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !serve.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        // A daemon wedged by a failing case is left detached, not joined.
        if serve.is_finished() {
            let _ = serve.join();
        }
    }
}

/// R1: a session the gateway refuses, before any tool ran, is reset
/// once and the operator's message is answered from the new session.
#[test]
fn a_refused_master_session_is_reset_and_the_message_answered() {
    let m = Master::start();
    let msg = m.ask("what is running? REFUSED");
    assert_eq!(msg["state"], "completed", "{msg}");
    assert_eq!(m.count("new_session", None), 1, "{:?}", m.journal());
    assert_eq!(m.count("prompt", Some("REFUSED")), 2, "{:?}", m.journal());
    assert_eq!(
        m.resets_for(&msg),
        1,
        "the thread shows the reset: {:?}",
        m.thread()
    );
    let answered = m.thread().iter().any(|e| {
        e["text"]
            .as_str()
            .is_some_and(|t| t.contains("master answer in session 1"))
    });
    assert!(answered, "the answer reaches the thread: {:?}", m.thread());
}

/// R2 (forbidden harm): a turn whose tool already ran is never
/// replayed after the refusal; the message fails with the provider's
/// error and the session is left alone.
#[test]
fn a_refused_turn_that_ran_a_tool_fails_and_is_never_replayed() {
    let m = Master::start();
    let msg = m.ask("deploy the docs TOOL-REFUSED");
    assert_eq!(msg["state"], "failed", "{msg}");
    assert!(
        msg.to_string().contains("invalid_request"),
        "the refusal is visible: {msg}"
    );
    assert_eq!(m.count("tool", None), 1, "{:?}", m.journal());
    assert_eq!(
        m.count("prompt", Some("TOOL-REFUSED")),
        1,
        "{:?}",
        m.journal()
    );
    assert_eq!(m.count("new_session", None), 0, "{:?}", m.journal());
    assert_eq!(m.resets_for(&msg), 0, "{:?}", m.thread());
}

/// R3: a refusal that recurs on the fresh session fails the message
/// once — one reset, one retry, no loop.
#[test]
fn a_refusal_after_the_reset_fails_the_message_once() {
    let m = Master::start();
    let msg = m.ask("summarize the board ALWAYS-REFUSED");
    assert_eq!(msg["state"], "failed", "{msg}");
    assert!(
        msg.to_string().contains("invalid_request"),
        "the refusal is visible: {msg}"
    );
    assert_eq!(m.count("new_session", None), 1, "{:?}", m.journal());
    assert_eq!(
        m.count("prompt", Some("ALWAYS-REFUSED")),
        2,
        "{:?}",
        m.journal()
    );
    assert_eq!(m.resets_for(&msg), 1, "{:?}", m.thread());
}
