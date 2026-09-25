//! AgenticOS board identity assertions (CAD-526; contract
//! `claudedocs/dogfood/aos-board-identity-contract.md`). A hosted board
//! signs a named company member in with a compact Ed25519 JWS the
//! platform mints at `{app}/v2/board/authorize`; the daemon verifies it
//! here — every check of contract §4 — before any session exists.
//!
//! - **The trust config** is [`Config`] at
//!   `<state>/operator/board-identity.json` — the board's own public
//!   name (`aud`), the platform issuer (`iss`, which also names the
//!   JWKS origin) and this instance's company id. The `cadence ui`
//!   flags write it under the operator directory's strict modes; the
//!   daemon reads it fresh on every `board_session_open`, so nothing
//!   the request carries can pick a different issuer, audience or
//!   company.
//! - **Verification** is [`verify`]: structure first (three base64url
//!   parts, the exact `{alg:"EdDSA",typ:"JWT",kid}` header, the closed
//!   claim set — `client_id`/`azp` and friends must never appear), the
//!   signature against the platform JWKS, then `iss`, `aud`, the
//!   `exp`/`iat` bounds, `company` and the §2 role map. Every refusal
//!   carries the contract's `error.code`.
//! - **JWKS** is fetched from `{iss}/.well-known/agenticos-board-jwks.json`
//!   ([`fetch_keys`]) and cached briefly by the daemon
//!   ([`JwksCache`]); an unknown `kid` forces one refetch and a key id
//!   is only ever honoured from the signed header — `fail closed` on
//!   any fetch or parse failure.
//! - **Single use** (`jti`) lives with the sessions in
//!   [`crate::operator_auth`], persisted so a restart in the
//!   assertion's 60-second life cannot reopen a replay.

use std::path::{Path, PathBuf};

use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};

/// How long the daemon trusts one JWKS fetch.
const JWKS_TTL_SECS: i64 = 60;
/// The JWKS response is a handful of key ids — anything bigger is not
/// one; it is refused without ever buffering it whole.
const JWKS_CAP: usize = 64 * 1024;
/// One fetch's whole budget — a sign-in must never hang the daemon's
/// request thread on a wedged platform.
const JWKS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);
/// `exp` may trail the daemon clock by this much.
const SKEW_SECS: i64 = 30;
/// `iat` may lead the daemon clock by this much.
const IAT_AHEAD_SECS: i64 = 60;
/// The contract's assertion lifetime: `exp - iat` never exceeds it.
const MAX_LIFE_SECS: i64 = 60;

/// The daemon-side trust root for public-name sign-in: this board's
/// public host (the assertion's `aud`), the platform issuer (`iss` —
/// also the JWKS origin) and the one company this instance serves.
/// Lives at `<state>/operator/board-identity.json`, `0600` in the
/// `0700` operator directory — it names which issuer's keys are
/// trusted, so it gets the secret's file hygiene.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// This board's public name — `acme.cadencecloud.app`, or
    /// `acme.board.localhost:3011` in local dev (the contract's `aud`
    /// is `URL.host`, port included when present).
    pub host: String,
    /// The platform issuer — `new URL(BETTER_AUTH_URL).origin`. JWKS is
    /// fetched from `{issuer}/.well-known/agenticos-board-jwks.json`.
    pub issuer: String,
    /// The workspace id this instance serves — `company` must equal it.
    pub company: String,
}

const CONFIG_FILE: &str = "board-identity.json";

pub fn config_path(state_dir: &Path) -> PathBuf {
    crate::operator_auth::dir(state_dir).join(CONFIG_FILE)
}

/// Record the public-name config for the daemon — `ui run`/`ui start`
/// call this when the `--board-*` flags resolve. `0600` under the
/// operator directory; a partial write never replaces a good one (tmp
/// + rename).
pub fn write_config(state_dir: &Path, config: &Config) -> Result<()> {
    crate::operator_auth::write_private(state_dir, CONFIG_FILE, &serde_json::to_vec_pretty(config)?)
}

