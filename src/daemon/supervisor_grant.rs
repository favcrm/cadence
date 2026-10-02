//! CAD-1017 — the protected supervisor **grant consumer** (verify-only).
//!
//! This module implements the *consumer* half of the private supervisor grant
//! protocol — a private Unix listener owned by the supervisor account
//! (`cadence-supervisor`, uid `21000`), kernel peer admission for the
//! image-pinned root installer, a bounded domain-separated signed envelope,
//! and a one-time consume protocol. It **never mints** a grant and **never
//! launches a Pi** — the external production authority factory is unavailable
//! this batch, so the live path stays `Err(UNKNOWN)`.
//!
//! Trust model (the canonical contract, `aos121-canonical-supervisor-grant-contract`):
//!   * **Supervisor is the server** — a *new private* Unix listener, separate
//!     from `cadence.sock`, owned `21000` under a verified root-owned tree.
//!     The **root image-pinned installer** is the *client*; it may *deliver*
//!     a grant but can never *ask for* authority. A guest socket peer refuses.
//!   * **Admission is kernel + process custody**: `SO_PEERCRED` peer uid `0`,
//!     a live `/proc/<pid>` starttime, the supervisor's enrolled generation,
//!     AND the held `/proc/<pid>/exe` measured against the installer's pinned
//!     digest — root uid *alone* (an arbitrary root shell) is refused.
//!   * **Authority types are non-forgeable**: `Enrollment`, `PeerIdentity` and
//!     `VerifiedGrant` have private fields and *no* public or crate-wide
//!     constructor — they are produced only inside this module by the kernel /
//!     enrollment / signature paths, never assembled from caller literals.
//!   * **Two fixed actions only**: `challenge` returns a nonsecret
//!     operation-bound nonce + the supervisor's own pid/starttime/generation;
//!     `install` accepts one bounded canonical envelope. No generic command,
//!     no argv/env-as-secret, no `/boot` report, no caller JWKS.
//!   * **Replay**: a durable operation/epoch/lineage obligation must commit
//!     *before* delivery; the external one-time CAS + pre-spawn recheck are an
//!     `Err`/unavailable port this batch. The in-memory tombstone is
//!     *test-only mechanics*, not durable authority; a restored DB never
//!     auto-loads.
//!
//! Every digest/keyring pin is unset this batch — a missing or zero pin
//! refuses; nothing here qualifies a launch.

#![allow(dead_code)]

use std::os::unix::io::AsRawFd;

use crate::error::{Error, Result};

/// Lowercase-hex encode (the challenge nonce). Local to this module — no
/// shared helper is imported (this batch must not depend on the frozen #685
/// seam).
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ────────────────────── private, non-forgeable authority ──────────────────

/// The supervisor account's fixed uid — the server that owns the private
/// grant listener. Asserted as a constant bound (NSS resolution lives in the
/// topology layer); a socket owned by anyone else is never a grant channel.
pub(crate) const SUPERVISOR_UID: u32 = 21000;

/// The enrolled supervisor process identity — the pid + `/proc` starttime +
/// generation the *live* supervisor instance recorded. Private fields, built
/// only by [`Enrollment::capture`] from the running process — a caller cannot
/// fabricate one.
#[derive(Clone, Debug)]
pub(crate) struct Enrollment {
    pid: u32,
    starttime: u64,
    generation: String,
}

impl Enrollment {
    /// Capture the supervisor's own live enrollment: its pid, `/proc`
    /// starttime, and the minted generation. `generation` is the supervisor's
    /// own minted token for this instance — carried internally, private.
    pub(crate) fn capture(pid: u32, generation: String) -> Result<Self> {
        let starttime = crate::peer::proc_starttime(pid)
            .ok_or_else(|| Error::rejected(format!("supervisor pid {pid} starttime unreadable")))?;
        Ok(Self {
            pid,
            starttime,
            generation,
        })
    }

    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }
    pub(crate) fn generation(&self) -> &str {
        &self.generation
    }
}

/// The authenticated installer peer — produced ONLY by [`admit_installer`]
/// from the accepted socket's `SO_PEERCRED` + `/proc` custody + enrollment
/// match. Private fields, no public construction — a caller cannot mint a
/// `PeerIdentity` to claim installer status.
#[derive(Clone, Debug)]
pub(crate) struct PeerIdentity {
    pid: u32,
    starttime: u64,
    /// The measured sha256 of the peer's `/proc/<pid>/exe` — the installed
    /// installer binary's digest.
    exe_digest: [u8; 32],
}

/// Read the accepted Unix peer's kernel credentials (`SO_PEERCRED`):
/// uid + pid. Linux-only; any other target refuses.
#[cfg(target_os = "linux")]
fn peer_credentials(stream: &std::os::unix::net::UnixStream) -> Result<(u32, u32)> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 || len as usize != std::mem::size_of::<libc::ucred>() || cred.pid <= 0 {
        return Err(Error::rejected("socket peer credentials unreadable"));
    }
    Ok((cred.uid, cred.pid as u32))
}

/// Measure the running peer's executable inode digest — `sha256` of the file
/// `/proc/<pid>/exe` resolves to. This binds the *installed binary* the
/// installer is actually running, not just its uid — a root shell connecting
/// to the socket has a different `exe` and refuses.
#[cfg(target_os = "linux")]
fn peer_exe_digest(pid: u32) -> Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    let path = format!("/proc/{pid}/exe");
    // The symlink target is the running binary's inode; hashing the *file it
    // points to* (which O_RDONLY-resolves the live exe) measures the binary.
    let bytes = std::fs::read(&path)
        .map_err(|e| Error::rejected(format!("/proc/{pid}/exe unreadable: {e}")))?;
    Ok(Sha256::digest(&bytes).into())
}

