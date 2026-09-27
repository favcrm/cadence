//! CAD692 reviewed local release proofs through real native provider turns.
#![allow(clippy::disallowed_methods)]
pub(crate) mod fault;
use super::{daemon_opts, pi_policy_pm, TestDaemon};
use cadence_agent::issue::Pm;
pub(crate) use fault::Fault as AppFault;
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};
pub(crate) const OWNER: &str = "release-pm";
pub(crate) const WRITER: &str = "release-writer";
pub(crate) const REVIEWER: &str = "release-reviewer";
pub(crate) const A: &str = "CLIENT_A_PRIVATE_FACTS";
pub(crate) const B: &str = "CLIENT_B_PRIVATE_FACTS";
pub(crate) struct Release {
    pub(crate) root: tempfile::TempDir,
    pub(crate) daemon: TestDaemon,
    pub(crate) install: Value,
    pub(crate) connection: String,
}
impl Release {
    pub(crate) fn with_app_fault(fault: AppFault) -> Self {
        Self::with_options(move |opts, _| {
            fault::wrap(
                opts,
                fault,
                std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            );
        })
    }
    pub(crate) fn new() -> Self {
        Self::with_decision_gate(false)
    }
    pub(crate) fn with_decision_gate(hold_decided: bool) -> Self {
        Self::with_options(move |opts, _| {
            if hold_decided {
                opts.effect_execute_gate = Some(std::sync::Arc::new(|_| false));
            }
        })
    }
    pub(crate) fn with_options(
        configure: impl FnOnce(&mut cadence_agent::daemon::ServeOptions, &Path),
    ) -> Self {
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
        configure(&mut opts, &state);
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
    pub(crate) fn context(&self, label: &str, source: &str, request: &str) -> Value {
        self.daemon.operator_rpc("app_context_create",json!({"install_id":self.install["install_id"],"label":label,"input_defaults":{"source":format!("CONTEXT_SOURCE={source}")},"request_id":request})).unwrap()["context"].clone()
    }
    pub(crate) fn create(&self, context: &Value, request: &str) -> Value {
        self.daemon.operator_rpc("app_run_create",json!({"install_id":self.install["install_id"],"context_id":context["id"],"workflow":"draft","inputs":{"subject":"Context draft","writer":WRITER,"reviewer":REVIEWER},"request_id":request,"owner_pm":OWNER})).unwrap()
    }
    pub(crate) fn dispatch(&self, run: &Value) {
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
    pub(crate) fn wait_state(&self, id: &str, terminal: &str) -> Value {
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
    pub(crate) fn artifact(&self, run: &Value) -> Value {
        self.daemon
            .operator_rpc(
                "app_run_artifact",
                json!({"artifact_id":run["artifacts"][0]["id"]}),
            )
            .unwrap()
    }
    pub(crate) fn bind(&self, context: &Value, request: &str) -> Value {
        self.daemon.operator_rpc("app_binding_create", json!({"install_id":self.install["install_id"],"context_id":context["id"],"slot":"publication","connection_id":self.connection,"request_id":request})).unwrap()["binding"].clone()
    }
    pub(crate) fn complete(&self, context: &Value, request: &str) -> Value {
        let run = self.create(context, request);
        self.dispatch(&run);
        self.wait_state(run["id"].as_str().unwrap(), "succeeded")
    }
    pub(crate) fn stage(&self, run: &Value, request: &str) -> Value {
        self.daemon.operator_rpc("app_effect_stage", json!({"run_id":run["id"],"artifact_id":run["artifacts"][0]["id"],"slot":"publication","request_id":request,"title":"Reviewed draft"})).unwrap()["effect"].clone()
    }
    pub(crate) fn items(&self) -> Value {
        self.daemon
            .operator_rpc("platform_outbox", json!({}))
            .unwrap()["items"]
            .clone()
    }
    pub(crate) fn decide(&self, effect: &Value) -> Value {
        self.daemon.operator_rpc("app_effect_decide", json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"decision":"accept"})).unwrap()["effect"].clone()
    }
}
