//! CAD-1134 — the protected bootstrap **grant codec** (verify-only, protocol
//! evidence only).
//!
//! This module is the *signature-verification* half of a future authenticated
//! nonproduction bootstrap-admission protocol, in the `supervisor_grant` mold:
//! a domain-separated canonical challenge encoding and the finite-operation
//! signed-envelope contract. **It is a codec, not an authority.** It contains
//! NO `SO_PEERCRED` call, NO `/proc` measurement, NO external issuer process,
//! NO monotonic ledger, NO prepare/commit/abandon implementation, NO startup
//! wiring and NO reachable protected open — `OpenMode::Protected`'s
//! unconditional refusal in `store::seal` is unchanged, and every production
//! factory here is `Err`/unavailable this batch.
//!
//! What this module *does* establish, and its limits, stated precisely:
//!
//! * **The signed body's contract is fixed and injective.**
//!   [`canonical_challenge_bytes`] encodes the finite [`BootstrapOp`],
//!   a length-prefixed nonce and incarnation, the peer's recorded exe
//!   digest/pid/starttime fields, the db-image digest, the schema
//!   checkpoint and the deadline — every byte under the signature, with
//!   length-prefixing so two distinct field tuples can never collide.
//!   `verify_grant_signature` proves a signed message under a supplied
//!   keyring; it does NOT authenticate the *issuer*, the *peer process*
//!   or any ledger — those are future requirements, not implemented.
//!
//! * **The operation is inside the signature.** [`BootstrapOp`] is a
//!   signed field of the challenge, and [`VerifiedBootstrap::op`] is
//!   *derived from the verified signed data*, never assigned from a
//!   caller's label — a prepare grant cannot be relabeled into a commit
//!   (the defect this correction fixes).
//!
//! * **Trust root is compiled in, never a file.** `BOOTSTRAP_KEYRING` is
//!   a reviewed private constant bound to the protected-image build; a
//!   caller cannot supply or override it. Empty this batch → no envelope
//!   verifies, admission stays refused.
//!
//! * **Everything else is a stated future requirement, not a claim.** The
//!   peer-admission fields (`peer_*`) and the db/image/incarnation fields
//!   name what a real supervisor MUST bind — they are *payload the
//!   signature covers*, not evidence that any process was measured, any
//!   file was hashed at open, or any frontier advanced. No `VerifiedBootstrap`
//!   value asserts peer custody, ledger custody or a completed open.
//!
//! Remaining real prerequisites (unimplemented here): a provisioned
//! `cadence-supervisor` principal with a private unix listener, kernel
//! `SO_PEERCRED` + `/proc`/exe measurement of the connecting daemon, a
//! supervisor-owned monotonic frontier outside the restored image, the
//! prepare/commit/abandon lifecycle, and admission wired before
//! marker/recovery in `serve()`. None is provisioned or wired; none is
//! claimed.
//!
//! Every keyring/digest/enrollment pin is unset this batch — a missing or
//! zero pin refuses; nothing here qualifies a launch or an open.

#![allow(dead_code)]

use crate::error::{Error, Result};

// ────────────── private reviewed constants (never caller-supplied) ─────────

// The bootstrap authority principal is the future `cadence-supervisor`
// account (a dedicated uid), per the plan recorded on the ticket — it is
// named in prose, not bound to a constant here, because no listener, peer
// check or ledger in this module consumes it. Asserting it as a constant
// would be a claim a codec cannot back.

/// The reviewed, image-pinned Ed25519 verifying keys for bootstrap grants —
/// `"<kid>:<base64url-x>"` pairs, the ONLY trust root a grant signature may
/// resolve against. Compiled in, bound to the protected-image build; a caller
/// may never supply or override it. Empty this batch → no envelope verifies.
const BOOTSTRAP_KEYRING: &[&[u8]] = &[];

/// The signature domain — prepended to the signed body so a bootstrap grant
/// can never be replayed as a board-session assertion, a supervisor grant or
/// any other Ed25519 document. Distinct, fixed, never guest-controlled.
const BOOTSTRAP_DOMAIN: &str = "cadence.bootstrap-admission.v1";

