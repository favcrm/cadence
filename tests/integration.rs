//! End-to-end tests: real socket daemon in-process, fake provider.
//! These exercise the observable contract — queue order, idempotency,
//! restart fencing, approval brokering, serialization — without model calls.

use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cadence_agent::client;
use cadence_agent::daemon;
use cadence_agent::store::{NewAgent, Store, Take};
use serde_json::{json, Value};
use tempfile::TempDir;

struct TestDaemon {
    dir: TempDir,
    state: PathBuf,
    handle: Option<JoinHandle<cadence_agent::Result<()>>>,
}

impl TestDaemon {
    fn start() -> Self {
        let dir = TempDir::new().unwrap();
        let state = dir.path().to_path_buf();
        std::fs::create_dir_all(&state).unwrap();
        let owned = state.clone();
        let handle = thread::spawn(move || daemon::serve(&owned));
        let daemon = Self {
            dir,
            state,
            handle: Some(handle),
        };
        daemon.wait_health();
        daemon
    }

    /// Start a daemon over a pre-seeded state directory.
    fn start_on(state: PathBuf) -> Self {
        let dir = TempDir::new().unwrap(); // keeps lifetime uniform
        let owned = state.clone();
        let handle = thread::spawn(move || daemon::serve(&owned));
        let daemon = Self {
            dir,
            state,
            handle: Some(handle),
        };
        daemon.wait_health();
        daemon
    }

    fn wait_health(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.rpc("health", json!({})).is_ok() {
                return;
            }
            assert!(Instant::now() < deadline, "daemon did not become healthy");
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn rpc(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        client::rpc(&self.state, method, params)
    }

    fn register(&self, alias: &str) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        self.rpc(
            "agent_register",
            json!({"alias": alias, "provider": "fake",
                   "endpoint_kind": "fake", "cwd": cwd}),
        )
        .unwrap();
    }

    fn wait_agent(&self, alias: &str, want: &str, secs: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let agent = self
                .rpc("agent_show", json!({"alias": alias}))
                .unwrap()
                .remove("agent");
            if agent["state"].as_str() == Some(want) {
                return agent;
            }
            assert!(
                Instant::now() < deadline,
                "agent {alias} never reached {want}: {agent}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_message(&self, alias: &str, id: &str, want: &[&str], secs: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let show = self.rpc("agent_show", json!({"alias": alias})).unwrap();
            let messages = show["messages"].as_array().unwrap();
            if let Some(m) = messages.iter().find(|m| m["id"].as_str() == Some(id)) {
                if want.contains(&m["state"].as_str().unwrap_or("")) {
                    return m.clone();
                }
            }
            assert!(
                Instant::now() < deadline,
                "message {id} never reached {want:?}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn message_state(&self, alias: &str, id: &str) -> String {
        self.rpc("agent_show", json!({"alias": alias})).unwrap()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"].as_str() == Some(id))
            .unwrap()["state"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        let _ = self.rpc("shutdown", json!({}));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

trait Get {
    fn remove(&mut self, key: &str) -> Value;
}
impl Get for Value {
    fn remove(&mut self, key: &str) -> Value {
        self.as_object_mut().unwrap().remove(key).unwrap()
    }
}

#[test]
fn fifo_queue_and_idempotent_send() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    for n in 1..=3 {
        d.rpc(
            "agent_send",
            json!({"alias": "w1", "text": format!("task {n}"),
                   "message": format!("m{n}")}),
        )
        .unwrap();
    }
    // Duplicate of the exact same envelope is a no-op.
    let dup = d
        .rpc(
            "agent_send",
            json!({"alias": "w1", "text": "task 1", "message": "m1"}),
        )
        .unwrap();
    assert_eq!(dup["duplicate"], true);
    // Same id, different content: conflict.
    let conflict = d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "different", "message": "m1"}),
    );
    assert!(conflict.is_err());
    // All three complete in submission order.
    for n in 1..=3 {
        let m = d.wait_message("w1", &format!("m{n}"), &["completed"], 15);
        assert_eq!(m["result"]["text"], format!("FAKE_REPLY: task {n}"));
    }
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let seq: Vec<i64> = show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["seq"].as_i64().unwrap())
        .collect();
    assert_eq!(seq, vec![1, 2, 3]);
}

#[test]
fn agent_addressable_by_native_id() {
    let d = TestDaemon::start();
    d.register("w9");
    d.wait_agent("w9", "idle", 10);
    // The fake adapter publishes thread_id "fake-thread-w9" at open —
    // every alias-taking verb must resolve it to the canonical alias,
    // the same way a Devin session slug resolves.
    let native = "fake-thread-w9";
    let show = d.rpc("agent_show", json!({"alias": native})).unwrap();
    assert_eq!(show["agent"]["alias"], "w9");
    d.rpc(
        "agent_send",
        json!({"alias": native, "text": "task", "message": "m-native"}),
    )
    .unwrap();
    let m = d.wait_message("w9", "m-native", &["completed"], 15);
    assert_eq!(m["result"]["text"], "FAKE_REPLY: task");
    let events = d.rpc("agent_events", json!({"alias": native})).unwrap();
    assert!(!events["events"].as_array().unwrap().is_empty());
    // An exact alias always wins over another agent's native id.
    d.register(native);
    d.wait_agent(native, "idle", 10);
    let show = d.rpc("agent_show", json!({"alias": native})).unwrap();
    assert_eq!(show["agent"]["alias"], native);
    assert_eq!(show["agent"]["thread_id"], format!("fake-thread-{native}"));
}

#[test]
fn result_routing_wakes_pm() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register("w1");
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "review this", "message": "work-1",
               "reply_to": "pm"}),
    )
    .unwrap();
    d.wait_message("w1", "work-1", &["completed"], 15);
    // The routed delivery lands on pm with the deterministic id.
    let show = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    let routed = &show["messages"].as_array().unwrap()[0];
    assert_eq!(routed["source"], "worker_result");
    assert!(routed["reply_to"].is_null());
    assert!(routed["body"].as_str().unwrap().contains("work-1"));
    // pm's fake provider also completes it.
    let routed_id = routed["id"].as_str().unwrap().to_string();
    d.wait_message("pm", &routed_id, &["completed"], 15);
}

#[test]
fn upstream_param_defaults_reply_to() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register("other");
    // A worker joined to a group carries params.upstream = <pm alias>.
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "agent_register",
        json!({"alias": "w1", "provider": "fake", "endpoint_kind": "fake",
               "cwd": cwd, "params": "{\"upstream\":\"pm\"}"}),
    )
    .unwrap();
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("other", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    // No explicit reply_to — the upstream wiring routes the result to pm.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "work", "message": "u1"}),
    )
    .unwrap();
    d.wait_message("w1", "u1", &["completed"], 15);
    let show = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    let routed = &show["messages"].as_array().unwrap()[0];
    assert_eq!(routed["source"], "worker_result");
    assert!(routed["body"].as_str().unwrap().contains("u1"));
    let routed_id = routed["id"].as_str().unwrap().to_string();
    d.wait_message("pm", &routed_id, &["completed"], 15);
    // An explicit reply_to still wins over the upstream default.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "more work", "message": "u2",
               "reply_to": "other"}),
    )
    .unwrap();
    d.wait_message("w1", "u2", &["completed"], 15);
    let other = d.rpc("agent_show", json!({"alias": "other"})).unwrap();
    assert!(other["messages"].as_array().unwrap().iter().any(|m| {
        m["source"] == "worker_result" && m["body"].as_str().unwrap_or_default().contains("u2")
    }));
    // pm saw only the first routed delivery — u2 went to `other`.
    let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    assert_eq!(
        pm["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["source"] == "worker_result")
            .count(),
        1
    );
}

#[test]
fn approval_lifecycle() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "NEED_INPUT:run-tests", "message": "a1"}),
    )
    .unwrap();
    d.wait_agent("w1", "waiting_input", 10);
    let requests = d.rpc("agent_requests", json!({"alias": "w1"})).unwrap();
    let list = requests["requests"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["method"], "item/commandExecution/requestApproval");
    let handle = list[0]["request"].as_str().unwrap();
    // Wrong shape rejected.
    assert!(d
        .rpc(
            "agent_respond",
            json!({"alias": "w1", "request": handle, "decision": "maybe"}),
        )
        .is_err());
    d.rpc(
        "agent_respond",
        json!({"alias": "w1", "request": handle, "decision": "accept"}),
    )
    .unwrap();
    let m = d.wait_message("w1", "a1", &["completed"], 15);
    assert_eq!(
        m["result"]["text"],
        "FAKE_DECIDED:{\"decision\":\"accept\"}"
    );
    assert_eq!(
        d.rpc("agent_requests", json!({"alias": "w1"})).unwrap()["requests"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn restart_fences_unknown_inflight() {
    // Seed a state dir with an in-flight attempt, then start a daemon.
    let seeded = TempDir::new().unwrap();
    let state = seeded.path().to_path_buf();
    {
        let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
        let cwd = state.to_str().unwrap().to_string();
        store
            .register_agent(&NewAgent {
                alias: "w1",
                provider: "fake",
                endpoint_kind: "fake",
                role: "worker",
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
            })
            .unwrap();
        store.enqueue("w1", "work", None, "m1", "user").unwrap();
        match store.take_queued("w1").unwrap() {
            Take::Message(m) => assert_eq!(m.id, "m1"),
            _ => panic!("expected a message"),
        }
        // Simulate crash: store dropped while m1 is 'submitting'.
    }
    let d = TestDaemon::start_on(state);
    // The fenced actor lands in attention, not a silent relaunch.
    let agent = d.wait_agent("w1", "attention", 10);
    assert!(agent["error"].as_str().unwrap().contains("Uncertain"));
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let m1 = &show["messages"].as_array().unwrap()[0];
    assert_eq!(m1["state"], "unknown");
    // New work is durable but NOT executed while fenced.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "later", "message": "m2"}),
    )
    .unwrap();
    thread::sleep(Duration::from_secs(1));
    let m2 = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "m2")
        .unwrap()
        .clone();
    assert_eq!(m2["state"], "queued");
}

