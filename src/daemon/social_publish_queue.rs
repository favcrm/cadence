//! CAD-1143 selector-only AOS queue-validation receipt verifier.
//!
//! This purpose is distinct from `social.intent.read.v1`: a read assertion
//! cannot authorize attachment. The only authority accepted here is the
//! configured AOS issuer's short-lived Ed25519 `social.queue-validation.v1`
//! receipt, checked against a locally re-proved prepared intent. The receipt
//! is an inspection result, never send approval; the existing device send
//! preflight still enforces the grant's `not_before` at execution time.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::platform::agenticos_external::publish::{
    QueueValidationRequest, QueueValidationResponse,
};

use super::{social_owner_intent, Shared};

const PURPOSE: &str = "social.queue-validation.v1";
const MAX_RECEIPT_BYTES: usize = 8 * 1024;
const MAX_RECEIPT_AGE_SECS: i64 = 15;
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    typ: String,
    kid: String,
}

/// Closed contract claims. `image_digest` and `media_key` are `Value` so a
/// missing field is rejected and explicit JSON null remains distinguishable
/// from an omitted field.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    iss: String,
    aud: String,
    purpose: String,
    workspace: String,
    key: String,
    action_id: String,
    grant_id: String,
    cadence_run_id: String,
    cadence_effect_id: String,
    intent_id: String,
    intent_digest: String,
    connection_id: String,
    destination_id: String,
    toolkit: String,
    caption_digest: String,
    image_digest: Value,
    media_key: Value,
    cadence_approval_id: String,
    due_epoch: i64,
    not_before_ms: i64,
    expires_at_ms: i64,
    grant_max_uses: i64,
    grant_remaining_uses: i64,
    iat: i64,
    exp: i64,
    jti: String,
}

struct ParsedReceipt {
    header: Header,
    claims: Claims,
    signing_input: String,
    signature: Vec<u8>,
}

/// Verified by the shared, read-only inspection path. This private value is
/// never returned by status; only the attach wrapper may consume its JTI and
/// create `VerifiedPublishGrant`.
struct InspectedQueueReceipt {
    parsed: ParsedReceipt,
    verified_at: i64,
    prepared_descriptor_digest: String,
    owner_intent_digest: String,
}

enum QueueInspectionError {
    Pending,
    Unknown,
    Refused(Error),
}

impl From<Error> for QueueInspectionError {
    fn from(error: Error) -> Self {
        Self::Refused(error)
    }
}

impl QueueInspectionError {
    fn into_error(self) -> Error {
        match self {
            Self::Pending => Error::rejected("the matching AOS owner action is still pending"),
            Self::Unknown => Error::rejected(
                "queue validation is uncertain or the owner action is absent; the intent remains prepared",
            ),
            Self::Refused(error) => error,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OwnerCompletionStatus {
    Ready,
    Pending,
    Unknown,
    Refused,
}

impl OwnerCompletionStatus {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Pending => "pending",
            Self::Unknown => "unknown",
            Self::Refused => "refused",
        }
    }
}

/// A queue-validation receipt after signature, purpose, currentness, replay,
/// and exact local-scope checks. Private fields ensure callers cannot turn a
/// grant id or a READ-purpose assertion into attachment authority.
pub(crate) struct VerifiedPublishGrant {
    prepared_id: String,
    prepared_descriptor_digest: String,
    owner_intent_digest: String,
    grant_id: String,
}

impl VerifiedPublishGrant {
    pub(crate) fn prepared_id(&self) -> &str {
        &self.prepared_id
    }

    pub(crate) fn prepared_descriptor_digest(&self) -> &str {
        &self.prepared_descriptor_digest
    }

    pub(crate) fn owner_intent_digest(&self) -> &str {
        &self.owner_intent_digest
    }

    pub(crate) fn grant_id(&self) -> &str {
        &self.grant_id
    }
}

fn invalid(message: &str) -> Error {
    Error::rejected(format!(
        "social queue-validation receipt invalid: {message}"
    ))
}

fn decode_part(part: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| invalid("compact JWS is not base64url"))
}