/// The finite operations the authority speaks — the ONLY two values a
/// [`BootstrapChallenge`]'s signed `op` field may carry. This is a closed
/// enum, not a caller string, so an operation is a signed, typed fact —
/// a grant minted for one operation can never be relabeled into another,
/// and an unknown operation has no representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BootstrapOp {
    /// Mint a bound single-use challenge for a requested admission.
    Prepare,
    /// Consume the bound challenge and advance the frontier.
    Commit,
}

impl BootstrapOp {
    /// The canonical byte this op encodes to inside the signed body — a
    /// single fixed tag, so the operation is *inside* the signature and
    /// cannot be moved outside it.
    fn tag(self) -> u8 {
        match self {
            BootstrapOp::Prepare => 1,
            BootstrapOp::Commit => 2,
        }
    }
}

// ────────────────────── non-forgeable authority types ─────────────────────

/// The fields a signed bootstrap grant covers — the body the signature
/// proves. Private fields; produced only by the verification path, never a
/// caller literal. The `peer_*`/`db_digest`/`incarnation` fields record what
/// the supervisor certified; carrying them in a verified signature does NOT
/// prove any process was measured or any file was hashed — those are future
/// authority-path requirements, not facts this codec establishes.
#[derive(Clone, Debug)]
pub(crate) struct BootstrapChallenge {
    /// The finite operation this grant is FOR — inside the signature, so a
    /// `Prepare` grant cannot be relabeled `Commit`.
    op: BootstrapOp,
    /// The supervisor-minted single-use nonce for this start. Bounded and
    /// length-prefixed on the wire so it cannot absorb a field boundary.
    nonce: String,
    /// Peer identity fields a real supervisor would bind: the connecting
    /// daemon's recorded pid, `/proc` starttime and the sha256 of its held
    /// `/proc/<pid>/exe`. These are *payload slots in the signed body only* —
    /// no code here reads `/proc`, opens an exe fd, or verifies a peer; a
    /// populated field is a claim the signature covers, never a measurement
    /// this module performed.
    peer_pid: u32,
    peer_starttime: u64,
    peer_exe_digest: [u8; 32],
    /// The sha256 of the sealed db file the supervisor certified — the
    /// durable *image* identity. Signed payload only — NOT proof a real
    /// hash was taken at open time.
    db_digest: [u8; 32],
    /// The schema checkpoint the supervisor certified for this image.
    schema: i64,
    /// The incarnation the supervisor binds this image to. Bounded and
    /// length-prefixed on the wire.
    incarnation: String,
    /// Unix-deadline this challenge is valid until — bounded, single-use.
    deadline_unix: i64,
}

/// Largest accepted length for a variable string field — bounds the codec.
const MAX_FIELD_CHARS: usize = 256;

/// A signature-verified bootstrap grant — proof ONLY that some holder of a
/// keyring key signed this exact [`BootstrapChallenge`] under
/// `BOOTSTRAP_DOMAIN`. It does NOT prove the signer was the supervisor, that
/// any peer process was measured, that the db image was hashed at open, or
/// that a ledger/frontier advanced. Fields are private and there is no
/// caller-constructible path; the only producer is signature verification.
#[derive(Debug)]
pub(crate) struct VerifiedBootstrap {
    challenge: BootstrapChallenge,
}

impl VerifiedBootstrap {
    /// The signed operation this grant is for — read out of the verified
    /// challenge body, never a caller-supplied label.
    pub(crate) fn op(&self) -> BootstrapOp {
        self.challenge.op
    }
    pub(crate) fn challenge(&self) -> &BootstrapChallenge {
        &self.challenge
    }
}

// ──────────────────────── production factories ────────────────────────────
//
// Every production factory is `Err`/unavailable this batch: the supervisor
// uid, its private ledger dir and the launch helper are NOT provisioned on
// this host, and the worker is not authorized to provision them. Nothing on
// a live path calls these; a missing pin refuses and `OpenMode::Protected`
// stays unconditionally `Unknown`.

