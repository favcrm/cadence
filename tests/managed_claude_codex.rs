//! managed_claude_codex: area tests split from tests/integration.rs (CAD-426).
//! End-to-end tests: real socket daemon in-process, fake provider.
//! These exercise the observable contract — queue order, idempotency,
//! restart fencing, approval brokering, serialization — without model calls.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use cadence_agent::client;
use serde_json::json;
use serde_json::Value;
use std::path::Path;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::TempDir;

/// Whether the calling test named `name` should run its body here. A
/// test whose subject is process-global — the real env, a signal to
/// the whole process — must not share a process with parallel tests:
/// the first call re-runs just `name` in a child of this binary with
/// `envs` in its env from birth, asserts it ran and passed, and returns
/// false; in that child it returns true.
fn in_own_process(name: &str, envs: &[(&str, &str)]) -> bool {
    const CHILD: &str = "CADENCE_TEST_OWN_PROCESS";
    if std::env::var(CHILD).ok().as_deref() == Some(name) {
        return true;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, name)
        .envs(envs.iter().copied())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains(" 1 passed"),
        "{name} failed in its own process:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    false
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
    d.operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
    d.wait_agent("w1", "stopped", 15);
    d.operator_rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap();
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
    d.register_pcp(
        "quota",
        "codex",
        "managed",
        &cwd,
        "{\"quota\":{\"state\":\"available\",\"used_percent\":100}}",
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

    d.send(
        "quota",
        json!({"text": "clear", "message": "quota-explicit-null"}),
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

    d.send("quota", json!({"text": "omit", "message": "quota-omitted"}))
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

    d.send(
        "recover",
        json!({"text": "refresh", "message": "quota-recover"}),
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
    d.register_pcp(
        "luna",
        "codex",
        "managed",
        &cwd,
        "{\"model\":\"gpt-5.6-luna\",\"effort\":\"max\"}",
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

    d.operator_rpc("agent_stop", json!({"alias": "luna"}))
        .unwrap();
    d.wait_agent("luna", "stopped", 15);
    d.operator_rpc("agent_resume", json!({"alias": "luna"}))
        .unwrap();
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
    d.register_pcp(
        "bad-luna",
        "codex",
        "managed",
        &cwd,
        "{\"model\":\"gpt-5.6-luna\",\"effort\":\"ultra\"}",
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
    d.register_pcp(
        "unknown-luna",
        "codex",
        "managed",
        &cwd,
        "{\"model\":\"gpt-5.6-luna\"}",
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
    d.register_pcp(
        "luna-ws",
        "codex",
        "managed-ws",
        &cwd,
        "{\"model\":\"gpt-5.6-luna\",\"effort\":\"max\"}",
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
    d.operator_rpc(
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
        .register_pcp(
            "w1",
            "codex",
            "managed",
            &cwd,
            "{\"approval_policy\":\"bogus\"}",
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
        .operator_rpc(
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
    d.operator_rpc(
        "agent_set",
        json!({"alias": "w1", "next_launch": true,
               "patch": {"approval_policy": "untrusted"}}),
    )
    .unwrap();
    d.operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
    d.wait_agent("w1", "stopped", 15);
    d.operator_rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap();
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
    d.operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
    d.wait_agent("w1", "stopped", 15);
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET params='{\"approval_policy\":\"bogus\"}' WHERE alias='w1'",
        [],
    )
    .unwrap();
    drop(conn);
    d.operator_rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap();
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
fn codex_sandbox_rejected_at_open() {
    // A sandbox hand-edited behind the daemon's back is refused before
    // the provider process launches — on resume (thread id kept) and on
    // a fresh open (thread id cleared) alike.
    let d = TestDaemon::start();
    let mock = d.mock_codex("ok");
    d.register_codex("w1");
    d.wait_agent("w1", "idle", 15);
    for thread in ["thread_id", "NULL"] {
        d.operator_rpc("agent_stop", json!({"alias": "w1"}))
            .unwrap();
        d.wait_agent("w1", "stopped", 15);
        wait_pid_gone(&mock.pidfile, 15);
        std::fs::remove_file(&mock.pidfile).unwrap();
        let sent = mock_requests(&mock).len();
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            &format!(
                "UPDATE agents SET sandbox='danger-full-access', thread_id={thread} \
                 WHERE alias='w1'"
            ),
            [],
        )
        .unwrap();
        drop(conn);
        d.operator_rpc("agent_resume", json!({"alias": "w1"}))
            .unwrap();
        let agent = d.wait_agent("w1", "attention", 15);
        let err = agent["error"].as_str().unwrap().to_string();
        assert!(err.contains("'danger-full-access'"), "{thread}: {err}");
        for accepted in ["read-only", "workspace-write"] {
            assert!(
                err.contains(accepted),
                "{thread}: open error missing '{accepted}': {err}"
            );
        }
        // Nothing launched: the mock writes its pidfile first thing, and
        // no second thread/start or thread/resume reached the wire.
        assert!(!mock.pidfile.exists(), "{thread}: provider was launched");
        assert_eq!(mock_requests(&mock).len(), sent, "{thread}");
        // Restore a valid row so the next round starts from idle.
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "UPDATE agents SET sandbox='read-only', state='stopped', error=NULL \
             WHERE alias='w1'",
            [],
        )
        .unwrap();
        drop(conn);
        d.operator_rpc("agent_resume", json!({"alias": "w1"}))
            .unwrap();
        d.wait_agent("w1", "idle", 15);
    }
}

#[test]
fn codex_sandbox_values_reach_thread_start_and_are_reported() {
    // Every allowed sandbox launches and rides `thread/start` verbatim;
    // `agent show` reports it beside the effective approval policy and
    // where that policy came from.
    let d = TestDaemon::start();
    let mock = d.mock_codex("ok");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    for (i, sandbox) in ["read-only", "workspace-write"].iter().enumerate() {
        let alias = format!("w{i}");
        d.operator_rpc(
            "agent_register",
            json!({"alias": alias, "provider": "codex",
                   "endpoint_kind": "managed", "cwd": cwd,
                   "sandbox": sandbox}),
        )
        .unwrap();
        d.wait_agent(&alias, "idle", 15);
        let reqs = mock_requests(&mock);
        assert_eq!(reqs.len(), i + 1, "{reqs:?}");
        assert_eq!(reqs[i]["method"], "thread/start");
        assert_eq!(reqs[i]["params"]["sandbox"], *sandbox);
        let agent = d.rpc("agent_show", json!({"alias": alias})).unwrap()["agent"].clone();
        assert_eq!(agent["sandbox"], *sandbox, "{agent}");
        assert_eq!(agent["approval_policy"], "never", "{agent}");
        assert_eq!(
            agent["approval_policy_source"], "cadence default",
            "{agent}"
        );
    }
    // A configured policy is reported as configured.
    d.register_pcp(
        "wc",
        "codex",
        "managed",
        &cwd,
        "{\"approval_policy\":\"on-request\"}",
    )
    .unwrap();
    d.wait_agent("wc", "idle", 15);
    let agent = d.rpc("agent_show", json!({"alias": "wc"})).unwrap()["agent"].clone();
    assert_eq!(agent["approval_policy"], "on-request", "{agent}");
    assert_eq!(agent["approval_policy_source"], "configured", "{agent}");
    // Providers without the setting report neither field.
    d.register_pc("f", "fake", "fake", &cwd).unwrap();
    let agent = d.rpc("agent_show", json!({"alias": "f"})).unwrap()["agent"].clone();
    assert!(agent["approval_policy"].is_null(), "{agent}");
    assert!(agent["approval_policy_source"].is_null(), "{agent}");
}

#[test]
fn codex_cli_approval_policy_flag_roundtrips_through_resume() {
    let d = TestDaemon::start();
    let mock = d.mock_codex_ws("ok");
    let pm_repo = d.dir.path().join("pmrepo");
    git_repo(&pm_repo);
    d.register_pc("pm", "fake", "fake", pm_repo.to_str().unwrap())
        .unwrap();
    d.wait_agent("pm", "idle", 10);
    let bin = env!("CARGO_BIN_EXE_cadence");
    let run = |args: &[&str]| {
        std::process::Command::new(bin)
            .arg("--state-dir")
            .arg(&d.state)
            .args(args)
            .operator_output()
            .unwrap()
    };
    let cwd = pm_repo.to_str().unwrap();
    let launches: [(&str, &[&str], &str); 2] = [
        (
            "wc",
            &["codex", "--alias", "wc", "--detach", "--cwd", cwd],
            "on-failure",
        ),
        (
            "wj",
            &["join", "pm", "codex", "--alias", "wj", "--detach"],
            "untrusted",
        ),
    ];
    for (i, (alias, args, policy)) in launches.iter().enumerate() {
        let mut argv = args.to_vec();
        argv.extend(["--approval-policy", policy]);
        let out = run(&argv);
        assert!(
            out.status.success(),
            "{alias}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        d.wait_agent(alias, "idle", 20);
        let agent = d.rpc("agent_show", json!({"alias": alias})).unwrap()["agent"].clone();
        assert_eq!(agent["params"]["approval_policy"], *policy, "{agent}");
        assert_eq!(agent["approval_policy"], *policy, "{agent}");
        assert_eq!(agent["approval_policy_source"], "configured", "{agent}");
        let reqs = mock_requests(&mock);
        assert_eq!(reqs[reqs.len() - 1]["method"], "thread/start", "{reqs:?}");
        assert_eq!(reqs[reqs.len() - 1]["params"]["approvalPolicy"], *policy);
        // Stop + resume replays the stored policy on thread/resume.
        d.operator_rpc("agent_stop", json!({"alias": alias}))
            .unwrap();
        d.wait_agent(alias, "stopped", 15);
        d.operator_rpc("agent_resume", json!({"alias": alias}))
            .unwrap();
        d.wait_agent(alias, "idle", 15);
        let reqs = mock_requests(&mock);
        assert_eq!(reqs.len(), 2 * (i + 1), "{reqs:?}");
        let last = reqs.last().unwrap();
        assert_eq!(last["method"], "thread/resume");
        assert_eq!(last["params"]["approvalPolicy"], *policy);
    }
    // A bogus value is a clap rejection naming every accepted value.
    for args in [
        &["codex", "--alias", "wb", "--detach", "--cwd", cwd][..],
        &["join", "pm", "codex", "--alias", "wb", "--detach"][..],
    ] {
        let mut argv = args.to_vec();
        argv.extend(["--approval-policy", "bogus"]);
        let out = run(&argv);
        assert!(!out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        for accepted in ["never", "on-request", "on-failure", "untrusted"] {
            assert!(err.contains(accepted), "missing '{accepted}': {err}");
        }
    }
    // The flag is codex-only: another provider refuses it instead of
    // silently dropping it.
    let out = run(&[
        "join",
        "pm",
        "fake",
        "--alias",
        "wf",
        "--detach",
        "--approval-policy",
        "never",
    ]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--approval-policy"), "{err}");
    assert!(
        d.rpc("agent_show", json!({"alias": "wf"})).is_err(),
        "a refused join must not register the worker"
    );
}

#[test]
fn transport_eof_fences_turn_quickly() {
    let d = TestDaemon::start();
    let _mock = d.mock_codex("die-after-start");
    d.register_codex("w1");
    d.wait_agent("w1", "idle", 15);
    let began = Instant::now();
    d.send("w1", json!({"text": "hi", "message": "m1"}))
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
    d.send("w1", json!({"text": "hi", "message": "m1"}))
        .unwrap();
    d.wait_message("w1", "m1", &["running"], 15);
    let began = Instant::now();
    let stopped = d
        .operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
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
    d.send("w1", json!({"text": "hi", "message": "m1"}))
        .unwrap();
    // Acknowledged but uncorrelatable: the provider may have started work,
    // so the attempt is fenced unknown — not a definitive failure.
    let m1 = d.wait_message("w1", "m1", &["unknown"], 20);
    assert!(m1["error"].as_str().unwrap().contains("no turn id"), "{m1}");
    d.wait_agent("w1", "attention", 10);
    d.send("w1", json!({"text": "later", "message": "m2"}))
        .unwrap();
    // CAD-184 kept sleep: absence window — no actor runs for a fenced or
    // stopped agent, so nothing records a refusal to poll for.
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
    d.send("w1", json!({"text": "DISCONNECT", "message": "m1"}))
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
    let stopped = d
        .operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
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
    let again = d
        .operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
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
    let fenced = d
        .operator_rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap_err();
    let fenced = fenced.to_string();
    assert!(fenced.contains("resume refused"), "{fenced}");
    assert!(!fenced.contains("then `cadence agent resume"), "{fenced}");
}

#[test]
fn concurrent_stops_are_idempotent() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    d.send("w1", json!({"text": "NEED_INPUT:x", "message": "m1"}))
        .unwrap();
    d.wait_agent("w1", "waiting_input", 10);
    let mut racers = Vec::new();
    for _ in 0..2 {
        let state = d.state.clone();
        racers.push(thread::spawn(move || {
            // CAD-482: agent_stop is operator-gated — assert the
            // operator identity in-band (a no-op scope on a build
            // without the feature, where the ambient caller answers).
            cadence_agent::test_seam::scoped(cadence_agent::test_seam::Asserted::Operator, || {
                client::rpc(&state, "agent_stop", json!({"alias": "w1"}))
            })
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
    d.operator_rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap();
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
    let stopped = d
        .operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
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
    d.send("w1", json!({"text": "DIE now", "message": "m1"}))
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
    d.send("w1", json!({"text": "hi", "message": "m1"}))
        .unwrap();
    d.wait_message("w1", "m1", &["running"], 15);
    let began = Instant::now();
    let stopped = d
        .operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
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
    d.send("w1", json!({"text": "NEED_INPUT:x", "message": "m1"}))
        .unwrap();
    d.wait_agent("w1", "waiting_input", 15);
    let requests = d
        .operator_rpc("agent_requests", json!({"alias": "w1"}))
        .unwrap();
    let handle = requests["requests"][0]["request"].as_str().unwrap();
    let answered = d
        .operator_rpc(
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
    // published and the 30s initialize RPC is in flight — the mock
    // writes `<pidfile>.init` when that request arrives.
    let init = mock.pidfile.with_extension("pid.init");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !init.exists() {
        assert!(
            Instant::now() < deadline,
            "initialize never reached the provider"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let began = Instant::now();
    let stopped = d
        .operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
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
        d.register_pc("w1", "codex", "managed-ws", &cwd).unwrap();
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
    let stopped = d
        .operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
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
    d.send("w1", json!({"text": "DIE2 now", "message": "m1"}))
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
    d.send("w1", json!({"text": "NEED_INPUT_EXT:x", "message": "m1"}))
        .unwrap();
    d.wait_agent("w1", "waiting_input", 15);
    let requests = d
        .operator_rpc("agent_requests", json!({"alias": "w1"}))
        .unwrap();
    let handle = requests["requests"][0]["request"]
        .as_str()
        .unwrap()
        .to_string();
    // Resolve it externally, as an attached TUI would.
    std::fs::write(format!("{}.resolve", mock.pidfile.display()), b"1").unwrap();
    d.wait_message("w1", "m1", &["completed"], 20);
    // The stale handle is rejected; the provider already resolved it.
    let late = d.operator_rpc(
        "agent_respond",
        json!({"alias": "w1", "request": handle, "decision": "accept"}),
    );
    let late = late.expect_err("late respond should be rejected");
    assert!(
        late.to_string().contains("no longer pending"),
        "late respond should be rejected: {late}"
    );
    let requests = d
        .operator_rpc("agent_requests", json!({"alias": "w1"}))
        .unwrap();
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
    d.send("w1", json!({"text": "NEED_INPUT:x", "message": "m1"}))
        .unwrap();
    d.wait_agent("w1", "waiting_input", 15);
    let requests = d
        .operator_rpc("agent_requests", json!({"alias": "w1"}))
        .unwrap();
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
                d.operator_rpc(
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
    d.send("w1", json!({"text": "NEED_INPUT2:x", "message": "m1"}))
        .unwrap();
    d.wait_agent("w1", "waiting_input", 15);
    let requests = d
        .operator_rpc("agent_requests", json!({"alias": "w1"}))
        .unwrap();
    let handles: Vec<String> = requests["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["request"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(handles.len(), 2, "{requests}");
    let answered = d
        .operator_rpc(
            "agent_respond",
            json!({"alias": "w1", "request": handles[0], "decision": "accept"}),
        )
        .unwrap();
    assert_eq!(answered["state"], "answered");
    // One request still pending: the agent must stay waiting_input.
    // CAD-184 kept sleep: absence window — nothing records that the
    // relaxation check ran and left waiting_input alone.
    thread::sleep(Duration::from_millis(300));
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(
        agent["agent"]["state"], "waiting_input",
        "relaxation clobbered the remaining request"
    );
    let answered = d
        .operator_rpc(
            "agent_respond",
            json!({"alias": "w1", "request": handles[1], "decision": "accept"}),
        )
        .unwrap();
    assert_eq!(answered["state"], "answered");
    d.wait_message("w1", "m1", &["completed"], 20);
    d.wait_agent("w1", "idle", 10);
}

/// CAD-162 acceptance 2/4 on managed Claude: `kind: "ack"` with the
/// adapter-minted `claude-<gen>-…` token is accepted mid-turn and keeps
/// the message running (never `awaiting_report` — a managed turn owes no
/// report); a reported `result` is refused because the adapter's turn
/// result is the one writer of the outcome; that later turn result
/// completes the message.
#[test]
fn claude_managed_ack_keeps_running_until_the_turn_result() {
    let d = TestDaemon::start();
    let mock = d.mock_claude("hold", None);
    d.register_claude("w1", Value::Null);
    let agent = d.wait_agent("w1", "idle", 15);
    let gen = agent["generation"].as_str().unwrap().to_string();
    assert!(!gen.is_empty(), "{agent}");
    d.send("w1", json!({"text": "long task", "message": "m1"}))
        .unwrap();
    d.wait_message("w1", "m1", &["running"], 20);
    let token = running_token(&d, "m1");
    assert!(token.starts_with(&format!("claude-{gen}-")), "{token}");

    d.report("m1", &token, "ack", "understood").unwrap();
    let m = cad162_message(&d, "w1", "m1");
    assert_eq!(m["state"], "running", "{m}");
    assert_eq!(m["result"]["ack"]["text"], "understood", "{m}");
    assert_eq!(m["result"]["status"], "acknowledged", "{m}");
    assert!(
        m["awaiting_report"].is_null(),
        "managed ack owes no report: {m}"
    );
    assert!(
        event_kinds(&d, "w1").iter().any(|k| k == "acknowledged"),
        "ack must be recorded"
    );

    let err = d
        .report("m1", &token, "result", "done?")
        .expect_err("a reported result must not race the managed turn result");
    assert!(err.to_string().contains("report `ack` only"), "{err}");
    assert_eq!(d.message_state("w1", "m1"), "running");

    std::fs::write(mock.pidfile.with_extension("pid.release"), "").unwrap();
    let m = d.wait_message("w1", "m1", &["completed"], 20);
    assert_eq!(m["result"]["text"], "MOCK_OK:long task", "{m}");
    assert_eq!(m["result"]["turn_id"], token, "{m}");
}

/// CAD-162 acceptance 2/3 on managed Claude: the token is refused once
/// the endpoint generation moves on, and a pty-shaped token carrying the
/// LIVE generation is refused — never accepted as current.
#[test]
fn claude_managed_report_refuses_stale_generation_and_pty_token() {
    let d = TestDaemon::start();
    let mock = d.mock_claude("hold", None);
    d.register_claude("w1", Value::Null);
    let agent = d.wait_agent("w1", "idle", 15);
    let gen = agent["generation"].as_str().unwrap().to_string();
    d.send("w1", json!({"text": "task", "message": "m1"}))
        .unwrap();
    d.wait_message("w1", "m1", &["running"], 20);
    let token = running_token(&d, "m1");

    cad162_sql(
        &d,
        "UPDATE agents SET generation=?1 WHERE alias='w1'",
        &[CAD162_OTHER_GEN],
    );
    cad162_assert_refused(&d, "w1", "m1", &token, "earlier generation");
    cad162_sql(
        &d,
        "UPDATE agents SET generation=?1 WHERE alias='w1'",
        &[&gen],
    );

    let pty = format!("pty-{gen}-{}", "c".repeat(32));
    cad162_set_turn(&d, "m1", &pty);
    cad162_assert_refused(&d, "w1", "m1", &pty, "pty token on managed claude");

    cad162_set_turn(&d, "m1", &token);
    std::fs::write(mock.pidfile.with_extension("pid.release"), "").unwrap();
    d.wait_message("w1", "m1", &["completed"], 20);
}

/// CAD-162 fail closed: endpoint kinds whose tokens carry no generation
/// cadence can check refuse every report — managed and managed-ws Codex
/// (provider turn ids, no generation), the fake test double and the
/// mailbox. Each is tried with its real/natural token AND with pty- and
/// claude-shaped tokens forged under a generation planted on the row.
#[test]
fn report_refused_on_endpoint_kinds_without_a_token_scheme() {
    let d = TestDaemon::start();
    let _codex = d.mock_codex("silent");
    let _ws = d.mock_codex_ws("silent");
    d.register_codex("cx1");
    d.register_codex_ws("cw1");
    register_fake_opts(&d, "fk1", json!({}));
    d.register_inbox("ib1");
    for alias in ["cx1", "cw1", "fk1"] {
        d.wait_agent(alias, "idle", 20);
    }
    // Codex holds a real provider turn ("t-1") open.
    for alias in ["cx1", "cw1"] {
        d.send(
            alias,
            json!({"text": "hold", "message": format!("m-{alias}")}),
        )
        .unwrap();
        d.wait_message(alias, &format!("m-{alias}"), &["running"], 20);
        let real = running_token(&d, &format!("m-{alias}"));
        cad162_assert_refused(
            &d,
            alias,
            &format!("m-{alias}"),
            &real,
            "codex's own turn id",
        );
    }
    // The fake and the mailbox get a planted running row.
    let now = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
    );
    for alias in ["fk1", "ib1"] {
        let id = format!("m-{alias}");
        cad162_sql(
            &d,
            "INSERT INTO messages(id,alias,body,reply_to,source,state,turn_id,created)
             VALUES(?1,?2,'task',NULL,'test','running','fake-turn-1',CAST(?3 AS REAL))",
            &[&id, alias, &now],
        );
        cad162_assert_refused(&d, alias, &id, "fake-turn-1", "natural token");
    }
    for alias in ["cx1", "cw1", "fk1", "ib1"] {
        let id = format!("m-{alias}");
        cad162_sql(
            &d,
            "UPDATE agents SET generation=?1 WHERE alias=?2",
            &[CAD162_OTHER_GEN, alias],
        );
        for token in [
            format!("pty-{CAD162_OTHER_GEN}-n"),
            format!("claude-{CAD162_OTHER_GEN}-n"),
            format!("codex-{CAD162_OTHER_GEN}-n"),
            id.clone(),
        ] {
            cad162_set_turn(&d, &id, &token);
            cad162_assert_refused(&d, alias, &id, &token, &format!("{alias} forged"));
        }
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
    d.send(
        "w1",
        json!({"text": "Reply with exactly: PONG", "message": "m1"}),
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
    d.operator_rpc("agent_ready", json!({"alias": "pm"}))
        .unwrap();
    d.send("w1", json!({"text": "hi", "message": "m1"}))
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
    d.send("w1", json!({"text": "hi", "message": "m1"}))
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
    d.send("w1", json!({"text": "hi", "message": "m1"}))
        .unwrap();
    // EOF before any result event — outcome unknowable, fail closed.
    d.wait_message("w1", "m1", &["unknown"], 20);
    d.wait_agent("w1", "attention", 10);
    // The same mock command relaunches; flip it to "ok" for the resume.
    std::fs::write(mock.pidfile.with_extension("pid.mode"), "ok").unwrap();
    let unfenced = d
        .operator_rpc(
            "agent_unfence",
            json!({"alias": "w1", "status": "interrupted"}),
        )
        .unwrap();
    // Reconcile leaves the agent stopped; resume is the explicit step.
    assert_eq!(unfenced["state"], "stopped", "{unfenced}");
    d.operator_rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap();
    d.wait_agent("w1", "idle", 15);
    // `idle` only proves the transport opened — under load the
    // relaunched mock can still be booting, so `.argv` may still hold
    // the first launch's flags. A completed turn is the cause ordered
    // after the dump: init/result emits mean the script exec'd.
    d.send("w1", json!({"text": "again", "message": "m2"}))
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
    d.send("w1", json!({"text": "hi", "message": "m1"}))
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
    d.send("w1", json!({"text": "hi", "message": "m1"}))
        .unwrap();
    d.wait_message("w1", "m1", &["running"], 15);
    // Stop interrupts first: the mock emits an interrupted result, so
    // the message lands `interrupted` — never fenced unknown.
    let stopped = d
        .operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
    assert_eq!(stopped["state"], "stopped");
    d.wait_message("w1", "m1", &["interrupted"], 10);
}

#[test]
fn claude_denials_complete_with_event() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude("deny", None);
    d.register_claude("w1", Value::Null);
    d.wait_agent("w1", "idle", 15);
    d.send("w1", json!({"text": "hi", "message": "m1"}))
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
    // The scrub works on the daemon's real process env, which every
    // test in this binary shares — so the leaks are planted in a child
    // process of their own instead of mutated here. Scrub by rule:
    // every CLAUDE_*/CLAUDECODE/CODEX_*/CADENCE_* name a parent session
    // (or a test override) could leak is removed — except the
    // documented keep-list. ANTHROPIC_* auth is never touched.
    if !in_own_process(
        "claude_env_injected_and_scrubbed",
        &[
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
        ],
    ) {
        return;
    }
    let d = TestDaemon::start();
    let mock = d.mock_claude("ok", None);
    d.register_claude("w1", Value::Null);
    d.wait_agent("w1", "idle", 15);
    // The mock writes its env dump at process start, before any
    // protocol emit — `idle` only means the actor's transport opened.
    // A completed turn is the cause ordered after the dump.
    d.send("w1", json!({"text": "boot", "message": "m-env"}))
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
    d.send("w1", json!({"text": "boot", "message": "m-boot"}))
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
    d.operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
    d.operator_rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap();
    d.wait_agent("w1", "idle", 15);
    // Same spawn/write gap on the resumed generation — and the file
    // still holds the first launch's argv until the resumed mock
    // rewrites it. Another completed turn orders after the rewrite.
    d.send("w1", json!({"text": "boot2", "message": "m-boot2"}))
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
    d.send("w1", json!({"text": "boot", "message": "m-boot"}))
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
        .operator_rpc(
            "agent_set",
            json!({"alias": "w1", "patch": {"model": "opus"}}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("not live-settable"), "{err}");
    // With it (through the CLI): stored, the live process untouched.
    let pid = agent["pid"].clone();
    let (ok, stdout, stderr) = d.operator_cadence(&[
        "agent",
        "set",
        "w1",
        "model=opus",
        "effort=high",
        "--next-launch",
    ]);
    assert!(ok, "{stderr}");
    let reply: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(reply["applies"], "next launch", "{reply}");
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(agent["pid"], pid, "live process must not change: {agent}");
    assert_eq!(agent["state"], "idle", "{agent}");
    assert_eq!(agent["model_configured"], "opus", "{agent}");
    assert_eq!(agent["model_source"], "configured", "{agent}");
    assert_eq!(agent["effort"], "high", "{agent}");
    assert_eq!(std::fs::read_to_string(&argv_file).unwrap(), argv1);

    // stop + resume picks both up; a turn orders after the rewrite.
    d.operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
    d.operator_rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap();
    d.wait_agent("w1", "idle", 15);
    d.send("w1", json!({"text": "boot2", "message": "m-boot2"}))
        .unwrap();
    d.wait_message("w1", "m-boot2", &["completed"], 20);
    let argv2 = std::fs::read_to_string(&argv_file).unwrap();
    assert!(argv2.contains("--resume\n"), "{argv2}");
    assert!(argv2.contains("--model\nopus"), "{argv2}");
    assert!(argv2.contains("--effort\nhigh"), "{argv2}");

    // A bare key clears back to the provider default for the next open.
    d.operator_rpc(
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
        .register_pcp(
            "bad",
            "claude",
            "managed",
            &cwd,
            &json!({"effort": "extreme"}).to_string(),
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
            .operator_rpc(
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
    d.send("w1", json!({"text": "hi", "message": "m1"}))
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
    d.send("w1", json!({"text": "hi", "message": "m1"}))
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
        .operator_rpc(
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
    d.send("w1", json!({"text": "hi", "message": "m1"}))
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
    d.send("w1", json!({"text": "hi", "message": "m1"}))
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
    d.send("w1", json!({"text": "hi", "message": "m1"}))
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
    d.send("w1", json!({"text": "hi", "message": "m1"}))
        .unwrap();
    let m1 = d.wait_message("w1", "m1", &["unknown"], 30);
    assert!(
        m1["error"].as_str().unwrap_or("").contains("turn_max_secs"),
        "{m1}"
    );
    d.wait_agent("w1", "attention", 15);
}

/// CAD-227: codex turn liveness is activity-based like claude's — a
/// turn streaming past the idle window completes; a silent one fences.
#[test]
fn codex_idle_window_counts_activity() {
    let d = TestDaemon::start();
    let _mock = d.mock_codex("heartbeat");
    d.register_codex_params("cx1", "managed", json!({"turn_idle_secs": 2}));
    d.wait_agent("cx1", "idle", 15);
    d.send("cx1", json!({"text": "hi", "message": "m1"}))
        .unwrap();
    let m1 = d.wait_message("cx1", "m1", &["completed"], 30);
    assert!(
        m1["result"]["text"]
            .as_str()
            .unwrap_or("")
            .contains("MOCK_OK"),
        "{m1}"
    );
}

#[test]
fn codex_silent_turn_fences_after_idle_window() {
    let d = TestDaemon::start();
    let _mock = d.mock_codex("silent");
    d.register_codex_params("cx1", "managed", json!({"turn_idle_secs": 2}));
    d.wait_agent("cx1", "idle", 15);
    d.send("cx1", json!({"text": "hi", "message": "m1"}))
        .unwrap();
    let m1 = d.wait_message("cx1", "m1", &["unknown"], 30);
    assert!(
        m1["error"]
            .as_str()
            .unwrap_or("")
            .contains("No provider event for 2s"),
        "{m1}"
    );
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
    d.send("w1", json!({"text": "run ls", "message": "m1"}))
        .unwrap();
    // The prompt surfaces as a request holding the agent in
    // waiting_input — the cause orders after the launch argv write.
    d.wait_agent("w1", "waiting_input", 15);
    let req = d.wait_request("w1", 15);
    let handle = req["request"].as_str().unwrap().to_string();
    assert_eq!(req["method"], "cadence/approval", "{req}");
    assert_eq!(req["params"]["tool"], "Bash", "{req}");
    assert_eq!(req["params"]["input"]["command"], "run ls", "{req}");
    // The request came from w1's own permission server (a child of
    // its enrolled provider root); a connection without w1's identity
    // re-opening the same handle is refused (CAD-376) — still one
    // request. The owner's retry dedupe is
    // `brokered_request_handles_belong_to_their_agent`.
    let err = d
        .rpc(
            "request_open",
            json!({"alias": "w1", "kind": "approval", "tool": "Bash",
                   "request": handle, "input_summary": "run ls",
                   "input": {"command": "run ls"}}),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("CAD-376"), "{err}");
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
    d.operator_rpc(
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
    // A peer pane proves the unscoped `agent_events` read (Rule::Read).
    let home = TempDir::new().unwrap();
    let mut peer = LaneShell::spawn(home.path());
    plant_pane(&d, "w2", peer.pid());
    d.register_claude("w1", json!({"broker_approvals": true}));
    d.wait_agent("w1", "idle", 15);
    d.send("w1", json!({"text": "rm -rf /", "message": "m1"}))
        .unwrap();
    let req = d.wait_request("w1", 15);
    d.wait_agent("w1", "waiting_input", 15);
    // The operator's reason reaches the provider as the denial message.
    d.operator_rpc(
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
    // The denial lands on the standard permission_denied event — on the
    // unscoped lane (CAD-542) each denial keeps only the routing fields
    // and the same redacted one-line summary a tool_use carries: the
    // real CLI's verbatim `tool_input` (command + description — the
    // declined input is the most dangerous subset to re-publish) and
    // the operator's reason never cross.
    let denied_shape = |ev: &Value, who: &str| {
        let denial = &ev["payload"]["denials"][0];
        assert_eq!(denial["tool_name"], "Bash", "{who}: {ev}");
        assert_eq!(denial["tool_use_id"], "tu_permit", "{who}: {ev}");
        assert_eq!(denial["summary"], "Bash: rm -rf /", "{who}: {ev}");
        for field in ["tool_input", "message", "description"] {
            assert!(
                denial.get(field).is_none(),
                "{who}: denial carries {field}: {ev}"
            );
        }
    };
    denied_shape(&d.wait_event("w1", "permission_denied", 10), "operator");
    // A peer agent and a caller nothing proves read the same lane; the
    // canary the mock packs inside `tool_input.description` never
    // reaches either. (The turn's own result text is on the lane by
    // design — `turn_finished` carries it; the reason an operator gave
    // rides the verdict to the provider, not a denial field.)
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
            !text.contains("CANARY-DENIED-INPUT-4e7f"),
            "{who}: denied input on the event lane: {text}"
        );
        denied_shape(
            frame["result"]["events"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["kind"] == "permission_denied")
                .unwrap_or_else(|| panic!("{who}: no permission_denied in {frame}")),
            who,
        );
    }
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
    d.send("w1", json!({"text": "slow", "message": "m1"}))
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
    let state = d.state.clone();
    let mut mcp = Mcp::spawn(&state, "w1", 120);
    // Only w1's own connection opens and awaits its requests (CAD-376):
    // plant w1 as a brokered pane rooted at the server process itself,
    // standing in for the provider the real server is a child of. The
    // planted row survives the restart below.
    plant_pane(&d, "w1", mcp.child.id());
    cad162_sql(
        &d,
        "UPDATE agents SET params=?1 WHERE alias='w1'",
        &[&json!({"broker_approvals": true}).to_string()],
    );
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
    d.operator_rpc("shutdown", json!({})).unwrap();
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
    d.send("w1", json!({"text": "boot", "message": "m-boot"}))
        .unwrap();
    d.wait_agent("w1", "waiting_input", 15);
    // Answer it so the launch turn completes — one request only.
    let req = d.wait_request("w1", 10);
    d.operator_rpc(
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
    d.operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
    d.operator_rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap();
    d.wait_agent("w1", "idle", 15);
    // Resume replays the broker wiring verbatim — config regenerated,
    // same flags, resumed session.
    d.send("w1", json!({"text": "boot2", "message": "m-boot2"}))
        .unwrap();
    d.wait_agent("w1", "waiting_input", 15);
    let req2 = d.wait_request("w1", 10);
    d.operator_rpc(
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
    // request_open refuses a non-brokered agent — asked from that
    // agent's own connection, so the caller rule (CAD-376) passes.
    let lane_home = TempDir::new().unwrap();
    let mut own = LaneShell::spawn(lane_home.path());
    plant_pane(&d, "wn", own.pid());
    let r = own.rpc(
        &d.state,
        "request_open",
        json!({"alias": "wn", "kind": "approval", "tool": "Bash"}),
    );
    assert_refused(&r, "", "--broker-approvals", "non-brokered agent");
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
            .register_pcp(
                "bad",
                "claude",
                "managed",
                d.dir.path().to_str().unwrap(),
                &params.to_string(),
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

/// `cadence <args>` from the operator's own shell ([`OperatorOutput`]).
fn cadence_bin_operator(state: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .operator_output()
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
    d.operator_rpc("model_defaults_set", json!({"document": doc_a}))
        .unwrap();

    let conflict = cadence_bin_operator(
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
    let unsupported = cadence_bin_operator(
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

    d.operator_rpc(
        "agent_register",
        json!({"alias": "pm", "provider": "fake", "endpoint_kind": "fake", "cwd": cwd, "role": "pm"}),
    )
    .unwrap();
    let joined = cadence_bin_operator(
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
    d.send("qa-cli", json!({"text": "boot", "message": "m-qa"}))
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

    let explicit = cadence_bin_operator(
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
    d.send("explicit", json!({"text": "boot", "message": "m-ex"}))
        .unwrap();
    d.wait_message("explicit", "m-ex", &["completed"], 20);
    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(argv.contains("--model\nexplicit-model"), "{argv}");
    let shown = d.rpc("agent_show", json!({"alias": "explicit"})).unwrap();
    assert_eq!(shown["agent"]["model_selection"]["source"], "explicit");
    assert!(shown["agent"]["model_selection"]["revision"].is_null());

    let doc_b = r#"{"expected_revision":1,"config":{"schema":1,"providers":{"claude":{"default":{"mode":"model","model":"baseline-b"},"roles":{}}}}}"#;
    d.operator_rpc("model_defaults_set", json!({"document": doc_b}))
        .unwrap();
    d.operator_rpc("agent_stop", json!({"alias": "qa-cli"}))
        .unwrap();
    d.operator_rpc("agent_resume", json!({"alias": "qa-cli"}))
        .unwrap();
    d.wait_agent("qa-cli", "idle", 20);
    d.send("qa-cli", json!({"text": "again", "message": "m-resume"}))
        .unwrap();
    d.wait_message("qa-cli", "m-resume", &["completed"], 20);
    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(argv.contains("--model\nqa-model"), "{argv}");
    assert!(!argv.contains("baseline-b"), "{argv}");

    d.operator_rpc(
        "agent_register",
        json!({"alias": "fresh", "provider": "claude", "endpoint_kind": "managed", "cwd": cwd, "role": "worker"}),
    )
    .unwrap();
    d.wait_agent("fresh", "idle", 20);
    d.send("fresh", json!({"text": "boot", "message": "m-fresh"}))
        .unwrap();
    d.wait_message("fresh", "m-fresh", &["completed"], 20);
    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(argv.contains("--model\nbaseline-b"), "{argv}");

    d.operator_rpc(
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
    d.operator_rpc("agent_stop", json!({"alias": "fresh"}))
        .unwrap();
    d.operator_rpc("agent_resume", json!({"alias": "fresh"}))
        .unwrap();
    d.wait_agent("fresh", "idle", 20);
    d.send("fresh", json!({"text": "native", "message": "m-native"}))
        .unwrap();
    d.wait_message("fresh", "m-native", &["completed"], 20);
    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(!argv.contains("--model"), "{argv}");

    d.operator_rpc(
        "agent_register",
        json!({"alias": "box", "provider": "inbox", "endpoint_kind": "inbox", "team_role": "ops"}),
    )
    .unwrap();
    let inbox = d.rpc("agent_show", json!({"alias": "box"})).unwrap()["agent"].clone();
    assert!(inbox["model_selection"].is_null());
    assert!(inbox["model_configured"].is_null());
    assert_eq!(inbox["team_role"], "devops");
    assert_eq!(inbox["role"], "worker");

    let again = d.operator_rpc(
        "agent_register",
        json!({"alias": "qa-cli", "provider": "claude", "endpoint_kind": "managed", "cwd": cwd, "team_role": "dev"}),
    );
    assert!(again.unwrap_err().to_string().contains("UNIQUE"));
    assert_eq!(
        d.rpc("agent_show", json!({"alias": "qa-cli"})).unwrap()["agent"]["params"]["model"],
        "qa-model"
    );
}

/// CAD-337: model defaults decide the model every agent launches with,
/// so `model_defaults_set` is operator authority on POSITIVE proof, the
/// same gate as `slot_reconcile` and approval evidence. A pane and a
/// managed agent's own process are refused naming the rule; the audit
/// attribution is the verified connection's, and a request that tries
/// to supply one is refused rather than read. Nothing refused lands.
#[test]
fn model_defaults_set_is_operator_only() {
    let d = TestDaemon::start_opts(slot_opts(2, 1, 900, &[]));
    let home = TempDir::new().unwrap();
    let mut pane = LaneShell::spawn(home.path());
    plant_pane(&d, "pane-1", pane.pid());
    let mut wk = ManagedWorker::start(&d, "wk");
    let doc = r#"{"expected_revision":0,"config":{"schema":1,"providers":{"claude":{"default":{"mode":"model","model":"forged-model"},"roles":{}}}}}"#;
    let params = json!({"document": doc});
    let refused = |r: &Value, route: &str, who: &str| {
        assert_eq!(r["ok"], false, "{route}: {r}");
        let msg = r["error"]["message"].as_str().unwrap_or_default();
        assert!(
            msg.contains("model defaults set is an operator action") && msg.contains(who),
            "{route}: {r}"
        );
    };

    let r = pane.rpc(&d.state, "model_defaults_set", params.clone());
    refused(&r, "pane", "pane-1");
    let r = wk.rpc("self", "model_defaults_set", params.clone());
    refused(&r, "managed agent", "wk");
    // Deriving no agent identity is not operator proof: a detach off
    // the managed tool still carries its alias.
    let r = wk.rpc("detached", "model_defaults_set", params.clone());
    refused(&r, "managed detach", "not provably the operator");

    // Attribution is connection-bound: even the operator cannot name
    // who made the change.
    for field in ["attribution", "by", "actor"] {
        let mut forged = params.clone();
        forged[field] = json!("somebody-else");
        let err = d.operator_rpc("model_defaults_set", forged).unwrap_err();
        assert!(
            err.to_string().contains(&format!("'{field}'")),
            "{field}: {err}"
        );
    }
    let snap = d.rpc("model_defaults_get", json!({})).unwrap();
    assert_eq!(
        snap["revision"], 0,
        "no refused caller changed defaults: {snap}"
    );

    let snap = d.operator_rpc("model_defaults_set", params).unwrap();
    assert_eq!(snap["revision"], 1, "{snap}");
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    let payloads: Vec<String> = conn
        .prepare("SELECT payload FROM events WHERE kind='model_defaults_updated'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(payloads.len(), 1, "{payloads:?}");
    let audit: Value = serde_json::from_str(&payloads[0]).unwrap();
    assert_eq!(audit["attribution"], "operator", "{audit}");
    assert_eq!(audit["transport"], "operator-connection", "{audit}");
}

/// CAD-376: a brokered request handle belongs to its agent's own
/// permission server. Another agent (even a group peer) and a
/// connection with no agent identity can neither open a request on a
/// brokered agent — nothing is parked, no `waiting_input` — nor close
/// or await one, which would retire or consume it as a denial; the
/// handle stays pending and the owner still opens, awaits and closes
/// normally.
#[test]
fn brokered_request_handles_belong_to_their_agent() {
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
    let state = |alias: &str| {
        d.rpc("agent_show", json!({"alias": alias})).unwrap()["agent"]["state"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let opened = |alias: &str| {
        d.events(alias)
            .iter()
            .filter(|e| e["kind"] == "request_opened")
            .count()
    };
    let open = |handle: &str| {
        json!({"alias": "wr", "kind": "approval", "tool": "Bash",
               "input_summary": "rm -rf /tmp/x", "request": handle})
    };

    // A fake open on the brokered agent: refused from a peer's pane,
    // with forged identity fields, and from a connection that is no
    // agent. The target stays busy with nothing pending.
    let r = peer.rpc(&d.state, "request_open", open("h1"));
    assert_refused(&r, "request_open", "cannot act on 'wr''s", "peer open");
    for (field, value) in [("by", "wr"), ("pane", "wr"), ("actor", "wr")] {
        let r = peer.rpc(&d.state, "request_open", forged(&open("h1"), field, value));
        assert_refused(&r, "request_open", "connection-bound", field);
    }
    let err = d.rpc("request_open", open("h1")).unwrap_err().to_string();
    assert!(
        err.contains("request_open refused") && err.contains("no agent identity"),
        "{err}"
    );
    assert!(d.requests("wr").is_empty());
    assert_eq!(state("wr"), "busy");
    assert_eq!(opened("wr"), 0);

    // The owner's own connection opens it and parks itself; its retry
    // of the same handle dedupes.
    let r = owner.rpc(&d.state, "request_open", open("h1"));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(state("wr"), "waiting_input");
    let r = owner.rpc(&d.state, "request_open", open("h1"));
    assert_eq!(r["result"]["existing"], true, "{r}");
    assert_eq!(d.requests("wr").len(), 1);
    assert_eq!(opened("wr"), 1);

    // A peer's close or wait is refused and the handle stays pending;
    // so is one from a connection that is no agent.
    let h1 = json!({"request": "h1"});
    let r = peer.rpc(&d.state, "request_close", h1.clone());
    assert_refused(&r, "request_close", "cannot act on 'wr''s", "peer close");
    let r = peer.rpc(
        &d.state,
        "request_wait",
        json!({"request": "h1", "wait": 1}),
    );
    assert_refused(&r, "request_wait", "cannot act on 'wr''s", "peer wait");
    let err = d.rpc("request_close", h1.clone()).unwrap_err().to_string();
    assert!(err.contains("request_close refused"), "{err}");
    assert_eq!(d.requests("wr")[0]["request"], "h1");
    assert_eq!(state("wr"), "waiting_input");
    let closed = d.events("wr").iter().any(|e| e["kind"] == "request_closed");
    assert!(!closed, "a refused close must not retire the handle");

    // Once the PM answers, the parked answer is the owner's alone: a
    // peer can neither take it by waiting nor discard it by closing.
    let r = pm.rpc(
        &d.state,
        "agent_respond",
        json!({"alias": "wr", "request": "h1", "decision": "accept"}),
    );
    assert_eq!(r["ok"], true, "{r}");
    let r = peer.rpc(
        &d.state,
        "request_wait",
        json!({"request": "h1", "wait": 1}),
    );
    assert_refused(
        &r,
        "request_wait",
        "cannot act on 'wr''s",
        "peer takes answer",
    );
    let r = peer.rpc(&d.state, "request_close", h1.clone());
    assert_refused(
        &r,
        "request_close",
        "cannot act on 'wr''s",
        "peer drops answer",
    );
    let r = owner.rpc(
        &d.state,
        "request_wait",
        json!({"request": "h1", "wait": 1}),
    );
    assert_eq!(r["result"]["state"], "answered", "{r}");
    assert_eq!(r["result"]["answer"]["decision"], "accept", "{r}");

    // The owner retires its own abandoned request normally.
    let r = owner.rpc(&d.state, "request_open", open("h2"));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(state("wr"), "waiting_input");
    let r = owner.rpc(&d.state, "request_close", json!({"request": "h2"}));
    assert_eq!(r["result"]["state"], "closed", "{r}");
    assert!(d.requests("wr").is_empty());
    assert_eq!(state("wr"), "busy");
    d.wait_event("wr", "request_closed", 5);
}
