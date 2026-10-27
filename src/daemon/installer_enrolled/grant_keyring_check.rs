//! Independent reviewer check: real QualifiedBootstrap grant-keyring producer
//! feeds the real supervisor_grant verifier. Authored by reviewer (not the
//! implementer); no production edit, no native authority, no physical Root.
//!
//! Fixture basis (all already committed / derivable in-test):
//!   * receipt envelope: the public RFC8032 test-1 vector in
//!     `src/daemon/fixtures/installer-enrollment-wire-v1.json` (include path
//!     below is exact for this file's declared destination).
//!   * grant envelope: signed in-test under the real `GRANT_DOMAIN`
//!     ("cadence.supervisor-launch-grant.v1") with a freshly generated key —
//!     the same construction `supervisor_grant.rs`'s own tests use.
//!   * claims TTL: grant `nbf`/`exp` are SECONDS in `parse_claims`
//!     (`now < nbf || now > exp`) and `exp - nbf <= MAX_GRANT_WINDOW_SECS`
//!     (300); we sign nbf=1/exp=301 and pass now=2s — inside the window.
//!   * binding parser contract: `verify_enrolled_format` re-parses the
//!     `challenge` member of the canonical binding JSON with
//!     `parse_supervisor_challenge` and requires structural equality with the
//!     grant's challenge; we embed the fixture's committed challenge verbatim.
//!
//! The producer path is QualifiedBootstrap::grant_public_keys -> Box::leak ->
//! &'static [u8], exactly what context::publish stores; the consumer is the
//! real verify_signature_with via verify_enrolled_format.
//!
//! Before the production serializer fix this must FAIL on the producer shape
//! assertions (a raw 32-byte key is not a "kid:b64url" text entry). After the
//! fix it must pass, proving producer->consumer end to end.

use crate::daemon::supervisor_grant as grant;
use crate::installer_bundle::constructor::grant_keyring_probe::produced_grant_keyring_entries;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::json;

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../fixtures/installer-enrollment-wire-v1.json"
    ))
    .unwrap()
}

fn canonical_challenge() -> serde_json::Value {
    fixture()["vectors"][0]["payload"]["binding"]["challenge"].clone()
}

fn canonical_binding_json() -> Vec<u8> {
    // Exact committed binding bytes from the vector payload — the same bytes
    // verify_receipt_format would emit as binding_json. Re-serializing the
    // parsed value is sufficient here because verify_enrolled_format only
    // re-parses `challenge` out of it (structural, not byte, equality).
    serde_json::to_vec(&fixture()["vectors"][0]["payload"]["binding"]).unwrap()
}

fn signed_grant(kid: &str, challenge: serde_json::Value, key: &Ed25519KeyPair) -> String {
    let h = URL_SAFE_NO_PAD.encode(json!({"alg":"EdDSA","kid":kid}).to_string().as_bytes());
    let p = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({"challenge":challenge,"nbf":1u64,"exp":301u64})).unwrap(),
    );
    let msg = format!("cadence.supervisor-launch-grant.v1\0{h}.{p}");
    let s = URL_SAFE_NO_PAD.encode(key.sign(msg.as_bytes()).as_ref());
    format!("{h}.{p}.{s}")
}

fn fresh_key() -> Ed25519KeyPair {
    let rng = ring::rand::SystemRandom::new();
    Ed25519KeyPair::from_pkcs8(Ed25519KeyPair::generate_pkcs8(&rng).unwrap().as_ref()).unwrap()
}

/// The real producer output must feed the real verifier — the end-to-end
/// keyring contract. A format mismatch between what context::publish stores
/// and what verify_signature_with parses shows here.
#[test]
fn produced_grant_keyring_verifies_signed_grant_envelope() {
    let key = fresh_key();
    // Real producer: QualifiedBootstrap::grant_public_keys via the probe,
    // whose synthetic manifest's grantTrust pins THIS key under kid grant-k1.
    let produced = produced_grant_keyring_entries(key.public_key().as_ref().try_into().unwrap());
    assert_eq!(produced.len(), 1, "one manifest grantTrust entry produced");
    let entry = produced[0];

    // Whatever the producer emits, the consumer requires UTF-8 "kid:b64url"
    // with exactly one ':' separator, or verify_signature_with skips it.
    let text = std::str::from_utf8(entry)
        .expect("producer emitted non-UTF8 keyring entry — consumer cannot select it");
    let (kid, b64key) = text
        .split_once(':')
        .expect("producer entry lacks 'kid:' prefix — consumer skips it");
    assert_eq!(kid, "grant-k1", "producer must preserve manifest kid");
    let decoded = URL_SAFE_NO_PAD
        .decode(b64key)
        .expect("producer publicKey is not base64url");
    assert_eq!(decoded.len(), 32);
    assert_eq!(
        decoded.as_slice(),
        key.public_key().as_ref(),
        "producer must preserve the exact manifest public key bytes"
    );

    // Real consumer: verify_enrolled_format -> verify_signature_with.
    let binding_json = canonical_binding_json();
    let envelope = signed_grant("grant-k1", canonical_challenge(), &key);
    let keyring: Vec<&[u8]> = vec![entry];
    grant::verify_enrolled_format(&envelope, &keyring, 2, &binding_json)
        .expect("produced keyring must verify the genuine grant signature");

    // Unknown kid refuses — no silent key selection.
    let wrong_kid = signed_grant("unknown-kid", canonical_challenge(), &key);
    assert!(grant::verify_enrolled_format(&wrong_kid, &keyring, 2, &binding_json).is_err());
}

/// Real signature forgery: a DIFFERENT signer signs the SAME canonical claims
/// under the SAME known kid. Schema, kid lookup and binding all pass; only
/// cryptographic verification can refuse — a genuine negative control on
/// verify_signature_with, not merely a binding mismatch.
#[test]
fn foreign_signature_under_known_kid_refuses() {
    let key = fresh_key();
    let produced = produced_grant_keyring_entries(key.public_key().as_ref().try_into().unwrap());
    let keyring: Vec<&[u8]> = vec![produced[0]];
    let binding_json = canonical_binding_json();
    let attacker = fresh_key();
    let forged = signed_grant("grant-k1", canonical_challenge(), &attacker);
    assert!(
        grant::verify_enrolled_format(&forged, &keyring, 2, &binding_json).is_err(),
        "a signature from an unpinned signer under a known kid must refuse"
    );
}

/// Binding-mismatch control (distinct from signature forgery): the TRUSTED
/// key signs structurally different claims. This proves the challenge-equality
/// arm of verify_enrolled_format, separately from cryptographic refusal.
#[test]
fn trusted_key_over_altered_claims_refuses_binding_mismatch() {
    let key = fresh_key();
    let produced = produced_grant_keyring_entries(key.public_key().as_ref().try_into().unwrap());
    let keyring: Vec<&[u8]> = vec![produced[0]];
    let binding_json = canonical_binding_json();
    let mut bad_claims = canonical_challenge();
    bad_claims["launch"]["epoch"] = json!(999u64);
    let mismatched = signed_grant("grant-k1", bad_claims, &key);
    assert!(grant::verify_enrolled_format(&mismatched, &keyring, 2, &binding_json).is_err());
}

/// Producer output must not be silently reinterpreted: a bare 32-byte public
/// key is not a consumable keyring entry (no kid to select by).
#[test]
fn producer_output_is_not_a_raw_key() {
    let key = fresh_key();
    let produced = produced_grant_keyring_entries(key.public_key().as_ref().try_into().unwrap());
    assert_ne!(
        produced[0].len(),
        32,
        "a bare 32-byte public key is not a consumable keyring entry"
    );
}
