//! Independent test-only producer probe for the grant keyring.
//! Authored by the reviewer, NOT the implementer. cfg(test) only.
//! A QualifiedBootstrap is built from a synthetic PublicTrust manifest via
//! serde (test fixture material, never native authority). The probe reads the
//! manifest trust list the way context::publish does, then exercises the REAL
//! producer chain QualifiedBootstrap::grant_public_keys -> Box::leak ->
//! &'static [u8] exactly as context::publish stores it.
//! No production format fix is authored here; this file observes whether the
//! real producer output can be consumed by the real verifier.
//!
//! The public key is supplied by the caller (the check's generated test key);
//! the probe no longer bakes in a constant that could never match.
//!
//! `use super::*` is load-bearing here: `QualifiedBootstrap`, `Manifest` and
//! `base64::Engine` all resolve through the parent module's imports/scope.
//! The file is written for byte-exact copy into `constructor/` — do NOT
//! strip the wildcard import when relocating.

use super::*;

fn synthetic_public_trust(kid: &str, public_key: [u8; 32]) -> serde_json::Value {
    serde_json::json!({
        "issuer": "agenticos-native-owner",
        "kid": kid,
        "keyVersion": 1,
        "publicKey": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(public_key),
    })
}

fn synthetic_manifest(public_key: [u8; 32]) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "image": "registry.local/img@sha256:".to_string() + &"a".repeat(64),
        "source": "b".repeat(40),
        "artifacts": {
            "constructor": "c".repeat(64),
            "client": "d".repeat(64),
            "carrier": "e".repeat(64),
            "observer": "f".repeat(64),
            "supervisor": "0".repeat(63) + "1",
            "helper": "1".repeat(64),
            "node": "2".repeat(64),
            "piGraph": "3".repeat(64),
            "policy": "4".repeat(64),
        },
        "notBeforeMs": 1u64,
        "expiresAtMs": 9_007_199_254_740_991u64,
        "receiptTrust": [synthetic_public_trust("receipt-k1", public_key)],
        "grantTrust": [synthetic_public_trust("grant-k1", public_key)],
    })
}

fn synthetic_launch() -> crate::daemon::supervisor_grant::LaunchBinding {
    crate::daemon::supervisor_grant::LaunchBinding {
        request: crate::daemon::supervisor_grant::LaunchRequest {
            identity: crate::daemon::supervisor_grant::ExecutorIdentity {
                company: "acme".into(),
                instance: "prod-1".into(),
                backend: "native".into(),
                tier: "basic".into(),
                generation: 1,
                image_lane: None,
            },
            purpose: "fresh_start".into(),
            challenge: "11111111-1111-4111-8111-111111111111".into(),
            image: "registry.local/img@sha256:".to_string() + &"a".repeat(64),
        },
        epoch: 1,
    }
}

fn synthetic_lineage() -> crate::daemon::supervisor_grant::Lineage {
    crate::daemon::supervisor_grant::Lineage {
        reference: "lin-1".into(),
        database_epoch: 1,
    }
}

/// Build a synthetic QualifiedBootstrap from fixture JSON. This is test
/// material only: the manifest bytes were not authenticated against
/// IMAGE_AUTHORITY_KEYS, so the object carries no production authority.
fn synthetic_bootstrap(public_key: [u8; 32]) -> QualifiedBootstrap {
    let manifest: Manifest =
        serde_json::from_value(synthetic_manifest(public_key)).expect("synthetic manifest");
    QualifiedBootstrap {
        manifest,
        image_attestation: String::new(),
        launch: synthetic_launch(),
        lineage: synthetic_lineage(),
        operation: "11111111-1111-4111-8111-111111111111".into(),
        barrier_nonce: "22222222-2222-4222-8222-222222222222".into(),
        expires_at_ms: 9_007_199_254_740_991,
        authenticated_at_ms: 1,
    }
}

/// The exact conversion context::publish performs before storing grant_keys.
/// Producer output -> Box::leak -> &'static [u8]. The caller passes the test
/// key's real public key so the manifest trust list pins the same key the
/// check signs with.
pub(crate) fn produced_grant_keyring_entries(public_key: [u8; 32]) -> Vec<&'static [u8]> {
    let bootstrap = synthetic_bootstrap(public_key);
    bootstrap
        .grant_public_keys()
        .expect("synthetic producer")
        .into_iter()
        .map(|key| &*Box::leak(Box::new(key)) as &'static [u8])
        .collect()
}
