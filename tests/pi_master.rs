//! CAD-322 slice-1 coverage for the managed Pi adapter — driven
//! adapter-level (no daemon harness; the integration suite is being
//! split under #267) against `tests/e2e/fake-pi.py`, a line-protocol
//! stand-in for `pi --mode rpc` reached through `CADENCE_PI_COMMAND`.
//!
//! Covers: start/open, prompt + streamed reply, tool progress events,
//! cancellation via `abort`, a crashed provider failing fast instead of
//! hanging, reopen minting a fresh session (the disposable-session /
//! continuity-pack contract), effort verification, missing-credentials
//! error quality, auto-cancelled extension UI dialogs, and the CAD-559
//! model/provider-package gates (allowlist, pinned `-e`, no silent
//! fallback).

#![allow(clippy::disallowed_methods)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use cadence_agent::adapter::pi::PiAdapter;
use cadence_agent::adapter::{registry, AdapterHooks, ProviderAdapter, ProviderEnv};
use cadence_agent::store::Agent;
use common::in_own_process;
use common::pi_policy_pm;
use serde_json::{json, Value};

fn fake_pi(mode: &str) -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/fake-pi.py");
    format!("python3 {} {mode}", script.display())
}

fn agent(alias: &str, params: Value) -> Agent {
    let mut params = params;
    // CAD-559: a pi agent opens only on an explicit allowlisted model —
    // tests that want another value (or none) set the key themselves.
    if let Some(p) = params.as_object_mut() {
        p.entry("model".to_string())
            .or_insert_with(|| json!("fake/model-1"));
    }
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
    adapter_env(mode, dir, &[])
}

fn adapter_env(
    mode: &str,
    dir: &Path,
    own: &[(&str, String)],
) -> (PiAdapter, mpsc::Receiver<(String, Value)>) {
    let env = ProviderEnv::default();
    env.set("CADENCE_PI_COMMAND", fake_pi(mode));
    // CAD-559: pi opens only under an operator `[pi]` policy — `own`
    // can still point CADENCE_PM_DIR at a test's own pm.yaml.
    let pm = dir.join("pm");
    pi_policy_pm(&pm);
    env.set("CADENCE_PM_DIR", pm.to_string_lossy().to_string());
    for (k, v) in own {
        env.set(k, v.clone());
    }
    std::fs::create_dir_all(dir.join("logs")).unwrap();
    let (tx, rx) = mpsc::channel();
    let hooks = AdapterHooks {
        on_event: Box::new(move |method, params| {
            let _ = tx.send((method.to_string(), params));
        }),
        on_request: Box::new(|_| {}),
    };
    (
        PiAdapter::new(hooks, &dir.join("logs").join("pi-stderr.log"), &env),
        rx,
    )
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
    // CAD-559: the tracker's `[pi]` table governs what this master may
    // launch on — `own` can repoint CADENCE_PM_DIR at a custom policy.
    let pm = state.join("pm");
    pi_policy_pm(&pm);
    env.set("CADENCE_PM_DIR", pm.to_string_lossy().to_string());
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
/// `--no-extensions -e <guard> --tools bash,read` is the lockdown (I1:
/// deleting the guard block from `build_command` fails this test).
/// `read` joined the toolset under CAD-552 — the guard confines it to
/// `master/tmp`.
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
        "bash,read",
        // CAD-559: the resolved allowlisted model always lands last —
        // a pi launch with no explicit `--model` is refused before
        // this argv is ever built.
        "--model",
        "fake/model-1",
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
    // Disposable sessions are the MASTER's contract: a reopened master
    // endpoint is a NEW native session — the daemon rebuilds context
    // from the continuity pack (CAD-324). A pi WORKER's reopen resumes
    // its stored session file instead (CAD-544; tests/pi_worker.rs).
    let state = tempfile::tempdir().unwrap();
    let pi = master_adapter(
        "normal",
        state.path(),
        &[(cadence_agent::master::TEST_NO_LANDLOCK, "1".into())],
    );
    let agent = master_agent(state.path(), json!({"unconfined": true}));
    let first = pi.open(&agent).unwrap();
    pi.close();
    let second = pi.open(&agent).unwrap();
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
    // CAD-570: under the policy the pi-devin catalog write lands in
    // the master's own XDG_CACHE_HOME — before the fix this open
    // EACCES'd `~/.cache/pi-devin/models.json` on the smoke host.
    assert!(
        state
            .path()
            .join("master/pi/cache/pi-devin/models.json")
            .is_file(),
        "confined master: the catalog cache did not land in master/pi/cache"
    );
    assert!(
        !log.contains("EACCES"),
        "confined master logged a cache EACCES: {log}"
    );
    pi.close();
}

/// CAD-322 round 3 (N1): the policy a confined Pi master actually gets
/// is witnessed — `pi_master_confinement` emits the argv `open` feeds
/// `cadence confine`. Its write set holds `master/pi` and never
/// `master/claude`; its read set holds neither `master/claude` nor the
/// operator's `~/.local/share/claude` (Claude's `home_read`). Mutation:
/// pointing `provider_dir` at `claude_config_dir` fails this test.
#[test]
fn the_emitted_pi_policy_is_pis_own_dirs_never_claudes() {
    let state = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let env = ProviderEnv::default();
    env.set("CADENCE_PI_COMMAND", fake_pi("normal"));
    env.set("HOME", home.path().to_string_lossy().to_string());
    env.set(
        "CADENCE_PM_DIR",
        state.path().join("pm").to_string_lossy().to_string(),
    );
    let (_confine, policy) = cadence_agent::adapter::pi::pi_master_confinement(&env, state.path());
    let pi_dir = state.path().join("master/pi");
    let claude_dir = state.path().join("master/claude");
    let claude_home = home.path().join(".local/share/claude");
    let has = |set: &[std::path::PathBuf], dir: &Path| set.iter().any(|p| p.starts_with(dir));
    assert!(
        has(&policy.write, &pi_dir),
        "write set lacks master/pi: {policy:?}"
    );
    // CAD-570: the master's XDG_CACHE_HOME (`master/pi/cache`, where
    // pi-devin writes its catalog) is inside that write grant — the
    // path pi-devin touches must be covered, never only readable.
    let cache = pi_dir.join("cache/pi-devin");
    assert!(
        policy.write.iter().any(|grant| cache.starts_with(grant)),
        "write set does not cover master/pi/cache: {policy:?}"
    );
    assert!(
        !has(&policy.write, &claude_dir),
        "write set holds master/claude: {policy:?}"
    );
    assert!(
        !has(&policy.read, &claude_dir),
        "read set holds master/claude: {policy:?}"
    );
    assert!(
        !has(&policy.read, &claude_home),
        "read set holds ~/.local/share/claude: {policy:?}"
    );
    // `master/pi` is also never a read-only grant; the guard extension
    // is the only extra read the provider sees.
    assert_eq!(
        policy
            .read
            .iter()
            .filter(|p| p.starts_with(&pi_dir))
            .count(),
        0,
        "master/pi is write-only: {policy:?}"
    );
}