/// The configured trust root — `Err` (code `capability_unavailable`)
/// when the board is not provisioned for public-name sign-in or the
/// file fails the operator directory's strict modes.
pub fn read_config(state_dir: &Path) -> Result<Config> {
    crate::operator_auth::read_private(state_dir, CONFIG_FILE).and_then(|bytes| {
        serde_json::from_slice::<Config>(&bytes).map_err(|e| {
            Error::invalid(
                "capability_unavailable",
                format!("{}: {e}", config_path(state_dir).display()),
            )
        })
    })
}

/// Is this state dir provisioned for public-name sign-in? (The board
/// reads this to decide whether `POST /__platform/session` exists;
/// `ui status` prints it.)
pub fn configured(state_dir: &Path) -> bool {
    read_config(state_dir).is_ok()
}

/// The Cadence role an assertion maps to (contract §2): `owner` is the
/// board operator, `member` the least-privilege participant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Operator,
    Member,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Member => "member",
        }
    }

    /// Does this role hold operator authority on the board? Only an
    /// `owner`-mapped session decides approvals and other
    /// operator-only writes.
    pub fn is_operator(self) -> bool {
        self == Self::Operator
    }
}

/// A verified assertion's named user — what the session carries.
#[derive(Clone, Debug)]
pub struct Identity {
    pub sub: String,
    pub email: String,
    pub name: String,
    pub role: Role,
    /// The single-use token id — the daemon remembers it until `exp`.
    pub jti: String,
    pub exp: i64,
}

/// Why an assertion was refused. `code` is the contract's error
/// vocabulary — the handoff page surfaces it verbatim.
#[derive(Clone, Debug)]
pub struct Rejection {
    pub code: &'static str,
    pub message: String,
}

impl Rejection {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn invalid(message: impl Into<String>) -> Self {
        Self::new("assertion_invalid", message)
    }
}

impl From<Rejection> for Error {
    fn from(r: Rejection) -> Self {
        Error::invalid(r.code, r.message)
    }
}

/// The JWS header — exactly `alg`, `typ`, `kid` (contract §4).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JwsHeader {
    alg: String,
    typ: String,
    kid: String,
}

/// The closed claim set — every field required, nothing else permitted
/// (`client_id`, `redirect_uri`, `azp`, a method marker: the contract
/// forbids them so a second mint entry point stays contract-free).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    iss: String,
    aud: String,
    sub: String,
    email: String,
    name: String,
    company: String,
    role: String,
    iat: i64,
    exp: i64,
    jti: String,
}

/// An assertion parsed but not yet trusted — the daemon peeks at `kid`
/// and `iss` to locate the verifying key. Nothing in it is believed
/// until [`verify`] passes.
pub struct Parsed {
    header: JwsHeader,
    claims: Claims,
    /// The signed bytes: `<header>.<payload>` verbatim.
    signed: String,
    signature: Vec<u8>,
}

impl Parsed {
    pub fn kid(&self) -> &str {
        &self.header.kid
    }
    /// The claimed issuer — used ONLY to check the configured issuer
    /// (which in turn names the JWKS origin). Never a fetched URL's
    /// input before it matches [`Config::issuer`].
    pub fn issuer(&self) -> &str {
        &self.claims.iss
    }
    /// The claimed audience — the daemon's early refuse is this, so a
    /// foreign-board assertion never reaches the JWKS fetch.
    pub fn audience(&self) -> &str {
        &self.claims.aud
    }
    /// The single-use token id, for the daemon to remember until `exp`.
    pub fn jti(&self) -> &str {
        &self.claims.jti
    }
    pub fn exp(&self) -> i64 {
        self.claims.exp
    }
}