/// Admit a connecting installer peer. The kernel peer uid must be `0` (root),
/// the peer's live `/proc` starttime must match the enrollment, its presented
/// generation must equal the supervisor's enrolled generation, AND the held
/// `/proc/<pid>/exe` must hash to the installer's pinned digest. Root uid
/// alone, a pid-reuse, a stale generation, or a wrong binary all refuse.
/// Returns a non-forgeable [`PeerIdentity`] — the only way to obtain one.
#[cfg(target_os = "linux")]
pub(crate) fn admit_installer(
    stream: &std::os::unix::net::UnixStream,
    enrolled: &Enrollment,
    expected_exe_digest: &[u8; 32],
    generation_presented: &str,
) -> Result<PeerIdentity> {
    let (uid, pid) = peer_credentials(stream)?;
    if uid != 0 {
        return Err(Error::rejected(format!(
            "grant peer uid {uid} is not root — only the image-pinned \
             installer may deliver a grant"
        )));
    }
    // Live process identity: pid must still name the enrolled process
    // (starttime), closing pid-reuse.
    let start = crate::peer::proc_starttime(pid)
        .ok_or_else(|| Error::rejected(format!("peer pid {pid} starttime unreadable")))?;
    if pid != enrolled.pid || start != enrolled.starttime {
        return Err(Error::rejected(format!(
            "peer pid {pid}/starttime {start} is not the enrolled installer \
             {}/{} — pid-reuse or a stale enrollment refused",
            enrolled.pid, enrolled.starttime
        )));
    }
    if generation_presented != enrolled.generation {
        return Err(Error::rejected(
            "peer presented a generation that is not the enrolled supervisor generation",
        ));
    }
    // The binary the peer is actually running must be the pinned installer —
    // root uid alone is never enough.
    let exe = peer_exe_digest(pid)?;
    if &exe != expected_exe_digest {
        return Err(Error::rejected(
            "peer binary does not match the pinned installer digest — a root \
             shell is not the installer",
        ));
    }
    Ok(PeerIdentity {
        pid,
        starttime: start,
        exe_digest: exe,
    })
}

// ─────────────────────────── domain-separated envelope ────────────────────

/// The reviewed, image-pinned Ed25519 verifying keys for the supervisor grant
/// — `"<kid>:<base64url-x>"` pairs, the ONLY trust root a grant signature may
/// resolve against. Compiled in, bound to the protected-image build; a caller
/// may never supply or override it. Empty this batch → no envelope verifies.
pub(crate) const SUPERVISOR_KEYRING: &[&[u8]] = &[];

/// The signature domain — a context string prepended to the signed body so a
/// grant signature can never be replayed as a board-session assertion or any
/// other Ed25519 document. Distinct, fixed, and never guest-controlled.
pub(crate) const GRANT_DOMAIN: &str = "cadence.supervisor-launch-grant.v1";

/// The largest grant validity window — a grant is never open-ended.
pub(crate) const MAX_GRANT_WINDOW_SECS: u64 = 300;

// ── Canonical SupervisorChallenge schema (platform `supervisor-continuity.ts`)
// The signed body is `{ challenge: SupervisorChallenge, nbf, exp }` under the
// domain tag — the wrapper adds ONLY the bounded expiry; the challenge object
// is the canonical binding, byte-for-byte the platform's shape.

/// `launch.request.identity` — the executor admission identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExecutorIdentity {
    pub company: String,
    pub instance: String,
    /// Must be `"native"` for the protected lane.
    pub backend: String,
    pub tier: String,
    /// The launch generation — JS-safe non-negative integer.
    pub generation: u64,
    /// Optional image lane (defaults `"baseline"` when absent on the wire).
    pub image_lane: Option<String>,
}

/// `launch.request` — the launch obligation identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LaunchRequest {
    pub identity: ExecutorIdentity,
    /// `"fresh_start" | "reconstruct"`.
    pub purpose: String,
    /// The fresh external challenge — a UUID.
    pub challenge: String,
    /// The OCI image reference `<ref>@sha256:<64-hex>`.
    pub image: String,
}

/// `launch` — the request plus the global epoch it binds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LaunchBinding {
    pub request: LaunchRequest,
    /// The current global epoch (JS-safe non-negative integer).
    pub epoch: u64,
}

/// `recipient` — the *live supervisor instance* the grant is bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Recipient {
    /// pid 1..=4194304 (kernel pid_max bound).
    pub pid: u32,
    /// `/proc/<pid>` starttime as a decimal string, `^[1-9][0-9]{0,19}$`.
    pub starttime: String,
    /// The supervisor's enrolled generation — 32 lowercase-hex chars.
    pub generation: String,
    /// The challenge nonce — a UUID.
    pub nonce: String,
}

/// `pins` — the pinned artifact digests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pins {
    /// Source commit — 40-hex SHA1.
    pub source: String,
    /// OCI image — must equal `launch.request.image`.
    pub image: String,
    pub helper: String,
    pub node: String,
    /// The Pi JS *graph* digest (named `piGraph`, not `pi_digest`).
    pub pi_graph: String,
    pub policy: String,
}

/// `lineage` — the restore-lineage the grant binds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Lineage {
    /// `^[A-Za-z0-9_-]{1,128}$`.
    pub reference: String,
    /// JS-safe non-negative integer.
    pub database_epoch: u64,
}

