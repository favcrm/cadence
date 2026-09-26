//! CAD-544 coverage for Pi as a managed WORKER provider — `cadence
//! join <pm> pi` launches a headless `pi --mode rpc` process that keeps
//! a persistent session file under the state dir, gets the dev toolset
//! (`--tools read,bash,edit,write,grep,find,ls` + `--no-extensions`),
//! and runs on the same cleared-then-allowlisted environment posture as
//! the master (plus the dev-tool essentials). Driven adapter-level
//! against `tests/e2e/fake-pi.py` (the fake writes a per-alias launch
//! record under `<state>/agents/pi-record-<alias>.json`) and
//! daemon-level for the join → dispatch → PM route.

#![allow(clippy::disallowed_methods)]

mod common;

use std::path::Path;
use std::time::Duration;

use cadence_agent::adapter::pi::PiAdapter;
use cadence_agent::adapter::{AdapterHooks, ProviderAdapter, ProviderEnv};
use cadence_agent::store::Agent;
use common::*;
use serde_json::{json, Value};

fn fake_pi(mode: &str) -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/fake-pi.py");
    format!("python3 {} {mode}", script.display())
}

fn worker(alias: &str, cwd: &Path, params: Value) -> Agent {
    Agent {
        alias: alias.into(),
        provider: "pi".into(),
        endpoint_kind: "managed".into(),
        role: "worker".into(),
        team_role: None,
        cwd: cwd.to_string_lossy().into(),
        sandbox: "read-only".into(),
        instructions: None,
        thread_id: None,
        session_id: None,
        model: None,
        effort: None,
        pid: None,
        pid_start: None,
        endpoint: None,
        params: Some(params),
        model_selection: None,
        quota: None,
        generation: None,
        state: "starting".into(),
        enabled: true,
        error: None,
        created: 0.0,
        updated: 0.0,
    }
}

/// An adapter whose state dir is `state` — the log path's grandparent
/// is what `PiAdapter` derives `state_dir` from, matching the daemon's
/// `state/agents/<alias>.provider.log` layout.
fn adapter(mode: &str, state: &Path, own: &[(&str, String)]) -> PiAdapter {
    let env = ProviderEnv::default();
    env.set("CADENCE_PI_COMMAND", fake_pi(mode));
    for (k, v) in own {
        env.set(k, v.clone());
    }
    std::fs::create_dir_all(state.join("agents")).unwrap();
    let hooks = AdapterHooks {
        on_event: Box::new(|_, _| {}),
        on_request: Box::new(|_| {}),
    };
    PiAdapter::new(hooks, &state.join("agents").join("w.provider.log"), &env)
}

/// The fake-pi launch record for `alias` — argv tail + env NAMES.
fn record(state: &Path, alias: &str) -> Value {
    let path = state.join("agents").join(format!("pi-record-{alias}.json"));
    serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("no launch record at {}: {e}", path.display())),
    )
    .unwrap()
}

#[test]
fn worker_argv_carries_a_session_file_and_the_dev_tools() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    let ad = adapter("normal", state, &[]);
    let agent = worker("w1", dir.path(), json!({"model": "acme/demo-1"}));
    let identity = ad.open(&agent).unwrap();
    ad.close();

    let rec = record(state, "w1");
    let argv: Vec<String> = serde_json::from_value(rec["argv"].clone()).unwrap();
    // A worker's context lives in its own session file — the master's
    // `--no-session`/continuity-pack posture does not apply here.
    let session_arg = argv
        .windows(2)
        .find(|w| w[0] == "--session")
        .map(|w| w[1].clone());
    assert_eq!(
        session_arg.as_deref(),
        Some(
            state
                .join("agents")
                .join("w1")
                .join("session.jsonl")
                .to_string_lossy()
                .as_ref()
        ),
        "{argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "--no-session"),
        "worker must keep its session: {argv:?}"
    );
    // The master's confinement wrapper and cadence-only guard are
    // master-only; a worker gets the plain `pi --mode rpc` invocation.
    assert!(!argv.iter().any(|a| a == "--no-context-files"), "{argv:?}");
    assert!(
        !argv.iter().any(|a| a.contains("pi-guard")),
        "worker must not carry the master's guard extension: {argv:?}"
    );
    // Dev tools, named explicitly — never the master's `bash`-only set.
    let tools = argv
        .windows(2)
        .find(|w| w[0] == "--tools")
        .map(|w| w[1].clone());
    assert_eq!(
        tools.as_deref(),
        Some("read,bash,edit,write,grep,find,ls"),
        "{argv:?}"
    );
    assert!(argv.iter().any(|a| a == "--no-extensions"), "{argv:?}");
    // `--model` replays the stored param.
    let model = argv
        .windows(2)
        .find(|w| w[0] == "--model")
        .map(|w| w[1].clone());
    assert_eq!(model.as_deref(), Some("acme/demo-1"), "{argv:?}");
    assert_ne!(identity.pid, 0);
}

