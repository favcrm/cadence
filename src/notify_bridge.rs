//! CAD-1423 / AOS-238: the native half of the host-custodied Web Push bridge.
//!
//! The platform (`http://api.internal`) holds the VAPID private key and does
//! the encryption and send. This module never sees a private key. It verifies
//! the platform's compact Ed25519 service proofs, which carry a DISTINCT `typ`
//! and `purpose`, so no human board assertion, membership proof or generic
//! proxy proof can stand in for one.
//!
//! Every proof is checked in this order, and nothing is trusted before the
//! signature passes:
//! 1. shape and bounded length, exact header `alg`/`typ`/`kid`;
//! 2. the configured issuer's public key for `kid` (fail closed if unknown);
//! 3. Ed25519 over `header.payload`;
//! 4. closed claims: exact purpose, issuer, company, instance, method, path,
//!    expiry window, `jti`, and for finish the lease and outcome binding.
//!
//! A refused proof answers `invalid_proof`. Business refusals (suppressed,
//! stale or active lease) are answered only after the proof passes.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::Deserialize;

use crate::board_identity::{JwksCache, PublicKey};
use crate::error::{Error, Result};

/// The fixed issuer of the private platform channel.
pub const ISSUER: &str = "http://api.internal";
/// The one purpose a delivery proof may carry.
pub const PURPOSE: &str = "cadence-notification-delivery/1";
/// The distinct JWS `typ` for delivery proofs. Never `JWT`.
pub const TYP: &str = "agenticos-notification-delivery+jwt";
/// Proof lifetime cap, in seconds.
const MAX_LIFE_SECS: i64 = 5;
/// Compact token upper bound, in bytes.
const TOKEN_MAX: usize = 4096;
/// Lease length for one claimed device delivery.
pub const LEASE_SECS: i64 = 30;

/// The service paths this bridge accepts, relayed to the native guard. The
/// method and path are fixed by the handler, never caller-chosen.
pub const PREPARE_PATH: &str = "/api/internal/notifications/prepare";
pub const FINISH_PATH: &str = "/api/internal/notifications/finish";

/// The JWS header: exactly `alg`, `typ`, `kid`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    typ: String,
    kid: String,
}

/// The closed delivery claim set. Any unknown field is refused. `outcome` and
/// `lease_id` are present only on finish proofs.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claims {
    pub purpose: String,
    pub iss: String,
    pub aud: String,
    pub instance: String,
    pub intent_id: i64,
    pub device_id: String,
    pub method: String,
    pub path: String,
    pub iat: i64,
    pub exp: i64,
    pub jti: String,
    #[serde(default)]
    pub lease_id: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
}

/// What the verified proof must bind to. Every value comes from trusted native
/// config and the fixed handler, never from the caller's body.
pub struct Expected<'a> {
    pub company: &'a str,
    pub instance: &'a str,
    pub method: &'a str,
    pub path: &'a str,
    pub intent_id: i64,
    pub device_id: &'a str,
    /// Set for finish only.
    pub lease_id: Option<&'a str>,
    pub outcome: Option<&'a str>,
}

/// The one static refusal for a proof. It carries no token, claim or key value.
pub fn invalid_proof() -> Error {
    Error::invalid("invalid_proof", "the delivery proof could not be verified")
}

fn bounded_text(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}

fn segment(part: &str) -> Option<Vec<u8>> {
    if part.is_empty()
        || !part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    URL_SAFE_NO_PAD.decode(part).ok()
}

/// Verify one compact delivery proof with the configured issuer's keyring.
/// Signature first, then claims. `now` is read by the caller after the
/// network and key fetches.
pub fn verify(
    token: &str,
    expected: &Expected<'_>,
    jwks: &Mutex<JwksCache>,
    now: i64,
) -> Result<Claims> {
    if token.is_empty() || token.len() > TOKEN_MAX {
        return Err(invalid_proof());
    }
    let mut it = token.split('.');
    let (Some(h), Some(p), Some(s), None) = (it.next(), it.next(), it.next(), it.next()) else {
        return Err(invalid_proof());
    };
    let header: Header = serde_json::from_slice(&segment(h).ok_or_else(invalid_proof)?)
        .map_err(|_| invalid_proof())?;
    if header.alg != "EdDSA" || header.typ != TYP || !bounded_text(&header.kid, 128) {
        return Err(invalid_proof());
    }
    let payload = segment(p).ok_or_else(invalid_proof)?;
    let signature = segment(s).ok_or_else(invalid_proof)?;
    if signature.len() != 64 {
        return Err(invalid_proof());
    }
    let key: PublicKey = {
        let mut cache = jwks.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .key(ISSUER, &header.kid, now)
            .map_err(|_| invalid_proof())?
    };
    UnparsedPublicKey::new(&ED25519, &key.x)
        .verify(format!("{h}.{p}").as_bytes(), &signature)
        .map_err(|_| invalid_proof())?;
    let c: Claims = serde_json::from_slice(&payload).map_err(|_| invalid_proof())?;
    bind(&c, expected, now)?;
    Ok(c)
}

/// Bind every claim to the handler's expected values and the time window.
fn bind(c: &Claims, e: &Expected<'_>, now: i64) -> Result<()> {
    let life = c.exp.checked_sub(c.iat).ok_or_else(invalid_proof)?;
    let mismatch = c.purpose != PURPOSE
        || c.iss != ISSUER
        || c.aud != e.company
        || c.instance != e.instance
        || c.method != e.method
        || c.path != e.path
        || c.intent_id != e.intent_id
        || c.device_id != e.device_id
        || c.lease_id.as_deref() != e.lease_id
        || c.outcome.as_deref() != e.outcome
        || !bounded_text(&c.jti, 128);
    if mismatch || life <= 0 || life > MAX_LIFE_SECS || c.iat > now || now >= c.exp {
        return Err(invalid_proof());
    }
    Ok(())
}

/// Current unix seconds, read at the point of use.
pub fn now_unix() -> Result<i64> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| invalid_proof())?
        .as_secs();
    i64::try_from(secs).map_err(|_| invalid_proof())
}
