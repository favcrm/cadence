//! CAD-322 slice-1 coverage for the managed Pi adapter — driven
//! adapter-level (no daemon harness; the integration suite is being
//! split under #267) against `tests/e2e/fake-pi.py`, a line-protocol
//! stand-in for `pi --mode rpc` reached through `CADENCE_PI_COMMAND`.
//!
//! Covers: start/open, prompt + streamed reply, tool progress events,
//! cancellation via `abort`, a crashed provider failing fast instead of
//! hanging, reopen minting a fresh session (the disposable-session /
//! continuity-pack contract), effort verification, missing-credentials
//! error quality, and auto-cancelled extension UI dialogs.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use cadence_agent::adapter::pi::PiAdapter;
use cadence_agent::adapter::{registry, AdapterHooks, ProviderAdapter, ProviderEnv};
use cadence_agent::store::Agent;
use serde_json::{json, Value};

fn fake_pi(mode: &str) -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/fake-pi.py");
    format!("python3 {} {mode}", script.display())
}

fn agent(alias: &str, params: Value) -> Agent {
    Agent {
        alias: alias.into(),
        provider: "pi".into(),
        endpoint_kind: "managed".into(),
        role: "worker".into(),
        team_role: None,
        cwd: std::env::temp_dir().to_string_lossy().into(),
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

/// An adapter over fake-pi plus the channel its events land on.
fn adapter(mode: &str, dir: &Path) -> (PiAdapter, mpsc::Receiver<(String, Value)>) {
    let env = ProviderEnv::default();
    env.set("CADENCE_PI_COMMAND", fake_pi(mode));
    let (tx, rx) = mpsc::channel();
    let hooks = AdapterHooks {
        on_event: Box::new(move |method, params| {
            let _ = tx.send((method.to_string(), params));
        }),
        on_request: Box::new(|_| {}),
    };
    (PiAdapter::new(hooks, &dir.join("pi-stderr.log"), &env), rx)
}

fn collect(rx: &mpsc::Receiver<(String, Value)>, wait: Duration) -> Vec<(String, Value)> {
    let deadline = Instant::now() + wait;
    let mut out = Vec::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(ev) => out.push(ev),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    out
}

/// The `master` alias on a state-dir-shaped adapter (the adapter derives
/// `state_dir` as the log's grandparent) — this is the same code path
/// `master start` launches: guard written, lockdown argv, env posture.
fn master_adapter(mode: &str, state: &Path, own: &[(&str, String)]) -> PiAdapter {
    let env = ProviderEnv::default();
    env.set("CADENCE_PI_COMMAND", fake_pi(mode));
    env.set(
        "CADENCE_PM_DIR",
        state.join("pm").to_string_lossy().to_string(),
    );
    std::fs::create_dir_all(state.join("logs")).unwrap();
    for (k, v) in own {
        env.set(k, v.clone());
    }
    PiAdapter::new(
        AdapterHooks {
            on_event: Box::new(|_, _| {}),
            on_request: Box::new(|_| {}),
        },
        &state.join("logs").join("pi-master.log"),
        &env,
    )
}

/// The master agent row the daemon would store: alias `master`, cwd the
/// master's own empty dir under the state dir.
fn master_agent(state: &Path, params: Value) -> Agent {
    let cwd = state.join("master").join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut a = agent(cadence_agent::master::ALIAS, params);
    a.cwd = cwd.to_string_lossy().into();
    a
}

/// The provider argv fake-pi recorded — the tail after its script name,
/// mode word first (e.g. `["normal", "--mode", "rpc", …]`).
fn recorded_argv(state: &Path) -> Vec<String> {
    let file = state.join("master/cwd/pi-argv.json");
    serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap()
}

/// The env NAMES fake-pi recorded (values are never written).
fn recorded_env(state: &Path) -> Vec<String> {
    let file = state.join("master/cwd/pi-env.json");
    serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap()
}

/// The exact flags the adapter puts on the provider for a master —
/// `--no-extensions -e <guard> --tools bash` is the lockdown (I1:
/// deleting the guard block from `build_command` fails this test).
fn expected_master_argv(mode: &str, guard: &Path) -> Vec<String> {
    [
        mode,
        "--mode",
        "rpc",
        "--no-session",
        "--offline",
        "--no-themes",
        "--no-skills",
        "--no-prompt-templates",
        "--no-context-files",
        "--no-extensions",
        "--extension",
        guard.to_str().unwrap(),
        "--tools",
        "bash",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[test]
fn open_and_prompt_streams_reply_and_settles() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("normal", dir.path());
    let id = pi.open(&agent("dev-1", json!({}))).unwrap();
    assert!(!id.thread_id.is_empty());
    assert_eq!(id.model.as_deref(), Some("fake/model-1"));
    let started = std::cell::RefCell::new(String::new());
    let turn = pi
        .run_turn("hello there", "m1", &|tok| {
            *started.borrow_mut() = tok.to_string()
        })
        .unwrap();
    assert_eq!(turn.status, "completed");
    assert!(
        turn.text.contains("fake-pi reply: hello there"),
        "{}",
        turn.text
    );
    assert!(
        registry::PI_MANAGED_TURN_TOKENS.is_current(&id.generation.unwrap(), &turn.turn_id),
        "turn token binds to this endpoint's generation"
    );
    pi.close();
}

#[test]
fn tool_lifecycle_maps_to_cadence_events() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, rx) = adapter("normal", dir.path());
    pi.open(&agent("dev-1", json!({}))).unwrap();
    let turn = pi.run_turn("please run tool now", "m1", &|_| {}).unwrap();
    assert_eq!(turn.status, "completed");
    let events = collect(&rx, Duration::from_secs(2));
    let use_ev = events
        .iter()
        .find(|(m, _)| m == "cadence/tool_use")
        .expect("tool_use event");
    assert_eq!(use_ev.1["tool"], "bash");
    assert_eq!(use_ev.1["tool_use_id"], "call_1");
    assert!(
        use_ev.1["summary"]
            .as_str()
            .unwrap_or("")
            .contains("cadence status"),
        "summary carries the redacted command: {}",
        use_ev.1
    );
    assert!(events.iter().any(|(m, _)| m == "cadence/tool_progress"));
    let res = events
        .iter()
        .find(|(m, _)| m == "cadence/tool_result")
        .expect("tool_result event");
    assert_eq!(res.1["is_error"], false);
    pi.close();
}

#[test]
fn missing_credentials_is_a_clear_provider_error() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("no-credits", dir.path());
    pi.open(&agent("dev-1", json!({}))).unwrap();
    let err = pi.run_turn("hi", "m1", &|_| {}).err().expect("fails");
    let text = err.to_string();
    // F18: a credential failure names the fix — never a raw provider dump.
    assert!(
        text.contains("/login") || text.contains("PI_CODING_AGENT_DIR"),
        "{text}"
    );
    pi.close();
}

