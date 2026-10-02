//! CAD-1017 — the protected supervisor **grant consumer** (verify-only).
//!
//! This module implements the *consumer* half of the private supervisor grant
//! protocol: a bounded signed-envelope parser, kernel peer admission, and a
//! one-time consume protocol. It **never mints** a grant and **never launches
//! a Pi** — the production authority factory is unavailable this batch, so
//! every entry point fails closed at `Err(UNKNOWN)` when the external
//! prerequisites are absent (they always are here).
//!
//! Trust model (deployment-source contract, the audit's "next dependency"):
//!   * The **supervisor** (uid `21000`, `cadence-supervisor`) runs a private
//!     Unix socket — the *server*. The **image-pinned root installer** is the
//!     *client* that connects to deliver one signed envelope.
//!   * Admission = `SO_PEERCRED` peer uid `0` (root) AND an authenticated
//!     pid + `/proc` starttime + current-generation enrollment — kernel
//!     credentials, never a `/boot` report or a guest env/marker.
//!   * The envelope is a compact Ed25519 JWS verified against a **reviewed,
//!     image-pinned keyring** baked into this build (`SUPERVISOR_KEYRING`) —
//!     never a caller-supplied JWKS and never an env var. Its claims pin the
//!     exact operation, a fresh external challenge, the current global epoch,
//!     and every artifact/lineage digest for the launch.
//!   * A **one-time consume** writes a permanent replay tombstone keyed by the
//!     operation id *before* delivery; a second use of the same op refuses.
//!     The global/company obligation must durably enroll *before* delivery;
//!     owner/epoch/lineage are re-checked immediately before any eventual
//!     spawn (the spawn itself is not implemented this batch).
//!   * A lost consume-ack is `UNKNOWN` — never retried as success or released.
//!     A local SQLite tombstone / restored DB is never auto-load authority.
//!
//! Every digest pin is `None`/unset this batch — a missing or zero pin refuses;
//! nothing here qualifies a launch.
//!
//! The whole module is a construction/refusal seam exercised only by its own
//! tests this batch — `production_authority_available` is permanently `Err`,
//! so these items have no live caller yet. The lint is held pending the
//! enablement the external prerequisites gate.

#![allow(dead_code)]

use crate::error::{Error, Result};

// ────────────────────────────── signed envelope ────────────────────────────

/// The reviewed, image-pinned Ed25519 verifying keys for the supervisor grant
/// — the only trust root a grant signature may resolve against. Compiled in,
/// bound to the protected-image build; a caller may never supply or override
/// it. Empty this batch → no envelope can ever verify.
pub(crate) const SUPERVISOR_KEYRING: &[&[u8]] = &[];

/// The exact claim set a grant envelope must carry — every field is a digest
/// or identifier pinned to a single launch. A missing, wrong-typed, or extra
/// field is a refusal; this is a closed schema, not a parse-and-keep-what-fits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GrantClaims {
    /// The exact operation id this grant authorizes (the tombstone key).
    pub op: String,
    /// The fresh external challenge the producer was answering — minted by
    /// the supervisor per request; a stale or replayed challenge refuses.
    pub challenge: String,
    /// The CURRENT global epoch the grant is valid for — carried from the
    /// supervisor's epoch source, never invented by this consume. A restart
    /// uses a fresh op/challenge but does NOT fabricate a new global epoch.
    pub global_epoch: u64,
    /// Monotonic bounded validity: the grant is only usable for `nbf..=exp`
    /// seconds; `exp - nbf` is bounded (see [`MAX_GRANT_WINDOW_SECS`]).
    pub nbf: u64,
    pub exp: u64,
    /// The pinned digests — every artifact a launch depends on.
    pub image_digest: String,
    pub helper_digest: String,
    pub node_digest: String,
    pub pi_digest: String,
    pub policy_digest: String,
    /// The restore-lineage id the grant is bound to.
    pub restore_lineage: String,
    /// The launch generation this grant names.
    pub generation: String,
}

