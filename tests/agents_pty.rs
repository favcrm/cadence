//! agents_pty: area tests split from tests/integration.rs (CAD-426).
//! End-to-end tests: real socket daemon in-process, fake provider.
//! These exercise the observable contract — queue order, idempotency,
//! restart fencing, approval brokering, serialization — without model calls.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use cadence_agent::daemon;
use serde_json::{json, Value};
use std::io::Write;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

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

/// CAD-250 `send --nudge`: steering pasted into the live pane while a
/// turn is held. The nudge passes the one-turn hold, never becomes
/// `running`, owes no report and completes at its confirmed paste; the
/// held turn stays the one running row and queued tasks stay queued.
#[test]
fn pty_nudge_steers_without_owning_a_turn() {
    let d = TestDaemon::start();
    let _mock = d.mock_stub();
    d.register("pm");
    d.register_stub("w1", json!({"auto_ready": "verified"}));
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 20);
    for id in ["t1", "t2", "t3"] {
        d.rpc(
            "agent_send",
            json!({"alias": "w1", "text": format!("task {id}"),
                   "message": id, "reply_to": "pm"}),
        )
        .unwrap();
    }
    let token = pty_token(&d, "w1", "t1");
    let (ok, sent) = cadence_cli(
        &d.state,
        &[
            "message",
            "send",
            "w1",
            "--nudge",
            "--text",
            "steer: prefer the smaller fix",
        ],
        &[],
    );
    assert!(ok, "{sent}");
    let nid = sent["message"].as_str().unwrap().to_string();
    let n = d.wait_message("w1", &nid, &["completed"], 20);
    assert_eq!(n["source"], "nudge", "{n}");
    assert_eq!(n["nudge"], true, "{n}");
    assert_eq!(n["result"]["via"], "pty_nudge", "{n}");
    assert!(n["reply_to"].is_null(), "a nudge owes no report: {n}");
    assert!(n.get("awaiting_report").is_none(), "{n}");
    // It never became a turn: no `turn_started` for the nudge.
    assert!(d
        .events("w1")
        .iter()
        .all(|e| { !(e["kind"] == "turn_started" && e["payload"]["message"] == nid.as_str()) }));
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let running: Vec<&str> = show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["state"] == "running")
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(running, ["t1"], "{show}");
    assert_eq!(show["agent"]["awaiting_report"]["message"], "t1", "{show}");
    assert_eq!(d.message_state("w1", "t2"), "queued");
    assert_eq!(d.message_state("w1", "t3"), "queued");
    // No notice or result reached the PM for the nudge.
    let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    assert!(
        pm["messages"].as_array().unwrap().iter().all(|m| {
            !m["body"]
                .as_str()
                .unwrap_or_default()
                .contains(nid.as_str())
        }),
        "{pm}"
    );
    // The held turn is unchanged and reports normally.
    d.rpc(
        "message_report",
        json!({"message": "t1", "token": token, "kind": "result", "text": "done"}),
    )
    .unwrap();
    d.wait_message("w1", "t1", &["completed"], 10);
    pty_token(&d, "w1", "t2");
}

/// CAD-250 N1/N2/N4: a nudge needs a live pane and dies with it. Queued
/// behind an open menu, it is cancelled (`nudge_cancelled`, reason
/// `stop`) when the agent stops — never pasted into a later pane — and a
/// nudge to the stopped agent is refused. Over 500 characters or with a
/// task it is refused outright.
#[test]
fn pty_nudge_needs_a_live_pane_and_dies_with_the_actor() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    let long = "x".repeat(501);
    let err = d
        .rpc(
            "agent_send",
            json!({"alias": "dv1", "text": long, "nudge": true}),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("500"), "{err}");
    let err = d
        .rpc(
            "agent_send",
            json!({"alias": "dv1", "text": "steer", "nudge": true, "task": "t-1"}),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("--task"), "{err}");
    atomic_write(d.pane_file(&mock, "dv1", "tui-state"), DEVIN_MENU);
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "steer", "message": "n1", "nudge": true}),
    )
    .unwrap();
    d.wait_event_where("dv1", "gate_wait", |e| e["payload"]["message"] == "n1", 20);
    d.rpc("agent_stop", json!({"alias": "dv1"})).unwrap();
    d.wait_agent("dv1", "stopped", 20);
    let m = d.wait_message("dv1", "n1", &["cancelled"], 10);
    assert_eq!(m["result"]["via"], "stop_cancelled", "{m}");
    let ev = d.wait_event_where(
        "dv1",
        "nudge_cancelled",
        |e| e["payload"]["message"] == "n1",
        5,
    );
    assert_eq!(ev["payload"]["reason"], "stop", "{ev}");
    let err = d
        .rpc(
            "agent_send",
            json!({"alias": "dv1", "text": "steer", "nudge": true}),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("agent dv1 has no live pane"), "{err}");
    // Nothing reached the pane.
    let input = std::fs::read_to_string(d.pane_file(&mock, "dv1", "input")).unwrap_or_default();
    assert!(!input.contains("steer"), "{input}");
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
    // Since CAD-245 the actor takes the send at once, so a fixed sleep
    // can land while the gate still probes (`submitting`); wait for the
    // recorded refusal of this message, after which it sits out the
    // gate back-off as `queued` (CAD-278).
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "first task", "message": "m1"}),
    )
    .unwrap();
    d.wait_event_where("dv1", "gate_wait", |e| e["payload"]["message"] == "m1", 20);
    assert_eq!(d.message_state("dv1", "m1"), "queued");

    // Operator claim: the head of the FIFO queue (m1) is pasted.
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    let token1 = pty_token(&d, "dv1", "m1");
    assert!(token1.starts_with(&format!("pty-{gen}-")), "{token1}");

    // CAD-250: m1 owes its report before the actor claims the next turn.
    pty_report_done(&d, "dv1", "m1");

    // Literal text with shell metacharacters is pasted verbatim into
    // the pane input — one paste per claim, so m2 needs a new one.
    let tricky = "quote ' $HOME `id` ; rm -rf / & | <tag> \"double\"";
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": tricky, "message": "m2"}),
    )
    .unwrap();
    // m1's success cleared the gate notice, so m2's refusal records its
    // own `gate_wait` (CAD-278: this was the 400 ms sleep that failed).
    d.wait_event_where("dv1", "gate_wait", |e| e["payload"]["message"] == "m2", 20);
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
    // CAD-250: m1 reports so m2 is held by the spent claim, not by the
    // actor's one report-owing turn.
    pty_report_done(&d, "dv1", "m1");
    // The claim was consumed: a second send queues, it does not paste.
    // Wait for m2's own recorded refusal, not a fixed sleep: the actor
    // takes the send at once and the gate may still be probing (CAD-286).
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "two", "message": "m2"}),
    )
    .unwrap();
    // The recorded refusal proves the actor tried m2 and held it; the
    // reason proves it was the missing claim.
    let gate = d.wait_event_where("dv1", "gate_wait", |e| e["payload"]["message"] == "m2", 20);
    assert!(
        gate["payload"]["reason"]
            .as_str()
            .is_some_and(|r| r.contains("no fresh `agent ready` claim")),
        "{gate}"
    );
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

