//! CAD-1128 TEST-ONLY execution-capable SMTP publication fixture — PRIVATE
//! to `cad1128_boundaries_tests` (module `cad1128_exec_smtp_fixture`), and
//! NOT the merged `crate::platform::smtp::test_artifact_adapter` helper.
//!
//! WHY THIS FILE EXISTS (separately authorized independent-author fixture):
//! the merged CAD-1156 helper is deliberately stage-only — it declares the
//! synthetic `text.publish` capability/mapping so the real typing/custody
//! screening is reached, but it does NOT implement `execute_app_artifact`,
//! so the inherited default refuses. The CAD-1128 legacy-SMTP acceptance
//! oracle must observe a REAL `execute_app_artifact` callback and a
//! `{"kind":"released",...}` outcome through the real `app_effect_decide`
//! consumer. Weakening that oracle to stage-only (or counting a refusal as
//! successful execution) would be dishonest, so this distinct private
//! fixture preserves the historical fixture's counted callback and
//! deterministic receipt behavior instead. The merged helper is not
//! edited, wrapped, or re-exported.
//!
//! TEST-ONLY / SYNTHETIC LIMITS (stated, never implied to ship):
//! - `text.publish`/`1`/`publish`/`connection_account` and its capability
//!   entry are SYNTHETIC fixture metadata. The real SMTP descriptor
//!   declares only `email.send`; the real `smtp-connections/1`
//!   `MANIFEST_PIN`/`REGISTRATION` and the real `email.send` tool/scope are
//!   reused so `validate`, registration receipt and manifest pinning
//!   behave exactly like production's reviewed values.
//! - `execute_app_artifact` returns a deterministic in-memory receipt
//!   (`Mutex<Option<Value>>`) — NO real SMTP delivery, credential use,
//!   network call, crypto, or provider admission. `read_back` is a
//!   title-only sanity check against that receipt, never full artifact
//!   verification.
//! - `counters()` observes `prepare`/`preview`/`execute` callback
//!   invocations; a guarded-out encrypted path must leave all three at
//!   ZERO, so "the guard fired before any callback" is observed, not
//!   assumed.
//!
//! Source/test authorship by a separately assigned independent author — not
//! registered head-pinned code review or operator approval. Constructed
//! against the current `ToolTable::from_json` shape; the historical
//! `.tmp/approved` copies stay immutable.

#![allow(dead_code)] // Private fixture: only the app-artifact hooks are used.

use crate::contract_fixture::{ToolTable, Verified};
use crate::platform::adapter::{AppArtifactError, PlatformAdapter};
use crate::platform::connections::{
    BoundActionMapping, CapabilityDescriptor, CapabilitySemantics, ProviderDescriptor,
    TEXT_PUBLICATION_INPUT_V1, TEXT_PUBLICATION_RECEIPT_V1,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// The REAL `smtp` provider name; `text.publish` is TEST-ONLY metadata.
const PROVIDER: &str = crate::platform::smtp::PLATFORM;
/// The REAL smtp pin — `smtp-connections/1`.
const MANIFEST_PIN: &str = crate::platform::smtp::MANIFEST_PIN;
/// The REAL smtp registration — `registration_digest` resolves exactly
/// like production's.
const REGISTRATION: &str = crate::platform::smtp::REGISTRATION;

/// TEST-ONLY tool table — the reviewed `email.send` declaration only (the
/// synthetic `text.publish` mapping lives on the descriptor, and its
/// `tool` names this declared entry, which `validate` requires).
const TABLE_JSON: &str = r#"{
    "platform": "smtp",
    "manifest_version": "smtp-connections/1",
    "tools": [
        {"tool": "email.send", "effect": "send", "scopes": ["email:send"],
         "label": "CAD-1128 TEST-ONLY execution fixture"}
    ]
}"#;

/// Counters the fixture exposes for the pre/post-callback oracle.
#[derive(Default)]
pub struct Counters {
    /// `prepare_app_text`/`prepare_app_artifact` invocations.
    pub prepare: AtomicUsize,
    /// `preview` invocations.
    pub preview: AtomicUsize,
    /// `execute_app_artifact` invocations.
    pub execute: AtomicUsize,
}