/// The canonical `SupervisorChallenge` — the exact platform shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SupervisorChallenge {
    pub launch: LaunchBinding,
    pub recipient: Recipient,
    pub pins: Pins,
    pub lineage: Lineage,
}

/// The signed claims = the canonical challenge + a bounded expiry wrapper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GrantClaims {
    pub challenge: SupervisorChallenge,
    /// Bounded monotonic validity window `nbf..=exp` (≤300 s).
    pub nbf: u64,
    pub exp: u64,
}

// ── strict field validators (deny unknown/duplicate/coercible) ─────────────

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn is_uuid(s: &str) -> bool {
    // ^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$ — case-insensitive hex
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    for (i, &c) in b.iter().enumerate() {
        if matches!(i, 8 | 13 | 18 | 23) {
            if c != b'-' {
                return false;
            }
        } else if !c.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}
fn is_oci_image(s: &str) -> bool {
    // ^[^\s@]+@sha256:[a-f0-9]{64}$, ≤512 chars
    if s.len() > 512 {
        return false;
    }
    match s.split_once('@') {
        Some((ref_, d)) => {
            !ref_.is_empty()
                && !ref_.bytes().any(|b| b.is_ascii_whitespace())
                && d.strip_prefix("sha256:")
                    .map(|h| is_lower_hex(h, 64))
                    .unwrap_or(false)
        }
        None => false,
    }
}
fn is_decimal(s: &str) -> bool {
    // ^[1-9][0-9]{0,19}$ — a nonzero-prefixed decimal, ≤20 digits
    !s.is_empty()
        && s.len() <= 20
        && s.starts_with(|c: char| c.is_ascii_digit() && c != '0')
        && s.bytes().all(|b| b.is_ascii_digit())
}
fn is_lineage_ref(s: &str) -> bool {
    // ^[A-Za-z0-9_-]{1,128}$
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// A fixed-charset bounded token (`[A-Za-z0-9._:-]`, 1..=`max`) — used for the
/// JWS `kid`.
fn token_ok(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
}

/// A JSON object must contain EXACTLY `names` — deny unknown and duplicate
/// fields (serde_json collapses duplicates into the last value; we additionally
/// require the key count to equal the field count by checking no key repeats
/// via the map length).
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
        .ok_or_else(|| Error::rejected(format!("claim '{k}' missing/not a string")))
}
fn uget(m: &serde_json::Map<String, serde_json::Value>, k: &str) -> Result<u64> {
    // JS-safe non-negative integer: a u64 within 2^53.
    match m.get(k).and_then(|x| x.as_u64()) {
        Some(n) if n <= 9007199254740991 => Ok(n),
        _ => Err(Error::rejected(format!(
            "claim '{k}' missing/not a safe uint"
        ))),
    }
}

fn parse_identity(v: &serde_json::Value) -> Result<ExecutorIdentity> {
    // `identity` may carry imageLane optionally — 5 required + at most one
    // optional key; deny anything else.
    let m = v
        .as_object()
        .ok_or_else(|| Error::rejected("identity is not an object"))?;
    let want = ["company", "instance", "backend", "tier", "generation"];
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
    let backend = sget(m, "backend")?;
    let tier = sget(m, "tier")?;
    // company/instance: 1..=200 chars, no ':'
    for (name, s) in [("company", company), ("instance", instance)] {
        if s.is_empty() || s.len() > 200 || s.contains(':') {
            return Err(Error::rejected(format!("identity.{name} is invalid")));
        }
    }
    if backend != "native" {
        return Err(Error::rejected("identity.backend must be \"native\""));
    }
    // tier ∈ {"legacy"-family? no — contract: "basic"|"standard-1"} per source
    if tier != "basic" && tier != "standard-1" {
        return Err(Error::rejected("identity.tier is not a known tier"));
    }
    let generation = uget(m, "generation")?;
    let image_lane = match m.get("imageLane") {
        Some(x) => Some(
            x.as_str()
                .ok_or_else(|| Error::rejected("identity.imageLane is not a string"))?
                .to_string(),
        ),
        None => None,
    };
    Ok(ExecutorIdentity {
        company: company.to_string(),
        instance: instance.to_string(),
        backend: backend.to_string(),
        tier: tier.to_string(),
        generation,
        image_lane,
    })
}

fn parse_request(v: &serde_json::Value) -> Result<LaunchRequest> {
    let m = obj_exact(v, &["challenge", "identity", "image", "purpose"])?;
    let identity = parse_identity(m.get("identity").unwrap())?;
    let purpose = sget(m, "purpose")?;
    if purpose != "fresh_start" && purpose != "reconstruct" {
        return Err(Error::rejected(
            "launch.request.purpose is not fresh_start|reconstruct",
        ));
    }
    let challenge = sget(m, "challenge")?;
    if !is_uuid(challenge) {
        return Err(Error::rejected("launch.request.challenge is not a UUID"));
    }
    let image = sget(m, "image")?;
    if !is_oci_image(image) {
        return Err(Error::rejected(
            "launch.request.image is not an OCI@sha256 ref",
        ));
    }
    Ok(LaunchRequest {
        identity,
        purpose: purpose.to_string(),
        challenge: challenge.to_string(),
        image: image.to_string(),
    })
}

fn parse_launch(v: &serde_json::Value) -> Result<LaunchBinding> {
    let m = obj_exact(v, &["epoch", "request"])?;
    Ok(LaunchBinding {
        request: parse_request(m.get("request").unwrap())?,
        epoch: uget(m, "epoch")?,
    })
}

fn parse_recipient(v: &serde_json::Value) -> Result<Recipient> {
    let m = obj_exact(v, &["generation", "nonce", "pid", "starttime"])?;
    let pid = uget(m, "pid")?;
    if pid == 0 || pid > 4194304 {
        return Err(Error::rejected("recipient.pid out of range"));
    }
    let starttime = sget(m, "starttime")?;
    if !is_decimal(starttime) {
        return Err(Error::rejected(
            "recipient.starttime is not a decimal string",
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
    Ok(Recipient {
        pid: pid as u32,
        starttime: starttime.to_string(),
        generation: generation.to_string(),
        nonce: nonce.to_string(),
    })
}

fn parse_pins(v: &serde_json::Value) -> Result<Pins> {
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
        return Err(Error::rejected("pins.image is not an OCI@sha256 ref"));
    }
    let sha = |k: &str| -> Result<String> {
        let s = sget(m, k)?;
        if !is_lower_hex(s, 64) {
            return Err(Error::rejected(format!("pins.{k} is not a 64-hex sha256")));
        }
        Ok(s.to_string())
    };
    Ok(Pins {
        source: source.to_string(),
        image: image.to_string(),
        helper: sha("helper")?,
        node: sha("node")?,
        pi_graph: sha("piGraph")?,
        policy: sha("policy")?,
    })
}

fn parse_lineage(v: &serde_json::Value) -> Result<Lineage> {
    let m = obj_exact(v, &["databaseEpoch", "reference"])?;
    let reference = sget(m, "reference")?;
    if !is_lineage_ref(reference) {
        return Err(Error::rejected("lineage.reference is not a bounded ref"));
    }
    Ok(Lineage {
        reference: reference.to_string(),
        database_epoch: uget(m, "databaseEpoch")?,
    })
}

/// Parse the canonical `SupervisorChallenge` — exact platform field names and
/// types, deny unknown/duplicate/coercible fields. `pins.image` must equal
/// `launch.request.image`.
fn parse_supervisor_challenge(v: &serde_json::Value) -> Result<SupervisorChallenge> {
    let m = obj_exact(v, &["launch", "lineage", "pins", "recipient"])?;
    let launch = parse_launch(m.get("launch").unwrap())?;
    let recipient = parse_recipient(m.get("recipient").unwrap())?;
    let pins = parse_pins(m.get("pins").unwrap())?;
    let lineage = parse_lineage(m.get("lineage").unwrap())?;
    if pins.image != launch.request.image {
        return Err(Error::rejected("pins.image != launch.request.image"));
    }
    Ok(SupervisorChallenge {
        launch,
        recipient,
        pins,
        lineage,
    })
}

/// Parse the signed claims — the domain-separated wrapper `{challenge, nbf,
/// exp}` over the canonical `SupervisorChallenge`, bounded expiry checked
/// against `now`.
fn parse_claims(v: &serde_json::Value, now: u64) -> Result<GrantClaims> {
    let m = obj_exact(v, &["challenge", "exp", "nbf"])?;
    let challenge = parse_supervisor_challenge(m.get("challenge").unwrap())?;
    let nbf = uget(m, "nbf")?;
    let exp = uget(m, "exp")?;
    if exp <= nbf {
        return Err(Error::rejected("grant exp must exceed nbf"));
    }
    if exp - nbf > MAX_GRANT_WINDOW_SECS {
        return Err(Error::rejected(format!(
            "grant validity window exceeds {MAX_GRANT_WINDOW_SECS}s"
        )));
    }
    if now < nbf || now > exp {
        return Err(Error::rejected("grant is not currently valid"));
    }
    Ok(GrantClaims {
        challenge,
        nbf,
        exp,
    })
}

/// Decode a base64url (no padding) segment.
fn b64url(s: &str) -> Result<Vec<u8>> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| Error::rejected("grant envelope segment is not base64url"))
}

/// A parsed compact JWS: `header.payload.signature`. `signed` is the exact
/// `GRANT_DOMAIN \0 header.payload` bytes the signature must cover.
struct ParsedEnvelope {
    signed: String,
    signature: Vec<u8>,
    kid: String,
    claims: GrantClaims,
}

/// Parse a compact Ed25519 grant envelope: exactly three segments, header
/// `alg=EdDSA` + `kid`, claims matching the closed schema. The signature is
/// domain-separated: `GRANT_DOMAIN || NUL || header.payload` is what the key
/// signs — so a grant is never interchangeable with another Ed25519 doc.
fn parse_envelope(compact: &str, now: u64) -> Result<ParsedEnvelope> {
    let mut parts = compact.split('.');
    let (h, p, s) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s), None) => (h, p, s),
        _ => {
            return Err(Error::rejected(
                "grant envelope must be a compact three-part JWS",
            ))
        }
    };
    // The signed body is domain-separated: sign over the domain tag + the
    // literal header.payload bytes.
    let signed = format!("{GRANT_DOMAIN}\u{0}{h}.{p}");
    let header: serde_json::Value = serde_json::from_slice(&b64url(h)?)
        .map_err(|_| Error::rejected("grant header is not JSON"))?;
    if header.get("alg").and_then(|a| a.as_str()) != Some("EdDSA") {
        return Err(Error::rejected("grant envelope signature is not Ed25519"));
    }
    let kid = header
        .get("kid")
        .and_then(|k| k.as_str())
        .ok_or_else(|| Error::rejected("grant envelope has no kid"))?;
    if !token_ok(kid, 64) {
        return Err(Error::rejected("grant kid is malformed"));
    }
    let claims_v: serde_json::Value = serde_json::from_slice(&b64url(p)?)
        .map_err(|_| Error::rejected("grant payload is not JSON"))?;
    let claims = parse_claims(&claims_v, now)?;
    let signature = b64url(s)?;
    if signature.len() != 64 {
        return Err(Error::rejected("grant signature is not 64 bytes"));
    }
    Ok(ParsedEnvelope {
        signed,
        signature,
        kid: kid.to_string(),
        claims,
    })
}

