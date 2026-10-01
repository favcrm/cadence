//! agent_wait (CAD-886): `cadence agent wait` — block until an agent
//! reports, idles or needs attention. End-to-end over a real socket
//! daemon in-process with the fake provider: one test per `--until`
//! condition, timeout exit 75, sub-second return after the matching
//! event, disconnect freeing, and turn-token withholding for
//! non-owners. Unit proofs for each predicate arm live in
//! `src/daemon/agent_wait.rs`.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use cadence_agent::{client, test_seam};
use serde_json::json;
use serde_json::Value;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

/// `client::rpc` against `state` from a thread (no `TestDaemon`
/// sharing): the waiter side of every blocking test.
fn wait_rpc(state: PathBuf, params: Value) -> cadence_agent::Result<Value> {
    client::rpc(&state, "agent_wait", params)
}

/// The same call as an asserted agent (needs a seam-armed fixture,
/// i.e. `--features test-seam`, like CI).
fn wait_agent_rpc(state: PathBuf, alias: &str, params: Value) -> cadence_agent::Result<Value> {
    test_seam::scoped(test_seam::Asserted::Agent(alias.to_string()), || {
        client::rpc(&state, "agent_wait", params)
    })
}

/// Run the built CLI against `state`, returning (exit code, stdout,
/// stderr). Operator identity is asserted when the fixture arms the
/// seam (inert otherwise); `agent wait` is `Rule::Read` either way.
fn cli_wait(state: &Path, dir: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env(test_seam::AS_ENV, "operator")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Human reading of a wait answer for assertion messages.
fn summary(v: &Value) -> String {
    v.to_string()
}

/// Poll `agent_show` until `alias` reaches `want`, returning when first
/// seen (the matching event's observation time for latency asserts).
fn wait_state(d: &TestDaemon, alias: &str, want: &str, secs: u64) -> Instant {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let state = d.rpc("agent_show", json!({"alias": alias})).unwrap()["agent"]["state"]
            .as_str()
            .unwrap()
            .to_string();
        if state == want {
            return Instant::now();
        }
        assert!(
            Instant::now() < deadline,
            "agent {alias} never reached {want}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// Poll until message `id` on `alias` reaches one of `want`.
fn wait_msg(d: &TestDaemon, alias: &str, id: &str, want: &[&str], secs: u64) -> Instant {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let show = d.rpc("agent_show", json!({"alias": alias})).unwrap();
        if let Some(m) = show["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"].as_str() == Some(id))
        {
            if want.contains(&m["state"].as_str().unwrap_or("")) {
                return Instant::now();
            }
        }
        assert!(
            Instant::now() < deadline,
            "message {id} never reached {want:?}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn wait_idle_returns_at_once_on_an_idle_agent() {
    let d = TestDaemon::start();
    d.register("w");
    d.wait_agent("w", "idle", 10);
    let t0 = Instant::now();
    let out = d
        .rpc(
            "agent_wait",
            json!({"alias": "w", "until": "idle", "timeout": 30}),
        )
        .unwrap();
    assert!(t0.elapsed() < Duration::from_secs(2), "blocked on idle");
    assert_eq!(out["alias"], "w", "{}", summary(&out));
    assert_eq!(out["state"], "idle");
    assert_eq!(out["reason"], "idle");
    assert!(out.get("message").is_none(), "{}", summary(&out));
    assert!(out.get("turn").is_none(), "{}", summary(&out));
    assert!(out["waited_secs"].as_f64().unwrap() < 2.0);
}

#[test]
fn wait_reported_fires_within_a_second_of_the_turn_completing() {
    let d = TestDaemon::start();
    d.register("w");
    d.wait_agent("w", "idle", 10);
    let state = d.state.clone();
    let waiter = thread::spawn(move || {
        wait_rpc(
            state,
            json!({"alias": "w", "until": "reported", "timeout": 30}),
        )
    });
    // Let the waiter block so the return proves the daemon woke it.
    thread::sleep(Duration::from_millis(300));
    d.send("w", json!({"text": "hello", "message": "m1"}))
        .unwrap();
    let done_at = wait_msg(&d, "w", "m1", &["completed", "failed"], 15);
    let out = waiter.join().unwrap().unwrap();
    assert!(
        Instant::now() < done_at + Duration::from_secs(1),
        "waiter returned more than 1 s after the completion"
    );
    assert_eq!(out["reason"], "reported", "{}", summary(&out));
    assert_eq!(out["message"], "m1");
    assert_eq!(out["state"], "idle");
    // The token is dead (terminal message): visible, like `agent_show`.
    assert!(out["turn"].as_str().is_some(), "{}", summary(&out));
}

#[test]
fn wait_message_narrows_reported_to_that_turn() {
    let d = TestDaemon::start();
    d.register("w");
    d.wait_agent("w", "idle", 10);
    d.send("w", json!({"text": "SLEEP:3", "message": "m-sleep"}))
        .unwrap();
    wait_msg(&d, "w", "m-sleep", &["running"], 10);
    let state = d.state.clone();
    let t0 = Instant::now();
    let waiter = thread::spawn(move || {
        wait_rpc(
            state,
            json!({"alias": "w", "until": "reported", "timeout": 30, "message": "m-sleep"}),
        )
    });
    let out = waiter.join().unwrap().unwrap();
    // The hold ran ~3 s: the wait blocked for the named turn, not a timeout.
    assert!(
        t0.elapsed() >= Duration::from_secs(2),
        "returned too fast: {out}"
    );
    assert_eq!(out["reason"], "reported", "{}", summary(&out));
    assert_eq!(out["message"], "m-sleep");
}

#[test]
fn wait_attention_fires_on_fence_within_a_second() {
    let d = TestDaemon::start();
    d.register("w");
    d.wait_agent("w", "idle", 10);
    let state = d.state.clone();
    let waiter = thread::spawn(move || {
        wait_rpc(
            state,
            json!({"alias": "w", "until": "attention", "timeout": 30}),
        )
    });
    thread::sleep(Duration::from_millis(300));
    d.send("w", json!({"text": "DISCONNECT", "message": "m-fence"}))
        .unwrap();
    let fenced_at = wait_state(&d, "w", "attention", 15);
    let out = waiter.join().unwrap().unwrap();
    assert!(
        Instant::now() < fenced_at + Duration::from_secs(1),
        "waiter returned more than 1 s after the fence"
    );
    assert_eq!(out["reason"], "fence", "{}", summary(&out));
    assert_eq!(out["message"], "m-fence");
    assert_eq!(out["state"], "attention");
}

#[test]
fn wait_attention_fires_on_agent_stop() {
    let d = TestDaemon::start();
    d.register("w");
    d.wait_agent("w", "idle", 10);
    let state = d.state.clone();
    let waiter = thread::spawn(move || {
        wait_rpc(
            state,
            json!({"alias": "w", "until": "attention", "timeout": 30}),
        )
    });
    thread::sleep(Duration::from_millis(300));
    let t0 = Instant::now();
    d.fixture_rpc("agent_stop", json!({"alias": "w"})).unwrap();
    let out = waiter.join().unwrap().unwrap();
    assert!(
        t0.elapsed() < Duration::from_secs(1),
        "waiter returned more than 1 s after the stop"
    );
    assert_eq!(out["reason"], "stopped", "{}", summary(&out));
    assert_eq!(out["state"], "stopped");
}

/// Owner sees the live turn, the PM sees `null`, a stranger's wait never
/// fires on the brokered approval (I2 + I3).
#[test]
fn wait_approval_pending_withholds_the_turn_from_non_owners() {
    let d = TestDaemon::start();
    d.register("pm");
    // `w` answers to `pm`: registered after it with `params.upstream`.
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.fixture_rpc(
        "agent_register",
        json!({"alias": "w", "provider": "fake", "endpoint_kind": "fake",
               "cwd": cwd, "role": "worker",
               "params": "{\"upstream\":\"pm\"}"}),
    )
    .unwrap();
    d.wait_agent("w", "idle", 10);
    let state = d.state.clone();
    let owner = thread::spawn({
        let state = state.clone();
        move || {
            wait_agent_rpc(
                state,
                "w",
                json!({"alias": "w", "until": "attention", "timeout": 30}),
            )
        }
    });
    let pm = thread::spawn({
        let state = state.clone();
        move || {
            wait_agent_rpc(
                state,
                "pm",
                json!({"alias": "w", "until": "attention", "timeout": 30}),
            )
        }
    });
    let stranger = thread::spawn({
        let state = state.clone();
        move || {
            wait_agent_rpc(
                state,
                "stranger",
                json!({"alias": "w", "until": "attention", "timeout": 3}),
            )
        }
    });
    thread::sleep(Duration::from_millis(300));
    d.send(
        "w",
        json!({"text": "NEED_INPUT:rm -rf /", "message": "m-appr"}),
    )
    .unwrap();
    // The matching event: the brokered request opens.
    let deadline = Instant::now() + Duration::from_secs(15);
    let opened_at = loop {
        let reqs = d
            .operator_rpc("agent_requests", json!({"alias": "w"}))
            .unwrap();
        if !reqs["requests"].as_array().unwrap().is_empty() {
            break Instant::now();
        }
        assert!(Instant::now() < deadline, "approval never opened");
        thread::sleep(Duration::from_millis(20));
    };
    let own = owner.join().unwrap().unwrap();
    assert!(
        Instant::now() < opened_at + Duration::from_secs(1),
        "owner waiter lagged the approval open"
    );
    assert_eq!(own["reason"], "approval_pending", "{}", summary(&own));
    assert_eq!(own["message"], "m-appr");
    let live = own["turn"].as_str().expect("owner sees the live turn");
    assert!(!live.is_empty());
    let pm_out = pm.join().unwrap().unwrap();
    assert_eq!(pm_out["reason"], "approval_pending", "{}", summary(&pm_out));
    assert_eq!(pm_out["message"], "m-appr");
    assert!(
        pm_out.get("turn").is_none() || pm_out["turn"].is_null(),
        "PM must not see the live turn: {}",
        summary(&pm_out)
    );
    // The stranger never fired on the approval: its 3 s bound lapsed.
    let err = stranger
        .join()
        .unwrap()
        .expect_err("stranger must time out");
    assert_eq!(err.kind(), "busy", "{err}");
    // Cleanup: answer the approval so the turn completes.
    let handle = d
        .operator_rpc("agent_requests", json!({"alias": "w"}))
        .unwrap()["requests"][0]["request"]
        .as_str()
        .unwrap()
        .to_string();
    d.operator_rpc(
        "agent_respond",
        json!({"alias": "w", "request": handle, "decision": "accept"}),
    )
    .unwrap();
    wait_msg(&d, "w", "m-appr", &["completed", "failed"], 15);
}

/// Q5: a `--message` wait on a turn that ends without a report settles
/// instead of hanging to a retryable timeout on a terminal state.
#[test]
fn wait_message_settles_when_the_turn_ends_without_a_report() {
    let d = TestDaemon::start();
    d.register("w");
    d.wait_agent("w", "idle", 10);
    d.send("w", json!({"text": "SLEEP:5", "message": "m-hold"}))
        .unwrap();
    wait_msg(&d, "w", "m-hold", &["running"], 10);
    d.send(
        "w",
        json!({"text": "queued behind the hold", "message": "m-q"}),
    )
    .unwrap();
    let state = d.state.clone();
    let waiter = thread::spawn(move || {
        wait_rpc(
            state,
            json!({"alias": "w", "until": "reported", "timeout": 30, "message": "m-q"}),
        )
    });
    thread::sleep(Duration::from_millis(300));
    d.operator_rpc("message_cancel", json!({"message": "m-q"}))
        .unwrap();
    let out = waiter.join().unwrap().unwrap();
    assert_eq!(out["reason"], "settled", "{}", summary(&out));
    assert_eq!(out["message"], "m-q");
    // The held turn still runs out; leave the agent clean.
    wait_msg(&d, "w", "m-hold", &["completed", "failed"], 15);
}

#[test]
fn wait_refuses_message_combos_and_forged_messages() {
    let d = TestDaemon::start();
    d.register("w");
    d.register("peer");
    d.wait_agent("w", "idle", 10);
    d.send("w", json!({"text": "hi", "message": "m1"})).unwrap();
    d.wait_message("w", "m1", &["completed"], 15);
    // `--message` narrows reported|any only.
    for until in ["idle", "attention"] {
        let err = d
            .rpc(
                "agent_wait",
                json!({"alias": "w", "until": until, "timeout": 5, "message": "m1"}),
            )
            .expect_err("message with {until} must be refused");
        assert!(err.to_string().contains("--message narrows"), "{err}");
    }
    // Another agent's message is forged scope.
    let err = d
        .rpc(
            "agent_wait",
            json!({"alias": "peer", "until": "reported", "timeout": 5, "message": "m1"}),
        )
        .expect_err("foreign message must be refused");
    assert!(err.to_string().contains("belongs to"), "{err}");
    // So is a nonexistent one.
    let err = d
        .rpc(
            "agent_wait",
            json!({"alias": "w", "until": "reported", "timeout": 5, "message": "nope"}),
        )
        .expect_err("unknown message must be refused");
    assert!(err.to_string().contains("Unknown message"), "{err}");
    // Unknown `--until` and unknown alias are refused, not waited on.
    assert!(d
        .rpc(
            "agent_wait",
            json!({"alias": "w", "until": "soon", "timeout": 5}),
        )
        .is_err());
    assert!(d
        .rpc(
            "agent_wait",
            json!({"alias": "ghost", "until": "idle", "timeout": 5}),
        )
        .is_err());
}

#[test]
fn wait_timeout_zero_checks_once_without_blocking() {
    let d = TestDaemon::start();
    d.register("w");
    d.wait_agent("w", "idle", 10);
    let t0 = Instant::now();
    let out = d
        .rpc(
            "agent_wait",
            json!({"alias": "w", "until": "idle", "timeout": 0}),
        )
        .unwrap();
    assert_eq!(out["reason"], "idle");
    assert!(t0.elapsed() < Duration::from_secs(5));
    // Nothing to report: the single evaluation misses, no blocking.
    let t0 = Instant::now();
    let err = d
        .rpc(
            "agent_wait",
            json!({"alias": "w", "until": "reported", "timeout": 0}),
        )
        .expect_err("timeout 0 with nothing reported must miss");
    assert_eq!(err.kind(), "busy", "{err}");
    assert!(t0.elapsed() < Duration::from_secs(5));
}

/// Timeout is `busy`: the CLI exits 75, and success prints the one JSON
/// line with exit 0.
#[test]
fn wait_cli_timeout_exits_75_and_success_prints_one_line() {
    let d = TestDaemon::start();
    d.register("w");
    d.wait_agent("w", "idle", 10);
    let dir = d.dir.path().to_path_buf();
    let t0 = Instant::now();
    let (code, _, stderr) = cli_wait(
        &d.state,
        &dir,
        &[
            "agent",
            "wait",
            "w",
            "--until",
            "reported",
            "--timeout",
            "2",
        ],
    );
    assert_eq!(code, Some(75), "stderr: {stderr}");
    assert!(
        stderr.contains("\"busy\"") || stderr.contains("busy"),
        "stderr: {stderr}"
    );
    assert!(
        t0.elapsed() >= Duration::from_millis(1500),
        "returned without waiting"
    );
    let (code, stdout, _) = cli_wait(
        &d.state,
        &dir,
        &["agent", "wait", "w", "--until", "idle", "--timeout", "30"],
    );
    assert_eq!(code, Some(0));
    let out: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(out["alias"], "w");
    assert_eq!(out["reason"], "idle");
    assert!(out["waited_secs"].as_f64().is_some());
    // `--message` with `idle` is a usage-shape refusal at the daemon.
    let (code, _, _) = cli_wait(
        &d.state,
        &dir,
        &["agent", "wait", "w", "--until", "idle", "--message", "m1"],
    );
    assert_eq!(code, Some(3));
}

/// A client that disconnects mid-wait leaves the daemon healthy: no
/// wedged connection, later waits unaffected.
#[test]
fn wait_client_disconnect_leaves_the_daemon_healthy() {
    let d = TestDaemon::start();
    d.register("w");
    d.wait_agent("w", "idle", 10);
    let sock = client::socket_path(&d.state);
    let mut stream = UnixStream::connect(&sock).unwrap();
    let frame = json!({
        "method": "agent_wait",
        "params": {"alias": "w", "until": "reported", "timeout": 120},
    });
    use std::io::Write as _;
    writeln!(stream, "{frame}").unwrap();
    // Disconnect without reading: the waiter must release, not wedge.
    drop(stream);
    thread::sleep(Duration::from_secs(1));
    let out = d
        .rpc(
            "agent_wait",
            json!({"alias": "w", "until": "idle", "timeout": 5}),
        )
        .unwrap();
    assert_eq!(out["reason"], "idle");
    let show = d.rpc("agent_show", json!({"alias": "w"})).unwrap();
    assert_eq!(show["agent"]["state"], "idle");
}
