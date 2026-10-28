//! CAD-1085: the native credential-entry bridge.
//!
//! Two independent authorities meet here and neither substitutes for the
//! other:
//!
//! - the daemon proves the *human* caller ([`Shared::operator_connection`] —
//!   no agent ancestry, no request-supplied identity) and re-validates the
//!   live authenticated owner session/page-key proof the board forwarded,
//!   deriving the one-use nonce binding itself; and
//! - the store proves the *attempt*: one unclaimed nonce bound to that
//!   session, the exact request revision, provider/verb/Connection, expected
//!   credential and authorization revisions and the current protected
//!   context digest is claimed atomically before anything is forwarded.
//!
//! The provider mutation itself belongs to the parent daemon integration:
//! this module constructs the private, non-serializable
//! [`VerifiedHumanAttemptV1`] plus the bounded [`BoundedSecretInputV1`] and
//! hands them to the supplied `apply` callback. A refusal is reported
//! truthfully; no branch fabricates success, retries a mutation or falls
//! back to plaintext.
//!
//! r3 I2: the operator's typed account/transport values are never written
//! to the request rows. A fresh SMTP enrollment carries them only in the
//! isolated form's single submit call, screened together with the password by
//! the parent before any new metadata or projection exists. Rotation and
//! verification read the non-secret transport from the already-admitted
//! encrypted record.
//!
//! Model operations never reach these methods: the per-runtime operations
//! socket has no management verb, and this module exposes nothing the agent
//! snapshot/invoke surface advertises.

use super::{optional_str, required_str, Shared};
use crate::error::{Error, Result};
use crate::store::credential_requests::{
    is_native_model_shape, AttemptOutcome, CredentialRequest, CredentialVerb, NewCredentialRequest,
    MAX_SECRET_BYTES, NONCE_TTL_MS,
};
use serde_json::{json, Value};
use std::sync::Arc;

/// The truthful diagnostic a human sees while the authenticated protected
/// profile/owner evidence is not current. Collection is disabled, never
/// downgraded.
pub(crate) const NOT_READY: &str = "Protected credential entry is not available on this daemon — the protected profile and its authenticated root owner must be current before a credential can be collected.";

/// What a request targets. The board/CLI names an existing agent alias or a
/// chat participant; the parent resolves the live context and decides
/// whether it is unambiguous, qualified and current.
#[derive(Clone, Debug)]
pub(crate) struct EntryTargetV1 {
    /// An operator-selected alias. Never authority by itself: the parent
    /// resolves its stable UID and requires a live qualified context.
    pub agent_alias: Option<String>,
    /// The chat producer's authenticated participant UID.
    pub participant_uid: Option<String>,
}

/// The live protected context the parent derives from the authenticated
/// management profile/supervisor/core evidence — never from SQLite metadata,
/// a caller boolean, or an arbitrary live agent runtime.
///
/// A human credential request has no agent runtime subject, so there is no
/// runtime generation and no loaded registry. It binds the actual measured
/// supervisor generation, the live authenticated owner epoch and the durable
/// database epoch instead; those are real facts, not a readiness claim.
#[derive(Clone, Debug)]
pub(crate) struct EntryContextV1 {
    pub workspace_id: String,
    pub custody_domain: String,
    pub custody_epoch: i64,
    pub owner_epoch: i64,
    pub supervisor_generation: String,
    /// Always `None` for human management: no agent registry is loaded for
    /// the human profile. It stays in the digest as an explicit fact.
    pub loaded_registry_digest: Option<String>,
    pub target_agent_uid: Option<String>,
}

impl EntryContextV1 {
    /// A canonical digest binding a request and its attempts to this exact
    /// context. Derived here so the store never has to trust a caller value.
    fn binding_digest(&self) -> Result<String> {
        if self.workspace_id.is_empty()
            || self.workspace_id.len() > 128
            || self.custody_domain.is_empty()
            || self.custody_domain.len() > 64
            || self.custody_epoch <= 0
            || self.owner_epoch <= 0
            || self.supervisor_generation.is_empty()
            || self.supervisor_generation.len() > 128
        {
            return Err(Error::invalid("request_not_ready", NOT_READY));
        }
        // One canonical digest implementation across native and hosted:
        // the shared typed-value digest, never an ad-hoc string join.
        Ok(
            cadence_core::connection_dispatch::material_digest_for_domain(
                "cadence-protected-entry-context-v1",
                &json!({
                    "v": 1,
                    "workspace_id": self.workspace_id,
                    "custody_domain": self.custody_domain,
                    "custody_epoch": self.custody_epoch,
                    "owner_epoch": self.owner_epoch,
                    "supervisor_generation": self.supervisor_generation,
                    "loaded_registry_digest": self.loaded_registry_digest,
                }),
            ),
        )
    }
}

fn context_digest(context: &EntryContextV1) -> Result<String> {
    context.binding_digest()
}