/// CAD-322 round 2 (I3): the master's provider starts from an EMPTY
/// environment — credentials planted in the daemon's env (the review's
/// names and neighbours) never reach the child; only the allowlist and
/// the daemon-injected pairs do.
const PLANTED_ENV: &[&str] = &[
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

#[test]
fn master_env_is_allowlisted_not_inherited() {
    // The plants ride in the child's env from birth (CAD-587) — no
    // in-process mutation of the env this binary's tests share.
    if !in_own_process(
        "master_env_is_allowlisted_not_inherited",
        &PLANTED_ENV
            .iter()
            .map(|n| (*n, "planted-test-value"))
            .collect::<Vec<_>>(),
    ) {
        return;
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
    for name in PLANTED_ENV {
        assert!(
            !names.iter().any(|n| n == name),
            "planted {name} reached the pi child: {names:?}"
        );
    }
    for name in [
        "PATH",
        "CADENCE_ALIAS",
        "CADENCE_STATE_DIR",
        "TMPDIR",
        "XDG_CACHE_HOME",
    ] {
        assert!(names.iter().any(|n| n == name), "{name} missing: {names:?}");
    }
    pi.close();
}

/// CAD-570: the master's XDG_CACHE_HOME is its own — the value is
/// proven by WHERE the pi-devin catalog write lands, not by the env
/// name (the record stores names only). A daemon-env XDG_CACHE_HOME
/// planted at an empty dir must NOT receive the catalog; the file
/// must land under `<state>/master/pi/cache/pi-devin/` (created 0700).
/// Mutation: dropping the env pair or pointing it at the inherited
/// `~/.cache` writes the catalog to the planted dir and fails this.
#[test]
fn master_cache_home_is_its_own_private_dir() {
    let planted = tempfile::tempdir().unwrap();
    if !in_own_process(
        "master_cache_home_is_its_own_private_dir",
        &[("XDG_CACHE_HOME", planted.path().to_str().unwrap())],
    ) {
        return;
    }
    // The planted operator cache is the one this child's env carries,
    // not a tempdir minted here.
    let planted = PathBuf::from(std::env::var("XDG_CACHE_HOME").unwrap());
    let state = tempfile::tempdir().unwrap();
    let pi = master_adapter(
        "normal",
        state.path(),
        &[(cadence_agent::master::TEST_NO_LANDLOCK, "1".into())],
    );
    pi.open(&master_agent(state.path(), json!({"unconfined": true})))
        .unwrap();
    pi.close();

    let cache = state.path().join("master/pi/cache");
    assert!(
        cache.join("pi-devin/models.json").is_file(),
        "the pi-devin catalog did not land in the master's own cache dir"
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
        0o700,
        "the master's cache dir is private"
    );
    assert!(
        !planted.join("pi-devin").exists(),
        "the inherited operator cache received the catalog — the explicit pair lost"
    );
    // The master record only stores env NAMES — XDG_CACHE_HOME must be
    // among them (the explicit pair), whatever was planted.
    let names = recorded_env(state.path());
    assert!(names.iter().any(|n| n == "XDG_CACHE_HOME"), "{names:?}");
}

/// Open a master adapter (writes the guard) and return the generated
/// extension's grammar section — the lines BETWEEN the markers,
/// verbatim — for verbatim evaluation under Node.
fn guard_grammar(state: &Path) -> String {
    let src = std::fs::read_to_string(state.join("master/pi-guard.js")).unwrap();
    let (open_marker, close_marker) = (
        ">>> cadence-guard-grammar >>>",
        "<<< cadence-guard-grammar <<<",
    );
    // Start at the newline after the open marker, end at the start of
    // the close marker's own line.
    let after_open = src.find(open_marker).expect("grammar marker missing") + open_marker.len();
    let start = src[after_open..]
        .find('\n')
        .map(|i| after_open + i + 1)
        .expect("grammar opens");
    let end = src[..src.rfind(close_marker).expect("grammar end marker missing")]
        .rfind('\n')
        .map(|i| i + 1)
        .expect("grammar closes");
    src[start..end].to_string()
}

/// `node -e <js>` — the pi toolchain needs node on PATH (CI provides
/// node 22). Asserts success and parses stdout as JSON.
fn node_eval<T: serde::de::DeserializeOwned>(js: &str) -> T {
    let out = cadence_agent::reaper::output(std::process::Command::new("node").arg("-e").arg(js))
        .expect("node is required for the pi toolchain");
    assert!(
        out.status.success(),
        "guard grammar failed to evaluate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// CAD-322 round 2 (C1): evaluate the generated guard's grammar section
/// verbatim under Node — every review bypass is refused, every real
/// allowlisted command parses. CAD-552 added the missing read verbs and
/// write-verb refusals to the case lists.
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
    let grammar = guard_grammar(state.path());
    let allow = [
        "cadence status",
        "cadence status --long",
        "cadence issue ls",
        "cadence issue ls --project demo",
        "cadence issue ls --summary --json", // CAD-552 one-call status
        "cadence issue show D-1",
        "cadence issue log D-1",
        "cadence issue log D-1 --limit 10",
        "cadence issue epic ls",
        "cadence issue epic ls --project demo",
        "cadence issue epic show D-1",
        "cadence issue project ls",
        "cadence plan ls",
        "cadence plan ls --state approved",
        "cadence plan show p1",
        "cadence thread show swe-1",
        "cadence overview",
        "cadence overview --json",
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
        // CAD-552: write verbs stay refused — `=` is inside the
        // charset, so only the argv-prefix table stands in the way.
        "cadence issue new x",
        "cadence issue set D-1 status=done",
        "cadence plan approve D-1",
        "cadence epic ls",     // the verb's path is `issue epic`
        "cadence issue show",  // the `*` form needs an argument
        "cadence thread show", // same
        "cadence read /tmp/x", // `read` is a tool, never a verb
        "cadence issue ls --json | head -5",
    ];
    let cases: Vec<&str> = allow.iter().chain(&deny).copied().collect();
    let js = format!(
        "{grammar}\nprocess.stdout.write(JSON.stringify(({}).map(piGuardAllows)));",
        json!(cases)
    );
    let verdicts: Vec<bool> = node_eval(&js);
    for (i, cmd) in cases.iter().enumerate() {
        assert_eq!(verdicts[i], i < allow.len(), "{cmd:?}");
    }
}

/// CAD-552: a refusal names the rule the command hit and the nearest
/// allowed form — the master self-corrects instead of probing.
#[test]
fn the_pi_guard_refusal_names_the_rule_and_allowed_forms() {
    let state = tempfile::tempdir().unwrap();
    let pi = master_adapter(
        "normal",
        state.path(),
        &[(cadence_agent::master::TEST_NO_LANDLOCK, "1".into())],
    );
    pi.open(&master_agent(state.path(), json!({"unconfined": true})))
        .unwrap();
    pi.close();
    let grammar = guard_grammar(state.path());
    let cases = [
        "cadence status | head -5",
        "rm -rf /",
        "cadence issue new x",
        "cadence status",
        "",
    ];
    let js = format!(
        "{grammar}\nprocess.stdout.write(JSON.stringify(({}).map(piGuardRefusal)));",
        json!(cases)
    );
    let reasons: Vec<Option<String>> = node_eval(&js);
    assert!(
        reasons[0].as_deref().unwrap_or("").contains("no pipes"),
        "charset refusal names the rule: {:?}",
        reasons[0]
    );
    assert!(
        reasons[1]
            .as_deref()
            .unwrap_or("")
            .contains("starts `cadence`"),
        "non-cadence refusal names argv[0]: {:?}",
        reasons[1]
    );
    let verb_refusal = reasons[2].as_deref().unwrap_or("");
    assert!(
        verb_refusal.contains("not an allowlisted verb"),
        "{verb_refusal}"
    );
    // The nearest allowed form — the refusal lists the verb table.
    assert!(verb_refusal.contains("cadence issue ls"), "{verb_refusal}");
    assert!(verb_refusal.contains("cadence overview"), "{verb_refusal}");
    assert_eq!(reasons[3], None, "an allowlisted command has no refusal");
    assert!(reasons[4].is_some(), "the empty command refuses");
}

/// CAD-552: the `read` tool is confined to the master's own tmp dir —
/// where Pi spills long bash output. `piReadPathAllowed` is evaluated
/// verbatim under Node: inside passes, every escape refuses — prefix
/// siblings (`tmp-evil`), `..` out of the dir, absolute paths outside,
/// relative paths (cwd is the master's empty workdir, never the spill
/// dir). The `<tmp>-evil` case is the mutation proof for the
/// `dir + "/"` prefix check.
#[test]
fn the_pi_guard_confines_read_to_master_tmp() {
    let state = tempfile::tempdir().unwrap();
    let pi = master_adapter(
        "normal",
        state.path(),
        &[(cadence_agent::master::TEST_NO_LANDLOCK, "1".into())],
    );
    pi.open(&master_agent(state.path(), json!({"unconfined": true})))
        .unwrap();
    pi.close();
    let grammar = guard_grammar(state.path());
    let tmp = state.path().join("master/tmp");
    let tmp = tmp.to_string_lossy();
    let allow = [
        format!("{tmp}/pi-bash-1.log"),
        format!("{tmp}/sub/x.log"),
        tmp.to_string(),
        format!("{tmp}//double-slash.log"),
        format!("{tmp}/a/../b.log"), // stays inside after normalization
    ];
    let deny = [
        "/etc/passwd".to_string(),
        "/".to_string(),
        format!("{tmp}-evil/x.log"), // a sibling whose name shares the prefix
        format!("{tmp}/../cwd/pi-argv.json"),
        format!("{tmp}/.."),         // the parent, master/ itself
        "pi-bash-1.log".to_string(), // relative resolves against cwd — never tmp
        String::new(),
    ];
    let js = format!(
        "{grammar}\nprocess.stdout.write(JSON.stringify({{allow:({}).map(piReadPathAllowed),deny:({}).map(piReadPathAllowed),nonString:piReadPathAllowed(undefined),num:piReadPathAllowed(42)}}));",
        json!(allow),
        json!(deny)
    );
    let out: Value = node_eval(&js);
    for (i, p) in allow.iter().enumerate() {
        assert_eq!(out["allow"][i], json!(true), "allow {p}");
    }
    for (i, p) in deny.iter().enumerate() {
        assert_eq!(out["deny"][i], json!(false), "deny {p}");
    }
    assert_eq!(out["nonString"], json!(false));
    assert_eq!(out["num"], json!(false));
}

/// CAD-552: the generated extension itself — imported as a module with
/// a stub `pi` — routes `tool_call` through the grammar: bash by the
/// allowlist, `read` by the tmp confinement, every other tool refused.
/// Dropping the read branch fails the in-tmp case; widening the prefix
/// check fails an out-of-tmp one.
#[test]
fn the_generated_extension_routes_tool_calls() {
    let state = tempfile::tempdir().unwrap();
    let pi = master_adapter(
        "normal",
        state.path(),
        &[(cadence_agent::master::TEST_NO_LANDLOCK, "1".into())],
    );
    pi.open(&master_agent(state.path(), json!({"unconfined": true})))
        .unwrap();
    pi.close();
    let guard = state.path().join("master/pi-guard.js");
    // `export default` needs ESM — import a .mjs copy.
    let mjs = state.path().join("guard.mjs");
    std::fs::copy(&guard, &mjs).unwrap();
    let tmp = state.path().join("master/tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let tmp = tmp.to_string_lossy().to_string();
    let cases = json!([
        {"tool": "bash", "input": {"command": "cadence status"}},
        {"tool": "bash", "input": {"command": "cadence issue ls --summary --json"}},
        {"tool": "bash", "input": {"command": "cadence issue new x"}},
        {"tool": "bash", "input": {"command": "cadence status | head"}},
        {"tool": "bash", "input": {"command": "rm -rf /"}},
        {"tool": "read", "input": {"path": format!("{tmp}/pi-bash-1.log")}},
        {"tool": "read", "input": {"path": "/etc/passwd"}},
        {"tool": "read", "input": {"path": format!("{tmp}-evil/x")}},
        {"tool": "read", "input": {"path": format!("{tmp}/../cwd/x")}},
        {"tool": "read", "input": {}},
        {"tool": "write", "input": {"path": format!("{tmp}/x"), "content": "y"}},
        {"tool": "grep", "input": {"pattern": "x"}},
    ]);
    let harness = format!(
        r#"import guard from "file://{}";
let handler;
guard({{ on: (name, cb) => {{ if (name === "tool_call") handler = cb; }} }});
const out = [];
for (const c of {}) {{
  const r = await handler({{ toolName: c.tool, input: c.input }});
  out.push(r === undefined ? null : String(r.reason ?? ""));
}}
process.stdout.write(JSON.stringify(out));"#,
        mjs.display(),
        cases
    );
    let out = cadence_agent::reaper::output(
        std::process::Command::new("node")
            .arg("--input-type=module")
            .arg("-e")
            .arg(&harness),
    )
    .expect("node is required for the pi toolchain");
    assert!(
        out.status.success(),
        "extension import failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let reasons: Vec<Option<String>> = serde_json::from_slice(&out.stdout).unwrap();
    for (i, reason) in reasons.iter().enumerate() {
        let want_allow = i <= 1 || i == 5; // the two good bash calls + the in-tmp read
        assert_eq!(reason.is_none(), want_allow, "case {i}: {cases}");
    }
    // The refusals carry their rule, not a bare no.
    assert!(reasons[2]
        .as_deref()
        .unwrap()
        .contains("not an allowlisted verb"));
    assert!(reasons[3].as_deref().unwrap().contains("no pipes"));
    assert!(reasons[6].as_deref().unwrap().contains("confined"));
    assert!(reasons[10].as_deref().unwrap().contains("not enabled"));
}

/// CAD-552's before/after probe: one REAL Pi turn — the "report status
/// of our projects" prompt after the briefing text — timed, with tool
/// calls and guard refusals counted from the adapter events. It runs
/// only when asked, against the host's real `pi` + login:
///
/// ```sh
/// CADENCE_PI_PROBE=1 CADENCE_PM_DIR=<pm> cargo test --test pi_master \
///     --release -- --ignored --nocapture status_prompt_probe
/// ```
///
/// `CADENCE_PROBE_STATE` pins the state dir (a tempdir otherwise);
/// `CADENCE_PROBE_MODEL` overrides pi's default model; the result line
/// is also written to `$CADENCE_PROBE_OUT` when set. Unconfined — the
/// guard, briefing and argv are what differ, not Landlock.
#[test]
#[ignore = "real Pi turn — opt in with CADENCE_PI_PROBE=1"]
fn status_prompt_probe() {
    if std::env::var("CADENCE_PI_PROBE").is_err() {
        return;
    }
    // A real `pi`, never a mock — a leaked CADENCE_PI_COMMAND would
    // silently swap the provider.
    std::env::remove_var("CADENCE_PI_COMMAND");
    // A plain dir (no TempDir drop): the spill logs survive for
    // inspection. CADENCE_PROBE_STATE pins it.
    let state = std::env::var("CADENCE_PROBE_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::temp_dir().join(format!("cad552-probe-{}", std::process::id()))
        });
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(state.join("logs")).unwrap();
    // Real Pi needs its login inside `<state>/master/pi` — provision it
    // the way `master start --copy-login` does, from the operator's own
    // agent dir. No login anywhere → the probe skips loudly, not fails.
    let operator_config = std::env::var("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap()).join(".pi/agent"));
    match cadence_agent::master::copy_login_for("pi", &state, &operator_config) {
        Ok(cadence_agent::master::Login::None) => {
            eprintln!(
                "PROBE skipped: no pi login in {}",
                operator_config.display()
            );
            return;
        }
        Err(e) => panic!("probe could not provision pi login: {e}"),
        _ => {}
    }
    let env = ProviderEnv::default();
    if let Ok(pm) = std::env::var("CADENCE_PM_DIR") {
        env.set("CADENCE_PM_DIR", pm);
    }
    env.set(cadence_agent::master::TEST_NO_LANDLOCK, "1");
    // The master's own PATH-prepend: the worktree's `cadence` first, so
    // `cadence` inside the turn is the build under test.
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // The env-scan exemption reads the name on the set_var line.
    std::env::set_var("PATH", path);
    let (tx, rx) = mpsc::channel();
    let pi = PiAdapter::new(
        AdapterHooks {
            on_event: Box::new(move |method, params| {
                let _ = tx.send((method.to_string(), params));
            }),
            on_request: Box::new(|_| {}),
        },
        &state.join("logs").join("pi-master.log"),
        &env,
    );
    let mut params = json!({"unconfined": true, "turn_max_secs": 300});
    if let Ok(model) = std::env::var("CADENCE_PROBE_MODEL") {
        params["model"] = json!(model);
    }
    pi.open(&master_agent(&state, params)).unwrap();
    let briefing = cadence_agent::master::compose(&[
        (
            "SOUL.md".into(),
            std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("agents/master/SOUL.md"),
            )
            .unwrap(),
        ),
        (
            "AGENT.md".into(),
            std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("agents/master/AGENT.md"),
            )
            .unwrap(),
        ),
    ]);
    let prompt = format!("{briefing}\n\nreport status of our projects");
    let started = Instant::now();
    let turn = pi.run_turn(&prompt, "probe-1", &|_| {}).unwrap();
    let wall = started.elapsed();
    let events = collect(&rx, Duration::from_secs(2));
    let calls = events
        .iter()
        .filter(|(m, _)| m == "cadence/tool_use")
        .count();
    let refused = events
        .iter()
        .filter(|(m, p)| m == "cadence/tool_result" && p["is_error"].as_bool() == Some(true))
        .count();
    let summary = json!({
        "wall_secs": wall.as_secs_f64(),
        "tool_calls": calls,
        "refusals": refused,
        "status": turn.status,
    });
    println!("PROBE {}", serde_json::to_string(&summary).unwrap());
    if let Ok(out) = std::env::var("CADENCE_PROBE_OUT") {
        std::fs::write(out, summary.to_string()).unwrap();
    }
    pi.close();
}

/// CAD-322 round 2/3 (I4): close → reopen → first turn, with the race
/// actually open. The `linger` fake forks a grandchild that keeps the
/// generation-1 stdout pipe open ~1.5 s after the parent exits, so
/// `close()` returns while the old reader is still waiting for EOF —
/// and the stale EOF lands inside generation 2's lifetime. The
/// generation tag must drop it; without the tag the reopened session
/// is marked dead and the first turn ends
/// `OutcomeUnknown("Pi process exited before the turn settled")`.
#[test]
fn close_then_reopen_then_first_turn_completes() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("linger", dir.path());
    // (A worker alias resumes its session file — the generation race
    // is what this test pins, not session freshness.)
    let _first = pi.open(&agent("dev-1", json!({}))).unwrap();
    pi.close();
    // The parent is reaped but a grandchild still holds its stdout —
    // the old reader has not seen EOF yet. Reopen inside that window.
    let _second = pi.open(&agent("dev-1", json!({}))).unwrap();
    // Wait until the grandchild let the pipe go — the marker file is
    // written the instant before — plus a beat for the stale EOF to
    // reach the reader thread inside this generation's lifetime.
    let marker = dir.path().join("linger-eof");
    for _ in 0..100 {
        if marker.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        marker.exists(),
        "the linger grandchild never released stdout"
    );
    std::thread::sleep(Duration::from_millis(200));
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

// ---- CAD-559: the model gate and provider-package pins ----

/// No model on the row means no launch — a pi argv without `--model`
/// would run whatever the provider falls back to.
#[test]
fn open_refuses_a_missing_model() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("normal", dir.path());
    let err = pi
        .open(&agent("dev-1", json!({"model": null})))
        .err()
        .expect("a pi launch without a model must refuse");
    assert!(err.to_string().contains("no model"), "{err}");
    pi.close();
}

/// A model off `[pi].models.allow` is refused at open — the register
/// and `agent set` gates keep it off the row, and the adapter re-checks
/// the stored value every launch regardless.
#[test]
fn open_refuses_a_model_off_the_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("normal", dir.path());
    let err = pi
        .open(&agent("dev-1", json!({"model": "evil/unlisted-9"})))
        .err()
        .expect("a model off the allowlist must refuse");
    let text = err.to_string();
    assert!(
        text.contains("evil/unlisted-9") && text.contains("allowlist"),
        "{text}"
    );
    pi.close();
}

/// CAD-575: a role's own list replaces `allow` at open — a model only
/// `allow` offers refuses on the master (`master_allow` narrows it),
/// and one only `master_allow` offers refuses on the worker (its own
/// list absent → `allow` still applies). pm.yaml is rewritten after
/// the adapter helpers seed the default policy — the gate re-reads it
/// at every open.
#[test]
fn open_gates_models_per_role() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let pi = master_adapter(
        "normal",
        &state,
        &[(cadence_agent::master::TEST_NO_LANDLOCK, "1".into())],
    );
    std::fs::write(
        state.join("pm/pm.yaml"),
        "pi:\n  models:\n    allow: [\"fake/model-1\"]\n    master_allow: [\"acme/demo-1\"]\n",
    )
    .unwrap();
    // fake/model-1 is allowlisted — for workers. The master's own
    // list replaces `allow` outright.
    let err = pi
        .open(&master_agent(
            &state,
            json!({"model": "fake/model-1", "unconfined": true}),
        ))
        .err()
        .expect("a worker-side model must refuse on the master");
    let text = err.to_string();
    assert!(
        text.contains("fake/model-1") && text.contains("master_allow"),
        "{text}"
    );
    // Its own entry launches.
    pi.open(&master_agent(
        &state,
        json!({"model": "acme/demo-1", "unconfined": true}),
    ))
    .unwrap();
    pi.close();

    // The mirror: a worker under the same policy keeps `allow` and
    // may not take the master's entry.
    let wdir = tempfile::tempdir().unwrap();
    let (worker, _rx) = adapter("normal", wdir.path());
    std::fs::write(
        wdir.path().join("pm/pm.yaml"),
        "pi:\n  models:\n    allow: [\"fake/model-1\"]\n    master_allow: [\"acme/demo-1\"]\n",
    )
    .unwrap();
    let err = worker
        .open(&agent("dev-1", json!({"model": "acme/demo-1"})))
        .err()
        .expect("a master-only model must refuse on the worker");
    let text = err.to_string();
    assert!(
        text.contains("acme/demo-1") && text.contains("allow"),
        "{text}"
    );
    worker
        .open(&agent("dev-1", json!({"model": "fake/model-1"})))
        .unwrap();
    worker.close();
}

