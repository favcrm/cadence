//! CAD-506 / ADR 0006 §5.1, §5.2: the platform-adapter seam the effect
//! gate executes through. An adapter knows one platform's API: its
//! reviewed tool table (`{tool → {effect, scopes, label}}`), the
//! manifest version the platform currently reports, and the calls —
//! execute and read-back — the proxy drives.
//!
//! The gate owns classification (C1–C3), grants and the pending-effect
//! lifecycle; the adapter owns the platform's terms. A call reaches
//! `execute` only inside the agent's grant and, for a send, only after
//! the durable operator press — custody bytes cross the seam at call
//! time and never return.
//!
//! The shared-fixture vocabulary types ([`ToolTable`], [`Verified`])
//! live in [`crate::contract_fixture`]: they are the contract both
//! repos implement (CAD-505), not test doubles — the fake platform is
//! the same file's executable copy.
//!
//! Adapters register per platform name in
//! [`crate::daemon::ServeOptions::platforms`]; CAD-367 lands
//! Cloudflare's, and CAD-501 registers AgenticOS when the daemon is
//! hosted. A platform with no registered adapter refuses every call —
//! the gate fails closed.

use serde_json::Value;

use crate::contract_fixture::{ToolTable, Verified};

/// An app-artifact send distinguishes a known refusal from a write whose
/// completion cannot be confirmed. Uncertainty must never trigger a retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppArtifactError {
    Refused(String),
    Uncertain(String),
}

impl From<String> for AppArtifactError {
    fn from(message: String) -> Self {
        Self::Refused(message)
    }
}

impl From<&str> for AppArtifactError {
    fn from(message: &str) -> Self {
        Self::Refused(message.into())
    }
}

impl std::fmt::Display for AppArtifactError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(message) | Self::Uncertain(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for AppArtifactError {}

/// One platform's adapter — the proxy's outward leg.
pub trait PlatformAdapter: Send + Sync {
    /// The adapter's declared tool table, reviewed like code and pinned
    /// to the manifest version it was reviewed against (§5.2).
    fn table(&self) -> &ToolTable;

    /// Reviewed discovery metadata. Legacy adapters default unavailable.
    fn connection_descriptor(&self) -> Option<super::connections::ProviderDescriptor> {
        None
    }

    /// Composition receipt, not a live account/deployment health proof.
    fn connection_registration(&self) -> Option<String> {
        None
    }

    /// Reviewed internal tools require persisted app-artifact authority,
    /// regardless of a legacy account grant held by a worker.
    fn app_artifact_tool(&self, tool: &str) -> bool {
        self.connection_descriptor().is_some_and(|descriptor| {
            descriptor
                .action_mappings
                .iter()
                .any(|mapping| mapping.tool == tool)
        })
    }

    /// Provider-owned execution of a frozen app read/draft action. The broker
    /// proves the active assigned turn, exact binding and frozen price before
    /// calling this. Implementations must validate their own resource fields
    /// against `authority`. A repeated idempotency key must return the same
    /// recorded provider outcome without another charged operation; reuse
    /// with changed input or approved price must refuse. Paid providers must
    /// enforce `authority.quote.total_price_micros` atomically at the charged
    /// call, not merely compare an earlier discovery response — unless the
    /// operator policy is pass-through pricing, in which case the provider
    /// bills at the upstream rate and records the actual charge in the
    /// receipt. Either way the daemon's execution-time re-quote still refuses
    /// a rate that changed since approval. The default refuses every provider.
    fn execute_app_capability(
        &self,
        _credential: &[u8],
        _authority: &Value,
        _input: &Value,
        _idempotency_key: &str,
    ) -> std::result::Result<super::AppCapabilityOutput, String> {
        Err("provider does not support run-bound app capabilities".into())
    }

    /// Quote the exact reviewed mapping and credential account in a frozen
    /// binding. Providers without live price discovery remain unavailable.
    fn quote_app_capability(
        &self,
        _credential: &[u8],
        _binding: &Value,
    ) -> std::result::Result<super::AppCapabilityQuote, String> {
        Err("provider does not support app capability price discovery".into())
    }

    /// Translate trusted persisted text into the provider's exact input.
    /// Unsupported providers refuse; this does not stage or publish anything.
    fn prepare_app_text(
        &self,
        _title: &str,
        _body: &str,
        _provenance: &Value,
    ) -> std::result::Result<Value, String> {
        Err("provider does not support app text publication".into())
    }

    /// Optional server-read binary receipt accompanies the accepted text.
    /// Providers must opt in; the caller supplies no path or raw bytes.
    fn prepare_app_artifact(
        &self,
        title: &str,
        body: &str,
        provenance: &Value,
        asset: Option<&Value>,
    ) -> std::result::Result<Value, String> {
        if asset.is_some() {
            return Err("provider does not support reviewed binary assets".into());
        }
        self.prepare_app_text(title, body, provenance)
    }

    /// The manifest version the *platform* reports now — the platform
    /// side of the pin. `None` reports nothing, so every call gates as
    /// `send` (a call argument can never assert the match).
    fn reported_manifest_version(&self) -> Option<String>;

    /// The rendered, bounded preview the press reviews — what the
    /// platform will do, in the platform's terms, on `account`
    /// (§5.4 `preview`).
    fn preview(&self, account: &str, tool: &str, input: &Value) -> String;

    /// Perform the call against the platform. `credential` is the
    /// enrolled custody bytes — read inside the gate, attached here,
    /// never returned. `idempotency_key` derives from the `effect_id`
    /// of the staged send (C9); a repeated key must return the recorded
    /// outcome without executing again. `expected_hash` is the approved
    /// input's content hash where the platform supports one.
    fn execute(
        &self,
        credential: &[u8],
        tool: &str,
        input: &Value,
        idempotency_key: &str,
        expected_hash: Option<&str>,
    ) -> std::result::Result<Value, String>;

    /// Execute only with the persisted app-artifact authority checked by the
    /// broker and the adapter. Legacy execution support is not permission to
    /// publish an app artifact; a provider must explicitly implement this hook.
    fn execute_app_artifact(
        &self,
        _credential: &[u8],
        _tool: &str,
        _input: &Value,
        _idempotency_key: &str,
        _expected_hash: Option<&str>,
    ) -> std::result::Result<Value, AppArtifactError> {
        Err(AppArtifactError::Refused(
            "provider does not support app artifact execution".into(),
        ))
    }

    /// §5.4 step 6 read-back: does platform state match the approved
    /// input? `Verified::Unknown` where the platform offers none.
    fn read_back(&self, tool: &str, input: &Value) -> Verified;

    /// The content hash (`sha256:<hex>`) of a reviewed source artifact
    /// the platform holds — `None` when it holds no such artifact. A
    /// send pins it as `source_hash`; Execute re-verifies it before
    /// firing (§5.4 step 3). `agent` is the row's proven requester: an
    /// adapter whose artifacts name caller-owned scope binds the name
    /// to it — `local`'s attachment descriptor may pin only that
    /// agent's own worktree (CAD-553).
    fn source_hash(&self, agent: &str, source: &str) -> Option<String>;

    /// The reviewed artifact a send will release, named by the
    /// adapter from the proven caller and its input when `input.source`
    /// does not name one (CAD-553: `local` names the declared
    /// attachment set it will read under the requester's worktree).
    /// `None` implies nothing — the send stages unpinned. An implied
    /// name rides the same `source_hash` pin and `source_changed`
    /// close as a caller-declared one; the gate knows no adapter's
    /// name format.
    fn implied_source(&self, _agent: &str, _tool: &str, _input: &Value) -> Option<String> {
        None
    }
}
