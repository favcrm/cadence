//! Independent CAD-1085 native issuer encoder/consumer acceptance.
//!
//! Registered by the manager as a cfg(test) child of supervisor_grant. This
//! exercises real Ed25519 signing and the actual grant codec, with the native
//! profile-style `kid:base64url-public-key` ring representation. It does not
//! construct peer, root-custody, consume, launch or runtime-readiness evidence.

use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair};

fn challenge() -> SupervisorChallenge {
    let image = format!("registry.invalid/cadence-fixture@sha256:{}", "a".repeat(64));
    SupervisorChallenge {
        launch: LaunchBinding {
            epoch: 7,
            request: LaunchRequest {
                identity: ExecutorIdentity {
                    company: "workspace-installer-acceptance".to_string(),
                    instance: "123e4567-e89b-42d3-a456-426614174010".to_string(),
                    backend: "native".to_string(),
                    tier: "basic".to_string(),
                    generation: 9,
                    image_lane: Some("protected-local-1".to_string()),
                },
                purpose: "fresh_start".to_string(),
                challenge: "123e4567-e89b-42d3-a456-426614174011".to_string(),
                image: image.clone(),
            },
        },
        recipient: Recipient {
            pid: 1234,
            starttime: "987654321".to_string(),
            generation: "b".repeat(32),
            nonce: "123e4567-e89b-42d3-a456-426614174012".to_string(),
        },
        pins: Pins {
            source: "c".repeat(40),
            image,
            helper: "d".repeat(64),
            node: "e".repeat(64),
            pi_graph: "f".repeat(64),
            policy: "0".repeat(64),
        },
        lineage: Lineage {
            reference: "installer_acceptance".to_string(),
            database_epoch: 3,
        },
    }
}

fn signed_grant(
    challenge: &SupervisorChallenge,
    key: &Ed25519KeyPair,
    kid: &str,
    nbf: u64,
    exp: u64,
) -> String {
    let (header, payload, material) = grant_signing_material(challenge, nbf, exp, kid)
        .expect("the bounded native issuer must produce signing material");
    let signature = key.sign(&material);
    encode_grant_envelope(&header, &payload, signature.as_ref())
        .expect("a real Ed25519 signature must encode as a native grant")
}

#[test]
fn native_issuer_colon_kid_requires_exact_pinned_key_and_bounded_window() {
    // Synthetic deterministic keys, never installed or used as trust roots.
    let issuer =
        Ed25519KeyPair::from_seed_unchecked(&[0x31; 32]).expect("synthetic Ed25519 issuer key");
    let foreign =
        Ed25519KeyPair::from_seed_unchecked(&[0x72; 32]).expect("synthetic foreign Ed25519 key");
    let expected_challenge = challenge();
    let kid = "fixture:1";
    let nbf = 1_000;
    let exp = 1_300;
    let now = 1_100;

    // Literal boundary from the reviewed contract, not the implementation
    // constant: removing the actual encoder guard must reach this assertion.
    assert!(
        grant_signing_material(&expected_challenge, nbf, 1_301, kid).is_err(),
        "the issuer must refuse a 301-second signing window before signing"
    );

    let envelope = signed_grant(&expected_challenge, &issuer, kid, nbf, exp);
    let binding = crate::protected_runtime::registry::canonical(&serde_json::json!({
        "challenge": canonical_challenge_value(&expected_challenge),
    }))
    .expect("canonical enrollment challenge binding");
    let public_key = crate::protected_runtime::keys::b64(issuer.public_key().as_ref());
    let ring_entry = format!("{kid}:{public_key}").into_bytes();
    let keyring = [ring_entry.as_slice()];

    let verified = verify_enrolled_format(&envelope, &keyring, now, &binding)
        .expect("a colon-containing pinned issuer ID must verify its genuine grant");
    assert_eq!(
        verified.claims(),
        &GrantClaims {
            challenge: expected_challenge.clone(),
            nbf,
            exp,
        },
        "the real consumer must preserve the full exact challenge and validity claims"
    );

    let foreign_public = crate::protected_runtime::keys::b64(foreign.public_key().as_ref());
    let wrong_ring_entry = format!("{kid}:{foreign_public}").into_bytes();
    let wrong_keyring = [wrong_ring_entry.as_slice()];
    assert!(
        verify_enrolled_format(&envelope, &wrong_keyring, now, &binding).is_err(),
        "matching issuer ID must never substitute for the pinned public key"
    );

    let unknown_envelope = signed_grant(&expected_challenge, &issuer, "fixture:unknown", nbf, exp);
    assert!(
        verify_enrolled_format(&unknown_envelope, &keyring, now, &binding).is_err(),
        "a genuine signature by the same key must not authorize an unknown issuer ID"
    );
}
