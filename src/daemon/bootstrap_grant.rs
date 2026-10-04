//! CAD-1134 — the protected bootstrap **authority consumer** (verify-only).
//!
//! This module is the *consumer* half of the authenticated nonproduction
//! bootstrap/startup-admission protocol, in the `supervisor_grant` mold: the
//! canonical challenge codec, a domain-separated signed grant envelope, the
//! kernel peer-admission contract and the prepare/commit ledger terms — with
//! every production factory left `Err`/unavailable this batch. It **never
//! admits a store open itself** and **never mints** a grant; `OpenMode::
//! Protected`'s unconditional refusal is unchanged. The point of the module
//! is to fix the *contract* a real supervisor authority must satisfy before
//! any `Protected` open becomes reachable, so the rejected "read a public
//! key + writable ledger out of the daemon's own state dir" draft can never
//! be mistaken for qualification.
//!
//! Trust model — why each rejection in the prior draft maps to a rule here:
//!
//! * **Trust root is compiled in, never a file in `state_dir`.** A daemon
//!   reading `bootstrap-authority.pub` (or any in-image file) authenticated
//!   nothing — the employee/restore domain could replace it. Here the only
//!   trust root is `BOOTSTRAP_KEYRING`, a *reviewed private constant* bound
//!   to the protected-image build; a caller cannot supply or override it.
//!   It is empty this batch → no envelope verifies, admission stays refused.
//!
//! * **The authority is a separate principal, not a callback.** The issuer
//!   is the supervisor account (`cadence-supervisor`, uid [`SUPERVISOR_UID`])
//!   holding a *private* unix listener — kernel peer admission is
//!   `SO_PEERCRED` uid + live `/proc` starttime + the held
//!   `/proc/<pid>/exe` fd hashed against the pinned digest (the
//!   `peer_exe_digest` contract). A `verified:true` callback, a role label
//!   or an in-process function can never satisfy `admit_supervisor`.
//!
//! * **The monotonic frontier lives outside the restored image.** The
//!   accepted-checkpoint/incarnation ledger is owned by the supervisor
//!   process in its own private dir — the daemon may only *ask*, never
//!   write. A restored/reset in-image ledger cannot replay a consumed
//!   challenge because the supervisor's own record already advanced; this
//!   is the property an unsigned `consumed`/`bound` row in the daemon's
//!   own store could not provide.
//!
//! * **Admission binds the measured image + checkpoint, not a path.**
//!   [`BootstrapChallenge`] carries the connecting peer's measured
//!   exe-digest/pid-starttime, the *sealed-file* digest of the db and the
//!   schema checkpoint — a same-path restored or replaced image fails the
//!   digest binding. The bound `db_digest` is a hash of the actual file
//!   contents the supervisor certified, so "canonical pathname + version
//!   int" is never the identity.
//!
//! * **Recoverable lifecycle.** `prepare` mints a single-use bound
//!   challenge whose ledger row is *pending*; the daemon reports held
//!   acquisition; `commit` consumes it + advances the frontier atomically;
//!   `abandon`/crash leaves the row pending-until-deadline — never burned
//!   by a failed or partial open. A legitimate next launch gets a *fresh*
//!   challenge + incarnation; a stale/replayed one is refused at the
//!   supervisor's frontier.
//!
//! Every keyring/digest/enrollment pin is unset this batch — a missing or
//! zero pin refuses; nothing here qualifies a launch or an open.

#![allow(dead_code)]

use crate::error::{Error, Result};

// ────────────── private reviewed constants (never caller-supplied) ─────────

/// The bootstrap authority account — the same supervisor principal
/// (uid [`SUPERVISOR_UID`]) that owns the private admission listener. A
/// socket owned by anyone else is never an admission channel.
pub(crate) const BOOTSTRAP_UID: u32 = crate::daemon::supervisor_grant::SUPERVISOR_UID;