fn parse_receipt(token: &str) -> Result<ParsedReceipt> {
    if token.is_empty() || token.len() > MAX_RECEIPT_BYTES || !token.is_ascii() {
        return Err(invalid("compact JWS is empty, oversized, or non-ASCII"));
    }
    let mut parts = token.split('.');
    let (encoded_header, encoded_claims, encoded_signature) =
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(header), Some(claims), Some(signature), None)
                if !header.is_empty() && !claims.is_empty() && !signature.is_empty() =>
            {
                (header, claims, signature)
            }
            _ => return Err(invalid("compact JWS must have exactly three parts")),
        };
    let header: Header = serde_json::from_slice(&decode_part(encoded_header)?)
        .map_err(|_| invalid("JWS header is not the closed EdDSA shape"))?;
    if header.alg != "EdDSA"
        || header.typ != "JWT"
        || header.kid.is_empty()
        || header.kid.len() > 200
    {
        return Err(invalid("JWS header is not EdDSA JWT with a bounded kid"));
    }
    let claims: Claims = serde_json::from_slice(&decode_part(encoded_claims)?)
        .map_err(|_| invalid("JWS claims are not the closed queue-validation contract"))?;
    let jti = Uuid::parse_str(&claims.jti).map_err(|_| invalid("jti is not a UUID"))?;
    if jti.get_version_num() != 4 || jti.to_string() != claims.jti {
        return Err(invalid("jti is not a canonical lowercase UUIDv4"));
    }
    let signature = decode_part(encoded_signature)?;
    if signature.len() != 64 {
        return Err(invalid("JWS signature is not 64-byte Ed25519"));
    }
    Ok(ParsedReceipt {
        header,
        claims,
        signing_input: format!("{encoded_header}.{encoded_claims}"),
        signature,
    })
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| invalid("locally re-proved intent is missing a required string"))
}

fn required_epoch(value: &Value, field: &str) -> Result<i64> {
    value
        .get(field)
        .and_then(Value::as_i64)
        .filter(|number| *number >= 0 && *number <= MAX_SAFE_INTEGER)
        .ok_or_else(|| invalid("locally re-proved execution window is malformed"))
}

fn nullable_string(value: &Value) -> Result<Option<&str>> {
    match value {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text)),
        _ => Err(invalid("signed nullable selector has the wrong JSON type")),
    }
}

fn current_receipt_time(claims: &Claims, now: i64) -> bool {
    claims.iat >= 0
        && claims.iat <= MAX_SAFE_INTEGER
        && claims.exp >= 0
        && claims.exp <= MAX_SAFE_INTEGER
        && claims.iat <= now
        && claims.exp > now
        && claims.exp > claims.iat
        && claims.exp.saturating_sub(claims.iat) <= MAX_RECEIPT_AGE_SECS
}

fn safe_millis(seconds: i64) -> Result<i64> {
    seconds
        .checked_mul(1000)
        .filter(|value| *value <= MAX_SAFE_INTEGER)
        .ok_or_else(|| invalid("execution window exceeds safe milliseconds"))
}

