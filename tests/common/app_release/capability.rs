//! Reviewed in-process read adapter for the actual app-turn authorization test.
use cadence_agent::contract_fixture::{ToolDecl, ToolTable, Verified};
use cadence_agent::platform::connections::{
    BoundActionMapping, CapabilityDescriptor, CapabilitySemantics, ProviderDescriptor,
};
use cadence_agent::platform::{
    AppArtifactError, AppCapabilityAsset, AppCapabilityOutput, AppCapabilityQuote, PlatformAdapter,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

struct SourceAdapter {
    inner: Arc<dyn PlatformAdapter>,
    table: ToolTable,
    calls: Arc<AtomicUsize>,
    price: Arc<AtomicUsize>,
    delay: Duration,
}

pub(crate) fn wrap(
    opts: &mut cadence_agent::daemon::ServeOptions,
    calls: Arc<AtomicUsize>,
    price: Arc<AtomicUsize>,
    delay: Duration,
) {
    let inner = opts.platforms.get("local").unwrap().clone();
    let mut table = inner.table().clone();
    table.tools.push(ToolDecl {
        tool: "fixture_source_posts".into(),
        effect: Some("read".into()),
        scopes: vec![],
        label: Some("Scoped source posts".into()),
    });
    opts.platforms.insert(
        "local".into(),
        Arc::new(SourceAdapter {
            inner,
            table,
            calls,
            price,
            delay,
        }),
    );
}

impl PlatformAdapter for SourceAdapter {
    fn quote_app_capability(
        &self,
        _credential: &[u8],
        binding: &Value,
    ) -> Result<AppCapabilityQuote, String> {
        if binding["config"]["mapping"]["tool"] != "fixture_source_posts" {
            return Err("unrecognized fixture mapping".into());
        }
        let amount = self.price.load(Ordering::SeqCst) as u64;
        Ok(AppCapabilityQuote {
            schema: 1,
            currency: "USD".into(),
            unit_price_micros: amount,
            units: 1,
            total_price_micros: amount,
            price_revision: format!("fixture-price-{amount}"),
        })
    }
    fn table(&self) -> &ToolTable {
        &self.table
    }
    fn connection_descriptor(&self) -> Option<ProviderDescriptor> {
        let mut descriptor = self.inner.connection_descriptor()?;
        descriptor.revision = "cad632-source-fixture/1".into();
        descriptor.capabilities.push(CapabilityDescriptor {
            id: "social.read".into(),
            version: 1,
            tools: vec!["fixture_source_posts".into()],
            scopes: vec![],
            effect: "read".into(),
            semantics: CapabilitySemantics::MetadataRead,
        });
        descriptor.action_mappings.push(BoundActionMapping {
            capability: "social.read".into(),
            version: 1,
            action: "list_posts".into(),
            resource_kind: "connection_account".into(),
            tool: "fixture_source_posts".into(),
            scopes: vec![],
            effect: "read".into(),
            semantics: CapabilitySemantics::MetadataRead,
            input_contract: "cad632.fixture.source.input@1".into(),
            output_contract: "cad632.fixture.source.receipt@1".into(),
        });
        Some(descriptor)
    }
    fn connection_registration(&self) -> Option<String> {
        self.inner.connection_registration()
    }
    fn prepare_app_text(
        &self,
        title: &str,
        body: &str,
        provenance: &Value,
    ) -> Result<Value, String> {
        self.inner.prepare_app_text(title, body, provenance)
    }
    fn prepare_app_artifact(
        &self,
        title: &str,
        body: &str,
        provenance: &Value,
        asset: Option<&Value>,
    ) -> Result<Value, String> {
        self.inner
            .prepare_app_artifact(title, body, provenance, asset)
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
    ) -> Result<Value, String> {
        self.inner.execute(credential, tool, input, key, hash)
    }
    fn execute_app_artifact(
        &self,
        credential: &[u8],
        tool: &str,
        input: &Value,
        key: &str,
        hash: Option<&str>,
    ) -> Result<Value, AppArtifactError> {
        self.inner
            .execute_app_artifact(credential, tool, input, key, hash)
    }
    fn execute_app_capability(
        &self,
        _credential: &[u8],
        authority: &Value,
        input: &Value,
        _key: &str,
    ) -> Result<AppCapabilityOutput, String> {
        let source = authority["inputs"]["source"]
            .as_str()
            .ok_or("frozen source is absent")?;
        if authority["binding"]["config"]["mapping"]["tool"] != "fixture_source_posts"
            || input != &json!({"source":source})
        {
            return Err("source request differs from frozen run resource".into());
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        // Simulate a slow external capability, through the real call-site locks.
        std::thread::sleep(self.delay);
        Ok(AppCapabilityOutput {
            result: json!({"posts":[{"id":"post-1","caption":format!("{source}\nSecond line"),
                "permalink":"https://example.invalid/post-1","taken_at":"2026-09-28T00:00:00Z"}]}),
            asset: Some(AppCapabilityAsset {
                media_type: "application/octet-stream".into(),
                bytes: b"run-bound fixture asset".to_vec(),
            }),
        })
    }
    fn read_back(&self, tool: &str, input: &Value) -> Verified {
        self.inner.read_back(tool, input)
    }
    fn source_hash(&self, agent: &str, source: &str) -> Option<String> {
        self.inner.source_hash(agent, source)
    }
    fn implied_source(&self, agent: &str, tool: &str, input: &Value) -> Option<String> {
        self.inner.implied_source(agent, tool, input)
    }
}
