//! CAD631 pending input privacy through actual Codex WebSocket provider events.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
use std::path::Path;

const OWNER: &str = "requests-pm";
const WRITER: &str = "requests-writer";
const REVIEWER: &str = "requests-reviewer";
const CLEAN: &str = "requests-clean";
const CANARY: &str = "NATIVE_APP_PENDING_CANARY";

fn actual(lane: &mut LaneShell, state: &Path, detached: bool, params: Value) -> Value {
    let frame = json!({"method":"agent_requests","params":params});
    assert_eq!(frame.as_object().unwrap().len(), 2);
    assert!(frame.get(cadence_agent::test_seam::FRAME_FIELD).is_none());
    let wire = frame.to_string();
    assert!(!wire.contains(cadence_agent::test_seam::AS_HEADER));
    assert!(!wire.contains(cadence_agent::test_seam::TOKEN_HEADER));
    let path = lane.dir.path().join(format!("requests-{}.json", lane.seq));
    std::fs::write(&path, wire).unwrap();
    let prefix = if detached { "setsid " } else { "" };
    let (code, body) = lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.socket(socket.AF_UNIX);s.settimeout(10);s.connect(sys.argv[1]);s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");print(s.makefile().readline())' {} {}", state.join("cadence.sock").display(), path.display()));
    assert_eq!(code, 0, "actual requests transport failed: {body}");
    serde_json::from_str(body.trim()).unwrap()
}

#[test]
fn cad631_actual_pm_cannot_read_app_pending_request_but_keeps_non_app_scope() {
    let root = tempfile::tempdir().unwrap();
    let pm = Pm::init(&root.path().join("pm")).unwrap();
    let opts = daemon_opts();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
    let d = TestDaemon::start_opts(opts);
    let _mock = d.mock_codex_ws("ok");
    // Adapt only the test-private copy. Seed/handshake and request transport
    // remain the existing protocol fixture; no daemon state or outcome is forged.
    let script = d.dir.path().join("mock-ws-ok.py");
    let code = std::fs::read_to_string(&script).unwrap();
    let needle = "elif text.startswith(\"NEED_INPUT\"):";
    assert_eq!(code.matches(needle).count(), 1);
    assert!(code.contains("\"params\": {\"command\": \"x\"}"));
    let code = code
        .replacen(
            needle,
            "elif \"NATIVE_APP_PENDING_CANARY\" in text or text.startswith(\"NEED_INPUT\"):",
            1,
        )
        .replace(
            "\"params\": {\"command\": \"x\"}",
            "\"params\": {\"command\": \"echo NATIVE_APP_PENDING_CANARY\"}",
        );
    std::fs::write(&script, code).unwrap();
    d.fixture_rpc("agent_register", json!({"alias":OWNER,"provider":"inbox","endpoint_kind":"inbox","role":"pm","cwd":d.dir.path()})).unwrap();
    let mut owner = LaneShell::spawn(root.path());
    plant_pane(&d, OWNER, owner.pid());
    assert_eq!(
        d.operator_rpc("agent_show", json!({"alias":OWNER}))
            .unwrap()["agent"]["role"],
        "pm"
    );
    for alias in [WRITER, REVIEWER, CLEAN] {
        d.register_codex_params(alias, "managed-ws", json!({"upstream":OWNER}));
        d.wait_agent(alias, "idle", 20);
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("apps/local-content");
    let installed = d
        .operator_rpc("app_workspace_install", json!({"source":source}))
        .unwrap();
    d.operator_rpc(
        "app_local_install_approve",
        json!({"install_id":installed["install_id"],"digest":installed["digest"]}),
    )
    .unwrap();
    let created = d.operator_rpc("app_run_create", json!({"install_id":installed["install_id"],"workflow":"draft","inputs":{"subject":CANARY,"source":"Private source facts for this local run.","writer":WRITER,"reviewer":REVIEWER},"request_id":"native-pending-1","owner_pm":OWNER})).unwrap();
    d.operator_rpc(
        "app_run_approve",
        json!({"run_id":created["id"],"digest":created["snapshot_digest"]}),
    )
    .unwrap();
    d.operator_rpc("app_run_dispatch", json!({"run_id":created["id"]}))
        .unwrap();
    d.wait_agent(WRITER, "waiting_input", 20);
    let app_pending = d.wait_request(WRITER, 20);
    assert_eq!(
        app_pending["method"],
        "item/commandExecution/requestApproval"
    );
    assert!(app_pending["params"].to_string().contains(CANARY));
    let app_handle = app_pending["request"].as_str().unwrap();
    let before = d
        .operator_rpc("app_run_show", json!({"run_id":created["id"]}))
        .unwrap();
    assert_eq!(before["state"], "running");
    assert!(before["artifacts"].as_array().unwrap().is_empty());

    // A genuine ordinary request from a clean endpoint in the same group is
    // intentionally still visible to this PM. Both provider rows are populated.
    d.send(
        CLEAN,
        json!({"text":"NEED_INPUT:ordinary scoped control","message":"ordinary-pending"}),
    )
    .unwrap();
    d.wait_agent(CLEAN, "waiting_input", 20);
    let clean_pending = d.wait_request(CLEAN, 20);
    assert_eq!(
        clean_pending["method"],
        "item/commandExecution/requestApproval"
    );
    let clean_handle = clean_pending["request"].as_str().unwrap();
    let mut failures = Vec::new();
    for detached in [false, true] {
        let clean = actual(&mut owner, &d.state, detached, json!({"alias":CLEAN}));
        assert_eq!(
            clean["ok"], true,
            "legitimate non-app PM control failed: {clean}"
        );
        assert!(clean["result"]["requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["request"] == clean_handle));
        for params in [
            json!({"alias":WRITER}),
            json!({"alias":WRITER,"operator":true,"actor":"operator","agent":OWNER}),
        ] {
            let frame = actual(&mut owner, &d.state, detached, params);
            eprintln!(
                "actual PM app pending detached={detached} ok={} error={}",
                frame["ok"], frame["error"]
            );
            if frame["ok"] != false
                || frame["error"]["kind"] != "rejected"
                || frame.get("result").is_some()
                || frame.to_string().contains(CANARY)
            {
                failures.push(format!(
                    "detached={detached} app pending request was not privately refused"
                ));
            }
        }
    }
    let retained = d
        .operator_rpc("agent_requests", json!({"alias":WRITER}))
        .unwrap();
    let row = retained["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["request"] == app_handle)
        .expect("read-denial lost the genuine provider request");
    assert_eq!(row, &app_pending);
    let after = d
        .operator_rpc("app_run_show", json!({"run_id":created["id"]}))
        .unwrap();
    assert_eq!(
        after, before,
        "pending inspection changed app material or lifecycle"
    );
    assert!(
        failures.is_empty(),
        "actual PM app pending request privacy guard failed: {failures:?}"
    );
}