/// `worker_allow` narrows the worker side symmetrically: an `allow`
/// entry it omits refuses at the worker's open while the master —
/// never reading `worker_allow` — still launches on it.
#[test]
fn worker_allow_narrows_the_worker_open() {
    let dir = tempfile::tempdir().unwrap();
    let (worker, _rx) = adapter("normal", dir.path());
    std::fs::write(
        dir.path().join("pm/pm.yaml"),
        "pi:\n  models:\n    allow: [\"fake/model-1\", \"acme/demo-1\"]\n    worker_allow: [\"acme/demo-1\"]\n",
    )
    .unwrap();
    let err = worker
        .open(&agent("dev-1", json!({"model": "fake/model-1"})))
        .err()
        .expect("an allow-only model must refuse when worker_allow omits it");
    let text = err.to_string();
    assert!(
        text.contains("fake/model-1") && text.contains("worker_allow"),
        "{text}"
    );
    worker
        .open(&agent("dev-1", json!({"model": "acme/demo-1"})))
        .unwrap();
    worker.close();
}

/// The `/model` gate runs under the role the adapter opened as:
/// `fake/model-1` is fine for workers but not on `master_allow`, so
/// the master's switch is refused BEFORE `set_model` crosses — the
/// journal beside the master's cwd proves it — while the same switch
/// on a worker adapter lands (CAD-575).
#[test]
fn session_command_model_gates_per_role() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let pi = master_adapter(
        "normal",
        &state,
        &[(cadence_agent::master::TEST_NO_LANDLOCK, "1".into())],
    );
    std::fs::write(
        state.join("pm/pm.yaml"),
        "pi:\n  models:\n    allow: [\"fake/model-1\", \"acme/demo-1\"]\n    master_allow: [\"acme/demo-1\"]\n",
    )
    .unwrap();
    pi.open(&master_agent(
        &state,
        json!({"model": "acme/demo-1", "unconfined": true}),
    ))
    .unwrap();

    let err = pi
        .session_command("model", Some("fake/model-1"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("fake/model-1") && err.contains("master_allow"),
        "{err}"
    );
    let journal = state.join("master/cwd/pi-rpc.jsonl");
    let received = std::fs::read_to_string(&journal)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|row| row["rpc"].as_str().map(str::to_string))
        .collect::<Vec<_>>();
    assert!(!received.iter().any(|m| m == "set_model"), "{received:?}");
    pi.close();

    // Same switch on a worker adapter: `master_allow` is not its list,
    // `allow` covers it — the wire carries set_model and get_state
    // verifies.
    let wdir = tempfile::tempdir().unwrap();
    let (worker, _rx) = adapter("normal", wdir.path());
    std::fs::write(
        wdir.path().join("pm/pm.yaml"),
        "pi:\n  models:\n    allow: [\"fake/model-1\", \"acme/demo-1\"]\n    master_allow: [\"acme/demo-1\"]\n",
    )
    .unwrap();
    worker.open(&agent("dev-1", json!({}))).unwrap();
    let out = worker
        .session_command("model", Some("fake/model-1"))
        .unwrap();
    assert_eq!(out["model"]["id"], "model-1", "{out}");
    worker.close();
}