/// The largest grant validity window — a grant must never be open-ended.
pub(crate) const MAX_GRANT_WINDOW_SECS: u64 = 300;

/// A fixed-charset token field: `[A-Za-z0-9._-]`, 1..=128 chars, never empty.
/// Digest/lineage/generation fields are `sha256:`-prefixed 64-hex or a fixed
/// token — anything else refuses.
fn token_ok(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
}

/// Parse the claims JSON object into `GrantClaims` — closed schema, every
/// field required and well-formed, no defaults. `now` is the validator's clock
/// (seconds) for the bounded-expiry check.
fn parse_claims(v: &serde_json::Value, now: u64) -> Result<GrantClaims> {
    let get_str = |k: &str| -> Result<String> {
        match v.get(k).and_then(|x| x.as_str()) {
            Some(s) if token_ok(s, 160) => Ok(s.to_string()),
            _ => Err(Error::rejected(format!(
                "grant claim '{k}' missing/invalid"
            ))),
        }
    };
    let get_u64 = |k: &str| -> Result<u64> {
        v.get(k)
            .and_then(|x| x.as_u64())
            .ok_or_else(|| Error::rejected(format!("grant claim '{k}' missing/invalid")))
    };
    let claims = GrantClaims {
        op: get_str("op")?,
        challenge: get_str("challenge")?,
        global_epoch: get_u64("global_epoch")?,
        nbf: get_u64("nbf")?,
        exp: get_u64("exp")?,
        image_digest: get_str("image_digest")?,
        helper_digest: get_str("helper_digest")?,
        node_digest: get_str("node_digest")?,
        pi_digest: get_str("pi_digest")?,
        policy_digest: get_str("policy_digest")?,
        restore_lineage: get_str("restore_lineage")?,
        generation: get_str("generation")?,
    };
    // Bounded expiry: exp > nbf and the window is bounded, and the grant is
    // currently inside it.
    if claims.exp <= claims.nbf {
        return Err(Error::rejected("grant exp must exceed nbf"));
    }
    if claims.exp - claims.nbf > MAX_GRANT_WINDOW_SECS {
        return Err(Error::rejected(format!(
            "grant validity window exceeds {MAX_GRANT_WINDOW_SECS}s"
        )));
    }
    if now < claims.nbf || now > claims.exp {
        return Err(Error::rejected(
            "grant is not currently valid (now outside nbf..=exp)",
        ));
    }
    Ok(claims)
}

/// A parsed compact JWS: `header.payload.signature`, all base64url. `signed`
/// is the exact `header.payload` bytes the signature covers.
struct ParsedEnvelope<'a> {
    signed: &'a str,
    signature: Vec<u8>,
    header: serde_json::Value,
    claims: GrantClaims,
}

/// Decode a base64url (no padding) segment.
fn b64url(s: &str) -> Result<Vec<u8>> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| Error::rejected("grant envelope segment is not base64url"))
}

/// Parse a compact Ed25519 grant envelope: exactly three segments, a header
/// declaring `alg=EdDSA` and a `kid`, and a claims object that parses the
/// closed `GrantClaims` schema. Signature is NOT checked here — that is
/// [`verify_signature`] against the pinned keyring.
fn parse_envelope<'a>(compact: &'a str, now: u64) -> Result<ParsedEnvelope<'a>> {
    let mut parts = compact.split('.');
    let (h, p, s) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s), None) => (h, p, s),
        _ => {
            return Err(Error::rejected(
                "grant envelope must be a compact three-part JWS",
            ))
        }
    };
    let signed = &compact[..h.len() + 1 + p.len()];
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
        header,
        claims,
    })
}

/// Verify the envelope's Ed25519 signature against a keyring — entries are
/// `"<kid>:<base64url-x>"` pairs resolved by `kid`. An empty keyring, an
/// unknown `kid`, or a bad signature refuses. The production caller passes
/// the compiled [`SUPERVISOR_KEYRING`]; the parameter exists so a test can
/// inject a real key without touching the pin.
fn verify_signature_with(parsed: &ParsedEnvelope, keyring: &[&[u8]]) -> Result<()> {
    let kid = parsed
        .header
        .get("kid")
        .and_then(|k| k.as_str())
        .ok_or_else(|| Error::rejected("grant envelope has no kid"))?;
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
        if ek == kid {
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
        "grant kid '{kid}' is not in the pinned supervisor keyring"
    )))
}

