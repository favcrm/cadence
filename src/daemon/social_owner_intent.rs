//! CAD-1143 AOS-issued, single-use authorization for the private prepared-intent read.
//!
//! This verifier is deliberately separate from board-session and relay-proof
//! assertions. The configured AOS issuer attests its cookie-owner/workspace
//! checks; the host compares only the source-backed PublicBoard identity and
//! the locally re-proved immutable intent. It never maps AOS runtime identity
//! onto the daemon UUID or lease epoch.

use std::collections::BTreeMap;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{Error, Result};

use super::Shared;

const PURPOSE: &str = "social.intent.read.v1";
const VERSION: &str = "social-owner-intent.v1";
const MAX_ASSERTION_BYTES: usize = 8 * 1024;
const MAX_ASSERTION_AGE_SECS: i64 = 15;
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    typ: String,
    kid: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    iss: String,
    aud: String,
    sub: String,
    purpose: String,
    workspace: String,
    intent_id: String,
    expected_intent_digest: String,
    iat: i64,
    exp: i64,
    jti: String,
}

struct ParsedAssertion {
    header: Header,
    claims: Claims,
    signing_input: String,
    signature: Vec<u8>,
}

fn invalid(message: &str) -> Error {
    Error::rejected(format!("social intent assertion invalid: {message}"))
}

fn decode_part(part: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| invalid("compact JWS is not base64url"))
}

fn parse_assertion(token: &str) -> Result<ParsedAssertion> {
    if token.is_empty() || token.len() > MAX_ASSERTION_BYTES || !token.is_ascii() {
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
        .map_err(|_| invalid("JWS header is not the closed contract shape"))?;
    if header.alg != "EdDSA"
        || header.typ != "JWT"
        || header.kid.is_empty()
        || header.kid.len() > 200
    {
        return Err(invalid("JWS header is not EdDSA JWT with a bounded kid"));
    }
    let claims: Claims = serde_json::from_slice(&decode_part(encoded_claims)?)
        .map_err(|_| invalid("JWS claims are not the closed contract shape"))?;
    if claims.iss.is_empty()
        || claims.aud.is_empty()
        || claims.sub.is_empty()
        || !valid_hosted_id(&claims.workspace)
        || !valid_contract_id(&claims.intent_id)
        || !valid_sha256(&claims.expected_intent_digest)
    {
        return Err(invalid(
            "JWS claims contain an empty or malformed bounded value",
        ));
    }
    let jti = Uuid::parse_str(&claims.jti).map_err(|_| invalid("jti is not a UUID"))?;
    if jti.get_version_num() != 4 || jti.to_string() != claims.jti {
        return Err(invalid("jti is not a canonical lowercase UUIDv4"));
    }
    let signature = decode_part(encoded_signature)?;
    if signature.len() != 64 {
        return Err(invalid("JWS signature is not 64-byte Ed25519"));
    }
    Ok(ParsedAssertion {
        header,
        claims,
        signing_input: format!("{encoded_header}.{encoded_claims}"),
        signature,
    })
}

pub(super) fn valid_contract_id(value: &str) -> bool {
    let Some(first) = value.as_bytes().first() else {
        return false;
    };
    value.len() <= 120
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

fn valid_hosted_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn claim_time_is_current(claims: &Claims, now: i64) -> bool {
    claims.iat <= now
        && claims.exp > now
        && claims.exp > claims.iat
        && claims.exp.saturating_sub(claims.iat) <= MAX_ASSERTION_AGE_SECS
}

pub(super) fn field_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| Error::rejected(format!("prepared owner-intent {field} is missing")))
}

fn required_safe_epoch(value: &Value, field: &str) -> Result<i64> {
    value
        .get(field)
        .and_then(Value::as_i64)
        .filter(|number| *number >= 0 && *number <= MAX_SAFE_INTEGER)
        .ok_or_else(|| Error::rejected(format!("prepared owner-intent {field} is invalid")))
}

fn nullable_string(value: &Value, field: &str) -> Result<Value> {
    match value.get(field) {
        Some(Value::Null) => Ok(Value::Null),
        Some(Value::String(text)) if valid_contract_id(text) => Ok(json!(text)),
        _ => Err(Error::rejected(format!(
            "prepared owner-intent {field} is not a bounded id or null"
        ))),
    }
}