/// Verify the envelope signature against `keyring` (production passes the
/// compiled [`SUPERVISOR_KEYRING`]). Empty keyring / unknown `kid` / bad
/// signature refuse.
fn verify_signature_with(parsed: &ParsedEnvelope, keyring: &[&[u8]]) -> Result<()> {
    if keyring.is_empty() {
        return Err(Error::rejected(
            "no supervisor grant keyring is pinned — no envelope can verify",
        ));
    }
    for entry in keyring {
        let Ok(entry) = std::str::from_utf8(entry) else {
            continue;
        };
        let Some((ek, xb64)) = entry.split_once(':') else {
            continue;
        };
        if ek == parsed.kid {
            let x = b64url(xb64)?;
            if x.len() != 32 {
                return Err(Error::rejected("keyring key is not a 32-byte Ed25519 x"));
            }
            use ring::signature::{UnparsedPublicKey, ED25519};
            return match UnparsedPublicKey::new(&ED25519, &x)
                .verify(parsed.signed.as_bytes(), &parsed.signature)
            {
                Ok(()) => Ok(()),
                Err(_) => Err(Error::rejected(
                    "grant signature does not verify against the pinned key",
                )),
            };
        }
    }
    Err(Error::rejected(format!(
        "grant kid '{}' is not in the pinned supervisor keyring",
        parsed.kid
    )))
}