/// CAD-162 on pty: the current `pty-<gen>-…` token is accepted; the
/// same message's token is refused once the endpoint generation moves
/// on, and a managed-claude-shaped token carrying the LIVE generation is
/// refused — never accepted as current.
#[test]
fn pty_report_refuses_stale_generation_and_managed_token() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv1", None);
    let agent = d.wait_agent("dv1", "idle", 20);
    let gen = agent["generation"].as_str().unwrap().to_string();
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    let token = pty_token(&d, "dv1", "m1");
    assert!(token.starts_with(&format!("pty-{gen}-")), "{token}");

    // The endpoint moved to a newer generation: m1's token is stale.
    cad162_sql(
        &d,
        "UPDATE agents SET generation=?1 WHERE alias='dv1'",
        &[CAD162_OTHER_GEN],
    );
    cad162_assert_refused(&d, "dv1", "m1", &token, "earlier generation");
    cad162_sql(
        &d,
        "UPDATE agents SET generation=?1 WHERE alias='dv1'",
        &[&gen],
    );

    // Another endpoint kind's token under the live generation.
    let managed = format!("claude-{gen}-{}", "c".repeat(32));
    cad162_set_turn(&d, "m1", &managed);
    cad162_assert_refused(&d, "dv1", "m1", &managed, "managed token on pty");

    // The genuine token still reports — the refusals were the token's.
    cad162_set_turn(&d, "m1", &token);
    d.rpc(
        "message_report",
        json!({"message": "m1", "token": token, "kind": "ack", "text": "seen"}),
    )
    .unwrap();
    let m = cad162_message(&d, "dv1", "m1");
    assert_eq!(m["state"], "running", "{m}");
    assert_eq!(
        m["result"]["status"], "submitted",
        "pty ack keeps the marker: {m}"
    );
    d.rpc(
        "message_report",
        json!({"message": "m1", "token": token, "kind": "result", "text": "done"}),
    )
    .unwrap();
    d.wait_message("dv1", "m1", &["completed"], 10);
}

#[test]
fn pty_locked_session_refuses_takeover() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    // A foreign process holds the session lock — simulating another TUI.
    let lock = mock.locks.join("held-session.lock");
    // The holder marks `held` only once it owns the flock.
    let held = d.dir.path().join("holder.held");
    let mut holder = std::process::Command::new("python3")
        .args([
            "-c",
            "import fcntl,sys,time; f=open(sys.argv[1],'a'); \
             fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB); \
             open(sys.argv[2],'w').close(); time.sleep(30)",
            lock.to_str().unwrap(),
            held.to_str().unwrap(),
        ])
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !held.exists() {
        assert!(
            Instant::now() < deadline,
            "lock holder never took the flock"
        );
        thread::sleep(Duration::from_millis(20));
    }
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
        .operator_output()
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
        .operator_output()
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
fn pty_respond_rejected_and_mode_blocks_send() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    // No approval channel exists for pty.
    assert!(d
        .operator_rpc(
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
    // The recorded refusal, not a fixed sleep (CAD-286).
    let gate = d.wait_event_where("dv1", "gate_wait", |e| e["payload"]["message"] == "m1", 20);
    assert!(
        gate["payload"]["reason"]
            .as_str()
            .is_some_and(|r| r.contains("tmux mode")),
        "{gate}"
    );
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
    // CAD-175: a body the literal paste can never deliver is refused at
    // send, not answered `queued` and failed later at delivery.
    let err = d
        .rpc(
            "agent_send",
            json!({"alias": "dv1", "text": "line1\nline2", "message": "m1"}),
        )
        .unwrap_err();
    assert!(format!("{err:?}").contains("control characters"), "{err:?}");
    let input = std::fs::read_to_string(d.pane_file(&_mock, "dv1", "input")).unwrap_or_default();
    assert!(!input.contains("line1"));
    let shown = d.rpc("agent_show", json!({"alias": "dv1"})).unwrap();
    assert!(
        !shown["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == "m1"),
        "refused body must not be stored: {shown}"
    );
    // A pre-write rejection must not fence the agent: it stays idle,
    // the pane survives, and the queue keeps draining.
    // CAD-184 kept sleep: absence window — the refusal happened in the
    // RPC; any later fence would be a side effect with no event to await.
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

/// CAD-375: `CADENCE_ALIAS` names the agent `cadence self` asks about,
/// but the daemon shows a running turn's token only to that agent's own
/// pane — a process outside it that sets the env is refused, naming the
/// rule, and never prints the token. The pane's own `cadence self` is
/// `turn_tokens_are_shown_only_to_the_owning_connection`.
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

    // CADENCE_ALIAS set, but not inside dv1's pane: refused, no token.
    let bin = env!("CARGO_BIN_EXE_cadence");
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .arg("self")
        .env("CADENCE_ALIAS", "dv1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stdout}");
    assert!(
        stderr.contains("shown only to that agent's own pane"),
        "{stderr}"
    );
    assert!(!stdout.contains(&token) && !stderr.contains(&token));

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
        .operator_output()
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
        .operator_output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_message("w1", "m10", &["completed"], 15);
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
        .operator_output()
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
    // The gate's recorded refusal proves the actor tried it and held it.
    d.wait_event_where(
        "w-join",
        "gate_wait",
        |e| e["payload"]["message"] == "bootstrap-w-join",
        10,
    );
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
        .operator_output()
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
        .operator_output()
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
    let err = d
        .operator_rpc("agent_remove", json!({"alias": "dv1"}))
        .unwrap_err();
    assert!(err.to_string().contains("agent stop"), "{err}");

    // Fake agents have no endpoint but are actor-owned while running.
    let err = d
        .operator_rpc("agent_remove", json!({"alias": "w-old"}))
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
    d.operator_rpc("agent_remove", json!({"alias": "w-old"}))
        .unwrap();
    assert!(d.rpc("agent_show", json!({"alias": "w-old"})).is_err());

    // gc --older-than filters by `updated` age: both were just stopped.
    let swept = d
        .operator_rpc("agent_gc", json!({"older_than": 3600.0}))
        .unwrap();
    assert_eq!(swept["removed"].as_array().unwrap().len(), 0);
    // Default sweep removes every dead stopped/attention agent.
    let swept = d.operator_rpc("agent_gc", json!({})).unwrap();
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

/// CAD-284: a queued message refuses removal and is named; `--force`
/// removes anyway and records it.
#[test]
fn agent_remove_refuses_queued_message_and_force_overrides() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "stopped", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "later", "message": "m-queued"}),
    )
    .unwrap();
    assert_eq!(d.message_state("w1", "m-queued"), "queued");

    let err = d
        .operator_rpc("agent_remove", json!({"alias": "w1"}))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("message m-queued (queued)") && err.contains("--force"),
        "{err}"
    );
    assert_eq!(d.message_state("w1", "m-queued"), "queued");
    assert!(forced_removals(&d).is_empty());

    d.operator_rpc("agent_remove", json!({"alias": "w1", "force": true}))
        .unwrap();
    assert!(d.rpc("agent_show", json!({"alias": "w1"})).is_err());
    let forced = forced_removals(&d);
    assert_eq!(forced.len(), 1, "{forced:?}");
    assert_eq!(forced[0]["payload"]["alias"], "w1");
    assert_eq!(
        forced[0]["payload"]["messages"],
        json!([["m-queued", "queued"]])
    );
}

/// CAD-284: a non-terminal task assigned to the alias refuses removal
/// (through the CLI); `agent remove --force` overrides and records it.
#[test]
fn agent_remove_refuses_non_terminal_task_and_cli_force_overrides() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "open task");
    d.job_new("pm", "j1", &spec, &sha);
    d.rpc(
        "task_new",
        json!({"job": "j1", "task": "j1-fix", "assignee": "w1"}),
    )
    .unwrap();
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "stopped", 15);

    let remove = |extra: &[&str]| {
        let mut args = vec!["agent", "remove", "w1"];
        args.extend_from_slice(extra);
        d.operator_cadence(&args)
    };
    let (ok, _, err) = remove(&[]);
    assert!(!ok, "removal with an open task succeeded");
    assert!(
        err.contains("task j1-fix (draft)") && err.contains("--force"),
        "{err}"
    );
    d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert!(forced_removals(&d).is_empty());

    let (ok, _, err) = remove(&["--force"]);
    assert!(ok, "{err}");
    assert!(d.rpc("agent_show", json!({"alias": "w1"})).is_err());
    let forced = forced_removals(&d);
    assert_eq!(forced.len(), 1, "{forced:?}");
    assert_eq!(forced[0]["payload"]["tasks"], json!([["j1-fix", "draft"]]));
    // The task itself is untouched — reassign with `job dispatch --to`.
    assert_eq!(d.task_state("j1-fix"), "draft");
}