#[test]
fn turns_are_serialized_per_agent() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    for n in 1..=4 {
        d.rpc(
            "agent_send",
            json!({"alias": "w1", "text": format!("job {n}")}),
        )
        .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
        let messages = show["messages"].as_array().unwrap();
        if messages.iter().all(|m| m["state"] == "completed") && messages.len() == 4 {
            // Started strictly in queue order.
            let turns: Vec<&str> = messages
                .iter()
                .map(|m| m["turn_id"].as_str().unwrap())
                .collect();
            assert_eq!(
                turns,
                vec!["fake-turn-1", "fake-turn-2", "fake-turn-3", "fake-turn-4"]
            );
            return;
        }
        assert!(Instant::now() < deadline, "turns did not finish serialized");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn stop_and_resume() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "stopped", 10);
    d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();
    let agent = d.wait_agent("w1", "idle", 10);
    assert_eq!(agent["thread_id"], "fake-thread-w1");
}

#[test]
fn unknown_outcome_never_replays() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "DISCONNECT", "message": "x1"}),
    )
    .unwrap();
    let m = d.wait_message("w1", "x1", &["unknown"], 15);
    assert!(m["error"].as_str().unwrap().contains("uncertain"));
    d.wait_agent("w1", "attention", 10);
    // Subsequent messages stay queued — no automatic replay or relaunch.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "after", "message": "x2"}),
    )
    .unwrap();
    thread::sleep(Duration::from_secs(1));
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let x2 = show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "x2")
        .unwrap();
    assert_eq!(x2["state"], "queued");
}

#[test]
fn cli_doctor_smoke() {
    let dir = TempDir::new().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .args(["--state-dir"])
        .arg(dir.path())
        .arg("doctor")
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["checks"]["storage"]["ok"], true);
}

// ---- review regressions: daemon singleton, actor ownership, bounded
// disconnect/stop, unknown fencing, initialization cleanup ----

#[test]
fn second_daemon_fails_without_touching_state() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    // Active work in-flight: a second daemon's recovery must never run.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "NEED_INPUT:hold", "message": "m1"}),
    )
    .unwrap();
    d.wait_agent("w1", "waiting_input", 10);
    let err = daemon::serve(&d.state).unwrap_err();
    assert!(
        err.to_string().contains("already owns"),
        "unexpected error: {err}"
    );
    // The first owner is untouched: socket answers, m1 still in-flight.
    d.rpc("health", json!({})).unwrap();
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let m1 = &show["messages"].as_array().unwrap()[0];
    assert_eq!(m1["state"], "running");
    assert_eq!(
        d.rpc("agent_requests", json!({"alias": "w1"})).unwrap()["requests"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn daemon_restarts_after_owner_exit() {
    let seeded = TempDir::new().unwrap();
    let state = seeded.path().to_path_buf();
    {
        let d = TestDaemon::start_on(state.clone());
        d.rpc("health", json!({})).unwrap();
        // Drop shuts the daemon down and releases the singleton lock.
    }
    let d2 = TestDaemon::start_on(state);
    d2.rpc("health", json!({})).unwrap();
}

#[test]
fn resume_rejected_while_actor_stopping() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    // SLEEP ignores interrupt; only a forced close ends the turn, which
    // makes the in-flight attempt unknown — a real stopping window.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "SLEEP:60", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("w1", "m1", &["running"], 10);
    let stop = {
        let state = d.state.clone();
        thread::spawn(move || client::rpc(&state, "agent_stop", json!({"alias": "w1"})))
    };
    // While the stop grace runs, the actor is still owned: resume must
    // be rejected and must not re-enable the agent.
    thread::sleep(Duration::from_millis(500));
    let resumed = d.rpc("agent_resume", json!({"alias": "w1"}));
    assert!(resumed.is_err(), "resume during stop must be rejected");
    let stopped = stop.join().unwrap().unwrap();
    // Forced close made the in-flight attempt unknown -> fenced.
    assert_eq!(stopped["state"], "attention");
    let agent = d.wait_agent("w1", "attention", 10);
    assert_eq!(agent["enabled"], false);
    d.wait_message("w1", "m1", &["unknown"], 10);
    // A later resume hits the fence, not a relaunch, and stays disabled.
    let fenced = d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();
    assert_eq!(fenced["state"], "attention");
    let agent = d
        .rpc("agent_show", json!({"alias": "w1"}))
        .unwrap()
        .remove("agent");
    assert_eq!(agent["enabled"], false);
    // No second actor ever existed: m2 is accepted but never runs.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "later", "message": "m2"}),
    )
    .unwrap();
    thread::sleep(Duration::from_millis(500));
    assert_eq!(d.message_state("w1", "m2"), "queued");
}

#[test]
fn concurrent_resume_has_single_winner() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "stopped", 10);
    let mut racers = Vec::new();
    for _ in 0..4 {
        let state = d.state.clone();
        racers.push(thread::spawn(move || {
            client::rpc(&state, "agent_resume", json!({"alias": "w1"}))
        }));
    }
    let results: Vec<_> = racers.into_iter().map(|t| t.join().unwrap()).collect();
    let winners = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(
        winners, 1,
        "expected exactly one resume to win: {results:?}"
    );
    d.wait_agent("w1", "idle", 10);
}

#[test]
fn stop_during_approval_is_bounded() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "NEED_INPUT:block", "message": "m1"}),
    )
    .unwrap();
    d.wait_agent("w1", "waiting_input", 10);
    let began = Instant::now();
    let stopped = d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(15),
        "stop was not bounded"
    );
    assert_eq!(stopped["state"], "stopped");
    d.wait_agent("w1", "stopped", 10);
    // Interrupt aborts the provider's approval wait; the attempt ends
    // interrupted, not stuck running.
    d.wait_message("w1", "m1", &["interrupted"], 10);
    assert_eq!(
        d.rpc("agent_requests", json!({"alias": "w1"})).unwrap()["requests"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn unclassifiable_completion_fences_agent() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "BAD_STATUS", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("w1", "m1", &["unknown"], 15);
    d.wait_agent("w1", "attention", 10);
    // Queued work is preserved but never run while fenced.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "after", "message": "m2"}),
    )
    .unwrap();
    thread::sleep(Duration::from_millis(500));
    assert_eq!(d.message_state("w1", "m2"), "queued");
}

// ---- mock Codex provider over real stdio (no model calls) ----

/// Serialize tests that override the process-wide provider command.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A stdio JSON-RPC provider speaking just enough of the app-server wire
/// to reach each failure mode. Writes its pid to a file for leak checks.
const MOCK_PY: &str = r#"
import json, os, sys, time
pidfile, mode = sys.argv[1], sys.argv[2]
with open(pidfile, "w") as f:
    f.write(str(os.getpid()))
