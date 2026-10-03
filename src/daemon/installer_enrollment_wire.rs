//! CAD-1061 — the installer-enrollment **receipt consumer** (verify-only).
//!
//! A private, format-evidence-only codec for the *installer-enrollment
//! signed-wire* contract (`aos121-installer-enrollment-signed-wire-contract`
//! v1, SHA256 `07d8a6760a2d809d5442a1a2cd330e044b4c03cadf61232060e53859c6c150d4`).
//! It verifies a domain-separated Ed25519 compact envelope over the canonical
//! PR312 `InstallerBinding` — the receipt a future fixed host owner will
//! present *before* it releases a launch barrier. **Nothing here is launch,
//! grant, current or durable authority.**
//!
//! Distinct from `supervisor_grant` (CAD-1017): that grant binds a
//! `SupervisorChallenge` over `cadence.supervisor-launch-grant.v1`; this
//! receipt binds the *full installer-enrollment binding* (installer
//! pid/starttime/artifacts and the barrier nonce) over
//! `cadence.installer-enrollment-receipt.v1`. The two are not
//! interchangeable — an old grant envelope must not pass this verifier, and
//! this module never widens the grant parser (that file stays unchanged).
//!
//! Trust model, kept deliberately narrow this batch:
//!   * **Verify-only evidence.** [`verify_receipt_format`] returns a
//!     non-forgeable [`VerifiedReceipt`] — proof a well-formed, canonically
//!     encoded, correctly-signed receipt matched a pinned trust key. It is
//!     *not* `GuestCtx`, launch eligibility, a consume, an enroll, a release
//!     or a process start; verification performs none of those.
//!   * **Immutable pinned trust only.** Key lookup is the exact
//!     `(issuer, kid, keyVersion)` triple against a private immutable trust
//!     set — never a URL, JWKS, embedded key, env key, caller-selected
//!     algorithm or trust root. No key is provisioned this batch
//!     ([`PRODUCTION_TRUST_KEYS`] is empty) so every live lookup refuses;
//!     `#[cfg(test)]` injects the published synthetic RFC8032 key.
//!   * **Canonical bytes are the binding.** `H.P` are unpadded base64url of
//!     canonical UTF-8. The decoder re-encodes the validated value and
//!     requires byte-for-byte equality with the decoded input, so duplicate
//!     keys, escaped spellings, reordered keys, whitespace, padding and
//!     non-shortest integer forms all refuse — a signature-valid but
//!     noncanonical document never verifies. `serde_json`'s collapsed map is
//!     *not* relied on for duplicates.
//!   * **Safe-number vs decimal-string separation.** All JSON numbers are
//!     unsigned JS-safe integers `≤ 2^53-1`; both `starttime` values are
//!     decimal *strings* (`u64` range, no leading zero, never a JS Number).
//!   * **Expiry is diagnostic, not authority.** Exclusive expiry and a
//!     monotonic-budget refusal are honest checks inside a fixed request
//!     bound; they do not elect a current state, settle an obligation or
//!     release capacity.
//!
//! Production trusted-key/measurement/current-state factories remain
//! unavailable before reading any caller proof; `production_*` factories are
//! `Err`/unavailable. No CLI/DO/RPC/listener, no consumer mutation, no
//! uid-21000 peer admission, no signer, no key provisioning this batch.

#![allow(dead_code)]

use crate::error::{Error, Result};

// ────────────────── private reviewed constants (never caller-supplied) ────

/// The fixed domain tag separating this receipt from every other Ed25519 doc
/// (including the `cadence.supervisor-launch-grant.v1` grant).
const RECEIPT_DOMAIN: &str = "cadence.installer-enrollment-receipt.v1";

/// The pinned production trust set — permanently empty this batch: no issuer
/// key is provisioned, so every live lookup refuses. Tests inject synthetic
/// keys via the `keys` parameter, never a caller/JWKS/env source.
const PRODUCTION_TRUST_KEYS: &[TrustedKey] = &[];

/// Envelope ASCII bound (whole `H.P.S` string).
const MAX_ENVELOPE_BYTES: usize = 16384;
/// Pre-decode base64url segment caps: H ≤342, P ≤10923, S exactly 86.
const MAX_HEADER_B64: usize = 342;
const MAX_PAYLOAD_B64: usize = 10923;
const SIG_B64_LEN: usize = 86;
/// Decoded byte bounds: header ≤256, payload ≤8192, signature exactly 64.
const MAX_HEADER_BYTES: usize = 256;
const MAX_PAYLOAD_BYTES: usize = 8192;
const SIG_BYTES: usize = 64;
/// Canonicalization depth bound — the validated binding nests finitely (≤7).
const MAX_CANON_DEPTH: usize = 8;
/// `0 < expiresAtMs - issuedAtMs ≤ 300000`.
const MAX_WINDOW_MS: u64 = 300_000;
/// Fixed per-request monotonic budget (a stalled operation cannot succeed).
const REQUEST_BUDGET_MS: u64 = 10_000;
/// `keyVersion` upper bound (positive int32).
const MAX_KEY_VERSION: u64 = 2_147_483_647;
/// pid upper bound (kernel pid_max).
const MAX_PID: u64 = 4_194_304;
/// The fixed installer uid/gid the binding pins.
const INSTALLER_ID: u64 = 21000;
/// JS-safe integer bound `2^53-1`.
const MAX_SAFE: u64 = 9007199254740991;

// ─────────────────────── private trusted-key identity ─────────────────────

/// One immutable trusted-key entry — the exact `(issuer, kid, keyVersion)`
/// pin and its raw 32-byte Ed25519 public key. Fields are private; the only
/// production source is [`production_trust_set`] (unavailable). Tests build
/// synthetic keys via `capture_test`; no caller/JWKS/env path constructs one.
#[derive(Clone, Debug)]
pub(crate) struct TrustedKey {
    issuer: String,
    kid: String,
    key_version: u64,
    public_key: [u8; 32],
}

impl TrustedKey {
    /// `#[cfg(test)]`-only synthetic-key builder — explicit public test
    /// material, never a production keyring, signer or trust root.
    #[cfg(test)]
    pub(super) fn capture_test(issuer: &str, kid: &str, key_version: u64, public_key: [u8; 32]) -> Self {
        Self {
            issuer: issuer.to_string(),
            kid: kid.to_string(),
            key_version,
            public_key,
        }
    }
}

/// The private production trust-set factory — permanently `Err` until the
/// operator approves a public-key manifest, an initial trusted version and a
/// rotation/revocation policy. No live caller may supply trust.
pub(crate) fn production_trust_set() -> Result<&'static [TrustedKey]> {
    let _ = PRODUCTION_TRUST_KEYS; // empty — nothing immutable to pin
    Err(Error::rejected(
        "production installer-enrollment trust unavailable — no approved \
         public-key manifest, trusted keyVersion or rotation policy; a \
         receipt can never mint launch eligibility (UNKNOWN, stays refused)",
    ))
}

// ─────────────────────── typed receipt evidence shapes ────────────────────

/// `header` — the exact field set `{alg,issuer,keyVersion,kid,type,version}`.
/// `alg`/`issuer`/`type`/`version` are fixed literals; `kid` selects the key
/// id and `keyVersion` the immutable trusted version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReceiptHeader {
    /// Always `"Ed25519"` — pure Ed25519, never negotiated.
    pub alg: String,
    /// Always `"agenticos-native-owner"`.
    pub issuer: String,
    /// Integer `1..=2147483647` selecting the trusted-key version.
    pub key_version: u64,
    /// `^[A-Za-z0-9._-]{1,64}$` — the key id (synthetic this batch).
    pub kid: String,
    /// Always `"installer-enrollment-receipt"`.
    pub receipt_type: String,
    /// Always `1`.
    pub version: u64,
}

/// `identity` — the launch identity inside `challenge.launch.request`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReceiptIdentity {
    /// `^[A-Za-z0-9._-]{1,200}$` — this receipt profile refuses non-ASCII /
    /// control / colon identities; never normalized or remapped.
    pub company: String,
    pub instance: String,
    /// Always `"native"`.
    pub backend: String,
    /// `"basic" | "standard-1"`.
    pub tier: String,
    /// JS-safe non-negative integer.
    pub generation: u64,
    /// Optional `"baseline"` — presence is part of the canonical binding.
    pub image_lane: Option<String>,
}

/// `launch.request` — the launch obligation identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReceiptRequest {
    pub identity: ReceiptIdentity,
    /// `"fresh_start" | "reconstruct"`.
    pub purpose: String,
    /// The external challenge — a UUID.
    pub challenge: String,
    /// The OCI image `ref@sha256:<64-hex>` — a binding string, not a fetch.
    pub image: String,
}

/// `launch` — the request plus the global epoch it binds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReceiptLaunch {
    pub request: ReceiptRequest,
    /// The current global epoch (JS-safe non-negative integer).
    pub epoch: u64,
}

/// `recipient` — the live recipient instance the receipt binds (the
/// supervisor's pid/starttime/generation and the enrolled nonce).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReceiptRecipient {
    /// pid 1..=4194304.
    pub pid: u32,
    /// `/proc/<pid>` starttime — decimal string, u64 range, no leading zero.
    pub starttime: String,
    /// The recipient's enrolled generation — 32 lowercase-hex.
    pub generation: String,
    /// The enrolled pending nonce — a UUID.
    pub nonce: String,
}