/// The production bootstrap keyring — permanently `Err` until a reviewed,
/// image-pinned Ed25519 trust set exists. Empty pin → no grant verifies.
pub(crate) fn production_bootstrap_keyring() -> Result<&'static [&'static [u8]]> {
    let _ = BOOTSTRAP_KEYRING;
    Err(Error::unknown(
        "bootstrap authority keyring unavailable — no reviewed image-pinned \
         Ed25519 trust set this batch; admission stays refused",
    ))
}

/// The production authority endpoint — the supervisor's private admission
/// socket + its owner-private ledger. Unavailable until uid 21000, its
/// private dir and the launch helper are provisioned; never derivable from
/// the daemon's own `state_dir`.
pub(crate) fn production_authority_endpoint() -> Result<()> {
    Err(Error::rejected(
        "bootstrap authority endpoint unavailable — no provisioned \
         cadence-supervisor listener/ledger dir this batch; startup admission \
         UNKNOWN and stays refused",
    ))
}

/// Length-prefix one variable field: a 4-byte big-endian byte count then
/// the raw bytes. Length-prefixing is what makes the encoding *injective* —
/// two distinct field tuples can never serialize to the same bytes, so a
/// boundary can never be smuggled inside a variable string the way a
/// NUL-delimited `nonce`/`incarnation` could (a `nonce` of `"a\0…"` would
/// otherwise reinterpret the following field).
fn field(v: &mut Vec<u8>, bytes: &[u8]) {
    v.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    v.extend_from_slice(bytes);
}

/// A variable string field is bounded and contains no NUL/control bytes —
/// belt-and-suspenders under the length prefix, and a hard refusal on
/// unbounded or NUL-carrying input so a challenge can never be built whose
/// encoding is ambiguous even in principle.
fn check_field(value: &str, name: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_FIELD_CHARS {
        return Err(Error::rejected(format!(
            "bootstrap challenge {name} is empty or exceeds {MAX_FIELD_CHARS} bytes"
        )));
    }
    if value.bytes().any(|b| b == 0 || b.is_ascii_control()) {
        return Err(Error::rejected(format!(
            "bootstrap challenge {name} carries a NUL/control byte"
        )));
    }
    Ok(())
}

/// The canonical wire encoding of a [`BootstrapChallenge`] — the exact bytes
/// a grant signature covers under `BOOTSTRAP_DOMAIN`. Deterministic and
/// injective: domain tag, the op tag, then each field length-prefixed or
/// fixed-width in one fixed order. Produced only inside this module so the
/// digest domain and field order are one definition.
pub(crate) fn canonical_challenge_bytes(c: &BootstrapChallenge) -> Result<Vec<u8>> {
    check_field(&c.nonce, "nonce")?;
    check_field(&c.incarnation, "incarnation")?;
    let mut v = Vec::new();
    v.extend_from_slice(BOOTSTRAP_DOMAIN.as_bytes());
    v.push(c.op.tag());
    field(&mut v, c.nonce.as_bytes());
    v.extend_from_slice(&c.peer_pid.to_be_bytes());
    v.extend_from_slice(&c.peer_starttime.to_be_bytes());
    v.extend_from_slice(&c.peer_exe_digest);
    v.extend_from_slice(&c.db_digest);
    v.extend_from_slice(&c.schema.to_be_bytes());
    field(&mut v, c.incarnation.as_bytes());
    v.extend_from_slice(&c.deadline_unix.to_be_bytes());
    Ok(v)
}

