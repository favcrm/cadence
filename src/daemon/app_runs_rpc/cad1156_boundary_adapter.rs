//! TEST-ONLY boundary adapter; retains sealed SMTP metadata and counted capture.
//! No descriptor/mapping, transport, custody or production API change.
use crate::contract_fixture::{ToolTable, Verified};
use crate::platform::connections::ProviderDescriptor;
use crate::platform::smtp::test_artifact_adapter::SmtpArtifactAdapter;
use crate::platform::{AppArtifactError, PlatformAdapter};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) struct BoundaryAdapter {
    pub(super) stage: SmtpArtifactAdapter,
    echo_prepare_error: bool,
    calls: AtomicU64,
}
impl BoundaryAdapter {
    pub(super) fn new(echo_prepare_error: bool) -> Self {
        Self {
            stage: SmtpArtifactAdapter::new(),
            echo_prepare_error,
            calls: AtomicU64::new(0),
        }
    }
    pub(super) fn count(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}
impl PlatformAdapter for BoundaryAdapter {
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
        title: &str,
        body: &str,
        provenance: &Value,
    ) -> std::result::Result<Value, String> {
        let input = self.stage.prepare_app_text(title, body, provenance)?;
        if self.echo_prepare_error {
            Err(title.to_owned())
        } else {
            Ok(input)
        }
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
        // Deliberately does NOT decode: the daemon must refuse corrupt custody
        // before any provider attempt, not rely on the fake adapter to refuse.
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"receipt":"safe"}))
    }
    fn read_back(&self, _tool: &str, _input: &Value) -> Verified {
        Verified::True
    }
    fn source_hash(&self, _agent: &str, _source: &str) -> Option<String> {
        None
    }
}
