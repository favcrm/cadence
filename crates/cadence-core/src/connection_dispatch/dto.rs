//! Strict v1 wire DTOs for protected Connections: dispatch, management,
//! manifest/snapshot/receipt. Every struct rejects unknown fields; every
//! selector is a bounded identifier, canonical UUID, digest or exact integer
//! string. No DTO accepts secrets, URLs the caller chooses, actor/role
//! assertions, runtime proofs or `authorized` booleans.
use crate::connection_dispatch::canonical::{parse_strict, ParseError, CONNECTION_DISPATCH_DOMAIN};
use crate::connection_dispatch::ledger::{CheckpointAckV1, LedgerHeadV1, LedgerRecordV1};
use crate::connection_dispatch::primitives::{
    AgentUid, Counter, Digest, EpochMs, HttpsUrl, Ident, MaxOutputTokens, ModelId, PicoUsd,
    Revision, UsdMicros, UuidV1,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

pub const CONNECTION_CONTRACT_VERSION: u32 = 1;
pub const MAX_OPERATION_BYTES: usize = 1024 * 1024;
pub const MAX_MANAGEMENT_BYTES: usize = 256 * 1024;
pub const MAX_CREDENTIAL_SUBMIT_BYTES: usize = 96 * 1024;
pub const MAX_RAW_CREDENTIAL_BYTES: usize = 65_536;
pub const MAX_SNAPSHOT_BYTES: usize = 256 * 1024;
pub const MAX_REFUSAL_BYTES: usize = 2 * 1024;
pub const MAX_MODEL_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_RECEIPT_METADATA_BYTES: usize = 8 * 1024;
pub const MAX_RECEIPT_BYTES: usize = MAX_MODEL_TEXT_BYTES + MAX_RECEIPT_METADATA_BYTES;
pub const MAX_MESSAGES: usize = 100;
pub const MAX_MESSAGE_BYTES: usize = 65_536;
pub const SNAPSHOT_LIFETIME_MS: u64 = 60_000;
pub const QUOTE_LIFETIME_MS: u64 = 60_000;
pub const RATE_WINDOW_MS: u64 = 60_000;
pub const DAILY_WINDOW_MS: u64 = 86_400_000;

/// The closed refusal/admission error set. The wire string and the human
/// message are fixed per variant; adapters never compose them.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCodeV1 {
    InvalidRequest,
    UnsupportedVersion,
    DigestMismatch,
    PrincipalRequired,
    SessionExpired,
    ManagementForbidden,
    OperationNotGranted,
    RuntimeRevoked,
    DataPolicyRefused,
    DestinationRefused,
    NotFound,
    MethodNotAllowed,
    StaleRevision,
    IdempotencyConflict,
    LoadedStateMismatch,
    PlacementChanged,
    ApprovalRequired,
    PriceChanged,
    PayloadTooLarge,
    RateLimit,
    SpendLimit,
    RuntimeUnqualified,
    CapabilityUnavailable,
    CustodyLocked,
    PricingUnavailable,
    UnsupportedVerification,
    EntrySessionRequired,
    AlreadySubmitted,
    NonceExpired,
    RequestChanged,
    RequestNotReady,
    ProviderRefused,
    ReservationOverrun,
    ClockRegression,
    StateConflict,
    LedgerMismatch,
}

impl ErrorCodeV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCodeV1::InvalidRequest => "invalid_request",
            ErrorCodeV1::UnsupportedVersion => "unsupported_version",
            ErrorCodeV1::DigestMismatch => "digest_mismatch",
            ErrorCodeV1::PrincipalRequired => "principal_required",
            ErrorCodeV1::SessionExpired => "session_expired",
            ErrorCodeV1::ManagementForbidden => "management_forbidden",
            ErrorCodeV1::OperationNotGranted => "operation_not_granted",
            ErrorCodeV1::RuntimeRevoked => "runtime_revoked",
            ErrorCodeV1::DataPolicyRefused => "data_policy_refused",
            ErrorCodeV1::DestinationRefused => "destination_refused",
            ErrorCodeV1::NotFound => "not_found",
            ErrorCodeV1::MethodNotAllowed => "method_not_allowed",
            ErrorCodeV1::StaleRevision => "stale_revision",
            ErrorCodeV1::IdempotencyConflict => "idempotency_conflict",
            ErrorCodeV1::LoadedStateMismatch => "loaded_state_mismatch",
            ErrorCodeV1::PlacementChanged => "placement_changed",
            ErrorCodeV1::ApprovalRequired => "approval_required",
            ErrorCodeV1::PriceChanged => "price_changed",
            ErrorCodeV1::PayloadTooLarge => "payload_too_large",
            ErrorCodeV1::RateLimit => "rate_limit",
            ErrorCodeV1::SpendLimit => "spend_limit",
            ErrorCodeV1::RuntimeUnqualified => "runtime_unqualified",
            ErrorCodeV1::CapabilityUnavailable => "capability_unavailable",
            ErrorCodeV1::CustodyLocked => "custody_locked",
            ErrorCodeV1::PricingUnavailable => "pricing_unavailable",
            ErrorCodeV1::UnsupportedVerification => "unsupported_verification",
            ErrorCodeV1::EntrySessionRequired => "entry_session_required",
            ErrorCodeV1::AlreadySubmitted => "already_submitted",
            ErrorCodeV1::NonceExpired => "nonce_expired",
            ErrorCodeV1::RequestChanged => "request_changed",
            ErrorCodeV1::RequestNotReady => "request_not_ready",
            ErrorCodeV1::ProviderRefused => "provider_refused",
            ErrorCodeV1::ReservationOverrun => "reservation_overrun",
            ErrorCodeV1::ClockRegression => "clock_regression",
            ErrorCodeV1::StateConflict => "state_conflict",
            ErrorCodeV1::LedgerMismatch => "ledger_mismatch",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            ErrorCodeV1::InvalidRequest => "request is malformed",
            ErrorCodeV1::UnsupportedVersion => "contract version is unsupported",
            ErrorCodeV1::DigestMismatch => "material digest does not match the frozen dispatch",
            ErrorCodeV1::PrincipalRequired => "a positively identified caller is required",
            ErrorCodeV1::SessionExpired => "the session binding has expired",
            ErrorCodeV1::ManagementForbidden => "management authority is required",
            ErrorCodeV1::OperationNotGranted => "the connection does not grant this operation",
            ErrorCodeV1::RuntimeRevoked => "the runtime is fenced or revoked",
            ErrorCodeV1::DataPolicyRefused => "the data scope does not permit this payload",
            ErrorCodeV1::DestinationRefused => "the destination is not approved",
            ErrorCodeV1::NotFound => "the selector is unavailable",
            ErrorCodeV1::MethodNotAllowed => "the method is not allowed",
            ErrorCodeV1::StaleRevision => {
                "the credential, authorization or policy revision changed"
            }
            ErrorCodeV1::IdempotencyConflict => {
                "the idempotency key is bound to different material"
            }
            ErrorCodeV1::LoadedStateMismatch => "the loaded runtime state does not match",
            ErrorCodeV1::PlacementChanged => "the placement or custody epoch changed",
            ErrorCodeV1::ApprovalRequired => "a current human approval is required",
            ErrorCodeV1::PriceChanged => "the model quote is stale or bound to other material",
            ErrorCodeV1::PayloadTooLarge => "the payload exceeds its byte limit",
            ErrorCodeV1::RateLimit => "a rate or concurrency limit refused the operation",
            ErrorCodeV1::SpendLimit => "a spending limit refused the operation",
            ErrorCodeV1::RuntimeUnqualified => "no qualified runtime is current",
            ErrorCodeV1::CapabilityUnavailable => "the capability is not available",
            ErrorCodeV1::CustodyLocked => "custody is locked or unavailable",
            ErrorCodeV1::PricingUnavailable => "no bounded price is available",
            ErrorCodeV1::UnsupportedVerification => {
                "the provider has no reviewed verification adapter"
            }
            ErrorCodeV1::EntrySessionRequired => "an authenticated human entry session is required",
            ErrorCodeV1::AlreadySubmitted => "the entry attempt was already submitted",
            ErrorCodeV1::NonceExpired => "the one-use form nonce expired",
            ErrorCodeV1::RequestChanged => "the credential request changed",
            ErrorCodeV1::RequestNotReady => "the credential request is not ready",
            ErrorCodeV1::ProviderRefused => "the provider refused before execution",
            ErrorCodeV1::ReservationOverrun => "actual validated cost exceeded the reservation",
            ErrorCodeV1::ClockRegression => "the durable clock floor is ahead of the request time",
            ErrorCodeV1::StateConflict => "the dispatch state does not permit this transition",
            ErrorCodeV1::LedgerMismatch => "the protected ledger prefix does not match",
        }
    }

    pub fn status(self) -> u16 {
        match self {
            ErrorCodeV1::InvalidRequest
            | ErrorCodeV1::UnsupportedVersion
            | ErrorCodeV1::DigestMismatch
            | ErrorCodeV1::UnsupportedVerification => 400,
            ErrorCodeV1::PrincipalRequired
            | ErrorCodeV1::SessionExpired
            | ErrorCodeV1::EntrySessionRequired => 401,
            ErrorCodeV1::ManagementForbidden
            | ErrorCodeV1::OperationNotGranted
            | ErrorCodeV1::RuntimeRevoked
            | ErrorCodeV1::DataPolicyRefused
            | ErrorCodeV1::DestinationRefused => 403,
            ErrorCodeV1::NotFound => 404,
            ErrorCodeV1::MethodNotAllowed => 405,
            ErrorCodeV1::StaleRevision
            | ErrorCodeV1::IdempotencyConflict
            | ErrorCodeV1::LoadedStateMismatch
            | ErrorCodeV1::PlacementChanged
            | ErrorCodeV1::ApprovalRequired
            | ErrorCodeV1::PriceChanged
            | ErrorCodeV1::AlreadySubmitted
            | ErrorCodeV1::NonceExpired
            | ErrorCodeV1::RequestChanged
            | ErrorCodeV1::ClockRegression
            | ErrorCodeV1::StateConflict
            | ErrorCodeV1::LedgerMismatch => 409,
            ErrorCodeV1::PayloadTooLarge => 413,
            ErrorCodeV1::RateLimit | ErrorCodeV1::SpendLimit => 429,
            ErrorCodeV1::RuntimeUnqualified
            | ErrorCodeV1::CapabilityUnavailable
            | ErrorCodeV1::CustodyLocked
            | ErrorCodeV1::PricingUnavailable
            | ErrorCodeV1::RequestNotReady => 503,
            // Terminal receipt states answer 200 with a closed refusal.
            ErrorCodeV1::ProviderRefused | ErrorCodeV1::ReservationOverrun => 200,
        }
    }

    /// Codes a settlement may report as a definitive pre-execution refusal.
    pub fn is_settlement_refusal(self) -> bool {
        matches!(
            self,
            ErrorCodeV1::ProviderRefused
                | ErrorCodeV1::OperationNotGranted
                | ErrorCodeV1::DataPolicyRefused
                | ErrorCodeV1::DestinationRefused
                | ErrorCodeV1::PricingUnavailable
                | ErrorCodeV1::CustodyLocked
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchError {
    pub code: ErrorCodeV1,
    pub message: &'static str,
}

impl DispatchError {
    pub fn new(code: ErrorCodeV1) -> Self {
        Self {
            code,
            message: code.message(),
        }
    }
    pub fn code_str(&self) -> &'static str {
        self.code.as_str()
    }
}

impl fmt::Display for DispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}
impl std::error::Error for DispatchError {}