/// CAD-602: neither an allowed model nor forged row fields can move
/// the guard-dependent master onto a provider executing its own tools.
#[test]
fn agentic_provider_master_open_and_concurrent_switch_refuse() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let pi = master_adapter(
        "normal",
        &state,
        &[(cadence_agent::master::TEST_NO_LANDLOCK, "1".into())],
    );
    std::fs::write(
        state.join("pm/pm.yaml"),
        "pi:\n  models:\n    allow: [\"fake/model-1\", \"cursor/grok-4.7-high\"]\n",
    )
    .unwrap();
    let mut forged = master_agent(
        &state,
        json!({"model": "cursor/grok-4.7-high", "role": "worker", "unconfined": true}),
    );
    forged.role = "worker".into();
    let err = pi.open(&forged).unwrap_err().to_string();
    assert!(err.contains("agentic"), "{err}");
    assert!(!state.join("master/cwd/pi-rpc.jsonl").exists());

    pi.open(&master_agent(
        &state,
        json!({"model": "fake/model-1", "unconfined": true}),
    ))
    .unwrap();
    std::thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| {
                let err = pi
                    .session_command("model", Some("cursor/grok-4.7-high"))
                    .unwrap_err()
                    .to_string();
                assert!(err.contains("agentic"), "{err}");
            });
        }
    });
    let journal = std::fs::read_to_string(state.join("master/cwd/pi-rpc.jsonl")).unwrap();
    assert!(
        !journal.lines().any(|line| {
            serde_json::from_str::<Value>(line).unwrap()["rpc"] == "set_model"
        }),
        "{journal}"
    );
    pi.close();
}

