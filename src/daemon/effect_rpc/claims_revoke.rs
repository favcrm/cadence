//! CAD-1128 original-r2 PRIORITY case — the direct `platform_revoke`
//! expected-(id,revision) source defect, as a small standalone driver the
//! sole writer can mount and run BEFORE the rest of the claims package.
//! Independently authored by `cad1128-claims-swe`; artifact-only,
//! uncompiled/unexecuted — `unrun is unrun`. Expected outcomes derive from
//! the original reviewed contract SHA-256
//! `3b17bc61f057d992ed2a423887d458002b75b4c8f6a8fa54d9d49ba7794167e8`, never
//! from the code under test.
//!
//! Mount as `src/daemon/effect_rpc/claims_revoke.rs` + ONE line in
//! `src/daemon/effect_rpc.rs` beside `mod platform_call_floor_tests;`:
//!     #[cfg(all(test, unix, feature = "test-seam"))]
//!     #[path = "effect_rpc/claims_revoke.rs"]
//!     mod claims_revoke;
//! Only `daemon::effect_rpc` tree visibility is needed for consistency with
//! the rest of the claims package; this file itself uses only `Rig`,
//! `cad1128_canary_fx`, `client::rpc_private_timeout` and `test_seam`.
//!
//! THE DEFECT (source, current `platform_rpc.rs`):
//!   `rpc_platform_revoke` → `revoke_inner(params, None)` →
//!   `revoke_inner_expected(params, None, None)` — the caller's
//!   `expected_connection_id`/`expected_revision` are never forwarded, then
//!   `platform_revoke_encrypted` is invoked with
//!   `expected_id.unwrap_or(&record.connection_id)` /
//!   `expected_revision.unwrap_or(record.credential_revision)` — i.e. the
//!   freshly-read CURRENT values are substituted for the caller's intent.
//!   `platform_revoke` has no parameter allowlist (`reject_operator_fields`/
//!   `reject_identity_fields` gate only identity fields), so a supplied
//!   `expected_*` pair is silently dropped and the revoke still retires the
//!   live record.
//!
//! CONTRACT (r2 "Enrollment, rotation, revoke"): "For encrypted
//! rotate/revoke, bind each attempt to expected_connection_id and
//! expected_revision." The `connection_revoke` facade already does; the
//! direct `platform_revoke` path does not. A caller-supplied STALE
//! expectation must refuse and retire nothing. This test asserts the
//! CONTRACT behavior, so it is RED on the shipping defective path (the
//! stale revoke succeeds) and GREEN only after the owned repair binds the
//! caller's pair on the direct path too.
//!
//! This file deliberately carries NO other claim — it is the priority
//! test-first reproduction. The full lifecycle/reason coverage lives in
//! `claims_lifecycle.rs`/`claims_reason.rs` (same package).
#![cfg(all(unix, feature = "test-seam"))]

use crate::daemon::cad1128_canary_fx as fx;
use crate::daemon::custody_acceptance_fixture::Rig;
use crate::error::{Error, Result};
use serde_json::{json, Value};
use std::time::Duration;

/// Synthetic credential halves — never a real secret shape.
const A: &str = "cad1128-claims-A-9f3c1d2e";
const B: &str = "cad1128-claims-B-4a7b8c9d";
const RPC: Duration = Duration::from_secs(30);

fn must<T, E>(r: std::result::Result<T, E>, stage: &'static str) -> T {
    r.unwrap_or_else(|_| panic!("{stage}"))
}

/// Operator RPC over the REAL socket. Asserts the public result never
/// echoes synthetic credential material before returning it.
fn rpc(rig: &Rig, method: &str, params: Value) -> Result<Value> {
    let answer = crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc_private_timeout(&rig.state_dir, method, params, RPC)
    });
    if let Ok(v) = &answer {
        let text = v.to_string();
        assert!(
            !text.contains(A) && !text.contains(B),
            "a public RPC result disclosed synthetic credential material"
        );
    }
    answer
}

fn refused(answer: Result<Value>, stage: &'static str) -> Error {
    match answer {
        Err(e) => e,
        Ok(v) => panic!("{stage}: unexpectedly succeeded: {v}"),
    }
}

fn code(err: &Error) -> Option<String> {
    err.code().map(|c| c.to_string())
}

/// `connection_show` -> `revision` (the operator-prefetch expectation a
/// revoke/rotate binds to).
fn live_revision(rig: &Rig, id: &str) -> u64 {
    let shown = must(
        rpc(rig, "connection_show", json!({"connection_id": id})),
        "connection_show failed",
    );
    shown["connection"]["revision"]
        .as_u64()
        .or_else(|| shown["revision"].as_u64())
        .or_else(|| shown["connection"]["credential_revision"].as_u64())
        .expect("connection_show carried no revision")
}

fn rotate_params(rig: &Rig, id: &str, expected_rev: u64, secret: &str) -> Value {
    json!({
        "connection_id": id,
        "expected_connection_id": id,
        "expected_revision": expected_rev,
        "host": "localhost", "port": rig.smtp_port, "tls_mode": "implicit",
        "username": "fixture-user", "secret": secret,
        "sender": "fixture@fixture.cadence", "sender_name": "Fixture",
        "scopes": ["email:send"], "accept_same_uid_risk": true
    })
}