/// CAD-284: removing a finished worker and its reviewer keeps the job
/// history they carried — kickoff, verdict message and job-scoped
/// events — while unreferenced history is still pruned. Re-joining the
/// same aliases (the provider-change path) never redelivers it.
#[test]
fn agent_remove_keeps_job_history() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.register("qa");
    d.wait_agent("w1", "idle", 10);
    d.wait_agent("qa", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "keep history");
    d.job_new("pm", "j1", &spec, &sha);
    d.rpc(
        "task_new",
        json!({"job": "j1", "task": "j1-fix", "assignee": "w1",
               "acceptance": format!("tests pass REPORT_SHA:{SHA_A}")}),
    )
    .unwrap();

    // Unreferenced history: prunable, as before.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "chat", "message": "chat-1"}),
    )
    .unwrap();
    d.wait_message("w1", "chat-1", &["completed"], 15);

    let kickoff = d.job_dispatch("j1-fix", json!({})).unwrap()["message"]
        .as_str()
        .unwrap()
        .to_string();
    d.wait_message("w1", &kickoff, &["completed"], 15);
    d.wait_task("j1-fix", "review", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "qa", "text": "review j1-fix", "message": "qa-report"}),
    )
    .unwrap();
    d.wait_message("qa", "qa-report", &["completed"], 15);
    d.operator_rpc(
        "task_verdict",
        json!({"task": "j1-fix", "sha": SHA_A, "verdict": "pass",
               "message": "qa-report"}),
    )
    .unwrap();
    assert_eq!(d.task_state("j1-fix"), "verified");

    for alias in ["w1", "qa"] {
        d.rpc("agent_stop", json!({"alias": alias})).unwrap();
        d.wait_agent(alias, "stopped", 15);
        d.operator_rpc("agent_remove", json!({"alias": alias}))
            .unwrap();
    }

    let task = d.rpc("job_show", json!({"job": "j1"})).unwrap()["job"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "j1-fix")
        .unwrap()
        .clone();
    assert!(task.get("attention").is_none(), "{task}");
    assert_eq!(task["kickoff"]["id"], kickoff.as_str(), "{task}");
    assert_eq!(task["kickoff"]["state"], "completed", "{task}");
    assert_eq!(task["latest_verdict"]["message"], "qa-report", "{task}");
    let shown = d.rpc("task_show", json!({"task": "j1-fix"})).unwrap();
    let ids: Vec<&str> = shown["task"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert!(ids.contains(&kickoff.as_str()), "{ids:?}");
    let kinds: Vec<String> = d.rpc("job_events", json!({"job": "j1"})).unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["kind"].as_str().map(str::to_string))
        .collect();
    for k in ["task_running", "task_reported", "verdict_recorded"] {
        assert!(
            kinds.iter().any(|have| have == k),
            "missing {k} in {kinds:?}"
        );
    }

    // Re-join under the same aliases: the kept rows still resolve by
    // id, the unreferenced chat is gone, nothing is redelivered — and
    // the new agents do not list the old agents' rows as their own
    // (CAD-304 S4).
    d.register_member("w1", "pm");
    d.register("qa");
    d.wait_agent("w1", "idle", 10);
    d.wait_agent("qa", "idle", 10);
    let messages = |alias: &str| -> Vec<(String, String)> {
        d.rpc("agent_show", json!({"alias": alias})).unwrap()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                (
                    m["id"].as_str().unwrap().to_string(),
                    m["state"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    assert_eq!(messages("w1"), vec![]);
    assert_eq!(messages("qa"), vec![]);
    let task = d.rpc("job_show", json!({"job": "j1"})).unwrap()["job"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "j1-fix")
        .unwrap()
        .clone();
    assert_eq!(task["kickoff"]["id"], kickoff.as_str(), "{task}");
    assert_eq!(task["kickoff"]["state"], "completed", "{task}");
    assert_eq!(task["latest_verdict"]["message"], "qa-report", "{task}");
    // The new w1's own traffic is listed as usual.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "fresh", "message": "fresh-1"}),
    )
    .unwrap();
    d.wait_message("w1", "fresh-1", &["completed"], 15);
    assert_eq!(messages("w1"), vec![("fresh-1".into(), "completed".into())]);
}

/// CAD-284 train finding with CAD-250: a job kickoff whose pty turn
/// was never reported ends `unknown`. `--force` refuses to decide or
/// discard that outcome; once reconciled it removes the agent, and the
/// same alias re-registered starts idle and unfenced while `job show`
/// still resolves the old kickoff.
#[test]
fn agent_remove_force_refuses_unknown_kickoff_and_rejoin_is_unfenced() {
    let d = TestDaemon::start();
    let _mock = d.mock_stub();
    d.register("pm");
    d.wait_agent("pm", "idle", 10);
    let params = json!({"auto_ready": "verified", "upstream": "pm"});
    d.register_stub("w1", params.clone());
    d.wait_agent("w1", "idle", 20);
    let (spec, sha) = d.spec_file("spec.md", "unknown kickoff");
    d.job_new("pm", "j1", &spec, &sha);
    d.rpc(
        "task_new",
        json!({"job": "j1", "task": "j1-fix", "assignee": "w1"}),
    )
    .unwrap();
    let kickoff = d.job_dispatch("j1-fix", json!({})).unwrap()["message"]
        .as_str()
        .unwrap()
        .to_string();
    pty_token(&d, "w1", &kickoff);
    d.operator_rpc(
        "agent_set",
        json!({"alias": "w1", "patch": {"report_timeout_secs": "1"}}),
    )
    .unwrap();
    d.wait_message("w1", &kickoff, &["unknown"], 20);
    d.wait_agent("w1", "attention", 20);

    let err = d
        .operator_rpc("agent_remove", json!({"alias": "w1"}))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(&format!("message {kickoff} (unknown)")),
        "{err}"
    );
    let err = d
        .operator_rpc("agent_remove", json!({"alias": "w1", "force": true}))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(&format!("message reconcile {kickoff}")),
        "{err}"
    );
    assert_eq!(d.message_state("w1", &kickoff), "unknown");
    assert!(forced_removals(&d).is_empty());

    d.operator_rpc(
        "message_reconcile",
        json!({"message": kickoff, "status": "interrupted"}),
    )
    .unwrap();
    // The task is still open, so removal still needs --force.
    d.operator_rpc("agent_remove", json!({"alias": "w1", "force": true}))
        .unwrap();
    let forced = forced_removals(&d);
    assert_eq!(forced.len(), 1, "{forced:?}");
    assert_eq!(forced[0]["payload"]["messages"], json!([]), "{forced:?}");

    d.register_stub("w1", params);
    d.wait_agent("w1", "idle", 20);
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(show["unknown"], 0, "{show}");
    assert_eq!(show["queued"], 0, "{show}");
    let task = d.rpc("job_show", json!({"job": "j1"})).unwrap()["job"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "j1-fix")
        .unwrap()
        .clone();
    assert_eq!(task["kickoff"]["id"], kickoff.as_str(), "{task}");
    assert_eq!(task["kickoff"]["state"], "interrupted", "{task}");
    // The new incarnation takes new work — nothing old fences it.
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "fresh", "message": "fresh-1"}),
    )
    .unwrap();
    pty_token(&d, "w1", "fresh-1");
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
        .operator_output()
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
        .operator_output()
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
        .operator_output()
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
        .operator_output()
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
        .operator_output()
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
        .operator_output()
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
        .operator_output()
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
        .operator_output()
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
    // The actor requeues before it records `gate_wait`, and the back-off
    // holds it there — no settle sleep needed.
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

