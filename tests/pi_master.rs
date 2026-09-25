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