/// Verify a signed bootstrap grant against a keyring. `Err` when the keyring
/// is empty (this batch), the signature is malformed, or it does not verify
/// — never a partial or default pass. The returned [`VerifiedBootstrap`]
/// carries the challenge *as signed*; its `op` is read out of the verified
/// body, never assigned. This is signature evidence only — it proves nothing
/// about the issuer's identity, the peer process, or any ledger.
///
/// `expected_op` is a caller-side *check*, not an input to the signature: a
/// handler that needs a `Commit` compares the verified `op()` against
/// `BootstrapOp::Commit` and refuses otherwise — the signature already bound
/// the operation, so the comparison can only ever *reject*, never grant.
#[cfg(target_os = "linux")]
fn verify_grant_signature(
    challenge: &BootstrapChallenge,
    signature: &[u8],
    keyring: &[&'static [u8]],
) -> Result<VerifiedBootstrap> {
    let msg = canonical_challenge_bytes(challenge)?;
    if keyring.is_empty() {
        return Err(Error::unknown(
            "bootstrap grant keyring is empty — no signature verifies this batch",
        ));
    }
    for key in keyring {
        let peer = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, *key);
        if peer.verify(&msg, signature).is_ok() {
            return Ok(VerifiedBootstrap {
                challenge: challenge.clone(),
            });
        }
    }
    Err(Error::rejected(
        "bootstrap grant signature does not verify under the pinned keyring",
    ))
}