fn validation_error(code: ErrorCodeV1) -> DispatchError {
    DispatchError::new(code)
}

/// Exact length of a canonical-decimal vector, refusing oversized snapshots.
fn bounded<T>(items: &[T], max: usize, code: ErrorCodeV1) -> Result<(), DispatchError> {
    if items.len() > max {
        return Err(validation_error(code));
    }
    Ok(())
}

fn require_v(v: u32) -> Result<(), DispatchError> {
    if v != CONNECTION_CONTRACT_VERSION {
        return Err(validation_error(ErrorCodeV1::UnsupportedVersion));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Operation union (agent path: three tags; smtp.verify@1 is not representable)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TextGenerateKindV1 {
    #[serde(rename = "text.generate@1")]
    TextGenerate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SmtpSendKindV1 {
    #[serde(rename = "smtp.send@1")]
    SmtpSend,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SmtpTestSendKindV1 {
    #[serde(rename = "smtp.test_send@1")]
    SmtpTestSend,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SmtpVerifyKindV1 {
    #[serde(rename = "smtp.verify@1")]
    SmtpVerify,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TextGeneratedKindV1 {
    #[serde(rename = "text.generated@1")]
    TextGenerated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SmtpAcceptedKindV1 {
    #[serde(rename = "smtp.accepted@1")]
    SmtpAccepted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SmtpVerifiedKindV1 {
    #[serde(rename = "smtp.verified@1")]
    SmtpVerified,
}

/// The typed operations a protected Connection dispatch may name.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OperationKindV1 {
    #[serde(rename = "text.generate@1")]
    TextGenerate,
    #[serde(rename = "smtp.send@1")]
    SmtpSend,
    #[serde(rename = "smtp.test_send@1")]
    SmtpTestSend,
    #[serde(rename = "smtp.verify@1")]
    SmtpVerify,
}

impl OperationKindV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            OperationKindV1::TextGenerate => "text.generate@1",
            OperationKindV1::SmtpSend => "smtp.send@1",
            OperationKindV1::SmtpTestSend => "smtp.test_send@1",
            OperationKindV1::SmtpVerify => "smtp.verify@1",
        }
    }
    /// Operations a human may invoke; never obtainable from an agent grant.
    pub fn is_human_only(self) -> bool {
        matches!(
            self,
            OperationKindV1::SmtpTestSend | OperationKindV1::SmtpVerify
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MessageV1 {
    pub role: MessageRole,
    pub content: String,
}

/// `text.generate@1`: explicit model, bounded messages, explicit caller cap.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextGenerateV1 {
    pub kind: TextGenerateKindV1,
    pub quote_id: UuidV1,
    pub model: ModelId,
    pub messages: Vec<MessageV1>,
    pub max_output_tokens: MaxOutputTokens,
    pub max_cost_micros: UsdMicros,
    pub data_scope_id: Ident,
}

/// `smtp.send@1`: the exact server-resolved human-approved send only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SmtpSendV1 {
    pub kind: SmtpSendKindV1,
    pub approved_send_id: Ident,
}

/// `smtp.test_send@1`: human-only test whose server record binds the content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeSmtpTestV1 {
    pub kind: SmtpTestSendKindV1,
    pub test_request_id: UuidV1,
}

/// Internal human verification operation. Never part of the agent union.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HumanSmtpVerifyV1 {
    pub kind: SmtpVerifyKindV1,
    pub verification_request_id: UuidV1,
}

/// The agent-invokable union: three tags, `smtp.verify@1` impossible.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum AgentOperationV1 {
    TextGenerate(TextGenerateV1),
    SmtpSend(SmtpSendV1),
    SmtpTestSend(NativeSmtpTestV1),
}

impl AgentOperationV1 {
    pub fn kind(&self) -> OperationKindV1 {
        match self {
            AgentOperationV1::TextGenerate(_) => OperationKindV1::TextGenerate,
            AgentOperationV1::SmtpSend(_) => OperationKindV1::SmtpSend,
            AgentOperationV1::SmtpTestSend(_) => OperationKindV1::SmtpTestSend,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionInvokeV1 {
    pub v: u32,
    pub connection_id: Ident,
    pub expected_credential_revision: Revision,
    pub expected_authorization_revision: Revision,
    pub idempotency_key: Ident,
    pub snapshot_id: UuidV1,
    pub loaded_receipt_id: UuidV1,
    pub capability_id: Ident,
    pub operation: AgentOperationV1,
}

/// Human verification admission, constructed only after the trusted custody
/// owner claimed the exact human attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HumanSmtpVerifyAdmissionV1 {
    pub v: u32,
    pub connection_id: Ident,
    pub expected_credential_revision: Revision,
    pub expected_authorization_revision: Revision,
    pub verification_request_id: UuidV1,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmissionRequestV1 {
    Agent(ConnectionInvokeV1),
    HumanSmtpVerify(HumanSmtpVerifyAdmissionV1),
}

impl AdmissionRequestV1 {
    pub fn kind(&self) -> OperationKindV1 {
        match self {
            AdmissionRequestV1::Agent(invoke) => invoke.operation.kind(),
            AdmissionRequestV1::HumanSmtpVerify(_) => OperationKindV1::SmtpVerify,
        }
    }
    pub fn connection_id(&self) -> &Ident {
        match self {
            AdmissionRequestV1::Agent(invoke) => &invoke.connection_id,
            AdmissionRequestV1::HumanSmtpVerify(request) => &request.connection_id,
        }
    }
    pub fn expected_credential_revision(&self) -> Revision {
        match self {
            AdmissionRequestV1::Agent(invoke) => invoke.expected_credential_revision,
            AdmissionRequestV1::HumanSmtpVerify(request) => request.expected_credential_revision,
        }
    }
    pub fn expected_authorization_revision(&self) -> Revision {
        match self {
            AdmissionRequestV1::Agent(invoke) => invoke.expected_authorization_revision,
            AdmissionRequestV1::HumanSmtpVerify(request) => request.expected_authorization_revision,
        }
    }
    pub fn idempotency_key(&self) -> &str {
        match self {
            AdmissionRequestV1::Agent(invoke) => invoke.idempotency_key.as_str(),
            AdmissionRequestV1::HumanSmtpVerify(request) => {
                request.verification_request_id.as_str()
            }
        }
    }
    pub fn validate(&self) -> Result<(), DispatchError> {
        match self {
            AdmissionRequestV1::Agent(invoke) => validate_connection_invoke_v1(invoke),
            AdmissionRequestV1::HumanSmtpVerify(request) => {
                require_v(request.v)?;
                Ok(())
            }
        }
    }
}

pub fn validate_connection_invoke_v1(invoke: &ConnectionInvokeV1) -> Result<(), DispatchError> {
    require_v(invoke.v)?;
    match &invoke.operation {
        AgentOperationV1::TextGenerate(operation) => {
            if operation.messages.is_empty() || operation.messages.len() > MAX_MESSAGES {
                return Err(validation_error(ErrorCodeV1::InvalidRequest));
            }
            let mut bytes = 0usize;
            for message in &operation.messages {
                bytes = bytes
                    .checked_add(message.content.len())
                    .ok_or_else(|| validation_error(ErrorCodeV1::PayloadTooLarge))?;
            }
            if bytes > MAX_MESSAGE_BYTES {
                return Err(validation_error(ErrorCodeV1::PayloadTooLarge));
            }
        }
        AgentOperationV1::SmtpSend(_) | AgentOperationV1::SmtpTestSend(_) => {}
    }
    Ok(())
}

/// The canonical material of an agent operation (used for input/payload
/// digests and the frozen commitment).
pub fn operation_material(invoke: &ConnectionInvokeV1) -> serde_json::Value {
    serde_json::to_value(&invoke.operation).expect("an operation serializes")
}

pub fn messages_digest(messages: &[MessageV1]) -> Digest {
    let material = serde_json::json!({"kind": "text.input@1", "messages": messages});
    crate::connection_dispatch::canonical::material_digest_value(
        CONNECTION_DISPATCH_DOMAIN,
        &material,
    )
}

/// The exact destination material digest used by send approvals and the
/// dispatch commitment.
pub fn destination_digest_for(destination: &ResolvedDestinationV1) -> Digest {
    crate::connection_dispatch::canonical::material_digest_value(
        CONNECTION_DISPATCH_DOMAIN,
        &serde_json::json!({"kind": "destination@1", "destination": destination}),
    )
}

// ---------------------------------------------------------------------------
// Args for the five operations
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SnapshotArgsV1 {
    pub v: u32,
    pub manifest_id: Ident,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuoteArgsV1 {
    pub v: u32,
    pub connection_id: Ident,
    pub expected_credential_revision: Revision,
    pub expected_authorization_revision: Revision,
    pub snapshot_id: UuidV1,
    pub loaded_receipt_id: UuidV1,
    pub capability_id: Ident,
    pub model: ModelId,
    pub max_output_tokens: MaxOutputTokens,
    pub max_cost_micros: UsdMicros,
    pub data_scope_id: Ident,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StageSendArgsV1 {
    pub v: u32,
    pub connection_id: Ident,
    pub snapshot_id: UuidV1,
    pub loaded_receipt_id: UuidV1,
    pub capability_id: Ident,
    pub source_id: Ident,
    pub expected_source_digest: Digest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResultArgsV1 {
    pub v: u32,
    pub dispatch_id: UuidV1,
    pub request_id: UuidV1,
}

// ---------------------------------------------------------------------------
// Manifest / snapshot / loaded receipt (public, secret-free)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKindV1 {
    Skill,
    Mcp,
    Tool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectClassV1 {
    Read,
    Draft,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactV1 {
    pub artifact_id: Ident,
    pub kind: ArtifactKindV1,
    pub version: Ident,
    pub content_digest: Digest,
    pub implementation_digest: Digest,
    pub resource_digest: Digest,
    pub input_schema_digest: Digest,
    pub output_schema_digest: Digest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CallableV1 {
    pub name: Ident,
    pub version: Ident,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CapabilityV1 {
    pub capability_id: Ident,
    pub callable: CallableV1,
    pub artifact_id: Ident,
    pub operation: OperationKindV1,
    pub effect: EffectClassV1,
    pub connection_scope: Ident,
    pub policy_scope: Ident,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ManifestV1 {
    pub v: u32,
    pub manifest_id: Ident,
    pub revision: Revision,
    pub artifacts: Vec<ArtifactV1>,
    pub capabilities: Vec<CapabilityV1>,
    pub policy_digest: Digest,
    pub material_digest: Digest,
}

pub fn validate_manifest_v1(manifest: &ManifestV1) -> Result<(), DispatchError> {
    require_v(manifest.v)?;
    if manifest.artifacts.is_empty() || manifest.capabilities.is_empty() {
        return Err(validation_error(ErrorCodeV1::InvalidRequest));
    }
    bounded(&manifest.artifacts, 1024, ErrorCodeV1::PayloadTooLarge)?;
    bounded(&manifest.capabilities, 1024, ErrorCodeV1::PayloadTooLarge)?;
    let mut artifact_ids = std::collections::BTreeSet::new();
    for artifact in &manifest.artifacts {
        if !artifact_ids.insert(artifact.artifact_id.as_str()) {
            return Err(validation_error(ErrorCodeV1::InvalidRequest));
        }
    }
    let mut capability_ids = std::collections::BTreeSet::new();
    for capability in &manifest.capabilities {
        if !capability_ids.insert(capability.capability_id.as_str()) {
            return Err(validation_error(ErrorCodeV1::InvalidRequest));
        }
        if !artifact_ids.contains(capability.artifact_id.as_str()) {
            return Err(validation_error(ErrorCodeV1::CapabilityUnavailable));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequesterV1 {
    pub agent_uid: AgentUid,
    pub session_binding_id: UuidV1,
    pub runtime_generation: Revision,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionBindingV1 {
    pub connection_id: Ident,
    pub provider_kind: ProviderKindV1,
    pub credential_revision: Revision,
    pub authorization_revision: Revision,
    pub allowed_operations: Vec<OperationKindV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SnapshotV1 {
    pub v: u32,
    pub snapshot_id: UuidV1,
    pub requester: RequesterV1,
    pub manifest_id: Ident,
    pub manifest_revision: Revision,
    pub manifest_digest: Digest,
    pub policy_digest: Digest,
    pub connection_bindings: Vec<ConnectionBindingV1>,
    pub capabilities: Vec<CapabilityV1>,
    pub issued_at_ms: EpochMs,
    pub expires_at_ms: EpochMs,
}

pub fn validate_snapshot_v1(snapshot: &SnapshotV1) -> Result<(), DispatchError> {
    require_v(snapshot.v)?;
    let lifetime = snapshot
        .expires_at_ms
        .get()
        .saturating_sub(snapshot.issued_at_ms.get());
    if lifetime == 0 || lifetime > SNAPSHOT_LIFETIME_MS {
        return Err(validation_error(ErrorCodeV1::InvalidRequest));
    }
    let mut connections = std::collections::BTreeSet::new();
    for binding in &snapshot.connection_bindings {
        if !connections.insert(binding.connection_id.as_str()) {
            return Err(validation_error(ErrorCodeV1::InvalidRequest));
        }
        if binding.allowed_operations.is_empty() {
            return Err(validation_error(ErrorCodeV1::InvalidRequest));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoadedReceiptV1 {
    pub v: u32,
    pub loaded_receipt_id: UuidV1,
    pub snapshot_id: UuidV1,
    pub manifest_digest: Digest,
    pub observed_registry_digest: Digest,
    pub runtime_generation: Revision,
    pub enrollment_id: UuidV1,
    pub issued_at_ms: EpochMs,
    pub expires_at_ms: EpochMs,
}

pub fn validate_loaded_receipt_v1(
    receipt: &LoadedReceiptV1,
    snapshot: &SnapshotV1,
) -> Result<(), DispatchError> {
    require_v(receipt.v)?;
    if receipt.snapshot_id != snapshot.snapshot_id
        || receipt.manifest_digest != snapshot.manifest_digest
        || receipt.runtime_generation != snapshot.requester.runtime_generation
    {
        return Err(validation_error(ErrorCodeV1::LoadedStateMismatch));
    }
    if receipt.expires_at_ms.get() > snapshot.expires_at_ms.get()
        || receipt.expires_at_ms.get() <= receipt.issued_at_ms.get()
    {
        return Err(validation_error(ErrorCodeV1::InvalidRequest));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Provider, destination, principal binding
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKindV1 {
    Smtp,
    Openrouter,
}

impl ProviderKindV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKindV1::Smtp => "smtp",
            ProviderKindV1::Openrouter => "openrouter",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RuntimeProfileV1 {
    #[serde(rename = "native-oci-agent/1")]
    NativeOciAgent1,
    #[serde(rename = "tenant-custody-worker/1")]
    TenantCustodyWorker1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FenceStateV1 {
    Clear,
    Revoking,
    Revoked,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKindV1 {
    Agent,
    Human,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RoleV1 {
    Master,
    Worker,
}

impl RoleV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            RoleV1::Master => "master",
            RoleV1::Worker => "worker",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GrantStateV1 {
    Active,
    Revoked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeSessionV1 {
    pub session_id: Ident,
    pub generation: Revision,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeRuntimeBindingV1 {
    pub kind: NativeBindingKindV1,
    pub launch_digest: Digest,
    pub continuity_digest: Digest,
    pub agent_generation: Revision,
    pub owner_generation: Revision,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum NativeBindingKindV1 {
    #[serde(rename = "native")]
    Native,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HostedRuntimeBindingV1 {
    pub kind: HostedBindingKindV1,
    pub session_object_id: Ident,
    pub placement_epoch: Revision,
    pub activation_id: UuidV1,
    pub custody_epoch: Revision,
    pub restore_epoch: Revision,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum HostedBindingKindV1 {
    #[serde(rename = "hosted")]
    Hosted,
}

/// Native launch/continuity or hosted object/placement/activation/restore.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum RuntimeBindingV1 {
    Native(NativeRuntimeBindingV1),
    Hosted(HostedRuntimeBindingV1),
}

/// The persisted principal binding. Untrusted selectors never reach this
/// shape; it is built by the trusted adapter from durable rows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrincipalBindingV1 {
    pub custody_domain: Ident,
    pub workspace_id: Ident,
    pub kind: PrincipalKindV1,
    pub principal_id: Ident,
    pub agent_uid: AgentUid,
    pub alias: Option<Ident>,
    pub native_session: Option<NativeSessionV1>,
    pub session_authority_ref: Option<Ident>,
    pub session_generation: Option<Revision>,
    pub conversation_generation: Option<Revision>,
    pub enrollment_generation: Revision,
    pub binding: RuntimeBindingV1,
}

// ---------------------------------------------------------------------------
// Trusted-adapter admission facts
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeFactsV1 {
    pub profile: RuntimeProfileV1,
    pub runtime_generation: Revision,
    pub enrollment_id: UuidV1,
    pub loaded_receipt_id: UuidV1,
    pub observed_registry_digest: Digest,
    pub owner_epoch: Revision,
    pub custody_epoch: Option<Revision>,
    pub fence: FenceStateV1,
    pub snapshot_issued_ms: EpochMs,
    pub snapshot_expires_ms: EpochMs,
    pub receipt_issued_ms: EpochMs,
    pub receipt_expires_ms: EpochMs,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionFactsV1 {
    pub connection_id: Ident,
    pub provider_kind: ProviderKindV1,
    pub workspace_id: Ident,
    pub custody_domain: Ident,
    pub account: Ident,
    pub incarnation: Ident,
    pub credential_revision: Revision,
    pub authorization_revision: Revision,
    pub policy_revision: Revision,
    pub key_id: Ident,
    pub exchange: Option<Ident>,
    pub enabled: bool,
    pub fence: FenceStateV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DataScopeSourceV1 {
    CallerText,
    ImmutableSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DataScopeV1 {
    pub scope_id: Ident,
    pub source: DataScopeSourceV1,
    pub retention_policy: Ident,
    pub allow_caller_text: bool,
    pub content_digest: Option<Digest>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationPolicyV1 {
    pub policy_revision: Revision,
    pub active: bool,
    pub allowed_operations: Vec<OperationKindV1>,
    pub allowed_human_operations: Vec<OperationKindV1>,
    pub allowed_destinations: Vec<Ident>,
    pub allowed_models: Vec<ModelId>,
    pub provider_profile: Ident,
    pub data_scopes: Vec<DataScopeV1>,
    pub approved_document_digest: Digest,
}

impl OperationPolicyV1 {
    pub fn data_scope(&self, scope_id: &Ident) -> Option<&DataScopeV1> {
        self.data_scopes
            .iter()
            .find(|scope| scope.scope_id == *scope_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UidGrantV1 {
    pub agent_uid: AgentUid,
    pub role: RoleV1,
    pub state: GrantStateV1,
    /// Hosted grants bind explicit subject ids; a native grant leaves it empty.
    pub allowed_subject_ids: Vec<Ident>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoleModelPolicyV1 {
    pub allow: Vec<ModelId>,
    pub master_allow: Option<Vec<ModelId>>,
    pub worker_allow: Option<Vec<ModelId>>,
    pub agentic_providers: Vec<Ident>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LimitSetV1 {
    pub requests_per_60s: Counter,
    pub unresolved: Counter,
    pub daily_micros: UsdMicros,
    pub lifetime_micros: UsdMicros,
    pub per_call_micros: UsdMicros,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OwnerLimitsV1 {
    pub principal: LimitSetV1,
    pub account: LimitSetV1,
    pub workspace: LimitSetV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExposureScopeV1 {
    /// Request timestamps inside the caller's durable view of the window.
    pub recent_request_ms: Vec<EpochMs>,
    pub unresolved: Counter,
    pub daily_settled_micros: UsdMicros,
    /// ALL unresolved reservations regardless of age.
    pub unresolved_reserved_micros: UsdMicros,
    pub lifetime_settled_micros: UsdMicros,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExposureFactsV1 {
    pub principal: ExposureScopeV1,
    pub account: ExposureScopeV1,
    pub workspace: ExposureScopeV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DestinationKindV1 {
    Smtp,
    Https,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolvedDestinationV1 {
    pub kind: DestinationKindV1,
    pub host: Ident,
    pub port: Option<u16>,
    pub path: Option<String>,
}

impl ResolvedDestinationV1 {
    /// Exact SMTP destination label used by owner allowlists.
    pub fn smtp_label(&self) -> Option<String> {
        match (self.kind, self.port) {
            (DestinationKindV1::Smtp, Some(port)) => Some(format!("{}:{port}", self.host)),
            _ => None,
        }
    }

    /// The canonical `https://host[:port]/path` the quote endpoint set must
    /// contain. `path` is stored without its leading slash.
    pub fn https_endpoint(&self) -> Option<String> {
        match (self.kind, &self.path) {
            (DestinationKindV1::Https, Some(path)) => {
                let port = match self.port {
                    None | Some(443) => String::new(),
                    Some(port) => format!(":{port}"),
                };
                Some(format!("https://{}{}/{}", self.host, port, path))
            }
            _ => None,
        }
    }
}

/// SMTP is exactly 465/587 with no path; HTTPS is 443 (or default) with an
/// absolute path and no query/fragment. Anything else refuses.
pub fn validate_resolved_destination_v1(
    destination: &ResolvedDestinationV1,
) -> Result<(), DispatchError> {
    match destination.kind {
        DestinationKindV1::Smtp => {
            if !matches!(destination.port, Some(465) | Some(587)) || destination.path.is_some() {
                return Err(validation_error(ErrorCodeV1::DestinationRefused));
            }
        }
        DestinationKindV1::Https => {
            if !matches!(destination.port, None | Some(443)) {
                return Err(validation_error(ErrorCodeV1::DestinationRefused));
            }
            let Some(path) = destination.path.as_deref() else {
                return Err(validation_error(ErrorCodeV1::DestinationRefused));
            };
            if path.contains('?') || path.contains('#') || path.contains(' ') {
                return Err(validation_error(ErrorCodeV1::DestinationRefused));
            }
            let Some(endpoint) = destination.https_endpoint() else {
                return Err(validation_error(ErrorCodeV1::DestinationRefused));
            };
            HttpsUrl::try_from(endpoint)
                .map_err(|_| validation_error(ErrorCodeV1::DestinationRefused))?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolvedMaterialV1 {
    pub destination: ResolvedDestinationV1,
    pub provider_body_digest: Digest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendApprovalV1 {
    pub approval_id: Ident,
    pub connection_id: Ident,
    pub source_id: Ident,
    pub source_digest: Digest,
    pub action: Ident,
    pub principal_digest: Digest,
    pub credential_revision: Revision,
    pub authorization_revision: Revision,
    pub policy_revision: Revision,
    pub body_digest: Digest,
    pub destination_digest: Digest,
    pub policy_digest: Digest,
    /// Positive reference to the durable human decision record.
    pub human_decision_ref: Ident,
    pub consumed_dispatch_id: Option<UuidV1>,
}

/// The server record that binds a human test send or verification to exact
/// revisions, content and destination. The human decision reference is a
/// positive durable row id; there is no `approved` boolean.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HumanActionRecordV1 {
    pub record_id: Ident,
    pub kind: OperationKindV1,
    pub connection_id: Ident,
    pub credential_revision: Revision,
    pub authorization_revision: Revision,
    pub policy_revision: Revision,
    pub body_digest: Digest,
    pub destination_digest: Digest,
    pub human_decision_ref: Ident,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExistingDispatchV1 {
    pub v: u32,
    pub dispatch_id: UuidV1,
    pub request_id: UuidV1,
    pub state: DispatchStateV1,
    pub idempotency_digest: Digest,
    pub commitment_digest: Digest,
    pub payload_digest: Digest,
    pub reserved_micros: UsdMicros,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdmissionSnapshotV1 {
    pub v: u32,
    pub snapshot_id: UuidV1,
    pub dispatch_id: UuidV1,
    pub request_id: UuidV1,
    pub now_ms: EpochMs,
    pub clock_floor_ms: EpochMs,
    pub database_epoch: Revision,
    pub ledger_head: LedgerHeadV1,
    pub requester: RequesterV1,
    pub principal: PrincipalBindingV1,
    pub runtime: RuntimeFactsV1,
    pub connection: ConnectionFactsV1,
    pub policy: OperationPolicyV1,
    pub grants: Vec<UidGrantV1>,
    pub role_policy: Option<RoleModelPolicyV1>,
    pub limits: OwnerLimitsV1,
    pub exposure: ExposureFactsV1,
    pub manifest: ManifestV1,
    pub observed_registry_digest: Digest,
    pub policy_digest: Digest,
    pub resolved: ResolvedMaterialV1,
    pub approval: Option<SendApprovalV1>,
    pub test_send_record: Option<HumanActionRecordV1>,
    pub verify_record: Option<HumanActionRecordV1>,
    pub existing: Option<ExistingDispatchV1>,
}

pub fn validate_admission_snapshot_v1(snapshot: &AdmissionSnapshotV1) -> Result<(), DispatchError> {
    require_v(snapshot.v)?;
    if !snapshot.now_ms.at_or_after_floor(snapshot.clock_floor_ms) {
        return Err(validation_error(ErrorCodeV1::ClockRegression));
    }
    if snapshot.now_ms.get() >= snapshot.runtime.snapshot_expires_ms.get() {
        return Err(validation_error(ErrorCodeV1::SessionExpired));
    }
    if snapshot.now_ms.get() >= snapshot.runtime.receipt_expires_ms.get() {
        return Err(validation_error(ErrorCodeV1::LoadedStateMismatch));
    }
    if snapshot.runtime.receipt_expires_ms.get() > snapshot.runtime.snapshot_expires_ms.get()
        || snapshot.runtime.receipt_expires_ms.get() <= snapshot.runtime.receipt_issued_ms.get()
        || snapshot.runtime.snapshot_expires_ms.get() <= snapshot.runtime.snapshot_issued_ms.get()
        || snapshot
            .runtime
            .snapshot_expires_ms
            .get()
            .saturating_sub(snapshot.runtime.snapshot_issued_ms.get())
            > SNAPSHOT_LIFETIME_MS
    {
        return Err(validation_error(ErrorCodeV1::InvalidRequest));
    }
    if snapshot.runtime.profile != profile_of_principal(&snapshot.principal)
        || snapshot.principal.agent_uid != snapshot.requester.agent_uid
        || snapshot.runtime.runtime_generation != snapshot.requester.runtime_generation
    {
        return Err(validation_error(ErrorCodeV1::PrincipalRequired));
    }
    if snapshot.connection.fence != FenceStateV1::Clear
        || snapshot.runtime.fence != FenceStateV1::Clear
    {
        return Err(validation_error(ErrorCodeV1::RuntimeRevoked));
    }
    if snapshot.policy.policy_revision != snapshot.connection.policy_revision
        || snapshot.connection.workspace_id != snapshot.principal.workspace_id
        || snapshot.connection.custody_domain != snapshot.principal.custody_domain
    {
        return Err(validation_error(ErrorCodeV1::StaleRevision));
    }
    validate_manifest_v1(&snapshot.manifest)?;
    // Digests must agree between the manifest row, the snapshot view and the
    // observed registry; equality here is consistency, not authority.
    if snapshot.manifest.policy_digest != snapshot.policy_digest
        || snapshot.manifest.material_digest.as_str().is_empty()
    {
        return Err(validation_error(ErrorCodeV1::DigestMismatch));
    }
    if snapshot.observed_registry_digest != snapshot.runtime.observed_registry_digest {
        return Err(validation_error(ErrorCodeV1::LoadedStateMismatch));
    }
    // Ledger head shape and epoch floors.
    snapshot
        .ledger_head
        .validate()
        .map_err(|_| validation_error(ErrorCodeV1::LedgerMismatch))?;
    if snapshot.database_epoch.get() < snapshot.ledger_head.database_epoch.get() {
        return Err(validation_error(ErrorCodeV1::LedgerMismatch));
    }
    bounded(&snapshot.grants, 128, ErrorCodeV1::PayloadTooLarge)?;
    bounded(
        &snapshot.policy.allowed_models,
        512,
        ErrorCodeV1::PayloadTooLarge,
    )?;
    bounded(
        &snapshot.policy.data_scopes,
        128,
        ErrorCodeV1::PayloadTooLarge,
    )?;
    bounded(
        &snapshot.policy.allowed_destinations,
        128,
        ErrorCodeV1::PayloadTooLarge,
    )?;
    for scope in &snapshot.policy.data_scopes {
        match scope.source {
            DataScopeSourceV1::CallerText if !scope.allow_caller_text => {
                return Err(validation_error(ErrorCodeV1::DataPolicyRefused))
            }
            DataScopeSourceV1::ImmutableSource if scope.content_digest.is_none() => {
                return Err(validation_error(ErrorCodeV1::DataPolicyRefused))
            }
            _ => {}
        }
    }
    let mut scopes = std::collections::BTreeSet::new();
    for scope in &snapshot.policy.data_scopes {
        if !scopes.insert(scope.scope_id.as_str()) {
            return Err(validation_error(ErrorCodeV1::InvalidRequest));
        }
    }
    for scope in [
        &snapshot.exposure.principal,
        &snapshot.exposure.account,
        &snapshot.exposure.workspace,
    ] {
        bounded(&scope.recent_request_ms, 4096, ErrorCodeV1::PayloadTooLarge)?;
    }
    Ok(())
}

fn profile_of_principal(principal: &PrincipalBindingV1) -> RuntimeProfileV1 {
    match &principal.binding {
        RuntimeBindingV1::Native(_) => RuntimeProfileV1::NativeOciAgent1,
        RuntimeBindingV1::Hosted(_) => RuntimeProfileV1::TenantCustodyWorker1,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrozenCredentialIdentityV1 {
    pub workspace_id: Ident,
    pub custody_domain: Ident,
    pub connection_id: Ident,
    pub provider: Ident,
    pub account: Ident,
    pub credential_revision: Revision,
    pub key_id: Ident,
    pub exchange: Option<Ident>,
}

// ---------------------------------------------------------------------------
// Model quote and price
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum CurrencyV1 {
    #[serde(rename = "USD")]
    Usd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ReasoningBillingV1 {
    /// Hidden reasoning tokens are billed inside the completion rate. Every
    /// other billing shape is unrepresentable, so it refuses at parse.
    #[serde(rename = "included_in_completion")]
    IncludedInCompletion,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuotePriceV1 {
    pub currency: CurrencyV1,
    pub prompt_pico_per_token: PicoUsd,
    pub completion_pico_per_token: PicoUsd,
    pub request_pico: PicoUsd,
    pub reasoning_billing: ReasoningBillingV1,
}

/// Provider routing is fixed: no fallback and required parameters. The closed
/// enums make any other routing shape unrepresentable, so it refuses at parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum FallbackPolicyV1 {
    #[serde(rename = "disallowed")]
    Disallowed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ParameterPolicyV1 {
    #[serde(rename = "required")]
    Required,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelQuoteV1 {
    pub v: u32,
    pub quote_id: UuidV1,
    pub connection_id: Ident,
    pub credential_revision: Revision,
    pub authorization_revision: Revision,
    pub policy_revision: Revision,
    pub provider_profile: Ident,
    pub routing_profile: Ident,
    pub model: ModelId,
    /// Exact fixed routing endpoint set; the destination must be one of them.
    pub endpoints: Vec<HttpsUrl>,
    pub fallback: FallbackPolicyV1,
    pub parameters: ParameterPolicyV1,
    pub service_tier: Ident,
    pub metadata_digest: Digest,
    pub metadata_observed_ms: EpochMs,
    pub metadata_expires_ms: EpochMs,
    pub context_bound: Counter,
    pub max_output_bound: Counter,
    pub price: QuotePriceV1,
    pub privacy_policy: Ident,
    pub max_cost_micros: UsdMicros,
}

pub fn validate_model_quote_v1(quote: &ModelQuoteV1) -> Result<(), DispatchError> {
    require_v(quote.v)?;
    if quote.context_bound.get() == 0
        || quote.max_output_bound.get() == 0
        || quote.endpoints.is_empty()
    {
        return Err(validation_error(ErrorCodeV1::PricingUnavailable));
    }
    let mut endpoints = std::collections::BTreeSet::new();
    for endpoint in &quote.endpoints {
        if !endpoints.insert(endpoint.as_str()) {
            return Err(validation_error(ErrorCodeV1::InvalidRequest));
        }
    }
    let lifetime = quote
        .metadata_expires_ms
        .get()
        .saturating_sub(quote.metadata_observed_ms.get());
    if lifetime == 0 || lifetime > QUOTE_LIFETIME_MS {
        return Err(validation_error(ErrorCodeV1::PricingUnavailable));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Durable dispatch facts, prepared/committed handles and settlement
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RateDimensionsV1 {
    pub principal_id: Ident,
    pub account_id: Ident,
    pub workspace_id: Ident,
    pub window_ms: Counter,
}

/// The exact durable `connection_dispatches` row material. Serialize-only in
/// the native Rust API (construct it through the planner); the hosted adapter
/// may read its own row back and must pass [`validate_commit_v1`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DispatchCommitV1 {
    pub v: u32,
    pub dispatch_id: UuidV1,
    pub request_id: UuidV1,
    pub state: DispatchStateV1,
    pub workspace_id: Ident,
    pub custody_domain: Ident,
    pub connection_id: Ident,
    pub provider_kind: ProviderKindV1,
    pub connection_incarnation: Ident,
    pub credential_revision: Revision,
    pub authorization_revision: Revision,
    pub policy_revision: Revision,
    pub idempotency_key: Ident,
    pub idempotency_digest: Digest,
    pub principal_binding_digest: Digest,
    pub snapshot_id: UuidV1,
    pub loaded_receipt_id: UuidV1,
    pub manifest_digest: Digest,
    pub observed_registry_digest: Digest,
    pub runtime_generation: Revision,
    pub enrollment_id: UuidV1,
    pub capability_id: Ident,
    pub operation_kind: OperationKindV1,
    pub model: Option<ModelId>,
    pub input_digest: Digest,
    pub payload_digest: Digest,
    pub body_digest: Digest,
    pub destination_digest: Digest,
    pub quote_id: Option<UuidV1>,
    pub approval_id: Option<Ident>,
    pub request_timestamp_ms: EpochMs,
    pub database_epoch: Revision,
    pub rate_dimensions: RateDimensionsV1,
    pub reserved_micros: UsdMicros,
    pub frozen_quote: Option<ModelQuoteV1>,
    pub frozen_identity: FrozenCredentialIdentityV1,
    pub commitment_digest: Digest,
    pub ledger_sequence: Counter,
    pub ledger_digest: Digest,
}

impl DispatchCommitV1 {
    /// Exact immutable request material: idempotency key scope, binding,
    /// revisions, operation, payload/body/destination and reservation.
    /// Transient dispatch/request ids and wall-clock timestamps are excluded
    /// so a reconnect replays the status instead of charging twice.
    pub fn idempotency_material(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": "connection.dispatch.idempotency@1",
            "workspace_id": self.workspace_id,
            "custody_domain": self.custody_domain,
            "principal_binding_digest": self.principal_binding_digest,
            "idempotency_key": self.idempotency_key,
            "connection_id": self.connection_id,
            "connection_incarnation": self.connection_incarnation,
            "credential_revision": self.credential_revision,
            "authorization_revision": self.authorization_revision,
            "policy_revision": self.policy_revision,
            "operation_kind": self.operation_kind,
            "model": self.model,
            "input_digest": self.input_digest,
            "payload_digest": self.payload_digest,
            "body_digest": self.body_digest,
            "destination_digest": self.destination_digest,
            "quote_id": self.quote_id,
            "approval_id": self.approval_id,
            "reserved_micros": self.reserved_micros,
        })
    }

    /// The immutable binding/reservation material: every dispatch field except
    /// the final journal position (`ledger_sequence`/`ledger_digest`) and the
    /// commitment digest itself. Safe to journal and safe to include in the
    /// ledger record's `refs` (it cannot contain the head it precedes).
    pub fn binding_material(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": "connection.dispatch.binding@1",
            "dispatch_id": self.dispatch_id,
            "request_id": self.request_id,
            "state": self.state,
            "workspace_id": self.workspace_id,
            "custody_domain": self.custody_domain,
            "connection_id": self.connection_id,
            "provider_kind": self.provider_kind,
            "connection_incarnation": self.connection_incarnation,
            "credential_revision": self.credential_revision,
            "authorization_revision": self.authorization_revision,
            "policy_revision": self.policy_revision,
            "idempotency_key": self.idempotency_key,
            "idempotency_digest": self.idempotency_digest,
            "principal_binding_digest": self.principal_binding_digest,
            "snapshot_id": self.snapshot_id,
            "loaded_receipt_id": self.loaded_receipt_id,
            "manifest_digest": self.manifest_digest,
            "observed_registry_digest": self.observed_registry_digest,
            "runtime_generation": self.runtime_generation,
            "enrollment_id": self.enrollment_id,
            "capability_id": self.capability_id,
            "operation_kind": self.operation_kind,
            "model": self.model,
            "input_digest": self.input_digest,
            "payload_digest": self.payload_digest,
            "body_digest": self.body_digest,
            "destination_digest": self.destination_digest,
            "quote_id": self.quote_id,
            "approval_id": self.approval_id,
            "request_timestamp_ms": self.request_timestamp_ms,
            "database_epoch": self.database_epoch,
            "rate_dimensions": self.rate_dimensions,
            "reserved_micros": self.reserved_micros,
            "frozen_quote": self.frozen_quote,
            "frozen_identity": self.frozen_identity,
        })
    }

    /// Digest of the immutable binding/reservation, independent of the final
    /// ledger position.
    pub fn binding_digest(&self) -> Digest {
        crate::connection_dispatch::canonical::material_digest_value(
            CONNECTION_DISPATCH_DOMAIN,
            &self.binding_material(),
        )
    }

    /// Metadata digests an adapter may put in the ledger record's `refs`
    /// without referencing the head or the final commitment digest (so a
    /// generic metadata journal never has a circular hash).
    pub fn binding_refs(&self) -> Vec<Digest> {
        vec![
            self.principal_binding_digest.clone(),
            self.idempotency_digest.clone(),
            self.input_digest.clone(),
            self.payload_digest.clone(),
            self.body_digest.clone(),
            self.destination_digest.clone(),
        ]
    }

    /// The frozen commitment digest over every immutable binding and the real
    /// journal position.
    pub fn compute_commitment_digest(&self) -> Digest {
        let material = serde_json::json!({
            "kind": "connection.dispatch.commitment@1",
            "dispatch_id": self.dispatch_id,
            "request_id": self.request_id,
            "workspace_id": self.workspace_id,
            "custody_domain": self.custody_domain,
            "principal_binding_digest": self.principal_binding_digest,
            "connection_id": self.connection_id,
            "connection_incarnation": self.connection_incarnation,
            "provider_kind": self.provider_kind,
            "credential_revision": self.credential_revision,
            "authorization_revision": self.authorization_revision,
            "policy_revision": self.policy_revision,
            "idempotency_digest": self.idempotency_digest,
            "snapshot_id": self.snapshot_id,
            "loaded_receipt_id": self.loaded_receipt_id,
            "manifest_digest": self.manifest_digest,
            "observed_registry_digest": self.observed_registry_digest,
            "runtime_generation": self.runtime_generation,
            "enrollment_id": self.enrollment_id,
            "capability_id": self.capability_id,
            "operation_kind": self.operation_kind,
            "model": self.model,
            "input_digest": self.input_digest,
            "payload_digest": self.payload_digest,
            "body_digest": self.body_digest,
            "destination_digest": self.destination_digest,
            "quote_id": self.quote_id,
            "approval_id": self.approval_id,
            "request_timestamp_ms": self.request_timestamp_ms,
            "rate_dimensions": self.rate_dimensions,
            "reserved_micros": self.reserved_micros,
            "frozen_quote": self.frozen_quote,
            "frozen_identity": self.frozen_identity,
            "ledger_sequence": self.ledger_sequence,
            "ledger_digest": self.ledger_digest,
        });
        crate::connection_dispatch::canonical::material_digest_value(
            CONNECTION_DISPATCH_DOMAIN,
            &material,
        )
    }
}

/// Verify a durable commit row before it becomes an execution handle.
pub fn validate_commit_v1(commit: &DispatchCommitV1) -> Result<(), DispatchError> {
    require_v(commit.v)?;
    if commit.state != DispatchStateV1::Committed {
        return Err(validation_error(ErrorCodeV1::StateConflict));
    }
    if commit.ledger_sequence.get() == 0 {
        return Err(validation_error(ErrorCodeV1::LedgerMismatch));
    }
    let expected = commit.compute_commitment_digest();
    if expected != commit.commitment_digest {
        return Err(validation_error(ErrorCodeV1::DigestMismatch));
    }
    let idempotency = crate::connection_dispatch::canonical::material_digest_value(
        CONNECTION_DISPATCH_DOMAIN,
        &commit.idempotency_material(),
    );
    if idempotency != commit.idempotency_digest {
        return Err(validation_error(ErrorCodeV1::DigestMismatch));
    }
    if commit.operation_kind == OperationKindV1::TextGenerate && commit.frozen_quote.is_none() {
        return Err(validation_error(ErrorCodeV1::PricingUnavailable));
    }
    Ok(())
}

/// The planner's output: immutable binding/reservation data BEFORE the
/// durable transaction. Its `ledger_sequence`/`ledger_digest`/
/// `commitment_digest` are zero placeholders; [`Self::finalize`] binds the
/// actual transaction journal head. Plain data; possessing it authorizes
/// nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedDispatchV1 {
    commit: DispatchCommitV1,
    previous_head: LedgerHeadV1,
}

impl PreparedDispatchV1 {
    pub(crate) fn new(commit: DispatchCommitV1, previous_head: LedgerHeadV1) -> Self {
        Self {
            commit,
            previous_head,
        }
    }
    /// Reconstruct the planner output from its own wire form (the trusted
    /// adapter's plan result) plus the ledger head it observed, for
    /// [`Self::finalize`].
    pub fn resume(commit: DispatchCommitV1, previous_head: LedgerHeadV1) -> Self {
        Self {
            commit,
            previous_head,
        }
    }
    pub fn previous_head(&self) -> &LedgerHeadV1 {
        &self.previous_head
    }
    pub fn commit_facts(&self) -> &DispatchCommitV1 {
        &self.commit
    }
    pub fn dispatch_id(&self) -> &UuidV1 {
        &self.commit.dispatch_id
    }
    pub fn request_id(&self) -> &UuidV1 {
        &self.commit.request_id
    }
    pub fn reservation(&self) -> UsdMicros {
        self.commit.reserved_micros
    }
    pub fn binding_digest(&self) -> Digest {
        self.commit.binding_digest()
    }
    pub fn binding_refs(&self) -> Vec<Digest> {
        self.commit.binding_refs()
    }
    /// Bind the exact journal head produced by the NEW durable transaction.
    /// The record(s) that produced this head must reference
    /// [`Self::binding_refs`] (never this head or the final commitment
    /// digest), so no hash cycle exists. Returns the durable commit row.
    pub fn finalize(&self, head: &LedgerHeadV1) -> Result<DispatchCommitV1, DispatchError> {
        if self.commit.ledger_sequence.get() != 0 {
            return Err(validation_error(ErrorCodeV1::StateConflict));
        }
        if head.sequence.get() == 0
            || head.database_epoch != self.commit.database_epoch
            || self.previous_head.database_epoch != self.commit.database_epoch
            || head.sequence.get() <= self.previous_head.sequence.get()
        {
            return Err(validation_error(ErrorCodeV1::LedgerMismatch));
        }
        let mut commit = self.commit.clone();
        commit.ledger_sequence = head.sequence;
        commit.ledger_digest = head.digest.clone();
        commit.commitment_digest = commit.compute_commitment_digest();
        validate_commit_v1(&commit)?;
        Ok(commit)
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanOutcomeV1 {
    Prepared(PreparedDispatchV1),
    Duplicate(ExistingDispatchStatusV1),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExistingDispatchStatusV1 {
    pub dispatch_id: UuidV1,
    pub request_id: UuidV1,
    pub state: DispatchStateV1,
    pub commitment_digest: Digest,
    pub payload_digest: Digest,
    pub reserved_micros: UsdMicros,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DispatchStateV1 {
    Committed,
    Done,
    Refused,
    Unknown,
    Overrun,
}

#[allow(clippy::struct_field_names)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageV1 {
    pub input_tokens: Counter,
    pub output_tokens: Counter,
    pub reasoning_tokens: Counter,
    pub total_tokens: Counter,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReceiptCostV1 {
    pub currency: CurrencyV1,
    pub reserved_micros: UsdMicros,
    pub actual_micros: Option<UsdMicros>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReceiptErrorV1 {
    pub v: u32,
    pub code: ErrorCodeV1,
    pub message: String,
}

impl ReceiptErrorV1 {
    pub fn fixed(code: ErrorCodeV1) -> Self {
        Self {
            v: CONNECTION_CONTRACT_VERSION,
            code,
            message: code.message().to_string(),
        }
    }
    pub fn validate(&self) -> Result<(), DispatchError> {
        require_v(self.v)?;
        if self.message != self.code.message() {
            return Err(validation_error(ErrorCodeV1::InvalidRequest));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextGeneratedV1 {
    pub kind: TextGeneratedKindV1,
    pub model: ModelId,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SmtpAcceptedV1 {
    pub kind: SmtpAcceptedKindV1,
    pub accepted: bool,
    /// Exactly `server-acceptance-only`; no delivery claim exists.
    pub claim: SmtpAcceptanceClaimV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SmtpAcceptanceClaimV1 {
    #[serde(rename = "server-acceptance-only")]
    ServerAcceptanceOnly,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SmtpVerifiedV1 {
    pub kind: SmtpVerifiedKindV1,
    pub verified: bool,
    /// Exactly `authentication-only`; verification grants no send/spend.
    pub claim: SmtpVerificationClaimV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SmtpVerificationClaimV1 {
    #[serde(rename = "authentication-only")]
    AuthenticationOnly,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ReceiptResultV1 {
    TextGenerated(TextGeneratedV1),
    SmtpAccepted(SmtpAcceptedV1),
    SmtpVerified(SmtpVerifiedV1),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DispatchReceiptV1 {
    pub v: u32,
    pub dispatch_id: UuidV1,
    pub request_id: UuidV1,
    pub payload_digest: Digest,
    pub commitment_digest: Digest,
    pub state: DispatchStateV1,
    pub result: Option<ReceiptResultV1>,
    pub error: Option<ReceiptErrorV1>,
    pub cost: ReceiptCostV1,
    pub usage: Option<UsageV1>,
}

impl DispatchReceiptV1 {
    /// Every nullable combination is pinned; an impossible receipt refuses.
    pub fn validate(&self) -> Result<(), DispatchError> {
        require_v(self.v)?;
        if let Some(error) = &self.error {
            error.validate()?;
        }
        let model_result = matches!(&self.result, Some(ReceiptResultV1::TextGenerated(result)) if {
            result.kind == TextGeneratedKindV1::TextGenerated
                && result.text.len() <= MAX_MODEL_TEXT_BYTES
        });
        match self.state {
            DispatchStateV1::Committed | DispatchStateV1::Unknown => {
                if self.result.is_some()
                    || self.error.is_some()
                    || self.usage.is_some()
                    || self.cost.actual_micros.is_some()
                {
                    return Err(validation_error(ErrorCodeV1::InvalidRequest));
                }
            }
            DispatchStateV1::Done => {
                if self.error.is_some() || self.cost.actual_micros.is_none() {
                    return Err(validation_error(ErrorCodeV1::InvalidRequest));
                }
                match &self.result {
                    Some(ReceiptResultV1::TextGenerated(_)) => {
                        if !model_result || self.usage.is_none() {
                            return Err(validation_error(ErrorCodeV1::InvalidRequest));
                        }
                    }
                    Some(ReceiptResultV1::SmtpAccepted(result)) => {
                        if !result.accepted || self.usage.is_some() {
                            return Err(validation_error(ErrorCodeV1::InvalidRequest));
                        }
                    }
                    Some(ReceiptResultV1::SmtpVerified(result)) => {
                        if !result.verified || self.usage.is_some() {
                            return Err(validation_error(ErrorCodeV1::InvalidRequest));
                        }
                    }
                    None => return Err(validation_error(ErrorCodeV1::InvalidRequest)),
                }
            }
            DispatchStateV1::Refused => {
                if self.result.is_some() || self.usage.is_some() || self.error.is_none() {
                    return Err(validation_error(ErrorCodeV1::InvalidRequest));
                }
                if !self
                    .error
                    .as_ref()
                    .map(|e| e.code.is_settlement_refusal())
                    .unwrap_or(false)
                {
                    return Err(validation_error(ErrorCodeV1::InvalidRequest));
                }
            }
            DispatchStateV1::Overrun => {
                if self.result.is_some() || self.usage.is_none() {
                    return Err(validation_error(ErrorCodeV1::InvalidRequest));
                }
                match &self.error {
                    Some(error) if error.code == ErrorCodeV1::ReservationOverrun => {}
                    _ => return Err(validation_error(ErrorCodeV1::InvalidRequest)),
                }
                if self.cost.actual_micros.is_none() {
                    return Err(validation_error(ErrorCodeV1::InvalidRequest));
                }
            }
        }
        // Receipt metadata is bounded to 8 KiB; the model text may add 1 MiB.
        let mut metadata = self.clone();
        if let Some(ReceiptResultV1::TextGenerated(result)) = metadata.result.as_mut() {
            result.text.clear();
        }
        let bytes = serde_json::to_vec(&metadata)
            .map_err(|_| validation_error(ErrorCodeV1::PayloadTooLarge))?;
        if bytes.len() > MAX_RECEIPT_METADATA_BYTES {
            return Err(validation_error(ErrorCodeV1::PayloadTooLarge));
        }
        Ok(())
    }
}

pub fn validate_dispatch_receipt_v1(receipt: &DispatchReceiptV1) -> Result<(), DispatchError> {
    receipt.validate()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKindV1 {
    Done,
    Refused,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextOutcomeV1 {
    pub kind: TextGeneratedKindV1,
    pub model: ModelId,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SmtpAcceptedOutcomeV1 {
    pub kind: SmtpAcceptedKindV1,
    pub accepted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SmtpVerifiedOutcomeV1 {
    pub kind: SmtpVerifiedKindV1,
    pub verified: bool,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum OutcomeResultV1 {
    TextGenerated(TextOutcomeV1),
    SmtpAccepted(SmtpAcceptedOutcomeV1),
    SmtpVerified(SmtpVerifiedOutcomeV1),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidatedOutcomeV1 {
    pub v: u32,
    pub kind: OutcomeKindV1,
    pub usage: Option<UsageV1>,
    pub result: Option<OutcomeResultV1>,
    pub refused_code: Option<ErrorCodeV1>,
}

impl ValidatedOutcomeV1 {
    pub fn validate(&self) -> Result<(), DispatchError> {
        require_v(self.v)?;
        match self.kind {
            OutcomeKindV1::Done => {
                if self.result.is_none() || self.refused_code.is_some() {
                    return Err(validation_error(ErrorCodeV1::InvalidRequest));
                }
                match self.result.as_ref() {
                    Some(OutcomeResultV1::TextGenerated(result)) => {
                        if self.usage.is_none()
                            || result.kind != TextGeneratedKindV1::TextGenerated
                            || result.text.len() > MAX_MODEL_TEXT_BYTES
                        {
                            return Err(validation_error(ErrorCodeV1::InvalidRequest));
                        }
                    }
                    Some(OutcomeResultV1::SmtpAccepted(result)) => {
                        if self.usage.is_some() || !result.accepted {
                            return Err(validation_error(ErrorCodeV1::InvalidRequest));
                        }
                    }
                    Some(OutcomeResultV1::SmtpVerified(result)) => {
                        if self.usage.is_some() || !result.verified {
                            return Err(validation_error(ErrorCodeV1::InvalidRequest));
                        }
                    }
                    None => return Err(validation_error(ErrorCodeV1::InvalidRequest)),
                }
            }
            OutcomeKindV1::Refused => {
                if self.usage.is_some() || self.result.is_some() {
                    return Err(validation_error(ErrorCodeV1::InvalidRequest));
                }
                let Some(code) = self.refused_code else {
                    return Err(validation_error(ErrorCodeV1::InvalidRequest));
                };
                if !code.is_settlement_refusal() {
                    return Err(validation_error(ErrorCodeV1::InvalidRequest));
                }
            }
            OutcomeKindV1::Unknown => {
                if self.usage.is_some() || self.result.is_some() || self.refused_code.is_some() {
                    return Err(validation_error(ErrorCodeV1::InvalidRequest));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettlementV1 {
    pub v: u32,
    pub receipt: DispatchReceiptV1,
    /// True when the settlement recorded an overrun and the adapter must fence.
    pub fence: bool,
}

// ---------------------------------------------------------------------------
// Human management DTOs (metadata only; no credential field exists here)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HumanVerbV1 {
    Enroll,
    Rotate,
    Verify,
}

/// Metadata-only guest intent. Actor, conversation, custody and request state
/// are derived by the trusted parent; no credential field is accepted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CredentialRequestArgsV1 {
    pub v: u32,
    pub provider: String,
    pub verb: HumanVerbV1,
    #[serde(default)]
    pub connection_id: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CredentialRequestStateV1 {
    Pending,
    Submitting,
    Done,
    Cancelled,
    Expired,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CredentialRequestStatusV1 {
    PendingHuman,
    Recorded,
    Refused,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentIntentV1 {
    pub workspace_id: Ident,
    pub custody_domain: Ident,
    pub custody_epoch: Revision,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CredentialRequestV1 {
    pub v: u32,
    pub request_id: UuidV1,
    pub participant_uid: AgentUid,
    pub conversation_id: Ident,
    pub provider_kind: ProviderKindV1,
    pub connection_id: Option<Ident>,
    pub enrollment_intent: Option<EnrollmentIntentV1>,
    pub expected_credential_revision: Option<Revision>,
    pub expected_authorization_revision: Option<Revision>,
    pub expected_request_revision: Revision,
    pub custody_domain: Ident,
    pub custody_epoch: Revision,
    pub allowed_verb: HumanVerbV1,
    pub state: CredentialRequestStateV1,
    pub status: CredentialRequestStatusV1,
}

pub fn validate_credential_request_v1(request: &CredentialRequestV1) -> Result<(), DispatchError> {
    require_v(request.v)?;
    match (&request.connection_id, &request.enrollment_intent) {
        (Some(_), None) => {
            if request.expected_credential_revision.is_none()
                || request.expected_authorization_revision.is_none()
            {
                return Err(validation_error(ErrorCodeV1::InvalidRequest));
            }
            if request.allowed_verb == HumanVerbV1::Enroll {
                return Err(validation_error(ErrorCodeV1::InvalidRequest));
            }
        }
        (None, Some(intent)) => {
            if request.allowed_verb != HumanVerbV1::Enroll
                || request.expected_credential_revision.is_some()
                || request.expected_authorization_revision.is_some()
                || intent.custody_domain != request.custody_domain
                || intent.custody_epoch != request.custody_epoch
            {
                return Err(validation_error(ErrorCodeV1::InvalidRequest));
            }
        }
        _ => return Err(validation_error(ErrorCodeV1::InvalidRequest)),
    }
    Ok(())
}

/// `verifyV1`: exactly the five fields; no password/role/destination/adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifyConnectionV1 {
    pub v: u32,
    pub attempt_id: UuidV1,
    pub connection_id: Ident,
    pub expected_credential_revision: Revision,
    pub expected_authorization_revision: Revision,
    pub verification_request_id: UuidV1,
}

pub fn validate_verify_connection_v1(request: &VerifyConnectionV1) -> Result<(), DispatchError> {
    require_v(request.v)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionMethodV1 {
    SnapshotV1,
    ModelQuoteV1,
    StageSendV1,
    InvokeV1,
    ResultV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionCallV1 {
    pub v: u32,
    pub invocation_id: UuidV1,
    pub session_object_id: Ident,
    pub method: SessionMethodV1,
    pub input_digest: Digest,
    pub task_key: Ident,
}

pub fn validate_session_call_v1(call: &SessionCallV1) -> Result<(), DispatchError> {
    require_v(call.v)
}

/// A receipt's frozen identity vs the handle that produced it.
pub fn validate_receipt_against_commit(
    receipt: &DispatchReceiptV1,
    commit: &DispatchCommitV1,
) -> Result<(), DispatchError> {
    receipt.validate()?;
    if receipt.dispatch_id != commit.dispatch_id
        || receipt.request_id != commit.request_id
        || receipt.payload_digest != commit.payload_digest
        || receipt.commitment_digest != commit.commitment_digest
        || receipt.cost.reserved_micros != commit.reserved_micros
    {
        return Err(validation_error(ErrorCodeV1::LoadedStateMismatch));
    }
    Ok(())
}

/// Result of a definitive settlement, including overrun accounting.
pub fn settlement_actual_is_overrun(actual: UsdMicros, reserved: UsdMicros) -> bool {
    actual.get() > reserved.get()
}

/// Ledger record re-export so adapters can journal metadata-only changes.
pub fn ledger_record_matches_prefix(
    record: &LedgerRecordV1,
    head: &LedgerHeadV1,
) -> Result<(), DispatchError> {
    if record.sequence != head.sequence || record.digest != head.digest {
        return Err(validation_error(ErrorCodeV1::LedgerMismatch));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Byte-bounded strict decoders (the exact v1 limits, applied before parsing)
// ---------------------------------------------------------------------------

/// Management JSON: 256 KiB.
pub fn parse_management_json<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
) -> Result<T, ParseError> {
    parse_strict(bytes, MAX_MANAGEMENT_BYTES)
}

/// Credential submit: 96 KiB (the raw credential itself stays <= 65,536).
pub fn parse_credential_submit_json<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
) -> Result<T, ParseError> {
    parse_strict(bytes, MAX_CREDENTIAL_SUBMIT_BYTES)
}

/// One whole operation: 1 MiB.
pub fn parse_operation_json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, ParseError> {
    parse_strict(bytes, MAX_OPERATION_BYTES)
}

/// Snapshot / provider metadata: 256 KiB.
pub fn parse_snapshot_json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, ParseError> {
    parse_strict(bytes, MAX_SNAPSHOT_BYTES)
}

/// Pure acknowledgement comparison for a durable commit row. Confers no
/// authority (see [`PreparedDispatchV1::checkpoint_matches`]).
pub fn checkpoint_matches_commit(
    commit: &DispatchCommitV1,
    acknowledgement: &CheckpointAckV1,
) -> Result<(), DispatchError> {
    acknowledgement
        .validate()
        .map_err(|_| validation_error(ErrorCodeV1::LedgerMismatch))?;
    // `ack.custody_domain` is the checkpoint plane (local|hosted), not the
    // Connection's data custody domain, so it is intentionally not compared
    // with `commit.custody_domain`.
    if acknowledgement.workspace_id != commit.workspace_id
        || acknowledgement.database_epoch != commit.database_epoch
        || acknowledgement.sequence != commit.ledger_sequence
        || acknowledgement.digest != commit.ledger_digest
    {
        return Err(validation_error(ErrorCodeV1::LedgerMismatch));
    }
    Ok(())
}