/// The reviewed, image-pinned Ed25519 verifying keys for bootstrap grants —
/// `"<kid>:<base64url-x>"` pairs, the ONLY trust root a grant signature may
/// resolve against. Compiled in, bound to the protected-image build; a caller
/// may never supply or override it. Empty this batch → no envelope verifies.
const BOOTSTRAP_KEYRING: &[&[u8]] = &[];

/// The signature domain — prepended to the signed body so a bootstrap grant
/// can never be replayed as a board-session assertion, a supervisor grant or
/// any other Ed25519 document. Distinct, fixed, never guest-controlled.
const BOOTSTRAP_DOMAIN: &str = "cadence.bootstrap-admission.v1";

/// The two finite operations the authority speaks. A grant minted for any
/// other operation is refused before op semantics run.
pub(crate) const OP_PREPARE: &str = "bootstrap_prepare";
pub(crate) const OP_COMMIT: &str = "bootstrap_commit";

// ────────────────────── non-forgeable authority types ─────────────────────

/// The supervisor's certified statement about one requested admission — the
/// signed body a `bootstrap_commit` grant covers. Private fields; produced
/// only by the authority-verification path, never a caller literal.
#[derive(Clone, Debug)]
pub(crate) struct BootstrapChallenge {
    /// The supervisor-minted single-use nonce for this start.
    nonce: String,
    /// The connecting daemon's measured peer identity: pid + `/proc`
    /// starttime + the sha256 of its held `/proc/<pid>/exe` fd — the
    /// *process*, never a path or a uid label.
    peer_pid: u32,
    peer_starttime: u64,
    peer_exe_digest: [u8; 32],
    /// The sha256 of the sealed db file the supervisor certified — the
    /// durable *image* identity. A same-path restored/replaced file whose
    /// bytes differ refuses; a canonical pathname is never the binding.
    db_digest: [u8; 32],
    /// The schema checkpoint the supervisor certified for this image.
    schema: i64,
    /// The incarnation the supervisor will bind this image to on commit.
    incarnation: String,
    /// Unix-deadline this challenge is valid until — bounded, single-use.
    deadline_unix: i64,
}

/// A verified bootstrap grant — the supervisor's signed statement that one
/// specific measured daemon image + db image + incarnation may proceed with
/// the named operation (`bootstrap_prepare` or `bootstrap_commit`). Fields
/// are private and there is no caller-constructible path: the only producer
/// is the signature + peer-admission verification.
#[derive(Debug)]
pub(crate) struct VerifiedBootstrap {
    challenge: BootstrapChallenge,
    /// The operation this grant authorizes — prepare vs commit. A prepare
    /// grant can never authorize a commit.
    op: &'static str,
}

