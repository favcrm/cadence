//! CAD692 reviewed local release proofs through real native provider turns.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, pi_policy_pm, plant_member_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};
const OWNER: &str = "release-pm";
const WRITER: &str = "release-writer";
const REVIEWER: &str = "release-reviewer";
const A: &str = "CLIENT_A_PRIVATE_FACTS";
const B: &str = "CLIENT_B_PRIVATE_FACTS";
struct Release {
    root: tempfile::TempDir,
    daemon: TestDaemon,
    install: Value,
    connection: String,
}
impl Release {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        pi_policy_pm(&pm.dir);
        let mut opts = daemon_opts();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/app-release-pi.py");
        opts.provider_env.set(
            "CADENCE_PI_COMMAND",
            format!("python3 {}", script.display()),
        );
        let state = root.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        cadence_agent::platform::local::register_at(
            &state,
            &mut opts,
            root.path().join("outbox"),
            "http://localhost:3119".into(),
        );
        let daemon = TestDaemon::start_on_opts(state, opts);
        let connections = daemon.operator_rpc("connection_list", json!({})).unwrap();
        let connection = connections["connections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["provider"] == "local" && c["account"] == "local")
            .expect("registered Local builtin")["id"]
            .as_str()
            .unwrap()
            .to_string();
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
        let manifest = std::fs::read_to_string(original.join("app.md")).unwrap();
        let manifest = manifest.replace("  connections: []", "  connections: []\n  capabilities:\n    publication:\n      schema: 1\n      capability: text.publish\n      version: 1\n      action: publish\n      resource_kind: connection_account\n      effect: send");
        std::fs::write(source.join("app.md"), manifest).unwrap();
        let text = std::fs::read_to_string(original.join("workflows/draft.md")).unwrap();
        let workflow = text
            .replace("source: { ask:", "source: { context_default: true, ask:")
            .replacen("---\n", "---\npublication_slot: publication\n", 1);
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
            connection,
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
    fn bind(&self, context: &Value, request: &str) -> Value {
        self.daemon.operator_rpc("app_binding_create", json!({"install_id":self.install["install_id"],"context_id":context["id"],"slot":"publication","connection_id":self.connection,"request_id":request})).unwrap()["binding"].clone()
    }
    fn complete(&self, context: &Value, request: &str) -> Value {
        let run = self.create(context, request);
        self.dispatch(&run);
        self.wait_state(run["id"].as_str().unwrap(), "succeeded")
    }
    fn stage(&self, run: &Value, request: &str) -> Value {
        self.daemon.operator_rpc("app_effect_stage", json!({"run_id":run["id"],"artifact_id":run["artifacts"][0]["id"],"slot":"publication","request_id":request,"title":"Reviewed draft"})).unwrap()["effect"].clone()
    }
    fn items(&self) -> Value {
        self.daemon
            .operator_rpc("platform_outbox", json!({}))
            .unwrap()["items"]
            .clone()
    }
    fn decide(&self, effect: &Value) -> Value {
        self.daemon.operator_rpc("app_effect_decide", json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"decision":"accept"})).unwrap()["effect"].clone()
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
fn cad692_actual_accepted_artifact_waits_then_releases_one_project_free_local_item() {
    let h = Release::new();
    let context = h.context("Client A", A, "a");
    let binding = h.bind(&context, "binding-a");
    let run = h.complete(&context, "run-a");
    assert_eq!(run["snapshot"]["schema"], 3);
    assert_eq!(run["reviews"].as_array().unwrap().len(), 1);
    let artifact = h.artifact(&run);
    assert_eq!(artifact["text"], format!("Context draft: {A}"));
    assert!(
        h.items().as_array().unwrap().is_empty(),
        "review completion wrote outward material"
    );
    let waiting = h.stage(&run, "release-a");
    assert_eq!(waiting["state"], "waiting");
    assert!(waiting["digest"].is_string());
    assert_eq!(
        h.stage(&run, "release-a"),
        waiting,
        "immutable stage replay changed receipt"
    );
    assert!(
        h.items().as_array().unwrap().is_empty(),
        "staging performed a send"
    );
    let changed = h.daemon.operator_rpc("app_effect_stage", json!({"run_id":run["id"],"artifact_id":artifact["id"],"slot":"publication","request_id":"release-a","title":"Different title"}));
    assert!(changed.is_err(), "same request accepted changed input");
    let done = h.decide(&waiting);
    assert_eq!(done["state"], "done");
    let items = h.items();
    assert_eq!(items.as_array().unwrap().len(), 1);
    assert_eq!(items[0]["effect_id"], waiting["effect_id"]);
    let detail = h
        .daemon
        .operator_rpc("platform_outbox", json!({"effect_id":waiting["effect_id"]}))
        .unwrap();
    assert_eq!(
        detail["item"]["post"],
        format!("# Reviewed draft\n\nContext draft: {A}")
    );
    assert_eq!(detail["item"]["scope"]["kind"], "app_artifact");
    assert_eq!(
        detail["item"]["scope"]["install_id"],
        h.install["install_id"]
    );
    assert_eq!(detail["item"]["scope"]["context_id"], context["id"]);
    assert!(
        detail["item"]["project"].is_null(),
        "app item impersonated a project"
    );
    assert_eq!(detail["item"]["provenance"]["run_id"], run["id"]);
    assert_eq!(detail["item"]["provenance"]["artifact_id"], artifact["id"]);
    assert_eq!(detail["item"]["provenance"]["binding_id"], binding["id"]);
    let _ = h.daemon.operator_rpc(
        "app_effect_decide",
        json!({"effect_id":waiting["effect_id"],"digest":waiting["digest"],"decision":"accept"}),
    );
    assert_eq!(h.items(), items, "repeated release duplicated Local write");
    let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
    for table in ["platform_grants", "platform_account_defaults"] {
        let count: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "release created ambient authority in {table}");
    }
    assert!(!h.root.path().join("pm/projects/site").exists());
}