def emit(msg):
    sys.stdout.write(json.dumps(msg) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    try: msg = json.loads(line)
    except Exception: continue
    mid, method = msg.get("id"), msg.get("method")
    if mid is None: continue
    if method == "initialize":
        if mode == "slow-init": time.sleep(30)
        emit({"id": mid, "result": {"serverInfo": {"name": "mock", "version": "0"}}})
    elif method in ("thread/start", "thread/resume"):
        if mode == "bad-thread":
            emit({"id": mid, "result": {"thread": {}}})
        else:
            emit({"id": mid, "result": {"thread": {"id": "th-1", "sessionId": "s-1"}}})
    elif method == "turn/start":
        if mode == "bad-turn":
            emit({"id": mid, "result": {"turn": {}}})
        elif mode == "die-after-start":
            emit({"id": mid, "result": {"turn": {"id": "t-1"}}}); os._exit(0)
        elif mode == "silent":
            emit({"id": mid, "result": {"turn": {"id": "t-1"}}})
        else:
            emit({"id": mid, "result": {"turn": {"id": "t-1"}}})
            emit({"method": "turn/completed", "params": {"turn": {
                "id": "t-1", "status": "completed", "items": [
                    {"id": "i1", "type": "agentMessage",
                     "text": "MOCK_OK", "phase": "final_answer"}]}}})
    elif method == "turn/interrupt":
        emit({"id": mid, "result": {}})
"#;

struct MockCodex {
    _guard: std::sync::MutexGuard<'static, ()>,
    pidfile: PathBuf,
}

// ---- mock Codex app-server over a real WebSocket (no model calls) ----

/// A WebSocket JSON-RPC provider speaking the same app-server wire as
/// MOCK_PY. Parses `--listen ws://host:port` from argv (appended by the
/// transport), handshakes with stdlib sockets, and serves text frames.
/// Turn text directives: `DIE` closes the TCP connection after the ack,
/// `DIE2` sends a WS close frame instead, `FRAG` delivers
/// turn/completed as two continuations with an interleaved ping,
/// `NEED_INPUT:x` raises a server->client approval request that must be
/// answered before the turn completes, `NEED_INPUT_EXT:x` raises one
/// then resolves it externally via `serverRequest/resolved` once
/// `<pidfile>.resolve` appears. Modes: `silent` never completes
/// non-seed turns, `no-upgrade` accepts TCP but never answers the WS
/// handshake, `drip` feeds a valid 101 one byte/second,
/// `bad-upgrade` answers 200 instead of 101, `ping-first` pings before
/// the non-seed ack and records pong receipt in `<pidfile>.pong`.
/// The handshake is strict: a Sec-WebSocket-Key that does not decode
/// to exactly 16 bytes is refused with 400.
const MOCK_WS_PY: &str = r##"
import base64, hashlib, json, os, socket, struct, sys, threading, time

pidfile, mode = sys.argv[1], sys.argv[2]
url = sys.argv[sys.argv.index("--listen") + 1]
host, port = url.split("://", 1)[1].split(":")
srv = socket.socket()
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind((host, int(port)))
srv.listen(4)
with open(pidfile, "w") as f:
    f.write(str(os.getpid()))
GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
SEED = "Cadence endpoint initialization"

def recv_exact(conn, n):
    data = b""
    while len(data) < n:
        chunk = conn.recv(n - len(data))
        if not chunk:
            return None
        data += chunk
    return data

def read_frame(conn):
    hdr = recv_exact(conn, 2)
    if hdr is None:
        return None, None
    opcode, flags = hdr[0] & 0x0F, hdr[1]
    length = flags & 0x7F
    if length == 126:
        length = struct.unpack(">H", recv_exact(conn, 2))[0]
    elif length == 127:
        length = struct.unpack(">Q", recv_exact(conn, 8))[0]
    mask = recv_exact(conn, 4) if flags & 0x80 else b""
    payload = recv_exact(conn, length) if length else b""
    if payload is None:
        return None, None
    if mask:
        payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
    return opcode, payload

def send_frag(conn, fin, opcode, payload):
    n = len(payload)
    if n < 126:
        hdr = bytes([(0x80 if fin else 0) | opcode, n])
    elif n < 65536:
        hdr = bytes([(0x80 if fin else 0) | opcode, 126]) + struct.pack(">H", n)
    else:
        hdr = bytes([(0x80 if fin else 0) | opcode, 127]) + struct.pack(">Q", n)
    conn.sendall(hdr + payload)

def send_frame(conn, opcode, payload):
    send_frag(conn, True, opcode, payload)

def send_json(conn, msg):
    send_frame(conn, 1, json.dumps(msg).encode())

def send_fragmented_complete(conn, turn, text):
    # One message split across two continuations with an interleaved
    # ping: the client must reassemble it and answer the control frame.
    body = json.dumps({"method": "turn/completed", "params": {"turn": {
        "id": turn, "status": "completed", "items": [
            {"id": "i1", "type": "agentMessage",
             "text": text, "phase": "final_answer"}]}}}).encode()
    half = len(body) // 2
    send_frag(conn, False, 0x1, body[:half])
    send_frame(conn, 0x9, b"mid-frag")
    send_frag(conn, True, 0x0, body[half:])

def complete(conn, turn, text):
    send_json(conn, {"method": "turn/completed", "params": {"turn": {
        "id": turn, "status": "completed", "items": [
            {"id": "i1", "type": "agentMessage",
             "text": text, "phase": "final_answer"}]}}})

def handshake(conn):
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = conn.recv(4096)
        if not chunk:
            return False
        data += chunk
    key = ""
    for line in data.decode().split("\r\n"):
        if line.lower().startswith("sec-websocket-key:"):
            key = line.split(":", 1)[1].strip()
    # RFC 6455: the client nonce must decode to exactly 16 bytes.
    # Refuse anything else, as a strict standards-compliant server does.
    try:
        valid = len(base64.b64decode(key)) == 16
    except Exception:
        valid = False
    if not valid:
        conn.sendall(b"HTTP/1.1 400 Bad Request\r\n\r\n")
        return False
    accept = base64.b64encode(
        hashlib.sha1((key + GUID).encode()).digest()).decode()
    conn.sendall((
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Accept: {accept}\r\n\r\n").encode())
    return True

def ping_check(conn):
    # Ping, then expect a pong before completing: proves the client
    # serializes control replies on the same write path as requests.
    send_frame(conn, 0x9, b"ping-check")
    conn.settimeout(5)
    pong = False
    try:
        while True:
            op, _ = read_frame(conn)
            if op is None:
                break
            if op == 0xA:
                pong = True
                break
    except Exception:
        pass
    conn.settimeout(None)
    with open(pidfile + ".pong", "w") as f:
        f.write("yes" if pong else "no")

def external_resolve(conn):
    # An attached TUI answered the approval: once the test drops the
    # trigger file, resolve the pending request outside the client and
    # let the turn finish without a client response.
    deadline = time.time() + 30
    while not os.path.exists(pidfile + ".resolve"):
        if time.time() > deadline:
            break
        time.sleep(0.05)
    send_json(conn, {"method": "serverRequest/resolved",
        "params": {"requestId": "srv-1", "threadId": "th-1"}})
    complete(conn, "t-1", "MOCK_OK")

def read_request(conn):
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = conn.recv(4096)
        if not chunk:
            return None
        data += chunk
    key = ""
    for line in data.decode().split("\r\n"):
        if line.lower().startswith("sec-websocket-key:"):
            key = line.split(":", 1)[1].strip()
    return key

def handle(conn):
    if mode == "no-upgrade":
        # Accept TCP, never answer the handshake. The client's bounded
        # handshake must give up and clean up the owned process.
        time.sleep(3600)
        return
    if mode == "drip":
        # Answer with a valid 101 one byte per second: only an absolute
        # deadline bounds this — per-read timeouts never fire.
        key = read_request(conn)
        if key is None:
            return
        accept = base64.b64encode(
            hashlib.sha1((key + GUID).encode()).digest()).decode()
        response = ("HTTP/1.1 101 Switching Protocols\r\n"
            "Upgrade: websocket\r\nConnection: Upgrade\r\n"
            f"Sec-WebSocket-Accept: {accept}\r\n\r\n").encode()
        for byte in response:
            conn.sendall(bytes([byte]))
            time.sleep(1)
        time.sleep(3600)
        return
    if mode == "bad-upgrade":
        # Refuse the upgrade outright: HTTP 200, not 101.
        if read_request(conn) is None:
            return
        conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
        time.sleep(3600)
        return
    if not handshake(conn):
        return
    approvals = set()
    while True:
        op, payload = read_frame(conn)
        if op is None or op == 8:
            break
        if op == 9:
            send_frame(conn, 0xA, payload)
            continue
        if op != 1:
            continue
        try:
            msg = json.loads(payload)
        except Exception:
            continue
        mid, method = msg.get("id"), msg.get("method")
        if method is None:
            if mid in approvals:
                approvals.discard(mid)
                if not approvals:
                    complete(conn, "t-1", "MOCK_OK")
            continue
        if method == "initialize":
            if mode == "slow-init":
                time.sleep(30)
            send_json(conn, {"id": mid, "result": {
                "serverInfo": {"name": "mock-ws", "version": "0"}}})
        elif method in ("thread/start", "thread/resume"):
            send_json(conn, {"id": mid, "result": {
                "thread": {"id": "th-1", "sessionId": "s-1"}}})
        elif method == "turn/start":
            text = ""
            try:
                text = msg["params"]["input"][0]["text"]
            except Exception:
                pass
            if mode == "ping-first" and not text.startswith(SEED):
                ping_check(conn)
            send_json(conn, {"id": mid, "result": {"turn": {"id": "t-1"}}})
            if text.startswith("DIE2"):
                send_frame(conn, 8, b"")
                return
            if text.startswith("DIE"):
                conn.close()
                return
            if text.startswith("FRAG"):
                send_fragmented_complete(conn, "t-1", "MOCK_OK")
                continue
            if text.startswith(SEED):
                complete(conn, "t-1", "READY")
            elif mode == "silent":
                pass
            elif text.startswith("NEED_INPUT_EXT"):
                approvals.add("srv-1")
                send_json(conn, {"id": "srv-1",
                    "method": "item/commandExecution/requestApproval",
                    "params": {"command": "x"}})
                external_resolve(conn)
                return
            elif text.startswith("NEED_INPUT2"):
                # Two outstanding approvals: answering one must leave
                # the agent waiting_input until both are answered.
                approvals.update(("srv-1", "srv-2"))
                for rid in ("srv-1", "srv-2"):
                    send_json(conn, {"id": rid,
                        "method": "item/commandExecution/requestApproval",
                        "params": {"command": "x"}})
            elif text.startswith("NEED_INPUT"):
                approvals.add("srv-1")
                send_json(conn, {"id": "srv-1",
                    "method": "item/commandExecution/requestApproval",
                    "params": {"command": "x"}})
            else:
                complete(conn, "t-1", "MOCK_OK")
        elif method == "turn/interrupt":
            send_json(conn, {"id": mid, "result": {}})
    conn.close()

while True:
    conn, _ = srv.accept()
    threading.Thread(target=handle, args=(conn,), daemon=True).start()
"##;

impl TestDaemon {
    /// Install a mock codex command for `mode`, returning its pidfile.
    fn mock_codex(&self, mode: &str) -> MockCodex {
        let guard = ENV_LOCK.lock().unwrap();
        let pidfile = self.dir.path().join(format!("mock-{mode}.pid"));
        let script = self.dir.path().join(format!("mock-{mode}.py"));
        std::fs::write(&script, MOCK_PY).unwrap();
        std::env::set_var(
            "CADENCE_CODEX_COMMAND",
            format!(
                "python3 {} {} {}",
                script.display(),
                pidfile.display(),
                mode
            ),
        );
        MockCodex {
            _guard: guard,
            pidfile,
        }
    }

    /// Install a mock WebSocket app-server command for `mode`. `dir`
    /// hosts the script + pidfile and must outlive every daemon that
    /// will spawn it (restart tests use the seeded state dir).
    fn mock_codex_ws_at(&self, dir: &Path, mode: &str) -> MockCodex {
        let guard = ENV_LOCK.lock().unwrap();
        let pidfile = dir.join(format!("mock-ws-{mode}.pid"));
        let script = dir.join(format!("mock-ws-{mode}.py"));
        std::fs::write(&script, MOCK_WS_PY).unwrap();
        std::env::set_var(
            "CADENCE_CODEX_WS_COMMAND",
            format!(
                "python3 {} {} {}",
                script.display(),
                pidfile.display(),
                mode
            ),
        );
        MockCodex {
            _guard: guard,
            pidfile,
        }
    }

    fn mock_codex_ws(&self, mode: &str) -> MockCodex {
        self.mock_codex_ws_at(self.dir.path(), mode)
    }

    fn register_codex(&self, alias: &str) {
        self.register_kind(alias, "managed");
    }

    fn register_codex_ws(&self, alias: &str) {
        self.register_kind(alias, "managed-ws");
    }

    fn register_kind(&self, alias: &str, endpoint_kind: &str) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        self.rpc(
            "agent_register",
            json!({"alias": alias, "provider": "codex",
                   "endpoint_kind": endpoint_kind, "cwd": cwd}),
        )
        .unwrap();
    }
}

impl Drop for MockCodex {
    fn drop(&mut self) {
        std::env::remove_var("CADENCE_CODEX_COMMAND");
        std::env::remove_var("CADENCE_CODEX_WS_COMMAND");
    }
}

fn pid_alive(path: &Path) -> bool {
    let Ok(pid) = std::fs::read_to_string(path) else {
        return true; // not written yet -> treat as alive until proven
    };
    PathBuf::from(format!("/proc/{}", pid.trim())).exists()
}

fn wait_pid_gone(path: &Path, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while pid_alive(path) {
        assert!(
            Instant::now() < deadline,
            "provider process still alive after {secs}s"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn transport_eof_fences_turn_quickly() {
    let d = TestDaemon::start();
    let _mock = d.mock_codex("die-after-start");
    d.register_codex("w1");
    d.wait_agent("w1", "idle", 15);
    let began = Instant::now();
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "hi", "message": "m1"}),
    )
    .unwrap();
    // EOF must reach the turn-completion wait; without propagation this
    // would sit on turn_cv until the 600s deadline.
    d.wait_message("w1", "m1", &["unknown"], 20);
    assert!(
        began.elapsed() < Duration::from_secs(20),
        "EOF did not wake the turn wait"
    );
    d.wait_agent("w1", "attention", 10);
}

#[test]
fn stop_is_bounded_when_interrupt_is_ignored() {
    let d = TestDaemon::start();
    let mock = d.mock_codex("silent");
    d.register_codex("w1");
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "hi", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("w1", "m1", &["running"], 15);
    let began = Instant::now();
    let stopped = d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(20),
        "stop was not bounded when interrupt was ignored"
    );
    // Forced close made the outcome unknown; the fence is preserved
    // rather than overwritten with a clean stop.
    assert_eq!(stopped["state"], "attention");
    d.wait_message("w1", "m1", &["unknown"], 10);
    wait_pid_gone(&mock.pidfile, 10);
}

#[test]
fn malformed_turn_start_is_unknown_not_failed() {
    let d = TestDaemon::start();
    let _mock = d.mock_codex("bad-turn");
    d.register_codex("w1");
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "hi", "message": "m1"}),
    )
    .unwrap();
    // Acknowledged but uncorrelatable: the provider may have started work,
    // so the attempt is fenced unknown — not a definitive failure.
    let m1 = d.wait_message("w1", "m1", &["unknown"], 20);
    assert!(m1["error"].as_str().unwrap().contains("uncertain"));
    d.wait_agent("w1", "attention", 10);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "later", "message": "m2"}),
    )
    .unwrap();
    thread::sleep(Duration::from_millis(500));
    assert_eq!(d.message_state("w1", "m2"), "queued");
}