/// The `wrong-model` fake accepts `--model` then reports a different
/// one — Pi's silent-fallback shape. `open` must refuse rather than
/// trust the launch flag.
#[test]
fn a_provider_reporting_the_wrong_model_fails_the_open() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("wrong-model", dir.path());
    let err = pi
        .open(&agent("dev-1", json!({"model": "fake/model-1"})))
        .err()
        .expect("a model mismatch must fail the open");
    let text = err.to_string();
    assert!(
        text.contains("fake/model-1") && text.contains("not-the-asked-1"),
        "{text}"
    );
    pi.close();
}

/// A pinned `[pi].providers` package contributes its `pi.extensions`
/// files as `-e` argv entries — the only extensions `--no-extensions`
/// admits — and its dir joins the confinement read set, read-only.
#[test]
fn pinned_provider_packages_extend_argv_and_the_read_set() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    // The OPERATOR's install, pinned and complete:
    // <PI_CODING_AGENT_DIR>/npm/node_modules/pi-devin.
    let operator = dir.path().join("operator-pi");
    let pkg = operator.join("npm/node_modules/pi-devin");
    std::fs::create_dir_all(pkg.join("extensions")).unwrap();
    std::fs::write(
        pkg.join("package.json"),
        r#"{"version":"0.1.2","pi":{"extensions":["./extensions/index.ts","./extensions/extra.js"]}}"#,
    )
    .unwrap();
    for entry in ["extensions/index.ts", "extensions/extra.js"] {
        std::fs::write(pkg.join(entry), "// provider ext").unwrap();
    }
    let pm = state.join("pm");
    std::fs::create_dir_all(&pm).unwrap();
    std::fs::write(
        pm.join("pm.yaml"),
        "pi:\n  providers: [\"pi-devin@0.1.2\"]\n  models:\n    allow: [\"fake/model-1\"]\n",
    )
    .unwrap();
    let pi = master_adapter(
        "normal",
        &state,
        &[
            (cadence_agent::master::TEST_NO_LANDLOCK, "1".into()),
            ("PI_CODING_AGENT_DIR", operator.to_string_lossy().into()),
        ],
    );
    pi.open(&master_agent(&state, json!({"unconfined": true})))
        .unwrap();
    let argv = recorded_argv(&state);
    for entry in ["extensions/index.ts", "extensions/extra.js"] {
        let want = pkg.join(entry).to_string_lossy().to_string();
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--extension" && w[1] == want),
            "argv lacks -e {want}: {argv:?}"
        );
    }
    assert!(
        argv.iter().any(|a| a == "--no-extensions"),
        "--no-extensions must survive: {argv:?}"
    );
    // The guard is still the first extension.
    let first = argv
        .windows(2)
        .find(|w| w[0] == "--extension")
        .map(|w| w[1].clone())
        .unwrap();
    assert!(first.contains("pi-guard"), "{argv:?}");
    pi.close();

    // The same resolution feeds confinement: the package dir is in the
    // read set (never write), nothing wider than it.
    let env = ProviderEnv::default();
    env.set("CADENCE_PI_COMMAND", fake_pi("normal"));
    env.set("CADENCE_PM_DIR", pm.to_string_lossy().to_string());
    env.set(
        "PI_CODING_AGENT_DIR",
        operator.to_string_lossy().to_string(),
    );
    let (_cmd, policy) = cadence_agent::adapter::pi::pi_master_confinement(&env, &state);
    assert!(
        policy.read.iter().any(|p| p.starts_with(&pkg)),
        "read set lacks the pinned package dir: {policy:?}"
    );
    assert!(
        !policy.write.iter().any(|p| p.starts_with(&pkg)),
        "the package dir is writable: {policy:?}"
    );
}