/// A metadata-only intent a trusted parent path may turn into a request for
/// an agent participant (the chat surface). It cannot carry a secret; the
/// browser fills that in on the trusted entry page.
#[derive(Clone, Debug)]
#[cfg_attr(
    not(all(target_os = "linux", target_arch = "x86_64")),
    allow(dead_code)
)]
pub(crate) struct AgentCredentialIntent {
    pub provider: String,
    pub verb: CredentialVerb,
    pub connection_id: Option<String>,
    pub shape: Option<String>,
    pub scopes: Vec<String>,
    pub expected_credential_revision: Option<i64>,
    pub expected_authorization_revision: Option<i64>,
}

/// The boundary input of one human attempt. Constructed only inside this
/// module from a claimed nonce; not `Clone`, not `Serialize`, no public
/// constructor.
pub(crate) struct VerifiedHumanAttemptV1 {
    request_id: String,
    attempt_id: String,
    verb: String,
    provider: String,
    connection_id: Option<String>,
    target_id: String,
    shape: Option<String>,
    scopes: Vec<String>,
    expected_credential_revision: Option<i64>,
    expected_authorization_revision: Option<i64>,
    checked_context: EntryContextV1,
    verification_request_id: Option<String>,
    entry_session_binding: String,
}

impl VerifiedHumanAttemptV1 {
    pub(crate) fn request_id(&self) -> &str {
        &self.request_id
    }
    pub(crate) fn attempt_id(&self) -> &str {
        &self.attempt_id
    }
    pub(crate) fn verb(&self) -> &str {
        &self.verb
    }
    pub(crate) fn provider(&self) -> &str {
        &self.provider
    }
    pub(crate) fn connection_id(&self) -> Option<&str> {
        self.connection_id.as_deref()
    }
    /// The opaque target account for a fresh enrollment; the connection id
    /// for rotate/verify.
    pub(crate) fn target_id(&self) -> &str {
        &self.target_id
    }
    pub(crate) fn shape(&self) -> Option<&str> {
        self.shape.as_deref()
    }
    pub(crate) fn scopes(&self) -> &[String] {
        &self.scopes
    }
    pub(crate) fn expected_credential_revision(&self) -> Option<i64> {
        self.expected_credential_revision
    }
    pub(crate) fn expected_authorization_revision(&self) -> Option<i64> {
        self.expected_authorization_revision
    }
    /// A claimed nonce cannot be rebound to a restarted owner or restored
    /// database by the parent's subsequent live-evidence read.
    pub(crate) fn require_management_context(
        &self,
        workspace_id: &str,
        custody_domain: &str,
        database_epoch: u64,
        owner_epoch: u64,
        supervisor_generation: &str,
    ) -> Result<()> {
        let checked = &self.checked_context;
        if checked.workspace_id != workspace_id
            || checked.custody_domain != custody_domain
            || u64::try_from(checked.custody_epoch).ok() != Some(database_epoch)
            || u64::try_from(checked.owner_epoch).ok() != Some(owner_epoch)
            || checked.supervisor_generation != supervisor_generation
        {
            return Err(Error::invalid(
                "request_changed",
                "the claimed credential attempt belongs to a different protected context",
            ));
        }
        Ok(())
    }
    /// The stable server verification ID of a `verify` request.
    pub(crate) fn verification_request_id(&self) -> Option<&str> {
        self.verification_request_id.as_deref()
    }
    /// The digest of the live authenticated owner session the nonce was
    /// bound to (never the session token/id itself).
    pub(crate) fn entry_session_binding(&self) -> &str {
        &self.entry_session_binding
    }
}

/// Which credential grammar the trusted form submitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SecretKind {
    SmtpPassword,
    OpenRouterApiKey,
    /// The bounded native-model source API key (never a business Connection).
    NativeProviderApiKey,
    /// The bounded native-model Devin credential file bytes.
    NativeProviderDevinCredentials,
    /// A verify request carries no credential.
    None,
}

/// The SMTP transport/sender the human typed on the isolated form. It is
/// not secret, but it must not be persisted before the secret is screened
/// with it, so it travels only inside [`BoundedSecretInputV1`].
pub(crate) struct PrivateSmtpTransportV1 {
    host: String,
    port: u16,
    tls_mode: String,
    username: String,
    sender: String,
    sender_name: String,
}

impl PrivateSmtpTransportV1 {
    pub(crate) fn host(&self) -> &str {
        &self.host
    }
    pub(crate) fn port(&self) -> u16 {
        self.port
    }
    pub(crate) fn tls_mode(&self) -> &str {
        &self.tls_mode
    }
    pub(crate) fn username(&self) -> &str {
        &self.username
    }
    pub(crate) fn sender(&self) -> &str {
        &self.sender
    }
    pub(crate) fn sender_name(&self) -> &str {
        &self.sender_name
    }
}

/// The one bounded submission crossing from the trusted form into the
/// daemon's provider mutation. The secret is zeroized on drop; no `Clone`,
/// no `Debug`, no `Serialize`.
pub(crate) struct BoundedSecretInputV1 {
    kind: SecretKind,
    bytes: zeroize::Zeroizing<Vec<u8>>,
    transport: Option<PrivateSmtpTransportV1>,
}

