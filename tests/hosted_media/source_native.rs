//! CAD-1060: a hosted source run through the real broker and native Pi turn.
//! The real hosted descriptor/registration are kept; only the lease door's
//! answers are synthetic. A down gateway must leave nothing retained.
use super::*;
use cadence_agent::contract_fixture::{ToolTable, Verified};
use cadence_agent::platform::connections::ProviderDescriptor;
use cadence_agent::platform::{
    agenticos_external, AppCapabilityOutput, AppCapabilityQuote, PlatformAdapter,
};
use common::app_release::{Release, OWNER, WRITER};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

struct LeaseDoor {
    inner: Arc<dyn PlatformAdapter>,
    up: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
}
impl PlatformAdapter for LeaseDoor {
    fn table(&self) -> &ToolTable {
        self.inner.table()
    }
    fn connection_descriptor(&self) -> Option<ProviderDescriptor> {
        self.inner.connection_descriptor()
    }
    fn connection_registration(&self) -> Option<String> {
        self.inner.connection_registration()
    }
    fn app_credentialless_account(&self, account: &str) -> bool {
        self.inner.app_credentialless_account(account)
    }
    fn reported_manifest_version(&self) -> Option<String> {
        self.inner.reported_manifest_version()
    }
    fn preview(&self, account: &str, tool: &str, input: &Value) -> String {
        self.inner.preview(account, tool, input)
    }
    fn execute(
        &self,
        c: &[u8],
        t: &str,
        i: &Value,
        k: &str,
        h: Option<&str>,
    ) -> Result<Value, String> {
        self.inner.execute(c, t, i, k, h)
    }
    fn read_back(&self, tool: &str, input: &Value) -> Verified {
        self.inner.read_back(tool, input)
    }
    fn source_hash(&self, agent: &str, source: &str) -> Option<String> {
        self.inner.source_hash(agent, source)
    }
    fn quote_app_capability(
        &self,
        credential: &[u8],
        binding: &Value,
    ) -> Result<AppCapabilityQuote, String> {
        assert!(credential.is_empty());
        assert_eq!(binding["config"]["account"], "hosted");
        assert_eq!(binding["config"]["mapping"]["capability"], "social.read");
        Ok(AppCapabilityQuote {
            schema: 1,
            currency: "USD".into(),
            unit_price_micros: 2_000,
            units: 1,
            total_price_micros: 2_000,
            price_revision: "sha256:synthetic1060".into(),
        })
    }
    fn execute_app_capability(
        &self,
        credential: &[u8],
        authority: &Value,
        input: &Value,
        key: &str,
    ) -> Result<AppCapabilityOutput, String> {
        assert!(credential.is_empty(), "no token reaches the hosted door");
        let config = &authority["binding"]["config"];
        assert_eq!(
            (
                config["account"].as_str(),
                config["connection_kind"].as_str()
            ),
            (Some("hosted"), Some("builtin"))
        );
        assert_eq!(authority["slot"], "source");
        assert_eq!(authority["inputs"]["profile_handle"], "juicysuite_crm");
        assert_eq!(authority["call_id"], key);
        assert_eq!(input, &json!({}));
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.up.load(Ordering::SeqCst) {
            return Err("AgenticOS source request could not reach the provider".into());
        }
        Ok(AppCapabilityOutput {
            result: json!({"schema":1,"kind":"social.posts","provider":"agenticos_external","handle":"juicysuite_crm","posts":[{"id":"post-1","caption":"Hosted caption"}]}),
            asset: None,
        })
    }
}

#[test]
fn cad1060_native_hosted_source_down_gateway_retains_nothing_then_one_receipt() {
    let up = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let (door_up, door_calls) = (up.clone(), calls.clone());
    let h = Release::with_social_image(move |opts, _| {
        opts.provider_deployments = Some(DeploymentMetadata::parse(br#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@2","transport":"hosted-media-lease@1"}]}"#).unwrap());
        agenticos_external::attach(opts).unwrap();
        let inner = opts.platforms.remove("agenticos_external").unwrap();
        let door = LeaseDoor {
            inner,
            up: door_up,
            calls: door_calls,
        };
        opts.platforms
            .insert("agenticos_external".into(), Arc::new(door));
    });
    let connections = h.daemon.operator_rpc("connection_list", json!({})).unwrap();
    let hosted = connections["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["provider"] == "agenticos_external")
        .unwrap();
    h.daemon.operator_rpc("app_binding_create", json!({"install_id":h.install["install_id"],"slot":"source","connection_id":hosted["id"],"request_id":"native-hosted-source"})).unwrap();
    let results = |run: &Value| -> Vec<Value> {
        let reply = h
            .daemon
            .operator_rpc("app_run_capability_results", json!({"run_id":run["id"]}))
            .unwrap();
        reply["results"].as_array().unwrap().clone()
    };
    let mut runs = Vec::new();
    for (request, state) in [
        ("hosted-source-down", "failed"),
        ("hosted-source-up", "succeeded"),
    ] {
        if state == "succeeded" {
            up.store(true, Ordering::SeqCst);
        }
        let run = h.daemon.operator_rpc("app_run_create", json!({"install_id":h.install["install_id"],"workflow":"source-instagram","inputs":{"profile_handle":"juicysuite_crm","writer":WRITER},"request_id":request,"owner_pm":OWNER})).unwrap();
        assert_eq!(
            run["snapshot"]["quotes"]["source"]["total_price_micros"],
            2_000
        );
        let probe = h.daemon.state.join(format!(
            "social-image-probe-{}.json",
            run["id"].as_str().unwrap()
        ));
        std::fs::write(probe, json!({"slot":"source"}).to_string()).unwrap();
        h.dispatch(&run);
        runs.push(h.wait_state(run["id"].as_str().unwrap(), state));
    }
    assert!(runs[0]["artifacts"].as_array().unwrap().is_empty());
    assert!(
        results(&runs[0]).is_empty(),
        "a down gateway retains no receipt"
    );
    let retained = results(&runs[1]);
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0]["result"]["posts"][0]["id"], "post-1");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "one door call per run; the replay is broker-served"
    );
}
