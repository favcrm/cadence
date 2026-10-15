//! Board membership proof consumer (AOS-218, CAD-1374).
//!
//! The daemon asks the platform, over its private `http://api.internal`
//! channel, whether the authenticated board subject still holds a membership
//! in the bound company. The platform answers with a compact Ed25519 JWS
//! signed by the same board keyring the platform publishes for board
//! assertions. This module:
//!
//! - builds the request from the raw board cookie (only its SHA-256 digest and
//!   a fresh nonce leave the process);
//! - POSTs it to a fixed URL with a bounded deadline and body cap;
//! - verifies the returned proof with [`verify_proof`], which checks the
//!   signature before any claim is trusted and then binds every claim to the
//!   caller's expected values.
//!
//! It grants no capability and proves no human. A verified `Ok(None)` means
//! the subject has no current membership (authenticated absence). Every other
//! failure is one static, sanitised refusal that carries no token, body or
//! cookie material.
//!
//! The caller owns the [`JwksCache`]: its key-refresh lock is separate from
//! any operator-session state, and only public keys are cached here. No
//! membership or role answer is cached.

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::digest;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};

use crate::board_identity::{JwksCache, PublicKey, Role};
use crate::error::{Error, Result};

/// The fixed proof purpose. Any other value is refused.
pub const PURPOSE: &str = "cadence-board-membership/1";
/// The fixed issuer of the private platform channel.
const ISSUER: &str = "http://api.internal";
const CHECK_URL: &str = "http://api.internal/v1/runtime/cadence/member/check";
/// Compact proof JWS upper bound, in bytes.
const TOKEN_MAX: usize = 4096;
/// Response body cap. The success envelope is far smaller.
const RESPONSE_MAX: u64 = 4096;
/// One lookup's network budget.
const DEADLINE: Duration = Duration::from_secs(2);
/// The proof lifetime: `exp - iat` never exceeds this.
const MAX_LIFE_SECS: i64 = 5;

/// What a proof must bind to. `company` and `instance` come from the daemon's
/// trusted configuration; `sub` comes from the daemon-authenticated cookie
/// session; `session` is the cookie digest and `nonce` is this check's nonce.
pub struct Expected<'a> {
    pub company: &'a str,
    pub instance: &'a str,
    pub sub: &'a str,
    pub session: &'a str,
    pub nonce: &'a str,
}

/// The JWS header: exactly `alg`, `typ`, `kid`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    typ: String,
    kid: String,
}

/// The closed claim set. A `null` role is an authenticated absence.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    purpose: String,
    iss: String,
    aud: String,
    instance: String,
    sub: String,
    session: String,
    nonce: String,
    role: serde_json::Value,
    iat: i64,
    exp: i64,
    jti: String,
}

/// The success envelope `{ "ok": true, "data": { "proof": ... } }`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    ok: bool,
    data: EnvelopeData,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeData {
    proof: String,
}

/// The request body. Company and instance are never sent.
#[derive(Serialize)]
struct CheckRequest {
    schema_version: u8,
    sub: String,
    session: String,
    nonce: String,
}

/// The one static refusal. It carries no token, body, cookie or claim value.
fn refused() -> Error {
    Error::invalid(
        "capability_unavailable",
        "board membership could not be confirmed",
    )
}

/// The same refusal, tagged with a fixed diagnostic phase so a 503 can show
/// WHERE the lookup failed. `phase` is always one of a small set of static
/// constants — request, transport, http_status, body, envelope, jwks, proof.
/// `status` carries only the numeric HTTP status for `http_status`; no raw
/// error text, body, header, URL, proof, claim, digest, nonce or sub value is
/// ever appended.
fn refused_phase(phase: &'static str, status: Option<u16>) -> Error {
    let mut msg = String::from("board membership could not be confirmed (");
    msg.push_str(phase);
    if let Some(code) = status {
        msg.push(' ');
        msg.push_str(&code.to_string());
    }
    msg.push(')');
    Error::invalid("capability_unavailable", msg)
}

fn bounded_text(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}

fn lower_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn company_ok(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn instance_ok(s: &str) -> bool {
    (1..=12).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
}

fn expected_ok(e: &Expected<'_>) -> bool {
    company_ok(e.company)
        && instance_ok(e.instance)
        && bounded_text(e.sub, 128)
        && lower_hex(e.session, 64)
        && lower_hex(e.nonce, 32)
}

/// One base64url segment: a non-empty run of the URL-safe alphabet that decodes.
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

/// A structurally sound compact JWS. Nothing in it is trusted yet.
struct Parts<'a> {
    /// The signed bytes: `<header>.<payload>` verbatim.
    signed: &'a str,
    header: Header,
    payload: Vec<u8>,
    signature: Vec<u8>,
}

