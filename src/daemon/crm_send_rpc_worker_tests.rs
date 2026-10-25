//! CAD-1128 worker preclaim/pretoken canaries — for `crm_send_rpc.rs`'s own
//! `#[cfg(test)] mod` (the only place the private `fn`s `crm_send_run` /
//! `crm_send_row_step` are reachable). Mount as a child module of
//! `crm_send_rpc::tests` — no API widening, no placeholders. Reuses the
//! sibling `cad1128_canary_fx` helpers (crate-visible under the test-seam
//! gate) and the real `Rig`. `RecordStore` is already imported in the file.
//! Owned by cad1128-confidentiality-swe.
#![cfg(all(unix, feature = "test-seam"))]

use crate::daemon::cad1128_canary_fx as fx;

const A: &str = "cad1128-custody-a";
const B: &str = "cad1128-custody-b";

/// Gate 2 — `crm_send_run` preclaim C→D: a `sending` send pinned to a
/// now-encrypted connection must never flip a queued delivery to
/// `submitting`, mint a token, index it, or open a socket. The classify+claim
/// in one short C→D section refuses `unsupported_custody_use` before the
/// claim — assert the exact code AND the row never reaches `submitting`/
/// `sent` (a setup error would also Err, so the code pin matters).
#[test]
fn worker_preclaim_never_submitting_zero_tokens_zero_socket() {
    let dir = fx::new_root();
    let rig = crate::daemon::custody_acceptance_fixture::Rig::start(
        dir.path(),
        A.as_bytes(),
        B.as_bytes(),
    )
    .expect("rig start failed");
    fx::unlock_vault(&rig);
    let conn = fx::enroll_smtp(&rig, "enc-run", A);
    assert!(fx::is_encrypted(&rig, &conn).unwrap());
    let (context, customer, email) = fx::context_with_customer(&rig, &rig.install_id, "wr");
    let send_id = fx::seed_stale_encrypted_send(
        &rig,
        &rig.install_id,
        &context,
        "camp-wr",
        "frz-wr",
        &conn,
        &customer,
        &email,
    )
    .expect("seed failed");
    let shared = fx::shared(&rig);
    // `crm_send_run` is a private `fn` reachable in this module — drive the
    // real worker loop and assert the exact refusal code, not just any Err.
    let r = shared.crm_send_run(&rig.install_id, &context, &send_id);
    assert_eq!(
        r.unwrap_err().code(),
        Some("unsupported_custody_use"),
        "encrypted run must refuse at the preclaim classify/claim section"
    );
    let st = fx::delivery_state(&rig, &rig.install_id, &context, &send_id, &customer).unwrap();
    assert_ne!(
        st.as_deref(),
        Some("submitting"),
        "encrypted row reached submitting"
    );
    assert_ne!(st.as_deref(), Some("sent"), "encrypted row was sent");
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
    assert_eq!(counts.accepted_messages, 0);
    assert_eq!(counts.auth_a, 0);
    rig.finish().unwrap();
}

/// Gate 3 — `crm_send_row_step` pretoken: drive the submission step directly
/// on a pre-claimed row with an encrypted target. The held custody section +
/// `refuse_encrypted_campaign_sender` refuses BEFORE the unsubscribe-token
/// mint/record/index writes — assert the exact refusal code and zero
/// token/index rows, no socket.
#[test]
fn worker_row_step_pretoken_refuses_zero_writes() {
    let dir = fx::new_root();
    let rig = crate::daemon::custody_acceptance_fixture::Rig::start(
        dir.path(),
        A.as_bytes(),
        B.as_bytes(),
    )
    .expect("rig start failed");
    fx::unlock_vault(&rig);
    let conn = fx::enroll_smtp(&rig, "enc-step", A);
    assert!(fx::is_encrypted(&rig, &conn).unwrap());
    let (context, customer, email) = fx::context_with_customer(&rig, &rig.install_id, "ws");
    let send_id = fx::seed_stale_encrypted_send(
        &rig,
        &rig.install_id,
        &context,
        "camp-ws",
        "frz-ws",
        &conn,
        &customer,
        &email,
    )
    .expect("seed failed");
    // Claim one delivery so row_step gets a real claimed input — not a canned
    // outcome, just the precondition row_step expects.
    let records = crate::store::app_records::RecordStore::open(&rig.state_dir, &rig.install_id)
        .expect("records open failed");
    records
        .app_campaign_delivery_claim(&context, &send_id, &customer)
        .expect("delivery claim failed");
    let send = records
        .app_campaign_send(&context, &send_id)
        .unwrap()
        .unwrap();
    let shared = fx::shared(&rig);
    let r = shared.crm_send_row_step(&records, &rig.install_id, &context, &send, &customer, None);
    // `Step` is not `Debug` — use `err()`, not `unwrap_err()`.
    assert_eq!(
        r.err().expect("encrypted row_step must refuse").code(),
        Some("unsupported_custody_use"),
        "encrypted row_step must refuse at the pretoken custody section"
    );
    assert_eq!(
        fx::unsubscribe_tokens(&rig, &rig.install_id, &send_id).unwrap(),
        0
    );
    assert_eq!(
        fx::unsubscribe_index_count(&rig, &rig.install_id, &context).unwrap(),
        0
    );
    assert_eq!(rig.smtp_barrier().unwrap().tcp_sessions, 0);
    rig.finish().unwrap();
}