impl BoundedSecretInputV1 {
    fn smtp(bytes: Vec<u8>, transport: Option<PrivateSmtpTransportV1>) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_SECRET_BYTES {
            return Err(Error::invalid(
                "invalid_request",
                "the credential exceeds its supported size",
            ));
        }
        Ok(Self {
            kind: SecretKind::SmtpPassword,
            bytes: zeroize::Zeroizing::new(bytes),
            transport,
        })
    }
    fn token(bytes: Vec<u8>) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > 8192 {
            return Err(Error::invalid(
                "invalid_request",
                "the credential exceeds its supported size",
            ));
        }
        Ok(Self {
            kind: SecretKind::OpenRouterApiKey,
            bytes: zeroize::Zeroizing::new(bytes),
            transport: None,
        })
    }
    /// The native-model API-key source. Bounded exactly like the provider key
    /// grammar; it never becomes a business Connection or vault row.
    pub(crate) fn native_api_key(bytes: Vec<u8>) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > 8192 {
            return Err(Error::invalid(
                "invalid_request",
                "the credential exceeds its supported size",
            ));
        }
        Ok(Self {
            kind: SecretKind::NativeProviderApiKey,
            bytes: zeroize::Zeroizing::new(bytes),
            transport: None,
        })
    }
    /// The native-model Devin credential-file bytes.
    pub(crate) fn native_credentials(bytes: Vec<u8>) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > 65_536 {
            return Err(Error::invalid(
                "invalid_request",
                "the credential exceeds its supported size",
            ));
        }
        Ok(Self {
            kind: SecretKind::NativeProviderDevinCredentials,
            bytes: zeroize::Zeroizing::new(bytes),
            transport: None,
        })
    }
    fn absent() -> Self {
        Self {
            kind: SecretKind::None,
            bytes: zeroize::Zeroizing::new(Vec::new()),
            transport: None,
        }
    }
    pub(crate) fn kind(&self) -> SecretKind {
        self.kind
    }
    /// The exact submitted bytes, for the consuming provider mutation.
    pub(crate) fn expose(&self) -> &[u8] {
        &self.bytes
    }
    pub(crate) fn transport(&self) -> Option<&PrivateSmtpTransportV1> {
        self.transport.as_ref()
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// Validate the ordinary operator session/page-key proof used by protected
/// management metadata (the board's `/settings/connections`). Management is
/// not secret collection, so it keeps the existing board-origin proof and
/// rejects only the public surface. Credential collection uses the strict
/// typed entry binding below.
pub(crate) fn management_session_binding(shared: &Shared, proof: &Value) -> Result<String> {
    let object = proof
        .as_object()
        .ok_or_else(|| Error::invalid("entry_session_required", "entry session is unavailable"))?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "token" | "key" | "origin"))
    {
        return Err(Error::invalid(
            "entry_session_required",
            "entry session is unavailable",
        ));
    }
    let token = required_str(proof, "token")?;
    let key = required_str(proof, "key")?;
    let origin = crate::operator_auth::Origin::parse(required_str(proof, "origin")?)
        .ok_or_else(|| Error::invalid("entry_session_required", "entry session is unavailable"))?;
    if origin == crate::operator_auth::Origin::Public {
        return Err(Error::invalid(
            "entry_session_required",
            "management is the operator's own browser session",
        ));
    }
    if token.is_empty() || key.is_empty() {
        return Err(Error::invalid(
            "entry_session_required",
            "entry session is unavailable",
        ));
    }
    let session = shared
        .operator_auth()
        .verified_session(token, key, origin, shared.operator_now())?
        .ok_or_else(|| Error::invalid("entry_session_required", "entry session is unavailable"))?;
    if session
        .user
        .as_ref()
        .is_some_and(|user| !user.is_operator())
    {
        return Err(Error::invalid(
            "entry_session_required",
            "management is the operator's own browser session",
        ));
    }
    Ok(crate::operator_auth::digest(&format!(
        "cadence-credential-entry-session-v1\0{}\0{}",
        origin.as_str(),
        session.id
    )))
}

/// Validate the live typed credential-entry session/page-key proof and derive
/// the nonce binding from it. The browser never chooses this value, and the
/// session must have been issued ON the root-pinned entry origin (not an
/// ordinary board/loopback session with an extra caller string).
pub(crate) fn entry_session_binding(shared: &Shared, proof: &Value) -> Result<String> {
    let entry_origin = shared.protected_entry_origin().ok_or_else(|| {
        Error::invalid(
            "entry_session_required",
            "this deployment has no separate trusted credential origin",
        )
    })?;
    let object = proof
        .as_object()
        .ok_or_else(|| Error::invalid("entry_session_required", "entry session is unavailable"))?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "token" | "key" | "origin" | "entry_origin"))
    {
        return Err(Error::invalid(
            "entry_session_required",
            "entry session is unavailable",
        ));
    }
    let token = required_str(proof, "token")?;
    let key = required_str(proof, "key")?;
    let origin = crate::operator_auth::Origin::parse(required_str(proof, "origin")?);
    if origin != Some(crate::operator_auth::Origin::Loopback) {
        return Err(Error::invalid(
            "entry_session_required",
            "credential entry is the operator's own entry-origin session",
        ));
    }
    let claimed_origin = required_str(proof, "entry_origin")?;
    if claimed_origin != entry_origin {
        return Err(Error::invalid(
            "entry_session_required",
            "credential entry is the operator's own entry-origin session",
        ));
    }
    if token.is_empty() || key.is_empty() {
        return Err(Error::invalid(
            "entry_session_required",
            "entry session is unavailable",
        ));
    }
    let session = shared
        .verify_entry_session(token, key, entry_origin, shared.operator_now())?
        .ok_or_else(|| {
            Error::invalid(
                "entry_session_required",
                "credential entry is the operator's own entry-origin session",
            )
        })?;
    if session
        .user
        .as_ref()
        .is_some_and(|user| !user.is_operator())
    {
        return Err(Error::invalid(
            "entry_session_required",
            "credential entry is the operator's own entry-origin session",
        ));
    }
    // Bind the LIVE profile origin, never the proof's copy of it (they were
    // required equal above, but the server value is the authority).
    Ok(crate::operator_auth::digest(&format!(
        "cadence-credential-entry-session-v1\0{}\0{}",
        entry_origin, session.id
    )))
}