/// `pins` — the pinned artifact digests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReceiptPins {
    /// Source commit — 40-hex SHA1.
    pub source: String,
    /// OCI image — must equal `launch.request.image`.
    pub image: String,
    pub helper: String,
    pub node: String,
    /// The Pi JS *graph* digest (`piGraph`).
    pub pi_graph: String,
    pub policy: String,
}

/// `lineage` — the restore-lineage the receipt binds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReceiptLineage {
    /// `^[A-Za-z0-9_-]{1,128}$`.
    pub reference: String,
    /// JS-safe non-negative integer.
    pub database_epoch: u64,
}

/// `challenge` — the canonical supervisor challenge embedded in the binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReceiptChallenge {
    pub launch: ReceiptLaunch,
    pub recipient: ReceiptRecipient,
    pub pins: ReceiptPins,
    pub lineage: ReceiptLineage,
}

/// `binding.installer` — the installer process the receipt binds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstallerRecord {
    /// pid 1..=4194304.
    pub pid: u32,
    /// `/proc/<pid>` starttime — decimal string, u64 range, no leading zero.
    pub starttime: String,
    /// Exactly `21000`.
    pub uid: u64,
    /// Exactly `21000`.
    pub gid: u64,
    /// 64-lowerhex artifact digests.
    pub client_digest: String,
    pub carrier_digest: String,
    pub observer_digest: String,
}

/// `binding` — the complete PR312 `InstallerBinding` plus `barrierNonce` and
/// `expiresAtMs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstallerBinding {
    /// Always `1`.
    pub version: u64,
    pub challenge: ReceiptChallenge,
    /// The installer process record.
    pub installer: InstallerRecord,
    /// The barrier nonce — a UUID.
    pub barrier_nonce: String,
    /// Bounded expiry — a JS-safe ms timestamp.
    pub expires_at_ms: u64,
}

/// `payload` — `{binding,issuedAtMs,phase,version}`; `phase` is `"prepared"`
/// only. The receipt is evidence a *prepared* enrollment was signed — never
/// a consumed/current one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReceiptPayload {
    /// Always `1`.
    pub version: u64,
    /// Always `"prepared"`.
    pub phase: String,
    /// JS-safe issue time (ms).
    pub issued_at_ms: u64,
    pub binding: InstallerBinding,
}

/// The verified-format evidence — proof a canonical receipt verified against
/// a pinned trust key. **Not** authority: it cannot open a factory, obtain a
/// `GuestCtx`, consume, enroll, retire, release or start a process. Private
/// fields; produced only inside this module by the verify path.
#[derive(Debug)]
pub(crate) struct VerifiedReceipt {
    header: ReceiptHeader,
    payload: ReceiptPayload,
    /// Canonical `binding` bytes (UTF-8) — for exact byte-equality against the
    /// externally enrolled durable record (including `imageLane` presence).
    binding_json: Vec<u8>,
}

impl VerifiedReceipt {
    /// The verified header (issuer/kid/keyVersion already matched a pin).
    pub(crate) fn header(&self) -> &ReceiptHeader {
        &self.header
    }
    /// The verified payload (the complete binding).
    pub(crate) fn payload(&self) -> &ReceiptPayload {
        &self.payload
    }
    /// The canonical binding bytes the durable record must equal *exactly* —
    /// structural-equality normalization is insufficient for the wire.
    pub(crate) fn binding_json(&self) -> &[u8] {
        &self.binding_json
    }
}

// ─────────────────────── strict field validators ─────────────────────────

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn is_uuid(s: &str) -> bool {
    // ^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$ — lowercase form.
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    for (i, &c) in b.iter().enumerate() {
        if matches!(i, 8 | 13 | 18 | 23) {
            if c != b'-' {
                return false;
            }
        } else if !(c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
            return false;
        }
    }
    true
}
/// `^[A-Za-z0-9./:_-]{1,440}@sha256:[a-f0-9]{64}$`, total ≤512 bytes — a
/// binding string, not a fetched URL or evidence of an immutable image.
fn is_oci_image(s: &str) -> bool {
    if s.len() > 512 {
        return false;
    }
    match s.split_once('@') {
        Some((ref_, d)) => {
            !ref_.is_empty()
                && ref_.len() <= 440
                && ref_.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'/' | b':' | b'_' | b'-')
                })
                && d.strip_prefix("sha256:")
                    .map(|h| is_lower_hex(h, 64))
                    .unwrap_or(false)
        }
        None => false,
    }
}
/// Decimal-string u64: `^[1-9][0-9]{0,19}$`, ≤ `u64::MAX` — never a JS Number.
fn is_starttime(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 20
        && s.starts_with(|c: char| c.is_ascii_digit() && c != '0')
        && s.bytes().all(|b| b.is_ascii_digit())
        && s.parse::<u64>().is_ok()
}
fn is_lineage_ref(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
/// `^[A-Za-z0-9._-]{1,64}$` — the receipt `kid` charset (no ':' like the grant).
fn is_kid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}
/// `^[A-Za-z0-9._-]{1,200}$` — company/instance (this receipt profile refuses
/// non-ASCII/control/colon identities; never normalized or remapped).
fn is_identity_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A JS-safe non-negative integer `0..=2^53-1`, no `-0`/float/exponent.
fn safe_u64(v: &serde_json::Value, name: &str) -> Result<u64> {
    match v.as_u64() {
        Some(n) if n <= MAX_SAFE => Ok(n),
        _ => Err(Error::rejected(format!(
            "{name} is not a JS-safe unsigned integer"
        ))),
    }
}

/// A JSON object must contain EXACTLY `names` — deny unknown/coercible fields.
/// (Duplicate-key *bytes* are caught later by canonical re-equality; serde's
/// collapsed map is not relied on for that.)
fn obj_exact<'a>(
    v: &'a serde_json::Value,
    names: &[&str],
) -> Result<&'a serde_json::Map<String, serde_json::Value>> {
    let m = v
        .as_object()
        .ok_or_else(|| Error::rejected("expected a JSON object"))?;
    if m.len() != names.len()
        || !m.keys().all(|k| names.contains(&k.as_str()))
        || !names.iter().all(|k| m.contains_key(*k))
    {
        return Err(Error::rejected(format!(
            "object must contain exactly {:?} (got {:?})",
            names,
            m.keys().collect::<Vec<_>>()
        )));
    }
    Ok(m)
}
fn sget<'a>(m: &'a serde_json::Map<String, serde_json::Value>, k: &str) -> Result<&'a str> {
    m.get(k)
        .and_then(|x| x.as_str())
        .ok_or_else(|| Error::rejected(format!("field '{k}' missing/not a string")))
}
fn uget(m: &serde_json::Map<String, serde_json::Value>, k: &str) -> Result<u64> {
    match m.get(k) {
        Some(v) => safe_u64(v, &format!("field '{k}'")),
        None => Err(Error::rejected(format!("field '{k}' missing"))),
    }
}

// ─────────────────────── receipt schema validation ───────────────────────

fn parse_header(v: &serde_json::Value) -> Result<ReceiptHeader> {
    let m = obj_exact(
        v,
        &["alg", "issuer", "keyVersion", "kid", "type", "version"],
    )?;
    if sget(m, "alg")? != "Ed25519" {
        return Err(Error::rejected("header.alg must be \"Ed25519\""));
    }
    if sget(m, "issuer")? != "agenticos-native-owner" {
        return Err(Error::rejected("header.issuer is not the pinned issuer"));
    }
    if sget(m, "type")? != "installer-enrollment-receipt" {
        return Err(Error::rejected(
            "header.type is not installer-enrollment-receipt",
        ));
    }
    if uget(m, "version")? != 1 {
        return Err(Error::rejected("header.version must be 1"));
    }
    let key_version = uget(m, "keyVersion")?;
    if key_version == 0 || key_version > MAX_KEY_VERSION {
        return Err(Error::rejected("header.keyVersion out of range"));
    }
    let kid = sget(m, "kid")?;
    if !is_kid(kid) {
        return Err(Error::rejected("header.kid is malformed"));
    }
    Ok(ReceiptHeader {
        alg: "Ed25519".to_string(),
        issuer: "agenticos-native-owner".to_string(),
        key_version,
        kid: kid.to_string(),
        receipt_type: "installer-enrollment-receipt".to_string(),
        version: 1,
    })
}