fn bare_sha256(value: &str, what: &str) -> Result<String> {
    let bare = value.strip_prefix("sha256:").unwrap_or(value);
    if valid_sha256(bare) {
        Ok(bare.to_owned())
    } else {
        Err(Error::rejected(format!(
            "prepared owner-intent {what} is invalid"
        )))
    }
}

pub(super) fn reprove_staged_effect(
    frozen: &Value,
    material: &Value,
    effect: &Value,
) -> Result<()> {
    let effect_id = field_string(frozen, "effect_id")?;
    let authority = &effect["effect"]["authority"];
    if effect["effect"]["effect_id"].as_str() != Some(effect_id)
        || effect["effect"]["state"] != "waiting"
        || authority["install_id"] != frozen["install_id"]
        || authority["context"]["id"] != frozen["context_id"]
        || authority["run_id"] != frozen["run_id"]
        || authority["run_snapshot_digest"] != frozen["run_snapshot_digest"]
        || authority["bundle_digest"] != frozen["bundle_digest"]
        || authority["artifact_id"] != frozen["artifact_id"]
        || authority["artifact_digest"] != frozen["artifact_digest"]
        || authority["slot"] != frozen["slot"]
        || authority["binding"] != material["binding"]
        || authority["material_digest"] != crate::store::app_runs::material_digest(material)
        || authority["provenance"]["effect_id"] != effect_id
        || authority["review_receipt_digest"] != frozen["review_receipt_digest"]
        || authority.get("asset") != material.get("asset")
    {
        return Err(Error::rejected(
            "prepared owner-intent staged effect no longer matches its reviewed material",
        ));
    }
    Ok(())
}

