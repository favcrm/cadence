//! CAD690 context-local draft proofs through real native provider turns.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, pi_policy_pm, plant_member_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};
const OWNER: &str = "context-pm";
const WRITER: &str = "context-writer";
const REVIEWER: &str = "context-reviewer";
const A: &str = "CLIENT_A_PRIVATE_FACTS";
const B: &str = "CLIENT_B_PRIVATE_FACTS";
struct Contexts {
    root: tempfile::TempDir,
    daemon: TestDaemon,
    install: Value,
    workflow: String,
}
impl Contexts {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        pi_policy_pm(&pm.dir);
        let opts = daemon_opts();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/app-context-pi.py");
        opts.provider_env.set(
            "CADENCE_PI_COMMAND",
            format!("python3 {}", script.display()),
        );
        let daemon = TestDaemon::start_opts(opts);
        daemon.fixture_rpc("agent_register",json!({"alias":OWNER,"provider":"inbox","endpoint_kind":"inbox","role":"pm","cwd":daemon.dir.path()})).unwrap();
        for alias in [WRITER, REVIEWER] {
            daemon.register_pi(
                alias,
                json!({"upstream":OWNER,"model":"fake/model-1","effort":"high"}),
            );
            daemon.wait_agent(alias, "idle", 20);
        }
        let source = root.path().join("bundle");
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        let original = Path::new(env!("CARGO_MANIFEST_DIR")).join("apps/local-content");
        std::fs::copy(original.join("app.md"), source.join("app.md")).unwrap();
        let text = std::fs::read_to_string(original.join("workflows/draft.md")).unwrap();
        let workflow = text.replace("source: { ask:", "source: { context_default: true, ask:");
        assert_ne!(text, workflow);
        std::fs::write(source.join("workflows/draft.md"), &workflow).unwrap();
        let install = daemon
            .operator_rpc("app_workspace_install", json!({"source":source}))
            .unwrap();
        daemon
            .operator_rpc(
                "app_local_install_approve",
                json!({"install_id":install["install_id"],"digest":install["digest"]}),
            )
            .unwrap();
        Self {
            root,
            daemon,
            install,
            workflow,
        }
    }
    fn context(&self, label: &str, source: &str, request: &str) -> Value {
        self.daemon.operator_rpc("app_context_create",json!({"install_id":self.install["install_id"],"label":label,"input_defaults":{"source":format!("CONTEXT_SOURCE={source}")},"request_id":request})).unwrap()["context"].clone()
    }
    fn create(&self, context: &Value, request: &str) -> Value {
        self.daemon.operator_rpc("app_run_create",json!({"install_id":self.install["install_id"],"context_id":context["id"],"workflow":"draft","inputs":{"subject":"Context draft","writer":WRITER,"reviewer":REVIEWER},"request_id":request,"owner_pm":OWNER})).unwrap()
    }
    fn dispatch(&self, run: &Value) {
        self.daemon
            .operator_rpc(
                "app_run_approve",
                json!({"run_id":run["id"],"digest":run["snapshot_digest"]}),
            )
            .unwrap();
        self.daemon
            .operator_rpc("app_run_dispatch", json!({"run_id":run["id"]}))
            .unwrap();
    }
    fn wait_state(&self, id: &str, terminal: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            let run = self
                .daemon
                .operator_rpc("app_run_show", json!({"run_id":id}))
                .unwrap();
            if run["state"] == terminal {
                return run;
            }
            assert!(
                Instant::now() < deadline,
                "actual context run did not reach {terminal}: {run}"
            );
            if terminal == "succeeded" {
                assert!(
                    !matches!(run["state"].as_str(), Some("failed" | "cancelled")),
                    "actual provider context run failed: {run}"
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    fn artifact(&self, run: &Value) -> Value {
        self.daemon
            .operator_rpc(
                "app_run_artifact",
                json!({"artifact_id":run["artifacts"][0]["id"]}),
            )
            .unwrap()
    }
    fn context_show(&self, c: &Value) -> Value {
        self.daemon
            .operator_rpc(
                "app_context_show",
                json!({"install_id":self.install["install_id"],"context_id":c["id"]}),
            )
            .unwrap()["context"]
            .clone()
    }
}
fn native(
    lane: &mut LaneShell,
    state: &Path,
    detached: bool,
    method: &str,
    params: Value,
) -> Value {
    let frame = json!({"method":method,"params":params});
    assert_eq!(frame.as_object().unwrap().len(), 2);
    assert!(frame.get(cadence_agent::test_seam::FRAME_FIELD).is_none());
    let wire = frame.to_string();
    assert!(!wire.contains(cadence_agent::test_seam::AS_HEADER));
    assert!(!wire.contains(cadence_agent::test_seam::TOKEN_HEADER));
    let request = lane
        .dir
        .path()
        .join(format!("context-rpc-{}.json", lane.seq));
    std::fs::write(&request, wire).unwrap();
    let prefix = if detached { "setsid " } else { "" };
    let (rc,text)=lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.socket(socket.AF_UNIX);s.settimeout(10);s.connect(sys.argv[1]);s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");print(s.makefile().readline())' {} {}",state.join("cadence.sock").display(),request.display()));
    assert_eq!(rc, 0, "native process failed: {text}");
    serde_json::from_str(text.trim()).unwrap()
}
#[test]
fn cad690_context_crud_native_and_setsid_requires_operator_and_exact_revision() {
    let h = Contexts::new();
    let c = h.context("Client A", A, "client-a");
    assert_eq!(h.context_show(&c), c);
    let list = h
        .daemon
        .operator_rpc(
            "app_context_list",
            json!({"install_id":h.install["install_id"]}),
        )
        .unwrap();
    assert_eq!(list["contexts"][0], c);
    assert_eq!(
        h.context("Client A", A, "client-a"),
        c,
        "idempotent create changed identity"
    );
    let mut lane = LaneShell::spawn(h.root.path());
    plant_member_pane(
        &h.daemon,
        "context-native-worker",
        "claude",
        None,
        lane.pid(),
    );
    let methods = [
        (
            "app_context_create",
            json!({"install_id":h.install["install_id"],"label":"Injected","input_defaults":{"source":"x"},"request_id":"attack"}),
        ),
        (
            "app_context_list",
            json!({"install_id":h.install["install_id"]}),
        ),
        (
            "app_context_show",
            json!({"install_id":h.install["install_id"],"context_id":c["id"]}),
        ),
        (
            "app_context_update",
            json!({"install_id":h.install["install_id"],"context_id":c["id"],"expected_revision":c["revision"],"label":"Other","input_defaults":{"source":"x"}}),
        ),
        (
            "app_context_archive",
            json!({"install_id":h.install["install_id"],"context_id":c["id"],"expected_revision":c["revision"]}),
        ),
    ];
    let mut failures = vec![];
    for detached in [false, true] {
        for (method, params) in &methods {
            for forged in [false, true] {
                let mut p = params.clone();
                if forged {
                    p["operator"] = json!(true);
                    p["agent"] = json!(OWNER);
                }
                let r = native(&mut lane, &h.daemon.state, detached, method, p);
                if r["ok"] != false || r["error"]["kind"] != "rejected" || r.get("result").is_some()
                {
                    failures.push(format!("{method} detached={detached} forged={forged}: {r}"));
                }
                assert!(!r.to_string().contains(A), "private settings leaked");
            }
        }
    }
    assert!(
        failures.is_empty(),
        "native context guard failed: {failures:?}"
    );
    assert_eq!(h.context_show(&c), c);
    let updated=h.daemon.operator_rpc("app_context_update",json!({"install_id":h.install["install_id"],"context_id":c["id"],"expected_revision":c["revision"],"label":"Changed","input_defaults":{"source":format!("CONTEXT_SOURCE={B}")}})).unwrap()["context"].clone();
    assert!(updated["revision"].as_u64().unwrap() > c["revision"].as_u64().unwrap());
    assert_ne!(updated["digest"], c["digest"]);
    assert!(
        h.daemon
            .operator_rpc(
                "app_run_approve",
                json!({"run_id":stale_run["id"],"digest":stale_run["snapshot_digest"]})
            )
            .is_err(),
        "old context revision execution approval survived update"
    );

    assert!(h.daemon.operator_rpc("app_context_archive",json!({"install_id":h.install["install_id"],"context_id":c["id"],"expected_revision":c["revision"]})).is_err());
    assert!(h
        .daemon
        .operator_rpc(
            "app_context_show",
            json!({"install_id":"different-install","context_id":c["id"]})
        )
        .is_err());
    assert_eq!(h.context_show(&updated), updated);
    let archived=h.daemon.operator_rpc("app_context_archive",json!({"install_id":h.install["install_id"],"context_id":c["id"],"expected_revision":updated["revision"]})).unwrap()["context"].clone();
    assert_eq!(archived["state"], "archived");
    assert!(h.daemon.operator_rpc("app_run_create",json!({"install_id":h.install["install_id"],"context_id":c["id"],"workflow":"draft","inputs":{"subject":"x","writer":WRITER,"reviewer":REVIEWER},"request_id":"archived","owner_pm":OWNER})).is_err());
}
#[test]
fn cad690_defaults_are_explicit_content_only_bounded_and_strict() {
    let h = Contexts::new();
    let _positive = h.context("Valid", A, "valid-defaults");
    for defaults in [
        json!({"writer":WRITER}),
        json!({"subject":"not explicitly eligible"}),
        json!({"unknown":"x"}),
        json!({"source":1}),
        json!({"source":"x".repeat(32*1024+1)}),
    ] {
        assert!(h.daemon.operator_rpc("app_context_create",json!({"install_id":h.install["install_id"],"label":"No","input_defaults":defaults,"request_id":"invalid-default"})).is_err(),"unsafe default accepted");
    }
    for field in ["agent", "operator", "revision", "context_id"] {
        let mut p = json!({"install_id":h.install["install_id"],"label":"No","input_defaults":{"source":"x"},"request_id":"forged-default"});
        p[field] = json!("operator");
        assert!(h.daemon.operator_rpc("app_context_create", p).is_err());
    }
    let structural = h
        .workflow
        .replace("writer: { ask:", "writer: { context_default: true, ask:");
    assert!(
        cadence_agent::store::app_runs::LocalWorkflow::validate_template(&structural).is_err(),
        "an agent assignment input became default-eligible"
    );
    for bad in ["null", "\"true\"", "1"] {
        let text = h
            .workflow
            .replace("context_default: true", &format!("context_default: {bad}"));
        assert!(
            cadence_agent::issue::workflow::parse_template(&text).is_err(),
            "nonboolean context_default admitted"
        );
    }
}
#[test]
fn cad690_two_contexts_real_shared_pi_reviewer_cannot_fetch_other_client() {
    let h = Contexts::new();
    let a = h.context("A", A, "context-a");
    let b = h.context("B", B, "context-b");
    let run_a = h.create(&a, "run-a");
    assert_eq!(run_a["snapshot"]["schema"], 2);
    assert_eq!(run_a["snapshot"]["context"]["id"], a["id"]);
    assert_eq!(run_a["snapshot"]["context"]["revision"], a["revision"]);
    assert_eq!(run_a["snapshot"]["context"]["digest"], a["digest"]);
    h.dispatch(&run_a);
    let done_a = h.wait_state(run_a["id"].as_str().unwrap(), "succeeded");
    let artifact_a = h.artifact(&done_a);
    assert_eq!(artifact_a["text"], format!("Context draft: {A}"));
    let run_b = h.create(&b, "run-b");
    assert_eq!(
        run_b["snapshot"]["assignments"], run_a["snapshot"]["assignments"],
        "fixture must actually reuse the same endpoint identities"
    );
    std::fs::write(h.daemon.state.join(format!("context-probe-{}.json",run_b["id"].as_str().unwrap())),json!({"artifact_id":artifact_a["id"],"context_id":a["id"],"install_id":h.install["install_id"],"run_id":run_a["id"],"task_id":done_a["steps"][0]["task_id"],"canaries":[A,B]}).to_string()).unwrap();
    h.dispatch(&run_b);
    let done_b = h.wait_state(run_b["id"].as_str().unwrap(), "succeeded");
    let artifact_b = h.artifact(&done_b);
    assert_eq!(artifact_b["text"], format!("Context draft: {B}"));
    assert_ne!(artifact_b["digest"], artifact_a["digest"]);
    assert_eq!(done_a["reviews"][0]["decision"], "approve");
    assert_eq!(done_b["reviews"][0]["decision"], "approve");
    let receipts = std::fs::read_to_string(
        h.daemon
            .state
            .join("agents")
            .join(REVIEWER)
            .join("app-context-receipts.jsonl"),
    )
    .unwrap();
    let receipt = receipts
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|r| r["run_id"] == run_b["id"])
        .unwrap();
    assert_eq!(receipt["native_turn_observed"], true);
    assert_eq!(receipt["own_dependency_positive"], true);
    let cases = receipt["native_scope_cases"].as_array().unwrap();
    assert_eq!(cases.len(), 8);
    assert!(cases
        .iter()
        .all(|c| c["ok"] == false && c["error_kind"] == "rejected"));
    assert!(cases.iter().any(|c| c["detached"] == true));
    let projections = receipt["projection_cases"].as_array().unwrap();
    assert_eq!(projections.len(), 16);
    assert!(projections.iter().all(|p| p["redacted"] == true));
    assert_eq!(projections.iter().filter(|p| p["ok"] == true).count(), 10);

    h.daemon.operator_rpc("app_context_archive",json!({"install_id":h.install["install_id"],"context_id":a["id"],"expected_revision":a["revision"]})).unwrap();
    assert_eq!(
        h.artifact(&done_a),
        artifact_a,
        "archival destroyed operator durable artifact audit"
    );
    assert!(!h.root.path().join("pm/site").exists());
    let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
    for table in ["platform_grants", "platform_effects"] {
        let n: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "context execution widened account authority");
    }
}
#[test]
fn cad690_archive_during_actual_writer_turn_rejects_late_material_and_preserves_history() {
    let h = Contexts::new();
    let c = h.context("Archive", A, "archive-context");
    let run = h.create(&c, "held-run");
    let id = run["id"].as_str().unwrap();
    std::fs::write(
        h.daemon.state.join(format!("context-hold-writer-{id}")),
        "hold",
    )
    .unwrap();
    h.dispatch(&run);
    let held = h.daemon.state.join(format!("context-writer-{id}.held"));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !held.exists() {
        assert!(Instant::now() < deadline, "actual writer never held");
        std::thread::sleep(Duration::from_millis(20));
    }
    h.daemon.operator_rpc("app_context_archive",json!({"install_id":h.install["install_id"],"context_id":c["id"],"expected_revision":c["revision"]})).unwrap();
    std::fs::write(
        h.daemon.state.join(format!("context-writer-{id}.release")),
        "release",
    )
    .unwrap();
    let after_dispatch = h
        .daemon
        .operator_rpc("app_run_show", json!({"run_id":id}))
        .unwrap();
    let step = after_dispatch["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["message_id"].is_string())
        .unwrap();
    let emitted = h.daemon.state.join(format!(
        "context-result-emitted-{id}-{}",
        step["step_id"].as_str().unwrap()
    ));
    let result_deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let agent = h
            .daemon
            .operator_rpc("agent_show", json!({"alias":WRITER}))
            .unwrap();
        if emitted.exists()
            && matches!(agent["agent"]["state"].as_str(), Some("idle" | "attention"))
        {
            break;
        }
        assert!(
            Instant::now() < result_deadline,
            "actual late provider result never settled: {agent}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let final_run = h
        .daemon
        .operator_rpc("app_run_show", json!({"run_id":id}))
        .unwrap();
    assert_ne!(final_run["state"], "succeeded");
    assert!(final_run["artifacts"].as_array().unwrap().is_empty());
    assert!(final_run["reviews"].as_array().unwrap().is_empty());
    assert_eq!(
        final_run["snapshot"], run["snapshot"],
        "archive rewrote immutable provenance"
    );
    assert!(h
        .daemon
        .operator_rpc("app_run_dispatch", json!({"run_id":id}))
        .is_err());
    let historical = h.context_show(&c);
    assert_eq!(historical["state"], "archived");
}

#[test]
fn cad690_concurrent_context_updates_compare_exact_revision_without_lost_write() {
    let h = Contexts::new();
    let c = h.context("CAS", A, "cas-context");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let updates = std::thread::scope(|scope| {
        let one = barrier.clone();
        let two = barrier.clone();
        let daemon = &h.daemon;
        let install = &h.install;
        let context = &c;
        let a=scope.spawn(move || {one.wait();daemon.operator_rpc("app_context_update",json!({"install_id":install["install_id"],"context_id":context["id"],"expected_revision":context["revision"],"label":"Winner A","input_defaults":{"source":format!("CONTEXT_SOURCE={A}")}}))});
        let b=scope.spawn(move || {two.wait();daemon.operator_rpc("app_context_update",json!({"install_id":install["install_id"],"context_id":context["id"],"expected_revision":context["revision"],"label":"Winner B","input_defaults":{"source":format!("CONTEXT_SOURCE={B}")}}))});
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_ne!(
        updates.0.is_ok(),
        updates.1.is_ok(),
        "revision CAS allowed both writes or refused both populated operators"
    );
    let winner = updates.0.or(updates.1).unwrap()["context"].clone();
    assert_eq!(
        winner["revision"].as_u64().unwrap(),
        c["revision"].as_u64().unwrap() + 1
    );
    assert_eq!(h.context_show(&c), winner);
}

#[test]
fn cad690_existing_other_installation_cannot_transfer_context_authority() {
    let h = Contexts::new();
    let a = h.context("First", A, "first-install-context");
    let bundle = h.root.path().join("other-bundle");
    std::fs::create_dir_all(bundle.join("workflows")).unwrap();
    let manifest = std::fs::read_to_string(h.root.path().join("bundle/app.md")).unwrap();
    let renamed = manifest.replace("app: local-content", "app: other-content");
    assert_ne!(manifest, renamed);
    std::fs::write(bundle.join("app.md"), renamed).unwrap();
    std::fs::write(bundle.join("workflows/draft.md"), &h.workflow).unwrap();
    let other = h
        .daemon
        .operator_rpc("app_workspace_install", json!({"source":bundle}))
        .unwrap();
    assert_ne!(other["install_id"], h.install["install_id"]);
    h.daemon
        .operator_rpc(
            "app_local_install_approve",
            json!({"install_id":other["install_id"],"digest":other["digest"]}),
        )
        .unwrap();
    let b=h.daemon.operator_rpc("app_context_create",json!({"install_id":other["install_id"],"label":"Second","input_defaults":{"source":format!("CONTEXT_SOURCE={B}")},"request_id":"second-install-context"})).unwrap()["context"].clone();
    assert_eq!(
        h.daemon
            .operator_rpc(
                "app_context_show",
                json!({"install_id":other["install_id"],"context_id":b["id"]})
            )
            .unwrap()["context"],
        b
    );
    let positive=h.daemon.operator_rpc("app_run_create",json!({"install_id":other["install_id"],"context_id":b["id"],"workflow":"draft","inputs":{"subject":"Other install","writer":WRITER,"reviewer":REVIEWER},"request_id":"other-positive","owner_pm":OWNER})).unwrap();
    assert_eq!(positive["snapshot"]["context"]["id"], b["id"]);
    for (method, params) in [
        (
            "app_context_show",
            json!({"install_id":other["install_id"],"context_id":a["id"]}),
        ),
        (
            "app_context_update",
            json!({"install_id":other["install_id"],"context_id":a["id"],"expected_revision":a["revision"],"label":"Transferred","input_defaults":{"source":"x"}}),
        ),
        (
            "app_context_archive",
            json!({"install_id":other["install_id"],"context_id":a["id"],"expected_revision":a["revision"]}),
        ),
        (
            "app_run_create",
            json!({"install_id":other["install_id"],"context_id":a["id"],"workflow":"draft","inputs":{"subject":"Wrong install","writer":WRITER,"reviewer":REVIEWER},"request_id":"wrong-install","owner_pm":OWNER}),
        ),
    ] {
        let error = h.daemon.operator_rpc(method, params).unwrap_err();
        assert_eq!(error.kind(), "rejected");
        assert!(!error.to_string().contains(A));
    }
    assert_eq!(
        h.context_show(&a),
        a,
        "cross-install mutation touched original context"
    );
    assert_eq!(
        h.daemon
            .operator_rpc(
                "app_context_show",
                json!({"install_id":other["install_id"],"context_id":b["id"]})
            )
            .unwrap()["context"],
        b
    );
}
#[test]
fn cad690_actual_context_restart_preserves_receipts_without_replay_or_fallback() {
    let h = Contexts::new();
    let a = h.context("History", A, "restart-a");
    let b = h.context("Interrupted", B, "restart-b");
    let run_a = h.create(&a, "restart-run-a");
    h.dispatch(&run_a);
    let done_a = h.wait_state(run_a["id"].as_str().unwrap(), "succeeded");
    let artifact_a = h.artifact(&done_a);
    let archived=h.daemon.operator_rpc("app_context_archive",json!({"install_id":h.install["install_id"],"context_id":a["id"],"expected_revision":a["revision"]})).unwrap()["context"].clone();
    let run_b = h.create(&b, "restart-run-b");
    let id = run_b["id"].as_str().unwrap();
    std::fs::write(
        h.daemon.state.join(format!("context-hold-reviewer-{id}")),
        "hold",
    )
    .unwrap();
    h.dispatch(&run_b);
    let held = h.daemon.state.join(format!("context-reviewer-{id}.held"));
    let deadline = Instant::now() + Duration::from_secs(25);
    while !held.exists() {
        assert!(
            Instant::now() < deadline,
            "real dependent reviewer did not hold before restart"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let partial = h
        .daemon
        .operator_rpc("app_run_show", json!({"run_id":id}))
        .unwrap();
    assert_eq!(partial["state"], "running");
    assert_eq!(partial["artifacts"].as_array().unwrap().len(), 1);
    assert!(partial["reviews"].as_array().unwrap().is_empty());
    let artifact_b = h.artifact(&partial);
    assert_eq!(artifact_b["text"], format!("Context draft: {B}"));
    let Contexts {
        root,
        mut daemon,
        install,
        workflow: _,
    } = h;
    let state = daemon.state.clone();
    let _state_owner = std::mem::replace(&mut daemon.dir, tempfile::tempdir().unwrap());
    drop(daemon);
    let opts = daemon_opts();
    opts.provider_env
        .set("CADENCE_PM_DIR", root.path().join("pm").to_str().unwrap());
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/app-context-pi.py");
    opts.provider_env.set(
        "CADENCE_PI_COMMAND",
        format!("python3 {}", script.display()),
    );
    let restarted = TestDaemon::start_on_opts(state, opts);
    let deadline = Instant::now() + Duration::from_secs(40);
    let failed = loop {
        let row = restarted
            .operator_rpc("app_run_show", json!({"run_id":id}))
            .unwrap();
        if row["state"] == "failed" {
            break row;
        }
        assert_eq!(
            row["state"], "running",
            "uncertain contextual review revived or completed"
        );
        assert!(
            Instant::now() < deadline,
            "interrupted contextual run never recovered"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(failed["snapshot"], run_b["snapshot"]);
    assert_eq!(failed["snapshot_digest"], run_b["snapshot_digest"]);
    assert_eq!(failed["artifacts"], partial["artifacts"]);
    assert!(failed["reviews"].as_array().unwrap().is_empty());
    let restored_a = restarted
        .operator_rpc("app_run_show", json!({"run_id":run_a["id"]}))
        .unwrap();
    assert_eq!(restored_a["state"], "succeeded");
    assert_eq!(restored_a["snapshot"], run_a["snapshot"]);
    assert_eq!(restored_a["reviews"], done_a["reviews"]);
    for (context, expected) in [(&a, &archived), (&b, &b)] {
        assert_eq!(
            restarted
                .operator_rpc(
                    "app_context_show",
                    json!({"install_id":install["install_id"],"context_id":context["id"]})
                )
                .unwrap()["context"],
            *expected
        );
    }
    for original in [&artifact_a, &artifact_b] {
        assert_eq!(
            restarted
                .operator_rpc("app_run_artifact", json!({"artifact_id":original["id"]}))
                .unwrap(),
            *original,
            "restart lost operator durable context material"
        );
    }
    assert!(restarted
        .operator_rpc("app_run_dispatch", json!({"run_id":id}))
        .is_err());
    let error=restarted.operator_rpc("app_run_create",json!({"install_id":install["install_id"],"context_id":a["id"],"workflow":"draft","inputs":{"subject":"No fallback","source":format!("CONTEXT_SOURCE={A}"),"writer":WRITER,"reviewer":REVIEWER},"request_id":"archived-after-restart","owner_pm":OWNER})).unwrap_err();
    assert_eq!(error.kind(), "rejected");
    assert!(
        error.to_string().contains("archived"),
        "wrong rejection hid fallback proof: {error}"
    );
    let db = rusqlite::Connection::open(restarted.state.join("cadence.sqlite3")).unwrap();
    let kickoffs: i64 = db
        .query_row(
            "SELECT count(*) FROM messages WHERE source='app_run_dispatch'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kickoffs, 4, "restart re-dispatched contextual work");
}

#[test]
fn cad690_current_context_revision_rejects_actual_late_reviewer_result() {
    let h = Contexts::new();
    let context = h.context("Revision fence", A, "revision-fence");
    let run = h.create(&context, "revision-held-review");
    let id = run["id"].as_str().unwrap();
    std::fs::write(
        h.daemon.state.join(format!("context-hold-reviewer-{id}")),
        "hold",
    )
    .unwrap();
    h.dispatch(&run);
    let deadline = Instant::now() + Duration::from_secs(25);
    while !h
        .daemon
        .state
        .join(format!("context-reviewer-{id}.held"))
        .exists()
    {
        assert!(
            Instant::now() < deadline,
            "real reviewer never fetched its own dependency"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let before = h
        .daemon
        .operator_rpc("app_run_show", json!({"run_id":id}))
        .unwrap();
    assert_eq!(before["state"], "running");
    assert_eq!(before["approved_digest"], before["snapshot_digest"]);
    assert_eq!(before["artifacts"].as_array().unwrap().len(), 1);
    assert!(before["reviews"].as_array().unwrap().is_empty());
    let artifact = h.artifact(&before);
    assert_eq!(artifact["text"], format!("Context draft: {A}"));
    let reviewer = before["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["state"] == "running")
        .expect("actual reviewer step running");
    let emitted = h.daemon.state.join(format!(
        "context-result-emitted-{id}-{}",
        reviewer["step_id"].as_str().unwrap()
    ));
    // Counterfactual fixture: change only the persisted CAS revision, bypassing
    // CRUD's proactive cancellation. The real authenticated completion must
    // independently enforce current-context proof inside its SQL transaction.
    let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
    assert_eq!(db.execute("UPDATE app_contexts SET revision=revision+1 WHERE id=? AND install_id=? AND revision=?",
        rusqlite::params![context["id"].as_str(), h.install["install_id"].as_str(), context["revision"].as_i64()]).unwrap(), 1);
    drop(db);
    assert_eq!(
        h.daemon
            .operator_rpc("app_run_show", json!({"run_id":id}))
            .unwrap(),
        before,
        "raw revision fixture unexpectedly invalidated run before completion"
    );
    std::fs::write(
        h.daemon
            .state
            .join(format!("context-reviewer-{id}.release")),
        "release",
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let agent = h
            .daemon
            .operator_rpc("agent_show", json!({"alias":REVIEWER}))
            .unwrap();
        if emitted.exists()
            && matches!(agent["agent"]["state"].as_str(), Some("idle" | "attention"))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "actual reviewer finish never settled: {agent}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let finished = h
        .daemon
        .operator_rpc("app_run_show", json!({"run_id":id}))
        .unwrap();
    assert_eq!(
        finished["state"], "failed",
        "stale-context authenticated review was accepted"
    );
    assert!(
        finished["reviews"].as_array().unwrap().is_empty(),
        "stale-context review was persisted"
    );
    assert_eq!(
        finished["artifacts"], before["artifacts"],
        "historical writer artifact was changed"
    );
    assert_eq!(finished["snapshot"], run["snapshot"]);
    assert_eq!(
        h.daemon
            .operator_rpc("app_run_artifact", json!({"artifact_id":artifact["id"]}))
            .unwrap(),
        artifact
    );
}