/// Split and shape-check the compact JWS: three base64url parts, the
/// exact header, the closed claim set. No signature or claim check
/// happens here.
pub fn parse(assertion: &str) -> std::result::Result<Parsed, Rejection> {
    let bad = |m: &str| Rejection::invalid(m.to_string());
    if assertion.is_empty() || assertion.len() > 8192 || !assertion.is_ascii() {
        return Err(bad("assertion is not a compact JWS"));
    }
    let mut parts = assertion.split('.');
    let (h, p, s) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s), None) if !h.is_empty() && !p.is_empty() && !s.is_empty() => {
            (h, p, s)
        }
        _ => return Err(bad("assertion is not a compact JWS")),
    };
    let decode = |part: &str| {
        URL_SAFE_NO_PAD
            .decode(part)
            .map_err(|_| bad("assertion is not base64url"))
    };
    let header: JwsHeader = serde_json::from_slice(&decode(h)?)
        .map_err(|_| bad("assertion header is not the contract shape"))?;
    if header.alg != "EdDSA" || header.typ != "JWT" || header.kid.is_empty() {
        return Err(bad("assertion header is not the contract shape"));
    }
    let claims: Claims = serde_json::from_slice(&decode(p)?)
        .map_err(|_| bad("assertion claims are not the contract shape"))?;
    if claims.sub.is_empty()
        || claims.sub.len() > 200
        || claims.email.is_empty()
        || claims.email.len() > 320
        || claims.jti.is_empty()
        || claims.jti.len() > 100
    {
        return Err(bad("assertion claims are not the contract shape"));
    }
    let signature = decode(s)?;
    if signature.len() != 64 {
        return Err(bad("assertion signature is not Ed25519"));
    }
    Ok(Parsed {
        header,
        claims,
        signed: format!("{h}.{p}"),
        signature,
    })
}

/// An Ed25519 public key from the platform JWKS.
#[derive(Clone)]
pub struct PublicKey {
    pub kid: String,
    /// The `x` parameter — 32 bytes.
    pub x: [u8; 32],
}

/// `GET {issuer}/.well-known/agenticos-board-jwks.json` (contract §6).
/// Bounded body, strict shape, `fail closed`: any transport, status or
/// schema error is an `Err` and verifies nothing.
pub fn fetch_keys(issuer: &str) -> Result<Vec<PublicKey>> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(JWKS_TIMEOUT))
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(agent);
    let url = format!(
        "{}/.well-known/agenticos-board-jwks.json",
        issuer.trim_end_matches('/')
    );
    let mut resp = agent
        .get(&url)
        .call()
        .map_err(|e| Error::internal(format!("platform JWKS fetch failed: {e}")))?;
    if resp.status() != 200 {
        return Err(Error::internal(format!(
            "platform JWKS fetch answered HTTP {}",
            resp.status().as_u16()
        )));
    }
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(JWKS_CAP as u64)
        .read_to_vec()
        .map_err(|e| Error::internal(format!("platform JWKS read failed: {e}")))?;
    let body: Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::internal(format!("platform JWKS is not JSON: {e}")))?;
    let keys = body["keys"]
        .as_array()
        .ok_or_else(|| Error::internal("platform JWKS has no keys array"))?;
    let mut out = Vec::new();
    for (i, k) in keys.iter().enumerate() {
        let well_formed = || {
            let kty = k["kty"].as_str()?;
            let crv = k["crv"].as_str()?;
            let kid = k["kid"].as_str()?;
            let x = k["x"].as_str()?;
            if kty != "OKP" || crv != "Ed25519" || kid.is_empty() {
                return None;
            }
            let raw = URL_SAFE_NO_PAD
                .decode(x)
                .or_else(|_| URL_SAFE.decode(x))
                .ok()?;
            let x: [u8; 32] = raw.try_into().ok()?;
            Some(PublicKey {
                kid: kid.to_string(),
                x,
            })
        };
        match well_formed() {
            Some(key) => out.push(key),
            None => {
                return Err(Error::internal(format!(
                    "platform JWKS key {i} is not an Ed25519 public key"
                )))
            }
        }
    }
    if out.is_empty() {
        return Err(Error::internal("platform JWKS published no keys"));
    }
    Ok(out)
}

/// The daemon's short JWKS memory (contract §6: cache briefly, refetch
/// on an unknown `kid`). One issuer per instance; a stale or
/// never-fetched cache refetches, and a miss for `kid` refetches once.
/// The key id is trusted only from the signed header.
#[derive(Default)]
pub struct JwksCache {
    issuer: String,
    keys: Vec<PublicKey>,
    fetched: i64,
}