/// Installed 0.2.0 against a 0.1.2 pin refuses — the loadable neighbor
/// is never "close enough".
#[test]
fn provider_package_version_drift_refuses_open() {
    let dir = tempfile::tempdir().unwrap();
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&pm).unwrap();
    std::fs::write(
        pm.join("pm.yaml"),
        "pi:\n  providers: [\"pi-devin@0.1.2\"]\n  models:\n    allow: [\"fake/model-1\"]\n",
    )
    .unwrap();
    let operator = dir.path().join("operator-pi");
    let pkg = operator.join("npm/node_modules/pi-devin");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("package.json"),
        r#"{"version":"0.2.0","pi":{"extensions":["./e.ts"]}}"#,
    )
    .unwrap();
    std::fs::write(pkg.join("e.ts"), "// drifted").unwrap();
    let (pi, _rx) = adapter_env(
        "normal",
        dir.path(),
        &[("PI_CODING_AGENT_DIR", operator.to_string_lossy().into())],
    );
    let err = pi
        .open(&agent("dev-1", json!({"model": "fake/model-1"})))
        .err()
        .expect("a version drift must refuse");
    let text = err.to_string();
    assert!(text.contains("0.2.0") && text.contains("0.1.2"), "{text}");
    pi.close();
}

/// A pinned package absent from the operator's npm dir refuses.
#[test]
fn a_missing_provider_package_refuses_open() {
    let dir = tempfile::tempdir().unwrap();
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&pm).unwrap();
    std::fs::write(
        pm.join("pm.yaml"),
        "pi:\n  providers: [\"ghost@1.0.0\"]\n  models:\n    allow: [\"fake/model-1\"]\n",
    )
    .unwrap();
    let operator = dir.path().join("operator-pi");
    std::fs::create_dir_all(operator.join("npm/node_modules")).unwrap();
    let (pi, _rx) = adapter_env(
        "normal",
        dir.path(),
        &[("PI_CODING_AGENT_DIR", operator.to_string_lossy().into())],
    );
    let err = pi
        .open(&agent("dev-1", json!({"model": "fake/model-1"})))
        .err()
        .expect("a missing package must refuse");
    assert!(err.to_string().contains("ghost"), "{err}");
    pi.close();
}