impl Counters {
    /// `(prepare, preview, execute)` — `(0,0,0)` on a guarded-out refusal.
    pub fn snapshot(&self) -> (usize, usize, usize) {
        (
            self.prepare.load(Ordering::SeqCst),
            self.preview.load(Ordering::SeqCst),
            self.execute.load(Ordering::SeqCst),
        )
    }
}

/// The TEST-ONLY execution-capable publication adapter. Declares
/// `text.publish` so an encrypted smtp binding's `resolve_action` passes
/// and the custody refusal is what fires, while the non-encrypted twin
/// reaches genuine `prepare`/`preview`/`execute_app_artifact` callbacks.
/// Injected through `Rig::start_with_platforms`'s
/// `extra_platforms["smtp"]`.
pub struct Cad1128ExecSmtpArtifactAdapter {
    table: ToolTable,
    counters: Counters,
    out: Mutex<Option<Value>>,
}

impl Cad1128ExecSmtpArtifactAdapter {
    pub fn new() -> Self {
        let declaration: Value =
            serde_json::from_str(TABLE_JSON).expect("CAD-1128 execution fixture tool table parses");
        let table = ToolTable::from_json(&declaration)
            .expect("CAD-1128 execution fixture tool table is valid");
        let adapter = Self {
            table,
            counters: Counters::default(),
            out: Mutex::new(None),
        };
        let descriptor = adapter
            .connection_descriptor()
            .expect("CAD-1128 execution fixture descriptor missing");
        assert!(
            descriptor.validate(&adapter.table).is_ok(),
            "CAD-1128 execution fixture descriptor must match its tool table"
        );
        assert!(
            descriptor
                .resolve_action("text.publish", 1, "publish", "connection_account")
                .is_ok(),
            "CAD-1128 execution fixture publication action must resolve"
        );
        adapter
    }

    /// The pre/post-callback oracle — `(prepare, preview, execute)` counts.
    pub fn counters(&self) -> &Counters {
        &self.counters
    }
}

impl Default for Cad1128ExecSmtpArtifactAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformAdapter for Cad1128ExecSmtpArtifactAdapter {
    fn table(&self) -> &ToolTable {
        &self.table
    }

    /// REAL `smtp` envelope + TEST-ONLY `text.publish` `action_mapping`.
    /// `capabilities` carries the synthetic `text.publish` entry plus the
    /// real `email.send` capability; the mapping uses the real
    /// `TEXT_PUBLICATION_*` contract pair and `tool` = the declared
    /// `email.send` table entry, `scopes` = `email:send` (the enrolled smtp
    /// connection's scope), `effect`/`semantics` matching the capability
    /// entry — `validate`'s consistency holds.
    fn connection_descriptor(&self) -> Option<ProviderDescriptor> {
        Some(ProviderDescriptor {
            schema: 1,
            provider: PROVIDER.into(),
            revision: MANIFEST_PIN.into(),
            enrollment_shapes: vec![crate::platform::smtp::ENROLLMENT_SHAPE.into()],
            builtin_accounts: vec![],
            capabilities: vec![
                // TEST-ONLY `text.publish` — synthetic, not a real smtp
                // capability; exists so `resolve_action` can bind the slot.
                CapabilityDescriptor {
                    id: "text.publish".into(),
                    version: 1,
                    tools: vec![crate::platform::smtp::TOOL_EMAIL_SEND.into()],
                    scopes: vec![crate::platform::smtp::SCOPE_EMAIL_SEND.into()],
                    effect: "send".into(),
                    semantics: CapabilitySemantics::EmailSend,
                },
                // The REAL smtp `email.send` capability — retained.
                CapabilityDescriptor {
                    id: crate::platform::smtp::CAPABILITY_EMAIL_SEND.into(),
                    version: crate::platform::smtp::CAPABILITY_VERSION,
                    tools: vec![crate::platform::smtp::TOOL_EMAIL_SEND.into()],
                    scopes: vec![crate::platform::smtp::SCOPE_EMAIL_SEND.into()],
                    effect: "send".into(),
                    semantics: CapabilitySemantics::EmailSend,
                },
            ],
            // THE `text.publish` `connection_account` publication mapping —
            // `resolve_action("text.publish",1,"publish","connection_account")`
            // matches, so an encrypted binding reaches the custody refusal
            // and a legacy FILE binding reaches real preparation.
            action_mappings: vec![BoundActionMapping {
                capability: "text.publish".into(),
                version: 1,
                action: "publish".into(),
                resource_kind: "connection_account".into(),
                tool: crate::platform::smtp::TOOL_EMAIL_SEND.into(),
                scopes: vec![crate::platform::smtp::SCOPE_EMAIL_SEND.into()],
                effect: "send".into(),
                semantics: CapabilitySemantics::EmailSend,
                input_contract: TEXT_PUBLICATION_INPUT_V1.into(),
                output_contract: TEXT_PUBLICATION_RECEIPT_V1.into(),
            }],
        })
    }