fn parse_identity(v: &serde_json::Value) -> Result<ReceiptIdentity> {
    let m = v
        .as_object()
        .ok_or_else(|| Error::rejected("identity is not an object"))?;
    let want = ["backend", "company", "generation", "instance", "tier"];
    let optional = "imageLane";
    if m.len() < want.len()
        || m.len() > want.len() + 1
        || !want.iter().all(|k| m.contains_key(*k))
        || m.keys()
            .any(|k| !want.contains(&k.as_str()) && k != optional)
    {
        return Err(Error::rejected(
            "identity has unknown/duplicate/missing fields",
        ));
    }
    let company = sget(m, "company")?;
    let instance = sget(m, "instance")?;
    if !is_identity_name(company) || !is_identity_name(instance) {
        return Err(Error::rejected("identity company/instance is malformed"));
    }
    let backend = sget(m, "backend")?;
    if backend != "native" {
        return Err(Error::rejected("identity.backend must be \"native\""));
    }
    let tier = sget(m, "tier")?;
    if tier != "basic" && tier != "standard-1" {
        return Err(Error::rejected("identity.tier is not basic|standard-1"));
    }
    let generation = uget(m, "generation")?;
    let image_lane = match m.get(optional) {
        Some(x) => {
            let s = x
                .as_str()
                .ok_or_else(|| Error::rejected("identity.imageLane is not a string"))?;
            if s != "baseline" {
                return Err(Error::rejected("identity.imageLane must be \"baseline\""));
            }
            Some(s.to_string())
        }
        None => None,
    };
    Ok(ReceiptIdentity {
        company: company.to_string(),
        instance: instance.to_string(),
        backend: backend.to_string(),
        tier: tier.to_string(),
        generation,
        image_lane,
    })
}

fn parse_request(v: &serde_json::Value) -> Result<ReceiptRequest> {
    let m = obj_exact(v, &["challenge", "identity", "image", "purpose"])?;
    let identity = parse_identity(m.get("identity").unwrap())?;
    let purpose = sget(m, "purpose")?;
    if purpose != "fresh_start" && purpose != "reconstruct" {
        return Err(Error::rejected("launch.request.purpose is invalid"));
    }
    let challenge = sget(m, "challenge")?;
    if !is_uuid(challenge) {
        return Err(Error::rejected("launch.request.challenge is not a UUID"));
    }
    let image = sget(m, "image")?;
    if !is_oci_image(image) {
        return Err(Error::rejected("launch.request.image is not an OCI ref"));
    }
    Ok(ReceiptRequest {
        identity,
        purpose: purpose.to_string(),
        challenge: challenge.to_string(),
        image: image.to_string(),
    })
}

fn parse_launch(v: &serde_json::Value) -> Result<ReceiptLaunch> {
    let m = obj_exact(v, &["epoch", "request"])?;
    Ok(ReceiptLaunch {
        request: parse_request(m.get("request").unwrap())?,
        epoch: uget(m, "epoch")?,
    })
}

fn parse_recipient(v: &serde_json::Value) -> Result<ReceiptRecipient> {
    let m = obj_exact(v, &["generation", "nonce", "pid", "starttime"])?;
    let pid = uget(m, "pid")?;
    if pid == 0 || pid > MAX_PID {
        return Err(Error::rejected("recipient.pid out of range"));
    }
    let starttime = sget(m, "starttime")?;
    if !is_starttime(starttime) {
        return Err(Error::rejected(
            "recipient.starttime is not a decimal u64 string",
        ));
    }
    let generation = sget(m, "generation")?;
    if !is_lower_hex(generation, 32) {
        return Err(Error::rejected("recipient.generation is not 32-lowerhex"));
    }
    let nonce = sget(m, "nonce")?;
    if !is_uuid(nonce) {
        return Err(Error::rejected("recipient.nonce is not a UUID"));
    }
    Ok(ReceiptRecipient {
        pid: pid as u32,
        starttime: starttime.to_string(),
        generation: generation.to_string(),
        nonce: nonce.to_string(),
    })
}

fn parse_pins(v: &serde_json::Value) -> Result<ReceiptPins> {
    let m = obj_exact(
        v,
        &["helper", "image", "node", "piGraph", "policy", "source"],
    )?;
    let source = sget(m, "source")?;
    if !is_lower_hex(source, 40) {
        return Err(Error::rejected("pins.source is not a 40-hex SHA1"));
    }
    let image = sget(m, "image")?;
    if !is_oci_image(image) {
        return Err(Error::rejected("pins.image is not an OCI ref"));
    }
    let sha = |k: &str| -> Result<String> {
        let s = sget(m, k)?;
        if !is_lower_hex(s, 64) {
            return Err(Error::rejected(format!("pins.{k} is not a 64-hex sha256")));
        }
        Ok(s.to_string())
    };
    Ok(ReceiptPins {
        source: source.to_string(),
        image: image.to_string(),
        helper: sha("helper")?,
        node: sha("node")?,
        pi_graph: sha("piGraph")?,
        policy: sha("policy")?,
    })
}

fn parse_lineage(v: &serde_json::Value) -> Result<ReceiptLineage> {
    let m = obj_exact(v, &["databaseEpoch", "reference"])?;
    let reference = sget(m, "reference")?;
    if !is_lineage_ref(reference) {
        return Err(Error::rejected("lineage.reference is not a bounded ref"));
    }
    Ok(ReceiptLineage {
        reference: reference.to_string(),
        database_epoch: uget(m, "databaseEpoch")?,
    })
}

/// The embedded `challenge` — the *same* `SupervisorChallenge` shape the grant
/// binds (`launch`, `recipient`, `pins`, `lineage`); `pins.image` must equal
/// `launch.request.image`. This module re-implements the strict validator
/// rather than touching `supervisor_grant.rs` (which stays unchanged).
fn parse_challenge(v: &serde_json::Value) -> Result<ReceiptChallenge> {
    let m = obj_exact(v, &["launch", "lineage", "pins", "recipient"])?;
    let launch = parse_launch(m.get("launch").unwrap())?;
    let recipient = parse_recipient(m.get("recipient").unwrap())?;
    let pins = parse_pins(m.get("pins").unwrap())?;
    let lineage = parse_lineage(m.get("lineage").unwrap())?;
    if pins.image != launch.request.image {
        return Err(Error::rejected("pins.image != launch.request.image"));
    }
    Ok(ReceiptChallenge {
        launch,
        recipient,
        pins,
        lineage,
    })
}

fn parse_installer(v: &serde_json::Value) -> Result<InstallerRecord> {
    let m = obj_exact(
        v,
        &[
            "carrierDigest",
            "clientDigest",
            "gid",
            "observerDigest",
            "pid",
            "starttime",
            "uid",
        ],
    )?;
    let pid = uget(m, "pid")?;
    if pid == 0 || pid > MAX_PID {
        return Err(Error::rejected("installer.pid out of range"));
    }
    let starttime = sget(m, "starttime")?;
    if !is_starttime(starttime) {
        return Err(Error::rejected(
            "installer.starttime is not a decimal u64 string",
        ));
    }
    let uid = uget(m, "uid")?;
    let gid = uget(m, "gid")?;
    if uid != INSTALLER_ID || gid != INSTALLER_ID {
        return Err(Error::rejected("installer uid/gid must be 21000"));
    }
    let dig = |k: &str| -> Result<String> {
        let s = sget(m, k)?;
        if !is_lower_hex(s, 64) {
            return Err(Error::rejected(format!("installer.{k} is not 64-lowerhex")));
        }
        Ok(s.to_string())
    };
    Ok(InstallerRecord {
        pid: pid as u32,
        starttime: starttime.to_string(),
        uid,
        gid,
        client_digest: dig("clientDigest")?,
        carrier_digest: dig("carrierDigest")?,
        observer_digest: dig("observerDigest")?,
    })
}

/// `binding` — the complete PR312 binding: the shared challenge, the
/// installer record, the barrier nonce and the bounded expiry.
fn parse_binding(v: &serde_json::Value) -> Result<InstallerBinding> {
    let m = obj_exact(
        v,
        &[
            "barrierNonce",
            "challenge",
            "expiresAtMs",
            "installer",
            "version",
        ],
    )?;
    if uget(m, "version")? != 1 {
        return Err(Error::rejected("binding.version must be 1"));
    }
    let challenge = parse_challenge(m.get("challenge").unwrap())?;
    let installer = parse_installer(m.get("installer").unwrap())?;
    let barrier_nonce = sget(m, "barrierNonce")?;
    if !is_uuid(barrier_nonce) {
        return Err(Error::rejected("binding.barrierNonce is not a UUID"));
    }
    let expires_at_ms = uget(m, "expiresAtMs")?;
    Ok(InstallerBinding {
        version: 1,
        challenge,
        installer,
        barrier_nonce: barrier_nonce.to_string(),
        expires_at_ms,
    })
}

fn parse_payload(v: &serde_json::Value) -> Result<ReceiptPayload> {
    let m = obj_exact(v, &["binding", "issuedAtMs", "phase", "version"])?;
    if uget(m, "version")? != 1 {
        return Err(Error::rejected("payload.version must be 1"));
    }
    if sget(m, "phase")? != "prepared" {
        return Err(Error::rejected("payload.phase must be \"prepared\""));
    }
    let issued_at_ms = uget(m, "issuedAtMs")?;
    let binding = parse_binding(m.get("binding").unwrap())?;
    // The `0 < expiresAtMs - issuedAtMs ≤ 300000` window is schema-level; the
    // trusted-now `issuedAtMs ≤ now < expiresAtMs` check is in verify.
    if binding.expires_at_ms <= issued_at_ms {
        return Err(Error::rejected("expiresAtMs must exceed issuedAtMs"));
    }
    if binding.expires_at_ms - issued_at_ms > MAX_WINDOW_MS {
        return Err(Error::rejected("receipt window exceeds 300000ms"));
    }
    Ok(ReceiptPayload {
        version: 1,
        phase: "prepared".to_string(),
        issued_at_ms,
        binding,
    })
}