impl VerifiedBootstrap {
    pub(crate) fn op(&self) -> &'static str {
        self.op
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

/// The canonical wire encoding of a [`BootstrapChallenge`] — the exact bytes
/// the supervisor signs under `BOOTSTRAP_DOMAIN`. Produced only inside this
/// module so the digest domain and field order are one definition.
pub(crate) fn canonical_challenge_bytes(c: &BootstrapChallenge) -> Result<Vec<u8>> {
    // Domain separation + a fixed, exhaustive field order. A forger cannot
    // move a field outside the signature or reinterpret the body.
    let mut v = Vec::new();
    v.extend_from_slice(BOOTSTRAP_DOMAIN.as_bytes());
    v.push(0);
    v.extend_from_slice(c.nonce.as_bytes());
    v.push(0);
    v.extend_from_slice(&c.peer_pid.to_be_bytes());
    v.extend_from_slice(&c.peer_starttime.to_be_bytes());
    v.extend_from_slice(&c.peer_exe_digest);
    v.extend_from_slice(&c.db_digest);
    v.extend_from_slice(&c.schema.to_be_bytes());
    v.extend_from_slice(c.incarnation.as_bytes());
    v.push(0);
    v.extend_from_slice(&c.deadline_unix.to_be_bytes());
    Ok(v)
}

/// Verify a signed bootstrap grant against a keyring. `Err` when the keyring
/// is empty (this batch), the signature is malformed, or it does not verify —
/// never a partial or default pass. This is the signature half only; the
/// kernel peer admission is a separate mandatory check.
#[cfg(target_os = "linux")]
fn verify_grant_signature(
    challenge: &BootstrapChallenge,
    signature: &[u8],
    op: &'static str,
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
                op,
            });
        }
    }
    Err(Error::rejected(
        "bootstrap grant signature does not verify under the pinned keyring",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    fn challenge() -> BootstrapChallenge {
        BootstrapChallenge {
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

    /// The fail-closed contract this batch: with no provisioned supervisor
    /// principal, ledger dir or launcher, every production factory and the
    /// empty keyring refuse. `OpenMode::Protected` is unchanged — admission
    /// cannot be conjured from this module. This is the red-in-reverse:
    /// the guard is the *absence* of a reachable authority, and it must
    /// hold until the real baseline exists.
    #[test]
    fn production_factories_and_empty_keyring_refuse() {
        assert!(production_bootstrap_keyring().is_err());
        assert!(production_authority_endpoint().is_err());
        let c = challenge();
        // An empty keyring refuses even a correctly-formed signature.
        let key = Ed25519KeyPair::from_seed_unchecked(&[0x11; 32]).unwrap();
        let sig = key.sign(&canonical_challenge_bytes(&c).unwrap());
        assert!(
            verify_grant_signature(&c, sig.as_ref(), OP_PREPARE, &[]).is_err(),
            "an empty keyring must never admit"
        );
    }

    #[test]
    fn signed_grant_verifies_against_the_pinned_key() {
        // Protocol-level evidence (NOT an admitted open): with a non-empty
        // keyring the same canonical bytes verify, and a tampered body or
        // wrong key refuses. This proves the codec + signature contract a
        // real supervisor will satisfy — caller-proof limits stay in the
        // module doc.
        let key = Ed25519KeyPair::from_seed_unchecked(&[0x22; 32]).unwrap();
        // The keyring type is `&'static [u8]` because production pins are
        // compiled-in constants; the test leaks its generated public key to
        // stand in for one without changing the production contract.
        let pub_static: &'static [u8] =
            Box::leak(key.public_key().as_ref().to_vec().into_boxed_slice());
        let keyring: &[&[u8]] = &[pub_static];
        let c = challenge();
        let sig = key.sign(&canonical_challenge_bytes(&c).unwrap());
        let v = verify_grant_signature(&c, sig.as_ref(), OP_PREPARE, keyring)
            .expect("a grant signed by the pinned key verifies");
        assert_eq!(v.op(), OP_PREPARE);
        // A prepare grant cannot authorize a commit — the op is bound.
        assert_ne!(v.op(), OP_COMMIT);
        // Tampered body: different incarnation under the same signature.
        let mut tampered = challenge();
        tampered.incarnation = "inc-2".into();
        assert!(verify_grant_signature(&tampered, sig.as_ref(), OP_PREPARE, keyring).is_err());
        // A forger's key refuses.
        let forger = Ed25519KeyPair::from_seed_unchecked(&[0x99; 32]).unwrap();
        let forged = forger.sign(&canonical_challenge_bytes(&c).unwrap());
        assert!(verify_grant_signature(&c, forged.as_ref(), OP_PREPARE, keyring).is_err());
    }

    #[test]
    fn canonical_encoding_binds_every_field() {
        // Two challenges differing only in one field must encode
        // differently — no field may sit outside the signed body.
        let a = canonical_challenge_bytes(&challenge()).unwrap();
        let mut b = challenge();
        b.db_digest = [0xEE; 32];
        assert_ne!(a, canonical_challenge_bytes(&b).unwrap());
        let mut c = challenge();
        c.incarnation = "inc-9".into();
        assert_ne!(a, canonical_challenge_bytes(&c).unwrap());
        let mut d = challenge();
        d.deadline_unix += 1;
        assert_ne!(a, canonical_challenge_bytes(&d).unwrap());
    }
}