    /// The REAL `smtp-connections/1` registration.
    fn connection_registration(&self) -> Option<String> {
        Some(REGISTRATION.into())
    }

    /// `email.send` is the declared app-artifact tool.
    fn app_artifact_tool(&self, tool: &str) -> bool {
        tool == crate::platform::smtp::TOOL_EMAIL_SEND
    }

    fn prepare_app_text(
        &self,
        title: &str,
        body: &str,
        provenance: &Value,
    ) -> std::result::Result<Value, String> {
        self.counters.prepare.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"schema":1,"title":title,
                "tool":crate::platform::smtp::TOOL_EMAIL_SEND,
                "body":body,"provenance":provenance}))
    }

    fn prepare_app_artifact(
        &self,
        title: &str,
        body: &str,
        provenance: &Value,
        asset: Option<&Value>,
    ) -> std::result::Result<Value, String> {
        self.counters.prepare.fetch_add(1, Ordering::SeqCst);
        if asset.is_some() {
            return Err("CAD-1128 execution fixture does not support binary assets".into());
        }
        Ok(json!({"schema":1,"title":title,
                "tool":crate::platform::smtp::TOOL_EMAIL_SEND,
                "body":body,"provenance":provenance}))
    }

    fn preview(&self, account: &str, tool: &str, input: &Value) -> String {
        self.counters.preview.fetch_add(1, Ordering::SeqCst);
        format!("{tool} through smtp/{account}: {}", input["title"])
    }

    /// The genuinely exercised execution hook: a deterministic receipt in
    /// `Mutex<Value>` (never a real local-file publication or SMTP send)
    /// recorded only after the tool is validated. `execute` counts the
    /// callback, so a refusal that never reaches it stays at ZERO.
    fn execute_app_artifact(
        &self,
        _credential: &[u8],
        tool: &str,
        input: &Value,
        idempotency_key: &str,
        _expected_hash: Option<&str>,
    ) -> std::result::Result<Value, AppArtifactError> {
        self.counters.execute.fetch_add(1, Ordering::SeqCst);
        if tool != crate::platform::smtp::TOOL_EMAIL_SEND {
            return Err(AppArtifactError::Refused(format!(
                "CAD-1128 execution fixture does not execute {tool}"
            )));
        }
        let receipt = json!({"published":true,"tool":tool,
            "idempotency_key":idempotency_key,
            "title":input["title"],"body":input["body"]});
        *self
            .out
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(receipt.clone());
        Ok(receipt)
    }

    /// Read-back is a TITLE-only sanity check against the simulated
    /// receipt — never a full approved-artifact verification.
    fn read_back(&self, tool: &str, input: &Value) -> Verified {
        if tool != crate::platform::smtp::TOOL_EMAIL_SEND {
            return Verified::Unknown;
        }
        match &*self
            .out
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            Some(receipt) if receipt["title"] == input["title"] => Verified::True,
            Some(_) => Verified::False,
            None => Verified::Unknown,
        }
    }

    /// No artifact pinning — the fixture holds no source artifact.
    fn source_hash(&self, _agent: &str, _source: &str) -> Option<String> {
        None
    }

    /// The generic legacy `execute` is unused for app-artifact paths; the
    /// app-artifact authority must go through `execute_app_artifact`.
    fn execute(
        &self,
        _credential: &[u8],
        tool: &str,
        _input: &Value,
        _idempotency_key: &str,
        _expected_hash: Option<&str>,
    ) -> std::result::Result<Value, String> {
        Err(format!(
            "CAD-1128 execution fixture executes {tool} only via app_artifact"
        ))
    }

    fn reported_manifest_version(&self) -> Option<String> {
        self.table.manifest_version.clone()
    }
}