#[test]
fn worker_env_is_an_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    // Plant credentials in the daemon's own environment — the worker
    // must never see them however they are spelled.
    for name in [
        "PI544_PLANTED_TOKEN",
        "AWS_SECRET_ACCESS_KEY",
        "GH_TOKEN",
        "ANTHROPIC_API_KEY",
    ] {
        std::env::set_var(name, "should-not-leak");
    }
    std::env::set_var("SSH_AUTH_SOCK", "/tmp/pi544-agent.sock");
    let ad = adapter("normal", state, &[]);
    let agent = worker("w2", dir.path(), json!({}));
    ad.open(&agent).unwrap();
    ad.close();
    for name in [
        "PI544_PLANTED_TOKEN",
        "AWS_SECRET_ACCESS_KEY",
        "GH_TOKEN",
        "ANTHROPIC_API_KEY",
        "SSH_AUTH_SOCK",
    ] {
        std::env::remove_var(name);
    }

    let rec = record(state, "w2");
    let env: Vec<String> = serde_json::from_value(rec["env"].clone()).unwrap();
    for leaked in [
        "PI544_PLANTED_TOKEN",
        "AWS_SECRET_ACCESS_KEY",
        "GH_TOKEN",
        "ANTHROPIC_API_KEY",
    ] {
        assert!(
            !env.iter().any(|n| n == leaked),
            "{leaked} reached the worker env: {env:?}"
        );
    }
    // Development essentials + the worker's own wiring DO pass.
    for kept in [
        "PATH",
        "HOME",
        "SSH_AUTH_SOCK",
        "CADENCE_ALIAS",
        "CADENCE_STATE_DIR",
        "PI_CODING_AGENT_DIR",
        "PI_OFFLINE",
        "GIT_TERMINAL_PROMPT",
    ] {
        assert!(
            env.iter().any(|n| n == kept),
            "{kept} missing from the worker env: {env:?}"
        );
    }
}

