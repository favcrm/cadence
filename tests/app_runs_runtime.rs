//! CAD631: actual registered Pi transport, authenticated artifact fetch and restart.
#![allow(clippy::disallowed_methods)]
mod common;

use cadence_agent::issue::Pm;
use common::{daemon_opts, pi_policy_pm, TestDaemon};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::{Duration, Instant};

const OWNER: &str = "op-local-pm";
const WRITER: &str = "op-local-writer";
const REVIEWER: &str = "op-local-reviewer";
const DRAFT: &str = "Lunch is served from noon to 3pm.";

fn count(state: &Path, sql: &str) -> i64 {
    rusqlite::Connection::open(state.join("cadence.sqlite3"))
        .unwrap()
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}

fn wait_run(d: &TestDaemon, id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let run = d
            .operator_rpc("app_run_show", json!({"run_id":id}))
            .unwrap();
        if run["state"] == "succeeded" {
            return run;
        }
        assert!(
            !matches!(run["state"].as_str(), Some("failed" | "cancelled")),
            "local provider lifecycle failed: {run}"
        );
        assert!(Instant::now() < deadline, "local run did not settle: {run}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn provider_receipts(state: &Path, alias: &str) -> Vec<Value> {
    std::fs::read_to_string(
        state
            .join("agents")
            .join(alias)
            .join("app-run-receipts.jsonl"),
    )
    .unwrap()
    .lines()
    .map(|line| serde_json::from_str(line).unwrap())
    .collect()
}

#[test]
fn cad631_registered_pi_writer_and_reviewer_complete_without_project_or_duplicate_restart() {
    let root = tempfile::tempdir().unwrap();
    let pm = Pm::init(&root.path().join("pm")).unwrap();
    pi_policy_pm(&pm.dir);
    let opts = daemon_opts();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/app-run-pi.py");
    opts.provider_env.set(
        "CADENCE_PI_COMMAND",
        format!("python3 {}", fixture.display()),
    );
    let mut d = TestDaemon::start_opts(opts);
    d.fixture_rpc("agent_register", json!({"alias":OWNER,"provider":"inbox","endpoint_kind":"inbox","role":"pm","cwd":d.dir.path()})).unwrap();
    for alias in [WRITER, REVIEWER] {
        d.register_pi(
            alias,
            json!({"upstream":OWNER,"model":"fake/model-1","effort":"high"}),
        );
        d.wait_agent(alias, "idle", 20);
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("apps/local-content");
    let installed = d
        .operator_rpc("app_workspace_install", json!({"source":source}))
        .unwrap();
    let install_id = installed["install_id"].as_str().unwrap();
    d.operator_rpc(
        "app_local_install_approve",
        json!({"install_id":install_id,"digest":installed["digest"]}),
    )
    .unwrap();
    let params = json!({"install_id":install_id,"workflow":"draft","inputs":{"subject":"Lunch menu","source":DRAFT,"writer":WRITER,"reviewer":REVIEWER},"request_id":"local-runtime-1","owner_pm":OWNER});
    let created = d.operator_rpc("app_run_create", params.clone()).unwrap();
    let id = created["id"].as_str().unwrap();
    assert_eq!(created["state"], "awaiting_approval");
    assert!(created["project_link"].is_null());
    assert!(d
        .operator_rpc("app_run_dispatch", json!({"run_id":id}))
        .is_err());
    assert_eq!(
        count(
            &d.state,
            "SELECT count(*) FROM messages WHERE source='app_run_dispatch'"
        ),
        0
    );
    let repeated = d.operator_rpc("app_run_create", params).unwrap();
    assert_eq!(repeated["id"], created["id"]);
    d.operator_rpc(
        "app_run_approve",
        json!({"run_id":id,"digest":created["snapshot_digest"]}),
    )
    .unwrap();
    std::thread::scope(|scope| {
        let first = scope.spawn(|| d.operator_rpc("app_run_dispatch", json!({"run_id":id})));
        let second = scope.spawn(|| d.operator_rpc("app_run_dispatch", json!({"run_id":id})));
        assert!(first.join().unwrap().is_ok());
        assert!(second.join().unwrap().is_ok());
    });
    assert_eq!(
        count(
            &d.state,
            "SELECT count(*) FROM messages WHERE source='app_run_dispatch'"
        ),
        1,
        "concurrent dispatch duplicated writer or released dependency early"
    );
    std::fs::write(d.state.join("app-run-release-writer"), "release").unwrap();
    let finished = wait_run(&d, id);
    assert_eq!(finished["steps"].as_array().unwrap().len(), 2);
    assert!(finished["steps"]
        .as_array()
        .unwrap()
        .iter()
        .all(|step| step["state"] == "succeeded"));
    assert_eq!(finished["reviews"].as_array().unwrap().len(), 1);
    assert_eq!(finished["reviews"][0]["reviewer"], REVIEWER);
    assert_eq!(finished["reviews"][0]["decision"], "approve");
    let artifact = &finished["artifacts"][0];
    let expected_digest = format!("sha256:{:x}", Sha256::digest(DRAFT.as_bytes()));
    assert_eq!(artifact["digest"], expected_digest);
    let fetched = d
        .operator_rpc("app_run_artifact", json!({"artifact_id":artifact["id"]}))
        .unwrap();
    assert_eq!(fetched["text"], DRAFT);
    assert_eq!(fetched["digest"], expected_digest);
    for alias in [WRITER, REVIEWER] {
        let receipts = provider_receipts(&d.state, alias);
        assert_eq!(
            receipts.len(),
            1,
            "provider {alias} received duplicate prompt"
        );
        assert_eq!(receipts[0]["run_id"], id);
        assert_eq!(receipts[0]["native_turn_observed"], true);
        assert_eq!(receipts[0]["artifact_sha256"], expected_digest);
        if alias == REVIEWER {
            assert_eq!(
                receipts[0]["dependency_fetched"], true,
                "review never fetched authenticated material"
            );
        }
    }
    assert!(cadence_agent::issue::project::list(&pm.dir)
        .unwrap()
        .is_empty());
    assert_eq!(
        count(
            &d.state,
            "SELECT count(*) FROM messages WHERE source='app_run_dispatch'"
        ),
        2
    );
    assert_eq!(
        count(
            &d.state,
            "SELECT count(*) FROM messages WHERE source='app_run_dispatch' AND state='completed'"
        ),
        2
    );
    assert_eq!(
        count(
            &d.state,
            "SELECT count(*) FROM tasks WHERE head_sha IS NOT NULL"
        ),
        0,
        "artifact digest was stored as a Git SHA"
    );
    assert_eq!(count(&d.state, "SELECT count(*) FROM platform_effects"), 0);
    assert_eq!(count(&d.state, "SELECT count(*) FROM platform_drafts"), 0);
    assert_eq!(count(&d.state, "SELECT count(*) FROM app_grants"), 0);
    assert_eq!(
        count(
            &d.state,
            "SELECT count(*) FROM events WHERE kind='app_run_step_dispatched'"
        ),
        2
    );
    assert_eq!(
        count(
            &d.state,
            "SELECT count(*) FROM events WHERE kind='app_run_completed'"
        ),
        1
    );

    // Keep only this owned temp state alive while its daemon shuts down.
    let state = d.state.clone();
    let _state_owner = std::mem::replace(&mut d.dir, tempfile::tempdir().unwrap());
    drop(d);
    let restarted = TestDaemon::start_on_opts(state, daemon_opts());
    for alias in [WRITER, REVIEWER] {
        restarted.wait_agent(alias, "idle", 20);
    }
    let restored = restarted
        .operator_rpc("app_run_show", json!({"run_id":id}))
        .unwrap();
    assert_eq!(restored["state"], "succeeded");
    assert_eq!(restored["snapshot_digest"], created["snapshot_digest"]);
    assert_eq!(restored["artifacts"], finished["artifacts"]);
    assert_eq!(restored["reviews"], finished["reviews"]);
    assert!(restarted
        .operator_rpc("app_run_dispatch", json!({"run_id":id}))
        .is_err());
    assert_eq!(
        count(
            &restarted.state,
            "SELECT count(*) FROM messages WHERE source='app_run_dispatch'"
        ),
        2
    );
    assert_eq!(
        count(
            &restarted.state,
            "SELECT count(*) FROM events WHERE kind='app_run_completed'"
        ),
        1
    );
    assert_eq!(provider_receipts(&restarted.state, WRITER).len(), 1);
    assert_eq!(provider_receipts(&restarted.state, REVIEWER).len(), 1);
}

#[test]
fn cad631_restart_during_real_review_preserves_draft_and_never_replays_uncertain_work() {
    let root = tempfile::tempdir().unwrap();
    let pm = Pm::init(&root.path().join("pm")).unwrap();
    pi_policy_pm(&pm.dir);
    let opts = daemon_opts();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/app-run-pi.py");
    opts.provider_env.set(
        "CADENCE_PI_COMMAND",
        format!("python3 {}", fixture.display()),
    );
    let mut d = TestDaemon::start_opts(opts);
    d.fixture_rpc("agent_register", json!({"alias":OWNER,"provider":"inbox","endpoint_kind":"inbox","role":"pm","cwd":d.dir.path()})).unwrap();
    for alias in [WRITER, REVIEWER] {
        d.register_pi(
            alias,
            json!({"upstream":OWNER,"model":"fake/model-1","effort":"high"}),
        );
        d.wait_agent(alias, "idle", 20);
    }
    let installed = d
        .operator_rpc(
            "app_workspace_install",
            json!({"source":Path::new(env!("CARGO_MANIFEST_DIR")).join("apps/local-content")}),
        )
        .unwrap();
    let install = installed["install_id"].as_str().unwrap();
    d.operator_rpc(
        "app_local_install_approve",
        json!({"install_id":install,"digest":installed["digest"]}),
    )
    .unwrap();
    let created = d.operator_rpc("app_run_create", json!({"install_id":install,"workflow":"draft","inputs":{"subject":"Lunch menu","source":DRAFT,"writer":WRITER,"reviewer":REVIEWER},"request_id":"partial-restart-1","owner_pm":OWNER})).unwrap();
    let id = created["id"].as_str().unwrap();
    d.operator_rpc(
        "app_run_approve",
        json!({"run_id":id,"digest":created["snapshot_digest"]}),
    )
    .unwrap();
    std::fs::write(d.state.join("app-run-hold-reviewer"), "hold").unwrap();
    std::fs::write(d.state.join("app-run-release-writer"), "release").unwrap();
    d.operator_rpc("app_run_dispatch", json!({"run_id":id}))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(40);
    while !d.state.join("app-run-reviewer-held").exists() {
        assert!(
            Instant::now() < deadline,
            "actual reviewer never fetched the draft"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let partial = d
        .operator_rpc("app_run_show", json!({"run_id":id}))
        .unwrap();
    assert_eq!(partial["state"], "running");
    assert_eq!(partial["steps"][0]["state"], "succeeded");
    assert_eq!(partial["steps"][1]["state"], "dispatched");
    assert_eq!(count(&d.state, "SELECT count(*) FROM messages WHERE source='app_run_dispatch' AND alias='op-local-reviewer' AND state='running'"), 1, "reviewer was not running at the owned shutdown boundary");
    assert!(partial["reviews"].as_array().unwrap().is_empty());
    assert_eq!(partial["artifacts"].as_array().unwrap().len(), 1);
    let artifact_id = partial["artifacts"][0]["id"].as_str().unwrap();
    assert_eq!(provider_receipts(&d.state, WRITER).len(), 1);

    // Shut down only this test's daemon while the actual dependent turn
    // is held. No review material or approval is injected into storage.
    let state = d.state.clone();
    let _state_owner = std::mem::replace(&mut d.dir, tempfile::tempdir().unwrap());
    drop(d);
    let restarted = TestDaemon::start_on_opts(state, daemon_opts());
    let deadline = Instant::now() + Duration::from_secs(40);
    let restored = loop {
        let run = restarted
            .operator_rpc("app_run_show", json!({"run_id":id}))
            .unwrap();
        if run["state"] == "failed" {
            break run;
        }
        assert_eq!(
            run["state"], "running",
            "uncertain reviewer became successful or revived: {run}"
        );
        assert!(
            Instant::now() < deadline,
            "interrupted app run silently stuck after restart: {run}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(restored["snapshot_digest"], created["snapshot_digest"]);
    assert_eq!(restored["artifacts"], partial["artifacts"]);
    assert!(restored["reviews"].as_array().unwrap().is_empty());
    assert_eq!(count(&restarted.state, "SELECT count(*) FROM messages WHERE source='app_run_dispatch' AND alias='op-local-reviewer' AND state IN ('failed','unknown')"), 1, "interrupted reviewer lacks its transport outcome");
    let fetched = restarted
        .operator_rpc("app_run_artifact", json!({"artifact_id":artifact_id}))
        .unwrap();
    assert_eq!(fetched["text"], DRAFT);
    assert!(restarted
        .operator_rpc("app_run_dispatch", json!({"run_id":id}))
        .is_err());
    assert_eq!(
        count(
            &restarted.state,
            "SELECT count(*) FROM messages WHERE source='app_run_dispatch'"
        ),
        2,
        "interrupted turn was duplicated"
    );
    assert_eq!(
        count(
            &restarted.state,
            "SELECT count(*) FROM events WHERE kind='app_run_completed'"
        ),
        0
    );
    assert_eq!(provider_receipts(&restarted.state, WRITER).len(), 1);
    assert_eq!(
        count(&restarted.state, "SELECT count(*) FROM platform_effects"),
        0
    );
}

#[test]
fn cad631_revoke_real_running_writer_refuses_material_and_dependent_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let pm = Pm::init(&root.path().join("pm")).unwrap();
    pi_policy_pm(&pm.dir);
    let opts = daemon_opts();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/app-run-pi.py");
    opts.provider_env.set(
        "CADENCE_PI_COMMAND",
        format!("python3 {}", fixture.display()),
    );
    let d = TestDaemon::start_opts(opts);
    d.fixture_rpc("agent_register", json!({"alias":OWNER,"provider":"inbox","endpoint_kind":"inbox","role":"pm","cwd":d.dir.path()})).unwrap();
    for alias in [WRITER, REVIEWER] {
        d.register_pi(
            alias,
            json!({"upstream":OWNER,"model":"fake/model-1","effort":"high"}),
        );
        d.wait_agent(alias, "idle", 20);
    }
    let installed = d
        .operator_rpc(
            "app_workspace_install",
            json!({"source":Path::new(env!("CARGO_MANIFEST_DIR")).join("apps/local-content")}),
        )
        .unwrap();
    let install = installed["install_id"].as_str().unwrap();
    d.operator_rpc(
        "app_local_install_approve",
        json!({"install_id":install,"digest":installed["digest"]}),
    )
    .unwrap();
    let created = d.operator_rpc("app_run_create", json!({"install_id":install,"workflow":"draft","inputs":{"subject":"Lunch menu","source":DRAFT,"writer":WRITER,"reviewer":REVIEWER},"request_id":"revoke-running-1","owner_pm":OWNER})).unwrap();
    let id = created["id"].as_str().unwrap();
    d.operator_rpc(
        "app_run_approve",
        json!({"run_id":id,"digest":created["snapshot_digest"]}),
    )
    .unwrap();
    d.operator_rpc("app_run_dispatch", json!({"run_id":id}))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !d.state.join("app-run-writer-held").exists() {
        assert!(
            Instant::now() < deadline,
            "real writer never observed its native active turn"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(count(&d.state, "SELECT count(*) FROM messages WHERE source='app_run_dispatch' AND alias='op-local-writer' AND state='running'"), 1);
    let revoked = d
        .operator_rpc(
            "app_local_install_revoke",
            json!({"install_id":install,"digest":installed["digest"]}),
        )
        .unwrap();
    assert_eq!(revoked["approved"], false);
    std::fs::write(d.state.join("app-run-release-writer"), "release").unwrap();
    // Wait for the real successful producer envelope to arrive, rather than
    // assert zero artifacts while the material callback is still pending.
    let deadline = Instant::now() + Duration::from_secs(20);
    while count(&d.state, "SELECT count(*) FROM messages WHERE source='app_run_dispatch' AND alias='op-local-writer' AND state IN ('queued','submitting','running')") != 0 {
        assert!(Instant::now() < deadline, "revoked producer transport did not settle");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        provider_receipts(&d.state, WRITER).len(),
        1,
        "producer never returned its actual material envelope"
    );
    let run = d
        .operator_rpc("app_run_show", json!({"run_id":id}))
        .unwrap();
    assert_eq!(
        run["state"], "failed",
        "revoked run accepted real provider material: {run}"
    );
    assert!(
        run["artifacts"].as_array().unwrap().is_empty(),
        "revoked producer material was persisted"
    );
    assert!(run["reviews"].as_array().unwrap().is_empty());
    assert_eq!(
        count(
            &d.state,
            "SELECT count(*) FROM messages WHERE source='app_run_dispatch'"
        ),
        1,
        "revocation released the dependent reviewer"
    );
    assert_eq!(
        count(
            &d.state,
            "SELECT count(*) FROM events WHERE kind='app_run_completed'"
        ),
        0
    );
    assert_eq!(count(&d.state, "SELECT count(*) FROM platform_effects"), 0);
    assert_eq!(count(&d.state, "SELECT count(*) FROM platform_drafts"), 0);
    assert_eq!(count(&d.state, "SELECT count(*) FROM app_grants"), 0);
}