#[test]
fn abort_interrupts_the_running_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("hang", dir.path());
    pi.open(&agent("dev-1", json!({}))).unwrap();
    let adapter = &pi;
    let handle = std::thread::scope(|s| {
        let h = s.spawn(|| adapter.run_turn("wait forever", "m1", &|_| {}));
        // Let the prompt land and the deltas stream, then abort.
        std::thread::sleep(Duration::from_millis(500));
        pi.interrupt();
        h.join().unwrap()
    });
    let turn = handle.unwrap();
    assert_eq!(turn.status, "interrupted");
    pi.close();
}

#[test]
fn interrupt_turn_only_targets_the_live_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("hang", dir.path());
    pi.open(&agent("dev-1", json!({}))).unwrap();
    // A turn that is not running is refused — nothing is sent.
    let outcome = pi
        .interrupt_turn("pi-0000-notrunning", &|| Ok(false))
        .unwrap();
    assert_eq!(
        outcome,
        cadence_agent::adapter::InterruptOutcome::NotRunning
    );
    pi.close();
}

#[test]
fn a_crashed_pi_fails_instead_of_hanging() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("crash", dir.path());
    pi.open(&agent("dev-1", json!({}))).unwrap();
    let started = Instant::now();
    let err = pi.run_turn("boom", "m1", &|_| {}).err().expect("fails");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "crash took too long to surface"
    );
    assert!(pi.disconnected());
    let _ = err;
    pi.close();
}