impl JwksCache {
    /// The public key named `kid`, refreshing the cache when it is
    /// stale or the kid is unknown. `Err` is fail-closed — a JWKS that
    /// cannot be fetched verifies nothing.
    pub fn key(&mut self, issuer: &str, kid: &str, now: i64) -> Result<PublicKey> {
        let fresh = self.issuer == issuer
            && now.saturating_sub(self.fetched) <= JWKS_TTL_SECS
            && !self.keys.is_empty();
        let known = |keys: &[PublicKey]| keys.iter().find(|k| k.kid == kid).cloned();
        if fresh {
            if let Some(key) = known(&self.keys) {
                return Ok(key);
            }
        }
        let keys = fetch_keys(issuer)?;
        self.issuer = issuer.to_string();
        self.keys = keys;
        self.fetched = now;
        known(&self.keys).ok_or_else(|| {
            Error::invalid(
                "assertion_invalid",
                "the assertion names a key id the platform does not publish",
            )
        })
    }
}

/// The signature check: Ed25519 over `header.payload` — nothing else
/// signs, nothing else verifies.
fn signature_ok(parsed: &Parsed, key: &PublicKey) -> bool {
    use ring::signature::{UnparsedPublicKey, ED25519};
    UnparsedPublicKey::new(&ED25519, &key.x)
        .verify(parsed.signed.as_bytes(), &parsed.signature)
        .is_ok()
}

/// `identifier`-safe attribution derived from `sub` — the comment
/// author and monitor-ack `by` grammar (`[A-Za-z0-9_-]`, ≤64). `sub`
/// is the durable id; the full email and name ride in the session view
/// for display.
fn handle_of(sub: &str, email: &str) -> String {
    let clean = |raw: &str| -> String {
        raw.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .take(64)
            .collect()
    };
    let h = clean(sub);
    if !h.is_empty() {
        return h;
    }
    let local = email.split('@').next().unwrap_or_default();
    clean(local)
}

/// Verify a parsed assertion end to end (contract §4): signature
/// against the platform key first — nothing else is trusted until it
/// passes — then `iss`, `aud`, the `exp`/`iat` bounds, `company` and
/// the §2 role map. The early `iss`/`aud` checks the daemon ran to
/// locate `key` run again: `verify` is whole on its own.
pub fn verify(
    parsed: &Parsed,
    config: &Config,
    key: &PublicKey,
    now: i64,
) -> std::result::Result<Identity, Rejection> {
    if parsed.header.kid != key.kid {
        return Err(Rejection::invalid(
            "the assertion's key id does not name the verifying key",
        ));
    }
    if !signature_ok(parsed, key) {
        return Err(Rejection::invalid(
            "the assertion's signature does not verify",
        ));
    }
    let c = &parsed.claims;
    if c.iss != config.issuer {
        return Err(Rejection::new(
            "issuer_mismatch",
            "the assertion was not issued by this board's platform",
        ));
    }
    if c.aud != config.host {
        return Err(Rejection::new(
            "audience_mismatch",
            "the assertion was minted for another board host",
        ));
    }
    if now > c.exp + SKEW_SECS || c.iat > now + IAT_AHEAD_SECS || c.exp - c.iat > MAX_LIFE_SECS {
        return Err(Rejection::new(
            "assertion_expired",
            "the assertion is expired or outside its time bounds",
        ));
    }
    if c.exp - c.iat < 0 {
        return Err(Rejection::invalid("the assertion's exp precedes its iat"));
    }
    if c.company != config.company {
        return Err(Rejection::new(
            "not_a_member",
            "the assertion names a company this instance does not serve",
        ));
    }
    let role = match c.role.as_str() {
        "owner" => Role::Operator,
        "member" => Role::Member,
        _ => {
            return Err(Rejection::new(
                "role_unmapped",
                "the assertion's membership role has no board role",
            ))
        }
    };
    Ok(Identity {
        sub: c.sub.clone(),
        email: c.email.clone(),
        name: c.name.chars().take(200).collect(),
        role,
        jti: c.jti.clone(),
        exp: c.exp,
    })
}

/// The `ui sessions`/`board_session_open` view of a verified user —
/// what the session stores and audit surfaces.
impl Identity {
    /// As the session row's `user` record: identifier-shaped `handle`
    /// for field-limited attributions, `email`/`name` for display.
    pub fn user(&self) -> crate::operator_auth::BoardUser {
        crate::operator_auth::BoardUser {
            sub: self.sub.clone(),
            email: self.email.clone(),
            name: self.name.clone(),
            role: self.role.as_str().to_string(),
            handle: handle_of(&self.sub, &self.email),
        }
    }
}

