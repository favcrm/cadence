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

    fn register_codex(&self, alias: &str) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        self.rpc(
            "agent_register",
            json!({"alias": alias, "provider": "codex",
                   "endpoint_kind": "managed", "cwd": cwd}),
        )
        .unwrap();
    }
}

impl Drop for MockCodex {
    fn drop(&mut self) {
        std::env::remove_var("CADENCE_CODEX_COMMAND");
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