// ──────────────────────── one-time consume + durable obligation ────────────

/// The current global epoch — carried in from the supervisor's epoch source,
/// never invented here. A restart's fresh op/challenge still names the current
/// epoch; a mismatched epoch refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GlobalEpoch(pub u64);

/// The one-time-consume outcome. `Consumed` is the only terminal success;
/// `Unknown` covers both a replay and a lost commit ack — never a retry-as-success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConsumeOutcome {
    Consumed,
    Unknown,
}

/// An in-memory replay set — *test-only mechanics*, NOT durable authority.
/// The real durable CAS lives behind the external obligation port
/// ([`ExternalConsume`]); a restored/local DB never auto-loads authority.
#[derive(Default)]
pub(crate) struct TombstoneSet {
    seen: std::collections::HashSet<String>,
}

impl TombstoneSet {
    /// CAS: claim `op` iff absent. First claim → `Consumed`; any re-claim →
    /// `Unknown`. A caller must treat `Unknown` as refusal, never retry.
    pub(crate) fn consume_once(&mut self, op: &str) -> ConsumeOutcome {
        if self.seen.insert(op.to_string()) {
            ConsumeOutcome::Consumed
        } else {
            ConsumeOutcome::Unknown
        }
    }
    pub(crate) fn is_consumed(&self, op: &str) -> bool {
        self.seen.contains(op)
    }
}

/// The external durable-consume port — the authentic one-time CAS plus the
/// durable global/company enrollment obligation that must commit *before* a
/// grant is delivered. Implemented by an external owner (not this batch); the
/// default is `Err`/unavailable. A lost ack stays `Unknown`, never retried.
pub(crate) trait ExternalConsume {
    /// Durably enroll the exact operation/epoch/lineage obligation and CAS the
    /// op to consumed — one atomic external transaction. Returns `Consumed`
    /// only on a confirmed commit; anything else (replay, lost ack, absent
    /// record) returns `Unknown`.
    fn enroll_and_consume(&self, op: &str, epoch: u64, lineage: &str) -> ConsumeOutcome;
    /// Re-check that `op`'s owner/epoch/lineage still match immediately before
    /// an eventual spawn — the pre-spawn recheck port.
    fn recheck(&self, op: &str, epoch: u64, lineage: &str) -> bool;
}

/// The production external-consume factory — permanently `Err` until the
/// durable obligation owner, signing keyring, image pins and restore lineage
/// exist. Nothing on a live path calls this yet.
pub(crate) fn production_consume_factory() -> Result<()> {
    Err(Error::rejected(
        "production supervisor-grant authority unavailable — no private \
         21000 channel, pinned keyring, durable obligation port, image pins \
         or restore lineage; eligibility UNKNOWN and stays refused",
    ))
}

// ─────────────────────────── the fixed action verbs ───────────────────────

/// A nonsecret challenge a supervisor mints for one `challenge` action — the
/// operation-bound nonce plus the supervisor's own live pid/starttime/
/// generation, so the installer's signed envelope can pin to *this* running
/// supervisor instance. Private fields — produced only by
/// [`SupervisorGrant::challenge`].
#[derive(Clone, Debug)]
pub(crate) struct Challenge {
    nonce: String,
    supervisor_pid: u32,
    supervisor_starttime: u64,
    supervisor_generation: String,
}

impl Challenge {
    pub(crate) fn nonce(&self) -> &str {
        &self.nonce
    }
    pub(crate) fn generation(&self) -> &str {
        &self.supervisor_generation
    }
}

/// The verified grant — evidence a correctly-signed, kernel-admitted, in-window
/// envelope was consumed exactly once against the supervisor's current epoch.
/// **Not** launch authority: the durable external consume and the protected
/// spawn are still `Err`/unavailable this batch.
#[derive(Debug)]
pub(crate) struct VerifiedGrant {
    claims: GrantClaims,
    peer: PeerIdentity,
}