#[test]
fn reopen_mints_a_fresh_session() {
    // Disposable sessions: a reopened endpoint is a NEW native session —
    // the daemon rebuilds context from the continuity pack (CAD-324).
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("normal", dir.path());
    let first = pi.open(&agent("dev-1", json!({}))).unwrap();
    pi.close();
    let second = pi.open(&agent("dev-1", json!({}))).unwrap();
    assert_ne!(first.thread_id, second.thread_id);
    pi.close();
}

#[test]
fn effort_is_verified_through_get_state() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("normal", dir.path());
    let id = pi.open(&agent("dev-1", json!({"effort": "high"}))).unwrap();
    assert_eq!(id.effort.as_deref(), Some("high"));
    pi.close();
}

#[test]
fn extension_dialogs_are_auto_cancelled_and_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, rx) = adapter("dialog", dir.path());
    pi.open(&agent("dev-1", json!({}))).unwrap();
    let events = collect(&rx, Duration::from_secs(2));
    let ui = events
        .iter()
        .find(|(m, _)| m == "cadence/pi_ui_request")
        .expect("the dialog request is recorded");
    assert_eq!(ui.1["ui_method"], "confirm");
    assert_eq!(ui.1["cancelled"], true);
    // And the turn still works — the dialog did not wedge the provider.
    let turn = pi.run_turn("after dialog", "m1", &|_| {}).unwrap();
    assert_eq!(turn.status, "completed");
    pi.close();
}

#[test]
fn respond_is_refused_in_slice_1() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("normal", dir.path());
    pi.open(&agent("dev-1", json!({}))).unwrap();
    assert!(pi.respond(&json!("x"), json!({})).is_err());
    pi.close();
}

#[test]
fn launch_params_validate_pi_effort_names() {
    for level in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
        assert!(
            registry::validate_launch_params("pi", "managed", &json!({"effort": level})).is_ok(),
            "{level}"
        );
    }
    assert!(
        registry::validate_launch_params("pi", "managed", &json!({"effort": "bogus"})).is_err()
    );
    assert!(registry::validate_launch_params("pi", "managed", &json!({"model": ""})).is_err());
}

/// CAD-322 round 2 (I1): the master alias launches the provider with the
/// exact lockdown argv — `--no-extensions -e <guard> --tools bash` —
/// and the generated guard file carries the grammar. Deleting the guard
/// wiring from `build_command` or `write_pi_guard` fails this test.
#[test]
fn master_launch_has_exact_lockdown_argv_and_a_real_guard() {
    let state = tempfile::tempdir().unwrap();
    let pi = master_adapter(
        "normal",
        state.path(),
        &[(cadence_agent::master::TEST_NO_LANDLOCK, "1".into())],
    );
    pi.open(&master_agent(state.path(), json!({"unconfined": true})))
        .unwrap();
    let guard = state.path().join("master/pi-guard.js");
    assert_eq!(
        recorded_argv(state.path()),
        expected_master_argv("normal", &guard),
        "the master's provider argv"
    );
    let src = std::fs::read_to_string(&guard).unwrap();
    assert!(
        src.contains("piGuardAllows"),
        "guard has the grammar: {src}"
    );
    assert!(src.contains("\"argv\":[\"status\"]"), "rules table: {src}");
    pi.close();
}

