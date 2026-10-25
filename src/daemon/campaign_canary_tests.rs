//! CAD-1128 campaign unsupported-consumer admission canaries — REV 4,
//! executable against the real `Rig` daemon + real TLS SMTP peer.
//! Owned by cad1128-confidentiality-swe. `Result`-propagating observations;
//! legacy positive control first; real `smtp_barrier` (accept-owner join)
//! instead of sleeps.
//!
//! REV 4 corrections: legacy control now requires BOTH `accepted_messages>0`
//! AND `auth_a>0` (a real DATA under the real secret, not a bare session or an
//! auth-only pass) AND `auth_other==0` (no wrong credential authed). The spawn
//! gate is asserted on its synchronous observable — the queued delivery never
//! claims and a real `smtp_barrier` shows zero sessions/accepted/auth — not a
//! registry field (private) and not a 400ms sleep (not an acceptance barrier).
//! `crm_send_run`/`crm_send_row_step` stay private `fn`; the preclaim/pretoken
//! cases mount inside `crm_send_rpc::tests` — see integration.md.
#![cfg(all(unix, feature = "test-seam"))]

use crate::daemon::cad1128_canary_fx as fx;
use crate::daemon::custody_acceptance_fixture::Rig;
use serde_json::json;
use tempfile::TempDir;

/// The rig's two real SMTP auth secrets — the only creds the TLS peer accepts.
const A: &str = "cad1128-custody-a";
const B: &str = "cad1128-custody-b";

fn encrypted_rig(dir: &TempDir) -> Rig {
    let rig =
        Rig::start(dir.path(), A.as_bytes(), B.as_bytes()).expect("encrypted rig start failed");
    fx::unlock_vault(&rig);
    rig
}
fn legacy_rig(dir: &TempDir) -> Rig {
    Rig::start_legacy(dir.path(), A.as_bytes(), B.as_bytes()).expect("legacy rig start failed")
}

// ============================================================================
// LEGACY POSITIVE CONTROL — real DATA under real auth, no wrong-credential
// ============================================================================

/// Legacy daemon: enroll SMTP with real secret `A`, bind, prepare+approve.
/// The worker claims the delivery, mints a token, and the real TLS peer
/// accepts a DATA body authenticated under `auth_a` — never `auth_other`.
/// Baseline for every encrypted zero-residue assertion.
#[test]
fn control_legacy_campaign_delivers_under_auth_a() {
    let dir = fx::new_root();
    let rig = legacy_rig(&dir);
    let conn = fx::enroll_smtp(&rig, "legacy-ctl", A);
    assert!(!fx::is_encrypted(&rig, &conn).unwrap());
    let (context, customer, _email) = fx::context_with_customer(&rig, &rig.install_id, "lctl");
    fx::content_and_audience(&rig, &rig.install_id, &context, "camp-l", "frz-l");
    // Configure the unsubscribe origin BEFORE prepare — the origin
    // precondition fails first otherwise (no configured origin).
    fx::send_origin(&rig);
    fx::smtp_bind(&rig, &rig.install_id, &context, &conn).expect("legacy bind refused");
    // The real guard needs an SMTP-accepted test send on this exact
    // binding+content BEFORE prepare (crm_send_facts requires it). Send to
    // the consenting customer; the real TLS peer auths under secret A.
    let customer_email = "lctl@example.test".to_owned();
    let sent = fx::op(
        &rig,
        "crm_smtp_test_send",
        json!({"install_id": rig.install_id, "context_id": context,
            "campaign_id": "camp-l", "to_email": customer_email}),
    );
    assert_eq!(
        sent["receipt"]["accepted"], true,
        "test send not accepted: {sent}"
    );
    // Snapshot AFTER the test-send so the campaign's own DATA/auth is
    // asserted as additional traffic, not the test-send's positive masking a
    // failed campaign send.
    let baseline = rig.smtp_barrier().unwrap();
    let (before_auth_a, before_accepted) = (baseline.auth_a, baseline.accepted_messages);
    let send_id = fx::prepare_and_approve(&rig, &rig.install_id, &context, "camp-l", "frz-l")
        .expect("legacy prepare/approve refused");
    let state =
        fx::wait_delivery_state(&rig, &rig.install_id, &context, &send_id, &customer, 30).unwrap();
    assert_ne!(state.as_deref(), Some("queued"), "legacy row stayed queued");
    // Real accept-owner barrier joins the listener's admitted workers.
    let counts = rig.smtp_barrier().unwrap();
    assert!(
        counts.accepted_messages > before_accepted,
        "campaign produced no additional accepted DATA beyond the test send"
    );
    assert!(
        counts.auth_a > before_auth_a,
        "campaign did not auth under secret A beyond the test send"
    );
    assert_eq!(counts.auth_other, 0, "a wrong credential authenticated");
    rig.finish().unwrap();
}