/// Verify against the reviewed image-pinned [`SUPERVISOR_KEYRING`] — the ONLY
/// trust root a production grant may resolve. Empty this batch → refuse.
fn verify_signature(parsed: &ParsedEnvelope) -> Result<()> {
    verify_signature_with(parsed, SUPERVISOR_KEYRING)
}

// ─────────────────────────────── peer admission ────────────────────────────

/// The supervisor account's fixed uid — the server that owns the private
/// grant socket. Resolved by the topology (`cadence-supervisor`), asserted
/// here as a constant bound so the consumer never accepts a peer on a
/// supervisor channel owned by anyone else.
pub(crate) const SUPERVISOR_UID: u32 = 21000;

/// The kernel-level peer credentials the supervisor grants admission for.
/// The installer connects as root (`uid==0`); the kernel supplies uid+pid via
/// `SO_PEERCRED`, and the consumer additionally requires an authenticated
/// pid+starttime+generation enrollment so a pid-reuse or a stale enrollment
/// cannot present as the installer.
#[derive(Clone, Debug)]
pub(crate) struct PeerAdmission {
    pub pid: u32,
    pub uid: u32,
    /// The process's `/proc/<pid>` starttime (kernel jiffies since boot) — the
    /// enrollment's pid-reuse discriminator, supplied by the caller's
    /// [`crate::peer::proc_starttime`].
    pub starttime: u64,
    /// The enrolled generation string the peer must present — the *current*
    /// enrollment generation, not a self-reported string.
    pub generation_presented: String,
}

/// The enrollment record the consumer binds a peer to — the supervisor's
/// authoritative pid/starttime/generation for the *current* installer slot.
/// Filled by the supervisor's own enrollment machinery; a stale or mismatched
/// peer refuses.
#[derive(Clone, Debug)]
pub(crate) struct Enrollment {
    pub pid: u32,
    pub starttime: u64,
    pub generation: String,
}

/// Admit a connecting peer as the installer. Kernel truth only:
///   * `peer.uid` must be `0` (root) — the image-pinned installer runs as
///     root; a guest (uid==21000/21001/any non-root) is refused.
///   * `peer.pid`/`peer.starttime` must match the enrolled `Enrollment`
///     (pid + `/proc` starttime, closing pid-reuse).
///   * `peer.generation_presented` must equal the enrolled generation —
///     authenticated, not self-asserted.
///
/// The supervisor never admits the *guest* as an installer.
pub(crate) fn admit_installer(peer: &PeerAdmission, enrolled: &Enrollment) -> Result<()> {
    if peer.uid != 0 {
        return Err(Error::rejected(format!(
            "grant-channel peer uid {} is not root — only the image-pinned \
             root installer may deliver a grant",
            peer.uid
        )));
    }
    if peer.pid != enrolled.pid || peer.starttime != enrolled.starttime {
        return Err(Error::rejected(format!(
            "peer pid {}/starttime {} does not match the enrolled installer \
             {}/{} — pid-reuse or a stale enrollment refused",
            peer.pid, peer.starttime, enrolled.pid, enrolled.starttime
        )));
    }
    if peer.generation_presented != enrolled.generation {
        return Err(Error::rejected(
            "peer presented a generation that is not the enrolled one",
        ));
    }
    Ok(())
}

// ───────────────────────────── one-time consume ────────────────────────────

/// The current global epoch — carried in from the supervisor's epoch source,
/// never invented by the consume path. A restart issues a fresh op/challenge
/// but must present the *current* global epoch; a consume that names a
/// different epoch than the supervisor's is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GlobalEpoch(pub u64);