#[test]
fn malformed_init_leaves_no_provider_process() {
    let d = TestDaemon::start();
    let mock = d.mock_codex("bad-thread");
    d.register_codex("w1");
    let agent = d.wait_agent("w1", "attention", 15);
    assert!(agent["error"].as_str().unwrap().contains("thread"));
    // The failed initialization must not leave its provider running.
    wait_pid_gone(&mock.pidfile, 10);
}

#[test]
fn stop_on_fenced_agent_preserves_attention() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "DISCONNECT", "message": "m1"}),
    )
    .unwrap();
    let fenced = d.wait_agent("w1", "attention", 10);
    let reason = fenced["error"].clone();
    assert!(reason.as_str().unwrap().contains("Uncertain"));
    // Stop only disables: the fence state and its reason stay visible.
    let stopped = d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    assert_eq!(stopped["state"], "attention");
    let agent = d.wait_agent("w1", "attention", 10);
    assert_eq!(agent["enabled"], false);
    assert_eq!(agent["error"], reason);
    // Repeated stop is idempotent and still does not mask the fence.
    let again = d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    assert_eq!(again["state"], "attention");
    let agent = d.wait_agent("w1", "attention", 5);
    assert_eq!(agent["error"], reason);
    assert_eq!(d.message_state("w1", "m1"), "unknown");
    let fenced = d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();
    assert_eq!(fenced["state"], "attention");
}

#[test]
fn concurrent_stops_are_idempotent() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "NEED_INPUT:x", "message": "m1"}),
    )
    .unwrap();
    d.wait_agent("w1", "waiting_input", 10);
    let mut racers = Vec::new();
    for _ in 0..2 {
        let state = d.state.clone();
        racers.push(thread::spawn(move || {
            client::rpc(&state, "agent_stop", json!({"alias": "w1"}))
        }));
    }
    // Overlapping stops: exactly one owns the reservation and mutates;
    // the loser is rejected before touching any state.
    let results: Vec<_> = racers.into_iter().map(|r| r.join().unwrap()).collect();
    let winners = results
        .iter()
        .filter(|r| matches!(r, Ok(v) if v["state"] == "stopped"))
        .count();
    let losers = results
        .iter()
        .filter(|r| matches!(r, Err(e) if e.to_string().contains("already stopping")))
        .count();
    assert_eq!((winners, losers), (1, 1), "{results:?}");
    d.wait_agent("w1", "stopped", 10);
    assert_eq!(d.message_state("w1", "m1"), "interrupted");
    // Ownership was fully released: a resume works on the first try.
    d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "idle", 10);
}

#[test]
fn stop_during_initialization_is_bounded() {
    let d = TestDaemon::start();
    let mock = d.mock_codex("slow-init");
    d.register_codex("w1");
    // Wait until the provider process exists: the adapter is then
    // published and the initialize RPC (30s) is in flight.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !mock.pidfile.exists() {
        assert!(Instant::now() < deadline, "provider never launched");
        thread::sleep(Duration::from_millis(50));
    }
    let began = Instant::now();
    let stopped = d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(20),
        "stop during initialization was not bounded: {:?}",
        began.elapsed()
    );
    // Init was force-closed mid-flight: outcome uncertain -> attention,
    // and no provider process is left behind.
    assert_eq!(stopped["state"], "attention");
    wait_pid_gone(&mock.pidfile, 10);
}

// ---- managed-ws: WebSocket app-server endpoint ----