/// Daemon-level (`PlanFixture`, real tracker): `master start --provider
/// pi` resolves its model through the operator's gate — no `[pi]` at
/// all refuses, an explicit `--model` off the allowlist refuses, and
/// AGENT.md's `preferred` pi model is the fallback that lands.
#[test]
fn master_start_resolves_the_pi_model_gate() {
    let f = common::PlanFixture::start();
    common::test_env().set(cadence_agent::master::TEST_NO_LANDLOCK, "1");
    // Leg 1: no `[pi]` table — even a bare start has no legal model.
    // (mock_pi would append the suite's default table, so it comes
    // only after this leg.)
    let err =
        f.d.operator_rpc(
            "master_start",
            json!({"provider": "pi", "unconfined": true}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("no model"), "{err}");
    // A tracker policy arrives: the allowlist binds, the role default
    // fills a bare start.
    let pm_yaml = f.pm_dir.join("pm.yaml");
    let mut yaml = std::fs::read_to_string(&pm_yaml).unwrap();
    yaml.push_str(
        "pi:\n  models:\n    allow: [\"fake/model-1\", \"devin/swe-2-high\"]\n    default: {master: \"fake/model-1\", worker: \"fake/model-1\"}\n",
    );
    std::fs::write(&pm_yaml, yaml).unwrap();
    let _pi = f.d.mock_pi("normal");
    // Leg 2: explicit --model off the list — refused, no row stored.
    let err =
        f.d.operator_rpc(
            "master_start",
            json!({"provider": "pi", "unconfined": true, "model": "evil/unlisted-9"}),
        )
        .unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("evil/unlisted-9") && text.contains("allowlist"),
        "{text}"
    );
    assert!(f.d.rpc("agent_show", json!({"alias": "master"})).is_err());
    // Leg 3: AGENT.md prefers pi on an allowlisted model — it becomes
    // the launch model over the role default. Agent files have one
    // writer (CAD-339): a hand edit would refuse the brief, so this
    // goes through `agent_file_write` like the operator's editor.
    f.d.operator_rpc(
        "agent_file_write",
        json!({"agent": "master", "file": "AGENT.md", "text":
            "---\nname: master\npreferred: {provider: pi, model: devin/swe-2-high}\nfallbacks: []\n---\n# Master\n\nThe test master.\n"}),
    )
    .unwrap();
    let out =
        f.d.operator_rpc(
            "master_start",
            json!({"provider": "pi", "unconfined": true}),
        )
        .unwrap();
    assert_eq!(out["alias"], "master", "{out}");
    let show = f.d.rpc("agent_show", json!({"alias": "master"})).unwrap();
    assert_eq!(
        show["agent"]["params"]["model"], "devin/swe-2-high",
        "{show}"
    );
    // The flag and the lockdown reached the child argv.
    let argv_file = f.d.state.join("master/cwd/pi-argv.json");
    for _ in 0..100 {
        if argv_file.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let argv: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(&argv_file).unwrap()).unwrap();
    assert!(
        argv.windows(2)
            .any(|w| w == ["--model", "devin/swe-2-high"]),
        "{argv:?}"
    );
    assert!(argv.iter().any(|a| a == "--no-extensions"), "{argv:?}");
    // And get_state verified the launch: the reported model is the
    // requested one, never a provider fallback.
    assert_eq!(
        show["agent"]["model"].as_str(),
        Some("devin/swe-2-high"),
        "{show}"
    );
}

// ---- CAD-551: the operator's provider-session verbs ----

/// `session_commands` declares exactly the verbs the daemon's
/// `master_command` allowlist maps onto provider RPCs.
#[test]
fn session_commands_declares_the_pi_set() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("normal", dir.path());
    assert_eq!(
        pi.session_commands(),
        &["state", "stats", "models", "model", "levels", "effort", "compact", "new", "stop"]
    );
    pi.close();
}

