//! CAD631 real Unix peer admission. No caller assertion frames or guessed objects.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, pi_policy_pm, plant_member_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
use sha2::Digest;
use std::path::Path;
use std::time::{Duration, Instant};

const OWNER: &str = "native-local-pm";
const WRITER: &str = "native-local-writer";
const REVIEWER: &str = "native-local-reviewer";
const PEER: &str = "native-app-worker";
const DRAFT: &str = "Lunch is served from noon to 3pm.";

struct LocalRun {
    root: tempfile::TempDir,
    daemon: TestDaemon,
    install: Value,
    run: Value,
    create: Value,
}
impl LocalRun {
    fn new() -> Self {
        Self::with_result_format(None)
    }
    fn with_result_format(format: Option<&str>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        pi_policy_pm(&pm.dir);
        let opts = daemon_opts();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/app-run-pi.py");
        opts.provider_env.set(
            "CADENCE_PI_COMMAND",
            format!("python3 {}", script.display()),
        );
        let daemon = TestDaemon::start_opts(opts);
        if format == Some("fenced") {
            std::fs::write(daemon.state.join("app-run-fenced-result"), "enabled").unwrap();
        }
        daemon.fixture_rpc("agent_register", json!({"alias":OWNER,"provider":"inbox","endpoint_kind":"inbox","role":"pm","cwd":daemon.dir.path()})).unwrap();
        for alias in [WRITER, REVIEWER] {
            daemon.register_pi(
                alias,
                json!({"upstream":OWNER,"model":"fake/model-1","effort":"high"}),
            );
            daemon.wait_agent(alias, "idle", 20);
        }
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("apps/local-content");
        let install = daemon
            .operator_rpc("app_workspace_install", json!({"source":source}))
            .unwrap();
        daemon
            .operator_rpc(
                "app_local_install_approve",
                json!({"install_id":install["install_id"],"digest":install["digest"]}),
            )
            .unwrap();
        let create = json!({"install_id":install["install_id"],"workflow":"draft","inputs":{"subject":"Lunch menu","source":DRAFT,"writer":WRITER,"reviewer":REVIEWER},"request_id":"native-local-1","owner_pm":OWNER});
        let run = daemon
            .operator_rpc("app_run_create", create.clone())
            .unwrap();
        assert_eq!(run["state"], "awaiting_approval");
        let shown = daemon
            .operator_rpc("app_run_show", json!({"run_id":run["id"]}))
            .unwrap();
        assert_eq!(shown, run);
        let listed = daemon
            .operator_rpc("app_run_list", json!({"install_id":install["install_id"]}))
            .unwrap();
        assert_eq!(listed["runs"][0]["id"], run["id"]);
        Self {
            root,
            daemon,
            install,
            run,
            create,
        }
    }
    fn finish(&self) -> Value {
        self.daemon
            .operator_rpc(
                "app_run_approve",
                json!({"run_id":self.run["id"],"digest":self.run["snapshot_digest"]}),
            )
            .unwrap();
        self.daemon
            .operator_rpc("app_run_dispatch", json!({"run_id":self.run["id"]}))
            .unwrap();
        std::fs::write(self.daemon.state.join("app-run-release-writer"), "release").unwrap();
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            let run = self
                .daemon
                .operator_rpc("app_run_show", json!({"run_id":self.run["id"]}))
                .unwrap();
            if run["state"] == "succeeded" {
                return run;
            }
            assert!(
                !matches!(run["state"].as_str(), Some("failed" | "cancelled")),
                "actual provider run failed with task error {:?}: {run}",
                rusqlite::Connection::open(self.daemon.state.join("cadence.sqlite3"))
                    .unwrap()
                    .query_row(
                        "SELECT error FROM tasks WHERE id=?",
                        [format!("{}-s1", self.run["id"].as_str().unwrap())],
                        |row| row.get::<_, Option<String>>(0)
                    )
                    .unwrap()
            );
            assert!(
                Instant::now() < deadline,
                "actual provider run stalled: {run}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn persisted(&self) -> Value {
        let db = rusqlite::Connection::open(self.daemon.state.join("cadence.sqlite3")).unwrap();
        let mut counts = serde_json::Map::new();
        for table in [
            "app_runs",
            "app_run_steps",
            "app_run_artifacts",
            "app_run_reviews",
            "tasks",
            "jobs",
            "messages",
            "app_grants",
            "platform_effects",
            "platform_drafts",
        ] {
            let count: i64 = db
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            counts.insert(table.into(), json!(count));
        }
        let capability: (i64, String, String) = db
            .query_row(
                "SELECT epoch,state,digest FROM app_install_capabilities WHERE install_id=?",
                [self.install["install_id"].as_str().unwrap()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let events: i64 = db.query_row("SELECT count(*) FROM events WHERE kind LIKE 'app_run_%' OR kind LIKE 'app_install_capability_%'", [], |row| row.get(0)).unwrap();
        let run = self
            .daemon
            .operator_rpc("app_run_show", json!({"run_id":self.run["id"]}))
            .unwrap();
        let head = std::process::Command::new("git")
            .arg("-C")
            .arg(self.root.path().join("pm"))
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(head.status.success());
        json!({"counts":counts,"capability":capability,"events":events,"run":run,"pm_head":String::from_utf8(head.stdout).unwrap()})
    }
}

#[test]
fn cad749_real_pi_fenced_single_result_completes_app_material_handoff() {
    let run = LocalRun::with_result_format(Some("fenced"));
    let finished = run.finish();
    let db = rusqlite::Connection::open(run.daemon.state.join("cadence.sqlite3")).unwrap();
    let raw: String = db
        .query_row(
            "SELECT result FROM messages WHERE source='app_run_dispatch' AND alias=?",
            [WRITER],
            |row| row.get(0),
        )
        .unwrap();
    let final_text = serde_json::from_str::<Value>(&raw).unwrap()["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(final_text.starts_with("The source capability call succeeded"));
    assert!(final_text.contains("\n```json\n"));
    assert!(final_text.contains("\n## Scope compliance\n"));
    assert_eq!(finished["state"], "succeeded");
    assert_eq!(
        finished["artifacts"][0]["digest"],
        format!("sha256:{:x}", sha2::Sha256::digest(DRAFT.as_bytes()))
    );
    assert_eq!(finished["reviews"][0]["decision"], "approve");
}

fn native(
    lane: &mut LaneShell,
    state: &Path,
    detached: bool,
    method: &str,
    params: Value,
) -> Value {
    // Build literal two-field JSON, avoiding proto::request's optional test seam.
    let frame = json!({"method":method,"params":params});
    assert_eq!(frame.as_object().unwrap().len(), 2);
    assert!(frame.get(cadence_agent::test_seam::FRAME_FIELD).is_none());
    let wire = frame.to_string();
    assert!(!wire.contains(cadence_agent::test_seam::AS_HEADER));
    assert!(!wire.contains(cadence_agent::test_seam::TOKEN_HEADER));
    let request = lane.dir.path().join(format!("native-{}.json", lane.seq));
    std::fs::write(&request, wire).unwrap();
    let prefix = if detached { "setsid " } else { "" };
    let (rc, text) = lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.socket(socket.AF_UNIX);s.settimeout(10);s.connect(sys.argv[1]);s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");print(s.makefile().readline())' {} {}", state.join("cadence.sock").display(), request.display()));
    assert_eq!(rc, 0, "native socket process failed: {text}");
    serde_json::from_str(text.trim()).unwrap()
}
fn denied(frame: &Value, alias: &str) -> bool {
    frame["ok"] == false
        && frame["error"]["kind"] == "rejected"
        && frame.get("result").is_none()
        && frame["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("operator action") && message.contains(alias))
}

#[test]
fn cad631_actual_native_and_setsid_management_requires_operator_before_mutation() {
    let f = LocalRun::new();
    let mut lane = LaneShell::spawn(f.root.path());
    plant_member_pane(&f.daemon, PEER, "claude", None, lane.pid());
    let before = f.persisted();
    let mut new_create = f.create.clone();
    new_create["request_id"] = json!("native-agent-attempt");
    let cases = [
        (
            "app_local_install_approve",
            json!({"install_id":f.install["install_id"],"digest":f.install["digest"]}),
        ),
        (
            "app_local_install_revoke",
            json!({"install_id":f.install["install_id"],"digest":f.install["digest"]}),
        ),
        ("app_run_create", new_create),
        (
            "app_run_list",
            json!({"install_id":f.install["install_id"]}),
        ),
        ("app_run_show", json!({"run_id":f.run["id"]})),
        (
            "app_run_approve",
            json!({"run_id":f.run["id"],"digest":f.run["snapshot_digest"]}),
        ),
        ("app_run_cancel", json!({"run_id":f.run["id"]})),
        ("app_run_dispatch", json!({"run_id":f.run["id"]})),
    ];
    let mut failures = Vec::new();
    for detached in [false, true] {
        for (method, params) in &cases {
            let reply = native(&mut lane, &f.daemon.state, detached, method, params.clone());
            eprintln!("native management detached={detached} method={method} response={reply}");
            if !denied(&reply, PEER) {
                failures.push(format!("detached={detached} {method}: {reply}"));
            }
            let mut forged = params.clone();
            forged["operator"] = json!(true);
            forged["agent"] = json!("operator");
            let reply = native(&mut lane, &f.daemon.state, detached, method, forged);
            eprintln!("native forged detached={detached} method={method} response={reply}");
            if reply["ok"] != false
                || reply["error"]["kind"] != "rejected"
                || !reply["error"]["message"].as_str().is_some_and(|message| {
                    message.contains("unsupported fields") || message.contains("not accepted")
                })
            {
                failures.push(format!("forged detached={detached} {method}: {reply}"));
            }
        }
    }
    let after = f.persisted();
    if before != after {
        failures.push("rejected native lifecycle calls changed persisted state".into());
    }
    assert!(
        failures.is_empty(),
        "native lifecycle operator guard failed: {failures:?}"
    );
}

#[test]
fn cad631_actual_native_and_setsid_completed_artifact_is_operator_only() {
    let f = LocalRun::new();
    let completed = f.finish();
    let id = completed["artifacts"][0]["id"].as_str().unwrap();
    let receipt = f
        .daemon
        .operator_rpc("app_run_artifact", json!({"artifact_id":id}))
        .unwrap();
    assert_eq!(receipt["text"], DRAFT);
    assert_eq!(receipt["digest"], completed["artifacts"][0]["digest"]);
    f.daemon
        .operator_rpc(
            "app_local_install_revoke",
            json!({"install_id":f.install["install_id"],"digest":f.install["digest"]}),
        )
        .unwrap();
    let install_id = f.install["install_id"].as_str().unwrap();
    let workflow = f
        .root
        .path()
        .join("pm/.apps/installations")
        .join(install_id)
        .join("bundle/workflows/draft.md");
    let mut text = std::fs::read_to_string(&workflow).unwrap();
    text.push_str("\nHistorical outputs remain audit evidence after this valid edit.\n");
    std::fs::write(workflow, text).unwrap();
    let current = f
        .daemon
        .operator_rpc("app_workspace_show", json!({"install_id":install_id}))
        .unwrap();
    assert_ne!(current["digest"], f.install["digest"]);
    let historical = f
        .daemon
        .operator_rpc("app_run_artifact", json!({"artifact_id":id}))
        .expect("operator durable audit must survive revoke and bundle replacement");
    assert_eq!(historical, receipt);
    std::fs::remove_dir_all(
        f.root
            .path()
            .join("pm/.apps/installations")
            .join(install_id)
            .join("bundle"),
    )
    .unwrap();
    let removed = f
        .daemon
        .operator_rpc("app_run_artifact", json!({"artifact_id":id}))
        .expect("operator historical audit must not depend on an existing bundle");
    assert_eq!(removed, receipt);
    let mut lane = LaneShell::spawn(f.root.path());
    plant_member_pane(&f.daemon, PEER, "claude", None, lane.pid());
    let before = f.persisted();
    let mut failures = Vec::new();
    for detached in [false, true] {
        let reply = native(
            &mut lane,
            &f.daemon.state,
            detached,
            "app_run_artifact",
            json!({"artifact_id":id}),
        );
        eprintln!("native completed artifact detached={detached} response={reply}");
        if !denied(&reply, PEER) {
            failures.push(format!("detached={detached}: {reply}"));
        }
        let reply = native(
            &mut lane,
            &f.daemon.state,
            detached,
            "app_run_artifact",
            json!({"artifact_id":id,"operator":true,"agent":"operator"}),
        );
        if reply["ok"] != false || reply["error"]["kind"] != "rejected" {
            failures.push(format!("forged detached={detached}: {reply}"));
        }
    }
    let after = f.persisted();
    if before != after {
        failures.push("artifact denial changed persisted state".into());
    }
    assert!(
        failures.is_empty(),
        "native completed artifact operator guard failed: {failures:?}"
    );
}

#[test]
fn cad631_operator_cannot_approve_snapshot_after_valid_installed_workflow_edit() {
    let f = LocalRun::new();
    let id = f.install["install_id"].as_str().unwrap();
    let path = f
        .root
        .path()
        .join("pm")
        .join(".apps/installations")
        .join(id)
        .join("bundle/workflows/draft.md");
    let mut workflow = std::fs::read_to_string(&path).unwrap();
    workflow.push_str("\nKeep the original source facts unchanged.\n");
    std::fs::write(&path, workflow).unwrap();
    // A valid current catalog read, rather than corrupt files/invalid setup,
    // establishes that admission must compare the new material digest.
    let current = f
        .daemon
        .operator_rpc("app_workspace_show", json!({"install_id":id}))
        .unwrap();
    assert_eq!(current["install_id"], id);
    assert_ne!(current["digest"], f.install["digest"]);
    assert_eq!(f.run["snapshot"]["bundle_digest"], f.install["digest"]);
    let error = f
        .daemon
        .operator_rpc(
            "app_run_approve",
            json!({"run_id":f.run["id"],"digest":f.run["snapshot_digest"]}),
        )
        .expect_err("operator approved a run whose installed material changed");
    assert!(
        error.to_string().contains("bundle digest is stale"),
        "refusal must identify stale material, not invalid fixture: {error}"
    );
    let retained = f
        .daemon
        .operator_rpc("app_run_show", json!({"run_id":f.run["id"]}))
        .unwrap();
    assert_eq!(retained["state"], "awaiting_approval");
    assert!(retained["approved_digest"].is_null());
    assert_eq!(retained["snapshot_digest"], f.run["snapshot_digest"]);
    let db = rusqlite::Connection::open(f.daemon.state.join("cadence.sqlite3")).unwrap();
    let approvals: i64 = db
        .query_row(
            "SELECT count(*) FROM events WHERE kind='app_run_execution_approved'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(approvals, 0, "stale approval emitted execution authority");
    let kickoffs: i64 = db
        .query_row(
            "SELECT count(*) FROM messages WHERE source='app_run_dispatch'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kickoffs, 0, "stale approval released execution");
}