/// A caller-side operation check on an already-verified grant: the verified
/// `op` must equal `expected`, else refuse. Reads the signed fact; never
/// mutates it.
#[cfg(target_os = "linux")]
fn require_op(verified: &VerifiedBootstrap, expected: BootstrapOp) -> Result<()> {
    if verified.op() != expected {
        return Err(Error::rejected(format!(
            "bootstrap grant is for a different operation than {expected:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    fn challenge_for(op: BootstrapOp) -> BootstrapChallenge {
        BootstrapChallenge {
            op,
            nonce: "n1".into(),
            peer_pid: 4242,
            peer_starttime: 7_777,
            peer_exe_digest: [0xAB; 32],
            db_digest: [0xCD; 32],
            schema: 32,
            incarnation: "inc-1".into(),
            deadline_unix: 1_800_000_000,
        }
    }

    fn keyring_of(key: &Ed25519KeyPair) -> Vec<&'static [u8]> {
        // The keyring type is `&'static [u8]` because production pins are
        // compiled-in constants; the test leaks its generated public key to
        // stand in for one without changing the production contract.
        vec![Box::leak(
            key.public_key().as_ref().to_vec().into_boxed_slice(),
        )]
    }

    /// The fail-closed contract this batch: with no provisioned supervisor
    /// principal, ledger dir or launcher, every production factory and the
    /// empty keyring refuse. `OpenMode::Protected` is unchanged — admission
    /// cannot be conjured from this module. The guard is the *absence* of a
    /// reachable authority, and it must hold until the real baseline exists.
    #[test]
    fn production_factories_and_empty_keyring_refuse() {
        assert!(production_bootstrap_keyring().is_err());
        assert!(production_authority_endpoint().is_err());
        let c = challenge_for(BootstrapOp::Prepare);
        // An empty keyring refuses even a correctly-formed signature.
        let key = Ed25519KeyPair::from_seed_unchecked(&[0x11; 32]).unwrap();
        let sig = key.sign(&canonical_challenge_bytes(&c).unwrap());
        assert!(
            verify_grant_signature(&c, sig.as_ref(), &[]).is_err(),
            "an empty keyring must never admit"
        );
    }

    /// Adversarial witness for the corrected seam: the operation lives inside
    /// the signed body, so a signature over a `Prepare` challenge can never
    /// authenticate a `Commit`. This is the red the prior draft shipped
    /// green: previously `op` was a caller `&'static str` outside the
    /// signature, so the identical signature/body verified for either op.
    #[test]
    fn prepare_signature_cannot_be_relabeled_commit() {
        let key = Ed25519KeyPair::from_seed_unchecked(&[0x33; 32]).unwrap();
        let keyring = keyring_of(&key);
        // Sign a Prepare challenge.
        let prepare = challenge_for(BootstrapOp::Prepare);
        let sig = key.sign(&canonical_challenge_bytes(&prepare).unwrap());
        // It verifies as a Prepare grant.
        let v = verify_grant_signature(&prepare, sig.as_ref(), &keyring)
            .expect("the signed prepare grant verifies");
        assert_eq!(v.op(), BootstrapOp::Prepare);
        // The SAME signature/body, asked to stand for a Commit, refuses —
        // the verified op is read from the signed body, and `require_op`
        // compares it, never assigns it.
        assert!(
            require_op(&v, BootstrapOp::Commit).is_err(),
            "a prepare grant must never satisfy a commit"
        );
        // And the identical bytes cannot be re-decoded as a Commit at all:
        // there is no op field to relabel — the op is part of the signed
        // challenge, so presenting the same challenge with a different op is
        // a different object whose signature does not match.
        let relabeled = challenge_for(BootstrapOp::Commit);
        assert!(verify_grant_signature(&relabeled, sig.as_ref(), &keyring).is_err());
    }

    #[test]
    fn signed_grant_verifies_against_the_pinned_key() {
        // Protocol-level evidence (NOT an admitted open): with a non-empty
        // keyring the same canonical bytes verify, and a tampered body or
        // wrong key refuses. Caller-proof limits stay in the module doc.
        let key = Ed25519KeyPair::from_seed_unchecked(&[0x22; 32]).unwrap();
        let keyring = keyring_of(&key);
        let c = challenge_for(BootstrapOp::Commit);
        let sig = key.sign(&canonical_challenge_bytes(&c).unwrap());
        let v = verify_grant_signature(&c, sig.as_ref(), &keyring)
            .expect("a grant signed by the pinned key verifies");
        // The op is the signed one — commit, because that is what was signed.
        assert_eq!(v.op(), BootstrapOp::Commit);
        // A caller expecting Prepare is refused by the comparison.
        assert!(require_op(&v, BootstrapOp::Prepare).is_err());
        assert!(require_op(&v, BootstrapOp::Commit).is_ok());
        // Tampered body: different incarnation under the same signature.
        let mut tampered = challenge_for(BootstrapOp::Commit);
        tampered.incarnation = "inc-2".into();
        assert!(verify_grant_signature(&tampered, sig.as_ref(), &keyring).is_err());
        // A forger's key refuses.
        let forger = Ed25519KeyPair::from_seed_unchecked(&[0x99; 32]).unwrap();
        let forged = forger.sign(&canonical_challenge_bytes(&c).unwrap());
        assert!(verify_grant_signature(&c, forged.as_ref(), &keyring).is_err());
    }

    #[test]
    fn canonical_encoding_is_injective() {
        // Two challenges differing in ANY field must encode to different
        // bytes — length-prefixing means a field boundary can never be
        // absorbed into a variable string. Check each field independently.
        let base = challenge_for(BootstrapOp::Prepare);
        let base_bytes = canonical_challenge_bytes(&base).unwrap();
        // op inside the signature: a Commit differs at the op tag.
        assert_ne!(
            base_bytes,
            canonical_challenge_bytes(&challenge_for(BootstrapOp::Commit)).unwrap()
        );
        // nonce / incarnation / digests / deadline each change the bytes.
        let mut v = base.clone();
        v.nonce = "n2".into();
        assert_ne!(base_bytes, canonical_challenge_bytes(&v).unwrap());
        let mut v = base.clone();
        v.incarnation = "inc-9".into();
        assert_ne!(base_bytes, canonical_challenge_bytes(&v).unwrap());
        let mut v = base.clone();
        v.db_digest = [0xEE; 32];
        assert_ne!(base_bytes, canonical_challenge_bytes(&v).unwrap());
        let mut v = base.clone();
        v.deadline_unix += 1;
        assert_ne!(base_bytes, canonical_challenge_bytes(&v).unwrap());
    }

    #[test]
    fn variable_fields_are_bounded_and_nul_free() {
        // A variable-length field carrying a NUL or control byte, or
        // exceeding the bound, refuses at encode — the length prefix makes
        // the bytes unambiguous, and validation keeps the field honest.
        let mut bad = challenge_for(BootstrapOp::Prepare);
        bad.nonce = "a\0b".into();
        assert!(canonical_challenge_bytes(&bad).is_err());
        let mut long = challenge_for(BootstrapOp::Prepare);
        long.incarnation = "i".repeat(MAX_FIELD_CHARS + 1);
        assert!(canonical_challenge_bytes(&long).is_err());
        let mut empty = challenge_for(BootstrapOp::Prepare);
        empty.nonce = String::new();
        assert!(canonical_challenge_bytes(&empty).is_err());
    }
}