#[test]
fn ws_turn_roundtrip_exposes_endpoint() {
    let d = TestDaemon::start();
    let mock = d.mock_codex_ws("ok");
    d.register_codex_ws("w1");
    let agent = d.wait_agent("w1", "idle", 15);
    // The endpoint is discoverable for `agent attach`; loopback only.
    let endpoint = agent["endpoint"].as_str().unwrap();
    assert!(endpoint.starts_with("ws://127.0.0.1:"), "{endpoint}");
    assert_eq!(agent["thread_id"], "th-1");
    let reply = d
        .rpc(
            "agent_ask",
            json!({"alias": "w1", "text": "hello", "message": "m1", "wait": 30}),
        )
        .unwrap();
    assert_eq!(reply["state"], "completed");
    assert!(reply["result"].to_string().contains("MOCK_OK"), "{reply}");
    // The provider process stays alive while the agent is up.
    assert!(pid_alive(&mock.pidfile));
}

#[test]
fn ws_disconnect_fences_turn_unknown() {
    let d = TestDaemon::start();
    let mock = d.mock_codex_ws("ok");
    d.register_codex_ws("w1");
    d.wait_agent("w1", "idle", 15);
    let began = Instant::now();
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "DIE now", "message": "m1"}),
    )
    .unwrap();
    // Server closed the socket mid-turn: EOF must reach the wait fast.
    d.wait_message("w1", "m1", &["unknown"], 20);
    assert!(began.elapsed() < Duration::from_secs(20));
    d.wait_agent("w1", "attention", 10);
    wait_pid_gone(&mock.pidfile, 10);
}

#[test]
fn ws_stop_is_bounded_when_silent() {
    let d = TestDaemon::start();
    let mock = d.mock_codex_ws("silent");
    d.register_codex_ws("w1");
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "hi", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("w1", "m1", &["running"], 15);
    let began = Instant::now();
    let stopped = d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(20),
        "ws stop was not bounded"
    );
    assert_eq!(stopped["state"], "attention");
    d.wait_message("w1", "m1", &["unknown"], 10);
    wait_pid_gone(&mock.pidfile, 10);
}

#[test]
fn ws_approval_is_brokered() {
    let d = TestDaemon::start();
    let _mock = d.mock_codex_ws("ok");
    d.register_codex_ws("w1");
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "NEED_INPUT:x", "message": "m1"}),
    )
    .unwrap();
    d.wait_agent("w1", "waiting_input", 15);
    let requests = d.rpc("agent_requests", json!({"alias": "w1"})).unwrap();
    let handle = requests["requests"][0]["request"].as_str().unwrap();
    let answered = d
        .rpc(
            "agent_respond",
            json!({"alias": "w1", "request": handle, "decision": "accept"}),
        )
        .unwrap();
    assert_eq!(answered["state"], "answered");
    d.wait_message("w1", "m1", &["completed"], 20);
}

#[test]
fn ws_stop_during_init_is_bounded() {
    let d = TestDaemon::start();
    let mock = d.mock_codex_ws("slow-init");
    d.register_codex_ws("w1");
    // Wait until the provider accepted the WebSocket: the adapter is
    // published and the 30s initialize RPC is in flight.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !mock.pidfile.exists() {
        assert!(Instant::now() < deadline, "provider never launched");
        thread::sleep(Duration::from_millis(50));
    }
    thread::sleep(Duration::from_millis(500)); // let connect+handshake land
    let began = Instant::now();
    let stopped = d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(20),
        "ws init stop was not bounded: {:?}",
        began.elapsed()
    );
    assert_eq!(stopped["state"], "attention");
    wait_pid_gone(&mock.pidfile, 10);
}

#[test]
fn ws_restart_resumes_thread_with_fresh_endpoint() {
    // State dir outlives both daemon instances (d.state dies with d);
    // the agent cwd must too, so both point at `seeded`.
    let seeded = TempDir::new().unwrap();
    let state = seeded.path().to_path_buf();
    let cwd = seeded.path().to_str().unwrap().to_string();
    let mock;
    let first_endpoint;
    {
        let d = TestDaemon::start_on(state.clone());
        mock = d.mock_codex_ws_at(seeded.path(), "ok");
        d.rpc(
            "agent_register",
            json!({"alias": "w1", "provider": "codex",
                   "endpoint_kind": "managed-ws", "cwd": cwd}),
        )
        .unwrap();
        let first = d.wait_agent("w1", "idle", 15);
        first_endpoint = first["endpoint"].as_str().unwrap().to_string();
    }
    // Restart relaunches the enabled actor: a fresh app-server process,
    // a fresh loopback port, and thread/resume on the saved thread.
    let d = TestDaemon::start_on(state);
    let resumed = d.wait_agent("w1", "idle", 15);
    assert_eq!(resumed["thread_id"], "th-1");
    let new_endpoint = resumed["endpoint"].as_str().unwrap();
    assert!(new_endpoint.starts_with("ws://127.0.0.1:"));
    assert_ne!(new_endpoint, first_endpoint, "endpoint was not refreshed");
    // The resumed adapter still answers turns on the same thread.
    let reply = d
        .rpc(
            "agent_ask",
            json!({"alias": "w1", "text": "hi", "message": "m2", "wait": 30}),
        )
        .unwrap();
    assert_eq!(reply["state"], "completed");
    assert!(pid_alive(&mock.pidfile));
}

#[test]
fn ws_handshake_stall_is_bounded_and_cleans_child() {
    // The reviewer's probe: TCP accepts but never upgrades. The bounded
    // handshake must fail startup and kill the owned child — before the
    // fix, connect blocked past the deadline and the child leaked.
    let d = TestDaemon::start();
    let mock = d.mock_codex_ws("no-upgrade");
    d.register_codex_ws("w1");
    let began = Instant::now();
    let agent = d.wait_agent("w1", "attention", 25);
    assert!(
        began.elapsed() < Duration::from_secs(25),
        "handshake stall was not bounded"
    );
    let error = agent["error"].as_str().unwrap_or_default().to_string();
    assert!(
        error.contains("handshake") || error.contains("app-server"),
        "unexpected error: {error}"
    );
    assert!(agent["endpoint"].is_null());
    wait_pid_gone(&mock.pidfile, 10);
}

#[test]
fn ws_stop_during_handshake_is_bounded() {
    // Close must reach a child still stuck in connect/handshake: the
    // child is published before connecting so stop kills it, and the
    // connect loop observes the removal instead of hanging.
    let d = TestDaemon::start();
    let mock = d.mock_codex_ws("no-upgrade");
    d.register_codex_ws("w1");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !mock.pidfile.exists() {
        assert!(Instant::now() < deadline, "provider never launched");
        thread::sleep(Duration::from_millis(50));
    }
    let began = Instant::now();
    let stopped = d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(20),
        "stop during handshake was not bounded"
    );
    assert!(
        matches!(
            stopped["state"].as_str(),
            Some("attention") | Some("stopped")
        ),
        "{stopped}"
    );
    wait_pid_gone(&mock.pidfile, 10);
}

#[test]
fn ws_close_frame_disconnect_fences_unknown() {
    // A WS close frame (not just TCP EOF) must sever the transport:
    // the reader replies close, marks disconnected, and the in-flight
    // turn resolves unknown without waiting out its deadline.
    let d = TestDaemon::start();
    let mock = d.mock_codex_ws("ok");
    d.register_codex_ws("w1");
    d.wait_agent("w1", "idle", 15);
    let began = Instant::now();
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "DIE2 now", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("w1", "m1", &["unknown"], 20);
    assert!(began.elapsed() < Duration::from_secs(20));
    d.wait_agent("w1", "attention", 10);
    wait_pid_gone(&mock.pidfile, 10);
}

#[test]
fn ws_control_frames_share_the_write_path() {
    // The server pings mid-request; the client's pong must be written
    // on the same serialized path as data frames. The mock records
    // whether a pong arrived before it answered the turn.
    let d = TestDaemon::start();
    let mock = d.mock_codex_ws("ping-first");
    d.register_codex_ws("w1");
    d.wait_agent("w1", "idle", 15);
    let reply = d
        .rpc(
            "agent_ask",
            json!({"alias": "w1", "text": "hello", "message": "m1", "wait": 30}),
        )
        .unwrap();
    assert_eq!(reply["state"], "completed");
    let pong = std::fs::read_to_string(format!("{}.pong", mock.pidfile.display()))
        .unwrap_or_else(|_| "missing".into());
    assert_eq!(pong, "yes", "server never received a pong");
}

#[test]
fn ws_external_approval_resolution_drops_pending() {
    // An attached TUI answered the approval: `serverRequest/resolved`
    // must drop the pending handle so a late Cadence respond is
    // rejected rather than double-answering, and the turn completes
    // without a Cadence response.
    let d = TestDaemon::start();
    let mock = d.mock_codex_ws("ok");
    d.register_codex_ws("w1");
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "NEED_INPUT_EXT:x", "message": "m1"}),
    )
    .unwrap();
    d.wait_agent("w1", "waiting_input", 15);
    let requests = d.rpc("agent_requests", json!({"alias": "w1"})).unwrap();
    let handle = requests["requests"][0]["request"]
        .as_str()
        .unwrap()
        .to_string();
    // Resolve it externally, as an attached TUI would.
    std::fs::write(format!("{}.resolve", mock.pidfile.display()), b"1").unwrap();
    d.wait_message("w1", "m1", &["completed"], 20);
    // The stale handle is rejected; the provider already resolved it.
    let late = d.rpc(
        "agent_respond",
        json!({"alias": "w1", "request": handle, "decision": "accept"}),
    );
    let late = late.expect_err("late respond should be rejected");
    assert!(
        late.to_string().contains("no longer pending"),
        "late respond should be rejected: {late}"
    );
    let requests = d.rpc("agent_requests", json!({"alias": "w1"})).unwrap();
    assert_eq!(requests["requests"].as_array().unwrap().len(), 0);
    d.wait_agent("w1", "idle", 10);
    // The rejected late respond must not regress the finished turn to
    // busy — the conditional transition only relaxes waiting_input.
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(agent["agent"]["state"], "idle", "{agent}");
}