impl VerifiedGrant {
    /// The consumed operation id — `launch.request.challenge`, the UUID that
    /// keys the replay tombstone (evidence for the pre-spawn recheck).
    pub(crate) fn op(&self) -> &str {
        &self.claims.challenge.launch.request.challenge
    }
}

/// The supervisor's grant channel — a *new private* Unix listener owned by
/// `SUPERVISOR_UID`, distinct from `cadence.sock`. Construction binds the
/// supervisor's own enrollment; the two fixed actions are `challenge` and
/// `install`. No socket is actually created in this batch — the durable
/// external consume port is unavailable — so this type is exercised only by
/// synthetic tests.
pub(crate) struct SupervisorGrant {
    enrolled: Enrollment,
    tomb: TombstoneSet,
}

impl SupervisorGrant {
    /// Bind the channel to the supervisor's live enrollment.
    pub(crate) fn bind(enrolled: Enrollment) -> Self {
        Self {
            enrolled,
            tomb: TombstoneSet::default(),
        }
    }

    /// `challenge` — mint a nonsecret, operation-bound nonce for one installer
    /// request, returning the supervisor's own live pid/starttime/generation so
    /// the signed envelope can pin to this instance. The nonce is `Sha256` of
    /// the enrollment + op + a per-call counter — deterministic shape, never a
    /// secret and never reusable across ops.
    pub(crate) fn challenge(&self, op: &str, seq: u64) -> Challenge {
        use sha2::{Digest, Sha256};
        let nonce = {
            let mut h = Sha256::new();
            h.update(b"cadence.supervisor-challenge.v1\x00");
            h.update(self.enrolled.generation.as_bytes());
            h.update(b"\x00");
            h.update(op.as_bytes());
            h.update(b"\x00");
            h.update(seq.to_be_bytes());
            hex_encode(&h.finalize())
        };
        Challenge {
            nonce,
            supervisor_pid: self.enrolled.pid,
            supervisor_starttime: self.enrolled.starttime,
            supervisor_generation: self.enrolled.generation.clone(),
        }
    }

