//! Independent Pi-purpose signature. Only the real owner factory supplies the
//! constructor-qualified public ring/expiry; there is no caller key, fallback
//! ring, test trust root, permit boolean or serialized opaque-result constructor.
use super::{refuse, OperationScope};
use crate::error::Result;
use crate::installer_bundle::constructor::PiKeyRecord;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DOMAIN: &[u8] = b"cadence.protected-pi-launch.v1\0";
const MAX_SAFE: u64 = 9007199254740991;
const MAX_COMPACT: usize = 32768;
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Header {
    alg: String,
    issuer: String,
    kid: String,
    key_version: u64,
    #[serde(rename = "type")]
    kind: String,
    version: u32,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Payload {
    version: u32,
    reference: String,
    scope: OperationScope,
    issued_at_ms: u64,
    expires_at_ms: u64,
}
fn decode(part: &str, max: usize) -> Result<Vec<u8>> {
    if part.is_empty()
        || part.len() > max.div_ceil(3) * 4
        || !part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(refuse());
    }
    let bytes = URL_SAFE_NO_PAD.decode(part).map_err(|_| refuse())?;
    if bytes.len() > max || URL_SAFE_NO_PAD.encode(&bytes) != part {
        return Err(refuse());
    }
    Ok(bytes)
}
fn canonical<T: for<'a> Deserialize<'a> + Serialize>(bytes: &[u8]) -> Result<T> {
    let parsed: T = serde_json::from_slice(bytes).map_err(|_| refuse())?;
    // Typed parsing + exact sorted serialization rejects duplicate, unknown,
    // missing fields, floats-as-integers, alternate escaping/order/whitespace.
    let normalized = serde_json::to_vec(&serde_json::to_value(&parsed).map_err(|_| refuse())?)
        .map_err(|_| refuse())?;
    if normalized != bytes {
        return Err(refuse());
    }
    Ok(parsed)
}
pub(super) fn now_ms() -> Result<u64> {
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| refuse())?
            .as_millis(),
    )
    .map_err(|_| refuse())?;
    if now > MAX_SAFE {
        return Err(refuse());
    }
    Ok(now)
}
/// Non-Clone/private origin. Descriptions cannot reconstruct this result or
/// extend its signed deadline. Live custody/current/CAS are STILL required.
pub(super) struct AuthenticatedOperation {
    authorization: String,
    expires_at_ms: u64,
    until: Instant,
    last_wall: Cell<u64>,
}
impl AuthenticatedOperation {
    pub(super) fn deadline(&self) -> Instant {
        self.until
    }
    pub(super) fn recheck(&self, now: u64) -> Result<()> {
        self.require(&self.authorization, now)
    }
    pub(super) fn require(&self, authorization: &str, now: u64) -> Result<()> {
        if authorization != self.authorization
            || now < self.last_wall.get()
            || now >= self.expires_at_ms
            || Instant::now() >= self.until
        {
            return Err(refuse());
        }
        self.last_wall.set(now);
        Ok(())
    }
}
/// Pure finite cryptographic comparison. Private to the owner module: the ONLY
/// product call gets ring/expiry from actual qualified constructor/runtime,
/// never a caller, guest file, returned key, signer or generic Proof conversion.
pub(super) fn authenticate_operation(
    scope: &OperationScope,
    reference: &str,
    authorization: &str,
    keys: &[PiKeyRecord],
    now: u64,
    runtime_image_expiry: u64,
) -> Result<AuthenticatedOperation> {
    let observed = Instant::now();
    if authorization.len() > MAX_COMPACT || keys.is_empty() || keys.len() > 8 || now > MAX_SAFE {
        return Err(refuse());
    }
    let mut parts = authorization.split('.');
    let h = parts.next().ok_or_else(refuse)?;
    let p = parts.next().ok_or_else(refuse)?;
    let s = parts.next().ok_or_else(refuse)?;
    if parts.next().is_some() {
        return Err(refuse());
    }
    let header: Header = canonical(&decode(h, 256)?)?;
    let body: Payload = canonical(&decode(p, 20000)?)?;
    let signature = decode(s, 64)?;
    if header.alg != "Ed25519"
        || header.issuer != "agenticos-native-owner"
        || header.kind != "protected-pi-launch"
        || header.version != 1
        || header.key_version == 0
        || header.key_version > 2147483647
        || header.kid.is_empty()
        || header.kid.len() > 64
        || !header
            .kid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || body.version != 1
        || body.reference != reference
        || &body.scope != scope
        || reference.len() != 32
        || !reference.bytes().any(|b| b != b'0')
        || !reference
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || body.issued_at_ms > now
        || body.expires_at_ms <= now
        || body.expires_at_ms > MAX_SAFE
        || body.expires_at_ms > runtime_image_expiry
        || body.expires_at_ms.saturating_sub(body.issued_at_ms) > 30000
        || signature.len() != 64
    {
        return Err(refuse());
    }
    let mut selected = keys.iter().filter(|(issuer, kid, version, _)| {
        issuer == &header.issuer && kid == &header.kid && *version == header.key_version
    });
    let key = selected.next().ok_or_else(refuse)?;
    if selected.next().is_some() || key.3 == [0; 32] {
        return Err(refuse());
    }
    let mut signed = Vec::with_capacity(DOMAIN.len() + h.len() + p.len() + 1);
    signed.extend_from_slice(DOMAIN);
    signed.extend_from_slice(h.as_bytes());
    signed.push(b'.');
    signed.extend_from_slice(p.as_bytes());
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key.3)
        .verify(&signed, &signature)
        .map_err(|_| refuse())?;
    Ok(AuthenticatedOperation {
        authorization: authorization.to_owned(),
        expires_at_ms: body.expires_at_ms,
        until: observed + Duration::from_millis(body.expires_at_ms - now),
        last_wall: Cell::new(now),
    })
}