#[test]
fn worker_auth_is_copied_scoped_and_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let operator = dir.path().join("operator-pi");
    std::fs::create_dir_all(&operator).unwrap();
    std::fs::write(operator.join("auth.json"), r#"{"token":"op-1"}"#).unwrap();
    let ad = adapter(
        "normal",
        &state,
        &[(
            "PI_CODING_AGENT_DIR",
            operator.to_string_lossy().to_string(),
        )],
    );
    let agent = worker("w3", dir.path(), json!({}));
    ad.open(&agent).unwrap();

    // The copy lands in the worker's own config dir at 0600 — never
    // shared with the master, never the operator's own file.
    let copied = state.join("agents").join("w3").join("pi").join("auth.json");
    assert_eq!(
        std::fs::read_to_string(&copied).unwrap(),
        r#"{"token":"op-1"}"#
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&copied).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(copied.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    // A reopened worker keeps its own login even when the operator's
    // file changed underneath — the copy is a one-time seed.
    std::fs::write(operator.join("auth.json"), r#"{"token":"op-2"}"#).unwrap();
    ad.close();
    ad.open(&agent).unwrap();
    ad.close();
    assert_eq!(
        std::fs::read_to_string(&copied).unwrap(),
        r#"{"token":"op-1"}"#
    );
    // The child was handed its private dir, not the operator's.
    let rec = record(&state, "w3");
    let env: Vec<String> = serde_json::from_value(rec["env"].clone()).unwrap();
    assert!(env.iter().any(|n| n == "PI_CODING_AGENT_DIR"), "{env:?}");
    assert!(
        !env.iter()
            .any(|n| n == "HOME_PI" || n.contains("AUTH_JSON")),
        "{env:?}"
    );
}

#[test]
fn worker_reopen_resumes_the_same_pi_session() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    let ad = adapter("normal", state, &[]);
    let agent = worker("w4", dir.path(), json!({}));

    let first = ad.open(&agent).unwrap();
    let _ = ad.run_turn("first turn", "m1", &|_| {}).unwrap();
    ad.close();

    // A reopen on the same alias resumes the session file: same Pi
    // session id, prompts accumulate instead of restarting.
    let second = ad.open(&agent).unwrap();
    assert_eq!(
        first.session_id, second.session_id,
        "a worker reopen must land on the stored session"
    );
    assert_eq!(first.thread_id, second.thread_id);
    let _ = ad.run_turn("second turn", "m2", &|_| {}).unwrap();
    ad.close();

    let session_file = state.join("agents").join("w4").join("session.jsonl");
    let lines: Vec<Value> = std::fs::read_to_string(&session_file)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let prompts: Vec<&Value> = lines
        .iter()
        .filter(|row| row["type"].as_str() == Some("prompt"))
        .collect();
    assert_eq!(prompts.len(), 2, "{lines:?}");
}

#[test]
fn worker_reopen_after_session_loss_mints_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    let ad = adapter("normal", state, &[]);
    let agent = worker("w5", dir.path(), json!({}));
    let first = ad.open(&agent).unwrap();
    ad.close();

    // The session file is cadence-owned state — losing it must mint a
    // fresh Pi session (the daemon then injects the continuity pack),
    // never wedge on a dead id.
    let session_file = state.join("agents").join("w5").join("session.jsonl");
    std::fs::remove_file(&session_file).unwrap();
    let second = ad.open(&agent).unwrap();
    ad.close();
    assert_ne!(
        first.session_id, second.session_id,
        "a lost session file must not resurrect the old id"
    );
}

#[test]
fn bad_worker_alias_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    for alias in ["..", ".", "a/b", "x\\y", " "] {
        let ad = adapter("normal", dir.path(), &[]);
        let agent = worker(alias, dir.path(), json!({}));
        let Err(err) = ad.open(&agent) else {
            panic!("{alias}: open must refuse an invalid worker alias")
        };
        assert!(
            err.to_string().contains("invalid worker alias"),
            "{alias}: {err}"
        );
    }
}

// ---- daemon-level: the join → dispatch → PM route ----