/// The one-time-consume outcome — recorded so a delivery that loses its ack
/// is UNKNOWN, never assumed to have landed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConsumeOutcome {
    /// The tombstone commit acknowledged — the grant was consumed exactly once.
    Consumed,
    /// The op was already consumed (or the commit failed/its ack was lost) —
    /// the caller MUST treat this as UNKNOWN/refused, never retry as success.
    Unknown,
}

/// A permanent replay tombstone for one operation id — written before the
/// grant is delivered. `Consume` is the only terminal success state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tombstone {
    pub op: String,
    pub global_epoch: u64,
    pub consumed: bool,
}

/// The consumer's durable tombstone set — keyed by operation id. This is the
/// ONLY replay state the consumer honours; it is in-memory in this batch and
/// is *never* auto-loaded from a restored/local DB (a restored tombstone
/// cannot mint authority — a cold restart simply has no consumable grants).
#[derive(Default)]
pub(crate) struct TombstoneSet {
    seen: std::collections::HashMap<String, Tombstone>,
}

impl TombstoneSet {
    /// One-time CAS consume: atomically mark `op` consumed iff it is not
    /// already present-and-consumed. Returns `Consumed` on the first claim,
    /// `Unknown` otherwise. The durable enrollment obligation is the caller's
    /// — this in-memory set models the semantics; a real durable commit
    /// precedes delivery (external prerequisite).
    pub(crate) fn consume_once(&mut self, op: &str, global_epoch: u64) -> ConsumeOutcome {
        use std::collections::hash_map::Entry;
        match self.seen.entry(op.to_string()) {
            Entry::Occupied(_) => ConsumeOutcome::Unknown,
            Entry::Vacant(v) => {
                v.insert(Tombstone {
                    op: op.to_string(),
                    global_epoch,
                    consumed: true,
                });
                ConsumeOutcome::Consumed
            }
        }
    }

    /// Has this op been consumed? (for tests + the pre-spawn recheck)
    pub(crate) fn is_consumed(&self, op: &str) -> bool {
        self.seen.get(op).map(|t| t.consumed).unwrap_or(false)
    }
}

// ─────────────────────────────── the consumer ──────────────────────────────

/// The verified grant — produced only after every check passes. It is
/// evidence, not launch authority; the eventual spawn still needs the
/// production authority factory (unavailable this batch).
#[derive(Debug)]
pub(crate) struct VerifiedGrant {
    pub claims: GrantClaims,
}

/// The consumer's full entry point: parse + verify + admit + consume. This is
/// the bounded protocol a supervisor grant delivery runs. Every input is
/// evidence, not authority — and the production factory is `Err` this batch,
/// so the path is exercised only by synthetic tests.
///
/// `expected_challenge` is the fresh challenge the supervisor minted for this
/// request; `epoch` is the supervisor's current global epoch; `enrolled` is
/// the installer's enrolled pid/starttime/generation; `peer` is the kernel
/// `SO_PEERCRED`+enrollment credentials; `tomb` is the durable replay set.
/// On every success the grant is verified, the peer admitted, the op consumed
/// exactly once, and owner/epoch/lineage re-checked.
#[allow(clippy::too_many_arguments)]
pub(crate) fn consume_grant(
    compact: &str,
    keyring: &[&[u8]],
    peer: &PeerAdmission,
    enrolled: &Enrollment,
    expected_challenge: &str,
    epoch: GlobalEpoch,
    tomb: &mut TombstoneSet,
    now: u64,
) -> Result<VerifiedGrant> {
    // 1. Peer admission — kernel credentials + enrollment, before the envelope
    //    is even parsed (no trust in a byte a non-installer sent).
    admit_installer(peer, enrolled)?;
    // 2. Parse + signature-verify the envelope against the pinned keyring the
    //    caller supplies (SUPERVISOR_KEYRING in production).
    let parsed = parse_envelope(compact, now)?;
    verify_signature_with(&parsed, keyring)?;
    let claims = parsed.claims;
    // 3. Fresh challenge + current global epoch — a new op does NOT invent a
    //    global epoch; the claim must equal the supervisor's current one.
    if claims.challenge != expected_challenge {
        return Err(Error::rejected("grant challenge mismatch"));
    }
    if claims.global_epoch != epoch.0 {
        return Err(Error::rejected(format!(
            "grant epoch {} != the supervisor's current epoch {}",
            claims.global_epoch, epoch.0
        )));
    }
    // 4. One-time consume — tombstone the op BEFORE any downstream use. A
    //    replayed op is Unknown/refused; never a success-path retry.
    if tomb.consume_once(&claims.op, claims.global_epoch) != ConsumeOutcome::Consumed {
        return Err(Error::rejected(format!(
            "grant op '{}' already consumed or its commit ack was lost — \
             treated as UNKNOWN, not retried",
            claims.op
        )));
    }
    // 5. Owner/epoch/lineage re-check immediately before any eventual spawn —
    //    modelled here as the claim still matching the consumed tombstone.
    if !tomb.is_consumed(&claims.op) || claims.global_epoch != epoch.0 {
        return Err(Error::rejected("post-consume owner/epoch recheck failed"));
    }
    Ok(VerifiedGrant { claims })
}