/// Idle pty, `auto_ready` unset, no `agent ready`: a routed
/// `worker_result` is submitted by the route's wake. The empty-queue
/// poll is pinned far past the wait, so a missing wake cannot hide
/// behind it (CAD-391: a 2 s deadline against the default 5 s poll
/// failed under load). The same pane then refuses a `source=user` send.
#[test]
fn pty_routed_notice_delivers_idle_without_claim() {
    let d = TestDaemon::start_opts(daemon::ServeOptions {
        idle_poll: Some(Duration::from_secs(600)),
        ..daemon_opts()
    });
    let mock = d.mock_devin();
    d.register_devin("pm", None);
    d.register("w1");
    d.wait_agent("pm", "idle", 20);
    d.wait_agent("w1", "idle", 10);
    let routed_id = route_worker_result(&d, "w1", "pm", "work-1", "review this pane");
    let started = Instant::now();
    let deadline = started + Duration::from_secs(30);
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
        "routed notice still {} after {:?} — the route did not wake the idle actor",
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
    // CAD-250: m2 is claimed once m1 has reported; bob's claim waits.
    pty_report_done(&d, "dv", "m1");
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
    d.operator_rpc(
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
    pty_report_done(&d, "dv", "m1");
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
        .operator_rpc(
            "agent_set",
            json!({"alias": "dv", "patch": {"upstream": "x"}}),
        )
        .unwrap_err();
    assert!(e.to_string().contains("auto_ready"), "{e}");
    let e = d
        .operator_rpc(
            "agent_set",
            json!({"alias": "dv", "patch": {"session": "other"}}),
        )
        .unwrap_err();
    assert!(e.to_string().contains("auto_ready"), "{e}");
    // Only "verified" (or removal) is a valid value.
    let e = d
        .operator_rpc(
            "agent_set",
            json!({"alias": "dv", "patch": {"auto_ready": "bogus"}}),
        )
        .unwrap_err();
    assert!(e.to_string().contains("verified"), "{e}");
    // auto_ready only exists on pty endpoints.
    let e = d
        .operator_rpc(
            "agent_set",
            json!({"alias": "fk", "patch": {"auto_ready": "verified"}}),
        )
        .unwrap_err();
    assert!(e.to_string().contains("pty"), "{e}");
    // The allowed operations still work on a live pty agent.
    d.operator_rpc(
        "agent_set",
        json!({"alias": "dv", "patch": {"auto_ready": "verified"}}),
    )
    .unwrap();
    assert_eq!(
        d.rpc("agent_show", json!({"alias": "dv"})).unwrap()["agent"]["params"]["auto_ready"],
        "verified"
    );
    d.operator_rpc(
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
    // CAD-185: the retry waits (5s after each miss, 5s gate back-off) were
    // most of this test's ~41s and nothing here asserts their length — the
    // count, flags, park and survival are the contract. 1s keeps them.
    test_env().set("CADENCE_PTY_RETRY_SECS", "1");
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
    assert_retry_gaps_under(&d, "pm", &routed_id, 3, 4.0);
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
    d.operator_rpc(
        "agent_set",
        json!({"alias": "dv", "patch": {"auto_ready": "verified"}}),
    )
    .unwrap();
    d.wait_message("dv", "m1", &["running"], 20);
    let show = d.rpc("agent_show", json!({"alias": "dv"})).unwrap();
    assert_eq!(show["agent"]["params"]["auto_ready"], "verified");
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
    assert!(d.job_verdict("j1-t2", SHA_A, "pass").is_err()); // NULL head_sha — never inferred
    d.rpc(
        "task_sha",
        json!({"task": "j1-t2", "sha": SHA_A, "by": "pm"}),
    )
    .unwrap();
    d.job_verdict("j1-t2", SHA_A, "pass").unwrap();
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
    pty_report_done(&d, "st", "m1");
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
    pty_report_done(&d, "st", "m1");
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
    // A blank override must not fall through to a real Devin key, and
    // the API base must not be the live host if a test does open.
    test_env().set("CADENCE_DEVIN_API_KEY", "");
    test_env().set("CADENCE_DEVIN_ORG_ID", "");
    test_env().set("CADENCE_DEVIN_API_BASE", "http://127.0.0.1:9");
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
    pty_report_done(&d, "cl", "m1");
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
    pty_report_done(&d, "cl", "m2");
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

/// CAD-294: Claude Code's prompt suggestion is dim (`ESC[2m`) ghost
/// text in an empty input box. The probe reads the pane with
/// `capture-pane -e`, so a live frame showing a suggestion probes idle
/// and a readiness claim goes through — while a live frame with a
/// typed (undimmed) draft still refuses one. The mock's cursor sits on
/// its own prompt row, not the replayed box: the attributes alone
/// decide.
#[test]
fn pty_claude_dim_prompt_suggestion_probes_idle() {
    let d = TestDaemon::start();
    let mock = d.mock_claude_tui();
    d.register_claude_pty("cl", json!({}));
    d.wait_agent("cl", "idle", 20);
    let fixture = |name: &str| {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/claude-tui/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    };
    let state = d.claude_pane_file(&mock, "cl", "tui-state");
    atomic_write(state.clone(), fixture("suggestion.ansi"));
    let probe = d.rpc("agent_probe", json!({"alias": "cl"})).unwrap();
    assert_eq!(probe["idle"], true, "{probe}");
    assert_eq!(probe["input_nonempty"], false, "{probe}");
    d.rpc("agent_ready", json!({"alias": "cl"})).unwrap();

    atomic_write(state, fixture("typed.ansi"));
    let probe = d.rpc("agent_probe", json!({"alias": "cl"})).unwrap();
    assert_eq!(probe["idle"], false, "{probe}");
    assert_eq!(probe["input_nonempty"], true, "{probe}");
    assert_eq!(
        probe["reason"], "unsubmitted text in the input line",
        "{probe}"
    );
    let err = d.rpc("agent_ready", json!({"alias": "cl"})).unwrap_err();
    assert!(err.to_string().contains("unsubmitted text"), "{err}");
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
        .operator_output()
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
        .operator_output()
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
        .operator_output()
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

/// Register `alias` as a `fake` agent and stop it; returns its full
/// `agent_show` so a refused launch can prove nothing changed.
fn stopped_fake_agent(d: &TestDaemon, alias: &str) -> Value {
    d.register(alias);
    d.wait_agent(alias, "idle", 10);
    d.rpc("agent_stop", json!({"alias": alias})).unwrap();
    d.wait_agent(alias, "stopped", 10);
    d.rpc("agent_show", json!({"alias": alias})).unwrap()
}

/// A launch onto an alias registered under another provider is refused,
/// naming both providers and the remove-then-join remedy.
fn assert_provider_refused(out: &std::process::Output, alias: &str, requested: &str) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    for want in [
        format!("'{alias}' is already registered as a fake agent"),
        format!("not {requested}"),
        format!("cadence agent remove {alias}"),
    ] {
        assert!(stderr.contains(&want), "missing {want:?}: {stderr}");
    }
}

/// CAD-283: `join <pm> claude --tui --alias X` onto a stopped agent of
/// another provider refuses instead of resuming the old one — no row,
/// param, worktree or tmux pane changes.
#[test]
fn cli_join_refuses_different_provider_pty() {
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
    let before = stopped_fake_agent(&d, "wx");
    let bin = env!("CARGO_BIN_EXE_cadence");
    for extra in [&[][..], &["--worktree", "feat-x"][..]] {
        let out = std::process::Command::new(bin)
            .arg("--state-dir")
            .arg(&d.state)
            .args(["join", "pm", "claude", "--tui", "--alias", "wx", "--detach"])
            .args(extra)
            .output()
            .unwrap();
        assert_provider_refused(&out, "wx", "claude");
    }
    let after = d.rpc("agent_show", json!({"alias": "wx"})).unwrap();
    assert_eq!(after, before);
    assert!(!pm_repo.join(".cadence/wt/feat-x").exists());
    assert!(!d.claude_pane_file(&mock, "wx", "argv").exists());
}

/// CAD-283: the managed launch paths — `join <pm> codex` and the
/// standalone `cadence claude` — refuse the same way, stopped or live.
#[test]
fn cli_join_refuses_different_provider_managed() {
    let d = TestDaemon::start();
    d.register("pm");
    d.wait_agent("pm", "idle", 10);
    let before = stopped_fake_agent(&d, "wx");
    let bin = env!("CARGO_BIN_EXE_cadence");
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["join", "pm", "codex", "--alias", "wx", "--detach"])
        .output()
        .unwrap();
    assert_provider_refused(&out, "wx", "codex");
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["claude", "--alias", "wx", "--detach", "--no-bootstrap"])
        .output()
        .unwrap();
    assert_provider_refused(&out, "wx", "claude");
    let after = d.rpc("agent_show", json!({"alias": "wx"})).unwrap();
    assert_eq!(after, before);
    // A live alias is not silently reused under the wrong provider
    // either — and the refusal leaves it exactly as it was (CAD-305).
    d.register("wl");
    d.wait_agent("wl", "idle", 10);
    let before = d.rpc("agent_show", json!({"alias": "wl"})).unwrap();
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["join", "pm", "codex", "--alias", "wl", "--detach"])
        .output()
        .unwrap();
    assert_provider_refused(&out, "wl", "codex");
    let after = d.rpc("agent_show", json!({"alias": "wl"})).unwrap();
    assert_eq!(after, before);
}

