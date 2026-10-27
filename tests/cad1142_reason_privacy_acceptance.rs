//! CAD-1142 independent bad-case acceptance, authored by qa-sol-1142.
//!
//! A failed app turn must retain a human-readable reason without publishing
//! arbitrary provider error prose or credentials. Both unproven and agent
//! readers exercise the real daemon events RPC; the operator's run-show RPC
//! must also return a nonempty, credential-free step reason.
//!
//! Setup uses public store APIs to seed one synthetic failed app completion,
//! then starts the existing isolated, armed test-seam daemon. This tests the
//! completion/projection/read boundary, not real worker admission or provider
//! transport. No real credential, live provider, or agent process is used.
#![cfg(feature = "test-seam")]

use cadence_agent::adapter::{Identity, ProviderEnv};
use cadence_agent::store::app_runs::{LocalRunRequest, LocalWorkflow};
use cadence_agent::store::{NewAgent, Store};
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{client, daemon};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SENTINEL: &str = "qa1142_SYNTHETIC_SECRET_NOT_REAL";
const PROVIDER_PROSE: &str = "provider rejected request";
const WORKFLOW: &str = r#"---
title: Local
goal: Retain text
---
## Write
agent: writer
action: local.text.produce

Write Markdown.

### Acceptance
- [ ] Markdown exists
"#;

struct Fx {
    root: tempfile::TempDir,
    run_id: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Fx {
    fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("c1142priv")
            .tempdir()
            .unwrap();
        let state = root.path().join("s");
        let pm = root.path().join("pm");
        cadence_agent::issue::Pm::init(&pm).unwrap();
        let run_id = seed_failed_turn(&state, root.path());
        let stop = Arc::new(AtomicBool::new(false));
        let env = ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let options = daemon::ServeOptions {
            provider_env: env,
            stop: Some(stop.clone()),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        let thread = std::thread::spawn(move || daemon::serve_with(&state, options).unwrap());
        let fx = Self {
            root,
            run_id,
            stop,
            thread: Some(thread),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&fx.state(), "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&fx.state()).is_none()
        {
            assert!(Instant::now() < deadline, "daemon never started");
            std::thread::sleep(Duration::from_millis(50));
        }
        fx
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("s")
    }

    fn rpc(&self, who: Asserted, method: &str, params: Value) -> Value {
        scoped(who, || client::rpc(&self.state(), method, params)).unwrap()
    }
}

/// Seed the same store completion path used for a managed provider's failed
/// turn. The synthetic error is deliberately supplied as both result.error
/// and the persisted message error; no private SQL or caller bypass is used.
fn seed_failed_turn(state: &Path, cwd: &Path) -> String {
    std::fs::create_dir_all(state).unwrap();
    let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
    for (alias, role) in [("lead", "pm"), ("writer", "worker")] {
        store
            .register_agent(&NewAgent {
                alias,
                provider: "claude",
                endpoint_kind: "managed",
                role,
                cwd: cwd.to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: Some("{\"upstream\":\"lead\"}"),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        store
            .set_identity(
                alias,
                &Identity {
                    thread_id: "t".into(),
                    session_id: "s".into(),
                    model: None,
                    effort: None,
                    pid: std::process::id(),
                    endpoint: None,
                    generation: Some("g1".into()),
                    attach: None,
                },
            )
            .unwrap();
    }
    let inputs = std::collections::BTreeMap::new();
    let workflow = LocalWorkflow::parse(WORKFLOW, &inputs).unwrap();
    store
        .app_capability_decide("install-1", "sha256:bundle", true)
        .unwrap();
    let run = store
        .app_run_create(LocalRunRequest {
            install_id: "install-1",
            bundle_digest: "sha256:bundle",
            workflow: &workflow,
            inputs: &inputs,
            request_id: "reason-privacy",
            owner_pm: Some("lead"),
            project_link: None,
        })
        .unwrap();
    let id = run["id"].as_str().unwrap().to_string();
    store
        .app_run_decide(
            &id,
            run["snapshot_digest"].as_str(),
            false,
            Some("sha256:bundle"),
        )
        .unwrap();
    let dispatched = store.app_run_dispatch(&id, "sha256:bundle").unwrap();
    let message_id = dispatched["steps"][0]["message_id"].as_str().unwrap();
    store.mark_running(message_id, "synthetic-turn").unwrap();
    let message = store.message(message_id).unwrap().unwrap();
    let error = format!("{PROVIDER_PROSE}; Authorization: Bearer {SENTINEL}");
    store
        .finish(
            &message,
            "failed",
            &json!({"status": "failed", "turn_id": "synthetic-turn", "error": error}),
            Some(&error),
        )
        .unwrap();
    id
}

fn contains_private_prose(text: &str) -> bool {
    text.contains(SENTINEL) || text.contains(PROVIDER_PROSE)
}

#[test]
fn failed_turn_reason_is_safe_on_run_show_and_public_daemon_events() {
    let fx = Fx::start();
    let shown = fx.rpc(
        Asserted::Operator,
        "app_run_show",
        json!({"run_id": fx.run_id}),
    );
    // Positive completion witnesses are obtained through the real read RPC,
    // not inferred from the fact that the fixture called finish().
    assert_eq!(shown["state"], "failed", "run did not actually fail");
    assert_eq!(shown["steps"][0]["state"], "failed", "step did not fail");
    let reason = shown["steps"][0]["reason"]
        .as_str()
        .expect("failed step has no human-readable reason");
    assert!(
        !reason.trim().is_empty() && reason.chars().any(char::is_alphabetic),
        "failed step has no human-readable reason"
    );

    // Collect all privacy failures so the red run observes both caller
    // classes AND run-show before reporting any disclosure assertion.
    let mut disclosures = Vec::new();
    for who in [Asserted::Unproven, Asserted::Agent("writer".into())] {
        let label = format!("{who:?}");
        let events = fx.rpc(who, "agent_events", json!({"alias": "daemon", "after": 0}));
        let failed_event = events["events"]
            .as_array()
            .expect("daemon events response has no event list")
            .iter()
            .find(|event| {
                event["kind"] == "app_run_failed" && event["payload"]["run_id"] == fx.run_id
            })
            .expect("failed run has no actual app_run_failed event on the daemon read path");
        assert_eq!(failed_event["payload"]["step_id"], "s1");
        if contains_private_prose(&events.to_string()) {
            disclosures.push(format!(
                "{label} daemon events exposed raw provider error text"
            ));
        }
    }
    if contains_private_prose(reason) {
        disclosures.push("steps[].reason exposed credential-bearing provider prose".into());
    }
    assert!(
        disclosures.is_empty(),
        "provider error privacy violated: {}",
        disclosures.join("; ")
    );
}
