#![cfg(feature = "test-seam")]
//! CAD-1624 independent refusal acceptance, authored from the ticket by an
//! author other than the implementer. The implementation author must not edit
//! or weaken this file.
//!
//! omp is a managed-rpc endpoint that rides `PiAdapter::open`. The Settings
//! allowlist gate (`pi_policy::require_allowed` against the daemon-bound
//! settings store) must refuse an omp agent exactly as it refuses a pi agent.
//! These checks drive the REAL `adapter::build` ("omp" arm) and the REAL
//! `PiAdapter::open` against a REAL `Store` whose `[pi]` policy is imported
//! from a real `pm.yaml`. No mock gate, no provider binary.
//!
//! Proven bad cases:
//! - an omp agent whose launch model is off the operator's allowlist is
//!   refused by the real gate; the refusal names the allowlist and the
//!   Settings edit point; no provider process is spawned (a sentinel program
//!   in CADENCE_PI_COMMAND would create a marker file if exec'd);
//! - an omp agent with no configured model (absent params, `{}`, blank model)
//!   is refused by the adapter's own missing-model check before the allowlist,
//!   and no provider process is spawned.
//!
//! Dependencies that make the bypass observable (stated, asserted directly):
//! - the omp registry spec carries `managed_pi_rpc` and
//!   `registry::pi_family_managed("omp", "managed")` is true. If either stops
//!   covering omp, the Settings gate no longer treats omp as a gated harness.
//! - the omp arm of `adapter::build` must construct `PiAdapter`. Otherwise
//!   `open` would not produce the Pi gate's refusal text and the message
//!   assertions below fail.
//!
//! Run only in an admitted build slot:
//! `cadence build-slot run test --org local -- env CARGO_BUILD_JOBS=4 cargo test --features test-seam --test cad1624_omp_gate_refusal_acceptance -- --test-threads 1`

use cadence_agent::adapter::registry;
use cadence_agent::adapter::{build, AdapterHooks, ProviderAdapter, ProviderEnv, ProviderRequest};
use cadence_agent::store::{Agent, Store};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The only model on the operator's allowlist for this fixture.
const ALLOWED: &str = "devin/claude-haiku-5-5";
/// omp's default model role from the ticket. Not on the allowlist.
const OFF_LIST: &str = "commandcode/deepseek/deepseek-v4.1-flash:high";

struct Fixture {
    _root: tempfile::TempDir,
    marker: PathBuf,
    log: PathBuf,
    cwd: String,
    env: ProviderEnv,
}

