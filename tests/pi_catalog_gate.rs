//! CAD-601 acceptance check (reviewer-written): the Pi open must
//! refuse a model the provider's own catalog cannot vouch for, with
//! zero prompts crossing RPC. A real daemon starts a real Pi master
//! over `tests/fixtures/pi-gateway.py` (Pi's RPC lines; its
//! `get_available_models` reply is selected per mode) and the fake's
//! journal proves `get_available_models` ran and no `prompt` was sent.
//! Refused: an unlisted id, the id under another provider, a forged
//! `thinkingLevelMap`, `models: []`, a missing `models` key, a failed
//! reply and a non-array `models`. The consistent catalog runs.
#![cfg(feature = "test-seam")]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cadence_agent::adapter::ProviderEnv;
use cadence_agent::test_seam::{scoped, Asserted};
use cadence_agent::{client, daemon, master, reaper, slots::SlotConfig};
use serde_json::{json, Value};
use tempfile::Builder;

fn run(mode: &str) -> (Option<Value>, Vec<Value>, String) {
    // Under the seam's temp root ($TMPDIR in CI), not a hardcoded /tmp:
    // arm() refuses a state dir outside std::env::temp_dir().
    let root = Builder::new().prefix("p601-").tempdir().unwrap();
    let [state, pm, home] = ["s", "pm", "h"].map(|d| root.path().join(d));
    let mut init = Command::new(env!("CARGO_BIN_EXE_cadence"));
    init.args(["issue", "init"])
        .env("CADENCE_PM_DIR", &pm)
        .env("HOME", &home)
        .env_remove("CADENCE_ALIAS");
    assert!(reaper::output(&mut init).unwrap().status.success());
    let yaml = pm.join("pm.yaml");
    let mut text = std::fs::read_to_string(&yaml).unwrap();
    text.push_str("\npi:\n  models:\n    allow: [\"fake/model-1\"]\n    default: {master: \"fake/model-1\", worker: \"fake/model-1\"}\n");
    std::fs::write(&yaml, text).unwrap();
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi-gateway.py");
    let env = ProviderEnv::refusing_providers();
    env.set(
        "CADENCE_PI_COMMAND",
        format!("python3 {} {mode}", fake.display()),
    );
    env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
    env.set("HOME", home.to_str().unwrap());
    env.set(master::TEST_NO_LANDLOCK, "1");
    let stop = Arc::new(AtomicBool::new(false));
    let opts = daemon::ServeOptions {
        provider_env: env,
        stop: Some(stop.clone()),
        slots: Some(SlotConfig::default()),
        test_seam: true,
        ..Default::default()
    };
    let dir: PathBuf = state.clone();
    let serve: JoinHandle<cadence_agent::Result<()>> =
        thread::spawn(move || daemon::serve_with(&dir, opts));
    let deadline = Instant::now() + Duration::from_secs(30);
    while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(1)).is_err() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(50));
    }
    let started = scoped(Asserted::Operator, || {
        client::rpc(
            &state,
            "master_start",
            json!({"provider": "pi", "unconfined": true}),
        )
    });
    let mut detail = format!("master_start: {started:?}");
    let mut boot = None;
    if started.is_ok() {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let show = client::rpc(&state, "agent_show", json!({"alias": "master"})).unwrap();
            let m = show["messages"][0].clone();
            if matches!(m["state"].as_str(), Some("completed" | "failed"))
                || Instant::now() > deadline
            {
                detail.push_str(&format!(" agent: {}", show["agent"]));
                boot = Some(m);
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    let journal: Vec<Value> = std::fs::read_to_string(master::workdir(&state).join("pi-rpc.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let _ = client::rpc_timeout(&state, "shutdown", json!({}), Duration::from_secs(5));
    stop.store(true, Ordering::SeqCst);
    let d = Instant::now() + Duration::from_secs(15);
    while !serve.is_finished() && Instant::now() < d {
        thread::sleep(Duration::from_millis(50));
    }
    (boot, journal, detail)
}

fn prompts(j: &[Value]) -> usize {
    j.iter().filter(|r| r["rpc"] == "prompt").count()
}

/// The consistent catalog case runs: the catalog call happened, the
/// bootstrap prompt crossed and the master's first turn completed.
#[test]
fn a_catalog_backed_model_opens_and_completes() {
    let (boot, j, d) = run("ok");
    eprintln!("OK: {d} boot={boot:?} journal={j:?}");
    assert!(j.iter().any(|r| r["rpc"] == "get_available_models"));
    assert_eq!(boot.unwrap()["state"], "completed");
    assert!(prompts(&j) >= 1);
}

/// Every bad catalog is refused at open, before any prompt: the
/// catalog call still ran (the guard was reached), zero prompts
/// crossed and the bootstrap message never completed.
#[test]
fn an_uncataloged_or_mismatched_model_is_refused_without_a_prompt() {
    for mode in [
        "absent",
        "other_provider",
        "forged",
        "empty",
        "missing",
        "fail",
        "notlist",
    ] {
        let (boot, j, d) = run(mode);
        eprintln!("MODE {mode}: {d} boot={boot:?} journal={j:?}");
        assert_eq!(prompts(&j), 0, "{mode}: a prompt crossed: {j:?}");
        assert!(
            j.iter().any(|r| r["rpc"] == "get_available_models"),
            "{mode}: guard not reached"
        );
        if let Some(b) = boot {
            assert_ne!(b["state"], "completed", "{mode}: {b}");
        }
    }
}