/// Split and shape-check the compact JWS: bounded length, three segments,
/// the exact header, and a 64-byte signature. Duplicate or unknown header
/// fields fail in the derived deserializer.
fn split(token: &str) -> Option<Parts<'_>> {
    if token.is_empty() || token.len() > TOKEN_MAX {
        return None;
    }
    let mut it = token.split('.');
    let (h, p, s) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() {
        return None;
    }
    let header: Header = serde_json::from_slice(&segment(h)?).ok()?;
    if header.alg != "EdDSA" || header.typ != "JWT" || !bounded_text(&header.kid, 128) {
        return None;
    }
    let payload = segment(p)?;
    let signature = segment(s)?;
    if signature.len() != 64 {
        return None;
    }
    Some(Parts {
        signed: &token[..h.len() + 1 + p.len()],
        header,
        payload,
        signature,
    })
}

/// Verify a compact proof against one trusted public key and the expected
/// binding. Every failure, including a foreign `kid`, a bad signature, any
/// claim mismatch or an out-of-window time, returns the same static refusal.
/// `Ok(None)` is an authenticated absence (`role: null`).
pub fn verify_proof(
    token: &str,
    expected: &Expected<'_>,
    key: &PublicKey,
    now: i64,
) -> Result<Option<Role>> {
    if !expected_ok(expected) {
        return Err(refused());
    }
    let parts = split(token).ok_or_else(refused)?;
    if parts.header.kid != key.kid {
        return Err(refused());
    }
    // The signature covers `header.payload` before any claim is parsed or trusted.
    UnparsedPublicKey::new(&ED25519, &key.x)
        .verify(parts.signed.as_bytes(), &parts.signature)
        .map_err(|_| refused())?;
    let c: Claims = serde_json::from_slice(&parts.payload).map_err(|_| refused())?;

    if c.purpose != PURPOSE
        || c.iss != ISSUER
        || c.aud != expected.company
        || c.instance != expected.instance
        || c.sub != expected.sub
        || c.session != expected.session
        || c.nonce != expected.nonce
        || !bounded_text(&c.sub, 128)
        || !bounded_text(&c.jti, 128)
    {
        return Err(refused());
    }
    // `exp > iat` and `exp - iat <= 5`, checked without overflow.
    let life = c.exp.checked_sub(c.iat).ok_or_else(refused)?;
    if life <= 0 || life > MAX_LIFE_SECS || c.iat > now || now >= c.exp {
        return Err(refused());
    }
    match c.role.as_str() {
        Some("owner") => Ok(Some(Role::Operator)),
        Some("member") => Ok(Some(Role::Member)),
        _ if c.role.is_null() => Ok(None),
        _ => Err(refused()),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 hex digest of a raw board cookie.
pub fn session_digest(raw_cookie: &str) -> String {
    hex(digest::digest(&digest::SHA256, raw_cookie.as_bytes()).as_ref())
}

/// The request for one check: the cookie's full SHA-256 digest and a fresh
/// 16-byte nonce. The digest is the only session value that leaves.
fn check_request(session: &str, sub: &str) -> Result<CheckRequest> {
    if !lower_hex(session, 64) || !bounded_text(sub, 128) {
        return Err(refused());
    }
    let mut nonce = [0u8; 16];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| refused())?;
    Ok(CheckRequest {
        schema_version: 1,
        sub: sub.to_string(),
        session: session.to_string(),
        nonce: hex(&nonce),
    })
}

/// The proof token from a 200 success envelope, or `None` for any other shape.
fn envelope_proof(bytes: &[u8]) -> Option<String> {
    let env: Envelope = serde_json::from_slice(bytes).ok()?;
    env.ok.then_some(env.data.proof)
}

fn now_unix() -> Result<i64> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| refused())?
        .as_secs();
    i64::try_from(secs).map_err(|_| refused())
}

/// Ask the platform whether `authenticated_sub` still holds a membership in
/// `trusted_company` at `trusted_instance`, bound to the raw board cookie.
/// The raw cookie is hashed here; see [`lookup_current_membership_by_digest`].
pub fn lookup_current_membership(
    raw_cookie: &str,
    authenticated_sub: &str,
    trusted_company: &str,
    trusted_instance: &str,
    jwks: &Mutex<JwksCache>,
) -> Result<Option<Role>> {
    if raw_cookie.is_empty() {
        return Err(refused_phase("request", None));
    }
    lookup_current_membership_by_digest(
        &session_digest(raw_cookie),
        authenticated_sub,
        trusted_company,
        trusted_instance,
        jwks,
    )
}