#[test]
fn join_dispatches_and_routes_the_result_to_pm() {
    let d = TestDaemon::start();
    let _pi = d.mock_pi("normal");
    d.register_inbox("pm");
    let bin = env!("CARGO_BIN_EXE_cadence");
    let run = |args: &[&str]| {
        std::process::Command::new(bin)
            .arg("--state-dir")
            .arg(&d.state)
            .args(args)
            .operator_output()
            .unwrap()
    };
    let out = run(&[
        "join",
        "pm",
        "pi",
        "--alias",
        "wp",
        "--detach",
        "--model",
        "acme/demo-1",
        "--effort",
        "high",
        "--no-bootstrap",
    ]);
    assert!(
        out.status.success(),
        "join: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_agent("wp", "idle", 20);

    let show = d.rpc("agent_show", json!({"alias": "wp"})).unwrap();
    assert_eq!(show["agent"]["provider"], "pi");
    assert_eq!(show["agent"]["endpoint_kind"], "managed");
    assert_eq!(show["agent"]["params"]["model"], "acme/demo-1");
    assert_eq!(show["agent"]["params"]["effort"], "high");
    assert_eq!(show["agent"]["params"]["upstream"], "pm");

    // The launch record proves the flags reached the child process.
    let rec: Value = serde_json::from_str(
        &std::fs::read_to_string(d.state.join("agents/pi-record-wp.json")).unwrap(),
    )
    .unwrap();
    let argv: Vec<String> = serde_json::from_value(rec["argv"].clone()).unwrap();
    assert!(
        argv.windows(2).any(|w| w == ["--model", "acme/demo-1"]),
        "{argv:?}"
    );
    assert!(argv.windows(2).any(|w| w[0] == "--session"), "{argv:?}");
    // Effort was applied AND verified through get_state at open.
    assert_eq!(show["agent"]["effort"].as_str(), Some("high"), "{show}");

    // Dispatch → settled text → routed to the PM's inbox.
    d.rpc(
        "agent_send",
        json!({"alias": "wp", "text": "Reply with exactly: PONG", "message": "m1"}),
    )
    .unwrap();
    let m1 = d.wait_message("wp", "m1", &["completed"], 20);
    assert_eq!(
        m1["result"]["text"],
        "fake-pi reply: Reply with exactly: PONG".to_string(),
        "{m1}"
    );
    let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    let routed = pm["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["source"].as_str() == Some("worker_result"))
        .cloned();
    let routed = routed.unwrap_or_else(|| panic!("no routed result on pm: {pm}"));
    assert!(
        routed["body"].as_str().unwrap().contains("fake-pi reply"),
        "{routed}"
    );
}

#[test]
fn pi_model_defaults_follow_team_role_like_other_providers() {
    let d = TestDaemon::start();
    let _pi = d.mock_pi("normal");
    d.register_inbox("pm");
    d.operator_rpc(
        "model_defaults_set",
        json!({"document": "{\"expected_revision\":0,\"config\":{\"schema\":1,\"providers\":{\"pi\":{\"default\":{\"mode\":\"model\",\"model\":\"pi-base\"},\"roles\":{\"qa\":{\"mode\":\"model\",\"model\":\"pi-qa-model\"}}}}}}"}),
    )
    .unwrap();
    let bin = env!("CARGO_BIN_EXE_cadence");
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "join",
            "pm",
            "pi",
            "--alias",
            "wq",
            "--team-role",
            "qa",
            "--detach",
            "--no-bootstrap",
        ])
        .operator_output()
        .unwrap();
    assert!(
        out.status.success(),
        "join: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    d.wait_agent("wq", "idle", 20);
    // The daemon's role lookup resolved `pi-qa-model` into params, which
    // the adapter replayed onto the child argv — identical plumbing to
    // claude's `model_defaults_register_resume_and_mock_argv`.
    let rec: Value = serde_json::from_str(
        &std::fs::read_to_string(d.state.join("agents/pi-record-wq.json")).unwrap(),
    )
    .unwrap();
    let argv: Vec<String> = serde_json::from_value(rec["argv"].clone()).unwrap();
    assert!(
        argv.windows(2).any(|w| w == ["--model", "pi-qa-model"]),
        "{argv:?}"
    );
    let show = d.rpc("agent_show", json!({"alias": "wq"})).unwrap();
    assert_eq!(show["agent"]["model_selection"]["source"], "role_default");
    assert_eq!(show["agent"]["model_selection"]["model"], "pi-qa-model");
}

#[test]
fn join_pi_refuses_claude_flags_and_bad_effort() {
    let d = TestDaemon::start();
    let _pi = d.mock_pi("normal");
    d.register_inbox("pm");
    let bin = env!("CARGO_BIN_EXE_cadence");
    let run = |args: &[&str]| {
        std::process::Command::new(bin)
            .arg("--state-dir")
            .arg(&d.state)
            .args(args)
            .operator_output()
            .unwrap()
    };
    // A claude-only surface must be refused, not silently dropped.
    for extra in [
        &["--bypass"][..],
        &["--permission-mode", "bypassPermissions"][..],
        &["--allow", "Bash(ls *)"][..],
        &["--broker-approvals"][..],
        &["--resume", "wp-x"][..],
    ] {
        let mut argv = vec!["join", "pm", "pi", "--alias", "wx", "--detach"];
        argv.extend_from_slice(extra);
        let out = run(&argv);
        assert!(
            !out.status.success(),
            "{argv:?} must be refused: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            d.rpc("agent_show", json!({"alias": "wx"})).is_err(),
            "a refused join must not register {argv:?}"
        );
    }
    // `bogus` is not even a clap value — rejected naming the vocabulary.
    let out = run(&[
        "join", "pm", "pi", "--alias", "wx", "--detach", "--effort", "bogus",
    ]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("off") && err.contains("xhigh"), "{err}");
    // A clap-valid word pi does not support is refused at registration.
    let err = d
        .fixture_rpc(
            "agent_register",
            json!({"alias": "wx", "provider": "pi", "endpoint_kind": "managed",
                   "cwd": d.dir.path().to_str().unwrap(),
                   "params": "{\"effort\": \"bogus\"}"}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("effort"), "{err}");
}

#[test]
fn pi_crash_is_a_provider_error_not_a_hang() {
    let d = TestDaemon::start();
    let _pi = d.mock_pi("crash");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.fixture_rpc(
        "agent_register",
        json!({"alias": "wc", "provider": "pi", "endpoint_kind": "managed",
               "cwd": cwd}),
    )
    .unwrap();
    d.wait_agent("wc", "idle", 20);
    let began = std::time::Instant::now();
    d.rpc(
        "agent_send",
        json!({"alias": "wc", "text": "hi", "message": "m1"}),
    )
    .unwrap();
    // The provider exits mid-turn: the message fences quickly as
    // unknown/failed — never waits out the idle window.
    let m1 = d.wait_message("wc", "m1", &["unknown"], 30);
    assert!(
        began.elapsed() < Duration::from_secs(30),
        "crash fenced but not quickly: {m1}"
    );
    d.wait_agent("wc", "attention", 15);
}

#[test]
fn idle_stopped_worker_resumes_its_session() {
    let d = TestDaemon::start();
    let _pi = d.mock_pi("normal");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.fixture_rpc(
        "agent_register",
        json!({"alias": "ws", "provider": "pi", "endpoint_kind": "managed",
               "cwd": cwd}),
    )
    .unwrap();
    d.wait_agent("ws", "idle", 20);
    let thread_id = d.rpc("agent_show", json!({"alias": "ws"})).unwrap()["agent"]["thread_id"]
        .as_str()
        .unwrap()
        .to_string();
    d.rpc(
        "agent_send",
        json!({"alias": "ws", "text": "one", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("ws", "m1", &["completed"], 20);

    // Idle stop drops the process; resume reopens on the stored file —
    // same Pi session id, same Cadence thread id. (`fixture_rpc`: stop
    // is an operator act — a suite running in a pane is retried the
    // operator's way.)
    d.fixture_rpc("agent_stop", json!({"alias": "ws"})).unwrap();
    d.wait_agent("ws", "stopped", 15);
    d.fixture_rpc("agent_resume", json!({"alias": "ws"}))
        .unwrap();
    d.wait_agent("ws", "idle", 20);
    let show = d.rpc("agent_show", json!({"alias": "ws"})).unwrap();
    assert_eq!(
        show["agent"]["thread_id"].as_str(),
        Some(thread_id.as_str()),
        "resumed worker must land on the same pi session: {show}"
    );
    d.rpc(
        "agent_send",
        json!({"alias": "ws", "text": "two", "message": "m2"}),
    )
    .unwrap();
    d.wait_message("ws", "m2", &["completed"], 20);
    // Both prompts accumulated on one session file.
    let session_file = d.state.join("agents/ws/session.jsonl");
    let prompts = std::fs::read_to_string(&session_file)
        .unwrap()
        .lines()
        .filter(|l| l.contains("\"prompt\""))
        .count();
    assert_eq!(prompts, 2, "session file {session_file:?}");
}