#[test]
fn ws_drip_handshake_is_bounded() {
    // The reviewer's drip probe: a peer feeding one header byte/second
    // defeats per-read timeouts; only the absolute deadline bounds it.
    let d = TestDaemon::start();
    let mock = d.mock_codex_ws("drip");
    d.register_codex_ws("w1");
    let began = Instant::now();
    d.wait_agent("w1", "attention", 25);
    assert!(
        began.elapsed() < Duration::from_secs(25),
        "drip handshake was not wall-clock bounded"
    );
    wait_pid_gone(&mock.pidfile, 10);
}

#[test]
fn ws_refused_upgrade_is_bounded() {
    // A 200-instead-of-101 response must fail startup, not hang.
    let d = TestDaemon::start();
    let mock = d.mock_codex_ws("bad-upgrade");
    d.register_codex_ws("w1");
    let began = Instant::now();
    d.wait_agent("w1", "attention", 25);
    assert!(began.elapsed() < Duration::from_secs(25));
    wait_pid_gone(&mock.pidfile, 10);
}

#[test]
fn ws_fragmented_message_with_interleaved_ping() {
    // turn/completed arrives as two continuations around a ping: the
    // vetted codec must reassemble it and answer the control frame.
    let d = TestDaemon::start();
    let _mock = d.mock_codex_ws("ok");
    d.register_codex_ws("w1");
    d.wait_agent("w1", "idle", 15);
    let reply = d
        .rpc(
            "agent_ask",
            json!({"alias": "w1", "text": "FRAG me", "message": "m1", "wait": 30}),
        )
        .unwrap();
    assert_eq!(reply["state"], "completed");
    assert!(reply["result"].to_string().contains("MOCK_OK"), "{reply}");
}

#[test]
fn ws_concurrent_respond_has_single_winner() {
    // Two racing responds on one handle: the atomic claim gives exactly
    // one winner; the loser is rejected before any provider write.
    let d = TestDaemon::start();
    let _mock = d.mock_codex_ws("ok");
    d.register_codex_ws("w1");
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "NEED_INPUT:x", "message": "m1"}),
    )
    .unwrap();
    d.wait_agent("w1", "waiting_input", 15);
    let requests = d.rpc("agent_requests", json!({"alias": "w1"})).unwrap();
    let handle = requests["requests"][0]["request"]
        .as_str()
        .unwrap()
        .to_string();
    let mut results = Vec::new();
    thread::scope(|scope| {
        let mut racers = Vec::new();
        for _ in 0..2 {
            let d = &d;
            let handle = handle.clone();
            racers.push(scope.spawn(move || {
                d.rpc(
                    "agent_respond",
                    json!({"alias": "w1", "request": handle, "decision": "accept"}),
                )
            }));
        }
        for racer in racers {
            results.push(racer.join().unwrap());
        }
    });
    let winners = results
        .iter()
        .filter(|r| matches!(r, Ok(v) if v["state"] == "answered"))
        .count();
    let losers = results
        .iter()
        .filter(|r| matches!(r, Err(e) if e.to_string().contains("no longer pending")))
        .count();
    assert_eq!((winners, losers), (1, 1), "{results:?}");
    d.wait_message("w1", "m1", &["completed"], 20);
}

#[test]
fn ws_second_pending_request_keeps_waiting() {
    // Two outstanding approvals: answering the first must NOT relax
    // waiting_input while the second remains — the relaxation is
    // coordinated with the pending set, not a check-then-write.
    let d = TestDaemon::start();
    let _mock = d.mock_codex_ws("ok");
    d.register_codex_ws("w1");
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "NEED_INPUT2:x", "message": "m1"}),
    )
    .unwrap();
    d.wait_agent("w1", "waiting_input", 15);
    let requests = d.rpc("agent_requests", json!({"alias": "w1"})).unwrap();
    let handles: Vec<String> = requests["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["request"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(handles.len(), 2, "{requests}");
    let answered = d
        .rpc(
            "agent_respond",
            json!({"alias": "w1", "request": handles[0], "decision": "accept"}),
        )
        .unwrap();
    assert_eq!(answered["state"], "answered");
    // One request still pending: the agent must stay waiting_input.
    thread::sleep(Duration::from_millis(300));
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(
        agent["agent"]["state"], "waiting_input",
        "relaxation clobbered the remaining request"
    );
    let answered = d
        .rpc(
            "agent_respond",
            json!({"alias": "w1", "request": handles[1], "decision": "accept"}),
        )
        .unwrap();
    assert_eq!(answered["state"], "answered");
    d.wait_message("w1", "m1", &["completed"], 20);
    d.wait_agent("w1", "idle", 10);
}

// ---- mock Devin TUI over a mock tmux (no model calls) ----

/// Fake `tmux` speaking just enough of the CLI for the pty adapter.
/// `tmux -L <sock> <cmd> <args>`; per-socket state lives under
/// `<mockdir>/tmux-state/<sock>/`. `new-session` really spawns the pane
/// command (`bash -c`) in its own process group so pane_pid and the
/// /proc lock-descendant checks exercise real ownership logic.
const MOCK_TMUX_PY: &str = r##"#!/usr/bin/env python3
import os, signal, subprocess, sys

args = sys.argv[1:]
if args[0] == "-L":
    sock = args[1]; args = args[2:]
state = os.path.join(os.environ["MOCK_TMUX_STATE"], sock)
os.makedirs(state, exist_ok=True)

def sess_path(name, ext):
    return os.path.join(state, name + "." + ext)

def sess_pid(name):
    try:
        pid = int(open(sess_path(name, "pid")).read().strip())
        os.kill(pid, 0)
        return pid
    except Exception:
        return None

def die(msg, code=1):
    sys.stderr.write(msg + "\n"); sys.exit(code)

cmd, rest = args[0], args[1:]
if cmd == "new-session":
    name = rest[rest.index("-s") + 1]
    cwd = rest[rest.index("-c") + 1] if "-c" in rest else os.getcwd()
    pane_cmd = rest[-1]
    pane = os.path.join(state, name)
    env = dict(os.environ, FAKE_PANE=pane)
    # Detach the pane's stdio to a file — the adapter's Command::output
    # would otherwise wait on pipes the long-lived pane inherited.
    log = open(sess_path(name, "log"), "ab")
    proc = subprocess.Popen(["bash", "-c", pane_cmd], cwd=cwd, env=env,
                            stdin=subprocess.DEVNULL, stdout=log,
                            stderr=log, start_new_session=True)
    open(sess_path(name, "pid"), "w").write(str(proc.pid))
    open(sess_path(name, "screen"), "a").close()
    sys.exit(0)
if cmd == "has-session":
    name = rest[rest.index("-t") + 1]
    sys.exit(0 if sess_pid(name) else 1)
if cmd == "display-message":
    name = rest[rest.index("-t") + 1]
    fmt = rest[-1]
    pid = sess_pid(name)
    if fmt == "#{pane_pid}":
        # A dead pane keeps its pid (tmux keeps dead panes); emulate.
        try: print(int(open(sess_path(name, "pid")).read().strip()))
        except Exception: die("no such session")
    elif fmt == "#{pane_dead}":
        print("0" if pid else "1")
    elif fmt == "#{pane_in_mode}":
        try: print(open(sess_path(name, "mode")).read().strip() or "0")
        except FileNotFoundError: print("0")
    else: die("unknown format " + fmt)
    sys.exit(0)
if cmd == "capture-pane":
    name = rest[rest.index("-t") + 1]
    out = ""
    try: out += open(sess_path(name, "screen")).read()
    except FileNotFoundError: die("no such session")
    try: out += open(sess_path(name, "input")).read()
    except FileNotFoundError: pass
    sys.stdout.write(out); sys.exit(0)
if cmd == "load-buffer":
    open(os.path.join(state, "buffer"), "w").write(open(rest[-1]).read())
    sys.exit(0)
if cmd == "paste-buffer":
    name = rest[rest.index("-t") + 1]
    with open(sess_path(name, "input"), "a") as f:
        f.write(open(os.path.join(state, "buffer")).read())
    sys.exit(0)
if cmd == "send-keys":
    name = rest[rest.index("-t") + 1]
    key = rest[-1]
    with open(sess_path(name, "input"), "a") as f:
        f.write("<ENTER>" if key == "Enter" else "<KEY:" + key + ">")
    sys.exit(0)
if cmd == "kill-session":
    name = rest[rest.index("-t") + 1]
    pid = sess_pid(name)
    if pid:
        try: os.killpg(pid, signal.SIGKILL)
        except ProcessLookupError: pass
    sys.exit(0)