/// CAD-283 guard: a same-provider join onto a stopped alias still
/// resumes the existing agent.
#[test]
fn cli_join_same_provider_resumes_stopped_alias() {
    let d = TestDaemon::start();
    d.register("pm");
    d.wait_agent("pm", "idle", 10);
    let before = stopped_fake_agent(&d, "wx");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["join", "pm", "fake", "--alias", "wx", "--detach"])
        .operator_output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("already registered"), "{stderr}");
    let agent = d.wait_agent("wx", "idle", 15);
    assert_eq!(agent["provider"], "fake");
    assert_eq!(agent["params"], before["agent"]["params"]);
}

/// A launch onto an alias registered under the same provider but another
/// endpoint kind is refused, naming both kinds and the remove-then-launch
/// remedy.
fn assert_kind_refused(out: &std::process::Output, alias: &str, registered: &str, asked: &str) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    for want in [
        format!("'{alias}' is already registered as a {registered} agent, not {asked}"),
        format!("cadence agent remove {alias}"),
    ] {
        assert!(stderr.contains(&want), "missing {want:?}: {stderr}");
    }
}

/// CAD-305: an alias registered as managed claude, relaunched with
/// `--tui` (standalone or `join`, with or without `--worktree`), is
/// refused rather than silently reopened as the managed agent — no row,
/// param or worktree changes.
#[test]
fn cli_launch_refuses_endpoint_kind_change_claude() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude("ok", None);
    let pm_repo = d.dir.path().join("pmrepo");
    git_repo(&pm_repo);
    d.rpc(
        "agent_register",
        json!({"alias": "pm", "provider": "fake", "endpoint_kind": "fake",
               "cwd": pm_repo}),
    )
    .unwrap();
    d.wait_agent("pm", "idle", 10);
    d.register_claude("cm", Value::Null);
    d.wait_agent("cm", "idle", 15);
    d.rpc("agent_stop", json!({"alias": "cm"})).unwrap();
    d.wait_agent("cm", "stopped", 10);
    let before = d.rpc("agent_show", json!({"alias": "cm"})).unwrap();
    let bin = env!("CARGO_BIN_EXE_cadence");
    let launches: [&[&str]; 3] = [
        &[
            "claude",
            "--tui",
            "--alias",
            "cm",
            "--detach",
            "--no-bootstrap",
        ],
        &["join", "pm", "claude", "--tui", "--alias", "cm", "--detach"],
        &[
            "join",
            "pm",
            "claude",
            "--tui",
            "--alias",
            "cm",
            "--detach",
            "--worktree",
            "feat-k",
        ],
    ];
    for args in launches {
        let out = std::process::Command::new(bin)
            .arg("--state-dir")
            .arg(&d.state)
            .args(args)
            .output()
            .unwrap();
        assert_kind_refused(&out, "cm", "claude managed", "claude pty");
    }
    let after = d.rpc("agent_show", json!({"alias": "cm"})).unwrap();
    assert_eq!(after, before);
    assert!(!pm_repo.join(".cadence/wt/feat-k").exists());
}

/// CAD-305: Devin pty and Devin Cloud share provider `devin` — a stopped
/// pty alias relaunched with `--cloud` is not resumed as pty (cloud
/// params dropped), and a cloud alias relaunched without `--cloud` is
/// not a silent no-op. Both refuse and leave the row untouched.
#[test]
fn cli_launch_refuses_endpoint_kind_change_devin() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    // A blank key keeps the cloud adapter from ever reaching the network:
    // its open refuses before any HTTP.
    test_env().set("CADENCE_DEVIN_API_KEY", "");
    d.register_devin("dvx", None);
    d.wait_agent("dvx", "idle", 20);
    d.rpc("agent_stop", json!({"alias": "dvx"})).unwrap();
    d.wait_agent("dvx", "stopped", 10);
    let before = d.rpc("agent_show", json!({"alias": "dvx"})).unwrap();
    let bin = env!("CARGO_BIN_EXE_cadence");
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "devin",
            "--cloud",
            "--cloud-params",
            "repo=o/r",
            "--alias",
            "dvx",
        ])
        .args(["--detach", "--no-bootstrap"])
        .output()
        .unwrap();
    assert_kind_refused(&out, "dvx", "devin pty", "devin cloud");
    let after = d.rpc("agent_show", json!({"alias": "dvx"})).unwrap();
    assert_eq!(after, before);

    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.rpc(
        "agent_register",
        json!({"alias": "dvc", "provider": "devin", "endpoint_kind": "cloud",
               "cwd": cwd, "params": json!({"repos": ["o/r"]}).to_string()}),
    )
    .unwrap();
    // With no key the cloud open refuses and parks the agent in
    // `attention` — a settled row to compare against.
    d.wait_agent("dvc", "attention", 10);
    let before = d.rpc("agent_show", json!({"alias": "dvc"})).unwrap();
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["devin", "--alias", "dvc", "--detach", "--no-bootstrap"])
        .output()
        .unwrap();
    assert_kind_refused(&out, "dvc", "devin cloud", "devin pty");
    let after = d.rpc("agent_show", json!({"alias": "dvc"})).unwrap();
    assert_eq!(after, before);
    assert!(!d.pane_file(&mock, "dvc", "argv").exists());
}