/// Build the closed AOS v1 descriptor only from a persisted prepared row and
/// the store's current provenance re-proof. `intent_digest` is SHA-256 of the
/// compact JSON object with every other ASCII-sorted field; it never hashes
/// itself. All optional fields are present with JSON null when absent.
pub(super) fn owner_intent_descriptor(
    intent_id: &str,
    frozen: &Value,
    material: &Value,
) -> Result<Value> {
    if !valid_contract_id(intent_id) {
        return Err(Error::rejected("prepared owner-intent id is invalid"));
    }
    let run_id = field_string(frozen, "run_id")?;
    let artifact_id = field_string(frozen, "artifact_id")?;
    let bundle_digest = field_string(frozen, "bundle_digest")?;
    let slot = field_string(frozen, "slot")?;
    if material["run"]["id"].as_str() != Some(run_id)
        || material["run"]["install_id"] != frozen["install_id"]
        || material["run"]["context_id"] != frozen["context_id"]
        || material["artifact"]["id"].as_str() != Some(artifact_id)
        || material["run"]["snapshot"]["bundle_digest"].as_str() != Some(bundle_digest)
        || material["run"]["snapshot"]["publication"]["slot"].as_str() != Some(slot)
    {
        return Err(Error::rejected(
            "prepared owner-intent no longer matches the approved run scope",
        ));
    }
    if frozen["run_snapshot_digest"] != material["run"]["snapshot_digest"]
        || frozen["approved_digest"] != material["run"]["approved_digest"]
        || frozen["artifact_digest"] != material["artifact"]["digest"]
        || frozen["review_receipt_digest"]
            != crate::store::app_runs::material_digest(&material["review_receipt"])
    {
        return Err(Error::rejected(
            "prepared owner-intent run, artifact, or review receipt changed",
        ));
    }
    let binding = &material["binding"];
    let publish = &binding["config"]["publish"];
    if frozen["binding_id"] != binding["id"]
        || frozen["binding_revision"] != binding["revision"]
        || frozen["binding_digest"] != binding["digest"]
        || frozen["binding_connection_id"] != binding["config"]["connection_id"]
        || frozen["destination_id"] != publish["destination_id"]
        || frozen["toolkit"] != publish["toolkit"]
    {
        return Err(Error::rejected(
            "prepared owner-intent no longer matches the current publication binding",
        ));
    }
    let review_id = field_string(&material["review_receipt"], "message_id")?;
    let connection_id = field_string(frozen, "aos_connection_id")?;
    let destination_id = field_string(frozen, "destination_id")?;
    let toolkit = field_string(frozen, "toolkit")?;
    if !matches!(toolkit, "instagram" | "facebook") {
        return Err(Error::rejected("prepared owner-intent toolkit is invalid"));
    }
    let caption_digest = bare_sha256(field_string(frozen, "caption_digest")?, "caption digest")?;
    let image_digest = match frozen.get("image_digest") {
        Some(Value::Null) => Value::Null,
        Some(Value::String(value)) => json!(bare_sha256(value, "image digest")?),
        _ => {
            return Err(Error::rejected(
                "prepared owner-intent image digest is invalid",
            ))
        }
    };
    let material_image_digest = material
        .get("asset")
        .and_then(|asset| asset.get("digest"))
        .and_then(Value::as_str)
        .map(|digest| bare_sha256(digest, "reviewed image digest"))
        .transpose()?;
    let material_caption_digest = bare_sha256(
        field_string(&material["artifact"], "digest")?,
        "artifact digest",
    )?;
    if image_digest.as_str() != material_image_digest.as_deref()
        || caption_digest != material_caption_digest
    {
        return Err(Error::rejected(
            "prepared owner-intent caption or image differs from current reviewed material",
        ));
    }
    let mode = match field_string(frozen, "mode")? {
        "now" => "immediate",
        "schedule" => "scheduled",
        _ => return Err(Error::rejected("prepared owner-intent mode is invalid")),
    };
    let due_epoch = required_safe_epoch(frozen, "due_epoch")?;
    let not_before = required_safe_epoch(frozen, "not_before_epoch")?;
    let expires_at = required_safe_epoch(frozen, "expires_epoch")?;
    if not_before > due_epoch || due_epoch > expires_at {
        return Err(Error::rejected(
            "prepared owner-intent execution window is invalid",
        ));
    }
    let binding_revision = binding["revision"]
        .as_i64()
        .filter(|value| *value > 0 && *value <= MAX_SAFE_INTEGER)
        .ok_or_else(|| Error::rejected("prepared owner-intent binding revision is invalid"))?;
    let install_id = field_string(frozen, "install_id")?;
    let context_id = nullable_string(frozen, "context_id")?;
    let binding_id = field_string(frozen, "binding_id")?;
    let app_id = field_string(frozen, "app_id")?;
    let binding_digest = bare_sha256(field_string(frozen, "binding_digest")?, "binding digest")?;
    let bundle_digest = bare_sha256(bundle_digest, "bundle digest")?;
    let effect_id = field_string(frozen, "effect_id")?;
    let cadence_approval_id = field_string(frozen, "approval_id")?;

    for id in [
        install_id,
        binding_id,
        run_id,
        artifact_id,
        review_id,
        effect_id,
    ] {
        if !valid_contract_id(id) {
            return Err(Error::rejected(
                "prepared owner-intent contains an invalid id",
            ));
        }
    }
    if app_id.len() > 120 || connection_id.len() > 80 || destination_id.len() > 120 {
        return Err(Error::rejected(
            "prepared owner-intent contains an oversized target",
        ));
    }
    if cadence_approval_id.len() > 120 {
        return Err(Error::rejected(
            "prepared owner-intent approval id is oversized",
        ));
    }

    let mut fields = BTreeMap::<String, Value>::new();
    fields.insert("version".into(), json!(VERSION));
    fields.insert("intent_id".into(), json!(intent_id));
    fields.insert("app_id".into(), json!(app_id));
    fields.insert("install_id".into(), json!(install_id));
    fields.insert("context_id".into(), context_id);
    fields.insert("binding_id".into(), json!(binding_id));
    fields.insert("binding_revision".into(), json!(binding_revision));
    fields.insert("binding_digest".into(), json!(binding_digest));
    fields.insert("bundle_digest".into(), json!(bundle_digest));
    fields.insert("run_id".into(), json!(run_id));
    fields.insert("artifact_id".into(), json!(artifact_id));
    fields.insert("review_id".into(), json!(review_id));
    fields.insert("connection_id".into(), json!(connection_id));
    fields.insert("destination_id".into(), json!(destination_id));
    fields.insert("toolkit".into(), json!(toolkit));
    fields.insert("caption_digest".into(), json!(caption_digest));
    fields.insert("image_digest".into(), image_digest);
    fields.insert("effect_id".into(), json!(effect_id));
    fields.insert("cadence_approval_id".into(), json!(cadence_approval_id));
    fields.insert("mode".into(), json!(mode));
    fields.insert("due_epoch".into(), json!(due_epoch));
    fields.insert("not_before".into(), json!(not_before));
    fields.insert("expires_at".into(), json!(expires_at));

    let payload = serde_json::to_vec(&fields)?;
    let intent_digest = Sha256::digest(payload)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    fields.insert("intent_digest".into(), json!(intent_digest));
    serde_json::to_value(fields).map_err(Into::into)
}

