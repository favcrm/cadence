//! End-to-end tests: real socket daemon in-process, fake provider.
//! These exercise the observable contract — queue order, idempotency,
//! restart fencing, approval brokering, serialization — without model calls.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use cadence_agent::adapter::ProviderEnv;
use cadence_agent::client;
use cadence_agent::daemon;
use cadence_agent::memory::{
    self, FinalizationReceipt, Front, IdentityProof, Memory, ReviewReceipt, Scope,
};
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
        Self::start_opts(daemon_opts())
    }

    /// `start` with explicit daemon options — slot tests shrink the
    /// pools this way.
    fn start_opts(opts: daemon::ServeOptions) -> Self {
        suite_slot();
        let dir = TempDir::new().unwrap();
        let state = dir.path().to_path_buf();
        std::fs::create_dir_all(&state).unwrap();
        let owned = state.clone();
        let handle = thread::spawn(move || daemon::serve_with(&owned, opts));
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
        Self::start_on_opts(state, daemon_opts())
    }

    /// `start_on` with explicit daemon options — slot tests shrink the
    /// pools or inject the clock this way.
    fn start_on_opts(state: PathBuf, opts: daemon::ServeOptions) -> Self {
        suite_slot();
        let dir = TempDir::new().unwrap(); // keeps lifetime uniform
        let owned = state.clone();
        let handle = thread::spawn(move || daemon::serve_with(&owned, opts));
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

/// Fixture-only provider evidence. The automatic coordinator must read this
/// provider-owned column; a caller-supplied `params.quota` is intentionally
/// not sufficient to pass admission.
fn seed_provider_quota(d: &TestDaemon, alias: &str, observed_epoch: i64) {
    let agent = d.rpc("agent_show", json!({"alias": alias})).unwrap()["agent"].clone();
    let thread_id = agent["thread_id"]
        .as_str()
        .unwrap_or_else(|| panic!("fake agent {alias} has no thread identity"));
    let account_id = format!("acct-{alias}");
    let quota = json!({
        "provider": agent["provider"],
        "assignee": alias,
        "account_id": account_id,
        "thread_id": thread_id,
        "state": "available",
        "source": "account/rateLimits/read",
        "observed_at": cadence_agent::issue::time::iso(observed_epoch),
        "updated_at": cadence_agent::issue::time::iso(observed_epoch),
        "data": {
            "accountId": account_id,
            "rateLimits": {"primary": {"usedPercent": 1}}
        }
    });
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET quota=?1 WHERE alias=?2",
        rusqlite::params![quota.to_string(), alias],
    )
    .unwrap();
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
                team_role: None,
                model_policy: None,
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
    let error = agent["error"].as_str().unwrap();
    assert!(
        error.contains("Runtime restarted during provider turn"),
        "{error}"
    );
    assert!(error.contains("does not prove"), "{error}");
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
fn restart_keeps_original_unknown_reason_beside_later_inflight() {
    // An older unknown already names the provider account. A later
    // running turn is still in flight at crash. Restart must fence that
    // later turn without letting its blanket sentence replace the
    // original account on agent.error, and without rewriting the older
    // row or dropping the later turn token.
    const ORIGINAL: &str = "submission accepted but never rendered on the endpoint";
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
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        store.enqueue("w1", "first", None, "m1", "user").unwrap();
        let m1 = match store.take_queued("w1").unwrap() {
            Take::Message(message) => *message,
            _ => panic!("expected m1"),
        };
        store
            .finish(
                &m1,
                "unknown",
                &json!({"status": "unknown", "text": "", "error": ORIGINAL}),
                Some(ORIGINAL),
            )
            .unwrap();
        store.enqueue("w1", "second", None, "m2", "user").unwrap();
        let m2 = match store.take_queued("w1").unwrap() {
            Take::Message(message) => *message,
            _ => panic!("expected m2"),
        };
        store.mark_running(&m2.id, "pty-gen-m2").unwrap();
    }
    let d = TestDaemon::start_on(state);
    let agent = d.wait_agent("w1", "attention", 10);
    let error = agent["error"].as_str().unwrap();
    assert!(error.contains(ORIGINAL), "{error}");
    assert!(error.contains("does not prove"), "{error}");
    assert!(
        !error.contains("Runtime restarted during provider turn"),
        "{error}"
    );
    assert!(
        !error.contains("agent fenced at restart; turn never verified"),
        "{error}"
    );
    assert!(agent["endpoint"].is_null(), "{agent}");
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let messages = show["messages"].as_array().unwrap();
    let m1 = messages.iter().find(|m| m["id"] == "m1").unwrap();
    let m2 = messages.iter().find(|m| m["id"] == "m2").unwrap();
    assert_eq!(m1["state"], "unknown");
    assert_eq!(m1["error"], ORIGINAL);
    assert_eq!(m2["state"], "unknown");
    assert_eq!(m2["error"], "Runtime restarted during provider turn");
    assert_eq!(m2["turn_id"], "pty-gen-m2");
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "later", "message": "m3"}),
    )
    .unwrap();
    thread::sleep(Duration::from_secs(1));
    assert_eq!(d.message_state("w1", "m3"), "queued");
    assert_eq!(d.message_state("w1", "m1"), "unknown");
    assert_eq!(d.message_state("w1", "m2"), "unknown");
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
    // The fence carries the provider's own reason, not a generic label.
    assert!(
        m["error"].as_str().unwrap().contains("Connection lost"),
        "{m}"
    );
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

// ---- fence recovery: operator reconcile + agent unfence ----

/// The DISCONNECT keyword makes the fake provider drop mid-turn — the
/// message lands `unknown` and fences the agent.
fn fence_agent(d: &TestDaemon, alias: &str, id: &str) {
    d.rpc(
        "agent_send",
        json!({"alias": alias, "text": "DISCONNECT", "message": id}),
    )
    .unwrap();
    d.wait_message(alias, id, &["unknown"], 15);
    d.wait_agent(alias, "attention", 10);
}

fn event_kinds(d: &TestDaemon, alias: &str) -> Vec<String> {
    d.rpc("agent_events", json!({"alias": alias})).unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn reconcile_interrupted_clears_fence_and_preserves_history() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    fence_agent(&d, "w1", "x1");
    // Work queued behind the fence stays queued.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "after", "message": "x2"}),
    )
    .unwrap();
    // A bare resume is rejected, naming the reconcile-first path.
    let err = d.rpc("agent_resume", json!({"alias": "w1"})).unwrap_err();
    let err = err.to_string();
    assert!(err.contains("resume refused"), "{err}");
    assert!(err.contains("--no-resume"), "{err}");
    assert!(!err.contains("then `cadence agent resume"), "{err}");
    // The operator's verdict: interrupted, with a note and caller.
    let r = d
        .rpc(
            "message_reconcile",
            json!({"message": "x1", "status": "interrupted",
                   "note": "pane lost mid-turn", "by": "cookie-cesium"}),
        )
        .unwrap();
    assert_eq!(r["state"], "reconciled");
    assert_eq!(r["message"]["state"], "interrupted");
    assert_eq!(r["message"]["result"]["via"], "operator_reconcile");
    assert_eq!(r["message"]["result"]["note"], "pane lost mid-turn");
    // The last unknown reconciled → attention drops to stopped, and the
    // agent is NOT auto-started.
    let agent = d.wait_agent("w1", "stopped", 10);
    assert!(agent["endpoint"].is_null());
    // The reconciled event carries id, status, note and caller.
    let events = d.rpc("agent_events", json!({"alias": "w1"})).unwrap()["events"].clone();
    let rec = events
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "reconciled")
        .expect("reconciled event");
    assert_eq!(rec["payload"]["message"], "x1");
    assert_eq!(rec["payload"]["status"], "interrupted");
    assert_eq!(rec["payload"]["note"], "pane lost mid-turn");
    assert_eq!(rec["payload"]["by"], "cookie-cesium");
    // History is intact: x1 still listed (interrupted), x2 still queued.
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(show["unknown"], 0);
    assert_eq!(d.message_state("w1", "x1"), "interrupted");
    assert_eq!(d.message_state("w1", "x2"), "queued");
    // The normal resume path works again and drains the backlog.
    d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "idle", 15);
    d.wait_message("w1", "x2", &["completed"], 15);
}

#[test]
fn reconcile_completed_routes_result_interrupted_routes_notice() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register("w1");
    d.register("w2");
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    d.wait_agent("w2", "idle", 10);
    // Both fence on an unknown outcome; both were reply_to wired to pm.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "DISCONNECT", "message": "x1",
               "reply_to": "pm"}),
    )
    .unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "w2", "text": "DISCONNECT", "message": "x2",
               "reply_to": "pm"}),
    )
    .unwrap();
    d.wait_message("w1", "x1", &["unknown"], 15);
    d.wait_message("w2", "x2", &["unknown"], 15);
    // Each fence routed pm ONE informational notice — plainly not a
    // result, no reply_to (no loops), deterministic ids distinct from
    // the result slot a reconcile may still fill.
    let show = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    let pm_msgs = show["messages"].as_array().unwrap();
    let notices: Vec<&Value> = pm_msgs
        .iter()
        .filter(|m| m["source"] == "worker_notice")
        .collect();
    assert_eq!(notices.len(), 2, "one notice per fence: {show}");
    assert!(pm_msgs.iter().all(|m| m["source"] != "worker_result"));
    let x1_notice = notices
        .iter()
        .find(|m| m["body"].as_str().unwrap().contains("\"x1\""))
        .expect("x1 fence notice");
    let body = x1_notice["body"].as_str().unwrap();
    assert!(body.contains("unknown"), "{body}");
    assert!(body.contains("fenced"), "{body}");
    assert!(body.contains("reconcile"), "{body}");
    assert!(body.contains("not a result"), "{body}");
    assert!(x1_notice["reply_to"].is_null());
    // completed → the reconciled result routes to pm exactly once,
    // under the `cadence-result:` id the notice never touched.
    d.rpc(
        "message_reconcile",
        json!({"message": "x1", "status": "completed",
               "note": "pane showed the answer"}),
    )
    .unwrap();
    // interrupted → one more notice (the operator closed the turn),
    // still no result.
    d.rpc(
        "message_reconcile",
        json!({"message": "x2", "status": "interrupted"}),
    )
    .unwrap();
    let show = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    let pm_msgs = show["messages"].as_array().unwrap();
    let results: Vec<&Value> = pm_msgs
        .iter()
        .filter(|m| m["source"] == "worker_result")
        .collect();
    assert_eq!(results.len(), 1, "exactly one routed result: {show}");
    assert!(results[0]["body"].as_str().unwrap().contains("\"x1\""));
    assert!(results[0]["body"]
        .as_str()
        .unwrap()
        .contains("operator_reconcile"));
    let notices: Vec<&Value> = pm_msgs
        .iter()
        .filter(|m| m["source"] == "worker_notice")
        .collect();
    assert_eq!(
        notices.len(),
        3,
        "fence notices + interrupted notice: {show}"
    );
    let interrupted = notices
        .iter()
        .find(|m| {
            let b = m["body"].as_str().unwrap();
            b.contains("\"x2\"") && b.contains("interrupted")
        })
        .expect("x2 interrupted notice");
    assert!(
        interrupted["body"]
            .as_str()
            .unwrap()
            .contains("operator closed"),
        "{}",
        interrupted["body"]
    );
    // All four deliveries carry distinct deterministic ids — no
    // primary-key collision in any ordering.
    let ids: Vec<&str> = pm_msgs
        .iter()
        .filter(|m| m["source"] != "user")
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(ids.len(), 4);
    assert_eq!(unique.len(), 4, "colliding delivery ids: {ids:?}");
    // Event trail: w1 routed notice + result; w2 routed two notices.
    let w1_events = event_kinds(&d, "w1");
    assert!(w1_events.iter().any(|k| k == "notice_routed"));
    assert!(w1_events.iter().any(|k| k == "result_routed"));
    let w2_events = event_kinds(&d, "w2");
    assert_eq!(
        w2_events.iter().filter(|k| *k == "notice_routed").count(),
        2
    );
    assert!(!w2_events.iter().any(|k| k == "result_routed"));
}

#[test]
fn reconcile_rejects_non_unknown_and_repeats() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    // completed → rejected, naming the state.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "done", "message": "c1"}),
    )
    .unwrap();
    d.wait_message("w1", "c1", &["completed"], 15);
    let err = d
        .rpc(
            "message_reconcile",
            json!({"message": "c1", "status": "interrupted"}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("'completed'"), "{err}");
    // running → rejected, naming the state.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "NEED_INPUT:hold", "message": "r1"}),
    )
    .unwrap();
    d.wait_message("w1", "r1", &["running"], 15);
    let err = d
        .rpc(
            "message_reconcile",
            json!({"message": "r1", "status": "interrupted"}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("'running'"), "{err}");
    // queued → rejected, naming the state (w1 is busy holding r1).
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "next", "message": "q2"}),
    )
    .unwrap();
    assert_eq!(d.message_state("w1", "q2"), "queued");
    let err = d
        .rpc(
            "message_reconcile",
            json!({"message": "q2", "status": "interrupted"}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("'queued'"), "{err}");
    // A real fence on a second agent: reconcile once, then a second
    // reconcile rejects — the message is no longer unknown.
    d.register("w2");
    d.wait_agent("w2", "idle", 10);
    fence_agent(&d, "w2", "x9");
    d.rpc(
        "message_reconcile",
        json!({"message": "x9", "status": "failed"}),
    )
    .unwrap();
    let err = d
        .rpc(
            "message_reconcile",
            json!({"message": "x9", "status": "failed"}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("'failed'"), "{err}");
    // An invalid status is rejected before any state check.
    let err = d
        .rpc(
            "message_reconcile",
            json!({"message": "x9", "status": "bogus"}),
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("interrupted|completed|failed"),
        "{err}"
    );
}

#[test]
fn agent_unfence_reconciles_all_then_resume_works() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    fence_agent(&d, "w1", "x1");
    // An agent with no unknowns refuses cleanly.
    d.register("w2");
    d.wait_agent("w2", "idle", 10);
    let err = d
        .rpc(
            "agent_unfence",
            json!({"alias": "w2", "status": "interrupted"}),
        )
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("no unknown messages to reconcile — use `cadence agent resume w2`"),
        "{err}"
    );
    // Unfence reconciles each unknown and lands the agent stopped.
    let r = d
        .rpc(
            "agent_unfence",
            json!({"alias": "w1", "status": "interrupted",
                   "note": "bulk"}),
        )
        .unwrap();
    assert_eq!(r["reconciled"], json!(["x1"]));
    assert_eq!(r["state"], "stopped");
    assert_eq!(d.message_state("w1", "x1"), "interrupted");
    // Resume now works through the normal path.
    d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "idle", 15);
}

#[test]
fn daemon_restart_skips_fenced_and_relaunches_healthy() {
    // Seed: one agent mid-flight (crash → unknown fence) + one healthy.
    let seeded = TempDir::new().unwrap();
    let state = seeded.path().to_path_buf();
    {
        let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
        let cwd = state.to_str().unwrap().to_string();
        for alias in ["fenced", "healthy"] {
            store
                .register_agent(&NewAgent {
                    alias,
                    provider: "fake",
                    endpoint_kind: "fake",
                    role: "worker",
                    cwd: &cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params: None,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
        store.set_agent_state("fenced", "idle", None).unwrap();
        store.set_agent_state("healthy", "idle", None).unwrap();
        store.enqueue("fenced", "work", None, "m1", "user").unwrap();
        match store.take_queued("fenced").unwrap() {
            Take::Message(m) => assert_eq!(m.id, "m1"),
            _ => panic!("expected a message"),
        }
        // Store dropped mid-flight — the crash this daemon recovers.
    }
    let d = TestDaemon::start_on(state);
    // Healthy relaunched; the fenced one was skipped, still attention.
    d.wait_agent("healthy", "idle", 15);
    let agent = d.wait_agent("fenced", "attention", 15);
    assert!(agent["endpoint"].is_null());
    // relaunch_skipped was emitted — no actor was spawned for it, so a
    // queued task is never taken.
    let kinds = event_kinds(&d, "fenced");
    assert!(
        kinds.iter().any(|k| k == "relaunch_skipped"),
        "events: {kinds:?}"
    );
    d.rpc(
        "agent_send",
        json!({"alias": "fenced", "text": "later", "message": "m2"}),
    )
    .unwrap();
    thread::sleep(Duration::from_secs(1));
    assert_eq!(d.message_state("fenced", "m2"), "queued");
    // Unfence + resume still recovers it through the normal path.
    d.rpc(
        "agent_unfence",
        json!({"alias": "fenced", "status": "interrupted"}),
    )
    .unwrap();
    d.rpc("agent_resume", json!({"alias": "fenced"})).unwrap();
    d.wait_agent("fenced", "idle", 15);
    d.wait_message("fenced", "m2", &["completed"], 15);
}

#[test]
fn restart_preserves_attention_fence_without_unknowns() {
    // Seed: an agent fenced for a session-mismatch — `attention` state,
    // recorded error, stored thread — with NO unknown messages.
    // recover() must not rewrite the fence to `offline` before the
    // serve loop reads it.
    let seeded = TempDir::new().unwrap();
    let state = seeded.path().to_path_buf();
    {
        let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
        let cwd = state.to_str().unwrap().to_string();
        for alias in ["mismatch", "healthy"] {
            store
                .register_agent(&NewAgent {
                    alias,
                    provider: "fake",
                    endpoint_kind: "fake",
                    role: "worker",
                    cwd: &cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params: None,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
        store
            .set_identity(
                "mismatch",
                &cadence_agent::adapter::Identity {
                    thread_id: "th-mismatch".into(),
                    session_id: "s-mismatch".into(),
                    model: None,
                    effort: None,
                    pid: 1,
                    endpoint: None,
                    generation: None,
                    attach: None,
                },
            )
            .unwrap();
        store
            .set_agent_state(
                "mismatch",
                "attention",
                Some("pane owns session 'other', expected 's-mismatch'"),
            )
            .unwrap();
        store.set_agent_state("healthy", "idle", None).unwrap();
    }
    let d = TestDaemon::start_on(state);
    // Healthy relaunched; the fenced agent kept its fence AND its
    // original error — recover cleared only the dead runtime fields.
    d.wait_agent("healthy", "idle", 15);
    let agent = d.wait_agent("mismatch", "attention", 15);
    assert!(agent["endpoint"].is_null());
    assert!(
        agent["error"]
            .as_str()
            .unwrap()
            .contains("owns session 'other'"),
        "fence error must survive restart verbatim: {}",
        agent["error"]
    );
    let kinds = event_kinds(&d, "mismatch");
    assert!(
        kinds.iter().any(|k| k == "relaunch_skipped"),
        "events: {kinds:?}"
    );
    // No actor ever spawned for it: a queued task is never taken.
    d.rpc(
        "agent_send",
        json!({"alias": "mismatch", "text": "later", "message": "m2"}),
    )
    .unwrap();
    thread::sleep(Duration::from_secs(1));
    assert_eq!(d.message_state("mismatch", "m2"), "queued");
    // `resume --all` agrees with startup: reported under `fenced` with
    // the remove-and-rejoin hint — never attempted.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["resume", "--all"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let fenced = v["fenced"].as_array().unwrap();
    let entry = fenced
        .iter()
        .find(|r| r["alias"] == "mismatch")
        .expect("mismatch listed under fenced");
    assert_eq!(entry["state"], "attention");
    assert!(
        entry["hint"]
            .as_str()
            .unwrap()
            .contains("cadence agent remove mismatch"),
        "{entry}"
    );
    assert!(v["resumed"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["alias"] != "mismatch"));
    assert_eq!(d.message_state("mismatch", "m2"), "queued");

    // A real CLI restart must not label this unchanged fence as a new
    // restart failure. The healthy agent still relaunches; the fenced
    // agent must neither execute its queued work nor lose its error.
    let home = TempDir::new().unwrap();
    let out = cadence_at(home.path(), &d.state, &["daemon", "restart"]);
    // Stop the detached replacement before asserting the restart outcome.
    let restarted = d.wait_agent("mismatch", "attention", 15);
    d.wait_agent("healthy", "idle", 15);
    let queued = d.message_state("mismatch", "m2");
    let stop = cadence_at(home.path(), &d.state, &["daemon", "stop"]);
    assert!(stop.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "unchanged fence failed restart: {stdout} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("Existing fences retained: mismatch"),
        "{stdout}"
    );
    assert_eq!(queued, "queued");
    assert_eq!(restarted["error"], agent["error"]);
}

#[test]
fn unfenced_agent_stays_stopped_across_restart() {
    let mut d = TestDaemon::start();
    d.register("w1");
    d.register("w2");
    d.wait_agent("w1", "idle", 10);
    d.wait_agent("w2", "idle", 10);
    fence_agent(&d, "w1", "x1");
    // Unfence without resume: the reconcile leaves the agent in the
    // same condition as an operator stop — stopped AND disabled.
    d.rpc(
        "agent_unfence",
        json!({"alias": "w1", "status": "interrupted"}),
    )
    .unwrap();
    let agent = d.wait_agent("w1", "stopped", 10);
    assert_eq!(agent["enabled"], false);
    // Daemon restart: a stopped, disabled member is not relaunched.
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    d.wait_agent("w2", "idle", 15);
    thread::sleep(Duration::from_secs(1));
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(agent["state"], "stopped");
    assert_eq!(agent["enabled"], false);
    assert!(agent["endpoint"].is_null());
    // No actor spawned: a queued message is never taken.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "later", "message": "x2"}),
    )
    .unwrap();
    thread::sleep(Duration::from_secs(1));
    assert_eq!(d.message_state("w1", "x2"), "queued");
    // The operator's explicit resume still works — the normal path.
    d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "idle", 15);
    d.wait_message("w1", "x2", &["completed"], 15);
}

#[test]
fn resume_all_lists_fenced_without_attempting() {
    let d = TestDaemon::start();
    d.register("w1");
    d.register("w2");
    d.wait_agent("w1", "idle", 10);
    d.wait_agent("w2", "idle", 10);
    fence_agent(&d, "w1", "x1");
    // w2 is a resumable member: stored thread, no live endpoint.
    d.rpc("agent_stop", json!({"alias": "w2"})).unwrap();
    d.wait_agent("w2", "stopped", 10);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["resume", "--all"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    // The fenced member is reported under `fenced` with the reconcile
    // hint — never attempted.
    let fenced = v["fenced"].as_array().unwrap();
    let w1 = fenced
        .iter()
        .find(|r| r["alias"] == "w1")
        .expect("w1 listed under fenced");
    let hint = w1["hint"].as_str().unwrap();
    assert!(hint.contains("not resumed"), "{w1}");
    assert!(hint.contains("--no-resume"), "{w1}");
    assert!(!hint.contains("then `cadence agent resume"), "{w1}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("FENCED"), "{stderr}");
    // Still fenced: the message is untouched, the agent never launched.
    assert_eq!(d.message_state("w1", "x1"), "unknown");
    d.wait_agent("w1", "attention", 5);
    // The healthy member resumed normally through the same sweep.
    assert!(v["resumed"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["alias"] == "w2"));
    d.wait_agent("w2", "idle", 15);
}

/// G2: a pty pane that survives a daemon restart mid-turn is re-adopted
/// by the normal resume path after the operator unfences — the adapter
/// reattaches to the surviving tmux session and verifies the pane's
/// native-session lock against the stored thread_id.
#[test]
fn pty_restart_fence_unfence_resume_readopts_pane() {
    let mut d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    let agent = d.wait_agent("dv1", "idle", 20);
    let native = agent["thread_id"].as_str().unwrap().to_string();
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    pty_token(&d, "dv1", "m1");
    let pane_pid: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // Daemon dies mid-turn — the tmux pane survives (detached). The
    // stop itself is clean; deleting the marker makes it a crash —
    // this test asserts the fence path, not hot-restart adoption.
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    drop_shutdown_marker(&d.state);
    let state = d.state.clone();
    // Leak d's TempDir — it owns both the state dir and the mock's pane
    // state, which must outlive the second daemon. forget() also keeps
    // Drop from shutting the new daemon's socket.
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    // Recover fenced the in-flight turn; the restart skipped relaunch.
    let agent = d.wait_agent("dv1", "attention", 15);
    assert!(agent["endpoint"].is_null());
    assert!(event_kinds(&d, "dv1")
        .iter()
        .any(|k| k == "relaunch_skipped"));
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    // The pane itself is still alive.
    let pid_now: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(pid_now, pane_pid);
    // Unfence → stopped, then resume adopts the surviving pane.
    d.rpc(
        "agent_unfence",
        json!({"alias": "dv1", "status": "interrupted"}),
    )
    .unwrap();
    d.wait_agent("dv1", "stopped", 10);
    d.rpc("agent_resume", json!({"alias": "dv1"})).unwrap();
    let agent = d.wait_agent("dv1", "idle", 20);
    // Same pane pid + same native session = adopted, not respawned.
    let pid_after: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(pid_after, pane_pid, "pane was not re-adopted");
    assert_eq!(agent["thread_id"].as_str().unwrap(), native);
    // New work flows over the adopted pane.
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "again", "message": "m2"}),
    )
    .unwrap();
    d.wait_message("dv1", "m2", &["running"], 20);
}

#[test]
fn pty_reattach_waits_for_session_proof() {
    // The reattach branch once read `owned_session` exactly once: a
    // proof that was merely not-yet-visible (a provider still
    // registering, a raced /proc scan) fenced a live pane — and the
    // failed open then closed it, destroying the session it was meant
    // to adopt. The stub's `owned_misses` file replays that window
    // deterministically: the first probes see nothing while the pane
    // holds its lock throughout, and adoption must still succeed.
    let dir = TempDir::new().unwrap();
    let seeded = dir.path().join("state");
    std::fs::create_dir_all(&seeded).unwrap();
    let fixtures = TempDir::new().unwrap();
    {
        let mock = install_mock_stub(fixtures.path());
        let d = TestDaemon::start_on(seeded.clone());
        d.register_stub("st1", json!({}));
        let agent = d.wait_agent("st1", "idle", 20);
        let native = agent["thread_id"].as_str().unwrap().to_string();
        let pane_pid = agent["pid"].as_i64().unwrap();
        drop(d);
        // The next owned_session calls report no session — the
        // transient-miss window the reattach must wait out rather than
        // fence on. The knob is a file in this stub's locks dir,
        // scoped to this fixture, never process env.
        std::fs::write(mock.locks.join("owned_misses"), "3").unwrap();
        let d2 = TestDaemon::start_on(seeded.clone());
        let agent2 = d2.wait_agent("st1", "idle", 25);
        assert_eq!(agent2["thread_id"].as_str().unwrap(), native);
        assert_eq!(agent2["pid"].as_i64().unwrap(), pane_pid);
    }
}

#[test]
fn pty_shutdown_straggler_detaches_pane() {
    // Regression for the force-close half of the re-adoption race:
    // stop_ctls once ran adapter.close() on actors still working when
    // the stop grace expired — even during daemon shutdown, where the
    // contract is detach. kill-session SIGKILLed the pane the restart
    // was meant to re-adopt, and resume then legitimately spawned a
    // replacement ("pane was not re-adopted"). MOCK_TMUX_HOLD stretches
    // every `#{pane_dead}` read past the grace window, so the actor is a
    // deterministic straggler — no sleeps, the latency lives in the
    // mock's response time. (Only pane_dead is held: holding every
    // display-message would let a held cursor read burn the render
    // deadline and fail the send before it ever reaches `running`.)
    let mut d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    let agent = d.wait_agent("dv1", "idle", 25);
    let native = agent["thread_id"].as_str().unwrap().to_string();
    let pane_pid: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // Real env, under MockDevin's ENV_LOCK: the mock tmux reads its hold
    // knobs per call from the env it inherits from the daemon.
    std::env::set_var("MOCK_TMUX_HOLD", "4"); // > STOP_GRACE (3s)
    std::env::set_var("MOCK_TMUX_HOLD_FMT", "#{pane_dead}");
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    // Once the turn is running the actor is inside held adapter probes
    // (gate, then the idle loop's disconnected check); it cannot finish
    // inside the 3s grace, so the straggler path fires every run.
    d.wait_message("dv1", "m1", &["running"], 40);
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    // The straggler stop was clean; a crash leaves no marker — this
    // test asserts the fence-then-resume path, not hot adoption.
    drop_shutdown_marker(&d.state);
    let state = d.state.clone();
    std::env::remove_var("MOCK_TMUX_HOLD");
    std::env::remove_var("MOCK_TMUX_HOLD_FMT");
    // Leak d's TempDir — it owns the state dir and the mock's pane
    // state, which must outlive the second daemon.
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    // Recover fenced the in-flight turn; the pane itself survived the
    // straggler stop, so unfence → resume re-adopts the same pane.
    d.wait_agent("dv1", "attention", 15);
    d.rpc(
        "agent_unfence",
        json!({"alias": "dv1", "status": "interrupted"}),
    )
    .unwrap();
    d.wait_agent("dv1", "stopped", 10);
    d.rpc("agent_resume", json!({"alias": "dv1"})).unwrap();
    let agent = d.wait_agent("dv1", "idle", 25);
    let pid_after: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        pid_after, pane_pid,
        "straggler pane was killed, not detached"
    );
    assert_eq!(agent["thread_id"].as_str().unwrap(), native);
}

// ---- CAD-89: hot restart — a provably clean stop re-adopts running
// pty turns (same token, same pane); every other path still fences.

/// A clean stop writes `shutdown.json`; a crash leaves nothing. Tests
/// asserting the fence path delete it to stand in for the crash.
fn drop_shutdown_marker(state: &Path) {
    let _ = std::fs::remove_file(state.join("shutdown.json"));
}

/// A mid-turn devin put through a clean shutdown. Returns the state
/// dir, the mock (whose pane outlives the stop), the running turn's
/// original token and the pane pid. The stopped TestDaemon is leaked:
/// its TempDir owns both the state dir and the mock's pane state, and
/// its Drop must not race the second daemon's socket.
fn stopped_mid_turn_devin() -> (PathBuf, MockDevin, String, i32) {
    let mut d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.register("pm");
    d.wait_agent("dv1", "idle", 20);
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1",
               "reply_to": "pm"}),
    )
    .unwrap();
    let token = pty_token(&d, "dv1", "m1");
    let pane_pid: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    assert!(
        state.join("shutdown.json").exists(),
        "clean stop must leave a shutdown marker"
    );
    std::mem::forget(d);
    (state, mock, token, pane_pid)
}

/// Read the marker file the stopped daemon left behind.
fn read_marker(state: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(state.join("shutdown.json")).unwrap()).unwrap()
}

/// The single `turn_adopt_refused` event for the alias, if any.
fn adopt_refusal(d: &TestDaemon, alias: &str) -> Option<String> {
    d.rpc("agent_events", json!({"alias": alias})).unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"].as_str() == Some("turn_adopt_refused"))
        .map(|e| e["payload"]["reason"].as_str().unwrap_or("").to_string())
}

#[test]
fn pty_hot_restart_adopts_running_turn() {
    // The acceptance shape: mid-turn, `shutdown`, a new daemon on the
    // same state — the agent never fences, the message stays running,
    // the pane is the same, `turn_adopted` is emitted, and the
    // ORIGINAL token still completes the turn and routes the result.
    let (state, mock, token, pane_pid) = stopped_mid_turn_devin();
    let d = TestDaemon::start_on(state);
    let agent = d.wait_agent("dv1", "idle", 25);
    assert_eq!(d.message_state("dv1", "m1"), "running");
    let pid_now: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(pid_now, pane_pid, "adopted pane changed pid");
    assert_eq!(agent["pid"].as_i64().unwrap() as i32, pane_pid);
    let kinds = event_kinds(&d, "dv1");
    assert!(kinds.iter().any(|k| k == "turn_adopted"), "{kinds:?}");
    assert!(
        !kinds.iter().any(|k| k == "turn_adopt_refused"),
        "{kinds:?}"
    );
    // The turn's original token is still authoritative — the recorded
    // generation was reused, so the report validates.
    d.rpc(
        "message_report",
        json!({"message": "m1", "token": token, "kind": "result",
               "text": "done"}),
    )
    .unwrap();
    d.wait_message("dv1", "m1", &["completed"], 15);
    // ... and the result routed to reply_to as always.
    let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    assert!(
        pm["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["source"] == "worker_result"),
        "adopted turn's result never routed: {pm}"
    );
}

#[test]
fn pty_hot_restart_no_marker_fences() {
    // No marker == crash: the pane survives but the turn fences
    // exactly as before — unknown, attention, no adoption events.
    let (state, _mock, _token, _pid) = stopped_mid_turn_devin();
    drop_shutdown_marker(&state);
    let d = TestDaemon::start_on(state);
    let agent = d.wait_agent("dv1", "attention", 20);
    let error = agent["error"].as_str().unwrap_or("");
    assert!(
        error.contains("Runtime restarted during provider turn"),
        "{error}"
    );
    assert!(error.contains("does not prove"), "{error}");
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    let kinds = event_kinds(&d, "dv1");
    assert!(!kinds.iter().any(|k| k == "turn_adopted"), "{kinds:?}");
    assert!(
        !kinds.iter().any(|k| k == "turn_adopt_refused"),
        "no marker means nothing to refuse: {kinds:?}"
    );
}

#[test]
fn pty_hot_restart_stale_instance_fences() {
    // A marker bound to a different daemon run is not proof this
    // daemon stopped cleanly — every recorded entry is refused.
    let (state, _mock, _token, _pid) = stopped_mid_turn_devin();
    std::fs::write(state.join("daemon-instance"), "some-other-run\n").unwrap();
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "attention", 20);
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    let reason = adopt_refusal(&d, "dv1").expect("missing refusal");
    assert!(reason.contains("daemon run"), "{reason}");
}

#[test]
fn pty_hot_restart_expired_marker_fences() {
    // A marker older than its bound reads stale pane state as fresh —
    // it expires into the fence path instead.
    let (state, _mock, _token, _pid) = stopped_mid_turn_devin();
    let path = state.join("shutdown.json");
    let mut marker = read_marker(&state);
    marker["at"] = json!(marker["at"].as_f64().unwrap() - 3600.0);
    std::fs::write(&path, marker.to_string()).unwrap();
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "attention", 20);
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    let reason = adopt_refusal(&d, "dv1").expect("missing refusal");
    assert!(reason.contains("expired"), "{reason}");
}

#[test]
fn pty_hot_restart_dead_pane_fences() {
    // The pane died between stop and start — nothing left to adopt.
    let (state, _mock, _token, pane_pid) = stopped_mid_turn_devin();
    unsafe { libc::killpg(pane_pid, libc::SIGKILL) };
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "attention", 20);
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    let reason = adopt_refusal(&d, "dv1").expect("missing refusal");
    assert!(reason.contains("pane"), "{reason}");
}

#[test]
fn pty_hot_restart_native_mismatch_fences() {
    // The pane is alive but cannot prove it still holds the recorded
    // native session — the recorded checks fail, the turn fences.
    let (state, _mock, _token, _pid) = stopped_mid_turn_devin();
    let path = state.join("shutdown.json");
    let mut marker = read_marker(&state);
    marker["entries"][0]["native_session"] = json!("bogus-session");
    std::fs::write(&path, marker.to_string()).unwrap();
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "attention", 20);
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    let reason = adopt_refusal(&d, "dv1").expect("missing refusal");
    assert!(reason.contains("session"), "{reason}");
}

#[test]
fn pty_hot_restart_submitting_never_records_running() {
    // A paste whose render never proved must not be recorded running:
    // held captures + a swallowed paste keep m1 `submitting` through
    // the drain, where the render deadline resolves it `unknown` — the
    // restart then fences it like any other uncertain outcome.
    let mut d = TestDaemon::start();
    let mock = d.mock_devin();
    // Real env, under MockDevin's ENV_LOCK: the mock tmux reads its
    // hold knobs per call from the env it inherits from the daemon.
    std::env::set_var("MOCK_TMUX_HOLD", "5");
    std::env::set_var("MOCK_TMUX_HOLD_CMD", "capture-pane");
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 40);
    atomic_write(d.pane_file(&mock, "dv1", "swallow"), "1");
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("dv1", "m1", &["submitting"], 15);
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    std::env::remove_var("MOCK_TMUX_HOLD");
    std::env::remove_var("MOCK_TMUX_HOLD_CMD");
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "attention", 20);
    let m1 = d.message_state("dv1", "m1");
    assert!(
        m1 == "unknown" || m1 == "queued",
        "submitting survived: {m1}"
    );
    assert!(
        !event_kinds(&d, "dv1").iter().any(|k| k == "turn_adopted"),
        "an unproven paste must never be adopted"
    );
}

#[test]
fn pty_hot_restart_render_during_stop_adopts() {
    // The other half of the drain rule: a paste whose render check is
    // still in flight when the stop lands is allowed to finish — it IS
    // a proven running turn and is adopted like any other.
    let mut d = TestDaemon::start();
    let _mock = d.mock_devin();
    // Real env, under MockDevin's ENV_LOCK: the mock tmux reads its
    // hold knobs per call from the env it inherits from the daemon.
    // 2s hold vs the 4s render deadline: comfortably inside it while
    // still spanning the stop that must land mid-render.
    std::env::set_var("MOCK_TMUX_HOLD", "2");
    std::env::set_var("MOCK_TMUX_HOLD_CMD", "capture-pane");
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 40);
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("dv1", "m1", &["submitting"], 15);
    // The stop lands mid-render; the actor finishes the proof before
    // detaching, so the marker records a genuinely running turn.
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    std::env::remove_var("MOCK_TMUX_HOLD");
    std::env::remove_var("MOCK_TMUX_HOLD_CMD");
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "idle", 25);
    assert_eq!(d.message_state("dv1", "m1"), "running");
    assert!(
        event_kinds(&d, "dv1").iter().any(|k| k == "turn_adopted"),
        "render proven during the drain must adopt"
    );
}

#[test]
fn hot_restart_managed_turn_stays_unknown() {
    // Managed endpoints are never adoptable — the provider process
    // dies with its daemon, so a clean stop changes nothing for them.
    let mut d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 25);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "SLEEP:60", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("w1", "m1", &["running"], 25);
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    d.wait_agent("w1", "attention", 25);
    d.wait_message("w1", "m1", &["unknown"], 25);
}

#[test]
fn pty_hot_restart_report_before_adoption_rejected_stale() {
    // The pre-verification window: the store keeps the message
    // `running` but clears `generation` until `open_adopted` proves
    // the pane. A report landing inside the window must be refused
    // as stale — the turn is not yet known to be alive — and the
    // same token must complete once `turn_adopted` fires.
    let (state, _mock, token, _pid) = stopped_mid_turn_devin();
    // Hold the first pane check inside open_adopted so the socket is
    // serving while the adoption is still unproven. Real env, under
    // MockDevin's ENV_LOCK: the mock tmux reads its hold knobs per
    // call from the env it inherits from the daemon.
    std::env::set_var("MOCK_TMUX_HOLD", "8");
    std::env::set_var("MOCK_TMUX_HOLD_CMD", "has-session");
    let d = TestDaemon::start_on(state);
    let early = d.rpc(
        "message_report",
        json!({"message": "m1", "token": token, "kind": "result",
               "text": "early"}),
    );
    std::env::remove_var("MOCK_TMUX_HOLD");
    std::env::remove_var("MOCK_TMUX_HOLD_CMD");
    let err = early.expect_err("pre-proof report must be refused");
    assert!(err.to_string().contains("stale"), "{err}");
    let agent = d.wait_agent("dv1", "idle", 25);
    assert!(
        agent["generation"].as_str().is_some_and(|g| !g.is_empty()),
        "adoption must restore the recorded generation: {agent}"
    );
    assert!(
        event_kinds(&d, "dv1").iter().any(|k| k == "turn_adopted"),
        "pane proof did not adopt the turn"
    );
    d.rpc(
        "message_report",
        json!({"message": "m1", "token": token, "kind": "result",
               "text": "done"}),
    )
    .unwrap();
    d.wait_message("dv1", "m1", &["completed"], 15);
}

#[test]
fn pty_hot_restart_adopts_multiple_running_turns() {
    // Pasted pty messages stay `running` until reported and
    // `take_queued` has no one-running-per-alias guard, so an agent
    // can hold more than one in-flight turn. Every qualifying turn is
    // adopted — same pane proof covers them all — not just the last
    // one the marker listed. The mock pane reads busy through m1's
    // turn, so the second `running` row is crafted the way the real
    // race would leave it: a queued send flipped to running under the
    // same generation.
    let mut d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv1", None);
    let agent = d.wait_agent("dv1", "idle", 20);
    let generation = agent["generation"].as_str().unwrap().to_string();
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    let token1 = pty_token(&d, "dv1", "m1");
    // m2's row as a second proven turn on the same pane — inserted
    // `running` outright so the actor never sees it `queued` and the
    // marker records it verbatim.
    let token2 = format!("pty-{generation}-turn-m2");
    {
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "INSERT INTO messages(id,alias,body,reply_to,source,state,turn_id,
                 created)
             VALUES('m2','dv1','task',NULL,'test','running',?1,1.0)",
            rusqlite::params![token2],
        )
        .unwrap();
    }
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    let marker = read_marker(&state);
    assert_eq!(
        marker["entries"].as_array().unwrap().len(),
        2,
        "both running turns must be recorded: {marker}"
    );
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "idle", 25);
    wait_event_count(&d, "dv1", "turn_adopted", 2, 15);
    assert_eq!(d.message_state("dv1", "m1"), "running");
    assert_eq!(d.message_state("dv1", "m2"), "running");
    for (id, token) in [("m1", token1), ("m2", token2)] {
        d.rpc(
            "message_report",
            json!({"message": id, "token": token, "kind": "result",
                   "text": "done"}),
        )
        .unwrap();
        d.wait_message("dv1", id, &["completed"], 15);
    }
}

/// Releases the CAD-241 snapshot barrier once, including when the test
/// panics, so `serve` cannot stay parked after a failed assertion.
struct SnapshotGate {
    barrier: Option<Arc<Barrier>>,
}

impl SnapshotGate {
    fn release(&mut self) {
        if let Some(barrier) = self.barrier.take() {
            barrier.wait();
        }
    }
}

impl Drop for SnapshotGate {
    fn drop(&mut self) {
        self.release();
    }
}

/// Park two running turns on one PTY whose screen and registry both
/// read idle — the actor is back in its wait, which is the wake that
/// detaches before `Shared::shutdown`.
fn park_idle_labelled_turns(d: &TestDaemon, mock: &MockDevin) -> (String, String, i32) {
    d.register_devin("dv1", None);
    d.register("pm");
    d.wait_agent("dv1", "idle", 20);
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1",
               "reply_to": "pm"}),
    )
    .unwrap();
    let token1 = pty_token(d, "dv1", "m1");
    // `submitted` means `run_turn` has returned. The actor is in the
    // idle wait, not inside the paste, so the next wake can detach it.
    d.wait_event("dv1", "submitted", 15);
    let generation = d.rpc("agent_show", json!({"alias": "dv1"})).unwrap()["agent"]["generation"]
        .as_str()
        .unwrap()
        .to_string();
    let token2 = format!("pty-{generation}-turn-m2");
    {
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "INSERT INTO messages(id,alias,body,reply_to,source,state,turn_id,
                 created)
             VALUES('m2','dv1','other',NULL,'test','running',?1,1.0)",
            rusqlite::params![token2],
        )
        .unwrap();
        conn.execute("UPDATE agents SET state='idle' WHERE alias='dv1'", [])
            .unwrap();
    }
    let agent = d.rpc("agent_show", json!({"alias": "dv1"})).unwrap()["agent"].clone();
    assert_eq!(agent["state"], "idle", "{agent}");
    assert!(agent["pid"].as_i64().is_some(), "{agent}");
    let probe = d.rpc("agent_probe", json!({"alias": "dv1"})).unwrap();
    assert_eq!(probe["idle"], true, "{probe}");
    assert_eq!(probe["reason"], "idle", "{probe}");
    assert_eq!(d.message_state("dv1", "m1"), "running");
    assert_eq!(d.message_state("dv1", "m2"), "running");
    let pane_pid: i32 = std::fs::read_to_string(d.pane_file(mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    (token1, token2, pane_pid)
}

fn message_ids(d: &TestDaemon, alias: &str, id: &str) -> usize {
    d.rpc("agent_show", json!({"alias": alias})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["id"].as_str() == Some(id))
        .count()
}

/// Stop an idle-labelled PTY only after its actor has detached, then
/// prove both turns adopt on the same pane and tokens with no replay.
fn restart_idle_pty_after_forced_detach(via_signal: bool) {
    let barrier = Arc::new(Barrier::new(2));
    let mut opts = daemon_opts();
    opts.release_shutdown_snapshot = Some(Arc::clone(&barrier));
    let mut d = TestDaemon::start_opts(opts);
    let mock = d.mock_devin();
    let (token1, token2, pane_pid) = park_idle_labelled_turns(&d, &mock);
    let screen_before = std::fs::read_to_string(d.pane_file(&mock, "dv1", "screen")).unwrap();
    let mut gate = SnapshotGate { barrier: None };
    if via_signal {
        unsafe { libc::kill(std::process::id() as i32, libc::SIGTERM) };
    } else {
        d.rpc("shutdown", json!({})).unwrap();
    }
    // Arm only after shutdown was requested, so a panic below releases
    // `serve` instead of leaving it blocked.
    gate.barrier = Some(barrier);
    // Do not RPC here. `serve` may already be waiting on the barrier,
    // so the listener will not accept another call until we release it.
    // The agent row is the detach evidence the former snapshot used to miss.
    let deadline = Instant::now() + Duration::from_secs(15);
    let row = loop {
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        let row: (Option<i64>, Option<String>, String) = conn
            .query_row(
                "SELECT pid, generation, state FROM agents WHERE alias='dv1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        if row.0.is_none() && row.1.is_none() {
            break row;
        }
        assert!(
            Instant::now() < deadline,
            "idle actor did not detach before the former facts snapshot: {row:?}"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        row.2, "offline",
        "detach during shutdown must land offline, not a finished turn: {row:?}"
    );
    gate.release();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    let marker = read_marker(&state);
    let entries = marker["entries"].as_array().unwrap();
    assert_eq!(
        entries.len(),
        2,
        "both turns recorded after detach: {marker}"
    );
    for id in ["m1", "m2"] {
        assert!(
            entries.iter().any(|e| e["message_id"].as_str() == Some(id)),
            "{marker}"
        );
    }
    assert!(
        entries
            .iter()
            .all(|e| e["pane_pid"].as_u64() == Some(pane_pid as u64)),
        "{marker}"
    );
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    let agent = d.wait_agent("dv1", "idle", 25);
    assert_eq!(d.message_state("dv1", "m1"), "running");
    assert_eq!(d.message_state("dv1", "m2"), "running");
    assert_eq!(message_ids(&d, "dv1", "m1"), 1);
    assert_eq!(message_ids(&d, "dv1", "m2"), 1);
    let pid_now: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(pid_now, pane_pid, "adopted pane changed pid");
    assert_eq!(agent["pid"].as_i64().unwrap() as i32, pane_pid);
    wait_event_count(&d, "dv1", "turn_adopted", 2, 15);
    let kinds = event_kinds(&d, "dv1");
    assert!(
        !kinds.iter().any(|k| k == "turn_adopt_refused"),
        "{kinds:?}"
    );
    let screen_after = std::fs::read_to_string(d.pane_file(&mock, "dv1", "screen")).unwrap();
    assert_eq!(
        screen_after, screen_before,
        "restart must not replay a paste into the pane"
    );
    for (id, token) in [("m1", token1.as_str()), ("m2", token2.as_str())] {
        d.rpc(
            "message_report",
            json!({"message": id, "token": token, "kind": "result",
                   "text": "done"}),
        )
        .unwrap();
        d.wait_message("dv1", id, &["completed"], 15);
    }
    let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    assert_eq!(
        pm["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["source"] == "worker_result")
            .count(),
        1,
        "m1's original token must route once: {pm}"
    );
}

#[test]
fn pty_shutdown_facts_before_detach_rpc() {
    restart_idle_pty_after_forced_detach(false);
}

#[test]
fn pty_shutdown_facts_before_detach_signal() {
    // Process-per-test: SIGTERM is delivered to this process, and the
    // daemon's signal hook is what requests shutdown.
    restart_idle_pty_after_forced_detach(true);
}

#[test]
fn pty_shutdown_facts_before_detach_unprovable() {
    // Identity cleared before the snapshot is not inferred. Each
    // in-flight turn is refused by name; nothing is adopted or replayed.
    let mut d = TestDaemon::start();
    let mock = d.mock_devin();
    let (_token1, _token2, _pid) = park_idle_labelled_turns(&d, &mock);
    let screen_before = std::fs::read_to_string(d.pane_file(&mock, "dv1", "screen")).unwrap();
    {
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "UPDATE agents SET pid=NULL, generation=NULL, endpoint=NULL
             WHERE alias='dv1'",
            [],
        )
        .unwrap();
    }
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    let marker = read_marker(&state);
    assert!(
        marker["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["alias"].as_str() != Some("dv1")),
        "unprovable identity must not be recorded: {marker}"
    );
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "attention", 20);
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    assert_eq!(d.message_state("dv1", "m2"), "unknown");
    assert_eq!(message_ids(&d, "dv1", "m1"), 1);
    assert_eq!(message_ids(&d, "dv1", "m2"), 1);
    assert!(
        !event_kinds(&d, "dv1").iter().any(|k| k == "turn_adopted"),
        "unprovable identity must not be adopted"
    );
    let refusals: Vec<Value> = d.rpc("agent_events", json!({"alias": "dv1"})).unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"].as_str() == Some("turn_adopt_refused"))
        .cloned()
        .collect();
    assert_eq!(refusals.len(), 2, "{refusals:?}");
    for id in ["m1", "m2"] {
        let reason = refusals
            .iter()
            .find(|e| e["payload"]["message"].as_str() == Some(id))
            .unwrap_or_else(|| panic!("missing refusal for {id}: {refusals:?}"));
        let text = reason["payload"]["reason"].as_str().unwrap_or("");
        assert!(
            text.contains("not provable") && text.contains("do not replay"),
            "{id}: {text}"
        );
    }
    let screen_after = std::fs::read_to_string(d.pane_file(&mock, "dv1", "screen")).unwrap();
    assert_eq!(
        screen_after, screen_before,
        "refusal must not replay a paste"
    );
}

#[test]
fn pty_hot_restart_token_predates_generation_fences() {
    // Facts are captured when shutdown is requested, before actors
    // detach, while RPC threads are still live: a resume that re-opened
    // the pane under a newer generation leaves old tokens stale forever. Recording
    // such a turn would roll the agent back to a dead generation —
    // `shutdown_entries` skips it and names the refusal instead.
    // Doctoring the row is the deterministic stand-in for that race.
    let mut d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    let _token = pty_token(&d, "dv1", "m1");
    // A "resume" that minted a newer generation than m1's token.
    {
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "UPDATE agents SET generation='gen-rewritten' WHERE alias='dv1'",
            [],
        )
        .unwrap();
    }
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    let marker = read_marker(&state);
    assert!(
        marker["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["message_id"].as_str() != Some("m1")),
        "a stale-generation turn must not be recorded: {marker}"
    );
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "attention", 20);
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    let reason = adopt_refusal(&d, "dv1").expect("missing refusal");
    assert!(reason.contains("predates"), "{reason}");
}

#[test]
fn pty_hot_restart_fenced_agent_resume_opens_fresh() {
    // A kept turn on an agent that still fences at restart — a swept
    // sibling left it `unknown` — is orphaned, and its adoption entry
    // must be discarded with it. A later unfence+resume is an ordinary
    // open: a fresh generation, no `turn_adopted` for a reconciled
    // message. Otherwise the stale entry would let the resume reuse
    // the pre-restart generation for a turn already fenced `unknown`.
    let (state, _mock, _token, _pid) = stopped_mid_turn_devin();
    // A second in-flight turn the marker never recorded — inserted
    // `running` post-stop, the sweep makes it `unknown` and the
    // unknown fences the agent, stranding m1's kept entry.
    let generation = read_marker(&state)["entries"][0]["generation"]
        .as_str()
        .unwrap()
        .to_string();
    {
        let conn = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "INSERT INTO messages(id,alias,body,reply_to,source,state,turn_id,
                 created)
             VALUES('m2','dv1','task',NULL,'test','running',?1,1.0)",
            rusqlite::params![format!("pty-{generation}-turn-m2")],
        )
        .unwrap();
    }
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "attention", 20);
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    assert_eq!(d.message_state("dv1", "m2"), "unknown");
    let reason = adopt_refusal(&d, "dv1").expect("missing refusal");
    assert!(reason.contains("agent fenced"), "{reason}");
    // Unfence + resume: the pane is still alive so the open re-adopts
    // it by session ownership — but under a NEW generation, and with
    // no adoption event.
    d.rpc(
        "agent_unfence",
        json!({"alias": "dv1", "status": "interrupted"}),
    )
    .unwrap();
    d.wait_agent("dv1", "stopped", 10);
    d.rpc("agent_resume", json!({"alias": "dv1"})).unwrap();
    let agent = d.wait_agent("dv1", "idle", 25);
    assert_ne!(
        agent["generation"].as_str().unwrap(),
        generation,
        "resume after a restart fence must mint a fresh generation"
    );
    let kinds = event_kinds(&d, "dv1");
    assert!(
        !kinds.iter().any(|k| k == "turn_adopted"),
        "no adoption may fire for a discarded entry: {kinds:?}"
    );
}

#[test]
fn pty_hot_restart_divergent_marker_entries_fence() {
    // One pane proof covers a whole alias list — every entry must
    // describe the same endpoint facts. `shutdown_entries` writes one
    // facts tuple per alias, so a list whose records disagree can only
    // come from a hand-built or corrupt marker: the whole list refuses
    // and the turns fence like any other unprovable restart.
    let (state, _mock, _token, _pid) = stopped_mid_turn_devin();
    // m2 is a real `running` turn that would qualify on its own — its
    // marker entry carries a DIFFERENT pane_pid than m1's.
    let marker = read_marker(&state);
    let generation = marker["entries"][0]["generation"]
        .as_str()
        .unwrap()
        .to_string();
    {
        let conn = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "INSERT INTO messages(id,alias,body,reply_to,source,state,turn_id,
                 created)
             VALUES('m2','dv1','task',NULL,'test','running',?1,1.0)",
            rusqlite::params![format!("pty-{generation}-turn-m2")],
        )
        .unwrap();
    }
    let mut marker = marker;
    let mut entry2 = marker["entries"][0].clone();
    entry2["message_id"] = json!("m2");
    entry2["turn_id"] = json!(format!("pty-{generation}-turn-m2"));
    entry2["pane_pid"] = json!(1);
    marker["entries"].as_array_mut().unwrap().push(entry2);
    std::fs::write(state.join("shutdown.json"), marker.to_string()).unwrap();
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "attention", 20);
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    assert_eq!(d.message_state("dv1", "m2"), "unknown");
    let reason = adopt_refusal(&d, "dv1").expect("missing refusal");
    assert!(reason.contains("disagree"), "{reason}");
}

#[test]
fn daemon_restart_reports_fenced_turn() {
    // The failure half of the TURN column: a turn whose pane did not
    // survive prints `fenced` and the command exits non-zero. The
    // pane dies while the daemon still runs — unnoticed before the
    // stop — so the marker records it and the new daemon's pane proof
    // refuses it.
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
    let _token = pty_token(&d, "dv1", "m1");
    let pane_pid: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    unsafe { libc::killpg(pane_pid, libc::SIGKILL) };
    let home = TempDir::new().unwrap();
    let out = cadence_at(home.path(), &d.state, &["daemon", "restart"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a fenced turn must fail the restart: {stdout} {stderr}"
    );
    assert!(stdout.contains("fenced"), "{stdout} {stderr}");
    let stop = cadence_at(home.path(), &d.state, &["daemon", "stop"]);
    assert!(stop.status.success());
}

/// `daemon restart`'s table reports what happened to an in-flight
/// turn: `kept` when the hot restart adopted it.
#[test]
fn daemon_restart_reports_kept_turn() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv", None);
    d.wait_agent("dv", "idle", 20);
    d.rpc("agent_ready", json!({"alias": "dv"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv", "text": "task", "message": "m1"}),
    )
    .unwrap();
    let token = pty_token(&d, "dv", "m1");
    let home = TempDir::new().unwrap();
    let out = cadence_at(home.path(), &d.state, &["daemon", "restart"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "restart failed: {stdout} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("TURN"), "{stdout}");
    assert!(stdout.contains("kept"), "{stdout}");
    // The restarted daemon adopted the turn — its token completes.
    assert_eq!(d.message_state("dv", "m1"), "running");
    d.rpc(
        "message_report",
        json!({"message": "m1", "token": token, "kind": "result",
               "text": "done"}),
    )
    .unwrap();
    d.wait_message("dv", "m1", &["completed"], 15);
    let stop = cadence_at(home.path(), &d.state, &["daemon", "stop"]);
    assert!(stop.status.success());
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
    // A later resume is rejected by the fence — it is not a relaunch
    // and stays disabled. The rejection does not chain a second resume.
    let fenced = d.rpc("agent_resume", json!({"alias": "w1"})).unwrap_err();
    let fenced = fenced.to_string();
    assert!(fenced.contains("resume refused"), "{fenced}");
    assert!(!fenced.contains("then `cadence agent resume"), "{fenced}");
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

/// Serialize tests that set real process environment variables — the
/// mock-side knobs a provider child inherits (`MOCK_TMUX_STATE`, …).
/// Provider launch commands never go through the environment: see
/// `test_env`.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

thread_local! {
    static TEST_ENV: ProviderEnv = ProviderEnv::default();
    static TEST_STALL_SAMPLE: std::sync::Arc<std::sync::atomic::AtomicU64> =
        std::sync::Arc::default();
}

/// Shrink this test's stall screen-sample interval (0 = daemon default)
/// — a per-daemon option, never `CADENCE_STALL_SAMPLE_SECS` in the
/// shared process env, and live for daemons already running.
fn stall_sample(secs: u64) {
    TEST_STALL_SAMPLE.with(|s| s.store(secs, std::sync::atomic::Ordering::Relaxed));
}

/// This test's provider launch overrides (mock commands). Each test
/// runs on its own thread, so every daemon it starts — restarts
/// included — shares them, and no other test's daemon ever sees them.
fn test_env() -> ProviderEnv {
    TEST_ENV.with(ProviderEnv::clone)
}

/// Host-wide suite slot (CAD-71). With `CADENCE_SUITE_LOCK` set, an
/// unfiltered run of this binary — the full suite — holds that
/// exclusive `flock` for the process lifetime, so concurrent full
/// suites on one host take turns instead of starving each other into
/// load flakes. A filtered run (one test, one group) never queues. The
/// first daemon a test starts acquires it; the kernel releases it at
/// exit. The wait is bounded (`CADENCE_SUITE_LOCK_WAIT_SECS`, default
/// 3600) and every test panics with the reason if it runs out.
static SUITE_SLOT: std::sync::OnceLock<Result<Option<std::fs::File>, String>> =
    std::sync::OnceLock::new();

fn suite_slot() {
    if let Err(msg) = SUITE_SLOT.get_or_init(acquire_suite_slot) {
        panic!("{msg}");
    }
}

/// libtest's positional arguments are test-name filters; these flags
/// take a separate value that is not one.
fn is_filtered_run() -> bool {
    const VALUED: &[&str] = &[
        "--test-threads",
        "--skip",
        "--logfile",
        "--color",
        "--format",
        "--shuffle-seed",
        "-Z",
    ];
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if VALUED.contains(&a.as_str()) {
            args.next();
        } else if !a.starts_with('-') {
            return true;
        }
    }
    false
}

/// Nextest launches every test in a process-per-test child and exposes
/// `NEXTEST=1` plus `NEXTEST_EXECUTION_MODE`. A filtered child cannot
/// safely acquire the host slot itself. The outer review process owns
/// the flock and explicitly clears the child path instead.
fn is_nextest_run() -> bool {
    std::env::var("NEXTEST").ok().as_deref() == Some("1")
        || std::env::var("NEXTEST_EXECUTION_MODE").is_ok()
}

fn nextest_outer_lock_required(
    nextest: bool,
    lock_path: Option<&str>,
    review_held: bool,
) -> Result<(), &'static str> {
    if !nextest {
        return Ok(());
    }
    if review_held && lock_path.is_none() {
        return Ok(());
    }
    Err(
        "nextest requires the external CADENCE_SUITE_LOCK; run `cadence review` or use the pinned outer wrapper",
    )
}

fn acquire_suite_slot() -> Result<Option<std::fs::File>, String> {
    use std::io::Write;
    use std::os::unix::io::AsRawFd;
    let path = std::env::var("CADENCE_SUITE_LOCK")
        .ok()
        .filter(|p| !p.is_empty());
    let review_held = std::env::var("CADENCE_REVIEW_SUITE_LOCK_HELD")
        .ok()
        .as_deref()
        == Some("1");
    nextest_outer_lock_required(is_nextest_run(), path.as_deref(), review_held)
        .map_err(str::to_string)?;
    let Some(path) = path else { return Ok(None) };
    if is_filtered_run() {
        return Ok(None);
    }
    let wait_secs: u64 = std::env::var("CADENCE_SUITE_LOCK_WAIT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3600);
    let path = PathBuf::from(path);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| {
            format!(
                "cannot create the directory of the host suite slot {} \
                 (CADENCE_SUITE_LOCK): {e} — fix the path or unset the variable",
                path.display()
            )
        })?;
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| {
            format!(
                "cannot open the host suite slot {} (CADENCE_SUITE_LOCK): {e} \
                 — fix the path or unset the variable",
                path.display()
            )
        })?;
    let epoch = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    };
    // Raw stderr, not eprintln!: libtest captures the macros per test,
    // and the queueing must be visible while it happens.
    let say = |msg: String| {
        let _ = std::io::stderr().write_all(format!("{msg}\n").as_bytes());
    };
    let start = Instant::now();
    let mut announced = false;
    loop {
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            say(format!(
                "suite slot {} acquired at epoch {} after {}s (pid {})",
                path.display(),
                epoch(),
                start.elapsed().as_secs(),
                std::process::id()
            ));
            return Ok(Some(file));
        }
        if !announced {
            say(format!(
                "suite slot {} busy — another full suite runs on this host; \
                 waiting up to {wait_secs}s (epoch {})",
                path.display(),
                epoch()
            ));
            announced = true;
        }
        if start.elapsed() >= Duration::from_secs(wait_secs) {
            return Err(format!(
                "timed out after {wait_secs}s waiting for the host suite slot {} \
                 (CADENCE_SUITE_LOCK) — another full suite still holds it; \
                 unset the variable to run unserialized",
                path.display()
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

#[test]
fn nextest_requires_external_suite_lock_without_nested_flock() {
    // Ordinary cargo filtered tests retain the historical no-slot path.
    assert!(nextest_outer_lock_required(false, None, false).is_ok());
    // Direct nextest is refused whether the caller forgot the path or
    // supplied one without proving that an outer review owns it.
    assert!(nextest_outer_lock_required(true, None, false).is_err());
    assert!(nextest_outer_lock_required(true, Some("/tmp/suite.lock"), false).is_err());
    // Review's outer Flock is the only accepted child contract: it clears
    // the path and sets the marker, so no nested flock can deadlock.
    assert!(nextest_outer_lock_required(true, None, true).is_ok());
    assert!(nextest_outer_lock_required(true, Some("/tmp/suite.lock"), true).is_err());
}

fn daemon_opts() -> daemon::ServeOptions {
    daemon::ServeOptions {
        provider_env: test_env(),
        stall_sample_secs: TEST_STALL_SAMPLE.with(std::sync::Arc::clone),
        // Explicit defaults keep test daemons hermetic — a real pm.yaml
        // [host] table on the dev host must never leak into a test.
        slots: Some(cadence_agent::slots::SlotConfig::default()),
        slot_clock: None,
        release_shutdown_snapshot: None,
    }
}

/// A stdio JSON-RPC provider speaking just enough of the app-server wire
/// to reach each failure mode. Writes its pid to a file for leak checks.
const MOCK_PY: &str = r#"
import json, os, sys, time
pidfile, mode = sys.argv[1], sys.argv[2]
turn_count = 0
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
    elif method == "model/list":
        # Metadata-only response: configured Codex tests can exercise the
        # pair validator without making a paid model turn.
        if mode == "bad-model-list":
            emit({"id": mid, "result": {}})
        else:
            emit({"id": mid, "result": {"data": [
                {"id": "gpt-5.6-luna", "model": "gpt-5.6-luna",
                 "isDefault": False,
                 "supportedReasoningEfforts": [
                     {"reasoningEffort": "low"},
                     {"reasoningEffort": "medium"},
                     {"reasoningEffort": "high"},
                     {"reasoningEffort": "xhigh"},
                     {"reasoningEffort": "max"}]},
                {"id": "mock-model", "model": "mock-model",
                 "isDefault": True,
                 "supportedReasoningEfforts": [{"reasoningEffort": "medium"}]}
            ], "nextCursor": None}})
    elif method in ("thread/start", "thread/resume"):
        # Record the launch payload before answering so tests can read
        # exactly what reached the wire (<pidfile>.requests).
        with open(pidfile + ".requests", "a") as rf:
            rf.write(json.dumps({"method": method,
                                 "params": msg.get("params", {})}) + "\n")
        if mode == "bad-thread":
            emit({"id": mid, "result": {"thread": {}}})
        else:
            launch = msg.get("params", {})
            effort = launch.get("config", {}).get("model_reasoning_effort", "medium")
            model = launch.get("model", "mock-model")
            emit({"id": mid, "result": {"thread": {
                "id": "th-1", "sessionId": "s-1", "model": model,
                "reasoningEffort": effort},
                "model": model, "reasoningEffort": effort}})
    elif method == "account/rateLimits/read":
        if mode in ("no-quota", "quota-recover"):
            emit({"id": mid, "error": {"code": -32601,
                 "message": "rate limits unavailable in this auth mode"}})
        else:
            emit({"id": mid, "result": {
                "accountId": "acct-codex-test",
                "rateLimits": {
                    "primary": {"usedPercent": 23,
                                 "windowDurationMins": 60,
                                 "resetsAt": 1900000000},
                    "secondary": None},
                "rateLimitsByLimitId": {
                    "codex": {"usedPercent": 7,
                              "windowDurationMins": 10080,
                              "resetsAt": 1900100000}},
                "planType": "mock-pro"}})
    elif method == "turn/start":
        turn_count += 1
        if mode == "bad-turn":
            emit({"id": mid, "result": {"turn": {}}})
        elif mode == "die-after-start":
            emit({"id": mid, "result": {"turn": {"id": "t-1"}}}); os._exit(0)
        elif mode == "silent":
            emit({"id": mid, "result": {"turn": {"id": "t-1"}}})
        else:
            emit({"id": mid, "result": {"turn": {"id": "t-1"}}})
            if mode in ("quota-update", "quota-recover"):
                if mode == "quota-recover":
                    update = {"accountId": "acct-recovered",
                              "rateLimits": {"primary": {"usedPercent": 42}}}
                elif turn_count == 1:
                    # First update explicitly clears nullable window fields;
                    # the account id is tested separately as a conservative
                    # identity field and must survive its explicit null.
                    update = {"accountId": None,
                              "rateLimits": {"primary": {
                                  "usedPercent": 42,
                                  "windowDurationMins": None,
                                  "resetsAt": None}}}
                else:
                    # The second update omits the nullable fields entirely.
                    # Omission must preserve their already-cleared state.
                    update = {"rateLimits": {"primary": {"usedPercent": 44}}}
                emit({"method": "account/rateLimits/updated", "params": update})
            emit({"method": "turn/completed", "params": {"turn": {
                "id": "t-1", "status": "completed", "items": [
                    {"id": "i1", "type": "agentMessage",
                     "text": "MOCK_OK", "phase": "final_answer"}]}}})
    elif method == "turn/interrupt":
        emit({"id": mid, "result": {}})
"#;

struct MockCodex {
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
        elif method == "model/list":
            send_json(conn, {"id": mid, "result": {"data": [
                {"id": "gpt-5.6-luna", "model": "gpt-5.6-luna",
                 "isDefault": False,
                 "supportedReasoningEfforts": [
                     {"reasoningEffort": "low"},
                     {"reasoningEffort": "medium"},
                     {"reasoningEffort": "high"},
                     {"reasoningEffort": "xhigh"},
                     {"reasoningEffort": "max"}]},
                {"id": "mock-model", "model": "mock-model",
                 "isDefault": True,
                 "supportedReasoningEfforts": [{"reasoningEffort": "medium"}]}
            ], "nextCursor": None}})
        elif method in ("thread/start", "thread/resume"):
            # Record the launch payload before answering (<pidfile>.requests).
            with open(pidfile + ".requests", "a") as rf:
                rf.write(json.dumps({"method": method,
                                     "params": msg.get("params", {})}) + "\n")
            launch = msg.get("params", {})
            effort = launch.get("config", {}).get("model_reasoning_effort", "medium")
            model = launch.get("model", "mock-model")
            send_json(conn, {"id": mid, "result": {"thread": {
                "id": "th-1", "sessionId": "s-1", "model": model,
                "reasoningEffort": effort},
                "model": model, "reasoningEffort": effort}})
        elif method == "account/rateLimits/read":
            if mode == "no-quota":
                send_json(conn, {"id": mid, "error": {"code": -32601,
                    "message": "rate limits unavailable in this auth mode"}})
            else:
                send_json(conn, {"id": mid, "result": {
                    "accountId": "acct-codex-test",
                    "rateLimits": {"primary": {"usedPercent": 23,
                        "windowDurationMins": 60, "resetsAt": 1900000000}},
                    "rateLimitsByLimitId": {}, "planType": "mock-pro"}})
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
        let pidfile = self.dir.path().join(format!("mock-{mode}.pid"));
        let script = self.dir.path().join(format!("mock-{mode}.py"));
        std::fs::write(&script, MOCK_PY).unwrap();
        test_env().set(
            "CADENCE_CODEX_COMMAND",
            format!(
                "python3 {} {} {}",
                script.display(),
                pidfile.display(),
                mode
            ),
        );
        MockCodex { pidfile }
    }

    /// Install a mock WebSocket app-server command for `mode`. `dir`
    /// hosts the script + pidfile and must outlive every daemon that
    /// will spawn it (restart tests use the seeded state dir).
    fn mock_codex_ws_at(&self, dir: &Path, mode: &str) -> MockCodex {
        let pidfile = dir.join(format!("mock-ws-{mode}.pid"));
        let script = dir.join(format!("mock-ws-{mode}.py"));
        std::fs::write(&script, MOCK_WS_PY).unwrap();
        test_env().set(
            "CADENCE_CODEX_WS_COMMAND",
            format!(
                "python3 {} {} {}",
                script.display(),
                pidfile.display(),
                mode
            ),
        );
        MockCodex { pidfile }
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
        test_env().remove("CADENCE_CODEX_COMMAND");
        test_env().remove("CADENCE_CODEX_WS_COMMAND");
    }
}

/// The mock codex appends one `{"method","params"}` line per
/// `thread/start`/`thread/resume` to `<pidfile>.requests` — read it to
/// assert exactly what reached the wire.
fn mock_requests(mock: &MockCodex) -> Vec<Value> {
    std::fs::read_to_string(format!("{}.requests", mock.pidfile.display()))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
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

/// Poll `agent_probe` until the pane reads idle — a daemon-side cause
/// ordered after the endpoint's own process has exec'd and painted:
/// the mocks write their argv/env dumps BEFORE the first screen paint,
/// so `idle` proves those dumps are on disk. `wait_agent` on `idle`
/// alone only proves the transport opened (spawn happened); under load
/// the child may still be booting. Read launch-shape files only after
/// this — never poll the file itself, whose previous generation's
/// content looks valid while stale.
fn wait_probe_idle(d: &TestDaemon, alias: &str, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let probe = d.rpc("agent_probe", json!({"alias": alias})).unwrap();
        if probe["idle"] == true {
            return;
        }
        assert!(Instant::now() < deadline, "probe never read {alias} idle");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn codex_approval_policy_defaults_to_never_and_replays_on_resume() {
    let d = TestDaemon::start();
    let mock = d.mock_codex("ok");
    d.register_codex("w1");
    d.wait_agent("w1", "idle", 15);
    // A cadence-launched codex worker is unattended by default: the
    // wire carries `never` even when no policy was stored, and the
    // rpc-default `read-only` sandbox is what was sent.
    let reqs = mock_requests(&mock);
    assert_eq!(reqs.len(), 1, "{reqs:?}");
    assert_eq!(reqs[0]["method"], "thread/start");
    assert_eq!(reqs[0]["params"]["approvalPolicy"], "never");
    assert_eq!(reqs[0]["params"]["sandbox"], "read-only");
    // Stop + resume reopens the thread: the same effective policy is
    // replayed verbatim on `thread/resume`.
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "stopped", 15);
    d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "idle", 15);
    let reqs = mock_requests(&mock);
    assert_eq!(reqs.len(), 2, "{reqs:?}");
    assert_eq!(reqs[1]["method"], "thread/resume");
    assert_eq!(reqs[1]["params"]["approvalPolicy"], "never");
    assert_eq!(reqs[1]["params"]["threadId"], "th-1");
}

#[test]
fn codex_quota_is_provider_bound_and_sparse_updates_handle_nullable_fields() {
    let d = TestDaemon::start();
    let _mock = d.mock_codex("quota-update");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "agent_register",
        json!({"alias": "quota", "provider": "codex",
               "endpoint_kind": "managed", "cwd": cwd,
               "params": "{\"quota\":{\"state\":\"available\",\"used_percent\":100}}"}),
    )
    .unwrap();
    d.wait_agent("quota", "idle", 15);

    let initial = d.rpc("agent_show", json!({"alias": "quota"})).unwrap()["agent"].clone();
    let quota = &initial["quota"];
    assert_eq!(quota["provider"], "codex");
    assert_eq!(quota["assignee"], "quota");
    assert_eq!(quota["account_id"], "acct-codex-test");
    assert_eq!(quota["thread_id"], "th-1");
    assert_eq!(quota["state"], "available");
    assert_eq!(quota["data"]["rateLimits"]["primary"]["usedPercent"], 23);
    assert_eq!(
        quota["data"]["rateLimitsByLimitId"]["codex"]["usedPercent"],
        7
    );
    assert!(quota["observed_at"]
        .as_str()
        .is_some_and(|value| value.contains('T')));
    // A caller-supplied params value never becomes provider evidence.
    assert_eq!(initial["params"]["quota"]["used_percent"], 100);
    assert_eq!(quota["used_percent"], Value::Null);

    d.rpc(
        "agent_send",
        json!({"alias": "quota", "text": "clear", "message": "quota-explicit-null"}),
    )
    .unwrap();
    d.wait_message("quota", "quota-explicit-null", &["completed"], 15);
    let first = d.rpc("agent_show", json!({"alias": "quota"})).unwrap()["agent"].clone();
    let quota = &first["quota"];
    assert_eq!(quota["state"], "available");
    // Account identity is intentionally conservative: an explicit null does
    // not erase the provider-bound account id.
    assert_eq!(quota["account_id"], "acct-codex-test");
    assert_eq!(quota["data"]["rateLimits"]["primary"]["usedPercent"], 42);
    // Explicit nullable fields replace the previously reported values with
    // provider-declared nulls.
    let primary = &quota["data"]["rateLimits"]["primary"];
    assert!(
        primary
            .get("windowDurationMins")
            .is_some_and(Value::is_null),
        "{first}"
    );
    assert!(
        primary.get("resetsAt").is_some_and(Value::is_null),
        "{first}"
    );
    // These provider fields were omitted from the update and remain intact.
    assert_eq!(quota["data"]["planType"], "mock-pro");
    assert_eq!(
        quota["data"]["rateLimitsByLimitId"]["codex"]["windowDurationMins"],
        10080
    );
    assert_eq!(
        quota["data"]["rateLimitsByLimitId"]["codex"]["usedPercent"],
        7
    );

    d.rpc(
        "agent_send",
        json!({"alias": "quota", "text": "omit", "message": "quota-omitted"}),
    )
    .unwrap();
    d.wait_message("quota", "quota-omitted", &["completed"], 15);
    let updated = d.rpc("agent_show", json!({"alias": "quota"})).unwrap()["agent"].clone();
    let quota = &updated["quota"];
    assert_eq!(quota["state"], "available");
    assert_eq!(quota["account_id"], "acct-codex-test");
    assert_eq!(quota["data"]["rateLimits"]["primary"]["usedPercent"], 44);
    // Omitted fields preserve the explicit null state rather than restoring
    // the initial values.
    let primary = &quota["data"]["rateLimits"]["primary"];
    assert!(
        primary
            .get("windowDurationMins")
            .is_some_and(Value::is_null),
        "{updated}"
    );
    assert!(
        primary.get("resetsAt").is_some_and(Value::is_null),
        "{updated}"
    );
    assert_eq!(quota["data"]["planType"], "mock-pro");
    assert_eq!(
        quota["data"]["rateLimitsByLimitId"]["codex"]["usedPercent"],
        7
    );
}

#[test]
fn codex_quota_endpoint_failure_is_explicit_unknown() {
    let d = TestDaemon::start();
    let _mock = d.mock_codex("no-quota");
    d.register_codex("no-quota");
    d.wait_agent("no-quota", "idle", 15);
    let agent = d.rpc("agent_show", json!({"alias": "no-quota"})).unwrap()["agent"].clone();
    assert_eq!(agent["quota"]["state"], "unavailable");
    assert_eq!(agent["quota"]["account_id"], Value::Null);
    assert_eq!(agent["quota"]["data"], Value::Null);
    assert!(agent["quota"]["reason"]
        .as_str()
        .is_some_and(|reason| { reason.contains("unavailable") }));
}

#[test]
fn codex_quota_recovers_from_unavailable_to_available() {
    let d = TestDaemon::start();
    let _mock = d.mock_codex("quota-recover");
    d.register_codex("recover");
    d.wait_agent("recover", "idle", 15);
    let initial = d.rpc("agent_show", json!({"alias": "recover"})).unwrap()["agent"].clone();
    assert_eq!(initial["quota"]["state"], "unavailable");

    d.rpc(
        "agent_send",
        json!({"alias": "recover", "text": "refresh", "message": "quota-recover"}),
    )
    .unwrap();
    d.wait_message("recover", "quota-recover", &["completed"], 15);
    let recovered = d.rpc("agent_show", json!({"alias": "recover"})).unwrap()["agent"].clone();
    assert_eq!(recovered["quota"]["state"], "available");
    assert_eq!(recovered["quota"]["account_id"], "acct-recovered");
    assert_eq!(
        recovered["quota"]["data"]["rateLimits"]["primary"]["usedPercent"],
        42
    );
}

#[test]
fn codex_model_effort_are_validated_reported_and_replayed_on_resume() {
    let d = TestDaemon::start();
    let mock = d.mock_codex("ok");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "agent_register",
        json!({"alias": "luna", "provider": "codex",
               "endpoint_kind": "managed", "cwd": cwd,
               "params": "{\"model\":\"gpt-5.6-luna\",\"effort\":\"max\"}"}),
    )
    .unwrap();
    d.wait_agent("luna", "idle", 15);
    let reqs = mock_requests(&mock);
    assert_eq!(reqs.len(), 1, "{reqs:?}");
    assert_eq!(reqs[0]["method"], "thread/start");
    assert_eq!(reqs[0]["params"]["model"], "gpt-5.6-luna");
    assert_eq!(reqs[0]["params"]["config"]["model_reasoning_effort"], "max");
    let agent = d.rpc("agent_show", json!({"alias": "luna"})).unwrap()["agent"].clone();
    assert_eq!(agent["model_configured"], "gpt-5.6-luna", "{agent}");
    assert_eq!(agent["model_reported"], "gpt-5.6-luna", "{agent}");
    assert_eq!(agent["model_effective"], "gpt-5.6-luna", "{agent}");
    assert_eq!(agent["effort_configured"], "max", "{agent}");
    assert_eq!(agent["effort_reported"], "max", "{agent}");
    assert_eq!(agent["effort_effective"], "max", "{agent}");

    d.rpc("agent_stop", json!({"alias": "luna"})).unwrap();
    d.wait_agent("luna", "stopped", 15);
    d.rpc("agent_resume", json!({"alias": "luna"})).unwrap();
    d.wait_agent("luna", "idle", 15);
    let reqs = mock_requests(&mock);
    assert_eq!(reqs.len(), 2, "{reqs:?}");
    assert_eq!(reqs[1]["method"], "thread/resume");
    assert_eq!(reqs[1]["params"]["threadId"], "th-1");
    assert_eq!(reqs[1]["params"]["model"], "gpt-5.6-luna");
    assert_eq!(reqs[1]["params"]["config"]["model_reasoning_effort"], "max");
}

#[test]
fn codex_model_effort_pair_rejection_is_visible() {
    let d = TestDaemon::start();
    let _mock = d.mock_codex("ok");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "agent_register",
        json!({"alias": "bad-luna", "provider": "codex",
               "endpoint_kind": "managed", "cwd": cwd,
               "params": "{\"model\":\"gpt-5.6-luna\",\"effort\":\"ultra\"}"}),
    )
    .unwrap();
    let agent = d.wait_agent("bad-luna", "attention", 15);
    let error = agent["error"].as_str().unwrap_or_default();
    assert!(error.contains("provider rejected effort"), "{error}");
    assert!(error.contains("gpt-5.6-luna"), "{error}");
    assert!(error.contains("max"), "{error}");
}

#[test]
fn codex_model_availability_unknown_is_visible() {
    let d = TestDaemon::start();
    let _mock = d.mock_codex("bad-model-list");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "agent_register",
        json!({"alias": "unknown-luna", "provider": "codex",
               "endpoint_kind": "managed", "cwd": cwd,
               "params": "{\"model\":\"gpt-5.6-luna\"}"}),
    )
    .unwrap();
    let agent = d.wait_agent("unknown-luna", "attention", 15);
    let error = agent["error"].as_str().unwrap_or_default();
    assert!(error.contains("availability unknown"), "{error}");
    assert!(!error.contains("provider rejected"), "{error}");
}

#[test]
fn codex_ws_model_effort_are_replayed_and_reported() {
    let d = TestDaemon::start();
    let mock = d.mock_codex_ws("ok");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "agent_register",
        json!({"alias": "luna-ws", "provider": "codex",
               "endpoint_kind": "managed-ws", "cwd": cwd,
               "params": "{\"model\":\"gpt-5.6-luna\",\"effort\":\"max\"}"}),
    )
    .unwrap();
    d.wait_agent("luna-ws", "idle", 15);
    let agent = d.rpc("agent_show", json!({"alias": "luna-ws"})).unwrap()["agent"].clone();
    assert_eq!(agent["model_effective"], "gpt-5.6-luna", "{agent}");
    assert_eq!(agent["effort_effective"], "max", "{agent}");
    let reqs = mock_requests(&mock);
    assert_eq!(reqs[0]["params"]["model"], "gpt-5.6-luna");
    assert_eq!(reqs[0]["params"]["config"]["model_reasoning_effort"], "max");
}

#[test]
fn codex_approval_policy_reaches_thread_start_verbatim() {
    let d = TestDaemon::start();
    let mock = d.mock_codex("ok");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "agent_register",
        json!({"alias": "w1", "provider": "codex",
               "endpoint_kind": "managed", "cwd": cwd,
               "sandbox": "workspace-write",
               "params": "{\"approval_policy\":\"on-failure\"}"}),
    )
    .unwrap();
    d.wait_agent("w1", "idle", 15);
    let reqs = mock_requests(&mock);
    assert_eq!(reqs.len(), 1, "{reqs:?}");
    assert_eq!(reqs[0]["method"], "thread/start");
    assert_eq!(reqs[0]["params"]["approvalPolicy"], "on-failure");
    assert_eq!(reqs[0]["params"]["sandbox"], "workspace-write");
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(agent["sandbox"], "workspace-write");
}

#[test]
fn codex_approval_policy_rejected_at_register_and_next_launch() {
    let d = TestDaemon::start();
    let mock = d.mock_codex("ok");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    // Register: a bogus value is refused before it lands on the row,
    // and the error names every accepted value.
    let err = d
        .rpc(
            "agent_register",
            json!({"alias": "w1", "provider": "codex",
                   "endpoint_kind": "managed", "cwd": cwd,
                   "params": "{\"approval_policy\":\"bogus\"}"}),
        )
        .unwrap_err()
        .to_string();
    for accepted in ["never", "on-request", "on-failure", "untrusted"] {
        assert!(
            err.contains(accepted),
            "register error missing '{accepted}': {err}"
        );
    }
    d.register_codex("w1");
    d.wait_agent("w1", "idle", 15);
    // `agent set --next-launch`: the same vocabulary is enforced, the
    // same error lists all four.
    let err = d
        .rpc(
            "agent_set",
            json!({"alias": "w1", "next_launch": true,
                   "patch": {"approval_policy": "bogus"}}),
        )
        .unwrap_err()
        .to_string();
    for accepted in ["never", "on-request", "on-failure", "untrusted"] {
        assert!(
            err.contains(accepted),
            "next-launch error missing '{accepted}': {err}"
        );
    }
    // A valid next-launch value is stored and reaches the wire on the
    // next open — resume replays it verbatim.
    d.rpc(
        "agent_set",
        json!({"alias": "w1", "next_launch": true,
               "patch": {"approval_policy": "untrusted"}}),
    )
    .unwrap();
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "stopped", 15);
    d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "idle", 15);
    let reqs = mock_requests(&mock);
    let last = reqs.last().unwrap();
    assert_eq!(last["method"], "thread/resume");
    assert_eq!(last["params"]["approvalPolicy"], "untrusted");
}

#[test]
fn codex_approval_policy_rejected_at_open() {
    // Params corrupted behind the daemon's back still cannot reach the
    // wire — the adapter validates again inside `open`.
    let d = TestDaemon::start();
    let _mock = d.mock_codex("ok");
    d.register_codex("w1");
    d.wait_agent("w1", "idle", 15);
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "stopped", 15);
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET params='{\"approval_policy\":\"bogus\"}' WHERE alias='w1'",
        [],
    )
    .unwrap();
    drop(conn);
    d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();
    let agent = d.wait_agent("w1", "attention", 15);
    let err = agent["error"].as_str().unwrap().to_string();
    for accepted in ["never", "on-request", "on-failure", "untrusted"] {
        assert!(
            err.contains(accepted),
            "open error missing '{accepted}': {err}"
        );
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
    assert!(m1["error"].as_str().unwrap().contains("no turn id"), "{m1}");
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
    // Actor exit must keep the provider account. The generic review
    // sentence is only the fallback when no account was recorded.
    let reason = reason.as_str().unwrap();
    assert!(reason.contains("Connection lost during turn"), "{reason}");
    assert!(reason.contains("does not prove"), "{reason}");
    assert!(!reason.contains("then `cadence agent resume"), "{reason}");
    // Stop only disables: the fence state and its reason stay visible.
    let stopped = d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    assert_eq!(stopped["state"], "attention");
    let agent = d.wait_agent("w1", "attention", 10);
    assert_eq!(agent["enabled"], false);
    // Stop must not replace the provider account with a generic fence.
    assert!(
        agent["error"]
            .as_str()
            .unwrap()
            .contains("Connection lost during turn"),
        "{}",
        agent["error"]
    );
    // Repeated stop is idempotent and still does not mask the fence.
    let again = d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    assert_eq!(again["state"], "attention");
    let agent = d.wait_agent("w1", "attention", 5);
    assert!(
        agent["error"]
            .as_str()
            .unwrap()
            .contains("Connection lost during turn"),
        "{}",
        agent["error"]
    );
    assert_eq!(d.message_state("w1", "m1"), "unknown");
    // Resume is rejected until the operator reconciles — the fence is
    // not masked by either verb, and the rejection does not chain a second resume.
    let fenced = d.rpc("agent_resume", json!({"alias": "w1"})).unwrap_err();
    let fenced = fenced.to_string();
    assert!(fenced.contains("resume refused"), "{fenced}");
    assert!(!fenced.contains("then `cadence agent resume"), "{fenced}");
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
import os, signal, subprocess, sys, time

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

def awrite(path, text):
    # Atomic write — a capture-pane reader sees whole content or none.
    tmp = path + ".tmp"
    with open(tmp, "w") as f: f.write(text)
    os.rename(tmp, path)

def aappend(path, text):
    try: cur = open(path).read()
    except OSError: cur = ""
    awrite(path, cur + text)

cmd, rest = args[0], args[1:]
# Every invocation lands in calls.log — tests that must count a probe
# (e.g. `cadence status` probing exactly once per pty agent) read it.
try:
    with open(os.path.join(state, "calls.log"), "a") as f:
        f.write(cmd + " " + " ".join(rest) + "\n")
except Exception:
    pass
# Deterministic latency injection: MOCK_TMUX_HOLD=<secs> delays the
# command named by MOCK_TMUX_HOLD_CMD (default display-message) — the
# harness's way to make an adapter probe straggle past the daemon's
# stop grace without any sleep in test code. MOCK_TMUX_HOLD_FMT narrows
# the hold to calls whose args contain it (e.g. only #{pane_dead}), so
# latency lands on the probe under test instead of every probe.
hold = float(os.environ.get("MOCK_TMUX_HOLD", "0"))
hold_fmt = os.environ.get("MOCK_TMUX_HOLD_FMT", "")
if hold and cmd == os.environ.get("MOCK_TMUX_HOLD_CMD", "display-message") \
        and (not hold_fmt or hold_fmt in rest):
    time.sleep(hold)
# MOCK_TMUX_FAIL=<cmd> makes that subcommand die — deterministic
# failure injection, e.g. a transient capture-pane outage while a
# gate probe runs.
if cmd and cmd == os.environ.get("MOCK_TMUX_FAIL", ""):
    die("mock injected failure")
if cmd == "new-session":
    name = rest[rest.index("-s") + 1]
    cwd = rest[rest.index("-c") + 1] if "-c" in rest else os.getcwd()
    pane_cmd = rest[-1]
    pane = os.path.join(state, name)
    env = dict(os.environ, FAKE_PANE=pane)
    # tmux -e VAR=value exports into the pane process env.
    for i, a in enumerate(rest[:-1]):
        if a == "-e" and "=" in rest[i + 1]:
            k, v = rest[i + 1].split("=", 1)
            env[k] = v
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
    elif fmt == "#{cursor_x},#{cursor_y}":
        # The cursor sits on the TUI's input line: column 2 (right
        # after the prompt glyph + space) when empty, or after the
        # staged draft. Row = the rendered input row — the staged line
        # capture-pane appends when text is staged, else the last
        # prompt-glyph row of the screen.
        try: staged = open(sess_path(name, "input")).read()
        except FileNotFoundError: staged = ""
        try: rows = open(sess_path(name, "screen")).read().splitlines()
        except FileNotFoundError: rows = []
        if staged:
            print("%d,%d" % (2 + len(staged), len(rows)))
        else:
            y = max((i for i, l in enumerate(rows)
                     if l.strip().startswith(("❯", "❭", "»"))), default=0)
            print("2,%d" % y)
    else: die("unknown format " + fmt)
    sys.exit(0)
if cmd == "capture-pane":
    name = rest[rest.index("-t") + 1]
    # Count captures so tests can prove the stall ticker only samples
    # panes while a turn is running.
    try:
        with open(sess_path(name, "captures"), "a") as f: f.write("c")
    except OSError: pass
    out = ""
    try: out += open(sess_path(name, "screen")).read()
    except FileNotFoundError: die("no such session")
    # The input line renders like the TUI's own: the pane's `.glyph`
    # file (written by its TUI; `❭` is the Devin default) + staged draft.
    try:
        staged = open(sess_path(name, "input")).read()
        if staged:
            try: glyph = open(sess_path(name, "glyph")).read().strip() or "❭"
            except FileNotFoundError: glyph = "❭"
            out += glyph + " " + staged + "\n"
    except FileNotFoundError: pass
    # Test-controlled extra screen content — a file the test writes to
    # make the pane look busy, approval-blocked, etc.
    try: out += open(sess_path(name, "tui-state")).read()
    except FileNotFoundError: pass
    # Real tmux only prints the pane with `-p` — without it the capture
    # lands in the paste buffer and stdout stays empty. Emulate that so
    # a dropped `-p` fails loudly here the way it does on a real pane.
    if "-p" not in rest:
        sys.exit(0)
    sys.stdout.write(out); sys.exit(0)
if cmd == "load-buffer":
    open(os.path.join(state, "buffer"), "w").write(open(rest[-1]).read())
    sys.exit(0)
if cmd == "paste-buffer":
    name = rest[rest.index("-t") + 1]
    # A `.swallow` file models a busy TUI dropping the bracketed paste:
    # the write path "works" but the text never reaches the screen.
    if not os.path.exists(sess_path(name, "swallow")):
        aappend(sess_path(name, "input"),
                open(os.path.join(state, "buffer")).read())
    sys.exit(0)
if cmd == "send-keys":
    name = rest[rest.index("-t") + 1]
    for key in rest[rest.index("-t") + 2:]:
        if key == "--":  # ends tmux option parsing — not a key
            continue
        aappend(sess_path(name, "input"),
                "<ENTER>" if key == "Enter" else "<KEY:" + key + ">")
    sys.exit(0)
if cmd == "set-option":
    # Record option writes so tests can assert pane defaults.
    with open(os.path.join(state, "setopt.log"), "a") as f:
        f.write(" ".join(rest) + "\n")
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
import fcntl, json, os, socket, sys, time

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
# Record the launch argv — tests assert flags are replayed on resume.
# Temp + rename so a reader never sees the file torn mid-write.
_argv_tmp = os.environ["FAKE_PANE"] + ".argv.tmp"
open(_argv_tmp, "w").write("\n".join(sys.argv))
os.rename(_argv_tmp, os.environ["FAKE_PANE"] + ".argv")
# Record the pane env the adapter exported via tmux -e.
_env_tmp = os.environ["FAKE_PANE"] + ".env.tmp"
open(_env_tmp, "w").write(
    "CADENCE_ALIAS=%s\nCADENCE_STATE_DIR=%s\n" % (
        os.environ.get("CADENCE_ALIAS", ""),
        os.environ.get("CADENCE_STATE_DIR", "")))
os.rename(_env_tmp, os.environ["FAKE_PANE"] + ".env")
def awrite(path, text):
    # Atomic write — a capture-pane reader sees whole content or none.
    tmp = path + ".tmp"
    with open(tmp, "w") as f: f.write(text)
    os.rename(tmp, path)
def aappend(path, text):
    try: cur = open(path).read()
    except OSError: cur = ""
    awrite(path, cur + text)
def memory_rpc():
    # Test-only bridge: the lockholding provider process opens the real
    # daemon socket, so SO_PEERCRED and /proc ancestry see this pane rather
    # than the integration-test process.  The request/response files are
    # opt-in and scoped to this mock pane; production providers have no such
    # bridge.
    req_path = os.environ["FAKE_PANE"] + ".memory-rpc"
    try:
        raw = open(req_path).read()
    except FileNotFoundError:
        return
    try:
        request = json.loads(raw)
        sock_path = os.path.join(os.environ["CADENCE_STATE_DIR"], "cadence.sock")
        chunks = []
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(15)
            sock.connect(sock_path)
            sock.sendall((json.dumps(request, separators=(",", ":")) + "\n").encode())
            while True:
                chunk = sock.recv(65536)
                if not chunk:
                    break
                chunks.append(chunk)
                if b"\n" in chunk:
                    break
        response = json.loads(b"".join(chunks).split(b"\n", 1)[0].decode())
    except Exception as exc:
        response = {"ok": False, "error": {"kind": "internal", "message": str(exc)}}
    try:
        os.unlink(req_path)
    except FileNotFoundError:
        pass
    awrite(os.environ["FAKE_PANE"] + ".memory-rpc.response", json.dumps(response))
aappend(os.environ["FAKE_PANE"] + ".screen",
        "Mock Devin TUI [%s]\n" % sid +
        # An idle input line — the same shape the real TUI shows so the
        # screen probe recognizes an empty prompt.
        "❭ Ask Devin to build features, fix bugs, or work on your code\n")
while True:
    memory_rpc()
    inp = os.environ["FAKE_PANE"] + ".input"
    try:
        data = open(inp).read()
    except FileNotFoundError:
        data = ""
    if "<ENTER>" in data:
        text, rest = data.split("<ENTER>", 1)
        if os.path.exists(os.environ["FAKE_PANE"] + ".hold-enter"):
            # Enter swallowed: the marker is consumed but the draft
            # stays staged in the input line, unsubmitted.
            awrite(inp, text + rest)
        else:
            awrite(inp, rest)
            if text.strip():
                aappend(os.environ["FAKE_PANE"] + ".screen",
                        "> %s\nMOCK_REPLY: %s\n" % (text.strip(), text.strip()))
    if "<KEY:C-c>" in data:
        awrite(inp, "")
        aappend(os.environ["FAKE_PANE"] + ".screen", "^C interrupt\n")
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
    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    // Real env, under this mock's ENV_LOCK: the mock tmux runs as a fresh
    // child per call and finds its state only through inherited env.
    std::env::set_var("MOCK_TMUX_STATE", &tmux_state);
    test_env().set("CADENCE_TMUX_COMMAND", tmux.display().to_string());
    test_env().set(
        "CADENCE_DEVIN_COMMAND",
        format!("python3 {} {}", devin_py.display(), locks.display()),
    );
    test_env().set("CADENCE_DEVIN_LOCKS", locks.display().to_string());
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

    /// Ask a live mock Devin pane to issue one daemon RPC over the real
    /// Unix socket. The Python process owns the request connection, so the
    /// daemon sees its actual SO_PEERCRED pid and /proc ancestry.
    fn memory_rpc(
        &self,
        mock: &MockDevin,
        alias: &str,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, String> {
        let request_path = self.pane_file(mock, alias, "memory-rpc");
        let response_path = self.pane_file(mock, alias, "memory-rpc.response");
        let _ = std::fs::remove_file(&response_path);
        let request = json!({"method": method, "params": params});
        let temporary = request_path.with_extension("memory-rpc.tmp");
        std::fs::write(&temporary, request.to_string()).map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, &request_path).map_err(|e| e.to_string())?;

        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(text) = std::fs::read_to_string(&response_path) {
                let frame: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                if frame["ok"] == true {
                    return Ok(frame["result"].clone());
                }
                let message = frame
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("memory RPC refused")
                    .to_string();
                return Err(message);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "memory RPC from {alias} timed out; request={request_path:?}"
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl TestDaemon {
    fn mock_stub(&self) -> MockStub {
        install_mock_stub(self.dir.path())
    }

    /// Register a pty agent on the stub (test-double) profile.
    fn register_stub(&self, alias: &str, params: Value) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        self.rpc(
            "agent_register",
            json!({"alias": alias, "provider": "tui-stub",
                   "endpoint_kind": "pty", "cwd": cwd,
                   "params": params.to_string()}),
        )
        .unwrap();
    }

    /// The tmux-side state dir for a stub-pane agent session name.
    fn stub_pane_file(&self, mock: &MockStub, alias: &str, ext: &str) -> PathBuf {
        mock.dir
            .join("tmux-state")
            .join(socket_for(&self.state))
            .join(format!("{alias}.{ext}"))
    }
}

/// Temp-file + rename write: a concurrent `capture-pane` (or mock TUI
/// loop) sees whole content or none — no torn mid-write reads, so the
/// stall sampler only ever hashes a real screen.
fn atomic_write(path: PathBuf, contents: impl AsRef<[u8]>) {
    let tmp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name().unwrap().to_string_lossy()
    ));
    std::fs::write(&tmp, contents).unwrap();
    std::fs::rename(&tmp, &path).unwrap();
}

/// Panes legitimately outlive a daemon (shutdown detaches), so clean
/// any survivors ourselves by their recorded pane pids.
fn kill_mock_panes(dir: &Path) {
    if let Ok(socks) = std::fs::read_dir(dir.join("tmux-state")) {
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
}

impl Drop for MockDevin {
    fn drop(&mut self) {
        kill_mock_panes(&self.dir);
        test_env().remove("CADENCE_TMUX_COMMAND");
        test_env().remove("CADENCE_DEVIN_COMMAND");
        test_env().remove("CADENCE_DEVIN_LOCKS");
        std::env::remove_var("MOCK_TMUX_STATE");
    }
}

/// Fake `stub` TUI — the second profile's endpoint: different prompt
/// glyph (`»`), different placeholder, different session-lock dir. It
/// proves the adapter's gate/render/claim mechanics come from the
/// profile, not from Devin-shaped constants. Same contract as
/// MOCK_DEVIN_PY: flock `<locks>/<sid>.lock`, `-r` resumes.
const MOCK_STUB_PY: &str = r#"
import fcntl, os, sys, time

locks = sys.argv[1]
sid = sys.argv[sys.argv.index("-r") + 1] if "-r" in sys.argv else \
    "stub-session-%d" % os.getpid()
os.makedirs(locks, exist_ok=True)
lf = open(os.path.join(locks, sid + ".lock"), "a")
try:
    fcntl.flock(lf, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    print("session_locked: %s" % sid); sys.exit(1)
open(os.environ["FAKE_PANE"] + ".sid", "w").write(sid)
# The pane's own input-line glyph — the mock tmux renders staged text
# with it, so a staged draft reads as this TUI's prompt line.
open(os.environ["FAKE_PANE"] + ".glyph", "w").write("»")
def awrite(path, text):
    # Atomic write — a capture-pane reader sees whole content or none.
    tmp = path + ".tmp"
    with open(tmp, "w") as f: f.write(text)
    os.rename(tmp, path)
def aappend(path, text):
    try: cur = open(path).read()
    except OSError: cur = ""
    awrite(path, cur + text)
aappend(os.environ["FAKE_PANE"] + ".screen",
        "Mock Stub TUI [%s]\n" % sid +
        # The stub profile's empty-prompt signature.
        "» stub ready\n")
while True:
    inp = os.environ["FAKE_PANE"] + ".input"
    try:
        data = open(inp).read()
    except FileNotFoundError:
        data = ""
    if "<ENTER>" in data:
        text, rest = data.split("<ENTER>", 1)
        awrite(inp, rest)
        if text.strip():
            aappend(os.environ["FAKE_PANE"] + ".screen",
                    "> %s\nSTUB_REPLY: %s\n" % (text.strip(), text.strip()))
    time.sleep(0.05)
"#;

struct MockStub {
    _guard: std::sync::MutexGuard<'static, ()>,
    dir: PathBuf,
    locks: PathBuf,
}

/// Install the mock tmux/stub pair — the same private-tmux harness as
/// `install_mock_devin`, pointing the adapter at the stub profile's env
/// overrides instead.
fn install_mock_stub(dir: &Path) -> MockStub {
    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let locks = dir.join("stub-locks");
    let tmux_state = dir.join("tmux-state");
    std::fs::create_dir_all(&locks).unwrap();
    std::fs::create_dir_all(&tmux_state).unwrap();
    let tmux = dir.join("tmux");
    let stub_py = dir.join("mock-stub.py");
    std::fs::write(&tmux, MOCK_TMUX_PY).unwrap();
    std::fs::write(&stub_py, MOCK_STUB_PY).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Real env, under this mock's ENV_LOCK: the mock tmux runs as a fresh
    // child per call and finds its state only through inherited env.
    std::env::set_var("MOCK_TMUX_STATE", &tmux_state);
    test_env().set("CADENCE_TMUX_COMMAND", tmux.display().to_string());
    test_env().set(
        "CADENCE_STUB_COMMAND",
        format!("python3 {} {}", stub_py.display(), locks.display()),
    );
    test_env().set("CADENCE_STUB_LOCKS", locks.display().to_string());
    MockStub {
        _guard: guard,
        dir: dir.to_path_buf(),
        locks,
    }
}

impl Drop for MockStub {
    fn drop(&mut self) {
        kill_mock_panes(&self.dir);
        test_env().remove("CADENCE_TMUX_COMMAND");
        test_env().remove("CADENCE_STUB_COMMAND");
        test_env().remove("CADENCE_STUB_LOCKS");
        std::env::remove_var("MOCK_TMUX_STATE");
    }
}

/// Fake `claude` TUI — the Claude profile's endpoint. Instead of a
/// session lock it publishes the real registry's shape:
/// `<sessions>/<pid>.json` = `{"pid", "sessionId", "cwd", "procStart"}`.
/// `--session-id`/`--resume` arrive as argv (the profile appends them
/// to the verbatim override); MOCK_CLAUDE_SWAP makes the registry
/// claim a different session than asked — a changed-owner fence.
/// `$FAKE_PANE` (set by the mock tmux) points at the session state.
const MOCK_CLAUDE_TUI_PY: &str = r#"
import json, os, sys, time

sessions = sys.argv[1]
if "--resume" in sys.argv:
    sid = sys.argv[sys.argv.index("--resume") + 1]
elif "--session-id" in sys.argv:
    sid = sys.argv[sys.argv.index("--session-id") + 1]
else:
    sid = "mock-claude-%d" % os.getpid()
os.makedirs(sessions, exist_ok=True)
# Record the launch argv so tests can assert the profile's flags —
# temp + rename so a reader never sees the file torn mid-write.
_argv_tmp = os.environ["FAKE_PANE"] + ".argv.tmp"
open(_argv_tmp, "w").write(" ".join(sys.argv[1:]))
os.rename(_argv_tmp, os.environ["FAKE_PANE"] + ".argv")
pid = os.getpid()
# /proc/self/stat field 22 — what the real registry's procStart is.
try:
    stat = open("/proc/self/stat").read()
    proc_start = stat[stat.rindex(")") + 1:].split()[19]
except Exception:
    proc_start = None
entry = {"pid": pid, "sessionId": sid, "cwd": os.getcwd(),
         "procStart": proc_start, "kind": "interactive"}
if os.environ.get("MOCK_CLAUDE_SWAP"):
    entry["sessionId"] = "swapped-" + sid
# MOCK_CLAUDE_NO_REGISTRY keeps the pane alive but never publishes the
# session — the adapter's open wait then runs to its deadline, the
# transient-proof-timeout shape a Claude resume must survive.
if not os.environ.get("MOCK_CLAUDE_NO_REGISTRY"):
    open(os.path.join(sessions, "%d.json" % pid), "w").write(json.dumps(entry))
# Record the pane env the adapter exported via tmux -e.
_env_tmp = os.environ["FAKE_PANE"] + ".env.tmp"
open(_env_tmp, "w").write(
    "CADENCE_ALIAS=%s\nCADENCE_STATE_DIR=%s\n" % (
        os.environ.get("CADENCE_ALIAS", ""),
        os.environ.get("CADENCE_STATE_DIR", "")))
os.rename(_env_tmp, os.environ["FAKE_PANE"] + ".env")
# The pane's own input-line glyph + a boxed empty prompt — the shape
# the real TUI shows so the screen probe recognizes idle.
open(os.environ["FAKE_PANE"] + ".glyph", "w").write("❯")
def awrite(path, text):
    # Atomic write — a capture-pane reader sees whole content or none.
    tmp = path + ".tmp"
    with open(tmp, "w") as f: f.write(text)
    os.rename(tmp, path)
def aappend(path, text):
    try: cur = open(path).read()
    except OSError: cur = ""
    awrite(path, cur + text)
aappend(os.environ["FAKE_PANE"] + ".screen",
        "Mock Claude TUI [%s]\n" % sid +
        "  [Opus] mock-mode on\n" +
        "─" * 40 + "\n❯ \n" + "─" * 40 + "\n")
while True:
    inp = os.environ["FAKE_PANE"] + ".input"
    try:
        data = open(inp).read()
    except FileNotFoundError:
        data = ""
    if "<ENTER>" in data:
        text, rest = data.split("<ENTER>", 1)
        awrite(inp, rest)
        if text.strip():
            # The submitted line echoes into the transcript and the
            # box re-renders empty below it.
            aappend(os.environ["FAKE_PANE"] + ".screen",
                    "> %s\nMOCK_REPLY: %s\n" % (text.strip(), text.strip()) +
                    "─" * 40 + "\n❯ \n" + "─" * 40 + "\n")
    if "<KEY:C-c>" in data:
        awrite(inp, "")
        aappend(os.environ["FAKE_PANE"] + ".screen", "^C interrupt\n")
    time.sleep(0.05)
"#;

struct MockClaudeTui {
    _guard: std::sync::MutexGuard<'static, ()>,
    dir: PathBuf,
    sessions: PathBuf,
}

/// Install the mock tmux/claude pair — the same private-tmux harness as
/// `install_mock_devin`, pointing the adapter at the claude profile's
/// env overrides instead.
fn install_mock_claude_tui(dir: &Path) -> MockClaudeTui {
    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sessions = dir.join("claude-sessions");
    let tmux_state = dir.join("tmux-state");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::create_dir_all(&tmux_state).unwrap();
    let tmux = dir.join("tmux");
    let claude_py = dir.join("mock-claude.py");
    std::fs::write(&tmux, MOCK_TMUX_PY).unwrap();
    std::fs::write(&claude_py, MOCK_CLAUDE_TUI_PY).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Real env, under this mock's ENV_LOCK: the mock tmux runs as a fresh
    // child per call and finds its state only through inherited env.
    std::env::set_var("MOCK_TMUX_STATE", &tmux_state);
    test_env().set("CADENCE_TMUX_COMMAND", tmux.display().to_string());
    test_env().set(
        "CADENCE_CLAUDE_TUI_COMMAND",
        format!("python3 {} {}", claude_py.display(), sessions.display()),
    );
    test_env().set("CADENCE_CLAUDE_SESSIONS", sessions.display().to_string());
    MockClaudeTui {
        _guard: guard,
        dir: dir.to_path_buf(),
        sessions,
    }
}

impl TestDaemon {
    fn mock_claude_tui(&self) -> MockClaudeTui {
        install_mock_claude_tui(self.dir.path())
    }

    /// Register a pty agent on the claude profile.
    fn register_claude_pty(&self, alias: &str, params: Value) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        self.rpc(
            "agent_register",
            json!({"alias": alias, "provider": "claude",
                   "endpoint_kind": "pty", "cwd": cwd,
                   "params": params.to_string()}),
        )
        .unwrap();
    }

    /// The tmux-side state dir for a claude-pane agent session name.
    fn claude_pane_file(&self, mock: &MockClaudeTui, alias: &str, ext: &str) -> PathBuf {
        mock.dir
            .join("tmux-state")
            .join(socket_for(&self.state))
            .join(format!("{alias}.{ext}"))
    }
}

impl Drop for MockClaudeTui {
    fn drop(&mut self) {
        kill_mock_panes(&self.dir);
        test_env().remove("CADENCE_TMUX_COMMAND");
        test_env().remove("CADENCE_CLAUDE_TUI_COMMAND");
        test_env().remove("CADENCE_CLAUDE_SESSIONS");
        std::env::remove_var("MOCK_CLAUDE_SWAP");
        std::env::remove_var("MOCK_CLAUDE_NO_REGISTRY");
        std::env::remove_var("MOCK_TMUX_STATE");
    }
}

/// The live entries a mock-claude sessions dir holds: `(pid, sessionId)`.
fn claude_sessions(dir: &Path) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if let Ok(v) = serde_json::from_str::<Value>(
                &std::fs::read_to_string(e.path()).unwrap_or_default(),
            ) {
                out.push((
                    v["pid"].as_u64().unwrap_or(0) as u32,
                    v["sessionId"].as_str().unwrap_or_default().to_string(),
                ));
            }
        }
    }
    out
}

/// Fake `cursor-agent` TUI — the Cursor profile's endpoint. The real
/// TUI holds an open fd on `~/.cursor/chats/<hash>/<chat>/store.db`
/// for the session's whole life, so the mock opens the same file and
/// keeps it — the profile's ownership scan finds it under
/// `<chats>/mockhash/<chat>/store.db`. `create-chat` mints the id a
/// fresh launch resumes; `--resume` arrives as argv (the profile
/// appends it to the verbatim override); MOCK_CURSOR_SWAP makes the
/// pane open a different chat than asked — a changed-owner fence.
/// `$FAKE_PANE` (set by the mock tmux) points at the session state.
const MOCK_CURSOR_TUI_PY: &str = r#"
import os, sys, time, uuid

chats = sys.argv[1]
if "create-chat" in sys.argv:
    print(uuid.uuid4()); sys.exit(0)
sid = sys.argv[sys.argv.index("--resume") + 1] if "--resume" in sys.argv \
    else "missing-resume"
if os.environ.get("MOCK_CURSOR_SWAP"):
    sid = "swapped-" + sid
# A chat named by MOCK_CURSOR_DIE_ON is unresumable — the real TUI
# exits on a deleted/foreign chat, so the mock does too.
if sid == os.environ.get("MOCK_CURSOR_DIE_ON"):
    sys.exit(1)
chat_dir = os.path.join(chats, "mockhash", sid)
os.makedirs(chat_dir, exist_ok=True)
# The real TUI holds an fd on the chat's store.db for its whole life —
# the profile's ownership proof scans /proc fds for exactly this.
db = open(os.path.join(chat_dir, "store.db"), "a")
# Record the launch argv so tests can assert the profile's flags —
# temp + rename so a reader never sees the file torn mid-write.
_argv_tmp = os.environ["FAKE_PANE"] + ".argv.tmp"
open(_argv_tmp, "w").write(" ".join(sys.argv[1:]))
os.rename(_argv_tmp, os.environ["FAKE_PANE"] + ".argv")
# Record the pane env the adapter exported via tmux -e.
_env_tmp = os.environ["FAKE_PANE"] + ".env.tmp"
open(_env_tmp, "w").write(
    "CADENCE_ALIAS=%s\nCADENCE_STATE_DIR=%s\n" % (
        os.environ.get("CADENCE_ALIAS", ""),
        os.environ.get("CADENCE_STATE_DIR", "")))
os.rename(_env_tmp, os.environ["FAKE_PANE"] + ".env")
# The pane's own input-line glyph + the idle frame — the shape the real
# TUI shows so the screen probe recognizes idle.
open(os.environ["FAKE_PANE"] + ".glyph", "w").write("→")
with open(os.environ["FAKE_PANE"] + ".screen", "a") as f:
    f.write("Mock Cursor TUI [%s]\n" % sid)
    f.write("  → Plan, search, build anything\n")
    f.write("  Cursor Grok 4.6 High\n  /mock · main\n")
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
                f.write("  %s\nMOCK_REPLY: %s\n" % (text.strip(), text.strip()))
                # The submitted line echoes into the transcript and the
                # input's watermark flips to the follow-up form.
                f.write("  → Add a follow-up\n")
                f.write("  Cursor Grok 4.6 High\n  /mock · main\n")
    if "<KEY:C-c>" in data:
        open(inp, "w").write("")
        with open(os.environ["FAKE_PANE"] + ".screen", "a") as f:
            f.write("^C interrupt\n")
    time.sleep(0.05)
"#;

struct MockCursorTui {
    _guard: std::sync::MutexGuard<'static, ()>,
    dir: PathBuf,
    chats: PathBuf,
    /// The owning daemon's state dir, when the mock was installed
    /// through `TestDaemon::mock_cursor_tui`. Drop shuts the daemon
    /// down BEFORE clearing the env overrides: an in-process daemon
    /// that outlives the mock could still rebuild a profile in its
    /// teardown window, and a profile built without
    /// `CADENCE_CURSOR_CHATS` resolves the real `~/.cursor` — scans
    /// the user's real chats, and would merge `Shell(cadence)` into
    /// the real `cli-config.json`.
    state: Option<PathBuf>,
}

/// Install the mock tmux/cursor pair — the same private-tmux harness
/// as `install_mock_devin`, pointing the adapter at the cursor
/// profile's env overrides instead.
fn install_mock_cursor_tui(dir: &Path) -> MockCursorTui {
    install_mock_cursor_tui_inner(dir, None)
}

fn install_mock_cursor_tui_inner(dir: &Path, state: Option<PathBuf>) -> MockCursorTui {
    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let chats = dir.join("cursor-chats");
    let tmux_state = dir.join("tmux-state");
    std::fs::create_dir_all(&chats).unwrap();
    std::fs::create_dir_all(&tmux_state).unwrap();
    let tmux = dir.join("tmux");
    let cursor_py = dir.join("mock-cursor.py");
    std::fs::write(&tmux, MOCK_TMUX_PY).unwrap();
    std::fs::write(&cursor_py, MOCK_CURSOR_TUI_PY).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    // A `cursor-agent`-named symlink onto python3: the pane's argv[0]
    // then names the real binary, so the profile's `--resume` argv
    // proof (not only the store.db fd) is exercised in integration.
    let out = std::process::Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    let python = String::from_utf8(out.stdout).unwrap().trim().to_string();
    let cursor_bin = dir.join("cursor-agent");
    std::os::unix::fs::symlink(&python, &cursor_bin).unwrap();
    // Real env, under this mock's ENV_LOCK: the mock tmux runs as a fresh
    // child per call and finds its state only through inherited env.
    std::env::set_var("MOCK_TMUX_STATE", &tmux_state);
    test_env().set("CADENCE_TMUX_COMMAND", tmux.display().to_string());
    test_env().set(
        "CADENCE_CURSOR_COMMAND",
        format!(
            "{} {} {}",
            cursor_bin.display(),
            cursor_py.display(),
            chats.display()
        ),
    );
    test_env().set("CADENCE_CURSOR_CHATS", chats.display().to_string());
    // A swap/die-on set by an earlier test must not leak into this
    // install.
    std::env::remove_var("MOCK_CURSOR_SWAP");
    std::env::remove_var("MOCK_CURSOR_DIE_ON");
    MockCursorTui {
        _guard: guard,
        dir: dir.to_path_buf(),
        chats,
        state,
    }
}

impl TestDaemon {
    fn mock_cursor_tui(&self) -> MockCursorTui {
        install_mock_cursor_tui_inner(self.dir.path(), Some(self.state.clone()))
    }

    /// Register a pty agent on the cursor profile.
    fn register_cursor_pty(&self, alias: &str, params: Value) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        self.rpc(
            "agent_register",
            json!({"alias": alias, "provider": "cursor",
                   "endpoint_kind": "pty", "cwd": cwd,
                   "params": params.to_string()}),
        )
        .unwrap();
    }

    /// The tmux-side state dir for a cursor-pane agent session name.
    fn cursor_pane_file(&self, mock: &MockCursorTui, alias: &str, ext: &str) -> PathBuf {
        mock.dir
            .join("tmux-state")
            .join(socket_for(&self.state))
            .join(format!("{alias}.{ext}"))
    }
}

impl Drop for MockCursorTui {
    fn drop(&mut self) {
        // The daemon goes first: while it lives, a rebuilt profile
        // must still see the overrides. `TestDaemon::drop` re-runs
        // shutdown idempotently and only joins the thread.
        if let Some(state) = &self.state {
            let _ = client::rpc(state, "shutdown", json!({}));
            let deadline = Instant::now() + Duration::from_secs(10);
            while client::rpc(state, "health", json!({})).is_ok() {
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        kill_mock_panes(&self.dir);
        test_env().remove("CADENCE_TMUX_COMMAND");
        test_env().remove("CADENCE_CURSOR_COMMAND");
        test_env().remove("CADENCE_CURSOR_CHATS");
        std::env::remove_var("MOCK_CURSOR_SWAP");
        std::env::remove_var("MOCK_CURSOR_DIE_ON");
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

/// CAD-18: `params.permission_mode` lands on the agent row at register
/// and is replayed into the pane argv on every open — the fresh launch
/// gets `--permission-mode <mode>`, the resume gets it plus `-r <sid>`.
#[test]
fn devin_permission_mode_persisted_and_replayed() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "agent_register",
        json!({"alias": "dv1", "provider": "devin", "endpoint_kind": "pty",
               "cwd": cwd,
               "params": json!({"permission_mode": "smart"}).to_string()}),
    )
    .unwrap();
    let agent = d.wait_agent("dv1", "idle", 20);
    assert_eq!(agent["params"]["permission_mode"], "smart", "{agent}");
    let sid = agent["thread_id"].as_str().unwrap().to_string();
    // The mock devin records its launch argv per pane life. Probe-idle
    // is the cause ordered after the dump: the TUI paint that makes the
    // probe read idle happens after the mock writes .argv.
    let argv_file = d.pane_file(&mock, "dv1", "argv");
    wait_probe_idle(&d, "dv1", 15);
    let argv1 = std::fs::read_to_string(&argv_file).unwrap();
    assert!(argv1.contains("--permission-mode\nsmart"), "{argv1}");
    assert!(
        !argv1.contains("\n-r\n"),
        "fresh launch must not resume: {argv1}"
    );
    // A stop+resume respawns the pane with `-r <sid>` — the mode must
    // be replayed verbatim alongside it.
    d.rpc("agent_stop", json!({"alias": "dv1"})).unwrap();
    d.wait_agent("dv1", "stopped", 15);
    d.rpc("agent_resume", json!({"alias": "dv1"})).unwrap();
    let agent = d.wait_agent("dv1", "idle", 20);
    // Same cause on the new pane life — the file still holds the first
    // launch's argv until the respawned mock rewrites it.
    wait_probe_idle(&d, "dv1", 15);
    let argv2 = std::fs::read_to_string(&argv_file).unwrap();
    assert!(argv2.contains(&format!("-r\n{sid}")), "{argv2}");
    assert!(argv2.contains("--permission-mode\nsmart"), "{argv2}");
    assert_eq!(agent["params"]["permission_mode"], "smart", "{agent}");
}

/// CAD-18: the four-value vocabulary is enforced at `agent_register`
/// (not just the CLI), and `agent set` cannot patch it live.
#[test]
fn devin_permission_mode_validated_at_register_and_not_settable() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    let cwd = d.dir.path().to_str().unwrap().to_string();
    for bad in ["bogus", "manual", "bypass", "acceptEdits"] {
        let err = d
            .rpc(
                "agent_register",
                json!({"alias": "bad", "provider": "devin", "endpoint_kind": "pty",
                       "cwd": cwd,
                       "params": json!({"permission_mode": bad}).to_string()}),
            )
            .unwrap_err();
        let msg = err.to_string();
        for accepted in ["auto", "accept-edits", "smart", "dangerous"] {
            assert!(
                msg.contains(accepted),
                "'{bad}' error missing '{accepted}': {msg}"
            );
        }
    }
    // The same check does not fire for other providers' params.
    let err = d.rpc(
        "agent_register",
        json!({"alias": "cl1", "provider": "claude", "endpoint_kind": "managed",
               "cwd": cwd,
               "params": json!({"permission_mode": "anything-goes"}).to_string()}),
    );
    assert!(err.is_ok(), "claude params must pass through: {err:?}");
    d.rpc("agent_stop", json!({"alias": "cl1"})).unwrap();
    // agent set: launch params are not live-settable.
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    let err = d
        .rpc(
            "agent_set",
            json!({"alias": "dv1", "patch": {"permission_mode": "smart"}}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("not live-settable"), "{err}");
    // auto_ready stays the one live-settable key — the patch still works.
    d.rpc(
        "agent_set",
        json!({"alias": "dv1", "patch": {"auto_ready": "verified"}}),
    )
    .unwrap();
}

/// CAD-18, CLI end-to-end: `--bypass` persists `dangerous` on the agent
/// row and shows in the launch summary; an invalid `--permission-mode`
/// is rejected by the verb before any registration happens.
#[test]
fn cli_devin_permission_mode_flag_and_bypass() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    let bin = env!("CARGO_BIN_EXE_cadence");
    // --bypass → stored as permission_mode=dangerous, echoed in the
    // launch summary, and replayed into the pane argv.
    let out = std::process::Command::new(bin)
        .args(["--state-dir"])
        .arg(&d.state)
        .args([
            "devin",
            "--bypass",
            "--detach",
            "--no-bootstrap",
            "--alias",
            "dv9",
            "--cwd",
        ])
        .arg(d.dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let summary: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(summary["permission_mode"], "dangerous", "{summary}");
    let agent = d.wait_agent("dv9", "idle", 20);
    assert_eq!(agent["params"]["permission_mode"], "dangerous", "{agent}");
    // An invalid mode fails before agent_register — the rejection names
    // the four accepted values and leaves no agent behind.
    let out = std::process::Command::new(bin)
        .args(["--state-dir"])
        .arg(&d.state)
        .args(["devin", "--permission-mode", "bogus", "--detach"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    for accepted in ["auto", "accept-edits", "smart", "dangerous"] {
        assert!(err.contains(accepted), "missing '{accepted}': {err}");
    }
    let list = d.rpc("agent_list", json!({})).unwrap();
    let aliases: Vec<&str> = list["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["alias"].as_str())
        .collect();
    assert_eq!(aliases, ["dv9"], "{aliases:?}");
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
    // This kill is submitted-pane loss, not a session-lock refusal, so
    // the fence must keep that account plus the inspect guidance. A
    // lock substring is not an alternate success for this path.
    let error = agent["error"].as_str().unwrap_or("");
    assert!(error.contains("endpoint lost after submission"), "{agent}");
    assert!(error.contains("does not prove"), "{error}");
    assert!(!error.contains("then `cadence agent resume"), "{error}");
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
    atomic_write(d.pane_file(&mock, "dv1", "mode"), "1");
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
    // Since CAD-245 a routed notice pastes into an idle pane without an
    // operator claim (pty_routed_notice_delivers_idle_without_claim pins
    // the gate). Claiming here as well raced that paste: the claim's
    // probe could see the notice's text still in the input line and
    // refuse (CAD-266). A routed notification is fire-and-forget on a
    // pty endpoint: once the paste succeeds the message completes with
    // a delivery receipt — the PM is not expected to `message result` it.
    let done = d.wait_message("pm", routed["id"].as_str().unwrap(), &["completed"], 15);
    assert_eq!(
        done["result"]["via"].as_str(),
        Some("pty_deliver"),
        "{done}"
    );
    assert!(
        done["result"]["turn_id"]
            .as_str()
            .unwrap_or_default()
            .starts_with("pty-"),
        "{done}"
    );
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

#[test]
fn pty_pane_env_exports_identity() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    // tmux -e exports land in the pane process env: the mock TUI
    // records CADENCE_* so `cadence self` can identify the agent. The
    // env dump precedes the TUI paint — probe-idle orders after it.
    wait_probe_idle(&d, "dv1", 15);
    let env = std::fs::read_to_string(d.pane_file(&_mock, "dv1", "env")).unwrap();
    assert!(env.contains("CADENCE_ALIAS=dv1"), "{env}");
    assert!(
        env.contains(&format!("CADENCE_STATE_DIR={}", d.state.display())),
        "{env}"
    );
}

#[test]
fn cadence_self_reports_running_token() {
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

    // Inside a cadence pane (CADENCE_ALIAS set): alias + report token.
    let bin = env!("CARGO_BIN_EXE_cadence");
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .arg("self")
        .env("CADENCE_ALIAS", "dv1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["alias"], "dv1");
    assert_eq!(v["running"][0]["id"], "m1");
    assert_eq!(v["running"][0]["turn_id"].as_str().unwrap(), token);

    // Outside a cadence pane the command fails with a clear error.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .arg("self")
        .env_remove("CADENCE_ALIAS")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not inside a cadence-owned pane"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn message_send_ready_claims_then_sends() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.register("w1");
    d.wait_agent("dv1", "idle", 20);
    d.wait_agent("w1", "idle", 10);
    let bin = env!("CARGO_BIN_EXE_cadence");

    // pty: --ready IS the operator claim — no separate agent_ready call.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["message", "send", "dv1", "--text", "hi"])
        .args(["--message", "m9", "--ready"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let token = pty_token(&d, "dv1", "m9");
    assert!(token.starts_with("pty-"), "{token}");

    // non-pty: the claim is a silent no-op, the send proceeds normally.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["message", "send", "w1", "--text", "hi"])
        .args(["--message", "m10", "--ready"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_message("w1", "m10", &["completed"], 15);
}

/// `git init` + one empty commit so `worktree add -b` has a HEAD.
fn git_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(&args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
    }
}

/// `git status --porcelain` — empty means the repo is byte-identical
/// to its index+HEAD (audit N8's launch-purity check).
fn git_porcelain(repo: &Path) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(out.status.success(), "git status: {:?}", out.stderr);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn pty_pane_gets_default_options() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    // Pane defaults are scoped to the private -L socket's server.
    let path = d.pane_file(&mock, "setopt", "log");
    let deadline = Instant::now() + Duration::from_secs(5);
    let log = loop {
        if let Ok(log) = std::fs::read_to_string(&path) {
            if log.contains("pane-border-format") {
                break log;
            }
        }
        assert!(Instant::now() < deadline, "no set-option calls recorded");
        thread::sleep(Duration::from_millis(50));
    };
    for want in [
        "-g mouse on",
        "-g set-clipboard on",
        "-g status-left-length 40",
        "-gw pane-border-status top",
        "-gw pane-border-format  #{session_name} ",
    ] {
        assert!(log.contains(want), "missing `{want}` in:\n{log}");
    }
}

#[test]
fn join_bootstrap_briefs_and_queues() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    // The PM's cwd is a git repo so the briefing exercises .gitignore.
    let pm_repo = d.dir.path().join("pmrepo");
    git_repo(&pm_repo);
    d.rpc(
        "agent_register",
        json!({"alias": "pm", "provider": "fake", "endpoint_kind": "fake",
               "cwd": pm_repo}),
    )
    .unwrap();
    d.wait_agent("pm", "idle", 10);

    let bin = env!("CARGO_BIN_EXE_cadence");
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["join", "pm", "devin", "--alias", "w-join", "--detach"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_agent("w-join", "idle", 20);

    // Briefing persisted under the state dir's briefings/<pm>/ — the
    // PM's repo itself is left byte-identical (audit N8).
    let briefing = d
        .state
        .join("briefings")
        .join("pm")
        .join("BRIEFING-w-join.md");
    let text = std::fs::read_to_string(&briefing).unwrap();
    assert!(text.contains("w-join") && text.contains("pm"), "{text}");
    assert_eq!(git_porcelain(&pm_repo), "", "launch touched the repo");

    // The durable bootstrap message sits queued behind the ready gate —
    // no bypass — and its body is one pty-safe line naming alias, PM
    // and the briefing path.
    let m = d.wait_message("w-join", "bootstrap-w-join", &["queued", "submitting"], 15);
    assert_eq!(m["source"], "bootstrap");
    let body = m["body"].as_str().unwrap();
    assert!(!body.chars().any(|c| (c as u32) < 32 || c as u32 == 127));
    for want in ["w-join", "pm", briefing.to_str().unwrap(), "cadence self"] {
        assert!(body.contains(want), "bootstrap body missing {want}: {body}");
    }
    thread::sleep(Duration::from_millis(400));
    let mid = d.message_state("w-join", "bootstrap-w-join");
    assert!(matches!(mid.as_str(), "queued" | "submitting"), "{mid}");
    // A claim releases it — gated like any send, never bypassed.
    d.rpc("agent_ready", json!({"alias": "w-join"})).unwrap();
    pty_token(&d, "w-join", "bootstrap-w-join");

    // --no-bootstrap: no message, no briefing file.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "join",
            "pm",
            "devin",
            "--alias",
            "w-nb",
            "--detach",
            "--no-bootstrap",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_agent("w-nb", "idle", 20);
    let show = d.rpc("agent_show", json!({"alias": "w-nb"})).unwrap();
    assert!(
        !show["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["source"] == "bootstrap"),
        "{}",
        show["messages"]
    );
    assert!(!d.state.join("briefings/pm/BRIEFING-w-nb.md").exists());
}

#[test]
fn join_bootstrap_runs_on_fake_worker() {
    let d = TestDaemon::start();
    d.register("pm");
    d.wait_agent("pm", "idle", 10);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["join", "pm", "fake", "--alias", "w-fake", "--detach"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_agent("w-fake", "idle", 15);
    // No gate on a fake endpoint — the bootstrap completes its turn.
    let m = d.wait_message("w-fake", "bootstrap-w-fake", &["completed"], 15);
    assert_eq!(m["source"], "bootstrap");
    // The briefing still lands under the PM's briefings/ dir — inside
    // the state dir, never the PM's cwd.
    assert!(d.state.join("briefings/pm/BRIEFING-w-fake.md").exists());
}

#[test]
fn devin_worktree_isolates_checkout() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    let repo = d.dir.path().join("repo");
    git_repo(&repo);
    let bin = env!("CARGO_BIN_EXE_cadence");

    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "devin",
            "--worktree",
            "feat-a",
            "--cwd",
            repo.to_str().unwrap(),
            "--alias",
            "w-wt",
            "--detach",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let agent = d.wait_agent("w-wt", "idle", 20);
    let wt = repo.join(".cadence").join("wt").join("feat-a");
    assert_eq!(agent["cwd"].as_str().unwrap(), wt.to_str().unwrap());
    // Branch created, .cadence/ ignored.
    let branches = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["branch", "--list", "cadence/feat-a"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&branches.stdout).contains("cadence/feat-a"));
    let gitignore = std::fs::read_to_string(repo.join(".gitignore")).unwrap();
    assert!(gitignore.lines().any(|l| l.trim() == ".cadence/"));

    // Same alias + --worktree: clean refusal before touching anything.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "devin",
            "--worktree",
            "feat-b",
            "--cwd",
            repo.to_str().unwrap(),
            "--alias",
            "w-wt",
            "--detach",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("already registered"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Occupied worktree dir with a fresh alias: reuse hint.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "devin",
            "--worktree",
            "feat-a",
            "--cwd",
            repo.to_str().unwrap(),
            "--alias",
            "w-wt2",
            "--detach",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("already exists"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Not a git repository: clean rejection, nothing created.
    let plain = d.dir.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "devin",
            "--worktree",
            "x",
            "--cwd",
            plain.to_str().unwrap(),
            "--alias",
            "w-ng",
            "--detach",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("git repository"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn agent_remove_and_gc_sweep() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.register("w-old");
    d.register("w-recent");
    d.wait_agent("dv1", "idle", 20);
    d.wait_agent("w-old", "idle", 10);
    d.wait_agent("w-recent", "idle", 10);

    // Refused while the endpoint is live — suggests agent stop.
    let err = d.rpc("agent_remove", json!({"alias": "dv1"})).unwrap_err();
    assert!(err.to_string().contains("agent stop"), "{err}");

    // Fake agents have no endpoint but are actor-owned while running.
    let err = d
        .rpc("agent_remove", json!({"alias": "w-old"}))
        .unwrap_err();
    assert!(err.to_string().contains("agent stop"), "{err}");

    // A stopped fake is not dead — fake never dies — but it is
    // resumable: stopped with a saved thread and no fence.
    d.rpc("agent_stop", json!({"alias": "w-old"})).unwrap();
    d.rpc("agent_stop", json!({"alias": "w-recent"})).unwrap();
    d.wait_agent("w-old", "stopped", 15);
    d.wait_agent("w-recent", "stopped", 15);
    let list = d.rpc("agent_list", json!({})).unwrap();
    let w_old = list["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"] == "w-old")
        .unwrap();
    assert_eq!(w_old["dead"], false, "{w_old}");
    assert_eq!(w_old["resumable"], true, "{w_old}");
    assert!(w_old["endpoint"].is_null());
    let live = list["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"] == "dv1")
        .unwrap();
    assert_eq!(live["dead"], false, "{live}");
    assert_eq!(live["resumable"], false, "{live}");

    // Explicit remove drops the row and its history.
    d.rpc("agent_remove", json!({"alias": "w-old"})).unwrap();
    assert!(d.rpc("agent_show", json!({"alias": "w-old"})).is_err());

    // gc --older-than filters by `updated` age: both were just stopped.
    let swept = d.rpc("agent_gc", json!({"older_than": 3600.0})).unwrap();
    assert_eq!(swept["removed"].as_array().unwrap().len(), 0);
    // Default sweep removes every dead stopped/attention agent.
    let swept = d.rpc("agent_gc", json!({})).unwrap();
    let removed: Vec<&str> = swept["removed"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(removed, vec!["w-recent"], "{swept}");
    // The live pty agent was untouched.
    d.rpc("agent_show", json!({"alias": "dv1"})).unwrap();
}

#[test]
fn fenced_agent_resume_hint() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    let agent = d.wait_agent("dv1", "idle", 20);
    let native = agent["thread_id"].as_str().unwrap().to_string();
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    pty_token(&d, "dv1", "m1");
    // Fence it: pane dies with a submitted message in flight.
    let pid: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    unsafe { libc::killpg(pid, libc::SIGKILL) };
    let agent = d.wait_agent("dv1", "attention", 20);
    assert!(agent["endpoint"].is_null());

    // `devin -r <slug>` on the fenced agent must not print attach/ready
    // steps, and must not hand out an unfence-then-resume command chain.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["devin", "-r", &native, "--detach"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["state"], "attention", "{v}");
    assert!(
        v["next"]["inspect"]
            .as_str()
            .unwrap_or_default()
            .contains("does not prove"),
        "{v}"
    );
    assert!(
        v["next"]["decision"]
            .as_str()
            .unwrap_or_default()
            .contains("--no-resume"),
        "{v}"
    );
    assert!(v["next"]["unfence"].is_null(), "{v}");
    assert!(v["next"]["resume"].is_null(), "{v}");
    assert!(v["next"]["attach"].is_null(), "{v}");
    // `agent show` keeps the message's unknown account, not a second-resume command.
    let agent = d.rpc("agent_show", json!({"alias": "dv1"})).unwrap()["agent"].clone();
    let error = agent["error"].as_str().unwrap();
    assert!(error.contains("endpoint lost after submission"), "{error}");
    assert!(error.contains("does not prove"), "{error}");
    assert!(!error.contains("then `cadence agent resume"), "{error}");
}

/// Spawn the real `cadence` binary under a scratch HOME (skill install
/// targets `$HOME` directly — no daemon involved).
fn cadence_at(home: &Path, state: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .env("HOME", home)
        // A `daemon restart` child daemon is a separate process: it
        // gets this test's mock commands as its own env, and only it.
        .envs(test_env().vars())
        .output()
        .unwrap()
}

#[test]
fn skill_install_links_and_is_idempotent() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();

    let out = cadence_at(home.path(), state.path(), &["skill", "install"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let file = home.path().join(".agents/skills/cadence/SKILL.md");
    assert_eq!(v["installed"].as_str().unwrap(), file.to_str().unwrap());
    assert_eq!(v["linked"].as_array().unwrap().len(), 3);
    assert!(v["skipped"].as_array().unwrap().is_empty());
    // The installed file is byte-identical to the vendored copy.
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        cadence_agent::skill::SKILL_MD
    );
    for parent in [".claude/skills", ".cursor/skills", ".copilot/skills"] {
        let link = home.path().join(parent).join("cadence");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            home.path().join(".agents/skills/cadence"),
            "{parent}"
        );
    }

    // Second run: same result, links re-verified not duplicated.
    let out = cadence_at(home.path(), state.path(), &["skill", "install"]);
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["linked"].as_array().unwrap().len(), 0, "{v}");
    let status: Value =
        serde_json::from_slice(&cadence_at(home.path(), state.path(), &["skill", "status"]).stdout)
            .unwrap();
    assert_eq!(status["installed"], true);
    assert_eq!(status["content_match"], true);
    assert_eq!(status["links"]["claude"], "ok");
    assert_eq!(status["links"]["cursor"], "ok");
    assert_eq!(status["links"]["copilot"], "ok");
}

#[test]
fn skill_install_never_clobbers_real_entries() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    // A real directory sitting where the claude symlink would go.
    let foreign = home.path().join(".claude/skills/cadence");
    std::fs::create_dir_all(&foreign).unwrap();
    std::fs::write(foreign.join("KEEP"), "mine").unwrap();

    let out = cadence_at(home.path(), state.path(), &["skill", "install"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["linked"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!(v["skipped"].as_array().unwrap().len(), 1);
    // Untouched — still a real dir with its contents.
    assert!(foreign.is_dir() && !foreign.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(
        std::fs::read_to_string(foreign.join("KEEP")).unwrap(),
        "mine"
    );
    let status: Value =
        serde_json::from_slice(&cadence_at(home.path(), state.path(), &["skill", "status"]).stdout)
            .unwrap();
    assert_eq!(status["links"]["claude"], "foreign");
}

#[test]
fn skill_install_overwrites_stale_content() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    cadence_at(home.path(), state.path(), &["skill", "install"]);
    let file = home.path().join(".agents/skills/cadence/SKILL.md");
    std::fs::write(&file, "STALE").unwrap();
    let status: Value =
        serde_json::from_slice(&cadence_at(home.path(), state.path(), &["skill", "status"]).stdout)
            .unwrap();
    assert_eq!(status["content_match"], false);

    cadence_at(home.path(), state.path(), &["skill", "install"]);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        cadence_agent::skill::SKILL_MD
    );
}

#[test]
fn daemon_run_refreshes_skill_on_start() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap().path().join("state");
    // Seed a stale copy so the refresh (not just install) is exercised.
    let file = home.path().join(".agents/skills/cadence/SKILL.md");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "STALE").unwrap();

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&state)
        .args(["daemon", "run"])
        .env("HOME", home.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while std::fs::read_to_string(&file)
        .map(|s| s.as_str() == "STALE")
        .unwrap_or(true)
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        cadence_agent::skill::SKILL_MD,
        "daemon start must refresh stale skill content"
    );
    // Missing links get created too.
    assert!(home
        .path()
        .join(".claude/skills/cadence")
        .symlink_metadata()
        .is_ok());
    // Cleanly stop the daemon we spawned. The skill file lands before
    // `serve()` binds the socket, so the first `daemon stop` can race
    // the listener — retry briefly, and bound the exit wait so a wedged
    // daemon fails the test instead of hanging it.
    let stop_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let out = cadence_at(home.path(), &state, &["daemon", "stop"]);
        if out.status.success() {
            break;
        }
        assert!(
            Instant::now() < stop_deadline,
            "daemon stop never succeeded: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    let wait_deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < wait_deadline,
            "daemon run never exited after daemon stop"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `agent list` inside a cadence pane scopes to the caller's group.
#[test]
fn agent_list_scopes_to_callers_group() {
    let d = TestDaemon::start();
    d.register("pm1");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "agent_register",
        json!({"alias": "w1", "provider": "fake", "endpoint_kind": "fake",
               "cwd": cwd, "params": "{\"upstream\":\"pm1\"}"}),
    )
    .unwrap();
    d.register("other");
    d.wait_agent("pm1", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    d.wait_agent("other", "idle", 10);
    let bin = env!("CARGO_BIN_EXE_cadence");
    let list = |env_alias: Option<&str>, extra: &[&str]| -> Value {
        let mut cmd = std::process::Command::new(bin);
        cmd.arg("--state-dir")
            .arg(&d.state)
            .args(["agent", "list"])
            .args(extra)
            .env_remove("CADENCE_ALIAS");
        if let Some(a) = env_alias {
            cmd.env("CADENCE_ALIAS", a);
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let aliases = |v: &Value| -> Vec<String> {
        let mut names: Vec<String> = v["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["alias"].as_str().unwrap().to_string())
            .collect();
        names.sort();
        names
    };

    // Worker inside a pane: sees its group — root + itself.
    let v = list(Some("w1"), &[]);
    assert_eq!(aliases(&v), vec!["pm1", "w1"], "{v}");
    let root = v["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"] == "pm1")
        .unwrap();
    assert_eq!(root["group_root"], true, "{v}");
    let worker = v["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"] == "w1")
        .unwrap();
    assert!(worker["group_root"].is_null(), "{v}");

    // The PM itself has no upstream — it IS its group root.
    let v = list(Some("pm1"), &[]);
    assert_eq!(aliases(&v), vec!["pm1", "w1"], "{v}");

    // Unresolvable alias and no env both fall back to global — and
    // every row still carries its group wiring: "group" names the root
    // (upstream when wired, else the row's own alias) and roots are
    // marked, so consumers can render the tree from global output too.
    let v = list(Some("nobody"), &[]);
    assert_eq!(aliases(&v), vec!["other", "pm1", "w1"], "{v}");
    let v = list(None, &[]);
    assert_eq!(aliases(&v), vec!["other", "pm1", "w1"], "{v}");
    for a in v["agents"].as_array().unwrap() {
        match a["alias"].as_str().unwrap() {
            "pm1" | "other" => {
                assert_eq!(a["group"], a["alias"], "{a}");
                assert_eq!(a["group_root"], true, "{a}");
            }
            "w1" => {
                assert_eq!(a["group"], "pm1", "{a}");
                assert!(a["group_root"].is_null(), "{a}");
            }
            other => panic!("unexpected agent {other}"),
        }
    }

    // --all forces global from inside a pane.
    let v = list(Some("w1"), &["--all"]);
    assert_eq!(aliases(&v), vec!["other", "pm1", "w1"], "{v}");
}

/// Every launch writes the briefing under the state dir — the cwd repo
/// stays byte-identical unless the operator opts in. Standalone launches
/// stay silent (no message) unless --bootstrap is passed.
#[test]
fn standalone_launch_writes_briefing_only() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    let repo = d.dir.path().join("srepo");
    git_repo(&repo);
    let bin = env!("CARGO_BIN_EXE_cadence");

    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["devin", "--alias", "solo", "--detach", "--cwd"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_agent("solo", "idle", 20);

    // Standalone root = itself: briefing under briefings/<alias>/ in
    // the state dir. The repo is untouched — no .cadence/, .gitignore
    // or AGENTS.md (audit N8).
    let briefing = d.state.join("briefings/solo/BRIEFING-solo.md");
    let text = std::fs::read_to_string(&briefing).unwrap();
    assert!(text.contains("none — you are a group root"), "{text}");
    assert!(text.contains("cadence self"), "{text}");
    assert_eq!(git_porcelain(&repo), "", "launch touched the repo");
    assert!(!repo.join(".cadence").exists());
    assert!(!repo.join("AGENTS.md").exists());
    assert!(!repo.join(".gitignore").exists());
    // `agent show` points at the state-dir path.
    let show = d.rpc("agent_show", json!({"alias": "solo"})).unwrap();
    assert_eq!(
        show["agent"]["briefing"].as_str().unwrap(),
        briefing.to_str().unwrap()
    );
    // Silent by default — no bootstrap message was enqueued.
    assert!(!show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["source"] == "bootstrap"));

    // --bootstrap on a standalone launch enqueues the durable kickoff.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["devin", "--alias", "wb", "--bootstrap", "--detach", "--cwd"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_agent("wb", "idle", 20);
    let m = d.wait_message("wb", "bootstrap-wb", &["queued", "submitting"], 15);
    assert_eq!(m["source"], "bootstrap");

    // --no-bootstrap writes nothing at all.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "devin",
            "--alias",
            "nb",
            "--no-bootstrap",
            "--detach",
            "--cwd",
        ])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_agent("nb", "idle", 20);
    assert!(!d.state.join("briefings/nb/BRIEFING-nb.md").exists());
    let show = d.rpc("agent_show", json!({"alias": "nb"})).unwrap();
    assert!(!show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["source"] == "bootstrap"));
}

/// `agent bootstrap` retrofits a live agent launched before briefings:
/// writes the file (to its upstream's group dir when wired) and enqueues
/// the durable message; unknown aliases are refused.
#[test]
fn agent_bootstrap_retrofits_live_agent() {
    let d = TestDaemon::start();
    d.register("pm");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "agent_register",
        json!({"alias": "w1", "provider": "fake", "endpoint_kind": "fake",
               "cwd": cwd, "params": "{\"upstream\":\"pm\"}"}),
    )
    .unwrap();
    d.register("solo");
    d.wait_agent("w1", "idle", 10);
    d.wait_agent("solo", "idle", 10);
    let bin = env!("CARGO_BIN_EXE_cadence");

    // Wired worker: briefing lands under the state dir's
    // briefings/<pm>/ and the message enqueues (fake endpoint — it
    // completes its turn).
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["agent", "bootstrap", "w1"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["message"], "bootstrap-w1");
    let m = d.wait_message("w1", "bootstrap-w1", &["completed"], 15);
    assert_eq!(m["source"], "bootstrap");
    let briefing = d.state.join("briefings/pm/BRIEFING-w1.md");
    let text = std::fs::read_to_string(&briefing).unwrap();
    assert!(text.contains("`pm` — reported results route"), "{text}");

    // Standalone agent briefs under its own briefings/<self>/.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["agent", "bootstrap", "solo"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(d.state.join("briefings/solo/BRIEFING-solo.md").exists());
    d.wait_message("solo", "bootstrap-solo", &["completed"], 15);

    // Unknown alias is refused, nothing is written.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["agent", "bootstrap", "ghost"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

/// `agent resume` waits for the endpoint and attaches like a provider
/// launch: JSON state + attach hint, --detach opts out, non-TTY prints
/// the attach command rather than exec'ing it.
#[test]
fn agent_resume_waits_and_attaches_like_launch() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    d.rpc("agent_stop", json!({"alias": "dv1"})).unwrap();
    d.wait_agent("dv1", "stopped", 15);
    let bin = env!("CARGO_BIN_EXE_cadence");

    // --detach: JSON with the attach hint, no attach attempt.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["agent", "resume", "dv1", "--detach"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["alias"], "dv1");
    assert!(v["endpoint"].is_string(), "{v}");
    assert_eq!(
        v["next"]["attach"].as_str().unwrap(),
        "cadence agent attach dv1"
    );
    d.wait_agent("dv1", "idle", 20);

    // Default (non-TTY test env): after the summary, the attach command
    // is printed — same JSON shape `agent attach` produces.
    d.rpc("agent_stop", json!({"alias": "dv1"})).unwrap();
    d.wait_agent("dv1", "stopped", 15);
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["agent", "resume", "dv1"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("tmux -L"), "{text}");
    assert!(text.contains("attach-session"), "{text}");
    d.wait_agent("dv1", "idle", 20);

    // A kind with no attachable endpoint returns the receipt
    // immediately — no 30s wait.
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "stopped", 15);
    let started = Instant::now();
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["agent", "resume", "w1"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "fake resume must not wait"
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["state"], "starting", "{v}");
    d.wait_agent("w1", "idle", 10);
}

/// `cadence resume <group>` resumes PM-first then upstream members only,
/// skips live agents, and reports per-member outcomes.
#[test]
fn group_resume_orders_pm_first_and_skips_live() {
    let d = TestDaemon::start();
    d.register("pm");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    for (alias, params) in [
        ("w1", Some("{\"upstream\":\"pm\"}")),
        ("w2", Some("{\"upstream\":\"pm\"}")),
        ("other", None),
    ] {
        let mut req = json!({"alias": alias, "provider": "fake",
                             "endpoint_kind": "fake", "cwd": cwd});
        if let Some(p) = params {
            req["params"] = json!(p);
        }
        d.rpc("agent_register", req).unwrap();
    }
    for a in ["pm", "w1", "w2", "other"] {
        d.wait_agent(a, "idle", 10);
    }
    // pm + w1 down; w2 stays live (skip case); other is ungrouped.
    d.rpc("agent_stop", json!({"alias": "pm"})).unwrap();
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    d.wait_agent("pm", "stopped", 15);
    d.wait_agent("w1", "stopped", 15);

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["resume", "pm", "--detach"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let resumed: Vec<&str> = v["resumed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["alias"].as_str().unwrap())
        .collect();
    // PM first, then members — and only the ones that were down.
    assert_eq!(resumed, vec!["pm", "w1"], "{v}");
    let skipped: Vec<&str> = v["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["alias"].as_str().unwrap())
        .collect();
    assert_eq!(skipped, vec!["w2"], "{v}");
    assert!(v["failed"].as_array().unwrap().is_empty(), "{v}");
    // The ungrouped agent was never touched.
    assert_eq!(
        d.rpc("agent_show", json!({"alias": "other"})).unwrap()["agent"]["state"],
        "idle"
    );
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
}

/// `cadence resume --all` sweeps every resumable agent; `daemon start
/// --resume` runs the same sweep once the daemon answers.
#[test]
fn resume_all_and_daemon_start_resume_sweep() {
    let d = TestDaemon::start();
    d.register("a1");
    d.register("a2");
    d.wait_agent("a1", "idle", 10);
    d.wait_agent("a2", "idle", 10);
    d.rpc("agent_stop", json!({"alias": "a1"})).unwrap();
    d.wait_agent("a1", "stopped", 15);
    let bin = env!("CARGO_BIN_EXE_cadence");

    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["resume", "--all"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let resumed: Vec<&str> = v["resumed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["alias"].as_str().unwrap())
        .collect();
    // a2 is live — not a sweep target at all; a1 resumes.
    assert_eq!(resumed, vec!["a1"], "{v}");
    d.wait_agent("a1", "idle", 10);

    // `daemon start --resume` against a second, stopped-forever state
    // dir would need a fresh daemon — instead verify the flag parses
    // and reaches the sweep against the LIVE daemon: agents with no
    // thread/session or a live endpoint are ignored, so the sweep is a
    // no-op here.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["daemon", "start", "--resume"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["state"], "already_running", "{v}");
    assert!(v["resume"]["resumed"].as_array().unwrap().is_empty(), "{v}");
}

/// `cadence stop <group>` tears down members + PM; agents stay
/// registered and resumable.
#[test]
fn group_stop_tears_down_members_and_pm() {
    let d = TestDaemon::start();
    d.register("pm");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "agent_register",
        json!({"alias": "w1", "provider": "fake", "endpoint_kind": "fake",
               "cwd": cwd, "params": "{\"upstream\":\"pm\"}"}),
    )
    .unwrap();
    d.register("other");
    for a in ["pm", "w1", "other"] {
        d.wait_agent(a, "idle", 10);
    }
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["stop", "pm"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let stopped: Vec<&str> = v["stopped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["alias"].as_str().unwrap())
        .collect();
    assert_eq!(stopped, vec!["w1", "pm"], "{v}");
    d.wait_agent("pm", "stopped", 15);
    d.wait_agent("w1", "stopped", 15);
    // Untouched outsider; members remain registered.
    assert_eq!(
        d.rpc("agent_show", json!({"alias": "other"})).unwrap()["agent"]["state"],
        "idle"
    );
    assert!(d.rpc("agent_show", json!({"alias": "w1"})).is_ok());
}

/// Error hints: resuming a live agent suggests attach; an
/// unrecoverable session mismatch in the sweep gets the
/// remove-and-rejoin hint.
#[test]
fn resume_error_hints() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    // Live agent resume → daemon rejection naming `cadence attach`.
    let err = d
        .rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("cadence attach w1"), "{err}");
}

/// A member whose pane bound a different native session is reported
/// unrecoverable with the remove-and-rejoin hint — the rest of the
/// group still resumes.
#[test]
fn group_resume_reports_unrecoverable_session_mismatch() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("pm", None);
    // Member pinned to a specific native session.
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "agent_register",
        json!({"alias": "w-bad", "provider": "devin", "endpoint_kind": "pty",
               "cwd": cwd,
               "params": "{\"upstream\":\"pm\",\"session\":\"want-x\"}"}),
    )
    .unwrap();
    d.wait_agent("pm", "idle", 15);
    d.wait_agent("w-bad", "idle", 15);
    d.rpc("agent_stop", json!({"alias": "w-bad"})).unwrap();
    d.wait_agent("w-bad", "stopped", 15);

    // Before the group resume, plant a rogue pane under the member's
    // session name that holds a DIFFERENT native lock — the reattach
    // path must fail closed with the session-mismatch error.
    let sock = socket_for(&d.state);
    let devin_py = mock.dir.join("mock-devin.py");
    let st = std::process::Command::new(mock.dir.join("tmux"))
        .args(["-L", &sock, "new-session", "-d", "-s", "w-bad", "-c", &cwd])
        .arg(format!(
            "python3 {} {} -r other-y",
            devin_py.display(),
            mock.locks.display()
        ))
        .output()
        .unwrap();
    assert!(
        st.status.success(),
        "{}",
        String::from_utf8_lossy(&st.stderr)
    );
    // Wait until the rogue pane actually holds its lock.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !mock.locks.join("other-y.lock").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "rogue pane never took its lock"
        );
        thread::sleep(Duration::from_millis(100));
    }

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["resume", "pm", "--detach"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let failed = v["failed"].as_array().unwrap();
    let bad = failed
        .iter()
        .find(|r| r["alias"] == "w-bad")
        .unwrap_or_else(|| panic!("w-bad missing from failed: {v}"));
    assert_eq!(bad["unrecoverable"], true, "{bad}");
    let hint = bad["hint"].as_str().unwrap_or_default();
    assert!(hint.contains("cadence agent remove w-bad"), "{hint}");
    assert!(hint.contains("cadence join"), "{hint}");
    assert!(
        bad["error"].as_str().unwrap_or("").contains("owns session"),
        "{bad}"
    );
    // The PM was already live — skipped, not failed; the mismatch is
    // strictly per-member.
    assert!(
        v["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["alias"] == "pm"),
        "{v}"
    );
}

/// Bare `cadence attach` orders roots before their members and marks
/// each row with its group, so a worker is identifiable under its PM.
#[test]
fn attach_listing_groups_workers_under_pm() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("pm", None);
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "agent_register",
        json!({"alias": "w1", "provider": "devin", "endpoint_kind": "pty",
               "cwd": cwd, "params": "{\"upstream\":\"pm\"}"}),
    )
    .unwrap();
    d.wait_agent("pm", "idle", 15);
    d.wait_agent("w1", "idle", 15);
    // A second, unrelated root sorts by its own group.
    d.register_devin("zz-solo", None);
    d.wait_agent("zz-solo", "idle", 15);

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .arg("attach")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let rows = v["attachable"].as_array().unwrap();
    let aliases: Vec<&str> = rows.iter().map(|r| r["alias"].as_str().unwrap()).collect();
    // pm's group first, pm before its member; the unrelated root last.
    assert_eq!(aliases, vec!["pm", "w1", "zz-solo"], "{v}");
    assert_eq!(rows[0]["group"], "pm");
    assert_eq!(rows[0]["group_root"], true);
    assert_eq!(rows[1]["group"], "pm");
    assert_eq!(rows[1]["group_root"], false);
    assert_eq!(rows[2]["group_root"], true);

    // A named attach in a non-TTY context prints the command rather
    // than exec'ing it — the same rule launches and resume follow.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["attach", "pm"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["alias"], "pm");
    assert!(
        v["command"]
            .as_str()
            .unwrap_or("")
            .contains("attach-session"),
        "{v}"
    );
}

/// `cadence send` is the verb alias for `message send` — same durable
/// enqueue, same fields.
#[test]
fn send_verb_matches_message_send() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["send", "w1", "--text", "do thing", "--message", "m-verb"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["message"], "m-verb", "{v}");
    // Fake endpoints complete turns in-line — the message lands.
    d.wait_message("w1", "m-verb", &["completed"], 15);
}

/// The uncapped send path must read complete file and stdin bodies before
/// enqueueing them. Exercise both CLI input forms against a real temporary
/// daemon so a successful exit also proves the full body was persisted.
#[test]
fn send_file_and_stdin_persist_full_bodies() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);

    let file_body = "file body\nwith a second line\n";
    let file = d.dir.path().join("send-body.txt");
    std::fs::write(&file, file_body).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "send",
            "w1",
            "--file",
            file.to_str().unwrap(),
            "--message",
            "m-file-body",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let message = d.wait_message("w1", "m-file-body", &["completed"], 15);
    assert_eq!(message["body"], file_body);

    let stdin_body = "stdin body\nwith a second line\n";
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["send", "w1", "--message", "m-stdin-body"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin_body.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let message = d.wait_message("w1", "m-stdin-body", &["completed"], 15);
    assert_eq!(message["body"], stdin_body);
}

// ==== inbox endpoint kind ====

impl TestDaemon {
    /// Register a mailbox: provider+kind `inbox`, durable pseudo-endpoint,
    /// no actor.
    fn register_inbox(&self, alias: &str) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        self.rpc(
            "agent_register",
            json!({"alias": alias, "provider": "inbox",
                   "endpoint_kind": "inbox", "cwd": cwd}),
        )
        .unwrap();
    }

    /// Register a pty devin agent with arbitrary endpoint params.
    fn register_devin_opts(&self, alias: &str, params: Value) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        self.rpc(
            "agent_register",
            json!({"alias": alias, "provider": "devin",
                   "endpoint_kind": "pty", "cwd": cwd,
                   "params": params.to_string()}),
        )
        .unwrap();
    }

    /// All recorded events for an alias.
    fn events(&self, alias: &str) -> Vec<Value> {
        self.rpc("agent_events", json!({"alias": alias})).unwrap()["events"]
            .as_array()
            .unwrap()
            .clone()
    }

    /// Poll until an event of `kind` exists (bounded).
    fn wait_event(&self, alias: &str, kind: &str, secs: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(e) = self
                .events(alias)
                .into_iter()
                .find(|e| e["kind"].as_str() == Some(kind))
            {
                return e;
            }
            assert!(
                Instant::now() < deadline,
                "agent {alias} never emitted {kind}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Poll until an event of `kind` satisfying `pred` exists
    /// (bounded). Payload-scoped — an earlier event that merely shares
    /// the kind is never returned (CAD-222: a late `turn_stalled` for
    /// one message must not answer a wait meant for another's).
    fn wait_event_where(
        &self,
        alias: &str,
        kind: &str,
        pred: impl Fn(&Value) -> bool,
        secs: u64,
    ) -> Value {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(e) = self
                .events(alias)
                .into_iter()
                .find(|e| e["kind"].as_str() == Some(kind) && pred(e))
            {
                return e;
            }
            assert!(
                Instant::now() < deadline,
                "agent {alias} never emitted a matching {kind}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Emit a test-only timing trace for a routed PTY delivery. The daemon's
/// durable event/message timestamps are the phase clock here: using them
/// avoids charging the test's 50ms RPC polling to a render or retry phase.
/// A `submitting` row is the durable attempt-start boundary and a
/// `paste_not_rendered` row is its completion. The next `submitting` row is
/// the observable retry wake; no separate wake event exists. This is evidence
/// for the follow-up audit, not a change to the delivery contract.
fn emit_park_phase_trace(d: &TestDaemon, test_name: &str, alias: &str, routed_id: &str) {
    fn at(value: &Value) -> Option<f64> {
        value["at"].as_f64()
    }

    fn delta(start: Option<f64>, end: Option<f64>) -> Value {
        match (start, end) {
            (Some(start), Some(end)) if end >= start => json!(end - start),
            _ => Value::Null,
        }
    }

    let events = d.events(alias);
    let starts: Vec<&Value> = events
        .iter()
        .filter(|event| {
            event["kind"] == "submitting" && event["payload"]["message"].as_str() == Some(routed_id)
        })
        .collect();
    let misses: Vec<&Value> = events
        .iter()
        .filter(|event| {
            event["kind"] == "paste_not_rendered"
                && event["payload"]["message"].as_str() == Some(routed_id)
        })
        .collect();
    let parked = events.iter().find(|event| {
        event["kind"] == "delivery_parked"
            && event["payload"]["message"].as_str() == Some(routed_id)
    });
    let show = d.rpc("agent_show", json!({"alias": alias})).unwrap();
    let message = show["messages"]
        .as_array()
        .and_then(|messages| messages.iter().find(|message| message["id"] == routed_id));
    let enqueue_at = message.and_then(|message| message["created"].as_f64());
    let attempt_phases: Vec<Value> = misses
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let started_at = starts.get(index).and_then(|event| at(event));
            json!({
                "attempt": index + 1,
                "started_at_epoch_s": started_at,
                "completion_at_epoch_s": at(event),
                "enqueue_to_start_s": if index == 0 {
                    delta(enqueue_at, started_at)
                } else {
                    Value::Null
                },
                "render_attempt_s": delta(started_at, at(event)),
                "retry": event["payload"]["retry"],
            })
        })
        .collect();
    let retry_phases: Vec<Value> = misses
        .windows(2)
        .enumerate()
        .map(|(index, pair)| {
            json!({
                "after_attempt": index + 1,
                "retry_wake_at_epoch_s": starts.get(index + 1).and_then(|event| at(event)),
                "retry_to_next_attempt_start_s": delta(
                    at(pair[0]),
                    starts.get(index + 1).and_then(|event| at(event)),
                ),
            })
        })
        .collect();
    let parked_at = parked.and_then(at);
    let completed_at = message.and_then(|message| message["completed"].as_f64());
    let final_agent = show.get("agent").map(|agent| {
        json!({
            "state": agent["state"],
            "dead": agent["dead"],
            "updated_epoch_s": agent["updated"],
        })
    });
    let report = json!({
        "schema": "cad173.e4a.phase-trace.v1",
        "test": test_name,
        "alias": alias,
        "message": routed_id,
        "enqueue_at_epoch_s": enqueue_at,
        "attempts": attempt_phases,
        "submitting_events": starts.len(),
        "retry_gaps": retry_phases,
        "park_at_epoch_s": parked_at,
        "park_after_attempt4_s": delta(misses.last().and_then(|event| at(event)), parked_at),
        "failed_state_at_epoch_s": completed_at,
        "park_to_failed_state_s": delta(parked_at, completed_at),
        "enqueue_to_failed_state_s": delta(enqueue_at, completed_at),
        "message_state": message.map(|message| message["state"].clone()),
        "agent": final_agent,
        "clock": "durable events.at and messages.created/completed (epoch seconds)",
        "attempt_boundary": "submitting event is attempt start; paste_not_rendered is completion; next submitting event is the retry wake",
    });
    eprintln!("CAD173_E4A_PHASE {report}");
}

#[test]
fn inbox_registers_as_durable_mailbox() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    let show = d.rpc("agent_show", json!({"alias": "obs"})).unwrap();
    let agent = &show["agent"];
    assert_eq!(agent["endpoint_kind"], "inbox");
    assert_eq!(agent["provider"], "inbox");
    assert_eq!(agent["endpoint"], "inbox://obs");
    assert_eq!(agent["dead"], false, "{agent}");
    assert_eq!(agent["state"], "idle");
    assert_eq!(show["queued"], 0);
    assert_eq!(show["inbox"]["kind"], "passive", "{show}");
    assert_eq!(show["inbox"]["state"], "idle", "{show}");
    assert_eq!(show["inbox"]["receipt_only"], true, "{show}");
    assert_eq!(
        show["inbox"]["semantic_completion"], "external_consumer_required",
        "{show}"
    );
    // Registration wrote no briefing files in its cwd.
    assert!(!d.dir.path().join(".cadence").exists());

    // A mailbox never runs a process, so socket callers may omit cwd;
    // a process endpoint still requires it.
    d.rpc(
        "agent_register",
        json!({"alias": "obs2", "provider": "inbox", "endpoint_kind": "inbox"}),
    )
    .unwrap();
    assert_eq!(
        d.rpc("agent_show", json!({"alias": "obs2"})).unwrap()["agent"]["endpoint"],
        "inbox://obs2"
    );
    assert!(d
        .rpc(
            "agent_register",
            json!({"alias": "w9", "provider": "fake", "endpoint_kind": "fake"}),
        )
        .is_err());
}

#[test]
fn inbox_drains_messages_once() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    for (id, text) in [("n1", "note one"), ("n2", "note two")] {
        d.rpc(
            "agent_send",
            json!({"alias": "obs", "text": text, "message": id}),
        )
        .unwrap();
    }
    // Backlog is visible before draining.
    assert_eq!(
        d.rpc("agent_show", json!({"alias": "obs"})).unwrap()["queued"],
        2
    );
    let backlog = d.rpc("agent_show", json!({"alias": "obs"})).unwrap();
    assert_eq!(backlog["inbox"]["state"], "backlog", "{backlog}");
    assert_eq!(backlog["inbox"]["queued"], 2, "{backlog}");
    assert!(backlog["inbox"]["oldest_age_secs"].as_f64().unwrap_or(-1.0) >= 0.0);
    assert!(backlog["inbox"]["next_action"]
        .as_str()
        .unwrap()
        .contains("consumer"));
    let page = d.rpc("agent_inbox", json!({"alias": "obs"})).unwrap();
    let msgs = page["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2, "{page}");
    assert_eq!(msgs[0]["id"], "n1");
    assert_eq!(msgs[1]["id"], "n2");
    assert_eq!(page["cursor"], msgs[1]["seq"]);
    // Consumption completed each message via=inbox_read.
    let show = d.rpc("agent_show", json!({"alias": "obs"})).unwrap();
    for m in show["messages"].as_array().unwrap() {
        assert_eq!(m["state"], "completed", "{m}");
        assert_eq!(m["result"]["via"], "inbox_read", "{m}");
    }
    // A second drain returns nothing.
    let page2 = d.rpc("agent_inbox", json!({"alias": "obs"})).unwrap();
    assert!(page2["messages"].as_array().unwrap().is_empty());
    // Draining a non-inbox agent is a clean rejection.
    d.register("w1");
    assert!(d.rpc("agent_inbox", json!({"alias": "w1"})).is_err());
}

#[test]
fn inbox_wait_blocks_until_arrival() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    let state = d.state.clone();
    let sender = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        client::rpc(
            &state,
            "agent_send",
            json!({"alias": "obs", "text": "late note", "message": "n1"}),
        )
        .unwrap();
    });
    let started = Instant::now();
    // A consumer can block on the daemon — no polling loop needed.
    let page = d
        .rpc("agent_inbox", json!({"alias": "obs", "wait": 10}))
        .unwrap();
    sender.join().unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    let msgs = page["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["id"], "n1");
}

#[test]
fn inbox_group_root_collects_worker_results() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    // `cadence join obs fake` wires exactly this: worker params.upstream
    // = the inbox alias. Register the equivalent directly.
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "agent_register",
        json!({"alias": "w1", "provider": "fake", "endpoint_kind": "fake",
               "cwd": cwd, "params": json!({"upstream": "obs"}).to_string()}),
    )
    .unwrap();
    d.wait_agent("w1", "idle", 10);
    // The worker's send defaults reply_to=obs (its upstream) — the
    // completed result routes into the mailbox, not a pane.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "do work", "message": "j1"}),
    )
    .unwrap();
    d.wait_message("w1", "j1", &["completed"], 15);
    let page = d.rpc("agent_inbox", json!({"alias": "obs"})).unwrap();
    let msgs = page["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1, "{page}");
    assert_eq!(msgs[0]["source"], "worker_result");
    assert!(msgs[0]["body"].as_str().unwrap().contains("j1"));
    assert!(msgs[0]["body"].as_str().unwrap().contains("w1"));
    // Consuming it does not re-route — routed copies carry no reply_to.
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(
        show["messages"].as_array().unwrap().len(),
        1,
        "no echo back to the worker"
    );
}

#[test]
fn inbox_collects_direct_send_with_reply_to() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_inbox("obs");
    d.register_devin("sender", None);
    d.wait_agent("sender", "idle", 20);
    // Direct send on a pty agent with --reply-to obs: the reported
    // result routes into the mailbox.
    d.rpc("agent_ready", json!({"alias": "sender"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "sender", "text": "task", "message": "t1",
               "reply_to": "obs"}),
    )
    .unwrap();
    let token = pty_token(&d, "sender", "t1");
    d.rpc(
        "message_report",
        json!({"message": "t1", "token": token, "kind": "result",
               "text": "did the thing"}),
    )
    .unwrap();
    let page = d.rpc("agent_inbox", json!({"alias": "obs"})).unwrap();
    let msgs = page["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1, "{page}");
    assert_eq!(msgs[0]["source"], "worker_result");
    assert!(msgs[0]["body"].as_str().unwrap().contains("did the thing"));
}

#[test]
fn inbox_read_receipt_keeps_history_without_waking_reviewer() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude("ok", None);
    d.register_inbox("obs");
    d.register_claude("reviewer", Value::Null);
    d.register("worker");
    d.wait_agent("reviewer", "idle", 20);
    d.wait_agent("worker", "idle", 10);

    // A real worker result still lands in the mailbox and must survive the
    // same drain alongside the acknowledgement-only message.
    d.rpc(
        "agent_send",
        json!({"alias": "worker", "text": "do work", "message": "work-1",
               "reply_to": "obs"}),
    )
    .unwrap();
    d.wait_message("worker", "work-1", &["completed"], 15);

    // This is the actual receipt path: a mailbox message has a return
    // address, then the consumer drains it. Completing the read must not
    // manufacture a worker_result turn for the reviewer.
    d.rpc(
        "agent_send",
        json!({"alias": "obs", "text": "ack me", "message": "receipt-1",
               "reply_to": "reviewer"}),
    )
    .unwrap();

    let page = d.rpc("agent_inbox", json!({"alias": "obs"})).unwrap();
    let drained = page["messages"].as_array().unwrap();
    assert_eq!(drained.len(), 2, "{page}");
    let work = drained
        .iter()
        .find(|m| m["source"] == "worker_result")
        .expect("genuine worker result was not retained");
    assert!(work["body"].as_str().unwrap().contains("work-1"));

    // The consumed row remains the durable source of truth, including its
    // receipt marker and original reply address.
    let obs = d.rpc("agent_show", json!({"alias": "obs"})).unwrap();
    let receipt = obs["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "receipt-1")
        .expect("receipt row was not retained");
    assert_eq!(receipt["state"], "completed");
    assert_eq!(receipt["reply_to"], "reviewer");
    assert_eq!(receipt["result"]["via"], "inbox_read");

    // No routed copy means no queued reviewer prompt and no model turn.
    let reviewer = d.rpc("agent_show", json!({"alias": "reviewer"})).unwrap();
    assert!(
        reviewer["messages"].as_array().unwrap().is_empty(),
        "{reviewer}"
    );
    assert!(d
        .events("reviewer")
        .iter()
        .all(|event| event["kind"] != "turn_started"));
}

#[test]
fn inbox_lifecycle_guards() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    for method in ["agent_resume", "agent_stop"] {
        let err = d
            .rpc(method, json!({"alias": "obs"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("inbox"), "{method}: {err}");
    }
    for method in ["agent_probe", "agent_capture", "agent_ready"] {
        let err = d
            .rpc(method, json!({"alias": "obs"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("inbox"), "{method}: {err}");
    }
    // Removal works directly — a mailbox is never "live".
    d.rpc("agent_remove", json!({"alias": "obs"})).unwrap();
    assert!(d.rpc("agent_show", json!({"alias": "obs"})).is_err());
}

#[test]
fn cli_inbox_drains_and_self_reports_backlog() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    d.rpc(
        "agent_send",
        json!({"alias": "obs", "text": "hello inbox", "message": "n1"}),
    )
    .unwrap();
    let bin = env!("CARGO_BIN_EXE_cadence");

    // `cadence self` inside a hand-exported inbox alias reports the
    // queued count, not a running turn.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .arg("self")
        .env("CADENCE_ALIAS", "obs")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["alias"], "obs");
    assert_eq!(v["endpoint_kind"], "inbox");
    assert_eq!(v["queued"], 1, "{v}");

    // `cadence inbox obs` prints one JSON object per message.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["inbox", "obs"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 1, "{stdout}");
    assert_eq!(lines[0]["id"], "n1");
    assert_eq!(lines[0]["body"], "hello inbox");

    // Second call prints nothing at all.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["inbox", "obs"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
}

// ==== verified auto-ready (pty) ====

#[test]
fn pty_auto_ready_self_claims_when_idle() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);
    // No human claim — the daemon probes the pane, sees the idle
    // prompt, and self-claims.
    d.rpc(
        "agent_send",
        json!({"alias": "dv", "text": "auto ready task", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("dv", "m1", &["running"], 20);
    let claim = d.wait_event("dv", "ready_claimed", 5);
    assert_eq!(claim["payload"]["by"], "daemon", "{claim}");
    assert_eq!(claim["payload"]["probe"]["idle"], true, "{claim}");
    // The pane really received the text.
    let screen = std::fs::read_to_string(d.pane_file(&mock, "dv", "screen")).unwrap_or_default()
        + &std::fs::read_to_string(d.pane_file(&mock, "dv", "input")).unwrap_or_default();
    assert!(screen.contains("auto ready task"), "{screen}");
}

#[test]
fn pty_auto_ready_waits_on_busy_pane_then_delivers() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);
    // The TUI shows a working state — the probe must refuse the paste.
    // The busy tail is the real shape: status row directly above the
    // box, busy watermark in the input line.
    atomic_write(
        d.pane_file(&mock, "dv", "tui-state"),
        "⠸ Thinking · 12s (esc twice to interrupt)\n❭ Guide Devin while it works\n",
    );
    d.rpc(
        "agent_send",
        json!({"alias": "dv", "text": "wait for idle", "message": "m1"}),
    )
    .unwrap();
    let wait = d.wait_event("dv", "gate_wait", 10);
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("busy"),
        "{wait}"
    );
    thread::sleep(Duration::from_millis(400));
    assert_eq!(d.message_state("dv", "m1"), "queued");
    // Nothing was pasted while the pane looked busy.
    assert!(std::fs::read_to_string(d.pane_file(&mock, "dv", "input"))
        .unwrap_or_default()
        .is_empty());
    // Probe RPC exposes the same verdict the gate used.
    let probe = d.rpc("agent_probe", json!({"alias": "dv"})).unwrap();
    assert_eq!(probe["idle"], false);
    assert_eq!(probe["busy_marker"], true, "{probe}");
    // Pane goes idle — the bounded backoff retries and delivers.
    std::fs::remove_file(d.pane_file(&mock, "dv", "tui-state")).unwrap();
    d.wait_message("dv", "m1", &["running"], 20);
    let claim = d.wait_event("dv", "ready_claimed", 5);
    assert_eq!(claim["payload"]["by"], "daemon");
}

/// Send `text` to a fake worker that replies to `pm`, and return the
/// routed delivery id once the worker's own turn has completed. The
/// route and the completion commit together.
fn route_worker_result(d: &TestDaemon, worker: &str, pm: &str, id: &str, text: &str) -> String {
    d.rpc(
        "agent_send",
        json!({"alias": worker, "text": text, "message": id, "reply_to": pm}),
    )
    .unwrap();
    d.wait_message(worker, id, &["completed"], 15);
    d.rpc("agent_show", json!({"alias": pm})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| {
            m["source"] == "worker_result" && m["body"].as_str().unwrap_or_default().contains(id)
        })
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Idle pty, `auto_ready` unset, no `agent ready`: a routed
/// `worker_result` is submitted within 2s of the route (the missing
/// wake must not hide behind the 5s empty-queue poll). The same pane
/// then refuses a `source=user` send.
#[test]
fn pty_routed_notice_delivers_idle_without_claim() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("pm", None);
    d.register("w1");
    d.wait_agent("pm", "idle", 20);
    d.wait_agent("w1", "idle", 10);
    let routed_id = route_worker_result(&d, "w1", "pm", "work-1", "review this pane");
    let started = Instant::now();
    let deadline = started + Duration::from_secs(2);
    let mut submitted = false;
    while Instant::now() < deadline {
        let state = d.message_state("pm", &routed_id);
        if state == "running" || state == "completed" {
            submitted = true;
            break;
        }
        assert_ne!(state, "failed", "routed notice failed before submit");
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        submitted,
        "routed notice still {} after {:?} — wake did not beat the empty-queue poll",
        d.message_state("pm", &routed_id),
        started.elapsed()
    );
    let done = d.wait_message("pm", &routed_id, &["completed"], 10);
    assert_eq!(done["result"]["via"], "pty_deliver", "{done}");
    let claim = d
        .events("pm")
        .into_iter()
        .find(|e| e["kind"].as_str() == Some("ready_claimed"))
        .expect("routed paste recorded no ready_claimed");
    assert_eq!(claim["payload"]["by"], "daemon", "{claim}");
    assert_eq!(claim["payload"]["reason"], "routed", "{claim}");
    assert_eq!(claim["payload"]["probe"]["idle"], true, "{claim}");
    // The mock commits Enter by moving the draft onto the screen and
    // clearing the input file, same as the auto-ready paste check.
    let pasted = std::fs::read_to_string(d.pane_file(&mock, "pm", "screen")).unwrap_or_default()
        + &std::fs::read_to_string(d.pane_file(&mock, "pm", "input")).unwrap_or_default();
    assert!(pasted.contains("work-1"), "{pasted}");

    // The unclaimed flag must not leak onto the next user paste.
    d.wait_agent("pm", "idle", 10);
    d.rpc(
        "agent_send",
        json!({"alias": "pm", "text": "user task stays queued", "message": "u1",
               "source": "user"}),
    )
    .unwrap();
    let wait = d.wait_event("pm", "gate_wait", 10);
    assert_eq!(wait["payload"]["message"], "u1", "{wait}");
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("agent ready"),
        "{wait}"
    );
    assert_eq!(d.message_state("pm", "u1"), "queued");
    let after = std::fs::read_to_string(d.pane_file(&mock, "pm", "screen")).unwrap_or_default()
        + &std::fs::read_to_string(d.pane_file(&mock, "pm", "input")).unwrap_or_default();
    assert!(!after.contains("user task stays queued"), "{after}");
}

/// A busy pane still refuses a routed notice: `gate_wait`, no paste.
#[test]
fn pty_routed_notice_waits_on_busy_pane() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("pm", None);
    d.register("w1");
    d.wait_agent("pm", "idle", 20);
    d.wait_agent("w1", "idle", 10);
    atomic_write(
        d.pane_file(&mock, "pm", "tui-state"),
        "⠸ Thinking · 12s (esc twice to interrupt)\n❭ Guide Devin while it works\n",
    );
    let routed_id = route_worker_result(&d, "w1", "pm", "work-busy", "notice while busy");
    let wait = d.wait_event_where(
        "pm",
        "gate_wait",
        |event| event["payload"]["message"] == routed_id,
        10,
    );
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("busy"),
        "{wait}"
    );
    assert_eq!(d.message_state("pm", &routed_id), "queued");
    assert!(std::fs::read_to_string(d.pane_file(&mock, "pm", "input"))
        .unwrap_or_default()
        .is_empty());
}

/// An open approval menu refuses a routed notice the same way it
/// refuses a claimed paste. Reuses the Devin menu fixture.
#[test]
fn pty_routed_notice_waits_on_approval_menu() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("pm", None);
    d.register("w1");
    d.wait_agent("pm", "idle", 20);
    d.wait_agent("w1", "idle", 10);
    atomic_write(d.pane_file(&mock, "pm", "tui-state"), DEVIN_MENU);
    let routed_id = route_worker_result(&d, "w1", "pm", "work-menu", "notice under menu");
    let wait = d.wait_event_where(
        "pm",
        "gate_wait",
        |event| event["payload"]["message"] == routed_id,
        10,
    );
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("approval menu"),
        "{wait}"
    );
    assert_eq!(d.message_state("pm", &routed_id), "queued");
    assert!(std::fs::read_to_string(d.pane_file(&mock, "pm", "input"))
        .unwrap_or_default()
        .is_empty());
}

#[test]
fn pty_probe_busy_markers_survive_trailing_blank_rows() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);
    // capture-pane pads to pane height: a young session on a tall pane
    // leaves blank rows below the content. The probe must anchor the
    // status region at the last non-blank row or a busy pane reads
    // idle and the daemon pastes into it.
    atomic_write(
        d.pane_file(&mock, "dv", "tui-state"),
        "⠸ Thinking · 12s (esc twice to interrupt)\n❭ Guide Devin while it works\n".to_string()
            + &"\n".repeat(30),
    );
    let probe = d.rpc("agent_probe", json!({"alias": "dv"})).unwrap();
    assert_eq!(probe["idle"], false, "{probe}");
    assert_eq!(probe["busy_marker"], true, "{probe}");
    // And the gate refuses to paste into it.
    d.rpc(
        "agent_send",
        json!({"alias": "dv", "text": "do not paste", "message": "m1"}),
    )
    .unwrap();
    let wait = d.wait_event("dv", "gate_wait", 10);
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("busy"),
        "{wait}"
    );
    assert_eq!(d.message_state("dv", "m1"), "queued");
    assert!(std::fs::read_to_string(d.pane_file(&mock, "dv", "input"))
        .unwrap_or_default()
        .is_empty());
}

#[test]
fn pty_ready_claims_stack_fifo_with_claimer() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv", None);
    d.wait_agent("dv", "idle", 20);
    // Two claims land before either send — the queue stacks them FIFO
    // instead of overwriting (N6). Each releases exactly one message.
    d.rpc("agent_ready", json!({"alias": "dv", "by": "alice"}))
        .unwrap();
    d.rpc("agent_ready", json!({"alias": "dv", "by": "bob"}))
        .unwrap();
    for id in ["m1", "m2"] {
        d.rpc(
            "agent_send",
            json!({"alias": "dv", "text": format!("task {id}"), "message": id}),
        )
        .unwrap();
    }
    d.wait_message("dv", "m1", &["running"], 15);
    d.wait_message("dv", "m2", &["running"], 15);
    let used: Vec<Value> = d
        .events("dv")
        .into_iter()
        .filter(|e| e["kind"].as_str() == Some("claim_used"))
        .collect();
    assert_eq!(used.len(), 2, "{:?}", d.events("dv"));
    // FIFO: m1 consumed alice's claim, m2 consumed bob's.
    assert_eq!(used[0]["payload"]["message"], "m1");
    assert_eq!(used[0]["payload"]["by"], "alice");
    assert_eq!(used[1]["payload"]["message"], "m2");
    assert_eq!(used[1]["payload"]["by"], "bob");
}

#[test]
fn pty_unrendered_task_fences_unknown() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);
    // The pane drops the paste entirely (TUI swallowed the input).
    atomic_write(d.pane_file(&mock, "dv", "swallow"), "1");
    d.rpc(
        "agent_send",
        json!({"alias": "dv", "text": "task that vanishes", "message": "m1"}),
    )
    .unwrap();
    // A provable paste miss on a task message: unknown + attention —
    // never a blind replay.
    d.wait_message("dv", "m1", &["unknown"], 20);
    d.wait_agent("dv", "attention", 15);
    let e = d.wait_event("dv", "paste_not_rendered", 5);
    assert_eq!(e["payload"]["message"], "m1");
    assert_eq!(e["payload"]["retry"], false, "{e}");
}

/// A fence detaches the pane — never kills it. The surviving pane
/// stays inspectable and `agent resume` re-adopts it after the
/// operator reconciles: same pid, same native session.
#[test]
fn pty_fence_detaches_pane_for_resume() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    let agent = d.wait_agent("dv1", "idle", 20);
    let native = agent["thread_id"].as_str().unwrap().to_string();
    let pidfile = d.pane_file(&mock, "dv1", "pid");
    let pane_pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // The pane drops the paste entirely: unrendered → fence.
    atomic_write(d.pane_file(&mock, "dv1", "swallow"), "1");
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("dv1", "m1", &["unknown"], 20);
    d.wait_agent("dv1", "attention", 15);
    // The fence detached — the pane process and its pid file are
    // unchanged, and the screen is still there to inspect.
    assert_eq!(
        unsafe { libc::kill(pane_pid, 0) },
        0,
        "fence killed the pane"
    );
    let pid_now: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(pid_now, pane_pid);
    // Reconcile, then resume: the surviving pane is re-adopted.
    d.rpc(
        "agent_unfence",
        json!({"alias": "dv1", "status": "interrupted"}),
    )
    .unwrap();
    d.wait_agent("dv1", "stopped", 10);
    std::fs::remove_file(d.pane_file(&mock, "dv1", "swallow")).unwrap();
    d.rpc("agent_resume", json!({"alias": "dv1"})).unwrap();
    let agent = d.wait_agent("dv1", "idle", 20);
    let pid_after: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(pid_after, pane_pid, "pane was not re-adopted");
    assert_eq!(agent["thread_id"].as_str().unwrap(), native);
    // New work flows over the adopted pane.
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "again", "message": "m2"}),
    )
    .unwrap();
    d.wait_message("dv1", "m2", &["running"], 20);
}

/// Explicit lifecycle verbs own the kill: `agent stop`, `agent remove`,
/// and `agent gc` each kill a fenced agent's surviving pane — the row
/// never drops leaving an orphan session on the private socket.
#[test]
fn pty_stop_remove_gc_kill_surviving_panes() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    for alias in ["dv-stop", "dv-rm", "dv-gc"] {
        d.register_devin(alias, None);
        d.wait_agent(alias, "idle", 20);
    }
    // Fence all three: each pane survives its fence for inspection.
    for alias in ["dv-stop", "dv-rm", "dv-gc"] {
        atomic_write(d.pane_file(&mock, alias, "swallow"), "1");
        d.rpc("agent_ready", json!({"alias": alias})).unwrap();
        d.rpc(
            "agent_send",
            json!({"alias": alias, "text": "task",
                   "message": format!("m-{alias}")}),
        )
        .unwrap();
    }
    for alias in ["dv-stop", "dv-rm", "dv-gc"] {
        d.wait_agent(alias, "attention", 25);
    }
    // `agent stop` on a fenced agent is the explicit kill.
    d.rpc("agent_stop", json!({"alias": "dv-stop"})).unwrap();
    wait_pid_gone(&d.pane_file(&mock, "dv-stop", "pid"), 10);
    // `agent remove` kills the pane before dropping the row.
    d.rpc("agent_remove", json!({"alias": "dv-rm"})).unwrap();
    wait_pid_gone(&d.pane_file(&mock, "dv-rm", "pid"), 10);
    assert!(d.rpc("agent_show", json!({"alias": "dv-rm"})).is_err());
    // `agent gc` kills the dead agent's pane before dropping the row.
    let swept = d.rpc("agent_gc", json!({})).unwrap();
    assert!(
        swept["removed"]
            .as_array()
            .unwrap()
            .contains(&json!("dv-gc")),
        "{swept}"
    );
    wait_pid_gone(&d.pane_file(&mock, "dv-gc", "pid"), 10);
    assert!(d.rpc("agent_show", json!({"alias": "dv-gc"})).is_err());
}

/// `agent_unfence` with `resume: true` reconciles and resumes in one
/// call and reports what the endpoint actually did. A surviving pane
/// is adopted — same pid, same native session.
#[test]
fn pty_unfence_resume_reports_adopted_pane() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    let agent = d.wait_agent("dv1", "idle", 20);
    let native = agent["thread_id"].as_str().unwrap().to_string();
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    pty_token(&d, "dv1", "m1");
    let pane_pid: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // Fence: the paste never renders; the pane is detached, not killed.
    atomic_write(d.pane_file(&mock, "dv1", "swallow"), "1");
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "dropped", "message": "m2"}),
    )
    .unwrap();
    d.wait_agent("dv1", "attention", 25);
    // One call reconciles the unknown and brings the agent back.
    let r = d
        .rpc(
            "agent_unfence",
            json!({"alias": "dv1", "status": "interrupted",
                   "resume": true}),
        )
        .unwrap();
    assert_eq!(r["reconciled"], json!(["m2"]), "{r}");
    assert_eq!(r["resumed"], true, "{r}");
    assert_eq!(r["pane"], "adopted", "{r}");
    let agent = d.wait_agent("dv1", "idle", 20);
    let pid_after: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(pid_after, pane_pid, "adopted pane changed pid");
    assert_eq!(agent["thread_id"].as_str().unwrap(), native);
}

/// The same call reports `respawned` when no pane survived: a new
/// pane is launched on the recorded session — new pid, same native
/// thread.
#[test]
fn pty_unfence_resume_reports_respawned_pane() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    let agent = d.wait_agent("dv1", "idle", 20);
    let native = agent["thread_id"].as_str().unwrap().to_string();
    let pane_pid: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    atomic_write(d.pane_file(&mock, "dv1", "swallow"), "1");
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "dropped", "message": "m1"}),
    )
    .unwrap();
    d.wait_agent("dv1", "attention", 25);
    // The detached pane dies out-of-band before the resume — nothing
    // left to adopt.
    std::process::Command::new(mock.dir.join("tmux"))
        .args(["-L", &socket_for(&d.state), "kill-session", "-t", "dv1"])
        .env("MOCK_TMUX_STATE", mock.dir.join("tmux-state"))
        .output()
        .unwrap();
    wait_pid_gone(&d.pane_file(&mock, "dv1", "pid"), 10);
    let r = d
        .rpc(
            "agent_unfence",
            json!({"alias": "dv1", "status": "interrupted",
                   "resume": true}),
        )
        .unwrap();
    assert_eq!(r["resumed"], true, "{r}");
    assert_eq!(r["pane"], "respawned", "{r}");
    let agent = d.wait_agent("dv1", "idle", 20);
    let pid_after: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_ne!(pid_after, pane_pid, "respawn kept the dead pid");
    assert_eq!(agent["thread_id"].as_str().unwrap(), native);
}

/// Non-pty kinds carry no pane field at all — the response still says
/// `resumed` and the state the resume reached.
#[test]
fn unfence_resume_non_pty_reports_no_pane() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    fence_agent(&d, "w1", "x1");
    let r = d
        .rpc(
            "agent_unfence",
            json!({"alias": "w1", "status": "interrupted",
                   "resume": true}),
        )
        .unwrap();
    assert_eq!(r["reconciled"], json!(["x1"]), "{r}");
    assert_eq!(r["resumed"], true, "{r}");
    assert!(r.get("pane").is_none(), "{r}");
    d.wait_agent("w1", "idle", 15);
}

/// Adopted does not mean idle: a surviving pane that is visibly busy
/// is still adopted by resume, but sends stay gated until the screen
/// probe sees it idle.
#[test]
fn pty_unfence_resume_busy_adopted_pane_stays_gated() {
    let d = TestDaemon::start();
    let mock = d.mock_stub();
    d.register_stub("st", json!({"auto_ready": "verified"}));
    d.wait_agent("st", "idle", 20);
    // Fence; the pane survives detached.
    atomic_write(d.stub_pane_file(&mock, "st", "swallow"), "1");
    d.rpc("agent_ready", json!({"alias": "st"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "st", "text": "dropped", "message": "m1"}),
    )
    .unwrap();
    d.wait_agent("st", "attention", 25);
    // The surviving pane is visibly busy — adoption still lands.
    atomic_write(d.stub_pane_file(&mock, "st", "tui-state"), "stub working\n");
    let r = d
        .rpc(
            "agent_unfence",
            json!({"alias": "st", "status": "interrupted",
                   "resume": true}),
        )
        .unwrap();
    assert_eq!(r["resumed"], true, "{r}");
    assert_eq!(r["pane"], "adopted", "{r}");
    d.wait_agent("st", "idle", 20);
    // A send without ready must not paste into the busy pane.
    d.rpc(
        "agent_send",
        json!({"alias": "st", "text": "wait for idle", "message": "m2"}),
    )
    .unwrap();
    let wait = d.wait_event("st", "gate_wait", 15);
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("busy"),
        "{wait}"
    );
    assert_eq!(d.message_state("st", "m2"), "queued");
    // Operator clears the pane: busy marker off, the swallowed draft
    // out of the input line, the pane accepting pastes again — then
    // the queued send delivers.
    std::fs::remove_file(d.stub_pane_file(&mock, "st", "tui-state")).unwrap();
    std::fs::remove_file(d.stub_pane_file(&mock, "st", "swallow")).unwrap();
    atomic_write(d.stub_pane_file(&mock, "st", "input"), "");
    pty_token(&d, "st", "m2");
}

/// `dead` and `resumable` answer different questions per endpoint
/// kind: dead = "the surface is gone and this wasn't operator-stopped"
/// (attachable), or fenced/unattended (managed); inbox and fake never
/// die. Resumable = stopped-or-dead with a saved thread and no
/// unreconciled unknowns fencing it.
#[test]
fn agent_dead_and_resumable_per_endpoint_kind() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    // pty fenced: the actor is dead (detached pane, no endpoint) but
    // the unknown still fences it — dead, not resumable.
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    atomic_write(d.pane_file(&_mock, "dv1", "swallow"), "1");
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "dropped", "message": "m1"}),
    )
    .unwrap();
    let agent = d.wait_agent("dv1", "attention", 25);
    // The fence is atomic: the same snapshot that reads `attention`
    // must already carry the cleared endpoint — never a fenced agent
    // holding a live one.
    assert!(agent["endpoint"].is_null(), "{agent}");
    assert_eq!(agent["dead"], true, "{agent}");
    assert_eq!(agent["resumable"], false, "{agent}");
    // Unfence without resume → stopped: not dead, resumable — the
    // recorded thread can be re-attached.
    d.rpc(
        "agent_unfence",
        json!({"alias": "dv1", "status": "interrupted"}),
    )
    .unwrap();
    let agent = d.wait_agent("dv1", "stopped", 10);
    assert_eq!(agent["dead"], false, "{agent}");
    assert_eq!(agent["resumable"], true, "{agent}");
    // Live again: neither.
    d.rpc("agent_resume", json!({"alias": "dv1"})).unwrap();
    let agent = d.wait_agent("dv1", "idle", 20);
    assert_eq!(agent["dead"], false, "{agent}");
    assert_eq!(agent["resumable"], false, "{agent}");
    // A stopped fake: never dead, resumable.
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    let agent = d.wait_agent("w1", "stopped", 15);
    assert_eq!(agent["dead"], false, "{agent}");
    assert_eq!(agent["resumable"], true, "{agent}");
    // A fenced fake: not dead (fake never dies), not resumable while
    // the unknown stands.
    d.register("w2");
    d.wait_agent("w2", "idle", 10);
    fence_agent(&d, "w2", "x1");
    let agent = d.wait_agent("w2", "attention", 10);
    assert_eq!(agent["dead"], false, "{agent}");
    assert_eq!(agent["resumable"], false, "{agent}");
    // An inbox is a mailbox, not a process: never dead, never
    // resumable.
    d.register_inbox("obs");
    let show = d.rpc("agent_show", json!({"alias": "obs"})).unwrap();
    assert_eq!(show["agent"]["dead"], false, "{show}");
    assert_eq!(show["agent"]["resumable"], false, "{show}");
}

/// `agent ready` runs the same probe verified auto-ready runs: a
/// visibly busy pane refuses with the probe's reason. `--force`
/// claims anyway and the event records the override; an idle pane
/// claims as before.
#[test]
fn pty_ready_claim_refuses_busy_pane_unless_forced() {
    let d = TestDaemon::start();
    let mock = d.mock_stub();
    d.register_stub("st", json!({}));
    d.wait_agent("st", "idle", 20);
    // The stub's own busy marker on screen — the claim must refuse.
    atomic_write(d.stub_pane_file(&mock, "st", "tui-state"), "stub working\n");
    let err = d.rpc("agent_ready", json!({"alias": "st"})).unwrap_err();
    assert!(err.to_string().contains("busy"), "{err}");
    // `--force` claims anyway; the busy verdict still rides the event.
    d.rpc("agent_ready", json!({"alias": "st", "force": true}))
        .unwrap();
    let e = d.wait_event("st", "ready_claimed", 10);
    assert_eq!(e["payload"]["forced"], true, "{e}");
    assert_eq!(e["payload"]["probe"]["busy_marker"], true, "{e}");
    // Idle again: a plain claim lands and is not marked forced.
    std::fs::remove_file(d.stub_pane_file(&mock, "st", "tui-state")).unwrap();
    d.rpc("agent_ready", json!({"alias": "st"})).unwrap();
    let claims: Vec<Value> = d
        .events("st")
        .into_iter()
        .filter(|e| e["kind"].as_str() == Some("ready_claimed"))
        .collect();
    let last = claims.last().unwrap();
    assert!(last["payload"]["forced"].is_null(), "{last}");
    assert_eq!(last["payload"]["probe"]["idle"], true, "{last}");
}

/// An unrendered paste preserves its evidence: the normalized screen
/// tail before the paste, the tail after the render deadline, and the
/// probe verdict that admitted the send.
#[test]
fn pty_paste_not_rendered_carries_screen_evidence() {
    let d = TestDaemon::start();
    let mock = d.mock_stub();
    d.register_stub("st", json!({"auto_ready": "verified"}));
    d.wait_agent("st", "idle", 20);
    atomic_write(d.stub_pane_file(&mock, "st", "swallow"), "1");
    d.rpc(
        "agent_send",
        json!({"alias": "st", "text": "gone", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("st", "m1", &["unknown"], 20);
    let e = d.wait_event("st", "paste_not_rendered", 10);
    assert_eq!(e["payload"]["message"], "m1", "{e}");
    assert!(
        e["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("never rendered"),
        "{e}"
    );
    let before = e["payload"]["before"].as_array().unwrap();
    let after = e["payload"]["after"].as_array().unwrap();
    assert!(!before.is_empty() && before.len() <= 12, "{e}");
    assert!(!after.is_empty() && after.len() <= 12, "{e}");
    // The before-tail is what the gate saw: the stub's idle prompt.
    assert!(
        before
            .iter()
            .any(|l| l.as_str().unwrap_or("").contains("stub ready")),
        "{e}"
    );
    // The daemon's own idle probe admitted the send — recorded verdict.
    let probe = &e["payload"]["claim_probe"];
    assert_eq!(probe["idle"], true, "{e}");
}

#[test]
fn pty_render_check_is_differential_not_contains() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);
    // An identical body already rendered once — the screen provably
    // contains the text before the second paste.
    d.rpc(
        "agent_send",
        json!({"alias": "dv", "text": "identical notification body",
               "message": "m1"}),
    )
    .unwrap();
    d.wait_message("dv", "m1", &["running"], 20);
    // Now the pane drops the paste: the body is already on screen, so a
    // `contains` check would pass — the differential check must not.
    atomic_write(d.pane_file(&mock, "dv", "swallow"), "1");
    d.rpc(
        "agent_send",
        json!({"alias": "dv", "text": "identical notification body",
               "message": "m2"}),
    )
    .unwrap();
    d.wait_message("dv", "m2", &["unknown"], 25);
    let e = d.wait_event("dv", "paste_not_rendered", 5);
    assert_eq!(e["payload"]["message"], "m2", "{e}");
}

#[test]
fn pty_rendered_but_not_submitted_is_not_running() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);
    // The TUI renders the paste but swallows Enter — the body sits in
    // the input line as a staged draft. That is not a submission.
    atomic_write(d.pane_file(&mock, "dv", "hold-enter"), "1");
    d.rpc(
        "agent_send",
        json!({"alias": "dv", "text": "staged but unsent", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("dv", "m1", &["unknown"], 25);
    d.wait_agent("dv", "attention", 15);
    let e = d.wait_event("dv", "paste_not_rendered", 5);
    assert!(
        e["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("never submitted"),
        "{e}"
    );
    // The draft was left untouched — no blind second Enter.
    let input = std::fs::read_to_string(d.pane_file(&mock, "dv", "input")).unwrap();
    assert!(input.contains("staged but unsent"), "{input}");
}

#[test]
fn agent_set_rejects_non_allowlisted_params() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv", None);
    d.register("fk");
    d.wait_agent("dv", "idle", 20);
    d.wait_agent("fk", "idle", 10);
    // Wiring keys are not live-settable.
    let e = d
        .rpc(
            "agent_set",
            json!({"alias": "dv", "patch": {"upstream": "x"}}),
        )
        .unwrap_err();
    assert!(e.to_string().contains("auto_ready"), "{e}");
    let e = d
        .rpc(
            "agent_set",
            json!({"alias": "dv", "patch": {"session": "other"}}),
        )
        .unwrap_err();
    assert!(e.to_string().contains("auto_ready"), "{e}");
    // Only "verified" (or removal) is a valid value.
    let e = d
        .rpc(
            "agent_set",
            json!({"alias": "dv", "patch": {"auto_ready": "bogus"}}),
        )
        .unwrap_err();
    assert!(e.to_string().contains("verified"), "{e}");
    // auto_ready only exists on pty endpoints.
    let e = d
        .rpc(
            "agent_set",
            json!({"alias": "fk", "patch": {"auto_ready": "verified"}}),
        )
        .unwrap_err();
    assert!(e.to_string().contains("pty"), "{e}");
    // The allowed operations still work on a live pty agent.
    d.rpc(
        "agent_set",
        json!({"alias": "dv", "patch": {"auto_ready": "verified"}}),
    )
    .unwrap();
    assert_eq!(
        d.rpc("agent_show", json!({"alias": "dv"})).unwrap()["agent"]["params"]["auto_ready"],
        "verified"
    );
    d.rpc(
        "agent_set",
        json!({"alias": "dv", "patch": {"auto_ready": null}}),
    )
    .unwrap();
    assert!(
        d.rpc("agent_show", json!({"alias": "dv"})).unwrap()["agent"]["params"]
            .get("auto_ready")
            .is_none()
    );
}

#[test]
fn pty_unrendered_worker_result_requeues_then_parks() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin_opts("pm", json!({"auto_ready": "verified"}));
    d.wait_agent("pm", "idle", 20);
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    // Pane swallows before the routed notification lands.
    atomic_write(d.pane_file(&mock, "pm", "swallow"), "1");
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "work", "message": "j1",
               "reply_to": "pm"}),
    )
    .unwrap();
    d.wait_message("w1", "j1", &["completed"], 15);
    let routed_id = d.rpc("agent_show", json!({"alias": "pm"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["source"] == "worker_result")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    // A dropped paste can leave the real TUI briefly showing its busy
    // interrupt hint before the next retry. This gate refusal must not reset
    // the routed delivery's render-miss budget.
    d.wait_event_where(
        "pm",
        "paste_not_rendered",
        |event| event["payload"]["message"] == routed_id && event["payload"]["attempt"] == 1,
        20,
    );
    atomic_write(
        d.pane_file(&mock, "pm", "tui-state"),
        "⠸ Thinking · 12s (esc twice to interrupt)\n❭ Guide Devin while it works\n",
    );
    let gate = d.wait_event_where(
        "pm",
        "gate_wait",
        |event| {
            event["payload"]["message"] == routed_id
                && event["payload"]["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("busy"))
        },
        15,
    );
    assert!(gate["payload"]["reason"].as_str().unwrap().contains("busy"));
    std::fs::remove_file(d.pane_file(&mock, "pm", "tui-state")).unwrap();
    // At-least-once: bounded requeues with paste_not_rendered evidence,
    // then the delivery is PARKED — a notification must never fence the
    // recipient or kill its pane.
    d.wait_message("pm", &routed_id, &["failed"], 90);
    let parked = d
        .events("pm")
        .into_iter()
        .find(|e| e["kind"].as_str() == Some("delivery_parked"))
        .expect("no delivery_parked event");
    assert_eq!(parked["payload"]["message"], routed_id);
    assert_eq!(parked["payload"]["attempts"], 4, "{parked}");
    let misses: Vec<Value> = d
        .events("pm")
        .into_iter()
        .filter(|e| e["kind"].as_str() == Some("paste_not_rendered"))
        .collect();
    assert_eq!(misses.len(), 4, "{:?}", misses);
    let attempts: Vec<Value> = misses
        .iter()
        .map(|event| event["payload"]["attempt"].clone())
        .collect();
    assert_eq!(attempts, vec![json!(1), json!(2), json!(3), json!(4)]);
    assert!(misses.iter().take(3).all(|e| e["payload"]["retry"] == true));
    let show = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    let msg = show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == routed_id)
        .unwrap();
    assert_eq!(msg["result"]["via"], "pty_render_miss", "{msg}");
    // The PM stays alive and idle — the pane is still owned, and the
    // next message still delivers once the pane renders again.
    d.wait_agent("pm", "idle", 15);
    assert_eq!(
        d.rpc("agent_show", json!({"alias": "pm"})).unwrap()["agent"]["dead"],
        false
    );
    std::fs::remove_file(d.pane_file(&mock, "pm", "swallow")).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "pm", "text": "still alive", "message": "after"}),
    )
    .unwrap();
    // Delivered = render-verified `running` (a task then awaits an
    // explicit report, so the agent correctly stays busy on it).
    d.wait_message("pm", "after", &["running"], 20);
    emit_park_phase_trace(
        &d,
        "pty_unrendered_worker_result_requeues_then_parks",
        "pm",
        &routed_id,
    );
}

#[test]
fn agent_set_opts_live_agent_into_auto_ready() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv", None);
    d.wait_agent("dv", "idle", 20);
    // Without opt-in the queue still waits on a human claim. Since
    // CAD-245 the actor wakes on the send at once, so a fixed sleep can
    // land mid-attempt (`submitting` while the gate probes). Wait for
    // the recorded refusal; the requeued message then sits out the
    // gate back-off (CAD-266).
    d.rpc(
        "agent_send",
        json!({"alias": "dv", "text": "gated", "message": "m1"}),
    )
    .unwrap();
    d.wait_event("dv", "gate_wait", 20);
    assert_eq!(d.message_state("dv", "m1"), "queued");
    // Retrofit via agent_set — the running actor reads params per send.
    d.rpc(
        "agent_set",
        json!({"alias": "dv", "patch": {"auto_ready": "verified"}}),
    )
    .unwrap();
    d.wait_message("dv", "m1", &["running"], 20);
    let show = d.rpc("agent_show", json!({"alias": "dv"})).unwrap();
    assert_eq!(show["agent"]["params"]["auto_ready"], "verified");
}

// ==== managed claude (stream-json) endpoint ====

/// A mock Claude stream-json provider over real stdio — speaks the
/// observed wire: per user line it emits `system/init`, an `assistant`
/// event, then one `result`. argv is `<script> <pidfile> <mode>` then
/// the real CLI flags appended by the adapter (`--session-id|--resume`,
/// `--permission-mode`, `--allowedTools`, `--model`) — recorded to
/// `<pidfile>.argv` for launch-shape assertions. The pane env lands in
/// `<pidfile>.env` for scrub/injection checks.
///
/// Modes:
///   ok               — result success, text `MOCK_OK:<prompt>`
///   fail             — result error_during_execution, is_error, errors[]
///   deny             — result success carrying a non-empty
///                      permission_denials array
///   die              — exits on the first user message (mid-turn death)
///   bad-session      — init reports a session id that is not argv's
///   await-interrupt  — no result until SIGINT, then an interrupted one
///   replay           — replays the `<pidfile>.fixture` events verbatim,
///                      rewriting session_id fields to the argv id
///   heartbeat        — activity every ~0.3s for ~3.6s, then success —
///                      a turn longer than a short idle window
///   silent           — init, then nothing; stays alive (idle fence)
///   chatty           — activity every ~0.3s forever, never a result
///                      (absolute-cap fence)
///   tooluse          — one assistant tool_use block, then success
///   permit           — asks the configured `--mcp-config` server to
///                      approve a Bash call for the prompt; allow →
///                      MOCK_OK, deny → DENIED:<message> plus a
///                      permission_denials entry. The verdict lands in
///                      `<pidfile>.verdict` too, so tests can observe a
///                      denial even after the daemon is gone.
const MOCK_CLAUDE_PY: &str = r#"
import json, os, signal, subprocess, sys, time

pidfile = sys.argv[1]
# Mode travels in the pidfile basename — the daemon scrubs CADENCE_*
# from the child env, so an env var would never arrive.
mode = os.path.basename(pidfile).removeprefix("claude-").removesuffix(".pid")
argv = sys.argv[2:]
sid = ""
for i, a in enumerate(argv):
    if a in ("--session-id", "--resume") and i + 1 < len(argv):
        sid = argv[i + 1]
with open(pidfile, "w") as f:
    f.write(str(os.getpid()))
# Atomic dump — a reader between truncate and write must never see a
# torn file; the same temp+rename shape .env uses below.
argv_tmp = pidfile + ".argv.tmp"
with open(argv_tmp, "w") as f:
    f.write("\n".join(sys.argv))
os.rename(argv_tmp, pidfile + ".argv")
env_tmp = pidfile + ".env.tmp"
with open(env_tmp, "w") as f:
    for k in sorted(os.environ):
        f.write("%s=%s\n" % (k, os.environ[k]))
os.rename(env_tmp, pidfile + ".env")

count = [0]

def emit(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()

def result(**kw):
    count[0] += 1
    base = {"type": "result", "session_id": sid, "num_turns": 1,
            "total_cost_usd": 0.001, "result_index": count[0] - 1}
    base.update(kw)
    emit(base)

def init():
    # bad-session reports a session the process was NOT opened with.
    reported = "00000000-foreign-session" if current_mode() == "bad-session" else sid
    emit({"type": "system", "subtype": "init", "session_id": reported,
          "model": "mock-claude", "tools": []})

def on_sigint(signum, frame):
    init()
    result(subtype="interrupted", is_error=False, result="INTERRUPTED",
           stop_reason="interrupted")

signal.signal(signal.SIGINT, on_sigint)

# The fixture path rides a sidecar like `.mode` — never the env, which
# every concurrent test's mock child would inherit.
fixture = open(pidfile + ".fixture").read().strip() if os.path.exists(pidfile + ".fixture") else None
fixture_lines = open(fixture).read().splitlines() if fixture else []

def current_mode():
    # <pidfile>.mode overrides the env mode per message — lets a test
    # switch a resumed provider from "die" to "ok".
    try:
        return open(pidfile + ".mode").read().strip()
    except FileNotFoundError:
        return mode

# ---- brokered permission flow (permit mode) ----
mcp_proc = None
mcp_next_id = [0]

def mcp_server():
    """Spawn the `--mcp-config` server once, like the real CLI: env
    from the config overlays ours, then initialize/initialized."""
    global mcp_proc
    if mcp_proc is not None:
        return mcp_proc
    cfg_path = None
    for i, a in enumerate(argv):
        if a == "--mcp-config" and i + 1 < len(argv):
            cfg_path = argv[i + 1]
    if cfg_path is None:
        return None
    srv = json.load(open(cfg_path))["mcpServers"]["cadence"]
    env = dict(os.environ)
    env.update(srv.get("env", {}))
    proc = subprocess.Popen([srv["command"]] + srv["args"],
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            env=env, text=True, bufsize=1)
    mcp_proc = proc
    mcp_rpc(proc, "initialize",
            {"protocolVersion": "2025-11-25", "capabilities": {},
             "clientInfo": {"name": "mock-claude", "version": "0"}})
    proc.stdin.write(json.dumps(
        {"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
    proc.stdin.flush()
    mcp_rpc(proc, "tools/list", {})
    return proc

def mcp_rpc(proc, method, params):
    mcp_next_id[0] += 1
    proc.stdin.write(json.dumps(
        {"jsonrpc": "2.0", "id": mcp_next_id[0],
         "method": method, "params": params}) + "\n")
    proc.stdin.flush()
    line = proc.stdout.readline()
    if not line:
        raise RuntimeError("mcp server exited")
    return json.loads(line)

def record_verdict(verdict):
    tmp = pidfile + ".verdict.tmp"
    with open(tmp, "w") as f:
        f.write(json.dumps(verdict))
    os.rename(tmp, pidfile + ".verdict")

def ask_permission(command_text):
    """One approve call — returns the verdict object the server put
    inside the text content block (allow/deny), or a local deny when
    the broker is unreachable."""
    proc = mcp_server()
    if proc is None:
        verdict = {"behavior": "deny",
                   "message": "no --mcp-config on argv"}
        record_verdict(verdict)
        return verdict
    try:
        resp = mcp_rpc(proc, "tools/call",
                       {"name": "approve",
                        "arguments": {"tool_name": "Bash",
                                      "input": {"command": command_text},
                                      "tool_use_id": "tu_permit_%d"
                                      % mcp_next_id[0]}})
        text = resp["result"]["content"][0]["text"]
        verdict = json.loads(text)
    except Exception as e:
        verdict = {"behavior": "deny", "message": "mcp call failed: %s" % e}
    record_verdict(verdict)
    return verdict

for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    if msg.get("type") != "user":
        continue
    mode_now = current_mode()
    content = msg["message"]["content"]
    text = content if isinstance(content, str) else \
        " ".join(b.get("text", "") for b in content)
    if mode_now == "die":
        os._exit(0)
    if mode_now == "replay":
        for raw in fixture_lines:
            try:
                ev = json.loads(raw)
            except Exception:
                continue
            if "session_id" in ev:
                ev["session_id"] = sid
            emit(ev)
        continue
    init()
    emit({"type": "assistant",
          "message": {"role": "assistant",
                      "content": [{"type": "text", "text": "working"}]},
          "session_id": sid})
    if mode_now == "await-interrupt":
        continue  # the SIGINT handler emits the result
    if mode_now == "silent":
        while True:
            time.sleep(5)  # alive but eventless — the idle fence path
    if mode_now == "chatty":
        while True:
            emit({"type": "assistant",
                  "message": {"role": "assistant",
                              "content": [{"type": "text", "text": "."}]},
                  "session_id": sid})
            time.sleep(0.3)
    if mode_now == "heartbeat":
        for _ in range(12):
            emit({"type": "assistant",
                  "message": {"role": "assistant",
                              "content": [{"type": "text", "text": "."}]},
                  "session_id": sid})
            time.sleep(0.3)
    if mode_now == "tooluse":
        emit({"type": "assistant",
              "message": {"role": "assistant",
                          "content": [{"type": "tool_use", "name": "Bash",
                                       "input": {"command": "true"}}]},
              "session_id": sid})
    if mode_now == "fail":
        result(subtype="error_during_execution", is_error=True,
               errors=["mock exploded"], stop_reason="error")
        continue
    if mode_now == "permit":
        # The real CLI blocks on the permission-prompt tool here — one
        # approve call per tool use; the verdict decides the outcome.
        verdict = ask_permission(text)
        if verdict.get("behavior") == "allow":
            result(subtype="success", is_error=False,
                   result="MOCK_OK:" + text, stop_reason="end_turn",
                   permission_denials=[])
        else:
            message = verdict.get("message", "denied")
            denials = [{"tool_name": "Bash",
                        "tool_use_id": "tu_permit",
                        "tool_input": {"command": text},
                        "message": message}]
            result(subtype="success", is_error=False,
                   result="DENIED:" + message, stop_reason="end_turn",
                   permission_denials=denials)
        continue
    denials = []
    if mode_now == "deny":
        denials = [{"tool_name": "Bash", "tool_use_id": "tu_1",
                    "tool_input": {"command": "touch /tmp/x"}}]
    result(subtype="success", is_error=False, result="MOCK_OK:" + text,
           stop_reason="end_turn", permission_denials=denials)
"#;

struct MockClaude {
    pidfile: PathBuf,
}

impl TestDaemon {
    /// Install a mock claude command for `mode` (optionally replaying
    /// `fixture`), returning its pidfile path.
    fn mock_claude(&self, mode: &str, fixture: Option<&Path>) -> MockClaude {
        let pidfile = self.dir.path().join(format!("claude-{mode}.pid"));
        let script = self.dir.path().join(format!("claude-{mode}.py"));
        std::fs::write(&script, MOCK_CLAUDE_PY).unwrap();
        test_env().set(
            "CADENCE_CLAUDE_COMMAND",
            format!("python3 {} {}", script.display(), pidfile.display()),
        );
        if let Some(f) = fixture {
            std::fs::write(pidfile.with_extension("pid.fixture"), f.to_str().unwrap()).unwrap();
        }
        MockClaude { pidfile }
    }

    fn register_claude(&self, alias: &str, params: Value) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        let params = (!params.is_null()).then(|| params.to_string());
        self.rpc(
            "agent_register",
            json!({"alias": alias, "provider": "claude",
                   "endpoint_kind": "managed", "cwd": cwd,
                   "params": params}),
        )
        .unwrap();
    }

    /// Poll `agent_requests` until one request is pending (bounded).
    fn wait_request(&self, alias: &str, secs: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let requests = self.rpc("agent_requests", json!({"alias": alias})).unwrap()["requests"]
                .as_array()
                .unwrap()
                .clone();
            if let Some(req) = requests.first() {
                return req.clone();
            }
            assert!(
                Instant::now() < deadline,
                "agent {alias} never showed a pending request"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Pending request handles for an alias right now.
    fn requests(&self, alias: &str) -> Vec<Value> {
        self.rpc("agent_requests", json!({"alias": alias})).unwrap()["requests"]
            .as_array()
            .unwrap()
            .clone()
    }
}

impl Drop for MockClaude {
    fn drop(&mut self) {
        test_env().remove("CADENCE_CLAUDE_COMMAND");
        test_env().remove("CADENCE_MCP_PERMISSION_COMMAND");
    }
}

#[test]
fn claude_turn_completes_and_routes_to_inbox_pm() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude("ok", None);
    d.register_inbox("pm");
    d.register_claude("w1", json!({"upstream": "pm"}));
    d.wait_agent("w1", "idle", 15);
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    // A fresh open mints the session id that --session-id carries.
    let sid = show["agent"]["session_id"].as_str().unwrap().to_string();
    assert_eq!(sid.len(), 36, "{sid}");
    assert_eq!(show["agent"]["endpoint_kind"], "managed");
    assert!(show["agent"]["endpoint"].is_null(), "{show}");
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "Reply with exactly: PONG", "message": "m1"}),
    )
    .unwrap();
    let m1 = d.wait_message("w1", "m1", &["completed"], 20);
    assert_eq!(
        m1["result"]["text"], "MOCK_OK:Reply with exactly: PONG",
        "{m1}"
    );
    assert_eq!(
        m1["result"]["turn_id"].as_str().unwrap()[..6].to_string(),
        "claude"
    );
    // The upstream PM receives the routed worker_result.
    let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    let routed = pm["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["source"].as_str() == Some("worker_result"))
        .cloned();
    let routed = routed.unwrap_or_else(|| panic!("no routed result on pm: {pm}"));
    assert!(
        routed["body"].as_str().unwrap().contains("MOCK_OK"),
        "{routed}"
    );
}

#[test]
fn claude_result_routes_to_pty_pm() {
    let d = TestDaemon::start();
    let pm_mock = d.mock_devin();
    let _worker_mock = d.mock_claude("ok", None);
    d.register_devin("pm", None);
    d.wait_agent("pm", "idle", 20);
    d.register_claude("w1", json!({"upstream": "pm"}));
    d.wait_agent("w1", "idle", 15);
    // Claim the pty gate so the routed result may be pasted.
    d.rpc("agent_ready", json!({"alias": "pm"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "hi", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("w1", "m1", &["completed"], 20);
    // The routed copy on the PM pane completes on delivery (is_routed).
    let routed = {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
            if let Some(m) = pm["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["source"].as_str() == Some("worker_result"))
            {
                break m.clone();
            }
            assert!(Instant::now() < deadline, "no routed result on pm: {pm}");
            thread::sleep(Duration::from_millis(50));
        }
    };
    d.wait_message("pm", routed["id"].as_str().unwrap(), &["completed"], 20);
    let screen = std::fs::read_to_string(d.pane_file(&pm_mock, "pm", "screen")).unwrap_or_default();
    assert!(screen.contains("MOCK_OK"), "{screen}");
}

#[test]
fn claude_failed_result_fails_message() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude("fail", None);
    d.register_claude("w1", Value::Null);
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "hi", "message": "m1"}),
    )
    .unwrap();
    // A clean provider-side error result is a definitive answer —
    // failed, never unknown.
    let m1 = d.wait_message("w1", "m1", &["failed"], 20);
    assert!(
        m1["result"]["error"]
            .as_str()
            .unwrap_or("")
            .contains("mock exploded"),
        "{m1}"
    );
    // The endpoint survives a failed turn — next message still works.
    d.wait_agent("w1", "idle", 10);
}

#[test]
fn claude_death_mid_turn_unknown_then_unfence_resume() {
    let d = TestDaemon::start();
    let mock = d.mock_claude("die", None);
    d.register_claude("w1", Value::Null);
    d.wait_agent("w1", "idle", 15);
    let sid = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"]["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "hi", "message": "m1"}),
    )
    .unwrap();
    // EOF before any result event — outcome unknowable, fail closed.
    d.wait_message("w1", "m1", &["unknown"], 20);
    d.wait_agent("w1", "attention", 10);
    // The same mock command relaunches; flip it to "ok" for the resume.
    std::fs::write(mock.pidfile.with_extension("pid.mode"), "ok").unwrap();
    let unfenced = d
        .rpc(
            "agent_unfence",
            json!({"alias": "w1", "status": "interrupted"}),
        )
        .unwrap();
    // Reconcile leaves the agent stopped; resume is the explicit step.
    assert_eq!(unfenced["state"], "stopped", "{unfenced}");
    d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "idle", 15);
    // `idle` only proves the transport opened — under load the
    // relaunched mock can still be booting, so `.argv` may still hold
    // the first launch's flags. A completed turn is the cause ordered
    // after the dump: init/result emits mean the script exec'd.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "again", "message": "m2"}),
    )
    .unwrap();
    d.wait_message("w1", "m2", &["completed"], 20);
    // Resume relaunched on the SAME session id via --resume.
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(agent["session_id"].as_str().unwrap(), sid);
    let argv = std::fs::read_to_string(mock.pidfile.with_extension("pid.argv")).unwrap();
    assert!(argv.contains(&format!("--resume\n{sid}")), "{argv}");
}

#[test]
fn claude_session_mismatch_fences_attention() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude("bad-session", None);
    d.register_claude("w1", Value::Null);
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "hi", "message": "m1"}),
    )
    .unwrap();
    // init reported a foreign session — the turn fails and the agent
    // fences with session-mismatch wording.
    d.wait_agent("w1", "attention", 20);
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    let err = agent["error"].as_str().unwrap_or_default();
    assert!(err.contains("owns session"), "{err}");
    d.wait_message("w1", "m1", &["failed"], 10);
}

#[test]
fn claude_interrupt_yields_interrupted() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude("await-interrupt", None);
    d.register_claude("w1", Value::Null);
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "hi", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("w1", "m1", &["running"], 15);
    // Stop interrupts first: the mock emits an interrupted result, so
    // the message lands `interrupted` — never fenced unknown.
    let stopped = d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    assert_eq!(stopped["state"], "stopped");
    d.wait_message("w1", "m1", &["interrupted"], 10);
}

#[test]
fn claude_denials_complete_with_event() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude("deny", None);
    d.register_claude("w1", Value::Null);
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "hi", "message": "m1"}),
    )
    .unwrap();
    // permission_denials is not a failure — the turn still completes,
    // and the denial is recorded as an auditable event.
    d.wait_message("w1", "m1", &["completed"], 20);
    let denied = d.wait_event("w1", "permission_denied", 10);
    assert_eq!(
        denied["payload"]["denials"][0]["tool_name"].as_str(),
        Some("Bash"),
        "{denied}"
    );
    // Result metadata is recorded too (cost accounting source).
    d.wait_event("w1", "claude_result", 5);
}

#[test]
fn claude_env_injected_and_scrubbed() {
    let d = TestDaemon::start();
    // Real process env is mutated here — hold ENV_LOCK across the
    // mutation + spawn so no other env-setting test interleaves.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Scrub by rule: every CLAUDE_*/CLAUDECODE/CODEX_*/CADENCE_* name a
    // parent session (or a test override) could leak is removed — except
    // the documented keep-list. ANTHROPIC_* auth is never touched.
    for (k, v) in [
        ("CLAUDECODE", "1"),
        ("CLAUDE_CODE_EXECPATH", "/usr/bin/claude"),
        ("CLAUDE_CODE_SUBAGENT_MODEL", "sonnet"),
        ("CLAUDE_EFFORT", "high"),
        ("CLAUDE_PID", "4242"),
        ("CLAUDE_CODE_SESSION_ID", "stale-parent-sid"),
        ("CODEX_THREAD_ID", "stale-thread"),
        ("CADENCE_CLAUDE_MODE", "leak"),
        // keep-list: operator-set on purpose, must survive
        ("CLAUDE_CONFIG_DIR", "/tmp/claude-cfg"),
        ("CLAUDE_CODE_OAUTH_TOKEN", "tok-keep"),
        ("ANTHROPIC_API_KEY", "sk-keep"),
    ] {
        std::env::set_var(k, v);
    }
    let mock = d.mock_claude("ok", None);
    d.register_claude("w1", Value::Null);
    d.wait_agent("w1", "idle", 15);
    for k in [
        "CLAUDECODE",
        "CLAUDE_CODE_EXECPATH",
        "CLAUDE_CODE_SUBAGENT_MODEL",
        "CLAUDE_EFFORT",
        "CLAUDE_PID",
        "CLAUDE_CODE_SESSION_ID",
        "CODEX_THREAD_ID",
        "CADENCE_CLAUDE_MODE",
        "CLAUDE_CONFIG_DIR",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ANTHROPIC_API_KEY",
    ] {
        std::env::remove_var(k);
    }
    // The mock writes its env dump at process start, before any
    // protocol emit — `idle` only means the actor's transport opened.
    // A completed turn is the cause ordered after the dump.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "boot", "message": "m-env"}),
    )
    .unwrap();
    d.wait_message("w1", "m-env", &["completed"], 20);
    let env = std::fs::read_to_string(mock.pidfile.with_extension("pid.env")).unwrap();
    assert!(env.contains("CADENCE_ALIAS=w1\n"), "{env}");
    assert!(
        env.contains(&format!("CADENCE_STATE_DIR={}\n", d.state.display())),
        "{env}"
    );
    for leaked in [
        "CLAUDECODE=",
        "CLAUDE_CODE_EXECPATH=",
        "CLAUDE_CODE_SUBAGENT_MODEL=",
        "CLAUDE_EFFORT=",
        "CLAUDE_PID=",
        "CLAUDE_CODE_SESSION_ID=",
        "CODEX_THREAD_ID=",
        "CADENCE_CLAUDE_COMMAND=",
        "CADENCE_CLAUDE_MODE=",
    ] {
        assert!(!env.contains(leaked), "{leaked} leaked into child:\n{env}");
    }
    // The keep-list and auth variables survive untouched.
    assert!(env.contains("CLAUDE_CONFIG_DIR=/tmp/claude-cfg\n"), "{env}");
    assert!(env.contains("CLAUDE_CODE_OAUTH_TOKEN=tok-keep\n"), "{env}");
    assert!(env.contains("ANTHROPIC_API_KEY=sk-keep\n"), "{env}");
}

#[test]
fn claude_params_replayed_on_resume() {
    let d = TestDaemon::start();
    let mock = d.mock_claude("ok", None);
    d.register_claude(
        "w1",
        json!({"permission_mode": "acceptEdits",
               "allowed_tools": ["Bash(git *)", "Read"],
               "model": "haiku",
               "turn_idle_secs": 5,
               "turn_max_secs": 3600}),
    );
    d.wait_agent("w1", "idle", 15);
    // `idle` means the actor's transport opened — the mock may not have
    // exec'd its script and written the argv dump yet under load. A
    // completed turn is the cause ordered after the dump: init/result
    // emits mean the script ran.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "boot", "message": "m-boot"}),
    )
    .unwrap();
    d.wait_message("w1", "m-boot", &["completed"], 20);
    let argv_file = mock.pidfile.with_extension("pid.argv");
    let argv1 = std::fs::read_to_string(&argv_file).unwrap();
    assert!(argv1.contains("--session-id"), "{argv1}");
    assert!(argv1.contains("--permission-mode\nacceptEdits"), "{argv1}");
    assert!(argv1.contains("--allowedTools\nBash(cadence *)"), "{argv1}");
    assert!(argv1.contains("--allowedTools\nBash(git *)"), "{argv1}");
    assert!(argv1.contains("--allowedTools\nRead"), "{argv1}");
    assert!(argv1.contains("--model\nhaiku"), "{argv1}");
    let sid = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"]["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "idle", 15);
    // Same spawn/write gap on the resumed generation — and the file
    // still holds the first launch's argv until the resumed mock
    // rewrites it. Another completed turn orders after the rewrite.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "boot2", "message": "m-boot2"}),
    )
    .unwrap();
    d.wait_message("w1", "m-boot2", &["completed"], 20);
    let argv2 = std::fs::read_to_string(&argv_file).unwrap();
    // Resume replays the same permission/model params verbatim and
    // resumes the stored session — it never mints a fresh one.
    assert!(argv2.contains(&format!("--resume\n{sid}")), "{argv2}");
    assert!(argv2.contains("--permission-mode\nacceptEdits"), "{argv2}");
    assert!(argv2.contains("--allowedTools\nBash(git *)"), "{argv2}");
    assert!(argv2.contains("--model\nhaiku"), "{argv2}");
    // The turn-liveness params persist on the agent row across resume.
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(agent["params"]["turn_idle_secs"], 5, "{agent}");
    assert_eq!(agent["params"]["turn_max_secs"], 3600, "{agent}");
}

/// CAD-88: `effort` rides the launch line like `model`, the stream's
/// init model is reported beside the configured params, and `agent set
/// --next-launch` changes model/effort for the next open only.
#[test]
fn claude_effort_next_launch_and_model_reported() {
    let d = TestDaemon::start();
    let mock = d.mock_claude("ok", None);
    d.register_claude("w1", json!({"effort": "low"}));
    d.wait_agent("w1", "idle", 15);
    // A completed turn orders after the argv dump and the init event.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "boot", "message": "m-boot"}),
    )
    .unwrap();
    d.wait_message("w1", "m-boot", &["completed"], 20);
    let argv_file = mock.pidfile.with_extension("pid.argv");
    let argv1 = std::fs::read_to_string(&argv_file).unwrap();
    assert!(argv1.contains("--effort\nlow"), "{argv1}");
    assert!(!argv1.contains("--model"), "{argv1}");
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(agent["model_reported"], "mock-claude", "{agent}");
    assert!(agent["model_configured"].is_null(), "{agent}");
    assert_eq!(agent["model_source"], "provider default", "{agent}");
    assert_eq!(agent["effort"], "low", "{agent}");
    let row = d.rpc("agent_list", json!({})).unwrap()["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"] == "w1")
        .cloned()
        .unwrap();
    assert_eq!(row["model_reported"], "mock-claude", "{row}");
    assert_eq!(row["model_source"], "provider default", "{row}");

    // Without --next-launch the live-set refusal is unchanged.
    let err = d
        .rpc(
            "agent_set",
            json!({"alias": "w1", "patch": {"model": "opus"}}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("not live-settable"), "{err}");
    // With it (through the CLI): stored, the live process untouched.
    let pid = agent["pid"].clone();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "agent",
            "set",
            "w1",
            "model=opus",
            "effort=high",
            "--next-launch",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let reply: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reply["applies"], "next launch", "{reply}");
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(agent["pid"], pid, "live process must not change: {agent}");
    assert_eq!(agent["state"], "idle", "{agent}");
    assert_eq!(agent["model_configured"], "opus", "{agent}");
    assert_eq!(agent["model_source"], "configured", "{agent}");
    assert_eq!(agent["effort"], "high", "{agent}");
    assert_eq!(std::fs::read_to_string(&argv_file).unwrap(), argv1);

    // stop + resume picks both up; a turn orders after the rewrite.
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "boot2", "message": "m-boot2"}),
    )
    .unwrap();
    d.wait_message("w1", "m-boot2", &["completed"], 20);
    let argv2 = std::fs::read_to_string(&argv_file).unwrap();
    assert!(argv2.contains("--resume\n"), "{argv2}");
    assert!(argv2.contains("--model\nopus"), "{argv2}");
    assert!(argv2.contains("--effort\nhigh"), "{argv2}");

    // A bare key clears back to the provider default for the next open.
    d.rpc(
        "agent_set",
        json!({"alias": "w1", "patch": {"model": null}, "next_launch": true}),
    )
    .unwrap();
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(agent["model_source"], "provider default", "{agent}");
}

/// CAD-88: a bad effort level is refused at register, at the CLI and
/// by `--next-launch`, naming the allowed values; `--next-launch` takes
/// model and effort only.
#[test]
fn claude_effort_validated() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude("ok", None);
    let cwd = d.dir.path().to_str().unwrap().to_string();
    let err = d
        .rpc(
            "agent_register",
            json!({"alias": "bad", "provider": "claude", "endpoint_kind": "managed",
                   "cwd": cwd, "params": json!({"effort": "extreme"}).to_string()}),
        )
        .unwrap_err()
        .to_string();
    for level in ["low", "medium", "high", "xhigh", "max"] {
        assert!(err.contains(level), "{level} missing: {err}");
    }
    for verb in [&["claude"][..], &["join", "pm", "claude"][..]] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&d.state)
            .args(verb)
            .args(["--effort", "extreme", "--detach"])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{verb:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("xhigh"), "{verb:?}: {stderr}");
    }
    d.register_claude("w1", Value::Null);
    d.wait_agent("w1", "idle", 15);
    for (patch, want) in [
        (json!({"effort": "extreme"}), "xhigh"),
        (
            json!({"upstream": "pm"}),
            "cannot be set for the next launch",
        ),
        (json!({"model": ""}), "non-empty"),
    ] {
        let err = d
            .rpc(
                "agent_set",
                json!({"alias": "w1", "patch": patch, "next_launch": true}),
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains(want), "{want} missing: {err}");
    }
}

#[test]
fn claude_replay_fixture_turn() {
    let d = TestDaemon::start();
    // Replay a real captured stream: init + assistant + success result.
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/turn1.jsonl");
    let _mock = d.mock_claude("replay", Some(&fixture));
    d.register_claude("w1", Value::Null);
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "hi", "message": "m1"}),
    )
    .unwrap();
    let m1 = d.wait_message("w1", "m1", &["completed"], 20);
    assert_eq!(m1["result"]["text"], "PONG", "{m1}");
    // The real capture carries a cost figure — recorded as an event.
    let ev = d.wait_event("w1", "claude_result", 10);
    assert!(
        ev["payload"]["total_cost_usd"].as_f64().unwrap() > 0.0,
        "{ev}"
    );
}

#[test]
fn claude_replay_failed_fixture() {
    let d = TestDaemon::start();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/fail-session.jsonl");
    let _mock = d.mock_claude("replay", Some(&fixture));
    d.register_claude("w1", Value::Null);
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "hi", "message": "m1"}),
    )
    .unwrap();
    let m1 = d.wait_message("w1", "m1", &["failed"], 20);
    assert!(
        m1["result"]["error"]
            .as_str()
            .unwrap_or("")
            .contains("No conversation found"),
        "{m1}"
    );
}

#[test]
fn claude_respond_is_rejected_naming_opt_ups() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude("ok", None);
    d.register_claude("w1", Value::Null);
    d.wait_agent("w1", "idle", 15);
    let err = d
        .rpc(
            "agent_respond",
            json!({"alias": "w1", "request": "req-1", "decision": "accept"}),
        )
        .unwrap_err()
        .to_string();
    // The hint names real cadence opt-ups, not flags that don't exist.
    assert!(err.contains("--permission-mode"), "{err}");
    assert!(err.contains("--allow"), "{err}");
    assert!(err.contains("--bypass"), "{err}");
    assert!(err.contains("permission_denied"), "{err}");
}

#[test]
fn claude_tool_use_events_recorded() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude("tooluse", None);
    d.register_claude("w1", Value::Null);
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "hi", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("w1", "m1", &["completed"], 20);
    // Lifecycle envelope: the tool name lands as a compact event —
    // no arguments, no transcript text.
    let ev = d.wait_event("w1", "tool_use", 10);
    assert_eq!(ev["payload"]["tool"], "Bash", "{ev}");
    assert!(ev["payload"].get("input").is_none(), "{ev}");
    assert!(ev["payload"].get("command").is_none(), "{ev}");
}

#[test]
fn claude_idle_window_counts_activity() {
    let d = TestDaemon::start();
    // heartbeat: an event every ~0.3s for ~3.6s — longer than the 2s
    // idle window, but never silent — must complete, not fence.
    let _mock = d.mock_claude("heartbeat", None);
    d.register_claude("w1", json!({"turn_idle_secs": 2}));
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "hi", "message": "m1"}),
    )
    .unwrap();
    let m1 = d.wait_message("w1", "m1", &["completed"], 30);
    assert!(
        m1["result"]["text"]
            .as_str()
            .unwrap_or("")
            .contains("MOCK_OK"),
        "{m1}"
    );
}

#[test]
fn claude_silent_turn_fences_unknown() {
    let d = TestDaemon::start();
    // silent: alive but eventless — the idle window declares unknown.
    let _mock = d.mock_claude("silent", None);
    d.register_claude("w1", json!({"turn_idle_secs": 2}));
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "hi", "message": "m1"}),
    )
    .unwrap();
    let m1 = d.wait_message("w1", "m1", &["unknown"], 30);
    assert!(
        m1["error"]
            .as_str()
            .unwrap_or("")
            .contains("No provider event"),
        "{m1}"
    );
    d.wait_agent("w1", "attention", 15);
}

#[test]
fn claude_max_turn_fences_chatty() {
    let d = TestDaemon::start();
    // chatty: activity every ~0.3s forever — the absolute cap still
    // fences it (idle window alone never fires on a chatty turn).
    let _mock = d.mock_claude("chatty", None);
    d.register_claude("w1", json!({"turn_idle_secs": 30, "turn_max_secs": 2}));
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "hi", "message": "m1"}),
    )
    .unwrap();
    let m1 = d.wait_message("w1", "m1", &["unknown"], 30);
    assert!(
        m1["error"].as_str().unwrap_or("").contains("turn_max_secs"),
        "{m1}"
    );
    d.wait_agent("w1", "attention", 15);
}

// ==== brokered claude permissions (cadence mcp-permission) ====

/// Point the daemon's generated `--mcp-config` at the real cadence
/// binary — `current_exe` is the test binary without the override.
fn broker_command() {
    test_env().set(
        "CADENCE_MCP_PERMISSION_COMMAND",
        env!("CARGO_BIN_EXE_cadence"),
    );
}

/// A spawned `cadence mcp-permission` talking stdio — drives the real
/// server binary directly for wire-shape and restart assertions.
struct Mcp {
    child: std::process::Child,
    next_id: u64,
}

impl Mcp {
    fn spawn(state: &Path, alias: &str, timeout_secs: u64) -> Self {
        let child = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .args(["mcp-permission"])
            .env("CADENCE_STATE_DIR", state)
            .env("CADENCE_ALIAS", alias)
            .env("CADENCE_PERMISSION_TIMEOUT_SECS", timeout_secs.to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut mcp = Self { child, next_id: 0 };
        mcp.rpc(
            "initialize",
            json!({"protocolVersion": "2025-11-25", "capabilities": {},
                   "clientInfo": {"name": "test", "version": "0"}}),
        );
        mcp.notify("notifications/initialized");
        mcp
    }

    fn notify(&mut self, method: &str) {
        use std::io::Write;
        let line = json!({"jsonrpc": "2.0", "method": method});
        writeln!(self.child.stdin.as_mut().unwrap(), "{line}").unwrap();
    }

    /// One request → one response line (blocks until it arrives).
    fn rpc(&mut self, method: &str, params: Value) -> Value {
        use std::io::{BufRead, BufReader, Write};
        self.next_id += 1;
        let id = self.next_id;
        let line = json!({"jsonrpc": "2.0", "id": id,
                          "method": method, "params": params});
        writeln!(self.child.stdin.as_mut().unwrap(), "{line}").unwrap();
        let mut out = String::new();
        BufReader::new(self.child.stdout.as_mut().unwrap())
            .read_line(&mut out)
            .unwrap();
        serde_json::from_str(&out).unwrap_or_else(|_| panic!("mcp EOF: {out}"))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// CAD-62 diagnosis: the mock's `.env` identity lines beside this
/// test's own alias and state dir — a foreign pair means another
/// test's daemon ran this test's mock and overwrote the dump.
fn argv_origin(d: &TestDaemon, mock: &MockClaude, alias: &str) -> String {
    let env = std::fs::read_to_string(mock.pidfile.with_extension("pid.env")).unwrap_or_default();
    let dump: Vec<&str> = env
        .lines()
        .filter(|l| l.starts_with("CADENCE_ALIAS=") || l.starts_with("CADENCE_STATE_DIR="))
        .collect();
    format!(
        "dump: {dump:?}; this test: CADENCE_ALIAS={alias} CADENCE_STATE_DIR={}",
        d.state.display()
    )
}

#[test]
fn claude_brokered_permission_accept() {
    let d = TestDaemon::start();
    let mock = d.mock_claude("permit", None);
    broker_command();
    d.register_inbox("pm");
    d.register_claude("w1", json!({"upstream": "pm", "broker_approvals": true}));
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "run ls", "message": "m1"}),
    )
    .unwrap();
    // The prompt surfaces as a request holding the agent in
    // waiting_input — the cause orders after the launch argv write.
    d.wait_agent("w1", "waiting_input", 15);
    let req = d.wait_request("w1", 15);
    let handle = req["request"].as_str().unwrap().to_string();
    assert_eq!(req["method"], "cadence/approval", "{req}");
    assert_eq!(req["params"]["tool"], "Bash", "{req}");
    assert_eq!(req["params"]["input"]["command"], "run ls", "{req}");
    // A retried open with the same handle dedupes — still one request.
    d.rpc(
        "request_open",
        json!({"alias": "w1", "kind": "approval", "tool": "Bash",
               "request": handle, "input_summary": "run ls",
               "input": {"command": "run ls"}}),
    )
    .unwrap();
    assert_eq!(d.requests("w1").len(), 1);
    // request_opened event names the handle.
    let ev = d.wait_event("w1", "request_opened", 10);
    assert_eq!(ev["payload"]["request"], handle, "{ev}");
    // Launch argv carries the broker wiring.
    let argv = std::fs::read_to_string(mock.pidfile.with_extension("pid.argv")).unwrap();
    assert!(
        argv.contains("--permission-prompt-tool\nmcp__cadence__approve"),
        "{} {}",
        argv,
        argv_origin(&d, &mock, "w1")
    );
    assert!(
        argv.contains("--strict-mcp-config"),
        "{} {}",
        argv,
        argv_origin(&d, &mock, "w1")
    );
    assert!(
        argv.contains("--mcp-config\n"),
        "{} {}",
        argv,
        argv_origin(&d, &mock, "w1")
    );
    // The generated config names the mcp-permission server with the
    // identity env it needs independent of provider propagation.
    let cfg: Value =
        serde_json::from_str(&std::fs::read_to_string(d.state.join("agents/w1.mcp.json")).unwrap())
            .unwrap();
    let server = &cfg["mcpServers"]["cadence"];
    assert_eq!(server["args"], json!(["mcp-permission"]), "{cfg}");
    assert_eq!(server["env"]["CADENCE_ALIAS"], "w1", "{cfg}");
    assert_eq!(
        server["env"]["CADENCE_STATE_DIR"].as_str().unwrap(),
        d.state.to_str().unwrap(),
        "{cfg}"
    );
    // Accept unblocks the tool call; the turn completes and the agent
    // leaves waiting_input.
    d.rpc(
        "agent_respond",
        json!({"alias": "w1", "request": handle, "decision": "accept"}),
    )
    .unwrap();
    let m1 = d.wait_message("w1", "m1", &["completed"], 20);
    assert_eq!(m1["result"]["text"], "MOCK_OK:run ls", "{m1}");
    d.wait_agent("w1", "idle", 15);
    assert!(d.requests("w1").is_empty());
    // The verdict the provider received: allow carrying the input.
    let verdict: Value = serde_json::from_str(
        &std::fs::read_to_string(mock.pidfile.with_extension("pid.verdict")).unwrap(),
    )
    .unwrap();
    assert_eq!(verdict["behavior"], "allow", "{verdict}");
    assert_eq!(verdict["updatedInput"]["command"], "run ls", "{verdict}");
    // input_answered closed the request lifecycle.
    d.wait_event("w1", "input_answered", 10);
    // Exactly one upstream notice, naming the agent and the command.
    let notices: Vec<Value> = {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
            let found: Vec<Value> = pm["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| m["source"].as_str() == Some("worker_notice"))
                .cloned()
                .collect();
            if !found.is_empty() {
                break found;
            }
            assert!(Instant::now() < deadline, "no worker_notice on pm: {pm}");
            thread::sleep(Duration::from_millis(50));
        }
    };
    assert_eq!(notices.len(), 1, "{notices:?}");
    let body = notices[0]["body"].as_str().unwrap();
    assert!(
        body.contains(&format!("agent respond w1 --request {handle}")),
        "{body}"
    );
    assert!(body.contains("w1"), "{body}");
}

#[test]
fn claude_brokered_permission_decline_with_reason() {
    let d = TestDaemon::start();
    let mock = d.mock_claude("permit", None);
    broker_command();
    d.register_claude("w1", json!({"broker_approvals": true}));
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "rm -rf /", "message": "m1"}),
    )
    .unwrap();
    let req = d.wait_request("w1", 15);
    d.wait_agent("w1", "waiting_input", 15);
    // The operator's reason reaches the provider as the denial message.
    d.rpc(
        "agent_respond",
        json!({"alias": "w1", "request": req["request"],
               "decision": "decline", "reason": "no destructive commands"}),
    )
    .unwrap();
    let m1 = d.wait_message("w1", "m1", &["completed"], 20);
    assert_eq!(
        m1["result"]["text"], "DENIED:no destructive commands",
        "{m1}"
    );
    // The denial lands on the standard permission_denied event too.
    let ev = d.wait_event("w1", "permission_denied", 10);
    assert!(
        ev["payload"]["denials"][0]["message"]
            .as_str()
            .unwrap()
            .contains("no destructive commands"),
        "{ev}"
    );
    let verdict: Value = serde_json::from_str(
        &std::fs::read_to_string(mock.pidfile.with_extension("pid.verdict")).unwrap(),
    )
    .unwrap();
    assert_eq!(verdict["behavior"], "deny", "{verdict}");
    assert_eq!(verdict["message"], "no destructive commands", "{verdict}");
}

#[test]
fn claude_brokered_permission_timeout_denies() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude("permit", None);
    broker_command();
    d.register_claude(
        "w1",
        // turn_idle_secs (2s) is SHORTER than the permission deadline
        // (4s): the turn only survives because an open brokered request
        // counts as provider activity — an idle fence here proves the
        // liveness path is broken.
        json!({"broker_approvals": true, "permission_timeout_secs": 4,
               "turn_idle_secs": 2}),
    );
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "slow", "message": "m1"}),
    )
    .unwrap();
    d.wait_agent("w1", "waiting_input", 15);
    d.wait_request("w1", 15);
    // Nobody responds: the broker's deadline denies the tool call and
    // retires the server-side request so the agent is not stuck.
    let m1 = d.wait_message("w1", "m1", &["completed"], 30);
    assert!(
        m1["result"]["text"]
            .as_str()
            .unwrap_or("")
            .contains("DENIED:permission request timed out"),
        "{m1}"
    );
    d.wait_agent("w1", "idle", 15);
    assert!(d.requests("w1").is_empty(), "timed-out request must retire");
    d.wait_event("w1", "request_closed", 10);
}

#[test]
fn claude_brokered_permission_daemon_restart_denies() {
    // The real server binary under test: open a brokered request,
    // restart the daemon mid-wait, and read the verdict off the wire.
    let mut d = TestDaemon::start();
    let _mock = d.mock_claude("ok", None);
    broker_command();
    d.register_claude("w1", json!({"broker_approvals": true}));
    d.wait_agent("w1", "idle", 15);
    let state = d.state.clone();
    let mut mcp = Mcp::spawn(&state, "w1", 120);
    // tools/call blocks in request_wait — its response arrives after
    // the restart as a clean denial, never a hang or a crash.
    let verdict_reader = {
        use std::io::Write;
        let mut stdin = mcp.child.stdin.take().unwrap();
        let mut stdout = mcp.child.stdout.take().unwrap();
        stdin
            .write_all(
                json!({"jsonrpc": "2.0", "id": 99, "method": "tools/call",
                       "params": {"name": "approve",
                                  "arguments": {"tool_name": "Bash",
                                                "input": {"command": "ls"},
                                                "tool_use_id": "tu_1"}}})
                .to_string()
                .as_bytes(),
            )
            .unwrap();
        stdin.write_all(b"\n").unwrap();
        thread::spawn(move || {
            use std::io::BufRead;
            let mut line = String::new();
            std::io::BufReader::new(&mut stdout)
                .read_line(&mut line)
                .unwrap();
            line
        })
    };
    d.wait_request("w1", 15);
    // Restart: pending lives in memory, so the new daemon reports the
    // request closed — the server retries through the socket gap and
    // denies cleanly.
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let d2 = TestDaemon::start_on(state);
    let line = verdict_reader.join().unwrap();
    let resp: Value = serde_json::from_str(&line).unwrap();
    let verdict: Value =
        serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(verdict["behavior"], "deny", "{verdict}");
    assert!(
        verdict["message"].as_str().unwrap().contains("closed"),
        "{verdict}"
    );
    // The restarted daemon shows no residue of the lost request.
    assert!(d2.requests("w1").is_empty());
}

#[test]
fn claude_brokered_params_replayed_on_resume() {
    let d = TestDaemon::start();
    let mock = d.mock_claude("permit", None);
    broker_command();
    d.register_claude(
        "w1",
        json!({"broker_approvals": true, "permission_timeout_secs": 120}),
    );
    d.wait_agent("w1", "idle", 15);
    // A completed turn is the cause ordered after the argv dump.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "boot", "message": "m-boot"}),
    )
    .unwrap();
    d.wait_agent("w1", "waiting_input", 15);
    // Answer it so the launch turn completes — one request only.
    let req = d.wait_request("w1", 10);
    d.rpc(
        "agent_respond",
        json!({"alias": "w1", "request": req["request"], "decision": "accept"}),
    )
    .unwrap();
    d.wait_message("w1", "m-boot", &["completed"], 20);
    let argv_file = mock.pidfile.with_extension("pid.argv");
    let argv1 = std::fs::read_to_string(&argv_file).unwrap();
    assert!(
        argv1.contains("--session-id"),
        "{} {}",
        argv1,
        argv_origin(&d, &mock, "w1")
    );
    assert!(
        argv1.contains("--mcp-config\n"),
        "{} {}",
        argv1,
        argv_origin(&d, &mock, "w1")
    );
    assert!(
        argv1.contains("--strict-mcp-config"),
        "{} {}",
        argv1,
        argv_origin(&d, &mock, "w1")
    );
    assert!(
        argv1.contains("--permission-prompt-tool\nmcp__cadence__approve"),
        "{} {}",
        argv1,
        argv_origin(&d, &mock, "w1")
    );
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(agent["params"]["broker_approvals"], true, "{agent}");
    assert_eq!(agent["params"]["permission_timeout_secs"], 120, "{agent}");
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "idle", 15);
    // Resume replays the broker wiring verbatim — config regenerated,
    // same flags, resumed session.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "boot2", "message": "m-boot2"}),
    )
    .unwrap();
    d.wait_agent("w1", "waiting_input", 15);
    let req2 = d.wait_request("w1", 10);
    d.rpc(
        "agent_respond",
        json!({"alias": "w1", "request": req2["request"], "decision": "accept"}),
    )
    .unwrap();
    d.wait_message("w1", "m-boot2", &["completed"], 20);
    let argv2 = std::fs::read_to_string(&argv_file).unwrap();
    assert!(
        argv2.contains("--resume\n"),
        "{} {}",
        argv2,
        argv_origin(&d, &mock, "w1")
    );
    assert!(
        argv2.contains("--mcp-config\n"),
        "{} {}",
        argv2,
        argv_origin(&d, &mock, "w1")
    );
    assert!(
        argv2.contains("--permission-prompt-tool\nmcp__cadence__approve"),
        "{} {}",
        argv2,
        argv_origin(&d, &mock, "w1")
    );
}

#[test]
fn claude_brokered_flag_validation() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude("ok", None);
    // request_open refuses a non-brokered agent.
    d.register_claude("w1", Value::Null);
    d.wait_agent("w1", "idle", 15);
    let err = d
        .rpc(
            "request_open",
            json!({"alias": "w1", "kind": "approval", "tool": "Bash"}),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("--broker-approvals"), "{err}");
    // Register-time validation rejects malformed broker params.
    for (params, want) in [
        (json!({"broker_approvals": "yes"}), "boolean"),
        (json!({"permission_timeout_secs": 0}), "positive integer"),
        (
            json!({"broker_approvals": true,
                   "permission_mode": "bypassPermissions"}),
            "bypassPermissions",
        ),
    ] {
        let err = d
            .rpc(
                "agent_register",
                json!({"alias": "bad", "provider": "claude",
                       "endpoint_kind": "managed",
                       "cwd": d.dir.path().to_str().unwrap(),
                       "params": params.to_string()}),
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains(want), "{want} missing in: {err}");
    }
    // CLI refuses --broker-approvals with --bypass and --tui, and
    // --permission-timeout-secs without broker mode.
    let bin = env!("CARGO_BIN_EXE_cadence");
    let cases: &[(&[&str], &str)] = &[
        (&["claude", "--broker-approvals", "--bypass"], "bypass"),
        (&["claude", "--broker-approvals", "--tui"], "tui"),
        (
            &["claude", "--permission-timeout-secs", "30"],
            "broker-approvals",
        ),
        (
            &["join", "pm", "claude", "--broker-approvals", "--tui"],
            "tui",
        ),
    ];
    let home = TempDir::new().unwrap();
    for (args, want) in cases {
        let out = std::process::Command::new(bin)
            .args(*args)
            .env("CADENCE_STATE_DIR", d.state.clone())
            .env("HOME", home.path())
            .output()
            .unwrap();
        assert!(!out.status.success(), "{args:?} unexpectedly succeeded");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(want), "{args:?} error missing '{want}': {err}");
    }
}

// ==================== jobs / tasks / verdicts (M3a) ====================

const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SHA_C: &str = "cccccccccccccccccccccccccccccccccccccccc";

impl TestDaemon {
    /// A fake worker joined to `pm`'s group (`params.upstream`).
    fn register_member(&self, alias: &str, pm: &str) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        self.rpc(
            "agent_register",
            json!({"alias": alias, "provider": "fake",
                   "endpoint_kind": "fake", "cwd": cwd,
                   "params": json!({"upstream": pm}).to_string()}),
        )
        .unwrap();
    }

    /// A spec file in the temp dir; returns (path, sha256 hex).
    fn spec_file(&self, name: &str, content: &str) -> (String, String) {
        use sha2::{Digest, Sha256};
        let path = self.dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        (
            path.to_str().unwrap().to_string(),
            format!("{:x}", Sha256::digest(content.as_bytes())),
        )
    }

    fn job_new(&self, pm: &str, job: &str, spec: &str, spec_sha: &str) -> Value {
        self.rpc(
            "job_new",
            json!({"pm": pm, "job": job, "spec": spec, "spec_sha256": spec_sha}),
        )
        .unwrap()
    }

    fn task_state(&self, task: &str) -> String {
        self.rpc("task_show", json!({"task": task})).unwrap()["task"]["state"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn job_state(&self, job: &str) -> String {
        self.rpc("job_show", json!({"job": job})).unwrap()["job"]["state"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// `job dispatch` — returns the full RPC payload.
    fn job_dispatch(&self, task: &str, extra: Value) -> cadence_agent::Result<Value> {
        let mut p = json!({"task": task});
        for (k, v) in extra.as_object().unwrap_or(&serde_json::Map::new()) {
            p[k] = v.clone();
        }
        self.rpc("task_dispatch", p)
    }

    fn job_verdict(
        &self,
        task: &str,
        sha: &str,
        verdict: &str,
        reviewer: Option<&str>,
        pane: Option<&str>,
    ) -> cadence_agent::Result<Value> {
        self.rpc(
            "task_verdict",
            json!({"task": task, "sha": sha, "verdict": verdict,
                   "reviewer": reviewer, "pane": pane}),
        )
    }

    /// Wait for a task state.
    fn wait_task(&self, task: &str, want: &str, secs: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let t = self.rpc("task_show", json!({"task": task})).unwrap()["task"].clone();
            if t["state"].as_str() == Some(want) {
                return t;
            }
            assert!(
                Instant::now() < deadline,
                "task {task} never reached {want}: {t}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
fn job_new_creates_default_task_and_validates_issue() {
    let d = TestDaemon::start();
    d.register("pm");
    let (spec, sha) = d.spec_file("spec.md", "do the seeded-bug fix");

    let r = d.rpc(
        "job_new",
        json!({"pm": "pm", "job": "j1", "spec": spec, "spec_sha256": sha,
               "title": "fix it", "issue": "CAD-26", "repo": "/tmp/x",
               "base_ref": "main", "max_revisions": 3}),
    );
    let job = r.unwrap()["job"].clone();
    assert_eq!(job["state"], "open");
    assert_eq!(job["issue"], "CAD-26");
    assert_eq!(job["pm"], "pm");
    assert_eq!(job["spec_sha256"], sha.as_str());
    assert_eq!(job["max_revisions"], 3);

    // The default task <job>-t1 is created draft.
    let show = d.rpc("job_show", json!({"job": "j1"})).unwrap();
    let tasks = show["job"]["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["id"], "j1-t1");
    assert_eq!(tasks[0]["state"], "draft");
    assert_eq!(tasks[0]["revision"], 0);

    // job list shows the issue id + task counts.
    let list = d.rpc("job_list", json!({})).unwrap()["jobs"].clone();
    assert_eq!(list[0]["issue"], "CAD-26");
    assert_eq!(list[0]["tasks"]["draft"], 1);

    // Idempotent re-create with identical params.
    let dup = d.rpc(
        "job_new",
        json!({"pm": "pm", "job": "j1", "spec": spec, "spec_sha256": sha,
               "issue": "CAD-26"}),
    );
    assert_eq!(dup.unwrap()["duplicate"], true);

    // Same id, different content → rejected.
    assert!(d
        .rpc(
            "job_new",
            json!({"pm": "pm", "job": "j1", "spec": "/other.md",
                   "spec_sha256": "0".repeat(64), "issue": "CAD-26"}),
        )
        .is_err());

    // Issue grammar is validated — not the filesystem.
    assert!(d
        .rpc(
            "job_new",
            json!({"pm": "pm", "job": "j2", "spec": spec,
                   "spec_sha256": sha, "issue": "not-an-issue"}),
        )
        .is_err());

    // One leaf issue → one open job.
    assert!(d
        .rpc(
            "job_new",
            json!({"pm": "pm", "job": "j2", "spec": spec,
                   "spec_sha256": sha, "issue": "CAD-26"}),
        )
        .is_err());

    // Unknown PM rejected.
    assert!(d
        .rpc(
            "job_new",
            json!({"pm": "nobody", "job": "j3", "spec": spec,
                   "spec_sha256": sha}),
        )
        .is_err());
}

#[test]
fn job_seeded_bug_loop_end_to_end() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "seeded bug: off-by-one");
    d.job_new("pm", "j1", &spec, &sha);

    let wt = d.dir.path().join("wt-t1");
    std::fs::create_dir_all(&wt).unwrap();
    d.rpc(
        "task_new",
        json!({"job": "j1", "task": "j1-fix", "title": "fix the bug",
               "assignee": "w1", "worktree": wt.to_str().unwrap(),
               "branch": "cadence/fix", "base_sha": SHA_B,
               "acceptance": format!("tests pass REPORT_SHA:{SHA_A}")}),
    )
    .unwrap();

    // r1: dispatch → running → completed with the SHA trailer → review.
    let r1 = d.job_dispatch("j1-fix", json!({})).unwrap();
    let kickoff1 = r1["message"].as_str().unwrap().to_string();
    assert_eq!(r1["task"]["state"], "dispatched");
    assert_eq!(r1["task"]["revision"], 1);
    let m1 = d.wait_message("w1", &kickoff1, &["completed"], 15);
    assert_eq!(m1["source"], "job_dispatch");
    assert_eq!(m1["task_id"], "j1-fix");
    assert!(m1["body"].as_str().unwrap().contains("worktree"));
    let t = d.wait_task("j1-fix", "review", 15);
    assert_eq!(t["head_sha"], SHA_A, "{t}");

    // verdict revise (r1 < max 2) → revising; PM got a job_event.
    d.job_verdict("j1-fix", SHA_A, "revise", Some("pm"), None)
        .unwrap();
    assert_eq!(d.task_state("j1-fix"), "revising");

    // r2: a fresh deterministic kickoff id, not the r1 one.
    let r2 = d.job_dispatch("j1-fix", json!({})).unwrap();
    let kickoff2 = r2["message"].as_str().unwrap().to_string();
    assert_ne!(kickoff1, kickoff2);
    assert_eq!(r2["task"]["revision"], 2);
    d.wait_message("w1", &kickoff2, &["completed"], 15);
    d.wait_task("j1-fix", "review", 15);

    // pass → verified → accept → done; PM notification routed.
    d.job_verdict("j1-fix", SHA_A, "pass", Some("pm"), None)
        .unwrap();
    assert_eq!(d.task_state("j1-fix"), "verified");
    d.rpc(
        "task_accept",
        json!({"task": "j1-fix", "merged_sha": SHA_C, "by": "pm"}),
    )
    .unwrap();
    assert_eq!(d.task_state("j1-fix"), "done");
    assert_eq!(d.job_state("j1"), "open"); // j1-t1 default task still draft

    // The PM received routed job_event notifications (verified + done)
    // plus the worker_result for each kickoff — self-describing.
    let pm_msgs = d.rpc("agent_show", json!({"alias": "pm"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .clone();
    let events: Vec<&Value> = pm_msgs
        .iter()
        .filter(|m| m["source"] == "job_event")
        .collect();
    assert!(events.len() >= 2, "{pm_msgs:?}");
    let worker_results: Vec<&Value> = pm_msgs
        .iter()
        .filter(|m| m["source"] == "worker_result")
        .collect();
    assert_eq!(worker_results.len(), 2, "{pm_msgs:?}");
    assert_eq!(worker_results[0]["task_id"], "j1-fix");

    // job events carry the scoped history.
    let evs = d.rpc("job_events", json!({"job": "j1"})).unwrap()["events"]
        .as_array()
        .unwrap()
        .clone();
    let kinds: Vec<&str> = evs.iter().filter_map(|e| e["kind"].as_str()).collect();
    for k in [
        "job_created",
        "task_created",
        "task_dispatched",
        "task_running",
        "task_reported",
        "verdict_recorded",
        "task_done",
    ] {
        assert!(kinds.contains(&k), "missing {k} in {kinds:?}");
    }
    assert!(evs.iter().all(|e| e["job_id"] == "j1"));
}

#[test]
fn job_inbox_pm_receives_notifications() {
    let d = TestDaemon::start();
    d.register_inbox("pm-in");
    d.register_member("w1", "pm-in");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "inbox pm spec");
    d.job_new("pm-in", "j1", &spec, &sha);

    d.rpc(
        "task_new",
        json!({"job": "j1", "task": "j1-t2", "assignee": "w1",
               "acceptance": format!("ok REPORT_SHA:{SHA_A}")}),
    )
    .unwrap();
    let r = d.job_dispatch("j1-t2", json!({})).unwrap();
    let kickoff = r["message"].as_str().unwrap().to_string();
    d.wait_message("w1", &kickoff, &["completed"], 15);
    d.wait_task("j1-t2", "review", 15);
    d.job_verdict("j1-t2", SHA_A, "pass", Some("operator"), None)
        .unwrap();
    d.rpc("task_accept", json!({"task": "j1-t2", "by": "operator"}))
        .unwrap();
    assert_eq!(d.task_state("j1-t2"), "done");

    // Drain the PM inbox: worker_result + job_event copies landed.
    let drained = d.rpc("agent_inbox", json!({"alias": "pm-in"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .clone();
    let sources: Vec<&str> = drained
        .iter()
        .filter_map(|m| m["source"].as_str())
        .collect();
    assert!(sources.contains(&"worker_result"), "{sources:?}");
    assert!(sources.contains(&"job_event"), "{sources:?}");
    let done_note = drained
        .iter()
        .find(|m| m["source"] == "job_event" && m["body"].as_str().unwrap_or("").contains("done"))
        .expect("no done notification");
    assert_eq!(done_note["task_id"], "j1-t2");
}

#[test]
fn verdict_rejects_every_bad_shape() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "verdict rejections");
    d.job_new("pm", "j1", &spec, &sha);
    d.rpc(
        "task_new",
        json!({"job": "j1", "task": "j1-t2", "assignee": "w1",
               "acceptance": format!("ok REPORT_SHA:{SHA_A}")}),
    )
    .unwrap();

    // Not in review → rejected.
    assert!(d
        .job_verdict("j1-t2", SHA_A, "pass", Some("rev"), None)
        .is_err());

    let r = d.job_dispatch("j1-t2", json!({})).unwrap();
    let kickoff = r["message"].as_str().unwrap().to_string();
    d.wait_message("w1", &kickoff, &["completed"], 15);
    d.wait_task("j1-t2", "review", 15);

    // Wrong sha → rejected.
    let e = d
        .job_verdict("j1-t2", SHA_B, "pass", Some("rev"), None)
        .unwrap_err();
    assert!(e.to_string().contains("does not match"), "{e}");
    // Malformed sha → rejected.
    assert!(d
        .job_verdict("j1-t2", "abc123", "pass", Some("rev"), None)
        .is_err());
    // reviewer == assignee → rejected.
    let e = d
        .job_verdict("j1-t2", SHA_A, "pass", Some("w1"), None)
        .unwrap_err();
    assert!(e.to_string().contains("assignee"), "{e}");
    // Pane rules: --reviewer inside a pane → rejected; operator inside a
    // pane → rejected; pane alias wins the verdict when legal.
    assert!(d
        .job_verdict("j1-t2", SHA_A, "pass", Some("rev"), Some("rev-pane"))
        .is_err());
    assert!(d
        .job_verdict("j1-t2", SHA_A, "pass", None, Some("operator"))
        .is_err());
    // Outside a pane, --reviewer is required.
    assert!(d.job_verdict("j1-t2", SHA_A, "pass", None, None).is_err());
    // Stale revision → rejected.
    assert!(d
        .rpc(
            "task_verdict",
            json!({"task": "j1-t2", "sha": SHA_A, "verdict": "pass",
                   "reviewer": "rev", "revision": 7}),
        )
        .is_err());
    // Bad verdict word → rejected.
    assert!(d
        .job_verdict("j1-t2", SHA_A, "maybe", Some("rev"), None)
        .is_err());

    // The good verdict lands — reviewer recorded, pane flag false.
    d.job_verdict("j1-t2", SHA_A, "pass", Some("rev"), None)
        .unwrap();
    let t = d.rpc("task_show", json!({"task": "j1-t2"})).unwrap()["task"].clone();
    assert_eq!(t["state"], "verified");
    let v = &t["verdicts"][0];
    assert_eq!(v["reviewer"], "rev");
    assert_eq!(v["sha"], SHA_A);
    assert_eq!(v["revision"], 1);
    assert_eq!(v["pane"], Value::Null);
}

#[test]
fn verdict_rejects_null_sha_until_repaired() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "no-sha report");
    d.job_new("pm", "j1", &spec, &sha);
    // No REPORT_SHA directive — the fake reply carries no SHA line.
    d.rpc(
        "task_new",
        json!({"job": "j1", "task": "j1-t2", "assignee": "w1",
               "acceptance": "plain echo"}),
    )
    .unwrap();
    let r = d.job_dispatch("j1-t2", json!({})).unwrap();
    let kickoff = r["message"].as_str().unwrap().to_string();
    d.wait_message("w1", &kickoff, &["completed"], 15);
    let t = d.wait_task("j1-t2", "review", 15);
    assert_eq!(t["head_sha"], Value::Null);
    // `job show` flags the missing SHA.
    let show = d.rpc("job_show", json!({"job": "j1"})).unwrap();
    let tj = show["job"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "j1-t2")
        .unwrap();
    assert!(
        tj["attention"].as_str().unwrap().contains("job task sha"),
        "{tj}"
    );

    // Verdict rejected naming the fix.
    let e = d
        .job_verdict("j1-t2", SHA_A, "pass", Some("rev"), None)
        .unwrap_err();
    assert!(e.to_string().contains("job task sha"), "{e}");

    // `job task sha` repairs it — recorded as an event, verdict proceeds.
    d.rpc(
        "task_sha",
        json!({"task": "j1-t2", "sha": SHA_A, "by": "pm"}),
    )
    .unwrap();
    // Same sha again → idempotent ok; a different sha → rejected.
    d.rpc(
        "task_sha",
        json!({"task": "j1-t2", "sha": SHA_A, "by": "pm"}),
    )
    .unwrap();
    assert!(d
        .rpc(
            "task_sha",
            json!({"task": "j1-t2", "sha": SHA_B, "by": "pm"})
        )
        .is_err());
    d.job_verdict("j1-t2", SHA_A, "pass", Some("rev"), None)
        .unwrap();
    assert_eq!(d.task_state("j1-t2"), "verified");
    let ev = d.events("pm");
    assert!(ev.iter().any(|e| e["kind"] == "task_sha_recorded"));
}

#[test]
fn max_revisions_escalates_to_blocked_once() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "cap test");
    d.rpc(
        "job_new",
        json!({"pm": "pm", "job": "j1", "spec": spec, "spec_sha256": sha,
               "max_revisions": 2}),
    )
    .unwrap();
    d.rpc(
        "task_new",
        json!({"job": "j1", "task": "j1-t2", "assignee": "w1",
               "acceptance": format!("ok REPORT_SHA:{SHA_A}")}),
    )
    .unwrap();

    // r1 → review → revise → revising.
    let r = d.job_dispatch("j1-t2", json!({})).unwrap();
    let k = r["message"].as_str().unwrap().to_string();
    d.wait_message("w1", &k, &["completed"], 15);
    d.wait_task("j1-t2", "review", 15);
    d.job_verdict("j1-t2", SHA_A, "revise", Some("rev"), None)
        .unwrap();
    assert_eq!(d.task_state("j1-t2"), "revising");

    // r2 → review → revise at the cap → blocked, PM notified once.
    let r = d.job_dispatch("j1-t2", json!({})).unwrap();
    let k = r["message"].as_str().unwrap().to_string();
    d.wait_message("w1", &k, &["completed"], 15);
    d.wait_task("j1-t2", "review", 15);
    d.job_verdict("j1-t2", SHA_A, "revise", Some("rev"), None)
        .unwrap();
    assert_eq!(d.task_state("j1-t2"), "blocked");

    // The loop cannot continue: dispatch without --to names reopen.
    let e = d.job_dispatch("j1-t2", json!({})).unwrap_err();
    assert!(e.to_string().contains("reopen"), "{e}");
    // A verdict lands only on review — blocked task rejects.
    assert!(d
        .job_verdict("j1-t2", SHA_A, "pass", Some("rev"), None)
        .is_err());

    // Exactly one blocked notification to the PM.
    let pm_msgs = d.rpc("agent_show", json!({"alias": "pm"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .clone();
    let blocked: Vec<&Value> = pm_msgs
        .iter()
        .filter(|m| {
            m["source"] == "job_event" && m["body"].as_str().unwrap_or("").contains("blocked")
        })
        .collect();
    assert_eq!(blocked.len(), 1, "{pm_msgs:?}");

    // Operator reopen re-scopes: draft, revision 0, dispatch works.
    d.rpc("task_reopen", json!({"task": "j1-t2", "by": "operator"}))
        .unwrap();
    assert_eq!(d.task_state("j1-t2"), "draft");
    let r = d.job_dispatch("j1-t2", json!({})).unwrap();
    assert_eq!(r["task"]["revision"], 1);
}

#[test]
fn job_cancel_semantics() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.register_member("w2", "pm");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "cancel semantics");
    d.job_new("pm", "j1", &spec, &sha);

    // Queued kickoff: stop w2 first so its queue never drains.
    d.rpc("agent_stop", json!({"alias": "w2"})).unwrap();
    d.rpc(
        "task_new",
        json!({"job": "j1", "task": "j1-tq", "assignee": "w2"}),
    )
    .unwrap();
    let r = d.job_dispatch("j1-tq", json!({})).unwrap();
    assert_eq!(r["queued_behind_dead"], true, "{r}");
    let k = r["message"].as_str().unwrap().to_string();
    assert_eq!(d.message_state("w2", &k), "queued");
    d.rpc("task_cancel", json!({"task": "j1-tq", "by": "pm"}))
        .unwrap();
    assert_eq!(d.task_state("j1-tq"), "cancelled");
    // The queued kickoff was cancelled in the same transaction.
    assert_eq!(d.message_state("w2", &k), "cancelled");
    // The agent itself was never stopped by the job layer.
    let w2 = d.rpc("agent_show", json!({"alias": "w2"})).unwrap()["agent"].clone();
    assert_eq!(w2["state"], "stopped"); // operator stop, unchanged

    // Running kickoff: cancel leaves it alone — it completes on its own.
    d.rpc(
        "task_new",
        json!({"job": "j1", "task": "j1-tr", "assignee": "w1",
               "acceptance": format!("ok REPORT_SHA:{SHA_A}")}),
    )
    .unwrap();
    let r = d.job_dispatch("j1-tr", json!({})).unwrap();
    let k = r["message"].as_str().unwrap().to_string();
    d.rpc("task_cancel", json!({"task": "j1-tr", "by": "pm"}))
        .unwrap();
    assert_eq!(d.task_state("j1-tr"), "cancelled");
    // The kickoff still ran to completion; the task stays cancelled.
    d.wait_message("w1", &k, &["completed"], 15);
    thread::sleep(Duration::from_millis(300));
    assert_eq!(d.task_state("j1-tr"), "cancelled");
    d.wait_agent("w1", "idle", 10); // agent alive and untouched

    // job cancel cancels every non-terminal task + the job.
    d.rpc("task_new", json!({"job": "j1", "task": "j1-tz"}))
        .unwrap();
    d.rpc("job_cancel", json!({"job": "j1", "by": "pm"}))
        .unwrap();
    assert_eq!(d.job_state("j1"), "cancelled");
    assert_eq!(d.task_state("j1-tz"), "cancelled");
    // Terminal job rejects new tasks/dispatch.
    assert!(d.job_dispatch("j1-tq", json!({})).is_err());
    // job close requires all-done.
    assert!(d
        .rpc("job_close", json!({"job": "j1", "by": "pm"}))
        .is_err());
}

#[test]
fn dispatch_dedupes_live_kickoff_and_reassign_bumps() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.register_member("w2", "pm");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "dedupe + reassign");
    d.job_new("pm", "j1", &spec, &sha);
    d.rpc(
        "task_new",
        json!({"job": "j1", "task": "j1-t2", "assignee": "w1",
               "acceptance": format!("ok REPORT_SHA:{SHA_A}")}),
    )
    .unwrap();

    // Live kickoff → second dispatch is the SAME revision, same id.
    // Deterministic: stop w1 first so its queue never drains.
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    let r1 = d.job_dispatch("j1-t2", json!({})).unwrap();
    let k1 = r1["message"].as_str().unwrap().to_string();
    assert_eq!(r1["task"]["state"], "dispatched");
    assert_eq!(r1["task"]["revision"], 1);
    assert_eq!(d.message_state("w1", &k1), "queued");
    let r2 = d.job_dispatch("j1-t2", json!({})).unwrap();
    assert_eq!(r2["duplicate"], true, "{r2}");
    assert_eq!(r2["message"], k1);
    assert_eq!(r2["task"]["revision"], 1);
    // Still exactly one kickoff row.
    let t = d.rpc("task_show", json!({"task": "j1-t2"})).unwrap()["task"].clone();
    let kicks: Vec<&Value> = t["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["source"] == "job_dispatch")
        .collect();
    assert_eq!(kicks.len(), 1, "{kicks:?}");

    // Reassign under a live kickoff is refused.
    let e = d.job_dispatch("j1-t2", json!({"to": "w2"})).unwrap_err();
    assert!(e.to_string().contains("live kickoff"), "{e}");

    // The natural reassign path: let the kickoff complete, revise,
    // then --to bumps the revision with a fresh kickoff id.
    d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();
    d.wait_message("w1", &k1, &["completed"], 15);
    d.wait_task("j1-t2", "review", 15);
    d.job_verdict("j1-t2", SHA_A, "revise", Some("rev"), None)
        .unwrap();
    assert_eq!(d.task_state("j1-t2"), "revising");
    let r = d.job_dispatch("j1-t2", json!({"to": "w2"})).unwrap();
    assert_eq!(r["task"]["revision"], 2, "{r}");
    assert_eq!(r["task"]["assignee"], "w2");
    assert_ne!(r["message"], k1);
    d.wait_message("w2", r["message"].as_str().unwrap(), &["completed"], 15);
    d.wait_task("j1-t2", "review", 15);
}

#[test]
fn restart_fences_task_kickoff_and_job_show_reports_drift() {
    // Seed a state dir with a dispatched task whose kickoff is in
    // flight, then start the daemon — recovery fences the message, the
    // task is untouched, `job show` flags the drift and dispatch is
    // legal again.
    let seeded = TempDir::new().unwrap();
    let state = seeded.path().to_path_buf();
    {
        let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
        let cwd = state.to_str().unwrap().to_string();
        store
            .register_agent(&NewAgent {
                alias: "pm",
                provider: "fake",
                endpoint_kind: "fake",
                role: "pm",
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        store
            .register_agent(&NewAgent {
                alias: "w1",
                provider: "fake",
                endpoint_kind: "fake",
                role: "worker",
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params: Some(&json!({"upstream": "pm"}).to_string()),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        store
            .create_job(
                "j1",
                None,
                "/tmp/spec.md",
                &"0".repeat(64),
                "pm",
                None,
                None,
                None,
                2,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        store
            .create_task(
                "j1",
                "j1-t2",
                None,
                Some("w1"),
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        let (task, kickoff, dup, _dead) = store.dispatch_task("j1-t2", None, None, "test").unwrap();
        assert!(!dup);
        assert_eq!(task.state, "dispatched");
        // Simulate a mid-turn crash: kickoff taken + running, store dropped.
        match store.take_queued("w1").unwrap() {
            Take::Message(m) => assert_eq!(m.id, kickoff),
            _ => panic!("expected kickoff"),
        }
        store.mark_running(&kickoff, "fake-1-abc").unwrap();
        assert_eq!(store.task("j1-t2").unwrap().state, "running");
    }
    let d = TestDaemon::start_on(state);
    // Recovery fenced the kickoff unknown; the task stays running.
    let show = d.rpc("job_show", json!({"job": "j1"})).unwrap();
    let t = show["job"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "j1-t2")
        .unwrap();
    assert_eq!(t["state"], "running", "{t}");
    assert_eq!(t["kickoff"]["state"], "unknown");
    assert!(t["attention"].as_str().unwrap().contains("unfence"), "{t}");

    // Dispatch is legal from the flagged state — a new revision.
    let r = d.job_dispatch("j1-t2", json!({})).unwrap();
    assert_eq!(r["task"]["revision"], 2, "{r}");
    assert_ne!(r["message"].as_str().unwrap(), t["kickoff"]["id"]);
}

#[test]
fn task_attached_send_and_self() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "send --task");
    d.job_new("pm", "j1", &spec, &sha);
    d.rpc(
        "task_new",
        json!({"job": "j1", "task": "j1-t2", "assignee": "w1",
               "acceptance": format!("ok REPORT_SHA:{SHA_A}")}),
    )
    .unwrap();

    // `send --task` attaches for indexing — the message completes
    // normally and does NOT drive the task state machine.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "ping", "message": "adhoc1",
               "task": "j1-t2"}),
    )
    .unwrap();
    d.wait_message("w1", "adhoc1", &["completed"], 15);
    assert_eq!(d.task_state("j1-t2"), "draft");
    let m = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "adhoc1")
        .unwrap()
        .clone();
    assert_eq!(m["task_id"], "j1-t2");
    // Task show lists the attached delivery.
    let t = d.rpc("task_show", json!({"task": "j1-t2"})).unwrap()["task"].clone();
    let ids: Vec<&str> = t["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert!(ids.contains(&"adhoc1"), "{ids:?}");

    // `message result --sha` lands the sha on a pty-less fake path? —
    // fake completions go through the adapter text; the --sha flag path
    // is covered by the store-level edge test. Here: attach + report
    // via reconcile path already covered.
    // `agent list` exposes the open task binding.
    let list = d.rpc("agent_list", json!({})).unwrap()["agents"]
        .as_array()
        .unwrap()
        .clone();
    let w1 = list.iter().find(|a| a["alias"] == "w1").unwrap();
    assert_eq!(w1["tasks"], json!(["j1-t2"]), "{w1}");
}

#[test]
fn job_event_parks_on_unrendered_pty_pm() {
    let state_dir = TempDir::new().unwrap();
    let mock_dir = TempDir::new().unwrap();
    let mock = install_mock_devin(mock_dir.path());
    let state = state_dir.path();
    let cwd = state.to_str().unwrap();
    let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
    store
        .register_agent(&NewAgent {
            alias: "pm",
            provider: "devin",
            endpoint_kind: "pty",
            role: "pm",
            cwd,
            sandbox: "read-only",
            instructions: None,
            params: Some(r#"{"auto_ready":"verified"}"#),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let spec = state.join("spec.md");
    let spec_body = b"pty pm park test";
    std::fs::write(&spec, spec_body).unwrap();
    use sha2::{Digest, Sha256};
    let spec_sha = format!("{:x}", Sha256::digest(spec_body));
    store
        .create_job(
            "j1",
            Some("pty pm park test"),
            spec.to_str().unwrap(),
            &spec_sha,
            "pm",
            None,
            None,
            None,
            2,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    // Seed the notification through the same Store::job_notice →
    // route_job_event path used by job verdict, while leaving worker
    // dispatch/review lifecycle coverage to the other job tests. The
    // queued row is present before daemon boot, so no second Store can
    // reset an active endpoint and the daemon sees its normal wake path.
    store
        .job_notice(
            "j1-t1",
            "verified",
            "fixture:job_event_park",
            "fixture routed job event",
        )
        .unwrap();
    drop(store);
    let socket_dir = mock.dir.join("tmux-state").join(socket_for(state));
    std::fs::create_dir_all(&socket_dir).unwrap();
    atomic_write(socket_dir.join("pm.swallow"), "1");
    let d = TestDaemon::start_on(state.to_path_buf());
    d.wait_agent("pm", "idle", 20);

    // The job_event notification requeues bounded, then parks — the
    // PM pane survives, never fenced.
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
        let parked = pm["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["source"] == "job_event" && m["state"] == "failed");
        if parked {
            break;
        }
        assert!(Instant::now() < deadline, "job_event never parked");
        thread::sleep(Duration::from_millis(200));
    }
    let evs = d.events("pm");
    let parked_ev = evs
        .iter()
        .find(|e| e["kind"] == "delivery_parked")
        .expect("no delivery_parked event");
    let parked_id = parked_ev["payload"]["message"].as_str().unwrap();
    let msg = d.rpc("agent_show", json!({"alias": "pm"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == parked_id)
        .unwrap()
        .clone();
    assert_eq!(msg["result"]["via"], "pty_render_miss", "{msg}");
    // PM still alive + idle, never fenced.
    d.wait_agent("pm", "idle", 15);
    let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap()["agent"].clone();
    assert_eq!(pm["dead"], false);
    emit_park_phase_trace(&d, "job_event_parks_on_unrendered_pty_pm", "pm", parked_id);
}

#[test]
fn message_result_sha_flag_on_pty_path() {
    // --sha on `message result` binds the reported commit — exercised
    // through the store edge (the fake path reports via trailer; the
    // pty report path is covered by unit tests in store.rs).
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin_opts("pm", json!({"auto_ready": "verified"}));
    d.wait_agent("pm", "idle", 20);
    d.register_member("w1", "pm");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "sha flag");
    d.job_new("pm", "j1", &spec, &sha);
    d.rpc(
        "task_new",
        json!({"job": "j1", "task": "j1-t2", "assignee": "w1"}),
    )
    .unwrap();
    // Report path uses explicit sha param (what --sha sends).
    let r = d.job_dispatch("j1-t2", json!({})).unwrap();
    let k = r["message"].as_str().unwrap().to_string();
    d.wait_message("w1", &k, &["completed"], 15);
    d.wait_task("j1-t2", "review", 15);
    // operator reconcile of a completed message isn't the path — the
    // edge is task_on_completed reading result.sha; cover via verdict.
    assert!(d
        .job_verdict("j1-t2", SHA_A, "pass", Some("rev"), None)
        .is_err()); // NULL head_sha — never inferred
    d.rpc(
        "task_sha",
        json!({"task": "j1-t2", "sha": SHA_A, "by": "pm"}),
    )
    .unwrap();
    d.job_verdict("j1-t2", SHA_A, "pass", Some("rev"), None)
        .unwrap();
    assert_eq!(d.task_state("j1-t2"), "verified");
}

// ==== pty profile split: second profile + forbidden prefixes (CAD-32) ====

/// The generic adapter driven by a non-Devin profile: a different
/// prompt glyph, placeholder, session-lock dir and launch argv — the
/// profile, not hardcoded Devin facts, carries the TUI.
#[test]
fn pty_stub_profile_drives_gate_and_render() {
    let d = TestDaemon::start();
    let mock = d.mock_stub();
    d.register_stub("st", json!({"auto_ready": "verified"}));
    d.wait_agent("st", "idle", 20);
    // Idle detection is the stub's own `» stub ready` line: the daemon
    // probe self-claims and the send pastes. `/` is forbidden on Devin
    // but literal here — the prefix list is the profile's too.
    d.rpc(
        "agent_send",
        json!({"alias": "st", "text": "/looks like a command elsewhere",
               "message": "m1"}),
    )
    .unwrap();
    let token = pty_token(&d, "st", "m1");
    assert!(token.starts_with("pty-"), "{token}");
    let claim = d.wait_event("st", "ready_claimed", 5);
    assert_eq!(claim["payload"]["by"], "daemon", "{claim}");
    // The render check passed on the stub's screen.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let screen =
            std::fs::read_to_string(d.stub_pane_file(&mock, "st", "screen")).unwrap_or_default();
        if screen.contains("> /looks like a command elsewhere") {
            break;
        }
        assert!(Instant::now() < deadline, "stub never echoed: {screen}");
        thread::sleep(Duration::from_millis(50));
    }
    d.rpc(
        "message_report",
        json!({"message": "m1", "token": token, "kind": "result",
               "text": "done"}),
    )
    .unwrap();
    d.wait_message("st", "m1", &["completed"], 10);
    // The stub's own forbidden prefixes still reject pre-write.
    d.rpc(
        "agent_send",
        json!({"alias": "st", "text": "~tilde is stub's mode key",
               "message": "m2"}),
    )
    .unwrap();
    let failed = d.wait_message("st", "m2", &["failed"], 15);
    let err = failed["error"].as_str().unwrap_or("").to_string();
    assert!(
        err.contains("'~'") && err.contains("command or mode switch"),
        "{err}"
    );
    assert!(
        std::fs::read_to_string(d.stub_pane_file(&mock, "st", "input"))
            .unwrap_or_default()
            .is_empty()
    );
}

/// The stub's busy signature — not Devin's — is what blocks the gate.
#[test]
fn pty_stub_profile_busy_marker_blocks_paste() {
    let d = TestDaemon::start();
    let mock = d.mock_stub();
    d.register_stub("st", json!({"auto_ready": "verified"}));
    d.wait_agent("st", "idle", 20);
    // Devin's busy marker means nothing to the stub profile: a send
    // with it on screen still goes through.
    atomic_write(
        d.stub_pane_file(&mock, "st", "tui-state"),
        "(esc twice to interrupt)\n",
    );
    d.rpc(
        "agent_send",
        json!({"alias": "st", "text": "devin marker is inert here",
               "message": "m1"}),
    )
    .unwrap();
    pty_token(&d, "st", "m1");
    // The stub's own marker must block.
    atomic_write(d.stub_pane_file(&mock, "st", "tui-state"), "stub working\n");
    d.rpc(
        "agent_send",
        json!({"alias": "st", "text": "do not paste", "message": "m2"}),
    )
    .unwrap();
    let wait = d.wait_event("st", "gate_wait", 10);
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("busy"),
        "{wait}"
    );
    assert_eq!(d.message_state("st", "m2"), "queued");
    assert!(
        std::fs::read_to_string(d.stub_pane_file(&mock, "st", "input"))
            .unwrap_or_default()
            .is_empty()
    );
    let probe = d.rpc("agent_probe", json!({"alias": "st"})).unwrap();
    assert_eq!(probe["busy_marker"], true, "{probe}");
    // Busy clears — the queued send delivers.
    std::fs::remove_file(d.stub_pane_file(&mock, "st", "tui-state")).unwrap();
    pty_token(&d, "st", "m2");
}

/// Claims stay single-use under a second profile: one claim releases
/// exactly one send; the next waits at the gate.
#[test]
fn pty_stub_profile_consumes_one_claim_per_send() {
    let d = TestDaemon::start();
    let _mock = d.mock_stub();
    d.register_stub("st", json!({}));
    d.wait_agent("st", "idle", 20);
    d.rpc("agent_ready", json!({"alias": "st"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "st", "text": "first", "message": "m1"}),
    )
    .unwrap();
    pty_token(&d, "st", "m1");
    // The claim is spent: a second send waits at the gate.
    d.rpc(
        "agent_send",
        json!({"alias": "st", "text": "second", "message": "m2"}),
    )
    .unwrap();
    let wait = d.wait_event("st", "gate_wait", 10);
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("agent ready"),
        "{wait}"
    );
    // A fresh claim releases exactly the queued send.
    d.rpc("agent_ready", json!({"alias": "st"})).unwrap();
    pty_token(&d, "st", "m2");
}

/// Devin's forbidden input prefixes — `/`, `!`, `@` observed live
/// (command menu, bash mode, file picker) — are `PreWrite` rejections:
/// the message fails, the pane is untouched, the agent stays idle, and
/// the readiness claim is not consumed. `#` is a literal draft char.
#[test]
fn pty_forbidden_prefix_is_prewrite_and_keeps_claim() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv", None);
    d.wait_agent("dv", "idle", 20);
    // One claim: every rejection below must leave it unconsumed.
    d.rpc("agent_ready", json!({"alias": "dv"})).unwrap();
    for (id, body, prefix) in [
        ("m1", "/menu", '/'),
        ("m2", "!bash", '!'),
        ("m3", "@file", '@'),
    ] {
        d.rpc(
            "agent_send",
            json!({"alias": "dv", "text": body, "message": id}),
        )
        .unwrap();
        d.wait_message("dv", id, &["failed"], 15);
        let failed = d.rpc("agent_show", json!({"alias": "dv"})).unwrap()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id)
            .unwrap()
            .clone();
        let err = failed["result"]["error"].as_str().unwrap_or("").to_string();
        assert!(err.contains(&format!("'{prefix}'")), "{err}");
        assert!(err.contains("command or mode switch"), "{err}");
    }
    // Provably no bytes reached the pane for any rejection...
    assert!(std::fs::read_to_string(d.pane_file(&mock, "dv", "input"))
        .unwrap_or_default()
        .is_empty());
    // ...the agent is not fenced...
    let agent = d.rpc("agent_show", json!({"alias": "dv"})).unwrap()["agent"].clone();
    assert_eq!(agent["state"].as_str().unwrap(), "idle", "{agent}");
    // ...and the original claim survived: `#` is literal and this send
    // goes through on the SAME claim — no second `agent_ready`.
    d.rpc(
        "agent_send",
        json!({"alias": "dv", "text": "#literal tag", "message": "m4"}),
    )
    .unwrap();
    let token = pty_token(&d, "dv", "m4");
    assert!(token.starts_with("pty-"), "{token}");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let screen =
            std::fs::read_to_string(d.pane_file(&mock, "dv", "screen")).unwrap_or_default();
        if screen.contains("> #literal tag") {
            break;
        }
        assert!(Instant::now() < deadline, "never echoed: {screen}");
        thread::sleep(Duration::from_millis(50));
    }
}

/// `agent show`/`agent list` expose the registry's capabilities object
/// verbatim for every (provider, endpoint_kind) spec — the daemon is
/// authoritative, so the wire value must match the table exactly.
#[test]
fn agent_capabilities_match_the_registry_table() {
    use cadence_agent::adapter::registry;
    // Registering every spec launches every provider: point them all
    // at `false` so nothing real (or another test's mock) is spawned.
    for name in [
        "CADENCE_CLAUDE_COMMAND",
        "CADENCE_CODEX_COMMAND",
        "CADENCE_CODEX_WS_COMMAND",
        "CADENCE_DEVIN_COMMAND",
        "CADENCE_CLAUDE_TUI_COMMAND",
        "CADENCE_STUB_COMMAND",
        "CADENCE_CURSOR_COMMAND",
        "CADENCE_TMUX_COMMAND",
    ] {
        test_env().set(name, "false");
    }
    let d = TestDaemon::start();
    let cwd = d.dir.path().to_str().unwrap().to_string();
    for spec in registry::SPECS {
        let alias = format!("cap-{}-{}", spec.provider, spec.endpoint_kind);
        d.rpc(
            "agent_register",
            json!({"alias": alias, "provider": spec.provider,
                   "endpoint_kind": spec.endpoint_kind, "cwd": cwd}),
        )
        .unwrap();
        let show = d.rpc("agent_show", json!({"alias": alias})).unwrap();
        assert_eq!(
            show["agent"]["capabilities"],
            spec.to_json(),
            "capabilities mismatch for {}/{}",
            spec.provider,
            spec.endpoint_kind
        );
        let list = d.rpc("agent_list", json!({})).unwrap();
        let row = list["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["alias"].as_str() == Some(alias.as_str()))
            .expect("registered agent missing from agent_list");
        assert_eq!(row["capabilities"], spec.to_json());
    }
    // `health.capabilities` is generated from the same table plus the
    // daemon-level features — identical content to the legacy list.
    let health = d.rpc("health", json!({})).unwrap();
    let caps: Vec<&str> = health["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    assert_eq!(caps, registry::capabilities());
    for name in [
        "managed_codex_stdio",
        "managed_codex_ws",
        "managed_claude_stream",
        "pty_devin_tmux",
        "inbox_endpoint",
        "fake_provider_tests",
    ] {
        assert!(caps.contains(&name), "health missing {name}");
    }
    // An unknown registered pair renders `capabilities: null`.
    d.rpc(
        "agent_register",
        json!({"alias": "cap-bogus", "provider": "devin",
               "endpoint_kind": "bogus", "cwd": cwd}),
    )
    .unwrap();
    let show = d.rpc("agent_show", json!({"alias": "cap-bogus"})).unwrap();
    assert!(show["agent"]["capabilities"].is_null());
}

// ── Claude TUI profile through the generic adapter (CAD-17) ────────

/// A fresh `claude` pty launch mints `--session-id`, the pane's own
/// process records it in the sessions registry, and ownership is
/// proven by matching that entry to a process under the pane.
#[test]
fn pty_claude_launch_mints_session_and_proves_ownership() {
    let d = TestDaemon::start();
    let mock = d.mock_claude_tui();
    d.register_claude_pty("cl", json!({"auto_ready": "verified"}));
    let agent = d.wait_agent("cl", "idle", 20);
    // The TUI paint that makes the probe read idle happens after the
    // mock's argv/env dumps — the cause ordered after them.
    wait_probe_idle(&d, "cl", 15);
    let session = agent["session_id"].as_str().unwrap().to_string();
    assert!(!session.is_empty(), "{agent}");
    // The profile passed its own launch flag verbatim to the pane.
    let argv = std::fs::read_to_string(d.claude_pane_file(&mock, "cl", "argv")).unwrap();
    assert!(
        argv.contains(&format!("--session-id {session}")),
        "argv: {argv} / session: {session}"
    );
    // The registry entry belongs to a live process in the pane tree.
    let entries = claude_sessions(&mock.sessions);
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].1, session);
    // tmux -e exports reached the pane (the real TUI's Stop hook cwd).
    let env = std::fs::read_to_string(d.claude_pane_file(&mock, "cl", "env")).unwrap();
    assert!(env.contains("CADENCE_ALIAS=cl"), "{env}");
    // The registry row advertises the tmux endpoint.
    let show = d.rpc("agent_show", json!({"alias": "cl"})).unwrap();
    let caps = &show["agent"]["capabilities"];
    assert_eq!(caps["attach"], "tmux", "{caps}");
    assert_eq!(caps["ready_gate"], true, "{caps}");
    assert_eq!(caps["reports"], "explicit", "{caps}");
}

/// `-r <id>` resumes the native session: the pane argv carries
/// `--resume` and the registry confirms the same id.
#[test]
fn pty_claude_resume_passes_resume_flag() {
    let d = TestDaemon::start();
    let mock = d.mock_claude_tui();
    d.register_claude_pty(
        "cl",
        json!({"session": "4f9b1c2e-aaaa-bbbb-cccc-0000000000aa"}),
    );
    let agent = d.wait_agent("cl", "idle", 20);
    assert_eq!(
        agent["session_id"].as_str().unwrap(),
        "4f9b1c2e-aaaa-bbbb-cccc-0000000000aa"
    );
    // Idle probe ⇒ the mock painted its TUI ⇒ its argv dump is on disk.
    wait_probe_idle(&d, "cl", 15);
    let argv = std::fs::read_to_string(d.claude_pane_file(&mock, "cl", "argv")).unwrap();
    assert!(
        argv.contains("--resume 4f9b1c2e-aaaa-bbbb-cccc-0000000000aa"),
        "{argv}"
    );
    let entries = claude_sessions(&mock.sessions);
    assert_eq!(entries[0].1, "4f9b1c2e-aaaa-bbbb-cccc-0000000000aa");
}

/// A live registry entry owned by a process outside the pane refuses
/// takeover — the session is already attached elsewhere.
#[test]
fn pty_claude_foreign_session_refuses_takeover() {
    let d = TestDaemon::start();
    let mock = d.mock_claude_tui();
    // A foreign live process claims "held-session".
    let mut holder = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = holder.id();
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let proc_start: u64 = stat[stat.rfind(')').unwrap() + 1..]
        .split_whitespace()
        .nth(19)
        .unwrap()
        .parse()
        .unwrap();
    std::fs::write(
        mock.sessions.join(format!("{pid}.json")),
        json!({"pid": pid, "sessionId": "held-session", "procStart": proc_start}).to_string(),
    )
    .unwrap();
    d.register_claude_pty("cl", json!({"session": "held-session"}));
    let agent = d.wait_agent("cl", "attention", 20);
    let err = agent["error"].as_str().unwrap_or("");
    assert!(err.contains("owned by another terminal"), "{err}");
    holder.kill().unwrap();
    let _ = holder.wait();
}

/// The registry claiming a different native session than the one we
/// asked for is a changed-owner fence, never an adoption.
#[test]
fn pty_claude_session_mismatch_fences_closed() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude_tui();
    // Real env, under the TUI mock's ENV_LOCK: the pane's mock process
    // reads the swap knob from the env the daemon passes to tmux.
    std::env::set_var("MOCK_CLAUDE_SWAP", "1");
    d.register_claude_pty("cl", json!({"session": "want-session"}));
    let agent = d.wait_agent("cl", "attention", 20);
    let err = agent["error"].as_str().unwrap_or("");
    assert!(err.contains("changed owner fails closed"), "{err}");
}

/// Send → paste → echo → explicit report, under the claude profile.
/// `auto_ready=verified` self-claims on the boxed idle `❯` prompt.
#[test]
fn pty_claude_send_pastes_and_completes_via_report() {
    let d = TestDaemon::start();
    let mock = d.mock_claude_tui();
    d.register_claude_pty("cl", json!({"auto_ready": "verified"}));
    d.wait_agent("cl", "idle", 20);
    d.rpc(
        "agent_send",
        json!({"alias": "cl", "text": "say hi claude", "message": "m1"}),
    )
    .unwrap();
    let token = pty_token(&d, "cl", "m1");
    assert!(token.starts_with("pty-"), "{token}");
    let claim = d.wait_event("cl", "ready_claimed", 5);
    assert_eq!(claim["payload"]["by"], "daemon", "{claim}");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let screen =
            std::fs::read_to_string(d.claude_pane_file(&mock, "cl", "screen")).unwrap_or_default();
        if screen.contains("MOCK_REPLY: say hi claude") {
            break;
        }
        assert!(Instant::now() < deadline, "no reply: {screen}");
        thread::sleep(Duration::from_millis(50));
    }
    d.rpc(
        "message_report",
        json!({"message": "m1", "token": token, "kind": "result",
               "text": "done"}),
    )
    .unwrap();
    d.wait_message("cl", "m1", &["completed"], 10);
}

/// Claude's forbidden prefixes — `/` command menu, `!` shell mode,
/// `@` autocomplete — observed live — reject pre-write and keep the
/// claim; `#` is a literal draft char.
#[test]
fn pty_claude_forbidden_prefixes_reject_prewrite() {
    let d = TestDaemon::start();
    let mock = d.mock_claude_tui();
    d.register_claude_pty("cl", json!({}));
    d.wait_agent("cl", "idle", 20);
    d.rpc("agent_ready", json!({"alias": "cl"})).unwrap();
    for (id, body, prefix) in [
        ("m1", "/quit", '/'),
        ("m2", "!ls", '!'),
        ("m3", "@agent", '@'),
        ("m4", "  /indented also forbidden", '/'),
    ] {
        d.rpc(
            "agent_send",
            json!({"alias": "cl", "text": body, "message": id}),
        )
        .unwrap();
        d.wait_message("cl", id, &["failed"], 15);
        let failed = d.rpc("agent_show", json!({"alias": "cl"})).unwrap()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id)
            .unwrap()
            .clone();
        let err = failed["result"]["error"].as_str().unwrap_or("").to_string();
        assert!(err.contains(&format!("'{prefix}'")), "{err}");
        assert!(err.contains("command or mode switch"), "{err}");
    }
    // No bytes reached the pane for any rejection.
    assert!(
        std::fs::read_to_string(d.claude_pane_file(&mock, "cl", "input"))
            .unwrap_or_default()
            .is_empty()
    );
    // The claim survived: `#` pastes on it without a second ready.
    d.rpc(
        "agent_send",
        json!({"alias": "cl", "text": "# literal tag", "message": "m5"}),
    )
    .unwrap();
    let token = pty_token(&d, "cl", "m5");
    assert!(token.starts_with("pty-"), "{token}");
}

/// Claude's own busy line — not Devin's — holds the gate, and its
/// permission menu (answered in the terminal, never via respond)
/// blocks sends until the worker resolves it.
#[test]
fn pty_claude_busy_and_approval_gate_sends() {
    let d = TestDaemon::start();
    let mock = d.mock_claude_tui();
    d.register_claude_pty("cl", json!({"auto_ready": "verified"}));
    d.wait_agent("cl", "idle", 20);
    let state = d.claude_pane_file(&mock, "cl", "tui-state");
    // Devin's marker is inert under this profile.
    std::fs::write(&state, "stub working\n").unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "cl", "text": "stub marker is inert", "message": "m1"}),
    )
    .unwrap();
    pty_token(&d, "cl", "m1");
    // The claude spinner's own phrase holds the gate.
    std::fs::write(&state, "✻ Churning… (esc to interrupt · 4s)\n").unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "cl", "text": "do not paste", "message": "m2"}),
    )
    .unwrap();
    let wait = d.wait_event("cl", "gate_wait", 10);
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("busy"),
        "{wait}"
    );
    let probe = d.rpc("agent_probe", json!({"alias": "cl"})).unwrap();
    assert_eq!(probe["busy_marker"], true, "{probe}");
    std::fs::remove_file(&state).unwrap();
    pty_token(&d, "cl", "m2");
    // A permission select holds the next send; the pane answers it.
    std::fs::write(
        &state,
        "Do you want to proceed?\n ❯ 1. Yes\n   2. No\n\n Esc to cancel\n",
    )
    .unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "cl", "text": "queued behind menu", "message": "m3"}),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let wait = loop {
        let e = d
            .events("cl")
            .into_iter()
            .find(|e| {
                e["kind"].as_str() == Some("gate_wait")
                    && e["payload"]["message"].as_str() == Some("m3")
            })
            .unwrap_or_default();
        if !e.is_null() {
            break e;
        }
        assert!(Instant::now() < deadline, "m3 never hit the gate");
        thread::sleep(Duration::from_millis(50));
    };
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("approval menu"),
        "{wait}"
    );
    let probe = d.rpc("agent_probe", json!({"alias": "cl"})).unwrap();
    assert_eq!(probe["approval_menu"], true, "{probe}");
    std::fs::remove_file(&state).unwrap();
    pty_token(&d, "cl", "m3");
}

/// `cadence claude --tui` launches the pty endpoint through the CLI —
/// flags, endpoint selection and param passing end to end.
#[test]
fn cli_claude_tui_flag_launches_pty_endpoint() {
    let d = TestDaemon::start();
    let mock = d.mock_claude_tui();
    let bin = env!("CARGO_BIN_EXE_cadence");
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["claude", "--tui", "--alias", "cl", "--cwd"])
        .arg(d.dir.path())
        .args(["--detach", "--no-bootstrap"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let agent = d.wait_agent("cl", "idle", 20);
    assert_eq!(agent["provider"], "claude");
    assert_eq!(agent["endpoint_kind"], "pty");
    wait_probe_idle(&d, "cl", 15);
    let argv = std::fs::read_to_string(d.claude_pane_file(&mock, "cl", "argv")).unwrap();
    assert!(argv.contains("--session-id"), "{argv}");
}

/// `cadence claude --tui -r` forwards the resume flag to the pane.
#[test]
fn cli_claude_tui_resume_passes_resume() {
    let d = TestDaemon::start();
    let mock = d.mock_claude_tui();
    let bin = env!("CARGO_BIN_EXE_cadence");
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "claude",
            "--tui",
            "-r",
            "c1d2e3f4-1111-2222-3333-444455556666",
            "--alias",
            "cl",
            "--cwd",
        ])
        .arg(d.dir.path())
        .args(["--detach", "--no-bootstrap"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_agent("cl", "idle", 20);
    wait_probe_idle(&d, "cl", 15);
    let argv = std::fs::read_to_string(d.claude_pane_file(&mock, "cl", "argv")).unwrap();
    assert!(
        argv.contains("--resume c1d2e3f4-1111-2222-3333-444455556666"),
        "{argv}"
    );
}

/// `-r` without `--tui` names a managed-agent resume — rejected rather
/// than silently registered as a param the managed endpoint ignores.
#[test]
fn cli_claude_resume_without_tui_rejected() {
    let d = TestDaemon::start();
    let bin = env!("CARGO_BIN_EXE_cadence");
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["claude", "-r", "some-session", "--cwd"])
        .arg(d.dir.path())
        .args(["--detach", "--no-bootstrap"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("requires `--tui`"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `--tui` on a provider with no pty endpoint is a clear error.
#[test]
fn cli_tui_flag_rejects_provider_without_pty() {
    let d = TestDaemon::start();
    let bin = env!("CARGO_BIN_EXE_cadence");
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["codex", "--tui", "--cwd"])
        .arg(d.dir.path())
        .args(["--detach", "--no-bootstrap"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("has no pty endpoint"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `cadence join <group> claude --tui` threads the flag through:
/// the worker opens as a claude pty endpoint and its briefing names
/// the forbidden input prefixes.
#[test]
fn cli_join_claude_tui_briefs_prefixes() {
    let d = TestDaemon::start();
    let mock = d.mock_claude_tui();
    let pm_repo = d.dir.path().join("pmrepo");
    git_repo(&pm_repo);
    d.rpc(
        "agent_register",
        json!({"alias": "pm", "provider": "fake", "endpoint_kind": "fake",
               "cwd": pm_repo}),
    )
    .unwrap();
    d.wait_agent("pm", "idle", 10);
    let bin = env!("CARGO_BIN_EXE_cadence");
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["join", "pm", "claude", "--tui", "--alias", "wj", "--detach"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let agent = d.wait_agent("wj", "idle", 20);
    assert_eq!(agent["provider"], "claude");
    assert_eq!(agent["endpoint_kind"], "pty");
    wait_probe_idle(&d, "wj", 15);
    let argv = std::fs::read_to_string(d.claude_pane_file(&mock, "wj", "argv")).unwrap();
    assert!(argv.contains("--session-id"), "{argv}");
    // The briefing tells the worker which leading chars never paste.
    let briefing = d.state.join("briefings/pm/BRIEFING-wj.md");
    let text = std::fs::read_to_string(&briefing).unwrap();
    for want in ["`/`", "`!`", "`@`", "refused"] {
        assert!(text.contains(want), "briefing missing {want}:\n{text}");
    }
    // The group upstream is in params so results route to the PM.
    assert_eq!(agent["params"]["upstream"], "pm", "{agent}");
}

/// `join <pm> codex` records the worker's sandbox on the agent row and
/// sends it on `thread/start` — `workspace-write` by default (the
/// unattended-worker posture), `read-only` only when asked.
#[test]
fn cli_join_codex_sandbox_defaults_writable_and_flag_roundtrips() {
    let d = TestDaemon::start();
    let mock = d.mock_codex_ws("ok");
    let pm_repo = d.dir.path().join("pmrepo");
    git_repo(&pm_repo);
    d.rpc(
        "agent_register",
        json!({"alias": "pm", "provider": "fake", "endpoint_kind": "fake",
               "cwd": pm_repo}),
    )
    .unwrap();
    d.wait_agent("pm", "idle", 10);
    let bin = env!("CARGO_BIN_EXE_cadence");
    // Default: a joined codex worker is writable — the same trust
    // posture the other providers already run.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["join", "pm", "codex", "--alias", "wj", "--detach"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let agent = d.wait_agent("wj", "idle", 20);
    assert_eq!(agent["sandbox"], "workspace-write", "{agent}");
    // Explicit `read-only` still round-trips.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "join",
            "pm",
            "codex",
            "--alias",
            "wr",
            "--detach",
            "--sandbox",
            "read-only",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let agent = d.wait_agent("wr", "idle", 20);
    assert_eq!(agent["sandbox"], "read-only", "{agent}");
    // Both postures reached the wire on thread/start, paired with the
    // default `never` approval policy.
    let reqs = mock_requests(&mock);
    assert_eq!(reqs.len(), 2, "{reqs:?}");
    assert_eq!(reqs[0]["method"], "thread/start");
    assert_eq!(reqs[0]["params"]["sandbox"], "workspace-write");
    assert_eq!(reqs[0]["params"]["approvalPolicy"], "never");
    assert_eq!(reqs[1]["params"]["sandbox"], "read-only");
    assert_eq!(reqs[1]["params"]["approvalPolicy"], "never");
    // A bogus value is a clap rejection naming both accepted values.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "join",
            "pm",
            "codex",
            "--alias",
            "wb",
            "--detach",
            "--sandbox",
            "bogus",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("read-only") && err.contains("workspace-write"),
        "{err}"
    );
}

// ── Cursor TUI profile through the generic adapter (CAD-56) ────────

/// A fresh `cursor` pty launch mints a chat id via `create-chat`,
/// resumes it in the pane argv, and proves ownership through the
/// `store.db` fd the pane holds under the chats dir.
#[test]
fn pty_cursor_launch_mints_chat_and_proves_ownership() {
    let d = TestDaemon::start();
    let mock = d.mock_cursor_tui();
    d.register_cursor_pty("cu", json!({"auto_ready": "verified"}));
    let agent = d.wait_agent("cu", "idle", 20);
    // The TUI paint that makes the probe read idle happens after the
    // mock's argv/env dumps — the cause ordered after them.
    wait_probe_idle(&d, "cu", 15);
    let session = agent["session_id"].as_str().unwrap().to_string();
    assert!(uuid::Uuid::parse_str(&session).is_ok(), "{session}");
    // The profile minted the chat then resumed it — the pane's own
    // argv names its session.
    let argv = std::fs::read_to_string(d.cursor_pane_file(&mock, "cu", "argv")).unwrap();
    assert!(
        argv.contains(&format!("--resume {session}")),
        "argv: {argv} / session: {session}"
    );
    assert!(argv.contains("--trust"), "{argv}");
    // The chat's store.db sits under the chats dir, held open by the
    // pane — the same proof the real TUI's fd gives.
    let db = mock.chats.join("mockhash").join(&session).join("store.db");
    assert!(db.exists(), "{db:?}");
    // tmux -e exports reached the pane (cadence self inside the TUI).
    let env = std::fs::read_to_string(d.cursor_pane_file(&mock, "cu", "env")).unwrap();
    assert!(env.contains("CADENCE_ALIAS=cu"), "{env}");
    // The registry row advertises the tmux endpoint.
    let show = d.rpc("agent_show", json!({"alias": "cu"})).unwrap();
    let caps = &show["agent"]["capabilities"];
    assert_eq!(caps["attach"], "tmux", "{caps}");
    assert_eq!(caps["ready_gate"], true, "{caps}");
    assert_eq!(caps["reports"], "explicit", "{caps}");
    assert_eq!(caps["session_id_label"], "Cursor chat", "{caps}");
}

/// `-r <chatId>` resumes an existing chat: the pane argv carries
/// `--resume <id>` and the store the pane opens names the same chat.
#[test]
fn pty_cursor_resume_passes_resume_flag() {
    let d = TestDaemon::start();
    let mock = d.mock_cursor_tui();
    // A random id: the ownership scan is host-wide, so a fixed uuid
    // could collide with a real `cursor-agent --resume` left running.
    let chat = format!("test-resume-{}", uuid::Uuid::new_v4().simple());
    d.register_cursor_pty("cu", json!({"session": chat}));
    let agent = d.wait_agent("cu", "idle", 20);
    assert_eq!(agent["session_id"].as_str().unwrap(), chat);
    // Idle probe ⇒ the mock painted its TUI ⇒ its argv dump is on disk.
    wait_probe_idle(&d, "cu", 15);
    let argv = std::fs::read_to_string(d.cursor_pane_file(&mock, "cu", "argv")).unwrap();
    assert!(argv.contains(&format!("--resume {chat}")), "{argv}");
    assert!(mock
        .chats
        .join("mockhash")
        .join(&chat)
        .join("store.db")
        .exists());
}

/// A live store.db held by a process outside the pane refuses
/// takeover — the chat is already attached elsewhere.
#[test]
fn pty_cursor_foreign_session_refuses_takeover() {
    let d = TestDaemon::start();
    let mock = d.mock_cursor_tui();
    // A foreign live process holds "held-chat"'s store.db — the same
    // proof shape as another cursor-agent TUI.
    let held = mock.chats.join("mockhash").join("held-chat");
    std::fs::create_dir_all(&held).unwrap();
    let db = held.join("store.db");
    std::fs::write(&db, "").unwrap();
    let mut holder = std::process::Command::new("python3")
        .args([
            "-c",
            &format!(
                "import time; f=open('{}','a'); time.sleep(30)",
                db.display()
            ),
        ])
        .spawn()
        .unwrap();
    d.register_cursor_pty("cu", json!({"session": "held-chat"}));
    let agent = d.wait_agent("cu", "attention", 20);
    let err = agent["error"].as_str().unwrap_or("");
    assert!(err.contains("owned by another terminal"), "{err}");
    holder.kill().unwrap();
    let _ = holder.wait();
}

/// A foreign `cursor-agent` argv carrying `--resume <chat>` is the
/// same ownership proof as the store.db fd — a fabricated argv with
/// no chat-store fd at all still refuses takeover.
#[test]
fn pty_cursor_foreign_argv_refuses_takeover() {
    let d = TestDaemon::start();
    let _mock = d.mock_cursor_tui();
    // `exec -a` fabricates argv[0]=cursor-agent; python keeps the
    // `--resume argv-held` tail on its cmdline while it sleeps.
    let mut holder = std::process::Command::new("bash")
        .args([
            "-c",
            "exec -a cursor-agent python3 -c 'import time; time.sleep(60)' --resume argv-held",
        ])
        .spawn()
        .unwrap();
    // The exec is async — wait until the fabricated argv is visible
    // in /proc before registering.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let cmdline = std::fs::read(format!("/proc/{}/cmdline", holder.id())).unwrap_or_default();
        if cmdline.windows(8).any(|w| w == b"--resume") {
            break;
        }
        assert!(Instant::now() < deadline, "fabricated argv never appeared");
        thread::sleep(Duration::from_millis(20));
    }
    d.register_cursor_pty("cu", json!({"session": "argv-held"}));
    let agent = d.wait_agent("cu", "attention", 20);
    let err = agent["error"].as_str().unwrap_or("");
    assert!(err.contains("owned by another terminal"), "{err}");
    holder.kill().unwrap();
    let _ = holder.wait();
}

/// A fresh open mints once: the id is folded into `params.session`
/// before the pane exists (`cadence/session_minted`), so the respawn
/// after a stop resumes the same chat rather than minting a new one.
/// `agent set --next-launch model=…` is legal for a pty endpoint and
/// replays verbatim on the next open.
#[test]
fn pty_cursor_minted_session_persists_and_model_replays() {
    let d = TestDaemon::start();
    let mock = d.mock_cursor_tui();
    d.register_cursor_pty("cu", json!({"model": "grok-one"}));
    let agent = d.wait_agent("cu", "idle", 20);
    wait_probe_idle(&d, "cu", 15);
    let sid = agent["session_id"].as_str().unwrap().to_string();
    let argv_file = d.cursor_pane_file(&mock, "cu", "argv");
    let argv1 = std::fs::read_to_string(&argv_file).unwrap();
    assert!(argv1.contains("--model grok-one"), "{argv1}");
    assert!(argv1.contains(&format!("--resume {sid}")), "{argv1}");
    // The mint landed in params.session at open — not only in
    // thread_id after it — so even an unproven launch keeps its id.
    let show = d.rpc("agent_show", json!({"alias": "cu"})).unwrap();
    assert_eq!(
        show["agent"]["params"]["session"].as_str().unwrap(),
        sid,
        "{show}"
    );
    d.rpc(
        "agent_set",
        json!({"alias": "cu", "patch": {"model": "grok-two"},
               "next_launch": true}),
    )
    .unwrap();
    d.rpc("agent_stop", json!({"alias": "cu"})).unwrap();
    d.rpc("agent_resume", json!({"alias": "cu"})).unwrap();
    let agent = d.wait_agent("cu", "idle", 20);
    // Same chat — the respawn resumed the minted id, never re-minted.
    assert_eq!(agent["session_id"].as_str().unwrap(), sid);
    wait_probe_idle(&d, "cu", 15);
    let argv2 = std::fs::read_to_string(&argv_file).unwrap();
    assert!(argv2.contains("--model grok-two"), "{argv2}");
    assert!(!argv2.contains("--model grok-one"), "{argv2}");
    assert!(argv2.contains(&format!("--resume {sid}")), "{argv2}");
}

/// Launch merges `Shell(cadence)` into the CLI's allowlist
/// (`<chats>/../cli-config.json`), preserving the document and
/// dropping a `.bak`; a second launch leaves the file untouched.
#[test]
fn pty_cursor_allowlist_merged_into_cli_config() {
    let d = TestDaemon::start();
    let mock = d.mock_cursor_tui();
    let config = mock.dir.join("cli-config.json");
    std::fs::write(
        &config,
        "{\n  \"permissions\": {\n    \"allow\": [\"Shell(ls)\"],\n    \"deny\": []\n  },\n  \"display\": {\"mode\": \"zen\"}\n}\n",
    )
    .unwrap();
    d.register_cursor_pty("cu", json!({}));
    d.wait_agent("cu", "idle", 20);
    let text = std::fs::read_to_string(&config).unwrap();
    let doc: Value = serde_json::from_str(&text).unwrap();
    let allow = doc["permissions"]["allow"].as_array().unwrap();
    assert!(allow.iter().any(|e| e == "Shell(cadence)"), "{text}");
    assert!(allow.iter().any(|e| e == "Shell(ls)"), "{text}");
    assert_eq!(doc["display"]["mode"].as_str().unwrap(), "zen", "{text}");
    let bak = std::fs::read_to_string(mock.dir.join("cli-config.json.bak")).unwrap();
    assert!(bak.contains("Shell(ls)"), "{bak}");
    // A resume leaves an already-merged config byte-identical.
    let before = std::fs::read_to_string(&config).unwrap();
    d.rpc("agent_stop", json!({"alias": "cu"})).unwrap();
    d.rpc("agent_resume", json!({"alias": "cu"})).unwrap();
    d.wait_agent("cu", "idle", 20);
    assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
}

/// An unparseable `cli-config.json` refuses the launch with a clear
/// error rather than clobbering the user's settings.
#[test]
fn pty_cursor_malformed_cli_config_refuses_launch() {
    let d = TestDaemon::start();
    let mock = d.mock_cursor_tui();
    let config = mock.dir.join("cli-config.json");
    std::fs::write(&config, "{ not json").unwrap();
    d.register_cursor_pty("cu", json!({}));
    let agent = d.wait_agent("cu", "attention", 20);
    let err = agent["error"].as_str().unwrap_or("");
    assert!(err.contains("not valid JSON"), "{err}");
    assert!(err.contains("refusing to launch"), "{err}");
    // The malformed file is left exactly as found.
    assert_eq!(std::fs::read_to_string(&config).unwrap(), "{ not json");
}

/// The pane opening a different chat than the one we asked for is a
/// changed-owner fence, never an adoption.
#[test]
fn pty_cursor_session_mismatch_fences_closed() {
    let d = TestDaemon::start();
    let _mock = d.mock_cursor_tui();
    // Real env, under the TUI mock's ENV_LOCK: the pane's mock process
    // reads the swap knob from the env the daemon passes to tmux.
    std::env::set_var("MOCK_CURSOR_SWAP", "1");
    d.register_cursor_pty("cu", json!({"session": "want-chat"}));
    let agent = d.wait_agent("cu", "attention", 20);
    let err = agent["error"].as_str().unwrap_or("");
    assert!(err.contains("changed owner fails closed"), "{err}");
}

/// A stored chat the TUI cannot resume (exits on it — the real CLI
/// dies on a deleted chat) must not wedge the alias: the failed
/// resume clears `params.session`, and the next open mints fresh.
#[test]
fn pty_cursor_unresumable_chat_clears_and_mints_fresh() {
    let d = TestDaemon::start();
    let _mock = d.mock_cursor_tui();
    std::env::set_var("MOCK_CURSOR_DIE_ON", "dead-chat");
    d.register_cursor_pty("cu", json!({"session": "dead-chat"}));
    let agent = d.wait_agent("cu", "attention", 20);
    let err = agent["error"].as_str().unwrap_or("");
    assert!(err.contains("pane exited during TUI startup"), "{err}");
    // The failure is operator-visible until the next open: the error
    // names the cleared id and says what happens next.
    assert!(err.contains("'dead-chat'"), "{err}");
    assert!(err.contains("was cleared"), "{err}");
    let ev = d.wait_event("cu", "session_resume_failed", 5);
    assert_eq!(ev["payload"]["session"], "dead-chat", "{ev}");
    // The stored session is cleared — a resume attempt must not find
    // the dead id again.
    let show = d.rpc("agent_show", json!({"alias": "cu"})).unwrap();
    assert!(
        show["agent"]["params"]["session"].is_null(),
        "session not cleared: {show}"
    );
    // `agent resume` mints a fresh chat — the alias unwedges instead
    // of retrying the dead id forever.
    d.rpc("agent_resume", json!({"alias": "cu"})).unwrap();
    let agent = d.wait_agent("cu", "idle", 20);
    let native = agent["thread_id"].as_str().unwrap();
    assert_ne!(native, "dead-chat");
    let minted = d.wait_event("cu", "session_minted", 5);
    assert_eq!(minted["payload"]["session"], native, "{minted}");
}

/// A chat that WAS proven once and then dies still unwedges: the
/// clear drops `thread_id` alongside `params.session` — both feed the
/// adapter's `desired_session`, so dropping only params would keep
/// resuming the dead id through the thread fallback.
#[test]
fn pty_cursor_proven_chat_deleted_mints_fresh() {
    let d = TestDaemon::start();
    let _mock = d.mock_cursor_tui();
    d.register_cursor_pty("cu", json!({}));
    let agent = d.wait_agent("cu", "idle", 20);
    let proven = agent["thread_id"].as_str().unwrap().to_string();
    // The proven chat is gone from the host: a TUI asked to resume it
    // exits, the deleted-chat shape.
    std::env::set_var("MOCK_CURSOR_DIE_ON", &proven);
    d.rpc("agent_stop", json!({"alias": "cu"})).unwrap();
    d.rpc("agent_resume", json!({"alias": "cu"})).unwrap();
    let agent = d.wait_agent("cu", "attention", 20);
    let err = agent["error"].as_str().unwrap_or("");
    assert!(err.contains("pane exited during TUI startup"), "{err}");
    assert!(err.contains(&format!("'{proven}'")), "{err}");
    let ev = d.wait_event("cu", "session_resume_failed", 5);
    assert_eq!(ev["payload"]["session"], proven.as_str(), "{ev}");
    let show = d.rpc("agent_show", json!({"alias": "cu"})).unwrap();
    assert!(
        show["agent"]["params"]["session"].is_null(),
        "session not cleared: {show}"
    );
    assert!(
        show["agent"]["thread_id"].is_null(),
        "thread_id not cleared: {show}"
    );
    d.rpc("agent_resume", json!({"alias": "cu"})).unwrap();
    let agent = d.wait_agent("cu", "idle", 20);
    let native = agent["thread_id"].as_str().unwrap();
    assert_ne!(native, proven);
}

/// A foreign session on the host is never adopted: once the stored id
/// is cleared, the next open mints a fresh chat and leaves the
/// foreign-owned one alone.
#[test]
fn pty_cursor_cleared_session_leaves_foreign_chat() {
    let d = TestDaemon::start();
    let mock = d.mock_cursor_tui();
    // A foreign live process holds "foreign-chat"'s store.db — the
    // same proof shape as another cursor-agent TUI.
    let held = mock.chats.join("mockhash").join("foreign-chat");
    std::fs::create_dir_all(&held).unwrap();
    let db = held.join("store.db");
    std::fs::write(&db, "").unwrap();
    let mut holder = std::process::Command::new("python3")
        .args([
            "-c",
            &format!(
                "import time; f=open('{}','a'); time.sleep(60)",
                db.display()
            ),
        ])
        .spawn()
        .unwrap();
    std::env::set_var("MOCK_CURSOR_DIE_ON", "dead-chat");
    d.register_cursor_pty("cu", json!({"session": "dead-chat"}));
    d.wait_agent("cu", "attention", 20);
    let show = d.rpc("agent_show", json!({"alias": "cu"})).unwrap();
    assert!(
        show["agent"]["params"]["session"].is_null(),
        "session not cleared: {show}"
    );
    // The next open mints fresh — it neither resumes the dead id nor
    // adopts the foreign chat.
    d.rpc("agent_resume", json!({"alias": "cu"})).unwrap();
    let agent = d.wait_agent("cu", "idle", 20);
    let native = agent["thread_id"].as_str().unwrap();
    assert_ne!(native, "dead-chat");
    assert_ne!(native, "foreign-chat");
    holder.kill().unwrap();
    let _ = holder.wait();
}

/// The unresumable-session escape is opt-in for disposable sessions
/// only: a Claude TUI resume whose session proof times out keeps
/// `params.session` and lands `attention` — an operator-supplied
/// Claude session must never be dropped on a transient failure.
#[test]
fn pty_claude_resume_timeout_keeps_session() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude_tui();
    // The pane stays alive but never publishes its session — the open
    // wait runs to the claude profile's deadline.
    std::env::set_var("MOCK_CLAUDE_NO_REGISTRY", "1");
    d.register_claude_pty("cl", json!({"session": "claude-session-1"}));
    let agent = d.wait_agent("cl", "attention", 60);
    let err = agent["error"].as_str().unwrap_or("");
    assert!(err.contains("timed out"), "{err}");
    assert!(
        d.events("cl")
            .iter()
            .all(|e| e["kind"].as_str() != Some("session_resume_failed")),
        "session_resume_failed must not fire for a non-disposable profile"
    );
    let show = d.rpc("agent_show", json!({"alias": "cl"})).unwrap();
    assert_eq!(
        show["agent"]["params"]["session"].as_str(),
        Some("claude-session-1"),
        "session must survive a transient proof timeout: {show}"
    );
}

/// A pane that survives a daemon restart is re-adopted under the same
/// chat — the surviving store.db fd is the ownership proof again.
#[test]
fn pty_cursor_restart_readopts_pane() {
    let dir = TempDir::new().unwrap();
    let seeded = dir.path().join("state");
    std::fs::create_dir_all(&seeded).unwrap();
    let fixtures = TempDir::new().unwrap();
    {
        let _mock = install_mock_cursor_tui(fixtures.path());
        let d = TestDaemon::start_on(seeded.clone());
        d.register_cursor_pty("cu", json!({}));
        let agent = d.wait_agent("cu", "idle", 20);
        let native = agent["thread_id"].as_str().unwrap().to_string();
        let pane_pid = agent["pid"].as_i64().unwrap();
        // Daemon restart: the mock tmux server (fixture dir) outlives
        // it, so the pane is still alive and must be reattached, not
        // relaunched.
        drop(d);
        let d2 = TestDaemon::start_on(seeded.clone());
        let agent2 = d2.wait_agent("cu", "idle", 25);
        assert_eq!(agent2["thread_id"].as_str().unwrap(), native);
        assert_eq!(agent2["pid"].as_i64().unwrap(), pane_pid);
    }
}

/// Send → paste → echo → explicit report, under the cursor profile.
/// `auto_ready=verified` self-claims on the empty `→` prompt.
#[test]
fn pty_cursor_send_pastes_and_completes_via_report() {
    let d = TestDaemon::start();
    let mock = d.mock_cursor_tui();
    d.register_cursor_pty("cu", json!({"auto_ready": "verified"}));
    d.wait_agent("cu", "idle", 20);
    d.rpc(
        "agent_send",
        json!({"alias": "cu", "text": "say hi cursor", "message": "m1"}),
    )
    .unwrap();
    let token = pty_token(&d, "cu", "m1");
    assert!(token.starts_with("pty-"), "{token}");
    let claim = d.wait_event("cu", "ready_claimed", 5);
    assert_eq!(claim["payload"]["by"], "daemon", "{claim}");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let screen =
            std::fs::read_to_string(d.cursor_pane_file(&mock, "cu", "screen")).unwrap_or_default();
        if screen.contains("MOCK_REPLY: say hi cursor") {
            break;
        }
        assert!(Instant::now() < deadline, "no reply: {screen}");
        thread::sleep(Duration::from_millis(50));
    }
    d.rpc(
        "message_report",
        json!({"message": "m1", "token": token, "kind": "result",
               "text": "done"}),
    )
    .unwrap();
    d.wait_message("cu", "m1", &["completed"], 10);
}

/// Cursor's forbidden prefixes — `/` command menu, `!` shell mode,
/// `@` file picker — observed live — reject pre-write and keep the
/// claim; `#` is a literal draft char.
#[test]
fn pty_cursor_forbidden_prefixes_reject_prewrite() {
    let d = TestDaemon::start();
    let mock = d.mock_cursor_tui();
    d.register_cursor_pty("cu", json!({}));
    d.wait_agent("cu", "idle", 20);
    d.rpc("agent_ready", json!({"alias": "cu"})).unwrap();
    for (id, body, prefix) in [
        ("m1", "/skills", '/'),
        ("m2", "!git status", '!'),
        ("m3", "@file", '@'),
        ("m4", "  /indented also forbidden", '/'),
    ] {
        d.rpc(
            "agent_send",
            json!({"alias": "cu", "text": body, "message": id}),
        )
        .unwrap();
        d.wait_message("cu", id, &["failed"], 15);
        let failed = d.rpc("agent_show", json!({"alias": "cu"})).unwrap()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id)
            .unwrap()
            .clone();
        let err = failed["result"]["error"].as_str().unwrap_or("").to_string();
        assert!(err.contains(&format!("'{prefix}'")), "{err}");
        assert!(err.contains("command or mode switch"), "{err}");
    }
    // No bytes reached the pane for any rejection.
    assert!(
        std::fs::read_to_string(d.cursor_pane_file(&mock, "cu", "input"))
            .unwrap_or_default()
            .is_empty()
    );
    // The claim survived: `#` pastes on it without a second ready.
    d.rpc(
        "agent_send",
        json!({"alias": "cu", "text": "# literal tag", "message": "m5"}),
    )
    .unwrap();
    let token = pty_token(&d, "cu", "m5");
    assert!(token.starts_with("pty-"), "{token}");
}

/// Cursor's own busy line — the interrupt hint on the input row or
/// the spinner above it — holds the gate, and its permission menu
/// (answered in the terminal, never via respond) blocks sends until
/// the worker resolves it.
#[test]
fn pty_cursor_busy_and_approval_gate_sends() {
    let d = TestDaemon::start();
    let mock = d.mock_cursor_tui();
    d.register_cursor_pty("cu", json!({"auto_ready": "verified"}));
    d.wait_agent("cu", "idle", 20);
    let state = d.cursor_pane_file(&mock, "cu", "tui-state");
    // Devin's marker is inert under this profile.
    std::fs::write(&state, "stub working\n").unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "cu", "text": "stub marker is inert", "message": "m1"}),
    )
    .unwrap();
    pty_token(&d, "cu", "m1");
    // The cursor spinner's own shape holds the gate — status row
    // above the input line plus the interrupt hint on it. (The
    // model/cwd bar renders below the input row on real frames.)
    std::fs::write(
        &state,
        " ⠠⠛ Running  30 tokens\n  → Add a follow-up     ctrl+c to stop\n  /mock · main\n",
    )
    .unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "cu", "text": "do not paste", "message": "m2"}),
    )
    .unwrap();
    let wait = d.wait_event("cu", "gate_wait", 10);
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("busy"),
        "{wait}"
    );
    let probe = d.rpc("agent_probe", json!({"alias": "cu"})).unwrap();
    assert_eq!(probe["busy_marker"], true, "{probe}");
    std::fs::remove_file(&state).unwrap();
    pty_token(&d, "cu", "m2");
    // A permission select holds the next send; the pane answers it.
    std::fs::write(
        &state,
        " Run this command?\n Not in allowlist: whoami\n  → Run (once) (y)\n    Skip & tell the agent what to do instead (esc or n)\n",
    )
    .unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "cu", "text": "queued behind menu", "message": "m3"}),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let wait = loop {
        let e = d
            .events("cu")
            .into_iter()
            .find(|e| {
                e["kind"].as_str() == Some("gate_wait")
                    && e["payload"]["message"].as_str() == Some("m3")
            })
            .unwrap_or_default();
        if !e.is_null() {
            break e;
        }
        assert!(Instant::now() < deadline, "m3 never hit the gate");
        thread::sleep(Duration::from_millis(50));
    };
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("approval menu"),
        "{wait}"
    );
    let probe = d.rpc("agent_probe", json!({"alias": "cu"})).unwrap();
    assert_eq!(probe["approval_menu"], true, "{probe}");
    std::fs::remove_file(&state).unwrap();
    pty_token(&d, "cu", "m3");
}

/// `cadence cursor` launches the pty endpoint through the CLI — the
/// pane mints a chat, resumes it with `--trust`, and the endpoint
/// opens as cursor/pty.
#[test]
fn cli_cursor_launch_opens_pty_endpoint() {
    let d = TestDaemon::start();
    let mock = d.mock_cursor_tui();
    let bin = env!("CARGO_BIN_EXE_cadence");
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["cursor", "--alias", "cu", "--cwd"])
        .arg(d.dir.path())
        .args(["--detach", "--no-bootstrap"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let agent = d.wait_agent("cu", "idle", 20);
    assert_eq!(agent["provider"], "cursor");
    assert_eq!(agent["endpoint_kind"], "pty");
    wait_probe_idle(&d, "cu", 15);
    let argv = std::fs::read_to_string(d.cursor_pane_file(&mock, "cu", "argv")).unwrap();
    assert!(
        argv.contains("--trust") && argv.contains("--resume"),
        "{argv}"
    );
}

/// `cadence cursor -r <chatId>` forwards the resume flag to the pane.
#[test]
fn cli_cursor_resume_passes_resume() {
    let d = TestDaemon::start();
    let mock = d.mock_cursor_tui();
    // Random like the pty resume test — a real `cursor-agent --resume`
    // holding the same chat id would (correctly) refuse the takeover.
    let chat = format!("test-resume-{}", uuid::Uuid::new_v4().simple());
    let bin = env!("CARGO_BIN_EXE_cadence");
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["cursor", "-r", &chat, "--alias", "cu", "--cwd"])
        .arg(d.dir.path())
        .args(["--detach", "--no-bootstrap"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_agent("cu", "idle", 20);
    wait_probe_idle(&d, "cu", 15);
    let argv = std::fs::read_to_string(d.cursor_pane_file(&mock, "cu", "argv")).unwrap();
    assert!(argv.contains(&format!("--resume {chat}")), "{argv}");
}

/// `cadence cursor --permission-mode bogus` names the two accepted
/// values and registers nothing.
#[test]
fn cli_cursor_permission_mode_validated() {
    let d = TestDaemon::start();
    let _mock = d.mock_cursor_tui();
    let bin = env!("CARGO_BIN_EXE_cadence");
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["cursor", "--permission-mode", "bogus", "--detach"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    for accepted in ["auto-review", "force"] {
        assert!(err.contains(accepted), "missing '{accepted}': {err}");
    }
    let list = d.rpc("agent_list", json!({})).unwrap();
    assert_eq!(
        list["agents"].as_array().unwrap().len(),
        0,
        "rejected launch left an agent behind"
    );
}

/// `cadence join <pm> cursor` opens the worker on the pty endpoint
/// and its briefing names the forbidden input prefixes.
#[test]
fn cli_join_cursor_briefs_prefixes() {
    let d = TestDaemon::start();
    let mock = d.mock_cursor_tui();
    let pm_repo = d.dir.path().join("pmrepo");
    git_repo(&pm_repo);
    d.rpc(
        "agent_register",
        json!({"alias": "pm", "provider": "fake", "endpoint_kind": "fake",
               "cwd": pm_repo}),
    )
    .unwrap();
    d.wait_agent("pm", "idle", 10);
    let bin = env!("CARGO_BIN_EXE_cadence");
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["join", "pm", "cursor", "--alias", "wj", "--detach"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let agent = d.wait_agent("wj", "idle", 20);
    assert_eq!(agent["provider"], "cursor");
    assert_eq!(agent["endpoint_kind"], "pty");
    wait_probe_idle(&d, "wj", 15);
    let argv = std::fs::read_to_string(d.cursor_pane_file(&mock, "wj", "argv")).unwrap();
    assert!(argv.contains("--resume"), "{argv}");
    // The briefing tells the worker which leading chars never paste.
    let briefing = d.state.join("briefings/pm/BRIEFING-wj.md");
    let text = std::fs::read_to_string(&briefing).unwrap();
    for want in ["`/`", "`!`", "`@`", "refused"] {
        assert!(text.contains(want), "briefing missing {want}:\n{text}");
    }
    assert_eq!(agent["params"]["upstream"], "pm", "{agent}");
}

/// Audit N8: a launch of any kind leaves the agent's cwd repository
/// byte-identical — briefings live under the state dir, AGENTS.md and
/// .gitignore writes are opt-in only.
#[test]
fn launch_leaves_cwd_repo_untouched() {
    let d = TestDaemon::start();
    d.register("pm");
    d.wait_agent("pm", "idle", 10);
    let repo = d.dir.path().join("repo");
    git_repo(&repo);
    let bin = env!("CARGO_BIN_EXE_cadence");

    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "join", "pm", "fake", "--alias", "w-clean", "--detach", "--cwd",
        ])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_agent("w-clean", "idle", 15);

    // Nothing in the repo changed — no .cadence/, .gitignore or
    // AGENTS.md appeared.
    assert_eq!(git_porcelain(&repo), "", "launch touched the repo");
    assert!(!repo.join(".cadence").exists());
    assert!(!repo.join(".gitignore").exists());
    assert!(!repo.join("AGENTS.md").exists());

    // The briefing lives under the state dir; the bootstrap message
    // and `agent show` both name that absolute path.
    let file = d.state.join("briefings/pm/BRIEFING-w-clean.md");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("w-clean"), "{text}");
    let m = d.wait_message("w-clean", "bootstrap-w-clean", &["completed"], 15);
    let body = m["body"].as_str().unwrap();
    assert!(
        body.contains(file.to_str().unwrap()),
        "bootstrap body missing the briefing path: {body}"
    );
    let show = d.rpc("agent_show", json!({"alias": "w-clean"})).unwrap();
    assert_eq!(
        show["agent"]["briefing"].as_str().unwrap(),
        file.to_str().unwrap()
    );
}

/// A launch whose endpoint never opens writes nothing — the cwd repo
/// stays byte-identical and no briefing is created.
#[test]
fn failed_open_writes_nothing() {
    let d = TestDaemon::start();
    d.register("pm");
    d.wait_agent("pm", "idle", 10);
    let repo = d.dir.path().join("repo");
    git_repo(&repo);
    // The managed claude spawn fails outright — a bad command.
    test_env().set("CADENCE_CLAUDE_COMMAND", "/definitely-not-a-claude");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "join", "pm", "claude", "--alias", "w-bad", "--detach", "--cwd",
        ])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_agent("w-bad", "attention", 15);
    test_env().remove("CADENCE_CLAUDE_COMMAND");

    assert_eq!(git_porcelain(&repo), "", "failed open touched the repo");
    assert!(!repo.join(".cadence").exists());
    assert!(!d.state.join("briefings/pm/BRIEFING-w-bad.md").exists());
    // No bootstrap was enqueued for a pane that never opened.
    let show = d.rpc("agent_show", json!({"alias": "w-bad"})).unwrap();
    assert!(!show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["source"] == "bootstrap"));
}

/// `--agents-md` is the opt-in: the marker block lands in the worker's
/// repo AGENTS.md once and resume re-applies it without duplicating.
/// `--worktree` is the other opt-in: `.cadence/` is gitignored only
/// when the worktree is actually created, and only once.
#[test]
fn agents_md_opt_in_and_worktree_gitignore() {
    let d = TestDaemon::start();
    d.register("pm");
    d.wait_agent("pm", "idle", 10);
    let repo = d.dir.path().join("repo");
    git_repo(&repo);
    let bin = env!("CARGO_BIN_EXE_cadence");

    // Opted-in worker: AGENTS.md gains the marker block exactly once.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "join",
            "pm",
            "fake",
            "--alias",
            "w-am",
            "--detach",
            "--agents-md",
            "--cwd",
        ])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_agent("w-am", "idle", 15);
    let agents = std::fs::read_to_string(repo.join("AGENTS.md")).unwrap();
    assert_eq!(
        agents.matches("<!-- cadence:begin -->").count(),
        1,
        "{agents}"
    );
    // The opt-in persisted in params — resume replays it.
    let show = d.rpc("agent_show", json!({"alias": "w-am"})).unwrap();
    assert_eq!(show["agent"]["params"]["agents_md"], true);

    // Resume re-applies the block idempotently — still exactly one.
    d.rpc("agent_stop", json!({"alias": "w-am"})).unwrap();
    d.wait_agent("w-am", "stopped", 15);
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["agent", "resume", "w-am", "--detach"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_agent("w-am", "idle", 15);
    let agents = std::fs::read_to_string(repo.join("AGENTS.md")).unwrap();
    assert_eq!(
        agents.matches("<!-- cadence:begin -->").count(),
        1,
        "{agents}"
    );

    // --worktree creates .cadence/wt inside the repo — only then does
    // .gitignore gain the line, and only when missing.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "join",
            "pm",
            "fake",
            "--alias",
            "w-wt",
            "--detach",
            "--worktree",
            "wt1",
            "--cwd",
        ])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_agent("w-wt", "idle", 15);
    let gitignore = std::fs::read_to_string(repo.join(".gitignore")).unwrap();
    assert_eq!(
        gitignore
            .lines()
            .filter(|l| l.trim() == ".cadence/")
            .count(),
        1,
        "{gitignore}"
    );

    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "join",
            "pm",
            "fake",
            "--alias",
            "w-wt2",
            "--detach",
            "--worktree",
            "wt2",
            "--cwd",
        ])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_agent("w-wt2", "idle", 15);
    let gitignore = std::fs::read_to_string(repo.join(".gitignore")).unwrap();
    assert_eq!(
        gitignore
            .lines()
            .filter(|l| l.trim() == ".cadence/")
            .count(),
        1,
        "{gitignore}"
    );
}

// ---- message cancel (CAD-25) ----

#[test]
fn message_cancel_queued_lifecycle() {
    let d = TestDaemon::start();
    d.register("w1");
    d.register_inbox("pm");
    d.wait_agent("w1", "idle", 10);

    // A completed message refuses, naming its terminal state.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "done work", "message": "m-done"}),
    )
    .unwrap();
    d.wait_message("w1", "m-done", &["completed"], 15);
    let err = d
        .rpc("message_cancel", json!({"message": "m-done"}))
        .unwrap_err();
    assert!(err.to_string().contains("'completed'"), "{err}");

    // Stop the worker so the next send parks queued.
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "stopped", 10);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "queued work", "message": "m-q",
               "reply_to": "pm"}),
    )
    .unwrap();
    assert_eq!(d.message_state("w1", "m-q"), "queued");

    // Cancel: state, result payload, event, and exactly one notice on pm.
    let out = d
        .rpc(
            "message_cancel",
            json!({"message": "m-q", "by": "board-dev", "reason": "wrong spec"}),
        )
        .unwrap();
    assert_eq!(out["state"], "cancelled");
    assert_eq!(out["message"]["state"], "cancelled");
    assert_eq!(out["message"]["result"]["status"], "cancelled");
    assert_eq!(out["message"]["result"]["by"], "board-dev");
    assert_eq!(out["message"]["result"]["reason"], "wrong spec");
    let ev = d
        .events("w1")
        .into_iter()
        .find(|e| e["kind"] == "cancelled" && e["payload"]["message"] == "m-q")
        .expect("no cancelled event");
    assert_eq!(ev["payload"]["by"], "board-dev");
    assert_eq!(ev["payload"]["reason"], "wrong spec");
    let inbox = d.rpc("agent_inbox", json!({"alias": "pm"})).unwrap();
    let notices: Vec<&Value> = inbox["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["source"] == "worker_notice")
        .collect();
    assert_eq!(notices.len(), 1, "{inbox}");
    let body = notices[0]["body"].as_str().unwrap();
    assert!(body.contains("cancelled"), "{body}");
    assert!(body.contains("m-q"), "{body}");
    assert!(body.contains("wrong spec"), "{body}");

    // Cancelling again refuses, naming the state; an unknown id refuses.
    let err = d
        .rpc("message_cancel", json!({"message": "m-q"}))
        .unwrap_err();
    assert!(err.to_string().contains("'cancelled'"), "{err}");
    let err = d
        .rpc("message_cancel", json!({"message": "m-nope"}))
        .unwrap_err();
    assert!(err.to_string().contains("No such message"), "{err}");

    // Resume: the cancelled message never delivers; a fresh one does.
    d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "idle", 10);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "real work", "message": "m-new"}),
    )
    .unwrap();
    d.wait_message("w1", "m-new", &["completed"], 15);
    assert_eq!(d.message_state("w1", "m-q"), "cancelled");
}

#[test]
fn message_cancel_gate_pty_and_running_refusal() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);

    // Queued behind the ready gate: the message is durable but the pane
    // has not been claimed.
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "first", "message": "m1"}),
    )
    .unwrap();
    thread::sleep(Duration::from_secs(1));
    assert_eq!(d.message_state("dv1", "m1"), "queued");
    d.rpc("message_cancel", json!({"message": "m1", "reason": "typo"}))
        .unwrap();
    assert_eq!(d.message_state("dv1", "m1"), "cancelled");

    // A ready claim now must NOT paste m1 — the gate only releases a
    // queued message.
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    thread::sleep(Duration::from_secs(2));
    let input = std::fs::read_to_string(d.pane_file(&mock, "dv1", "input")).unwrap_or_default();
    assert!(!input.contains("first"), "{input}");

    // The claim stays outstanding, so the next queued message delivers
    // normally.
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "second", "message": "m2"}),
    )
    .unwrap();
    let token = pty_token(&d, "dv1", "m2");

    // A running turn refuses — interruption happens at the provider.
    let err = d
        .rpc("message_cancel", json!({"message": "m2"}))
        .unwrap_err();
    assert!(
        err.to_string().contains("'running'") || err.to_string().contains("'submitting'"),
        "{err}"
    );
    d.rpc(
        "message_report",
        json!({"message": "m2", "token": token, "kind": "result", "text": "done"}),
    )
    .unwrap();
    d.wait_message("dv1", "m2", &["completed"], 15);
}

#[test]
fn message_cancel_races_claim_atomically() {
    // Store-level: cancel vs take_queued — the state-guarded UPDATE and
    // the claim's own guard make exactly one winner per message; a
    // claimed message is never cancelled and a cancelled one is never
    // claimed.
    let seeded = TempDir::new().unwrap();
    let store = Store::open(&seeded.path().join("cadence.sqlite3")).unwrap();
    let cwd = seeded.path().to_str().unwrap().to_string();
    store
        .register_agent(&NewAgent {
            alias: "w",
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: &cwd,
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    for i in 0..40 {
        let id = format!("m{i}");
        store.enqueue("w", "work", None, &id, "user").unwrap();
        std::thread::scope(|s| {
            s.spawn(|| {
                let _ = store.take_queued("w");
            });
            s.spawn(|| {
                let _ = store.cancel(&id, "tester", None);
            });
        });
        let m = store.message(&id).unwrap().unwrap();
        match m.state.as_str() {
            // Claim won the race; cancel was refused with the state.
            "submitting" | "running" => {}
            // Cancel won; the claim found nothing queued.
            "cancelled" => {
                assert_eq!(m.result.as_ref().unwrap()["status"], "cancelled")
            }
            other => panic!("message {id} landed in '{other}'"),
        }
    }
}

#[test]
fn message_cancel_task_bound_refused() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register("w1");
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "task-bound work");
    d.job_new("pm", "j1", &spec, &sha);
    // --task binds the delivery to j1-t1; message cancel defers to the
    // task lifecycle.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "followup", "message": "m-t",
               "task": "j1-t1"}),
    )
    .unwrap();
    let err = d
        .rpc("message_cancel", json!({"message": "m-t"}))
        .unwrap_err();
    assert!(err.to_string().contains("task cancel j1-t1"), "{err}");
    // The message itself is untouched — still whatever the send did.
    let m = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let mt = m["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "m-t")
        .unwrap();
    assert_ne!(mt["state"], "cancelled");
}

// ---- CAD-23: result text must never truncate on the store/route ----

#[test]
fn long_result_text_survives_store_and_inbox_route() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude("ok", None);
    d.register_inbox("pm");
    d.register_claude("w1", json!({"upstream": "pm"}));
    d.wait_agent("w1", "idle", 15);
    // The claude result echoes the prompt ("MOCK_OK:<prompt>") — the
    // same single-line result text at each probed size.
    for size in [200usize, 2_000, 8_000, 39_000] {
        let id = format!("m{size}");
        d.rpc(
            "agent_send",
            json!({"alias": "w1", "text": "x".repeat(size), "message": id}),
        )
        .unwrap();
        let m = d.wait_message("w1", &id, &["completed"], 30);
        assert_eq!(
            m["result"]["text"].as_str().unwrap().len(),
            size + 8,
            "store truncated the {size}-char result"
        );
    }
    // An inbox PM receives the full text in every routed body — nothing
    // bounds a non-pty delivery.
    let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    let routed: Vec<&Value> = pm["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["source"] == "worker_result")
        .collect();
    assert_eq!(routed.len(), 4, "{pm}");
    for (m, size) in routed.iter().zip([200usize, 2_000, 8_000, 39_000]) {
        assert!(
            m["body"].as_str().unwrap().contains(&"x".repeat(size)),
            "routed body truncated at {size}: {}",
            m["body"].as_str().unwrap().len()
        );
    }
}

#[test]
fn long_result_bounded_only_for_pty_paste() {
    // The pty paste gate refuses bodies over its size bound — the routed
    // delivery would FAIL outright. The store keeps the record whole;
    // only the paste is bounded, with a pointer to the full record.
    let d = TestDaemon::start();
    let pm_mock = d.mock_devin();
    let _worker_mock = d.mock_claude("ok", None);
    d.register_devin("pm", None);
    d.wait_agent("pm", "idle", 20);
    d.register_claude("w1", json!({"upstream": "pm"}));
    d.wait_agent("w1", "idle", 15);
    d.rpc("agent_ready", json!({"alias": "pm"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "x".repeat(39_000), "message": "m1"}),
    )
    .unwrap();
    let m1 = d.wait_message("w1", "m1", &["completed"], 30);
    assert_eq!(m1["result"]["text"].as_str().unwrap().len(), 39_008);
    // The routed delivery pastes (completes) — before the bound it
    // failed at the pty pre-write gate.
    let routed = {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
            if let Some(m) = pm["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["source"].as_str() == Some("worker_result"))
            {
                break m.clone();
            }
            assert!(Instant::now() < deadline, "no routed result on pm");
            thread::sleep(Duration::from_millis(50));
        }
    };
    let body = routed["body"].as_str().unwrap();
    assert!(body.len() < 4000, "routed body not bounded: {}", body.len());
    assert!(body.contains("agent show w1"), "{body}");
    assert!(
        !body.contains(&"x".repeat(3900)),
        "routed body carried the full text"
    );
    d.wait_message("pm", routed["id"].as_str().unwrap(), &["completed"], 20);
    let screen = std::fs::read_to_string(d.pane_file(&pm_mock, "pm", "screen")).unwrap_or_default();
    assert!(screen.contains("agent show w1"), "{screen}");
}

#[test]
fn long_reconcile_note_survives_store_and_route() {
    let d = TestDaemon::start();
    d.register("w1");
    d.register_inbox("pm");
    d.wait_agent("w1", "idle", 10);
    // An unknown message with reply_to reconciled completed routes its
    // note — a 40k single-line note keeps whole in store and inbox.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "DISCONNECT", "message": "u1",
               "reply_to": "pm"}),
    )
    .unwrap();
    d.wait_message("w1", "u1", &["unknown"], 15);
    let note = "n".repeat(40_000);
    d.rpc(
        "message_reconcile",
        json!({"message": "u1", "status": "completed", "note": note}),
    )
    .unwrap();
    let m = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "u1")
        .unwrap()
        .clone();
    assert_eq!(m["result"]["note"].as_str().unwrap().len(), 40_000);
    let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    let routed = pm["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["source"] == "worker_result")
        .cloned()
        .expect("no routed result on pm");
    assert!(
        routed["body"].as_str().unwrap().contains(&note),
        "routed body truncated the note"
    );
}

#[test]
fn long_report_text_survives_pty_report_to_inbox() {
    // The pty report path: `message result --text` carries the full
    // single-line text into the store, and an inbox PM's routed
    // worker_result body carries it whole.
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("w1", None);
    d.register_inbox("pm");
    d.wait_agent("w1", "idle", 20);
    for size in [200usize, 2_000, 8_000, 40_000] {
        let id = format!("r{size}");
        d.rpc(
            "agent_send",
            json!({"alias": "w1", "text": "w", "message": id, "reply_to": "pm"}),
        )
        .unwrap();
        d.rpc("agent_ready", json!({"alias": "w1"})).unwrap();
        let token = pty_token(&d, "w1", &id);
        d.rpc(
            "message_report",
            json!({"message": id, "token": token, "kind": "result",
                   "text": "x".repeat(size)}),
        )
        .unwrap();
        let m = d.wait_message("w1", &id, &["completed"], 15);
        assert_eq!(
            m["result"]["text"].as_str().unwrap().len(),
            size,
            "store truncated the {size}-char report"
        );
    }
    let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    let routed: Vec<&Value> = pm["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["source"] == "worker_result")
        .collect();
    assert_eq!(routed.len(), 4, "{pm}");
    for (m, size) in routed.iter().zip([200usize, 2_000, 8_000, 40_000]) {
        assert!(
            m["body"].as_str().unwrap().contains(&"x".repeat(size)),
            "inbox routed body truncated at {size}"
        );
    }
}

/// CAD-43 `--job`: `issue start --job --pm --spec` opens an M3 job +
/// a worktree-scoped task through the same `job_new`/`task_new` RPCs —
/// and refuses before creating anything when the daemon can't answer.
#[test]
fn issue_start_job_opens_scoped_task() {
    let d = TestDaemon::start();
    let tmp = TempDir::new().unwrap();
    let (pm, repo, home) = (
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("home"),
    );
    for dir in [&pm, &repo, &home] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let git = |dir: &Path, args: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr)
        );
    };
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "x").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    // The tracker's pre-commit hook runs `cadence` from PATH — the
    // just-built binary must come first.
    let cli = |state: &Path, args: &[&str]| -> (bool, Value) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        let text = if out.stdout.is_empty() {
            String::from_utf8_lossy(&out.stderr).to_string()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        (
            out.status.success(),
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
        )
    };
    assert!(cli(&d.state, &["issue", "init"]).0);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(
        cli(
            &d.state,
            &["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s]
        )
        .0
    );
    assert!(
        cli(
            &d.state,
            &["issue", "new", "Job Start", "--project", "demo"]
        )
        .0
    );
    let (spec, _sha) = d.spec_file("spec.md", "do the seeded work");
    let tracker_commits = || {
        String::from_utf8_lossy(
            &std::process::Command::new("git")
                .arg("-C")
                .arg(&pm)
                .args(["rev-list", "--count", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .parse::<usize>()
        .unwrap()
    };
    let wt = repo.join(".cadence/wt/d-1-job-start");

    // Daemon down (state dir without a socket): refused, nothing created.
    let dead = TempDir::new().unwrap();
    let (ok, err) = cli(
        dead.path(),
        &[
            "issue", "start", "D-1", "--job", "--pm", "pm", "--spec", &spec,
        ],
    );
    assert!(
        !ok && err["error"].as_str().unwrap().contains("not reachable"),
        "{err}"
    );
    assert!(!wt.exists());
    let before = tracker_commits();

    // Daemon up but the pm alias unknown: also refused before creating.
    let (ok, _) = cli(
        &d.state,
        &[
            "issue", "start", "D-1", "--job", "--pm", "pm", "--spec", &spec,
        ],
    );
    assert!(!ok);
    assert!(!wt.exists());
    assert_eq!(tracker_commits(), before);

    // Daemon up, pm known, assignee unknown: refused before creating.
    d.register("pm");
    let (ok, err) = cli(
        &d.state,
        &[
            "issue",
            "start",
            "D-1",
            "--job",
            "--pm",
            "pm",
            "--spec",
            &spec,
            "--assignee",
            "ghost",
        ],
    );
    assert!(
        !ok && err["error"].as_str().unwrap().contains("ghost"),
        "{err}"
    );
    assert!(!wt.exists());
    assert_eq!(tracker_commits(), before);

    // Assignee exists but outside the pm's group: refused too.
    d.register("outsider");
    let (ok, err) = cli(
        &d.state,
        &[
            "issue",
            "start",
            "D-1",
            "--job",
            "--pm",
            "pm",
            "--spec",
            &spec,
            "--assignee",
            "outsider",
        ],
    );
    assert!(
        !ok && err["error"].as_str().unwrap().contains("group"),
        "{err}"
    );
    assert!(!wt.exists());
    assert_eq!(tracker_commits(), before);

    // Real path: one job, one task — <job>-t1 scoped to the worktree.
    d.register_member("w1", "pm");
    let (ok, out) = cli(
        &d.state,
        &[
            "issue",
            "start",
            "D-1",
            "--job",
            "--pm",
            "pm",
            "--spec",
            &spec,
            "--assignee",
            "w1",
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(out["created"], true);
    assert_eq!(tracker_commits(), before + 1);
    let (job_id, task_id) = (
        out["job"].as_str().unwrap().to_string(),
        out["task"].as_str().unwrap().to_string(),
    );
    assert_eq!(task_id, format!("{job_id}-t1"));
    let show = d.rpc("job_show", json!({"job": job_id})).unwrap();
    let job = &show["job"];
    assert_eq!(job["issue"], "D-1");
    assert_eq!(job["repo"].as_str().unwrap(), repo_s);
    assert_eq!(
        job["base_ref"].as_str().unwrap(),
        out["base"]["sha"].as_str().unwrap()
    );
    let tasks = job["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1, "{job}");
    let scoped = &tasks[0];
    assert_eq!(scoped["id"].as_str().unwrap(), task_id);
    assert_eq!(scoped["worktree"].as_str().unwrap(), "d-1-job-start");
    assert_eq!(scoped["branch"].as_str().unwrap(), "cadence/d-1-job-start");
    assert_eq!(
        scoped["base_sha"].as_str().unwrap(),
        out["base"]["sha"].as_str().unwrap()
    );
    assert_eq!(scoped["assignee"].as_str().unwrap(), "w1");
}

// ---- CAD-51: `job verdict` worktree verification + qa-verdict bridge ----

/// git in tests — panics on failure, returns trimmed stdout.
fn tgit(dir: &Path, args: &[&str]) -> String {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// Invoke the real `cadence` binary — the verify checks and the gh
/// bridge run client-side. `envs` overlays PATH/FAKE_GH_* after the
/// defaults; CADENCE_ALIAS is always removed (operator reviewer).
fn cadence_cli(state: &Path, args: &[&str], envs: &[(String, String)]) -> (bool, Value) {
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(state)
        .args(args)
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("CADENCE_ALIAS");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let text = if out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stderr).to_string()
    } else {
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    (
        out.status.success(),
        serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
    )
}

/// Repo fixture for verdict verification: `main` + branch `cadence/fix`
/// one commit ahead, checked out in `.cadence/wt/fix`. `origin`:
/// `None` = no remote; a bare-path string is pushed for real; a
/// GitHub URL is recorded as the remote and its tracking ref placed
/// by hand (never fetched).
struct VerifyRepo {
    _tmp: TempDir,
    repo: PathBuf,
    worktree: PathBuf,
    base: String,
    head: String,
}

fn verify_repo(origin: Option<&str>) -> VerifyRepo {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let wt = repo.join(".cadence/wt/fix");
    std::fs::create_dir_all(&repo).unwrap();
    tgit(&repo, &["init", "-b", "main"]);
    tgit(&repo, &["config", "user.email", "t@t"]);
    tgit(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "base\n").unwrap();
    tgit(&repo, &["add", "-A"]);
    tgit(&repo, &["commit", "-qm", "init"]);
    let base = tgit(&repo, &["rev-parse", "HEAD"]);
    tgit(
        &repo,
        &["worktree", "add", "-b", "cadence/fix", wt.to_str().unwrap()],
    );
    std::fs::write(wt.join("f"), "work\n").unwrap();
    tgit(&wt, &["commit", "-qam", "work"]);
    let head = tgit(&repo, &["rev-parse", "cadence/fix"]);
    if let Some(url) = origin {
        tgit(&repo, &["remote", "add", "origin", url]);
        if url.contains("github.com") {
            // A GitHub URL is never fetched in tests — place the
            // remote-tracking ref by hand instead.
            tgit(
                &repo,
                &["update-ref", "refs/remotes/origin/cadence/fix", &head],
            );
        } else {
            tgit(&repo, &["push", "-qu", "origin", "cadence/fix"]);
        }
    }
    VerifyRepo {
        _tmp: tmp,
        repo,
        worktree: wt,
        base,
        head,
    }
}

/// pm + worker + job `j1` bound to `repo`.
fn verdict_setup(d: &TestDaemon, repo: &Path) {
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "do the work");
    d.rpc(
        "job_new",
        json!({"pm": "pm", "job": "j1", "spec": spec, "spec_sha256": sha,
               "repo": repo.to_str().unwrap()}),
    )
    .unwrap();
}

/// Dispatch `task` to the fake worker; its REPORT_SHA trailer lands
/// `head` as the reported sha and the task reaches `review`.
fn task_to_review(d: &TestDaemon, task: &str, head: &str, scope: Value) {
    let mut new = json!({"job": "j1", "task": task, "assignee": "w1",
        "acceptance": format!("ok REPORT_SHA:{head}")});
    for (k, v) in scope.as_object().unwrap_or(&serde_json::Map::new()) {
        new[k] = v.clone();
    }
    d.rpc("task_new", new).unwrap();
    d.job_dispatch(task, json!({})).unwrap();
    d.wait_task(task, "review", 15);
}

fn verdict_args(task: &str, sha: &str, flag: &str) -> Vec<String> {
    vec![
        "job".to_string(),
        "verdict".to_string(),
        task.to_string(),
        "--sha".to_string(),
        sha.to_string(),
        flag.to_string(),
        "--reviewer".to_string(),
        "operator".to_string(),
    ]
}

fn cli_verdict(
    d: &TestDaemon,
    task: &str,
    sha: &str,
    flag: &str,
    extra: &[&str],
    envs: &[(String, String)],
) -> (bool, Value) {
    let mut args: Vec<String> = verdict_args(task, sha, flag);
    args.extend(extra.iter().map(|s| s.to_string()));
    cadence_cli(
        &d.state,
        &args.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        envs,
    )
}

/// The task's recorded verdicts.
fn task_verdicts(d: &TestDaemon, task: &str) -> Value {
    d.rpc("task_show", json!({"task": task})).unwrap()["task"]["verdicts"].clone()
}

#[test]
fn job_verdict_worktree_verify_binds_and_records() {
    let d = TestDaemon::start();
    // A real bare repo gets a real `origin/<branch>` ref via push.
    let bare = TempDir::new().unwrap();
    tgit(bare.path(), &["init", "--bare"]);
    let r = verify_repo(Some(bare.path().to_str().unwrap()));
    verdict_setup(&d, &r.repo);
    task_to_review(
        &d,
        "j1-t2",
        &r.head,
        json!({"worktree": "fix", "branch": "cadence/fix", "base_sha": r.base}),
    );

    let (ok, out) = cli_verdict(&d, "j1-t2", &r.head, "--pass", &[], &[]);
    assert!(ok, "{out}");
    let verify = out["verdict"]["verify"].clone();
    assert_eq!(
        verify["checked"].as_array().unwrap(),
        &json!([
            "commit",
            "branch tip",
            "base ancestor",
            "worktree clean",
            "pushed"
        ])
        .as_array()
        .unwrap()
        .clone(),
        "{verify}"
    );
    assert_eq!(verify["skipped"], json!([]), "{verify}");
    // The origin is a local path — not a GitHub remote, so the bridge
    // reports instead of posting; the verdict still committed.
    assert_eq!(out["status"]["posted"], false, "{out}");
    assert!(
        out["status"]["reason"]
            .as_str()
            .unwrap()
            .contains("not a GitHub remote"),
        "{out}"
    );

    // The verify result is stored on the row and echoed on the event.
    let vs = task_verdicts(&d, "j1-t2");
    assert_eq!(vs.as_array().unwrap().len(), 1, "{vs}");
    assert_eq!(vs[0]["verify"]["checked"], verify["checked"], "{vs}");
    let events = d.rpc("job_events", json!({"job": "j1"})).unwrap()["events"].clone();
    let recorded = events
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "verdict_recorded")
        .expect("verdict_recorded event");
    assert_eq!(
        recorded["payload"]["verify"]["checked"], verify["checked"],
        "{recorded}"
    );
}

#[test]
fn job_verdict_worktree_verify_rejects_each_check() {
    let d = TestDaemon::start();
    let bare = TempDir::new().unwrap();
    tgit(bare.path(), &["init", "--bare"]);
    let r = verify_repo(Some(bare.path().to_str().unwrap()));
    verdict_setup(&d, &r.repo);
    let scope = || json!({"worktree": "fix", "branch": "cadence/fix", "base_sha": r.base});
    let still_review = |task: &str| {
        assert_eq!(
            task_verdicts(&d, task),
            json!([]),
            "verdict row written for {task}"
        );
        assert_eq!(d.task_state(task), "review");
    };

    // Dirty worktree — the uncommitted file names the clean check.
    task_to_review(&d, "j1-t2", &r.head, scope());
    std::fs::write(r.worktree.join("dirty.txt"), "x").unwrap();
    let (ok, out) = cli_verdict(&d, "j1-t2", &r.head, "--pass", &[], &[]);
    assert!(!ok, "{out}");
    let err = out["error"].as_str().unwrap();
    assert!(
        err.contains("worktree clean") && err.contains("uncommitted") && err.contains("dirty.txt"),
        "{err}"
    );
    std::fs::remove_file(r.worktree.join("dirty.txt")).unwrap();
    still_review("j1-t2");

    // Wrong tip — head_sha is the real base commit, not the branch tip.
    task_to_review(&d, "j1-t3", &r.base, scope());
    let (ok, out) = cli_verdict(&d, "j1-t3", &r.base, "--pass", &[], &[]);
    assert!(!ok, "{out}");
    let err = out["error"].as_str().unwrap();
    assert!(
        err.contains("branch tip") && err.contains(&r.head) && err.contains(&r.base),
        "{err}"
    );
    still_review("j1-t3");

    // Base not an ancestor — a newer main commit never joined the branch.
    std::fs::write(r.repo.join("m"), "main\n").unwrap();
    tgit(&r.repo, &["add", "-A"]);
    tgit(&r.repo, &["commit", "-qm", "main work"]);
    let main_tip = tgit(&r.repo, &["rev-parse", "main"]);
    task_to_review(
        &d,
        "j1-t4",
        &r.head,
        json!({"worktree": "fix", "branch": "cadence/fix", "base_sha": main_tip}),
    );
    let (ok, out) = cli_verdict(&d, "j1-t4", &r.head, "--pass", &[], &[]);
    assert!(!ok, "{out}");
    let err = out["error"].as_str().unwrap();
    assert!(
        err.contains("base ancestor") && err.contains(&main_tip) && err.contains(&r.head),
        "{err}"
    );
    still_review("j1-t4");

    // Unpushed — the branch advanced locally, origin stayed behind.
    std::fs::write(r.worktree.join("f"), "more\n").unwrap();
    tgit(&r.worktree, &["commit", "-qam", "more"]);
    let head2 = tgit(&r.repo, &["rev-parse", "cadence/fix"]);
    assert_ne!(head2, r.head);
    task_to_review(&d, "j1-t5", &head2, scope());
    let (ok, out) = cli_verdict(&d, "j1-t5", &head2, "--pass", &[], &[]);
    assert!(!ok, "{out}");
    let err = out["error"].as_str().unwrap();
    assert!(
        err.contains("pushed") && err.contains(&r.head) && err.contains(&head2),
        "{err}"
    );
    still_review("j1-t5");

    // A bogus reported sha never resolves to a commit at all — the
    // worker can report any 40-hex; the check is what binds it.
    let bogus = "1".repeat(40);
    task_to_review(&d, "j1-t6", &bogus, scope());
    let (ok, out) = cli_verdict(&d, "j1-t6", &bogus, "--pass", &[], &[]);
    assert!(!ok, "{out}");
    let err = out["error"].as_str().unwrap();
    assert!(
        err.contains("commit") && err.contains(&bogus) && err.contains("does not resolve"),
        "{err}"
    );
    still_review("j1-t6");
}

#[test]
fn job_verdict_worktree_verify_skips_and_opt_out() {
    let d = TestDaemon::start();
    let r = verify_repo(None); // no origin at all
    verdict_setup(&d, &r.repo);
    task_to_review(
        &d,
        "j1-t2",
        &r.head,
        json!({"worktree": "fix", "branch": "cadence/fix", "base_sha": r.base}),
    );
    // The worktree dir is gone — both the clean check and the pushed
    // check cannot apply.
    std::fs::remove_dir_all(&r.worktree).unwrap();
    let (ok, out) = cli_verdict(&d, "j1-t2", &r.head, "--pass", &["--no-status"], &[]);
    assert!(ok, "{out}");
    let verify = out["verdict"]["verify"].clone();
    let skipped: Vec<&str> = verify["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["check"].as_str().unwrap())
        .collect();
    assert_eq!(
        verify["checked"].as_array().unwrap(),
        &json!(["commit", "branch tip", "base ancestor"])
            .as_array()
            .unwrap()
            .clone(),
        "{verify}"
    );
    assert!(skipped.contains(&"worktree clean"), "{verify}");
    assert!(skipped.contains(&"pushed"), "{verify}");

    // No base_sha → the ancestor check skips too.
    task_to_review(
        &d,
        "j1-t3",
        &r.head,
        json!({"worktree": "fix", "branch": "cadence/fix"}),
    );
    let (ok, out) = cli_verdict(&d, "j1-t3", &r.head, "--pass", &["--no-status"], &[]);
    assert!(ok, "{out}");
    let skipped: Vec<&str> = out["verdict"]["verify"]["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["check"].as_str().unwrap())
        .collect();
    assert!(skipped.contains(&"base ancestor"), "{out}");

    // --no-verify-worktree records the opt-out on the verdict.
    task_to_review(
        &d,
        "j1-t4",
        &r.head,
        json!({"worktree": "fix", "branch": "cadence/fix", "base_sha": r.base}),
    );
    let (ok, out) = cli_verdict(
        &d,
        "j1-t4",
        &r.head,
        "--pass",
        &["--no-verify-worktree", "--no-status"],
        &[],
    );
    assert!(ok, "{out}");
    let verify = &out["verdict"]["verify"];
    assert_eq!(verify["checked"], json!([]), "{verify}");
    assert!(
        verify["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["reason"]
                .as_str()
                .unwrap()
                .contains("--no-verify-worktree")),
        "{verify}"
    );
    let vs = task_verdicts(&d, "j1-t4");
    assert!(
        vs[0]["verify"]["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["reason"]
                .as_str()
                .unwrap()
                .contains("--no-verify-worktree")),
        "{vs}"
    );
}

/// A fake `gh` that logs each invocation to $FAKE_GH_LOG (one
/// tab-joined line) and answers from the environment — same pattern
/// as tests/scripts/test_qa_verdict.py. FAKE_GH_FAIL forces exit N.
const FAKE_GH: &str = r#"#!/usr/bin/env bash
(IFS=$'\t'; printf '%s\n' "$*") >> "$FAKE_GH_LOG"
if [ -n "${FAKE_GH_FAIL:-}" ]; then echo "fake gh: forced failure" >&2; exit "$FAKE_GH_FAIL"; fi
case "$1 ${2:-}" in
  "pr list") printf '%s\n' "${FAKE_GH_PRS:-[]}" ;;
  "pr view") printf '{"number": %s, "headRefOid": "%s"}\n' "${FAKE_GH_PR_NUM:-9}" "${FAKE_GH_HEAD:-}" ;;
  "api --method") echo '{}' ;;
  *) echo "fake gh: unexpected call: $*" >&2; exit 64 ;;
esac
"#;

/// Fake-gh dir + call log; env vars for `cadence_cli`.
struct FakeGh {
    _tmp: TempDir,
    bin: PathBuf,
    log: PathBuf,
}

fn fake_gh() -> FakeGh {
    let tmp = TempDir::new().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    std::fs::write(&gh, FAKE_GH).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let log = tmp.path().join("gh.log");
    std::fs::write(&log, "").unwrap();
    FakeGh {
        _tmp: tmp,
        bin,
        log,
    }
}

impl FakeGh {
    /// Env overlay for cadence_cli: this gh first on PATH + the log +
    /// any FAKE_GH_* knobs.
    fn envs(&self, extra: &[(String, String)]) -> Vec<(String, String)> {
        let mut v = vec![
            (
                "PATH".to_string(),
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            ),
            (
                "FAKE_GH_LOG".to_string(),
                self.log.to_str().unwrap().to_string(),
            ),
        ];
        for (k, val) in extra {
            v.push((k.clone(), val.clone()));
        }
        v
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

#[test]
fn job_verdict_status_bridge_posts_qa_verdict() {
    let d = TestDaemon::start();
    let r = verify_repo(Some("https://github.com/acme/widgets.git"));
    let gh = fake_gh();
    verdict_setup(&d, &r.repo);
    let scope = || json!({"worktree": "fix", "branch": "cadence/fix", "base_sha": r.base});
    let open_pr = |head: &str| {
        (
            "FAKE_GH_PRS".to_string(),
            format!("[{{\"number\": 7, \"headRefOid\": \"{head}\"}}]"),
        )
    };

    // pass → success on the head sha, task + revision in the description.
    task_to_review(&d, "j1-t2", &r.head, scope());
    let envs = gh.envs(&[open_pr(&r.head)]);
    let (ok, out) = cli_verdict(&d, "j1-t2", &r.head, "--pass", &[], &envs);
    assert!(ok, "{out}");
    assert_eq!(
        out["status"],
        json!({"posted": true, "pr": 7, "sha": r.head}),
        "{out}"
    );
    let calls = gh.calls();
    assert!(
        calls.iter().any(|c| c.contains(&format!(
            "api\t--method\tPOST\trepos/acme/widgets/statuses/{}\t-f\tcontext=qa-verdict\t-f\tstate=success\t-f\tdescription=pass — j1-t2 r1",
            r.head
        ))),
        "{calls:?}"
    );

    // revise → failure; blocked → failure. Same head sha throughout.
    task_to_review(&d, "j1-t3", &r.head, scope());
    let (ok, out) = cli_verdict(&d, "j1-t3", &r.head, "--revise", &[], &envs);
    assert!(ok, "{out}");
    assert_eq!(out["status"]["posted"], true, "{out}");
    d.job_dispatch("j1-t3", json!({})).unwrap();
    d.wait_task("j1-t3", "review", 15);
    let (ok, out) = cli_verdict(&d, "j1-t3", &r.head, "--blocked", &[], &envs);
    assert!(ok, "{out}");
    assert_eq!(out["status"]["posted"], true, "{out}");
    let calls = gh.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.contains("state=failure") && c.contains("description=revise — j1-t3 r1")),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c.contains("state=failure") && c.contains("description=blocked — j1-t3 r2")),
        "{calls:?}"
    );

    // --pr names the PR instead of branch discovery.
    task_to_review(&d, "j1-t4", &r.head, scope());
    let envs = gh.envs(&[
        ("FAKE_GH_PR_NUM".to_string(), "9".to_string()),
        ("FAKE_GH_HEAD".to_string(), r.head.clone()),
    ]);
    let (ok, out) = cli_verdict(&d, "j1-t4", &r.head, "--pass", &["--pr", "9"], &envs);
    assert!(ok, "{out}");
    assert_eq!(out["status"]["pr"], 9, "{out}");
    assert!(
        gh.calls().iter().any(|c| c.contains("pr\tview\t9")),
        "{:?}",
        gh.calls()
    );
}

#[test]
fn job_verdict_status_bridge_failures_keep_the_verdict() {
    let d = TestDaemon::start();
    let r = verify_repo(Some("https://github.com/acme/widgets.git"));
    let gh = fake_gh();
    verdict_setup(&d, &r.repo);
    let scope = || json!({"worktree": "fix", "branch": "cadence/fix", "base_sha": r.base});
    let envs_of = |extra: &[(String, String)]| -> Vec<(String, String)> { gh.envs(extra) };

    // No open PR — the verdict commits, the status reports why.
    task_to_review(&d, "j1-t2", &r.head, scope());
    let envs = envs_of(&[("FAKE_GH_PRS".to_string(), "[]".to_string())]);
    let (ok, out) = cli_verdict(&d, "j1-t2", &r.head, "--pass", &[], &envs);
    assert!(ok, "{out}");
    assert_eq!(out["status"]["posted"], false, "{out}");
    assert!(
        out["status"]["reason"]
            .as_str()
            .unwrap()
            .contains("no open PR for branch cadence/fix"),
        "{out}"
    );
    assert_eq!(d.task_state("j1-t2"), "verified");

    // A moved head is reported — never posted to a sha the reviewer
    // did not name.
    task_to_review(&d, "j1-t3", &r.head, scope());
    let other = "1".repeat(40);
    let envs = envs_of(&[(
        "FAKE_GH_PRS".to_string(),
        format!("[{{\"number\": 7, \"headRefOid\": \"{other}\"}}]"),
    )]);
    let (ok, out) = cli_verdict(&d, "j1-t3", &r.head, "--pass", &[], &envs);
    assert!(ok, "{out}");
    assert_eq!(
        out["status"]["reason"].as_str().unwrap(),
        format!("pr head {other} is not the judged sha {}", r.head),
        "{out}"
    );
    assert!(
        !gh.calls().iter().any(|c| c.contains("statuses/")),
        "{:?}",
        gh.calls()
    );
    assert_eq!(d.task_state("j1-t3"), "verified");

    // A failing gh leaves the verdict in place with the reason.
    task_to_review(&d, "j1-t4", &r.head, scope());
    let envs = envs_of(&[("FAKE_GH_FAIL".to_string(), "1".to_string())]);
    let (ok, out) = cli_verdict(&d, "j1-t4", &r.head, "--pass", &[], &envs);
    assert!(ok, "{out}");
    assert_eq!(out["status"]["posted"], false, "{out}");
    assert!(
        out["status"]["reason"]
            .as_str()
            .unwrap()
            .contains("forced failure"),
        "{out}"
    );
    assert_eq!(d.task_state("j1-t4"), "verified");

    // --no-status makes no gh call at all.
    task_to_review(&d, "j1-t5", &r.head, scope());
    let before = gh.calls().len();
    let envs = envs_of(&[]);
    let (ok, out) = cli_verdict(&d, "j1-t5", &r.head, "--pass", &["--no-status"], &envs);
    assert!(ok, "{out}");
    assert_eq!(out["status"]["posted"], false, "{out}");
    assert_eq!(gh.calls().len(), before, "{:?}", gh.calls());
    assert_eq!(d.task_state("j1-t5"), "verified");
}

#[test]
fn job_verdict_unscoped_task_is_unchanged() {
    let d = TestDaemon::start();
    verdict_setup(&d, Path::new("/tmp"));
    // No worktree/branch scope — the old path, no verify, no bridge.
    task_to_review(&d, "j1-t2", SHA_A, json!({}));
    let (ok, out) = cli_verdict(&d, "j1-t2", SHA_A, "--pass", &[], &[]);
    assert!(ok, "{out}");
    assert!(out["verdict"]["verify"].is_null(), "{out}");
    assert_eq!(out["status"]["posted"], false, "{out}");
    assert!(
        out["status"]["reason"]
            .as_str()
            .unwrap()
            .contains("no branch"),
        "{out}"
    );
    assert_eq!(d.task_state("j1-t2"), "verified");
}

// ---------- CAD-52: stall detection ----------

/// Poll until `alias` has at least `want` events of `kind`.
fn wait_event_count(d: &TestDaemon, alias: &str, kind: &str, want: usize, secs: u64) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let found: Vec<Value> = d
            .events(alias)
            .into_iter()
            .filter(|e| e["kind"].as_str() == Some(kind))
            .collect();
        if found.len() >= want {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "{alias}: wanted {want} {kind} events, got {}: {:?}",
            found.len(),
            d.events(alias)
                .iter()
                .map(|e| e["kind"].as_str().unwrap_or("?").to_string())
                .collect::<Vec<_>>()
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// The stored message list for `alias` — notices land here for both
/// actor and inbox recipients.
fn messages_for(d: &TestDaemon, alias: &str) -> Vec<Value> {
    d.rpc("agent_show", json!({"alias": alias})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .clone()
}

/// Poll until `alias` stores a message whose `source` matches.
fn wait_source(d: &TestDaemon, alias: &str, source: &str, want: usize, secs: u64) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let found: Vec<Value> = messages_for(d, alias)
            .into_iter()
            .filter(|m| m["source"].as_str() == Some(source))
            .collect();
        if found.len() >= want {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "{alias}: wanted {want} '{source}' messages, got {:?}",
            messages_for(d, alias)
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// A fake actor with explicit launch params — `stall_secs` included.
fn register_fake_opts(d: &TestDaemon, alias: &str, params: Value) {
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "agent_register",
        json!({"alias": alias, "provider": "fake",
               "endpoint_kind": "fake", "cwd": cwd,
               "params": params.to_string()}),
    )
    .unwrap();
}

/// A running turn on a static pty screen raises `turn_stalled` exactly
/// once, flags the agent views, and sends one `worker_notice` to the
/// message's `reply_to` — the turn itself is never touched.
#[test]
fn pty_stall_static_screen_fires_once_and_notices() {
    let d = TestDaemon::start();
    let _mock = d.mock_stub();
    stall_sample(2);
    d.register_inbox("pm");
    d.register_stub("w1", json!({"auto_ready": "verified", "stall_secs": 2}));
    d.wait_agent("w1", "idle", 20);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "do work", "reply_to": "pm",
               "message": "ms1"}),
    )
    .unwrap();
    d.wait_message("w1", "ms1", &["running"], 15);

    let e = d.wait_event("w1", "turn_stalled", 30);
    assert_eq!(e["payload"]["message"], "ms1", "{e}");
    assert!(
        e["payload"]["silent_secs"].as_u64().unwrap_or(0) >= 2,
        "{e}"
    );
    assert!(
        e["payload"]["last_activity"].as_f64().unwrap_or(0.0) > 0.0,
        "{e}"
    );

    // Once per episode: the silence continues but no second event fires.
    thread::sleep(Duration::from_secs(7));
    assert_eq!(wait_event_count(&d, "w1", "turn_stalled", 1, 2).len(), 1);

    // One `worker_notice` went to reply_to — fire-and-forget, so the
    // mailbox holds it with no reply path of its own.
    let notices = wait_source(&d, "pm", "worker_notice", 1, 10);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0]["reply_to"].is_null(), "{notices:?}");
    let body = notices[0]["body"].as_str().unwrap_or("");
    assert!(
        body.contains("w1") && body.contains("no activity"),
        "{body}"
    );

    // The views carry the flag — and the turn is still running:
    // stalling is informational, never an interruption.
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(agent["stalled"], true, "{agent}");
    assert!(agent["silent_secs"].as_u64().unwrap_or(0) >= 2, "{agent}");
    assert_eq!(d.message_state("w1", "ms1"), "running");
    let list = d.rpc("agent_list", json!({})).unwrap()["agents"].clone();
    let row = list
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"].as_str() == Some("w1"))
        .cloned()
        .unwrap();
    assert_eq!(row["stalled"], true, "{row}");

    // A real result still lands normally after the notice.
    let token = pty_token(&d, "w1", "ms1");
    d.rpc(
        "message_report",
        json!({"message": "ms1", "token": token, "kind": "result",
               "text": "done"}),
    )
    .unwrap();
    d.wait_message("w1", "ms1", &["completed"], 10);
    stall_sample(0);
}

/// A stalled turn resumes on real screen motion and re-arms: a second
/// silence raises a second `turn_stalled`. A ticking status counter
/// (`· Ns`) alone is not activity — it must never resume.
#[test]
fn pty_stall_resume_rearms_and_spinner_is_not_activity() {
    let d = TestDaemon::start();
    let mock = d.mock_stub();
    stall_sample(2);
    d.register_inbox("pm");
    d.register_stub("w1", json!({"auto_ready": "verified", "stall_secs": 2}));
    d.wait_agent("w1", "idle", 20);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "do work", "reply_to": "pm",
               "message": "ms2"}),
    )
    .unwrap();
    d.wait_message("w1", "ms2", &["running"], 15);
    // The status line exists from the baseline sample on — only its
    // counter will move.
    atomic_write(
        d.stub_pane_file(&mock, "w1", "tui-state"),
        "⠋ Working · 1s\n",
    );
    d.wait_event("w1", "turn_stalled", 30);

    // The counter ticks past several samples — normalized identically,
    // so nothing resumes.
    for i in 2..=4u64 {
        atomic_write(
            d.stub_pane_file(&mock, "w1", "tui-state"),
            format!("⠋ Working · {i}s\n"),
        );
        thread::sleep(Duration::from_secs(2));
    }
    assert!(
        d.events("w1")
            .iter()
            .all(|e| e["kind"].as_str() != Some("turn_resumed")),
        "spinner ticking resumed the turn: {:?}",
        d.events("w1")
    );

    // Real transcript motion resumes — same recipient gets the resolved
    // notice — and detection re-arms for the next silence.
    atomic_write(
        d.stub_pane_file(&mock, "w1", "tui-state"),
        "⠋ Working · 5s\nBUILD OK\n",
    );
    let e = d.wait_event("w1", "turn_resumed", 15);
    assert_eq!(e["payload"]["message"], "ms2", "{e}");
    wait_event_count(&d, "w1", "turn_stalled", 2, 20);
    assert_eq!(d.message_state("w1", "ms2"), "running");

    let notices = wait_source(&d, "pm", "worker_notice", 3, 10);
    let stalls = notices
        .iter()
        .filter(|m| m["body"].as_str().unwrap_or("").contains("no activity"))
        .count();
    let resumes = notices
        .iter()
        .filter(|m| m["body"].as_str().unwrap_or("").contains("active again"))
        .count();
    assert_eq!((stalls, resumes), (2, 1), "{notices:?}");
    stall_sample(0);
}

/// Screen activity is debounced: a hash seen for exactly one sample —
/// a capture taken mid-repaint — can neither reset the silence clock
/// nor resume a stalled turn. The mock `captures` counter pins the
/// empty tail to exactly one sighting: it is written after one
/// capture's read and reverted before the next-but-one. Real
/// persistent motion still resumes, one interval later.
#[test]
fn pty_stall_transient_sample_neither_resumes_nor_resets() {
    let d = TestDaemon::start();
    let mock = d.mock_stub();
    stall_sample(1);
    d.register_inbox("pm");
    d.register_stub("w1", json!({"auto_ready": "verified", "stall_secs": 8}));
    d.wait_agent("w1", "idle", 20);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "do work", "reply_to": "pm",
               "message": "mtr"}),
    )
    .unwrap();
    d.wait_message("w1", "mtr", &["running"], 15);
    atomic_write(d.stub_pane_file(&mock, "w1", "tui-state"), "⠋ Working\n");

    let captures = || {
        std::fs::read_to_string(d.stub_pane_file(&mock, "w1", "captures"))
            .unwrap_or_default()
            .len()
    };
    let wait_capture = |from: usize| {
        let deadline = Instant::now() + Duration::from_secs(15);
        while captures() <= from {
            assert!(Instant::now() < deadline, "no capture landed");
            thread::sleep(Duration::from_millis(30));
        }
    };
    let silent = || {
        d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"]["silent_secs"]
            .as_u64()
            .unwrap_or(0)
    };
    // `contents` is visible to exactly one sample: the counter ticks
    // before the capture reads the screen, so once a capture has
    // started its read is done — write now, and the NEXT capture is
    // the only one that can see it. Revert before the one after that.
    let transient = |contents: &str| {
        let n = captures();
        wait_capture(n);
        atomic_write(d.stub_pane_file(&mock, "w1", "tui-state"), contents);
        let n = captures();
        wait_capture(n);
        atomic_write(d.stub_pane_file(&mock, "w1", "tui-state"), "⠋ Working\n");
    };

    // Let the baseline settle — two consecutive identical samples.
    let n0 = captures();
    wait_capture(n0 + 1);
    // An empty tail for one sample must not reset the silence clock.
    let before = silent();
    transient("");
    let after = silent();
    assert!(
        after > before,
        "one-sample transient reset the silence clock: {before} -> {after}"
    );

    // The stall fires on budget — the clock kept accruing.
    d.wait_event("w1", "turn_stalled", 15);

    // Mid-stall, the same one-sample transient cannot resume.
    transient("");
    let n = captures();
    wait_capture(n); // one more sample on the restored screen
    assert!(
        d.events("w1")
            .iter()
            .all(|e| e["kind"].as_str() != Some("turn_resumed")),
        "one-sample transient resumed the turn: {:?}",
        d.events("w1")
    );
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(agent["stalled"], true, "{agent}");
    assert_eq!(
        messages_for(&d, "pm")
            .iter()
            .filter(|m| m["source"].as_str() == Some("worker_notice"))
            .count(),
        1,
        "one-sample transient sent a notice"
    );

    // Real motion still resumes — two DIFFERENT consecutive samples
    // confirm too (the scrolling-pane clause: a busy pane is never
    // starved into a false stall) — and the same recipient hears it.
    atomic_write(d.stub_pane_file(&mock, "w1", "tui-state"), "BUILD 1\n");
    let n = captures();
    wait_capture(n); // first differing sample — held as a candidate
    atomic_write(d.stub_pane_file(&mock, "w1", "tui-state"), "BUILD 2\n");
    let e = d.wait_event("w1", "turn_resumed", 20);
    assert_eq!(e["payload"]["message"], "mtr", "{e}");
    wait_source(&d, "pm", "worker_notice", 2, 10);
    stall_sample(0);
}

/// A job kickoff's stall goes to the PM as a `job_event`, the event is
/// job/task-scoped, and the task row carries the flag while it lasts.
#[test]
fn job_kickoff_stall_flags_task_and_notifies_pm() {
    let d = TestDaemon::start();
    let _mock = d.mock_stub();
    stall_sample(2);
    d.register_inbox("pm");
    d.register_stub("w1", json!({"upstream": "pm", "auto_ready": "verified"}));
    d.wait_agent("w1", "idle", 20);
    let (spec, sha) = d.spec_file("spec.md", "stall me");
    d.rpc(
        "job_new",
        json!({"pm": "pm", "job": "j1", "spec": spec, "spec_sha256": sha,
               "stall_secs": 3, "task_assignee": "w1"}),
    )
    .unwrap();
    d.job_dispatch("j1-t1", json!({})).unwrap();
    d.wait_task("j1-t1", "running", 15);

    let e = d.wait_event("w1", "turn_stalled", 30);
    assert_eq!(e["payload"]["task"], "j1-t1", "{e}");
    // Job-scoped: the job event stream sees it too.
    let ev = d.rpc("job_events", json!({"job": "j1"})).unwrap();
    assert!(
        ev["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"].as_str() == Some("turn_stalled")),
        "{ev}"
    );
    // The PM notice is a `job_event` — one per episode.
    let notices = wait_source(&d, "pm", "job_event", 1, 10);
    let stalled = notices
        .iter()
        .filter(|m| m["body"].as_str().unwrap_or("").contains("no activity"))
        .count();
    assert_eq!(stalled, 1, "{notices:?}");
    // The task row is flagged while the kickoff is stalled.
    let show = d.rpc("job_show", json!({"job": "j1"})).unwrap();
    let task = &show["job"]["tasks"][0];
    assert_eq!(task["stalled"], true, "{task}");
    assert!(task["silent_secs"].as_u64().unwrap_or(0) >= 3, "{task}");
    assert_eq!(d.task_state("j1-t1"), "running");
    stall_sample(0);
}

/// The managed path (the fake's `SLEEP` is the managed mock) stalls on
/// silence and just ends if the turn finishes while stalled. An open
/// brokered approval wait is activity — it can never stall.
#[test]
fn fake_silent_turn_stalls_and_brokered_wait_does_not() {
    let d = TestDaemon::start();
    d.register_inbox("pm");
    register_fake_opts(&d, "w1", json!({"stall_secs": 2}));
    d.wait_agent("w1", "idle", 10);

    // A brokered wait outlasts the budget without stalling.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "NEED_INPUT:hold",
               "reply_to": "pm", "message": "m-need"}),
    )
    .unwrap();
    d.wait_agent("w1", "waiting_input", 10);
    // Positive window-opened proof before any absence assertion
    // (CAD-221, the canary pattern from #70's slot-plant test): a
    // pending brokered request refreshes the turn's activity on every
    // stall tick, so a wait older than the budget still reporting
    // silence *under* the budget can only happen while the refresh
    // path runs. A dead or skipping ticker reports wall-clock age
    // instead and this loop fails loudly — absence is never asserted
    // inside a window that may not have opened.
    let wait_started = Instant::now();
    let canary_deadline = wait_started + Duration::from_secs(15);
    loop {
        let a = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
        let silent = a["silent_secs"].as_u64().unwrap_or(u64::MAX);
        if wait_started.elapsed() > Duration::from_secs(4) && silent < 2 {
            break;
        }
        assert!(
            Instant::now() < canary_deadline,
            "stall ticker never refreshed the brokered wait — the \
             absence window never provably opened: {a}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        d.events("w1")
            .iter()
            .all(|e| e["kind"].as_str() != Some("turn_stalled")),
        "brokered wait stalled: {:?}",
        d.events("w1")
    );
    let requests = d.rpc("agent_requests", json!({"alias": "w1"})).unwrap();
    let handle = requests["requests"][0]["request"].as_str().unwrap();
    d.rpc(
        "agent_respond",
        json!({"alias": "w1", "request": handle, "decision": "accept"}),
    )
    .unwrap();
    d.wait_message("w1", "m-need", &["completed"], 15);

    // A genuinely silent turn stalls once — then simply ends; no
    // recovery event is owed for a finished message. The wait selects
    // by payload: a `turn_stalled` for m-need landing late (between
    // respond and completion on a slow host) must not be returned
    // here (CAD-222).
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "SLEEP:12", "reply_to": "pm",
               "message": "m-sleep"}),
    )
    .unwrap();
    let e = d.wait_event_where(
        "w1",
        "turn_stalled",
        |e| e["payload"]["message"].as_str() == Some("m-sleep"),
        20,
    );
    assert_eq!(e["payload"]["message"], "m-sleep", "{e}");
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(agent["stalled"], true, "{agent}");
    d.wait_message("w1", "m-sleep", &["completed"], 20);
    assert!(
        d.events("w1")
            .iter()
            .all(|e| e["kind"].as_str() != Some("turn_resumed")),
        "finished turn reported resumed"
    );
    let notices = wait_source(&d, "pm", "worker_notice", 1, 5);
    assert_eq!(notices.len(), 1, "{notices:?}");
}

/// `stall_secs=0` disables detection entirely while `silent_secs` keeps
/// accruing for the views; a live `agent set` re-budgets the same
/// running turn (string values, as `agent set` sends them).
#[test]
fn stall_secs_zero_disables_and_live_set_rearms() {
    let d = TestDaemon::start();
    d.register_inbox("pm");
    register_fake_opts(&d, "w1", json!({"stall_secs": 0}));
    d.wait_agent("w1", "idle", 10);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "SLEEP:14", "reply_to": "pm",
               "message": "m-zero"}),
    )
    .unwrap();
    d.wait_message("w1", "m-zero", &["running"], 10);
    thread::sleep(Duration::from_secs(7));
    assert!(
        d.events("w1")
            .iter()
            .all(|e| e["kind"].as_str() != Some("turn_stalled")),
        "stall_secs=0 still fired"
    );
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(agent["stalled"], false, "{agent}");
    assert!(agent["silent_secs"].as_u64().unwrap_or(0) > 0, "{agent}");

    // Live-set to a real budget — silence already past it fires on the
    // next tick; the string form proves `agent set` parsing.
    d.rpc(
        "agent_set",
        json!({"alias": "w1", "patch": {"stall_secs": "3"}}),
    )
    .unwrap();
    let e = d.wait_event("w1", "turn_stalled", 15);
    assert_eq!(e["payload"]["message"], "m-zero", "{e}");
}

/// The ticker samples every live pty pane — idle included, so an
/// approval menu raised with no message in flight still surfaces —
/// and stops only when the agent does.
#[test]
fn pty_stall_sampling_runs_while_the_pane_lives() {
    let d = TestDaemon::start();
    let mock = d.mock_stub();
    stall_sample(1);
    // A high budget keeps this test out of stall semantics entirely —
    // it only measures the sampling clock.
    d.register_stub("w1", json!({"auto_ready": "verified", "stall_secs": 600}));
    d.wait_agent("w1", "idle", 20);
    let captures = || {
        std::fs::read_to_string(d.stub_pane_file(&mock, "w1", "captures"))
            .unwrap_or_default()
            .len()
    };
    // Nothing has ever been sent — an idle-but-live pane still
    // samples: the approval-menu watch needs the frames.
    let baseline = captures();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut grew = false;
    while Instant::now() < deadline {
        if captures() > baseline {
            grew = true;
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    assert!(grew, "idle pane was never sampled: {baseline}");

    // Stopping the pane mid-sampling returns promptly — the ticker
    // never holds the adapter across a capture.
    let stop_at = Instant::now();
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    assert!(
        stop_at.elapsed() < Duration::from_secs(10),
        "agent stop delayed by sampling: {:?}",
        stop_at.elapsed()
    );
    // The actor is gone — sampling stops with it. Settle first so a
    // sample already in flight at the stop lands in the baseline.
    d.wait_agent("w1", "stopped", 15);
    thread::sleep(Duration::from_secs(4));
    let idle_count = captures();
    thread::sleep(Duration::from_secs(4));
    assert_eq!(
        captures(),
        idle_count,
        "capture-pane ran after the agent stopped"
    );
    stall_sample(0);
}

// ---- CAD-102: approval menus are a probe state, not busy churn ----

/// The real Devin permission menu — option rows and the selection
/// footer ABOVE a still-visible busy input box (the CAD-102 incident
/// layout): everything the analyzer must see sits ~11 rows above the
/// frame end.
const DEVIN_MENU: &str = "\
❭ run the shell command: printenv FOO
 ⏺ Running command
 └ $ printenv FOO

❭ 1 Yes  (Approve once)
· 2 Yes, allow `printenv` commands
· 3 Yes, always allow `printenv` commands in `tmp`
· 4 Yes, always allow `printenv` commands in all projects
· 5 Yes, switch to bypass mode
· 6 Edit command
· 7 Describe change to command
· 8 No
↑↓ select · ↵ confirm · esc cancel
⠸ Thinking · 5s (esc twice to interrupt)
❭ Guide Devin while it works
";

/// A numbered menu over the busy box is `approval_menu`, never busy:
/// the gate refuses pastes under it (the claim survives untouched),
/// `agent answer` sends the option's one keystroke and records
/// `approval_answered`, and the sampled rise lands an `approval_menu`
/// event with the menu line.
#[test]
fn pty_approval_menu_blocks_pastes_and_answers() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    stall_sample(1);
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);
    d.rpc(
        "agent_send",
        json!({"alias": "dv", "text": "do work", "message": "m1"}),
    )
    .unwrap();
    let token1 = pty_token(&d, "dv", "m1");

    // The menu appears mid-turn.
    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);
    let rise = d.wait_event("dv", "approval_menu", 20);
    assert_eq!(rise["payload"]["message"], "m1", "{rise}");
    assert_eq!(rise["payload"]["line"], "$ printenv FOO", "{rise}");

    // The probe reads the menu line, not busy churn.
    let probe = d.rpc("agent_probe", json!({"alias": "dv"})).unwrap();
    assert_eq!(probe["approval_menu"], true, "{probe}");
    assert_eq!(probe["reason"], "$ printenv FOO", "{probe}");
    // The views carry the menu line while the turn runs.
    let show = d.rpc("agent_show", json!({"alias": "dv"})).unwrap()["agent"].clone();
    assert_eq!(show["pane_menu"], "$ printenv FOO", "{show}");

    // A paste under the menu is refused: m2 queues behind a gate_wait
    // naming the menu, and no claim is eaten by the refusal.
    d.rpc("agent_ready", json!({"alias": "dv", "force": true}))
        .unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv", "text": "wait for idle", "message": "m2"}),
    )
    .unwrap();
    let wait = d.wait_event("dv", "gate_wait", 15);
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("approval menu"),
        "{wait}"
    );
    assert_eq!(d.message_state("dv", "m2"), "queued");

    // `agent answer` validates the choice against the visible menu —
    // 9 is not on it — then sends the one digit key.
    assert!(d
        .rpc("agent_answer", json!({"alias": "dv", "choice": "9"}))
        .is_err());
    let answered = d
        .rpc(
            "agent_answer",
            json!({"alias": "dv", "choice": "8", "by": "test-op"}),
        )
        .unwrap();
    assert_eq!(answered["state"], "answered", "{answered}");
    let input = std::fs::read_to_string(d.pane_file(&mock, "dv", "input")).unwrap();
    assert!(
        input.ends_with("<KEY:8>"),
        "the digit key, never a paste: {input}"
    );
    let ev = d.wait_event("dv", "approval_answered", 10);
    assert_eq!(ev["payload"]["choice"], "8", "{ev}");
    assert_eq!(ev["payload"]["line"], "$ printenv FOO", "{ev}");
    // Identity is derived from the peer pid — the test process sits
    // outside every pane, so `by` is `operator` when it holds a
    // foreign terminal (a suite on a pty) and `unknown` when fully
    // detached; the supplied name survives only as a claim.
    let want = if (0..=2).any(|fd| {
        std::fs::read_link(format!("/proc/self/fd/{fd}"))
            .is_ok_and(|p| p.to_string_lossy().starts_with("/dev/pts/"))
    }) {
        "operator"
    } else {
        "unknown"
    };
    assert_eq!(ev["payload"]["by"], want, "{ev}");
    assert_eq!(ev["payload"]["claimed_by"], "test-op", "{ev}");
    assert_eq!(ev["payload"]["probe"]["approval_menu"], true, "{ev}");

    // With the menu cleared (operator closed it), the surviving claim
    // delivers m2 — no second `agent ready` needed: the refusal ate
    // nothing. And an answer on a non-menu pane refuses.
    std::fs::remove_file(d.pane_file(&mock, "dv", "tui-state")).unwrap();
    assert!(d
        .rpc("agent_answer", json!({"alias": "dv", "choice": "8"}))
        .is_err());
    d.wait_message("dv", "m2", &["running"], 20);
    let token2 = pty_token(&d, "dv", "m2");
    for (id, token) in [("m1", &token1), ("m2", &token2)] {
        d.rpc(
            "message_report",
            json!({"message": id, "token": token, "kind": "result",
                   "text": "done"}),
        )
        .unwrap();
        d.wait_message("dv", id, &["completed"], 10);
    }
    stall_sample(0);
}

/// A turn that ends at the idle prompt without reporting is detected
/// by the sampled probe: `turn_silent_end` fires once per message
/// carrying the age and the admitting probe, the views flag it
/// (`silent_ended`, `ended_secs`, `ended?:` in status, an overview
/// needs-me row), and a `--ready` send is the one-command recovery.
/// The message itself is never auto-resolved.
#[test]
fn pty_silent_end_fires_once_and_recovers() {
    let d = TestDaemon::start();
    let _mock = d.mock_stub();
    stall_sample(1);
    d.register_stub(
        "w1",
        json!({"auto_ready": "verified", "silent_end_secs": 4}),
    );
    d.wait_agent("w1", "idle", 20);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "do work", "message": "ms9"}),
    )
    .unwrap();
    let token = pty_token(&d, "w1", "ms9");

    // The stub pane returns to `» stub ready` after the submission —
    // the message still runs but the probe reads idle.
    let e = d.wait_event("w1", "turn_silent_end", 40);
    assert_eq!(e["payload"]["message"], "ms9", "{e}");
    assert!(e["payload"]["age_secs"].as_u64().unwrap_or(0) >= 4, "{e}");
    assert_eq!(e["payload"]["probe"]["idle"], true, "{e}");
    assert!(
        e["payload"]["last_activity"].as_f64().unwrap_or(0.0) > 0.0,
        "{e}"
    );

    // Once per message: the pane stays idle but no second event fires.
    thread::sleep(Duration::from_secs(6));
    assert_eq!(wait_event_count(&d, "w1", "turn_silent_end", 1, 2).len(), 1);

    // The views flag it: show/list carry silent_ended + ended_secs,
    // status renders `ended?:`, and the overview needs-me row names
    // the ready-gated recovery command.
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(show["silent_ended"], true, "{show}");
    assert!(show["ended_secs"].as_u64().unwrap_or(0) >= 4, "{show}");
    let row = d.rpc("agent_list", json!({})).unwrap()["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"].as_str() == Some("w1"))
        .cloned()
        .unwrap();
    assert_eq!(row["silent_ended"], true, "{row}");
    let table = status_table(&d.state, &[]);
    assert!(table.contains("ended?:"), "{table}");
    let home = TempDir::new().unwrap();
    let view = overview_at(home.path(), &d.state, None, &[]);
    let needs = view["needs_me"].as_array().unwrap();
    let ended = needs
        .iter()
        .find(|n| n["kind"] == "silent_end")
        .expect("silent_end row");
    assert_eq!(
        ended["command"],
        "cadence send w1 --ready --text \"continue …\""
    );
    assert!(ended["title"].as_str().unwrap().contains("w1"));

    // The message is flagged, never auto-resolved — and the remedy is
    // the documented ready-gated follow-up verbatim: the idle pane
    // passes the claim probe and the new turn proceeds normally.
    assert_eq!(d.message_state("w1", "ms9"), "running");
    let (ok, sent) = cadence_cli(
        &d.state,
        &["send", "w1", "--ready", "--text", "continue"],
        &[],
    );
    assert!(ok, "{sent}");
    let ms10 = sent["message"].as_str().unwrap().to_string();
    let token2 = pty_token(&d, "w1", &ms10);
    for (id, t) in [("ms9", &token), (ms10.as_str(), &token2)] {
        d.rpc(
            "message_report",
            json!({"message": id, "token": t, "kind": "result",
                   "text": "done"}),
        )
        .unwrap();
        d.wait_message("w1", id, &["completed"], 10);
    }
    stall_sample(0);
}

/// A menu that opens BEFORE any turn starts — the pane sits blocked
/// with a message still queued — must surface identically to a
/// mid-turn one: `approval_menu` fires against the queued head
/// (marked `queued`), the views carry `pane_menu`, the needs-me row
/// names the answer command, and `agent answer` unblocks delivery.
#[test]
fn pty_queued_menu_surfaces_and_answers() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    stall_sample(1);
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);

    // The menu opens first; the send behind it can only queue.
    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);
    d.rpc(
        "agent_send",
        json!({"alias": "dv", "text": "blocked send", "message": "mq"}),
    )
    .unwrap();
    // `queued` or `submitting` — the delivery loop may have already
    // claimed the head and be gate-waiting on the menu; it cannot be
    // running while the pane shows a menu.
    let st = d.message_state("dv", "mq");
    assert!(st == "queued" || st == "submitting", "{st}");

    // The queued head is tracked for menu detection: the event names
    // the waiting message and marks it queued, the views carry the
    // menu line with no running turn at all.
    let rise = d.wait_event("dv", "approval_menu", 20);
    assert_eq!(rise["payload"]["message"], "mq", "{rise}");
    assert_eq!(rise["payload"]["queued"], true, "{rise}");
    assert_eq!(rise["payload"]["line"], "$ printenv FOO", "{rise}");
    let show = d.rpc("agent_show", json!({"alias": "dv"})).unwrap()["agent"].clone();
    assert_eq!(show["pane_menu"], "$ printenv FOO", "{show}");
    let home = TempDir::new().unwrap();
    let view = overview_at(home.path(), &d.state, None, &[]);
    let needs = view["needs_me"].as_array().unwrap();
    assert!(
        needs.iter().any(|n| n["kind"] == "approval_menu"
            && n["command"]
                .as_str()
                .unwrap_or("")
                .starts_with("cadence agent answer dv ")),
        "{needs:?}"
    );

    // A garbage index is rejected at the RPC — the count never
    // reaches a key vector — and the daemon answers normally after.
    let err = d
        .rpc(
            "agent_answer",
            json!({"alias": "dv", "choice": "4000000000"}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("no option 4000000000"), "{err}");
    // And a menu whose option block cannot be parsed refuses rather
    // than walking blind — the legend anchor alone, no option rows.
    atomic_write(
        d.pane_file(&mock, "dv", "tui-state"),
        "↑↓ select · ↵ confirm · esc cancel\n⠸ Thinking · 5s (esc twice to interrupt)\n",
    );
    let probe = d.rpc("agent_probe", json!({"alias": "dv"})).unwrap();
    assert_eq!(probe["approval_menu"], true, "{probe}");
    assert!(d
        .rpc("agent_answer", json!({"alias": "dv", "choice": "1"}))
        .is_err());
    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);

    // Answering the menu unblocks the queued send — the pane claims
    // cleanly once the operator's menu is gone. A real TUI consumes
    // the answer key; the mock leaves it staged, so clear the input
    // file the way an answered menu would.
    d.rpc("agent_answer", json!({"alias": "dv", "choice": "8"}))
        .unwrap();
    std::fs::remove_file(d.pane_file(&mock, "dv", "tui-state")).unwrap();
    atomic_write(d.pane_file(&mock, "dv", "input"), "");
    d.rpc("agent_ready", json!({"alias": "dv"})).unwrap();
    d.wait_message("dv", "mq", &["running"], 20);
    let token = pty_token(&d, "dv", "mq");
    d.rpc(
        "message_report",
        json!({"message": "mq", "token": token, "kind": "result",
               "text": "done"}),
    )
    .unwrap();
    d.wait_message("dv", "mq", &["completed"], 10);
    stall_sample(0);
}

/// A menu raised on a pane with NO message at all still surfaces —
/// the stall watch samples idle panes, the event fires `idle: true`
/// and attributes no message, and the needs-me row names the remedy.
#[test]
fn pty_idle_pane_menu_surfaces() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    stall_sample(1);
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);

    // Nothing was ever sent — the menu arrives on a quiet pane.
    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);
    let rise = d.wait_event("dv", "approval_menu", 20);
    assert_eq!(rise["payload"]["idle"], true, "{rise}");
    assert_eq!(rise["payload"]["line"], "$ printenv FOO", "{rise}");
    assert!(rise["payload"]["message"].is_null(), "{rise}");
    let show = d.rpc("agent_show", json!({"alias": "dv"})).unwrap()["agent"].clone();
    assert_eq!(show["pane_menu"], "$ printenv FOO", "{show}");
    let home = TempDir::new().unwrap();
    let view = overview_at(home.path(), &d.state, None, &[]);
    let needs = view["needs_me"].as_array().unwrap();
    assert!(
        needs.iter().any(|n| n["kind"] == "approval_menu"
            && n["command"]
                .as_str()
                .unwrap_or("")
                .starts_with("cadence agent answer dv ")),
        "{needs:?}"
    );

    // The operator can answer it straight away.
    d.rpc("agent_answer", json!({"alias": "dv", "choice": "8"}))
        .unwrap();
    stall_sample(0);
}

/// A menu that closes and later reopens is a NEW approval — the same
/// subject must fire `approval_menu` again. The event history is
/// scoped to the open menu: it clears when the menu closes, so the
/// second occurrence is never deduped away.
#[test]
fn pty_menu_event_refires_after_close() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    stall_sample(1);
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);

    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);
    let first = d.wait_event("dv", "approval_menu", 20);
    assert_eq!(first["payload"]["line"], "$ printenv FOO", "{first}");

    // Menu closes — the idle screen returns. The sampler must observe
    // at least one non-menu frame before the reopen.
    std::fs::remove_file(d.pane_file(&mock, "dv", "tui-state")).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let show = d.rpc("agent_show", json!({"alias": "dv"})).unwrap()["agent"].clone();
        if show["pane_menu"].is_null() {
            break;
        }
        assert!(deadline.elapsed() < Duration::from_secs(20), "{show}");
        thread::sleep(Duration::from_millis(250));
    }

    // The same menu reopens — same subject — and fires again.
    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);
    let events = wait_event_count(&d, "dv", "approval_menu", 2, 20);
    assert_eq!(events[1]["payload"]["line"], "$ printenv FOO", "{events:?}");
    stall_sample(0);
}

/// The answerer's identity is derived from the socket peer's pid
/// walking its /proc ancestry into a pane — never from a `by` the
/// client chose. A caller inside the target's own pane is refused
/// outright; inside another agent's pane it stamps that agent.
#[test]
fn pty_answer_derives_caller_from_peer_pid() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.register_devin_opts("peer", json!({}));
    d.wait_agent("dv", "idle", 20);
    d.wait_agent("peer", "idle", 20);
    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);

    // Point a pane pid at this test process and every RPC it makes
    // descends from that pane — the unforgeable "caller is inside
    // the agent" signal.
    let me = std::process::id() as i64;
    let set_pid = |alias: &str, pid: i64| {
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "UPDATE agents SET pid=?1 WHERE alias=?2",
            rusqlite::params![pid, alias],
        )
        .unwrap();
    };

    // Self-approval refuses before any key is sent — even with `by`
    // claiming to be an operator.
    set_pid("dv", me);
    let err = d
        .rpc(
            "agent_answer",
            json!({"alias": "dv", "choice": "8", "by": "operator"}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("its own pane"), "{err}");
    let input = std::fs::read_to_string(d.pane_file(&mock, "dv", "input")).unwrap_or_default();
    assert!(!input.contains("<KEY"), "no key sent: {input}");

    // A caller inside ANOTHER agent's pane stamps that agent; a `by`
    // claiming otherwise is kept only as a claim.
    set_pid("dv", 999_999_999);
    set_pid("peer", me);
    d.rpc(
        "agent_answer",
        json!({"alias": "dv", "choice": "8", "by": "dv"}),
    )
    .unwrap();
    let ev = d.wait_event("dv", "approval_answered", 10);
    assert_eq!(ev["payload"]["by"], "peer", "{ev}");
    assert_eq!(ev["payload"]["by_kind"], "agent", "{ev}");
    assert_eq!(ev["payload"]["claimed_by"], "dv", "{ev}");
    assert_eq!(ev["payload"]["caller_pid"], me, "{ev}");

    // Outside every pane the caller is an operator only when it holds
    // a foreign terminal — this test process inherits one when the
    // suite runs on a pty, none under piped CI — and `unknown`
    // otherwise. A `by` naming the target is a claim, not an
    // attribution either way.
    set_pid("peer", 999_999_999);
    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);
    d.rpc(
        "agent_answer",
        json!({"alias": "dv", "choice": "1", "by": "dv"}),
    )
    .unwrap();
    let on_tty = (0..=2).any(|fd| {
        std::fs::read_link(format!("/proc/self/fd/{fd}"))
            .is_ok_and(|p| p.to_string_lossy().starts_with("/dev/pts/"))
    });
    let want = if on_tty { "operator" } else { "unknown" };
    let evs = wait_event_count(&d, "dv", "approval_answered", 2, 10);
    assert_eq!(evs[1]["payload"]["by"], want, "{evs:?}");
    assert_eq!(evs[1]["payload"]["by_kind"], want, "{evs:?}");
    assert_eq!(evs[1]["payload"]["claimed_by"], "dv", "{evs:?}");
}

/// `setsid` detaches the caller from the pane's /proc ancestry — the
/// self-approval guard must still see through it. The pane's own
/// `CADENCE_ALIAS` env survives the detach, so `setsid env
/// CADENCE_ALIAS=<self> cadence agent answer <self>` is refused rather
/// than stamped `operator`. A detached caller carrying another pane's
/// alias attributes to that pane — an agent, never an operator.
#[test]
fn pty_answer_setsid_cannot_launder_self_approval() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.register_devin_opts("peer", json!({}));
    d.wait_agent("dv", "idle", 20);
    d.wait_agent("peer", "idle", 20);
    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);

    // The detached call names its own pane in env — refused, no key
    // reaches the input, no event is stamped.
    let out = std::process::Command::new("setsid")
        .arg("env")
        .arg("CADENCE_ALIAS=dv")
        .arg(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["agent", "answer", "dv", "8"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "self-answer must refuse: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let input = std::fs::read_to_string(d.pane_file(&mock, "dv", "input")).unwrap_or_default();
    assert!(!input.contains("<KEY"), "no key sent: {input}");
    let events = d.rpc("agent_events", json!({"alias": "dv"})).unwrap();
    assert!(
        !events.to_string().contains("approval_answered"),
        "self-answer must not stamp an event: {events}"
    );

    // Detached and carrying ANOTHER pane's env — stamps that pane as
    // the agent, not `operator`.
    let out = std::process::Command::new("setsid")
        .arg("env")
        .arg("CADENCE_ALIAS=peer")
        .arg(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["agent", "answer", "dv", "8"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ev = d.wait_event("dv", "approval_answered", 10);
    assert_eq!(ev["payload"]["by"], "peer", "{ev}");
    assert_eq!(ev["payload"]["by_kind"], "agent", "{ev}");
}

/// A *full* detach — `env -u CADENCE_ALIAS setsid sh -c 'cadence agent
/// answer <self> </dev/null >/dev/null 2>&1'` — clears ancestry, env
/// and tty at once. The caller matches nothing, so the honest stamp is
/// `unknown`, never `operator`: `operator` needs positive terminal
/// evidence the detached process cannot carry.
#[test]
fn pty_answer_full_detach_stamps_unknown() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);
    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);

    let out = std::process::Command::new("setsid")
        .arg("env")
        .arg("-u")
        .arg("CADENCE_ALIAS")
        .arg(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["agent", "answer", "dv", "8"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "unmatched-but-unproven caller answers as unknown: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ev = d.wait_event("dv", "approval_answered", 10);
    assert_eq!(ev["payload"]["by"], "unknown", "{ev}");
    assert_eq!(ev["payload"]["by_kind"], "unknown", "{ev}");
}

/// A transcript that quotes a real menu verbatim — anchor, `❯`-led
/// numbered run and all — cannot flip the pane: a live menu replaces
/// the input box's interior, so the boxed `❯` prompt still rendered
/// below the quote proves the menu-looking rows are text. The probe
/// stays inert and `agent answer` refuses rather than keying a digit
/// into the live input line.
#[test]
fn pty_claude_quoted_menu_above_input_box_is_inert() {
    let d = TestDaemon::start();
    let mock = d.mock_claude_tui();
    d.register_claude_pty("cl", json!({"auto_ready": "verified"}));
    d.wait_agent("cl", "idle", 20);
    atomic_write(
        d.claude_pane_file(&mock, "cl", "tui-state"),
        "● I reproduced it. The pane printed:\n\n    Do you want to proceed?\n    ❯ 1. Yes\n      2. No, and tell Claude what to do differently\n\n  So it is waiting on you.\n────────────────────\n❯ \n",
    );
    let probe = d.rpc("agent_probe", json!({"alias": "cl"})).unwrap();
    assert_eq!(probe["approval_menu"], false, "{probe}");
    assert_eq!(probe["idle"], true, "{probe}");
    let err = d
        .rpc("agent_answer", json!({"alias": "cl", "choice": "2"}))
        .unwrap_err();
    assert!(err.to_string().contains("no approval menu"), "{err}");
    let input =
        std::fs::read_to_string(d.claude_pane_file(&mock, "cl", "input")).unwrap_or_default();
    assert!(!input.contains("<KEY"), "no key sent: {input}");
}

/// Same class on the Devin profile (CAD-102 r6): a verbatim quoted
/// menu above the live idle input box probes inert — the legend is
/// transcript text, and the editable `❭` row vetoes the region —
/// and `agent answer` refuses rather than keying the digit into the
/// input line.
#[test]
fn pty_devin_quoted_menu_above_input_box_is_inert() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);
    atomic_write(
        d.pane_file(&mock, "dv", "tui-state"),
        "● The pane showed:\n\n  Allow this tool call?\n  ❭ 1 Yes  (Approve once)\n  · 2 Yes, allow `env` commands\n  · 8 No\n  ↑↓ select · ↵ confirm · esc cancel\n\n  So it is waiting.\n\n────────────────────\n❭ Ask Devin to build features, fix bugs, or work on your code\n────────────────────\nSWE-2 Max   Context: 43k / 262k\n",
    );
    let probe = d.rpc("agent_probe", json!({"alias": "dv"})).unwrap();
    assert_eq!(probe["approval_menu"], false, "{probe}");
    assert_eq!(probe["idle"], true, "{probe}");
    let err = d
        .rpc("agent_answer", json!({"alias": "dv", "choice": "2"}))
        .unwrap_err();
    assert!(err.to_string().contains("no approval menu"), "{err}");
    let input = std::fs::read_to_string(d.pane_file(&mock, "dv", "input")).unwrap_or_default();
    assert!(!input.contains("<KEY"), "no key sent: {input}");
}

/// A transient `capture-pane` failure inside the gate probe refuses
/// the send like a busy pane — `gate_wait`, message still queued —
/// never an actor-fatal provider error. The daemon survives and the
/// send delivers once the outage clears.
#[test]
fn pty_gate_probe_failure_is_a_gate_refusal() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);

    // `MOCK_TMUX_FAIL` is process-global — a parallel test's mock
    // calls could trip on it inside this window. The outage is
    // seconds-long and the failure mode (a gate retry) is benign, so
    // the knob stays env-global rather than growing a per-pane
    // failure file.
    std::env::set_var("MOCK_TMUX_FAIL", "capture-pane");
    d.rpc(
        "agent_send",
        json!({"alias": "dv", "text": "during outage", "message": "mf"}),
    )
    .unwrap();
    let wait = d.wait_event("dv", "gate_wait", 15);
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("pane probe failed"),
        "{wait}"
    );
    assert_eq!(d.message_state("dv", "mf"), "queued");
    std::env::remove_var("MOCK_TMUX_FAIL");

    d.wait_message("dv", "mf", &["running"], 20);
    let token = pty_token(&d, "dv", "mf");
    d.rpc(
        "message_report",
        json!({"message": "mf", "token": token, "kind": "result",
               "text": "done"}),
    )
    .unwrap();
    d.wait_message("dv", "mf", &["completed"], 10);
}

/// `agent answer` is a menu channel only: a pane with no menu refuses
/// (idle, busy or fenced alike), and a non-pty endpoint has no such
/// channel at all.
#[test]
fn pty_answer_refuses_without_a_menu() {
    let d = TestDaemon::start();
    let _mock = d.mock_stub();
    d.register_stub("w1", json!({}));
    d.register("fx");
    d.wait_agent("w1", "idle", 20);
    d.wait_agent("fx", "idle", 10);
    let err = d
        .rpc("agent_answer", json!({"alias": "w1", "choice": "1"}))
        .unwrap_err();
    assert!(err.to_string().contains("no approval menu"), "{err}");
    let err = d
        .rpc("agent_answer", json!({"alias": "fx", "choice": "1"}))
        .unwrap_err();
    assert!(
        err.to_string().contains("no approval-menu channel"),
        "{err}"
    );
}

// ---- CAD-55: `cadence dispatch` + `cadence issue finish` against a live daemon ----

/// One dispatch: `issue start` side effects + exactly one templated
/// kickoff + comment + message ref. A second run reuses the worktree
/// and refuses the duplicate while the first is live. Fenced and
/// out-of-group workers are refused before anything is created, as is
/// a `--summary` that breaks the pty single-line rule. `--job` binds
/// the kickoff through `job dispatch` to the scoped task. `issue
/// finish` then refuses while the owner has a live message, while the
/// worktree is dirty, and while the branch is unmerged+unpushed —
/// and succeeds after the merge.
#[test]
fn dispatch_kickoff_and_finish_guards() {
    // Seed the group before the daemon starts: pm plus its members,
    // one fenced, one outside the group.
    let seeded = TempDir::new().unwrap();
    let state = seeded.path().to_path_buf();
    {
        let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
        let cwd = state.to_str().unwrap().to_string();
        // w1/w2 are `inbox` endpoints — no actor drains their queue,
        // so a queued kickoff stays live for the duplicate checks and
        // exercises the CAD-64 inbox-owner exemption in finish. The
        // others are fake.
        for (alias, params, kind) in [
            ("pm", None, "fake"),
            ("w1", Some("{\"upstream\":\"pm\"}"), "inbox"),
            ("w2", Some("{\"upstream\":\"pm\"}"), "inbox"),
            ("fenced", Some("{\"upstream\":\"pm\"}"), "fake"),
            ("outsider", None, "fake"),
        ] {
            store
                .register_agent(&NewAgent {
                    alias,
                    provider: "fake",
                    endpoint_kind: kind,
                    role: "worker",
                    cwd: &cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
        store
            .set_agent_state("fenced", "attention", Some("test fence"))
            .unwrap();
    }
    let d = TestDaemon::start_on(state);
    d.wait_agent("fenced", "attention", 10);

    // Tracker + project repo + issues fixture (same shape as the
    // issue-start test).
    let tmp = TempDir::new().unwrap();
    let (pm_dir, repo, home) = (
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("home"),
    );
    for dir in [&pm_dir, &repo, &home] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let git = |dir: &Path, args: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr)
        );
    };
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "x").unwrap();
    // A cargo checkout — CAD-95 r3 plants the dep-cache farm only in
    // repos with a Cargo.toml; this fixture wants the shared-farm
    // assertions, so it declares itself a cargo package (build
    // output gitignored, as real repos do).
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"m\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(repo.join(".gitignore"), "/target\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    let cli = |args: &[&str]| -> (bool, Value) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&d.state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        let text = if out.stdout.is_empty() {
            String::from_utf8_lossy(&out.stderr).to_string()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        (
            out.status.success(),
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
        )
    };
    assert!(cli(&["issue", "init"]).0);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s,]).0);
    for title in ["One", "Two", "Three", "Four"] {
        assert!(cli(&["issue", "new", title, "--project", "demo"]).0);
    }
    let tracker_commits = || {
        String::from_utf8_lossy(
            &std::process::Command::new("git")
                .arg("-C")
                .arg(&pm_dir)
                .args(["rev-list", "--count", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .parse::<usize>()
        .unwrap()
    };
    let note = tmp.path().join("kickoff.md");
    std::fs::write(&note, "# kickoff D-1").unwrap();
    let note_s = note.canonicalize().unwrap().to_str().unwrap().to_string();

    // A fenced worker refuses before anything is created.
    let (ok, err) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "fenced",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("fenced"),
        "{err}"
    );
    let wt1 = repo.join(".cadence/wt/d-1-one");
    assert!(!wt1.exists());

    // A body that breaks the single-line rule refuses pre-creation.
    let (ok, err) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
        "--summary",
        "line one\nline two",
    ]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("single line"),
        "{err}"
    );
    assert!(!wt1.exists());
    let before = tracker_commits();

    // The dispatch: worktree+branch, owner w1, exactly one kickoff.
    let (ok, out) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok, "{out}");
    assert_eq!(out["dispatched"], true);
    assert_eq!(out["created"], true);
    assert!(wt1.is_dir());
    // CAD-95: dispatch goes through `issue start` — the worktree's
    // hashed cargo subdirs are linked into the shared dep cache and
    // the lane's own target dir is reported.
    let shared = repo.join(".cadence/target/shared");
    assert_eq!(
        out["target_dir"].as_str().unwrap(),
        wt1.join("target").to_string_lossy()
    );
    assert_eq!(
        std::fs::read_link(wt1.join("target/debug/deps")).unwrap(),
        shared.join("debug/deps")
    );
    assert!(!wt1.join(".cargo").exists());
    let msg_id = out["message"].as_str().unwrap().to_string();
    assert!(!msg_id.is_empty());
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let msgs = show["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    let kickoff = &msgs[0];
    assert_eq!(kickoff["id"].as_str().unwrap(), msg_id);
    assert_eq!(kickoff["state"].as_str().unwrap(), "queued");
    assert_eq!(kickoff["reply_to"].as_str().unwrap(), "pm");
    let body = kickoff["body"].as_str().unwrap();
    assert!(
        body.starts_with(&format!("read {note_s} — D-1:"))
            && body.contains(".cadence/wt/d-1-one")
            && body.contains("(branch cadence/d-1-one, base ")
            && body.contains("Commit trailer: Issue: D-1")
            && body.contains("PR to main; reply to pm."),
        "{body}"
    );
    // Tracker: start + comment + ref commits; the comment and the
    // message ref name the worker and the kickoff id.
    assert_eq!(tracker_commits(), before + 3);
    let issue = cli(&["issue", "show", "D-1", "--json"]).1;
    assert_eq!(issue["owner"].as_str().unwrap(), "w1");
    assert!(
        issue["comments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["body"].as_str().unwrap().contains("Dispatched to w1")),
        "{issue}"
    );
    assert!(
        issue["refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "message" && r["path"] == msg_id),
        "{issue}"
    );

    // A second identical run reuses the worktree and refuses the
    // duplicate while the kickoff is still live — no new commits.
    let (ok, out) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok, "{out}");
    assert_eq!(out["dispatched"], false);
    assert_eq!(out["duplicate"], true);
    assert_eq!(out["message"].as_str().unwrap(), msg_id);
    assert_eq!(out["created"], false);
    assert_eq!(tracker_commits(), before + 3);
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(show["messages"].as_array().unwrap().len(), 1);

    // --job: an out-of-group worker is refused before the job exists.
    let (spec, _sha) = d.spec_file("spec.md", "job dispatch spec");
    let (ok, err) = cli(&[
        "dispatch",
        "D-2",
        "--to",
        "outsider",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
        "--job",
        "--spec",
        &spec,
    ]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("group"),
        "{err}"
    );
    assert!(!repo.join(".cadence/wt/d-2-two").exists());

    // --job to a member: job + scoped task + a task_dispatch kickoff,
    // all bound to the issue.
    let (ok, out) = cli(&[
        "dispatch",
        "D-2",
        "--to",
        "w2",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
        "--job",
        "--spec",
        &spec,
    ]);
    assert!(ok, "{out}");
    assert_eq!(out["dispatched"], true);
    let (job_id, task_id) = (
        out["job"].as_str().unwrap().to_string(),
        out["task"].as_str().unwrap().to_string(),
    );
    assert_eq!(task_id, format!("{job_id}-t1"));
    let job = d.rpc("job_show", json!({"job": job_id})).unwrap();
    let tasks = job["job"]["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["assignee"].as_str().unwrap(), "w2");
    assert_eq!(tasks[0]["worktree"].as_str().unwrap(), "d-2-two");
    assert_eq!(tasks[0]["state"].as_str().unwrap(), "dispatched");
    // The kickoff rides the task — the issue's message ref matches.
    let issue = cli(&["issue", "show", "D-2", "--json"]).1;
    let ref_msg = issue["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "message")
        .and_then(|r| r["path"].as_str())
        .unwrap()
        .to_string();
    assert_eq!(ref_msg, out["message"].as_str().unwrap());

    // `issue finish` with w1's kickoff still queued: the kickoff's
    // message ref is recorded against THIS worktree, but a queued
    // message on an `inbox` mailbox is durable backlog — it drains
    // only on `cadence inbox`, never on its own (CAD-64). Only the
    // unmerged branch blocks.
    std::fs::write(wt1.join("work.txt"), "x").unwrap();
    git(&wt1, &["add", "-A"]);
    git(&wt1, &["commit", "-qm", "d-1 work"]);
    let (ok, err) = cli(&["issue", "finish", "D-1"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("neither merged") && !msg.contains(&msg_id),
        "{msg}"
    );
    assert!(wt1.is_dir());

    // Merged: the queued inbox kickoff never blocks — finish succeeds
    // without --force and the mail stays queued, unconsumed.
    git(&repo, &["merge", "-q", "cadence/d-1-one"]);
    let (ok, out) = cli(&["issue", "finish", "D-1"]);
    assert!(
        ok && out["finished"] == true
            && out["overrode"] == json!([])
            && out["merged_by"] == "ancestry",
        "{out}"
    );
    assert!(!wt1.exists());
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(show["messages"].as_array().unwrap()[0]["state"], "queued");

    // A non-inbox owner: a mock-devin pty pane holding a RUNNING
    // message recorded against THIS worktree makes finish refuse
    // naming the message, and --force records the override. mk1 is
    // unbound until the test links it — before that, finish only
    // refuses the unmerged branch (CAD-94: a busy owner elsewhere is
    // not a reason).
    let _mock = d.mock_devin();
    d.register_devin("dvb", None);
    d.wait_agent("dvb", "idle", 15);
    let (ok, _) = cli(&["issue", "start", "D-3", "--owner", "dvb"]);
    assert!(ok);
    d.rpc(
        "agent_send",
        json!({"alias": "dvb", "text": "keep working", "message": "mk1"}),
    )
    .unwrap();
    d.rpc("agent_ready", json!({"alias": "dvb"})).unwrap();
    d.wait_message("dvb", "mk1", &["running"], 10);
    let wt3 = repo.join(".cadence/wt/d-3-three");
    // Give the branch real work so survivability blocks too — the
    // unbound running message must NOT add a refusal of its own.
    std::fs::write(wt3.join("work3.txt"), "x").unwrap();
    git(&wt3, &["add", "-A"]);
    git(&wt3, &["commit", "-qm", "d-3 work"]);
    let (ok, err) = cli(&["issue", "finish", "D-3"]);
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("neither merged"),
        "{err}"
    );
    // Record mk1 against D-3 → the same live message now blocks, named.
    let (ok, out) = cli(&["issue", "ref", "D-3", "message", "mk1"]);
    assert!(ok, "{out}");
    let (ok, err) = cli(&["issue", "finish", "D-3"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("mk1") && msg.contains("running") && msg.contains("dvb"),
        "{msg}"
    );
    assert!(wt3.is_dir());
    let (ok, out) = cli(&["issue", "finish", "D-3", "--force"]);
    assert!(ok && out["finished"] == true, "{out}");
    assert!(
        out["overrode"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o == "bound-message"),
        "{out}"
    );
    assert!(!wt3.exists());

    // D-4: owner w2 idle (its kickoff went to D-2's task, and w2's
    // queued message is the dispatch on D-2 — wait, w2 HAS a live
    // message from the job dispatch). Use a fresh started issue owned
    // by 'pm' — pm has no inbound messages.
    let (ok, _) = cli(&["issue", "start", "D-4", "--owner", "pm"]);
    assert!(ok);
    let wt4 = repo.join(".cadence/wt/d-4-four");
    // Dirty refusal lists the files.
    std::fs::write(wt4.join("wip.txt"), "x").unwrap();
    let (ok, err) = cli(&["issue", "finish", "D-4"]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("wip.txt"),
        "{err}"
    );
    // Unmerged+unpushed refusal once committed.
    git(&wt4, &["add", "-A"]);
    git(&wt4, &["commit", "-qm", "wip"]);
    let (ok, err) = cli(&["issue", "finish", "D-4"]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("neither merged"),
        "{err}"
    );
    // Merged → finish succeeds, refs closed in one commit.
    git(&repo, &["merge", "-q", "cadence/d-4-four"]);
    let before = tracker_commits();
    let (ok, out) = cli(&["issue", "finish", "D-4"]);
    assert!(
        ok && out["finished"] == true
            && out["overrode"] == json!([])
            && out["merged_by"] == "ancestry",
        "{out}"
    );
    assert!(!wt4.exists());
    assert_eq!(tracker_commits(), before + 1);
    let issue = cli(&["issue", "show", "D-4", "--json"]).1;
    assert!(
        issue["refs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["kind"] != "worktree" || r["closed"] == true),
        "{issue}"
    );
}

/// CAD-107: dispatch commits the `message` ref BEFORE the send — a
/// finish racing the dispatch sees the binding as soon as the ref
/// lands, and a send that then fails leaves only a stale ref, which
/// binds nothing.
#[test]
fn dispatch_records_ref_before_send() {
    let seeded = TempDir::new().unwrap();
    let state = seeded.path().to_path_buf();
    {
        let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
        let cwd = state.to_str().unwrap().to_string();
        for (alias, params, kind) in [
            ("pm", None, "fake"),
            ("w1", Some("{\"upstream\":\"pm\"}"), "inbox"),
        ] {
            store
                .register_agent(&NewAgent {
                    alias,
                    provider: "fake",
                    endpoint_kind: kind,
                    role: "worker",
                    cwd: &cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
    }
    let d = TestDaemon::start_on(state);
    let tmp = TempDir::new().unwrap();
    let (pm_dir, repo, home) = (
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("home"),
    );
    for dir in [&pm_dir, &repo, &home] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let git = |dir: &Path, args: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success());
    };
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "x").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    let cli = |args: &[&str]| -> (bool, Value) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&d.state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        let text = if out.stdout.is_empty() {
            String::from_utf8_lossy(&out.stderr).to_string()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        (
            out.status.success(),
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
        )
    };
    assert!(cli(&["issue", "init"]).0);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s,]).0);
    assert!(cli(&["issue", "new", "Reffirst", "--project", "demo"]).0);
    let note = tmp.path().join("kickoff.md");
    std::fs::write(&note, "# kickoff").unwrap();
    let note_s = note.canonicalize().unwrap().to_str().unwrap().to_string();

    // --reply-to names an agent that does not exist — the daemon's
    // enqueue rejects it, so the send fails AFTER the ref commits.
    let (ok, err) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "ghost",
    ]);
    assert!(!ok, "{err}");

    // The binding landed anyway — before the send — and the failed
    // send closed it: kept as history, never a live binding.
    let issue = cli(&["issue", "show", "D-1", "--json"]).1;
    let mref = issue["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "message")
        .expect("the message ref must be recorded before the send");
    let mid = mref["path"].as_str().unwrap().to_string();
    assert!(!mid.is_empty());
    assert_eq!(
        mref["closed"], true,
        "a failed send closes its orphan ref: {issue}"
    );
    // A second dispatch is not a duplicate — the closed ref is not a
    // live kickoff, so the retry sends fresh rather than refusing.
    let (ok, out) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(
        ok && out["dispatched"] == true,
        "the closed orphan must not read as an in-flight dispatch: {out}"
    );

    // A stale ref binds nothing: the forced finish still works and
    // the dead ref never becomes a bound-message block.
    let (ok, out) = cli(&["issue", "finish", "D-1", "--force"]);
    assert!(ok && out["finished"] == true, "{out}");
    assert!(
        !out["overrode"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o == "bound-message"),
        "a ref for a message that was never sent must not block: {out}"
    );

    // The daemon-side trigger the second-ref guard exists for: a
    // same-revision `task_dispatch` retry ignores the caller-minted id
    // and returns the still-live kickoff's. `issue dispatch --job`
    // always mints a fresh `<job>-t1` (a new job per `issue start`),
    // so this path can't be driven through the CLI — prove the rpc
    // contract directly so the guard's premise stays honest.
    let created = d
        .rpc(
            "job_new",
            json!({"pm": "pm", "spec": "s", "spec_sha256": "x",
                   "title": "t", "task_assignee": "w1"}),
        )
        .unwrap();
    let task = format!("{}-t1", created["job"]["id"].as_str().unwrap());
    let first = d
        .rpc(
            "task_dispatch",
            json!({"task": task, "message": "mint-one", "by": "pm"}),
        )
        .unwrap();
    assert_eq!(first["message"], "mint-one");
    let second = d
        .rpc(
            "task_dispatch",
            json!({"task": task, "message": "mint-two", "by": "pm"}),
        )
        .unwrap();
    assert_eq!(
        second["message"], "mint-one",
        "a retry returns the live kickoff id, not the minted one"
    );
}

/// CAD-94: the finish guard is per worktree, not per agent. An owner
/// busy on worktree A does not block finishing the same owner's
/// merged worktree B; a process with cwd inside B is refused naming
/// the pid; a live message recorded against B is refused naming the
/// message id.
#[test]
fn finish_guard_per_worktree() {
    let seeded = TempDir::new().unwrap();
    let state = seeded.path().to_path_buf();
    {
        let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
        let cwd = state.to_str().unwrap().to_string();
        for (alias, params, kind) in [
            ("pm", None, "fake"),
            ("w1", Some("{\"upstream\":\"pm\"}"), "inbox"),
        ] {
            store
                .register_agent(&NewAgent {
                    alias,
                    provider: "fake",
                    endpoint_kind: kind,
                    role: "worker",
                    cwd: &cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
        // A fenced devin/pty agent that was never launched: endpoint
        // none + state attention reads `dead: true` (a `starting`
        // agent races the daemon's failed-launch → `stopped` parking,
        // which would read alive). Its queue can never start — a
        // queued message bound to its worktree must not block finish.
        store
            .register_agent(&NewAgent {
                alias: "deadpty",
                provider: "devin",
                endpoint_kind: "pty",
                role: "worker",
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params: Some("{\"upstream\":\"pm\"}"),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        store
            .set_agent_state("deadpty", "attention", Some("never launched"))
            .unwrap();
        store
            .enqueue("deadpty", "queued forever", None, "mkdead", "test")
            .unwrap();
    }
    let d = TestDaemon::start_on(state);
    let tmp = TempDir::new().unwrap();
    let (pm_dir, repo, home) = (
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("home"),
    );
    for dir in [&pm_dir, &repo, &home] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let git = |dir: &Path, args: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr)
        );
    };
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "x").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    let cli = |args: &[&str]| -> (bool, Value) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&d.state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        let text = if out.stdout.is_empty() {
            String::from_utf8_lossy(&out.stderr).to_string()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        (
            out.status.success(),
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
        )
    };
    assert!(cli(&["issue", "init"]).0);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s,]).0);
    for title in [
        "Awt", "Bwt", "Cwt", "Dwt", "Ghost", "Scoped", "Inboxrun", "Nonowner",
    ] {
        assert!(cli(&["issue", "new", title, "--project", "demo"]).0);
    }
    let note = tmp.path().join("kickoff.md");
    std::fs::write(&note, "# kickoff").unwrap();
    let note_s = note.canonicalize().unwrap().to_str().unwrap().to_string();

    // w1 busy in worktree A: a queued kickoff recorded against D-1.
    let (ok, out) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok && out["dispatched"] == true, "{out}");
    let msg_a = out["message"].as_str().unwrap().to_string();

    // Same owner's merged worktree B finishes without --force: the
    // dispatch's message ref carries worktree A, so it doesn't bind
    // this target at all (and a queued inbox message is backlog, not
    // live work).
    let (ok, _) = cli(&["issue", "start", "D-2", "--owner", "w1"]);
    assert!(ok);
    let wt_b = repo.join(".cadence/wt/d-2-bwt");
    std::fs::write(wt_b.join("b.txt"), "x").unwrap();
    git(&wt_b, &["add", "-A"]);
    git(&wt_b, &["commit", "-qm", "b work"]);
    git(&repo, &["merge", "-q", "cadence/d-2-bwt"]);
    let (ok, out) = cli(&["issue", "finish", "D-2"]);
    assert!(
        ok && out["finished"] == true && out["overrode"] == json!([]),
        "busy elsewhere must not block: {out}"
    );
    assert!(!wt_b.exists());

    // A process with cwd inside worktree C refuses naming the pid.
    let (ok, _) = cli(&["issue", "start", "D-3", "--owner", "w1"]);
    assert!(ok);
    let wt_c = repo.join(".cadence/wt/d-3-cwt");
    let mut shell = std::process::Command::new("sleep")
        .arg("300")
        .current_dir(&wt_c)
        .spawn()
        .unwrap();
    let (ok, err) = cli(&["issue", "finish", "D-3"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains(&shell.id().to_string()) && msg.contains("cwd inside"),
        "proc refusal names the pid: {msg}"
    );
    shell.kill().unwrap();
    let _ = shell.wait();

    // A queued message blocks only on a LIVE non-inbox owner — give
    // C to a live devin pane and bind a queued message to it.
    let _mock = d.mock_devin();
    d.register_devin("dv", None);
    d.wait_agent("dv", "idle", 15);
    let (ok, _) = cli(&["issue", "set", "D-3", "owner=dv"]);
    assert!(ok);
    d.rpc(
        "agent_send",
        json!({"alias": "dv", "text": "queued against C", "message": "mkc"}),
    )
    .unwrap();
    let (ok, _) = cli(&["issue", "ref", "D-3", "message", "mkc"]);
    assert!(ok);
    let (ok, err) = cli(&["issue", "finish", "D-3"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("mkc")
            && msg.contains("recorded against this worktree")
            && !msg.contains(&msg_a),
        "message refusal names the bound id: {msg}"
    );
    assert!(wt_c.is_dir());

    // A dead pty owner with a queued bound message does not block —
    // its queue can never start.
    let show = d.rpc("agent_show", json!({"alias": "deadpty"})).unwrap();
    assert_eq!(show["agent"]["dead"], true, "fixture must read dead");
    let (ok, _) = cli(&["issue", "start", "D-4", "--owner", "deadpty"]);
    assert!(ok);
    let wt_d = repo.join(".cadence/wt/d-4-dwt");
    std::fs::write(wt_d.join("d.txt"), "x").unwrap();
    git(&wt_d, &["add", "-A"]);
    git(&wt_d, &["commit", "-qm", "d work"]);
    git(&repo, &["merge", "-q", "cadence/d-4-dwt"]);
    let (ok, _) = cli(&["issue", "ref", "D-4", "message", "mkdead"]);
    assert!(ok);
    let (ok, out) = cli(&["issue", "finish", "D-4"]);
    assert!(
        ok && out["finished"] == true && out["overrode"] == json!([]),
        "queued on a dead owner must not block: {out}"
    );
    assert!(!wt_d.exists());

    // An owner the daemon has never heard of cannot be using the
    // worktree — Rejected is an answer, not a transport failure.
    let (ok, _) = cli(&["issue", "start", "D-5"]);
    assert!(ok);
    let (ok, _) = cli(&["issue", "set", "D-5", "owner=ghost"]);
    assert!(ok);
    let wt_g = repo.join(".cadence/wt/d-5-ghost");
    std::fs::write(wt_g.join("g.txt"), "x").unwrap();
    git(&wt_g, &["add", "-A"]);
    git(&wt_g, &["commit", "-qm", "g work"]);
    git(&repo, &["merge", "-q", "cadence/d-5-ghost"]);
    // The probe runs WITHOUT the pm lock: with the lock file held, a
    // stale-socket daemon (it was there and stopped answering) still
    // returns the unreachable refusal — it never waits on (or times
    // out against) the lock.
    std::fs::write(pm_dir.join(".write.lock"), "held").unwrap();
    let dead_state = tmp.path().join("deadstate");
    std::fs::create_dir_all(&dead_state).unwrap();
    std::fs::write(dead_state.join("cadence.sock"), "stale").unwrap();
    let cli_on = |state_dir: &Path, args: &[&str]| -> std::process::Output {
        std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(state_dir)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap()
    };
    let out = cli_on(&dead_state, &["issue", "finish", "D-5"]);
    std::fs::remove_file(pm_dir.join(".write.lock")).unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success());
    assert!(
        text.contains("unreachable") && !text.contains("locked"),
        "the probe must not wait on the pm lock: {text}"
    );
    // A cleanly stopped daemon removes its socket: the same finish on
    // a socket-less state dir is "no agents", and the /proc + pane
    // scans carry the check — no --force needed.
    std::fs::remove_file(dead_state.join("cadence.sock")).unwrap();
    let out = cli_on(&dead_state, &["issue", "finish", "D-5", "--json"]);
    let nod = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "no daemon at all must not block a clean merged finish: {nod}"
    );
    let out: Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    assert!(
        out["finished"] == true && out["overrode"] == json!([]),
        "unknown owner must not block: {out}"
    );
    assert!(!wt_g.exists());
    // A second finish is the idempotent no-op.
    let (ok, out) = cli(&["issue", "finish", "D-5"]);
    assert!(ok && out["finished"] == false, "already finished: {out}");

    // D-6: a bound live message must be found on the agent that
    // HOLDS it, not only the current owner — re-assigning the issue
    // leaves the earlier dispatchee's message bound (CAD-107). And a
    // worktree-scoped message ref must not bind a re-started pair.
    let (ok, out) = cli(&[
        "dispatch",
        "D-6",
        "--to",
        "dv",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok && out["dispatched"] == true, "{out}");
    let msg_d6 = out["message"].as_str().unwrap().to_string();
    let (ok, err) = cli(&["issue", "finish", "D-6"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(msg.contains(&msg_d6), "bound refusal names the id: {msg}");
    // owner=w1 now, but the bound message lives on dv — it must
    // still block.
    let (ok, _) = cli(&["issue", "set", "D-6", "owner=w1"]);
    assert!(ok);
    let (ok, err) = cli(&["issue", "finish", "D-6"]);
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains(&msg_d6),
        "a non-owner recipient's bound message still blocks: {err}"
    );
    // Re-start under --name: the old pair still refuses; the new
    // pair is not bound to a message scoped to the old worktree.
    let (ok, out) = cli(&["issue", "start", "D-6", "--name", "scd"]);
    assert!(ok, "{out}");
    let (ok, err) = cli(&["issue", "finish", "D-6"]);
    assert!(!ok, "the first open pair still refuses: {err}");
    let (ok, out) = cli(&["issue", "finish", "D-6", "--force"]);
    assert!(
        ok && out["overrode"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o == "bound-message"),
        "the bound-message block is what --force overrode: {out}"
    );
    // The new pair's branch sits at the base tip (merged by
    // ancestry) and the still-live message is scoped to the removed
    // pair — the finish succeeds clean.
    let wt_scd = repo.join(".cadence/wt/d-6-scd");
    let (ok, out) = cli(&["issue", "finish", "D-6"]);
    assert!(
        ok && out["finished"] == true && out["overrode"] == json!([]),
        "a ref scoped to the old worktree must not bind the new one: {out}"
    );
    assert!(!wt_scd.exists());

    // D-7: `running` is live even on an inbox endpoint — the durable
    // backlog exemption covers only `queued`. The ref is unscoped
    // (`issue ref` writes no worktree) and still binds.
    let (ok, _) = cli(&["issue", "start", "D-7", "--owner", "w1"]);
    assert!(ok);
    {
        let store = Store::open(&d.state.join("cadence.sqlite3")).unwrap();
        store
            .enqueue("w1", "running on inbox", None, "mir", "test")
            .unwrap();
        store.mark_running("mir", "turn-mir").unwrap();
    }
    let (ok, _) = cli(&["issue", "ref", "D-7", "message", "mir"]);
    assert!(ok);
    let (ok, err) = cli(&["issue", "finish", "D-7"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("mir") && msg.contains("running"),
        "a running message on an inbox owner must block: {msg}"
    );

    // D-8: the running-on-inbox rule discriminates by recipient, not
    // owner — w1 (inbox) holds the bound running message while D-8 is
    // owned by dv, and it still blocks (D-7 kept as the owner case).
    let (ok, out) = cli(&[
        "dispatch",
        "D-8",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok && out["dispatched"] == true, "{out}");
    let msg_d8 = out["message"].as_str().unwrap().to_string();
    {
        let store = Store::open(&d.state.join("cadence.sqlite3")).unwrap();
        store.mark_running(&msg_d8, "turn-d8").unwrap();
    }
    let (ok, _) = cli(&["issue", "set", "D-8", "owner=dv"]);
    assert!(ok);
    let (ok, err) = cli(&["issue", "finish", "D-8"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains(&msg_d8) && msg.contains("running"),
        "a non-owner recipient's running inbox message must block: {msg}"
    );
}

/// CAD-242: `issue finish` holds a present merged worktree while an
/// unreconciled `unknown` still refers to it — bound to the worktree,
/// or sitting on an agent whose cwd is that directory (a child counts;
/// a sibling prefix does not). A reconciled lane with a stale fence
/// error still finishes. `--force` records `unreconciled-unknown`; the
/// sweep cannot force.
#[test]
fn finish_holds_unreconciled_unknown() {
    let seeded = TempDir::new().unwrap();
    let state = seeded.path().to_path_buf();
    let d = TestDaemon::start_on(state);
    let tmp = TempDir::new().unwrap();
    let (pm_dir, repo, home) = (
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("home"),
    );
    for dir in [&pm_dir, &repo, &home] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let git = |dir: &Path, args: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr)
        );
    };
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "x").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    let cli_raw = |args: &[&str]| -> (i32, String, String) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&d.state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let cli = |args: &[&str]| -> (bool, Value) {
        let (code, stdout, stderr) = cli_raw(args);
        let text = if stdout.is_empty() { stderr } else { stdout };
        (
            code == 0,
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
        )
    };
    assert!(cli(&["issue", "init"]).0);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s,]).0);
    for title in ["Reconciled", "Samecwd", "Bound", "Elsewhere", "Child"] {
        assert!(cli(&["issue", "new", title, "--project", "demo"]).0);
    }
    for id in ["D-1", "D-2", "D-3", "D-4", "D-5"] {
        let (ok, out) = cli(&["issue", "start", id]);
        assert!(ok, "{out}");
        let (ok, _) = cli(&["issue", "set", id, "owner="]);
        assert!(ok);
    }
    let (ok, _) = cli(&["issue", "set", "D-3", "owner=bound"]);
    assert!(ok);
    let (ok, out) = cli(&["issue", "ref", "D-3", "message", "m-bound"]);
    assert!(ok, "{out}");

    let wt = |slug: &str| repo.join(format!(".cadence/wt/{slug}"));
    for slug in [
        "d-1-reconciled",
        "d-2-samecwd",
        "d-3-bound",
        "d-4-elsewhere",
        "d-5-child",
    ] {
        std::fs::write(wt(slug).join(format!("{slug}.txt")), "x").unwrap();
        git(&wt(slug), &["add", "-A"]);
        git(&wt(slug), &["commit", "-qm", slug]);
        git(&repo, &["merge", "-q", &format!("cadence/{slug}")]);
    }
    let child_cwd = wt("d-5-child").join("nested");
    std::fs::create_dir_all(&child_cwd).unwrap();
    // A sibling whose name merely extends the worktree's prefix must
    // not count as cwd-on-worktree.
    let sibling = repo.join(".cadence/wt/d-4-elsewhere-extra");
    std::fs::create_dir_all(&sibling).unwrap();
    let elsewhere = tmp.path().join("other");
    std::fs::create_dir_all(&elsewhere).unwrap();

    let body = "secret-body-should-not-leak";
    {
        let store = Store::open(&d.state.join("cadence.sqlite3")).unwrap();
        let reg = |alias: &str, cwd: &Path| {
            store
                .register_agent(&NewAgent {
                    alias,
                    provider: "fake",
                    endpoint_kind: "inbox",
                    role: "worker",
                    cwd: cwd.to_str().unwrap(),
                    sandbox: "read-only",
                    instructions: None,
                    params: None,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
            store.set_enabled(alias, false).unwrap();
        };
        reg("settled", &wt("d-1-reconciled"));
        reg("holder", &wt("d-2-samecwd"));
        reg("bound", &elsewhere);
        reg("stray", &sibling);
        reg("child", &child_cwd);
        let fence = |alias: &str, id: &str| {
            store.enqueue(alias, body, None, id, "test").unwrap();
            store.mark_running(id, &format!("turn-{id}")).unwrap();
            let message = store.message(id).unwrap().unwrap();
            store
                .finish(
                    &message,
                    "unknown",
                    &json!({
                        "status": "unknown",
                        "text": "",
                        "error": "Uncertain provider outcome"
                    }),
                    Some("Uncertain provider outcome"),
                )
                .unwrap();
            store
                .set_agent_state(alias, "attention", Some("Uncertain provider outcome"))
                .unwrap();
        };
        fence("settled", "m-settled");
        fence("holder", "m-holder");
        fence("bound", "m-bound");
        fence("stray", "m-stray");
        fence("child", "m-child");
        store
            .reconcile(
                "m-settled",
                "interrupted",
                Some("outcome was a no-op"),
                "operator",
                None,
            )
            .unwrap();
    }

    let settled = d.rpc("agent_show", json!({"alias": "settled"})).unwrap();
    assert_eq!(settled["agent"]["state"], "stopped", "{settled}");
    assert!(
        settled["agent"]["error"]
            .as_str()
            .unwrap_or_default()
            .contains("Uncertain provider outcome"),
        "a reconciled fence must keep its stale error: {settled}"
    );
    assert_eq!(settled["messages"][0]["state"], "interrupted", "{settled}");
    let holder = d.rpc("agent_show", json!({"alias": "holder"})).unwrap();
    assert_eq!(holder["messages"][0]["state"], "unknown", "{holder}");

    let (code, stdout, _) = cli_raw(&["issue", "finish", "--merged", "--dry-run", "--json"]);
    assert_eq!(code, 1, "{stdout}");
    let plan: Value = serde_json::from_str(stdout.trim()).unwrap();
    let rows = plan["rows"].as_array().unwrap();
    let outcome = |id: &str| -> (String, String) {
        rows.iter()
            .find(|r| r["issue"] == id)
            .map(|r| {
                (
                    r["outcome"].as_str().unwrap().to_string(),
                    r["reason"].as_str().unwrap_or_default().to_string(),
                )
            })
            .unwrap_or_else(|| panic!("missing {id}: {plan}"))
    };
    assert_eq!(outcome("D-1").0, "would-finish", "{plan}");
    for (id, alias, mid) in [
        ("D-2", "holder", "m-holder"),
        ("D-3", "bound", "m-bound"),
        ("D-5", "child", "m-child"),
    ] {
        let (o, reason) = outcome(id);
        assert_eq!(o, "refused", "{id} {plan}");
        assert!(
            reason.contains(alias)
                && reason.contains(mid)
                && reason.contains("unreconciled")
                && !reason.contains(body),
            "{id} reason must name the alias and the unreconciled unknown, not the body: {reason}"
        );
    }
    assert_eq!(outcome("D-4").0, "would-finish", "{plan}");
    for slug in [
        "d-1-reconciled",
        "d-2-samecwd",
        "d-3-bound",
        "d-4-elsewhere",
        "d-5-child",
    ] {
        assert!(wt(slug).is_dir(), "{slug} must survive dry-run");
    }

    let (code, stdout, _) = cli_raw(&["issue", "finish", "--merged", "--json"]);
    assert_eq!(code, 1, "{stdout}");
    let swept: Value = serde_json::from_str(stdout.trim()).unwrap();
    let rows = swept["rows"].as_array().unwrap();
    let swept_outcome = |id: &str| {
        rows.iter()
            .find(|r| r["issue"] == id)
            .map(|r| r["outcome"].as_str().unwrap().to_string())
            .unwrap_or_else(|| panic!("missing {id}: {swept}"))
    };
    assert_eq!(swept_outcome("D-1"), "finished", "{swept}");
    assert_eq!(swept_outcome("D-4"), "finished", "{swept}");
    assert_eq!(swept_outcome("D-2"), "refused", "{swept}");
    assert_eq!(swept_outcome("D-3"), "refused", "{swept}");
    assert_eq!(swept_outcome("D-5"), "refused", "{swept}");
    assert!(!wt("d-1-reconciled").exists());
    assert!(!wt("d-4-elsewhere").exists());
    assert!(wt("d-2-samecwd").is_dir() && wt("d-3-bound").is_dir() && wt("d-5-child").is_dir());
    for id in ["D-2", "D-3", "D-5"] {
        let issue = cli(&["issue", "show", id, "--json"]).1;
        let open = issue["refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "worktree" && r["closed"] != true);
        assert!(open, "sweep must not close {id}'s worktree ref: {issue}");
    }

    let (ok, out) = cli(&["issue", "finish", "D-2", "--force"]);
    assert!(ok && out["finished"] == true, "{out}");
    assert!(
        out["overrode"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o == "unreconciled-unknown"),
        "{out}"
    );
    assert!(!wt("d-2-samecwd").exists());

    // The sweep still has no force flag: the remaining unknowns stay.
    let (code, stdout, _) = cli_raw(&["issue", "finish", "--merged", "--json"]);
    assert_eq!(code, 1, "{stdout}");
    let again: Value = serde_json::from_str(stdout.trim()).unwrap();
    let rows = again["rows"].as_array().unwrap();
    for id in ["D-3", "D-5"] {
        let row = rows.iter().find(|r| r["issue"] == id).unwrap();
        assert_eq!(row["outcome"], "refused", "{again}");
        assert!(
            row["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("unreconciled"),
            "{again}"
        );
    }
    assert!(wt("d-3-bound").is_dir() && wt("d-5-child").is_dir());
}

/// CAD-93: `issue finish --merged` sweeps every open worktree ref
/// whose branch is merged and whose guard passes — one row per
/// worktree (finished | skipped | refused), exit 1 on any refusal,
/// and `--dry-run` changes nothing. Ownerless issues need no daemon.
#[test]
fn finish_merged_sweep() {
    let tmp = TempDir::new().unwrap();
    let (pm_dir, repo, home, state) = (
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("home"),
        tmp.path().join("state"),
    );
    for dir in [&pm_dir, &repo, &home, &state] {
        std::fs::create_dir_all(dir).unwrap();
    }
    // A live daemon: the bound-message enumeration must not fail
    // open — a daemon that cannot answer `agent_list` is itself a
    // refusal now, so the idle-path rows need one that answers.
    let _d = TestDaemon::start_on(state.clone());
    let git = |dir: &Path, args: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr)
        );
    };
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "x").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    let cli_raw = |args: &[&str]| -> (i32, String, String) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let cli = |args: &[&str]| -> (bool, Value) {
        let (code, stdout, stderr) = cli_raw(args);
        let text = if stdout.is_empty() { stderr } else { stdout };
        (
            code == 0,
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
        )
    };
    assert!(cli(&["issue", "init"]).0);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s,]).0);
    for title in ["Merged", "Inuse", "Unmerged", "Dirty", "Ghost"] {
        assert!(cli(&["issue", "new", title, "--project", "demo"]).0);
    }
    // All started ownerless — no daemon involvement anywhere; D-5
    // keeps an owner the daemon can't answer for.
    for id in ["D-1", "D-2", "D-3", "D-4", "D-5"] {
        let (ok, out) = cli(&["issue", "start", id]);
        assert!(ok, "{out}");
        let (ok, _) = cli(&["issue", "set", id, "owner="]);
        assert!(ok);
    }
    let (ok, _) = cli(&["issue", "set", "D-5", "owner=ghost"]);
    assert!(ok);
    let wt = |slug: &str| repo.join(format!(".cadence/wt/{slug}"));
    // D-1 merged+idle, D-2 merged+in-use, D-3 unmerged, D-4 merged+dirty,
    // D-5 merged with an owner check the daemon can't answer.
    // Each branch writes its own file — identical diffs would collapse
    // to one SHA and every branch would read `merged_by: ancestry`.
    for slug in [
        "d-1-merged",
        "d-2-inuse",
        "d-3-unmerged",
        "d-4-dirty",
        "d-5-ghost",
    ] {
        std::fs::write(wt(slug).join(format!("{slug}.txt")), "x").unwrap();
        git(&wt(slug), &["add", "-A"]);
        git(&wt(slug), &["commit", "-qm", "work"]);
    }
    git(&repo, &["merge", "-q", "cadence/d-1-merged"]);
    git(&repo, &["merge", "-q", "cadence/d-2-inuse"]);
    git(&repo, &["merge", "-q", "cadence/d-4-dirty"]);
    git(&repo, &["merge", "-q", "cadence/d-5-ghost"]);
    let mut shell = std::process::Command::new("sleep")
        .arg("300")
        .current_dir(wt("d-2-inuse"))
        .spawn()
        .unwrap();
    std::fs::write(wt("d-4-dirty").join("wip.txt"), "x").unwrap();

    // --dry-run: the plan, nothing changes, refusals mean exit 1.
    let commits_before = String::from_utf8_lossy(
        &std::process::Command::new("git")
            .arg("-C")
            .arg(&pm_dir)
            .args(["rev-list", "--count", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    let (code, stdout, _) = cli_raw(&["issue", "finish", "--merged", "--dry-run", "--json"]);
    assert_eq!(code, 1, "{stdout}");
    let plan: Value = serde_json::from_str(stdout.trim()).unwrap();
    let rows = plan["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 5, "{plan}");
    let outcome = |id: &str| {
        rows.iter()
            .find(|r| r["issue"] == id)
            .map(|r| {
                (
                    r["outcome"].as_str().unwrap().to_string(),
                    r["reason"].as_str().unwrap_or_default().to_string(),
                )
            })
            .unwrap()
    };
    assert_eq!(outcome("D-1").0, "would-finish", "{plan}");
    let (o, r) = outcome("D-2");
    assert!(
        o == "refused" && r.contains(&shell.id().to_string()),
        "{plan}"
    );
    assert_eq!(
        outcome("D-3"),
        ("skipped".into(), "unmerged".into()),
        "{plan}"
    );
    let (o, r) = outcome("D-4");
    assert!(o == "refused" && r.contains("uncommitted"), "{plan}");
    // A ghost owner the daemon has never heard of is ABSENT, not
    // unreachable — the row would finish.
    assert_eq!(outcome("D-5").0, "would-finish", "{plan}");
    // …and the fail-open the round-2 review closed, now split by how
    // the daemon is absent. A STALE socket — a daemon that was there
    // and stopped answering — still refuses the enumeration: a bound
    // task could hide anywhere. No socket at all is a cleanly stopped
    // daemon: "no agents", and the /proc + pane scans carry the check.
    let dead_state = tmp.path().join("deadstate");
    std::fs::create_dir_all(&dead_state).unwrap();
    std::fs::write(dead_state.join("cadence.sock"), "stale").unwrap();
    let sweep_on = |state_dir: &Path| -> (i32, Value) {
        let dead = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(state_dir)
            .args(["issue", "finish", "--merged", "--dry-run", "--json"])
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        (
            dead.status.code().unwrap_or(-1),
            serde_json::from_str(String::from_utf8_lossy(&dead.stdout).trim()).unwrap_or_else(
                |_| {
                    panic!(
                        "not json: {}{}",
                        String::from_utf8_lossy(&dead.stdout),
                        String::from_utf8_lossy(&dead.stderr)
                    )
                },
            ),
        )
    };
    let (code, dead_plan) = sweep_on(&dead_state);
    assert_eq!(code, 1);
    let dead_rows = dead_plan["rows"].as_array().unwrap();
    for id in ["D-1", "D-5"] {
        let row = dead_rows.iter().find(|r| r["issue"] == id).unwrap();
        assert_eq!(row["outcome"], "refused", "{dead_plan}");
        assert!(
            row["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("unreachable"),
            "a stale-socket daemon refuses the enumeration: {dead_plan}"
        );
    }
    // The same rows on a socket-less dir — the daemon is simply not
    // running, so nothing enumerates and nothing refuses for it.
    std::fs::remove_file(dead_state.join("cadence.sock")).unwrap();
    let (code, gone_plan) = sweep_on(&dead_state);
    assert_eq!(code, 1, "{gone_plan}"); // D-2/D-4 still refuse on their own
    let gone_rows = gone_plan["rows"].as_array().unwrap();
    for id in ["D-1", "D-5"] {
        let row = gone_rows.iter().find(|r| r["issue"] == id).unwrap();
        assert_eq!(
            row["outcome"], "would-finish",
            "no daemon at all means no agents — {id} is clean: {gone_plan}"
        );
    }
    // Nothing changed: dirs exist, refs open, tracker untouched.
    for slug in [
        "d-1-merged",
        "d-2-inuse",
        "d-3-unmerged",
        "d-4-dirty",
        "d-5-ghost",
    ] {
        assert!(wt(slug).is_dir(), "{slug} must survive dry-run");
    }
    let commits_after = String::from_utf8_lossy(
        &std::process::Command::new("git")
            .arg("-C")
            .arg(&pm_dir)
            .args(["rev-list", "--count", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    assert_eq!(commits_before, commits_after, "dry-run must not commit");

    // Real sweep: D-1+D-5 finish, D-2/D-4 refuse, D-3 skips — exit 1.
    let (code, stdout, _) = cli_raw(&["issue", "finish", "--merged", "--json"]);
    assert_eq!(code, 1, "{stdout}");
    let out: Value = serde_json::from_str(stdout.trim()).unwrap();
    let rows = out["rows"].as_array().unwrap();
    let outcome = |id: &str| {
        rows.iter()
            .find(|r| r["issue"] == id)
            .map(|r| r["outcome"].as_str().unwrap().to_string())
            .unwrap()
    };
    assert_eq!(outcome("D-1"), "finished", "{out}");
    assert_eq!(outcome("D-2"), "refused", "{out}");
    assert_eq!(outcome("D-3"), "skipped", "{out}");
    assert_eq!(outcome("D-4"), "refused", "{out}");
    assert_eq!(outcome("D-5"), "finished", "{out}");
    assert!(!wt("d-1-merged").exists());
    assert!(wt("d-2-inuse").is_dir() && wt("d-3-unmerged").is_dir());
    assert!(wt("d-4-dirty").is_dir() && !wt("d-5-ghost").exists());

    // Clear the refusals and the second sweep exits 0 on skip-only.
    shell.kill().unwrap();
    let _ = shell.wait();
    std::fs::remove_file(wt("d-4-dirty").join("wip.txt")).unwrap();
    let (code, stdout, _) = cli_raw(&["issue", "finish", "--merged", "--json"]);
    assert_eq!(code, 0, "{stdout}");
    let out: Value = serde_json::from_str(stdout.trim()).unwrap();
    let rows = out["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "{out}"); // D-1/D-5 finished → no row
    assert!(!wt("d-2-inuse").exists() && !wt("d-4-dirty").exists());
    assert!(wt("d-3-unmerged").is_dir());

    // The done-hint: status=done with an open worktree prints it.
    let (code, _, stderr) = cli_raw(&["issue", "set", "D-3", "status=done"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("worktree open: run cadence issue finish D-3"),
        "{stderr}"
    );

    // CAD-106: a merged PR binds its recorded head COMMIT, not the
    // branch name. A github origin plus a `gh` stub answers per
    // branch: D-6's tip IS the recorded merge head (`merged_by: pr`);
    // D-7 reuses the name with an extra commit the recorded head does
    // not cover — the sweep must report it unmerged and leave the
    // branch alone.
    git(
        &repo,
        &["remote", "add", "origin", "https://github.com/x/y.git"],
    );
    for title in [
        "Prbound",
        "Prreused",
        "Prancestor",
        "Remoteok",
        "Remoteunm",
        "Remoteahead",
        "Remoteforce",
        "Remotestale",
    ] {
        assert!(cli(&["issue", "new", title, "--project", "demo"]).0);
    }
    // D-9..D-12 are started after the sweep — an untouched branch
    // reads merged-by-ancestry and the sweep would finish its
    // worktree out from under the --remote legs.
    for id in ["D-6", "D-7", "D-8"] {
        let (ok, out) = cli(&["issue", "start", id]);
        assert!(ok, "{out}");
        let (ok, _) = cli(&["issue", "set", id, "owner="]);
        assert!(ok);
    }
    let sha = |dir: &Path, rev: &str| -> String {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", rev])
            .output()
            .unwrap();
        assert!(o.status.success());
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    // D-6's single commit IS the merged PR head; D-7 records that
    // same position as its merged head, then adds an unmerged commit.
    std::fs::write(wt("d-6-prbound").join("pr6.txt"), "x").unwrap();
    git(&wt("d-6-prbound"), &["add", "-A"]);
    git(&wt("d-6-prbound"), &["commit", "-qm", "merged head"]);
    let tip6 = sha(&repo, "cadence/d-6-prbound");
    std::fs::write(wt("d-7-prreused").join("pr7.txt"), "x").unwrap();
    git(&wt("d-7-prreused"), &["add", "-A"]);
    git(&wt("d-7-prreused"), &["commit", "-qm", "merged head"]);
    let tip7a = sha(&repo, "cadence/d-7-prreused");
    std::fs::write(wt("d-7-prreused").join("extra.txt"), "x").unwrap();
    git(&wt("d-7-prreused"), &["add", "-A"]);
    git(&wt("d-7-prreused"), &["commit", "-qm", "unmerged extra"]);
    let tip7b = sha(&repo, "cadence/d-7-prreused");
    // D-8: the accepted ancestor path — the branch tip is an ancestor
    // of the recorded PR head (a local branch behind the merged head).
    // The head commit is made on a scratch branch so it exists in the
    // object store without moving the recorded branch.
    std::fs::write(wt("d-8-prancestor").join("pa.txt"), "x").unwrap();
    git(&wt("d-8-prancestor"), &["add", "-A"]);
    git(&wt("d-8-prancestor"), &["commit", "-qm", "work"]);
    git(&wt("d-8-prancestor"), &["checkout", "-q", "-b", "scratch8"]);
    std::fs::write(wt("d-8-prancestor").join("more.txt"), "x").unwrap();
    git(&wt("d-8-prancestor"), &["add", "-A"]);
    git(&wt("d-8-prancestor"), &["commit", "-qm", "pr head"]);
    let head8 = sha(&repo, "scratch8");
    git(
        &wt("d-8-prancestor"),
        &["checkout", "-q", "cadence/d-8-prancestor"],
    );
    let gh_bin = tmp.path().join("ghbin");
    std::fs::create_dir_all(&gh_bin).unwrap();
    std::fs::write(
        gh_bin.join("gh"),
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in\n\
             cadence/d-6-prbound) printf '[{{\"number\":6,\"headRefOid\":\"{tip6}\",\"baseRefName\":\"main\"}}]'; exit 0;;\n\
             cadence/d-7-prreused) printf '[{{\"number\":7,\"headRefOid\":\"{tip7a}\",\"baseRefName\":\"main\"}}]'; exit 0;;\n\
             cadence/d-8-prancestor) printf '[{{\"number\":8,\"headRefOid\":\"{head8}\",\"baseRefName\":\"main\"}}]'; exit 0;;\n\
             esac; done\nprintf '[]'\n"
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(gh_bin.join("gh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    let cli_gh = |args: &[&str]| -> (i32, String) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}:{}",
                    gh_bin.display(),
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
        )
    };
    let (code, stdout) = cli_gh(&["issue", "finish", "--merged", "--dry-run", "--json"]);
    // Skipped rows are not refusals — the dry-run exits clean.
    assert_eq!(code, 0, "{stdout}");
    let plan: Value = serde_json::from_str(stdout.trim()).unwrap();
    let rows = plan["rows"].as_array().unwrap();
    let row = |id: &str| rows.iter().find(|r| r["issue"] == id).unwrap().clone();
    assert_eq!(row("D-6")["outcome"], "would-finish", "{plan}");
    assert_eq!(row("D-6")["merged_by"], "pr", "{plan}");
    assert_eq!(row("D-7")["outcome"], "skipped", "{plan}");
    assert_eq!(row("D-7")["reason"], "unmerged", "{plan}");
    // Tip an ancestor of the recorded head — accepted via pr too.
    assert_eq!(row("D-8")["outcome"], "would-finish", "{plan}");
    assert_eq!(row("D-8")["merged_by"], "pr", "{plan}");
    // Real sweep: D-6/D-8 finish (each tip is covered by the recorded
    // head); D-7's extra commit keeps it skipped — worktree AND
    // branch stay.
    let (_, stdout) = cli_gh(&["issue", "finish", "--merged", "--json"]);
    let out: Value = serde_json::from_str(stdout.trim()).unwrap();
    let rows = out["rows"].as_array().unwrap();
    let row = |id: &str| rows.iter().find(|r| r["issue"] == id).unwrap().clone();
    assert_eq!(row("D-6")["outcome"], "finished", "{out}");
    assert_eq!(row("D-6")["deleted_branch"], true, "{out}");
    assert_eq!(row("D-7")["outcome"], "skipped", "{out}");
    assert_eq!(row("D-8")["outcome"], "finished", "{out}");
    assert_eq!(row("D-8")["deleted_branch"], true, "{out}");
    assert!(
        !wt("d-6-prbound").exists()
            && !wt("d-8-prancestor").exists()
            && wt("d-7-prreused").is_dir(),
        "the reused-name branch must survive: {out}"
    );
    assert_eq!(
        sha(&repo, "cadence/d-7-prreused"),
        tip7b,
        "the unmerged tip must still resolve — the branch survived: {out}"
    );

    // --remote: the remote delete is gated on merge evidence covering
    // the RESOLVED remote tip — never on survivability's "pushed"
    // (that evidence is the remote itself). A real local bare remote
    // replaces the github one; push + update-ref seed the remote and
    // its tracking ref deterministically.
    let remote_git = tmp.path().join("remote.git");
    git(tmp.path(), &["init", "--bare", "remote.git"]);
    let remote_s = remote_git
        .canonicalize()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    git(&repo, &["remote", "set-url", "origin", &remote_s]);
    for id in ["D-9", "D-10", "D-11", "D-12", "D-13"] {
        let (ok, out) = cli(&["issue", "start", id]);
        assert!(ok, "{out}");
        let (ok, _) = cli(&["issue", "set", id, "owner="]);
        assert!(ok);
    }
    let git_ok = |dir: &Path, args: &[&str]| -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success()
    };
    let track = |slug: &str, tip: &str| {
        git(
            &repo,
            &[
                "update-ref",
                &format!("refs/remotes/origin/cadence/{slug}"),
                tip,
            ],
        );
    };
    let remote_has = |slug: &str| {
        git_ok(
            &remote_git,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/cadence/{slug}"),
            ],
        )
    };
    let push = |slug: &str| {
        git(&repo, &["push", "-q", "origin", &format!("cadence/{slug}")]);
    };
    let commit_in = |slug: &str, file: &str| {
        std::fs::write(wt(slug).join(file), "x").unwrap();
        git(&wt(slug), &["add", "-A"]);
        git(&wt(slug), &["commit", "-qm", file]);
    };

    // D-9 merged + remote at the merged tip → remote deleted too.
    commit_in("d-9-remoteok", "r9.txt");
    push("d-9-remoteok");
    track("d-9-remoteok", &sha(&repo, "cadence/d-9-remoteok"));
    git(&repo, &["merge", "-q", "cadence/d-9-remoteok"]);
    let (ok, out) = cli(&["issue", "finish", "D-9", "--remote"]);
    assert!(
        ok && out["finished"] == true
            && out["deleted_branch"] == true
            && out["remote_deleted"] == true,
        "merged branch + matching remote deletes both: {out}"
    );
    assert!(!remote_has("d-9-remoteok"), "the remote branch is gone");

    // D-10 unmerged-but-pushed: the remote was the local's only
    // evidence — with --remote it is kept for lack of merge coverage,
    // and the local stays with it. Both copies survive, row explains.
    commit_in("d-10-remoteunm", "r10.txt");
    let tip10 = sha(&repo, "cadence/d-10-remoteunm");
    push("d-10-remoteunm");
    track("d-10-remoteunm", &tip10);
    let (ok, out) = cli(&["issue", "finish", "D-10", "--remote"]);
    assert!(
        ok && out["finished"] == true
            && out["deleted_branch"] == false
            && out["remote_deleted"] == false
            && out["branch_note"]
                .as_str()
                .unwrap_or_default()
                .contains("kept")
            && out["remote_note"]
                .as_str()
                .unwrap_or_default()
                .contains("kept"),
        "unmerged-but-pushed keeps both copies: {out}"
    );
    assert_eq!(sha(&repo, "cadence/d-10-remoteunm"), tip10);
    assert!(remote_has("d-10-remoteunm"));

    // D-11 merged, but origin moved past the covered tip — the remote
    // carries commits no evidence covers: remote kept; the merged
    // local is still deleted on its own covered tip.
    commit_in("d-11-remoteahead", "r11.txt");
    push("d-11-remoteahead");
    track("d-11-remoteahead", &sha(&repo, "cadence/d-11-remoteahead"));
    git(&repo, &["merge", "-q", "cadence/d-11-remoteahead"]);
    git(&wt("d-11-remoteahead"), &["checkout", "-q", "-b", "scr11"]);
    commit_in("d-11-remoteahead", "ahead.txt");
    let tip11b = sha(&repo, "scr11");
    git(
        &wt("d-11-remoteahead"),
        &[
            "push",
            "-q",
            "origin",
            "scr11:refs/heads/cadence/d-11-remoteahead",
        ],
    );
    track("d-11-remoteahead", &tip11b);
    git(
        &wt("d-11-remoteahead"),
        &["checkout", "-q", "cadence/d-11-remoteahead"],
    );
    let (ok, out) = cli(&["issue", "finish", "D-11", "--remote"]);
    assert!(
        ok && out["finished"] == true
            && out["deleted_branch"] == true
            && out["remote_deleted"] == false
            && out["remote_note"]
                .as_str()
                .unwrap_or_default()
                .contains("kept"),
        "origin-ahead keeps the remote, deletes the merged local: {out}"
    );
    assert!(remote_has("d-11-remoteahead"));

    // D-12 same as D-10 but --force: the uncovered remote is deleted
    // anyway and the override is recorded.
    commit_in("d-12-remoteforce", "r12.txt");
    push("d-12-remoteforce");
    track("d-12-remoteforce", &sha(&repo, "cadence/d-12-remoteforce"));
    let (ok, out) = cli(&["issue", "finish", "D-12", "--remote", "--force"]);
    assert!(
        ok && out["remote_deleted"] == true
            && out["deleted_branch"] == true
            && out["overrode"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o == "remote-delete-uncovered"),
        "--force deletes the uncovered remote and records it: {out}"
    );
    assert!(!remote_has("d-12-remoteforce"));

    // D-13: the tracking ref is stale — synced at tip A, then the
    // SERVER advanced the branch to B while refs/remotes/origin still
    // names A. The pre-delete fetch must reveal B; the stale tracking
    // ref can never prove coverage. Remote kept, B survives, and the
    // row explains.
    commit_in("d-13-remotestale", "r13.txt");
    let tip13a = sha(&repo, "cadence/d-13-remotestale");
    push("d-13-remotestale");
    git(&repo, &["merge", "-q", "cadence/d-13-remotestale"]);
    git(&wt("d-13-remotestale"), &["checkout", "-q", "-b", "scr13"]);
    commit_in("d-13-remotestale", "ahead.txt");
    let tip13b = sha(&repo, "scr13");
    git(
        &wt("d-13-remotestale"),
        &[
            "push",
            "-q",
            "origin",
            "scr13:refs/heads/cadence/d-13-remotestale",
        ],
    );
    git(
        &wt("d-13-remotestale"),
        &["checkout", "-q", "cadence/d-13-remotestale"],
    );
    // Push updated the tracking ref to B — force it back to A, the
    // stale view a fetch must correct before the gate reads it.
    track("d-13-remotestale", &tip13a);
    let (ok, out) = cli(&["issue", "finish", "D-13", "--remote"]);
    assert!(
        ok && out["finished"] == true
            && out["deleted_branch"] == true
            && out["remote_deleted"] == false
            && out["remote_note"]
                .as_str()
                .unwrap_or_default()
                .contains("kept"),
        "a stale tracking ref must not authorize remote deletion: {out}"
    );
    assert!(remote_has("d-13-remotestale"), "B must survive: {out}");
    assert_eq!(
        sha(&remote_git, "refs/heads/cadence/d-13-remotestale"),
        tip13b,
        "the server-side advance survives intact"
    );
}

/// Write an accepted memory fixture with the same authenticated evidence
/// shape produced by the native daemon path. Dispatch/match tests use this
/// fixture so they exercise retrieval eligibility without pretending that a
/// CLI child outside a native PTY can author or review a memory.
fn write_reviewed_memory(
    pm_dir: &Path,
    project: &str,
    slug: &str,
    kind: &str,
    scope: Scope,
    fact: &str,
) -> PathBuf {
    let identity = |alias: &str, registration: u64, role: &str| IdentityProof {
        alias: alias.to_string(),
        registration,
        generation: format!("fixture-{alias}-{registration}"),
        process_start: 100 + registration,
        role: role.to_string(),
    };
    let author = identity("fixture-author", 1, "worker");
    let reviewer_a = identity("fixture-reviewer-a", 2, "worker");
    let reviewer_b = identity("fixture-reviewer-b", 3, "worker");
    let pm = identity("fixture-pm", 4, "pm");
    let body = format!(
        "{fact}\n\n**Why:** reviewed integration fixture.\n\n**How to apply:** apply the fixture rule.\n"
    );
    let mut front = Front {
        id: slug.to_string(),
        kind: kind.to_string(),
        status: "accepted".to_string(),
        scope,
        source: Some("CAD-191".to_string()),
        confidence: "high".to_string(),
        created: "2026-01-01T00:00:00Z".to_string(),
        verified_at: Some("2026-01-02T00:00:00Z".to_string()),
        supersedes: None,
        author: Some(author.alias.clone()),
        author_proof: Some(author.clone()),
        contributors: Vec::new(),
        review_cycle: 1,
        active_operation: None,
        reviews: Vec::new(),
        finalizations: Vec::new(),
    };
    let path = pm_dir
        .join(project)
        .join("memory")
        .join(format!("{slug}.md"));
    let mut memory = Memory {
        project: project.to_string(),
        front: front.clone(),
        body,
        path: path.clone(),
    };
    let digest = memory::semantic_digest(&memory);
    let receipt_digest = digest.clone();
    let receipt = move |reviewer: &IdentityProof, evidence: &str| ReviewReceipt {
        reviewer: reviewer.alias.clone(),
        identity: reviewer.stable_id(),
        generation: reviewer.generation.clone(),
        process_start: reviewer.process_start,
        role: reviewer.role.clone(),
        operation: "accept".to_string(),
        cycle: 1,
        digest: receipt_digest.clone(),
        verdict: "pass".to_string(),
        evidence: evidence.to_string(),
        recorded_at: "2026-01-02T00:00:00Z".to_string(),
    };
    front.reviews = vec![
        receipt(&reviewer_a, "fixture reviewer A evidence"),
        receipt(&reviewer_b, "fixture reviewer B evidence"),
    ];
    front.finalizations.push(FinalizationReceipt {
        operation: "accept".to_string(),
        cycle: 1,
        digest,
        finalizer: pm,
        finalized_at: "2026-01-02T00:00:00Z".to_string(),
    });
    memory.front = front;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        cadence_agent::issue::parse::render(&memory.front, &memory.body).unwrap(),
    )
    .unwrap();
    path
}

struct PmDirGuard(Option<std::ffi::OsString>);

impl PmDirGuard {
    fn set(path: &Path) -> Self {
        let old = std::env::var_os("CADENCE_PM_DIR");
        std::env::set_var("CADENCE_PM_DIR", path);
        Self(old)
    }
}

impl Drop for PmDirGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(value) => std::env::set_var("CADENCE_PM_DIR", value),
            None => std::env::remove_var("CADENCE_PM_DIR"),
        }
    }
}

/// The positive CAD-191 path uses four real mock Devin panes. Each bridge
/// request is opened by the lockholding provider process itself, so the
/// daemon must resolve the actual Unix peer pid through the pane's /proc
/// ancestry and native flock ownership before allowing the write.
#[test]
fn memory_native_socket_identity_requires_distinct_reviewers() {
    let tmp = TempDir::new().unwrap();
    let pm_dir = tmp.path().join("pm");
    let pm = cadence_agent::issue::Pm::init(&pm_dir).unwrap();
    std::fs::create_dir_all(pm_dir.join("demo")).unwrap();
    std::fs::write(
        pm_dir.join("demo/project.yaml"),
        "key: demo\nprefix: D\ncomponents: []\n",
    )
    .unwrap();
    pm.commit("project fixture\n\nActor: test\n").unwrap();

    let mock_dir = TempDir::new().unwrap();
    let mock = install_mock_devin(mock_dir.path());
    let _pm_env = PmDirGuard::set(&pm_dir);
    let d = TestDaemon::start();
    let cwd = d.dir.path().to_str().unwrap().to_string();
    for (alias, role) in [
        ("author", "worker"),
        ("reviewer-a", "worker"),
        ("reviewer-b", "worker"),
        ("pm", "pm"),
    ] {
        d.rpc(
            "agent_register",
            json!({
                "alias": alias,
                "provider": "devin",
                "endpoint_kind": "pty",
                "cwd": cwd,
                "role": role,
                "params": "{\"auto_ready\":\"verified\"}"
            }),
        )
        .unwrap();
    }
    for alias in ["author", "reviewer-a", "reviewer-b", "pm"] {
        d.wait_agent(alias, "idle", 25);
    }

    // Keep the author in a real running turn while its provider socket
    // performs the proposal. This proves the resolver accepts a live,
    // owned endpoint in the ordinary worker state, not only an idle pane.
    d.rpc(
        "agent_send",
        json!({"alias": "author", "text": "hold native identity", "message": "memory-busy"}),
    )
    .unwrap();
    let busy_token = pty_token(&d, "author", "memory-busy");
    d.wait_agent("author", "busy", 10);

    let body = "\nsocket-bound memory claims native identity\n\n**Why:** the provider socket is the authority.\n\n**How to apply:** use only reviewed native memory.\n";
    let proposal = json!({
        "project": "demo",
        "kind": "rule",
        "scope": {"project": true},
        "source": "CAD-191",
        "confidence": "high",
        "text": body,
        "id": "native-socket-rule"
    });

    // Request identity claims are rejected before any PM write. The
    // author pane remains the only possible source of the later proposal.
    let err = d
        .memory_rpc(
            &mock,
            "author",
            "memory_propose",
            json!({"alias": "pm", "reviewer": "pm", "pane": "pm", "inner": proposal.clone()}),
        )
        .unwrap_err();
    assert!(err.contains("connection-bound"), "{err}");
    assert!(!pm_dir.join("demo/memory/native-socket-rule.md").exists());

    let proposed = d
        .memory_rpc(&mock, "author", "memory_propose", proposal)
        .unwrap();
    assert_eq!(proposed["status"], "proposed", "{proposed}");
    let digest = proposed["digest"].as_str().unwrap().to_string();
    assert_eq!(digest.len(), 64, "{proposed}");

    let read_memory = || std::fs::read(pm_dir.join("demo/memory/native-socket-rule.md")).unwrap();
    let before_author_review = read_memory();
    let err = d
        .memory_rpc(
            &mock,
            "author",
            "memory_review",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "author cannot review",
                "digest": digest,
            }),
        )
        .unwrap_err();
    assert!(err.contains("author cannot review"), "{err}");
    assert_eq!(before_author_review, read_memory());

    let err = d
        .memory_rpc(
            &mock,
            "reviewer-a",
            "memory_review",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "spoofed reviewer",
                "digest": digest,
                "reviewer": "reviewer-b",
                "pid": 1,
            }),
        )
        .unwrap_err();
    assert!(err.contains("connection-bound"), "{err}");

    let review_a = d
        .memory_rpc(
            &mock,
            "reviewer-a",
            "memory_review",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "reviewer A inspected the native socket claim",
                "digest": digest,
            }),
        )
        .unwrap();
    assert_eq!(review_a["quorum"]["eligible"], false, "{review_a}");
    assert!(review_a["quorum"]["reason"]
        .as_str()
        .unwrap()
        .contains("1/2"));

    let before_repeat = read_memory();
    let err = d
        .memory_rpc(
            &mock,
            "reviewer-a",
            "memory_review",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "same reviewer twice",
                "digest": digest,
            }),
        )
        .unwrap_err();
    assert!(err.contains("already reviewed"), "{err}");
    assert_eq!(before_repeat, read_memory());

    // A detached integration-test RPC has no pane ancestor and cannot
    // borrow an alias from its params to review or finalize.
    let err = d
        .rpc(
            "memory_review",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "outside all panes",
                "digest": digest,
            }),
        )
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("not owned by exactly one live native PTY"),
        "{err}"
    );

    let before_missing_quorum = read_memory();
    let err = d
        .memory_rpc(
            &mock,
            "pm",
            "memory_finalize",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "digest": digest,
            }),
        )
        .unwrap_err();
    assert!(err.contains("1/2"), "{err}");
    assert_eq!(before_missing_quorum, read_memory());

    let before_missing_evidence = read_memory();
    let err = d
        .memory_rpc(
            &mock,
            "reviewer-b",
            "memory_review",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "",
                "digest": digest,
            }),
        )
        .unwrap_err();
    assert!(err.contains("evidence must be nonempty"), "{err}");
    assert_eq!(before_missing_evidence, read_memory());

    let before_stale = read_memory();
    let err = d
        .memory_rpc(
            &mock,
            "reviewer-b",
            "memory_review",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "stale digest",
                "digest": "0000000000000000000000000000000000000000000000000000000000000000",
            }),
        )
        .unwrap_err();
    assert!(err.contains("revision changed"), "{err}");
    assert_eq!(before_stale, read_memory());

    // A live pane with a stale stored generation is still refused: the
    // endpoint must match the adapter's current native session proof.
    let reviewer_b_generation = d.rpc("agent_show", json!({"alias": "reviewer-b"})).unwrap()
        ["agent"]["generation"]
        .as_str()
        .unwrap()
        .to_string();
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET generation=?1 WHERE alias=?2",
        rusqlite::params!["stale-native-generation", "reviewer-b"],
    )
    .unwrap();
    let before_generation_mismatch = read_memory();
    let err = d
        .memory_rpc(
            &mock,
            "reviewer-b",
            "memory_review",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "stale endpoint generation",
                "digest": digest,
            }),
        )
        .unwrap_err();
    assert!(err.contains("generation changed"), "{err}");
    assert_eq!(before_generation_mismatch, read_memory());
    conn.execute(
        "UPDATE agents SET generation=?1 WHERE alias=?2",
        rusqlite::params![reviewer_b_generation, "reviewer-b"],
    )
    .unwrap();

    let review_b = d
        .memory_rpc(
            &mock,
            "reviewer-b",
            "memory_review",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "reviewer B independently inspected the native socket claim",
                "digest": digest,
            }),
        )
        .unwrap();
    assert_eq!(review_b["quorum"]["eligible"], true, "{review_b}");

    let finalized = d
        .memory_rpc(
            &mock,
            "pm",
            "memory_finalize",
            json!({
                "slug": "native-socket-rule",
                "project": "demo",
                "operation": "accept",
                "digest": digest,
            }),
        )
        .unwrap();
    assert_eq!(finalized["status"], "accepted", "{finalized}");
    assert_eq!(finalized["finalized"], true, "{finalized}");
    assert_eq!(finalized["quorum"]["eligible"], true, "{finalized}");

    let pm = cadence_agent::issue::Pm::at(&pm_dir).unwrap();
    let (_, accepted) = memory::find(&pm, Some("demo"), "native-socket-rule").unwrap();
    assert!(memory::retrieval_status(&accepted).0);
    assert_eq!(accepted.body, body);
    assert_eq!(
        accepted.front.author_proof.as_ref().unwrap().alias,
        "author"
    );
    assert_eq!(
        accepted
            .front
            .reviews
            .iter()
            .map(|r| r.reviewer.as_str())
            .collect::<Vec<_>>(),
        vec!["reviewer-a", "reviewer-b"]
    );

    // Native PM rejection is an ordinary authenticated mutation too. It
    // preserves an earlier review receipt and reports the tracker commit;
    // an HTTP/CLI caller cannot manufacture this result.
    let rejected_proposal = json!({
        "project": "demo",
        "kind": "gotcha",
        "scope": {"project": true},
        "source": "CAD-191",
        "confidence": "medium",
        "text": "native rejection keeps review history\n\n**Why:** the PM rejected it.\n\n**How to apply:** do not use it.\n",
        "id": "native-rejected-rule"
    });
    let proposed_rejected = d
        .memory_rpc(&mock, "author", "memory_propose", rejected_proposal)
        .unwrap();
    let rejected_digest = proposed_rejected["digest"].as_str().unwrap().to_string();
    let review = d
        .memory_rpc(
            &mock,
            "reviewer-a",
            "memory_review",
            json!({
                "slug": "native-rejected-rule",
                "project": "demo",
                "operation": "accept",
                "verdict": "pass",
                "evidence": "reviewer A recorded a retained rejection review",
                "digest": rejected_digest,
            }),
        )
        .unwrap();
    assert_eq!(review["quorum"]["eligible"], false, "{review}");
    let rejected = d
        .memory_rpc(
            &mock,
            "pm",
            "memory_finalize",
            json!({
                "slug": "native-rejected-rule",
                "project": "demo",
                "operation": "reject",
            }),
        )
        .unwrap();
    assert_eq!(rejected["status"], "rejected", "{rejected}");
    assert_eq!(rejected["committed"], true, "{rejected}");
    let (_, rejected_memory) = memory::find(&pm, Some("demo"), "native-rejected-rule").unwrap();
    assert_eq!(rejected_memory.front.status, "rejected");
    assert_eq!(rejected_memory.front.reviews.len(), 1);
    assert_eq!(rejected_memory.front.reviews[0].reviewer, "reviewer-a");
    assert!(!memory::retrieval_status(&rejected_memory).0);

    d.rpc(
        "message_report",
        json!({"message": "memory-busy", "token": busy_token, "kind": "result", "text": "done"}),
    )
    .unwrap();
    d.wait_message("author", "memory-busy", &["completed"], 10);
}

/// `dispatch` renders matching accepted memories into
/// `<state>/dispatch/<msg>-lessons.md`, names the file in the kickoff
/// and the issue comment, and reports slugs in JSON. `--no-lessons`
/// skips the whole path.
#[test]
fn dispatch_injects_project_memory_lessons() {
    let seeded = TempDir::new().unwrap();
    let state = seeded.path().to_path_buf();
    {
        let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
        let cwd = state.to_str().unwrap().to_string();
        // w1 is `inbox` — the queued kickoff stays inspectable.
        for (alias, params, kind) in [
            ("pm", None, "fake"),
            ("w1", Some("{\"upstream\":\"pm\"}"), "inbox"),
        ] {
            store
                .register_agent(&NewAgent {
                    alias,
                    provider: "fake",
                    endpoint_kind: kind,
                    role: "worker",
                    cwd: &cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
    }
    let d = TestDaemon::start_on(state);

    let tmp = TempDir::new().unwrap();
    let (pm_dir, repo, home) = (
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("home"),
    );
    for dir in [&pm_dir, &repo, &home] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let git = |dir: &Path, args: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr)
        );
    };
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "x").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    let cli = |args: &[&str]| -> (bool, Value) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&d.state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        let text = if out.stdout.is_empty() {
            String::from_utf8_lossy(&out.stderr).to_string()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        (
            out.status.success(),
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
        )
    };
    assert!(cli(&["issue", "init"]).0);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s,]).0);
    for title in ["One", "Two"] {
        assert!(cli(&["issue", "new", title, "--project", "demo"]).0);
    }

    // Three reviewed memories on the project: one project-wide rule
    // (matches), one component-scoped gotcha (no component on the issue
    // -> no match), and one provider-scoped rule for a different provider.
    // They are written as authenticated reviewed fixtures because an
    // ordinary CLI child is deliberately not a memory authority.
    write_reviewed_memory(
        &pm_dir,
        "demo",
        "always-drain",
        "rule",
        Scope {
            project: true,
            ..Scope::default()
        },
        "always drain the pipe before send",
    );
    write_reviewed_memory(
        &pm_dir,
        "demo",
        "comp-only",
        "gotcha",
        Scope {
            components: vec!["daemon".to_string()],
            ..Scope::default()
        },
        "daemon-only gotcha",
    );
    write_reviewed_memory(
        &pm_dir,
        "demo",
        "claude-only",
        "rule",
        Scope {
            providers: vec!["claude".to_string()],
            ..Scope::default()
        },
        "claude provider rule",
    );
    // A still-proposed memory never injects. It intentionally has no
    // authenticated proof, so it also documents legacy/proposed withholding.
    let pending = pm_dir.join("demo/memory/pending-one.md");
    std::fs::write(
        pending,
        "---\nid: pending-one\ntype: rule\nstatus: proposed\nconfidence: medium\ncreated: 2026-01-01T00:00:00Z\nscope:\n  project: true\n---\nnot yet accepted\n\n**Why:** pending.\n\n**How to apply:** do not inject.\n",
    )
    .unwrap();

    let note = tmp.path().join("kickoff.md");
    std::fs::write(&note, "# kickoff").unwrap();
    let note_s = note.canonicalize().unwrap().to_str().unwrap().to_string();

    // The dispatch: exactly the project-wide rule lands.
    let (ok, out) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok && out["dispatched"] == true, "{out}");
    assert_eq!(out["lessons"], json!(["always-drain"]), "{out}");
    let lessons_path = PathBuf::from(out["lessons_file"].as_str().unwrap());
    let msg_id = out["message"].as_str().unwrap().to_string();
    assert_eq!(
        lessons_path,
        d.state
            .join("dispatch")
            .join(format!("{msg_id}-lessons.md"))
    );
    let text = std::fs::read_to_string(&lessons_path).unwrap();
    assert!(text.contains("always drain the pipe before send"), "{text}");
    assert!(!text.contains("daemon-only gotcha"), "{text}");
    assert!(text.len() <= 4096);
    // The kickoff names the file; the comment records the injection.
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let kick = &show["messages"].as_array().unwrap()[0];
    assert_eq!(kick["id"].as_str().unwrap(), msg_id);
    let body_s = kick["body"].as_str().unwrap();
    assert!(
        body_s.contains(&format!("Lessons: {}.", lessons_path.display())),
        "{body_s}"
    );
    let issue = cli(&["issue", "show", "D-1", "--json"]).1;
    assert!(
        issue["comments"].as_array().unwrap().iter().any(|c| {
            c["body"]
                .as_str()
                .unwrap()
                .contains("Lessons injected: always-drain")
        }),
        "{issue}"
    );

    // --no-lessons: nothing rendered, nothing appended.
    let (ok, out) = cli(&[
        "dispatch",
        "D-2",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
        "--no-lessons",
    ]);
    assert!(ok && out["dispatched"] == true, "{out}");
    assert_eq!(out["lessons"], json!([]), "{out}");
    assert_eq!(out["lessons_file"], Value::Null, "{out}");
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let kick2 = show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"].as_str() == out["message"].as_str())
        .unwrap();
    assert!(
        !kick2["body"].as_str().unwrap().contains("Lessons:"),
        "{}",
        kick2["body"]
    );
    assert_eq!(
        std::fs::read_dir(d.state.join("dispatch")).unwrap().count(),
        1
    );

    // The bootstrap briefing carries the project's accepted rules for
    // an agent whose cwd sits inside the project repo — proposed and
    // non-matching scopes stay out.
    d.rpc(
        "agent_register",
        json!({"alias": "w2", "provider": "fake", "endpoint_kind": "fake",
               "cwd": repo, "params": json!({"upstream": "pm"}).to_string()}),
    )
    .unwrap();
    d.wait_agent("w2", "idle", 15);
    let (ok, out) = cli(&["agent", "bootstrap", "w2"]);
    assert!(ok, "{out}");
    let briefing = d.state.join("briefings").join("pm").join("BRIEFING-w2.md");
    let text = std::fs::read_to_string(&briefing).unwrap();
    assert!(
        text.contains("## Project memory — accepted rules (demo)"),
        "{text}"
    );
    assert!(text.contains("always-drain"), "{text}");
    assert!(!text.contains("pending-one"), "{text}");
    assert!(!text.contains("claude-only"), "{text}");
}

/// Explicit-axis matching resolves the current project from cwd and never
/// searches sibling projects. An explicit `--project` remains available for
/// callers whose cwd is outside a registered repo.
#[test]
fn memory_match_explicit_axes_stay_in_current_project() {
    let tmp = TempDir::new().unwrap();
    let pm_dir = tmp.path().join("pm");
    let repo_a = tmp.path().join("repo-a");
    let repo_b = tmp.path().join("repo-b");
    let home = tmp.path().join("home");
    for dir in [&pm_dir, &repo_a, &repo_b, &home] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let git = |dir: &Path| {
        for args in [
            &["init", "-b", "main"][..],
            &["config", "user.email", "test@example.invalid"][..],
            &["config", "user.name", "test"][..],
        ] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {:?}: {:?}", args, out);
        }
    };
    git(&repo_a);
    git(&repo_b);
    let bin = Path::new(env!("CARGO_BIN_EXE_cadence"));
    let run = |cwd: &Path, args: &[&str]| -> (bool, Value) {
        let out = std::process::Command::new(bin)
            .arg("--state-dir")
            .arg(tmp.path().join("state"))
            .args(args)
            .current_dir(cwd)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin.parent().unwrap().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        let text = if out.stdout.is_empty() {
            String::from_utf8_lossy(&out.stderr).to_string()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        (
            out.status.success(),
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
        )
    };
    assert!(run(&repo_a, &["issue", "init"]).0);
    let repo_a_s = repo_a.to_str().unwrap();
    let repo_b_s = repo_b.to_str().unwrap();
    assert!(
        run(
            &repo_a,
            &[
                "issue",
                "project",
                "add",
                "alpha",
                "--prefix",
                "A",
                "--repo",
                repo_a_s,
                "--component",
                "daemon",
            ],
        )
        .0
    );
    assert!(
        run(
            &repo_a,
            &[
                "issue",
                "project",
                "add",
                "beta",
                "--prefix",
                "B",
                "--repo",
                repo_b_s,
                "--component",
                "daemon",
            ],
        )
        .0
    );
    // Both records carry a real acceptance quorum; the test is about
    // project resolution, so legacy accepted text must not be enough.
    for (project, id, fact) in [
        ("alpha", "alpha-daemon", "alpha fact"),
        ("beta", "beta-daemon", "beta fact"),
    ] {
        write_reviewed_memory(
            &pm_dir,
            project,
            id,
            "rule",
            Scope {
                components: vec!["daemon".to_string()],
                ..Scope::default()
            },
            fact,
        );
    }

    let (ok, out) = run(
        &repo_a,
        &["memory", "match", "--component", "daemon", "--json"],
    );
    assert!(ok, "{out}");
    assert_eq!(out["context"]["project"], "alpha", "{out}");
    assert_eq!(out["matched"].as_array().unwrap().len(), 1, "{out}");
    assert_eq!(out["matched"][0]["project"], "alpha", "{out}");
    assert_eq!(out["matched"][0]["slug"], "alpha-daemon", "{out}");
    assert_eq!(out["matched"][0]["fact"], "alpha fact", "{out}");

    let (ok, out) = run(
        &home,
        &[
            "memory",
            "match",
            "--project",
            "beta",
            "--component",
            "daemon",
            "--json",
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(out["context"]["project"], "beta", "{out}");
    assert_eq!(out["matched"].as_array().unwrap().len(), 1, "{out}");
    assert_eq!(out["matched"][0]["slug"], "beta-daemon", "{out}");
}

/// Memory failures degrade, never sink a dispatch: a malformed memory
/// file fails matching → no lessons + `lessons_error`; a `Lessons:`
/// suffix that pushes the kickoff body over the pty cap is dropped
/// (original body restored, no file written). Briefings bound their
/// rule section to ≤8 entries and 4 KiB.
#[test]
fn dispatch_degrades_on_memory_failures() {
    let seeded = TempDir::new().unwrap();
    let state = seeded.path().to_path_buf();
    {
        let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
        let cwd = state.to_str().unwrap().to_string();
        for (alias, params, kind) in [
            ("pm", None, "fake"),
            ("w1", Some("{\"upstream\":\"pm\"}"), "inbox"),
        ] {
            store
                .register_agent(&NewAgent {
                    alias,
                    provider: "fake",
                    endpoint_kind: kind,
                    role: "worker",
                    cwd: &cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
    }
    let d = TestDaemon::start_on(state);

    let tmp = TempDir::new().unwrap();
    let (pm_dir, repo, home) = (
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("home"),
    );
    for dir in [&pm_dir, &repo, &home] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let git = |dir: &Path, args: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {:?}", args);
    };
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "x").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    let cli = |args: &[&str]| -> (bool, Value) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&d.state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        let text = if out.stdout.is_empty() {
            String::from_utf8_lossy(&out.stderr).to_string()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        (
            out.status.success(),
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
        )
    };
    assert!(cli(&["issue", "init"]).0);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s]).0);
    // D-2's title is sized so the kickoff body sits just under the
    // 4000-byte cap — the `Lessons:` suffix is what tips it over.
    let long_title = "x".repeat(3720);
    for title in ["One".to_string(), long_title, "Three".to_string()] {
        assert!(cli(&["issue", "new", &title, "--project", "demo"]).0);
    }
    // One good reviewed rule — matching works until the broken file.
    write_reviewed_memory(
        &pm_dir,
        "demo",
        "good-rule",
        "rule",
        Scope {
            project: true,
            ..Scope::default()
        },
        "a good fact",
    );

    let note = tmp.path().join("kickoff.md");
    std::fs::write(&note, "# kickoff").unwrap();
    let note_s = note.canonicalize().unwrap().to_str().unwrap().to_string();

    // Malformed memory file → excluded from the match and named;
    // the valid rule still reaches the kickoff.
    std::fs::write(
        pm_dir.join("demo/memory/broken.md"),
        "---\nid: [unclosed\n---\nbody\n",
    )
    .unwrap();
    let (ok, out) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok && out["dispatched"] == true, "{out}");
    assert_eq!(out["lessons"], json!(["good-rule"]), "{out}");
    let lessons_file = out["lessons_file"].as_str().unwrap_or_default();
    assert!(lessons_file.ends_with("-lessons.md"), "{out}");
    let err = out["lessons_error"].as_str().unwrap_or_default();
    assert!(err.contains("broken.md"), "{out}");
    assert!(!out["message"].as_str().unwrap().is_empty());
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let kick = show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"].as_str() == out["message"].as_str())
        .unwrap();
    assert!(kick["body"].as_str().unwrap().contains("Lessons:"));
    assert!(Path::new(lessons_file).is_file());

    // Over-cap: the good rule matches, but the `Lessons:` suffix would
    // push the kickoff body past the 4000-byte pty cap → the suffix
    // and the file are dropped, the original body still sends.
    std::fs::remove_file(pm_dir.join("demo/memory/broken.md")).unwrap();
    let (ok, out) = cli(&[
        "dispatch",
        "D-2",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok && out["dispatched"] == true, "{out}");
    assert_eq!(out["lessons"], json!([]), "{out}");
    assert_eq!(out["lessons_file"], Value::Null, "{out}");
    let err = out["lessons_error"].as_str().unwrap_or_default();
    assert!(err.contains("body limit"), "{out}");
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let kick = show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"].as_str() == out["message"].as_str())
        .unwrap();
    let sent = kick["body"].as_str().unwrap();
    assert!(!sent.contains("Lessons:"), "suffix dropped: {sent}");
    assert!(
        sent.len() > 3900 && sent.len() <= 4000,
        "original long body sent: {}",
        sent.len()
    );
    // No new lessons file — D-1's remains the only one — and no
    // half-written .tmp residue.
    let names: Vec<String> = std::fs::read_dir(d.state.join("dispatch"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(
        names.iter().filter(|n| n.ends_with("-lessons.md")).count(),
        1,
        "{names:?}"
    );
    assert!(!names.iter().any(|n| n.ends_with(".tmp")), "{names:?}");

    // Unwritable lessons dir: `<state>/dispatch` as a plain file →
    // create_dir_all fails → dispatch still lands, the error is
    // named, and nothing that looks like a lessons artifact exists.
    std::fs::remove_dir_all(d.state.join("dispatch")).unwrap();
    std::fs::write(d.state.join("dispatch"), "not a dir").unwrap();
    let (ok, out) = cli(&[
        "dispatch",
        "D-3",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok && out["dispatched"] == true, "{out}");
    assert_eq!(out["lessons"], json!([]), "{out}");
    assert_eq!(out["lessons_file"], Value::Null, "{out}");
    let err = out["lessons_error"].as_str().unwrap_or_default();
    assert!(err.contains("unwritable"), "{out}");
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let kick = show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"].as_str() == out["message"].as_str())
        .unwrap();
    assert!(!kick["body"].as_str().unwrap().contains("Lessons:"));
    assert!(
        d.state.join("dispatch").is_file(),
        "the placeholder is untouched — no dir or file replaced it"
    );

    // Briefing cap: an oversized first rule is skipped, not a stop —
    // later smaller rules still list, ≤8 entries and ≤4 KiB hold,
    // and the omission is counted. fat-rule-00's hand-edited 5 KiB
    // fact alone exceeds the byte budget: under the old `break` it
    // hid every rule after it.
    // These are reviewed fixtures too: accepted legacy text is deliberately
    // withheld from dispatch, so the briefing-cap assertion must use the
    // same authenticated evidence shape as the ordinary dispatch fixtures.
    write_reviewed_memory(
        &pm_dir,
        "demo",
        "fat-rule-00",
        "rule",
        Scope {
            project: true,
            ..Scope::default()
        },
        &"z".repeat(5 * 1024),
    );
    write_reviewed_memory(
        &pm_dir,
        "demo",
        "fat-rule-01-tiny",
        "rule",
        Scope {
            project: true,
            ..Scope::default()
        },
        "t",
    );
    for i in 2..10 {
        write_reviewed_memory(
            &pm_dir,
            "demo",
            &format!("fat-rule-{i:02}"),
            "rule",
            Scope {
                project: true,
                ..Scope::default()
            },
            &"y".repeat(700),
        );
    }
    d.rpc(
        "agent_register",
        json!({"alias": "w2", "provider": "fake", "endpoint_kind": "fake",
               "cwd": repo, "params": json!({"upstream": "pm"}).to_string()}),
    )
    .unwrap();
    d.wait_agent("w2", "idle", 15);
    let (ok, out) = cli(&["agent", "bootstrap", "w2"]);
    assert!(ok, "{out}");
    let briefing = d.state.join("briefings").join("pm").join("BRIEFING-w2.md");
    let text = std::fs::read_to_string(&briefing).unwrap();
    let section = text.split("## Project memory").nth(1).unwrap_or_default();
    let items = section.split("\n\n`cadence memory match").next().unwrap();
    let listed = items.matches("- `fat-rule-").count();
    assert!((1..=8).contains(&listed), "{listed} rules in section");
    assert!(items.len() <= 4 * 1024 + 128, "{} bytes", items.len());
    // The oversized rule never listed; the tiny rule after it did —
    // proof the budget skip keeps scanning. The omission is counted.
    assert!(!items.contains("fat-rule-00`"), "{items}");
    assert!(items.contains("- `fat-rule-01-tiny`"), "{items}");
    assert!(items.contains("accepted rule(s) omitted"), "{items}");
}

// ==== operator IX: cadence status, daemon restart, events tail ====

/// `cadence status --json` against a daemon's socket — the JSON shape
/// is the contract; extra args (`--group`) and env (`CADENCE_PM_DIR`)
/// thread through.
fn status_json(state: &Path, extra: &[&str], envs: &[(&str, &Path)]) -> Value {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(state)
        .arg("status")
        .arg("--json")
        .args(extra)
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_PM_DIR");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "status output not json: {e}: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// `cadence status` table form — same invocation, no --json.
fn status_table(state: &Path, envs: &[(&str, &Path)]) -> String {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(state)
        .arg("status")
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_PM_DIR");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// `cadence issue …` with the tracker pointed at `pm`.
fn issue_cli(home: &Path, state: &Path, pm: &Path, args: &[&str]) {
    let bin = env!("CARGO_BIN_EXE_cadence");
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .env("HOME", home)
        .env("CADENCE_PM_DIR", pm)
        // The tracker's pre-commit hook runs `cadence` from PATH —
        // put the just-built binary first so a stale ambient install
        // can't answer `issue lint` (the board harness does the same).
        .env(
            "PATH",
            format!(
                "{}:{}",
                std::path::Path::new(bin).parent().unwrap().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "issue {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `capture-pane` calls recorded by the mock tmux — the probe's
/// signature invocation, counted for the one-probe-per-pane rule.
fn tmux_call_count(mock: &MockDevin, state: &Path, cmd: &str) -> usize {
    let log = mock
        .dir
        .join("tmux-state")
        .join(socket_for(state))
        .join("calls.log");
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with(cmd))
        .count()
}

#[test]
fn status_rows_probe_once_and_footer() {
    // The stall watch now samples idle panes too — park it far out so
    // a tick cannot land inside the capture-count window below.
    stall_sample(3600);
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    let _chatty = d.mock_claude("chatty", None);
    d.register_devin("dv1", None);
    d.register_devin("dv2", None);
    d.register_inbox("pm");
    d.register_claude("w1", json!({}));
    d.register("fx");
    d.wait_agent("dv1", "idle", 20);
    d.wait_agent("dv2", "idle", 20);
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 15);
    d.wait_agent("fx", "idle", 10);
    // dv2's pane shows the busy watermark — its row must read busy.
    std::fs::write(
        d.pane_file(&mock, "dv2", "tui-state"),
        "⠸ Thinking · 12s (esc twice to interrupt)\n❭ Guide Devin while it works\n",
    )
    .unwrap();
    // w1 mid-turn: chatty never completes, so m1 stays running.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "draft the migration plan", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("w1", "m1", &["running"], 15);
    // pm holds an undrained message — unread inbox in the footer.
    d.rpc(
        "agent_send",
        json!({"alias": "pm", "text": "note for the operator", "message": "pm1"}),
    )
    .unwrap();
    // fx is fenced: one unknown message, attention state.
    fence_agent(&d, "fx", "mf1");
    // Tracker: one doing and one review issue owned by agents, plus a
    // backlog issue that must NOT appear.
    let home = d.dir.path().join("home");
    let pm_dir = d.dir.path().join("pm");
    std::fs::create_dir_all(&home).unwrap();
    issue_cli(&home, &d.state, &pm_dir, &["issue", "init"]);
    issue_cli(
        &home,
        &d.state,
        &pm_dir,
        &["issue", "project", "add", "cadence", "--prefix", "CAD"],
    );
    for title in ["one", "two", "three"] {
        issue_cli(
            &home,
            &d.state,
            &pm_dir,
            &["issue", "new", title, "--project", "cadence"],
        );
    }
    issue_cli(
        &home,
        &d.state,
        &pm_dir,
        &["issue", "set", "CAD-1", "status=doing", "owner=w1"],
    );
    issue_cli(
        &home,
        &d.state,
        &pm_dir,
        &["issue", "set", "CAD-2", "status=review", "owner=dv1"],
    );
    issue_cli(
        &home,
        &d.state,
        &pm_dir,
        &["issue", "set", "CAD-3", "owner=w1"],
    );
    // The stall watch samples each new pty pane once at registration —
    // the interval only gates REPEATS, so `stall_sample(3600)` cannot
    // hold that first capture back. Under suite load the watch's first
    // tick can land this late; wait it out so the window below counts
    // only the status probes.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while tmux_call_count(&mock, &d.state, "capture-pane") < 2 {
        assert!(
            deadline.elapsed() < Duration::from_secs(20),
            "first stall samples for dv1/dv2 never landed"
        );
        thread::sleep(Duration::from_millis(100));
    }
    let captures_before = tmux_call_count(&mock, &d.state, "capture-pane");
    let view = status_json(&d.state, &[], &[("CADENCE_PM_DIR", &pm_dir)]);
    let agents = view["agents"].as_array().unwrap();
    let row = |alias: &str| {
        agents
            .iter()
            .find(|a| a["alias"].as_str() == Some(alias))
            .unwrap_or_else(|| panic!("no row for {alias}: {view}"))
            .clone()
    };
    // w1: running message with age + head, owned doing issue.
    let w1 = row("w1");
    assert_eq!(w1["running"]["text"], "draft the migration plan", "{w1}");
    assert!(w1["running"]["age_secs"].as_u64().is_some(), "{w1}");
    assert_eq!(w1["issues"], json!(["CAD-1"]), "{w1}");
    // dv1 idle pane verdict + review issue; dv2 busy verdict.
    let dv1 = row("dv1");
    assert_eq!(dv1["pane"]["verdict"], "idle", "{dv1}");
    assert_eq!(dv1["issues"], json!(["CAD-2"]), "{dv1}");
    let dv2 = row("dv2");
    assert!(
        dv2["pane"]["verdict"]
            .as_str()
            .unwrap_or("")
            .starts_with("busy"),
        "{dv2}"
    );
    // fx fenced: attention state, one unknown.
    let fx = row("fx");
    assert_eq!(fx["state"], "attention", "{fx}");
    assert_eq!(fx["unknown"], 1, "{fx}");
    // pm inbox: no probe, unread lands in the footer.
    let pm = row("pm");
    assert!(pm["pane"].is_null(), "{pm}");
    assert!(view["footer"]["unread_inboxes"]
        .as_array()
        .unwrap()
        .contains(&json!("pm")));
    assert_eq!(view["footer"]["states"]["idle"], 3, "{view}");
    assert_eq!(view["footer"]["states"]["busy"], 1, "{view}");
    assert_eq!(view["footer"]["states"]["attention"], 1, "{view}");
    // Exactly one capture-pane per pty agent with a pane — no probe
    // for the managed, fake or inbox rows.
    let captures_after = tmux_call_count(&mock, &d.state, "capture-pane");
    assert_eq!(
        captures_after - captures_before,
        2,
        "status must probe each pty pane exactly once"
    );
    // Table form renders the same rows.
    let table = status_table(&d.state, &[("CADENCE_PM_DIR", &pm_dir)]);
    assert!(table.contains("w1"), "{table}");
    assert!(table.contains("draft the migration plan"), "{table}");
    assert!(table.contains("busy:"), "{table}");
    assert!(table.contains("unread: pm"), "{table}");
    // --group scopes to the root + its upstream members.
    d.rpc("agent_set", json!({"alias": "w1", "patch": {}})).ok();
}

/// --group filters to the named root plus agents naming it upstream.
#[test]
fn status_group_scopes_rows() {
    let d = TestDaemon::start();
    d.register_inbox("pm");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "agent_register",
        json!({"alias": "w1", "provider": "fake", "endpoint_kind": "fake",
               "cwd": cwd, "params": "{\"upstream\":\"pm\"}"}),
    )
    .unwrap();
    d.register("other");
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    d.wait_agent("other", "idle", 10);
    let view = status_json(&d.state, &["--group", "pm"], &[]);
    let aliases: Vec<&str> = view["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["alias"].as_str())
        .collect();
    assert_eq!(aliases, vec!["pm", "w1"], "{view}");
}

/// Why a restarted daemon fenced `alias`: its recorded error and the
/// newest events, read from the child daemon that now owns the state.
fn restart_diag(d: &TestDaemon, alias: &str) -> String {
    let error = d
        .rpc("agent_show", json!({"alias": alias}))
        .map(|v| v["agent"]["error"].clone())
        .unwrap_or_default();
    let events = d
        .rpc("events", json!({"alias": alias, "after": 0}))
        .map(|v| v["events"].clone())
        .unwrap_or_default();
    let tail: Vec<&Value> = events
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .take(6)
        .collect();
    format!("{alias} error: {error}\nnewest events: {tail:?}")
}

/// `daemon restart` through the real binary: a thread-daemon seeded
/// with a pty pane hands the lock to a fresh detached daemon, which
/// re-adopts the pane — the table must say the pid is the same.
#[test]
fn daemon_restart_keeps_pane_pid_and_reports_table() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv", None);
    d.register("w1");
    d.wait_agent("dv", "idle", 20);
    d.wait_agent("w1", "idle", 15);
    let pane_pid_before = d.rpc("agent_show", json!({"alias": "dv"})).unwrap()["agent"]["pid"]
        .as_u64()
        .unwrap();
    assert!(pane_pid_before > 0);
    let home = TempDir::new().unwrap();
    let out = cadence_at(home.path(), &d.state, &["daemon", "restart"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "restart failed: {stdout} {}\n{}",
        String::from_utf8_lossy(&out.stderr),
        restart_diag(&d, "dv")
    );
    assert!(stdout.contains("AGENT"), "{stdout}");
    assert!(stdout.contains("dv"), "{stdout}");
    assert!(stdout.contains("same"), "{stdout}");
    // The new daemon owns the state and reports the same pane pid.
    let probe = d.rpc("agent_probe", json!({"alias": "dv"})).unwrap();
    assert_eq!(probe["idle"], true, "{probe}");
    let pane_pid_after = d.rpc("agent_show", json!({"alias": "dv"})).unwrap()["agent"]["pid"]
        .as_u64()
        .unwrap();
    assert_eq!(pane_pid_before, pane_pid_after);
    let stop = cadence_at(home.path(), &d.state, &["daemon", "stop"]);
    assert!(
        stop.status.success(),
        "stop after restart failed: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
}

/// --when-idle refuses to touch a busy fleet, then proceeds once the
/// pane clears.
#[test]
fn daemon_restart_when_idle_gates_and_proceeds() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv", None);
    d.wait_agent("dv", "idle", 20);
    std::fs::write(
        d.pane_file(&mock, "dv", "tui-state"),
        "⠸ Thinking · 12s (esc twice to interrupt)\n❭ Guide Devin while it works\n",
    )
    .unwrap();
    let home = TempDir::new().unwrap();
    // Busy pane → timeout exits non-zero and the daemon is untouched.
    let out = cadence_at(
        home.path(),
        &d.state,
        &["daemon", "restart", "--when-idle", "--timeout", "3"],
    );
    assert!(
        !out.status.success(),
        "when-idle restart ran on a busy pane: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        d.rpc("agent_probe", json!({"alias": "dv"})).is_ok(),
        "daemon must be untouched after a when-idle timeout"
    );
    // Pane goes idle — the same command now completes the restart.
    std::fs::remove_file(d.pane_file(&mock, "dv", "tui-state")).unwrap();
    let out = cadence_at(
        home.path(),
        &d.state,
        &["daemon", "restart", "--when-idle", "--timeout", "30"],
    );
    assert!(
        out.status.success(),
        "when-idle restart failed on an idle pane: {}\n{}",
        String::from_utf8_lossy(&out.stderr),
        restart_diag(&d, "dv")
    );
    let stop = cadence_at(home.path(), &d.state, &["daemon", "stop"]);
    assert!(stop.status.success());
}

/// `daemon stop` waits for the process to release the state-dir lock,
/// so `stop && start` no longer races the drain. Ten iterations — the
/// old code lost this race whenever the drain outlived a millisecond.
#[test]
fn daemon_stop_then_start_never_races_lock() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap().path().join("state");
    let start = cadence_at(home.path(), &state, &["daemon", "start"]);
    assert!(
        start.status.success(),
        "initial start: {}",
        String::from_utf8_lossy(&start.stderr)
    );
    for i in 0..10 {
        let stop = cadence_at(home.path(), &state, &["daemon", "stop"]);
        assert!(
            stop.status.success(),
            "stop #{i}: {}",
            String::from_utf8_lossy(&stop.stderr)
        );
        let start = cadence_at(home.path(), &state, &["daemon", "start"]);
        assert!(
            start.status.success(),
            "start #{i} raced the drain: {}",
            String::from_utf8_lossy(&start.stderr)
        );
    }
    let stop = cadence_at(home.path(), &state, &["daemon", "stop"]);
    assert!(stop.status.success());
}

/// Events: the default page is the newest 50 (oldest first inside
/// it) with the forward cursor; --after keeps forward paging.
#[test]
fn events_default_page_is_newest_with_continue_cursor() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    // >50 events: each completed send writes several lifecycle events.
    for i in 0..20 {
        let id = format!("m{i}");
        d.rpc(
            "agent_send",
            json!({"alias": "w1", "text": format!("task {i}"), "message": id}),
        )
        .unwrap();
        d.wait_message("w1", &id, &["completed"], 10);
    }
    assert!(d.events("w1").len() > 50, "need >50 events to page");
    // RPC tail: 50 rows, ascending, ending at the latest seq.
    let page = d
        .rpc("agent_events", json!({"alias": "w1", "tail": true}))
        .unwrap();
    let events = page["events"].as_array().unwrap();
    assert_eq!(events.len(), 50, "{page}");
    assert_eq!(page["has_older"], true, "{page}");
    let seqs: Vec<i64> = events.iter().map(|e| e["seq"].as_i64().unwrap()).collect();
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "tail page not ascending"
    );
    let cursor = page["cursor"].as_i64().unwrap();
    assert_eq!(cursor, *seqs.last().unwrap());
    // Continuing forward from the cursor yields only newer events:
    // one more send lands strictly above it.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "epilogue", "message": "ep"}),
    )
    .unwrap();
    d.wait_message("w1", "ep", &["completed"], 10);
    let next = d
        .rpc("agent_events", json!({"alias": "w1", "after": cursor}))
        .unwrap();
    let next_seqs: Vec<i64> = next["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["seq"].as_i64().unwrap())
        .collect();
    assert!(!next_seqs.is_empty(), "post-cursor events missing");
    assert!(next_seqs.iter().all(|s| *s > cursor), "{next_seqs:?}");
    // CLI default page = the same tail.
    let home = TempDir::new().unwrap();
    let out = cadence_at(home.path(), &d.state, &["events", "w1"]);
    assert!(out.status.success());
    let cli_page: Value = serde_json::from_slice(&out.stdout).unwrap();
    let cli_events = cli_page["events"].as_array().unwrap();
    assert_eq!(cli_events.len(), 50);
    assert_eq!(
        cli_page["cursor"].as_i64().unwrap(),
        cli_events.last().unwrap()["seq"].as_i64().unwrap()
    );
    assert_eq!(cli_page["has_older"], true);
    // --after 0 keeps today's forward paging — the whole log.
    let out = cadence_at(home.path(), &d.state, &["events", "w1", "--after", "0"]);
    let full: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        full["events"].as_array().unwrap().len(),
        d.events("w1").len()
    );
    // The job view takes the same default page.
    let spec = d.dir.path().join("spec.md");
    std::fs::write(&spec, "spec").unwrap();
    d.register_inbox("pmj");
    d.job_new("pmj", "j1", spec.to_str().unwrap(), "a".repeat(40).as_str());
    let job_page = d
        .rpc("job_events", json!({"job": "j1", "tail": true}))
        .unwrap();
    assert!(
        !job_page["events"].as_array().unwrap().is_empty(),
        "job tail page must show the newest job events"
    );
    assert!(job_page.get("has_older").is_some(), "{job_page}");
}

/// `--follow` anchors at the tail: the first page is the newest 50,
/// then new events stream — it must not replay the whole log first.
#[test]
fn events_follow_starts_at_tail() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    for i in 0..15 {
        let id = format!("m{i}");
        d.rpc(
            "agent_send",
            json!({"alias": "w1", "text": format!("task {i}"), "message": id}),
        )
        .unwrap();
        d.wait_message("w1", &id, &["completed"], 10);
    }
    let total = d.events("w1").len();
    assert!(total > 50, "need >50 events, got {total}");
    let home = TempDir::new().unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["events", "w1", "--follow"])
        .env("HOME", home.path())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Drain stdout concurrently — the tail page alone can exceed the
    // pipe capacity, and a blocked writer means no stream at all.
    let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let sink = std::sync::Arc::clone(&captured);
    let mut stdout = child.stdout.take().unwrap();
    let reader = thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 8192];
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });
    // Wait for the tail page to land, then generate one more event and
    // wait for it to stream through a following page.
    let deadline = Instant::now() + Duration::from_secs(20);
    while !captured.lock().unwrap().contains("\"has_older\"") {
        assert!(Instant::now() < deadline, "tail page never printed");
        thread::sleep(Duration::from_millis(50));
    }
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "post-follow", "message": "mf"}),
    )
    .unwrap();
    d.wait_message("w1", "mf", &["completed"], 10);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !captured.lock().unwrap().contains("\"message\": \"mf\"") {
        assert!(
            Instant::now() < deadline,
            "post-follow event never streamed"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    let text = captured.lock().unwrap().clone();
    // Pages are pretty-printed JSON back to back — a `{` at column 0
    // starts each page.
    let mut pages: Vec<Value> = Vec::new();
    let mut depth = 0i32;
    let mut start = None;
    let mut in_str = false;
    let mut esc = false;
    for (i, c) in text.char_indices() {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start.take() {
                        if let Ok(v) = serde_json::from_str::<Value>(&text[s..=i]) {
                            pages.push(v);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    assert!(
        pages.len() >= 2,
        "follow emitted {} page(s): {text}",
        pages.len()
    );
    // First page = the tail: newest 50, not the whole log.
    let first = &pages[0];
    assert_eq!(first["events"].as_array().unwrap().len(), 50, "{first}");
    assert_eq!(first["has_older"], true, "{first}");
    // A later page carries the post-follow event.
    let streamed = pages[1..]
        .iter()
        .flat_map(|p| p["events"].as_array().unwrap().clone())
        .any(|e| e["payload"]["message"].as_str() == Some("mf"));
    assert!(streamed, "no post-follow event streamed: {text}");
}

// ---------------------------------------------------------------------------
// `cadence review` — the mechanical review routine against a temp repo
// with a fake `gh` and trivial gate commands.
// ---------------------------------------------------------------------------

fn review_git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn review_git_sha(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A fixture repo: `origin` (bare) + `repo` (clone) whose main moved
/// after the PR branches cut, four open PR heads under refs/pull/N/head
/// (7 = clean merge + a new wait-test, 8 = pairwise conflict with 7,
/// 9 = clean, 10 = merge conflict with main), a `cadence-review.toml`
/// driving trivial commands, and a fake `gh` answering from fixtures.
struct ReviewFixture {
    repo: PathBuf,
    state: PathBuf,
    fakebin: PathBuf,
    fakedir: PathBuf,
    gate_log: PathBuf,
    suite_ran: PathBuf,
    suite_lock: PathBuf,
    head7: String,
}

fn review_fixture(base: &Path) -> ReviewFixture {
    let origin = base.join("origin.git");
    let repo = base.join("repo");
    let state = base.join("state");
    let fakedir = base.join("gh-fixtures");
    let fakebin = base.join("bin");
    std::fs::create_dir_all(&fakedir).unwrap();
    std::fs::create_dir_all(&fakebin).unwrap();
    std::fs::create_dir_all(&state).unwrap();

    review_git(base, &["init", "-q", "--bare", &origin.to_string_lossy()]);
    review_git(
        base,
        &[
            "clone",
            "-q",
            &origin.to_string_lossy(),
            &repo.to_string_lossy(),
        ],
    );
    review_git(&repo, &["config", "user.email", "t@t"]);
    review_git(&repo, &["config", "user.name", "t"]);
    review_git(&repo, &["checkout", "-qb", "main"]);

    let put = |rel: &str, text: &str| {
        let p = repo.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };

    put("marker.txt", "a");
    put("shared.txt", "a");
    put("shared2.txt", "a");
    put("other.txt", "a");
    put(
        "tests/test_old.rs",
        "#[test]\nfn old_test() {}\n#[test]\nfn new_flaky() {}\n",
    );
    put(
        "cadence-review.toml",
        r#"prepare = ["echo prepared >> \"$GATE_LOG\""]
gates = [
    "echo gate1 >> \"$GATE_LOG\"",
    "echo gate2 >> \"$GATE_LOG\"",
    "sh gate_fail.sh",
    "echo gate4-never >> \"$GATE_LOG\"",
]
full_suite = "sh suite.sh"
test_globs = ["tests/**"]
test_command = "sh one_test.sh {test}"
stress_pattern = ["wait_"]
"#,
    );
    put(
        "gate_fail.sh",
        "echo gate3-output-line1\n\
         echo \"test new_flaky ... FAILED\"\n\
         echo \"test ghost_test ... FAILED\"\n\
         echo \"test bad;touch_pwn ... FAILED\"\n\
         echo \"\"\n\
         echo \"failures:\"\n\
         echo \"\"\n\
         echo \"    new_flaky\"\n\
         echo \"    ghost_test\"\n\
         echo \"    bad;touch_pwn\"\n\
         echo \"\"\n\
         echo \"test result: FAILED. 0 passed; 3 failed\"\n\
         exit 1\n",
    );
    put(
        "suite.sh",
        "touch \"$SUITE_RAN\"\necho \"test result: ok. 5 passed\"\nexit 0\n",
    );
    put(
        "one_test.sh",
        "case \"$1\" in\n\
         \x20 new_flaky) [ \"$(cat shared.txt)\" = \"a\" ] && exit 0 || exit 1 ;;\n\
         \x20 *) exit 0 ;;\n\
         esac\n",
    );
    review_git(&repo, &["add", "-A"]);
    review_git(&repo, &["commit", "-qm", "base A"]);
    let sha_a = review_git_sha(&repo, &["rev-parse", "HEAD"]);

    // PR 7: touches shared.txt and adds a test that waits — merges
    // cleanly with the moved base.
    review_git(&repo, &["checkout", "-qb", "pr-7", "main"]);
    put("shared.txt", "pr7");
    put(
        "tests/test_new.rs",
        "#[test]\nfn new_daemon_wait() {\n    let _ = wait_agent;\n}\n",
    );
    review_git(&repo, &["add", "-A"]);
    review_git(&repo, &["commit", "-qm", "pr7"]);
    let head7 = review_git_sha(&repo, &["rev-parse", "HEAD"]);
    review_git(&repo, &["push", "-q", "origin", "HEAD:refs/pull/7/head"]);

    // PR 8: same file, other content — pairwise conflict with PR 7.
    review_git(&repo, &["checkout", "-qb", "pr-8", "main"]);
    put("shared.txt", "pr8");
    review_git(&repo, &["commit", "-qam", "pr8"]);
    let head8 = review_git_sha(&repo, &["rev-parse", "HEAD"]);
    review_git(&repo, &["push", "-q", "origin", "HEAD:refs/pull/8/head"]);

    // PR 9: adds a file — clean against everything.
    review_git(&repo, &["checkout", "-qb", "pr-9", "main"]);
    put("extra9.txt", "nine");
    review_git(&repo, &["add", "-A"]);
    review_git(&repo, &["commit", "-qm", "pr9"]);
    let head9 = review_git_sha(&repo, &["rev-parse", "HEAD"]);
    review_git(&repo, &["push", "-q", "origin", "HEAD:refs/pull/9/head"]);

    // PR 10: touches shared2.txt, which main is about to change too —
    // a merge-result conflict. Also adds a wait-test.
    review_git(&repo, &["checkout", "-qb", "pr-10", "main"]);
    put("shared2.txt", "pr10");
    put(
        "tests/test_wait10.rs",
        "#[test]\nfn wait_thing() {\n    let _ = wait_agent;\n}\n",
    );
    review_git(&repo, &["add", "-A"]);
    review_git(&repo, &["commit", "-qm", "pr10"]);
    let head10 = review_git_sha(&repo, &["rev-parse", "HEAD"]);
    review_git(&repo, &["push", "-q", "origin", "HEAD:refs/pull/10/head"]);

    // main moves past every merge-base (commit B on disjoint files for
    // PR 7's clean merge; shared2.txt for PR 10's conflict).
    review_git(&repo, &["checkout", "-q", "main"]);
    put("other.txt", "b");
    put("shared2.txt", "b");
    review_git(&repo, &["commit", "-qam", "base B"]);
    review_git(&repo, &["push", "-q", "origin", "main"]);
    let _ = sha_a;

    // Fake gh + fixtures.
    let gh = fakebin.join("gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\n\
         case \"$1 $2\" in\n\
         \x20 \"pr view\") cat \"$FAKE_GH_DIR/pr-view-$3.json\" ;;\n\
         \x20 \"pr list\") cat \"$FAKE_GH_DIR/pr-list.json\" ;;\n\
         \x20 \"repo view\") echo \"o/r\" ;;\n\
         \x20 *) echo \"fake gh unhandled: $*\" >&2; exit 1 ;;\n\
         esac\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let view = |n: i64, sha: &str, branch: &str| {
        serde_json::json!({
            "number": n, "title": format!("PR {n}"),
            "url": format!("https://example/{n}"),
            "headRefName": branch, "headRefOid": sha,
            "baseRefName": "main",
            "files": [{"path": "shared.txt"}, {"path": "tests/test_new.rs"}],
            "state": "OPEN",
        })
    };
    std::fs::write(
        fakedir.join("pr-view-7.json"),
        serde_json::to_string(&view(7, &head7, "pr-7")).unwrap(),
    )
    .unwrap();
    std::fs::write(
        fakedir.join("pr-view-10.json"),
        serde_json::to_string(&view(10, &head10, "pr-10")).unwrap(),
    )
    .unwrap();
    std::fs::write(
        fakedir.join("pr-list.json"),
        serde_json::to_string(&serde_json::json!([
            {"number": 7, "title": "PR 7", "headRefOid": head7},
            {"number": 8, "title": "PR 8", "headRefOid": head8},
            {"number": 9, "title": "PR 9", "headRefOid": head9},
            {"number": 10, "title": "PR 10", "headRefOid": head10},
        ]))
        .unwrap(),
    )
    .unwrap();

    ReviewFixture {
        gate_log: base.join("gate.log"),
        suite_ran: base.join("suite.ran"),
        suite_lock: base.join("suite.lock"),
        repo,
        state,
        fakebin,
        fakedir,
        head7,
    }
}

fn review_cmd(f: &ReviewFixture) -> std::process::Command {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(&f.state)
        .arg("review")
        .arg("--repo")
        .arg("o/r")
        .current_dir(&f.repo)
        .env(
            "PATH",
            format!("{}:{}", f.fakebin.display(), std::env::var("PATH").unwrap()),
        )
        .env("FAKE_GH_DIR", &f.fakedir)
        .env("GATE_LOG", &f.gate_log)
        .env("SUITE_RAN", &f.suite_ran)
        .env("CADENCE_SUITE_LOCK", &f.suite_lock);
    cmd
}

fn review_report(f: &ReviewFixture, pr: i64) -> Value {
    let dir = f.state.join("reviews");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&format!("review-o_r-pr{pr}-")) && n.ends_with(".json"))
        .collect();
    names.sort();
    let last = names
        .last()
        .unwrap_or_else(|| panic!("no review report for pr {pr} in {}", dir.display()));
    serde_json::from_str(&std::fs::read_to_string(dir.join(last)).unwrap()).unwrap()
}

#[test]
fn review_verb_end_to_end() {
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    let out = review_cmd(&f).arg("7").output().unwrap();
    // blocked → exit 2
    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Gates ran in order; the failing gate stopped the sequence. The
    // second "prepared" is the base-head checkout prepared for the
    // equal-conditions compare.
    let log = std::fs::read_to_string(&f.gate_log).unwrap();
    assert_eq!(log, "prepared\ngate1\ngate2\nprepared\n", "{log}");
    // The full suite ran once.
    assert!(f.suite_ran.exists());
    // Both checkouts are gone afterwards.
    assert!(!f.repo.join(".cadence/wt/review-7").exists());
    assert!(!f.repo.join(".cadence/wt/review-7-base").exists());
    let wts = review_git_sha(&f.repo, &["worktree", "list"]);
    assert!(!wts.contains("review-7"), "{wts}");

    let r = review_report(&f, 7);
    assert_eq!(r["pr"], 7);
    assert_eq!(r["head"], json!(f.head7));
    assert_eq!(r["base"]["moved_since_merge_base"], true);
    assert_eq!(r["gated_tree"], json!("merge-result"));
    assert_eq!(r["merge"]["result"], json!("clean"));

    let gates = r["gates"].as_array().unwrap();
    let outcomes: Vec<&str> = gates
        .iter()
        .map(|g| g["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(outcomes, vec!["ok", "ok", "fail", "skipped"], "{gates:?}");
    // The failing gate's tail is kept.
    assert!(
        gates[2]["tail"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap_or("").contains("new_flaky")),
        "{:?}",
        gates[2]["tail"]
    );

    // New test detected and stressed 5 times.
    let stress = r["stress"].as_array().unwrap();
    assert_eq!(stress.len(), 1);
    assert_eq!(stress[0]["test"], json!("new_daemon_wait"));
    assert_eq!(stress[0]["runs"], 5);
    assert_eq!(stress[0]["failures"], 0);
    assert_eq!(stress[0]["detail"].as_array().unwrap().len(), 5);

    assert_eq!(r["full_suite"]["outcome"], json!("ok"));
    assert!(r["suite_lock"]["path"].is_string(), "{:?}", r["suite_lock"]);

    // Equal-conditions compare, sorted by name:
    // - `bad;touch_pwn` fails name validation — never executed,
    //   unknown on both sides → inconclusive.
    // - `ghost_test` cannot be located — never executed → inconclusive.
    // - `new_flaky` fails the gated tree, passes on the base →
    //   regression.
    let fails = r["failures"].as_array().unwrap();
    assert_eq!(fails.len(), 3, "{fails:?}");
    assert_eq!(fails[0]["test"], json!("bad;touch_pwn"));
    assert_eq!(fails[0]["verdict"], json!("inconclusive"));
    assert_eq!(
        fails[0]["isolated_gated"]["outcome"],
        json!("unknown"),
        "{fails:?}"
    );
    assert!(
        fails[0].get("cmd").is_none(),
        "invalid name must never reach a command: {fails:?}"
    );
    assert_eq!(fails[1]["test"], json!("ghost_test"));
    assert_eq!(fails[1]["verdict"], json!("inconclusive"));
    assert_eq!(fails[1]["isolated_base"]["outcome"], json!("unknown"));
    assert!(
        fails[1]["isolated_base"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("not found"),
        "{fails:?}"
    );
    assert_eq!(fails[2]["test"], json!("new_flaky"));
    assert_eq!(fails[2]["isolated_gated"]["outcome"], json!("fail"));
    assert_eq!(fails[2]["isolated_base"]["outcome"], json!("pass"));
    assert_eq!(fails[2]["verdict"], json!("regression"));
    // Base prepare ran and was recorded.
    let bp = r["base_prepare"].as_array().unwrap();
    assert_eq!(bp.len(), 1, "{bp:?}");
    assert_eq!(bp[0]["outcome"], json!("ok"));

    // Pairwise open-PR conflicts: PR 8 conflicts on shared.txt; PR 9
    // and PR 10 merge clean.
    let conflicts = r["open_pr_conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    assert_eq!(conflicts[0]["pr"], 8);
    assert_eq!(conflicts[0]["files"], json!(["shared.txt"]));

    assert_eq!(r["schema_migration"], false);
    // PR 7 leaves cadence-review.toml alone; the config came from the
    // base head.
    assert_eq!(
        r["config"],
        json!({"source": "base", "base_sha": r["base"]["sha"], "changed_by_pr": false})
    );
    assert_eq!(r["suggested_verdict"], json!("blocked"));
    assert!(r["report_md"].as_str().unwrap().ends_with(".md"));

    // The Markdown report exists and names the verdict.
    let md_path = PathBuf::from(r["report_md"].as_str().unwrap());
    let md = std::fs::read_to_string(&md_path).unwrap();
    assert!(md.contains("suggested verdict: blocked"), "{md}");
    assert!(md.contains("regression"), "{md}");
    assert!(md.contains("changed by this PR: **no**"), "{md}");
}

#[test]
fn review_verb_merge_conflict_blocks() {
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    let out = review_cmd(&f).arg("10").output().unwrap();
    // blocked → exit 2
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = review_report(&f, 10);
    assert_eq!(r["base"]["moved_since_merge_base"], true);
    assert_eq!(r["merge"]["result"], json!("conflict"));
    assert_eq!(r["merge"]["conflict_files"], json!(["shared2.txt"]));
    // The gates still ran, on the bare PR head.
    assert_eq!(r["gated_tree"], json!("pr-head"));
    let outcomes: Vec<&str> = r["gates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(outcomes, vec!["ok", "ok", "fail", "skipped"]);
    // Its wait-test was stressed too.
    assert_eq!(r["stress"][0]["test"], json!("wait_thing"));
    assert_eq!(r["suggested_verdict"], json!("blocked"));
    assert!(
        r["verdict_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.as_str().unwrap_or("").contains("does not merge")),
        "{:?}",
        r["verdict_reasons"]
    );
}

#[test]
fn review_verb_lock_refuses_a_second_run() {
    use std::os::unix::io::AsRawFd;
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    let reviews = f.state.join("reviews");
    std::fs::create_dir_all(&reviews).unwrap();
    let held = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(reviews.join("o_r.review.lock"))
        .unwrap();
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
    let out = review_cmd(&f).arg("7").output().unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("already running"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    drop(held);
}

#[test]
fn review_verb_suite_lock_serializes() {
    use std::os::unix::io::AsRawFd;
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    // Hold the suite slot: the run must wait on it before `full_suite`.
    let held = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&f.suite_lock)
        .unwrap();
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);

    let mut child = review_cmd(&f).arg("7").spawn().unwrap();
    // Everything before the suite takes well under 4s here; a still-
    // running child with no suite marker is waiting on the lock.
    std::thread::sleep(Duration::from_secs(4));
    assert!(
        child.try_wait().unwrap().is_none(),
        "review finished while the suite lock was held"
    );
    assert!(!f.suite_ran.exists(), "suite ran while its lock was held");
    drop(held);
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(f.suite_ran.exists());
}

#[test]
fn review_verb_refuses_to_adopt_an_existing_dir() {
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    let dir = f.repo.join(".cadence/wt/review-7");

    // A plain directory with an uncommitted file at the path — the
    // run refuses and leaves it byte-for-byte untouched.
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("precious.txt"), "keep me").unwrap();
    let out = review_cmd(&f).arg("7").output().unwrap();
    assert!(
        !out.status.success(),
        "expected refusal, got {:?}",
        out.status
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("already exists"), "{err}");
    assert!(err.contains("review-7"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.join("precious.txt")).unwrap(),
        "keep me"
    );

    // A foreign worktree at the same path — same refusal, still
    // registered and untouched afterwards.
    std::fs::remove_dir_all(&dir).unwrap();
    review_git(
        &f.repo,
        &[
            "worktree",
            "add",
            "--detach",
            &dir.to_string_lossy(),
            &f.head7,
        ],
    );
    std::fs::write(dir.join("precious.txt"), "keep me").unwrap();
    let out = review_cmd(&f).arg("7").output().unwrap();
    assert!(
        !out.status.success(),
        "expected refusal, got {:?}",
        out.status
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("already exists"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.join("precious.txt")).unwrap(),
        "keep me"
    );
    assert!(dir.join("marker.txt").exists(), "foreign checkout intact");
    let wts = review_git_sha(&f.repo, &["worktree", "list"]);
    assert!(wts.contains("review-7"), "{wts}");
    review_git(
        &f.repo,
        &["worktree", "remove", "--force", &dir.to_string_lossy()],
    );
}

#[test]
fn review_verb_base_prepare_failure_marks_inconclusive() {
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    // Prepare succeeds on the gated tree, fails on the base tree. The
    // config is read from the base head, so it lands on origin/main.
    std::fs::write(
        f.repo.join("cadence-review.toml"),
        r#"prepare = ["if [ \"$CADENCE_REVIEW_TREE\" = \"base\" ]; then echo base-prep-broke; exit 1; else echo prepared >> \"$GATE_LOG\"; fi"]
gates = ["echo gate1 >> \"$GATE_LOG\"", "sh gate_fail.sh"]
full_suite = "sh suite.sh"
test_globs = ["tests/**"]
test_command = "sh one_test.sh {test}"
stress_pattern = ["wait_"]
"#,
    )
    .unwrap();
    review_git(
        &f.repo,
        &["commit", "-qam", "base config: base prepare breaks"],
    );
    review_git(&f.repo, &["push", "-q", "origin", "main"]);
    let out = review_cmd(&f).arg("7").output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = review_report(&f, 7);
    // The base changed the config after PR 7 branched — that is not a
    // change by the PR.
    assert_eq!(
        r["config"]["changed_by_pr"],
        json!(false),
        "{:?}",
        r["config"]
    );
    // The failed prepare is recorded as its own step.
    let bp = r["base_prepare"].as_array().unwrap();
    assert_eq!(bp.len(), 1, "{bp:?}");
    assert_eq!(bp[0]["outcome"], json!("fail"));
    // Every comparison's base side is unknown — nothing laundered into
    // a fake "pre-existing".
    let fails = r["failures"].as_array().unwrap();
    assert_eq!(fails.len(), 3, "{fails:?}");
    for c in fails {
        assert_eq!(c["isolated_base"]["outcome"], json!("unknown"), "{c:?}");
        assert_eq!(c["verdict"], json!("inconclusive"), "{c:?}");
    }
    // new_flaky still ran on the gated tree and failed there.
    let nf = fails.iter().find(|c| c["test"] == "new_flaky").unwrap();
    assert_eq!(nf["isolated_gated"]["outcome"], json!("fail"));
    assert_eq!(r["suggested_verdict"], json!("blocked"));
}

#[test]
fn review_verb_gates_with_the_base_config_when_the_pr_rewrites_it() {
    let base = TempDir::new().unwrap();
    let f = review_fixture(base.path());
    // PR 11 rewrites its own gates to a no-op. The reviewer's checkout
    // stays on the PR branch, so the working tree holds the weakened
    // copy too — neither may be read.
    review_git(&f.repo, &["checkout", "-qb", "pr-11", "main"]);
    std::fs::write(
        f.repo.join("cadence-review.toml"),
        r#"prepare = []
gates = ["true"]
full_suite = "true"
test_globs = ["tests/**"]
test_command = "true {test}"
"#,
    )
    .unwrap();
    review_git(&f.repo, &["commit", "-qam", "pr11 weakens the gates"]);
    let head11 = review_git_sha(&f.repo, &["rev-parse", "HEAD"]);
    review_git(&f.repo, &["push", "-q", "origin", "HEAD:refs/pull/11/head"]);
    std::fs::write(
        f.fakedir.join("pr-view-11.json"),
        serde_json::to_string(&json!({
            "number": 11, "title": "PR 11", "url": "https://example/11",
            "headRefName": "pr-11", "headRefOid": head11,
            "baseRefName": "main",
            "files": [{"path": "cadence-review.toml"}],
            "state": "OPEN",
        }))
        .unwrap(),
    )
    .unwrap();

    let out = review_cmd(&f).args(["11", "--no-full"]).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // The base head's gates ran and its failing gate still failed.
    let log = std::fs::read_to_string(&f.gate_log).unwrap();
    assert!(log.starts_with("prepared\ngate1\ngate2\n"), "{log}");
    let r = review_report(&f, 11);
    let gates = r["gates"].as_array().unwrap();
    let cmds: Vec<&str> = gates.iter().map(|g| g["cmd"].as_str().unwrap()).collect();
    assert_eq!(cmds.len(), 4, "{gates:?}");
    assert_eq!(cmds[2], "sh gate_fail.sh");
    assert_eq!(gates[2]["outcome"], json!("fail"), "{gates:?}");
    assert_eq!(
        r["config"],
        json!({"source": "base", "base_sha": r["base"]["sha"], "changed_by_pr": true})
    );
    assert_ne!(r["suggested_verdict"], json!("pass"));
    assert!(
        r["verdict_reasons"].as_array().unwrap().iter().any(|s| s
            .as_str()
            .unwrap_or("")
            .contains("changes cadence-review.toml")),
        "{:?}",
        r["verdict_reasons"]
    );
    let md = std::fs::read_to_string(r["report_md"].as_str().unwrap()).unwrap();
    assert!(md.contains("changed by this PR: **yes**"), "{md}");
}
// ==== CAD-83: `cadence overview` — daemon-dependent rows ====

/// `cadence overview --json` against a scratch daemon's state dir;
/// `pm` binds a tracker dir via CADENCE_PM_DIR, `envs` add PATH etc.
fn overview_at(home: &Path, state: &Path, pm: Option<&Path>, envs: &[(&str, String)]) -> Value {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(state)
        .args(["overview", "--json"])
        .env("HOME", home)
        .env_remove("CADENCE_ALIAS");
    if let Some(pm) = pm {
        cmd.env("CADENCE_PM_DIR", pm);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "overview: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn git_at(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn overview_daemon_info_fenced_and_inbox_rows() {
    let d = TestDaemon::start();
    let home = TempDir::new().unwrap();

    // daemon_info carries the compiled-in build identity + start time.
    let info = d.rpc("daemon_info", json!({})).unwrap();
    assert_eq!(
        info["build_commit"].as_str().unwrap(),
        env!("CADENCE_BUILD_COMMIT")
    );
    assert_eq!(
        info["build_time"].as_str().unwrap(),
        env!("CADENCE_BUILD_TIME")
    );
    assert!(info["started_at"].as_f64().unwrap_or(0.0) > 0.0);

    // A fenced agent and unread inbox surface as needs-me rows with
    // their exact operator commands.
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    fence_agent(&d, "w1", "x1");
    d.register_inbox("pm");
    d.rpc(
        "agent_send",
        json!({"alias": "pm", "text": "ping", "message": "n1"}),
    )
    .unwrap();

    let view = overview_at(home.path(), &d.state, None, &[]);
    let needs = view["needs_me"].as_array().unwrap();
    let fenced = needs
        .iter()
        .find(|n| n["kind"] == "fenced")
        .expect("fenced row");
    assert_eq!(fenced["command"], "cadence agent unfence w1");
    let inbox = needs
        .iter()
        .find(|n| n["kind"] == "inbox_unread")
        .expect("inbox row");
    assert_eq!(inbox["command"], "cadence inbox pm");
    assert!(inbox["title"].as_str().unwrap().contains("pm"));
    // The daemon block carries identity through to the board payload.
    assert_eq!(view["daemon"]["reachable"], true);
    assert_eq!(
        view["daemon"]["build_commit"].as_str().unwrap(),
        env!("CADENCE_BUILD_COMMIT")
    );
    assert!(view["daemon"]["started_at"].as_f64().unwrap_or(0.0) > 0.0);
    // No tracker under the fake home → no repo can match the build.
    assert_eq!(view["drift"]["matched"], false);
}

#[test]
fn overview_approval_row_for_brokered_request() {
    let d = TestDaemon::start();
    let home = TempDir::new().unwrap();
    let _mock = d.mock_claude("permit", None);
    broker_command();
    d.register_inbox("pm");
    d.register_claude("w1", json!({"upstream": "pm", "broker_approvals": true}));
    d.wait_agent("w1", "idle", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "run ls", "message": "m1"}),
    )
    .unwrap();
    d.wait_agent("w1", "waiting_input", 15);
    let req = d.wait_request("w1", 15);
    let handle = req["request"].as_str().unwrap().to_string();

    let view = overview_at(home.path(), &d.state, None, &[]);
    let needs = view["needs_me"].as_array().unwrap();
    let approval = needs
        .iter()
        .find(|n| n["kind"] == "approval")
        .expect("approval row");
    assert_eq!(
        approval["command"],
        format!("cadence agent respond w1 --request {handle} --decision accept")
    );
    assert!(approval["title"].as_str().unwrap().contains("w1"));
}

#[test]
fn overview_drift_reports_commits_after_build() {
    // Needs the compiled-in repo identity — absent only when the crate
    // was built outside a git checkout.
    if env!("CADENCE_BUILD_REMOTE") == "unknown" || env!("CADENCE_BUILD_COMMIT") == "unknown" {
        return;
    }
    // The build root can vanish between build and test (a removed
    // worktree) — skip rather than fail on the missing clone source.
    if !Path::new(env!("CADENCE_BUILD_ROOT")).join(".git").exists() {
        return;
    }
    let d = TestDaemon::start();
    let home = TempDir::new().unwrap();
    let pm = TempDir::new().unwrap();

    // A clone of the build repo carrying the same remote string (the
    // remote match) but refs we control, so the drift count is exact.
    let clone = TempDir::new().unwrap();
    git_at(
        clone.path(),
        &["clone", "-q", env!("CADENCE_BUILD_ROOT"), "."],
    );
    git_at(
        clone.path(),
        &["remote", "set-url", "origin", env!("CADENCE_BUILD_REMOTE")],
    );
    // Drop every remote-tracking ref so local `main` is the default.
    let refs = git_at(
        clone.path(),
        &["for-each-ref", "--format=%(refname)", "refs/remotes"],
    );
    for r in refs.lines() {
        git_at(clone.path(), &["update-ref", "-d", r]);
    }
    git_at(
        clone.path(),
        &["checkout", "-qB", "main", env!("CADENCE_BUILD_COMMIT")],
    );
    git_at(clone.path(), &["config", "user.email", "t@t"]);
    git_at(clone.path(), &["config", "user.name", "t"]);
    for (i, msg) in ["drift one (#11)", "drift two", "drift three (#13)"]
        .iter()
        .enumerate()
    {
        std::fs::write(clone.path().join("f"), format!("{i}")).unwrap();
        git_at(clone.path(), &["add", "f"]);
        git_at(clone.path(), &["commit", "-qm", msg]);
    }

    // The tracker declares that clone as cadence's repo.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["issue", "init"])
        .env("HOME", home.path())
        .env("CADENCE_PM_DIR", pm.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let repo = clone.path().to_str().unwrap().to_string();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "issue", "project", "add", "cadence", "--prefix", "CAD", "--repo", &repo,
        ])
        .env("HOME", home.path())
        .env("CADENCE_PM_DIR", pm.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A stub `gh` keeps the GitHub section off the network entirely.
    let ghbin = TempDir::new().unwrap();
    let gh = ghbin.path().join("gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\nif [ \"$1\" = pr ]; then echo '[]'; else echo '{\"state\":\"success\"}'; fi\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!(
        "{}:{}",
        ghbin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let view = overview_at(home.path(), &d.state, Some(pm.path()), &[("PATH", path)]);
    let drift = &view["drift"];
    assert_eq!(drift["matched"], true, "{view}");
    assert_eq!(drift["project"], "cadence", "{view}");
    assert_eq!(drift["known"], true, "{view}");
    assert_eq!(drift["count"], 3, "{view}");
    let prs: Vec<Option<u64>> = drift["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["pr"].as_u64())
        .collect();
    assert_eq!(prs, vec![Some(13), None, Some(11)], "{view}");
    // All panes idle (no agents) → the drift row offers the restart.
    let needs = view["needs_me"].as_array().unwrap();
    let row = needs
        .iter()
        .find(|n| n["kind"] == "drift")
        .expect("drift row");
    assert_eq!(row["command"], "cadence daemon restart --when-idle --ui");
}

/// `doctor --host --json` on the real host: one object, the named
/// checks, each ok|warn|fail, exit code the worst level. What the host
/// measures is its own business — this only proves the surface runs
/// and reports honestly, never which level comes back.
#[test]
fn doctor_host_json_reports_all_checks() {
    let dir = TempDir::new().unwrap();
    let state = dir.path().join("state");
    let home = dir.path().join("home");
    let cwd = dir.path().join("nowhere");
    for d in [&state, &home, &cwd] {
        std::fs::create_dir_all(d).unwrap();
    }
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&state)
        .args(["doctor", "--host", "--json"])
        .env("HOME", &home)
        .env("CADENCE_PM_DIR", home.join("pm"))
        .current_dir(&cwd)
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    let report: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("doctor --host --json printed no JSON: {out:?}"));
    let names: Vec<&str> = report["checks"]
        .as_array()
        .expect("checks[]")
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "disk",
            "provider-state",
            "pipes",
            "memory",
            "processes",
            "sessions",
            "orphans",
            "temp-dirs",
            "task-targets",
            "worktrees",
            "load"
        ]
    );
    for c in report["checks"].as_array().unwrap() {
        assert!(matches!(c["level"].as_str(), Some("ok" | "warn" | "fail")));
        for k in ["value", "threshold", "detail", "remedy"] {
            assert!(c.get(k).is_some(), "check missing {k}: {c}");
        }
    }
    let worst = report["level"].as_str().unwrap();
    let expect = match worst {
        "fail" => 2,
        "warn" => 1,
        _ => 0,
    };
    assert_eq!(code, expect, "level {worst} should exit {expect}");
}

// ---------- CAD-95: shared cargo target dir ----------

/// pm + repo + home + state under one temp dir, a `cli` that runs the
/// binary with the test env, and a `cli_at` that also sets cwd (the
/// doctor checks scan the repo it is launched from).
struct SharedTarget {
    _tmp: TempDir,
    pm_dir: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    state: PathBuf,
    bin_dir: PathBuf,
}

impl SharedTarget {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let (pm_dir, repo, home, state) = (
            tmp.path().join("pm"),
            tmp.path().join("repo"),
            tmp.path().join("home"),
            tmp.path().join("state"),
        );
        for dir in [&pm_dir, &repo, &home, &state] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let git = |args: &[&str]| {
            let o = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(
                o.status.success(),
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&o.stderr)
            );
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(repo.join("f"), "x").unwrap();
        // A real repo ignores its build output — `target/` must not
        // read as dirty for `git status` or `issue finish`.
        std::fs::write(repo.join(".gitignore"), "/target\n").unwrap();
        // A tiny standalone bin crate so tests can build real per-lane
        // binaries in the worktrees.
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"marker\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
            .parent()
            .unwrap()
            .to_path_buf();
        let s = Self {
            _tmp: tmp,
            pm_dir,
            repo,
            home,
            state,
            bin_dir,
        };
        assert!(s.cli(&["issue", "init"]).0);
        let repo_s = s.repo.canonicalize().unwrap().to_str().unwrap().to_string();
        assert!(
            s.cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s])
                .0
        );
        s
    }

    fn cli_at(&self, cwd: &Path, args: &[&str]) -> (i32, String, String) {
        self.cli_at_env(cwd, args, &[])
    }

    fn cli_at_env(&self, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
        cmd.arg("--state-dir")
            .arg(&self.state)
            .args(args)
            .env("CADENCE_PM_DIR", &self.pm_dir)
            .env("HOME", &self.home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .current_dir(cwd);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    fn cli(&self, args: &[&str]) -> (bool, Value) {
        let (code, stdout, stderr) = self.cli_at(&self.repo, args);
        let text = if stdout.is_empty() { stderr } else { stdout };
        (
            code == 0,
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
        )
    }

    fn set_build_target_dir(&self, value: &str) {
        let yaml_path = self.pm_dir.join("demo/project.yaml");
        let yaml = std::fs::read_to_string(&yaml_path).unwrap();
        // Drop any prior appended `build:` block before adding ours —
        // serde rejects a duplicate field.
        let kept: Vec<&str> = yaml
            .lines()
            .filter(|l| *l != "build:" && !l.starts_with("  target_dir:"))
            .collect();
        std::fs::write(
            &yaml_path,
            format!("{}\nbuild:\n  target_dir: {value}\n", kept.join("\n")),
        )
        .unwrap();
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.pm_dir)
            .args(["add", "-A"])
            .output()
            .unwrap();
        assert!(o.status.success());
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.pm_dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "cfg",
            ])
            .output()
            .unwrap();
        assert!(o.status.success());
    }

    fn new_issue(&self, title: &str) {
        assert!(self.cli(&["issue", "new", title, "--project", "demo"]).0);
    }

    fn worktree_of(&self, id: &str) -> PathBuf {
        let show = self.cli(&["issue", "show", id, "--json"]).1;
        show["refs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["kind"] == "worktree")
            .and_then(|r| r["path"].as_str())
            .map(PathBuf::from)
            .expect("worktree ref")
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    /// `cargo build` the fixture's `marker` crate inside `wt`.
    fn cargo_build(&self, wt: &Path) {
        let o = std::process::Command::new("cargo")
            .arg("build")
            .arg("--quiet")
            .current_dir(wt)
            .env_remove("CARGO_TARGET_DIR")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "cargo build: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    /// Write the marker crate's source so the built binary prints
    /// `marker` — each lane carries a distinct build.
    fn set_marker(&self, wt: &Path, marker: &str) {
        std::fs::write(
            wt.join("src/main.rs"),
            format!("fn main() {{ println!(\"{marker}\"); }}\n"),
        )
        .unwrap();
    }
}

/// The default: `issue start` links the worktree's hashed-content
/// cargo subdirs into `<repo>/.cadence/target/shared/debug` while
/// keeping the lane's own `target/debug` real — uplifted binaries are
/// per-lane. The effective dir is recorded on the worktree ref and
/// the tree stays clean for `git status`.
#[test]
fn issue_start_links_shared_cargo_deps() {
    let s = SharedTarget::new();
    s.new_issue("Shared");
    let (ok, out) = s.cli(&["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let wt = s.worktree_of("D-1");
    let shared_debug = s.repo.join(".cadence/target/shared/debug");
    for name in ["deps", ".fingerprint", "build", "incremental"] {
        let link = wt.join("target/debug").join(name);
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            shared_debug.join(name),
            "{name}"
        );
    }
    // `examples` is NOT shared — cargo uplifts example binaries to
    // `debug/examples/<name>` unhashed, so a shared dir would hand one
    // lane another lane's example.
    assert!(!wt.join("target/debug/examples").is_symlink());
    for name in [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"] {
        let link = wt.join("target/debug").join(name);
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            shared_debug.join(name),
            "{name}"
        );
    }
    // `debug/` itself is real — no `.cargo/` is written anywhere.
    assert!(!wt.join("target/debug").is_symlink());
    assert!(!wt.join(".cargo").exists());
    assert_eq!(
        out["target_dir"].as_str().unwrap(),
        wt.join("target").to_string_lossy(),
        "{out}"
    );
    assert_eq!(s.git(&wt, &["status", "--porcelain"]), "");
    // The worktree ref records the effective target dir.
    let show = s.cli(&["issue", "show", "D-1", "--json"]).1;
    let wt_ref = show["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "worktree")
        .unwrap();
    assert_eq!(
        wt_ref["cargo_target"].as_str().unwrap(),
        wt.join("target").to_string_lossy()
    );
    assert!(show["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "branch")
        .unwrap()["cargo_target"]
        .is_null());
    // Re-start is idempotent — same farm, same ref, no second commit.
    let (ok, out) = s.cli(&["issue", "start", "D-1"]);
    assert!(ok && out["created"] == false, "{out}");
    assert_eq!(s.git(&wt, &["status", "--porcelain"]), "");
    // Re-attach: remove the dir, keep refs — start re-plants the farm.
    s.git(
        &s.repo,
        &["worktree", "remove", "--force", &wt.to_string_lossy()],
    );
    let (ok, out) = s.cli(&["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    assert!(wt.join("target/debug/deps").is_symlink());
}

/// Two lanes sharing the dep cache never share the uplifted binary:
/// each lane's `target/debug/marker` is its own file, so a lane's
/// `cargo test` execs its own code. This is the CAD-95 r2 acceptance
/// case — a shared `build.target-dir` would hand lane A lane B's
/// binary.
#[test]
fn lanes_share_deps_but_not_the_uplifted_binary() {
    let s = SharedTarget::new();
    s.new_issue("LaneA");
    s.new_issue("LaneB");
    assert!(s.cli(&["issue", "start", "D-1"]).0);
    assert!(s.cli(&["issue", "start", "D-2"]).0);
    let (wt_a, wt_b) = (s.worktree_of("D-1"), s.worktree_of("D-2"));
    // Both lanes' hashed subdirs point into the one shared cache.
    let shared_debug = s.repo.join(".cadence/target/shared/debug");
    for wt in [&wt_a, &wt_b] {
        assert_eq!(
            std::fs::read_link(wt.join("target/debug/deps")).unwrap(),
            shared_debug.join("deps")
        );
    }
    // Lane A builds "lane-A"; lane B overwrites nothing of A's when
    // it builds "lane-B".
    s.set_marker(&wt_a, "lane-A");
    s.cargo_build(&wt_a);
    s.set_marker(&wt_b, "lane-B");
    s.cargo_build(&wt_b);
    for (wt, want) in [(&wt_a, "lane-A"), (&wt_b, "lane-B")] {
        let bin = wt.join("target/debug/marker");
        let out = std::process::Command::new(&bin).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), want);
    }
    // Dep artifacts landed in the shared cache through the links.
    let deps: Vec<String> = std::fs::read_dir(shared_debug.join("deps"))
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    assert!(deps.iter().any(|d| d.starts_with("marker-")), "{deps:?}");
}

/// A tracked `.cargo/config.toml` — the file a project ships — is
/// never written, reserialized or excluded by `issue start`, and the
/// clean tree it leaves is exactly what `issue finish` checks.
#[test]
fn issue_start_never_touches_tracked_cargo_config() {
    let s = SharedTarget::new();
    // A tracked config with real settings — target-dir is absent, so
    // the effective dir stays `<wt>/target` and the farm plants.
    let conf = "[build]\njobs = 2\n\n[target.'cfg(target_os = \"linux\")']\nrustflags = [\"-C\", \"link-arg=-Wl,-rpath,/x\"]\n";
    std::fs::create_dir_all(s.repo.join(".cargo")).unwrap();
    std::fs::write(s.repo.join(".cargo/config.toml"), conf).unwrap();
    s.git(&s.repo, &["add", "-A"]);
    s.git(&s.repo, &["commit", "-qm", "cargo config"]);
    s.new_issue("Cfg");
    let (ok, _) = s.cli(&["issue", "start", "D-1"]);
    assert!(ok);
    let wt = s.worktree_of("D-1");
    assert_eq!(
        std::fs::read_to_string(wt.join(".cargo/config.toml")).unwrap(),
        conf,
        "tracked config rewritten"
    );
    assert!(wt.join("target/debug/deps").is_symlink());
    assert_eq!(s.git(&wt, &["status", "--porcelain"]), "");
    // And a lane whose tree is genuinely clean must not be refused as
    // dirty by finish's guard — the pre-r2 rewrite left it dirty.
    let (ok, _) = s.cli(&["issue", "set", "D-1", "owner="]);
    assert!(ok);
    std::fs::write(wt.join("work.txt"), "x").unwrap();
    s.git(&wt, &["add", "-A"]);
    s.git(&wt, &["commit", "-qm", "work"]);
    s.git(&s.repo, &["merge", "-q", "cadence/d-1-cfg"]);
    let (ok, out) = s.cli(&["issue", "finish", "D-1"]);
    assert!(ok && out["finished"] == true, "{out}");
}

/// `[build] target_dir = "per-worktree"` in project.yaml opts a lane
/// back onto its own `target/`; anything else is a rejected config.
#[test]
fn issue_start_per_worktree_and_invalid_target_dir() {
    let s = SharedTarget::new();
    s.new_issue("Lane");
    s.new_issue("Bad");
    s.set_build_target_dir("per-worktree");
    let (ok, out) = s.cli(&["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let wt = s.worktree_of("D-1");
    assert_eq!(
        out["target_dir"].as_str().unwrap(),
        wt.join("target").to_string_lossy()
    );
    // No farm, no config — the lane is fully private.
    assert!(!wt.join("target").exists());
    assert!(!wt.join(".cargo").exists());

    s.set_build_target_dir("bogus");
    let (ok, err) = s.cli(&["issue", "start", "D-2"]);
    assert!(!ok, "{err}");
    assert!(err["error"].as_str().unwrap().contains("bogus"), "{err}");
}

/// `issue finish` removes the worktree but never the shared cache —
/// the ref's `cargo_target` is reported with a literal `exists` check.
#[test]
fn issue_finish_keeps_shared_cargo_target() {
    let s = SharedTarget::new();
    s.new_issue("Done");
    let shared = s.repo.join(".cadence/target/shared");
    let (ok, _) = s.cli(&["issue", "start", "D-1"]);
    assert!(ok);
    std::fs::write(shared.join("dep.rlib"), "cached").unwrap();
    // Ownerless — no daemon in this fixture, and finish's owner check
    // only runs when an owner is recorded.
    let (ok, _) = s.cli(&["issue", "set", "D-1", "owner="]);
    assert!(ok);
    let wt = s.worktree_of("D-1");
    std::fs::write(wt.join("work.txt"), "x").unwrap();
    s.git(&wt, &["add", "-A"]);
    s.git(&wt, &["commit", "-qm", "work"]);
    s.git(&s.repo, &["merge", "-q", "cadence/d-1-done"]);
    let (ok, out) = s.cli(&["issue", "finish", "D-1"]);
    assert!(ok && out["finished"] == true, "{out}");
    // The lane's own target/ went with it — `cargo_target_exists` is
    // the literal post-removal check, and the shared cache the lane
    // linked into is still there (rm unlinks, never follows).
    assert_eq!(
        out["cargo_target"].as_str().unwrap(),
        wt.join("target").to_string_lossy()
    );
    assert_eq!(out["cargo_target_exists"], false, "{out}");
    assert!(shared.join("dep.rlib").is_file());
    assert!(!wt.exists());
}

/// `doctor --host` counts the shared cache once at repo level, and
/// `--reclaim-plan` lists candidates — stale lanes, per-lane targets,
/// the shared cache — without deleting anything.
#[test]
fn doctor_host_shared_target_and_reclaim_plan() {
    let s = SharedTarget::new();
    s.new_issue("One");
    s.new_issue("Two");
    let (ok, _) = s.cli(&["issue", "start", "D-1"]);
    assert!(ok);
    let (ok, _) = s.cli(&["issue", "start", "D-2"]);
    assert!(ok);
    let shared = s.repo.join(".cadence/target/shared");
    std::fs::create_dir_all(shared.join("debug")).unwrap();
    std::fs::write(shared.join("debug/dep.rlib"), vec![0_u8; 4096]).unwrap();
    // A per-lane target dir on D-1's worktree — the pre-CAD-95 layout.
    // A commit past base keeps the lane live: a stale lane's whole
    // dir is freed by its own row and emits no informational
    // worktree-target row.
    let wt1 = s.worktree_of("D-1");
    std::fs::write(wt1.join("wip.txt"), "x").unwrap();
    s.git(&wt1, &["add", "-A"]);
    s.git(
        &wt1,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "wip",
        ],
    );
    std::fs::create_dir_all(wt1.join("target/debug")).unwrap();
    std::fs::write(wt1.join("target/debug/dep.rlib"), vec![0_u8; 2048]).unwrap();

    let (_code, stdout, _) = s.cli_at(&s.repo, &["doctor", "--host", "--json"]);
    let report: Value = serde_json::from_str(stdout.trim()).unwrap();
    let wt_check = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "worktrees")
        .unwrap();
    assert_eq!(
        wt_check["value"]["shared_cargo_target"]["path"]
            .as_str()
            .unwrap(),
        shared.to_string_lossy(),
        "{wt_check}"
    );
    assert!(
        wt_check["value"]["shared_cargo_target"]["bytes"]
            .as_u64()
            .unwrap()
            >= 4096
    );

    let (code, stdout, _) = s.cli_at(&s.repo, &["doctor", "--host", "--reclaim-plan", "--json"]);
    // The plan is the full report plus a `reclaim` section — the exit
    // code is the worst check level, matching the plain report's.
    let expected = match report["level"].as_str().unwrap() {
        "fail" => 2,
        "warn" => 1,
        _ => 0,
    };
    assert_eq!(code, expected, "{stdout}");
    let merged: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(
        merged["checks"].as_array().unwrap().len() == report["checks"].as_array().unwrap().len()
    );
    let plan = &merged["reclaim"];
    let kinds: Vec<&str> = plan["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"worktree-target") && kinds.contains(&"shared-cargo-cache"),
        "{kinds:?}"
    );
    // Nothing deleted — both dirs still present.
    assert!(wt1.join("target/debug/dep.rlib").is_file());
    assert!(shared.join("debug/dep.rlib").is_file());
    // `--reclaim-plan` without `--host` is a usage error.
    let (code, _, _) = s.cli_at(&s.repo, &["doctor", "--reclaim-plan"]);
    assert_ne!(code, 0);
}

/// The r3 acceptance case: the emitted shared-cache command is run
/// through a real `sh` — it must empty the shared subdirs while
/// leaving the directories themselves in place, so every lane's
/// symlinks keep resolving and the lane still builds afterwards.
/// (The r2 command deleted the dirs outright; every lane then died
/// with `File exists (os error 17)` on its next build.)
#[test]
fn reclaim_plan_command_keeps_lanes_buildable() {
    let s = SharedTarget::new();
    s.new_issue("Warm");
    let (ok, _) = s.cli(&["issue", "start", "D-1"]);
    assert!(ok);
    let wt = s.worktree_of("D-1");
    s.set_marker(&wt, "warm");
    s.cargo_build(&wt);
    let shared_debug = s.repo.join(".cadence/target/shared/debug");
    // Deps really landed in the cache through the links.
    assert!(std::fs::read_dir(shared_debug.join("deps"))
        .unwrap()
        .next()
        .is_some());

    let (_code, stdout, _) = s.cli_at(&s.repo, &["doctor", "--host", "--reclaim-plan", "--json"]);
    let report: Value = serde_json::from_str(stdout.trim()).unwrap();
    let row = report["reclaim"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "shared-cargo-cache")
        .expect("shared row");
    let action = row["action"].as_str().unwrap().to_string();
    assert!(action.contains("rm -rf"), "{action}");
    // Run exactly what the plan prints — comment and all — via `sh`.
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(&action)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{action}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The symlink targets must still exist — dangling links are the
    // bug this round fixes.
    for name in ["deps", ".fingerprint", "build", "incremental"] {
        let dir = shared_debug.join(name);
        assert!(dir.is_dir(), "{name} deleted by the reclaim command");
        assert!(
            std::fs::read_dir(&dir).unwrap().next().is_none(),
            "{name} should be emptied"
        );
        // And the lane's link resolves to a real dir, not dangling.
        assert!(
            wt.join("target/debug").join(name).is_dir(),
            "{name} dangles"
        );
    }
    // The lane still builds — cargo recreates what it needs inside
    // the surviving dirs.
    s.cargo_build(&wt);
    let bin = wt.join("target/debug/marker");
    let out = std::process::Command::new(&bin).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "warm");
}

/// `CARGO_TARGET_DIR` outranks every config file — with it exported,
/// a planted farm would sit inert while every lane collided in the
/// env dir, so `issue start` plants nothing and records the env's
/// dir on the worktree ref.
#[test]
fn issue_start_honours_cargo_target_dir_env() {
    let s = SharedTarget::new();
    s.new_issue("Env");
    let envdir = s.repo.join("env-target");
    let (code, stdout, stderr) = s.cli_at_env(
        &s.repo,
        &["issue", "start", "D-1"],
        &[("CARGO_TARGET_DIR", envdir.to_str().unwrap())],
    );
    assert_eq!(code, 0, "{stderr}");
    let out: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        out["target_dir"].as_str().unwrap(),
        envdir.to_string_lossy(),
        "{out}"
    );
    // No farm — every build lands in the env dir instead.
    let wt = s.worktree_of("D-1");
    assert!(!wt.join("target/debug/deps").exists());
    let show = s.cli(&["issue", "show", "D-1", "--json"]).1;
    let wt_ref = show["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "worktree")
        .unwrap();
    assert_eq!(
        wt_ref["cargo_target"].as_str().unwrap(),
        envdir.to_string_lossy()
    );
}

// ---- session start|end: stub daemon socket, fixture pm + repo (CAD-92) ----

/// Canned `agent_show` data for one alias. `flip` replaces the answer
/// after the first `agent_show` — an agent that goes busy between the
/// fleet snapshot and the pre-stop re-check.
struct StubAgent {
    row: Value,
    messages: Vec<Value>,
    queued: i64,
    unknown: i64,
    flip: Option<Value>,
}

fn stub_agent(
    alias: &str,
    provider: &str,
    kind: &str,
    state: &str,
    idle_for_secs: i64,
    show: (Vec<Value>, i64, i64),
) -> StubAgent {
    let (messages, queued, unknown) = show;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    StubAgent {
        row: json!({
            "alias": alias, "provider": provider, "endpoint_kind": kind,
            "role": "", "cwd": "/", "state": state, "enabled": true,
            "dead": false, "endpoint": if state == "stopped" { Value::Null } else { json!("ep") },
            "created": now - 86_400.0, "updated": now - idle_for_secs as f64,
        }),
        messages,
        queued,
        unknown,
        flip: None,
    }
}

impl StubAgent {
    /// After the first `agent_show`, serve `flip` — the race case.
    fn flipping(mut self, flip: Value) -> Self {
        self.flip = Some(flip);
        self
    }
    /// The agent's working directory — `--project` membership is
    /// issue-owner first, cwd-under-repo second.
    fn with_cwd(mut self, cwd: &Path) -> Self {
        self.row["cwd"] = json!(cwd.to_str().unwrap_or("/"));
        self
    }
}

/// The daemon wire protocol on `<state>/cadence.sock` with canned
/// answers — the session verbs under test connect exactly like they
/// would to the real daemon. `calls` records `(method, params)` so a
/// test can prove `--dry-run` mutated nothing and which agent a stop
/// actually named.
struct StubDaemon {
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    _thread: JoinHandle<()>,
}

fn stub_daemon(state: &Path, build_commit: &str, agents: Vec<StubAgent>) -> StubDaemon {
    std::fs::create_dir_all(state).unwrap();
    let listener = UnixListener::bind(state.join("cadence.sock")).unwrap();
    let calls = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
    let calls_t = Arc::clone(&calls);
    let rows: Vec<Value> = agents.iter().map(|a| a.row.clone()).collect();
    let mut shows: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    let mut flips: std::collections::HashMap<String, (Arc<Mutex<u32>>, Value)> =
        std::collections::HashMap::new();
    for a in agents {
        let alias = a.row["alias"].as_str().unwrap().to_string();
        shows.insert(
            alias.clone(),
            json!({
                "agent": a.row, "messages": a.messages,
                "queued": a.queued, "unknown": a.unknown,
                "event_cursor": 0,
            }),
        );
        if let Some(flip) = a.flip {
            flips.insert(alias, (Arc::new(Mutex::new(0)), flip));
        }
    }
    let info = json!({
        "build_commit": build_commit,
        "build_time": "2026-01-01T00:00:00Z",
        "started_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 - 600,
    });
    let thread = thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut stream) = conn else { continue };
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_err() {
                continue;
            }
            let req: Value = serde_json::from_str(&line).unwrap_or_default();
            let method = req["method"].as_str().unwrap_or_default().to_string();
            calls_t
                .lock()
                .unwrap()
                .push((method.clone(), req["params"].clone()));
            let params = &req["params"];
            let result = match method.as_str() {
                "health" => json!({"ok": true, "version": 1}),
                "daemon_info" => info.clone(),
                "agent_list" => json!({"agents": rows}),
                "agent_show" => {
                    let alias = params["alias"].as_str().unwrap_or_default();
                    if let Some((n, flip)) = flips.get(alias) {
                        let mut n = n.lock().unwrap();
                        *n += 1;
                        if *n >= 2 {
                            flip.clone()
                        } else {
                            shows.get(alias).cloned().unwrap_or_default()
                        }
                    } else {
                        shows
                            .get(alias)
                            .cloned()
                            .unwrap_or_else(|| json!({"messages": [], "queued": 0, "unknown": 0}))
                    }
                }
                "agent_requests" => json!({"requests": []}),
                "agent_probe" => json!({"idle": true}),
                "agent_stop" => json!({"alias": params["alias"], "state": "stopped"}),
                "agent_gc" => json!({"removed": []}),
                _ => {
                    let _ = writeln!(
                        stream,
                        "{}",
                        json!({"ok": false, "error": {"kind": "rejected",
                              "message": format!("Unknown method {method}")}})
                    );
                    continue;
                }
            };
            let _ = writeln!(stream, "{}", json!({"ok": true, "result": result}));
        }
    });
    StubDaemon {
        calls,
        _thread: thread,
    }
}

fn stub_calls(sd: &StubDaemon) -> Vec<(String, Value)> {
    sd.calls.lock().unwrap().clone()
}

/// `<pm>` with one project pointing at `repo`, one `doing` issue
/// owned by an alias the stub does not serve, a second `doing` issue
/// with branch+worktree refs to `repo`'s merged `.cadence/wt/tst-7-done`
/// (what `issue start` records), and an empty notes dir. The pm dir is
/// a git repo — `issue finish` commits ref-closures into it.
fn seed_pm(pm: &Path, repo: &Path, notes: &Path) {
    std::fs::create_dir_all(pm.join("tst")).unwrap();
    std::fs::create_dir_all(notes).unwrap();
    std::fs::write(
        pm.join("pm.yaml"),
        format!(
            "schema: 1\nnotes_dir: {}\nstatuses:\n- backlog\n- ready\n- doing\n- review\n- done\n- dropped\n",
            notes.display()
        ),
    )
    .unwrap();
    std::fs::write(
        pm.join("tst/project.yaml"),
        format!(
            "key: tst\nprefix: TST\nrepos:\n- path: {}\n",
            repo.display()
        ),
    )
    .unwrap();
    let issue = pm.join("tst/TST-9");
    std::fs::create_dir_all(&issue).unwrap();
    std::fs::write(
        issue.join("issue.md"),
        "---\nid: TST-9\ntitle: ghost-owned doing issue\nstatus: doing\n\
         priority: P2\nowner: ghost-agent\ncreated: 2026-01-01T00:00:00Z\n---\n\nbody\n",
    )
    .unwrap();
    let wt = repo.join(".cadence/wt/tst-7-done");
    let done = pm.join("tst/TST-7");
    std::fs::create_dir_all(&done).unwrap();
    std::fs::write(
        done.join("issue.md"),
        format!(
            "---\nid: TST-7\ntitle: merged worktree\nstatus: doing\npriority: P2\n\
             created: 2026-01-01T00:00:00Z\nrefs:\n\
             - kind: branch\n  path: cadence/tst-7-done\n\
             - kind: worktree\n  path: {}\n---\n\nbody\n",
            wt.display()
        ),
    )
    .unwrap();
    // `issue finish` commits the ref-closure — the pm dir must be a
    // real git repo with an identity.
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
        vec!["add", "-A"],
        vec!["commit", "-qm", "seed"],
    ] {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(pm)
            .args(&args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

/// A git repo with a merged `cadence/tst-7-done` worktree and a plain
/// `.cadence/wt/tst-88-ghost` dir — one merge candidate, one orphan.
fn seed_repo(repo: &Path) {
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    std::fs::create_dir_all(repo).unwrap();
    git(&["init", "-b", "main"]);
    git(&["config", "user.email", "test@x"]);
    git(&["config", "user.name", "test"]);
    std::fs::write(repo.join("f.txt"), "x").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "init"]);
    git(&["branch", "cadence/tst-7-done"]);
    git(&[
        "worktree",
        "add",
        ".cadence/wt/tst-7-done",
        "cadence/tst-7-done",
    ]);
    std::fs::create_dir_all(repo.join(".cadence/wt/tst-88-ghost")).unwrap();
}

/// `cadence <args>` against the fixture state dir + pm dir, cwd at the
/// fixture repo. Output captured; the stub answers daemon RPCs.
fn run_session(state: &Path, pm: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    run_session_env(state, pm, cwd, args, &[])
}

/// `run_session` with extra env pairs — the host-report fixture and a
/// stubbed `gh` on PATH ride in through here.
fn run_session_env(
    state: &Path,
    pm: &Path,
    cwd: &Path,
    args: &[&str],
    envs: &[(&str, &Path)],
) -> std::process::Output {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(state)
        .args(args)
        .env("CADENCE_PM_DIR", pm)
        .current_dir(cwd);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

/// `run_session` with `--host-report <fixture>` appended — the flag,
/// never an env var, carries the fixture so an ambient environment
/// cannot soften the gate.
fn run_session_host(
    state: &Path,
    pm: &Path,
    cwd: &Path,
    args: &[&str],
    host: &Path,
    envs: &[(&str, &Path)],
) -> std::process::Output {
    let mut v: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    v.push("--host-report".to_string());
    v.push(host.display().to_string());
    let argrefs: Vec<&str> = v.iter().map(|a| a.as_str()).collect();
    run_session_env(state, pm, cwd, &argrefs, envs)
}

/// A clean doctor-host report on disk — `--host-report` makes the
/// verbs read it instead of scanning the real host, so the session
/// tests are identical on a dev box and a 97%-full CI host.
fn clean_host(dir: &Path) -> PathBuf {
    let f = dir.join("host-report.json");
    std::fs::write(&f, r#"{"level":"ok","checks":[]}"#).unwrap();
    f
}

#[test]
fn session_start_reports_failures_and_fix_only_starts_ui() {
    suite_slot();
    let dir = TempDir::new().unwrap();
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    let repo = dir.path().join("repo");
    seed_pm(&pm, &repo, &dir.path().join("notes"));
    seed_repo(&repo);
    let sd = stub_daemon(
        &state,
        "stale-build-000",
        vec![
            stub_agent(
                "w1",
                "fake",
                "fake",
                "attention",
                700,
                (
                    vec![json!({"id": "m-unk", "state": "unknown", "body": "lost turn"})],
                    0,
                    1,
                ),
            ),
            stub_agent(
                "pm-inbox",
                "inbox",
                "inbox",
                "idle",
                700,
                (
                    vec![json!({"id": "k1", "state": "queued", "body": "kickoff"})],
                    2,
                    0,
                ),
            ),
        ],
    );

    let host = clean_host(dir.path());
    let out = run_session_host(&state, &pm, &repo, &["session", "start"], &host, &[]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(2), "expected no-go exit 2:\n{text}");
    assert!(
        text.contains("stale-build-000"),
        "stale daemon build:\n{text}"
    );
    assert!(text.contains("m-unk"), "unknown message named:\n{text}");
    assert!(text.contains("tst-88-ghost"), "orphan worktree:\n{text}");
    assert!(text.contains("pm-inbox"), "unread inbox:\n{text}");
    assert!(
        text.contains("TST-9"),
        "doing issue with dead owner:\n{text}"
    );
    // Nothing was fixed or mutated without --fix.
    assert!(!state.join("ui.pid").exists());
    for (m, _) in stub_calls(&sd) {
        assert!(
            !matches!(m.as_str(), "agent_stop" | "agent_gc"),
            "read-only start mutated: {m}"
        );
    }

    // --fix: a free port persisted in ui.json lets the real binary
    // spawn `ui run`; the stub daemon must be left untouched.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    std::fs::write(state.join("ui.json"), format!("{{\"port\": {port}}}")).unwrap();
    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "start", "--fix"],
        &host,
        &[],
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !state.join("ui.pid").exists() {
        assert!(Instant::now() < deadline, "ui never started:\n{text}");
        thread::sleep(Duration::from_millis(100));
    }
    // Nothing else: the daemon was reachable, so no daemon start (the
    // log file it would create is absent) and no mutating RPCs.
    assert!(
        !state.join("daemon.log").exists(),
        "daemon was started:\n{text}"
    );
    for (m, _) in stub_calls(&sd) {
        assert!(
            !matches!(m.as_str(), "agent_stop" | "agent_gc" | "agent_send"),
            "--fix mutated: {m}"
        );
    }
    let stop = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&state)
        .args(["ui", "stop"])
        .output()
        .unwrap();
    assert!(stop.status.success());
}

#[test]
fn session_end_dry_run_plans_real_run_stops_only_idle() {
    suite_slot();
    let dir = TempDir::new().unwrap();
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    let repo = dir.path().join("repo");
    seed_pm(&pm, &repo, &dir.path().join("notes"));
    seed_repo(&repo);
    let sd = stub_daemon(
        &state,
        cadence_agent::overview::BUILD_COMMIT,
        vec![
            stub_agent("old-idle", "fake", "fake", "idle", 7200, (vec![], 0, 0)),
            stub_agent("fresh-idle", "fake", "fake", "idle", 60, (vec![], 0, 0)),
            // `state: idle` with a running message — the message, not
            // the state field, is what keeps it alive (running_msg).
            stub_agent(
                "idle-running",
                "fake",
                "fake",
                "idle",
                7200,
                (
                    vec![json!({"id": "m-ir", "state": "running",
                             "body": "claimed mid-run", "started": 1.0})],
                    0,
                    0,
                ),
            ),
            stub_agent(
                "busy-one",
                "fake",
                "fake",
                "busy",
                7200,
                (
                    vec![json!({"id": "m-run", "state": "running",
                             "body": "long turn", "started": 1.0})],
                    0,
                    0,
                ),
            ),
            stub_agent(
                "queued-one",
                "fake",
                "fake",
                "idle",
                7200,
                (
                    vec![json!({"id": "m-q", "state": "queued", "body": "queued"})],
                    1,
                    0,
                ),
            ),
            stub_agent("pm-inbox", "inbox", "inbox", "idle", 7200, (vec![], 2, 0)),
        ],
    );
    let host = clean_host(dir.path());
    // A stub `gh` first on PATH: a dry run must never invoke it —
    // the gh cache is read, not refreshed.
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh_called = dir.path().join("gh-called");
    std::fs::write(
        bin.join("gh"),
        format!(
            "#!/bin/sh\necho called >> '{}'\nexit 1\n",
            gh_called.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path_env = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let envs = [("PATH", Path::new(&path_env))];

    // Dry run: names the stop candidates and the merged worktree,
    // changes nothing.
    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "end", "--dry-run"],
        &host,
        &envs,
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("old-idle"),
        "dry-run names idle agent:\n{text}"
    );
    assert!(
        text.contains("tst-7-done"),
        "dry-run names merged worktree:\n{text}"
    );
    assert!(
        !text.contains("fresh-idle"),
        "dry-run must not list a still-active agent:\n{text}"
    );
    for (m, _) in stub_calls(&sd) {
        assert!(
            !matches!(m.as_str(), "agent_stop" | "agent_gc"),
            "dry-run mutated: {m}"
        );
    }
    // A dry run writes nothing — no sessions/ dir, and the markdown is
    // previewed to stdout instead.
    let sessions = state.join("sessions");
    assert!(
        !sessions.exists() || std::fs::read_dir(&sessions).unwrap().next().is_none(),
        "dry-run wrote a handoff file: {:?}",
        sessions
    );
    assert!(
        text.contains("would write") && text.contains("## open PRs"),
        "dry-run previews the handoff, writes nothing:\n{text}"
    );
    // Nothing at all was written — no handoff, no gh cache, no `gh`
    // subprocess at all.
    assert!(
        !gh_called.exists(),
        "dry-run invoked gh — a dry run writes nothing, cache included"
    );
    assert!(
        !state.join("overview-gh.json").exists(),
        "dry-run wrote the gh cache"
    );

    // Real run: only the agent idle past --idle-secs is stopped.
    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "end", "--idle-secs", "1800"],
        &host,
        &envs,
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stops: Vec<(String, Value)> = stub_calls(&sd)
        .into_iter()
        .filter(|(m, _)| m == "agent_stop")
        .collect();
    assert_eq!(stops.len(), 1, "exactly one stop:\n{text}");
    assert_eq!(
        stops[0].1["alias"].as_str().unwrap_or_default(),
        "old-idle",
        "the stop names only the idle agent:\n{text}"
    );
    // `idle-running` was state-idle but carries a running message —
    // never stopped.
    for (m, p) in stub_calls(&sd) {
        if m == "agent_stop" {
            assert_ne!(
                p["alias"].as_str().unwrap_or_default(),
                "idle-running",
                "an agent with a running message was stopped:\n{text}"
            );
        }
    }
    assert!(
        stub_calls(&sd).iter().any(|(m, _)| m == "agent_gc"),
        "agent gc ran:\n{text}"
    );
    assert!(text.contains("old-idle"));
    // S6: the real-run candidate count is finished+refused — the same
    // set the dry run counts, not every sweep row incl. skips.
    assert!(
        text.contains("1 finished of 1 candidate(s)"),
        "finish count uses the same set dry-run does:\n{text}"
    );

    // The handoff note landed with the required sections.
    let sessions = state.join("sessions");
    let notes: Vec<PathBuf> = std::fs::read_dir(&sessions)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(notes.len(), 1, "one handoff file: {notes:?}");
    let md = std::fs::read_to_string(&notes[0]).unwrap();
    for section in [
        "## open PRs",
        "## running turns",
        "## queued kickoffs",
        "## issues in review",
        "## done this run",
        "## next session first",
    ] {
        assert!(md.contains(section), "handoff missing {section}:\n{md}");
    }
    assert!(md.contains("old-idle"), "stopped agent recorded:\n{md}");

    // A second real run the same day never overwrites the first.
    let out = run_session_host(&state, &pm, &repo, &["session", "end"], &host, &envs);
    // The host fixture is clean and nothing failed — this is a green
    // run outright, on any host.
    assert!(
        out.status.success(),
        "second end failed:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let notes: Vec<PathBuf> = std::fs::read_dir(&sessions)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(
        notes.len(),
        2,
        "two handoff files, none overwritten: {notes:?}"
    );
}

/// Round-2 B1: the initial `fleet()` snapshot is only a candidate list.
/// If an agent turns busy between the snapshot and the stop, `session
/// end` re-shows it and must not stop it.
#[test]
fn session_end_stop_race_rechecks_show() {
    let tmp = TempDir::new().unwrap();
    let (state, pm, repo, notes) = (
        tmp.path().join("state"),
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("notes"),
    );
    for d in [&state, &pm, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    seed_pm(&pm, &repo, &notes);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    // First agent_show: an idle, stop-worthy candidate. Second show —
    // the pre-stop re-check — the agent has turned busy with a running
    // message. No agent_stop may be issued for it.
    let flip_show = json!({
        "agent": {
            "alias": "racy", "provider": "fake", "endpoint_kind": "fake",
            "endpoint": "ep", "state": "busy", "dead": false,
            "updated": now,
        },
        "messages": [{"id": "m-racy", "state": "running",
                      "body": "dispatched mid-sweep", "started": now}],
        "queued": 0, "unknown": 0, "event_cursor": 0,
    });
    let sd = stub_daemon(
        &state,
        cadence_agent::overview::BUILD_COMMIT,
        vec![
            stub_agent("racy", "fake", "fake", "idle", 7200, (vec![], 0, 0)).flipping(flip_show),
            stub_agent("calm", "fake", "fake", "idle", 7200, (vec![], 0, 0)),
        ],
    );
    let host = clean_host(tmp.path());
    let out = run_session_host(&state, &pm, &repo, &["session", "end"], &host, &[]);
    assert!(
        out.status.success(),
        "end failed:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let stops: Vec<(String, Value)> = stub_calls(&sd)
        .into_iter()
        .filter(|(m, _)| m == "agent_stop")
        .collect();
    assert_eq!(
        stops.len(),
        1,
        "only the still-idle agent was stopped:\n{text}"
    );
    assert_eq!(
        stops[0].1["alias"].as_str().unwrap_or_default(),
        "calm",
        "the raced agent must never be stopped:\n{text}"
    );
    assert!(
        text.contains("racy") && text.contains("skip"),
        "the skipped re-check is reported:\n{text}"
    );
    // ...once: a skipped candidate is one annotated row, not a bare
    // candidate line plus a second `— skipped` line.
    assert_eq!(
        text.matches("racy").count(),
        1,
        "the skipped alias listed twice:\n{text}"
    );
    // Contract: the re-check was a second agent_show for racy.
    let shows = stub_calls(&sd)
        .iter()
        .filter(|(m, p)| m == "agent_show" && p["alias"] == "racy")
        .count();
    assert!(
        shows >= 2,
        "racy was re-shown {shows}x before the stop decision"
    );
}

/// Round-2 B3: --project scopes the finish sweep; other projects'
/// merged worktrees are never touched.
#[test]
fn session_end_project_scopes_sweep() {
    let tmp = TempDir::new().unwrap();
    let (pm_dir, home, state) = (
        tmp.path().join("pm"),
        tmp.path().join("home"),
        tmp.path().join("state"),
    );
    let repo_a = tmp.path().join("repo-a");
    let repo_b = tmp.path().join("repo-b");
    for dir in [&pm_dir, &home, &state, &repo_a, &repo_b] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let git = |dir: &Path, args: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr)
        );
    };
    for repo in [&repo_a, &repo_b] {
        git(repo, &["init", "-b", "main"]);
        git(repo, &["config", "user.email", "t@t"]);
        git(repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("f"), "x").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "init"]);
    }
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    let cli_raw = |args: &[&str]| -> (i32, String, String) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let cli = |args: &[&str]| -> (bool, Value) {
        let (code, stdout, stderr) = cli_raw(args);
        (
            code == 0,
            serde_json::from_str(stdout.trim())
                .unwrap_or_else(|_| panic!("{args:?} not json ({stderr}): {stdout}")),
        )
    };
    assert!(cli(&["issue", "init"]).0);
    let ra = repo_a.canonicalize().unwrap().to_str().unwrap().to_string();
    let rb = repo_b.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(cli(&["issue", "project", "add", "aaa", "--prefix", "A", "--repo", &ra]).0);
    assert!(cli(&["issue", "project", "add", "bbb", "--prefix", "B", "--repo", &rb]).0);
    assert!(cli(&["issue", "new", "one", "--project", "aaa"]).0);
    assert!(cli(&["issue", "new", "two", "--project", "bbb"]).0);
    for id in ["A-1", "B-1"] {
        let (ok, out) = cli(&["issue", "start", id]);
        assert!(ok, "{out}");
        let (ok, _) = cli(&["issue", "set", id, "owner="]);
        assert!(ok);
    }
    // A-1's owner is the second way an agent belongs to the project —
    // cwd-under-repo is the first.
    let (ok, out) = cli(&["issue", "set", "A-1", "owner=a-owned"]);
    assert!(ok, "{out}");
    let wt_a = repo_a.join(".cadence/wt/a-1-one");
    let wt_b = repo_b.join(".cadence/wt/b-1-two");
    for (wt, file) in [(&wt_a, "a.txt"), (&wt_b, "b.txt")] {
        std::fs::write(wt.join(file), "x").unwrap();
        git(wt, &["add", "-A"]);
        git(wt, &["commit", "-qm", "work"]);
    }
    git(&repo_a, &["merge", "-q", "cadence/a-1-one"]);
    git(&repo_b, &["merge", "-q", "cadence/b-1-two"]);
    assert!(wt_a.exists() && wt_b.exists(), "fixture wts exist");

    // Fleet: `a-idle` works under repo_a, `a-owned` owns A-1 outright,
    // `b-idle` works under repo_b — all idle past the threshold.
    let sd = stub_daemon(
        &state,
        cadence_agent::overview::BUILD_COMMIT,
        vec![
            stub_agent("a-idle", "fake", "fake", "idle", 7200, (vec![], 0, 0))
                .with_cwd(&repo_a.canonicalize().unwrap()),
            stub_agent("a-owned", "fake", "fake", "idle", 7200, (vec![], 0, 0)),
            stub_agent("b-idle", "fake", "fake", "idle", 7200, (vec![], 0, 0))
                .with_cwd(&repo_b.canonicalize().unwrap()),
        ],
    );
    let host = clean_host(tmp.path());
    let out = run_session_host(
        &state,
        &pm_dir,
        &repo_a,
        &["session", "end", "--project", "aaa"],
        &host,
        &[],
    );
    assert!(
        out.status.success(),
        "scoped end failed:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !wt_a.exists(),
        "scoped run left project aaa's merged worktree"
    );
    assert!(wt_b.exists(), "scoped run removed project bbb's worktree");
    // --project scopes the stops too: aaa's agents stopped, bbb's left
    // running and reported as out-of-scope.
    let mut stopped: Vec<String> = stub_calls(&sd)
        .into_iter()
        .filter(|(m, _)| m == "agent_stop")
        .filter_map(|(_, p)| p["alias"].as_str().map(str::to_string))
        .collect();
    stopped.sort();
    assert_eq!(stopped, vec!["a-idle", "a-owned"], "scoped stops:\n{text}");
    assert!(
        text.contains("outside project 'aaa'"),
        "the out-of-scope agent is reported:\n{text}"
    );
    // `agent_gc` is fleet-wide — under --project it is skipped, and the
    // row says so rather than overreaching into bbb's agents.
    assert!(
        !stub_calls(&sd).iter().any(|(m, _)| m == "agent_gc"),
        "fleet-wide gc ran under --project:\n{text}"
    );
    assert!(
        text.contains("gc is fleet-wide — skipped under --project aaa"),
        "the gc row says which:\n{text}"
    );
}

/// Round-2 S7 + nit: `--json` stdout is exactly one JSON document, and
/// an unknown `--project` is rejected like `issue ls --project`.
#[test]
fn session_json_single_document_and_project_validation() {
    let tmp = TempDir::new().unwrap();
    let (state, pm, repo, notes) = (
        tmp.path().join("state"),
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("notes"),
    );
    for d in [&state, &pm, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    seed_pm(&pm, &repo, &notes);
    let _sd = stub_daemon(&state, cadence_agent::overview::BUILD_COMMIT, vec![]);
    // `ui.json` seeded with a free port so `start --fix` actually starts
    // the UI — the path that used to pollute the composed JSON.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    std::fs::write(state.join("ui.json"), format!("{{\"port\": {port}}}")).unwrap();
    let host = clean_host(tmp.path());

    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "start", "--fix", "--json"],
        &host,
        &[],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str::<Value>(text.trim())
        .unwrap_or_else(|e| panic!("start --fix --json is not one document: {e}\n{text}"));
    // `--fix` actually started the UI — without this the test goes
    // vacuous: a silently no-op fix still prints one clean document.
    assert!(
        state.join("ui.pid").exists(),
        "start --fix did not start the UI (no ui.pid):\n{text}"
    );
    let _ = run_session(&state, &pm, &repo, &["ui", "stop"]);

    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "end", "--json"],
        &host,
        &[],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str::<Value>(text.trim())
        .unwrap_or_else(|e| panic!("end --json is not one document: {e}\n{text}"));

    // Unknown project is an error, not an empty sweep.
    for args in [
        vec!["session", "start", "--project", "nosuch"],
        vec!["session", "end", "--project", "nosuch"],
    ] {
        let out = run_session_host(&state, &pm, &repo, &args, &host, &[]);
        assert!(
            !out.status.success(),
            "{args:?} accepted an unknown project:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let msg = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(msg.contains("nosuch"), "{args:?}: {msg}");
    }
}

/// Round-3 B1: the host sweep in `session end` is a report, not a
/// gate — a `fail`-level host caps at `warn` (exit 1) while `session
/// start` correctly treats the same host as a no-go (exit 2). The
/// fixture pins the state; the real host is never read.
#[test]
fn session_end_host_fail_caps_at_warn() {
    let tmp = TempDir::new().unwrap();
    let (state, pm, repo, notes) = (
        tmp.path().join("state"),
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("notes"),
    );
    for d in [&state, &pm, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    seed_pm(&pm, &repo, &notes);
    seed_repo(&repo);
    let _sd = stub_daemon(&state, cadence_agent::overview::BUILD_COMMIT, vec![]);
    // A `fail` host — the 97%-full disk that broke these tests.
    let host = tmp.path().join("host-fail.json");
    std::fs::write(
        &host,
        r#"{"level":"fail","checks":[{"name":"disk","level":"fail",
        "detail":"/ 97% full","remedy":"clean up"}]}"#,
    )
    .unwrap();
    let out = run_session_host(&state, &pm, &repo, &["session", "start"], &host, &[]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "start must still no-go on a fail host:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );

    let out = run_session_host(&state, &pm, &repo, &["session", "end"], &host, &[]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "end caps the host report at warn, exit 1:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("sweep     warn") || text.contains("sweep warn"),
        "the sweep row shows warn, not fail:\n{text}"
    );
}

/// Addendum §1: session output never prints process argv — every
/// scrubber leaks some shape, so orphans display as `exe (arg count)`.
/// The fixture plants a credential in `head`; it must not appear.
/// `pid = self` gives a readable cmdline; `u32::MAX` gives the
/// unavailable path.
#[test]
fn session_end_orphans_report_exe_and_argc_never_argv() {
    let tmp = TempDir::new().unwrap();
    let (state, pm, repo, notes) = (
        tmp.path().join("state"),
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("notes"),
    );
    for d in [&state, &pm, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    seed_pm(&pm, &repo, &notes);
    seed_repo(&repo);
    let _sd = stub_daemon(&state, cadence_agent::overview::BUILD_COMMIT, vec![]);
    let me = std::process::id();
    let host = tmp.path().join("host-orphans.json");
    std::fs::write(
        &host,
        format!(
            r#"{{"level":"warn","checks":[{{"name":"orphans","level":"warn",
            "detail":"2: pid {me} (1h npm exec --api-key=figd_PLANTEDLEAK --stdio)",
            "remedy":"kill {me} 4294967295",
            "value":{{"pids":[
                {{"pid":{me},"head":"npm exec --api-key=figd_PLANTEDLEAK --stdio",
                  "reasons":["cwd/exe under a deleted .cadence/wt worktree"]}},
                {{"pid":4294967295,"head":"./hung-test-binary --secret=hunter2",
                  "reasons":["cargo test binary older than an hour"]}}
            ]}}}}]}}"#
        ),
    )
    .unwrap();
    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "end", "--dry-run"],
        &host,
        &[],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("orphans: 2 orphaned pid(s)"),
        "count-only detail replaces the argv-bearing one:\n{text}"
    );
    assert!(
        text.contains(&format!("orphan pid {me} — integration-")),
        "self pid shows the executable basename:\n{text}"
    );
    assert!(
        text.contains(&format!("orphan pid {me} — integration-")) && text.contains(" arg(s))"),
        "the argument count is shown:\n{text}"
    );
    assert!(
        text.contains("orphan pid 4294967295 — (argv unavailable)"),
        "an unreadable cmdline degrades without head:\n{text}"
    );
    for leaked in [
        "figd_PLANTEDLEAK",
        "hunter2",
        "--api-key",
        "--secret",
        "npm exec",
        "hung-test-binary",
    ] {
        assert!(!text.contains(leaked), "argv leaked as {leaked:?}:\n{text}");
    }
    assert_eq!(
        out.status.code(),
        Some(1),
        "warn host, dry-run clean: exit 1:\n{text}"
    );
}

/// Round-4 blocker: the host fixture is `--host-report`, never an env
/// var — an ambient `CADENCE_SESSION_HOST_JSON` must not soften the
/// gate, a bad fixture path is a hard error, and every fixture run is
/// labelled in text and `--json`.
#[test]
fn session_host_report_flag_labels_errors_and_env_is_dead() {
    let tmp = TempDir::new().unwrap();
    let (state, pm, repo, notes) = (
        tmp.path().join("state"),
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("notes"),
    );
    for d in [&state, &pm, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    seed_pm(&pm, &repo, &notes);
    seed_repo(&repo);
    let _sd = stub_daemon(&state, cadence_agent::overview::BUILD_COMMIT, vec![]);

    // A bad fixture path errors — it never falls through to a real
    // scan (that would read the real host with no signal).
    let out = run_session(
        &state,
        &pm,
        &repo,
        &["session", "end", "--host-report", "/nonexistent/host.json"],
    );
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success() && msg.contains("/nonexistent/host.json"),
        "a bad fixture path must error naming the path:\n{msg}"
    );
    // …before any mutation — no handoff was written.
    assert!(
        !state.join("sessions").exists()
            || std::fs::read_dir(state.join("sessions"))
                .unwrap()
                .next()
                .is_none(),
        "a bad fixture must fail before the handoff write"
    );
    // Same for a parsable-path-but-not-JSON file.
    let garbage = tmp.path().join("not-json.json");
    std::fs::write(&garbage, "not json at all").unwrap();
    let out = run_session(
        &state,
        &pm,
        &repo,
        &["session", "end", "--host-report", garbage.to_str().unwrap()],
    );
    assert!(
        !out.status.success(),
        "an unparsable fixture must error:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A fixture run is labelled — in text and in --json.
    let host = clean_host(tmp.path());
    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "end", "--dry-run"],
        &host,
        &[],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("real host not scanned"),
        "text output labels the fixture:\n{text}"
    );
    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "end", "--dry-run", "--json"],
        &host,
        &[],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let j: Value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("end --json not one document: {e}\n{text}"));
    assert_eq!(
        j["host_source"].as_str().unwrap_or_default(),
        format!("fixture {}", host.display()),
        "--json labels the fixture:\n{text}"
    );
    // `session start` takes the same flag and labels it the same way.
    let out = run_session_host(&state, &pm, &repo, &["session", "start"], &host, &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("real host not scanned"),
        "session start labels the fixture:\n{text}"
    );
    let out = run_session_host(
        &state,
        &pm,
        &repo,
        &["session", "start", "--json"],
        &host,
        &[],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let j: Value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("start --json not one document: {e}\n{text}"));
    assert_eq!(
        j["host_source"].as_str().unwrap_or_default(),
        format!("fixture {}", host.display()),
        "start --json labels the fixture:\n{text}"
    );

    // The old env var is dead: set it to a *failing* fixture and run
    // without the flag — the real host is scanned, the sentinel
    // detail never appears, and host_source reports `scan`.
    let sentinel = tmp.path().join("env-fixture.json");
    std::fs::write(
        &sentinel,
        r#"{"level":"fail","checks":[{"name":"disk","level":"fail",
        "detail":"SENTINEL-DISK-SHOULD-NEVER-APPEAR","remedy":"x"}]}"#,
    )
    .unwrap();
    let out = run_session_env(
        &state,
        &pm,
        &repo,
        &["session", "end", "--json"],
        &[("CADENCE_SESSION_HOST_JSON", sentinel.as_path())],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.contains("SENTINEL-DISK-SHOULD-NEVER-APPEAR"),
        "the env var must have no effect — it read the fixture:\n{text}"
    );
    let j: Value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("end --json not one document: {e}\n{text}"));
    assert_eq!(
        j["host_source"].as_str().unwrap_or_default(),
        "scan",
        "env-set fixture must report a real scan:\n{text}"
    );
}

/// CAD-146: piping output into a reader that closes early
/// (`cadence … | head -12`) must exit 0 quietly. Rust ignores
/// SIGPIPE, so the closed read end turns the next stdout write into
/// EPIPE and `println!` panics — the startup panic hook turns exactly
/// that failure into exit 0. The read end is dropped before the
/// first write so the broken pipe is deterministic.
#[test]
fn issue_ls_survives_a_closed_downstream_pipe() {
    let tmp = TempDir::new().unwrap();
    let (pm_dir, home, state) = (
        tmp.path().join("pm"),
        tmp.path().join("home"),
        tmp.path().join("state"),
    );
    for d in [&pm_dir, &home, &state] {
        std::fs::create_dir_all(d).unwrap();
    }
    issue_cli(&home, &state, &pm_dir, &["issue", "init"]);
    issue_cli(
        &home,
        &state,
        &pm_dir,
        &["issue", "project", "add", "demo", "--prefix", "D"],
    );
    // Enough issues that the listing overflows the 64KiB pipe buffer
    // even if the drop raced the first writes — seeded directly since
    // 400 `issue new`s would spend the test in pm git commits.
    for i in 1..=400 {
        let dir = pm_dir.join("demo").join(format!("D-{i}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("issue.md"),
            format!(
                "---\nid: D-{i}\ntitle: a reasonably long issue title \
                 carrying some weight {i}\nstatus: backlog\npriority: P2\n\
                 created: 2026-09-20T00:00:00Z\n---\n\nbody\n"
            ),
        )
        .unwrap();
    }
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&state)
        .args(["issue", "ls", "--json"])
        .env("HOME", &home)
        .env("CADENCE_PM_DIR", &pm_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // The downstream reader is gone before the listing starts.
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a closed pipe must exit 0, not panic or die by signal: {stderr}"
    );
    assert!(
        !stderr.contains("panicked") && !stderr.contains("Broken pipe"),
        "the EPIPE must never reach the user: {stderr}"
    );

    // But a verb that FAILS must keep its real exit code — the hook
    // exits with the code the process already committed to, not a
    // hard-coded 0. `issue show NOSUCH` writes its error to stderr;
    // with both stream ends closed (`2>&1 | head -c 0`) that print
    // panics on EPIPE and the answer must still be failure.
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&state)
        .args(["issue", "show", "NOSUCH-9999"])
        .env("HOME", &home)
        .env("CADENCE_PM_DIR", &pm_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    drop(child.stderr.take());
    let status = child.wait().unwrap();
    assert_eq!(
        status.code(),
        Some(1),
        "a failing verb keeps its failure code on a closed pipe"
    );
    // …and with the reader still open, the same failure exits the
    // same way — the hook only fires when the pipe is gone.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&state)
        .args(["issue", "show", "NOSUCH-9999"])
        .env("HOME", &home)
        .env("CADENCE_PM_DIR", &pm_dir)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("NOSUCH-9999"),
        "the open-pipe failure still prints its error"
    );
}

// ---------- CAD-136: report intake ----------

/// pm + two repos (the `cadence` project and a `product` project) +
/// home + state under one temp dir; `cli_at` runs the real binary with
/// cwd control — report routing is decided by kind and cwd, so the
/// fixture keeps both an inside-a-project cwd and a foreign one.
struct ReportFx {
    _tmp: TempDir,
    pm_dir: PathBuf,
    notes_dir: PathBuf,
    cadence_repo: PathBuf,
    product_repo: PathBuf,
    foreign_cwd: PathBuf,
    home: PathBuf,
    state: PathBuf,
    bin_dir: PathBuf,
}

impl ReportFx {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let (pm_dir, notes_dir, cadence_repo, product_repo, foreign_cwd, home, state) = (
            tmp.path().join("pm"),
            tmp.path().join("notes"),
            tmp.path().join("cadence-repo"),
            tmp.path().join("product-repo"),
            tmp.path().join("nowhere"),
            tmp.path().join("home"),
            tmp.path().join("state"),
        );
        for dir in [&pm_dir, &notes_dir, &home, &state, &foreign_cwd] {
            std::fs::create_dir_all(dir).unwrap();
        }
        git_repo(&cadence_repo);
        git_repo(&product_repo);
        let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
            .parent()
            .unwrap()
            .to_path_buf();
        let s = Self {
            _tmp: tmp,
            pm_dir,
            notes_dir,
            cadence_repo,
            product_repo,
            foreign_cwd,
            home,
            state,
            bin_dir,
        };
        assert!(s.cli(&["issue", "init"]).0);
        // init defaults notes_dir to the shared /var/www/agent-notes —
        // a stray real note tagged `Issue: C-1` would flip a derived
        // status and flake these tests, so point it at the temp dir.
        let pm_yaml = s.pm_dir.join("pm.yaml");
        let text = std::fs::read_to_string(&pm_yaml).unwrap();
        let text = text
            .lines()
            .map(|l| {
                if l.starts_with("notes_dir:") {
                    format!("notes_dir: {}", s.notes_dir.display())
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&pm_yaml, format!("{text}\n")).unwrap();
        for (key, prefix, repo) in [
            ("cadence", "C", s.cadence_repo.clone()),
            ("product", "P", s.product_repo.clone()),
        ] {
            let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
            let (ok, out) = s.cli(&[
                "issue", "project", "add", key, "--prefix", prefix, "--repo", &repo_s,
            ]);
            assert!(ok, "project add {key}: {out}");
        }
        s
    }

    fn cli(&self, args: &[&str]) -> (bool, Value) {
        self.cli_at(&self.product_repo, args)
    }

    fn cli_at(&self, cwd: &Path, args: &[&str]) -> (bool, Value) {
        self.cli_at_env(cwd, args, &[]).2
    }

    /// `(success, stderr, parsed stdout-or-stderr-json)` — stderr kept
    /// separate so refusal tests can assert on the message text.
    fn cli_at_env(
        &self,
        cwd: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> (bool, String, (bool, Value)) {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
        cmd.arg("--state-dir")
            .arg(&self.state)
            .args(args)
            .env("CADENCE_PM_DIR", &self.pm_dir)
            .env("HOME", &self.home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .current_dir(cwd);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let text = if out.stdout.is_empty() {
            stderr.clone()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        (
            out.status.success(),
            stderr,
            (
                out.status.success(),
                serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
            ),
        )
    }

    fn issue_body(&self, project: &str, id: &str) -> String {
        std::fs::read_to_string(self.pm_dir.join(project).join(id).join("issue.md")).unwrap()
    }

    fn tracker_log(&self, n: usize) -> String {
        git_at(
            &self.pm_dir,
            &["log", &format!("-{n}"), "--format=%s%n%(trailers)"],
        )
    }
}

/// The routing contract: `question`, `feedback` and `bug` file into
/// `cadence` from any cwd; `idea` files into the cwd's project (or
/// --project) and refuses when neither resolves. Priorities default
/// P3 except `bug` (P2); every issue carries `intake` + kind tags.
#[test]
fn report_routes_by_kind_and_defaults() {
    let s = ReportFx::new();

    // bug/question/feedback from the product repo all land in cadence.
    for (kind, want_id) in [("bug", "C-1"), ("question", "C-2"), ("feedback", "C-3")] {
        let (ok, out) = s.cli_at(&s.product_repo, &["report", "--kind", kind, "-m", "x"]);
        assert!(ok && out["id"] == want_id, "{kind}: {out}");
        assert_eq!(out["project"], "cadence");
    }
    // bug defaults P2, the rest P3; --priority overrides.
    let (_, out) = s.cli(&["report", "show", "C-1"]);
    assert_eq!(out["priority"], "P2");
    let (_, out) = s.cli(&["report", "show", "C-2"]);
    assert_eq!(out["priority"], "P3");
    let (ok, out) = s.cli_at(
        &s.product_repo,
        &["report", "--kind", "bug", "--priority", "P0", "-m", "sev"],
    );
    assert!(ok && out["priority"] == "P0", "{out}");

    // idea from the product repo lands in product.
    let (ok, out) = s.cli_at(
        &s.product_repo,
        &["report", "--kind", "idea", "-m", "a product idea"],
    );
    assert!(
        ok && out["id"] == "P-1" && out["project"] == "product",
        "{out}"
    );

    // idea from a foreign cwd refuses, naming --project.
    let (_, stderr, _) = s.cli_at_env(
        &s.foreign_cwd,
        &["report", "--kind", "idea", "-m", "stray idea"],
        &[],
    );
    assert!(stderr.contains("--project"), "{stderr}");

    // --project wins even for kinds that would otherwise route by cwd.
    let (ok, out) = s.cli_at(
        &s.product_repo,
        &[
            "report",
            "--kind",
            "idea",
            "--project",
            "cadence",
            "-m",
            "a cadence idea",
        ],
    );
    assert!(ok && out["project"] == "cadence", "{out}");

    // Tags: intake + kind on every issue; the commit carries the
    // Actor trailer.
    let body = s.issue_body("cadence", "C-1");
    assert!(
        body.contains("- bug") && body.contains("- intake"),
        "{body}"
    );
    let log = s.tracker_log(6);
    assert!(log.contains("Actor:"), "{log}");

    // The default kind is feedback — `cadence report -m` files into
    // cadence's project.
    let (ok, out) = s.cli_at(&s.product_repo, &["report", "-m", "no kind"]);
    assert!(
        ok && out["kind"] == "feedback" && out["project"] == "cadence",
        "{out}"
    );
}

/// `--issue` attaches the report as a comment on the named issue and
/// creates nothing new; the comment carries the kind and context.
#[test]
fn report_issue_attaches_comment() {
    let s = ReportFx::new();
    let (ok, out) = s.cli(&["issue", "new", "Target", "--project", "product"]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();

    let (ok, out) = s.cli(&[
        "report",
        "--issue",
        &id,
        "--kind",
        "question",
        "-m",
        "what does the flag do?",
    ]);
    assert!(ok && out["id"] == id && out["kind"] == "question", "{out}");
    let comments = s.pm_dir.join("product").join(&id).join("comments");
    let comment = std::fs::read_dir(&comments)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let text = std::fs::read_to_string(comment).unwrap();
    assert!(text.contains("what does the flag do?"), "{text}");
    assert!(text.contains("## Report context"), "{text}");
}

/// A credential-shaped string in the report body is stored redacted —
/// body, title and context all pass through the shared scrubber.
#[test]
fn report_redacts_credential_shapes() {
    let s = ReportFx::new();
    let secret = "ghp_".to_string() + &"a".repeat(36);
    let (ok, out) = s.cli(&[
        "report",
        "--kind",
        "bug",
        "-m",
        &format!("leaked {secret} in CI log"),
    ]);
    assert!(ok, "{out}");
    let body = s.issue_body("cadence", out["id"].as_str().unwrap());
    assert!(!body.contains(&secret), "{body}");
    assert!(body.contains("[REDACTED]"), "{body}");
}

/// The Overview needs-me row appears while the issue sits in backlog
/// and clears when it leaves — and `report ls` filters by kind and
/// project.
#[test]
fn report_needs_me_row_and_ls_filters() {
    let s = ReportFx::new();
    let (ok, _) = s.cli_at(
        &s.product_repo,
        &["report", "--kind", "idea", "-m", "an idea"],
    );
    assert!(ok);
    let (ok, _) = s.cli_at(&s.product_repo, &["report", "--kind", "bug", "-m", "a bug"]);
    assert!(ok);

    let view = overview_at(&s.home, &s.state, Some(&s.pm_dir), &[]);
    let needs = view["needs_me"].as_array().unwrap();
    let intake: Vec<&Value> = needs.iter().filter(|n| n["kind"] == "intake").collect();
    assert_eq!(intake.len(), 2, "{needs:?}");
    let commands: Vec<&str> = intake
        .iter()
        .filter_map(|n| n["command"].as_str())
        .collect();
    assert!(
        commands.contains(&"cadence report show P-1"),
        "{commands:?}"
    );
    assert!(
        commands.contains(&"cadence report show C-1"),
        "{commands:?}"
    );

    // `ls` filters: by kind and by project.
    let (_, out) = s.cli(&["report", "ls", "--kind", "idea"]);
    assert_eq!(out["count"], 1);
    assert_eq!(out["reports"][0]["id"], "P-1");
    let (_, out) = s.cli(&["report", "ls", "--project", "cadence"]);
    assert_eq!(out["count"], 1);
    assert_eq!(out["reports"][0]["id"], "C-1");

    // Moving the issue off backlog clears the row.
    let (ok, _) = s.cli(&["issue", "set", "P-1", "status=ready"]);
    assert!(ok);
    let view = overview_at(&s.home, &s.state, Some(&s.pm_dir), &[]);
    let needs = view["needs_me"].as_array().unwrap();
    assert_eq!(
        needs.iter().filter(|n| n["kind"] == "intake").count(),
        1,
        "{needs:?}"
    );
}

/// A report notifies the project's PM inbox — `team.yaml`
/// `roles.pm.alias` names it (ADR 0001); absent any resolvable PM the
/// report still files, with `notified` recording the miss.
#[test]
fn report_notifies_pm_inbox() {
    let s = ReportFx::new();
    let d = TestDaemon::start_on(s.state.clone());
    d.register_inbox("pm");
    // The daemon's own state dir is s.state — report and daemon agree.

    // No team.yaml yet — no resolvable PM, still files fine.
    let (ok, out) = s.cli(&["report", "--kind", "bug", "-m", "first"]);
    assert!(ok && out["notified"].is_null(), "{out}");

    // team.yaml declares the PM inbox — the report sends one line.
    std::fs::write(
        s.pm_dir.join("cadence").join("team.yaml"),
        "roles:\n  pm:\n    kind: inbox\n    alias: pm\n",
    )
    .unwrap();
    let (ok, out) = s.cli(&["report", "--kind", "bug", "-m", "second"]);
    assert!(
        ok && out["notified"]["sent"] == true && out["notified"]["to"] == "pm",
        "{out}"
    );
    let show = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    assert_eq!(show["queued"].as_i64().unwrap(), 1);
    let drained = d.rpc("agent_inbox", json!({"alias": "pm"})).unwrap();
    let msgs = drained["messages"].as_array().unwrap();
    assert!(
        msgs[0]["body"].as_str().unwrap().contains("C-2"),
        "{msgs:?}"
    );
}

/// `cli_at_env` without the JSON parse — for clap-level refusals
/// (`--issue --project`) that exit 2 with plain-text usage.
fn cli_raw_at(s: &ReportFx, cwd: &Path, args: &[&str]) -> (bool, String) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(&s.state)
        .args(args)
        .env("CADENCE_PM_DIR", &s.pm_dir)
        .env("HOME", &s.home)
        .env_remove("CADENCE_ALIAS")
        .current_dir(cwd);
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// Round 2: a multi-line body keeps its line structure — blank lines
/// and indentation survive the prose scrubber — and the title is the
/// first line only, never the collapsed body.
#[test]
fn report_preserves_multiline_body() {
    let s = ReportFx::new();
    let body = "Steps to reproduce:\n\n    1. run `cadence status`\n\t2. see error\n";
    let (ok, out) = s.cli_at(&s.product_repo, &["report", "--kind", "bug", "-m", body]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();
    let stored = s.issue_body("cadence", &id);
    assert!(stored.contains("    1. run `cadence status`"), "{stored}");
    assert!(stored.contains("\t2. see error"), "{stored}");
    let (_, out) = s.cli(&["report", "show", &id]);
    assert_eq!(out["title"], "Steps to reproduce:", "{out}");
}

/// Round 2: the prose leak rows — each secret asserted absent from
/// the stored issue file.
#[test]
fn report_redacts_prose_secret_forms() {
    let s = ReportFx::new();
    let rows: [(&str, &str); 9] = [
        (
            "Auth header\nAuthorization: Basic dXNlcjpwYXNzd29yZA==",
            "dXNlcjpwYXNzd29yZA==",
        ),
        ("quoted flag\n--password \"correct horse battery\"", "horse"),
        ("user pair\n-u \"admin:hunter 2\"", "admin:hunter"),
        ("prose\nnote: the db password is hunter2 ok", "hunter2"),
        (
            "url\ncall https://api/x?api_key=abcd1234&page=2 done",
            "abcd1234",
        ),
        (
            "pem\n-----BEGIN RSA PRIVATE KEY-----\nMIIabc123\n-----END RSA PRIVATE KEY-----\ntail",
            "MIIabc123",
        ),
        (
            "password:\n  synthetic_boundary_value",
            "synthetic_boundary_value",
        ),
        ("Example\n--password=\"first second third\"", "second"),
        (
            "Example\n--password \"first\n synthetic_quote_tail\" ordinary tail",
            "synthetic_quote_tail",
        ),
    ];
    for (i, (body, gone)) in rows.iter().enumerate() {
        let (ok, out) = s.cli_at(&s.product_repo, &["report", "--kind", "bug", "-m", body]);
        assert!(ok, "row {i}: {out}");
        let stored = s.issue_body("cadence", &format!("C-{}", i + 1));
        assert!(!stored.contains(gone), "row {i}: {stored}");
        assert!(stored.contains("[REDACTED]"), "row {i}: {stored}");
    }
    // A PEM marker may itself be the title, so exercise the `--file` path
    // because clap treats a leading `-----` inline value as an option.
    let pem_file = s._tmp.path().join("pem-title.txt");
    std::fs::write(
        &pem_file,
        "-----BEGIN RSA PRIVATE KEY-----\nsynthetic_pem_payload\n-----END RSA PRIVATE KEY-----",
    )
    .unwrap();
    let (ok, out) = s.cli_at(
        &s.product_repo,
        &[
            "report",
            "--kind",
            "bug",
            "--file",
            pem_file.to_str().unwrap(),
        ],
    );
    assert!(ok, "{out}");
    let stored = s.issue_body("cadence", "C-10");
    assert!(!stored.contains("synthetic_pem_payload"), "{stored}");
    assert!(stored.contains("[REDACTED]"), "{stored}");
    // The URL keeps its non-secret query params and path.
    let stored = s.issue_body("cadence", "C-5");
    assert!(
        stored.contains("https://api/x?api_key=[REDACTED]&page=2"),
        "{stored}"
    );
}

/// Boundary redaction also applies to comments on existing issues, not just
/// newly filed intake rows.
#[test]
fn report_comment_redacts_boundary_secret_forms() {
    let s = ReportFx::new();
    let (ok, out) = s.cli(&["issue", "new", "Target", "--project", "product"]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();
    let body = r#"password:
  synthetic_comment_boundary

Example
--password="first synthetic_comment_glued"

-----BEGIN RSA PRIVATE KEY-----
synthetic_comment_pem
-----END RSA PRIVATE KEY-----

Example
--password "first
 synthetic_comment_quote" ordinary tail"#;
    let (ok, out) = s.cli(&["report", "--issue", &id, "--kind", "bug", "-m", body]);
    assert!(ok, "{out}");
    let comments = s.pm_dir.join("product").join(&id).join("comments");
    let comment = std::fs::read_dir(&comments)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let stored = std::fs::read_to_string(comment).unwrap();
    for gone in [
        "synthetic_comment_boundary",
        "synthetic_comment_glued",
        "synthetic_comment_pem",
        "synthetic_comment_quote",
    ] {
        assert!(!stored.contains(gone), "{gone}: {stored}");
    }
    assert!(stored.contains("[REDACTED]"), "{stored}");
}

/// Round 2: control characters reach neither the stored issue nor
/// the PM's inbox line.
#[test]
fn report_strips_control_chars() {
    let s = ReportFx::new();
    let d = TestDaemon::start_on(s.state.clone());
    d.register_inbox("pm");
    std::fs::write(
        s.pm_dir.join("cadence").join("team.yaml"),
        "roles:\n  pm:\n    kind: inbox\n    alias: pm\n",
    )
    .unwrap();
    // --file: argv cannot carry `\x00` at all — the OS rejects it
    // before cadence reads it.
    let body_file = s._tmp.path().join("body.txt");
    std::fs::write(
        &body_file,
        "crash\x1b[2J here\n\x1b]0;pwned\x07second\x00l\n",
    )
    .unwrap();
    let (ok, out) = s.cli(&[
        "report",
        "--kind",
        "bug",
        "--file",
        body_file.to_str().unwrap(),
    ]);
    assert!(ok, "{out}");
    let stored = s.issue_body("cadence", out["id"].as_str().unwrap());
    assert!(
        !stored.contains('\x1b') && !stored.contains('\x07') && !stored.contains('\x00'),
        "{stored}"
    );
    let msgs = d.rpc("agent_inbox", json!({"alias": "pm"})).unwrap();
    let line = msgs["messages"][0]["body"].as_str().unwrap().to_string();
    assert!(!line.chars().any(|c| c.is_control()), "{line:?}");
}

/// Round 2: a body over the 32 KB cap is refused with the cap named;
/// a first line over 200 chars becomes a capped title, not a 300-char
/// board row.
#[test]
fn report_caps_body_and_title() {
    let s = ReportFx::new();
    let big = "x".repeat(33 * 1024);
    let (_, stderr, (ok, _)) = s.cli_at_env(&s.product_repo, &["report", "-m", &big], &[]);
    assert!(!ok && stderr.contains("32 KB"), "{stderr}");

    let long_title = "a".repeat(300);
    let (ok, out) = s.cli(&["report", "-m", &format!("{long_title}\nrest")]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();
    let (_, out) = s.cli(&["report", "show", &id]);
    assert_eq!(out["title"].as_str().unwrap().chars().count(), 200);
}

/// Round 2: `intake` + kind are system vocabulary — a project with a
/// declared `tags:` allowlist still takes reports.
#[test]
fn report_ignores_project_tag_allowlist() {
    let s = ReportFx::new();
    let repo = s
        .product_repo
        .canonicalize()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let (ok, out) = s.cli(&[
        "issue", "project", "add", "strict", "--prefix", "S", "--repo", &repo, "--tag", "triage",
    ]);
    assert!(ok, "{out}");
    let (ok, out) = s.cli(&[
        "report",
        "--kind",
        "idea",
        "--project",
        "strict",
        "-m",
        "an idea",
    ]);
    assert!(ok && out["project"] == "strict", "{out}");
    let stored = s.issue_body("strict", "S-1");
    assert!(
        stored.contains("- intake") && stored.contains("- idea"),
        "{stored}"
    );
}

/// Round 2: the kind is its own frontmatter field — an extra tag
/// sorting ahead of it cannot mislabel `ls`/`show`.
#[test]
fn report_kind_survives_extra_tags() {
    let s = ReportFx::new();
    let (ok, out) = s.cli_at(&s.product_repo, &["report", "--kind", "bug", "-m", "x"]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();
    let (ok, out) = s.cli(&["issue", "tag", &id, "add", "aaa-first"]);
    assert!(ok, "{out}");
    let (_, out) = s.cli(&["report", "ls", "--kind", "bug"]);
    assert_eq!(out["count"], 1, "{out}");
    assert_eq!(out["reports"][0]["kind"], "bug");
    let (_, out) = s.cli(&["report", "show", &id]);
    assert_eq!(out["kind"], "bug", "{out}");
    let stored = s.issue_body("cadence", &id);
    assert!(stored.contains("kind: bug"), "{stored}");
}

/// Round 2: `report ls` reads the derived status — a verdict note
/// derives `done` while frontmatter still says `backlog`, and `ls`
/// agrees with the overview.
#[test]
fn report_ls_uses_derived_status() {
    let s = ReportFx::new();
    let (ok, out) = s.cli_at(&s.product_repo, &["report", "--kind", "bug", "-m", "x"]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();
    std::fs::write(
        s.notes_dir.join("20260101-000000-t-verdict.md"),
        format!("# Close-out\n> Issue: `{id}`\n\n## Verdict\npass\n"),
    )
    .unwrap();
    let (_, out) = s.cli(&["report", "ls"]);
    assert_eq!(out["count"], 0, "{out}");
    // The file still says backlog — `ls` followed the derived status.
    let stored = s.issue_body("cadence", &id);
    assert!(stored.contains("status: backlog"), "{stored}");
}

/// Round 2: `needs_me` caps intake rows at NEEDS_ME_CAP plus one
/// summary row — a flood cannot bury real work.
#[test]
fn report_needs_me_caps_intake_rows() {
    let s = ReportFx::new();
    for i in 0..12 {
        let (ok, out) = s.cli_at(
            &s.product_repo,
            &["report", "--kind", "bug", "-m", &format!("bug {i}")],
        );
        assert!(ok, "{out}");
    }
    let view = overview_at(&s.home, &s.state, Some(&s.pm_dir), &[]);
    let intake: Vec<&Value> = view["needs_me"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["kind"] == "intake")
        .collect();
    assert_eq!(intake.len(), 11, "{intake:?}");
    assert!(
        intake.iter().any(|n| n["title"]
            .as_str()
            .unwrap()
            .contains("2 more intake reports")),
        "{intake:?}"
    );
}

/// Round 2 nits: `--issue` rejects the flags it would ignore;
/// `report show` refuses non-intake issues.
#[test]
fn report_issue_conflicts_and_show_scope() {
    let s = ReportFx::new();
    let (ok, out) = s.cli(&["issue", "new", "Target", "--project", "product"]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();

    let (ok, text) = cli_raw_at(
        &s,
        &s.product_repo,
        &["report", "--issue", &id, "--project", "cadence", "-m", "x"],
    );
    assert!(!ok && text.contains("--project"), "{text}");
    let (ok, text) = cli_raw_at(
        &s,
        &s.product_repo,
        &["report", "--issue", &id, "--priority", "P0", "-m", "x"],
    );
    assert!(!ok && text.contains("--priority"), "{text}");

    let (ok, _, (_, out)) = s.cli_at_env(&s.product_repo, &["report", "show", &id], &[]);
    assert!(!ok, "{out}");
}

// ==================== persistent monitors (CAD-176) ====================

#[test]
fn monitor_migration_from_v6_bridges_provider_effort_before_v8_v9_and_v10() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("cadence.sqlite3");
    let store = Store::open(&path).unwrap();
    drop(store);
    // Model a v6 database before either change: CAD-176 must reserve the v7
    // column contract before advancing directly to its v8 tables.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "DROP TABLE monitor_alerts;
         DROP TABLE monitor_tasks;
         DROP TABLE monitors;
         ALTER TABLE agents DROP COLUMN effort;
         UPDATE schema_version SET version=6;",
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&path).unwrap();
    assert!(store.monitors().unwrap().is_empty());
    let conn = rusqlite::Connection::open(&path).unwrap();
    let columns: Vec<String> = conn
        .prepare("PRAGMA table_info(agents)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let version: i64 = conn
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 11);
    assert!(columns.iter().any(|column| column == "effort"));
    let monitor_columns: Vec<String> = conn
        .prepare("PRAGMA table_info(monitors)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(monitor_columns
        .iter()
        .any(|column| column == "auto_dispatch_enabled"));
}

#[test]
fn monitor_migration_after_provider_effort_v7_is_v8_v9_and_v10() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("cadence.sqlite3");
    let store = Store::open(&path).unwrap();
    drop(store);
    // Model PR80 first: v7 owns the effort column and CAD-176 owns the next
    // slot. Reopening must add monitors without touching the v7 contract.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "DROP TABLE monitor_alerts;
         DROP TABLE monitor_tasks;
         DROP TABLE monitors;
         UPDATE schema_version SET version=7;",
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&path).unwrap();
    assert!(store.monitors().unwrap().is_empty());
    let conn = rusqlite::Connection::open(&path).unwrap();
    let columns: Vec<String> = conn
        .prepare("PRAGMA table_info(agents)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let version: i64 = conn
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 11);
    assert!(columns.iter().any(|column| column == "effort"));
    let monitor_columns: Vec<String> = conn
        .prepare("PRAGMA table_info(monitors)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(monitor_columns
        .iter()
        .any(|column| column == "auto_dispatch_enabled"));
}

#[test]
fn monitor_migration_from_v8_defaults_auto_dispatch_off() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("cadence.sqlite3");
    let store = Store::open(&path).unwrap();
    drop(store);
    // Model a v8 store with an existing manual registration. The new
    // background bit must be added as an explicit opt-in and default off;
    // upgrading an old manual monitor must not start a scheduler.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "INSERT INTO monitors(
             id,project,owner,interval_secs,state,next_check_at,event_cursor,
             delivery_configured,delivery_state,dispatch_enabled,error,created,updated)
         VALUES('legacy','repo','operator',60,'active',NULL,0,0,'unconfigured',1,NULL,0,0)",
        [],
    )
    .unwrap();
    conn.execute_batch(
        "ALTER TABLE monitors DROP COLUMN auto_dispatch_enabled;
         UPDATE schema_version SET version=8;",
    )
    .unwrap();
    drop(conn);

    let store = Store::open(&path).unwrap();
    assert!(!store.monitor("legacy").unwrap().auto_dispatch_enabled);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = conn
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 11);
}

#[test]
fn monitor_migration_repairs_legacy_pr100_schema9_without_quota() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("cadence.sqlite3");
    let store = Store::open(&path).unwrap();
    drop(store);
    // An older PR100 candidate used v9 for the monitor consent column. A
    // provider-quota migration landing afterwards must not skip its agent
    // column merely because the shared version marker already says 9.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "ALTER TABLE agents DROP COLUMN quota;
         UPDATE schema_version SET version=9;",
    )
    .unwrap();
    drop(conn);

    let store = Store::open(&path).unwrap();
    assert!(store.monitors().unwrap().is_empty());
    let conn = rusqlite::Connection::open(&path).unwrap();
    let agent_columns: Vec<String> = conn
        .prepare("PRAGMA table_info(agents)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let monitor_columns: Vec<String> = conn
        .prepare("PRAGMA table_info(monitors)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let version: i64 = conn
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 11);
    assert!(agent_columns.iter().any(|column| column == "quota"));
    assert!(monitor_columns
        .iter()
        .any(|column| column == "auto_dispatch_enabled"));
}

#[test]
fn monitor_check_failure_is_degraded_without_healthy_claim() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("degraded-monitor.md", "check failure");
    let project = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "job_new",
        json!({"pm": "pm", "job": "badjob", "spec": spec,
               "spec_sha256": sha, "repo": project}),
    )
    .unwrap();
    d.rpc(
        "task_new",
        json!({"job": "badjob", "task": "badjob-watch", "assignee": "w1",
               "acceptance": "observe failures"}),
    )
    .unwrap();
    d.rpc(
        "monitor_register",
        json!({"monitor": "bad", "project": project, "owner": "operator",
               "tasks": ["badjob-watch"], "interval_secs": 1}),
    )
    .unwrap();
    let active = wait_monitor_state(&d, "bad", "active", 5);
    let last_success = active["last_success_at"].as_f64().unwrap();
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    // Invalid event evidence makes the check fail; it must remain visible as
    // degraded instead of being treated as a healthy empty scan.
    conn.execute(
        "INSERT INTO events(alias,kind,payload,job_id,task_id,at)
         VALUES(?,?,?,?,?,?)",
        rusqlite::params!["w1", "turn_finished", "{}", "badjob", "badjob-watch", "bad"],
    )
    .unwrap();
    drop(conn);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let monitor = d.rpc("monitor_show", json!({"monitor": "bad"})).unwrap()["monitor"].clone();
        if monitor["monitoring"] == "degraded" {
            assert!(
                monitor["error"].as_str().is_some_and(|e| !e.is_empty()),
                "{monitor}"
            );
            assert_eq!(
                monitor["last_success_at"].as_f64(),
                Some(last_success),
                "{monitor}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "monitor did not degrade: {monitor}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_monitor_state(d: &TestDaemon, monitor: &str, want: &str, secs: u64) -> Value {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let value = d.rpc("monitor_show", json!({"monitor": monitor})).unwrap()["monitor"].clone();
        if value["monitoring"].as_str() == Some(want) {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "monitor {monitor} never reached {want}: {value}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn monitor_persists_coverage_heartbeats_and_deduplicates_alerts() {
    let mut d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("monitor-spec.md", "watch this task");
    let project = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "job_new",
        json!({"pm": "pm", "job": "mjob", "spec": spec,
               "spec_sha256": sha, "repo": project}),
    )
    .unwrap();
    d.rpc(
        "task_new",
        json!({"job": "mjob", "task": "mjob-watch", "assignee": "w1",
               "acceptance": "observe the task"}),
    )
    .unwrap();
    let registered = d
        .rpc(
            "monitor_register",
            json!({"monitor": "m1", "project": project,
                   "owner": "operator", "tasks": ["mjob-watch"],
                   "interval_secs": 1}),
        )
        .unwrap();
    assert_eq!(
        registered["monitor"]["monitoring"], "degraded",
        "{registered}"
    );
    assert_eq!(registered["monitor"]["delivery"]["configured"], false);
    assert_eq!(registered["monitor"]["coverage"], json!(["mjob-watch"]));
    let active = wait_monitor_state(&d, "m1", "active", 5);
    assert!(active["last_success_at"].is_number(), "{active}");
    assert!(active["heartbeat_at"].is_number(), "{active}");
    assert!(active["next_check_at"].is_number(), "{active}");

    // Receipt-only task events do not manufacture an alert. The monitor
    // still advances its cursor, but no worker health is inferred.
    let store = Store::open(&d.state.join("cadence.sqlite3")).unwrap();
    store
        .event_public_scoped(
            "w1",
            "turn_finished",
            json!({"message": "m-finished"}),
            Some("mjob"),
            Some("mjob-watch"),
        )
        .unwrap();
    thread::sleep(Duration::from_millis(1200));
    let no_alert = d.rpc("monitor_alerts", json!({"monitor": "m1"})).unwrap();
    assert!(
        no_alert["alerts"].as_array().unwrap().is_empty(),
        "{no_alert}"
    );

    // Inject one concrete task-scoped stall observation through the same
    // durable event table the daemon consumes.
    store
        .event_public_scoped(
            "w1",
            "turn_stalled",
            json!({"message": "m-stall", "episode": 1}),
            Some("mjob"),
            Some("mjob-watch"),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let alert = loop {
        let page = d.rpc("monitor_alerts", json!({"monitor": "m1"})).unwrap();
        if let Some(alert) = page["alerts"].as_array().and_then(|a| a.first()) {
            break alert.clone();
        }
        assert!(Instant::now() < deadline, "monitor did not alert: {page}");
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(alert["kind"], "turn_stalled", "{alert}");
    assert_eq!(alert["task"], "mjob-watch");
    assert_eq!(alert["state"], "open");
    let cursor = d.rpc("monitor_show", json!({"monitor": "m1"})).unwrap()["monitor"]
        ["event_cursor"]
        .as_i64()
        .unwrap();
    thread::sleep(Duration::from_millis(1200));
    let again = d.rpc("monitor_alerts", json!({"monitor": "m1"})).unwrap();
    assert_eq!(again["alerts"].as_array().unwrap().len(), 1, "{again}");
    assert!(
        again["alerts"][0]["event_seq"].as_i64().unwrap() <= cursor,
        "alert event must be at or behind the durable monitor cursor: {again}"
    );

    let acked = d
        .rpc(
            "monitor_alert_ack",
            json!({"monitor": "m1", "alert": alert["seq"], "by": "operator"}),
        )
        .unwrap();
    assert_eq!(acked["alert"]["state"], "acknowledged", "{acked}");
    let open = d
        .rpc("monitor_alerts", json!({"monitor": "m1", "open": true}))
        .unwrap();
    assert!(open["alerts"].as_array().unwrap().is_empty(), "{open}");

    // The registration and cursor survive a daemon restart; the observed
    // event is not replayed as a second alert.
    let state = d.state.clone();
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let d2 = TestDaemon::start_on(state);
    let restored = wait_monitor_state(&d2, "m1", "active", 5);
    assert_eq!(restored["coverage"], json!(["mjob-watch"]));
    let after_restart = d2.rpc("monitor_alerts", json!({"monitor": "m1"})).unwrap();
    assert_eq!(after_restart["alerts"].as_array().unwrap().len(), 1);
    let _ = d2.rpc("monitor_stop", json!({"monitor": "m1"}));
}

#[test]
fn monitor_alerts_task_unknown_outcome_is_scoped_and_restart_safe() {
    let mut d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("unknown-monitor-spec.md", "inspect uncertain work");
    let project = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "job_new",
        json!({"pm": "pm", "job": "unknown-job", "spec": spec,
               "spec_sha256": sha, "repo": project}),
    )
    .unwrap();
    d.rpc(
        "task_new",
        json!({"job": "unknown-job", "task": "unknown-task", "assignee": "w1",
               "acceptance": "inspect uncertain work"}),
    )
    .unwrap();
    d.rpc(
        "monitor_register",
        json!({"monitor": "unknown-monitor", "project": project,
               "owner": "operator", "tasks": ["unknown-task"],
               "interval_secs": 1}),
    )
    .unwrap();
    wait_monitor_state(&d, "unknown-monitor", "active", 5);

    // Stop before dispatch. The kickoff stays queued on the stopped worker
    // until its body is the fake provider's DISCONNECT fixture and resume
    // starts the actor. Opening Store here would run crash recovery against
    // the live daemon, so the body rewrite uses a plain connection.
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    let stopped = d.wait_agent("w1", "stopped", 10);
    assert_eq!(stopped["enabled"], false, "{stopped}");
    assert!(stopped["endpoint"].is_null(), "{stopped}");
    let dispatched = d
        .rpc("task_dispatch", json!({"task": "unknown-task"}))
        .unwrap();
    assert_eq!(dispatched["duplicate"], false, "{dispatched}");
    let kickoff = dispatched["message"].as_str().unwrap().to_string();
    let queued = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["id"] == kickoff)
        .unwrap()
        .clone();
    assert_eq!(queued["state"], "queued", "{queued}");
    assert_eq!(queued["source"], "job_dispatch", "{queued}");
    assert_eq!(queued["task_id"], "unknown-task", "{queued}");
    let still_stopped = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(still_stopped["state"], "stopped", "{still_stopped}");
    assert_eq!(still_stopped["enabled"], false, "{still_stopped}");
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE messages SET body='DISCONNECT' WHERE id=?",
        rusqlite::params![kickoff],
    )
    .unwrap();
    drop(conn);
    let armed = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["id"] == kickoff)
        .unwrap()
        .clone();
    assert_eq!(armed["state"], "queued", "{armed}");
    assert_eq!(armed["body"], "DISCONNECT", "{armed}");
    assert_eq!(
        d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"]["state"],
        "stopped"
    );
    d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();

    let message = d.wait_message("w1", &kickoff, &["unknown"], 15);
    assert_eq!(message["state"], "unknown", "{message}");
    assert_eq!(message["source"], "job_dispatch", "{message}");
    assert_eq!(
        message["error"], "Connection lost during turn; provider outcome is unknown",
        "{message}"
    );
    assert_eq!(message["result"]["status"], "unknown", "{message}");
    assert!(
        message["turn_id"]
            .as_str()
            .unwrap()
            .starts_with("fake-turn-"),
        "{message}"
    );
    assert_eq!(d.task_state("unknown-task"), "running");
    let task = d.rpc("task_show", json!({"task": "unknown-task"})).unwrap()["task"].clone();
    assert_eq!(task["revision"], 1, "{task}");
    assert!(task["head_sha"].is_null(), "{task}");
    d.wait_agent("w1", "attention", 10);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "after", "message": "after-unknown"}),
    )
    .unwrap();
    thread::sleep(Duration::from_millis(400));
    assert_eq!(d.message_state("w1", "after-unknown"), "queued");
    assert_eq!(d.message_state("w1", &kickoff), "unknown");

    let deadline = Instant::now() + Duration::from_secs(5);
    let alert = loop {
        let page = d
            .rpc(
                "monitor_alerts",
                json!({"monitor": "unknown-monitor", "open": true}),
            )
            .unwrap();
        if let Some(alert) = page["alerts"].as_array().and_then(|alerts| alerts.first()) {
            break alert.clone();
        }
        assert!(Instant::now() < deadline, "monitor did not alert: {page}");
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(alert["kind"], "turn_unknown", "{alert}");
    assert_eq!(alert["task"], "unknown-task", "{alert}");
    assert_eq!(alert["state"], "open", "{alert}");
    let evidence = &alert["payload"]["payload"];
    assert_eq!(evidence["message"], kickoff);
    assert_eq!(
        evidence["reason"],
        "Connection lost during turn; provider outcome is unknown"
    );
    assert_eq!(evidence["owner"], "operator");
    assert!(evidence["next_action"]
        .as_str()
        .unwrap()
        .contains("reconcil"));
    let events = d
        .rpc("job_events", json!({"job": "unknown-job", "tail": true}))
        .unwrap()["events"]
        .as_array()
        .unwrap()
        .clone();
    let unknown_events: Vec<_> = events
        .iter()
        .filter(|event| event["kind"] == "turn_unknown")
        .collect();
    assert_eq!(unknown_events.len(), 1, "{events:?}");
    assert_eq!(unknown_events[0]["job_id"], "unknown-job");
    assert_eq!(unknown_events[0]["task_id"], "unknown-task");
    assert!(
        event_kinds(&d, "w1")
            .iter()
            .any(|kind| kind == "turn_finished"),
        "compatibility turn_finished stays on the agent stream"
    );
    let alert_seq = alert["seq"].clone();
    let fingerprint = alert["fingerprint"].clone();

    // The cursor and event fingerprint make repeated monitor ticks one alert.
    thread::sleep(Duration::from_millis(2200));
    let repeated = d
        .rpc(
            "monitor_alerts",
            json!({"monitor": "unknown-monitor", "open": true}),
        )
        .unwrap();
    assert_eq!(
        repeated["alerts"].as_array().unwrap().len(),
        1,
        "{repeated}"
    );
    assert_eq!(repeated["alerts"][0]["seq"], alert_seq, "{repeated}");
    assert_eq!(
        repeated["alerts"][0]["fingerprint"], fingerprint,
        "{repeated}"
    );
    // A same-kind event without task scope is outside this monitor's fixed
    // coverage and must remain unmonitored. A raw insert avoids Store::open,
    // whose recovery would rewrite the live daemon.
    let at = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "INSERT INTO events(alias,kind,payload,job_id,task_id,at)
         VALUES('w1','turn_unknown',?,NULL,NULL,?)",
        rusqlite::params![r#"{"reason":"unscoped"}"#, at],
    )
    .unwrap();
    drop(conn);
    thread::sleep(Duration::from_millis(1200));
    let unscoped = d
        .rpc(
            "monitor_alerts",
            json!({"monitor": "unknown-monitor", "open": true}),
        )
        .unwrap();
    assert_eq!(
        unscoped["alerts"].as_array().unwrap().len(),
        1,
        "{unscoped}"
    );
    assert_eq!(
        unscoped["alerts"][0]["fingerprint"], fingerprint,
        "{unscoped}"
    );
    assert_eq!(
        event_kinds(&d, "w1")
            .iter()
            .filter(|kind| kind.as_str() == "turn_unknown")
            .count(),
        2,
        "scoped finish plus the unscoped insert"
    );
    let scoped = d
        .rpc("job_events", json!({"job": "unknown-job", "tail": true}))
        .unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "turn_unknown")
        .count();
    assert_eq!(scoped, 1);
    assert_eq!(d.task_state("unknown-task"), "running");
    assert_eq!(d.message_state("w1", &kickoff), "unknown");

    // Restarting restores the monitor cursor and keeps the same open alert;
    // the unknown attempt remains fenced and the task never reaches review.
    let state = d.state.clone();
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let d2 = TestDaemon::start_on(state);
    wait_monitor_state(&d2, "unknown-monitor", "active", 5);
    thread::sleep(Duration::from_millis(1200));
    let restored = d2
        .rpc(
            "monitor_alerts",
            json!({"monitor": "unknown-monitor", "open": true}),
        )
        .unwrap();
    assert_eq!(
        restored["alerts"].as_array().unwrap().len(),
        1,
        "{restored}"
    );
    assert_eq!(
        restored["alerts"][0]["fingerprint"], fingerprint,
        "{restored}"
    );
    assert_eq!(restored["alerts"][0]["seq"], alert_seq, "{restored}");
    assert_eq!(d2.message_state("w1", &kickoff), "unknown");
    assert_eq!(d2.message_state("w1", "after-unknown"), "queued");
    assert_eq!(d2.task_state("unknown-task"), "running");
    let restored_task = d2
        .rpc("task_show", json!({"task": "unknown-task"}))
        .unwrap()["task"]
        .clone();
    assert_eq!(restored_task["revision"], 1, "{restored_task}");
    assert!(restored_task["head_sha"].is_null(), "{restored_task}");
    d2.wait_agent("w1", "attention", 5);
    let _ = d2.rpc("monitor_stop", json!({"monitor": "unknown-monitor"}));
}

#[test]
fn monitor_dispatch_requires_explicit_safe_eligibility() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.register_member("w2", "pm");
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    d.wait_agent("w2", "idle", 10);
    let (spec, sha) = d.spec_file("dispatch-monitor.md", "safe dispatch");
    let project = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "job_new",
        json!({"pm": "pm", "job": "djob", "spec": spec,
               "spec_sha256": sha, "repo": project}),
    )
    .unwrap();
    d.rpc(
        "task_new",
        json!({"job": "djob", "task": "djob-ready", "assignee": "w1",
               "acceptance": "run focused checks"}),
    )
    .unwrap();
    d.rpc(
        "task_new",
        json!({"job": "djob", "task": "djob-repeat", "assignee": "w2",
               "acceptance": "reuse the live kickoff"}),
    )
    .unwrap();
    d.rpc(
        "monitor_register",
        json!({"monitor": "dm", "project": project, "owner": "operator",
               "tasks": ["djob-ready", "djob-repeat"], "interval_secs": 1,
               "dispatch_enabled": true}),
    )
    .unwrap();
    wait_monitor_state(&d, "dm", "active", 5);
    // The legacy manual permission remains inert under the background
    // watcher. Automatic reconciliation requires its separate opt-in bit.
    thread::sleep(Duration::from_millis(1200));
    assert_eq!(d.task_state("djob-ready"), "draft");
    let before_manual = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(
        before_manual["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["source"] == "job_dispatch")
            .count(),
        0
    );

    let result = d
        .rpc(
            "monitor_dispatch",
            json!({"monitor": "dm", "task": "djob-ready"}),
        )
        .unwrap();
    assert_eq!(result["duplicate"], false, "{result}");
    assert_eq!(result["task"]["state"], "dispatched", "{result}");
    let kickoff = result["message"].as_str().unwrap().to_string();
    // Seed a second task with an existing queued kickoff while its worker is
    // stopped. The monitor retry must take the duplicate-only branch even
    // though a fresh dispatch would fail the live-worker eligibility gate.
    d.rpc("agent_stop", json!({"alias": "w2"})).unwrap();
    d.wait_agent("w2", "stopped", 10);
    let store = Store::open(&d.state.join("cadence.sqlite3")).unwrap();
    let (_, existing_kickoff, existing_duplicate, _) = store
        .dispatch_task("djob-repeat", None, None, "operator")
        .unwrap();
    assert!(!existing_duplicate);
    let duplicate = d
        .rpc(
            "monitor_dispatch",
            json!({"monitor": "dm", "task": "djob-repeat"}),
        )
        .unwrap();
    assert_eq!(duplicate["duplicate"], true, "{duplicate}");
    assert_eq!(duplicate["message"], existing_kickoff);
    let messages = d.rpc("agent_show", json!({"alias": "w2"})).unwrap();
    assert_eq!(
        messages["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["source"] == "job_dispatch")
            .count(),
        1,
        "{messages}"
    );
    assert_ne!(kickoff, existing_kickoff);
}

#[test]
fn automatic_monitor_dispatch_is_separate_guarded_and_restart_safe() {
    let mut d = TestDaemon::start();
    d.register("pm");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    let caller_quota_at = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    for (alias, params) in [
        ("w1", json!({"upstream": "pm"})),
        // These caller-owned values must be ignored by automatic admission;
        // the trusted fixture rows are seeded below after provider open.
        (
            "w2",
            json!({"upstream": "pm", "quota": {
                "source": "provider", "agent": "w2", "observed_at": caller_quota_at,
                "state": "available", "remaining": 0
            }}),
        ),
        (
            "w3",
            json!({"upstream": "pm", "quota": {
                "source": "provider", "agent": "w3", "observed_at": caller_quota_at,
                "state": "available", "remaining": 4
            }}),
        ),
    ] {
        d.rpc(
            "agent_register",
            json!({"alias": alias, "provider": "fake", "endpoint_kind": "fake",
                   "cwd": cwd, "params": params.to_string()}),
        )
        .unwrap();
    }
    for alias in ["pm", "w1", "w2", "w3"] {
        d.wait_agent(alias, "idle", 10);
    }
    let quota_now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    seed_provider_quota(&d, "w2", quota_now);
    seed_provider_quota(&d, "w3", quota_now - 301);
    // Leave one durable kickoff queued for a stopped worker. The automatic
    // retry must reuse it, proving the existing duplicate-only branch is the
    // idempotency boundary rather than minting another revision.
    let (spec, sha) = d.spec_file("automatic-monitor.md", "coordinator test");
    let project = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "job_new",
        json!({"pm": "pm", "job": "ajob", "spec": spec,
               "spec_sha256": sha, "repo": project}),
    )
    .unwrap();
    for (task, assignee) in [
        ("ajob-fresh", "w2"),
        ("ajob-duplicate", "w1"),
        ("ajob-blocked", "w3"),
    ] {
        d.rpc(
            "task_new",
            json!({"job": "ajob", "task": task, "assignee": assignee,
                   "acceptance": "run the focused coordinator checks"}),
        )
        .unwrap();
    }
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "stopped", 10);
    let existing = d
        .rpc(
            "task_dispatch",
            json!({"task": "ajob-duplicate", "by": "operator"}),
        )
        .unwrap();
    let existing_kickoff = existing["message"].as_str().unwrap().to_string();
    assert_eq!(existing["duplicate"], false);

    let invalid = d.rpc(
        "monitor_register",
        json!({"monitor": "auto-invalid", "project": project,
               "owner": "operator", "tasks": ["ajob-fresh"],
               "interval_secs": 1, "auto_dispatch_enabled": true}),
    );
    assert!(invalid
        .unwrap_err()
        .to_string()
        .contains("separate manual dispatch permission"));

    let registered = d
        .rpc(
            "monitor_register",
            json!({"monitor": "auto", "project": project,
                   "owner": "operator", "tasks": ["ajob-blocked", "ajob-duplicate", "ajob-fresh"],
                   "interval_secs": 1, "dispatch_enabled": true,
                   "auto_dispatch_enabled": true}),
        )
        .unwrap();
    assert_eq!(registered["monitor"]["dispatch_enabled"], true);
    assert_eq!(registered["monitor"]["auto_dispatch_enabled"], true);
    wait_monitor_state(&d, "auto", "active", 5);

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let show = d.rpc("agent_show", json!({"alias": "w2"})).unwrap();
        let dispatched = show["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["source"] == "job_dispatch")
            .count();
        if dispatched == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "automatic task was not dispatched: {show}; monitor={}; events={}",
            d.rpc("monitor_show", json!({"monitor": "auto"})).unwrap(),
            d.rpc("agent_events", json!({"alias": "w2"})).unwrap()
        );
        thread::sleep(Duration::from_millis(50));
    }
    let w2 = d.rpc("agent_show", json!({"alias": "w2"})).unwrap();
    assert_eq!(
        w2["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["source"] == "job_dispatch")
            .count(),
        1,
        "fresh automatic dispatch must mint one kickoff"
    );

    let blocked_deadline = Instant::now() + Duration::from_secs(5);
    let blocked = loop {
        let page = d.rpc("monitor_alerts", json!({"monitor": "auto"})).unwrap();
        if let Some(alert) = page["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|alert| alert["kind"] == "dispatch_blocked")
        {
            break alert.clone();
        }
        assert!(
            Instant::now() < blocked_deadline,
            "missing automatic block alert: {page}"
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(blocked["task"], "ajob-blocked");
    assert!(blocked["last_error"]
        .as_str()
        .unwrap()
        .contains("quota unknown"));
    assert!(blocked["payload"]["next_action"].is_string());
    let blocked_seq = blocked["seq"].as_i64().unwrap();
    thread::sleep(Duration::from_millis(1500));
    let repeated = d.rpc("monitor_alerts", json!({"monitor": "auto"})).unwrap();
    let alerts = repeated["alerts"].as_array().unwrap();
    assert_eq!(
        alerts
            .iter()
            .filter(|alert| alert["kind"] == "dispatch_blocked")
            .count(),
        1
    );
    assert_eq!(alerts[0]["seq"], blocked_seq);
    assert!(alerts[0]["attempts"].as_i64().unwrap() >= 2);

    // Provider evidence can arrive after a guarded refusal. The next
    // automatic attempt must reuse the same alert row and resolve it when
    // the durable kickoff is committed; a successful dispatch is not a new
    // alert and does not leave a stale open blocker behind.
    let refreshed_quota_at = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    seed_provider_quota(&d, "w3", refreshed_quota_at);
    let store = Store::open(&d.state.join("cadence.sqlite3")).unwrap();
    // Keep the fixture's worker lifecycle evidence explicit while changing
    // only the provider allowance sample.
    store.set_enabled("w3", true).unwrap();
    store.set_agent_state("w3", "idle", None).unwrap();
    let resolved_deadline = Instant::now() + Duration::from_secs(5);
    let resolved = loop {
        let page = d.rpc("monitor_alerts", json!({"monitor": "auto"})).unwrap();
        if let Some(alert) = page["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|alert| alert["seq"] == blocked_seq && alert["state"] == "resolved")
        {
            break alert.clone();
        }
        assert!(
            Instant::now() < resolved_deadline,
            "automatic dispatch did not resolve the prior block: {page}"
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        resolved["payload"]["resolution"],
        "automatic dispatch succeeded"
    );
    assert!(resolved["last_error"].is_null(), "{resolved}");
    let open_after_resolution = d
        .rpc("monitor_alerts", json!({"monitor": "auto", "open": true}))
        .unwrap();
    assert!(
        open_after_resolution["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|alert| alert["task"] != "ajob-blocked"),
        "resolved dispatch blocker must leave no open alert: {open_after_resolution}"
    );

    // The pre-existing kickoff is reused after a monitor tick and restart;
    // no second job_dispatch message appears for the stopped worker.
    let duplicate = d
        .rpc(
            "monitor_dispatch",
            json!({"monitor": "auto", "task": "ajob-duplicate"}),
        )
        .unwrap();
    assert_eq!(duplicate["duplicate"], true);
    assert_eq!(duplicate["message"], existing_kickoff);
    let state = d.state.clone();
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let d2 = TestDaemon::start_on(state);
    wait_monitor_state(&d2, "auto", "active", 5);
    thread::sleep(Duration::from_millis(1200));
    let w1 = d2.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(
        w1["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["source"] == "job_dispatch")
            .count(),
        1
    );
    let restored = d2
        .rpc("monitor_alerts", json!({"monitor": "auto"}))
        .unwrap();
    assert!(restored["alerts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|alert| { alert["seq"] == blocked_seq && alert["kind"] == "dispatch_blocked" }));
    let _ = d2.rpc("monitor_stop", json!({"monitor": "auto"}));
}

#[test]
fn automatic_monitor_dispatch_serializes_competing_callers() {
    let mut d = TestDaemon::start();
    d.register("pm");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "agent_register",
        json!({"alias": "w1", "provider": "fake", "endpoint_kind": "fake",
               "cwd": cwd, "params": json!({"upstream": "pm", "quota":
                   {"source": "provider", "state": "available"}}).to_string()}),
    )
    .unwrap();
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    let quota_at = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    seed_provider_quota(&d, "w1", quota_at);
    let (spec, sha) = d.spec_file("automatic-race.md", "serialize dispatch");
    let project = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "job_new",
        json!({"pm": "pm", "job": "race-job", "spec": spec,
               "spec_sha256": sha, "repo": project}),
    )
    .unwrap();
    d.rpc(
        "task_new",
        json!({"job": "race-job", "task": "race-task", "assignee": "w1",
               "acceptance": "serialize automatic dispatch"}),
    )
    .unwrap();
    d.rpc(
        "monitor_register",
        json!({"monitor": "race-monitor", "project": project, "owner": "operator",
               "tasks": ["race-task"], "interval_secs": 60,
               "dispatch_enabled": true, "auto_dispatch_enabled": true}),
    )
    .unwrap();

    // Stop the watcher before making the competing calls. The transaction
    // under test still sees an active monitor after this explicit check, but
    // no background tick can win the race or hide the two callers' result.
    let state = d.state.clone();
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let store = Arc::new(Store::open(&state.join("cadence.sqlite3")).unwrap());
    let at = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    store.check_monitor("race-monitor", at).unwrap();
    store.set_enabled("w1", true).unwrap();
    store.set_agent_state("w1", "idle", None).unwrap();

    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let pending = HashSet::new();
            barrier.wait();
            store.dispatch_automatic_monitor_task(
                "race-monitor",
                "race-task",
                &pending,
                "monitor:race-monitor",
            )
        }));
    }
    barrier.wait();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        2,
        "both callers should receive the same durable kickoff: {results:?}"
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| result.as_ref().unwrap().2)
            .count(),
        1,
        "one competing caller must reuse the live kickoff: {results:?}"
    );
    let kickoff_ids: HashSet<_> = results
        .iter()
        .map(|result| result.as_ref().unwrap().1.clone())
        .collect();
    assert_eq!(
        kickoff_ids.len(),
        1,
        "dedupe key must be stable: {results:?}"
    );
    let tasks = store.tasks_for_job("race-job").unwrap();
    assert_eq!(
        tasks
            .iter()
            .filter(|task| task.state == "dispatched")
            .count(),
        1,
        "exactly one task row may be claimed: {tasks:?}"
    );
    assert_eq!(
        tasks
            .iter()
            .map(|task| store.messages_for_task(&task.id).unwrap().len())
            .sum::<usize>(),
        1,
        "the transaction must mint one kickoff"
    );
}
// ---------- CAD-153: cadence audit -------------------------------------

/// A repo whose default branch holds squash-merge subjects `… (#N)`
/// plus the landed-head commits, so `contains_head` can patch-id
/// compare. Returns (repo, notes, report, heads) — `heads[i]` is the
/// headRefOid for PR i+1.
fn audit_repo(dir: &TempDir) -> (PathBuf, PathBuf, PathBuf, Vec<String>) {
    let repo = dir.path().join("repo");
    let notes = dir.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    git_repo(&repo);
    let g = |args: &[&str]| -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    // No global identity on CI runners.
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    let branch = g(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let mut heads = Vec::new();
    let mut prs = Vec::new();
    for n in 1..=3u8 {
        // The PR head: same change on a side branch.
        g(&["checkout", "-qb", &format!("pr{n}")]);
        std::fs::write(repo.join(format!("f{n}.txt")), format!("change {n}")).unwrap();
        g(&["add", "."]);
        g(&["commit", "-qm", &format!("work {n}")]);
        let head = g(&["rev-parse", "HEAD"]);
        // The squash merge: identical change on the default branch.
        g(&["checkout", "-q", &branch]);
        std::fs::write(repo.join(format!("f{n}.txt")), format!("change {n}")).unwrap();
        g(&["add", "."]);
        g(&["commit", "-qm", &format!("work {n} (CAD-{n}) (#{n})")]);
        let merge = g(&["rev-parse", "HEAD"]);
        prs.push(format!(
            r#"{{"number":{n},"title":"work {n} (CAD-{n})","headRefOid":"{head}",
              "mergeCommit":{{"oid":"{merge}"}},"mergedBy":{{"login":"ops-1"}},
              "mergedAt":"2026-09-20T12:00:0{n}Z"}}"#
        ));
        heads.push(head);
    }
    let report = dir.path().join("merge-report.json");
    std::fs::write(
        &report,
        format!("{{\"prs\":[{}],\"statuses\":{{}}}}", prs.join(",")),
    )
    .unwrap();
    (repo, notes, report, heads)
}

/// `cadence audit` fully fixtured — `--merge-report` + `--notes-dir`
/// replace gh and the notes tree, an empty state dir and PM dir keep
/// the daemon store and tracker out.
fn run_audit(
    state: &Path,
    pm: &Path,
    repo: &Path,
    notes: &Path,
    report: &Path,
    extra: &[&str],
) -> std::process::Output {
    run_audit_full(state, pm, repo, Some(notes), Some(report), extra, None)
}

/// The plumbing behind `run_audit`: optional fixture paths (a `None`
/// `--merge-report` exercises the live `gh` path) plus an optional
/// PATH override so tests can remove `gh` entirely.
fn run_audit_full(
    state: &Path,
    pm: &Path,
    repo: &Path,
    notes: Option<&Path>,
    report: Option<&Path>,
    extra: &[&str],
    path_env: Option<&Path>,
) -> std::process::Output {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(state)
        .arg("audit")
        .arg("--repo")
        .arg(repo);
    if let Some(n) = notes {
        cmd.arg("--notes-dir").arg(n);
    }
    if let Some(r) = report {
        cmd.arg("--merge-report").arg(r);
    }
    cmd.args(extra).env("CADENCE_PM_DIR", pm);
    if let Some(p) = path_env {
        cmd.env("PATH", p);
    }
    cmd.output().unwrap()
}

/// A PATH that resolves `git` (the audit shells it constantly) but
/// has no `gh` — proving neither fixture mode nor the live path can
/// accidentally reach the real CLI.
fn path_without_gh(dir: &TempDir) -> PathBuf {
    let bin = dir.path().join("no-gh-bin");
    std::fs::create_dir_all(&bin).unwrap();
    for p in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let git = p.join("git");
        if git.is_file() {
            std::os::unix::fs::symlink(&git, bin.join("git")).unwrap();
            return bin;
        }
    }
    panic!("no git on PATH to link into {bin:?}");
}

fn verdict_note(notes: &Path, name: &str, head: &str, from: &str, class: &str) {
    std::fs::write(
        notes.join(name),
        format!(
            "# Verdict: pass\n> From: `{from}`\n\n## Verdict\npass — head `{head}`\n\n\
             **Risk: {class} (test trigger)**\n\n**What an auditor should check:** the row.\n"
        ),
    )
    .unwrap();
}

#[test]
fn audit_reconstructs_clean_merge() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    // Every PR head carries a pass verdict note + SUCCESS status —
    // the fully clean run. Note filenames (11:59:xx) and status
    // `created_at` predate `mergedAt` 12:00:0n — a post-merge verdict
    // is post-hoc evidence, not a merge-time gate.
    let mut report_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    for (i, h) in heads.iter().enumerate() {
        verdict_note(
            &notes,
            &format!("20260920-1159{i}0-x-p{n}-verdict.md", i = i, n = i + 1),
            h,
            "qa-1",
            "auto",
        );
        report_json["statuses"][h] = json!({
            "statuses": [{
                "context": "qa-verdict", "state": "SUCCESS",
                "created_at": "2026-09-20T11:59:30Z",
                "creator": {"login": "qa-bot"}
            }]
        });
    }
    std::fs::write(&report, report_json.to_string()).unwrap();

    let out = run_audit(&state, &pm, &repo, &notes, &report, &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "clean rows must not flag:\n{text}"
    );
    assert!(text.contains("#1"), "{text}");
    assert!(text.contains("reviewer qa-1"), "{text}");
    assert!(text.contains("merger ops-1"), "{text}");
    assert!(text.contains("contains_head yes"), "{text}");
    assert!(text.contains("class auto"), "{text}");
    assert!(text.contains("trigger test trigger"), "{text}");
    assert!(!text.contains("FLAG"), "{text}");
    // The root commit shows as a `?` row but is exempt from flags —
    // it predates the PR process. Any *later* direct push would flag.
    assert!(text.contains("? init"), "{text}");
    // Read-only: the repo must be byte-identical afterwards.
    assert_eq!(git_porcelain(&repo), "", "audit must not dirty the repo");
}

#[test]
fn audit_flags_reviewer_equals_merger() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    // The note's `From:` is an agent alias — it happens to spell the
    // same string as the merger's GitHub login, which must NOT flag:
    // the namespaces differ.
    verdict_note(
        &notes,
        "20260920-115900-x-p3-verdict.md",
        &heads[2],
        "ops-1",
        "auto",
    );
    let mut report_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    let status = |login: &str| {
        json!({
            heads[2].clone(): {"statuses":[{
                "context":"qa-verdict","state":"SUCCESS",
                "created_at":"2026-09-20T11:59:30Z",
                "creator":{"login":login}
            }]}
        })
    };
    report_json["statuses"] = status("qa-1");
    std::fs::write(&report, report_json.to_string()).unwrap();

    // Alias collision alone: no flag.
    let out = run_audit(&state, &pm, &repo, &notes, &report, &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.contains("reviewer==merger"),
        "note From: alias must not feed the flag:\n{text}"
    );

    // Same GitHub identity posted qa-verdict and merged: flag.
    report_json["statuses"] = status("ops-1");
    std::fs::write(&report, report_json.to_string()).unwrap();
    let out = run_audit(&state, &pm, &repo, &notes, &report, &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "flag must exit 1:\n{text}");
    assert!(text.contains("FLAG[reviewer==merger]"), "{text}");
    assert!(text.contains("reviewer@gh ops-1"), "{text}");
}

#[test]
fn audit_flags_merge_with_no_verdict_on_head() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, _heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    // Empty notes dir, empty statuses — nothing proves a pass.
    let out = run_audit(&state, &pm, &repo, &notes, &report, &["--limit", "1"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "verdict-less head must flag:\n{text}"
    );
    assert!(text.contains("FLAG[no-passing-verdict]"), "{text}");
    assert!(text.contains("verdict unknown"), "{text}");
    assert!(text.contains("reviewer unknown"), "{text}");
    // The reasons must accompany the unknowns.
    let jout = run_audit(
        &state,
        &pm,
        &repo,
        &notes,
        &report,
        &["--limit", "1", "--json"],
    );
    let j: Value = serde_json::from_str(&String::from_utf8_lossy(&jout.stdout)).unwrap();
    let unknowns = j["merges"][0]["unknowns"].as_array().unwrap();
    assert!(
        unknowns.iter().any(|u| u["field"] == "verdict"),
        "unknown verdict needs a reason: {j}"
    );
}

#[test]
fn audit_json_shape_is_stable() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    verdict_note(
        &notes,
        "20260920-115900-x-p1-verdict.md",
        &heads[0],
        "qa-1",
        "auto",
    );

    let out = run_audit(
        &state,
        &pm,
        &repo,
        &notes,
        &report,
        &["--since", "24h", "--json"],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let j: Value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("--json not one document: {e}\n{text}"));
    assert_eq!(j["schema"].as_str().unwrap(), "cadence.audit/1");
    for key in [
        "repo",
        "default_ref",
        "since",
        "filters",
        "merges",
        "summary",
    ] {
        assert!(j.get(key).is_some(), "missing top-level {key}: {j}");
    }
    let m = &j["merges"][0];
    for key in [
        "pr",
        "title",
        "merge_sha",
        "landed_head",
        "reviewed_head",
        "contains_head",
        "qa_verdict_status",
        "qa_verdict_creator",
        "status_post_hoc",
        "verdict",
        "verdict_post_hoc",
        "reviewer",
        "merger",
        "class",
        "trigger",
        "gate_summary",
        "auditor_check",
        "residue",
        "outcome",
        "flags",
        "evidence_unavailable",
        "unknowns",
    ] {
        assert!(m.get(key).is_some(), "missing merges[].{key}: {m}");
    }
    for key in ["tree_match", "smoke", "daemon_restart", "revert"] {
        assert!(
            m["outcome"].get(key).is_some(),
            "missing outcome.{key}: {m}"
        );
    }
    assert!(j["summary"]["rows"].as_u64().unwrap() >= 3);
}

#[test]
fn audit_filters_since_class_project_limit() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    // Tracker: CAD-1 lives under project `alpha`.
    std::fs::create_dir_all(pm.join("alpha").join("CAD-1")).unwrap();
    std::fs::write(pm.join("alpha/CAD-1/issue.md"), "---\nid: CAD-1\n---\n").unwrap();
    verdict_note(
        &notes,
        "20260920-115800-x-p1-verdict.md",
        &heads[0],
        "qa-1",
        "auto",
    );
    verdict_note(
        &notes,
        "20260920-115810-x-p2-verdict.md",
        &heads[1],
        "qa-1",
        "human",
    );

    // --since far future → no rows.
    let out = run_audit(
        &state,
        &pm,
        &repo,
        &notes,
        &report,
        &["--since", "2999-01-01"],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("0 merges") || text.contains("no merges"),
        "{text}"
    );

    // --limit 2 → exactly two rows.
    let out = run_audit(&state, &pm, &repo, &notes, &report, &["--limit", "2"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("(2 merges"), "{text}");

    // --class auto → only the auto-classified row.
    let out = run_audit(&state, &pm, &repo, &notes, &report, &["--class", "auto"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("#1"), "{text}");
    assert!(!text.contains("#2"), "{text}");
    // --class human → the human row only.
    let out = run_audit(&state, &pm, &repo, &notes, &report, &["--class", "human"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("#2"), "{text}");
    assert!(!text.contains("#1"), "{text}");

    // --project alpha → only CAD-1's row.
    let out = run_audit(&state, &pm, &repo, &notes, &report, &["--project", "alpha"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("#1"), "{text}");
    assert!(!text.contains("#2"), "{text}");
}

#[test]
fn audit_fixture_never_shells_gh() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, _heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    // PATH really has no gh: if fixture mode shelled out anyway the
    // spawn would fail and every row would report evidence gaps.
    let path = path_without_gh(&dir);
    let out = run_audit_full(
        &state,
        &pm,
        &repo,
        Some(&notes),
        Some(&report),
        &["--json"],
        Some(&path),
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let j: Value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("fixture mode must not call gh: {e}\n{text}"));
    // 3 merge subjects + the init commit.
    assert_eq!(j["merges"].as_array().unwrap().len(), 4);
    let pr_rows = j["merges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["pr"].as_u64().is_some())
        .count();
    assert_eq!(pr_rows, 3);
    // No row reports a failed channel — the fixture answered everything.
    assert!(
        j["merges"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["evidence_unavailable"].is_null()),
        "a spawn attempt would surface as evidence_unavailable: {j}"
    );
}

/// gh absent from PATH on the live path: every row is
/// `unknown — evidence unavailable`, nothing flags, exit 0. Missing
/// evidence is never an accusation.
#[test]
fn audit_gh_unavailable_is_unknown_not_flag() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, _heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    // A github origin so the audit resolves a slug and really tries gh;
    // a verdict note proves notes answered (fail) — the row must still
    // not flag while gh itself is unreachable.
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success());
    };
    g(&["remote", "add", "origin", "https://github.com/x/y"]);
    // One non-PR direct push too — even it must not flag with gh down.
    std::fs::write(repo.join("direct.txt"), "d").unwrap();
    g(&["add", "."]);
    g(&["commit", "-qm", "direct push"]);

    let path = path_without_gh(&dir);
    let out = run_audit_full(
        &state,
        &pm,
        &repo,
        Some(&notes),
        None, // no --merge-report: the live gh path, with gh absent
        &["--json"],
        Some(&path),
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "unavailable evidence must not flag:\n{text}"
    );
    let j: Value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("--json not one document: {e}\n{text}"));
    let merges = j["merges"].as_array().unwrap();
    assert!(!merges.is_empty());
    for m in merges {
        assert!(
            m["flags"].as_array().unwrap().is_empty(),
            "no flags on missing evidence: {m}"
        );
    }
    // Every row that needed gh reports the gap with its reason.
    let gap_rows = merges
        .iter()
        .filter(|m| !m["evidence_unavailable"].is_null())
        .count();
    assert!(
        gap_rows >= merges.iter().filter(|m| m["pr"].is_u64()).count(),
        "gh-down PR rows must carry evidence_unavailable: {j}"
    );
    assert_eq!(j["summary"]["flagged"].as_u64().unwrap(), 0);
    // Suppress the unused-fixture warning — this test runs live.
    let _ = report;
}

#[test]
fn audit_evidence_unavailable_is_unknown_not_flag() {
    let dir = TempDir::new().unwrap();
    let (repo, _notes, report, _heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    // The notes directory does not exist — a verdict note could be in
    // it. `no-passing-verdict` must not fire: the row is unknown, and
    // unknown rows exit 0.
    let missing_notes = dir.path().join("no-such-notes");
    let out = run_audit(
        &state,
        &pm,
        &repo,
        &missing_notes,
        &report,
        &["--limit", "1"],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "missing evidence is unknown, not a flag:\n{text}"
    );
    assert!(!text.contains("FLAG["), "{text}");
    assert!(text.contains("evidence unavailable"), "{text}");

    // A PR absent from the fixture's `prs` list is likewise a data
    // gap, not a verdict failure.
    let mut report_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    report_json["prs"] = json!([]);
    std::fs::write(&report, report_json.to_string()).unwrap();
    let notes = dir.path().join("notes");
    let out = run_audit(
        &state,
        &pm,
        &repo,
        &notes,
        &report,
        &["--limit", "1", "--json"],
    );
    let j: Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(out.status.code(), Some(0), "{j}");
    let m = &j["merges"][0];
    assert!(
        m["flags"].as_array().unwrap().is_empty(),
        "evidence gaps must not flag: {m}"
    );
    assert!(
        m["evidence_unavailable"]
            .as_str()
            .is_some_and(|s| s.contains("no merged PR")),
        "gap reason must surface: {m}"
    );
}

#[test]
fn audit_post_hoc_verdict_does_not_clear_flag() {
    let dir = TempDir::new().unwrap();
    let (repo, notes, report, heads) = audit_repo(&dir);
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm).unwrap();
    // The verdict note's filename timestamp is *after* the merge —
    // evidence that arrived post-merge, not a merge-time review.
    verdict_note(
        &notes,
        "20260920-130000-x-p3-verdict.md",
        &heads[2],
        "qa-1",
        "auto",
    );
    let out = run_audit(
        &state,
        &pm,
        &repo,
        &notes,
        &report,
        &["--limit", "1", "--json"],
    );
    let j: Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(out.status.code(), Some(1), "post-hoc pass must flag: {j}");
    let m = &j["merges"][0];
    assert_eq!(m["pr"].as_u64(), Some(3), "{j}");
    assert_eq!(m["verdict_post_hoc"].as_bool(), Some(true), "{j}");
    assert!(
        m["flags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "no-passing-verdict"),
        "post-hoc pass must not clear the flag: {m}"
    );
}

// ---------- CAD-113: build slots ----------

/// A daemon with a shrunken slot config — hermetic (ServeOptions wins
/// over pm.yaml, so no host config can leak in).
fn slot_opts(build: usize, suite: usize, starve: u64, priority: &[&str]) -> daemon::ServeOptions {
    slot_opts_clock(build, suite, starve, priority, None)
}

/// `slot_opts` with an injected slot clock: a shared counter the test
/// advances instead of sleeping — starvation tests stay deterministic
/// under host load.
fn slot_opts_clock(
    build: usize,
    suite: usize,
    starve: u64,
    priority: &[&str],
    clock: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
) -> daemon::ServeOptions {
    daemon::ServeOptions {
        slots: Some(cadence_agent::slots::SlotConfig {
            build_slots: build,
            suite_slots: suite,
            starve_secs: starve,
            priority_lanes: priority.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }),
        slot_clock: clock.map(|c| {
            std::sync::Arc::new(move || c.load(std::sync::atomic::Ordering::Relaxed) as f64)
                as std::sync::Arc<dyn Fn() -> f64 + Send + Sync>
        }),
        ..daemon_opts()
    }
}

/// Plant `alias` as a live pty pane rooted at `pid` — the endpoint
/// facts the slot caller-identity derivation reads (CAD-113). The row
/// stays otherwise inert: registered as an actorless `inbox` pair and
/// marked `enabled=0`, so neither a register-time `set_identity` nor a
/// restart's relaunch sweep can overwrite or detach the planted facts.
/// `slot_*` RPCs derive caller identity from `SO_PEERCRED` + /proc
/// ancestry, so a test lane is only reachable from processes whose
/// ancestry includes this pid.
fn plant_pane(d: &TestDaemon, alias: &str, pid: u32) {
    // Register as an `inbox` mailbox: the pair owns no actor, so no
    // async `set_identity` can land after this plant and overwrite
    // the pid (`agent_register` on an existing alias errors —
    // idempotent on a daemon restarted over a kept state dir). And
    // `enabled=0` keeps a restarted daemon's relaunch sweep from
    // spawning a pty actor for the row — its open cannot verify a
    // planted pane and the exit-detach clears the pid the pane map
    // resolves callers by (the CAD-113 CI flake).
    let _ = d.rpc(
        "agent_register",
        json!({"alias": alias, "provider": "inbox",
               "endpoint_kind": "inbox",
               "cwd": d.dir.path().to_str().unwrap()}),
    );
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET endpoint_kind='pty', pid=?1, enabled=0, \
            generation='planted', session_id='planted' WHERE alias=?2",
        rusqlite::params![pid as i64, alias],
    )
    .unwrap();
}

/// The lane every in-process `d.rpc` slot call derives: the test
/// process's own pid planted as this alias's pane.
const SELF_LANE: &str = "pane-self";

/// Plant the test process itself as `SELF_LANE`'s pane — after this,
/// `d.rpc` slot calls and `Command`-spawned cadence CLIs all run as
/// that lane (their ancestry always includes the test pid).
fn plant_self(d: &TestDaemon) {
    plant_pane(d, SELF_LANE, std::process::id());
}

/// A long-lived `bash` whose pid is planted as a lane's pane:
/// commands written to its stdin run as its children, so their
/// socket-peer identity derives that lane — the only way to get a
/// second connection identity in-process tests can't reach.
struct LaneShell {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    dir: TempDir,
    seq: u64,
}

impl LaneShell {
    fn spawn(home: &Path) -> LaneShell {
        let mut child = std::process::Command::new("bash")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .env("HOME", home)
            .envs(test_env().vars())
            .spawn()
            .unwrap();
        LaneShell {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            dir: TempDir::new().unwrap(),
            seq: 0,
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Run a bash fragment under this lane; answer (exit code, output).
    fn run(&mut self, cmd: &str) -> (i64, String) {
        let tag = format!("__lane_rc_{}__", self.seq);
        self.seq += 1;
        // The bare `echo` first guarantees the marker opens a fresh
        // line even when the command's output ends mid-line.
        writeln!(self.stdin, "{{ {cmd} ; }} 2>&1; rc=$?; echo; echo {tag}$rc").unwrap();
        self.stdin.flush().unwrap();
        let mut out = String::new();
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "lane shell exited while running: {cmd}"
            );
            if let Some(rc) = line.strip_prefix(&tag) {
                return (rc.trim().parse().unwrap(), out);
            }
            out.push_str(&line);
        }
    }

    /// `cadence <args>` run under this lane's identity.
    fn cadence(&mut self, state: &Path, args: &str) -> (i64, String) {
        self.run(&format!(
            "{} --state-dir {} {args}",
            env!("CARGO_BIN_EXE_cadence"),
            state.display()
        ))
    }

    /// One raw JSONL RPC under this lane's identity — the answer is
    /// the wire frame (`{"ok":…, "result"|"error":…}`).
    fn rpc(&mut self, state: &Path, method: &str, params: Value) -> Value {
        let req = self.dir.path().join(format!("req-{}.json", self.seq));
        std::fs::write(
            &req,
            cadence_agent::proto::request(method, params).to_string(),
        )
        .unwrap();
        let (rc, out) = self.run(&format!(
            "python3 -c 'import socket,sys;\
             s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);\
             s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");\
             print(s.makefile().readline())' {} {}",
            client::socket_path(state).display(),
            req.display()
        ));
        assert_eq!(rc, 0, "lane rpc failed: {out}");
        serde_json::from_str(out.trim()).unwrap()
    }
}

impl Drop for LaneShell {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `slot_acquire` with the test process's pid — alive for the whole
/// test, so the pid check never reaps a live waiter here. The lane
/// param is ignored by the daemon (identity is the connection's);
/// callers pass SELF_LANE for honesty.
fn slot_acquire(d: &TestDaemon, kind: &str, lane: &str, req: &str) -> Value {
    slot_acquire_pid(d, kind, lane, std::process::id(), req)
}

/// `slot_acquire` claiming an explicit pid — must be the test process
/// or one of its /proc ancestors, or the daemon refuses.
fn slot_acquire_pid(d: &TestDaemon, kind: &str, lane: &str, pid: u32, req: &str) -> Value {
    d.rpc(
        "slot_acquire",
        json!({"kind": kind, "lane": lane, "pid": pid,
               "request_id": req}),
    )
    .unwrap()
}

/// `slot_release` naming the holding (lane, pid) — the identity the
/// grant was bound to.
fn slot_release(d: &TestDaemon, token: &str, lane: &str, pid: u32) -> Value {
    d.rpc(
        "slot_release",
        json!({"token": token, "lane": lane, "pid": pid}),
    )
    .unwrap()
}

/// N+1 acquires: the last queues until a release, FIFO order is kept,
/// and slot events land on the caller's stream. Every call here runs
/// as `SELF_LANE` — identity is connection-derived (CAD-113).
#[test]
fn slot_acquire_queues_until_release() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let g1 = slot_acquire(&d, "build", SELF_LANE, "r1");
    assert_eq!(g1["granted"], true);
    let t1 = g1["token"].as_str().unwrap().to_string();
    assert!(t1.starts_with("slot-"), "the daemon mints the token: {t1}");
    // The next acquire queues — answered, never hung.
    let q = slot_acquire(&d, "build", SELF_LANE, "r2");
    assert_eq!(q["granted"], false);
    assert_eq!(q["position"], 1);
    let s = d.rpc("slot_status", json!({})).unwrap();
    assert_eq!(s["pools"]["build"]["held"].as_array().unwrap().len(), 1);
    // The owner sees its own token — the hold's pid is on its chain.
    assert_eq!(
        s["pools"]["build"]["held"][0]["token"], t1,
        "the holding process's own chain sees its token"
    );
    let waiting = s["waiting"].as_array().unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0]["lane"], SELF_LANE);
    // A re-poll keeps the original place — same request id, same
    // position, no second slot_waited.
    let q = slot_acquire(&d, "build", SELF_LANE, "r2");
    assert_eq!(q["position"], 1);
    // Release frees the pool; the waiter's next poll grants.
    slot_release(&d, &t1, SELF_LANE, std::process::id());
    let g2 = slot_acquire(&d, "build", SELF_LANE, "r2");
    assert_eq!(g2["granted"], true);
    let t2 = g2["token"].as_str().unwrap().to_string();
    assert_ne!(t2, t1, "each grant mints a fresh token");
    // And a re-poll of a granted id returns the same token (the CLI's
    // poll loop depends on this idempotency).
    let again = slot_acquire(&d, "build", SELF_LANE, "r2");
    assert_eq!(again["token"], t2);
    let kinds = |a: &str| {
        d.events(a)
            .iter()
            .map(|e| e["kind"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    let own = kinds(SELF_LANE);
    assert!(own.contains(&"slot_acquired".to_string()));
    assert!(own.contains(&"slot_released".to_string()));
    assert_eq!(
        own.iter().filter(|k| *k == "slot_waited").count(),
        1,
        "one slot_waited for the whole wait: {own:?}"
    );
}

/// A holder whose pid dies frees its slot on the next acquire —
/// nothing kills the work, the slot just stops being owed by a corpse.
/// The hold binds to a lane shell's pid: killing the shell kills the
/// hold's owner.
#[test]
fn slot_dead_holder_is_reaped() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let home = TempDir::new().unwrap();
    let mut holder = LaneShell::spawn(home.path());
    plant_pane(&d, "dev-1", holder.pid());
    // The holder's child claims its own pane — `$$` in the shell is
    // the planted pane pid itself.
    let g = holder.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "build", "pid": holder.pid(), "request_id": "r1"}),
    );
    assert_eq!(g["ok"], true, "{g}");
    let token = g["result"]["token"].as_str().unwrap().to_string();
    holder.child.kill().unwrap();
    holder.child.wait().unwrap(); // reap the zombie so kill(pid,0) answers ESRCH
    let g2 = slot_acquire(&d, "build", SELF_LANE, "r2");
    assert_eq!(g2["granted"], true, "dead holder's slot must free");
    // The reap names the cause on the dead lane's stream.
    let evs = d.events("dev-1");
    assert!(
        evs.iter()
            .any(|e| e["kind"].as_str() == Some("slot_released")
                && e["payload"]["reason"].as_str() == Some("holder died")),
        "{evs:?}"
    );
    // Releasing the dead token is a named refusal, not a silent pass
    // — and nobody can claim the dead pid anyway.
    let err = d
        .rpc(
            "slot_release",
            json!({"token": token, "pid": std::process::id()}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("Unknown slot token"), "{err}");
}

/// BLOCKER: two callers sharing a request_id — the second queues, it
/// never adopts the first's hold; a same-identity re-poll does. The
/// "different pid" is the test's own parent — a second pid on the
/// connection's ancestry that may legitimately be claimed (CAD-113).
#[test]
fn slot_duplicate_request_id_different_pid_queues() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let parent = std::os::unix::process::parent_id();
    let g1 = slot_acquire_pid(&d, "build", SELF_LANE, parent, "r1");
    assert_eq!(g1["granted"], true);
    // Same request_id claiming a different pid — a different caller:
    // queued, never granted the first's hold.
    let q = slot_acquire(&d, "build", SELF_LANE, "r1");
    assert_eq!(q["granted"], false, "must not adopt another caller's hold");
    let s = d.rpc("slot_status", json!({})).unwrap();
    assert_eq!(s["waiting"].as_array().unwrap().len(), 1);
    // The true holder re-polls and still gets its own token.
    let again = slot_acquire_pid(&d, "build", SELF_LANE, parent, "r1");
    assert_eq!(again["token"], g1["token"]);
}

/// BLOCKER: release binds to the holding (lane, pid) — both derived
/// from the connection now. A foreign lane's release is a named
/// refusal; a claimed pid off the caller's own ancestry is refused
/// before the token is even looked at. The hold survives both.
#[test]
fn slot_release_foreign_caller_is_rejected() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let home = TempDir::new().unwrap();
    let mut foreign = LaneShell::spawn(home.path());
    plant_pane(&d, "dev-2", foreign.pid());
    let g = slot_acquire(&d, "build", SELF_LANE, "r1");
    let token = g["token"].as_str().unwrap().to_string();
    // The foreign lane knows the token but its derived lane doesn't
    // match the hold — refused. (The `pid` claim is honest here.)
    let f = foreign.rpc(
        &d.state,
        "slot_release",
        json!({"token": token, "pid": foreign.pid()}),
    );
    assert_eq!(f["ok"], false, "{f}");
    assert!(
        f["error"]["message"]
            .as_str()
            .unwrap()
            .contains("another caller"),
        "{f}"
    );
    // A claimed pid off the caller's own chain — a sibling lane's pid
    // is a live pid the shell does not descend from — is refused
    // outright, before the token is even looked at.
    let sibling = LaneShell::spawn(home.path());
    let f = foreign.rpc(
        &d.state,
        "slot_release",
        json!({"token": token, "pid": sibling.pid()}),
    );
    assert_eq!(f["ok"], false, "{f}");
    assert!(
        f["error"]["message"]
            .as_str()
            .unwrap()
            .contains("cannot claim"),
        "{f}"
    );
    // The hold still stands — the pool stays full.
    let q = slot_acquire(&d, "build", SELF_LANE, "r2");
    assert_eq!(q["granted"], false, "failed release must not free the slot");
    // And the true holder releases normally.
    slot_release(&d, &token, SELF_LANE, std::process::id());
}

/// ACCEPTANCE: `slot_status` reveals a token only to the connection
/// whose derived identity owns the hold — two real lanes. The owner
/// sees its token; a foreign lane passing the owner's `lane` sees the
/// hold but never the token (CAD-113 identity fork, option A).
#[test]
fn slot_status_reveals_tokens_only_to_the_owner() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    let home = TempDir::new().unwrap();
    let mut owner = LaneShell::spawn(home.path());
    plant_pane(&d, "owner", owner.pid());
    plant_self(&d); // the foreign observer
    let g = owner.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "build", "pid": owner.pid(), "request_id": "r1"}),
    );
    assert_eq!(g["result"]["granted"], true, "{g}");
    let token = g["result"]["token"].as_str().unwrap().to_string();
    // The owner's own status reveals its token — via the real CLI
    // too: the cadence child derives this lane from its ancestry.
    let s = owner.rpc(&d.state, "slot_status", json!({}));
    let held = s["result"]["pools"]["build"]["held"].as_array().unwrap();
    assert_eq!(held[0]["token"], token, "owner sees its own token");
    let (rc, out) = owner.cadence(&d.state, "build-slot status --json");
    assert_eq!(rc, 0, "{out}");
    let cli: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        cli["pools"]["build"]["held"][0]["token"], token,
        "owner CLI sees its own token"
    );
    // The foreign lane's status sees the hold but not the token —
    // even naming the owner's lane in the request.
    let s = d.rpc("slot_status", json!({"lane": "owner"})).unwrap();
    let held = s["pools"]["build"]["held"].as_array().unwrap();
    assert_eq!(held.len(), 1);
    assert!(
        held[0].get("token").is_none(),
        "foreign caller must not see the token: {held:?}"
    );
    assert_eq!(held[0]["lane"], "owner");
}

/// ACCEPTANCE: a `slot_acquire` whose claimed `pid` is not the socket
/// peer or one of its /proc ancestors is refused — the daemon never
/// rebinds it (CAD-113 identity fork, option A).
#[test]
fn slot_acquire_refuses_a_pid_off_the_caller_chain() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    let home = TempDir::new().unwrap();
    let mut lane = LaneShell::spawn(home.path());
    plant_pane(&d, "dev-1", lane.pid());
    // A sibling lane's pid is live but off this caller's ancestry.
    let other = LaneShell::spawn(home.path());
    let r = lane.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "build", "pid": other.pid(), "request_id": "r1"}),
    );
    assert_eq!(r["ok"], false, "{r}");
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("cannot claim"),
        "{r}"
    );
    // Nothing queued or held under either identity.
    plant_self(&d);
    let s = d.rpc("slot_status", json!({})).unwrap();
    assert!(s["waiting"].as_array().unwrap().is_empty());
    assert!(s["pools"]["build"]["held"].as_array().unwrap().is_empty());
    // An honest claim — the caller's own pid — grants normally.
    let g = lane.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "build", "pid": lane.pid(), "request_id": "r2"}),
    );
    assert_eq!(g["result"]["granted"], true, "{g}");
}

/// ACCEPTANCE: a caller detached from every registered pane derives
/// no identity at all — all three slot RPCs refuse it, and nothing is
/// stamped `operator` (the PR-#71 fail-open pattern, closed here).
#[test]
fn slot_rpc_refuses_an_underivable_caller() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    let home = TempDir::new().unwrap();
    // `stray` descends from the test process but nothing in its
    // ancestry is a registered pty pane — no pane is planted for it.
    let mut stray = LaneShell::spawn(home.path());
    // `observer` is a real lane so we can inspect the pools afterward.
    let mut observer = LaneShell::spawn(home.path());
    plant_pane(&d, "observer", observer.pid());
    for (method, params) in [
        (
            "slot_acquire",
            json!({"kind": "build", "pid": stray.pid(), "request_id": "r1"}),
        ),
        (
            "slot_release",
            json!({"token": "slot-x", "pid": stray.pid()}),
        ),
        ("slot_status", json!({})),
    ] {
        let r = stray.rpc(&d.state, method, params);
        assert_eq!(r["ok"], false, "{method}: {r}");
        assert!(
            r["error"]["message"]
                .as_str()
                .unwrap()
                .contains("caller identity underivable"),
            "{method} must refuse identity-less callers: {r}"
        );
    }
    // Nothing was recorded — and especially not as `operator`.
    let s = observer.rpc(&d.state, "slot_status", json!({}));
    assert_eq!(s["ok"], true, "{s}");
    assert!(s["result"]["waiting"].as_array().unwrap().is_empty());
    assert!(s["result"]["pools"]["build"]["held"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(
        !s["result"].to_string().contains("operator"),
        "no operator identity may appear: {}",
        s["result"]
    );
}

/// BLOCKER (r3): `slot_acquired` rides the victim's event stream —
/// readable by any local caller via `agent_events`. It must never
/// carry the token: token+lane+pid are the entire release credential,
/// so a peer's stream can never be mined for one. (r5: the victim is
/// a real second connection identity — a lane shell.)
#[test]
fn slot_acquired_event_cannot_release_a_peers_hold() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    let home = TempDir::new().unwrap();
    let mut victim = LaneShell::spawn(home.path());
    plant_pane(&d, "victim", victim.pid());
    plant_self(&d); // the snoop: every d.rpc runs as SELF_LANE
    let g = victim.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "build", "pid": victim.pid(), "request_id": "r1"}),
    );
    assert_eq!(g["ok"], true, "{g}");
    let token = g["result"]["token"].as_str().unwrap().to_string();
    // The peer reads the victim's stream — sees the acquisition…
    let ev = d
        .events("victim")
        .into_iter()
        .find(|e| e["kind"].as_str() == Some("slot_acquired"))
        .expect("victim emitted slot_acquired");
    assert!(
        ev["payload"].get("token").is_none() && !ev["payload"].to_string().contains(&token),
        "slot_acquired leaks the release credential: {}",
        ev["payload"]
    );
    // …but the visible fields can't release anything: a guessed token
    // is an unknown-token rejection and the hold survives.
    let err = d
        .rpc(
            "slot_release",
            json!({"token": "slot-guess", "pid": std::process::id()}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("Unknown slot token"), "{err}");
    // Even the real token under a foreign identity is refused — the
    // derived lane (pane-self) is not the hold's lane, whatever the
    // request's `lane` field claims.
    let err = d
        .rpc(
            "slot_release",
            json!({"token": token, "lane": "victim", "pid": std::process::id()}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("another caller"), "{err}");
    // And status passing the victim's lane still shows no token.
    let s = d.rpc("slot_status", json!({"lane": "victim"})).unwrap();
    let held = s["pools"]["build"]["held"].as_array().unwrap();
    assert_eq!(held.len(), 1);
    assert!(held[0].get("token").is_none(), "foreign token hidden");
    // The owner releases normally under its own connection identity.
    let r = victim.rpc(
        &d.state,
        "slot_release",
        json!({"token": token, "pid": victim.pid()}),
    );
    assert_eq!(r["ok"], true, "{r}");
}

/// BLOCKER: holds survive a daemon restart — persisted slots.json is
/// revalidated at boot: live holders keep their slots (never
/// re-granted), dead holders are dropped with a named reason.
#[test]
fn slot_restart_revalidates_holders() {
    let state = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let d = TestDaemon::start_on_opts(state.path().to_path_buf(), slot_opts(2, 1, 900, &[]));
    // Two holds: one bound to a lane shell that dies before the
    // restart, one bound to the test process which outlives it.
    let mut doomed = LaneShell::spawn(home.path());
    plant_pane(&d, "dev-1", doomed.pid());
    let g = doomed.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "build", "pid": doomed.pid(), "request_id": "r0"}),
    );
    assert_eq!(g["ok"], true, "{g}");
    plant_self(&d);
    let live = slot_acquire(&d, "build", SELF_LANE, "r1");
    assert_eq!(live["granted"], true);
    let live_tok = live["token"].as_str().unwrap().to_string();
    let live_pid = std::process::id();
    doomed.child.kill().unwrap();
    doomed.child.wait().unwrap();
    drop(d); // shutdown → serve returns → state dir kept
    let d2 = TestDaemon::start_on_opts(state.path().to_path_buf(), slot_opts(2, 1, 900, &[]));
    // The agent rows persisted but a clean shutdown clears endpoint
    // fields — re-stamp the live pane's facts before deriving.
    plant_pane(&d2, SELF_LANE, live_pid);
    // The live hold survived with its token intact; the dead one's
    // slot was reaped — one held, one free.
    let s = d2.rpc("slot_status", json!({})).unwrap();
    let held = s["pools"]["build"]["held"].as_array().unwrap();
    assert_eq!(held.len(), 1, "one live holder survives: {held:?}");
    assert_eq!(held[0]["token"], live_tok);
    assert_eq!(held[0]["pid"], live_pid);
    // The boot reap named the dead holder's cause on its lane.
    let evs = d2.events("dev-1");
    assert!(
        evs.iter()
            .any(|e| e["kind"].as_str() == Some("slot_released")
                && e["payload"]["reason"].as_str() == Some("holder died")),
        "{evs:?}"
    );
    // And an acquire never re-grants the survivor's slot — one free
    // slot grants once, then the pool is full again.
    let g = slot_acquire(&d2, "build", SELF_LANE, "r9");
    assert_eq!(g["granted"], true);
    let q = slot_acquire(&d2, "build", SELF_LANE, "r10");
    assert_eq!(q["granted"], false, "restarted holds keep the pool bounded");
    // The survivor still releases by its minted token.
    slot_release(&d2, &live_tok, SELF_LANE, live_pid);
}

/// Regression for CI 35542407390: a planted pane row must survive a
/// daemon restart's relaunch sweep untouched. The sweep relaunches
/// every enabled actor-owning row; an actor whose open can't verify
/// the planted pane exit-detaches it — clearing the pid/generation
/// the caller-identity pane map resolves by — or a real open's
/// `set_identity` overwrites it. Either way the next slot call fails
/// closed ("descends from no registered pane"). plant_pane's rows are
/// actorless (`inbox` pair) and `enabled=0`, so the sweep never
/// touches them: the planted facts persist through the whole window.
#[test]
fn slot_planted_pane_row_survives_restart() {
    let state = TempDir::new().unwrap();
    let live_pid = std::process::id();
    let d = TestDaemon::start_on_opts(state.path().to_path_buf(), slot_opts(2, 1, 900, &[]));
    plant_pane(&d, SELF_LANE, live_pid);
    let g = slot_acquire(&d, "build", SELF_LANE, "r1");
    assert_eq!(g["granted"], true, "{g}");
    // Canary in the pre-fix shape — an enabled (fake, pty) row the boot
    // relaunch sweep must launch. Its actor's adapter build fails
    // deterministically and the exit-detach emits `attention`. The alias
    // sorts after every other agent, so once its outcome lands the sweep
    // has spawned an actor for every earlier row.
    let conn = rusqlite::Connection::open(state.path().join("cadence.sqlite3")).unwrap();
    conn.execute(
        "INSERT INTO agents(alias,provider,endpoint_kind,role,cwd,sandbox,
            state,enabled,pid,generation,session_id,created,updated)
         VALUES('zz-canary','fake','pty','worker',?1,'read-only',
            'stopped',1,0,'planted','planted',0,0)",
        [state.path().to_str().unwrap()],
    )
    .unwrap();
    drop(conn);
    drop(d);
    let d2 = TestDaemon::start_on_opts(state.path().to_path_buf(), slot_opts(2, 1, 900, &[]));
    plant_pane(&d2, SELF_LANE, live_pid);
    // Positive window-closed signal (CAD-221): never assert absence
    // inside a window that may not have opened. The canary's `attention`
    // proves the sweep ran and an actor outcome landed on the very path
    // that would destroy a vulnerable planted row — the lane assertion
    // below is made only after that window provably closed.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if d2
            .events("zz-canary")
            .iter()
            .any(|e| e["kind"].as_str() == Some("attention"))
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "canary never detached — the relaunch sweep did not run: {:?}",
            d2.events("zz-canary")
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let conn = rusqlite::Connection::open(state.path().join("cadence.sqlite3")).unwrap();
    let p: i64 = conn
        .query_row("SELECT pid FROM agents WHERE alias=?", [SELF_LANE], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(p as u32, live_pid, "no actor clobbered the plant");
    assert!(
        !d2.events(SELF_LANE)
            .iter()
            .any(|e| e["kind"].as_str() == Some("attention")),
        "no actor should ever have launched: {:?}",
        d2.events(SELF_LANE)
    );
    let g = slot_acquire(&d2, "build", SELF_LANE, "r9");
    assert_eq!(g["granted"], true, "{g}");
}

/// `starve_secs` promotes a long waiter ahead of a priority lane:
/// priority wins inside the window, the starved waiter wins after it.
/// The slot clock is injected — the test advances it instead of
/// sleeping, so timing stays exact under host load. The waiter lanes
/// are real connection identities — one lane shell each (CAD-113).
#[test]
fn slot_starve_promotes_long_waiter() {
    let clock = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let d = TestDaemon::start_opts(slot_opts_clock(1, 1, 3, &["qa-1"], Some(clock.clone())));
    let home = TempDir::new().unwrap();
    plant_self(&d);
    let mut dev = LaneShell::spawn(home.path());
    plant_pane(&d, "dev-2", dev.pid());
    let mut qa = LaneShell::spawn(home.path());
    plant_pane(&d, "qa-1", qa.pid());
    let h1 = slot_acquire(&d, "build", SELF_LANE, "h1")["token"]
        .as_str()
        .unwrap()
        .to_string();
    // Ordinary waiter first, priority waiter second.
    let w = dev.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "build", "pid": dev.pid(), "request_id": "w1"}),
    );
    assert_eq!(w["result"]["granted"], false, "{w}");
    let w = qa.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "test", "pid": qa.pid(), "request_id": "w2"}),
    );
    assert_eq!(w["result"]["granted"], false, "{w}");
    slot_release(&d, &h1, SELF_LANE, std::process::id());
    // Inside the starve window the reviewer lane's test wins.
    let g = qa.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "test", "pid": qa.pid(), "request_id": "w2"}),
    );
    assert_eq!(
        g["result"]["granted"], true,
        "priority lane should outrank: {g}"
    );
    let w2 = g["result"]["token"].as_str().unwrap().to_string();
    // Once w1 has waited past starve_secs it outranks even a new
    // priority request — the never-starve bound.
    clock.store(4, std::sync::atomic::Ordering::Relaxed);
    let w = qa.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "test", "pid": qa.pid(), "request_id": "w3"}),
    );
    assert_eq!(w["result"]["granted"], false, "{w}");
    let r = qa.rpc(
        &d.state,
        "slot_release",
        json!({"token": w2, "pid": qa.pid()}),
    );
    assert_eq!(r["ok"], true, "{r}");
    let g = dev.rpc(
        &d.state,
        "slot_acquire",
        json!({"kind": "build", "pid": dev.pid(), "request_id": "w1"}),
    );
    assert_eq!(
        g["result"]["granted"], true,
        "starved waiter must outrank priority: {g}"
    );
    let s = d.rpc("slot_status", json!({})).unwrap();
    let w3 = s["waiting"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["request_id"] == "w3")
        .expect("w3 still queued");
    assert_eq!(w3["priority"], true);
}

/// suite draws on its own pool — a full suite queue never jams the
/// build lanes, and `test` shares the build pool. The two pool users
/// claim different pids on this connection's own ancestry (CAD-113):
/// the cross-pool deadlock guard keys on `(lane, pid)`, so the same
/// process must never hold one pool while queueing the other.
#[test]
fn slot_pools_are_independent() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let me = std::process::id();
    let parent = std::os::unix::process::parent_id();
    assert_eq!(
        slot_acquire_pid(&d, "suite", SELF_LANE, me, "s1")["granted"],
        true
    );
    assert_eq!(
        slot_acquire_pid(&d, "suite", SELF_LANE, me, "s2")["granted"],
        false,
        "second suite must queue"
    );
    // The suite pool being full does not touch build.
    assert_eq!(
        slot_acquire_pid(&d, "build", SELF_LANE, parent, "b1")["granted"],
        true
    );
    // test shares the build pool — now full too.
    assert_eq!(
        slot_acquire_pid(&d, "test", SELF_LANE, parent, "t1")["granted"],
        false
    );
}

/// The CLI: `--wait-secs 0` fails fast with a named error, a free slot
/// grants a bare token, release returns it, and `status` shows the
/// pool both ways.
#[test]
fn build_slot_cli_acquire_release_status() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let home = TempDir::new().unwrap();
    let t1 = slot_acquire(&d, "build", SELF_LANE, "r1")["token"]
        .as_str()
        .unwrap()
        .to_string(); // build pool full
    let me = std::process::id().to_string(); // the CLI child's parent
    let out = cadence_at(
        home.path(),
        &d.state,
        &[
            "build-slot",
            "acquire",
            "build",
            "--wait-secs",
            "0",
            "--pid",
            &me,
        ],
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("No build slot free"), "{err}");
    // --pid is required — a bare acquire refuses rather than binding
    // a transient parent the work outlives.
    let out = cadence_at(
        home.path(),
        &d.state,
        &["build-slot", "acquire", "build", "--wait-secs", "0"],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--pid"));
    // Free it through the CLI — release names the holding lane; the
    // default pid (the CLI's parent = this test) matches the hold.
    let out = cadence_at(
        home.path(),
        &d.state,
        &["build-slot", "release", &t1, "--lane", "dev-1"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("released slot-"));
    let out = cadence_at(
        home.path(),
        &d.state,
        &[
            "build-slot",
            "acquire",
            "build",
            "--wait-secs",
            "0",
            "--lane",
            "dev-9",
            "--pid",
            &me,
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        token.starts_with("slot-"),
        "bare minted token on stdout: {token:?}"
    );
    // The token round-trips: release by exactly what acquire printed,
    // same lane, same default pid.
    let out = cadence_at(
        home.path(),
        &d.state,
        &["build-slot", "release", &token, "--lane", "dev-9"],
    );
    assert!(out.status.success());
    // `--lane` is advisory only (CAD-113): the daemon derives the
    // caller's lane from the connection, so a release naming another
    // lane still acts on — and only on — the caller's own hold.
    let g = slot_acquire(&d, "build", SELF_LANE, "r9");
    let t9 = g["token"].as_str().unwrap().to_string();
    let out = cadence_at(
        home.path(),
        &d.state,
        &["build-slot", "release", &t9, "--lane", "dev-2"],
    );
    assert!(
        out.status.success(),
        "own hold releases whatever --lane claims: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // status --json shows the empty pool; bad kind is a named error.
    let out = cadence_at(home.path(), &d.state, &["build-slot", "status", "--json"]);
    let s: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(s["pools"]["build"]["held"].as_array().unwrap().is_empty());
    let out = cadence_at(
        home.path(),
        &d.state,
        &[
            "build-slot",
            "acquire",
            "bogus",
            "--wait-secs",
            "0",
            "--pid",
            &me,
        ],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("build, test or suite"));
}

/// `cadence status` carries the slot line — table and --json agree.
#[test]
fn status_footer_shows_slots() {
    let d = TestDaemon::start_opts(slot_opts(2, 1, 900, &[]));
    plant_self(&d);
    slot_acquire(&d, "build", SELF_LANE, "r1");
    slot_acquire(&d, "build", SELF_LANE, "r2");
    slot_acquire(&d, "build", SELF_LANE, "r3"); // the waiter
    let home = TempDir::new().unwrap();
    let out = cadence_at(home.path(), &d.state, &["status"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("slots: 2/2 build, 0/1 suite; waiting: 1"),
        "{text}"
    );
    let out = cadence_at(home.path(), &d.state, &["status", "--json"]);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["footer"]["slots"]["pools"]["build"]["held"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(v["footer"]["slots"]["waiting"].as_array().unwrap().len(), 1);
}

/// `issue start` writes the worktree slot env: `CARGO_BUILD_JOBS` from
/// `[host] jobs_per_lane` plus the helper path — idempotent, and a
/// foreign line in an existing `.env` survives.
#[test]
fn issue_start_writes_slot_env() {
    let d = TestDaemon::start();
    let tmp = TempDir::new().unwrap();
    let (pm_dir, repo, home) = (
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("home"),
    );
    for dir in [&pm_dir, &repo, &home] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let git = |dir: &Path, args: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr)
        );
    };
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "x").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    let cli = |args: &[&str]| -> (bool, Value) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&d.state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        let text = if out.stdout.is_empty() {
            String::from_utf8_lossy(&out.stderr).to_string()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        (
            out.status.success(),
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
        )
    };
    assert!(cli(&["issue", "init"]).0);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s]).0);
    assert!(cli(&["issue", "new", "One", "--project", "demo"]).0);
    // The [host] override lands before the start reads it.
    let pm_yaml = pm_dir.join("pm.yaml");
    let mut yaml = std::fs::read_to_string(&pm_yaml).unwrap();
    yaml.push_str("host:\n  jobs_per_lane: 7\n");
    std::fs::write(&pm_yaml, yaml).unwrap();
    let (ok, out) = cli(&["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let env_file = PathBuf::from(out["slot_env"]["path"].as_str().unwrap());
    assert_eq!(
        env_file,
        Path::new(out["worktree"].as_str().unwrap()).join(".env")
    );
    let text = std::fs::read_to_string(&env_file).unwrap();
    assert!(text.contains("CARGO_BUILD_JOBS=7"), "{text}");
    assert!(text.contains("CADENCE_BUILD_SLOT="), "{text}");
    assert!(text.contains("cadence"), "{text}");
    // Created 0600 — the file may hold build secrets someday.
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&env_file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // A second start is idempotent and keeps foreign lines — and an
    // existing file's mode survives the atomic rewrite.
    std::fs::write(&env_file, format!("OTHER=1\n{text}")).unwrap();
    std::fs::set_permissions(&env_file, std::fs::Permissions::from_mode(0o640)).unwrap();
    let (ok, _) = cli(&["issue", "start", "D-1"]);
    assert!(ok);
    let text = std::fs::read_to_string(&env_file).unwrap();
    assert_eq!(text.matches("CARGO_BUILD_JOBS=").count(), 1, "{text}");
    assert!(text.contains("OTHER=1"), "{text}");
    assert_eq!(
        std::fs::metadata(&env_file).unwrap().permissions().mode() & 0o777,
        0o640,
        "existing mode preserved"
    );
    drop(d);
}

/// `build-slot run` binds the hold to the REAL command process: the
/// CLI acquires with its own pid then execs, so the slot's holder IS
/// the running command — its exit frees the slot.
#[test]
fn build_slot_run_binds_the_real_process() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let home = TempDir::new().unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "build-slot",
            "run",
            "build",
            "--wait-secs",
            "5",
            "--",
            "sleep",
            "30",
        ])
        .env("HOME", home.path())
        .envs(test_env().vars())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // After exec the spawned pid IS `sleep 30` — the hold must bind
    // to exactly that process, not a wrapper that already exited.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let s = d.rpc("slot_status", json!({"lane": "unknown"})).unwrap();
        let held = s["pools"]["build"]["held"].as_array().unwrap();
        if held
            .iter()
            .any(|h| h["pid"].as_u64() == Some(child.id() as u64))
        {
            break;
        }
        assert!(Instant::now() < deadline, "run never held the slot: {s}");
        thread::sleep(Duration::from_millis(50));
    }
    // The command's exit frees its slot on the next read.
    child.kill().unwrap();
    child.wait().unwrap();
    let s = d.rpc("slot_status", json!({"lane": "unknown"})).unwrap();
    assert!(
        s["pools"]["build"]["held"].as_array().unwrap().is_empty(),
        "the command's exit frees its slot: {s}"
    );
    // A short command exits cleanly through run.
    let out = cadence_at(
        home.path(),
        &d.state,
        &[
            "build-slot",
            "run",
            "build",
            "--wait-secs",
            "5",
            "--",
            "true",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The CLI's queued path: `--wait-secs > 0` polls until a release
/// frees the pool — and `--pid` binds the hold to the named holder
/// (this test process, the CLI's parent).
#[test]
fn build_slot_cli_wait_then_grant() {
    let d = TestDaemon::start_opts(slot_opts(1, 1, 900, &[]));
    plant_self(&d);
    let home = TempDir::new().unwrap();
    let t1 = slot_acquire(&d, "build", SELF_LANE, "r1")["token"]
        .as_str()
        .unwrap()
        .to_string();
    let me = std::process::id().to_string();
    let cli = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "build-slot",
            "acquire",
            "build",
            "--wait-secs",
            "15",
            "--lane",
            "dev-9",
            "--pid",
            &me,
        ])
        .env("HOME", home.path())
        .envs(test_env().vars())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Let it queue — the waiter shows in status, then a release
    // frees the pool and the next poll grants.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let s = d.rpc("slot_status", json!({"lane": "dev-9"})).unwrap();
        if !s["waiting"].as_array().unwrap().is_empty() {
            break;
        }
        assert!(Instant::now() < deadline, "CLI never queued: {s}");
        thread::sleep(Duration::from_millis(50));
    }
    slot_release(&d, &t1, SELF_LANE, std::process::id());
    let out = cli.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(token.starts_with("slot-"), "minted token: {token}");
    // The explicit --pid bound the hold to the named pid — the test
    // process, still alive. The CLI's `--lane dev-9` was advisory:
    // the derived lane is this pane's alias.
    let s = d.rpc("slot_status", json!({})).unwrap();
    let held = s["pools"]["build"]["held"].as_array().unwrap();
    assert_eq!(held[0]["pid"].as_u64().unwrap() as u32, std::process::id());
    assert_eq!(held[0]["lane"], SELF_LANE);
    slot_release(&d, &token, SELF_LANE, std::process::id());
}

fn cadence_bin(state: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .output()
        .unwrap()
}

/// Inherited defaults, explicit override, resume stability, and a mock
/// argv that receives the resolved model. No paid provider is launched.
#[test]
fn model_defaults_register_resume_and_mock_argv() {
    let d = TestDaemon::start();
    let mock = d.mock_claude("ok", None);
    let health = d.rpc("health", json!({})).unwrap();
    assert!(health["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|cap| cap == "model_defaults"));
    let cwd = d.dir.path().to_str().unwrap();
    let doc_a = r#"{"expected_revision":0,"config":{"schema":1,"providers":{"claude":{"default":{"mode":"model","model":"baseline-a"},"roles":{"qa":{"mode":"model","model":"qa-model"},"dev":{"mode":"provider_default"}}}}}}"#;
    d.rpc("model_defaults_set", json!({"document": doc_a}))
        .unwrap();

    let conflict = cadence_bin(
        &d.state,
        &[
            "agent",
            "register",
            "nope",
            "--provider",
            "claude",
            "--endpoint",
            "managed",
            "--cwd",
            cwd,
            "--param",
            "model=sonnet",
            "--provider-default-model",
        ],
    );
    assert!(
        !conflict.status.success(),
        "explicit model and provider-default must conflict"
    );
    assert!(
        String::from_utf8_lossy(&conflict.stderr).contains("provider-default-model"),
        "{}",
        String::from_utf8_lossy(&conflict.stderr)
    );
    let unsupported = cadence_bin(
        &d.state,
        &[
            "agent",
            "register",
            "d1",
            "--provider",
            "devin",
            "--endpoint",
            "pty",
            "--cwd",
            cwd,
            "--provider-default-model",
        ],
    );
    assert!(!unsupported.status.success());
    assert!(String::from_utf8_lossy(&unsupported.stderr).contains("does not accept a model"));

    d.rpc(
        "agent_register",
        json!({"alias": "pm", "provider": "fake", "endpoint_kind": "fake", "cwd": cwd, "role": "pm"}),
    )
    .unwrap();
    let joined = cadence_bin(
        &d.state,
        &[
            "join",
            "pm",
            "claude",
            "--alias",
            "qa-cli",
            "--team-role",
            "qa",
            "--detach",
            "--no-bootstrap",
        ],
    );
    assert!(
        joined.status.success(),
        "{}",
        String::from_utf8_lossy(&joined.stderr)
    );
    d.wait_agent("qa-cli", "idle", 20);
    d.rpc(
        "agent_send",
        json!({"alias": "qa-cli", "text": "boot", "message": "m-qa"}),
    )
    .unwrap();
    d.wait_message("qa-cli", "m-qa", &["completed"], 20);
    let argv_file = mock.pidfile.with_extension("pid.argv");
    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(argv.contains("--model\nqa-model"), "{argv}");
    assert!(
        !argv.contains("team_role") && !argv.contains("devops"),
        "{argv}"
    );
    let qa = d.rpc("agent_show", json!({"alias": "qa-cli"})).unwrap()["agent"].clone();
    assert_eq!(qa["role"], "worker");
    assert_eq!(qa["team_role"], "qa");
    assert_eq!(qa["model_selection"]["source"], "role_default");
    assert_eq!(qa["model_selection"]["model"], "qa-model");
    assert_eq!(qa["model_configured"], "qa-model");
    assert_eq!(qa["model_reported"], "mock-claude");
    assert_ne!(qa["model_configured"], qa["model_reported"]);

    let explicit = cadence_bin(
        &d.state,
        &[
            "agent",
            "register",
            "explicit",
            "--provider",
            "claude",
            "--endpoint",
            "managed",
            "--cwd",
            cwd,
            "--team-role",
            "qa",
            "--param",
            "model=explicit-model",
        ],
    );
    assert!(
        explicit.status.success(),
        "{}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    d.wait_agent("explicit", "idle", 20);
    d.rpc(
        "agent_send",
        json!({"alias": "explicit", "text": "boot", "message": "m-ex"}),
    )
    .unwrap();
    d.wait_message("explicit", "m-ex", &["completed"], 20);
    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(argv.contains("--model\nexplicit-model"), "{argv}");
    let shown = d.rpc("agent_show", json!({"alias": "explicit"})).unwrap();
    assert_eq!(shown["agent"]["model_selection"]["source"], "explicit");
    assert!(shown["agent"]["model_selection"]["revision"].is_null());

    let doc_b = r#"{"expected_revision":1,"config":{"schema":1,"providers":{"claude":{"default":{"mode":"model","model":"baseline-b"},"roles":{}}}}}"#;
    d.rpc("model_defaults_set", json!({"document": doc_b}))
        .unwrap();
    d.rpc("agent_stop", json!({"alias": "qa-cli"})).unwrap();
    d.rpc("agent_resume", json!({"alias": "qa-cli"})).unwrap();
    d.wait_agent("qa-cli", "idle", 20);
    d.rpc(
        "agent_send",
        json!({"alias": "qa-cli", "text": "again", "message": "m-resume"}),
    )
    .unwrap();
    d.wait_message("qa-cli", "m-resume", &["completed"], 20);
    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(argv.contains("--model\nqa-model"), "{argv}");
    assert!(!argv.contains("baseline-b"), "{argv}");

    d.rpc(
        "agent_register",
        json!({"alias": "fresh", "provider": "claude", "endpoint_kind": "managed", "cwd": cwd, "role": "worker"}),
    )
    .unwrap();
    d.wait_agent("fresh", "idle", 20);
    d.rpc(
        "agent_send",
        json!({"alias": "fresh", "text": "boot", "message": "m-fresh"}),
    )
    .unwrap();
    d.wait_message("fresh", "m-fresh", &["completed"], 20);
    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(argv.contains("--model\nbaseline-b"), "{argv}");

    d.rpc(
        "agent_set",
        json!({"alias": "fresh", "next_launch": true, "patch": {"model": null}}),
    )
    .unwrap();
    let cleared = d.rpc("agent_show", json!({"alias": "fresh"})).unwrap()["agent"].clone();
    assert!(cleared["params"].get("model").is_none() || cleared["params"]["model"].is_null());
    assert_eq!(
        cleared["model_selection"]["source"],
        "explicit_provider_default"
    );
    d.rpc("agent_stop", json!({"alias": "fresh"})).unwrap();
    d.rpc("agent_resume", json!({"alias": "fresh"})).unwrap();
    d.wait_agent("fresh", "idle", 20);
    d.rpc(
        "agent_send",
        json!({"alias": "fresh", "text": "native", "message": "m-native"}),
    )
    .unwrap();
    d.wait_message("fresh", "m-native", &["completed"], 20);
    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(!argv.contains("--model"), "{argv}");

    d.rpc(
        "agent_register",
        json!({"alias": "box", "provider": "inbox", "endpoint_kind": "inbox", "team_role": "ops"}),
    )
    .unwrap();
    let inbox = d.rpc("agent_show", json!({"alias": "box"})).unwrap()["agent"].clone();
    assert!(inbox["model_selection"].is_null());
    assert!(inbox["model_configured"].is_null());
    assert_eq!(inbox["team_role"], "devops");
    assert_eq!(inbox["role"], "worker");

    let again = d.rpc(
        "agent_register",
        json!({"alias": "qa-cli", "provider": "claude", "endpoint_kind": "managed", "cwd": cwd, "team_role": "dev"}),
    );
    assert!(again.unwrap_err().to_string().contains("UNIQUE"));
    assert_eq!(
        d.rpc("agent_show", json!({"alias": "qa-cli"})).unwrap()["agent"]["params"]["model"],
        "qa-model"
    );
}