/// One DNS label or host[:port]: the board's public name is matched
/// verbatim against `aud` claims and `Host` headers, so it must carry
/// no scheme or path.
pub fn valid_aud(host: &str) -> bool {
    let (name, port) = host.rsplit_once(':').unwrap_or((host, ""));
    let name_ok = !name.is_empty()
        && name.len() <= 253
        && !name.starts_with('.')
        && !name.ends_with('.')
        && name.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        });
    let port_ok = port.is_empty() || port.bytes().all(|b| b.is_ascii_digit());
    name_ok && port_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use serde_json::json;

    // RFC 8032 §7.1 TEST 1's seed — fixed, so every minted assertion is
    // deterministic; `FOREIGN` is a key the platform never publishes.
    const SEED: [u8; 32] = [
        0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec, 0x2c,
        0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03, 0x1c, 0xae,
        0x7f, 0x60,
    ];
    const FOREIGN: [u8; 32] = [0x5e; 32];
    const NOW: i64 = 1_800_000_000;

    fn signer(seed: &[u8; 32]) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(seed).unwrap()
    }

    fn key() -> PublicKey {
        let mut x = [0u8; 32];
        x.copy_from_slice(signer(&SEED).public_key().as_ref());
        PublicKey {
            kid: "k1".to_string(),
            x,
        }
    }

    fn config() -> Config {
        Config {
            host: "acme.cadencecloud.app".to_string(),
            issuer: "https://platform.test".to_string(),
            company: "co_1".to_string(),
        }
    }

    fn header() -> Value {
        json!({"alg": "EdDSA", "typ": "JWT", "kid": "k1"})
    }

    fn claims() -> Value {
        json!({
            "iss": "https://platform.test",
            "aud": "acme.cadencecloud.app",
            "sub": "usr_9",
            "email": "fable@example.com",
            "name": "Fable Chen",
            "company": "co_1",
            "role": "owner",
            "iat": NOW,
            "exp": NOW + 30,
            "jti": "jti-1",
        })
    }

    fn b64(bytes: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(bytes)
    }

    /// Sign `header.payload` and assemble the compact JWS — exactly the
    /// shape the platform mints.
    fn mint(signer: &Ed25519KeyPair, header: &Value, claims: &Value) -> String {
        let h = b64(&serde_json::to_vec(header).unwrap());
        let p = b64(&serde_json::to_vec(claims).unwrap());
        let signed = format!("{h}.{p}");
        format!("{signed}.{}", b64(signer.sign(signed.as_bytes()).as_ref()))
    }

    /// Header/claims JSON without the signature — for assertions whose
    /// shape must fail before any crypto runs.
    fn unsigned(header: &Value, claims: &Value, sig: &[u8]) -> String {
        format!(
            "{}.{}.{}",
            b64(&serde_json::to_vec(header).unwrap()),
            b64(&serde_json::to_vec(claims).unwrap()),
            b64(sig)
        )
    }

    #[test]
    fn a_valid_assertion_verifies_and_maps_roles() {
        for (role, want) in [("owner", Role::Operator), ("member", Role::Member)] {
            let mut c = claims();
            c["role"] = json!(role);
            let parsed = parse(&mint(&signer(&SEED), &header(), &c)).unwrap();
            let id = verify(&parsed, &config(), &key(), NOW).unwrap();
            assert_eq!(id.role, want, "{role}");
            assert_eq!(id.sub, "usr_9");
            assert_eq!(id.email, "fable@example.com");
            assert_eq!(id.name, "Fable Chen");
            assert_eq!(id.jti, "jti-1");
            assert_eq!(id.exp, NOW + 30);
            let u = id.user();
            assert_eq!(u.sub, "usr_9");
            assert_eq!(u.is_operator(), want.is_operator());
        }
        // A host[:port] `aud` is exact-match too (the local-dev shape).
        let mut cfg = config();
        cfg.host = "acme.board.localhost:3111".to_string();
        let mut c = claims();
        c["aud"] = json!("acme.board.localhost:3111");
        let parsed = parse(&mint(&signer(&SEED), &header(), &c)).unwrap();
        assert!(verify(&parsed, &cfg, &key(), NOW).is_ok());
    }

    /// Every non-contract shape is refused at parse — before any key or
    /// claim is trusted. The claims set is closed: `client_id`, `azp`
    /// and friends must fail, so no second mint entry point sneaks in.
    #[test]
    fn parse_refuses_everything_not_the_contract_shape() {
        for bad in ["", "a.b", "a.b.c.d", "..", "a..b", "a.b.", "xyzzy.-."] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(parse(&"x".repeat(8193)).is_err());
        assert!(parse(&format!("{}.e30.e30", "Ã".repeat(4))).is_err());
        // The header is exactly {alg:"EdDSA", typ:"JWT", kid}.
        for h in [
            json!({"alg": "none", "typ": "JWT", "kid": "k1"}),
            json!({"alg": "RS256", "typ": "JWT", "kid": "k1"}), // alg confusion
            json!({"alg": "EdDSA", "kid": "k1"}),               // no typ
            json!({"alg": "EdDSA", "typ": "JWT", "kid": ""}),   // empty kid
            json!({"alg": "EdDSA", "typ": "JWT"}),              // no kid
            json!({"alg": "EdDSA", "typ": "JWT", "kid": "k1", "crit": ["b64"]}),
        ] {
            let a = unsigned(&h, &claims(), &[0u8; 64]);
            assert_eq!(parse(&a).err().unwrap().code, "assertion_invalid", "{h}");
        }
        // The claim set is closed and bounded.
        for c in [
            {
                let mut c = claims();
                c.as_object_mut().unwrap().remove("jti");
                c
            },
            {
                let mut c = claims();
                c["client_id"] = json!("x");
                c
            },
            {
                let mut c = claims();
                c["azp"] = json!("x");
                c
            },
            {
                let mut c = claims();
                c["sub"] = json!("");
                c
            },
            {
                let mut c = claims();
                c["iat"] = json!("soon");
                c
            },
        ] {
            let a = unsigned(&header(), &c, &[0u8; 64]);
            assert_eq!(parse(&a).err().unwrap().code, "assertion_invalid", "{c}");
        }
        // The signature is exactly 64 bytes of Ed25519.
        let a = unsigned(&header(), &claims(), &[0u8; 32]);
        assert!(parse(&a).is_err());
        // ...and none of this is lost under a valid signature: a bad
        // header signed honestly still fails.
        let a = mint(&signer(&SEED), &json!({"alg": "none"}), &claims());
        assert!(parse(&a).is_err());
    }

    /// Verify: signature first, then every claim against the configured
    /// trust root — each refusal carries the contract's code.
    #[test]
    fn verify_refuses_every_forgery_and_mismatch() {
        let signed = |c: &Value| parse(&mint(&signer(&SEED), &header(), c)).unwrap();
        let code = |c: &Value| verify(&signed(c), &config(), &key(), NOW).unwrap_err().code;

        // Signed by a key the platform never published.
        let forged = parse(&mint(&signer(&FOREIGN), &header(), &claims())).unwrap();
        assert_eq!(
            verify(&forged, &config(), &key(), NOW).unwrap_err().code,
            "assertion_invalid"
        );
        // The header's `kid` names a key other than the verifying one.
        let mut wrong_kid = key();
        wrong_kid.kid = "k2".to_string();
        assert_eq!(
            verify(&signed(&claims()), &config(), &wrong_kid, NOW)
                .unwrap_err()
                .code,
            "assertion_invalid"
        );
        // iss / aud / company / role.
        let mut c = claims();
        c["iss"] = json!("https://elsewhere.test");
        assert_eq!(code(&c), "issuer_mismatch");
        let mut c = claims();
        c["aud"] = json!("other.cadencecloud.app");
        assert_eq!(code(&c), "audience_mismatch");
        let mut c = claims();
        c["aud"] = json!("acme.cadencecloud.app.evil.test");
        assert_eq!(code(&c), "audience_mismatch");
        let mut c = claims();
        c["company"] = json!("co_2");
        assert_eq!(code(&c), "not_a_member");
        for role in ["admin", "viewer", "OWNER", ""] {
            let mut c = claims();
            c["role"] = json!(role);
            assert_eq!(code(&c), "role_unmapped", "{role}");
        }
        // Time bounds: expired (past skew), minted too far ahead, a life
        // over the contract's 60 s, and exp before iat.
        let mut c = claims();
        c["iat"] = json!(NOW - 120);
        c["exp"] = json!(NOW - SKEW_SECS - 1);
        assert_eq!(code(&c), "assertion_expired");
        let mut c = claims();
        c["iat"] = json!(NOW + IAT_AHEAD_SECS + 1);
        c["exp"] = json!(NOW + IAT_AHEAD_SECS + 30);
        assert_eq!(code(&c), "assertion_expired");
        let mut c = claims();
        c["exp"] = json!(NOW + 61);
        assert_eq!(code(&c), "assertion_expired");
        let mut c = claims();
        c["iat"] = json!(NOW);
        c["exp"] = json!(NOW - 1);
        assert_eq!(code(&c), "assertion_invalid");
        // Skew boundaries still pass: exp inside skew, iat inside the
        // 60 s ahead-allowance, exactly-60 s lives.
        let mut c = claims();
        c["iat"] = json!(NOW - 60);
        c["exp"] = json!(NOW);
        assert!(verify(&signed(&c), &config(), &key(), NOW).is_ok());
        let mut c = claims();
        c["iat"] = json!(NOW - 30);
        c["exp"] = json!(NOW + 30);
        assert!(verify(&signed(&c), &config(), &key(), NOW).is_ok());
        let mut c = claims();
        c["iat"] = json!(NOW + 30);
        c["exp"] = json!(NOW + 60);
        assert!(verify(&signed(&c), &config(), &key(), NOW).is_ok());
        let mut c = claims();
        c["exp"] = json!(NOW + 60);
        assert!(verify(&signed(&c), &config(), &key(), NOW).is_ok());
    }

    /// The trust root: written `0600` under the operator directory,
    /// read back whole, and a missing file is "not configured" — never
    /// a guess.
    #[test]
    fn config_round_trips_private() {
        use std::os::unix::fs::PermissionsExt;
        let s = tempfile::TempDir::new().unwrap();
        assert!(!configured(s.path()));
        write_config(s.path(), &config()).unwrap();
        assert!(configured(s.path()));
        assert_eq!(read_config(s.path()).unwrap(), config());
        let md = std::fs::metadata(config_path(s.path())).unwrap();
        assert_eq!(md.permissions().mode() & 0o777, 0o600);
        // Unknown fields and garbage are refused, not defaulted.
        std::fs::write(
            config_path(s.path()),
            br#"{"host":"x","issuer":"y","company":"z","jwks":"http://evil"}"#,
        )
        .unwrap();
        assert!(read_config(s.path()).is_err());
        std::fs::write(config_path(s.path()), b"not json").unwrap();
        assert!(!configured(s.path()));
    }

    #[test]
    fn public_names_validate() {
        for ok in [
            "acme.cadencecloud.app",
            "acme-staging.cadencecloud.app",
            "acme.board.localhost",
            "acme.board.localhost:3111",
            "a.b.c.d:65535",
        ] {
            assert!(valid_aud(ok), "{ok}");
        }
        for bad in [
            "",
            "https://acme.cadencecloud.app",
            "acme.cadencecloud.app/",
            "Acme.cadencecloud.app",
            "acme.cadencecloud.app:abc",
            ".acme.app",
            "acme..app",
            "acme.app.",
            "acme_app",
            "acme app",
        ] {
            assert!(!valid_aud(bad), "{bad}");
        }
    }

    /// Attribution handles land in the comment-author grammar —
    /// `[A-Za-z0-9_-]`, ≤64 — whatever `sub` carries; an empty cleaned
    /// `sub` falls back to the email's local part.
    #[test]
    fn attribution_handles_fit_the_comment_grammar() {
        assert_eq!(handle_of("usr_9", "f@x.co"), "usr_9");
        assert_eq!(handle_of("auth0|usr:9/x", ""), "auth0_usr_9_x");
        assert_eq!(handle_of(&"s".repeat(100), ""), "s".repeat(64));
        assert_eq!(handle_of("", "fable@x.co"), "fable");
        assert_eq!(handle_of("", ""), "");
    }
}