fn strict_string(params: &Value, field: &str) -> Result<Option<String>> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(Error::invalid(
            "invalid_request",
            format!("'{field}' must be a string"),
        )),
    }
}

fn strict_revision(params: &Value, field: &str) -> Result<Option<i64>> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_i64()
            .filter(|value| *value > 0)
            .map(Some)
            .ok_or_else(|| Error::invalid("invalid_request", format!("'{field}' is invalid"))),
        Some(_) => Err(Error::invalid(
            "invalid_request",
            format!("'{field}' must be a positive integer"),
        )),
    }
}

fn strict_scopes(params: &Value) -> Result<Vec<String>> {
    match params.get("scopes") {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str().map(str::to_owned).ok_or_else(|| {
                    Error::invalid("invalid_request", "'scopes' items must be strings")
                })
            })
            .collect(),
        Some(_) => Err(Error::invalid(
            "invalid_request",
            "'scopes' must be an array",
        )),
    }
}

/// Resolve the live protected context for one target. A parent refusal is
/// returned verbatim as `request_not_ready`; nothing is defaulted.
fn entry_context(shared: &Shared, target: &EntryTargetV1) -> Result<(EntryContextV1, String)> {
    // A parent refusal without a closed code is still a readiness refusal:
    // never reclassify it as caller error or invent a context.
    let context = shared.protected_entry_context(target).map_err(|error| {
        if error.code().is_some() {
            error
        } else {
            Error::invalid("request_not_ready", NOT_READY)
        }
    })?;
    let digest = context_digest(&context)?;
    Ok((context, digest))
}

fn target_of_request(request: &CredentialRequest) -> EntryTargetV1 {
    EntryTargetV1 {
        agent_alias: None,
        participant_uid: request.requester_participant.clone(),
    }
}

/// Resolve one genuine pending credential request for the local entry-login
/// prepare step: canonical id, current owning workspace/profile context and
/// an unexpired pending state. Read-only; never mints or mutates a request.
pub(crate) fn pending_request_for_login(
    shared: &Shared,
    request_id: &str,
    now_ms: i64,
) -> Result<CredentialRequest> {
    let canonical =
        cadence_core::connection_dispatch::primitives::UuidV1::try_from(request_id.to_string())
            .map_err(|_| Error::invalid("invalid_request", "the request id is invalid"))?;
    let request = shared
        .store
        .credential_request(canonical.as_str())?
        .ok_or_else(|| Error::invalid("request_changed", "credential request is unavailable"))?;
    if request.request_id.as_str() != canonical.as_str()
        || request.state != "pending"
        || request.expires_at_ms <= now_ms
    {
        return Err(Error::invalid(
            "request_changed",
            "the credential request is not pending",
        ));
    }
    let (context, digest) = entry_context(shared, &target_of_request(&request))?;
    if context.custody_epoch != request.custody_epoch || digest != request.context_digest {
        return Err(Error::invalid(
            "request_changed",
            "the protected context changed; start a new request",
        ));
    }
    Ok(request)
}