/// CAD-322 round 2 (I1, confined leg): on a Landlock host the master's
/// provider really is exec'd through `cadence confine` — fake-pi runs
/// under the policy, writes its record into the policy's writable cwd,
/// and the provider log carries the policy args. Skipped where Landlock
/// is unavailable (the unconfined argv test above covers the flags).
#[test]
fn confined_master_runs_fake_pi_under_the_policy() {
    if cadence_agent::confine::available().is_err() {
        eprintln!("no Landlock on this host — confined path skipped");
        return;
    }
    let state = tempfile::tempdir().unwrap();
    let e2e = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e");
    let pi = master_adapter(
        "normal",
        state.path(),
        &[
            // The test runner's exe is not `cadence` — the real bin is.
            (
                "CADENCE_CONFINE_COMMAND",
                env!("CARGO_BIN_EXE_cadence").to_string(),
            ),
            // fake-pi.py lives outside the policy dirs; the operator's
            // extra-read seam exposes it to the confined child.
            (
                cadence_agent::master::CONFINE_EXTRA_READ_ENV,
                e2e.to_string_lossy().to_string(),
            ),
        ],
    );
    pi.open(&master_agent(state.path(), json!({}))).unwrap();
    let guard = state.path().join("master/pi-guard.js");
    assert_eq!(
        recorded_argv(state.path()),
        expected_master_argv("normal", &guard),
        "confined master: the provider argv after `--` is unchanged"
    );
    let turn = pi.run_turn("confined hello", "m1", &|_| {}).unwrap();
    assert_eq!(turn.status, "completed", "{}", turn.status);
    let log = std::fs::read_to_string(state.path().join("logs/pi-master.log")).unwrap();
    assert!(log.contains("master confinement:"), "{log}");
    pi.close();
}

/// CAD-322 round 2 (I3): the master's provider starts from an EMPTY
/// environment — credentials planted in the daemon's env (the review's
/// names and neighbours) never reach the child; only the allowlist and
/// the daemon-injected pairs do.
#[test]
fn master_env_is_allowlisted_not_inherited() {
    let planted = [
        "AWS_SECRET_ACCESS_KEY",
        "AWS_BEARER_TOKEN_BEDROCK",
        "MOONSHOT_API_KEY",
        "TOGETHER_API_KEY",
        "MINIMAX_API_KEY",
        "ZAI_CN_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "COPILOT_GITHUB_TOKEN",
        "OPENAI_API_KEY",
        "SSH_AUTH_SOCK",
        "GIT_SSH_COMMAND",
        "NODE_OPTIONS",
        "PI_CODING_AGENT_DIR", // the daemon's own, not the operator's
    ];
    for name in planted {
        std::env::set_var(name, "planted-test-value");
    }
    let state = tempfile::tempdir().unwrap();
    let pi = master_adapter(
        "normal",
        state.path(),
        &[(cadence_agent::master::TEST_NO_LANDLOCK, "1".into())],
    );
    pi.open(&master_agent(state.path(), json!({"unconfined": true})))
        .unwrap();
    let names = recorded_env(state.path());
    for name in planted {
        assert!(
            !names.iter().any(|n| n == name),
            "planted {name} reached the pi child: {names:?}"
        );
    }
    for name in ["PATH", "CADENCE_ALIAS", "CADENCE_STATE_DIR", "TMPDIR"] {
        assert!(names.iter().any(|n| n == name), "{name} missing: {names:?}");
    }
    for name in planted {
        std::env::remove_var(name);
    }
    pi.close();
}