/// PRIORITY reproduction — the direct `platform_revoke` must bind BOTH
/// caller expectations independently; on the shipping defect both are
/// silently dropped, so each stale attempt SUCCEEDS and `refused` panics —
/// the contract-derived red. Not a compiled claim: authored for the sole
/// writer's admitted test-first run.
#[test]
fn direct_platform_revoke_binds_stale_expectation() {
    let dir = fx::new_root();
    let rig = must(
        Rig::start(dir.path(), A.as_bytes(), B.as_bytes()),
        "rig start failed",
    );
    fx::unlock_vault(&rig);
    let account = "drev-acct";
    let id = fx::enroll_smtp(&rig, account, A);
    assert!(
        fx::is_encrypted(&rig, &id).unwrap(),
        "enrolled connection is not encrypted"
    );
    let rev1 = live_revision(&rig, &id);
    assert_eq!(rev1, 1, "fresh enrollment is not revision 1");

    // The operator prefetched rev=1; a real `connection_rotate` commits
    // rev=2 before the revoke lands — a genuine two-revision record, never
    // a fabricated row.
    must(
        rpc(&rig, "connection_rotate", rotate_params(&rig, &id, rev1, B)),
        "setup rotate to rev2 failed",
    );
    assert_eq!(live_revision(&rig, &id), 2, "setup rotate did not commit");

    // STALE expected-REVISION with the MATCHING incarnation: the caller bound
    // the attempt to rev 1 but the record moved to rev 2 — the contract says
    // refuse `stale_revision` and retire nothing.
    let stale_revoke = rpc(
        &rig,
        "platform_revoke",
        json!({
            "platform": "smtp", "account": account,
            "expected_connection_id": id,
            "expected_revision": rev1,
        }),
    );
    let err = refused(
        stale_revoke,
        "stale-expectation platform_revoke revoked anyway",
    );
    assert_eq!(
        code(&err).as_deref(),
        Some("stale_revision"),
        "stale expected revision did not surface stale_revision: {err}"
    );
    let still = must(rig.core_record(&id), "post-revoke core read failed");
    assert_eq!(
        still.credential_revision, 2,
        "a stale-expectation revoke retired or changed live authority"
    );

    // STALE expected-INCARNATION with the MATCHING live revision: a different
    // connection id must refuse the incarnation guard — the two expectations
    // bind independently, not as one optional pair.
    let wrong_id = rpc(
        &rig,
        "platform_revoke",
        json!({
            "platform": "smtp", "account": account,
            "expected_connection_id": "conn-00000000000000000000000000000000",
            "expected_revision": 2u64,
        }),
    );
    let err2 = refused(wrong_id, "stale-incarnation platform_revoke revoked anyway");
    assert!(
        err2.to_string().contains("incarnation is stale"),
        "stale expected incarnation did not land the incarnation refusal: {err2}"
    );
    let still2 = must(rig.core_record(&id), "post-incarnation-read failed");
    assert_eq!(
        still2.credential_revision, 2,
        "a stale-incarnation revoke retired or changed live authority"
    );

    // POSITIVE control on the direct path: a revoke carrying the CURRENT
    // matching pair succeeds and retires the record — the expectation gate
    // admits only the exact current pair, not merely refuses.
    let ok = rpc(
        &rig,
        "platform_revoke",
        json!({
            "platform": "smtp", "account": account,
            "expected_connection_id": id, "expected_revision": 2u64,
        }),
    );
    assert!(
        ok.is_ok(),
        "current-expectation platform_revoke was refused: {ok:?}"
    );
    assert!(
        fx::shared(&rig)
            .store
            .platform_credential("smtp", account)
            .expect("credential read")
            .is_none(),
        "current-expectation revoke did not retire the record"
    );
    must(rig.finish(), "rig shutdown failed");
}

/// LEGACY positive control — the mandatory-pair guard is encrypted-only.
/// A FILE-custody `platform_revoke` carrying NO `expected_*` pair must still
/// retire the credential exactly as before the fix; this is the omitted-pair
/// continuation the repair must preserve, exercised against a real enrolled
/// legacy record (not a code-reading claim).
#[test]
fn legacy_platform_revoke_without_expectation_still_retires() {
    let dir = fx::new_root();
    let rig = must(
        Rig::start_legacy(dir.path(), A.as_bytes(), B.as_bytes()),
        "legacy rig start failed",
    );
    let account = "drev-legacy";
    let id = fx::enroll_smtp(&rig, account, A);
    assert!(
        !fx::is_encrypted(&rig, &id).unwrap(),
        "legacy enroll unexpectedly encrypted"
    );
    // The credential record is present and readable before the revoke — the
    // authoritative before-state, not an assumption.
    assert!(
        fx::shared(&rig)
            .store
            .platform_credential("smtp", account)
            .expect("credential read")
            .is_some(),
        "legacy credential absent before revoke"
    );
    // Direct `platform_revoke` with NO expectation pair: the contract's
    // mandatory-pair guard is encrypted-only, so the legacy path retires the
    // record.
    let out = rpc(
        &rig,
        "platform_revoke",
        json!({"platform": "smtp", "account": account}),
    );
    assert!(
        out.is_ok(),
        "legacy no-pair platform_revoke was refused: {out:?}"
    );
    assert_eq!(
        out.as_ref().unwrap()["state"].as_str().unwrap_or(""),
        "revoked",
        "legacy revoke did not report state:revoked"
    );
    // Authoritative after-state: the credential record is gone.
    assert!(
        fx::shared(&rig)
            .store
            .platform_credential("smtp", account)
            .expect("credential read")
            .is_none(),
        "legacy no-pair revoke did not retire the credential"
    );
    must(rig.finish(), "rig shutdown failed");
}