#[test]
fn cad692_populated_native_setsid_and_forged_callers_cannot_stage_bind_or_release() {
    let h = Release::new();
    let c = h.context("Private", A, "private");
    let binding = h.bind(&c, "private-binding");
    let run = h.complete(&c, "private-run");
    let effect = h.stage(&run, "private-effect");
    let mut lane = LaneShell::spawn(h.root.path());
    plant_member_pane(
        &h.daemon,
        "release-native-worker",
        "claude",
        None,
        lane.pid(),
    );
    let methods = [
        (
            "app_binding_list",
            json!({"install_id":h.install["install_id"]}),
        ),
        (
            "app_binding_show",
            json!({"install_id":h.install["install_id"],"binding_id":binding["id"]}),
        ),
        (
            "app_binding_update",
            json!({"install_id":h.install["install_id"],"binding_id":binding["id"],"expected_revision":binding["revision"],"connection_id":h.connection}),
        ),
        (
            "app_binding_revoke",
            json!({"install_id":h.install["install_id"],"binding_id":binding["id"],"expected_revision":binding["revision"]}),
        ),
        (
            "app_effect_stage",
            json!({"run_id":run["id"],"artifact_id":run["artifacts"][0]["id"],"slot":"publication","request_id":"attack","title":"Stolen"}),
        ),
        ("app_effect_show", json!({"effect_id":effect["effect_id"]})),
        (
            "app_effect_list",
            json!({"install_id":h.install["install_id"]}),
        ),
        (
            "app_effect_decide",
            json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"decision":"accept"}),
        ),
    ];
    let mut failures = Vec::new();
    for detached in [false, true] {
        for (method, params) in &methods {
            for forged in [false, true] {
                let mut params = params.clone();
                if forged {
                    params["operator"] = json!(true);
                    params["agent"] = json!(OWNER);
                }
                let answer = native(&mut lane, &h.daemon.state, detached, method, params);
                if answer["ok"] != false
                    || answer["error"]["kind"] != "rejected"
                    || answer.get("result").is_some()
                {
                    failures.push(format!(
                        "{method} detached={detached} forged={forged}: {answer}"
                    ));
                }
                assert!(
                    !answer.to_string().contains(A),
                    "native release projection leaked private body"
                );
            }
        }
    }
    assert!(
        failures.is_empty(),
        "native app release operator guard failed: {failures:?}"
    );
    assert_eq!(
        h.daemon
            .operator_rpc("app_effect_show", json!({"effect_id":effect["effect_id"]}))
            .unwrap()["effect"],
        effect
    );
    assert!(h.items().as_array().unwrap().is_empty());
    assert_eq!(
        h.decide(&effect)["state"],
        "done",
        "positive release unavailable after denied calls"
    );
}

#[test]
fn cad692_shared_workers_two_contexts_pin_exact_binding_and_artifact() {
    let h = Release::new();
    let a = h.context("A", A, "two-a");
    let b = h.context("B", B, "two-b");
    h.bind(&a, "bind-a");
    h.bind(&b, "bind-b");
    let ra = h.complete(&a, "two-run-a");
    let rb = h.complete(&b, "two-run-b");
    assert_eq!(ra["snapshot"]["assignments"], rb["snapshot"]["assignments"]);
    let ea = h.stage(&ra, "two-effect-a");
    let eb = h.stage(&rb, "two-effect-b");
    assert_ne!(ea["digest"], eb["digest"]);
    let wrong=h.daemon.operator_rpc("app_effect_stage",json!({"run_id":rb["id"],"artifact_id":ra["artifacts"][0]["id"],"slot":"publication","request_id":"wrong-artifact","title":"Wrong"}));
    assert_eq!(wrong.unwrap_err().kind(), "rejected");
    h.decide(&ea);
    h.decide(&eb);
    assert_eq!(h.items().as_array().unwrap().len(), 2);
    for (effect, canary, foreign) in [(&ea, A, B), (&eb, B, A)] {
        let item = h
            .daemon
            .operator_rpc("platform_outbox", json!({"effect_id":effect["effect_id"]}))
            .unwrap();
        assert!(item["item"]["post"].as_str().unwrap().contains(canary));
        assert!(!item["item"]["post"].as_str().unwrap().contains(foreign));
    }
}