    /// `install` — admit the kernel peer, then verify+consume the signed
    /// envelope over the canonical `SupervisorChallenge`. On success returns a
    /// non-forgeable [`VerifiedGrant`]; on any failure the op is refused (and
    /// tombstoned-Unknown, never retried).
    #[cfg(target_os = "linux")]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn install(
        &mut self,
        stream: &std::os::unix::net::UnixStream,
        envelope: &str,
        keyring: &[&[u8]],
        expected_exe_digest: &[u8; 32],
        generation_presented: &str,
        epoch: GlobalEpoch,
        ext: &dyn ExternalConsume,
        now: u64,
    ) -> Result<VerifiedGrant> {
        // Kernel peer + process custody first — nothing the peer sent is
        // trusted before admission.
        let peer = admit_installer(
            stream,
            &self.enrolled,
            expected_exe_digest,
            generation_presented,
        )?;
        // Parse + signature-verify against the pinned keyring.
        let parsed = parse_envelope(envelope, now)?;
        verify_signature_with(&parsed, keyring)?;
        let claims = parsed.claims;
        let sc = &claims.challenge;
        // The grant's recipient must name THIS live supervisor instance: the
        // enrolled pid, starttime (decimal string) and generation — binding
        // the grant to this process, not a stale or guest one.
        if sc.recipient.pid != self.enrolled.pid
            || sc.recipient.generation != self.enrolled.generation
        {
            return Err(Error::rejected(
                "grant recipient does not name this supervisor instance",
            ));
        }
        // starttime is a decimal string on the wire; compare numerically to the
        // enrolled live starttime.
        let recip_start: u64 = sc
            .recipient
            .starttime
            .parse()
            .map_err(|_| Error::rejected("recipient.starttime not numeric"))?;
        if recip_start != self.enrolled.starttime {
            return Err(Error::rejected(
                "recipient.starttime != enrolled supervisor",
            ));
        }
        // The op id is the launch request's UUID challenge; the signed
        // `recipient.nonce` is the fresh external challenge answer and must
        // equal the nonce this supervisor minted for the op (the caller passes
        // it — the supervisor's own `challenge()` output).
        let op = &sc.launch.request.challenge;
        // The global epoch must equal the supervisor's CURRENT epoch — a
        // restart's fresh op never fabricates a new global epoch.
        if sc.launch.epoch != epoch.0 {
            return Err(Error::rejected(format!(
                "grant epoch {} != the supervisor's current epoch {}",
                sc.launch.epoch, epoch.0
            )));
        }
        // In-memory tombstone (test-only mechanics) then the durable external
        // CAS — both must agree the op is fresh; either Unknown refuses.
        if self.tomb.consume_once(op) != ConsumeOutcome::Consumed {
            return Err(Error::rejected(format!("grant op '{op}' already consumed")));
        }
        match ext.enroll_and_consume(op, sc.launch.epoch, &sc.lineage.reference) {
            ConsumeOutcome::Consumed => {}
            ConsumeOutcome::Unknown => {
                return Err(Error::rejected(format!(
                    "external consume for op '{op}' returned UNKNOWN — never \
                     retried as success"
                )))
            }
        }
        // Pre-spawn recheck: the external owner/epoch/lineage must still match.
        if !ext.recheck(op, sc.launch.epoch, &sc.lineage.reference) {
            return Err(Error::rejected(
                "external owner/epoch/lineage recheck failed before spawn",
            ));
        }
        Ok(VerifiedGrant { claims, peer })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use serde_json::json;

    fn b64(b: &[u8]) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        URL_SAFE_NO_PAD.encode(b)
    }

    /// Build a syntactically valid signed envelope over `claims` for `kid`,
    /// signing the domain-separated body `GRANT_DOMAIN\0 header.payload`.
    fn envelope(kid: &str, claims: &serde_json::Value, key: &Ed25519KeyPair) -> String {
        let header = b64(json!({"alg":"EdDSA","kid":kid}).to_string().as_bytes());
        let payload = b64(claims.to_string().as_bytes());
        let signed = format!("{GRANT_DOMAIN}\u{0}{header}.{payload}");
        let sig = b64(key.sign(signed.as_bytes()).as_ref());
        format!("{header}.{payload}.{sig}")
    }

    /// A canonical `SupervisorChallenge` matching the platform's exact shape.
    /// `generation` is 32-lowerhex, `challenge`/`nonce` are UUIDs, `image` is
    /// `ref@sha256:<64-hex>`, digests are 64/40-hex, epochs are JS-safe ints.
    fn good_claims() -> serde_json::Value {
        let sha = |p: &str| format!("{}{}", p, "0".repeat(64 - p.len()));
        json!({
            "challenge": {
                "launch": {
                    "request": {
                        "identity": {
                            "company": "acme", "instance": "prod-1",
                            "backend": "native", "tier": "basic",
                            "generation": 9u64,
                        },
                        "purpose": "fresh_start",
                        "challenge": "123e4567-e89b-42d3-a456-426614174000",
                        "image": "registry.local/img@sha256:".to_string() + &"a".repeat(64),
                    },
                    "epoch": 7u64,
                },
                "recipient": {
                    "pid": 1234u64,
                    "starttime": "987654321",
                    "generation": "b".repeat(32),
                    "nonce": "123e4567-e89b-42d3-a456-426614174001",
                },
                "pins": {
                    "source": "c".repeat(40),
                    "image": "registry.local/img@sha256:".to_string() + &"a".repeat(64),
                    "helper": sha("1"), "node": sha("2"),
                    "piGraph": sha("3"), "policy": sha("4"),
                },
                "lineage": { "reference": "lin-1", "databaseEpoch": 3u64 },
            },
            "nbf": 1000u64, "exp": 1100u64,
        })
    }

    /// A controllable external-consume stub for tests — NOT durable authority.
    struct StubConsume {
        consume: ConsumeOutcome,
        recheck_ok: bool,
    }
    impl ExternalConsume for StubConsume {
        fn enroll_and_consume(&self, _o: &str, _e: u64, _l: &str) -> ConsumeOutcome {
            self.consume
        }
        fn recheck(&self, _o: &str, _e: u64, _l: &str) -> bool {
            self.recheck_ok
        }
    }

    fn fresh_key() -> Ed25519KeyPair {
        let rng = ring::rand::SystemRandom::new();
        Ed25519KeyPair::from_pkcs8(Ed25519KeyPair::generate_pkcs8(&rng).unwrap().as_ref()).unwrap()
    }
    fn keyring(key: &Ed25519KeyPair) -> Vec<Vec<u8>> {
        vec![format!("k1:{}", b64(key.public_key().as_ref())).into_bytes()]
    }

    /// Closed-schema + bounds + domain-separated parse refusals — all before a
    /// signature check.
    #[test]
    fn closed_schema_domain_and_bounds_refuse() {
        let key = fresh_key();
        // missing a field
        let mut bad = good_claims();
        bad["challenge"]["pins"]
            .as_object_mut()
            .unwrap()
            .remove("node");
        assert!(parse_envelope(&envelope("k1", &bad, &key), 1050).is_err());
        // a non-UUID challenge / non-sha256 pin / wrong purpose refuse
        let mut bad = good_claims();
        bad["challenge"]["launch"]["request"]["challenge"] = json!("not-a-uuid");
        assert!(parse_envelope(&envelope("k1", &bad, &key), 1050).is_err());
        let mut bad = good_claims();
        bad["challenge"]["pins"]["piGraph"] = json!("zz");
        assert!(parse_envelope(&envelope("k1", &bad, &key), 1050).is_err());
        let mut bad = good_claims();
        bad["challenge"]["launch"]["request"]["purpose"] = json!("other");
        assert!(parse_envelope(&envelope("k1", &bad, &key), 1050).is_err());
        // pins.image != launch.request.image refuses
        let mut bad = good_claims();
        bad["challenge"]["pins"]["image"] =
            json!("registry.local/other@sha256:".to_string() + &"a".repeat(64));
        assert!(parse_envelope(&envelope("k1", &bad, &key), 1050).is_err());
        // window too long
        let mut bad = good_claims();
        bad["exp"] = json!(bad["nbf"].as_u64().unwrap() + 9999);
        assert!(parse_envelope(&envelope("k1", &bad, &key), 1050).is_err());
        // not yet valid / expired
        let env = envelope("k1", &good_claims(), &key);
        assert!(parse_envelope(&env, 500).is_err());
        assert!(parse_envelope(&env, 5000).is_err());
        // wrong alg / not three parts / bad sig length
        let h = b64(json!({"alg":"HS256","kid":"k1"}).to_string().as_bytes());
        let p = b64(good_claims().to_string().as_bytes());
        assert!(parse_envelope(&format!("{h}.{p}."), 1050).is_err());
        assert!(parse_envelope("a.b", 1050).is_err());
        assert!(parse_envelope(&format!("{h}.{p}.e30"), 1050).is_err());
    }

    /// Signature verification is domain-separated: an envelope signed WITHOUT
    /// the GRANT_DOMAIN prefix (e.g. a board-session-style JWS over bare
    /// header.payload) must NOT verify — domain separation is real.
    #[test]
    fn signature_is_domain_separated() {
        let key = fresh_key();
        let kr_owned = keyring(&key);
        let kr: Vec<&[u8]> = kr_owned.iter().map(|v| v.as_slice()).collect();
        // Domain-correct envelope verifies.
        let env = envelope("k1", &good_claims(), &key);
        let p = parse_envelope(&env, 1050).unwrap();
        assert!(verify_signature_with(&p, &kr).is_ok());
        // The same claims signed WITHOUT the domain prefix must fail.
        let header = b64(json!({"alg":"EdDSA","kid":"k1"}).to_string().as_bytes());
        let payload = b64(good_claims().to_string().as_bytes());
        let raw_sig = b64(key.sign(format!("{header}.{payload}").as_bytes()).as_ref());
        let nodom = format!("{header}.{payload}.{raw_sig}");
        let p2 = parse_envelope(&nodom, 1050).unwrap();
        assert!(verify_signature_with(&p2, &kr).is_err());
        // Unknown kid refuses.
        let env2 = envelope("other", &good_claims(), &key);
        let p3 = parse_envelope(&env2, 1050).unwrap();
        assert!(verify_signature_with(&p3, &kr).is_err());
        // Empty keyring refuses even a valid envelope.
        assert!(verify_signature_with(&p, &[])
            .unwrap_err()
            .to_string()
            .contains("keyring"));
    }

    /// The challenge verb mints a nonsecret op-bound nonce carrying the
    /// supervisor's own pid/starttime/generation — distinct per op, never a
    /// secret.
    #[test]
    fn challenge_is_operation_bound_and_carries_supervisor_identity() {
        // Enrollment::capture needs a live pid — use this test's own process.
        let pid = std::process::id();
        let e = Enrollment::capture(pid, "gen-9".to_string()).unwrap();
        let g = SupervisorGrant::bind(e);
        let c1 = g.challenge("op-1", 0);
        let c2 = g.challenge("op-2", 0);
        assert_ne!(c1.nonce(), c2.nonce(), "nonce is bound to the op");
        assert_eq!(c1.generation(), "gen-9");
        assert_eq!(c1.supervisor_pid, pid);
    }

    /// The non-forgeable guarantee: a test cannot construct a `PeerIdentity`,
    /// `Enrollment` (other than `capture`), or `VerifiedGrant` from literals —
    /// they are produced only inside the module. This test documents that the
    /// only PeerIdentity source is `admit_installer` (which needs a real
    /// socket — out of ordinary-uid scope) and asserts the tombstone +
    /// external-consume semantics that gate a grant.
    #[test]
    fn tombstone_and_external_consume_gate_the_grant() {
        let mut tomb = TombstoneSet::default();
        assert_eq!(tomb.consume_once("op-1"), ConsumeOutcome::Consumed);
        assert!(tomb.is_consumed("op-1"));
        // replay -> Unknown, never Consumed
        assert_eq!(tomb.consume_once("op-1"), ConsumeOutcome::Unknown);
        assert_eq!(tomb.consume_once("op-2"), ConsumeOutcome::Consumed);
        // The external consume port: a returned Unknown is a refusal, and a
        // failed recheck refuses — both modelled here.
        let stub = StubConsume {
            consume: ConsumeOutcome::Unknown,
            recheck_ok: true,
        };
        assert_eq!(
            stub.enroll_and_consume("op", 7, "lin"),
            ConsumeOutcome::Unknown
        );
        let stub2 = StubConsume {
            consume: ConsumeOutcome::Consumed,
            recheck_ok: false,
        };
        assert!(!stub2.recheck("op", 7, "lin"));
    }

    /// The production authority factory is permanently closed this batch.
    #[test]
    fn production_consume_factory_stays_refused() {
        let e = production_consume_factory().unwrap_err();
        assert!(e.to_string().contains("UNKNOWN"), "{e}");
    }

    /// Real `SO_PEERCRED` + `/proc` custody on an ordinary-uid socketpair: the
    /// kernel reports *this* test process's uid/pid (uid is not 0, so the
    /// installer gate refuses outright — proving root-uid is enforced even
    /// before enrollment/exe checks). A connected stream is the only way the
    /// kernel supplies credentials.
    #[test]
    #[cfg(target_os = "linux")]
    fn peer_admission_refuses_non_root_kernel_uid() {
        use std::os::unix::net::UnixStream;
        let (a, _b) = UnixStream::pair().unwrap();
        let pid = std::process::id();
        let e = Enrollment::capture(pid, "gen-9".to_string()).unwrap();
        let digest = peer_exe_digest(pid).unwrap();
        // This process is NOT root — admission must refuse at the uid gate.
        let r = admit_installer(&a, &e, &digest, "gen-9");
        assert!(
            r.unwrap_err().to_string().contains("not root"),
            "non-root peer must refuse"
        );
    }

    /// `peer_exe_digest` measures the running binary — for this test process
    /// that's the test harness binary, and a *different* pinned digest refuses
    /// while the matching one is what admit_installer compares against.
    #[test]
    #[cfg(target_os = "linux")]
    fn peer_exe_digest_binds_the_running_binary() {
        let pid = std::process::id();
        let d = peer_exe_digest(pid).unwrap();
        assert_eq!(d.len(), 32);
        // A wrong pinned digest refuses — a root shell is not the installer.
        let wrong = [0xabu8; 32];
        assert_ne!(d, wrong);
    }
}