/// Build the request from operator RPC parameters. Target identity, the
/// protected context and the opaque account handle are resolved *here* —
/// never by the browser or CLI.
fn request_from_params(shared: &Shared, params: &Value) -> Result<NewCredentialRequest> {
    let provider = crate::platform::connections::provider_identifier(
        required_str(params, "provider")?,
        "Provider",
    )?;
    let verb = CredentialVerb::parse(required_str(params, "verb")?)?;
    let connection_id = strict_string(params, "connection_id")?;
    let mut shape = strict_string(params, "shape")?;
    let expected_credential_revision = strict_revision(params, "expected_credential_revision")?;
    let expected_authorization_revision =
        strict_revision(params, "expected_authorization_revision")?;
    let agent_alias = strict_string(params, "agent")?;
    if let Some(alias) = agent_alias.as_deref() {
        crate::proto::identifier(alias, "Agent")?;
    }
    let target_id_override = strict_string(params, "target_id")?;
    let target = EntryTargetV1 {
        agent_alias,
        participant_uid: None,
    };
    let (context, context_digest) = entry_context(shared, &target)?;
    if is_native_model_shape(shape.as_deref()) {
        // The native-model source is a broker-private encrypted source, not a
        // business Connection. It has no account/transport/descriptor.
        if provider != "devin" {
            return Err(Error::invalid(
                "invalid_request",
                "this provider does not enroll that shape",
            ));
        }
    } else {
        match verb {
            CredentialVerb::Enroll => {
                let shape = shape.as_deref().ok_or_else(|| {
                    Error::invalid("invalid_request", "an enrollment request needs its shape")
                })?;
                let descriptor = shared.connection_descriptor(&provider)?;
                if !descriptor.enrollment_shapes.iter().any(|s| s == shape) {
                    return Err(Error::invalid(
                        "invalid_request",
                        "this provider does not enroll that shape",
                    ));
                }
            }
            CredentialVerb::Rotate | CredentialVerb::Verify => {
                let id = connection_id.as_deref().ok_or_else(|| {
                    Error::invalid("invalid_request", "this request needs a connection")
                })?;
                let record = shared.store.connection_credential(id)?.ok_or_else(|| {
                    Error::invalid("request_changed", "connection is unavailable or stale")
                })?;
                if record.platform != provider {
                    return Err(Error::invalid(
                        "request_changed",
                        "the connection provider does not match this request",
                    ));
                }
                shape = Some(record.exchange.clone());
                if verb == CredentialVerb::Verify
                    && record.exchange != crate::platform::smtp::ENROLLMENT_SHAPE
                {
                    return Err(Error::invalid(
                        "unsupported_verification",
                        "this provider has no reviewed verification adapter",
                    ));
                }
            }
        }
    }
    Ok(NewCredentialRequest {
        provider,
        verb,
        connection_id,
        shape,
        scopes: strict_scopes(params)?,
        expected_credential_revision,
        expected_authorization_revision,
        requester_kind: "operator_cli".to_string(),
        requester_participant: None,
        conversation_id: None,
        target_agent_uid: context.target_agent_uid.clone(),
        custody_domain: context.custody_domain,
        custody_epoch: context.custody_epoch,
        context_digest,
        target_id_override,
    })
}

/// The SMTP transport/sender object the isolated form submits with the
/// password. Every field is validated against the same grammar the
/// enrollment path uses; nothing here is persisted by this module.
fn private_transport(params: &Value) -> Result<PrivateSmtpTransportV1> {
    let transport = params.get("transport").ok_or_else(|| {
        Error::invalid("invalid_request", "the SMTP transport fields are missing")
    })?;
    let object = transport.as_object().ok_or_else(|| {
        Error::invalid("invalid_request", "the SMTP transport fields are missing")
    })?;
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "host" | "port" | "tls_mode" | "username" | "sender" | "sender_name"
        )
    }) {
        return Err(Error::invalid(
            "invalid_request",
            "unsupported SMTP transport field",
        ));
    }
    let host = crate::platform::smtp::validate_host(required_str(transport, "host")?)?;
    let port = transport
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .ok_or_else(|| Error::invalid("invalid_request", "the SMTP port is invalid"))?;
    let tls_mode = required_str(transport, "tls_mode")?.to_string();
    crate::platform::smtp::validate_port_tls(&host, port, &tls_mode)?;
    let username = required_str(transport, "username")?.to_string();
    if username.is_empty()
        || username.len() > 320
        || username
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
        || username.contains(['<', '>', '"', '\'', '`', '\\'])
    {
        return Err(Error::invalid(
            "invalid_request",
            "the SMTP username exceeds its supported shape",
        ));
    }
    let sender = required_str(transport, "sender")?.to_string();
    crate::platform::smtp::validate_sender(&sender)?;
    let sender_name = strict_string(transport, "sender_name")?.unwrap_or_default();
    if sender_name.len() > 160
        || (!sender_name.is_empty() && sender_name.trim() != sender_name)
        || sender_name.chars().any(char::is_control)
        || sender_name.contains(['<', '>', '\r', '\n', '"', '`', '\\'])
        || !sender_name.is_ascii()
    {
        return Err(Error::invalid(
            "invalid_request",
            "the SMTP sender name exceeds its supported shape",
        ));
    }
    Ok(PrivateSmtpTransportV1 {
        host,
        port,
        tls_mode,
        username,
        sender,
        sender_name,
    })
}

/// A rotation password is screened for bounds here; the provider adapter's
/// own grammar still validates it before any custody write. A rotation
/// intentionally carries no transport: the trusted path must inherit the
/// pinned transport from the existing encrypted record instead of accepting
/// a new destination from the form.
fn smtp_password(secret: String) -> Result<BoundedSecretInputV1> {
    if secret.is_empty() || secret.len() > MAX_SECRET_BYTES || secret.chars().any(char::is_control)
    {
        return Err(Error::invalid(
            "invalid_request",
            "the credential exceeds its supported shape",
        ));
    }
    BoundedSecretInputV1::smtp(secret.into_bytes(), None)
}