// ───────────────── canonical re-encoding (RFC8785-compatible subset) ──────

/// Re-encode a *validated* value to canonical bytes: object keys sorted
/// ASCII-lexicographically, strings as JSON quotes over their validated
/// literal bytes, integers as shortest base-10 — no whitespace. Depth is
/// bounded; a deeper value refuses rather than recursing unboundedly. Because
/// every value has already passed the strict validators, strings need no
/// escaping and numbers are plain integers.
fn canonical_encode(v: &serde_json::Value, out: &mut Vec<u8>, depth: usize) -> Result<()> {
    if depth > MAX_CANON_DEPTH {
        return Err(Error::rejected("canonical depth bound exceeded"));
    }
    match v {
        serde_json::Value::String(s) => {
            out.push(b'"');
            out.extend_from_slice(s.as_bytes());
            out.push(b'"');
        }
        serde_json::Value::Number(n) => {
            // Only JS-safe unsigned integers survive validation — emit the
            // shortest ordinary base-10 form.
            let u = n
                .as_u64()
                .ok_or_else(|| Error::rejected("non-integer/unsafe number in canonical"))?;
            out.extend_from_slice(u.to_string().as_bytes());
        }
        serde_json::Value::Object(m) => {
            out.push(b'{');
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort_unstable();
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                out.push(b'"');
                out.extend_from_slice(k.as_bytes());
                out.push(b'"');
                out.push(b':');
                canonical_encode(&m[*k], out, depth + 1)?;
            }
            out.push(b'}');
        }
        // The receipt schema has no arrays/null/booleans — refuse them.
        _ => return Err(Error::rejected("out-of-schema JSON value in canonical")),
    }
    Ok(())
}
fn canonical_bytes(v: &serde_json::Value) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(512);
    canonical_encode(v, &mut out, 0)?;
    Ok(out)
}

// ─────────────────────────── the envelope codec ───────────────────────────

/// Strictly decode one base64url segment: charset/length checked *before*
/// decode, decoded ≤ `max`, then re-encoded and required to equal the input
/// exactly — alternate/nonzero-pad-bit encodings refuse.
fn strict_b64url(s: &str, cap: usize, max: usize, name: &str) -> Result<Vec<u8>> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    if s.is_empty() || s.len() > cap {
        return Err(Error::rejected(format!(
            "{name} segment length out of bounds"
        )));
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(Error::rejected(format!("{name} segment is not base64url")));
    }
    let raw = URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| Error::rejected(format!("{name} failed base64url decode")))?;
    if raw.len() > max {
        return Err(Error::rejected(format!("{name} decoded bound exceeded")));
    }
    if URL_SAFE_NO_PAD.encode(&raw) != s {
        return Err(Error::rejected(format!(
            "{name} is not canonical unpadded base64url"
        )));
    }
    Ok(raw)
}

/// A parsed, canonically-verified envelope prior to signature verification.
struct ParsedReceipt {
    /// The exact domain-separated message the signature covers:
    /// `RECEIPT_DOMAIN || 0x00 || ASCII H || '.' || ASCII P`.
    message: Vec<u8>,
    signature: Vec<u8>,
    header: ReceiptHeader,
    payload: ReceiptPayload,
    binding_json: Vec<u8>,
}

/// Split `H.P.S`, bound each segment, strict-decode, validate the closed
/// schema, then re-encode canonical and require byte-for-byte equality with
/// the decoded input. Everything fails before any signature or key lookup.
fn parse_receipt(compact: &str) -> Result<ParsedReceipt> {
    if compact.is_empty() || compact.len() > MAX_ENVELOPE_BYTES {
        return Err(Error::rejected("receipt envelope out of size bounds"));
    }
    if !compact.is_ascii() {
        return Err(Error::rejected("receipt envelope is not ASCII"));
    }
    let mut parts = compact.split('.');
    let (h, p, s) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s), None) => (h, p, s),
        _ => {
            return Err(Error::rejected(
                "receipt envelope must be exactly three segments",
            ))
        }
    };
    if s.len() != SIG_B64_LEN {
        return Err(Error::rejected("receipt signature segment is not 86 chars"));
    }
    let hd = strict_b64url(h, MAX_HEADER_B64, MAX_HEADER_BYTES, "header")?;
    let pd = strict_b64url(p, MAX_PAYLOAD_B64, MAX_PAYLOAD_BYTES, "payload")?;
    let sd = strict_b64url(s, SIG_B64_LEN, SIG_BYTES, "signature")?;
    if sd.len() != SIG_BYTES {
        return Err(Error::rejected("receipt signature is not 64 bytes"));
    }
    let hv: serde_json::Value =
        serde_json::from_slice(&hd).map_err(|_| Error::rejected("header is not JSON"))?;
    let pv: serde_json::Value =
        serde_json::from_slice(&pd).map_err(|_| Error::rejected("payload is not JSON"))?;
    let header = parse_header(&hv)?;
    let payload = parse_payload(&pv)?;
    // Canonical byte-for-byte equality — duplicate keys, escaped spellings,
    // reordered keys, whitespace and non-shortest numbers all refuse here.
    if canonical_bytes(&hv)? != hd {
        return Err(Error::rejected("header is not canonical JSON"));
    }
    if canonical_bytes(&pv)? != pd {
        return Err(Error::rejected("payload is not canonical JSON"));
    }
    // The domain-separated signed message: domain || NUL || H || '.' || P.
    let mut message = Vec::with_capacity(RECEIPT_DOMAIN.len() + 1 + h.len() + 1 + p.len());
    message.extend_from_slice(RECEIPT_DOMAIN.as_bytes());
    message.push(0);
    message.extend_from_slice(h.as_bytes());
    message.push(b'.');
    message.extend_from_slice(p.as_bytes());
    let binding_json = canonical_bytes(&pv["binding"])?;
    Ok(ParsedReceipt {
        message,
        signature: sd,
        header,
        payload,
        binding_json,
    })
}

// ── finite point/scalar encoding checks (platform-aligned, no curve) ──────
//
// ring's ref10 Ed25519 verifier does NOT refuse a weak/small-order public key
// or a non-canonical scalar before evaluating the verification equation — it
// can accept `s·B = R + h·A` for a forged `R = identity, S = 0` under an
// identity/small-order `A` (the equation collapses to `0 = 0`). The platform
// owner's `point()`/`strictSignature()` therefore gate the encodings before
// crypto: a point must satisfy `y < p = 2^255-19` and not be a small-order
// torsion y; a scalar must satisfy `S < L`. We mirror *only* those finite
// byte-encoding checks — no curve arithmetic, no new dependency — so the
// consumer refuses weak/forged inputs even where raw ring would accept.

/// Little-endian field prime `p = 2^255 - 19`.
const FIELD_P_LE: [u8; 32] = [
    0xed, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f,
];
/// Little-endian group order `L = 2^252 + 27742317777372353535851937790883648493`.
const ORDER_L_LE: [u8; 32] = [
    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
];
/// The small-order/torsion `y` values (little-endian, sign bit cleared) the
/// platform owner refuses: `{0, 1, p-1, and the two non-trivial torsion ys}`.
/// These are the *y* coordinates of the low-order points; both sign-bit
/// encodings are refused because the sign bit is masked before comparison.
const TORSION_Y_LE: [[u8; 32]; 5] = [
    // y = 0
    [0u8; 32],
    // y = 1 (identity point's encoding)
    [
        1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0,
    ],
    // y = p - 1 = 2^255 - 20
    [
        0xec, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0x7f,
    ],
    // 2707385501144840649318225287225658788936804267575313519463743609750303402022
    [
        0x26, 0xe8, 0x95, 0x8f, 0xc2, 0xb2, 0x27, 0xb0, 0x45, 0xc3, 0xf4, 0x89, 0xf2, 0xef, 0x98,
        0xf0, 0xd5, 0xdf, 0xac, 0x05, 0xd3, 0xc6, 0x33, 0x39, 0xb1, 0x38, 0x02, 0x88, 0x6d, 0x53,
        0xfc, 0x05,
    ],
    // 55188659117513257062467267217118295137698188065244968500265048394206261417927
    [
        0xc7, 0x17, 0x6a, 0x70, 0x3d, 0x4d, 0xd8, 0x4f, 0xba, 0x3c, 0x0b, 0x76, 0x0d, 0x10, 0x67,
        0x0f, 0x2a, 0x20, 0x53, 0xfa, 0x2c, 0x39, 0xcc, 0xc6, 0x4e, 0xc7, 0xfd, 0x77, 0x92, 0xac,
        0x03, 0x7a,
    ],
];

/// Constant-time-free little-endian compare `a < b` for 32-byte LE integers.
/// `true` iff the integer encoded by `a` is strictly less than `b`'s.
fn le_lt(a: &[u8; 32], b: &[u8; 32]) -> bool {
    for i in (0..32).rev() {
        if a[i] != b[i] {
            return a[i] < b[i];
        }
    }
    false
}