impl Shared {
    /// Resolve the one-use AOS assertion and return the bare, closed
    /// `social-owner-intent.v1` descriptor expected by the guarded AOS GET.
    /// This method intentionally has no local-operator/session fallback.
    pub(super) fn rpc_social_owner_intent_read(&self, params: &Value) -> Result<Value> {
        super::app_bindings_rpc::strict_fields(params, &["intent_id", "assertion"])?;
        let intent_id = params
            .get("intent_id")
            .and_then(Value::as_str)
            .filter(|value| valid_contract_id(value))
            .ok_or_else(|| Error::rejected("social owner-intent id is malformed"))?;
        let token = params
            .get("assertion")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::rejected("social intent assertion is missing"))?;
        let parsed = parse_assertion(token)?;
        let claims = &parsed.claims;
        let config = crate::board_identity::read_config(&self.state_dir)?;
        // These comparisons are safe early refusals only. No claim is trusted
        // until the configured issuer's Ed25519 key verifies the signature.
        if claims.iss != config.issuer
            || claims.aud != config.host
            || claims.workspace != config.company
            || claims.purpose != PURPOSE
            || claims.intent_id != intent_id
        {
            return Err(invalid(
                "issuer, audience, workspace, purpose, or intent scope mismatch",
            ));
        }
        let now = self.operator_now();
        if !claim_time_is_current(claims, now) {
            return Err(invalid(
                "iat/exp are outside the 15-second assertion window",
            ));
        }
        let key = {
            let mut cache = self
                .board_jwks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            cache.key(&config.issuer, &parsed.header.kid, now)?
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
        // Consume only a verified, current issuer assertion. The shared
        // operator-auth ledger stores only sha256(jti), atomically under its
        // mutex and across daemon restarts; replay is a refusal.
        let consumed = self
            .operator_auth
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .consume_assertion_jti(&claims.jti, claims.exp, now)?;
        if !consumed {
            return Err(Error::rejected(
                "social intent assertion jti was already used",
            ));
        }
        let shown = self.store.social_publish_prepared_owner_show(intent_id)?;
        let frozen = &shown["prepared"]["descriptor"];
        let run_id = field_string(frozen, "run_id")?;
        let artifact_id = field_string(frozen, "artifact_id")?;
        let bundle_digest = field_string(frozen, "bundle_digest")?;
        let slot = field_string(frozen, "slot")?;
        let material =
            self.store
                .app_publication_material(run_id, artifact_id, bundle_digest, slot)?;
        let effect_id = field_string(frozen, "effect_id")?;
        let effect = self.store.app_effect_show(effect_id)?;
        reprove_staged_effect(frozen, &material, &effect)?;
        let install_id = field_string(frozen, "install_id")?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        let current_app_id = crate::issue::app_catalog::workspace::with_completed_bundle_snapshot(
            &pm,
            install_id,
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
            ));
        }
        let image_digest = frozen.get("image_digest").and_then(Value::as_str);
        let media_key = shown["prepared"]["media_key"].as_str();
        let aos_connection_id = field_string(frozen, "aos_connection_id")?;
        match (image_digest, media_key) {
            (Some(digest), Some(key))
                if crate::platform::agenticos_external::publish::media_key_authorizes_connection(
                    key,
                    aos_connection_id,
                    digest,
                ) => {}
            (None, None) => {}
            _ => {
                return Err(Error::rejected(
                    "prepared owner-intent media receipt does not match its AOS connection",
                ));
            }
        }
        let descriptor = owner_intent_descriptor(intent_id, frozen, &material)?;
        if descriptor["intent_digest"] != claims.expected_intent_digest {
            return Err(Error::rejected("social intent descriptor digest mismatch"));
        }
        if descriptor["expires_at"]
            .as_i64()
            .is_none_or(|expires| expires <= now)
        {
            return Err(Error::rejected("prepared owner intent has expired"));
        }
        Ok(descriptor)
    }
}