fn token_secret(secret: String) -> Result<BoundedSecretInputV1> {
    if secret.is_empty()
        || secret.len() > 8192
        || secret
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
        || !secret.is_ascii()
    {
        return Err(Error::invalid(
            "invalid_request",
            "the credential exceeds its supported shape",
        ));
    }
    BoundedSecretInputV1::token(secret.into_bytes())
}

/// Read-only reconciliation: the stored metadata plus the *current*
/// protected-context readiness. A locked key, an advanced epoch or an
/// owner outage is reported as a blocker — never as a success, a decryption
/// or a new collection. This is what lets a lost enrollment acknowledgement
/// be reconciled while custody is locked.
fn readiness_projection(shared: &Shared, request: &CredentialRequest) -> Value {
    match shared.protected_entry_context(&target_of_request(request)) {
        Ok(context) => {
            let digest = context_digest(&context).unwrap_or_default();
            let current =
                context.custody_epoch == request.custody_epoch && digest == request.context_digest;
            json!({
                "ready": current,
                "blocker": if current { Value::Null } else { json!("request_changed") },
                "custody_epoch": context.custody_epoch,
                "context_current": current,
            })
        }
        Err(error) => json!({
            "ready": false,
            "blocker": error.code().unwrap_or("request_not_ready"),
            "custody_epoch": Value::Null,
            "context_current": false,
        }),
    }
}

fn request_response(shared: &Shared, request: &CredentialRequest, now_ms: i64) -> Value {
    let mut row = request.to_json(now_ms);
    row["readiness"] = readiness_projection(shared, request);
    json!({"request": row})
}

fn reveal_nonce(shared: &Shared, request_id: &str, proof: &Value, now_ms: i64) -> Result<Value> {
    let binding = entry_session_binding(shared, proof)?;
    let request = shared
        .store
        .credential_request(request_id)?
        .ok_or_else(|| Error::invalid("request_changed", "credential request is unavailable"))?;
    let (context, context_digest) = entry_context(shared, &target_of_request(&request))?;
    let (nonce, request) = shared.store.issue_credential_nonce(
        request_id,
        &binding,
        context.custody_epoch,
        &context_digest,
        now_ms,
    )?;
    let expires_in = (request.expires_at_ms - now_ms).max(0);
    Ok(json!({
        "request": request.to_json(now_ms),
        "nonce": nonce,
        "nonce_expires_in_ms": NONCE_TTL_MS.min(expires_in),
    }))
}