/// CAD-305: the alias lookup fails closed — an `agent_show` error other
/// than not-found (here: no daemon answering) is never read as "not
/// registered", so `--worktree` creates no checkout before the launch
/// fails.
#[test]
fn cli_launch_fails_closed_when_alias_lookup_errors() {
    let dir = TempDir::new().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let repo = dir.path().join("repo");
    git_repo(&repo);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&state)
        .args(["claude", "--alias", "wz", "--worktree", "feat-z", "--cwd"])
        .arg(&repo)
        .args(["--detach", "--no-bootstrap"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("Daemon is not reachable"), "{stderr}");
    assert!(!repo.join(".cadence/wt/feat-z").exists(), "{stderr}");
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
        .operator_output()
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
        .operator_output()
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
        .operator_output()
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
    d.operator_rpc(
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
    pty_report_done(&d, "cu", "m1");
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
    pty_report_done(&d, "cu", "m2");
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
        .operator_output()
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
        .operator_output()
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
        .operator_output()
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
        .operator_output()
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
        .operator_output()
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
        .operator_output()
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
        .operator_output()
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
        .operator_output()
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
        .operator_output()
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

/// A turn that ends at the idle prompt without reporting is detected
/// by the sampled probe: `turn_silent_end` fires once per message
/// carrying the age and the admitting probe, the views flag it
/// (`silent_ended`, `ended_secs`, `ended?:` in status, an overview
/// needs-me row naming the `send --nudge` remedy). The message itself is
/// never auto-resolved. Since CAD-250 the nudge pastes without owning a
/// turn, while a plain follow-up send queues behind the unreported turn
/// until it is reported.
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
    // CAD-184 kept sleep: timing is the behaviour (once per message over
    // several sweeps).
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
    // CAD-250: the remedy is a nudge — it owns no turn, so it pastes
    // past the unreported one instead of queueing behind it.
    assert_eq!(
        ended["command"],
        "cadence send w1 --nudge --text \"finish and report …\""
    );
    assert!(ended["title"].as_str().unwrap().contains("w1"));

    // The remedy verbatim: the nudge completes at its confirmed paste
    // with no report, and ms9 is still the one running turn.
    let (ok, nudged) = cadence_cli(
        &d.state,
        &["send", "w1", "--nudge", "--text", "finish and report …"],
        &[],
    );
    assert!(ok, "{nudged}");
    let nudge_id = nudged["message"].as_str().unwrap().to_string();
    let n = d.wait_message("w1", &nudge_id, &["completed"], 20);
    assert_eq!(n["result"]["via"], "pty_nudge", "{n}");
    assert_eq!(n["nudge"], true, "{n}");
    assert_eq!(d.message_state("w1", "ms9"), "running");

    // A plain `--ready` follow-up is accepted but held `queued` behind
    // the unreported turn (not pasted, not refused); the report
    // releases it.
    let (ok, sent) = cadence_cli(
        &d.state,
        &["send", "w1", "--ready", "--text", "continue"],
        &[],
    );
    assert!(ok, "{sent}");
    let ms10 = sent["message"].as_str().unwrap().to_string();
    assert_eq!(sent["state"], "queued", "{sent}");
    assert_eq!(d.message_state("w1", &ms10), "queued");
    d.rpc(
        "message_report",
        json!({"message": "ms9", "token": token, "kind": "result",
               "text": "done"}),
    )
    .unwrap();
    d.wait_message("w1", "ms9", &["completed"], 10);
    let token2 = pty_token(&d, "w1", &ms10);
    d.rpc(
        "message_report",
        json!({"message": ms10, "token": token2, "kind": "result",
               "text": "done"}),
    )
    .unwrap();
    d.wait_message("w1", &ms10, &["completed"], 10);
    stall_sample(0);
}

/// CAD-250 — the aos-pm accumulation shape: a pty worker that is sent
/// several tasks and never reports holds exactly ONE `running` turn;
/// the rest stay `queued` (accepted, never refused, never pasted) while
/// routed notifications still pass. The unreported turn is visible as
/// `awaiting_report` in `agent show`, `status` and the overview. A result
/// report releases the next turn; past `report_timeout_secs` (set live
/// through `agent set`) the unreported turn goes `unknown` with exactly
/// one notice to its `reply_to` and the agent fences — nothing is
/// completed, and nothing queued behind it is replayed or delivered.
#[test]
fn pty_unreported_turn_holds_queue_and_bounds_to_unknown() {
    let d = TestDaemon::start();
    let _mock = d.mock_stub();
    d.register("pm");
    d.register("helper");
    d.register_stub("w1", json!({"auto_ready": "verified"}));
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("helper", "idle", 10);
    d.wait_agent("w1", "idle", 20);
    for i in 1..=5 {
        d.rpc(
            "agent_send",
            json!({"alias": "w1", "text": format!("task {i}"),
                   "message": format!("acc{i}"), "reply_to": "pm"}),
        )
        .unwrap();
    }
    let token1 = pty_token(&d, "w1", "acc1");
    // Proof the actor kept claiming past the held turn: a routed
    // notification to w1 is delivered (complete at paste) while acc2..5
    // stay put.
    d.rpc(
        "agent_send",
        json!({"alias": "helper", "text": "ping", "message": "h1",
               "reply_to": "w1"}),
    )
    .unwrap();
    d.wait_message("helper", "h1", &["completed"], 15);
    let routed = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["source"] == "worker_result")
        .expect("routed result queued on w1")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let delivered = d.wait_message("w1", &routed, &["completed"], 20);
    assert_eq!(delivered["result"]["via"], "pty_deliver", "{delivered}");

    let running_ids = |d: &TestDaemon| -> Vec<String> {
        d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["state"] == "running")
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(running_ids(&d), ["acc1"]);
    for i in 2..=5 {
        assert_eq!(d.message_state("w1", &format!("acc{i}")), "queued");
    }

    // Visible: agent show, status and the overview name the state.
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let awaiting = &show["agent"]["awaiting_report"];
    assert_eq!(awaiting["message"], "acc1", "{awaiting}");
    assert_eq!(awaiting["queued_behind"], 4, "{awaiting}");
    assert_eq!(awaiting["report_timeout_secs"], 7200, "{awaiting}");
    assert!(
        awaiting["remaining_secs"].as_u64().unwrap() > 7000,
        "{awaiting}"
    );
    let acc1 = show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "acc1")
        .unwrap();
    assert_eq!(acc1["awaiting_report"], true, "{acc1}");
    let table = status_table(&d.state, &[]);
    assert!(table.contains("awaiting_report"), "{table}");
    let home = TempDir::new().unwrap();
    let view = overview_at(home.path(), &d.state, None, &[]);
    let row = view["needs_me"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["kind"] == "awaiting_report")
        .cloned()
        .expect("awaiting_report needs-me row");
    assert_eq!(row["command"], "cadence agent show w1", "{row}");
    assert_eq!(row["subject"]["id"], "w1", "{row}");
    assert!(row["title"].as_str().unwrap().contains("4 queued"), "{row}");

    // A result report releases exactly the next turn.
    d.rpc(
        "message_report",
        json!({"message": "acc1", "token": token1, "kind": "result",
               "text": "done"}),
    )
    .unwrap();
    d.wait_message("w1", "acc1", &["completed"], 10);
    pty_token(&d, "w1", "acc2");
    assert_eq!(running_ids(&d), ["acc2"]);

    // The bound, lowered live: acc2 goes `unknown`, never completed.
    d.operator_rpc(
        "agent_set",
        json!({"alias": "w1", "patch": {"report_timeout_secs": "1"}}),
    )
    .unwrap();
    let acc2 = d.wait_message("w1", "acc2", &["unknown"], 20);
    assert_eq!(acc2["result"]["via"], "report_timeout", "{acc2}");
    d.wait_agent("w1", "attention", 20);
    let timeouts = wait_event_count(&d, "w1", "report_timeout", 1, 5);
    assert_eq!(timeouts.len(), 1);
    assert_eq!(timeouts[0]["payload"]["message"], "acc2");
    // Exactly one notice to reply_to, and no fabricated result.
    let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    let about_acc2 = |source: &str| {
        pm["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| {
                m["source"] == source && m["body"].as_str().unwrap_or_default().contains("acc2")
            })
            .count()
    };
    assert_eq!(about_acc2("worker_notice"), 1, "{pm}");
    assert_eq!(about_acc2("worker_result"), 0, "{pm}");
    // Nothing behind it was replayed or delivered; nothing else ran.
    assert!(running_ids(&d).is_empty());
    for i in 3..=5 {
        assert_eq!(d.message_state("w1", &format!("acc{i}")), "queued");
    }
    assert_eq!(d.message_state("w1", "acc1"), "completed");
}