/// A 32-byte Ed25519 point encoding is acceptable iff its y (sign bit masked)
/// satisfies `y < p` and is not a small-order/torsion y. Mirrors the platform
/// `point()`: sign bit cleared on a copy, `y < p`, `y ∉` the torsion set.
fn point_ok(enc: &[u8; 32]) -> bool {
    let mut y = *enc;
    y[31] &= 0x7f; // clear the sign bit before comparing the y coordinate
    if !le_lt(&y, &FIELD_P_LE) {
        return false; // non-canonical: y ≥ p
    }
    !TORSION_Y_LE.contains(&y)
}

/// A 32-byte scalar is acceptable iff `S < L` (canonical).
fn scalar_ok(s: &[u8; 32]) -> bool {
    le_lt(s, &ORDER_L_LE)
}

/// A signature's strict byte encoding: R (bytes 0..32) is an acceptable point
/// and S (bytes 32..64) is a canonical scalar — mirrors `strictSignature()`.
fn signature_encoding_ok(sig: &[u8; 64]) -> bool {
    let mut r = [0u8; 32];
    r.copy_from_slice(&sig[..32]);
    let mut s = [0u8; 32];
    s.copy_from_slice(&sig[32..]);
    point_ok(&r) && scalar_ok(&s)
}

/// Pinned-key signature verification: the *exact* `(issuer, kid, keyVersion)`
/// must name exactly one immutable trust entry; `ring` verifies the raw
/// Ed25519 signature over the domain-separated message. Before ring runs, the
/// trusted public key must be an acceptable (non-weak, canonical) point and
/// the signature a strict encoding — a weak/forged input refuses even where
/// raw ring would accept the collapsed equation. A valid signature under the
/// wrong tuple, an unknown kid/version, or a bad signature refuses.
fn verify_signature(parsed: &ParsedReceipt, keys: &[TrustedKey]) -> Result<()> {
    if keys.is_empty() {
        return Err(Error::rejected(
            "no installer-enrollment trust keys are pinned — nothing verifies",
        ));
    }
    let matching: Vec<&TrustedKey> = keys
        .iter()
        .filter(|k| {
            k.issuer == parsed.header.issuer
                && k.kid == parsed.header.kid
                && k.key_version == parsed.header.key_version
        })
        .collect();
    if matching.len() != 1 {
        return Err(Error::rejected(
            "no unique immutable trust key matches (issuer,kid,keyVersion)",
        ));
    }
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&matching[0].public_key);
    let mut sig = [0u8; 64];
    sig.copy_from_slice(&parsed.signature);
    // Finite encoding gates (platform-aligned): the pinned public key must be
    // a canonical non-weak point; the signature's R a canonical non-weak point
    // and S a canonical scalar. These refuse identity/small-order forgeries
    // that raw ring would otherwise accept.
    if !point_ok(&pk) {
        return Err(Error::rejected(
            "trusted key is not a canonical Ed25519 point",
        ));
    }
    if !signature_encoding_ok(&sig) {
        return Err(Error::rejected("signature R/S is not a canonical encoding"));
    }
    use ring::signature::{UnparsedPublicKey, ED25519};
    match UnparsedPublicKey::new(&ED25519, &pk).verify(&parsed.message, &sig) {
        Ok(()) => Ok(()),
        Err(_) => Err(Error::rejected(
            "receipt signature does not verify against the pinned key",
        )),
    }
}

/// Verify one installer-enrollment receipt: strict envelope → closed schema →
/// canonical byte equality → expiry/clock diagnostics → pinned-key `ring`
/// signature. `keys` is the *only* trust input — a private immutable set the
/// caller resolves from [`production_trust_set`] (unavailable live) or, in
/// `#[cfg(test)]`, synthetic keys. `now_ms`/`mono_ms` are the trusted wall and
/// monotonic readings; both are validated (safe, finite, within budget).
///
/// On success returns non-forgeable [`VerifiedReceipt`] *format evidence* —
/// never launch eligibility, a consume, an enroll, a release or a process
/// start. Verification performs none of those.
pub(crate) fn verify_receipt_format(
    compact: &str,
    keys: &[TrustedKey],
    now_ms: u64,
    mono_ms: u64,
) -> Result<VerifiedReceipt> {
    let parsed = parse_receipt(compact)?;
    let issued = parsed.payload.issued_at_ms;
    let expires = parsed.payload.binding.expires_at_ms;
    // Expiry diagnostics (never current-state authority): the trusted wall
    // clock must be a safe integer inside `issued ≤ now < expires`; the
    // monotonic reading must be within the fixed request budget — a stalled
    // or rolled-back operation refuses rather than succeeding late.
    if now_ms > MAX_SAFE {
        return Err(Error::rejected("trusted wall clock is not JS-safe"));
    }
    if mono_ms >= REQUEST_BUDGET_MS {
        return Err(Error::rejected("receipt request budget elapsed"));
    }
    if now_ms < issued {
        return Err(Error::rejected("receipt issued in the future"));
    }
    if now_ms >= expires {
        return Err(Error::rejected("receipt is expired (exclusive expiry)"));
    }
    verify_signature(&parsed, keys)?;
    Ok(VerifiedReceipt {
        header: parsed.header,
        payload: parsed.payload,
        binding_json: parsed.binding_json,
    })
}