/// The trusted form's single submit. The order is the contract's:
/// live context/readiness, strict body grammar, atomic nonce claim, then —
/// and only then — the parent mutation.
fn submit(
    shared: &Arc<Shared>,
    params: &Value,
    now_ms: i64,
    apply: impl FnOnce(&VerifiedHumanAttemptV1, &BoundedSecretInputV1) -> Result<Value>,
) -> Result<Value> {
    let proof = params
        .get("session")
        .ok_or_else(|| Error::invalid("entry_session_required", "entry session is unavailable"))?;
    let session_binding = entry_session_binding(shared, proof)?;
    let request_id = required_str(params, "request_id")?.to_owned();
    let nonce = required_str(params, "nonce")?.to_owned();
    let secret = strict_string(params, "secret")?;
    let peek = shared
        .store
        .credential_request(&request_id)?
        .ok_or_else(|| Error::invalid("request_changed", "credential request is unavailable"))?;
    let (context, context_digest) = entry_context(shared, &target_of_request(&peek))?;
    if context.custody_epoch != peek.custody_epoch || context_digest != peek.context_digest {
        return Err(Error::invalid(
            "request_changed",
            "the protected context changed; start a new request",
        ));
    }
    let submission = match (peek.verb.as_str(), peek.shape.as_deref(), secret) {
        ("verify", _, None) => BoundedSecretInputV1::absent(),
        ("verify", _, Some(_)) => {
            return Err(Error::invalid(
                "invalid_request",
                "verification carries no credential",
            ))
        }
        // The native-model source: an API key or the exact credential-file
        // bytes. Neither becomes a business Connection.
        (_, Some("native-model-api-key"), Some(secret)) => {
            if secret.is_empty()
                || secret.len() > 8192
                || secret
                    .chars()
                    .any(|ch| ch.is_control() || ch.is_whitespace())
                || !secret.is_ascii()
            {
                return Err(Error::invalid(
                    "invalid_request",
                    "the credential exceeds its supported shape",
                ));
            }
            BoundedSecretInputV1::native_api_key(secret.into_bytes())?
        }
        (_, Some("native-model-devin-credentials"), Some(secret)) => {
            BoundedSecretInputV1::native_credentials(secret.into_bytes())?
        }
        (_, shape, None) if is_native_model_shape(shape) => {
            return Err(Error::invalid(
                "invalid_request",
                "the credential is missing",
            ))
        }
        ("enroll", Some("smtp"), Some(secret)) => {
            // The only path where human-typed transport first exists:
            // validate it with the secret, then hand both privately to the
            // parent. Nothing is written to the request rows.
            let transport = private_transport(params)?;
            let probe = json!({
                "host": transport.host(),
                "port": transport.port(),
                "tls_mode": transport.tls_mode(),
                "username": transport.username(),
                "sender": transport.sender(),
                "sender_name": transport.sender_name(),
                "secret": secret,
            });
            let parsed = crate::platform::smtp::parse_enrollment(&probe)?;
            let transport = PrivateSmtpTransportV1 {
                host: parsed.host,
                port: parsed.port,
                tls_mode: parsed.tls_mode,
                username: parsed.username,
                sender: parsed.sender,
                sender_name: parsed.sender_name,
            };
            BoundedSecretInputV1::smtp(parsed.secret, Some(transport))?
        }
        (_, Some("smtp"), Some(secret)) => smtp_password(secret)?,
        (_, Some("smtp"), None) => {
            return Err(Error::invalid(
                "invalid_request",
                "the credential is missing",
            ))
        }
        (_, _, Some(secret)) => token_secret(secret)?,
        (_, _, None) => {
            return Err(Error::invalid(
                "invalid_request",
                "the credential is missing",
            ))
        }
    };
    let claimed = shared.store.claim_credential_attempt(
        &request_id,
        &nonce,
        &session_binding,
        context.custody_epoch,
        &context_digest,
        now_ms,
    )?;
    let attempt = VerifiedHumanAttemptV1 {
        request_id: claimed.request.request_id.clone(),
        attempt_id: claimed.attempt_id.clone(),
        verb: claimed.request.verb.clone(),
        provider: claimed.request.provider.clone(),
        connection_id: claimed.request.connection_id.clone(),
        target_id: claimed.request.target_id.clone(),
        shape: claimed.request.shape.clone(),
        scopes: claimed.request.scopes.clone(),
        expected_credential_revision: claimed.request.expected_credential_revision,
        expected_authorization_revision: claimed.request.expected_authorization_revision,
        checked_context: context,
        verification_request_id: claimed.request.verification_request_id.clone(),
        entry_session_binding: session_binding,
    };
    let result = apply(&attempt, &submission);
    let outcome = match &result {
        Ok(value) => {
            let rendered = value.to_string();
            // The submitted secret is still owned here: screen the parent's
            // receipt before it can leave the daemon.
            crate::platform::refuse_leak(
                "credential entry receipt",
                &rendered,
                submission.expose(),
            )?;
            AttemptOutcome::Applied
        }
        Err(Error::OutcomeUnknown(_)) => AttemptOutcome::Unknown,
        Err(error) if error.code() == Some("outcome_unknown") => AttemptOutcome::Unknown,
        Err(_) => AttemptOutcome::Refused,
    };
    let code = match (&result, outcome) {
        (Err(error), AttemptOutcome::Refused) => Some(error_code(error)),
        (Err(_), AttemptOutcome::Unknown) => Some("outcome_unknown"),
        _ => None,
    };
    if let Err(error) = shared.store.finish_credential_attempt(
        &request_id,
        &attempt.attempt_id,
        outcome,
        code,
        result
            .as_ref()
            .ok()
            .and_then(|value| value.get("connection_id"))
            .and_then(Value::as_str),
        now_ms,
    ) {
        return Err(Error::OutcomeUnknown(format!(
            "credential entry outcome is unknown: {error}"
        )));
    }
    if let Err(error) = result {
        if outcome == AttemptOutcome::Unknown {
            return Err(error);
        }
        return Err(Error::invalid(
            code.unwrap_or("refused"),
            "the credential was refused; nothing more was changed",
        ));
    }
    let request = shared
        .store
        .credential_request(&request_id)?
        .ok_or_else(|| Error::internal("credential request did not persist"))?;
    Ok(json!({"request": request.to_json(now_ms), "status": request.status}))
}

fn error_code(error: &Error) -> &'static str {
    match error.code() {
        Some("invalid_request") => "invalid_request",
        Some("request_changed") => "request_changed",
        Some("already_submitted") => "already_submitted",
        Some("nonce_expired") => "nonce_expired",
        Some("entry_session_required") => "entry_session_required",
        Some("request_not_ready") => "request_not_ready",
        Some("unsupported_verification") => "unsupported_verification",
        Some("outcome_unknown") => "outcome_unknown",
        Some("stale_revision") => "request_changed",
        _ => "refused",
    }
}