die("unhandled tmux cmd " + cmd)
"##;

/// Fake `devin` TUI: takes the real session lock (`flock`, visible via
/// /proc/fd to the adapter's ownership scan), mirrors the pane input
/// file, and answers an `<ENTER>`-terminated paste by writing the
/// submitted line and a `MOCK_REPLY` to the screen file.
/// `$FAKE_PANE` (set by the mock tmux) points at the session state.
const MOCK_DEVIN_PY: &str = r#"
import fcntl, os, sys, time

locks = sys.argv[1]
sid = sys.argv[sys.argv.index("-r") + 1] if "-r" in sys.argv else \
    "mock-session-%d" % os.getpid()
os.makedirs(locks, exist_ok=True)
lf = open(os.path.join(locks, sid + ".lock"), "a")
try:
    fcntl.flock(lf, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    print("session_locked: %s" % sid); sys.exit(1)
open(os.environ["FAKE_PANE"] + ".sid", "w").write(sid)
with open(os.environ["FAKE_PANE"] + ".screen", "a") as f:
    f.write("Mock Devin TUI [%s]\n" % sid)
while True:
    inp = os.environ["FAKE_PANE"] + ".input"
    try:
        data = open(inp).read()
    except FileNotFoundError:
        data = ""
    if "<ENTER>" in data:
        text, rest = data.split("<ENTER>", 1)
        open(inp, "w").write(rest)
        if text.strip():
            with open(os.environ["FAKE_PANE"] + ".screen", "a") as f:
                f.write("> %s\nMOCK_REPLY: %s\n" % (text.strip(), text.strip()))
    if "<KEY:C-c>" in data:
        open(inp, "w").write("")
        with open(os.environ["FAKE_PANE"] + ".screen", "a") as f:
            f.write("^C interrupt\n")
    time.sleep(0.05)
"#;

struct MockDevin {
    _guard: std::sync::MutexGuard<'static, ()>,
    dir: PathBuf,
    locks: PathBuf,
}

/// Install the mock tmux/devin pair. Set the env overrides BEFORE a
/// daemon starts so its auto-relaunch sees them.
fn install_mock_devin(dir: &Path) -> MockDevin {
    let guard = ENV_LOCK.lock().unwrap();
    let locks = dir.join("devin-locks");
    let tmux_state = dir.join("tmux-state");
    std::fs::create_dir_all(&locks).unwrap();
    std::fs::create_dir_all(&tmux_state).unwrap();
    let tmux = dir.join("tmux");
    let devin_py = dir.join("mock-devin.py");
    std::fs::write(&tmux, MOCK_TMUX_PY).unwrap();
    std::fs::write(&devin_py, MOCK_DEVIN_PY).unwrap();
    // The adapter execs the tmux binary directly (no shell), so the
    // mock must be executable.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("MOCK_TMUX_STATE", &tmux_state);
    std::env::set_var("CADENCE_TMUX_COMMAND", &tmux);
    std::env::set_var(
        "CADENCE_DEVIN_COMMAND",
        format!("python3 {} {}", devin_py.display(), locks.display()),
    );
    std::env::set_var("CADENCE_DEVIN_LOCKS", &locks);
    MockDevin {
        _guard: guard,
        dir: dir.to_path_buf(),
        locks,
    }
}

impl TestDaemon {
    /// Install the mock tmux/devin pair for `dir` (which must outlive
    /// every daemon that will launch panes) and return their paths.
    fn mock_devin_at(&self, dir: &Path) -> MockDevin {
        install_mock_devin(dir)
    }

    fn mock_devin(&self) -> MockDevin {
        self.mock_devin_at(self.dir.path())
    }

    fn register_devin(&self, alias: &str, session: Option<&str>) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        let params = session.map(|s| json!({"session": s}).to_string());
        self.rpc(
            "agent_register",
            json!({"alias": alias, "provider": "devin",
                   "endpoint_kind": "pty", "cwd": cwd, "params": params}),
        )
        .unwrap();
    }

    /// The tmux-side state dir for a given agent session name.
    fn pane_file(&self, mock: &MockDevin, alias: &str, ext: &str) -> PathBuf {
        // The adapter derives its socket name from the state dir.
        mock.dir
            .join("tmux-state")
            .join(socket_for(&self.state))
            .join(format!("{alias}.{ext}"))
    }
}

impl Drop for MockDevin {
    fn drop(&mut self) {
        // Panes legitimately outlive a daemon (shutdown detaches), so
        // clean any survivors ourselves by their recorded pane pids.
        if let Ok(socks) = std::fs::read_dir(self.dir.join("tmux-state")) {
            for sock in socks.flatten() {
                if let Ok(files) = std::fs::read_dir(sock.path()) {
                    for f in files.flatten() {
                        if f.file_name().to_string_lossy().ends_with(".pid") {
                            if let Ok(pid) = std::fs::read_to_string(f.path())
                                .unwrap_or_default()
                                .trim()
                                .parse::<i32>()
                            {
                                unsafe { libc::killpg(pid, libc::SIGKILL) };
                            }
                        }
                    }
                }
            }
        }
        std::env::remove_var("CADENCE_TMUX_COMMAND");
        std::env::remove_var("CADENCE_DEVIN_COMMAND");
        std::env::remove_var("CADENCE_DEVIN_LOCKS");
        std::env::remove_var("MOCK_TMUX_STATE");
    }
}

/// Mirror of the adapter's `cadence-<fnv64(state_dir)>` socket name.
fn socket_for(state_dir: &Path) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in state_dir.to_string_lossy().as_bytes() {
        h = (h ^ u64::from(*b)).wrapping_mul(0x100000001b3);
    }
    format!("cadence-{h:016x}")
}

fn pty_token(d: &TestDaemon, alias: &str, id: &str) -> String {
    let m = d.wait_message(alias, id, &["running"], 20);
    m["turn_id"].as_str().unwrap().to_string()
}

#[test]
fn pty_send_pastes_literal_and_completes_via_report() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv1", None);
    let agent = d.wait_agent("dv1", "idle", 20);
    assert_eq!(agent["endpoint_kind"], "pty");
    assert!(
        agent["endpoint"]
            .as_str()
            .unwrap()
            .starts_with("tmux://cadence-"),
        "{}",
        agent
    );
    assert!(
        agent["thread_id"]
            .as_str()
            .unwrap()
            .starts_with("mock-session-"),
        "native session discovered from the lock: {}",
        agent
    );
    let gen = agent["generation"].as_str().unwrap().to_string();
    assert!(!gen.is_empty());

    // A queued message without a readiness claim must not be pasted.
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "first task", "message": "m1"}),
    )
    .unwrap();
    thread::sleep(Duration::from_millis(700));
    assert_eq!(d.message_state("dv1", "m1"), "queued");

    // Operator claim: the head of the FIFO queue (m1) is pasted.
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    let token1 = pty_token(&d, "dv1", "m1");
    assert!(token1.starts_with(&format!("pty-{gen}-")), "{token1}");

    // Literal text with shell metacharacters is pasted verbatim into
    // the pane input — one paste per claim, so m2 needs a new one.
    let tricky = "quote ' $HOME `id` ; rm -rf / & | <tag> \"double\"";
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": tricky, "message": "m2"}),
    )
    .unwrap();
    thread::sleep(Duration::from_millis(400));
    assert_eq!(d.message_state("dv1", "m2"), "queued");
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    let token = pty_token(&d, "dv1", "m2");
    assert!(token.starts_with(&format!("pty-{gen}-")), "{token}");

    // The mock TUI consumed the paste + Enter and replied on screen;
    // capture shows the verbatim submitted line and the separate reply.
    let deadline = Instant::now() + Duration::from_secs(10);
    let cap = loop {
        let out = d.rpc("agent_capture", json!({"alias": "dv1"})).unwrap()["capture"]
            .as_str()
            .unwrap()
            .to_string();
        if out.contains(&format!("MOCK_REPLY: {tricky}")) {
            break out;
        }
        assert!(Instant::now() < deadline, "no reply on screen: {out}");
        thread::sleep(Duration::from_millis(100));
    };
    assert!(cap.contains(&format!("> {tricky}")));

    // Still `running` — the screen reply does not finish the message;
    // only an explicit report does.
    assert_eq!(d.message_state("dv1", "m2"), "running");

    // Wrong token rejected; correct token completes and preserves text.
    let bad = d.rpc(
        "message_report",
        json!({"message": "m2", "token": "pty-wrong", "kind": "result",
               "text": "nope"}),
    );
    assert!(bad.is_err());
    d.rpc(
        "message_report",
        json!({"message": "m2", "token": token, "kind": "result",
               "text": "done: MOCK_REPLY observed"}),
    )
    .unwrap();
    let m = d.wait_message("dv1", "m2", &["completed"], 10);
    assert_eq!(m["result"]["via"], "pty_report");
    d.wait_agent("dv1", "idle", 10);
}