/// The same check from a known FULL session digest, for a trusted snapshot
/// that holds the verified row's hash rather than the raw cookie.
///
/// The caller must pass `authenticated_sub` from its own authenticated
/// session. Company and instance are trusted daemon config only. Each call
/// does a fresh POST and verifies with the current time. The `jwks` lock is
/// held only to resolve the key, after the response, never across the POST.
pub fn lookup_current_membership_by_digest(
    session: &str,
    authenticated_sub: &str,
    trusted_company: &str,
    trusted_instance: &str,
    jwks: &Mutex<JwksCache>,
) -> Result<Option<Role>> {
    let request =
        check_request(session, authenticated_sub).map_err(|_| refused_phase("request", None))?;
    let expected = Expected {
        company: trusted_company,
        instance: trusted_instance,
        sub: authenticated_sub,
        session: &request.session,
        nonce: &request.nonce,
    };
    if !expected_ok(&expected) {
        return Err(refused_phase("request", None));
    }

    let agent = crate::issuer_transport::agent(
        ureq::Agent::config_builder()
            .timeout_global(Some(DEADLINE))
            .http_status_as_error(false),
    );
    let mut resp = agent
        .post(CHECK_URL)
        .send_json(&request)
        .map_err(|_| refused_phase("transport", None))?;
    if resp.status() != 200 {
        return Err(refused_phase("http_status", Some(resp.status().as_u16())));
    }
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(RESPONSE_MAX)
        .read_to_vec()
        .map_err(|_| refused_phase("body", None))?;
    let token = envelope_proof(&bytes).ok_or_else(|| refused_phase("envelope", None))?;

    // The key id comes from the bounded header. The key is looked up on the
    // fixed issuer, and `now` is read after the network and key fetches.
    let kid = split(&token)
        .ok_or_else(|| refused_phase("proof", None))?
        .header
        .kid;
    let key = {
        let mut cache = jwks.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_unix().map_err(|_| refused_phase("jwks", None))?;
        cache
            .key(ISSUER, &kid, now)
            .map_err(|_| refused_phase("jwks", None))?
    };
    let now = now_unix().map_err(|_| refused_phase("proof", None))?;
    verify_proof(&token, &expected, &key, now).map_err(|_| refused_phase("proof", None))
}

#[cfg(test)]
mod refusal_redaction_acceptance {
    use super::*;

    fn assert_safe_refusal(error: &Error, phase: &str, status: Option<u16>, forbidden: &[&str]) {
        assert_eq!(error.code(), Some("capability_unavailable"));
        assert_eq!(error.kind(), "rejected");
        let message = error.to_string();
        let suffix = match status {
            Some(code) => format!("({phase} {code})"),
            None => format!("({phase})"),
        };
        assert!(message.ends_with(&suffix), "unexpected refusal: {message}");
        assert!(message.len() <= 64, "refusal is not bounded: {message}");
        for value in forbidden {
            assert!(
                !message.contains(value),
                "refusal leaked sensitive value {value:?}: {message}"
            );
        }
    }

    #[test]
    fn refusal_phases_are_bounded_and_retain_only_http_status() {
        for phase in ["request", "transport", "body", "envelope", "jwks", "proof"] {
            let error = refused_phase(phase, None);
            assert_safe_refusal(&error, phase, None, &[]);
        }

        let error = refused_phase("http_status", Some(503));
        assert_safe_refusal(&error, "http_status", Some(503), &[]);
    }

    #[test]
    fn lookup_request_refusal_redacts_request_material_and_grants_nothing() {
        let cookie = "cookie-secret-sentinel";
        let subject = "subject-secret-sentinel\n";
        let token = "proof-secret-sentinel";
        let nonce = "nonce-secret-sentinel";
        let digest = session_digest(cookie);
        let url = "http://api.internal/v1/runtime/cadence/member/check";
        let body = "raw-response-body-sentinel";
        let jwks = Mutex::new(JwksCache::new());

        let result = lookup_current_membership(cookie, subject, "company", "instance1", &jwks);
        let error = result.expect_err("malformed request must be refused before lookup");
        assert_safe_refusal(
            &error,
            "request",
            None,
            &[cookie, subject, token, nonce, &digest, url, body],
        );
    }

    #[test]
    fn invalid_proof_refusal_is_generic_and_redacted() {
        let token = "proof-secret-sentinel";
        let session = "a".repeat(64);
        let nonce = "b".repeat(32);
        let expected = Expected {
            company: "company",
            instance: "instance1",
            sub: "subject-secret-sentinel",
            session: &session,
            nonce: &nonce,
        };
        let key = PublicKey {
            kid: "kid-secret-sentinel".to_string(),
            x: [0; 32],
        };

        let error = verify_proof(token, &expected, &key, 1_700_000_000)
            .expect_err("malformed proof must not grant membership");
        assert_eq!(error.code(), Some("capability_unavailable"));
        assert_eq!(error.kind(), "rejected");
        assert_eq!(error.to_string(), "board membership could not be confirmed");
        for value in [token, "subject-secret-sentinel", "kid-secret-sentinel"] {
            assert!(!error.to_string().contains(value));
        }
    }
}