/// A trusted parent path (the chat producer) creates a request for an
/// authenticated agent participant. The participant UID and conversation
/// come from the authenticated server context, never from model arguments;
/// model text can at most ask the parent to call this.
#[cfg_attr(
    not(all(target_os = "linux", target_arch = "x86_64")),
    allow(dead_code)
)]
pub(crate) fn create_agent_credential_request(
    shared: &Shared,
    participant_uid: &str,
    conversation_id: &str,
    intent: AgentCredentialIntent,
    now_ms: i64,
) -> Result<CredentialRequest> {
    if !participant_uid.starts_with("agt-") || participant_uid.len() > 128 {
        return Err(Error::invalid(
            "invalid_request",
            "agent participant identity is invalid",
        ));
    }
    if conversation_id.is_empty() || conversation_id.len() > 128 {
        return Err(Error::invalid(
            "invalid_request",
            "conversation identity is invalid",
        ));
    }
    let target = EntryTargetV1 {
        agent_alias: None,
        participant_uid: Some(participant_uid.to_string()),
    };
    let (context, context_digest) = entry_context(shared, &target)?;
    if context.target_agent_uid.as_deref() != Some(participant_uid) {
        return Err(Error::invalid(
            "request_not_ready",
            "the request target is not the authenticated participant",
        ));
    }
    shared.store.create_credential_request(
        NewCredentialRequest {
            provider: intent.provider,
            verb: intent.verb,
            connection_id: intent.connection_id,
            shape: intent.shape,
            scopes: intent.scopes,
            expected_credential_revision: intent.expected_credential_revision,
            expected_authorization_revision: intent.expected_authorization_revision,
            requester_kind: "agent".to_string(),
            requester_participant: Some(participant_uid.to_string()),
            conversation_id: Some(conversation_id.to_string()),
            target_agent_uid: context.target_agent_uid,
            custody_domain: context.custody_domain,
            custody_epoch: context.custody_epoch,
            context_digest,
            target_id_override: None,
        },
        now_ms,
    )
}

/// The exact methods the daemon routes here. Every one runs the proven
/// operator gate and a strict parameter allowlist first.
pub(crate) fn rpc_credential_entry(
    shared: &Arc<Shared>,
    method: &str,
    params: &Value,
    peer_pid: u32,
    apply: impl FnOnce(&VerifiedHumanAttemptV1, &BoundedSecretInputV1) -> Result<Value>,
) -> Result<Value> {
    shared.operator_connection("credential entry management", params, peer_pid)?;
    let allowed: &[&str] = match method {
        "credential_request_create" => &[
            "provider",
            "verb",
            "connection_id",
            "shape",
            "scopes",
            "expected_credential_revision",
            "expected_authorization_revision",
            "agent",
            "target_id",
        ],
        "credential_request_show" | "credential_request_status" => &["request_id"],
        "credential_request_cancel" => &["request_id"],
        "credential_request_list" => &["conversation_id", "limit"],
        "credential_entry_session" => &["request_id", "session"],
        "credential_request_submit" => &["request_id", "nonce", "session", "secret", "transport"],
        _ => return Err(Error::rejected("unknown credential entry operation")),
    };
    let object = params.as_object().ok_or_else(|| {
        Error::invalid(
            "invalid_request",
            "credential entry parameters must be an object",
        )
    })?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(Error::invalid(
            "invalid_request",
            "unsupported credential entry parameter",
        ));
    }
    let now = now_ms();
    match method {
        "credential_request_create" => {
            let new = request_from_params(shared, params)?;
            let request = shared.store.create_credential_request(new, now)?;
            Ok(request_response(shared, &request, now))
        }
        "credential_request_show" | "credential_request_status" => {
            let request_id = required_str(params, "request_id")?;
            let request = shared
                .store
                .credential_request(request_id)?
                .ok_or_else(|| {
                    Error::invalid("request_changed", "credential request is unavailable")
                })?;
            Ok(request_response(shared, &request, now))
        }
        "credential_request_list" => {
            let limit = params.get("limit").and_then(Value::as_i64).unwrap_or(20);
            let rows = match optional_str(params, "conversation_id") {
                Some(conversation) => {
                    crate::proto::identifier(conversation, "Conversation")?;
                    shared
                        .store
                        .credential_requests_for_conversation(conversation, limit)?
                }
                None => shared.store.recent_credential_requests(limit)?,
            };
            let requests: Vec<Value> = rows
                .iter()
                .map(|row| {
                    let mut projected = row.to_json(now);
                    projected["readiness"] = readiness_projection(shared, row);
                    projected
                })
                .collect();
            Ok(json!({"requests": requests}))
        }
        "credential_entry_session" => {
            let request_id = required_str(params, "request_id")?;
            let proof = params.get("session").ok_or_else(|| {
                Error::invalid("entry_session_required", "entry session is unavailable")
            })?;
            reveal_nonce(shared, request_id, proof, now)
        }
        "credential_request_submit" => submit(shared, params, now, apply),
        "credential_request_cancel" => {
            let request_id = required_str(params, "request_id")?;
            let request = shared.store.cancel_credential_request(request_id, now)?;
            Ok(request_response(shared, &request, now))
        }
        _ => unreachable!(),
    }
}

impl Shared {
    /// The typed entry-session verifier shared by the entry RPC and the
    /// credential-collection binding. It re-reads the live root profile and
    /// requires the session row to carry the exact entry origin; a board or
    /// normal-loopback session is never entry authority.
    pub(crate) fn verify_entry_session(
        &self,
        token: &str,
        key: &str,
        entry_origin: &str,
        now: i64,
    ) -> Result<Option<crate::operator_auth::SessionView>> {
        if self.protected_entry_origin() != Some(entry_origin) {
            return Ok(None);
        }
        self.operator_auth()
            .check_entry(token, key, entry_origin, now)
    }
}

#[cfg(test)]
#[path = "credential_attempt_context_acceptance.rs"]
mod credential_attempt_context_acceptance;
