//! Test-only raw-title omission wrapper; no shipping descriptor or trait change.
//! Reuses exact sealed SMTP metadata and actual prepare/preview/counter capture.
use crate::contract_fixture::{ToolTable, Verified};
use crate::platform::connections::ProviderDescriptor;
use crate::platform::smtp::test_artifact_adapter::SmtpArtifactAdapter;
use crate::platform::{AppArtifactError, PlatformAdapter};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

// Keep the reviewed fixture's type name so its exact bytes need no changes.
pub(super) struct OutcomeAdapter {
    pub(super) stage: SmtpArtifactAdapter,
    calls: AtomicU64,
}
impl OutcomeAdapter {
    pub(super) fn new() -> Self {
        Self {
            stage: SmtpArtifactAdapter::new(),
            calls: AtomicU64::new(0),
        }
    }
    pub(super) fn count(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}
impl PlatformAdapter for OutcomeAdapter {
    fn table(&self) -> &ToolTable {
        self.stage.table()
    }
    fn connection_descriptor(&self) -> Option<ProviderDescriptor> {
        self.stage.connection_descriptor()
    }
    fn connection_registration(&self) -> Option<String> {
        self.stage.connection_registration()
    }
    fn reported_manifest_version(&self) -> Option<String> {
        self.stage.reported_manifest_version()
    }
    fn prepare_app_text(
        &self,
        _title: &str,
        body: &str,
        provenance: &Value,
    ) -> std::result::Result<Value, String> {
        // Intentionally omit the caller's raw title. This exact input is produced
        // by the actual RPC callback and captured by the existing sealed seam.
        self.stage
            .prepare_app_text("Renderer-owned safe heading", body, provenance)
    }
    fn preview(&self, account: &str, tool: &str, input: &Value) -> String {
        self.stage.preview(account, tool, input)
    }
    fn execute(
        &self,
        credential: &[u8],
        tool: &str,
        input: &Value,
        key: &str,
        hash: Option<&str>,
    ) -> std::result::Result<Value, String> {
        self.stage.execute(credential, tool, input, key, hash)
    }
    fn execute_app_artifact(
        &self,
        _credential: &[u8],
        _tool: &str,
        _input: &Value,
        _key: &str,
        _hash: Option<&str>,
    ) -> std::result::Result<Value, AppArtifactError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"receipt":"safe"}))
    }
    fn read_back(&self, tool: &str, input: &Value) -> Verified {
        self.stage.read_back(tool, input)
    }
    fn source_hash(&self, agent: &str, source: &str) -> Option<String> {
        self.stage.source_hash(agent, source)
    }
}
