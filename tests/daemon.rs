//! daemon: area tests split from tests/integration.rs (CAD-426).
//! End-to-end tests: real socket daemon in-process, fake provider.
//! These exercise the observable contract — queue order, idempotency,
//! restart fencing, approval brokering, serialization — without model calls.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use cadence_agent::client;
use cadence_agent::daemon;
use cadence_agent::store::NewAgent;
use cadence_agent::store::Store;
use cadence_agent::store::Take;
use serde_json::json;
use serde_json::Value;
use std::path::Path;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use tempfile::TempDir;

#[test]
fn fifo_queue_and_idempotent_send() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    for n in 1..=3 {
        d.send(
            "w1",
            json!({"text": format!("task {n}"), "message": format!("m{n}")}),
        )
        .unwrap();
    }
    // Duplicate of the exact same envelope is a no-op.
    let dup = d
        .send("w1", json!({"text": "task 1", "message": "m1"}))
        .unwrap();
    assert_eq!(dup["duplicate"], true);
    // Same id, different content: conflict.
    let conflict = d.send("w1", json!({"text": "different", "message": "m1"}));
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
    d.send(
        "w1",
        json!({"text": "review this", "message": "work-1", "reply_to": "pm"}),
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
    d.register_pcp("w1", "fake", "fake", &cwd, "{\"upstream\":\"pm\"}")
        .unwrap();
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("other", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    // No explicit reply_to — the upstream wiring routes the result to pm.
    d.send("w1", json!({"text": "work", "message": "u1"}))
        .unwrap();
    d.wait_message("w1", "u1", &["completed"], 15);
    // CAD-271: routed in u1's completing transaction — durable now. Find
    // it by source rather than position, and name a refused route.
    let show = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    let routed = show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["source"] == "worker_result")
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "u1's result not routed to pm: {show} {:?}",
                d.events("daemon")
            )
        });
    assert!(routed["body"].as_str().unwrap().contains("u1"));
    let routed_id = routed["id"].as_str().unwrap().to_string();
    d.wait_message("pm", &routed_id, &["completed"], 15);
    // An explicit reply_to still wins over the upstream default.
    d.send(
        "w1",
        json!({"text": "more work", "message": "u2", "reply_to": "other"}),
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
    d.send(
        "w1",
        json!({"text": "NEED_INPUT:run-tests", "message": "a1"}),
    )
    .unwrap();
    d.wait_agent("w1", "waiting_input", 10);
    let requests = d
        .operator_rpc("agent_requests", json!({"alias": "w1"}))
        .unwrap();
    let list = requests["requests"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["method"], "item/commandExecution/requestApproval");
    let handle = list[0]["request"].as_str().unwrap();
    // Wrong shape rejected.
    assert!(d
        .operator_rpc(
            "agent_respond",
            json!({"alias": "w1", "request": handle, "decision": "maybe"}),
        )
        .is_err());
    d.operator_rpc(
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
        d.operator_rpc("agent_requests", json!({"alias": "w1"}))
            .unwrap()["requests"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn restart_fences_unknown_inflight() {
    // Seed a state dir with an in-flight attempt, then start a daemon.
    let (_seeded, state) = seeded_state(&[("w1", None, "fake", "worker")], |store, _cwd| {
        store.enqueue("w1", "work", None, "m1", "user").unwrap();
        match store.take_queued("w1").unwrap() {
            Take::Message(m) => assert_eq!(m.id, "m1"),
            _ => panic!("expected a message"),
        }
        // Simulate crash: store dropped while m1 is 'submitting'.
    });
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
    d.send("w1", json!({"text": "later", "message": "m2"}))
        .unwrap();
    // CAD-184 kept sleep: absence window — no actor runs for a fenced or
    // stopped agent, so nothing records a refusal to poll for.
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
    let (_seeded, state) = seeded_state(&[("w1", None, "fake", "worker")], |store, _cwd| {
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
    });
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
    d.send("w1", json!({"text": "later", "message": "m3"}))
        .unwrap();
    // CAD-184 kept sleep: absence window — no actor runs for a fenced or
    // stopped agent, so nothing records a refusal to poll for.
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
        d.send("w1", json!({"text": format!("job {n}")})).unwrap();
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
    d.operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
    d.wait_agent("w1", "stopped", 10);
    d.operator_rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap();
    let agent = d.wait_agent("w1", "idle", 10);
    assert_eq!(agent["thread_id"], "fake-thread-w1");
}

#[test]
fn unknown_outcome_never_replays() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    d.send("w1", json!({"text": "DISCONNECT", "message": "x1"}))
        .unwrap();
    let m = d.wait_message("w1", "x1", &["unknown"], 15);
    // The fence carries the provider's own reason, not a generic label.
    assert!(
        m["error"].as_str().unwrap().contains("Connection lost"),
        "{m}"
    );
    d.wait_agent("w1", "attention", 10);
    // Subsequent messages stay queued — no automatic replay or relaunch.
    d.send("w1", json!({"text": "after", "message": "x2"}))
        .unwrap();
    // CAD-184 kept sleep: absence window — no actor runs for a fenced or
    // stopped agent, so nothing records a refusal to poll for.
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
fn reconcile_interrupted_clears_fence_and_preserves_history() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    fence_agent(&d, "w1", "x1");
    // Work queued behind the fence stays queued.
    d.send("w1", json!({"text": "after", "message": "x2"}))
        .unwrap();
    // A bare resume is rejected, naming the reconcile-first path.
    let err = d
        .operator_rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap_err();
    let err = err.to_string();
    assert!(err.contains("resume refused"), "{err}");
    assert!(err.contains("--no-resume"), "{err}");
    assert!(!err.contains("then `cadence agent resume"), "{err}");
    // The operator's verdict: interrupted, with a note. The caller is
    // the verified connection's (CAD-374): a claimed `by` is refused.
    let e = d
        .operator_rpc(
            "message_reconcile",
            json!({"message": "x1", "status": "interrupted", "by": "cookie-cesium"}),
        )
        .unwrap_err();
    assert!(e.to_string().contains("'by' is not accepted"), "{e}");
    let r = d
        .operator_rpc(
            "message_reconcile",
            json!({"message": "x1", "status": "interrupted",
                   "note": "pane lost mid-turn"}),
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
    assert_eq!(rec["payload"]["by"], "operator");
    // History is intact: x1 still listed (interrupted), x2 still queued.
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(show["unknown"], 0);
    assert_eq!(d.message_state("w1", "x1"), "interrupted");
    assert_eq!(d.message_state("w1", "x2"), "queued");
    // The normal resume path works again and drains the backlog.
    d.operator_rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap();
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
    d.send(
        "w1",
        json!({"text": "DISCONNECT", "message": "x1", "reply_to": "pm"}),
    )
    .unwrap();
    d.send(
        "w2",
        json!({"text": "DISCONNECT", "message": "x2", "reply_to": "pm"}),
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
    d.operator_rpc(
        "message_reconcile",
        json!({"message": "x1", "status": "completed",
               "note": "pane showed the answer"}),
    )
    .unwrap();
    // interrupted → one more notice (the operator closed the turn),
    // still no result.
    d.operator_rpc(
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
    d.send("w1", json!({"text": "done", "message": "c1"}))
        .unwrap();
    d.wait_message("w1", "c1", &["completed"], 15);
    let err = d
        .operator_rpc(
            "message_reconcile",
            json!({"message": "c1", "status": "interrupted"}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("'completed'"), "{err}");
    // running → rejected, naming the state.
    d.send("w1", json!({"text": "NEED_INPUT:hold", "message": "r1"}))
        .unwrap();
    d.wait_message("w1", "r1", &["running"], 15);
    let err = d
        .operator_rpc(
            "message_reconcile",
            json!({"message": "r1", "status": "interrupted"}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("'running'"), "{err}");
    // queued → rejected, naming the state (w1 is busy holding r1).
    d.send("w1", json!({"text": "next", "message": "q2"}))
        .unwrap();
    assert_eq!(d.message_state("w1", "q2"), "queued");
    let err = d
        .operator_rpc(
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
    d.operator_rpc(
        "message_reconcile",
        json!({"message": "x9", "status": "failed"}),
    )
    .unwrap();
    let err = d
        .operator_rpc(
            "message_reconcile",
            json!({"message": "x9", "status": "failed"}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("'failed'"), "{err}");
    // An invalid status is rejected before any state check.
    let err = d
        .operator_rpc(
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
        .operator_rpc(
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
        .operator_rpc(
            "agent_unfence",
            json!({"alias": "w1", "status": "interrupted",
                   "note": "bulk"}),
        )
        .unwrap();
    assert_eq!(r["reconciled"], json!(["x1"]));
    assert_eq!(r["state"], "stopped");
    assert_eq!(d.message_state("w1", "x1"), "interrupted");
    // Resume now works through the normal path.
    d.operator_rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap();
    d.wait_agent("w1", "idle", 15);
}

#[test]
fn restart_preserves_attention_fence_without_unknowns() {
    // Seed: an agent fenced for a session-mismatch — `attention` state,
    // recorded error, stored thread — with NO unknown messages.
    // recover() must not rewrite the fence to `offline` before the
    // serve loop reads it.
    let (_seeded, state) = seeded_state(
        &[
            ("mismatch", None, "fake", "worker"),
            ("healthy", None, "fake", "worker"),
        ],
        |store, _cwd| {
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
        },
    );
    let d = TestDaemon::start_on(state);
    let _reaper = DaemonReaper::new(&d.state);
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
    d.send("mismatch", json!({"text": "later", "message": "m2"}))
        .unwrap();
    // CAD-184 kept sleep: absence window — no actor runs for a fenced or
    // stopped agent, so nothing records a refusal to poll for.
    thread::sleep(Duration::from_secs(1));
    assert_eq!(d.message_state("mismatch", "m2"), "queued");
    // `resume --all` agrees with startup: reported under `fenced` with
    // the remove-and-rejoin hint — never attempted.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["resume", "--all"])
        .operator_output()
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
    hold_rollout_lease(home.path(), &d.state);
    let out = operator_cadence_at(
        home.path(),
        &d.state,
        &["daemon", "restart", "--as", "operator:test"],
    );
    // Stop the detached replacement before asserting the restart outcome.
    let restarted = d.wait_agent("mismatch", "attention", 15);
    d.wait_agent("healthy", "idle", 15);
    let queued = d.message_state("mismatch", "m2");
    let stop = operator_cadence_at(home.path(), &d.state, &["daemon", "stop"]);
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
    d.operator_rpc(
        "agent_unfence",
        json!({"alias": "w1", "status": "interrupted"}),
    )
    .unwrap();
    let agent = d.wait_agent("w1", "stopped", 10);
    assert_eq!(agent["enabled"], false);
    // Daemon restart: a stopped, disabled member is not relaunched.
    d.operator_rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    d.wait_agent("w2", "idle", 15);
    // CAD-184 kept sleep: absence window — the relaunch pass records
    // nothing for a stopped, disabled member.
    thread::sleep(Duration::from_secs(1));
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(agent["state"], "stopped");
    assert_eq!(agent["enabled"], false);
    assert!(agent["endpoint"].is_null());
    // No actor spawned: a queued message is never taken.
    d.send("w1", json!({"text": "later", "message": "x2"}))
        .unwrap();
    // CAD-184 kept sleep: absence window — no actor runs for a fenced or
    // stopped agent, so nothing records a refusal to poll for.
    thread::sleep(Duration::from_secs(1));
    assert_eq!(d.message_state("w1", "x2"), "queued");
    // The operator's explicit resume still works — the normal path.
    d.operator_rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap();
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
    d.operator_rpc("agent_stop", json!({"alias": "w2"}))
        .unwrap();
    d.wait_agent("w2", "stopped", 10);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["resume", "--all"])
        .operator_output()
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

// ---- review regressions: daemon singleton, actor ownership, bounded
// disconnect/stop, unknown fencing, initialization cleanup ----

#[test]
fn second_daemon_fails_without_touching_state() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    // Active work in-flight: a second daemon's recovery must never run.
    d.send("w1", json!({"text": "NEED_INPUT:hold", "message": "m1"}))
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
        d.operator_rpc("agent_requests", json!({"alias": "w1"}))
            .unwrap()["requests"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn resume_rejected_while_actor_stopping() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    // SLEEP ignores interrupt; only a forced close ends the turn, which
    // makes the in-flight attempt unknown — a real stopping window.
    d.send("w1", json!({"text": "SLEEP:60", "message": "m1"}))
        .unwrap();
    d.wait_message("w1", "m1", &["running"], 10);
    let stop = {
        let state = d.state.clone();
        thread::spawn(move || {
            cadence_agent::test_seam::scoped(cadence_agent::test_seam::Asserted::Operator, || {
                client::rpc(&state, "agent_stop", json!({"alias": "w1"}))
            })
        })
    };
    // While the stop grace runs, the actor is still owned: resume must
    // be rejected and must not re-enable the agent. `stopping` is
    // written after the stop reserved the alias and before the grace.
    d.wait_agent("w1", "stopping", 10);
    let resumed = d.operator_rpc("agent_resume", json!({"alias": "w1"}));
    assert!(resumed.is_err(), "resume during stop must be rejected");
    let stopped = stop.join().unwrap().unwrap();
    // Forced close made the in-flight attempt unknown -> fenced.
    assert_eq!(stopped["state"], "attention");
    let agent = d.wait_agent("w1", "attention", 10);
    assert_eq!(agent["enabled"], false);
    d.wait_message("w1", "m1", &["unknown"], 10);
    // A later resume is rejected by the fence — it is not a relaunch
    // and stays disabled. The rejection does not chain a second resume.
    let fenced = d
        .operator_rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap_err();
    let fenced = fenced.to_string();
    assert!(fenced.contains("resume refused"), "{fenced}");
    assert!(!fenced.contains("then `cadence agent resume"), "{fenced}");
    let agent = d
        .rpc("agent_show", json!({"alias": "w1"}))
        .unwrap()
        .remove("agent");
    assert_eq!(agent["enabled"], false);
    // No second actor ever existed: m2 is accepted but never runs.
    d.send("w1", json!({"text": "later", "message": "m2"}))
        .unwrap();
    // CAD-184 kept sleep: absence window — no actor runs for a fenced or
    // stopped agent, so nothing records a refusal to poll for.
    thread::sleep(Duration::from_millis(500));
    assert_eq!(d.message_state("w1", "m2"), "queued");
}

#[test]
fn concurrent_resume_has_single_winner() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    d.operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
    d.wait_agent("w1", "stopped", 10);
    let mut racers = Vec::new();
    for _ in 0..4 {
        let state = d.state.clone();
        racers.push(thread::spawn(move || {
            cadence_agent::test_seam::scoped(cadence_agent::test_seam::Asserted::Operator, || {
                client::rpc(&state, "agent_resume", json!({"alias": "w1"}))
            })
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
    d.send("w1", json!({"text": "NEED_INPUT:block", "message": "m1"}))
        .unwrap();
    d.wait_agent("w1", "waiting_input", 10);
    let began = Instant::now();
    let stopped = d
        .operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
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
        d.operator_rpc("agent_requests", json!({"alias": "w1"}))
            .unwrap()["requests"]
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
    d.send("w1", json!({"text": "BAD_STATUS", "message": "m1"}))
        .unwrap();
    d.wait_message("w1", "m1", &["unknown"], 15);
    d.wait_agent("w1", "attention", 10);
    // Queued work is preserved but never run while fenced.
    d.send("w1", json!({"text": "after", "message": "m2"}))
        .unwrap();
    // CAD-184 kept sleep: absence window — no actor runs for a fenced or
    // stopped agent, so nothing records a refusal to poll for.
    thread::sleep(Duration::from_millis(500));
    assert_eq!(d.message_state("w1", "m2"), "queued");
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

/// CAD-18: `params.permission_mode` lands on the agent row at register
/// and is replayed into the pane argv on every open — the fresh launch
/// gets `--permission-mode <mode>`, the resume gets it plus `-r <sid>`.
#[test]
fn devin_permission_mode_persisted_and_replayed() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.register_pcp(
        "dv1",
        "devin",
        "pty",
        &cwd,
        &json!({"permission_mode": "smart"}).to_string(),
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
    d.operator_rpc("agent_stop", json!({"alias": "dv1"}))
        .unwrap();
    d.wait_agent("dv1", "stopped", 15);
    d.operator_rpc("agent_resume", json!({"alias": "dv1"}))
        .unwrap();
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
            .register_pcp(
                "bad",
                "devin",
                "pty",
                &cwd,
                &json!({"permission_mode": bad}).to_string(),
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
    let err = d.register_pcp(
        "cl1",
        "claude",
        "managed",
        &cwd,
        &json!({"permission_mode": "anything-goes"}).to_string(),
    );
    assert!(err.is_ok(), "claude params must pass through: {err:?}");
    d.operator_rpc("agent_stop", json!({"alias": "cl1"}))
        .unwrap();
    // agent set: launch params are not live-settable.
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    let err = d
        .operator_rpc(
            "agent_set",
            json!({"alias": "dv1", "patch": {"permission_mode": "smart"}}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("not live-settable"), "{err}");
    // auto_ready stays the one live-settable key — the patch still works.
    d.operator_rpc(
        "agent_set",
        json!({"alias": "dv1", "patch": {"auto_ready": "verified"}}),
    )
    .unwrap();
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
        .operator_output()
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
        .operator_output()
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
        .operator_output()
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
        .operator_output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("git repository"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn fenced_agent_resume_hint() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    let agent = d.wait_agent("dv1", "idle", 20);
    let native = agent["thread_id"].as_str().unwrap().to_string();
    d.operator_rpc("agent_ready", json!({"alias": "dv1"}))
        .unwrap();
    d.send("dv1", json!({"text": "task", "message": "m1"}))
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

/// CAD-437: `agent list` shares the grammar — server-side any-of on
/// state/provider/kind, AND across flags, unknown values name the
/// valid set, and filters narrow the caller's group scope.
#[test]
fn agent_list_cad437_filters() {
    let d = TestDaemon::start();
    d.register("pm1");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.register_pcp("w1", "fake", "fake", &cwd, "{\"upstream\":\"pm1\"}")
        .unwrap();
    d.wait_agent("pm1", "idle", 10);
    d.wait_agent("w1", "idle", 10);

    // Server-side: any-of within a param, AND across params.
    let v = d.rpc("agent_list", json!({"states": ["idle"]})).unwrap();
    assert_eq!(v["agents"].as_array().unwrap().len(), 2);
    let v = d
        .rpc("agent_list", json!({"states": ["idle", "busy"]}))
        .unwrap();
    assert_eq!(v["agents"].as_array().unwrap().len(), 2);
    let v = d.rpc("agent_list", json!({"states": ["busy"]})).unwrap();
    assert_eq!(v["agents"].as_array().unwrap().len(), 0);
    let v = d
        .rpc(
            "agent_list",
            json!({"states": ["idle"], "providers": ["fake"],
                   "kinds": ["fake"]}),
        )
        .unwrap();
    assert_eq!(v["agents"].as_array().unwrap().len(), 2);
    // AND across params: a real provider no agent uses selects none.
    let v = d
        .rpc(
            "agent_list",
            json!({"states": ["idle"], "providers": ["claude"]}),
        )
        .unwrap();
    assert_eq!(v["agents"].as_array().unwrap().len(), 0);
    // Unknown values are errors naming the valid set.
    let err = d.rpc("agent_list", json!({"states": ["zzz"]})).unwrap_err();
    assert!(err.to_string().contains("idle"), "{err}");
    assert!(d.rpc("agent_list", json!({"providers": ["zzz"]})).is_err());
    assert!(d.rpc("agent_list", json!({"kinds": ["zzz"]})).is_err());

    // CLI: comma-joined and repeated flags are the same any-of; the
    // caller's group scope still binds.
    let bin = env!("CARGO_BIN_EXE_cadence");
    let run = |alias: Option<&str>, extra: &[&str]| -> (bool, String, String) {
        let mut cmd = std::process::Command::new(bin);
        cmd.arg("--state-dir")
            .arg(&d.state)
            .args(["agent", "list"])
            .args(extra)
            .env_remove("CADENCE_ALIAS");
        if let Some(a) = alias {
            cmd.env("CADENCE_ALIAS", a);
        }
        let out = cmd.output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let names = |v: &Value| -> Vec<String> {
        let mut n: Vec<String> = v["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["alias"].as_str().unwrap().to_string())
            .collect();
        n.sort();
        n
    };
    let (ok, out, err) = run(None, &["--state", "idle,busy", "--json"]);
    assert!(ok, "{err}");
    assert_eq!(names(&serde_json::from_str(&out).unwrap()), ["pm1", "w1"]);
    let (ok, out, err) = run(None, &["--provider", "fake", "--json"]);
    assert!(ok, "{err}");
    assert_eq!(
        names(&serde_json::from_str::<Value>(&out).unwrap()),
        ["pm1", "w1"]
    );
    // A state nobody is in narrows a pane-scoped list to empty — it
    // never widens it.
    let (ok, out, err) = run(Some("w1"), &["--state", "offline", "--json"]);
    assert!(ok, "{err}");
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["agents"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    // sort desc + limit + fields.
    let (ok, out, err) = run(None, &["--sort", "-alias", "--limit", "1", "--json"]);
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["agents"][0]["alias"], "w1");
    let (ok, out, err) = run(None, &["--fields", "alias,state", "--json"]);
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    let keys: Vec<&String> = v["agents"][0].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["alias", "state"], "{v}");
    // Unknown values fail on the CLI too. The output is JSON either
    // way, so --fields does not need --json on this command.
    let (ok, _, err) = run(None, &["--state", "zzz"]);
    assert!(!ok && err.contains("idle"), "{err}");
    let (ok, out, err) = run(None, &["--fields", "alias"]);
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    let keys: Vec<&String> = v["agents"][0].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["alias"], "{v}");
    let (ok, _, err) = run(None, &["--sort", "zzz"]);
    assert!(!ok && err.contains("--sort"), "{err}");
}

/// `cadence resume --all` sweeps every resumable agent; `daemon start
/// --resume` runs the same sweep once the daemon answers.
#[test]
fn resume_all_and_daemon_start_resume_sweep() {
    let d = TestDaemon::start();
    let _reaper = DaemonReaper::new(&d.state);
    d.register("a1");
    d.register("a2");
    d.wait_agent("a1", "idle", 10);
    d.wait_agent("a2", "idle", 10);
    d.operator_rpc("agent_stop", json!({"alias": "a1"}))
        .unwrap();
    d.wait_agent("a1", "stopped", 15);
    let bin = env!("CARGO_BIN_EXE_cadence");

    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["resume", "--all"])
        .operator_output()
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
        .operator_output()
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
        .operator_rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("cadence attach w1"), "{err}");
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
    d.fixture_rpc(
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
        d.send("obs", json!({"text": text, "message": id})).unwrap();
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
        // CAD-184 kept sleep: the late arrival IS the behaviour under test.
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
    d.fixture_rpc(
        "agent_register",
        json!({"alias": "w1", "provider": "fake", "endpoint_kind": "fake",
               "cwd": cwd, "params": json!({"upstream": "obs"}).to_string()}),
    )
    .unwrap();
    d.wait_agent("w1", "idle", 10);
    // The worker's send defaults reply_to=obs (its upstream) — the
    // completed result routes into the mailbox, not a pane.
    d.send("w1", json!({"text": "do work", "message": "j1"}))
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
    // result routes into the mailbox. Ready is claimed as the operator —
    // a fixture act, not the agent's own attestation.
    d.operator_rpc("agent_ready", json!({"alias": "sender"}))
        .unwrap();
    d.send(
        "sender",
        json!({"text": "task", "message": "t1", "reply_to": "obs"}),
    )
    .unwrap();
    let token = pty_token(&d, "sender", "t1");
    d.report("t1", &token, "result", "did the thing").unwrap();
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
    d.send(
        "worker",
        json!({"text": "do work", "message": "work-1", "reply_to": "obs"}),
    )
    .unwrap();
    d.wait_message("worker", "work-1", &["completed"], 15);
    // CAD-271: the routed result is inserted in the SAME transaction that
    // completes work-1, so it is already durable here — no wait needed.
    // Pin that cause directly: when this fails, the route was refused
    // (`handoff_unresolved`, e.g. the pre-merge CAD-176 head whose
    // recipient identity compared a JSON-rounded `created` f64 and
    // mismatched ~12% of timestamps), not raced by the drain below.
    let obs = d.rpc("agent_show", json!({"alias": "obs"})).unwrap();
    assert!(
        obs["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["source"] == "worker_result" && m["state"] == "queued"),
        "work-1's result must be queued on obs at completion: {obs} {:?}",
        d.events("daemon")
    );
    assert!(
        d.events("daemon")
            .iter()
            .all(|e| e["kind"] != "handoff_unresolved"),
        "{:?}",
        d.events("daemon")
    );

    // This is the actual receipt path: a mailbox message has a return
    // address, then the consumer drains it. Completing the read must not
    // manufacture a worker_result turn for the reviewer.
    d.send(
        "obs",
        json!({"text": "ack me", "message": "receipt-1", "reply_to": "reviewer"}),
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
    // Operator-gated verbs need the operator's proof before the store's
    // endpoint-kind check is even reached — assert the "inbox" refusal
    // through `operator_rpc` so the verdict under test is the mailbox's.
    for method in ["agent_resume", "agent_stop", "agent_ready"] {
        let err = d
            .operator_rpc(method, json!({"alias": "obs"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("inbox"), "{method}: {err}");
    }
    // Read-gated verbs admit an unattributed caller — plain `rpc`
    // reaches the same mailbox refusal.
    for method in ["agent_probe", "agent_capture"] {
        let err = d
            .rpc(method, json!({"alias": "obs"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("inbox"), "{method}: {err}");
    }
    // Removal works directly — a mailbox is never "live".
    d.operator_rpc("agent_remove", json!({"alias": "obs"}))
        .unwrap();
    assert!(d.rpc("agent_show", json!({"alias": "obs"})).is_err());
}

// ==== CAD-480: inbox peek/ack + reader cursor ====

/// Peek returns the queued set without consuming — a reader that
/// crashes or truncates after reading loses nothing.
#[test]
fn inbox_peek_loses_nothing_without_ack() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    for (id, text) in [("n1", "note one"), ("n2", "note two")] {
        d.send("obs", json!({"text": text, "message": id})).unwrap();
    }
    let page = d
        .rpc("agent_inbox", json!({"alias": "obs", "peek": true}))
        .unwrap();
    let msgs = page["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2, "{page}");
    assert_eq!(page["unread"], 2, "{page}");
    // Nothing was consumed: rows stay queued, and a reader that
    // "crashed" after this peek sees the same messages again.
    let show = d.rpc("agent_show", json!({"alias": "obs"})).unwrap();
    for m in show["messages"].as_array().unwrap() {
        assert_eq!(m["state"], "queued", "{m}");
    }
    let again = d
        .rpc("agent_inbox", json!({"alias": "obs", "peek": true}))
        .unwrap();
    assert_eq!(again["messages"].as_array().unwrap().len(), 2, "{again}");
    // An explicit `after` still bounds the peek.
    let seq1 = msgs[0]["seq"].as_i64().unwrap();
    let tail = d
        .rpc(
            "agent_inbox",
            json!({"alias": "obs", "peek": true, "after": seq1}),
        )
        .unwrap();
    let t = tail["messages"].as_array().unwrap();
    assert_eq!(t.len(), 1, "{tail}");
    assert_eq!(t[0]["id"], "n2");
}

/// `inbox ack` is a watermark: it completes every queued message at or
/// below `through`, records the reader's durable cursor, and a restart
/// resumes after it.
#[test]
fn inbox_ack_advances_the_reader_cursor() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    for id in ["n1", "n2", "n3"] {
        d.send("obs", json!({"text": id, "message": id})).unwrap();
    }
    let page = d
        .rpc("agent_inbox", json!({"alias": "obs", "peek": true}))
        .unwrap();
    let seqs: Vec<i64> = page["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["seq"].as_i64().unwrap())
        .collect();
    let ack = d
        .rpc(
            "agent_inbox_ack",
            json!({"alias": "obs", "seqs": [seqs[0], seqs[1]], "reader": "pm"}),
        )
        .unwrap();
    assert_eq!(ack["acked"], json!([seqs[0], seqs[1]]), "{ack}");
    assert_eq!(ack["unread"], 1, "{ack}");
    // A fresh peek — a restarted reader — resumes after the watermark.
    let resume = d
        .rpc(
            "agent_inbox",
            json!({"alias": "obs", "peek": true, "reader": "pm"}),
        )
        .unwrap();
    let left = resume["messages"].as_array().unwrap();
    assert_eq!(left.len(), 1, "{resume}");
    assert_eq!(left[0]["id"], "n3");
    // The queued set is shared — another reader sees the same rest;
    // its own cursor only differs once it acks.
    let other = d
        .rpc(
            "agent_inbox",
            json!({"alias": "obs", "peek": true, "reader": "pm2"}),
        )
        .unwrap();
    assert_eq!(other["messages"].as_array().unwrap().len(), 1);
    // Acked rows completed via=inbox_ack with the reader on the receipt.
    let show = d.rpc("agent_show", json!({"alias": "obs"})).unwrap();
    for m in show["messages"].as_array().unwrap() {
        if m["id"] == "n3" {
            assert_eq!(m["state"], "queued", "{m}");
            continue;
        }
        assert_eq!(m["state"], "completed", "{m}");
        assert_eq!(m["result"]["via"], "inbox_ack", "{m}");
        assert_eq!(m["result"]["reader"], "pm", "{m}");
    }
    // The cursor is durable mailbox evidence: status carries the
    // readers, the stream carries one `inbox_ack` event.
    assert_eq!(show["inbox"]["readers"]["pm"]["through"], seqs[1], "{show}");
    let acks: Vec<_> = d
        .events("obs")
        .into_iter()
        .filter(|e| e["kind"] == "inbox_ack")
        .collect();
    assert_eq!(acks.len(), 1, "{acks:?}");
    assert_eq!(acks[0]["payload"]["reader"], "pm");
    assert_eq!(acks[0]["payload"]["through"], seqs[1]);
    // An ack receipt never routes a synthetic worker result — the
    // mailbox stays a receipt, not a conversation (CAD-251).
    let routed: Vec<_> = show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["source"] == "worker_result")
        .collect();
    assert!(routed.is_empty(), "{routed:?}");
}

/// A `through` claim past the inbox's tail completes what exists but is
/// stored clamped to the tail — the reader's cursor can never blind it
/// to later arrivals. An operator `--reset` drops the cursor outright.
#[test]
fn inbox_ack_clamps_past_the_tail_and_reset_restores() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    for id in ["n1", "n2"] {
        d.send("obs", json!({"text": id, "message": id})).unwrap();
    }
    let page = d
        .rpc("agent_inbox", json!({"alias": "obs", "peek": true}))
        .unwrap();
    let tail = page["messages"].as_array().unwrap().last().unwrap()["seq"]
        .as_i64()
        .unwrap();
    // The poisoning claim: `through` far past the tail.
    let ack = d
        .rpc(
            "agent_inbox_ack",
            json!({"alias": "obs", "through": i64::MAX - 1, "reader": "pm"}),
        )
        .unwrap();
    assert_eq!(ack["through"], tail, "{ack}");
    let show = d.rpc("agent_show", json!({"alias": "obs"})).unwrap();
    assert_eq!(show["inbox"]["readers"]["pm"]["through"], tail, "{show}");
    // A later arrival is still visible to that reader — not blinded.
    d.send("obs", json!({"text": "n3", "message": "n3"}))
        .unwrap();
    let resume = d
        .rpc(
            "agent_inbox",
            json!({"alias": "obs", "peek": true, "reader": "pm"}),
        )
        .unwrap();
    assert_eq!(resume["messages"][0]["id"], "n3", "{resume}");

    // Reset is operator-only: a genuinely unattributed caller
    // (`unproven_rpc` — plain `rpc` IS the operator in CI) and a
    // proven foreign agent are both refused.
    let e = d
        .unproven_rpc(
            "agent_inbox_ack",
            json!({"alias": "obs", "reset": true, "reader": "pm"}),
        )
        .unwrap_err()
        .to_string();
    assert!(e.contains("operator action"), "{e}");
    plant_pane(&d, "w1", std::process::id());
    let e = d
        .rpc(
            "agent_inbox_ack",
            json!({"alias": "obs", "reset": true, "reader": "pm"}),
        )
        .unwrap_err()
        .to_string();
    assert!(e.contains("cannot ack another"), "{e}");
    // The operator's reset drops the cursor — the still-queued message
    // comes back; the completed ones stay completed.
    d.operator_rpc(
        "agent_inbox_ack",
        json!({"alias": "obs", "reset": true, "reader": "pm"}),
    )
    .unwrap();
    let show = d.rpc("agent_show", json!({"alias": "obs"})).unwrap();
    assert!(show["inbox"]["readers"].get("pm").is_none(), "{show}");
    let resume = d
        .rpc(
            "agent_inbox",
            json!({"alias": "obs", "peek": true, "reader": "pm"}),
        )
        .unwrap();
    let left = resume["messages"].as_array().unwrap();
    assert_eq!(left.len(), 1, "{resume}");
    assert_eq!(left[0]["id"], "n3");
}

/// Concurrent readers acking the same watermark never double-complete:
/// the `state='queued'` guard makes each row's completion exactly once.
#[test]
fn inbox_ack_is_idempotent_for_concurrent_readers() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    for id in ["a", "b", "c"] {
        d.send("obs", json!({"text": id, "message": id})).unwrap();
    }
    let page = d
        .rpc("agent_inbox", json!({"alias": "obs", "peek": true}))
        .unwrap();
    let through = page["messages"].as_array().unwrap().last().unwrap()["seq"]
        .as_i64()
        .unwrap();
    let mut hs = Vec::new();
    for i in 0..4 {
        let state = d.state.clone();
        hs.push(thread::spawn(move || {
            client::rpc(
                &state,
                "agent_inbox_ack",
                json!({"alias": "obs", "through": through,
                       "reader": format!("r{i}")}),
            )
        }));
    }
    let mut total_acked = 0usize;
    for h in hs {
        let r = h.join().unwrap().unwrap();
        total_acked += r["acked"].as_array().unwrap().len();
    }
    assert_eq!(total_acked, 3, "the three rows completed exactly once");
    // Re-acking the watermark is a no-op.
    let r = d
        .rpc(
            "agent_inbox_ack",
            json!({"alias": "obs", "through": through, "reader": "late"}),
        )
        .unwrap();
    assert!(r["acked"].as_array().unwrap().is_empty(), "{r}");
    assert_eq!(r["unread"], 0, "{r}");
    // Every reader's watermark is durable.
    let readers = d.rpc("agent_show", json!({"alias": "obs"})).unwrap()["inbox"]["readers"].clone();
    for i in 0..4 {
        assert_eq!(readers[format!("r{i}")]["through"], through, "{readers}");
    }
    assert_eq!(readers["late"]["through"], through, "{readers}");
}

/// The one guard on the mutation: an agent caller may ack only its own
/// alias. A mailbox's consumer is otherwise unattributed (CAD-251), so
/// the read stays unguarded and the ack refuses a foreign agent.
#[test]
fn inbox_ack_refuses_another_agents_inbox() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    d.send("obs", json!({"text": "x", "message": "n1"}))
        .unwrap();
    // The test process becomes agent 'w1' — every d.rpc from here on
    // is that caller. The plant flips w1's row to a pty pane.
    plant_pane(&d, "w1", std::process::id());
    let r = d.rpc("agent_inbox_ack", json!({"alias": "obs", "through": 1}));
    let e = r.unwrap_err().to_string();
    assert!(e.contains("cannot ack another"), "{e}");
    // Nothing was consumed by the refused ack.
    assert_eq!(
        d.rpc("agent_inbox", json!({"alias": "obs", "peek": true}))
            .unwrap()["unread"],
        1
    );
}

/// The legacy drain consumes too, so it carries the same guard as the
/// ack: a proven agent may not drain another agent's inbox — the peek
/// stays a plain read it may do.
#[test]
fn inbox_drain_refuses_a_foreign_agent() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    d.send("obs", json!({"text": "x", "message": "n1"}))
        .unwrap();
    // The test process becomes agent 'w1' — every d.rpc from here on
    // is that caller.
    plant_pane(&d, "w1", std::process::id());
    // Peeking is a read — the foreign agent may look.
    assert_eq!(
        d.rpc("agent_inbox", json!({"alias": "obs", "peek": true}))
            .unwrap()["unread"],
        1
    );
    // Draining consumes — refused, and nothing completes.
    let e = d
        .rpc("agent_inbox", json!({"alias": "obs"}))
        .unwrap_err()
        .to_string();
    assert!(e.contains("cannot drain another"), "{e}");
    let show = d.rpc("agent_show", json!({"alias": "obs"})).unwrap();
    assert_eq!(show["queued"], 1, "{show}");
    assert_eq!(show["messages"][0]["state"], "queued", "{show}");
}

/// Reader names are identifiers — the same grammar as aliases — so a
/// cursor key can never smuggle path or query syntax into the log.
#[test]
fn inbox_rejects_bad_reader_names() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    for bad in ["../x", "a b", "x?y=1", "", &"r".repeat(81)] {
        for req in [
            json!({"alias": "obs", "peek": true, "reader": bad}),
            json!({"alias": "obs", "through": 1, "reader": bad}),
        ] {
            let verb = if req.get("peek").is_some() {
                "agent_inbox"
            } else {
                "agent_inbox_ack"
            };
            let e = d.rpc(verb, req).unwrap_err().to_string();
            assert!(e.contains("invalid reader name"), "{verb} {bad:?}: {e}");
        }
    }
    // A normal name still works.
    d.rpc(
        "agent_inbox",
        json!({"alias": "obs", "peek": true, "reader": "pm-1.x"}),
    )
    .unwrap();
}

/// `cadence inbox ack` is the consume verb: an inbox literally named
/// 'ack' would be unreachable through the CLI, so registering one is
/// refused.
#[test]
fn inbox_register_refuses_the_reserved_ack_alias() {
    let d = TestDaemon::start();
    let e = d
        .fixture_rpc(
            "agent_register",
            json!({"alias": "ack", "provider": "inbox",
                   "endpoint_kind": "inbox",
                   "cwd": d.dir.path().to_str().unwrap()}),
        )
        .unwrap_err()
        .to_string();
    assert!(e.contains("reserved"), "{e}");
}

/// Caller identity is connection-bound: a self-asserted `by` field on
/// the ack verb is refused, like every other mutation verb.
#[test]
fn inbox_ack_rejects_identity_fields() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    let e = d
        .rpc(
            "agent_inbox_ack",
            json!({"alias": "obs", "through": 1, "by": "operator"}),
        )
        .unwrap_err()
        .to_string();
    assert!(e.contains("connection-bound"), "{e}");
    let e = d
        .rpc(
            "agent_inbox",
            json!({"alias": "obs", "peek": true, "caller": "w1"}),
        )
        .unwrap_err()
        .to_string();
    assert!(e.contains("connection-bound"), "{e}");
}

/// The CLI path: `inbox --peek` prints without consuming, `inbox ack`
/// completes the watermark, and `cadence self` reports the unread
/// backlog and its age.
#[test]
fn cli_inbox_peek_ack_and_self_reports_unread() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    for (id, text) in [("n1", "note one"), ("n2", "note two")] {
        d.send("obs", json!({"text": text, "message": id})).unwrap();
    }
    let bin = env!("CARGO_BIN_EXE_cadence");
    let cadence = |args: &[&str]| {
        let out = std::process::Command::new(bin)
            .arg("--state-dir")
            .arg(&d.state)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    // `self` shows the unread backlog and the oldest unread's age.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .arg("self")
        .env("CADENCE_ALIAS", "obs")
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["unread"], 2, "{v}");
    assert!(v["oldest_unread_age_secs"].as_f64().unwrap() >= 0.0, "{v}");

    // Peek prints both messages; the queue is untouched.
    let peek = cadence(&["inbox", "obs", "--peek"]);
    let msgs: Vec<Value> = peek
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(msgs.len(), 2, "{peek}");
    let seq = msgs[0]["seq"].as_i64().unwrap();
    assert_eq!(
        d.rpc("agent_show", json!({"alias": "obs"})).unwrap()["queued"],
        2
    );

    // Ack the first — the reader cursor advances; a restarted peek
    // shows only the rest.
    let ack: Value =
        serde_json::from_str(&cadence(&["inbox", "ack", "obs", &seq.to_string()])).unwrap();
    assert_eq!(ack["acked"], json!([seq]), "{ack}");
    let peek2 = cadence(&["inbox", "obs", "--peek"]);
    let left: Vec<Value> = peek2
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(left.len(), 1, "{peek2}");
    assert_eq!(left[0]["id"], "n2");

    // `inbox ack` with no seq is a clean refusal, not a silent ack.
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["inbox", "ack", "obs"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("at least one seq")
            || String::from_utf8_lossy(&out.stderr).contains("at least one seq")
    );
}

/// Poll a file's line count — the exec'd consumer's evidence file —
/// with a bounded deadline.
fn wait_lines(path: &Path, want: usize, secs: u64) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let lines: Vec<String> = std::fs::read_to_string(path)
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default();
        if lines.len() >= want || Instant::now() >= deadline {
            return lines;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// `--follow --exec` pushes each message's JSON to the command's stdin
/// and acks it on exit 0 — exactly once per message.
#[test]
fn inbox_follow_exec_acks_each_message_once() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    let bin = env!("CARGO_BIN_EXE_cadence");
    // The consumer: read one message JSON from stdin, record its id.
    let script = d.dir.path().join("collect.py");
    let log = d.dir.path().join("seen.log");
    std::fs::write(
        &script,
        "import json,sys\nm=json.load(sys.stdin)\nopen(sys.argv[1],'a').write(m['id']+'\\n')\n",
    )
    .unwrap();
    let mut child = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "inbox",
            "obs",
            "--follow",
            "--exec-retry-ms",
            "50",
            "--exec",
            "python3",
        ])
        .arg(&script)
        .arg(&log)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    for id in ["m1", "m2"] {
        d.send("obs", json!({"text": id, "message": id})).unwrap();
    }
    let seen = wait_lines(&log, 2, 15);
    assert_eq!(seen, vec!["m1", "m2"], "each delivered exactly once");
    // The last exec's line lands when it exits; the follower sends
    // `agent_inbox_ack` through that seq right after — the kill has to
    // wait for the watermark or the ack can die with it.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let show = d.rpc("agent_show", json!({"alias": "obs"})).unwrap();
        if show["queued"] == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "m2's ack never landed: {show}");
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    // Both acked — nothing queued, both completed via=inbox_ack.
    let show = d.rpc("agent_show", json!({"alias": "obs"})).unwrap();
    assert_eq!(show["queued"], 0, "{show}");
    for m in show["messages"].as_array().unwrap() {
        assert_eq!(m["state"], "completed", "{m}");
        assert_eq!(m["result"]["via"], "inbox_ack", "{m}");
    }
}

/// A non-zero exec exit leaves the message queued and retries it with
/// backoff — the ack lands only after a successful run.
#[test]
fn inbox_follow_exec_failure_retries_until_success() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    let bin = env!("CARGO_BIN_EXE_cadence");
    let script = d.dir.path().join("flaky.py");
    let log = d.dir.path().join("tries.log");
    let flag = d.dir.path().join("ok");
    // Record every attempt; fail until the flag file exists.
    std::fs::write(
        &script,
        "import json,os,sys\n\
         m=json.load(sys.stdin)\n\
         open(sys.argv[1],'a').write(m['id']+'\\n')\n\
         sys.exit(0 if os.path.exists(sys.argv[2]) else 1)\n",
    )
    .unwrap();
    let mut child = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "inbox",
            "obs",
            "--follow",
            "--exec-retry-ms",
            "50",
            "--exec",
            "python3",
        ])
        .arg(&script)
        .arg(&log)
        .arg(&flag)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    d.send("obs", json!({"text": "t", "message": "m1"}))
        .unwrap();
    // First attempts fail — the message is NOT acked. A retry may append
    // between polls, so assert the delivered identity, not a count.
    let tries = wait_lines(&log, 1, 15);
    assert!(tries.iter().all(|t| t == "m1"), "{tries:?}");
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        d.rpc("agent_show", json!({"alias": "obs"})).unwrap()["queued"],
        1,
        "failed exec must leave the message queued"
    );
    // Once the consumer succeeds the message is delivered and acked.
    std::fs::write(&flag, "").unwrap();
    let tries = wait_lines(&log, 2, 15);
    let deadline = Instant::now() + Duration::from_secs(15);
    while d.rpc("agent_show", json!({"alias": "obs"})).unwrap()["queued"] != 0 {
        assert!(Instant::now() < deadline, "message never acked");
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(tries.len() >= 2, "retried after failure: {tries:?}");
    let show = d.rpc("agent_show", json!({"alias": "obs"})).unwrap();
    assert_eq!(show["messages"][0]["result"]["via"], "inbox_ack");
}

/// A consumer that hangs is killed at the exec timeout and counted as
/// a failure; after the failure budget the message is parked — still
/// queued and unread, but skipped so the follower reaches the next one.
#[test]
fn inbox_follow_exec_timeout_kills_then_parks() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    let bin = env!("CARGO_BIN_EXE_cadence");
    let script = d.dir.path().join("hang.py");
    let log = d.dir.path().join("done.log");
    // 'm1' hangs forever; anything else succeeds.
    std::fs::write(
        &script,
        "import json,sys,time\n\
         m=json.load(sys.stdin)\n\
         if m['id']=='m1': time.sleep(600)\n\
         open(sys.argv[1],'a').write(m['id']+'\\n')\n",
    )
    .unwrap();
    let mut child = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "inbox",
            "obs",
            "--follow",
            "--exec-retry-ms",
            "50",
            "--exec-timeout-ms",
            "300",
            "--exec-max-failures",
            "2",
            "--exec",
            "python3",
        ])
        .arg(&script)
        .arg(&log)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    for id in ["m1", "m2"] {
        d.send("obs", json!({"text": id, "message": id})).unwrap();
    }
    // m1 times out twice (≈600ms of killed sleeps) and parks; m2 —
    // queued behind it — is still delivered and acked.
    let done = wait_lines(&log, 1, 20);
    assert_eq!(done, vec!["m2"], "{done:?}");
    // m2's line lands when its exec exits; the follower sends
    // `agent_inbox_ack` through that seq right after — the kill has to
    // wait for the watermark or the ack can die with it.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let show = d.rpc("agent_show", json!({"alias": "obs"})).unwrap();
        let m2 = show["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == "m2")
            .unwrap()
            .clone();
        if m2["state"] == "completed" {
            break;
        }
        assert!(Instant::now() < deadline, "m2 never acked: {show}");
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    // The poison message stays queued — parked, never lost.
    let show = d.rpc("agent_show", json!({"alias": "obs"})).unwrap();
    assert_eq!(show["queued"], 1, "{show}");
    let events = d.events("obs");
    let parks: Vec<_> = events
        .iter()
        .filter(|e| e["kind"] == "inbox_park")
        .collect();
    assert_eq!(parks.len(), 1, "{events:?}");
    assert_eq!(parks[0]["payload"]["message"], "m1");
    let fails: Vec<_> = events
        .iter()
        .filter(|e| e["kind"] == "inbox_exec_fail")
        .collect();
    assert_eq!(fails.len(), 2, "{events:?}");
    assert!(
        fails[0]["payload"]["fail"]["error"]
            .as_str()
            .unwrap()
            .contains("timeout"),
        "{fails:?}"
    );
    // Parked for the reader, not deleted: a different reader still sees
    // m1 queued ahead of nothing.
    let other = d
        .rpc(
            "agent_inbox",
            json!({"alias": "obs", "peek": true, "reader": "pm-other"}),
        )
        .unwrap();
    assert_eq!(other["messages"][0]["id"], "m1", "{other}");
}

/// A message that fails every exec is parked after the failure budget —
/// it stays queued and unread but stops blocking the queue.
#[test]
fn inbox_follow_exec_parks_a_poison_message() {
    let d = TestDaemon::start();
    d.register_inbox("obs");
    let bin = env!("CARGO_BIN_EXE_cadence");
    let script = d.dir.path().join("poison.py");
    let log = d.dir.path().join("ok.log");
    std::fs::write(
        &script,
        "import json,sys\n\
         m=json.load(sys.stdin)\n\
         if m['id']=='bad': sys.exit(1)\n\
         open(sys.argv[1],'a').write(m['id']+'\\n')\n",
    )
    .unwrap();
    let mut child = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "inbox",
            "obs",
            "--follow",
            "--exec-retry-ms",
            "30",
            "--exec-max-failures",
            "3",
            "--exec",
            "python3",
        ])
        .arg(&script)
        .arg(&log)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    for id in ["bad", "good"] {
        d.send("obs", json!({"text": id, "message": id})).unwrap();
    }
    let done = wait_lines(&log, 1, 20);
    assert_eq!(done, vec!["good"], "{done:?}");
    // "good"'s line lands when its exec exits; the follower sends
    // `agent_inbox_ack` through that seq right after. The kill has to
    // wait for the watermark — killing first can drop the ack and
    // leave the consumed message queued.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let show = d.rpc("agent_show", json!({"alias": "obs"})).unwrap();
        if show["queued"] == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "good's ack never landed: {show}");
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    let show = d.rpc("agent_show", json!({"alias": "obs"})).unwrap();
    assert_eq!(show["queued"], 1, "{show}");
    let parks: Vec<_> = d
        .events("obs")
        .into_iter()
        .filter(|e| e["kind"] == "inbox_park")
        .collect();
    assert_eq!(parks.len(), 1);
    assert_eq!(parks[0]["payload"]["message"], "bad");
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
        .operator_rpc(
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

#[test]
fn restart_fences_task_kickoff_and_job_show_reports_drift() {
    // Seed a state dir with a dispatched task whose kickoff is in
    // flight, then start the daemon — recovery fences the message, the
    // task is untouched, `job show` flags the drift and dispatch is
    // legal again.
    let upstream = json!({"upstream": "pm"}).to_string();
    let (_seeded, state) = seeded_state(
        &[
            ("pm", None, "fake", "pm"),
            ("w1", Some(upstream.as_str()), "fake", "worker"),
        ],
        |store, _cwd| {
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
            let (task, kickoff, dup, _dead) =
                store.dispatch_task("j1-t2", None, None, "test").unwrap();
            assert!(!dup);
            assert_eq!(task.state, "dispatched");
            // Simulate a mid-turn crash: kickoff taken + running, store dropped.
            match store.take_queued("w1").unwrap() {
                Take::Message(m) => assert_eq!(m.id, kickoff),
                _ => panic!("expected kickoff"),
            }
            store.mark_running(&kickoff, "fake-1-abc").unwrap();
            assert_eq!(store.task("j1-t2").unwrap().state, "running");
        },
    );
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

// ---- message cancel (CAD-25) ----

#[test]
fn message_cancel_queued_lifecycle() {
    let d = TestDaemon::start();
    d.register("w1");
    d.register_inbox("pm");
    d.wait_agent("w1", "idle", 10);

    // A completed message refuses, naming its terminal state.
    d.send("w1", json!({"text": "done work", "message": "m-done"}))
        .unwrap();
    d.wait_message("w1", "m-done", &["completed"], 15);
    let err = d
        .operator_rpc("message_cancel", json!({"message": "m-done"}))
        .unwrap_err();
    assert!(err.to_string().contains("'completed'"), "{err}");

    // Stop the worker so the next send parks queued.
    d.operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
    d.wait_agent("w1", "stopped", 10);
    d.send(
        "w1",
        json!({"text": "queued work", "message": "m-q", "reply_to": "pm"}),
    )
    .unwrap();
    assert_eq!(d.message_state("w1", "m-q"), "queued");

    // Cancel: state, result payload, event, and exactly one notice on pm.
    let out = d
        .operator_rpc(
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
        .operator_rpc("message_cancel", json!({"message": "m-q"}))
        .unwrap_err();
    assert!(err.to_string().contains("'cancelled'"), "{err}");
    let err = d
        .operator_rpc("message_cancel", json!({"message": "m-nope"}))
        .unwrap_err();
    assert!(err.to_string().contains("No such message"), "{err}");

    // Resume: the cancelled message never delivers; a fresh one does.
    d.operator_rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap();
    d.wait_agent("w1", "idle", 10);
    d.send("w1", json!({"text": "real work", "message": "m-new"}))
        .unwrap();
    d.wait_message("w1", "m-new", &["completed"], 15);
    assert_eq!(d.message_state("w1", "m-q"), "cancelled");
}

/// The gate-side of `message_cancel` needs a profile that still
/// requires `agent ready` — Devin's verified idle probe is itself the
/// claim (CAD-520), so this runs on the stub.
#[test]
fn message_cancel_gate_pty_and_running_refusal() {
    let d = TestDaemon::start();
    let mock = d.mock_stub();
    d.register_stub("st1", json!({}));
    d.wait_agent("st1", "idle", 20);

    // Queued behind the ready gate: the message is durable but the pane
    // has not been claimed.
    d.send("st1", json!({"text": "first", "message": "m1"}))
        .unwrap();
    // Wait for m1's own recorded refusal, not a fixed sleep: the actor
    // takes the send at once and the gate may still be probing, and a
    // cancel is refused unless the message is back to `queued` (CAD-293).
    d.wait_event_where("st1", "gate_wait", |e| e["payload"]["message"] == "m1", 20);
    assert_eq!(d.message_state("st1", "m1"), "queued");
    d.operator_rpc("message_cancel", json!({"message": "m1", "reason": "typo"}))
        .unwrap();
    assert_eq!(d.message_state("st1", "m1"), "cancelled");

    // A ready claim now must NOT paste m1 — the gate only releases a
    // queued message.
    d.operator_rpc("agent_ready", json!({"alias": "st1"}))
        .unwrap();

    // The claim stays outstanding, so the next queued message delivers
    // normally.
    d.send("st1", json!({"text": "second", "message": "m2"}))
        .unwrap();
    let token = pty_token(&d, "st1", "m2");
    // m2 consumed the one claim, so m1 never did: no trace of it on the
    // screen or in the input line (a pasted m1 would sit in either).
    let pane = std::fs::read_to_string(d.stub_pane_file(&mock, "st1", "screen"))
        .unwrap_or_default()
        + &std::fs::read_to_string(d.stub_pane_file(&mock, "st1", "input")).unwrap_or_default();
    assert!(!pane.contains("first"), "{pane}");

    // A running turn refuses — interruption happens at the provider.
    let err = d
        .operator_rpc("message_cancel", json!({"message": "m2"}))
        .unwrap_err();
    assert!(
        err.to_string().contains("'running'") || err.to_string().contains("'submitting'"),
        "{err}"
    );
    d.report("m2", &token, "result", "done").unwrap();
    d.wait_message("st1", "m2", &["completed"], 15);
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
    d.send(
        "w1",
        json!({"text": "followup", "message": "m-t", "task": "j1-t1"}),
    )
    .unwrap();
    let err = d
        .operator_rpc("message_cancel", json!({"message": "m-t"}))
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

/// CAD-468: a caller can nudge (that lane is theirs) but can never mint
/// the daemon's reminder — `sys-` ids are refused to every caller path,
/// and a forged `wake` source is refused outright.
#[test]
fn daemon_reminder_id_and_source_cannot_be_forged() {
    let d = TestDaemon::start();
    let _mock = d.mock_stub();
    d.register_stub("w1", json!({"auto_ready": "verified"}));
    d.wait_agent("w1", "idle", 20);
    let forged_id = cadence_agent::proto::daemon_message_id("nudge", "x");
    assert!(forged_id.starts_with("sys-nudge-"));
    for params in [
        // The exact id the daemon would mint, caller-supplied.
        json!({"alias": "w1", "text": "fake reminder", "message": forged_id,
               "source": "nudge"}),
        json!({"alias": "w1", "text": "fake reminder", "message": forged_id,
               "nudge": true}),
        // The daemon's wake source on an ordinary id.
        json!({"alias": "w1", "text": "fake wake", "source": "wake"}),
    ] {
        let err = d
            .operator_rpc("agent_send", params.clone())
            .unwrap_err()
            .to_string();
        assert!(err.contains("daemon"), "{params}: {err}");
        let err = d
            .operator_rpc("agent_send", params.clone())
            .unwrap_err()
            .to_string();
        assert!(err.contains("daemon"), "operator {params}: {err}");
    }
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert!(
        show["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["body"] != "fake reminder" && m["body"] != "fake wake"),
        "{show}"
    );
}

/// CAD-467 wrote `send --issue/--worktree` behind the steer gate — but
/// the gate checks caller-versus-target, never the values, so a PM
/// could mark a send to its own worker with a forged lane and
/// `dispatch_record` accepted it (CAD-378 R6). Now the fields are
/// refused on every send: a peer worker, a foreign PM, the target's
/// own PM, a self-send and a detached unprovable caller alike. Only
/// `dispatch_send` (daemon-resolved lane) and `task_dispatch` (job
/// rows) write the tags; a plain send is unaffected.
#[test]
fn send_lane_provenance_is_refused_for_every_caller() {
    let d = TestDaemon::start();
    let _mock = d.mock_stub();
    let mut p = guard_panes(&d);
    let forged = |extra: &[(&str, &str)]| -> Value {
        let mut v = json!({"alias": "w1", "text": "kickoff-shaped ask"});
        for (k, val) in extra {
            v[*k] = json!(val);
        }
        v
    };
    let both = forged(&[("issue", "D-9"), ("worktree", "/lane/d-9")]);
    let one = forged(&[("issue", "D-9")]);

    // Every caller is refused — the values are never read, let alone
    // authorized.
    let r = p.pm2.rpc(&d.state, "agent_send", both.clone());
    assert!(frame_err(&r).contains("only a dispatch sets"), "{r}");
    let r = p.w1.rpc(&d.state, "agent_send", both.clone());
    assert!(frame_err(&r).contains("only a dispatch sets"), "{r}");
    // The target's own PM — admitted by the old steer gate — is refused
    // like everyone else; so is a single field.
    let r = p.pm.rpc(&d.state, "agent_send", both.clone());
    assert!(frame_err(&r).contains("only a dispatch sets"), "{r}");
    let r = p.pm.rpc(&d.state, "agent_send", one.clone());
    assert!(frame_err(&r).contains("only a dispatch sets"), "{r}");
    // The operator is refused too — no caller field reaches the columns.
    let err = d
        .operator_rpc("agent_send", both.clone())
        .unwrap_err()
        .to_string();
    assert!(err.contains("only a dispatch sets"), "{err}");
    // A detached caller that proves nothing — refused outright.
    let r = unprovable_rpc(&d, "agent_send", both.clone());
    assert!(frame_err(&r).contains("only a dispatch sets"), "{r}");
    // Nothing was queued by any refused caller.
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert!(
        show["messages"].as_array().unwrap().is_empty(),
        "refused provenance sends must not enqueue: {show}"
    );

    // A bare send from the same peer is unaffected — the refusal fires
    // only on the provenance fields.
    let r = p.pm2.rpc(&d.state, "agent_send", forged(&[]));
    assert_eq!(r["ok"], true, "{r}");
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
        d.send("w1", json!({"text": format!("task {i}"), "message": id}))
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
    d.send("w1", json!({"text": "epilogue", "message": "ep"}))
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
        d.send("w1", json!({"text": format!("task {i}"), "message": id}))
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
    d.send("w1", json!({"text": "post-follow", "message": "mf"}))
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

/// Sends into an undrained mailbox past its unread threshold carry a
/// `warning` (receipt field + CLI stderr) and still queue; a recently
/// drained mailbox never warns. Routed results have no caller to warn,
/// so the mailbox's event stream records one `inbox_unconsumed` per
/// idle window. Status and overview name the stale inbox with its
/// count, oldest age and owner.
#[test]
fn inbox_without_consumer_warns_on_send_and_route() {
    stall_sample(1); // the inbox sweep rides the screen-sample cadence
    let d = TestDaemon::start();
    let home = TempDir::new().unwrap();
    const WINDOW: u64 = 6;
    let limits = |extra: Value| {
        let mut p = json!({"inbox_warn_unread": 2, "inbox_warn_idle_secs": WINDOW});
        p.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        p
    };
    register_inbox_with(&d, "stale", limits(json!({})));
    register_inbox_with(&d, "drained", limits(json!({})));
    // `routed` belongs to group root `boss`, so `boss` owns it.
    d.register_inbox("boss");
    register_inbox_with(&d, "routed", limits(json!({"upstream": "boss"})));
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.fixture_rpc(
        "agent_register",
        json!({"alias": "w1", "provider": "fake", "endpoint_kind": "fake",
               "cwd": cwd, "params": json!({"upstream": "routed"}).to_string()}),
    )
    .unwrap();
    d.wait_agent("w1", "idle", 10);
    let send = |alias: &str, id: &str| {
        d.send(alias, json!({"text": "note", "message": id}))
            .unwrap()
    };
    let job = |id: &str| {
        send("w1", id);
        d.wait_message("w1", id, &["completed"], 15);
    };

    // A young backlog is not stale: over the threshold, inside the window.
    for i in 0..3 {
        for alias in ["stale", "drained"] {
            let r = send(alias, &format!("{alias}-a{i}"));
            assert!(r["warning"].is_null(), "{r}");
        }
        job(&format!("j-a{i}"));
    }
    // CAD-184 kept sleep: timing is the behaviour (staleness window).
    thread::sleep(Duration::from_millis(WINDOW * 1000 + 300));

    // Routed results into the now-stale `routed` mailbox have no caller
    // to warn: its stream records one event, owned by the group root —
    // and another routed result inside the window adds none.
    let e = d.wait_event("routed", "inbox_unconsumed", 15);
    assert_eq!(e["payload"]["stale"], true, "{e}");
    assert_eq!(e["payload"]["owner"], "boss", "{e}");
    assert!(e["payload"]["unread"].as_u64().unwrap_or(0) >= 3, "{e}");
    job("j-b0");
    // CAD-184 kept sleep: absence window — no second event inside the
    // window, across sweeps.
    thread::sleep(Duration::from_millis(1500));
    let kinds = event_kinds(&d, "routed");
    assert_eq!(
        kinds.iter().filter(|k| *k == "inbox_unconsumed").count(),
        1,
        "{kinds:?}"
    );

    // `drained` is read; nothing warns for it afterwards.
    let drained_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let page = d.rpc("agent_inbox", json!({"alias": "drained"})).unwrap();
    assert_eq!(page["messages"].as_array().unwrap().len(), 3, "{page}");
    for i in 0..3 {
        let r = send("drained", &format!("drained-b{i}"));
        assert!(r["warning"].is_null(), "recently drained: {r}");
    }
    // `stale` was never read: the send warns and still queues.
    let r = send("stale", "stale-b0");
    assert_eq!(r["state"], "queued", "warned, never refused: {r}");
    let w = r["warning"].as_str().expect("stale inbox warns");
    assert!(
        w.contains("'stale'") && w.contains("4 unread") && w.contains("owner operator"),
        "{w}"
    );
    // The CLI says it on stderr; stdout stays the JSON receipt.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["send", "stale", "--text", "cli note"])
        .env("HOME", home.path())
        .env_remove("CADENCE_ALIAS")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("warning: inbox 'stale' has no consumer"),
        "{err}"
    );
    let receipt: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(receipt["warning"].is_string(), "{receipt}");
    // CAD-184 kept sleep: absence window — a drained inbox never warns,
    // across sweeps.
    thread::sleep(Duration::from_millis(2500));
    let late: Vec<Value> = d
        .events("drained")
        .into_iter()
        .filter(|e| e["kind"] == "inbox_unconsumed")
        .filter(|e| e["at"].as_f64().unwrap_or(0.0) >= drained_at)
        .collect();
    assert!(
        late.is_empty(),
        "a recently drained inbox never warns: {late:?}"
    );

    // Status footer and overview name the stale inboxes.
    let status = status_json(&d.state, &[], &[("HOME", home.path())]);
    let stale: Vec<&Value> = status["footer"]["stale_inboxes"]
        .as_array()
        .unwrap()
        .iter()
        .collect();
    let names: Vec<&str> = stale.iter().filter_map(|s| s["alias"].as_str()).collect();
    assert!(
        names.contains(&"stale") && names.contains(&"routed"),
        "{status}"
    );
    assert!(!names.contains(&"drained"), "{status}");
    let routed = stale.iter().find(|s| s["alias"] == "routed").unwrap();
    assert_eq!(routed["owner"], "boss", "{status}");
    assert!(
        routed["oldest_unread_age_secs"].as_u64().unwrap_or(0) >= WINDOW,
        "{status}"
    );
    let table = status_table(&d.state, &[("HOME", home.path())]);
    assert!(table.contains("stale inboxes (no consumer): "), "{table}");

    let view = overview_at(home.path(), &d.state, None, &[]);
    let row = view["needs_me"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["subject"]["id"] == "routed")
        .cloned()
        .expect("stale inbox row");
    assert_eq!(row["kind"], "inbox_stale", "{row}");
    assert_eq!(row["owner"], "boss", "{row}");
    let causes: Vec<&str> = row["causes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["cause"].as_str())
        .collect();
    assert_eq!(causes, ["inbox_stale", "inbox_unread"], "{row}");
    stall_sample(0);
}

/// CAD-291: the suite may run from an agent pane, so `CADENCE_ALIAS`
/// can sit on the runner's own ancestry. Re-run the probe below in a
/// child whose environment carries an agent's alias: there,
/// `operator_rpc` must still be accepted as the operator while a plain
/// call from the same (agent-descended) runner is refused.
#[test]
fn operator_rpc_is_the_operator_even_from_an_agent_runner() {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "operator_rpc_from_an_agent_runner_probe",
            "--ignored",
        ])
        .env("CADENCE_ALIAS", "cad291-runner")
        .env("CAD291_PROBE", "1")
        // A plain libtest child: the outer run's suite lock, nextest
        // markers and any seam assertion the runner's env might carry
        // are not its to honour — the probe's ambient call must stay
        // ambient (CAD-482/F14: the same answer in a pane and in CI).
        .env_remove("CADENCE_SUITE_LOCK")
        .env_remove("CADENCE_REVIEW_SUITE_LOCK_HELD")
        .env_remove(cadence_agent::test_seam::AS_ENV)
        .env_remove(cadence_agent::test_seam::ARM_ENV);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("NEXTEST") {
            child.env_remove(key);
        }
    }
    let out = child.output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("1 passed"),
        "the probe must actually run: {text}"
    );
}

#[test]
#[ignore = "run by operator_rpc_is_the_operator_even_from_an_agent_runner as an agent-shaped child"]
fn operator_rpc_from_an_agent_runner_probe() {
    if std::env::var("CAD291_PROBE").as_deref() != Ok("1") {
        return;
    }
    assert!(std::env::var("CADENCE_ALIAS").is_ok());
    let d = TestDaemon::start();
    let params = json!({"id": "ap-291", "source": "operator in chat",
                        "head": "abcdefabcdefabcdefabcdefabcdefabcdefabcd",
                        "repo": "x/y", "pr": 291});
    // This process carries an agent's environment: refused as such. A
    // plain ambient call, never an assertion — on a seam-armed fixture
    // `d.rpc` asserts nothing, so the real env-mark refusal is what
    // answers, identical in a pane and in CI.
    let err = d.rpc("approval_record", params.clone()).unwrap_err();
    assert!(err.to_string().contains("carries CADENCE_ALIAS"), "{err}");
    // The harness's operator caller from the same runner is the operator.
    let r = d.operator_rpc("approval_record", params).unwrap();
    assert_eq!(r["state"], "recorded", "{r}");
    assert_eq!(r["recorded_via"], "operator-connection", "{r}");
}

/// CAD-149 review F2, the reproduced sequence: the operator removes a
/// PM whose members survive, and the PM's alias is registered again.
/// The worker's attempt is refused outright (F3); and even a PM alias
/// the operator re-registers gains no authority over the members the
/// old PM left behind — PM authority is bound to a registration older
/// than the member, not to the name.
#[test]
fn reregistered_pm_alias_inherits_no_authority() {
    let d = TestDaemon::start();
    let mut p = guard_panes(&d);
    d.register_member("w2", "pm");
    d.wait_agent("w2", "idle", 10);
    let stall = |secs: &str| json!({"alias": "w2", "patch": {"stall_secs": secs}});
    // The real PM governs its member.
    let r = p.pm.rpc(&d.state, "agent_set", stall("600"));
    assert_eq!(r["ok"], true, "{r}");

    // The PM stops (its planted row is otherwise "live"), and the
    // operator removes it while its members survive.
    rusqlite::Connection::open(d.state.join("cadence.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE agents SET endpoint=NULL, state='stopped' WHERE alias='pm'",
            [],
        )
        .unwrap();
    d.operator_rpc("agent_remove", json!({"alias": "pm"}))
        .unwrap();
    // The worker tries to take the PM's alias from its own pane.
    let r = p.w1.rpc(
        &d.state,
        "agent_register",
        json!({"alias": "pm", "provider": "inbox", "endpoint_kind": "inbox",
               "cwd": d.dir.path().to_str().unwrap()}),
    );
    assert!(
        frame_err(&r).contains("agent 'w1' may register only its own members"),
        "{r}"
    );
    assert!(d.rpc("agent_show", json!({"alias": "pm"})).is_err());
    // Re-registered (here by the operator) behind a new pane: no
    // authority over the orphaned member.
    let home = TempDir::new().unwrap();
    let mut pm_new = LaneShell::spawn(home.path());
    plant_member_pane(&d, "pm", "inbox", None, pm_new.pid());
    for (method, params) in [
        ("agent_set", stall("0")),
        ("agent_remove", json!({"alias": "w2", "force": true})),
    ] {
        let r = pm_new.rpc(&d.state, method, params);
        let e = frame_err(&r);
        assert!(
            e.contains("agent 'pm' cannot change another agent")
                && e.contains("the operator (it has no PM)"),
            "{r}"
        );
    }
    let r = pm_new.rpc(&d.state, "agent_gc", json!({}));
    assert_eq!(r["result"]["removed"], json!([]), "{r}");
    assert_eq!(
        d.rpc("agent_show", json!({"alias": "w2"})).unwrap()["agent"]["params"]["stall_secs"],
        "600"
    );
    // A member joined AFTER the new registration is the new PM's.
    let r = pm_new.rpc(
        &d.state,
        "agent_register",
        json!({"alias": "w3", "provider": "fake", "endpoint_kind": "fake",
               "cwd": d.dir.path().to_str().unwrap(),
               "params": json!({"upstream": "pm"}).to_string()}),
    );
    assert_eq!(r["ok"], true, "{r}");
    d.wait_agent("w3", "idle", 10);
    let r = pm_new.rpc(
        &d.state,
        "agent_set",
        json!({"alias": "w3", "patch": {"stall_secs": "600"}}),
    );
    assert_eq!(r["ok"], true, "{r}");
}

/// CAD-149 review F3: registration is a mutation of the new agent by
/// its caller. A worker's pane may register nothing — not an agent it
/// would own with trust-bearing params, not a member of its PM's group,
/// not a root; another group's PM may not register into this group;
/// a PM registers its own members (raw RPC and `cadence join` from its
/// pane), and the operator shell joins as before.
#[test]
fn agent_register_caller_rule() {
    let d = TestDaemon::start();
    let _mock = d.mock_codex("ok");
    let mut p = guard_panes(&d);
    let cwd = d.dir.path().to_str().unwrap().to_string();
    let codex = |alias: &str, params: Value| {
        json!({"alias": alias, "provider": "codex", "endpoint_kind": "managed",
               "cwd": cwd, "params": params.to_string()})
    };
    // The reproduced case: a worker mints an agent it would be PM of,
    // carrying a key it may not set on itself.
    let r = p.w1.rpc(
        &d.state,
        "agent_register",
        codex("wx", json!({"upstream": "w1", "approval_policy": "never"})),
    );
    let e = frame_err(&r);
    assert!(
        e.contains("agent 'w1' may register only its own members")
            && e.contains("'w1' is a worker in 'pm''s group"),
        "{r}"
    );
    for params in [json!({"upstream": "pm"}), json!({})] {
        let r = p.w1.rpc(&d.state, "agent_register", codex("wx", params));
        assert!(
            frame_err(&r).contains("may register only its own members"),
            "{r}"
        );
    }
    // Through the CLI too.
    let (rc, out) = p.w1.cadence(&d.state, "join w1 fake --alias wy --detach");
    assert_ne!(rc, 0, "{out}");
    assert!(out.contains("may register only its own members"), "{out}");
    // Another group's PM cannot register into pm's group, nor a root.
    let r = p.pm2.rpc(
        &d.state,
        "agent_register",
        codex("wx", json!({"upstream": "pm"})),
    );
    assert!(
        frame_err(&r).contains("would answer to 'pm', not to 'pm2'"),
        "{r}"
    );
    let r = p
        .pm2
        .rpc(&d.state, "agent_register", codex("wx", json!({})));
    assert!(frame_err(&r).contains("would be a group root"), "{r}");
    for alias in ["wx", "wy"] {
        assert!(d.rpc("agent_show", json!({"alias": alias})).is_err());
    }
    // An existing alias is the store's duplicate refusal — nothing to
    // authorize, and the launch verbs' reopen path keys off it.
    let r = p.w1.rpc(
        &d.state,
        "agent_register",
        json!({"alias": "pm2", "provider": "inbox", "endpoint_kind": "inbox",
               "cwd": cwd}),
    );
    assert!(!frame_err(&r).contains("caller rule"), "{r}");
    assert!(r["ok"] != true, "{r}");

    // The PM registers its own member — trust-bearing params included.
    let r = p.pm.rpc(
        &d.state,
        "agent_register",
        codex("wp", json!({"upstream": "pm", "approval_policy": "never"})),
    );
    assert_eq!(r["ok"], true, "{r}");
    d.wait_agent("wp", "idle", 15);
    let (rc, out) = p.pm.cadence(&d.state, "join pm fake --alias wj --detach");
    assert_eq!(rc, 0, "{out}");
    d.wait_agent("wj", "idle", 15);
    let (ok, _, err) = d.operator_cadence(&["join", "pm", "fake", "--alias", "wo", "--detach"]);
    assert!(ok, "{err}");
    d.wait_agent("wo", "idle", 15);
    for alias in ["wj", "wo", "wp"] {
        let show = d.rpc("agent_show", json!({"alias": alias})).unwrap();
        assert_eq!(show["agent"]["params"]["upstream"], "pm", "{show}");
    }
}

/// CAD-373 / CAD-374: `monitor stop`, `monitor dispatch`, `message
/// reconcile` and `agent unfence` are operator actions decided by the
/// CONNECTION (`operator_connection`), never by a `pane`/`by` field the
/// caller controls; `job task reopen` refuses every agent that is not
/// the job's PM (its own test below). A pane agent speaking the
/// socket directly — even the fenced worker's own PM, even forging
/// `by:"operator"` or an empty `pane` — and a managed endpoint are
/// refused naming the verb and the rule; nothing lands. The proven
/// operator still succeeds, and reconcile records `by:"operator"`.
#[test]
fn operator_verbs_refuse_agents_whatever_they_claim() {
    let d = TestDaemon::start_opts(slot_opts(2, 1, 900, &[]));
    let home = TempDir::new().unwrap();
    // `lead` is a PM pane: w2 is its member, fenced by an unknown.
    let mut lead = LaneShell::spawn(home.path());
    plant_pane(&d, "lead", lead.pid());
    d.register_member("w2", "lead");
    d.wait_agent("w2", "idle", 10);
    fence_agent(&d, "w2", "x9");
    let mut wk = ManagedWorker::start(&d, "wk");

    // A blocked task (reopen) and a monitor over it (stop, dispatch).
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "gated verbs");
    let project = d.dir.path().to_str().unwrap().to_string();
    d.job_new_repo("pm", "j1", &spec, &sha, &project).unwrap();
    d.task_new_ac("j1", "j1-t", "w1", format!("ok REPORT_SHA:{SHA_A}"))
        .unwrap();
    d.job_dispatch("j1-t", json!({})).unwrap();
    d.wait_task("j1-t", "review", 15);
    d.job_verdict("j1-t", SHA_A, "blocked").unwrap();
    assert_eq!(d.task_state("j1-t"), "blocked");
    d.operator_rpc(
        "monitor_register",
        json!({"monitor": "m1", "project": project, "owner": "operator",
               "tasks": ["j1-t"], "interval_secs": 60, "dispatch_enabled": true}),
    )
    .unwrap();

    let r = lead.rpc(&d.state, "task_reopen", json!({"task": "j1-t"}));
    assert_refused(&r, "job task reopen", "not job 'j1''s PM", "pane reopen");
    let r = wk.rpc("self", "task_reopen", json!({"task": "j1-t"}));
    assert_refused(&r, "job task reopen", "not job 'j1''s PM", "managed reopen");
    let calls = [
        ("monitor_stop", "monitor stop", json!({"monitor": "m1"})),
        (
            "monitor_dispatch",
            "monitor dispatch",
            json!({"monitor": "m1", "task": "j1-t"}),
        ),
        (
            "message_reconcile",
            "message reconcile",
            json!({"message": "x9", "status": "completed", "sha": SHA_B}),
        ),
        (
            "agent_unfence",
            "agent unfence",
            json!({"alias": "w2", "status": "completed"}),
        ),
    ];
    for (method, verb, params) in &calls {
        let r = lead.rpc(&d.state, method, params.clone());
        assert_refused(&r, verb, "is an operator action", &format!("pane {method}"));
        assert!(
            r["error"]["message"].as_str().unwrap().contains("'lead'"),
            "{method}: {r}"
        );
        for (field, value) in FORGED_IDENTITY {
            let r = lead.rpc(&d.state, method, forged(params, field, value));
            assert_eq!(r["ok"], false, "pane {method} forging {field}: {r}");
            assert!(
                r["error"]["message"].as_str().unwrap().contains(verb),
                "pane {method} forging {field}: {r}"
            );
        }
        let r = wk.rpc("self", method, params.clone());
        assert_refused(
            &r,
            verb,
            "is an operator action",
            &format!("managed {method}"),
        );
    }
    // Nothing a refused caller sent landed.
    assert_eq!(d.task_state("j1-t"), "blocked");
    wait_monitor_state(&d, "m1", "active", 5);
    assert_eq!(d.message_state("w2", "x9"), "unknown");
    assert_eq!(
        d.rpc("agent_show", json!({"alias": "w2"})).unwrap()["unknown"],
        1
    );

    // The proven operator still acts, and is who the record names.
    d.operator_rpc("task_reopen", json!({"task": "j1-t"}))
        .unwrap();
    assert_eq!(d.task_state("j1-t"), "draft");
    d.operator_rpc("monitor_stop", json!({"monitor": "m1"}))
        .unwrap();
    let r = d
        .operator_rpc(
            "message_reconcile",
            json!({"message": "x9", "status": "interrupted"}),
        )
        .unwrap();
    assert_eq!(r["message"]["state"], "interrupted", "{r}");
    let rec = d
        .events("w2")
        .into_iter()
        .find(|e| e["kind"] == "reconciled")
        .expect("reconciled event");
    assert_eq!(rec["payload"]["by"], "operator", "{rec}");
}

/// CAD-375 (review R1): the withholding is one filter over every answer,
/// so it covers the read paths beyond `agent_show`: job and task views
/// (the kickoff's `turn_id`), the agent's thread and its message rows
/// (a body quoting the token), events, and a pane capture showing it —
/// and it holds while the generation is cleared, as in a hot restart's
/// window before adoption restores it.
#[test]
fn turn_tokens_are_withheld_on_every_read_path() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register("pm");
    d.fixture_rpc(
        "agent_register",
        json!({"alias": "dv1", "provider": "devin", "endpoint_kind": "pty",
               "cwd": d.dir.path().to_str().unwrap(),
               "params": json!({"upstream": "pm"}).to_string()}),
    )
    .unwrap();
    d.wait_agent("dv1", "idle", 20);
    let (spec, sha) = d.spec_file("spec.md", "token reads");
    d.job_new("pm", "j1", &spec, &sha);
    d.task_new_ac("j1", "j1-t", "dv1", "ok").unwrap();
    d.operator_rpc("agent_ready", json!({"alias": "dv1"}))
        .unwrap();
    let kickoff = d.job_dispatch("j1-t", json!({})).unwrap()["message"]
        .as_str()
        .unwrap()
        .to_string();
    let token = pty_token(&d, "dv1", &kickoff);
    // Prose quoting it: a queued chat message (row + thread entry) and the
    // pane's screen, as if the worker printed `cadence self`.
    d.operator_rpc(
        "thread_send",
        json!({"alias": "dv1", "text": format!("report with {token}"), "message": "q1"}),
    )
    .unwrap();
    let screen = d.pane_file(&mock, "dv1", "screen");
    let shown = std::fs::read_to_string(&screen).unwrap_or_default();
    atomic_write(
        screen.clone(),
        format!("{shown}\nrunning: {kickoff} {token}\n"),
    );

    let reads = [
        ("agent_show", json!({"alias": "dv1"})),
        ("agent_list", json!({})),
        ("agent_events", json!({"alias": "dv1"})),
        ("job_show", json!({"job": "j1"})),
        ("task_show", json!({"task": "j1-t"})),
        ("thread_read", json!({"alias": "dv1"})),
        ("agent_capture", json!({"alias": "dv1"})),
    ];
    let check = |when: &str| {
        for (method, params) in &reads {
            let r = d.rpc(method, params.clone()).unwrap();
            assert!(!r.to_string().contains(&token), "{when} {method}: {r}");
        }
    };
    check("live");
    let capture = d.rpc("agent_capture", json!({"alias": "dv1"})).unwrap();
    assert!(
        capture["capture"]
            .as_str()
            .unwrap()
            .contains("[turn token withheld]"),
        "{capture}"
    );
    let thread = d.rpc("thread_read", json!({"alias": "dv1"})).unwrap();
    assert!(
        thread
            .to_string()
            .contains("report with [turn token withheld]"),
        "{thread}"
    );
    let task = d.rpc("task_show", json!({"task": "j1-t"})).unwrap();
    assert!(task["task"]["kickoff"]["turn_id"].is_null(), "{task}");

    // The restart window: no generation, the turn still running.
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    let generation: String = conn
        .query_row("SELECT generation FROM agents WHERE alias='dv1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    conn.execute("UPDATE agents SET generation=NULL WHERE alias='dv1'", [])
        .unwrap();
    check("no generation");
    conn.execute(
        "UPDATE agents SET generation=?1 WHERE alias='dv1'",
        [&generation],
    )
    .unwrap();
}

/// CAD-370: a brokered request is answered by the operator or the
/// requester's own PM — never by the requesting agent itself (that
/// would defeat the broker), never by a peer, whatever it claims. A
/// refused answer leaves the request pending.
#[test]
fn agent_respond_refuses_the_requester_and_its_peers() {
    let d = TestDaemon::start();
    let home = TempDir::new().unwrap();
    let mut pm = LaneShell::spawn(home.path());
    plant_pane(&d, "lead", pm.pid());
    let mut worker = LaneShell::spawn(home.path());
    plant_pane(&d, "wr", worker.pid());
    let mut peer = LaneShell::spawn(home.path());
    plant_pane(&d, "wp", peer.pid());
    let brokered = json!({"upstream": "lead", "broker_approvals": true}).to_string();
    for alias in ["wr", "wp"] {
        cad162_sql(
            &d,
            "UPDATE agents SET params=?1 WHERE alias=?2",
            &[&brokered, alias],
        );
    }
    // The requester's own connection opens its requests (CAD-376).
    let open = |worker: &mut LaneShell, handle: &str| {
        let r = worker.rpc(
            &d.state,
            "request_open",
            json!({"alias": "wr", "tool": "Bash", "input_summary": "rm -rf /tmp/x",
                   "request": handle}),
        );
        assert_eq!(r["ok"], true, "{r}");
    };
    open(&mut worker, "h1");
    let accept = json!({"alias": "wr", "request": "h1", "decision": "accept"});

    let r = worker.rpc(&d.state, "agent_respond", accept.clone());
    assert_refused(
        &r,
        "agent respond",
        "cannot answer its own request",
        "requester",
    );
    for (field, value) in FORGED_IDENTITY {
        let r = worker.rpc(&d.state, "agent_respond", forged(&accept, field, value));
        assert_eq!(r["ok"], false, "requester forging {field}: {r}");
        assert!(
            r["error"]["message"]
                .as_str()
                .unwrap()
                .contains("agent respond"),
            "requester forging {field}: {r}"
        );
    }
    let r = peer.rpc(&d.state, "agent_respond", accept.clone());
    assert_refused(
        &r,
        "agent respond",
        "cannot answer another agent's request",
        "peer",
    );
    let pending = d
        .operator_rpc("agent_requests", json!({"alias": "wr"}))
        .unwrap();
    assert_eq!(pending["requests"][0]["request"], "h1", "{pending}");

    // The requester's PM answers; so does the operator.
    let r = pm.rpc(&d.state, "agent_respond", accept);
    assert_eq!(r["ok"], true, "{r}");
    open(&mut worker, "h2");
    d.operator_rpc(
        "agent_respond",
        json!({"alias": "wr", "request": "h2", "decision": "decline"}),
    )
    .unwrap();
    let pending = d
        .operator_rpc("agent_requests", json!({"alias": "wr"}))
        .unwrap();
    assert_eq!(pending["requests"], json!([]), "{pending}");
}

/// CAD-452: an answered request's handle stays its agent's until the
/// agent's wait collects the answer. `agent respond` parks the answer
/// and drops the pending entry, and handles are visible to peers
/// (`agent requests`, the `request_opened` event) — so a brokered
/// peer re-opening the handle under its own alias in that gap would
/// own it, the owner's wait would be refused, and the operator's
/// accept would reach the provider as a deny. The squat is refused
/// and records nothing; the owner's retry and wait still collect the
/// accept.
#[test]
fn an_answered_handle_cannot_be_squatted_before_its_wait() {
    let d = TestDaemon::start();
    let home = TempDir::new().unwrap();
    let mut pm = LaneShell::spawn(home.path());
    plant_pane(&d, "lead", pm.pid());
    let mut owner = LaneShell::spawn(home.path());
    plant_pane(&d, "wr", owner.pid());
    let mut peer = LaneShell::spawn(home.path());
    plant_pane(&d, "wp", peer.pid());
    let brokered = json!({"upstream": "lead", "broker_approvals": true}).to_string();
    for alias in ["wr", "wp"] {
        cad162_sql(
            &d,
            "UPDATE agents SET params=?1, state='busy' WHERE alias=?2",
            &[&brokered, alias],
        );
    }
    let open = |alias: &str| {
        json!({"alias": alias, "kind": "approval", "tool": "Bash",
               "input_summary": "rm -rf /tmp/x", "request": "h1"})
    };
    let r = owner.rpc(&d.state, "request_open", open("wr"));
    assert_eq!(r["ok"], true, "{r}");
    let r = pm.rpc(
        &d.state,
        "agent_respond",
        json!({"alias": "wr", "request": "h1", "decision": "accept"}),
    );
    assert_eq!(r["ok"], true, "{r}");
    // The handle is public: the peer read it from the owner's event.
    assert!(d
        .events("wr")
        .iter()
        .any(|e| e["kind"] == "request_opened" && e["payload"]["request"] == "h1"));

    // The squat: the peer opens the parked handle as its own request.
    let r = peer.rpc(&d.state, "request_open", open("wp"));
    assert_refused(&r, "request_open", "answer parked for 'wr'", "squat");
    assert!(d.requests("wp").is_empty());
    let wp = d.rpc("agent_show", json!({"alias": "wp"})).unwrap();
    assert_eq!(wp["agent"]["state"], "busy", "{wp}");
    assert!(!d.events("wp").iter().any(|e| e["kind"] == "request_opened"));

    // The owner's retried open of its answered handle dedupes without
    // re-pending it, and its wait collects the operator's accept.
    let r = owner.rpc(&d.state, "request_open", open("wr"));
    assert_eq!(r["result"]["existing"], true, "{r}");
    assert!(d.requests("wr").is_empty());
    let r = owner.rpc(
        &d.state,
        "request_wait",
        json!({"request": "h1", "wait": 1}),
    );
    assert_eq!(r["result"]["state"], "answered", "{r}");
    assert_eq!(r["result"]["answer"]["decision"], "accept", "{r}");
}

/// CAD-542: `agent_events` is an unscoped `Rule::Read` — a peer agent
/// or any unattributed caller reads another agent's lane — so a
/// brokered `request_opened` carries routing fields only. The
/// input-derived text stays on the pending row `agent_requests`
/// discloses to the operator, the owner and its PM, and on the PM's
/// notice — never on the lane itself (the CAD-506 fix for platform
/// effects, applied to brokered approvals).
#[test]
fn brokered_request_opened_carries_no_input_derived_text() {
    let d = TestDaemon::start();
    let home = TempDir::new().unwrap();
    let mut pm = LaneShell::spawn(home.path());
    plant_pane(&d, "lead", pm.pid());
    let mut owner = LaneShell::spawn(home.path());
    plant_pane(&d, "w1", owner.pid());
    let mut peer = LaneShell::spawn(home.path());
    plant_pane(&d, "w2", peer.pid());
    cad162_sql(
        &d,
        "UPDATE agents SET params=?1, state='busy' WHERE alias='w1'",
        &[&json!({"upstream": "lead", "broker_approvals": true}).to_string()],
    );
    let r = owner.rpc(
        &d.state,
        "request_open",
        json!({"alias": "w1", "kind": "approval", "tool": "Bash",
               "input_summary": "rm -rf CANARY-SUMMARY-9c1d /tmp/x",
               "input": {"command": "echo CANARY-INPUT-77ab"},
               "request": "h-c542"}),
    );
    assert_eq!(r["ok"], true, "{r}");

    // The unscoped lane: a proven peer agent and a caller nothing
    // proves both read it (Rule::Read) and neither sees a canary. The
    // open event still routes — handle, kind, tool — nothing else.
    for (who, frame) in [
        (
            "peer",
            peer.rpc(&d.state, "agent_events", json!({"alias": "w1"})),
        ),
        (
            "unproven",
            unprovable_rpc(&d, "agent_events", json!({"alias": "w1"})),
        ),
    ] {
        assert_eq!(frame["ok"], true, "{who}: {frame}");
        let text = frame["result"].to_string();
        for canary in ["CANARY-SUMMARY-9c1d", "CANARY-INPUT-77ab"] {
            assert!(
                !text.contains(canary),
                "{who}: {canary} on the event lane: {text}"
            );
        }
        let opened = frame["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "request_opened" && e["payload"]["request"] == "h-c542")
            .unwrap_or_else(|| panic!("{who}: no request_opened in {frame}"));
        let payload = &opened["payload"];
        assert_eq!(payload["kind"], "approval", "{payload}");
        assert_eq!(payload["tool"], "Bash", "{payload}");
        for field in ["input_summary", "input", "params", "preview"] {
            assert!(
                payload.get(field).is_none(),
                "{who}: request_opened carries {field}: {payload}"
            );
        }
    }

    // The scoped read keeps the full input: the owner, its PM and the
    // operator see it; the peer and the unproven caller are refused
    // with no hint of it.
    for (who, frame) in [
        (
            "owner",
            owner.rpc(&d.state, "agent_requests", json!({"alias": "w1"})),
        ),
        (
            "pm",
            pm.rpc(&d.state, "agent_requests", json!({"alias": "w1"})),
        ),
        (
            "peer",
            peer.rpc(&d.state, "agent_requests", json!({"alias": "w1"})),
        ),
        (
            "unproven",
            unprovable_rpc(&d, "agent_requests", json!({"alias": "w1"})),
        ),
    ] {
        match who {
            "peer" | "unproven" => {
                assert_eq!(frame["ok"], false, "{who}: {frame}");
                assert!(!frame.to_string().contains("CANARY"), "{who}: {frame}");
            }
            _ => {
                assert_eq!(frame["ok"], true, "{who}: {frame}");
                let text = frame["result"].to_string();
                for canary in ["CANARY-SUMMARY-9c1d", "CANARY-INPUT-77ab"] {
                    assert!(text.contains(canary), "{who}: {canary} missing: {text}");
                }
            }
        }
    }
    let operator = d
        .operator_rpc("agent_requests", json!({"alias": "w1"}))
        .unwrap();
    assert!(
        operator.to_string().contains("CANARY-SUMMARY-9c1d"),
        "{operator}"
    );
}

/// CAD-542: the provider-driven `input_required` is the same lane and
/// the same class — its `params` (a codex `requestApproval` carries
/// the command verbatim) are input-derived, so the event keeps the
/// routing fields (request handle, method) only. The full params stay
/// on the scoped `agent_requests` row the answerer reads.
#[test]
fn provider_input_required_carries_no_input_derived_text() {
    let d = TestDaemon::start();
    let home = TempDir::new().unwrap();
    let mut peer = LaneShell::spawn(home.path());
    plant_pane(&d, "w2", peer.pid());
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    d.operator_rpc(
        "agent_send",
        json!({"alias": "w1", "text": "NEED_INPUT:rm CANARY-PROVIDER-3b2c -rf",
               "message": "m1"}),
    )
    .unwrap();
    d.wait_agent("w1", "waiting_input", 10);

    for (who, frame) in [
        (
            "peer",
            peer.rpc(&d.state, "agent_events", json!({"alias": "w1"})),
        ),
        (
            "unproven",
            unprovable_rpc(&d, "agent_events", json!({"alias": "w1"})),
        ),
    ] {
        assert_eq!(frame["ok"], true, "{who}: {frame}");
        let text = frame["result"].to_string();
        assert!(
            !text.contains("CANARY-PROVIDER-3b2c"),
            "{who}: provider input on the event lane: {text}"
        );
        let required = frame["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "input_required")
            .unwrap_or_else(|| panic!("{who}: no input_required in {frame}"));
        let payload = &required["payload"];
        assert_eq!(
            payload["method"], "item/commandExecution/requestApproval",
            "{payload}"
        );
        assert!(payload["request"].is_string(), "{payload}");
        assert!(
            payload.get("params").is_none(),
            "{who}: input_required carries params: {payload}"
        );
    }

    // The scoped row keeps the full params for the authorised
    // answerer — here the operator reads the command verbatim.
    let operator = d
        .operator_rpc("agent_requests", json!({"alias": "w1"}))
        .unwrap();
    assert!(
        operator.to_string().contains("CANARY-PROVIDER-3b2c"),
        "{operator}"
    );
}

/// CAD-375: a running turn's token is `message_report`'s credential, so
/// the daemon shows it only to the connection that derives the owning
/// agent. A peer's (and the operator's) `agent_show`, `agent_list` and
/// `agent_events` of the agent carry no token anywhere; the agent's own
/// `cadence self` prints it and its `message ack`/`result` still work.
#[test]
fn turn_tokens_are_shown_only_to_the_owning_connection() {
    let d = TestDaemon::start();
    let home = TempDir::new().unwrap();
    let mut owner = LaneShell::spawn(home.path());
    plant_pane(&d, "wa", owner.pid());
    let mut peer = LaneShell::spawn(home.path());
    plant_pane(&d, "wb", peer.pid());
    // A pty worker's delivered turn, awaiting its report.
    cad162_sql(
        &d,
        "UPDATE agents SET provider='devin' WHERE alias IN ('wa','wb')",
        &[],
    );
    let token = "pty-planted-cad375token";
    let now = format!(
        "{}",
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
    );
    cad162_sql(
        &d,
        "INSERT INTO messages(id,alias,body,reply_to,source,state,turn_id,result,created,started)
         VALUES('m-a','wa','task',NULL,'user','running',?1,
                '{\"status\":\"submitted\",\"ack\":null}',CAST(?2 AS REAL),CAST(?2 AS REAL))",
        &[token, &now],
    );
    let bin = env!("CARGO_BIN_EXE_cadence");
    let state = d.state.display().to_string();

    // The owner acks through its own pane — the token works for it.
    let (rc, out) = owner.run(&format!(
        "{bin} --state-dir {state} message ack m-a --token {token}"
    ));
    assert_eq!(rc, 0, "{out}");

    // Every read path a peer has: no token anywhere in the answer.
    let reads = [
        ("agent_show", json!({"alias": "wa"})),
        ("agent_list", json!({})),
        ("agent_events", json!({"alias": "wa"})),
    ];
    for (method, params) in &reads {
        let r = peer.rpc(&d.state, method, params.clone());
        assert_eq!(r["ok"], true, "{method}: {r}");
        assert!(!r.to_string().contains(token), "peer {method}: {r}");
        let r = d.rpc(method, params.clone()).unwrap();
        assert!(!r.to_string().contains(token), "operator {method}: {r}");
    }
    let show = peer.rpc(&d.state, "agent_show", json!({"alias": "wa"}));
    assert!(
        show["result"]["agent"]["awaiting_report"]["message"] == "m-a",
        "{show}"
    );
    assert!(
        show["result"]["agent"]["awaiting_report"]["turn_id"].is_null(),
        "{show}"
    );
    // `cadence self` naming wa from the peer's pane: refused, no token.
    let (rc, out) = peer.run(&format!("CADENCE_ALIAS=wa {bin} --state-dir {state} self"));
    assert_ne!(rc, 0, "{out}");
    assert!(out.contains("shown only to that agent's own pane"), "{out}");
    assert!(!out.contains(token), "{out}");

    // A hot restart's window: the generation is NULL while the turn
    // keeps running and becomes current again on adoption (review R1).
    // A token that is not current NOW is still withheld.
    cad162_sql(
        &d,
        "UPDATE agents SET generation=NULL WHERE alias='wa'",
        &[],
    );
    for (method, params) in &reads {
        let r = peer.rpc(&d.state, method, params.clone());
        assert!(
            !r.to_string().contains(token),
            "peer {method}, no generation: {r}"
        );
    }
    cad162_sql(
        &d,
        "UPDATE agents SET generation='planted' WHERE alias='wa'",
        &[],
    );

    // The owner still reads its own token, on every path.
    for (method, params) in &reads {
        let r = owner.rpc(&d.state, method, params.clone());
        assert!(r.to_string().contains(token), "owner {method}: {r}");
    }
    let (rc, out) = owner.run(&format!("CADENCE_ALIAS=wa {bin} --state-dir {state} self"));
    assert_eq!(rc, 0, "{out}");
    let me: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(me["running"][0]["id"], "m-a", "{me}");
    assert_eq!(me["running"][0]["turn_id"], token, "{me}");
    let (rc, out) = owner.run(&format!(
        "{bin} --state-dir {state} message result m-a --token {token} --text done"
    ));
    assert_eq!(rc, 0, "{out}");
    assert_eq!(d.message_state("wa", "m-a"), "completed");
}

/// CAD-110: `--instructions-file` content reaches every provider through
/// the briefing's role-instructions section, and a later briefing
/// rewrite (`agent bootstrap`) carries it forward.
#[test]
fn instructions_file_lands_in_briefing() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude("ok", None);
    d.register("pm");
    d.wait_agent("pm", "idle", 10);
    let file = d.dir.path().join("qa-role.md");
    std::fs::write(&file, "# QA role\n\nNever merge; report defects to pm.\n").unwrap();
    let file_s = file.to_str().unwrap();

    for (provider, alias) in [("fake", "w-fake"), ("claude", "w-claude")] {
        let out = launch_cli(
            &d,
            &[
                "join",
                "pm",
                provider,
                "--alias",
                alias,
                "--detach",
                "--instructions-file",
                file_s,
            ],
        );
        assert!(
            out.status.success(),
            "{provider}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        d.wait_agent(alias, "idle", 20);
        let briefing = d.state.join(format!("briefings/pm/BRIEFING-{alias}.md"));
        let text = std::fs::read_to_string(&briefing).unwrap();
        assert!(text.contains("## Role instructions"), "{provider}: {text}");
        assert!(
            text.contains("# QA role\n\nNever merge; report defects to pm."),
            "{provider}: {text}"
        );
        // The kickoff points the agent at the section.
        let m = d.rpc("agent_show", json!({"alias": alias})).unwrap()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["source"] == "bootstrap")
            .cloned()
            .unwrap();
        assert!(
            m["body"].as_str().unwrap().contains("role instructions"),
            "{provider}: {m}"
        );
    }

    // `agent bootstrap` rewrites the file without the launch's
    // instructions in hand — the section survives.
    let out = launch_cli(&d, &["agent", "bootstrap", "w-fake"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(d.state.join("briefings/pm/BRIEFING-w-fake.md")).unwrap();
    assert!(
        text.contains("Never merge; report defects to pm."),
        "{text}"
    );
    assert_eq!(text.matches("## Role instructions").count(), 1, "{text}");

    // No instructions file → no section.
    let out = launch_cli(
        &d,
        &["join", "pm", "fake", "--alias", "w-plain", "--detach"],
    );
    assert!(out.status.success());
    d.wait_agent("w-plain", "idle", 15);
    let text = std::fs::read_to_string(d.state.join("briefings/pm/BRIEFING-w-plain.md")).unwrap();
    assert!(!text.contains("Role instructions"), "{text}");
}

/// CAD-110: with `--no-bootstrap` the briefing is skipped, so on every
/// provider but codex the instructions would have no delivery channel —
/// refused before anything is registered. Codex keeps its native
/// `developerInstructions` channel and still launches.
#[test]
fn instructions_file_with_no_bootstrap_refused_except_codex() {
    let d = TestDaemon::start();
    let mock = d.mock_codex_ws("ok");
    d.register("pm");
    d.wait_agent("pm", "idle", 10);
    let file = d.dir.path().join("role.md");
    std::fs::write(&file, "Role: reviewer.").unwrap();
    let file_s = file.to_str().unwrap();

    let refused: [&[&str]; 5] = [
        &["join", "pm", "fake", "--alias", "r-fake"],
        &["join", "pm", "claude", "--alias", "r-claude"],
        &["join", "pm", "devin", "--alias", "r-devin"],
        &["join", "pm", "cursor", "--alias", "r-cursor"],
        &["claude", "--alias", "r-solo"],
    ];
    for verb in refused {
        let mut args = verb.to_vec();
        args.extend(["--detach", "--no-bootstrap", "--instructions-file", file_s]);
        let out = launch_cli(&d, &args);
        assert!(!out.status.success(), "{verb:?} was accepted");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("--instructions-file with --no-bootstrap")
                && stderr.contains("no delivery channel"),
            "{verb:?}: {stderr}"
        );
        let alias = verb[verb.iter().position(|a| *a == "--alias").unwrap() + 1];
        assert!(
            d.rpc("agent_show", json!({"alias": alias})).is_err(),
            "{alias} was registered"
        );
    }

    let out = launch_cli(
        &d,
        &[
            "join",
            "pm",
            "codex",
            "--alias",
            "r-codex",
            "--detach",
            "--no-bootstrap",
            "--instructions-file",
            file_s,
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_agent("r-codex", "idle", 20);
    let reqs = mock_requests(&mock);
    assert_eq!(reqs[0]["method"], "thread/start", "{reqs:?}");
    assert_eq!(
        reqs[0]["params"]["developerInstructions"],
        "Role: reviewer."
    );
}

/// CAD-110: `agent show` advertises the briefing path only while the
/// file exists — a missing file is reported as missing.
#[test]
fn agent_show_reports_missing_briefing() {
    let d = TestDaemon::start();
    d.register("pm");
    d.wait_agent("pm", "idle", 10);
    let out = launch_cli(&d, &["join", "pm", "fake", "--alias", "w1", "--detach"]);
    assert!(out.status.success());
    d.wait_agent("w1", "idle", 15);
    let file = d.state.join("briefings/pm/BRIEFING-w1.md");
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(show["agent"]["briefing"], file.to_str().unwrap());
    assert!(show["agent"].get("briefing_missing").is_none(), "{show}");

    std::fs::remove_file(&file).unwrap();
    let out = launch_cli(&d, &["agent", "show", "w1"]);
    assert!(out.status.success());
    let show: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(show["agent"]["briefing"].is_null(), "{show}");
    assert_eq!(show["agent"]["briefing_missing"], file.to_str().unwrap());

    // A --no-bootstrap worker never had one — missing, not advertised.
    let out = launch_cli(
        &d,
        &[
            "join",
            "pm",
            "fake",
            "--alias",
            "w2",
            "--detach",
            "--no-bootstrap",
        ],
    );
    assert!(out.status.success());
    d.wait_agent("w2", "idle", 15);
    let show = d.rpc("agent_show", json!({"alias": "w2"})).unwrap();
    assert!(show["agent"]["briefing"].is_null(), "{show}");
    assert_eq!(
        show["agent"]["briefing_missing"],
        d.state
            .join("briefings/pm/BRIEFING-w2.md")
            .to_str()
            .unwrap()
    );
}

// ---- CAD-199: opt-in, records-only agent-gc timer ----

/// Seed a state dir with stopped, disabled fake agents pre-aged through
/// a side connection — no clock injection, no sleeping for age.
/// `old` (30 days) is eligible; `young` (2 days) sits under the 7-day
/// floor; `unknown` (30 days) holds an unknown message and `queued`
/// (30 days) a queued one.
fn seed_agent_gc_state() -> TempDir {
    let seeded = TempDir::new().unwrap();
    let state = seeded.path();
    let db = state.join("cadence.sqlite3");
    let cwd = state.to_str().unwrap().to_string();
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    {
        let store = Store::open(&db).unwrap();
        for alias in ["old", "young", "unknown", "queued"] {
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
            .enqueue("unknown", "work", None, "m-u", "user")
            .unwrap();
        store
            .enqueue("queued", "work", None, "m-q", "user")
            .unwrap();
    }
    let conn = rusqlite::Connection::open(&db).unwrap();
    for (alias, days) in [
        ("old", 30.0),
        ("young", 2.0),
        ("unknown", 30.0),
        ("queued", 30.0),
    ] {
        conn.execute(
            "UPDATE agents SET state='stopped', enabled=0, endpoint=NULL,
             updated=? WHERE alias=?",
            rusqlite::params![now - days * 86_400.0, alias],
        )
        .unwrap();
    }
    conn.execute("UPDATE messages SET state='unknown' WHERE id='m-u'", [])
        .unwrap();
    seeded
}

fn agent_gc_removed_events(d: &TestDaemon) -> Vec<Value> {
    d.rpc("agent_events", json!({"alias": "daemon", "after": 0}))
        .unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "agent_gc_removed")
        .cloned()
        .collect()
}

fn agent_gc_timer_status(d: &TestDaemon) -> Value {
    d.rpc("health", json!({})).unwrap()["agent_gc_timer"].clone()
}

#[test]
fn agent_gc_timer_configured_removes_old_row_with_event() {
    let seeded = seed_agent_gc_state();
    // One hour configured — below the floor, so the timer uses 7 days.
    let d = TestDaemon::start_on_opts(
        seeded.path().to_path_buf(),
        daemon::ServeOptions {
            agent_gc: Some(daemon::AgentGcSetting::older_than(3600)),
            ..daemon_opts()
        },
    );
    // The first stall tick after start sweeps; wait for its record.
    let deadline = Instant::now() + Duration::from_secs(15);
    let events = loop {
        let events = agent_gc_removed_events(&d);
        if !events.is_empty() && agent_gc_timer_status(&d)["last_sweep_at"].is_f64() {
            break events;
        }
        assert!(Instant::now() < deadline, "the agent-gc timer never swept");
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(events.len(), 1, "{events:?}");
    let payload = &events[0]["payload"];
    assert_eq!(payload["alias"], "old", "{payload}");
    assert_eq!(payload["records_only"], true, "{payload}");
    assert_eq!(payload["older_than_secs"], 604_800.0, "{payload}");
    assert!(payload["age_secs"].as_f64().unwrap() >= 29.0 * 86_400.0);
    assert!(payload["reason"]
        .as_str()
        .unwrap()
        .contains("agent-gc timer"));
    let note = payload["note"].as_str().unwrap();
    assert!(note.contains("frees no memory and no disk"), "{note}");
    assert!(note.contains("can no longer be resumed"), "{note}");
    assert!(d.rpc("agent_show", json!({"alias": "old"})).is_err());
    // Under the floor, an unknown message, a queued message: all kept.
    for kept in ["young", "unknown", "queued"] {
        d.rpc("agent_show", json!({"alias": kept})).unwrap();
    }
    // `cadence daemon status` renders the effective setting.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["daemon", "status"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status: Value = serde_json::from_slice(&out.stdout).unwrap();
    let timer = &status["agent_gc_timer"];
    assert_eq!(timer["enabled"], true, "{status}");
    assert_eq!(timer["older_than_secs"], 604_800, "{status}");
    assert_eq!(timer["configured_secs"], 3600, "{status}");
    assert_eq!(timer["last_removed"], 1, "{status}");
    assert!(timer["warning"].as_str().unwrap().contains("7-day floor"));
    assert!(timer["note"]
        .as_str()
        .unwrap()
        .contains("frees no memory and no disk"));
}

#[test]
fn agent_gc_timer_unconfigured_never_removes() {
    let seeded = seed_agent_gc_state();
    let d = TestDaemon::start_on_opts(seeded.path().to_path_buf(), daemon_opts());
    // Wait until the timer has checked its (off) setting at least once.
    let deadline = Instant::now() + Duration::from_secs(15);
    while !agent_gc_timer_status(&d)["last_check_at"].is_f64() {
        assert!(
            Instant::now() < deadline,
            "the agent-gc timer never checked"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let timer = agent_gc_timer_status(&d);
    assert_eq!(timer["enabled"], false, "{timer}");
    assert!(timer["older_than_secs"].is_null(), "{timer}");
    assert!(timer["last_sweep_at"].is_null(), "{timer}");
    for kept in ["old", "young", "unknown", "queued"] {
        d.rpc("agent_show", json!({"alias": kept})).unwrap();
    }
    assert!(agent_gc_removed_events(&d).is_empty());
}

// ---- CAD-384: one caller rule for every agent-mutating RPC ----

/// Every row of every table, in a stable order — a refused call must
/// leave this unchanged (CAD-384: every refusal writes nothing).
fn db_snapshot(d: &TestDaemon) -> String {
    let conn = rusqlite::Connection::open_with_flags(
        d.state.join("cadence.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut out = String::new();
    for table in tables {
        let mut stmt = conn.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
        let cols = stmt.column_count();
        let mut rows: Vec<String> = stmt
            .query_map([], |r| {
                Ok((0..cols)
                    .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        rows.sort();
        out.push_str(&format!("## {table}\n{}\n", rows.join("\n")));
    }
    out
}

/// Assert `frame` is a caller-rule refusal and nothing was written.
fn assert_refused_clean(d: &TestDaemon, before: &str, frame: &Value, what: &str) {
    assert_eq!(frame["ok"], false, "{what}: admitted: {frame}");
    let err = frame_err(frame);
    assert!(
        err.contains("caller rule") || err.contains("not provably the operator"),
        "{what}: not a caller-rule refusal: {err}"
    );
    assert_eq!(before, db_snapshot(d), "{what}: a refusal wrote");
}

/// The fleet for the agent-verb probes: fake agents `tgt` (live) and
/// `q` (stopped, one queued message `m-q`), both in pm's group;
/// pm/pm2/w1 are planted panes.
fn cad384_fleet(d: &TestDaemon) -> GuardPanes {
    let p = guard_panes(d);
    for alias in ["tgt", "q"] {
        d.register_member(alias, "pm");
        d.wait_agent(alias, "idle", 15);
    }
    d.operator_rpc("agent_stop", json!({"alias": "q"})).unwrap();
    d.wait_agent("q", "stopped", 10);
    d.send("q", json!({"text": "later", "message": "m-q"}))
        .unwrap();
    assert_eq!(d.message_state("q", "m-q"), "queued");
    p
}

/// CAD-384 acceptance 1 + 4: agent stop/resume and message cancel pass
/// one caller rule (`peer::may_mutate_agent`; unfence, reconcile and
/// respond are #221's operator gates, CAD-370/374). A peer worker,
/// another group's PM and a detached child of an agent (no agent
/// identity, not provably the operator) are refused before anything is
/// written; the target's own PM and the operator are admitted, each
/// attributed to itself.
#[test]
fn cad384_agent_verbs_one_caller_rule() {
    let d = TestDaemon::start();
    let mut p = cad384_fleet(&d);
    let cases = [
        ("agent_stop", json!({"alias": "tgt"})),
        ("agent_resume", json!({"alias": "q"})),
        ("message_cancel", json!({"message": "m-q"})),
    ];
    for (method, params) in &cases {
        let before = db_snapshot(&d);
        let r = p.pm2.rpc(&d.state, method, params.clone());
        assert_refused_clean(&d, &before, &r, &format!("pm2 {method}"));
        let r = p.w1.rpc(&d.state, method, params.clone());
        assert_refused_clean(&d, &before, &r, &format!("w1 {method}"));
        let r = unprovable_rpc(&d, method, params.clone());
        assert_refused_clean(&d, &before, &r, &format!("detached {method}"));
    }
    assert_eq!(d.wait_agent("tgt", "idle", 1)["state"], "idle");
    assert_eq!(d.message_state("q", "m-q"), "queued");

    // The target's PM may not act as the operator either.
    let before = db_snapshot(&d);
    let r = p.pm.rpc(
        &d.state,
        "message_cancel",
        json!({"message": "m-q", "by": "operator"}),
    );
    assert_refused_clean(&d, &before, &r, "pm cancel as operator");

    // The target's own PM: admitted, attributed to itself.
    let r =
        p.pm.rpc(&d.state, "message_cancel", json!({"message": "m-q"}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["result"]["message"]["result"]["by"], "pm", "{r}");
    let r = p.pm.rpc(&d.state, "agent_stop", json!({"alias": "tgt"}));
    assert_eq!(r["ok"], true, "{r}");
    // The operator, from a plain shell through the CLI.
    let (ok, out, err) = d.operator_cadence(&["agent", "resume", "q", "--detach"]);
    assert!(ok, "operator agent resume: {out} {err}");
    let (ok, out, err) = d.operator_cadence(&["agent", "stop", "q"]);
    assert!(ok, "operator agent stop: {out} {err}");
}

/// CAD-384 acceptance 2: the job/task verbs never default `by` to the
/// operator. A detached child of an agent is refused; an agent is
/// attributed to itself and may not name anyone else; the operator's
/// `by` defaults to `operator`.
#[test]
fn cad384_job_verbs_attribute_the_caller() {
    let d = TestDaemon::start();
    let mut p = guard_panes(&d);
    let (spec, sha) = d.spec_file("spec.md", "do the thing");
    for job in ["j1", "j2", "j3"] {
        d.job_new("pm", job, &spec, &sha);
    }
    let task_of = |job: &str| {
        d.rpc("job_show", json!({"job": job})).unwrap()["job"]["tasks"][0]["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (t1, t2) = (task_of("j1"), task_of("j2"));
    let cases = [
        ("job_cancel", json!({"job": "j1"})),
        ("job_close", json!({"job": "j1"})),
        ("task_fail", json!({"task": t1, "reason": "x"})),
        ("task_cancel", json!({"task": t1})),
        ("task_dispatch", json!({"task": t1, "to": "w1"})),
        ("task_accept", json!({"task": t1})),
        ("task_sha", json!({"task": t1, "sha": SHA_A})),
        (
            "monitor_register",
            json!({"monitor": "m1", "project": "p", "tasks": [t1]}),
        ),
    ];
    for (method, params) in &cases {
        let before = db_snapshot(&d);
        let r = unprovable_rpc(&d, method, params.clone());
        assert_refused_clean(&d, &before, &r, &format!("detached {method}"));
        // An agent naming the operator, or another agent, is refused.
        let mut forged = params.clone();
        forged["by"] = json!("operator");
        let r = p.pm2.rpc(&d.state, method, forged);
        assert_refused_clean(&d, &before, &r, &format!("pm2 {method} by operator"));
    }
    // An agent is attributed to itself.
    let r =
        p.pm.rpc(&d.state, "task_fail", json!({"task": t1, "reason": "boom"}));
    assert_eq!(r["ok"], true, "{r}");
    let r = p.pm.rpc(&d.state, "job_cancel", json!({"job": "j1"}));
    assert_eq!(r["ok"], true, "{r}");
    // The operator: `by` defaults to the operator.
    d.operator_rpc("task_cancel", json!({"task": t2})).unwrap();
    let by_of = |job: &str, kind: &str| -> Vec<Value> {
        d.rpc("job_events", json!({"job": job})).unwrap()["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == kind)
            .map(|e| e["payload"]["by"].clone())
            .collect()
    };
    assert_eq!(by_of("j1", "task_failed"), vec![json!("pm")]);
    assert_eq!(by_of("j2", "task_cancelled"), vec![json!("operator")]);
    // The operator CLI still works from a plain shell.
    let (ok, out, err) = d.operator_cadence(&["job", "cancel", "j3"]);
    assert!(ok, "operator job cancel: {out} {err}");
}

/// CAD-384 acceptance 3: operator-attributed writes from the socket
/// need positive operator proof. A detached child of an agent (no
/// agent identity) is refused for `thread_send` and for an
/// `agent_send` that would land in a thread as the operator's.
#[test]
fn cad384_operator_attributed_sends_need_proof() {
    let d = TestDaemon::start();
    d.register("chat");
    d.wait_agent("chat", "idle", 15);
    d.operator_rpc(
        "thread_send",
        json!({"alias": "chat", "text": "hello", "message": "t1"}),
    )
    .unwrap();
    d.wait_message("chat", "t1", &["completed"], 20);
    for (method, params) in [
        (
            "thread_send",
            json!({"alias": "chat", "text": "forged", "message": "f1"}),
        ),
        (
            "agent_send",
            json!({"alias": "chat", "text": "forged", "message": "f2"}),
        ),
    ] {
        let before = db_snapshot(&d);
        let r = unprovable_rpc(&d, method, params);
        assert_eq!(r["ok"], false, "{method}: {r}");
        assert!(
            frame_err(&r).contains("not provably the operator"),
            "{method}: {r}"
        );
        assert_eq!(before, db_snapshot(&d), "{method}: a refusal wrote");
    }
    // The operator's own send still lands as the operator's.
    d.operator_send("chat", json!({"text": "mine", "message": "t2"}))
        .unwrap();
    d.wait_message("chat", "t2", &["completed"], 20);
}

/// CAD-384 acceptance 1 + round-1 I1/I3: `shutdown` (daemon stop)
/// refuses a detached child of an agent and any agent that is not the
/// rollout lease holder under a live OPERATOR GRANT. An agent cannot
/// grant itself, cannot claim the lease without a grant, and a revoked
/// grant blocks both the claim and the shutdown. A refused `daemon
/// restart` from the holder's pane reports the refusal and records no
/// `rollout_restart_proceeded`.
#[test]
fn cad384_shutdown_needs_the_operator_or_a_granted_rollout_holder() {
    use cadence_agent::rollout::{claim, resolve_caller_with, unix_now, ClaimRequest};
    let d = TestDaemon::start();
    let mut p = guard_panes(&d);
    let before = db_snapshot(&d);
    let r = unprovable_rpc(&d, "shutdown", json!({}));
    assert_refused_clean(&d, &before, &r, "detached shutdown");
    let r = p.pm.rpc(&d.state, "shutdown", json!({}));
    assert_refused_clean(&d, &before, &r, "pm shutdown without the lease");

    // Grants are the operator's: an agent is refused, writing nothing.
    for method in ["rollout_grant", "rollout_revoke"] {
        let before = db_snapshot(&d);
        let r = p.pm.rpc(&d.state, method, json!({"agent": "pm"}));
        assert_eq!(r["ok"], false, "{method}: {r}");
        assert!(frame_err(&r).contains("operator action"), "{method}: {r}");
        assert_eq!(before, db_snapshot(&d), "{method}: a refusal wrote");
        let r = unprovable_rpc(&d, method, json!({"agent": "pm"}));
        assert_eq!(r["ok"], false, "detached {method}: {r}");
    }

    let pm = resolve_caller_with(Some("pm"), None).unwrap();
    let claim_pm = || {
        claim(
            &d.state,
            &ClaimRequest {
                caller: &pm,
                reason: "cad384 probe",
                target: None,
                ttl: Duration::from_secs(600),
                takeover: false,
                now: unix_now(),
            },
        )
    };
    // No grant: the agent's claim is refused.
    let e = claim_pm().unwrap_err().to_string();
    assert!(e.contains("holds no rollout grant"), "{e}");
    // Granted, then revoked: still refused.
    d.operator_rpc("rollout_grant", json!({"agent": "pm"}))
        .unwrap();
    d.operator_rpc("rollout_revoke", json!({"agent": "pm"}))
        .unwrap();
    let e = claim_pm().unwrap_err().to_string();
    assert!(e.contains("holds no rollout grant"), "{e}");
    // Granted: the claim lands.
    d.operator_rpc("rollout_grant", json!({"agent": "pm", "until_secs": 3600}))
        .unwrap();
    claim_pm().unwrap();
    let status = cadence_agent::rollout::status(&d.state).unwrap();
    assert_eq!(status["grants"][0]["alias"], "pm", "{status}");
    // Another agent is not the holder.
    let r = p.pm2.rpc(&d.state, "shutdown", json!({}));
    assert_eq!(r["ok"], false, "pm2 is not the holder: {r}");

    // The grant revoked while pm still holds the lease: its pane's
    // `daemon restart` is refused by the daemon — named as such, not
    // "not running" — and nothing records the restart as proceeding.
    d.operator_rpc("rollout_revoke", json!({"agent": "pm"}))
        .unwrap();
    let (rc, out) = p.pm.run(&format!(
        "CADENCE_ALIAS=pm {} --state-dir {} daemon restart",
        env!("CARGO_BIN_EXE_cadence"),
        d.state.display()
    ));
    assert_ne!(rc, 0, "{out}");
    assert!(out.contains("caller rule"), "{out}");
    assert!(!out.contains("does not answer the socket"), "{out}");
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    let proceeded: i64 = conn
        .query_row(
            "SELECT count(*) FROM events WHERE kind='rollout_restart_proceeded'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(proceeded, 0, "a refused restart recorded proceeding");
    assert!(d.rpc("health", json!({})).is_ok());
    let r = p.pm.rpc(&d.state, "shutdown", json!({}));
    assert_eq!(r["ok"], false, "a revoked grant: {r}");

    // Granted again: the holder's pane stops the daemon.
    d.operator_rpc("rollout_grant", json!({"agent": "pm"}))
        .unwrap();
    let r = p.pm.rpc(&d.state, "shutdown", json!({}));
    assert_eq!(r["ok"], true, "the granted holder's pane: {r}");
}

/// CAD-384: the operator's `cadence daemon stop` from a plain shell
/// still stops the daemon (a real `daemon run` process).
#[test]
fn cad384_operator_daemon_stop_from_a_plain_shell() {
    let d = TestDaemon::start_process_in(TempDir::new().unwrap());
    let (ok, out, err) = d.operator_cadence(&["daemon", "stop"]);
    assert!(ok, "operator daemon stop: {out} {err}");
    assert!(d.rpc("health", json!({})).is_err());
}

/// CAD-626: rollback must restart the previous binary even when the
/// failed replacement never reached its RPC socket. Exercise the actual
/// CLI, singleton lock and detached daemon rather than the update mock.
#[test]
fn cad626_restart_recovers_an_unreachable_daemon() {
    for when_idle in [false, true] {
        let mut d = TestDaemon::start_process_in(TempDir::new().unwrap());
        d.register_inbox("recovery-mailbox");
        d.send(
            "recovery-mailbox",
            json!({"text": "keep queued", "message": "recovery-note"}),
        )
        .unwrap();
        let (ok, out, err) = d.operator_cadence(&[
            "rollout",
            "claim",
            "--reason",
            "rollback recovery test",
            "--as",
            "operator:test",
        ]);
        assert!(ok, "claim: {out} {err}");
        let (ok, out, err) = d.operator_cadence(&["daemon", "stop"]);
        assert!(ok, "stop: {out} {err}");
        d.process.take().unwrap().wait().unwrap();
        assert!(d.rpc("health", json!({})).is_err());
        let mut args = vec!["daemon", "restart", "--as", "operator:test"];
        if when_idle {
            args.extend(["--when-idle", "--timeout", "1"]);
        }
        // A dead socket is insufficient: a process can still hold the
        // singleton while draining. It must not be replaced or audited
        // as a restart that proceeded.
        use std::os::unix::io::AsRawFd;
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .open(d.state.join("cadence.lock"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        let (ok, out, err) = d.operator_cadence(&args);
        assert!(!ok, "held singleton was replaced: {out} {err}");
        assert!(err.contains("owns the state-dir lock"), "{out} {err}");
        let proceeded: i64 = rusqlite::Connection::open(d.state.join("cadence.sqlite3"))
            .unwrap()
            .query_row(
                "SELECT count(*) FROM events WHERE kind='rollout_restart_proceeded'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(proceeded, 0);
        drop(lock);
        let (ok, out, err) = d.operator_cadence(&args);
        // Clean up even when a failing assertion would otherwise leave
        // the replacement detached from this fixture's Child handle.
        let health = d.rpc("health", json!({}));
        let mailbox = d.rpc("agent_show", json!({"alias": "recovery-mailbox"}));
        let _ = d.operator_cadence(&["daemon", "stop"]);
        assert!(ok, "when_idle={when_idle}: {out} {err}");
        assert!(health.is_ok(), "recovery never became healthy: {health:?}");
        let mailbox = mailbox.unwrap();
        assert_eq!(mailbox["agent"]["provider"], "inbox");
        assert_eq!(mailbox["messages"][0]["body"], "keep queued");
        assert_eq!(mailbox["messages"][0]["state"], "queued");
    }
}

/// CAD-384 round 2 (R2-1): the sandbox exemption belongs to a SANDBOX
/// daemon only. On a real `daemon run` outside any sandbox, a caller
/// whose ancestor carries an alias this daemon never registered (a
/// detached child of another daemon's agent) — no pane on its ancestry,
/// not a daemon descendant — is still refused `agent_stop` and
/// `shutdown`, and nothing is written. An in-process daemon cannot
/// probe this: every child of the test process descends from it.
#[test]
fn cad384_no_sandbox_exemption_outside_a_sandbox() {
    let d = TestDaemon::start_process_in(TempDir::new().unwrap());
    d.register("w1");
    d.wait_agent("w1", "idle", 15);
    let ghost = |method: &str| -> Value {
        let out = std::process::Command::new("sh")
            .arg("-c")
            // `sh` stays the parent (no exec of the last command), so the
            // ghost alias is on the caller's ancestry, not in its env.
            .arg("env -u CADENCE_ALIAS python3 -c \"$1\" \"$2\" \"$3\"; rc=$?; exit $rc")
            .arg("sh")
            .arg(
                "import socket,sys;s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);\
                 s.sendall(sys.argv[2].encode()+b'\\n');print(s.makefile().readline())",
            )
            .arg(client::socket_path(&d.state))
            .arg(cadence_agent::proto::request(method, json!({"alias": "w1"})).to_string())
            .env("CADENCE_ALIAS", "ghost")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        serde_json::from_slice(&out.stdout).unwrap()
    };
    for method in ["agent_stop", "shutdown"] {
        let before = db_snapshot(&d);
        let r = ghost(method);
        assert_refused_clean(&d, &before, &r, &format!("ghost {method}"));
    }
    assert!(d.rpc("health", json!({})).is_ok(), "the daemon was stopped");
    assert_eq!(d.wait_agent("w1", "idle", 1)["state"], "idle");
}

/// CAD-384 round 2: an operator-shaped lease holder must be the
/// operator. An agent's pane that drops its alias and claims
/// `--as operator:evil` is refused before any lease is written, so it
/// cannot sit on the lease and block the operator's own claim.
#[test]
fn cad384_agent_cannot_claim_the_lease_as_the_operator() {
    let d = TestDaemon::start();
    let mut p = guard_panes(&d);
    let (rc, out) = p.pm.run(&format!(
        "env -u CADENCE_ALIAS {} --state-dir {} rollout claim --reason probe --as operator:evil",
        env!("CARGO_BIN_EXE_cadence"),
        d.state.display()
    ));
    assert_ne!(rc, 0, "{out}");
    assert!(
        out.contains("rollout claim --as is an operator action"),
        "{out}"
    );
    let status = cadence_agent::rollout::status(&d.state).unwrap();
    assert_eq!(status["held"], false, "{status}");
    // The operator's own `--as` claim still lands.
    let (ok, out, err) = d.operator_cadence(&[
        "rollout",
        "claim",
        "--reason",
        "probe",
        "--as",
        "operator:ada",
    ]);
    assert!(ok, "{out} {err}");
}

/// CAD-561: the drain gate, behaviourally. While an update drains, the
/// actor loop claims no new turn: a queued message stays queued until
/// the drain is lifted (or the marker goes stale). Deleting the gate in
/// `daemon.rs` makes this fail — the turn would be claimed at once.
#[test]
fn cad561_the_actor_loop_stops_claiming_turns_while_the_update_drains() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 15);
    // The update's lease, then the drain that names its holder.
    hold_rollout_lease(d.dir.path(), &d.state);
    let drained = d
        .operator_rpc(
            "update_drain",
            json!({"on": true, "label": "operator:test", "target": "b".repeat(40)}),
        )
        .unwrap();
    assert_eq!(drained["draining"], json!(true), "{drained}");
    // A queued turn is not claimed while the fleet is drained.
    d.send(
        "w1",
        json!({"text": "held while draining", "message": "drain-1"}),
    )
    .unwrap();
    thread::sleep(Duration::from_secs(2));
    assert_eq!(
        d.message_state("w1", "drain-1"),
        "queued",
        "the drain must hold the turn"
    );
    // A label the lease does not back cannot drain (and writes nothing).
    let refused = d.operator_rpc(
        "update_drain",
        json!({"on": true, "label": "operator:mallory", "target": "b".repeat(40)}),
    );
    let err = refused.unwrap_err().to_string();
    assert!(
        err.contains("lease is not held by 'operator:mallory'"),
        "{err}"
    );
    assert_eq!(
        cadence_agent::update::pending_update(&d.state)
            .map(|p| p.by)
            .as_deref(),
        Some("operator:test"),
        "the refused drain must not have overwritten the marker"
    );
    // Lifting the drain lets the actor take the turn.
    d.operator_rpc(
        "update_drain",
        json!({"on": false, "label": "operator:test"}),
    )
    .unwrap();
    let row = d.wait_message("w1", "drain-1", &["completed"], 15);
    assert_eq!(row["state"], "completed", "{row}");
}

/// CAD-561: the CLI gate. Inside a pane `cadence update` is refused
/// outright — an agent can never push a build — before anything is
/// claimed or written.
#[test]
fn cad561_update_is_refused_inside_a_pane() {
    let d = TestDaemon::start();
    let mut p = guard_panes(&d);
    let (rc, out) = p.pm.run(&format!(
        "CADENCE_ALIAS=pm {} --state-dir {} update --as operator:ada --check",
        env!("CARGO_BIN_EXE_cadence"),
        d.state.display()
    ));
    assert_ne!(rc, 0, "{out}");
    assert!(
        out.contains("cadence update is an operator action"),
        "{out}"
    );
    assert!(out.contains("cadence pane 'pm'"), "{out}");
    let status = cadence_agent::rollout::status(&d.state).unwrap();
    assert_eq!(status["held"], false, "{status}");
    assert!(!d.state.join(cadence_agent::update::UPDATE_FILE).exists());
    assert!(!d.state.join(cadence_agent::update::LOCK_FILE).exists());
}

/// CAD-561: dropping the alias does not make a pane's child the
/// operator. `env -u CADENCE_ALIAS … update --as operator:evil` passes
/// the pane check (no alias in its own environment) but fails the
/// process proof — an ancestor carries `CADENCE_ALIAS` — and nothing is
/// claimed.
#[test]
fn cad561_update_refuses_a_dropped_alias_claiming_the_operator() {
    let d = TestDaemon::start();
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "env -u CADENCE_ALIAS {} --state-dir {} update --as operator:evil --check",
            env!("CARGO_BIN_EXE_cadence"),
            d.state.display()
        ))
        .env("CADENCE_ALIAS", "ghost")
        .env_remove(cadence_agent::test_seam::AS_ENV)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success(), "{out:?}");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("not provably the operator"), "{text}");
    assert!(text.contains("CADENCE_ALIAS"), "{text}");
    let status = cadence_agent::rollout::status(&d.state).unwrap();
    assert_eq!(status["held"], false, "{status}");
}