/// Read verbs before any turn: `state` carries the session's model and
/// thinking level, `stats` the context usage, `models`/`levels` the
/// offer lists — the header chips' data source.
#[test]
fn session_command_reads_state_stats_and_lists() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("normal", dir.path());
    pi.open(&agent("dev-1", json!({}))).unwrap();

    let state = pi.session_command("state", None).unwrap();
    assert_eq!(state["model"]["id"], "model-1", "{state}");
    assert_eq!(state["model"]["provider"], "fake", "{state}");
    assert_eq!(state["thinkingLevel"], "medium", "{state}");

    let stats = pi.session_command("stats", None).unwrap();
    assert_eq!(stats["contextUsage"]["contextWindow"], 200000, "{stats}");
    assert!(stats["contextUsage"]["tokens"].is_number(), "{stats}");

    let models = pi.session_command("models", None).unwrap();
    assert!(models["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["id"] == "model-1" && m["provider"] == "fake"));

    let levels = pi.session_command("levels", None).unwrap();
    assert!(levels["levels"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l == "high"));

    let err = pi.session_command("exec", None).unwrap_err().to_string();
    assert!(err.contains("not supported"), "{err}");
    pi.close();
}

/// `/model` with no arg lists; `/model provider/id` to an allowlisted
/// model switches and the answer reports both sides; a bare id on both
/// the provider list AND the allowlist resolves; an unlisted bare id
/// is refused with a pointer at `/models`.
#[test]
fn session_command_model_switches_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("normal", dir.path());
    pi.open(&agent("dev-1", json!({}))).unwrap();

    let listed = pi.session_command("model", None).unwrap();
    assert!(listed["models"].as_array().unwrap().len() >= 2, "{listed}");

    let out = pi.session_command("model", Some("acme/demo-1")).unwrap();
    assert_eq!(out["was"], "model-1", "{out}");
    assert_eq!(out["model"]["id"], "demo-1", "{out}");
    assert_eq!(out["model"]["provider"], "acme", "{out}");
    assert_eq!(out["requested"], "acme/demo-1", "{out}");

    // The change is real — the next `state` answers the new model.
    let state = pi.session_command("state", None).unwrap();
    assert_eq!(state["model"]["id"], "demo-1", "{state}");

    // A bare id found on the provider's own list resolves (demo-1 →
    // acme/demo-1, which the allowlist pins).
    let out = pi.session_command("model", Some("demo-1")).unwrap();
    assert_eq!(out["model"]["provider"], "acme", "{out}");
    assert_eq!(out["requested"], "acme/demo-1", "{out}");

    // A bare id nothing offers is refused, naming `/models`.
    let err = pi
        .session_command("model", Some("no-such-model"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("/models"), "{err}");
    pi.close();
}

/// `/model` to a model off `[pi].models.allow` is refused BEFORE
/// `set_model` crosses the wire — the fake's request journal proves
/// the RPC never left the adapter (CAD-559).
#[test]
fn session_command_model_refuses_an_offlist_model() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("normal", dir.path());
    pi.open(&agent("dev-1", json!({}))).unwrap();

    let err = pi
        .session_command("model", Some("fake/model-2"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("fake/model-2") && err.contains("allowlist"),
        "{err}"
    );

    // The wire never carried set_model: the fake's per-request journal
    // under <state>/agents/ names every RPC it actually received.
    let journal = dir.path().join("agents/pi-rpc-dev-1.jsonl");
    let received = std::fs::read_to_string(&journal)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|row| row["rpc"].as_str().map(str::to_string))
        .collect::<Vec<_>>();
    assert!(!received.iter().any(|m| m == "set_model"), "{received:?}");
    // And the session still reports the launch model.
    let state = pi.session_command("state", None).unwrap();
    assert_eq!(state["model"]["id"], "model-1", "{state}");
    pi.close();
}

/// No `[pi]` table in pm.yaml is an EMPTY allowlist — `/model`
/// refuses even a provider-listed model. The session opens under the
/// default policy, then the table is removed mid-session: the gate
/// re-reads the file on every call (CAD-559).
#[test]
fn session_command_model_refuses_without_pi_policy() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("normal", dir.path());
    pi.open(&agent("dev-1", json!({}))).unwrap();

    // The policy the open was gated under is gone — `pi_policy::read`
    // answers None, an allowlist of nothing.
    std::fs::remove_file(dir.path().join("pm/pm.yaml")).unwrap();

    let err = pi
        .session_command("model", Some("acme/demo-1"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("acme/demo-1") && err.contains("allowlist"),
        "{err}"
    );
    pi.close();
}

/// `/model`'s switch is verified against `get_state`, not the ack —
/// `model-drift` acks `set_model` then reports `fake/fell-back`, and
/// the command must fail the way `open` does on a silent fallback.
#[test]
fn session_command_model_fails_on_a_silent_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("model-drift", dir.path());
    pi.open(&agent("dev-1", json!({}))).unwrap();

    let err = pi
        .session_command("model", Some("acme/demo-1"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("acme/demo-1") && err.contains("fell-back"),
        "{err}"
    );
    pi.close();
}

/// `/effort` is NOT part of the model allowlist — levels set and
/// verify exactly as before (CAD-559 scopes the gate to models).
#[test]
fn session_command_effort_is_not_model_gated() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("normal", dir.path());
    pi.open(&agent("dev-1", json!({}))).unwrap();
    let out = pi.session_command("effort", Some("high")).unwrap();
    assert_eq!(out["level"], "high", "{out}");
    pi.close();
}

/// `/effort` with no arg lists the levels plus the live one; with a
/// level it sets AND verifies — a bogus level surfaces as an error
/// naming what stayed.
#[test]
fn session_command_effort_sets_and_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("normal", dir.path());
    pi.open(&agent("dev-1", json!({}))).unwrap();

    let levels = pi.session_command("effort", None).unwrap();
    assert_eq!(levels["current"], "medium", "{levels}");
    assert!(levels["levels"].as_array().unwrap().len() >= 5, "{levels}");

    let out = pi.session_command("effort", Some("high")).unwrap();
    assert_eq!(out["level"], "high", "{out}");

    // fake-pi falls back to `off` on a bogus level, like real Pi — the
    // adapter's verification refuses the silent lie.
    let err = pi
        .session_command("effort", Some("bogus"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("pi refused effort 'bogus'"), "{err}");
    pi.close();
}

/// `compact` answers the provider's compaction result; `new` mints a
/// fresh session id inside the same process and answers the new state.
#[test]
fn session_command_compact_and_new_session() {
    let dir = tempfile::tempdir().unwrap();
    let (pi, _rx) = adapter("normal", dir.path());
    pi.open(&agent("dev-1", json!({}))).unwrap();

    let before = pi.session_command("state", None).unwrap();
    let out = pi.session_command("compact", None).unwrap();
    assert_eq!(out["compacted"], true, "{out}");

    let out = pi.session_command("new", None).unwrap();
    let fresh = out["state"]["sessionId"].as_str().unwrap();
    assert_ne!(fresh, before["sessionId"].as_str().unwrap(), "{out}");
    pi.close();
}
