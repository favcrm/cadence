//! CAD-1156 test-only counted `smtp` app-artifact adapter — FRESH-MAIN
//! `4e3f729c` `PlatformAdapter`. Mounted `pub(crate) mod
//! test_artifact_adapter` under `#[cfg(all(test, unix,
//! feature = "test-seam"))]` in `src/platform/smtp.rs`; the daemon rig
//! injects it into `opts.platforms["smtp"]`.
//!
//! TEST-ONLY metadata (labeled, never a shipped descriptor):
//! - `TABLE` declares the real `email.send` tool (effect `send`, scope
//!   `email:send`, manifest pin `smtp-connections/1`) — the reviewed
//!   tool the smtp descriptor reports — so `ProviderDescriptor::validate`
//!   and the capability/tool match hold.
//! - Retain the email.send capability and add ONE synthetic text.publish/1
//!   capability plus a matching publish/connection_account mapping onto
//!   email.send. Tools, scopes, version, effect and semantics match exactly.
//!   This fixture metadata lets real typed SMTP custody reach publication
//!   screening; neither the synthetic capability nor mapping ships.
//!
//! `prepare_app_artifact`/`preview`/`execute` are counted and captured;
//! `preview_leak` is the TEST-ONLY leaky-preview variant. No custody/
//! crypto/fake-provider import.

use crate::contract_fixture::ToolTable;
use crate::platform::connections::{
    BoundActionMapping, CapabilityDescriptor, CapabilitySemantics, ProviderDescriptor,
    TEXT_PUBLICATION_INPUT_V1, TEXT_PUBLICATION_RECEIPT_V1,
};
use crate::platform::smtp::{
    CAPABILITY_EMAIL_SEND, CAPABILITY_VERSION, ENROLLMENT_SHAPE, MANIFEST_PIN, PLATFORM,
    REGISTRATION, SCOPE_EMAIL_SEND, TOOL_EMAIL_SEND,
};
use serde_json::{json, Value};
use std::sync::Mutex;

/// TEST-ONLY smtp tool table — the real `email.send` declaration so the
/// descriptor/tool match holds; the `text.publish` mapping lives on the
/// descriptor, not the table.
const TABLE_JSON: &str = r#"{
    "platform": "smtp",
    "manifest_version": "smtp-connections/1",
    "tools": [
        {"tool": "email.send", "effect": "send", "scopes": ["email:send"],
         "label": "TEST-ONLY counted smtp adapter"}
    ]
}"#;

/// TEST-ONLY counted smtp adapter. `last` records the actual input +
/// preview the daemon produced; `prepare`/`preview`/`execute` counts
/// prove the real RPC reached them; `preview_leak` makes `preview`
/// return the enrolled secret for the preview-only control.
pub struct SmtpArtifactAdapter {
    table: ToolTable,
    last: Mutex<(Option<Value>, Option<String>)>,
    prepare_count: std::sync::atomic::AtomicU64,
    preview_count: std::sync::atomic::AtomicU64,
    execute_count: std::sync::atomic::AtomicU64,
    preview_leak: Option<Vec<u8>>,
}

impl SmtpArtifactAdapter {
    pub fn new() -> Self {
        let declaration = match serde_json::from_str(TABLE_JSON) {
            Ok(declaration) => declaration,
            Err(_) => panic!("test SMTP tool table did not parse"),
        };
        let table = match ToolTable::from_json(&declaration) {
            Ok(table) => table,
            Err(_) => panic!("test SMTP tool table is invalid"),
        };
        let adapter = Self {
            table,
            last: Mutex::new((None, None)),
            prepare_count: std::sync::atomic::AtomicU64::new(0),
            preview_count: std::sync::atomic::AtomicU64::new(0),
            execute_count: std::sync::atomic::AtomicU64::new(0),
            preview_leak: None,
        };
        let descriptor =
            match <Self as crate::platform::PlatformAdapter>::connection_descriptor(&adapter) {
                Some(descriptor) => descriptor,
                None => panic!("test SMTP descriptor missing"),
            };
        assert!(
            descriptor.validate(&adapter.table).is_ok(),
            "test SMTP descriptor must match its tool table"
        );
        assert!(
            descriptor
                .resolve_action("text.publish", 1, "publish", "connection_account")
                .is_ok(),
            "test publication action must resolve before preparation"
        );
        adapter
    }
    /// TEST-ONLY variant — `preview` returns `bytes` (the enrolled
    /// secret) instead of the rendered caption. Only the preview-only
    /// control uses it.
    pub fn with_preview_leak(mut self, bytes: Vec<u8>) -> Self {
        self.preview_leak = Some(bytes);
        self
    }
}

impl Default for SmtpArtifactAdapter {
    fn default() -> Self {
        Self::new()
    }
}