fn require_local_binding(
    claims: &Claims,
    request: &QueueValidationRequest,
    owner_descriptor: &Value,
    prepared_descriptor: &Value,
    now: i64,
) -> Result<()> {
    let intent_digest = required_string(owner_descriptor, "intent_digest")?;
    let image_digest = nullable_string(
        owner_descriptor
            .get("image_digest")
            .ok_or_else(|| invalid("locally re-proved intent omits image_digest"))?,
    )?;
    let media_key = nullable_string(
        prepared_descriptor
            .get("media_key")
            .ok_or_else(|| invalid("prepared intent omits media_key"))?,
    )?;
    let due_epoch = required_epoch(owner_descriptor, "due_epoch")?;
    let not_before = required_epoch(owner_descriptor, "not_before")?;
    let expires_at = required_epoch(owner_descriptor, "expires_at")?;
    let due_ms = safe_millis(due_epoch)?;
    let not_before_ms = safe_millis(not_before)?;
    let expires_at_ms = safe_millis(expires_at)?;

    if claims.action_id.is_empty()
        || !social_owner_intent::valid_contract_id(&claims.action_id)
        || !crate::platform::agenticos_external::publish::valid_grant_id(&claims.grant_id)
        || !social_owner_intent::valid_contract_id(&claims.cadence_run_id)
        || !social_owner_intent::valid_contract_id(&claims.cadence_effect_id)
        || !social_owner_intent::valid_contract_id(&claims.intent_id)
        || !crate::platform::agenticos_external::publish::valid_connection_id(&claims.connection_id)
        || claims.destination_id.is_empty()
        || claims.destination_id.len() > 120
        || !crate::platform::agenticos_external::publish::valid_digest(&claims.caption_digest)
        || !crate::platform::agenticos_external::publish::valid_digest(&claims.intent_digest)
        || claims.cadence_approval_id.is_empty()
        || claims.cadence_approval_id.len() > 120
        || !matches!(claims.toolkit.as_str(), "instagram" | "facebook")
    {
        return Err(invalid("signed receipt contains a malformed bounded claim"));
    }

    let claim_image = nullable_string(&claims.image_digest)?;
    let claim_media = nullable_string(&claims.media_key)?;
    if claim_media
        .is_some_and(|key| !crate::platform::agenticos_external::publish::valid_media_key(key))
    {
        return Err(invalid("signed media selector is malformed"));
    }
    if claims.key != request.key
        || claims.cadence_run_id != request.cadence_run_id
        || claims.cadence_effect_id != request.cadence_effect_id
        || claims.intent_id != request.expected_intent_id
        || claims.intent_digest != request.expected_intent_digest
        || claims.intent_digest != intent_digest
        || claims.connection_id != request.connection_id
        || claims.destination_id != required_string(owner_descriptor, "destination_id")?
        || claims.toolkit != required_string(owner_descriptor, "toolkit")?
        || claims.caption_digest != required_string(owner_descriptor, "caption_digest")?
        || claim_image != image_digest
        || claim_media != media_key
        || claim_media != request.media_key.as_deref()
        || claims.cadence_approval_id != required_string(owner_descriptor, "cadence_approval_id")?
        || claims.due_epoch != due_epoch
        || claims.not_before_ms != not_before_ms
        || claims.expires_at_ms != expires_at_ms
        || claims.grant_max_uses != 1
        || claims.grant_remaining_uses != 1
        || claims.not_before_ms > due_ms
        || due_ms > claims.expires_at_ms
        || claims.expires_at_ms <= claims.not_before_ms
        || claims.expires_at_ms <= safe_millis(now)?
    {
        return Err(invalid(
            "signed receipt does not match the exact prepared scope/window",
        ));
    }
    if (claim_image.is_none()) != (claim_media.is_none()) {
        return Err(invalid("signed image and media selectors disagree"));
    }
    Ok(())
}

fn verify_signature(shared: &Shared, parsed: &ParsedReceipt, issuer: &str, now: i64) -> Result<()> {
    let key = {
        let mut cache = shared
            .board_jwks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.key(issuer, &parsed.header.kid, now)?
    };
    if parsed.header.kid != key.kid
        || UnparsedPublicKey::new(&ED25519, &key.x)
            .verify(parsed.signing_input.as_bytes(), &parsed.signature)
            .is_err()
    {
        return Err(invalid(
            "Ed25519 signature or configured kid does not verify",
        ));
    }
    Ok(())
}