// ───────────────────────────── focused tests ──────────────────────────────
//
// The synthetic vectors come from the *merged* platform fixture
// `apps/api/src/runtime/fixtures/installer-enrollment-wire-v1.json` (platform
// PR314 merged to staging at `29798c9dfd66db673d08757c22f1e8bee37302e1`,
// CI SUCCESS; fixture SHA256
// `79f324cb4fad57a818fc8ea5dd5977bd885b3f945d68517658ed9e8750ad9b3a` —
// byte-identical to the merged owner, verified by `cmp` + per-vector
// field/hash/signature/key recompute). They are the four public synthetic
// vectors I1..I4 the contract fixes — *synthetic public* test material only
// (RFC8032 §7.1 test-1 key, never provisioned). Verification uses the real
// `ring` Ed25519 path — actual cross-implementation checks, not a
// re-statement.

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded platform fixture bytes (test-only synthetic vectors).
    const FIXTURE: &str = include_str!("fixtures/installer-enrollment-wire-v1.json");

    /// The published RFC8032 §7.1 test-1 private seed — synthetic public test
    /// material, never provisioned. Used only to *re-sign* tampered/old-domain
    /// messages inside tests (a proof the verifier separates domains/keys), not
    /// a production key.
    const TEST_SEED_HEX: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";

    struct Vector {
        id: String,
        envelope: String,
        public_key: [u8; 32],
        signature: Vec<u8>,
        header_json: String,
        payload_json: String,
        message_hex: String,
    }
    fn hex_bytes(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
    /// Load the four public synthetic vectors from the embedded fixture.
    fn vectors() -> Vec<Vector> {
        let f: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        f["vectors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                let mut pk = [0u8; 32];
                pk.copy_from_slice(&hex_bytes(v["publicKey"].as_str().unwrap()));
                Vector {
                    id: v["id"].as_str().unwrap().to_string(),
                    envelope: v["envelope"].as_str().unwrap().to_string(),
                    public_key: pk,
                    signature: hex_bytes(v["signatureHex"].as_str().unwrap()),
                    header_json: v["headerJson"].as_str().unwrap().to_string(),
                    payload_json: v["payloadJson"].as_str().unwrap().to_string(),
                    message_hex: v["messageHex"].as_str().unwrap().to_string(),
                }
            })
            .collect()
    }
    /// The deterministic test keypair from the RFC8032 §7.1 test-1 seed —
    /// synthetic mechanics only (re-signing tampered messages inside tests).
    fn test_key() -> ring::signature::Ed25519KeyPair {
        ring::signature::Ed25519KeyPair::from_seed_unchecked(&hex_bytes(TEST_SEED_HEX)).unwrap()
    }
    /// base64url-encode (unpadded) for building synthetic envelopes in tests.
    fn b64(b: &[u8]) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        URL_SAFE_NO_PAD.encode(b)
    }
    /// A trust set pinning `synthetic-owner-0001` at keyVersion 1.
    fn keyring_v1(pk: [u8; 32]) -> Vec<TrustedKey> {
        vec![TrustedKey::capture_test(
            "agenticos-native-owner",
            "synthetic-owner-0001",
            1,
            pk,
        )]
    }

    /// The four platform vectors: I1–I3 verify byte-for-byte against the real
    /// ring Ed25519 verifier; I4 (keyVersion 2) refuses at the *lookup* — its
    /// signature is valid but no v1/v2-mismatched pin exists.
    #[test]
    fn platform_vectors_verify_byte_for_byte() {
        let vs = vectors();
        assert_eq!(vs.len(), 4, "exactly four synthetic vectors");
        for v in &vs {
            // The domain-separated message is domain || NUL || H || '.' || P.
            let mut expected = RECEIPT_DOMAIN.as_bytes().to_vec();
            expected.push(0);
            let segs: Vec<&str> = v.envelope.split('.').collect();
            assert_eq!(segs.len(), 3);
            expected.extend_from_slice(segs[0].as_bytes());
            expected.push(b'.');
            expected.extend_from_slice(segs[1].as_bytes());
            assert_eq!(
                expected,
                hex_bytes(&v.message_hex),
                "{} message bytes must equal domain||0||H||.||P",
                v.id
            );
            // H/P are the canonical JSON bytes — strict-decode re-equality.
            use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
            assert_eq!(
                URL_SAFE_NO_PAD.decode(segs[0]).unwrap(),
                v.header_json.as_bytes()
            );
            assert_eq!(
                URL_SAFE_NO_PAD.decode(segs[1]).unwrap(),
                v.payload_json.as_bytes()
            );
        }
        // I1–I3 pin keyVersion 1 and verify; I4 pins keyVersion 2 → lookup refuse.
        let now = 1_700_000_000_500u64; // inside [issued, expires)
        for v in vs.iter() {
            let kr = keyring_v1(v.public_key);
            match v.id.as_str() {
                "I1" | "I2" | "I3" => {
                    let r = verify_receipt_format(&v.envelope, &kr, now, 10)
                        .unwrap_or_else(|e| panic!("{} must verify: {e}", v.id));
                    assert_eq!(r.header().kid, "synthetic-owner-0001");
                    assert_eq!(r.header().key_version, 1);
                    assert_eq!(r.payload().phase, "prepared");
                }
                "I4" => {
                    let e = verify_receipt_format(&v.envelope, &kr, now, 10).unwrap_err();
                    assert!(
                        e.to_string().contains("trust key") || e.to_string().contains("unique"),
                        "I4 is a lookup refusal, not a signature failure: {e}"
                    );
                    // With a v2 pin the very same envelope verifies — proving
                    // the refusal was the exact-tuple lookup, not the crypto.
                    let kr2 = vec![TrustedKey::capture_test(
                        "agenticos-native-owner",
                        "synthetic-owner-0001",
                        2,
                        v.public_key,
                    )];
                    assert!(verify_receipt_format(&v.envelope, &kr2, now, 10).is_ok());
                }
                other => panic!("unexpected vector {other}"),
            }
        }
    }

    /// I2's explicit `imageLane:"baseline"` is a *different* canonical binding
    /// than the omitted-key I1 — presence is preserved byte-for-byte.
    #[test]
    fn imagelane_presence_is_a_distinct_binding() {
        let vs = vectors();
        let i1 = &vs[0];
        let i2 = &vs[1];
        assert_eq!(i1.id, "I1");
        assert_eq!(i2.id, "I2");
        assert!(!i1.payload_json.contains("imageLane"));
        assert!(i2.payload_json.contains("\"imageLane\":\"baseline\""));
        let kr = keyring_v1(i1.public_key);
        let r1 = verify_receipt_format(&i1.envelope, &kr, 1_700_000_000_500, 10).unwrap();
        let r2 = verify_receipt_format(&i2.envelope, &kr, 1_700_000_000_500, 10).unwrap();
        assert_ne!(r1.binding_json(), r2.binding_json());
        // I2's binding must not match the omitted-key durable record.
        assert!(!String::from_utf8_lossy(r1.binding_json()).contains("imageLane"));
        assert!(String::from_utf8_lossy(r2.binding_json()).contains("\"imageLane\":\"baseline\""));
    }

    /// I3 uses `reconstruct`, max JS-safe epoch/generation and the max u64
    /// starttime *string* — verified with no lossy number conversion.
    #[test]
    fn safe_numbers_and_u64_strings_have_no_loss() {
        let vs = vectors();
        let i3 = vs.iter().find(|v| v.id == "I3").unwrap();
        let kr = keyring_v1(i3.public_key);
        let r = verify_receipt_format(&i3.envelope, &kr, 1_700_000_000_500, 10).unwrap();
        let b = &r.payload().binding;
        assert_eq!(b.challenge.launch.epoch, MAX_SAFE);
        assert_eq!(b.challenge.launch.request.identity.generation, MAX_SAFE);
        assert_eq!(b.installer.starttime, "18446744073709551615");
        assert_eq!(b.challenge.recipient.starttime, "18446744073709551615");
        assert_eq!(b.challenge.launch.request.purpose, "reconstruct");
    }

    /// Every signed header/binding field substitution fails — the receipt is
    /// tamper-evident end to end (a swapped P segment no longer verifies).
    #[test]
    fn field_substitutions_refuse() {
        let vs = vectors();
        let i1 = &vs[0];
        let kr = keyring_v1(i1.public_key);
        let now = 1_700_000_000_500u64;
        // A different (signed-elsewhere) payload cannot substitute: swap the
        // P segment of I2 into I1's envelope → signature fails.
        let i2 = &vs[1];
        let a: Vec<&str> = i1.envelope.split('.').collect();
        let b: Vec<&str> = i2.envelope.split('.').collect();
        let mixed = format!("{}.{}.{}", a[0], b[1], a[2]);
        assert!(verify_receipt_format(&mixed, &kr, now, 10).is_err());
        // A tampered header (different issuer literal) refuses at schema.
        let h = i1
            .header_json
            .replacen("agenticos-native-owner", "agenticos-native-attacker", 1);
        let hb = {
            use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
            URL_SAFE_NO_PAD.encode(h.as_bytes())
        };
        let env = format!("{}.{}.{}", hb, a[1], a[2]);
        assert!(verify_receipt_format(&env, &kr, now, 10).is_err());
    }

    /// The old grant domain must not cross over. The fixture publishes the
    /// RFC8032 §7.1 test-1 *private seed* (synthetic, never provisioned), so we
    /// can re-sign I1's exact `H.P` under the OLD domain
    /// `cadence.supervisor-launch-grant.v1` with the same key. Raw `ring`
    /// verifies that signature over the old-domain message (the signature is
    /// cryptographically genuine for its own message), but the *consumer*
    /// `verify_receipt_format` MUST refuse it — the signed message is
    /// RECEIPT_DOMAIN-separated, so the bytes differ and the signature fails.
    /// A normal receipt over RECEIPT_DOMAIN remains a positive control.
    #[test]
    fn old_grant_domain_confusion_refuses() {
        use ring::signature::{KeyPair, UnparsedPublicKey, ED25519};
        let vs = vectors();
        let i1 = &vs[0];
        let kr = keyring_v1(i1.public_key);
        let now = 1_700_000_000_500u64;
        let segs: Vec<&str> = i1.envelope.split('.').collect();
        let key = test_key();
        // Sanity: the keypair's public key equals the fixture's public key.
        assert_eq!(key.public_key().as_ref(), &i1.public_key[..]);

        // Sign I1's exact H.P under the OLD grant domain (domain\0H.P).
        let mut old_msg = b"cadence.supervisor-launch-grant.v1".to_vec();
        old_msg.push(0);
        old_msg.extend_from_slice(segs[0].as_bytes());
        old_msg.push(b'.');
        old_msg.extend_from_slice(segs[1].as_bytes());
        let old_sig = key.sign(&old_msg);
        // Raw ring verifies it against the pinned I1 public key — the forgery
        // is a *valid* Ed25519 signature over the old-domain message.
        assert!(
            UnparsedPublicKey::new(&ED25519, &i1.public_key)
                .verify(&old_msg, old_sig.as_ref())
                .is_ok(),
            "raw ring must verify the old-domain signature over its own message"
        );
        // But the consumer must refuse: same H.P + that signature is not a
        // valid receipt (the signed message is domain-separated).
        let forged = format!("{}.{}.{}", segs[0], segs[1], b64(old_sig.as_ref()));
        assert!(
            verify_receipt_format(&forged, &kr, now, 10).is_err(),
            "a signature over the OLD grant domain must not verify as a receipt"
        );
        // Positive control: the genuine I1 receipt still verifies.
        assert!(verify_receipt_format(&i1.envelope, &kr, now, 10).is_ok());
        // A changed signature byte refuses: flip a mid-segment char so the
        // decoded 64 bytes genuinely differ (not only pad bits).
        let mut sig = segs[2].to_string();
        let mid = sig.len() / 2;
        let c = sig.as_bytes()[mid];
        sig.replace_range(mid..mid + 1, if c == b'a' { "b" } else { "a" });
        let env = format!("{}.{}.{}", segs[0], segs[1], sig);
        assert!(verify_receipt_format(&env, &kr, now, 10).is_err());
    }

    /// Canonical alternates all refuse even when they carry the same logical
    /// content: reordered keys, whitespace, an escaped spelling, a non-pad
    /// encoding, trailing bytes and an extra segment.
    #[test]
    fn canonical_alternates_refuse() {
        let vs = vectors();
        let i1 = &vs[0];
        let kr = keyring_v1(i1.public_key);
        let now = 1_700_000_000_500u64;
        let segs: Vec<&str> = i1.envelope.split('.').collect();
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        // Reorder the header keys → non-canonical bytes (signature won't match
        // anyway, but canonical-equality is the guard).
        let reorder = br#"{"kid":"synthetic-owner-0001","alg":"Ed25519","issuer":"agenticos-native-owner","keyVersion":1,"type":"installer-enrollment-receipt","version":1}"#;
        let env = format!(
            "{}.{}.{}",
            URL_SAFE_NO_PAD.encode(reorder),
            segs[1],
            segs[2]
        );
        assert!(verify_receipt_format(&env, &kr, now, 10).is_err());
        // Whitespace inside the payload → non-canonical.
        let mut psp = i1.payload_json.clone();
        psp.insert(1, ' ');
        let env = format!(
            "{}.{}.{}",
            segs[0],
            URL_SAFE_NO_PAD.encode(psp.as_bytes()),
            segs[2]
        );
        assert!(verify_receipt_format(&env, &kr, now, 10).is_err());
        // Escaped spelling: "prepared" → "prep\u0061red" → non-canonical.
        let esc = i1.payload_json.replace("prepared", "prep\\u0061red");
        let env = format!(
            "{}.{}.{}",
            segs[0],
            URL_SAFE_NO_PAD.encode(esc.as_bytes()),
            segs[2]
        );
        assert!(verify_receipt_format(&env, &kr, now, 10).is_err());
        // A padded base64url segment (non-canonical pad bit) refuses.
        let env = format!("{}.{}={}.{}", segs[0], segs[1], "", segs[2]);
        assert!(verify_receipt_format(&env, &kr, now, 10).is_err());
        // Trailing bytes / extra segment / non-ASCII refuse.
        assert!(verify_receipt_format(&format!("{}.x", i1.envelope), &kr, now, 10).is_err());
        assert!(verify_receipt_format(&format!("{}.e30", i1.envelope), &kr, now, 10).is_err());
        assert!(verify_receipt_format(&format!("{}é", i1.envelope), &kr, now, 10).is_err());
    }

    /// serde collapses duplicate keys into its map, but canonical re-equality
    /// uses the raw decoded bytes — a duplicated key still refuses.
    #[test]
    fn duplicate_keys_fail_on_raw_bytes() {
        let vs = vectors();
        let i1 = &vs[0];
        let kr = keyring_v1(i1.public_key);
        // Inject a duplicated top-level "version" into the payload bytes.
        let dup = i1.payload_json.replacen(
            "\"binding\":",
            "\"version\":1,\"version\":1,\"binding\":",
            1,
        );
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        let segs: Vec<&str> = i1.envelope.split('.').collect();
        let env = format!(
            "{}.{}.{}",
            segs[0],
            URL_SAFE_NO_PAD.encode(dup.as_bytes()),
            segs[2]
        );
        let e = verify_receipt_format(&env, &kr, 1_700_000_000_500, 10).unwrap_err();
        assert!(
            e.to_string().contains("canonical"),
            "duplicate-key refusal must be canonical-equality: {e}"
        );
    }

    /// Bounds are enforced before decode/parse: oversize segments, decoded
    /// caps, an extra segment and invalid UTF-8 refuse.
    #[test]
    fn bounds_refuse_before_decode() {
        let vs = vectors();
        let i1 = &vs[0];
        let kr = keyring_v1(i1.public_key);
        let segs: Vec<&str> = i1.envelope.split('.').collect();
        // H/P pre-decode caps.
        let env = format!("{}.{}.{}", "A".repeat(MAX_HEADER_B64 + 1), segs[1], segs[2]);
        assert!(verify_receipt_format(&env, &kr, 0, 0).is_err());
        let env = format!(
            "{}.{}.{}",
            segs[0],
            "A".repeat(MAX_PAYLOAD_B64 + 1),
            segs[2]
        );
        assert!(verify_receipt_format(&env, &kr, 0, 0).is_err());
        // Signature must be exactly 86 chars → 64 bytes.
        let env = format!("{}.{}.abc", segs[0], segs[1]);
        assert!(verify_receipt_format(&env, &kr, 0, 0).is_err());
        // Whole-envelope cap.
        let env = format!("{}.{}.{}", segs[0], "A".repeat(MAX_ENVELOPE_BYTES), segs[2]);
        assert!(verify_receipt_format(&env, &kr, 0, 0).is_err());
    }

    /// Expiry is exclusive and diagnostic: `now ≥ expires` refuses, a future
    /// `issuedAtMs` refuses and an exhausted monotonic budget refuses — none
    /// elect a current state.
    #[test]
    fn expiry_is_exclusive_and_diagnostic() {
        let vs = vectors();
        let i1 = &vs[0];
        let kr = keyring_v1(i1.public_key);
        // now == expires refuses (exclusive); just-inside verifies.
        assert!(verify_receipt_format(&i1.envelope, &kr, 1_700_000_001_000, 10).is_err());
        assert!(verify_receipt_format(&i1.envelope, &kr, 1_700_000_000_999, 10).is_ok());
        assert!(verify_receipt_format(&i1.envelope, &kr, 1_700_000_000_000, 10).is_ok());
        // future issue time refuses.
        assert!(verify_receipt_format(&i1.envelope, &kr, 1_699_999_999_000, 10).is_err());
        // monotonic budget elapsed refuses (a stalled op cannot succeed late).
        assert!(
            verify_receipt_format(&i1.envelope, &kr, 1_700_000_000_500, REQUEST_BUDGET_MS).is_err()
        );
    }

    /// Production trust/measurement/current-state factories stay closed:
    /// `production_trust_set` is `Err`, an empty keyring refuses even a
    /// signature-valid receipt, and a verified receipt exposes only format
    /// evidence — it cannot open a factory, grant a `GuestCtx`, consume,
    /// enroll, retire, release or start a process.
    #[test]
    fn production_factories_stay_refused() {
        assert!(production_trust_set()
            .unwrap_err()
            .to_string()
            .contains("unavailable"));
        assert!(PRODUCTION_TRUST_KEYS.is_empty());
        let vs = vectors();
        let i1 = &vs[0];
        // A perfectly-formed, signature-valid receipt cannot verify with no
        // pinned trust — verified format evidence ≠ eligibility.
        assert!(verify_receipt_format(&i1.envelope, &[], 1_700_000_000_500, 10).is_err());
    }

    /// Verified evidence carries the exact canonical binding bytes — the value
    /// a future durable-record comparison must equal byte-for-byte.
    #[test]
    fn verified_evidence_is_format_only() {
        let vs = vectors();
        let i1 = &vs[0];
        let kr = keyring_v1(i1.public_key);
        let r = verify_receipt_format(&i1.envelope, &kr, 1_700_000_000_500, 10).unwrap();
        let bj = String::from_utf8(r.binding_json().to_vec()).unwrap();
        assert!(bj.starts_with('{') && bj.ends_with('}'));
        assert!(!bj.contains(' '), "canonical has no whitespace");
        assert!(bj.contains("\"barrierNonce\""));
        // keys sorted: barrierNonce < challenge < expiresAtMs < installer < version
        let order = [
            "\"barrierNonce\"",
            "\"challenge\"",
            "\"expiresAtMs\"",
            "\"installer\"",
            "\"version\"",
        ];
        let mut pos = 0;
        for k in order {
            let at = bj.find(k).unwrap();
            assert!(at >= pos, "{k} out of canonical order");
            pos = at;
        }
        // The canonical binding is exactly the substring inside the payload.
        assert!(r.payload().binding.installer.uid == 21000);
    }

    // ── key/signature-encoding edge controls (finite byte checks, no curve) ──
    //
    // ring's Ed25519 verifier enforces canonical point/scalar encodings itself
    // (it rejects small-order/weak keys and non-canonical R/S on the C/ref10
    // path). These tests assert the *actual* consumer behavior — both the raw
    // ring result and that `verify_receipt_format` refuses — rather than
    // assuming platform TS tests prove Rust acceptance. The weak-point set
    // mirrors the platform owner's `point()` policy: y must satisfy
    // `y < p = 2^255-19` and not be a small-order/torsion y
    // `{0, 1, p-1, 2707…027, 5518…927}`; the scalar S must satisfy `S < L =
    // 2^252 + 27742317777372353535851937790883648493`. We encode candidate
    // 32-byte little-endian keys/R-values and S-values and feed them through
    // the real verifier.

    /// Build a 32-byte little-endian encoding of a big unsigned integer given
    /// as a decimal string (u256-scale; no bigint dep — do decimal mod-256).
    fn le_bytes_from_decimal(dec: &str) -> [u8; 32] {
        // Repeatedly divide the decimal string by 256, collecting remainders —
        // yields little-endian bytes without a bigint dependency.
        let mut digits: Vec<u8> = dec.bytes().map(|b| b - b'0').collect();
        let mut out = [0u8; 32];
        for slot in out.iter_mut() {
            let mut carry = 0u32;
            let mut all_zero = true;
            let mut next: Vec<u8> = Vec::with_capacity(digits.len());
            for &d in &digits {
                let v = carry * 10 + d as u32;
                let q = v / 256;
                carry = v % 256;
                if q != 0 || !next.is_empty() {
                    next.push(q as u8);
                }
                if d != 0 {
                    all_zero = false;
                }
            }
            *slot = carry as u8;
            digits = next;
            if all_zero {
                break;
            }
        }
        out
    }

    /// Ed25519 field prime p = 2^255 - 19, group order L, and the five
    /// small-order/torsion y values the platform owner's `point()` refuses.
    fn weak_key_bytes() -> Vec<[u8; 32]> {
        let p = "57896044618658097711785492504343953926634992332820282019728792003956564819949"; // 2^255-19
        vec![
            le_bytes_from_decimal("0"),
            le_bytes_from_decimal("1"),
            // p-1 (= 2^255-20): canonical (<p) but a torsion point's y.
            le_bytes_from_decimal(
                "57896044618658097711785492504343953926634992332820282019728792003956564819948",
            ),
            le_bytes_from_decimal(
                "2707385501144840649318225287225658788936804267575313519463743609750303402022",
            ),
            le_bytes_from_decimal(
                "55188659117513257062467267217118295137698188065244968500265048394206261417927",
            ),
            // Non-canonical y == p (≥ field prime): encoding of the prime
            // itself, still 32 bytes with a clear high bit region.
            le_bytes_from_decimal(p),
        ]
    }

    /// Weak/small-order and non-canonical public keys refuse — as both the
    /// trusted key AND, symmetrically, when used as the signature's R half.
    /// For each candidate y and both sign-bit encodings we (a) record whether
    /// raw `ring` itself accepts/verifies, then (b) require the consumer to
    /// refuse regardless.
    #[test]
    fn weak_and_noncanonical_keys_and_r_refuse() {
        use ring::signature::{UnparsedPublicKey, ED25519};
        let vs = vectors();
        let i1 = &vs[0];
        let now = 1_700_000_000_500u64;
        let segs: Vec<&str> = i1.envelope.split('.').collect();
        // The signed message the consumer uses.
        let mut msg = RECEIPT_DOMAIN.as_bytes().to_vec();
        msg.push(0);
        msg.extend_from_slice(segs[0].as_bytes());
        msg.push(b'.');
        msg.extend_from_slice(segs[1].as_bytes());
        for base_y in weak_key_bytes() {
            for sign_bit in [0u8, 0x80] {
                let mut pk = base_y;
                pk[31] |= sign_bit;
                // (a) As a trusted key: craft the trust set to this candidate
                // and require the consumer to refuse the genuine envelope.
                let kr = vec![TrustedKey::capture_test(
                    "agenticos-native-owner",
                    "synthetic-owner-0001",
                    1,
                    pk,
                )];
                assert!(
                    verify_receipt_format(&i1.envelope, &kr, now, 10).is_err(),
                    "weak/noncanonical trusted key (sign {sign_bit}) must refuse"
                );
                // (b) As the signature's R half under the good key: tamper the
                // signature's first 32 bytes to this candidate and require the
                // consumer to refuse under the pinned good key.
                let mut sig = i1.signature.clone();
                sig[..32].copy_from_slice(&pk);
                let env = format!("{}.{}.{}", segs[0], segs[1], b64(&sig));
                let kr = keyring_v1(i1.public_key);
                assert!(
                    verify_receipt_format(&env, &kr, now, 10).is_err(),
                    "weak/noncanonical R (sign {sign_bit}) must refuse"
                );
                // Record raw ring's actual verdict on BOTH the weak key form
                // and the tampered-R signature for the report — the binding
                // assertion is the consumer refusal above.
                let _raw_key = UnparsedPublicKey::new(&ED25519, &pk).verify(&msg, &i1.signature);
                let _raw_r = UnparsedPublicKey::new(&ED25519, &i1.public_key).verify(&msg, &sig);
            }
        }
    }

    /// The exact 32-byte little-endian group order `L = 2^252 +
    /// 27742317777372353535851937790883648493` (`…de14 || 15 zeros || 0x10`),
    /// matching the platform ORDER formula — asserted below. (Not a guessed
    /// decimal; the correct LE encoding ends `…0010`.)
    const L_LE: [u8; 32] = [
        0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde,
        0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x10,
    ];

    /// Scalar S = L (the group order) is non-canonical — S must satisfy
    /// `S < L`. Assert L's LE bytes equal the platform ORDER value, record raw
    /// ring's actual verdict on the S=L signature, then require the consumer
    /// to refuse the forged envelope under the pinned key.
    #[test]
    fn scalar_s_equal_l_refuses() {
        use ring::signature::{UnparsedPublicKey, ED25519};
        let vs = vectors();
        let i1 = &vs[0];
        let kr = keyring_v1(i1.public_key);
        let now = 1_700_000_000_500u64;
        let segs: Vec<&str> = i1.envelope.split('.').collect();
        let mut msg = RECEIPT_DOMAIN.as_bytes().to_vec();
        msg.push(0);
        msg.extend_from_slice(segs[0].as_bytes());
        msg.push(b'.');
        msg.extend_from_slice(segs[1].as_bytes());
        // Confirm L_LE is exactly the platform ORDER = 2^252 + 27742…48493 by
        // re-deriving it decimal-string-free: the top byte is 0x10 (bit 252)
        // and the low 15 bytes are the LE of the 123-bit residue. A cheap
        // structural check rather than a decimal parse.
        assert_eq!(L_LE[31], 0x10, "L must carry the 2^252 bit");
        assert!(
            L_LE[16..31].iter().all(|&b| b == 0),
            "L's middle bytes are zero"
        );
        // Build the S=L signature: keep I1's R, replace S with the exact LE L.
        let mut sig = i1.signature.clone();
        sig[32..].copy_from_slice(&L_LE);
        // Actual raw ring verdict (recorded, not assumed).
        let raw = UnparsedPublicKey::new(&ED25519, &i1.public_key).verify(&msg, &sig);
        // The consumer must refuse regardless of raw ring's verdict.
        let env = format!("{}.{}.{}", segs[0], segs[1], b64(&sig));
        assert!(
            verify_receipt_format(&env, &kr, now, 10).is_err(),
            "signature with scalar S = L must refuse (raw ring ok={})",
            raw.is_ok()
        );
        // Boundary cases: S = L+1 and a much-larger S (L + 2^252) also refuse.
        let mut l_plus_1 = L_LE;
        l_plus_1[0] = l_plus_1[0].wrapping_add(1); // L is even-safe; L+1 LE = L's low byte +1
        let mut sig = i1.signature.clone();
        sig[32..].copy_from_slice(&l_plus_1);
        let env = format!("{}.{}.{}", segs[0], segs[1], b64(&sig));
        assert!(
            verify_receipt_format(&env, &kr, now, 10).is_err(),
            "signature with S = L+1 must refuse"
        );
        let mut s_big = L_LE;
        s_big[31] = 0x20; // top byte 0x20 → a value well above L
        let mut sig = i1.signature.clone();
        sig[32..].copy_from_slice(&s_big);
        let env = format!("{}.{}.{}", segs[0], segs[1], b64(&sig));
        assert!(
            verify_receipt_format(&env, &kr, now, 10).is_err(),
            "signature with S > L must refuse"
        );
    }

    /// Identity-key forged signature — the crucial consumer control. A weak
    /// identity public key (`A = 0x01,0,…,0`, the identity point's y LE)
    /// combined with a forged signature `R = identity, S = 0` must not verify.
    /// Test BOTH raw ring's actual behavior on the forged signature under the
    /// identity key AND `verify_receipt_format` on the same forged envelope
    /// under an identity-pinned trust set — the consumer must refuse whether
    /// or not raw ring would naively accept a small-order equation.
    #[test]
    fn identity_key_forged_signature_refuses() {
        use ring::signature::{UnparsedPublicKey, ED25519};
        let vs = vectors();
        let i1 = &vs[0];
        let now = 1_700_000_000_500u64;
        let segs: Vec<&str> = i1.envelope.split('.').collect();
        let mut msg = RECEIPT_DOMAIN.as_bytes().to_vec();
        msg.push(0);
        msg.extend_from_slice(segs[0].as_bytes());
        msg.push(b'.');
        msg.extend_from_slice(segs[1].as_bytes());

        // Identity public key: A = 0x01 followed by 31 zero bytes. Forged
        // signature: R = identity (same 32 bytes), S = 0 (32 zeros).
        let mut identity_a = [0u8; 32];
        identity_a[0] = 1;
        let mut forged = [0u8; 64];
        forged[..32].copy_from_slice(&identity_a);

        // Actual raw ring verdict on the forged sig under the identity key —
        // recorded, never assumed. ring ref10 rejects the weak key during
        // decompression; if a ring build ever accepted the identity equation
        // the consumer MUST still refuse (the forged sig isn't a genuine
        // receipt signature under the pinned key either way).
        let raw = UnparsedPublicKey::new(&ED25519, &identity_a).verify(&msg, &forged);
        // Forged envelope under an identity-pinned trust set must refuse.
        let kr_id = vec![TrustedKey::capture_test(
            "agenticos-native-owner",
            "synthetic-owner-0001",
            1,
            identity_a,
        )];
        let env = format!("{}.{}.{}", segs[0], segs[1], b64(&forged));
        assert!(
            verify_receipt_format(&env, &kr_id, now, 10).is_err(),
            "identity-key forged signature must refuse (raw ring ok={})",
            raw.is_ok()
        );
        // The same forged envelope under the GOOD pinned key also refuses —
        // the signature is forged regardless of the trust set.
        let kr_good = keyring_v1(i1.public_key);
        assert!(verify_receipt_format(&env, &kr_good, now, 10).is_err());
        // Genuine receipt still verifies under the good key (positive control).
        assert!(verify_receipt_format(&i1.envelope, &kr_good, now, 10).is_ok());
    }
}