// ============================================================================
// PUBLIC-VERB EARLY REFUSAL — prepare/approve via crm_send_facts
// ============================================================================

/// A fresh encrypted SMTP link: `crm_send_prepare` refuses
/// `unsupported_custody_use` BEFORE any `sending` row — `crm_send_facts` →
/// `refuse_encrypted_connection(link.connection_id)`.
#[test]
fn prepare_encrypted_link_refuses_before_send_row() {
    let dir = fx::new_root();
    let rig = encrypted_rig(&dir);
    let conn = fx::enroll_smtp(&rig, "enc-prep", A);
    assert!(fx::is_encrypted(&rig, &conn).unwrap());
    let (context, _c, _e) = fx::context_with_customer(&rig, &rig.install_id, "ep");
    fx::content_and_audience(&rig, &rig.install_id, &context, "camp-ep", "frz-ep");
    // Configure the unsubscribe origin BEFORE prepare so the encrypted
    // classification — not the absent origin — is the first refusal hit.
    fx::send_origin(&rig);
    fx::smtp_bind(&rig, &rig.install_id, &context, &conn).expect("bind refused");
    let code = fx::refused(
        &rig,
        "crm_send_prepare",
        json!({"install_id": rig.install_id, "context_id": context, "campaign_id": "camp-ep",
            "audience_freeze_id": "frz-ep", "request_id": "prep-ep"}),
    );
    assert_eq!(
        code.as_deref(),
        Some(fx::UNSUPPORTED),
        "prepare on an encrypted link did not refuse with the admission code: {code:?}"
    );
    rig.finish().unwrap();
}

// ============================================================================
// SPAWN GATE — synchronous refusal, observable on durable/socket state
// ============================================================================

/// `spawn_crm_send_worker` (`pub(super)`) refuses an encrypted-target send
/// before registering a runner — synchronous, so the assertions run right
/// after the call: the seeded `sending` delivery is never claimed (stays
/// `queued`), zero unsubscribe tokens/index rows, and a real `smtp_barrier`
/// (which drains + joins the accept-owner's workers) shows zero sessions,
/// zero auth, zero accepted. No sleep, no private-registry read.
#[test]
fn worker_spawn_gate_refuses_encrypted_send_zero_residue() {
    let dir = fx::new_root();
    let rig = encrypted_rig(&dir);
    let conn = fx::enroll_smtp(&rig, "enc-spawn", A);
    assert!(fx::is_encrypted(&rig, &conn).unwrap());
    let (context, customer, email) = fx::context_with_customer(&rig, &rig.install_id, "sw");
    let send_id = fx::seed_stale_encrypted_send(
        &rig,
        &rig.install_id,
        &context,
        "camp-sw",
        "frz-sw",
        &conn,
        &customer,
        &email,
    )
    .expect("seed failed");
    let shared = fx::shared(&rig);
    // Synchronous admission refusal — returns without registering a runner.
    shared.spawn_crm_send_worker(&rig.install_id, &context, &send_id);
    // Assert zero residue on the real durable rows + the live accept barrier.
    let state = fx::delivery_state(&rig, &rig.install_id, &context, &send_id, &customer).unwrap();
    assert_eq!(
        state.as_deref(),
        Some("queued"),
        "encrypted send was claimed: {state:?}"
    );
    assert_eq!(
        fx::unsubscribe_tokens(&rig, &rig.install_id, &send_id).unwrap(),
        0
    );
    assert_eq!(
        fx::unsubscribe_index_count(&rig, &rig.install_id, &context).unwrap(),
        0
    );
    let counts = rig.smtp_barrier().unwrap();
    assert_eq!(
        counts.tcp_sessions, 0,
        "encrypted send opened an SMTP session"
    );
    assert_eq!(
        counts.accepted_messages, 0,
        "encrypted send was accepted by the peer"
    );
    assert_eq!(counts.auth_a, 0, "encrypted send authed under secret A");
    rig.finish().unwrap();
}

// ============================================================================
// Gates 2 & 3 (preclaim C→D, pretoken) mount inside crm_send_rpc::tests —
// crm_send_run/crm_send_row_step are private `fn`, sibling-unreachable. The
// in-file cases are provided in integration.md; they reuse
// `seed_stale_encrypted_send` and assert the same zero-residue set.
// ============================================================================