impl Shared {
    /// Shared private read-only inspection: re-prove the current prepared
    /// scope and verify the existing signed queue-validation receipt without
    /// consuming its JTI or constructing attachment authority. This path
    /// never calls send preflight or send.
    fn inspect_prepared_queue(
        &self,
        prepared_id: &str,
        install_id: &str,
        context_id: Option<&str>,
        shown: &Value,
        allow_authorized_recovery: bool,
    ) -> std::result::Result<InspectedQueueReceipt, QueueInspectionError> {
        let prepared = &shown["prepared"];
        let state = prepared["state"].as_str();
        if state != Some("prepared") && !(allow_authorized_recovery && state == Some("authorized"))
        {
            return Err(Error::rejected(
                "only a prepared intent or authorized recovery can be inspected",
            )
            .into());
        }
        let frozen = &prepared["descriptor"];
        let prepared_digest = prepared["descriptor_digest"]
            .as_str()
            .filter(|digest| crate::platform::agenticos_external::publish::valid_digest(digest))
            .ok_or_else(|| invalid("prepared descriptor receipt is malformed"))?;
        let context_matches = match context_id {
            Some(expected) => frozen["context_id"].as_str() == Some(expected),
            None => frozen["context_id"].is_null(),
        };
        if frozen["install_id"].as_str() != Some(install_id) || !context_matches {
            return Err(invalid("prepared intent scope changed before queue validation").into());
        }
        let run_id = social_owner_intent::field_string(frozen, "run_id")?;
        let artifact_id = social_owner_intent::field_string(frozen, "artifact_id")?;
        let bundle_digest = social_owner_intent::field_string(frozen, "bundle_digest")?;
        let slot = social_owner_intent::field_string(frozen, "slot")?;
        let material =
            self.store
                .app_publication_material(run_id, artifact_id, bundle_digest, slot)?;
        let effect_id = social_owner_intent::field_string(frozen, "effect_id")?;
        let effect = self.store.app_effect_show(effect_id)?;
        social_owner_intent::reprove_staged_effect(frozen, &material, &effect)?;
        let owner_descriptor =
            social_owner_intent::owner_intent_descriptor(prepared_id, frozen, &material)?;
        let now = self.operator_now();
        if required_epoch(&owner_descriptor, "expires_at")? <= now {
            return Err(invalid("prepared owner intent has expired").into());
        }

        let install = social_owner_intent::field_string(frozen, "install_id")?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        let current_app_id = crate::issue::app_catalog::workspace::with_completed_bundle_snapshot(
            &pm,
            install,
            bundle_digest,
            |_, files| {
                let manifest = crate::issue::app::parse_manifest(
                    files
                        .get("app.md")
                        .ok_or_else(|| Error::rejected("installation manifest unavailable"))?,
                )?;
                Ok(manifest.app)
            },
        )?;
        if frozen["app_id"].as_str() != Some(current_app_id.as_str()) {
            return Err(Error::rejected(
                "prepared owner-intent app identity differs from the current bundle",
            )
            .into());
        }

        let connection_id = social_owner_intent::field_string(&owner_descriptor, "connection_id")?;
        let caption = material["artifact"]["text"]
            .as_str()
            .filter(|text| crate::platform::agenticos_external::publish::valid_caption(text))
            .ok_or_else(|| Error::rejected("approved publish material has no valid caption"))?;
        if crate::platform::agenticos_external::publish::caption_digest_of(caption)
            != social_owner_intent::field_string(&owner_descriptor, "caption_digest")?
        {
            return Err(Error::rejected(
                "approved caption bytes differ from the prepared owner intent",
            )
            .into());
        }
        let image_digest = frozen.get("image_digest").and_then(Value::as_str);
        let media_key = match frozen.get("media_key") {
            Some(Value::Null) => None,
            Some(Value::String(key)) => Some(key.as_str()),
            _ => return Err(invalid("prepared intent media receipt is malformed").into()),
        };
        match (image_digest, media_key) {
            (Some(digest), Some(key))
                if crate::platform::agenticos_external::publish::media_key_authorizes_connection(
                    key,
                    connection_id,
                    digest,
                ) => {}
            (None, None) => {}
            _ => {
                return Err(Error::rejected(
                    "prepared owner-intent media receipt does not match its AOS connection",
                ).into());
            }
        }

        let request_key = prepared["request"]
            .as_str()
            .filter(|key| crate::platform::agenticos_external::publish::valid_idempotency_key(key))
            .ok_or_else(|| invalid("prepared send key is malformed"))?;
        let selector = QueueValidationRequest {
            key: request_key.to_owned(),
            connection_id: connection_id.to_owned(),
            caption: caption.to_owned(),
            media_key: media_key.map(str::to_owned),
            cadence_run_id: social_owner_intent::field_string(&owner_descriptor, "run_id")?
                .to_owned(),
            cadence_effect_id: social_owner_intent::field_string(&owner_descriptor, "effect_id")?
                .to_owned(),
            expected_intent_id: prepared_id.to_owned(),
            expected_intent_digest: social_owner_intent::field_string(
                &owner_descriptor,
                "intent_digest",
            )?
            .to_owned(),
        };
        selector
            .validate()
            .map_err(|refusal| Error::rejected(refusal.to_string()))?;

        let config = crate::board_identity::read_config(&self.state_dir)?;
        let sender = self.social_publish_sender.as_ref().ok_or_else(|| {
            Error::rejected("capability_unavailable: no AOS publish sender is configured")
        })?;
        let response: QueueValidationResponse = match sender.inspect_queue(&selector) {
            Ok(response) => response,
            Err(refusal) => match refusal.code.as_str() {
                // AOS returns this only after the exact selector, content,
                // destination, expiry, and runtime-epoch guards passed.
                "owner_action_pending" => return Err(QueueInspectionError::Pending),
                // Absence and bounded transport ambiguity are retryable
                // observations, never authority. Every other AOS refusal is
                // definite and fails closed.
                "owner_action_not_found" | "queue_validation_uncertain" => {
                    return Err(QueueInspectionError::Unknown);
                }
                _ => {
                    return Err(QueueInspectionError::Refused(Error::rejected(
                        refusal.to_string(),
                    )))
                }
            },
        };
        if response.version != PURPOSE {
            return Err(
                invalid("response version or purpose differs from the queue contract").into(),
            );
        }

        let parsed = parse_receipt(&response.receipt)?;
        let claims = &parsed.claims;
        if claims.iss != config.issuer
            || claims.aud != config.host
            || claims.workspace != config.company
            || claims.purpose != PURPOSE
        {
            return Err(invalid("issuer, audience, workspace, or purpose mismatch").into());
        }
        let now = self.operator_now();
        if !current_receipt_time(claims, now) {
            return Err(invalid("iat/exp are outside the 15-second receipt window").into());
        }
        require_local_binding(claims, &selector, &owner_descriptor, frozen, now)?;
        verify_signature(self, &parsed, &config.issuer, now)?;

        Ok(InspectedQueueReceipt {
            parsed,
            verified_at: now,
            prepared_descriptor_digest: prepared_digest.to_owned(),
            owner_intent_digest: selector.expected_intent_digest,
        })
    }

