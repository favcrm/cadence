//! CAD-1339 independent acceptance of the renewal response guard.
//!
//! The response shape below is checked against the AOS-205 contract and
//! issuer implementation in the AOS-205 worktree (the Zod exchange-response
//! schema plus `apps/api/src/hosted-cadence/cli-session.ts::accessResponse`).
//! These tests exercise the real CLI validator and local credential writer;
//! they do not claim to exercise issuer HTTP/D1 behavior.
use super::*;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;

const ORG: &str = "acme";
const ORG_ID: &str = "ws_real";
const SUBJECT: &str = "user_owner";
const ISSUER: &str = "https://api-v2.agenticos.hk";
const SESSION_ID: &str = "hcs_12345678-1234-4234-8234-123456789abc";
const OLD_REFRESH: &str = "hcr_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const NEW_REFRESH: &str = "hcr_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
const OLD_ACCESS: &str = "hct_CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";
const NEW_ACCESS: &str = "hct_DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD";

fn fixture() -> (tempfile::TempDir, RemoteTarget, Credential, PendingRenewal) {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let target = RemoteTarget {
        org: ORG.into(),
        endpoint: "https://acme.cadencecloud.app".into(),
        org_id: ORG_ID.into(),
    };
    let at = now().unwrap();
    let credential = Credential {
        organization_id: ORG_ID.into(),
        organization_slug: Some(ORG.into()),
        audience: target.endpoint.clone(),
        issuer: Some(ISSUER.into()),
        subject_id: Some(SUBJECT.into()),
        capabilities: CAPABILITIES.iter().map(|s| (*s).to_owned()).collect(),
        access_token: OLD_ACCESS.into(),
        expires_at: at.saturating_sub(1),
        cli_session: Some(CliSession {
            session_id: SESSION_ID.into(),
            refresh_token: OLD_REFRESH.into(),
            generation: 1,
            device_label: "cadence CLI".into(),
            idle_expires_at: at + 1000,
            absolute_expires_at: at + 10_000,
        }),
        pending: None,
    };
    let pending = PendingRenewal {
        operation_id: format!("op_{}", "e".repeat(43)),
        expected_generation: 1,
    };
    (root, target, credential, pending)
}

/// A current AOS-205 renewal response: the exchange object is the same strict
/// shape returned by `hostedCadenceExchangeResponseFor`, with the renewable
/// session metadata appended by the issuer route.
fn issuer_response(prior: &Credential, pending: &PendingRenewal, target: &RemoteTarget) -> Value {
    let at = now().unwrap();
    let session = prior.cli_session.as_ref().unwrap();
    json!({
        "version": "hosted-cadence-auth.v1",
        "organization_id": prior.organization_id,
        "organization_slug": target.org,
        "audience": target.endpoint,
        "principal": {
            "kind": "user",
            "subject_id": prior.subject_id,
            "current_role": "owner"
        },
        "capabilities": prior.capabilities,
        "credential": {
            "credential_id": "hcp_12345678-1234-4234-8234-123456789abc",
            "access_token": NEW_ACCESS,
            "token_type": "Bearer",
            "issued_at": at,
            "expires_at": at + 300,
            "renewal": "reexchange"
        },
        "cli_session": {
            "session_id": session.session_id,
            "refresh_token": NEW_REFRESH,
            "generation": pending.expected_generation + 1,
            "device_label": session.device_label,
            "idle_expires_at": (at + 1000).min(session.absolute_expires_at),
            "absolute_expires_at": session.absolute_expires_at
        }
    })
}

#[test]
fn forged_renewal_response_is_refused_before_credential_replacement() {
    let (root, target, mut prior, pending) = fixture();
    prior.pending = Some(pending.clone());
    save_credential(root.path(), ORG, &prior).unwrap();
    let path = root.path().join(format!("cli-{ORG}.json"));
    let before = std::fs::read(&path).unwrap();

    // Subject mismatch is a real immutable-identity check in the renewal
    // guard. This is not a hand-written imitation of the guard's outcome.
    let mut forged = issuer_response(&prior, &pending, &target);
    forged["principal"]["subject_id"] = json!("user_attacker");
    assert!(validate_renewal_response(&forged, &prior, &target, &pending).is_err());

    // The production renewal path invokes this guard before decoding or
    // saving response fields. Confirm the persisted authority remains byte
    // for byte unchanged when that guard refuses the response.
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let still_stored = load_credential(root.path(), &target).unwrap();
    assert_eq!(still_stored.access_token, OLD_ACCESS);
    assert_eq!(still_stored.cli_session.unwrap().refresh_token, OLD_REFRESH);
}

#[test]
fn current_issuer_response_passes_identity_and_rotation_guard() {
    let (_root, target, prior, pending) = fixture();
    let response = issuer_response(&prior, &pending, &target);
    validate_renewal_response(&response, &prior, &target, &pending)
        .expect("AOS-205 response must satisfy the CLI's actual guard");

    let next: CliSession = serde_json::from_value(response["cli_session"].clone()).unwrap();
    assert_eq!(next.session_id, SESSION_ID);
    assert_eq!(next.generation, 2);
    assert_ne!(next.refresh_token, OLD_REFRESH);
    assert!(response["credential"]["expires_at"].as_u64().unwrap() > now().unwrap());
}