/// CAD-322 round 2 (C1): evaluate the generated guard's grammar section
/// verbatim under Node — every review bypass is refused, every real
/// allowlisted command parses. Needs node on PATH (the pi toolchain
/// requires it; CI provides node 22).
#[test]
fn the_pi_guard_is_a_grammar_and_refuses_the_bypasses() {
    let state = tempfile::tempdir().unwrap();
    let pi = master_adapter(
        "normal",
        state.path(),
        &[(cadence_agent::master::TEST_NO_LANDLOCK, "1".into())],
    );
    pi.open(&master_agent(state.path(), json!({"unconfined": true})))
        .unwrap();
    pi.close();
    let src = std::fs::read_to_string(state.path().join("master/pi-guard.js")).unwrap();
    let (open_marker, close_marker) = (
        ">>> cadence-guard-grammar >>>",
        "<<< cadence-guard-grammar <<<",
    );
    // The grammar is the lines BETWEEN the marker lines: start at the
    // newline after the open marker, end at the start of the close
    // marker's own line.
    let after_open = src.find(open_marker).expect("grammar marker missing") + open_marker.len();
    let start = src[after_open..]
        .find('\n')
        .map(|i| after_open + i + 1)
        .expect("grammar opens");
    let end = src[..src.rfind(close_marker).expect("grammar end marker missing")]
        .rfind('\n')
        .map(|i| i + 1)
        .expect("grammar closes");
    let grammar = &src[start..end];
    let allow = [
        "cadence status",
        "cadence status --long",
        "cadence issue ls",
        "cadence issue ls --project demo",
        "cadence issue show D-1",
        "cadence issue project ls",
        "cadence plan show p1",
        "cadence plan propose --project demo --file /tmp/p.md",
        "cadence project new x",
        "cadence master dispatch D-1 --to swe-1",
        "cadence master escalate D-1 --file q.md",
        "cadence master summary",
        "cadence interrupt abc",
        "cadence report file reports/x.md",
        "cadence agent list",
        "cadence agent show swe-1",
        " cadence  status ", // whitespace still tokenizes to cadence+status
    ];
    let deny = [
        "cadence status; rm -rf /",
        "cadence status && rm -rf /",
        "cadence status || cat /etc/passwd",
        "cadence status | cat",
        "cadence status $(id)",
        "cadence status `id`",
        "cadence status > /tmp/x",
        "cadence status\nrm -rf /",
        "FOO=bar cadence status", // env-prefix: argv[0] is not cadence
        "env FOO=bar cadence status",
        "bash -c \"cadence status\"",
        "sh -c 'cadence status'",
        "cadence", // bare: no allowlisted subcommand
        "claude status",
        "cadence agent stop swe-1",
        "cadence build-slot run -- id",
        "cadence issue ls 'x'",
        "cadence issue ls \"x\"",
        "cadence issue show $HOME",
        "cadence\tstatus",
        "",
        "pi --version",
    ];
    let cases: Vec<&str> = allow.iter().chain(&deny).copied().collect();
    let js = format!(
        "{grammar}\nprocess.stdout.write(JSON.stringify(({}).map(piGuardAllows)));",
        json!(cases)
    );
    let out = cadence_agent::reaper::output(std::process::Command::new("node").arg("-e").arg(&js))
        .expect("node is required for the pi toolchain");
    assert!(
        out.status.success(),
        "guard grammar failed to evaluate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let verdicts: Vec<bool> = serde_json::from_slice(&out.stdout).unwrap();
    for (i, cmd) in cases.iter().enumerate() {
        assert_eq!(verdicts[i], i < allow.len(), "{cmd:?}");
    }
}

/// CAD-322 round 2 (I4): close → reopen → first turn. A stale EOF from
/// the previous reader must never mark the reopened endpoint dead — the
/// generation tag drops it, and the first turn after reopen completes.
#[test]
fn close_then_reopen_then_first_turn_completes() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("normal", dir.path());
    pi.open(&agent("dev-1", json!({}))).unwrap();
    pi.close();
    for _ in 0..100 {
        if pi.disconnected() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(pi.disconnected(), "the closed transport never observed EOF");
    pi.open(&agent("dev-1", json!({}))).unwrap();
    assert!(!pi.disconnected(), "stale EOF marked the new session dead");
    let turn = pi.run_turn("after reopen", "m1", &|_| {}).unwrap();
    assert_eq!(turn.status, "completed");
    assert!(turn.text.contains("fake-pi reply"), "{}", turn.text);
    pi.close();
}

/// CAD-322 round 2 (N3): a turn that never settles ends
/// `OutcomeUnknown` once `turn_max_secs` expires — the provider is still
/// streaming, the daemon just cannot trust the outcome.
#[test]
fn a_turn_that_never_settles_is_outcome_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("hang", dir.path());
    pi.open(&agent("dev-1", json!({"turn_max_secs": 1})))
        .unwrap();
    let err = pi
        .run_turn("wait forever", "m1", &|_| {})
        .err()
        .expect("a hanging turn must error, not hang");
    assert!(
        matches!(err, cadence_agent::error::Error::OutcomeUnknown(_)),
        "{err:?}"
    );
    pi.close();
}