/// `params_updated` events on `alias`'s stream that carry a caller.
fn audited_param_updates(d: &TestDaemon, alias: &str) -> Vec<Value> {
    d.rpc("agent_events", json!({"alias": alias, "after": 0}))
        .unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "params_updated" && !e["payload"]["by"].is_null())
        .map(|e| e["payload"].clone())
        .collect()
}

/// CAD-149: who may change which `--next-launch` key, per caller kind,
/// with the caller derived from the connection — never claimed.
#[test]
fn agent_set_caller_rule_per_caller_kind() {
    let d = TestDaemon::start();
    let _mock = d.mock_codex("ok");
    let mut p = guard_panes(&d);
    d.rpc(
        "agent_register",
        json!({"alias": "w2", "provider": "codex", "endpoint_kind": "managed",
               "cwd": d.dir.path().to_str().unwrap(),
               "params": json!({"upstream": "pm",
                                "approval_policy": "on-request"}).to_string()}),
    )
    .unwrap();
    d.wait_agent("w2", "idle", 15);
    let set = |policy: &str| {
        json!({"alias": "w2", "next_launch": true,
               "patch": {"approval_policy": policy}})
    };
    let param = |alias: &str, key: &str| {
        d.rpc("agent_show", json!({"alias": alias})).unwrap()["agent"]["params"][key].clone()
    };

    // A peer worker's pane: trust-bearing and self-service keys alike.
    let r = p.w1.rpc(&d.state, "agent_set", set("never"));
    let e = frame_err(&r);
    assert!(
        e.contains("agent 'w1' cannot change another agent") && e.contains("its PM 'pm'"),
        "{r}"
    );
    let r = p.w1.rpc(
        &d.state,
        "agent_set",
        json!({"alias": "w2", "next_launch": true, "patch": {"model": "gpt-x"}}),
    );
    assert!(frame_err(&r).contains("cannot change another agent"), "{r}");
    // Another group's PM.
    let r = p.pm2.rpc(&d.state, "agent_set", set("never"));
    assert!(
        frame_err(&r).contains("agent 'pm2' cannot change another agent"),
        "{r}"
    );
    // Tied to no pane and not provably the operator.
    let r = unprovable_rpc(&d, "agent_set", set("never"));
    assert!(frame_err(&r).contains("not provably the operator"), "{r}");
    // A claimed identity never changes who is asking: identity-shaped
    // request fields are refused, and an exported CADENCE_ALIAS of the
    // PM leaves the CLI caller the pane it runs in.
    for field in ["by", "pane", "as", "reviewer"] {
        let mut forged = set("never");
        forged[field] = json!("operator");
        let r = p.w1.rpc(&d.state, "agent_set", forged);
        assert!(
            frame_err(&r).contains(&format!("'{field}' is not accepted")),
            "{r}"
        );
    }
    let (rc, out) = p.w1.run(&format!(
        "CADENCE_ALIAS=pm {} --state-dir {} agent set w2 approval_policy=never --next-launch",
        env!("CARGO_BIN_EXE_cadence"),
        d.state.display()
    ));
    assert_ne!(rc, 0, "{out}");
    assert!(
        out.contains("agent 'w1' cannot change another agent"),
        "{out}"
    );
    assert_eq!(param("w2", "approval_policy"), "on-request");
    assert!(audited_param_updates(&d, "w2").is_empty());

    // The worker on itself: model/effort only.
    let r = p.w1.rpc(
        &d.state,
        "agent_set",
        json!({"alias": "w1", "next_launch": true,
               "patch": {"model": "opus", "effort": "high"}}),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(param("w1", "model"), "opus");
    for patch in [
        json!({"approval_policy": "never"}),
        // A mixed patch is judged by its most sensitive key — and
        // applies nothing.
        json!({"model": "sonnet", "approval_policy": "never"}),
    ] {
        let r = p.w1.rpc(
            &d.state,
            "agent_set",
            json!({"alias": "w1", "next_launch": true, "patch": patch}),
        );
        assert!(
            frame_err(&r).contains("agent 'w1' cannot make this change to itself"),
            "{r}"
        );
    }
    assert_eq!(param("w1", "model"), "opus");
    // Posture (a live key) is not self-service either.
    let r = p.w1.rpc(
        &d.state,
        "agent_set",
        json!({"alias": "w1", "patch": {"auto_stop": "off"}}),
    );
    assert!(
        frame_err(&r).contains("cannot make this change to itself"),
        "{r}"
    );
    // A PM is not its own PM.
    let r = p.pm.rpc(
        &d.state,
        "agent_set",
        json!({"alias": "pm", "next_launch": true,
               "patch": {"approval_policy": "never"}}),
    );
    assert!(
        frame_err(&r).contains("agent 'pm' cannot make this change to itself"),
        "{r}"
    );

    // The target's own PM, then the operator: accepted and recorded
    // with the derived caller and each key's old and new value.
    let r = p.pm.rpc(&d.state, "agent_set", set("never"));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(param("w2", "approval_policy"), "never");
    d.operator_rpc("agent_set", set("untrusted")).unwrap();
    assert_eq!(param("w2", "approval_policy"), "untrusted");
    let updates = audited_param_updates(&d, "w2");
    assert_eq!(updates.len(), 2, "{updates:?}");
    assert_eq!(updates[0]["by"], "pm", "{updates:?}");
    assert_eq!(updates[0]["by_kind"], "agent", "{updates:?}");
    assert_eq!(updates[0]["target"], "w2", "{updates:?}");
    assert_eq!(updates[0]["next_launch"], true, "{updates:?}");
    assert_eq!(
        updates[0]["changes"],
        json!([{"key": "approval_policy", "old": "on-request", "new": "never"}])
    );
    assert_eq!(updates[1]["by"], "operator", "{updates:?}");
    assert_eq!(updates[1]["by_kind"], "operator", "{updates:?}");
    assert_eq!(
        updates[1]["changes"],
        json!([{"key": "approval_policy", "old": "never", "new": "untrusted"}])
    );
    let own = audited_param_updates(&d, "w1");
    assert_eq!(own.len(), 1, "{own:?}");
    assert_eq!(own[0]["by"], "w1", "{own:?}");
}

/// CAD-304 S3: `agent remove` (plain or `--force`) and `agent gc`
/// delete history — the operator's or the target's own PM's call,
/// never a peer's, another group's PM's or the agent's own.
#[test]
fn agent_remove_and_gc_caller_rule_per_caller_kind() {
    let d = TestDaemon::start();
    let mut p = guard_panes(&d);
    for w in ["w3", "w4", "w5"] {
        d.register_member(w, "pm");
    }
    d.register("w6");
    for w in ["w3", "w4", "w5", "w6"] {
        d.wait_agent(w, "idle", 10);
        d.rpc("agent_stop", json!({"alias": w})).unwrap();
        d.wait_agent(w, "stopped", 15);
    }
    let present = |alias: &str| d.rpc("agent_show", json!({"alias": alias})).is_ok();

    for force in [false, true] {
        let r = p.w1.rpc(
            &d.state,
            "agent_remove",
            json!({"alias": "w3", "force": force}),
        );
        assert!(
            frame_err(&r).contains("agent 'w1' cannot change another agent"),
            "{r}"
        );
        let r = p.pm2.rpc(
            &d.state,
            "agent_remove",
            json!({"alias": "w3", "force": force}),
        );
        assert!(
            frame_err(&r).contains("agent 'pm2' cannot change another agent"),
            "{r}"
        );
    }
    let r = p.w1.rpc(&d.state, "agent_remove", json!({"alias": "w1"}));
    assert!(
        frame_err(&r).contains("agent 'w1' cannot make this change to itself"),
        "{r}"
    );
    let r = unprovable_rpc(&d, "agent_remove", json!({"alias": "w3"}));
    assert!(frame_err(&r).contains("not provably the operator"), "{r}");
    let r =
        p.w1.rpc(&d.state, "agent_remove", json!({"alias": "w3", "by": "pm"}));
    assert!(frame_err(&r).contains("'by' is not accepted"), "{r}");
    let (rc, out) = p.w1.run(&format!(
        "CADENCE_ALIAS=pm {} --state-dir {} agent remove w3 --force",
        env!("CARGO_BIN_EXE_cadence"),
        d.state.display()
    ));
    assert_ne!(rc, 0, "{out}");
    assert!(
        out.contains("agent 'w1' cannot change another agent"),
        "{out}"
    );
    // A sweep from a worker's pane removes nothing it is not PM of.
    let r = p.w1.rpc(&d.state, "agent_gc", json!({}));
    assert_eq!(r["result"]["removed"], json!([]), "{r}");
    assert_eq!(
        r["result"]["not_permitted"],
        json!(["w3", "w4", "w5", "w6"]),
        "{r}"
    );
    let r = unprovable_rpc(&d, "agent_gc", json!({}));
    assert!(frame_err(&r).contains("not provably the operator"), "{r}");
    for w in ["w1", "w3", "w4", "w5", "w6"] {
        assert!(present(w), "{w} was removed by a refused caller");
    }

    // The target's own PM: removes its member, sweeps only its group.
    let r = p.pm.rpc(&d.state, "agent_remove", json!({"alias": "w3"}));
    assert_eq!(r["ok"], true, "{r}");
    let r = p.pm.rpc(&d.state, "agent_gc", json!({}));
    assert_eq!(r["result"]["removed"], json!(["w4", "w5"]), "{r}");
    assert_eq!(r["result"]["not_permitted"], json!(["w6"]), "{r}");
    // The operator: anything.
    d.operator_rpc("agent_remove", json!({"alias": "w6"}))
        .unwrap();
    let removals: Vec<(String, String)> = d
        .rpc("agent_events", json!({"alias": "daemon", "after": 0}))
        .unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "agent_removed")
        .map(|e| {
            (
                e["payload"]["alias"].as_str().unwrap().to_string(),
                e["payload"]["by"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let want: Vec<(String, String)> =
        [("w3", "pm"), ("w4", "pm"), ("w5", "pm"), ("w6", "operator")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
    assert_eq!(removals, want);
}

/// CAD-304 S4 / review F4, operator ruling: `--force` unassigns the
/// removed alias's open tasks in the same transaction — state, revision
/// and history kept, the PM told — so an agent registered again under
/// the alias inherits nothing, and dispatch refuses until the task is
/// reassigned, naming the remedy.
#[test]
fn agent_remove_force_unassigns_open_tasks() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.register_member("w2", "pm");
    for a in ["pm", "w1", "w2"] {
        d.wait_agent(a, "idle", 10);
    }
    let (spec, sha) = d.spec_file("spec.md", "unassign on force");
    d.job_new("pm", "j1", &spec, &sha);
    d.rpc(
        "task_new",
        json!({"job": "j1", "task": "t1", "assignee": "w1",
               "acceptance": format!("tests pass REPORT_SHA:{SHA_A}")}),
    )
    .unwrap();
    d.rpc(
        "task_new",
        json!({"job": "j1", "task": "t2", "assignee": "w1",
               "acceptance": "later"}),
    )
    .unwrap();
    let kickoff = d.job_dispatch("t1", json!({})).unwrap()["message"]
        .as_str()
        .unwrap()
        .to_string();
    d.wait_message("w1", &kickoff, &["completed"], 15);
    d.wait_task("t1", "review", 15);
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "stopped", 15);

    d.operator_rpc("agent_remove", json!({"alias": "w1", "force": true}))
        .unwrap();
    let forced = forced_removals(&d);
    assert_eq!(forced.len(), 1, "{forced:?}");
    assert_eq!(forced[0]["payload"]["unassigned"], json!(["t1", "t2"]));
    let job = d.rpc("job_show", json!({"job": "j1"})).unwrap()["job"].clone();
    let task = |id: &str| {
        job["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .unwrap()
            .clone()
    };
    assert!(task("t1")["assignee"].is_null(), "{job}");
    assert_eq!(task("t1")["state"], "review", "{job}");
    assert_eq!(task("t1")["kickoff"]["id"], kickoff.as_str(), "{job}");
    assert!(task("t2")["assignee"].is_null(), "{job}");
    assert_eq!(task("t2")["state"], "draft", "{job}");
    // The PM hears which tasks to reassign.
    let pm_notes: Vec<String> = d.rpc("agent_show", json!({"alias": "pm"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["source"] == "job_event")
        .filter_map(|m| m["body"].as_str().map(str::to_string))
        .filter(|b| b.contains("is unassigned"))
        .collect();
    assert_eq!(pm_notes.len(), 2, "{pm_notes:?}");
    assert!(
        pm_notes.iter().all(|b| b.contains("--to <worker>")),
        "{pm_notes:?}"
    );

    // Re-registered: nothing attaches to the new agent.
    d.register_member("w1", "pm");
    d.wait_agent("w1", "idle", 10);
    let list = d.rpc("agent_list", json!({})).unwrap();
    let w1 = list["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"] == "w1")
        .unwrap()
        .clone();
    assert_eq!(w1["tasks"], json!([]), "{w1}");
    // Dispatch refuses the unassigned task until it is reassigned.
    let err = d.job_dispatch("t2", json!({})).unwrap_err().to_string();
    assert!(
        err.contains("has no assignee") && err.contains("--to <worker>"),
        "{err}"
    );
    // Reassigned, it dispatches — to the new w1 as well: no
    // "unfinished task work" inherited from the old one.
    d.job_dispatch("t2", json!({"to": "w1"})).unwrap();
    assert_ne!(d.task_state("t2"), "draft");
    // And a plain removal of the new agent meets only its own work.
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "stopped", 15);
    let err = d
        .operator_rpc("agent_remove", json!({"alias": "w1"}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("task t2") && !err.contains("task t1"), "{err}");
}

/// CAD-304 S2: a forced removal cancels queued work through the normal
/// cancel path — the `reply_to` hears it, and the forced event names
/// who was notified and by whom.
#[test]
fn agent_remove_force_notifies_reply_to() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    d.wait_agent("w1", "stopped", 15);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "later", "message": "m-queued", "reply_to": "pm"}),
    )
    .unwrap();
    d.operator_rpc("agent_remove", json!({"alias": "w1", "force": true}))
        .unwrap();
    let forced = forced_removals(&d);
    assert_eq!(forced.len(), 1, "{forced:?}");
    assert_eq!(
        forced[0]["payload"]["notified"],
        json!(["pm"]),
        "{forced:?}"
    );
    assert_eq!(forced[0]["payload"]["by"], "operator", "{forced:?}");
    let notice = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        b"cadence-notice:cancelled:m-queued",
    )
    .simple()
    .to_string();
    let m = d.wait_message("pm", &notice, &["completed", "queued", "running"], 15);
    assert_eq!(m["source"], "worker_notice", "{m}");
    assert!(
        m["body"].as_str().unwrap_or_default().contains("m-queued"),
        "{m}"
    );
}