fn fixture() -> Fixture {
    let root = tempfile::Builder::new()
        .prefix("cad1624-omp-gate-")
        .tempdir()
        .unwrap();

    // Operator's legacy pm.yaml: the Pi policy the daemon imports into the
    // settings authority. `PiPolicy` denies unknown fields, so only `pi:` here.
    let pm_dir = root.path().join("pm");
    std::fs::create_dir_all(&pm_dir).unwrap();
    std::fs::write(
        pm_dir.join("pm.yaml"),
        format!("pi:\n  models:\n    allow: [{ALLOWED}]\n"),
    )
    .unwrap();

    let state = root.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
    store.import_legacy_settings(&pm_dir).unwrap();
    let policy = store
        .pi_policy()
        .unwrap()
        .expect("the imported [pi] policy must be present");
    assert_eq!(
        policy.models.allow,
        vec![ALLOWED.to_string()],
        "fixture allowlist did not import"
    );

    // Sentinel provider: if the adapter ever execs it, the marker appears.
    let marker = root.path().join("provider-spawned");
    let sentinel = root.path().join("omp-sentinel");
    std::fs::write(
        &sentinel,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // `refusing_providers` seals every provider command; the omp arm keeps an
    // explicit non-`pi` command, so the sentinel is what would run if the gate
    // were bypassed.
    let env = ProviderEnv::refusing_providers()
        .with_var("CADENCE_PI_COMMAND", sentinel.to_str().unwrap())
        .with_settings_store(Arc::new(store));

    let cwd = root.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    Fixture {
        log: root.path().join("logs").join("omp.log"),
        cwd: cwd.to_str().unwrap().to_string(),
        marker,
        env,
        _root: root,
    }
}

fn omp_agent(cwd: &str, params: Option<Value>) -> Agent {
    Agent {
        alias: "cad1624-omp".into(),
        uid: "uid-cad1624-omp".into(),
        provider: "omp".into(),
        endpoint_kind: "managed".into(),
        role: "worker".into(),
        team_role: Some("dev".into()),
        cwd: cwd.into(),
        sandbox: "read-only".into(),
        instructions: None,
        thread_id: None,
        session_id: None,
        model: None,
        effort: None,
        pid: None,
        pid_start: None,
        endpoint: None,
        params,
        model_selection: None,
        quota: None,
        generation: None,
        state: "stopped".into(),
        enabled: true,
        gc_keep: false,
        error: None,
        created: 0.0,
        updated: 0.0,
    }
}

/// Build through the production registry arm and open. Returns the refusal
/// text; panics if the open succeeds.
fn open_omp_refusal(f: &Fixture, agent: &Agent) -> String {
    let hooks = AdapterHooks {
        on_event: Box::new(|_: &str, _: Value| {}),
        on_request: Box::new(|_: ProviderRequest| {}),
    };
    let adapter: Box<dyn ProviderAdapter> = build(agent, hooks, &f.log, &f.env, None)
        .expect("the omp arm must build the managed adapter");
    match adapter.open(agent) {
        Ok(_) => panic!("omp agent opened without any settings gate refusal"),
        Err(err) => err.to_string(),
    }
}

fn assert_no_spawn(f: &Fixture) {
    assert!(
        !Path::new(&f.marker).exists(),
        "a provider process was spawned before the refusal (marker {})",
        f.marker.display()
    );
}

/// Dependency assertions: omp is a gated managed-rpc harness.
fn assert_omp_is_pi_family() {
    let spec = registry::spec("omp", "managed").expect("omp must be a registered managed spec");
    assert!(
        spec.capabilities.contains(&"managed_pi_rpc"),
        "omp spec lost the managed_pi_rpc capability; the Settings gate no longer covers it"
    );
    assert!(
        registry::pi_family_managed("omp", "managed"),
        "pi_family_managed stopped covering omp; the allowlist gate is bypassed"
    );
}

#[test]
fn omp_model_off_the_allowlist_is_refused_before_any_provider_spawn() {
    assert_omp_is_pi_family();
    let f = fixture();
    let agent = omp_agent(&f.cwd, Some(json!({ "model": OFF_LIST })));

    let refusal = open_omp_refusal(&f, &agent);

    assert!(
        refusal.contains("not on the operator's allowlist"),
        "refusal did not come from the allowlist gate: {refusal}"
    );
    assert!(
        refusal.contains(OFF_LIST),
        "refusal did not name the refused model: {refusal}"
    );
    assert!(
        refusal.contains(ALLOWED),
        "refusal did not name the allowlist: {refusal}"
    );
    assert!(
        refusal.contains("Settings > Agents"),
        "refusal did not point the operator at Settings > Agents: {refusal}"
    );
    assert_no_spawn(&f);
}

#[test]
fn omp_agent_with_no_configured_model_is_refused_by_the_adapter_before_any_spawn() {
    assert_omp_is_pi_family();
    let f = fixture();
    for params in [None, Some(json!({})), Some(json!({ "model": "   " }))] {
        let agent = omp_agent(&f.cwd, params.clone());

        let refusal = open_omp_refusal(&f, &agent);

        assert!(
            refusal.contains("has no model configured"),
            "missing model was not refused by the adapter (params {params:?}): {refusal}"
        );
        assert!(
            !refusal.contains("allowlist"),
            "missing model reached the allowlist check (params {params:?}): {refusal}"
        );
        assert_no_spawn(&f);
    }
}
