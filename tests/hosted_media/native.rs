//! Existing native Pi fixture, with synthetic quote/output at the adapter seam.
//! The real trusted hosted descriptor/registration and broker are retained.
use super::*;
use cadence_agent::contract_fixture::{ToolTable, Verified};
use cadence_agent::platform::{
    agenticos_external, AppCapabilityAsset, AppCapabilityOutput, AppCapabilityQuote,
    PlatformAdapter,
};
use common::app_release::{Release, OWNER, REVIEWER, WRITER};
use image::ImageEncoder as _;
use serde_json::Value;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct SyntheticMedia {
    inner: Arc<dyn PlatformAdapter>,
    quotes: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    png: Vec<u8>,
}
impl PlatformAdapter for SyntheticMedia {
    fn table(&self) -> &ToolTable {
        self.inner.table()
    }
    fn connection_descriptor(
        &self,
    ) -> Option<cadence_agent::platform::connections::ProviderDescriptor> {
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
        credential: &[u8],
        tool: &str,
        input: &Value,
        key: &str,
        hash: Option<&str>,
    ) -> std::result::Result<Value, String> {
        self.inner.execute(credential, tool, input, key, hash)
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
    ) -> std::result::Result<AppCapabilityQuote, String> {
        assert!(credential.is_empty());
        assert_eq!(binding["config"]["account"], "hosted");
        assert_eq!(binding["config"]["connection_kind"], "builtin");
        self.quotes.fetch_add(1, Ordering::SeqCst);
        Ok(AppCapabilityQuote {
            schema: 1,
            currency: "USD".into(),
            unit_price_micros: 31_500,
            units: 1,
            total_price_micros: 31_500,
            price_revision: "media:sha256:synthetic868".into(),
        })
    }
    fn execute_app_capability(
        &self,
        credential: &[u8],
        authority: &Value,
        input: &Value,
        key: &str,
    ) -> std::result::Result<AppCapabilityOutput, String> {
        assert!(credential.is_empty());
        assert_eq!(authority["binding"]["config"]["account"], "hosted");
        assert_eq!(authority["binding"]["config"]["connection_kind"], "builtin");
        assert_eq!(
            authority["binding"]["config"]["mapping"]["capability"],
            "media.generate"
        );
        assert_eq!(authority["slot"], "image");
        assert_eq!(
            authority["inputs"]["source"],
            "JuicySuite CRM helps teams track customers"
        );
        assert_eq!(authority["quote"]["total_price_micros"], 31_500);
        assert_eq!(authority["call_id"], key);
        assert!(key.starts_with("app-call-"));
        assert_eq!(input, &json!({}));
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(AppCapabilityOutput {
            result: json!({"schema":1,"kind":"media.generated.image","provider":"agenticos_external","model":"openai/gpt-image-2.5","quoted_micros":31_500}),
            asset: Some(AppCapabilityAsset {
                media_type: "image/png".into(),
                bytes: self.png.clone(),
            }),
        })
    }
}

#[test]
fn cad868_native_assigned_image_quote_execute_and_replay_use_empty_builtin_custody() {
    let quotes = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let observed_quotes = quotes.clone();
    let observed_calls = calls.clone();
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&[0], 1, 1, image::ExtendedColorType::L8)
        .unwrap();
    let h = Release::with_social_image(move |opts, _| {
        opts.provider_deployments = Some(DeploymentMetadata::parse(br#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@2","transport":"hosted-media-lease@1"}]}"#).unwrap());
        agenticos_external::attach(opts).unwrap();
        let inner = opts.platforms.remove("agenticos_external").unwrap();
        opts.platforms.insert(
            "agenticos_external".into(),
            Arc::new(SyntheticMedia {
                inner,
                quotes,
                calls,
                png,
            }),
        );
    });
    let connections = h.daemon.operator_rpc("connection_list", json!({})).unwrap();
    let hosted = connections["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["provider"] == "agenticos_external")
        .unwrap();
    h.daemon.operator_rpc("app_binding_create", json!({"install_id":h.install["install_id"],"slot":"image","connection_id":hosted["id"],"request_id":"native-hosted-image"})).unwrap();
    h.daemon.operator_rpc("app_binding_create", json!({"install_id":h.install["install_id"],"slot":"publication","connection_id":h.connection,"request_id":"native-hosted-local"})).unwrap();
    let quote = h
        .daemon
        .operator_rpc(
            "app_binding_quote",
            json!({"install_id":h.install["install_id"],"slot":"image"}),
        )
        .unwrap();
    assert_eq!(quote["quote"]["total_price_micros"], 31_500);
    let run = h.daemon.operator_rpc("app_run_create", json!({"install_id":h.install["install_id"],"workflow":"image-instagram","inputs":{"subject":"Customer follow-up","source":"JuicySuite CRM helps teams track customers","brand_voice":"Warm and clear","writer":WRITER,"reviewer":REVIEWER},"request_id":"native-hosted-run","owner_pm":OWNER})).unwrap();
    std::fs::write(
        h.daemon.state.join(format!(
            "social-image-probe-{}.json",
            run["id"].as_str().unwrap()
        )),
        json!({"slot":"image"}).to_string(),
    )
    .unwrap();
    h.dispatch(&run);
    h.wait_state(run["id"].as_str().unwrap(), "succeeded");
    let results = h
        .daemon
        .operator_rpc("app_run_capability_results", json!({"run_id":run["id"]}))
        .unwrap();
    assert_eq!(results["results"].as_array().unwrap().len(), 1);
    assert_eq!(results["results"][0]["asset"]["media_type"], "image/png");
    assert_eq!(
        observed_calls.load(Ordering::SeqCst),
        1,
        "the existing native fixture replays the exact request without another execution"
    );
    assert!(
        observed_quotes.load(Ordering::SeqCst) >= 3,
        "operator discovery, run freeze and execution-time re-quote must all reach the broker"
    );
    let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
    let enrolled: i64 = db
        .query_row(
            "SELECT count(*) FROM platform_credentials WHERE platform='agenticos_external'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(enrolled, 0);
}