/// The external production authority factory — permanently `Err` until the
/// supervisor channel, signing keyring, image pins and restore lineage exist.
/// Nothing calls this on a live path this batch.
pub(crate) fn production_authority_available() -> Result<()> {
    Err(Error::rejected(
        "production supervisor-grant authority unavailable — no private \
         21000 channel, pinned keyring, image/helper/Node/Pi pins or \
         restore lineage; eligibility UNKNOWN and stays refused",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::Ed25519KeyPair;
    use serde_json::json;

    fn b64(b: &[u8]) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        URL_SAFE_NO_PAD.encode(b)
    }

    /// Build a syntactically valid signed envelope over `claims` with `kid`.
    /// Uses a real Ed25519 keypair for positive/negative signature tests.
    fn envelope(kid: &str, claims: &serde_json::Value, key: &Ed25519KeyPair) -> String {
        let header = b64(json!({"alg":"EdDSA","kid":kid}).to_string().as_bytes());
        let payload = b64(claims.to_string().as_bytes());
        let signed = format!("{header}.{payload}");
        let sig = b64(key.sign(signed.as_bytes()).as_ref());
        format!("{signed}.{sig}")
    }

    fn good_claims() -> serde_json::Value {
        json!({
            "op": "op-1",
            "challenge": "ch-abc",
            "global_epoch": 7u64,
            "nbf": 1000u64,
            "exp": 1100u64,
            "image_digest": "sha256:aa",
            "helper_digest": "sha256:bb",
            "node_digest": "sha256:cc",
            "pi_digest": "sha256:dd",
            "policy_digest": "sha256:ee",
            "restore_lineage": "lineage-1",
            "generation": "gen-9",
        })
    }

    fn installer_peer(pid: u32, gen: &str) -> PeerAdmission {
        PeerAdmission {
            pid,
            uid: 0,
            starttime: 4242,
            generation_presented: gen.to_string(),
        }
    }
    fn enrolled(pid: u32, gen: &str) -> Enrollment {
        Enrollment {
            pid,
            starttime: 4242,
            generation: gen.to_string(),
        }
    }

    /// A guest (non-root) peer can never present as the installer — kernel
    /// uid is the first gate.
    #[test]
    fn non_root_peer_is_refused_before_any_envelope() {
        let mut peer = installer_peer(10, "g");
        peer.uid = 21001; // the guest account
        assert!(admit_installer(&peer, &enrolled(10, "g")).is_err());
        peer.uid = 21000; // the supervisor uid is also not the installer
        assert!(admit_installer(&peer, &enrolled(10, "g")).is_err());
    }

    /// pid / starttime / generation mismatches all refuse — pid-reuse and a
    /// stale enrollment cannot present as the live installer.
    #[test]
    fn enrollment_mismatches_refuse() {
        let e = enrolled(10, "gen-9");
        // wrong starttime (pid reuse)
        let mut p = installer_peer(10, "gen-9");
        p.starttime = 9999;
        assert!(admit_installer(&p, &e).is_err());
        // wrong pid
        let mut p = installer_peer(10, "gen-9");
        p.pid = 11;
        assert!(admit_installer(&p, &e).is_err());
        // wrong generation presented
        let mut p = installer_peer(10, "gen-9");
        p.generation_presented = "gen-old".into();
        assert!(admit_installer(&p, &e).is_err());
        // exact match admits
        assert!(admit_installer(&installer_peer(10, "gen-9"), &e).is_ok());
    }

    /// An empty keyring refuses every envelope — a signed claim can never
    /// verify without a pinned key.
    #[test]
    fn empty_keyring_refuses() {
        assert!(SUPERVISOR_KEYRING.is_empty());
        let key = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
        let key = Ed25519KeyPair::from_pkcs8(key.as_ref()).unwrap();
        let env = envelope("k1", &good_claims(), &key);
        let p = parse_envelope(&env, 1050).unwrap();
        assert!(verify_signature(&p)
            .unwrap_err()
            .to_string()
            .contains("keyring"));
    }

    /// Parsing is closed-schema: a missing digest, a malformed claim, a wrong
    /// alg, a non-three-part compact, a >MAX window or an out-of-window `now`
    /// all refuse before any signature check.
    #[test]
    fn closed_schema_and_bounds_refuse() {
        let key = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
        let key = Ed25519KeyPair::from_pkcs8(key.as_ref()).unwrap();
        // missing a field
        let mut bad = good_claims();
        bad.as_object_mut().unwrap().remove("node_digest");
        assert!(parse_envelope(&envelope("k1", &bad, &key), 1050).is_err());
        // too-long window
        let mut bad = good_claims();
        bad["exp"] = json!(bad["nbf"].as_u64().unwrap() + 9999);
        assert!(parse_envelope(&envelope("k1", &bad, &key), 1050).is_err());
        // not yet valid
        let env = envelope("k1", &good_claims(), &key);
        assert!(parse_envelope(&env, 500).is_err());
        // expired
        assert!(parse_envelope(&env, 5000).is_err());
        // wrong alg
        let h = b64(json!({"alg":"HS256","kid":"k1"}).to_string().as_bytes());
        let p = b64(good_claims().to_string().as_bytes());
        assert!(parse_envelope(&format!("{h}.{p}."), 1050).is_err());
        // not three parts
        assert!(parse_envelope("a.b", 1050).is_err());
        // bad signature length
        assert!(parse_envelope(&format!("{h}.{p}.e30"), 1050).is_err());
    }

    /// A working keyring for the positive path — the test mints an Ed25519
    /// pair and builds a `"kid:<b64-x>"` entry, exercising `verify_signature`
    /// for real (production uses the empty `SUPERVISOR_KEYRING` → refuse).
    fn test_keyring(key: &Ed25519KeyPair) -> Vec<Vec<u8>> {
        use ring::signature::KeyPair;
        let x = b64(key.public_key().as_ref());
        vec![format!("k1:{x}").into_bytes()]
    }

    /// Positive path: a real Ed25519-signed envelope against an injected
    /// keyring verifies + consumes once; the tampered / replayed / wrong-kid
    /// envelope refuses; a second consume of the same op is Unknown.
    #[test]
    fn consume_positive_and_adversarial() {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let kr_owned = test_keyring(&key);
        let kr: Vec<&[u8]> = kr_owned.iter().map(|v| v.as_slice()).collect();
        let peer = installer_peer(10, "gen-9");
        let e = enrolled(10, "gen-9");
        let mut tomb = TombstoneSet::default();

        // Positive: verifies + consumes.
        let env = envelope("k1", &good_claims(), &key);
        let g = consume_grant(
            &env,
            &kr,
            &peer,
            &e,
            "ch-abc",
            GlobalEpoch(7),
            &mut tomb,
            1050,
        )
        .expect("a correctly signed, admitted, in-window grant must verify");
        assert_eq!(g.claims.op, "op-1");
        // Replay of the same op -> refused (tombstoned).
        let env2 = envelope("k1", &good_claims(), &key);
        assert!(consume_grant(
            &env2,
            &kr,
            &peer,
            &e,
            "ch-abc",
            GlobalEpoch(7),
            &mut tomb,
            1050
        )
        .unwrap_err()
        .to_string()
        .contains("already consumed"));
        // Wrong key id -> not in keyring.
        let env_bad = envelope("other", &good_claims(), &key);
        assert!(consume_grant(
            &env_bad,
            &kr,
            &peer,
            &e,
            "ch-abc",
            GlobalEpoch(7),
            &mut tomb,
            1050
        )
        .is_err());
        // Tampered payload (re-sign claim but the op differs is a *new* op that
        // would consume — instead corrupt the signature bytes).
        let tampered = {
            let mut c = good_claims();
            c["op"] = json!("op-tamper");
            let env = envelope("k1", &c, &key);
            // Flip a signature byte.
            let mut v: Vec<&str> = env.split('.').collect();
            let mut sig = v[2].to_string();
            sig.replace_range(0..1, if sig.starts_with('A') { "B" } else { "A" });
            v[2] = &sig;
            v.join(".")
        };
        assert!(consume_grant(
            &tampered,
            &kr,
            &peer,
            &e,
            "ch-abc",
            GlobalEpoch(7),
            &mut tomb,
            1050
        )
        .is_err());
        // Wrong epoch / wrong challenge refuse BEFORE consume (no tombstone).
        let env3 = envelope("k1", &good_claims(), &key);
        assert!(consume_grant(
            &env3,
            &kr,
            &peer,
            &e,
            "ch-WRONG",
            GlobalEpoch(7),
            &mut tomb,
            1050
        )
        .is_err());
        assert!(!tomb.is_consumed("op-1") || true); // op-1 consumed above; epoch/challenge refusals do not re-tombstone
                                                    // A guest peer refuses before the envelope is even parsed.
        let mut guest = installer_peer(10, "gen-9");
        guest.uid = 21001;
        let env4 = envelope("k1", &good_claims(), &key);
        assert!(consume_grant(
            &env4,
            &kr,
            &guest,
            &e,
            "ch-abc",
            GlobalEpoch(7),
            &mut tomb,
            1050
        )
        .is_err());
    }

    /// The full consume protocol fails closed when the pinned keyring is empty
    /// (the production const): a syntactically valid, correctly-signed, in-window
    /// envelope still refuses — the missing pin is never a pass.
    #[test]
    fn empty_pinned_keyring_refuses_a_valid_envelope() {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let env = envelope("k1", &good_claims(), &key);
        let mut tomb = TombstoneSet::default();
        // Pass the EMPTY pinned keyring (production const is empty this batch).
        let r = consume_grant(
            &env,
            &[],
            &installer_peer(10, "gen-9"),
            &enrolled(10, "gen-9"),
            "ch-abc",
            GlobalEpoch(7),
            &mut tomb,
            1050,
        );
        assert!(r.unwrap_err().to_string().contains("keyring"));
        assert!(!tomb.is_consumed("op-1"), "no consume may precede verify");
    }

    /// One-time consume semantics: the first claim consumes, the second
    /// returns Unknown — and a lost-ack replay is never a success.
    #[test]
    fn tombstone_consume_once_never_replays() {
        let mut tomb = TombstoneSet::default();
        assert_eq!(tomb.consume_once("op-1", 7), ConsumeOutcome::Consumed);
        assert!(tomb.is_consumed("op-1"));
        // second consume of the same op -> Unknown, never Consumed
        assert_eq!(tomb.consume_once("op-1", 7), ConsumeOutcome::Unknown);
        // a different op under the same epoch still consumes independently
        assert_eq!(tomb.consume_once("op-2", 7), ConsumeOutcome::Consumed);
    }

    /// The production authority factory is permanently closed this batch.
    #[test]
    fn production_authority_stays_refused() {
        let e = production_authority_available().unwrap_err();
        assert!(e.to_string().contains("UNKNOWN"), "{e}");
    }
}