// Counters + the captured input/preview the tests read.
impl SmtpArtifactAdapter {
    pub fn prepare_count(&self) -> u64 {
        self.prepare_count.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn preview_count(&self) -> u64 {
        self.preview_count.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn execute_count(&self) -> u64 {
        self.execute_count.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// The actual `prepare_app_artifact` input + `preview` output the
    /// daemon produced — captured, never fabricated.
    pub fn last_prepared(&self) -> (Option<Value>, Option<String>) {
        self.last
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl crate::platform::PlatformAdapter for SmtpArtifactAdapter {
    fn table(&self) -> &ToolTable {
        &self.table
    }

    fn reported_manifest_version(&self) -> Option<String> {
        self.table.manifest_version.clone()
    }

    /// TEST-ONLY descriptor: retain email.send and its real pin/shape,
    /// plus a synthetic text.publish capability AND matching action mapping.
    /// Both use email.send/email:send/send/EmailSend; only the fixture opts
    /// into publication. This is not the shipping SMTP descriptor.
    fn connection_descriptor(&self) -> Option<ProviderDescriptor> {
        Some(ProviderDescriptor {
            schema: 1,
            provider: PLATFORM.to_string(),
            revision: MANIFEST_PIN.to_string(),
            enrollment_shapes: vec![ENROLLMENT_SHAPE.to_string()],
            builtin_accounts: vec![],
            capabilities: vec![
                // The real reviewed `email.send` capability — retained.
                CapabilityDescriptor {
                    id: CAPABILITY_EMAIL_SEND.to_string(),
                    version: CAPABILITY_VERSION,
                    tools: vec![TOOL_EMAIL_SEND.to_string()],
                    scopes: vec![SCOPE_EMAIL_SEND.to_string()],
                    effect: "send".to_string(),
                    semantics: CapabilitySemantics::EmailSend,
                },
                // TEST-ONLY `text.publish` capability — every field matches
                // the `BoundActionMapping` exactly (`validate` requires the
                // mapping's capability/version/tools/scopes/effect/semantics
                // to equal a declared capability): id `text.publish`,
                // version 1, tools `[email.send]`, scopes `[email:send]`,
                // effect `send`, semantics `EmailSend`.
                CapabilityDescriptor {
                    id: "text.publish".to_string(),
                    version: 1,
                    tools: vec![TOOL_EMAIL_SEND.to_string()],
                    scopes: vec![SCOPE_EMAIL_SEND.to_string()],
                    effect: "send".to_string(),
                    semantics: CapabilitySemantics::EmailSend,
                },
            ],
            action_mappings: vec![BoundActionMapping {
                capability: "text.publish".to_string(),
                version: 1,
                action: "publish".to_string(),
                resource_kind: "connection_account".to_string(),
                tool: TOOL_EMAIL_SEND.to_string(),
                scopes: vec![SCOPE_EMAIL_SEND.to_string()],
                effect: "send".to_string(),
                semantics: CapabilitySemantics::EmailSend,
                input_contract: TEXT_PUBLICATION_INPUT_V1.to_string(),
                output_contract: TEXT_PUBLICATION_RECEIPT_V1.to_string(),
            }],
        })
    }

    fn connection_registration(&self) -> Option<String> {
        Some(REGISTRATION.to_string())
    }

    /// `prepare_app_text` produces the typed `{"schema":1` input the
    /// legacy smtp stage prepares — the canonical frame the current
    /// whole-envelope screen over-matches. The real stage runs this,
    /// then `preview`, then the two `refuse_leak` screens.
    fn prepare_app_text(
        &self,
        title: &str,
        body: &str,
        provenance: &Value,
    ) -> std::result::Result<Value, String> {
        self.prepare_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let input = json!({"schema": 1, "title": title, "tool": TOOL_EMAIL_SEND,
            "body": body, "provenance": provenance});
        self.last
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .0 = Some(input.clone());
        Ok(input)
    }

    fn preview(&self, account: &str, tool: &str, input: &Value) -> String {
        self.preview_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let rendered = match &self.preview_leak {
            Some(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            None => format!("{tool} through smtp/{account}: {input}"),
        };
        self.last
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .1 = Some(rendered.clone());
        rendered
    }

    fn execute(
        &self,
        _credential: &[u8],
        _tool: &str,
        _input: &Value,
        _idempotency_key: &str,
        _expected_hash: Option<&str>,
    ) -> std::result::Result<Value, String> {
        self.execute_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(json!({"ok": true}))
    }

    fn read_back(&self, _tool: &str, _input: &Value) -> crate::contract_fixture::Verified {
        crate::contract_fixture::Verified::Unknown
    }

    fn source_hash(&self, _agent: &str, _source: &str) -> Option<String> {
        None
    }
}