#[test]
fn pty_claim_is_single_use_and_expires() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "one", "message": "m1"}),
    )
    .unwrap();
    pty_token(&d, "dv1", "m1");
    // The claim was consumed: a second send queues, it does not paste.
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "two", "message": "m2"}),
    )
    .unwrap();
    thread::sleep(Duration::from_millis(700));
    assert_eq!(d.message_state("dv1", "m2"), "queued");
    let input = std::fs::read_to_string(d.pane_file(&_mock, "dv1", "input")).unwrap_or_default();
    assert!(!input.contains("two"), "second send pasted without a claim");
}

#[test]
fn pty_ack_then_result_and_duplicate_rules() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "work", "message": "m1"}),
    )
    .unwrap();
    let token = pty_token(&d, "dv1", "m1");

    d.rpc(
        "message_report",
        json!({"message": "m1", "token": token, "kind": "ack",
               "text": "seen"}),
    )
    .unwrap();
    assert_eq!(d.message_state("dv1", "m1"), "running");
    let m = d.rpc("agent_show", json!({"alias": "dv1"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "m1")
        .unwrap()
        .clone();
    assert_eq!(m["result"]["ack"]["text"], "seen");

    d.rpc(
        "message_report",
        json!({"message": "m1", "token": token, "kind": "result",
               "text": "final"}),
    )
    .unwrap();
    d.wait_message("dv1", "m1", &["completed"], 10);
    // Idempotent retry of the same result is fine...
    let dup = d.rpc(
        "message_report",
        json!({"message": "m1", "token": token, "kind": "result",
               "text": "final"}),
    );
    assert!(dup.is_ok(), "{dup:?}");
    // ...but a conflicting result for a finished message is rejected.
    assert!(d
        .rpc(
            "message_report",
            json!({"message": "m1", "token": token, "kind": "result",
                   "text": "DIFFERENT"}),
        )
        .is_err());
    // And ack after completion is rejected too.
    assert!(d
        .rpc(
            "message_report",
            json!({"message": "m1", "token": token, "kind": "ack"}),
        )
        .is_err());
}

#[test]
fn pty_stale_generation_report_rejected() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    let old_token = pty_token(&d, "dv1", "m1");

    // New endpoint life: stop + resume mints a fresh generation even
    // though the relaunched pane owns the same native session.
    d.rpc("agent_stop", json!({"alias": "dv1"})).unwrap();
    d.wait_agent("dv1", "stopped", 15);
    // m1 is still `running` (submitted before the stop); the report
    // carrying the old-generation token must be rejected.
    d.rpc("agent_resume", json!({"alias": "dv1"})).unwrap();
    let agent = d.wait_agent("dv1", "idle", 20);
    assert!(!agent["generation"].as_str().unwrap().is_empty());
    let stale = d.rpc(
        "message_report",
        json!({"message": "m1", "token": old_token, "kind": "result",
               "text": "late"}),
    );
    assert!(
        stale.is_err(),
        "stale-generation report accepted: {stale:?}"
    );
}

#[test]
fn pty_locked_session_refuses_takeover() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    // A foreign process holds the session lock — simulating another TUI.
    let lock = mock.locks.join("held-session.lock");
    let mut holder = std::process::Command::new("python3")
        .args([
            "-c",
            "import fcntl,sys,time; f=open(sys.argv[1],'a'); \
             fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB); time.sleep(30)",
            lock.to_str().unwrap(),
        ])
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(300));
    d.register_devin("dv1", Some("held-session"));
    let agent = d.wait_agent("dv1", "attention", 20);
    assert!(
        agent["error"]
            .as_str()
            .unwrap_or("")
            .contains("locked by another terminal"),
        "{}",
        agent
    );
    holder.kill().unwrap();
    let _ = holder.wait();
}

#[test]
fn pty_dead_pane_fences_submitted_and_stops_actor() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    pty_token(&d, "dv1", "m1");

    // Kill the pane's whole process group: the submitted message can no
    // longer be confirmed — it must go `unknown`, never silently replay.
    let pid: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    unsafe { libc::killpg(pid, libc::SIGKILL) };
    let agent = d.wait_agent("dv1", "attention", 20);
    assert!(
        agent["error"]
            .as_str()
            .unwrap_or("")
            .contains("disconnected")
            || agent["error"].as_str().unwrap_or("").contains("lock"),
        "{}",
        agent
    );
    d.wait_message("dv1", "m1", &["unknown"], 15);
}

#[test]
fn pty_restart_reattaches_same_native_session() {
    let dir = TempDir::new().unwrap();
    let seeded = dir.path().join("state");
    std::fs::create_dir_all(&seeded).unwrap();
    let fixtures = TempDir::new().unwrap();
    {
        let _mock = install_mock_devin(fixtures.path());
        let d = TestDaemon::start_on(seeded.clone());
        d.register_devin("dv1", None);
        let agent = d.wait_agent("dv1", "idle", 20);
        let native = agent["thread_id"].as_str().unwrap().to_string();
        let pane_pid = agent["pid"].as_i64().unwrap();
        // Daemon restart: the mock tmux server (fixture dir) outlives it,
        // so the pane is still alive and must be reattached, not relaunched.
        drop(d);
        let d2 = TestDaemon::start_on(seeded.clone());
        let agent2 = d2.wait_agent("dv1", "idle", 25);
        assert_eq!(agent2["thread_id"].as_str().unwrap(), native);
        assert_eq!(agent2["pid"].as_i64().unwrap(), pane_pid);
    }
}

#[test]
fn pty_stop_kills_only_owned_session() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    let pidfile = d.pane_file(&mock, "dv1", "pid");
    d.rpc("agent_stop", json!({"alias": "dv1"})).unwrap();
    d.wait_agent("dv1", "stopped", 15);
    wait_pid_gone(&pidfile, 10);
}

#[test]
fn pty_respond_rejected_and_mode_blocks_send() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    // No approval channel exists for pty.
    assert!(d
        .rpc(
            "agent_respond",
            json!({"alias": "dv1", "request": "r1", "decision": "accept"}),
        )
        .is_err());
    // pane_in_mode != 0 (copy mode etc.) keeps the message queued even
    // with a fresh claim.
    std::fs::write(d.pane_file(&mock, "dv1", "mode"), "1").unwrap();
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "x", "message": "m1"}),
    )
    .unwrap();
    thread::sleep(Duration::from_millis(700));
    assert_eq!(d.message_state("dv1", "m1"), "queued");
    std::fs::remove_file(d.pane_file(&mock, "dv1", "mode")).unwrap();
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    pty_token(&d, "dv1", "m1");
}

#[test]
fn pty_routed_result_body_is_single_line() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    // The PM is a pty agent; the worker is a fake provider reporting up.
    d.register_devin("pm", None);
    d.register("w1");
    d.wait_agent("pm", "idle", 20);
    d.wait_agent("w1", "idle", 10);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "work", "message": "j1",
               "reply_to": "pm"}),
    )
    .unwrap();
    d.wait_message("w1", "j1", &["completed"], 15);
    // The routed delivery is queued on pm with a single-line body —
    // no raw newlines or control characters.
    let show = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    let routed = &show["messages"].as_array().unwrap()[0];
    assert_eq!(routed["source"], "worker_result");
    let body = routed["body"].as_str().unwrap();
    assert!(
        !body.chars().any(|c| (c as u32) < 32 || c as u32 == 127),
        "routed body is not pty-safe: {body:?}"
    );
    assert!(body.contains("j1"));
    // Gated delivery, not a bypass: it waits queued for an operator
    // claim, then pastes like any send (running = validation passed).
    thread::sleep(Duration::from_millis(400));
    let mid = d.message_state("pm", routed["id"].as_str().unwrap());
    assert!(matches!(mid.as_str(), "queued" | "submitting"), "{mid}");
    d.rpc("agent_ready", json!({"alias": "pm"})).unwrap();
    pty_token(&d, "pm", routed["id"].as_str().unwrap());
}

#[test]
fn pty_send_rejects_control_chars() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "line1\nline2", "message": "m1"}),
    )
    .unwrap();
    // The newline is rejected by the adapter's literal-content rule —
    // the message fails without ever touching the pane.
    d.wait_message("dv1", "m1", &["failed"], 15);
    let input = std::fs::read_to_string(d.pane_file(&_mock, "dv1", "input")).unwrap_or_default();
    assert!(!input.contains("line1"));
    let failed = d.rpc("agent_show", json!({"alias": "dv1"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "m1")
        .unwrap()
        .clone();
    assert!(
        failed["result"]["error"]
            .as_str()
            .unwrap_or("")
            .contains("control characters"),
        "{failed}"
    );
    // A pre-write rejection must not fence the agent: it stays idle,
    // the pane survives, and the queue keeps draining.
    thread::sleep(Duration::from_millis(500));
    let agent = d.rpc("agent_show", json!({"alias": "dv1"})).unwrap()["agent"].clone();
    assert_eq!(agent["state"].as_str().unwrap(), "idle", "{agent}");
    assert!(agent["endpoint"].is_string(), "{agent}");
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "valid follow-up", "message": "m2"}),
    )
    .unwrap();
    let token = pty_token(&d, "dv1", "m2");
    d.rpc(
        "message_report",
        json!({"message": "m2", "token": token, "kind": "result",
               "text": "done"}),
    )
    .unwrap();
    d.wait_message("dv1", "m2", &["completed"], 10);
}
