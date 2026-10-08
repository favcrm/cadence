//! Test-only actual loaded-credential result wrapper; not shipped SMTP support.
//! Delegates exact existing descriptor, prepare/preview/capture/generic counter.
use super::{checked, PASSWORD};
use crate::contract_fixture::{ToolTable, Verified};
use crate::platform::connections::ProviderDescriptor;
use crate::platform::smtp::test_artifact_adapter::SmtpArtifactAdapter;
use crate::platform::{AppArtifactError, PlatformAdapter};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Clone, Copy)]
pub(super) struct Probe {
    pub(super) canonical_eight_quote: bool,
    pub(super) raw_scalar_unsafe: bool,
    pub(super) serialized_raw_pattern_clean: bool,
    pub(super) decoded_is_selected_password: bool,
}
// Exact retained material fixture expects this type name; its bytes unchanged.
pub(super) struct OutcomeAdapter {
    pub(super) stage: SmtpArtifactAdapter,
    leak_result: bool,
    calls: AtomicU64,
    pub(super) probe: Mutex<Option<Probe>>,
}
impl OutcomeAdapter {
    pub(super) fn new(leak_result: bool) -> Self {
        Self {
            stage: SmtpArtifactAdapter::new(),
            leak_result,
            calls: AtomicU64::new(0),
            probe: Mutex::new(None),
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
        title: &str,
        body: &str,
        provenance: &Value,
    ) -> std::result::Result<Value, String> {
        self.stage.prepare_app_text(title, body, provenance)
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
        credential: &[u8],
        _tool: &str,
        _input: &Value,
        _key: &str,
        _hash: Option<&str>,
    ) -> std::result::Result<Value, AppArtifactError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (envelope, _) = checked(
            crate::platform::smtp::custody_decode(credential),
            "actual provider canonical credential decode failed",
        );
        assert!(
            envelope.secret() == PASSWORD.as_bytes(),
            "provider must receive the actually enrolled same quoted password"
        );
        let secret = checked(
            std::str::from_utf8(envelope.secret()),
            "canonical password UTF-8 invalid",
        );
        let chars: Vec<_> = secret.chars().collect();
        let canonical_eight_quote = chars.len() == 8
            && chars.iter().take(7).all(|c| c.is_ascii_alphabetic())
            && chars.last() == Some(&'"');
        let value = if self.leak_result {
            Value::String(secret.to_string())
        } else {
            json!({"receipt":"safe"})
        };
        // Calibrate the ACTUAL returned Value; never a different earlier value.
        let serialized = zeroize::Zeroizing::new(value.to_string());
        let decoded: Value = checked(
            serde_json::from_str(&serialized),
            "actual provider JSON calibration failed",
        );
        let probe = Probe {
            canonical_eight_quote,
            raw_scalar_unsafe: value.as_str().is_some_and(|text| {
                crate::platform::refuse_leak("diagnostic", text, envelope.secret()).is_err()
            }),
            serialized_raw_pattern_clean: crate::platform::refuse_leak(
                "diagnostic",
                &serialized,
                envelope.secret(),
            )
            .is_ok(),
            decoded_is_selected_password: decoded
                .as_str()
                .is_some_and(|text| text.as_bytes() == envelope.secret()),
        };
        *self.probe.lock().unwrap_or_else(|p| p.into_inner()) = Some(probe);
        drop(envelope);
        Ok(value)
    }
    // Discriminator: unsafe output must override provider verification to Unknown.
    fn read_back(&self, _tool: &str, _input: &Value) -> Verified {
        Verified::True
    }
    fn source_hash(&self, agent: &str, source: &str) -> Option<String> {
        self.stage.source_hash(agent, source)
    }
}