    /// Advisory status calls only the shared read-only verifier. The verified
    /// receipt and JTI are dropped here and can never be passed to attach.
    pub(super) fn prepared_owner_completion_status(
        &self,
        prepared_id: &str,
        install_id: &str,
        context_id: Option<&str>,
        shown: &Value,
    ) -> OwnerCompletionStatus {
        match self.inspect_prepared_queue(prepared_id, install_id, context_id, shown, false) {
            Ok(_inspection) => OwnerCompletionStatus::Ready,
            Err(QueueInspectionError::Pending) => OwnerCompletionStatus::Pending,
            Err(QueueInspectionError::Unknown) => OwnerCompletionStatus::Unknown,
            Err(QueueInspectionError::Refused(_)) => OwnerCompletionStatus::Refused,
        }
    }

    /// Attach is the only caller that consumes a verified receipt JTI and
    /// constructs the private capability. Status never calls this wrapper.
    pub(super) fn verify_prepared_queue_grant(
        &self,
        prepared_id: &str,
        install_id: &str,
        context_id: Option<&str>,
        shown: &Value,
    ) -> Result<VerifiedPublishGrant> {
        self.verify_queue_grant(prepared_id, install_id, context_id, shown, false)
    }

    /// Authorized recovery remains attach-only: it re-fetches and consumes a
    /// fresh receipt, and is never reachable from the public status RPC.
    pub(super) fn verify_authorized_queue_grant_for_recovery(
        &self,
        prepared_id: &str,
        install_id: &str,
        context_id: Option<&str>,
        shown: &Value,
    ) -> Result<VerifiedPublishGrant> {
        self.verify_queue_grant(prepared_id, install_id, context_id, shown, true)
    }

    fn verify_queue_grant(
        &self,
        prepared_id: &str,
        install_id: &str,
        context_id: Option<&str>,
        shown: &Value,
        allow_authorized_recovery: bool,
    ) -> Result<VerifiedPublishGrant> {
        let inspection = self
            .inspect_prepared_queue(
                prepared_id,
                install_id,
                context_id,
                shown,
                allow_authorized_recovery,
            )
            .map_err(QueueInspectionError::into_error)?;
        let claims = &inspection.parsed.claims;
        let consumed = self
            .operator_auth
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .consume_assertion_jti(&claims.jti, claims.exp, inspection.verified_at)?;
        if !consumed {
            return Err(Error::rejected(
                "social queue-validation receipt jti was already used",
            ));
        }
        Ok(VerifiedPublishGrant {
            prepared_id: prepared_id.to_owned(),
            prepared_descriptor_digest: inspection.prepared_descriptor_digest,
            owner_intent_digest: inspection.owner_intent_digest,
            grant_id: claims.grant_id.clone(),
        })
    }
}
