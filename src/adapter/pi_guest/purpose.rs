//! Finite shared Pi-purpose verification for root and its selected helper.
//! Callers obtain keys only from independently qualified purpose trust. This
//! result is NOT a root launch permit, owned caller or durable current proof.
use super::authority::{refused, OperationScope};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::io;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) type PiKeyRecord = (String, String, u64, [u8; 32]);
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
fn decode(part: &str, max: usize) -> io::Result<Vec<u8>> {
    if part.is_empty()
        || part.len() > max.div_ceil(3) * 4
        || !part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(refused());
    }
    let bytes = URL_SAFE_NO_PAD.decode(part).map_err(|_| refused())?;
    if bytes.len() > max || URL_SAFE_NO_PAD.encode(&bytes) != part {
        return Err(refused());
    }
    Ok(bytes)
}
fn canonical<T: for<'a> Deserialize<'a> + Serialize>(bytes: &[u8]) -> io::Result<T> {
    let parsed: T = serde_json::from_slice(bytes).map_err(|_| refused())?;
    // Exact typed canonical serialization rejects duplicate, unknown, missing
    // fields, floats, alternative escapes, order and whitespace.
    let normalized = serde_json::to_vec(&serde_json::to_value(&parsed).map_err(|_| refused())?)
        .map_err(|_| refused())?;
    if normalized != bytes {
        return Err(refused());
    }
    Ok(parsed)
}
pub(crate) fn now_ms() -> io::Result<u64> {
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| refused())?
            .as_millis(),
    )
    .map_err(|_| refused())?;
    if now > MAX_SAFE {
        return Err(refused());
    }
    Ok(now)
}
/// Private non-Clone signature result. Cannot reconstruct a LaunchPermit or
/// extend signed lifetime; live custody/current/durable CAS remain mandatory.
pub(crate) struct AuthenticatedOperation {
    authorization: String,
    expires_at_ms: u64,
    until: Instant,
    last_wall: Cell<u64>,
}
impl AuthenticatedOperation {
    pub(crate) fn deadline(&self) -> Instant {
        self.until
    }
    pub(crate) fn original(&self) -> &str {
        &self.authorization
    }
    pub(crate) fn recheck(&self, now: u64) -> io::Result<()> {
        self.require(&self.authorization, now)
    }
    pub(crate) fn require(&self, authorization: &str, now: u64) -> io::Result<()> {
        if authorization != self.authorization
            || now < self.last_wall.get()
            || now >= self.expires_at_ms
            || Instant::now() >= self.until
        {
            return Err(refused());
        }
        self.last_wall.set(now);
        Ok(())
    }
}
/// Pure private-code comparison, never an RPC or public authority factory.
/// Root supplies constructor-qualified Pi ring; helper supplies only its opaque
/// independently authenticated fixed-media HelperImageTrust ring.
pub(crate) fn authenticate_operation(
    scope: &OperationScope,
    reference: &str,
    authorization: &str,
    keys: &[PiKeyRecord],
    now: u64,
    runtime_image_expiry: u64,
) -> io::Result<AuthenticatedOperation> {
    let observed = Instant::now();
    if authorization.len() > MAX_COMPACT || keys.is_empty() || keys.len() > 8 || now > MAX_SAFE {
        return Err(refused());
    }
    let mut parts = authorization.split('.');
    let h = parts.next().ok_or_else(refused)?;
    let p = parts.next().ok_or_else(refused)?;
    let s = parts.next().ok_or_else(refused)?;
    if parts.next().is_some() {
        return Err(refused());
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
        return Err(refused());
    }
    let mut selected = keys.iter().filter(|(issuer, kid, version, _)| {
        issuer == &header.issuer && kid == &header.kid && *version == header.key_version
    });
    let key = selected.next().ok_or_else(refused)?;
    if selected.next().is_some() || key.3 == [0; 32] {
        return Err(refused());
    }
    let mut signed = Vec::with_capacity(DOMAIN.len() + h.len() + p.len() + 1);
    signed.extend_from_slice(DOMAIN);
    signed.extend_from_slice(h.as_bytes());
    signed.push(b'.');
    signed.extend_from_slice(p.as_bytes());
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key.3)
        .verify(&signed, &signature)
        .map_err(|_| refused())?;
    Ok(AuthenticatedOperation {
        authorization: authorization.to_owned(),
        expires_at_ms: body.expires_at_ms,
        until: observed + Duration::from_millis(body.expires_at_ms - now),
        last_wall: Cell::new(now),
    })
}
