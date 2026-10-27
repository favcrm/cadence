//! CAD-1128 r2 row201 — reason/cleanup confidentiality at the REAL RPC
//! boundary. Private test-only child of `daemon::effect_rpc`; mount beside
//! `boundary_floor_tests`:
//!
//!     #[cfg(all(test, unix, feature = "test-seam"))]
//!     #[path = "effect_rpc/cad1128_reason_claim_tests.rs"]
//!     mod cad1128_reason_claim_tests;
//!
//! Independently authored from the exact contract
//! `.tmp/approved/cad1128-contract-r2.md` rows
//! `encrypted_custody_reason_cleanup_refuses_unscreenable_text` (row201) and
//! `encrypted_custody_refuses_paused_generic_clone_after_reenroll` (row200).
//! No production/merged/frozen file is edited; the manager mounts and runs.
//!
//! ROW201 (implemented here, current-tree real RPC):
//! - Encrypted target: supplied caller reason text containing the enrolled
//!   synthetic credential refuses BEFORE any durable decision/close_reason,
//!   row projection, public event or response residue — while the vault is
//!   unlocked, then locked, then key-file-absent, then the record is revoked.
//!   Close/decline refuse `reason_unscreenable`; accept refuses at the
//!   earlier encrypted use-site (`unsupported_custody_use`) or the reason
//!   policy — both are pre-write; the residue oracles are the same.
//! - Omitted reason performs an otherwise authorised decline/waiting-close
//!   with fixed `declined`/`cancelled` while the vault is locked and the key
//!   file is absent: no unlock, no provider traffic, no secret.
//! - Operator close of `reconcile` with omitted reason lands the fixed
//!   `operator_reconciled_outcome_unknown`; an encrypted revoke does NOT
//!   close a reconcile row (a possibly-fired outcome is retained, never
//!   reclassified as never-executed).
//! - Legacy available screening still works: on a legacy-custody rig a
//!   supplied reason containing the enrolled secret is refused by the
//!   existing bounded screen, while a benign reason is accepted on decline.
//!
//! ROW200 combined (legacy decided row → actual legacy revoke → encrypted
//! restart of the same root/core DB → fresh same-account incarnation →
//! historical consumer refuses before callback/claim/outcome) lives in
//! `cad1128_execute_claim_tests.rs`, beside the counted adapter and the
//! consumed-Rig restart journey. This file keeps r2 row201 only.
#![cfg(all(unix, feature = "test-seam"))]

use crate::daemon::cad1128_canary_fx as fx;
use crate::daemon::custody_acceptance_fixture::Rig;
use crate::daemon::Shared;
use crate::store::{EffectRow, NewAgent};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};
use std::time::Duration;

const A: &str = "cad1128-custody-a";
const B: &str = "cad1128-custody-b";
const RPC: Duration = Duration::from_secs(30);

fn rpc(rig: &Rig, method: &str, params: Value) -> crate::error::Result<Value> {
    crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc_private_timeout(&rig.state_dir, method, params, RPC)
    })
}

fn must<T, E>(result: std::result::Result<T, E>, stage: &'static str) -> T {
    result.unwrap_or_else(|_| panic!("{stage}"))
}

fn register(shared: &Shared, rig: &Rig, alias: &str) {
    shared
        .store
        .register_agent(&NewAgent {
            alias,
            provider: "codex",
            endpoint_kind: "managed",
            role: "worker",
            cwd: rig.state_dir.to_str().expect("rig state path"),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .expect("register agent failed");
}

/// A controlled `waiting` generic effect on `(smtp, account)` staged through
/// the real Store lifecycle (the fixture row; not a supported encrypted
/// send).
fn waiting(shared: &Shared, agent: &str, account: &str, request: &str) -> EffectRow {
    let row = EffectRow {
        effect_id: format!("eff-{}", uuid::Uuid::new_v4().simple()),
        request: request.to_string(),
        agent: agent.to_string(),
        platform: "smtp".to_string(),
        account: account.to_string(),
        tool: "email.send".to_string(),
        label: None,
        input: json!({"fixture": "r3-row"}),
        input_summary: "send".to_string(),
        preview: "fixture preview".to_string(),
        source_name: None,
        source_hash: None,
        scopes: vec![],
        task: None,
        state: "waiting".to_string(),
        close_reason: None,
        decision: None,
        outcome: None,
        needs_you: false,
        staged_at: 0.0,
        updated_at: 0.0,
    };
    let (row, _) = shared
        .store
        .effect_stage(&row)
        .expect("effect_stage failed");
    assert_eq!(row.state, "waiting");
    row
}

/// A controlled historical `reconcile` row (explicit fixture state, the
/// shape §5.4 step 8 leaves after an uncertain accepted effect).
fn reconcile(rig: &Rig, agent: &str, account: &str, request: &str, outcome: Value) {
    let conn = Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .expect("core db");
    conn.execute(
        "INSERT INTO platform_effects
         (effect_id,request,agent,platform,account,tool,label,input,input_summary,
          preview,source_name,source_hash,scopes,task,state,close_reason,decision,
          outcome,needs_you,staged_at,updated_at)
         VALUES (?1,?2,?3,'smtp',?4,'email.send',NULL,'{}','send','prev',
                 NULL,NULL,'[]',NULL,'reconcile',NULL,?5,?6,1,0,0)",
        params![
            format!("eff-{}", uuid::Uuid::new_v4().simple()),
            request,
            agent,
            account,
            json!({"by": {"member": "operator", "role": "operator",
                "rule": "operator-only"}, "at": "fixture"})
            .to_string(),
            outcome.to_string()
        ],
    )
    .expect("seed reconcile effect failed");
}

fn effect(shared: &Shared, id: &str) -> EffectRow {
    shared
        .store
        .effect_by_id(id)
        .expect("effect read failed")
        .expect("effect row vanished")
}

/// Full durable row snapshot. `EffectRow::to_record` is the shared §5.4
/// schema and intentionally omits bookkeeping (`scopes`, `task`,
/// `source_name`, timestamps, `needs_you`) and absent optional keys. Combine
/// the real record with those actual row fields so an unchanged-row oracle
/// still covers every durable column, without changing production or the
/// shared projection.
fn snapshot(row: &EffectRow) -> Value {
    let mut value = row.to_record();
    value["scopes"] = json!(row.scopes);
    value["task"] = json!(row.task);
    value["source_name"] = json!(row.source_name);
    value["source_hash"] = json!(row.source_hash);
    value["label"] = json!(row.label);
    value["close_reason"] = json!(row.close_reason);
    value["decision"] = json!(row.decision);
    value["outcome"] = json!(row.outcome);
    value["needs_you"] = json!(row.needs_you);
    value["staged_at"] = json!(row.staged_at);
    value["updated_at"] = json!(row.updated_at);
    value
}

fn snapshot_request(shared: &Shared, request: &str) -> Value {
    snapshot(
        &shared
            .store
            .effect_by_request(request)
            .expect("effect request read failed")
            .expect("effect request vanished"),
    )
}

/// A public response/error must not echo the planted reason, the enrolled
/// synthetic secret A, or any 8-byte fragment of A. The production screen is
/// not the oracle here: this rejects what an unguarded echo would actually
/// carry.
fn assert_no_secret(text: &str, marker: &str) {
    assert!(!text.contains(marker), "response echoed the planted reason");
    assert!(!text.contains(A), "response echoed the enrolled secret");
    for window in A.as_bytes().windows(8) {
        let fragment = std::str::from_utf8(window).expect("ascii fixture secret");
        assert!(
            !text.contains(fragment),
            "response echoed an 8-byte enrolled-secret fragment"
        );
    }
}

fn events_count(rig: &Rig) -> i64 {
    Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("core db")
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .expect("event count failed")
}

fn events_carrying(rig: &Rig, needle: &str) -> i64 {
    Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("core db")
        .query_row(
            "SELECT COUNT(*) FROM events WHERE payload LIKE ?1",
            [format!("%{needle}%")],
            |r| r.get(0),
        )
        .expect("event scan failed")
}

/// Supplied-reason refusal: no durable row/decision/event/response residue
/// and no echo of the planted secret. `codes` is the accepted pre-write
/// refusal set (encrypted accept has an independent earlier use-site).
fn refused_reason(
    rig: &Rig,
    shared: &Shared,
    method: &str,
    params: Value,
    row: &EffectRow,
    marker: &str,
    codes: &[&str],
) {
    let before = snapshot(&effect(shared, &row.effect_id));
    let events_before = events_count(rig);
    let result = rpc(rig, method, params);
    assert_eq!(
        snapshot(&effect(shared, &row.effect_id)),
        before,
        "{method} changed the durable row"
    );
    assert_eq!(
        events_count(rig),
        events_before,
        "{method} wrote a public event"
    );
    assert_eq!(
        events_carrying(rig, marker),
        0,
        "{method} persisted the planted reason in an event"
    );
    assert_eq!(
        events_carrying(rig, A),
        0,
        "{method} persisted the enrolled secret in an event"
    );
    let text = match &result {
        Ok(value) => value.to_string(),
        Err(error) => format!("{error:?} {error}"),
    };
    assert_no_secret(&text, marker);
    let error = result.expect_err("a supplied reason must refuse");
    assert!(
        codes.contains(&error.code().unwrap_or("")),
        "{method} refused with an unexpected code: {error:?}"
    );
}

fn status_unlocked(rig: &Rig) -> bool {
    fx::op(rig, "connection_custody_status", json!({}))["custody"]["unlocked"]
        .as_bool()
        .expect("custody status shape")
}

fn lock(rig: &Rig) {
    fx::op(rig, "connection_custody_lock", json!({}));
    assert!(!status_unlocked(rig), "vault lock did not take");
}

#[test]
fn encrypted_reason_cleanup_never_persists_or_echoes_planted_secret() {
    let dir = fx::new_root();
    let rig = Rig::start(dir.path(), A.as_bytes(), B.as_bytes()).expect("rig start failed");
    fx::unlock_vault(&rig);
    let account = "reason-acct";
    let id = fx::enroll_smtp(&rig, account, A);
    let shared = fx::shared(&rig);
    let agent = "agent-r";
    register(&shared, &rig, agent);
    let sessions_before = rig.smtp_barrier().expect("smtp barrier").tcp_sessions;
    // Planted reason carries the synthetic enrolled secret A.
    let marker = format!("planted-{A}-{}", uuid::Uuid::new_v4().simple());

    // 1. UNLOCKED encrypted target: close / decline / accept.
    let w = waiting(&shared, agent, account, "req-reason-open-close");
    refused_reason(
        &rig,
        &shared,
        "platform_effect_close",
        json!({"request": w.request, "reason": marker}),
        &w,
        &marker,
        &["reason_unscreenable"],
    );
    let w = waiting(&shared, agent, account, "req-reason-open-decline");
    refused_reason(
        &rig,
        &shared,
        "agent_respond",
        json!({"alias": agent, "request": w.request, "decision": "decline",
            "reason": marker}),
        &w,
        &marker,
        &["reason_unscreenable"],
    );
    let w = waiting(&shared, agent, account, "req-reason-open-accept");
    refused_reason(
        &rig,
        &shared,
        "agent_respond",
        json!({"alias": agent, "request": w.request, "decision": "accept",
            "reason": marker}),
        &w,
        &marker,
        &["unsupported_custody_use", "reason_unscreenable"],
    );

    // 2. LOCKED encrypted target: same three refusals before any write.
    lock(&rig);
    let w = waiting(&shared, agent, account, "req-reason-locked-close");
    refused_reason(
        &rig,
        &shared,
        "platform_effect_close",
        json!({"request": w.request, "reason": marker}),
        &w,
        &marker,
        &["reason_unscreenable"],
    );
    let w = waiting(&shared, agent, account, "req-reason-locked-decline");
    refused_reason(
        &rig,
        &shared,
        "agent_respond",
        json!({"alias": agent, "request": w.request, "decision": "decline",
            "reason": marker}),
        &w,
        &marker,
        &["reason_unscreenable"],
    );
    let w = waiting(&shared, agent, account, "req-reason-locked-accept");
    refused_reason(
        &rig,
        &shared,
        "agent_respond",
        json!({"alias": agent, "request": w.request, "decision": "accept",
            "reason": marker}),
        &w,
        &marker,
        &["unsupported_custody_use", "reason_unscreenable"],
    );

    // 3. KEY-FILE ABSENT (vault already locked; key bytes removed): refusal
    // still precedes any key read/write, and omitted reasons still work.
    let key = dir.path().join("keys").join("key");
    let key_gone = dir.path().join("keys").join("key.absent");
    std::fs::rename(&key, &key_gone).expect("fixture key hide failed");
    let w = waiting(&shared, agent, account, "req-reason-nokey-close");
    refused_reason(
        &rig,
        &shared,
        "platform_effect_close",
        json!({"request": w.request, "reason": marker}),
        &w,
        &marker,
        &["reason_unscreenable"],
    );
    let w = waiting(&shared, agent, account, "req-reason-nokey-decline");
    refused_reason(
        &rig,
        &shared,
        "agent_respond",
        json!({"alias": agent, "request": w.request, "decision": "decline",
            "reason": marker}),
        &w,
        &marker,
        &["reason_unscreenable"],
    );

    // Omitted reason: authorised fixed-cause decline/close WITHOUT unlocking
    // or reading the absent key, and with no provider traffic.
    let w = waiting(&shared, agent, account, "req-reason-omit-decline");
    let declined = must(
        rpc(
            &rig,
            "agent_respond",
            json!({"alias": agent, "request": w.request, "decision": "decline"}),
        ),
        "omitted-reason encrypted decline refused",
    );
    assert_no_secret(&declined.to_string(), &marker);
    let declined_row = effect(&shared, &w.effect_id);
    assert_eq!(declined_row.state, "declined");
    assert!(declined_row.close_reason.is_none());
    assert!(declined_row.outcome.is_none());
    assert!(
        declined_row
            .decision
            .as_ref()
            .and_then(|d| d.get("reason"))
            .is_none(),
        "omitted decline invented a caller reason"
    );

    let w = waiting(&shared, agent, account, "req-reason-omit-close");
    let closed = must(
        rpc(&rig, "platform_effect_close", json!({"request": w.request})),
        "omitted-reason encrypted close refused",
    );
    assert_no_secret(&closed.to_string(), &marker);
    let closed_row = effect(&shared, &w.effect_id);
    assert_eq!(closed_row.state, "closed");
    assert_eq!(closed_row.close_reason.as_deref(), Some("cancelled"));
    assert!(closed_row.outcome.is_none());
    assert!(
        !status_unlocked(&rig),
        "an omitted close unlocked the vault"
    );

    // 4. Operator close of reconcile with omitted reason: fixed cause.
    reconcile(
        &rig,
        agent,
        account,
        "req-reason-reconcile-close",
        json!({"result": "possibly-fired", "verified": "unknown"}),
    );
    let closed = must(
        rpc(
            &rig,
            "platform_effect_close",
            json!({"request": "req-reason-reconcile-close"}),
        ),
        "omitted-reason reconcile close refused",
    );
    assert_eq!(closed["state"], "closed");
    let reconcile_row = shared
        .store
        .effect_by_request("req-reason-reconcile-close")
        .expect("reconcile read failed")
        .expect("reconcile row vanished");
    assert_eq!(reconcile_row.state, "closed");
    assert_eq!(
        reconcile_row.close_reason.as_deref(),
        Some("operator_reconciled_outcome_unknown")
    );
    assert!(!status_unlocked(&rig), "reconcile close unlocked the vault");

    // 5. Encrypted revoke must NOT close a reconcile (possibly-fired) row.
    reconcile(
        &rig,
        agent,
        account,
        "req-reason-reconcile-survives",
        json!({"result": "possibly-fired", "verified": "unknown"}),
    );
    let survivor_before = snapshot_request(&shared, "req-reason-reconcile-survives");
    let revoked = must(
        rpc(
            &rig,
            "connection_revoke",
            json!({"connection_id": id, "expected_connection_id": id,
                "expected_revision": 1}),
        ),
        "encrypted revoke under a locked/key-absent vault failed",
    );
    assert_eq!(revoked["connection_id"].as_str(), Some(id.as_str()));
    assert_eq!(revoked["revoked"], true);
    assert!(!status_unlocked(&rig), "revoke unlocked the vault");
    let survivor_after = snapshot_request(&shared, "req-reason-reconcile-survives");
    assert_eq!(
        survivor_after, survivor_before,
        "revoke reclassified a reconcile row"
    );
    assert_eq!(survivor_after["state"], "reconcile");
    assert_eq!(
        survivor_after["outcome"]["result"], "possibly-fired",
        "revoke erased the possibly-fired outcome"
    );

    // 6. REVOKED record (absent credential): supplied reason refuses with no
    // residue; an omitted decline is still authorised for a waiting row.
    let w = waiting(&shared, agent, account, "req-reason-revoked-close");
    refused_reason(
        &rig,
        &shared,
        "platform_effect_close",
        json!({"request": w.request, "reason": marker}),
        &w,
        &marker,
        &["reason_unscreenable"],
    );
    let w = waiting(&shared, agent, account, "req-reason-revoked-decline");
    refused_reason(
        &rig,
        &shared,
        "agent_respond",
        json!({"alias": agent, "request": w.request, "decision": "decline",
            "reason": marker}),
        &w,
        &marker,
        &["reason_unscreenable"],
    );
    let w = waiting(&shared, agent, account, "req-reason-revoked-omit");
    must(
        rpc(
            &rig,
            "agent_respond",
            json!({"alias": agent, "request": w.request, "decision": "decline"}),
        ),
        "omitted decline on a revoked account refused",
    );
    assert_eq!(effect(&shared, &w.effect_id).state, "declined");
    assert_eq!(
        rig.smtp_barrier().expect("smtp barrier").tcp_sessions,
        sessions_before,
        "a reason/cleanup path opened provider traffic"
    );

    std::fs::rename(&key_gone, &key).expect("fixture key restore failed");
    drop(shared);
    rig.finish().expect("rig teardown failed");
}

#[test]
fn legacy_available_screen_still_refuses_a_planted_secret_reason() {
    let dir = fx::new_root();
    let rig =
        Rig::start_legacy(dir.path(), A.as_bytes(), B.as_bytes()).expect("legacy rig start failed");
    let account = "legacy-r";
    fx::enroll_smtp(&rig, account, A);
    let shared = fx::shared(&rig);
    let agent = "agent-l";
    register(&shared, &rig, agent);
    let sessions_before = rig.smtp_barrier().expect("smtp barrier").tcp_sessions;
    let marker = format!("planted-{A}-{}", uuid::Uuid::new_v4().simple());

    for (method, params) in [
        (
            "platform_effect_close",
            json!({"request": "req-legacy-close", "reason": marker}),
        ),
        (
            "agent_respond",
            json!({"alias": agent, "request": "req-legacy-decline",
                "decision": "decline", "reason": marker}),
        ),
        (
            "agent_respond",
            json!({"alias": agent, "request": "req-legacy-accept",
                "decision": "accept", "reason": marker}),
        ),
    ] {
        let request = params["request"].as_str().expect("request");
        let row = waiting(&shared, agent, account, request);
        let before = snapshot(&effect(&shared, &row.effect_id));
        let events_before = events_count(&rig);
        let result = rpc(&rig, method, params);
        assert_eq!(
            snapshot(&effect(&shared, &row.effect_id)),
            before,
            "{method} changed the row despite the screen refusal"
        );
        assert_eq!(events_count(&rig), events_before);
        assert_eq!(events_carrying(&rig, &marker), 0);
        assert_eq!(events_carrying(&rig, A), 0);
        let text = match &result {
            Ok(value) => value.to_string(),
            Err(error) => format!("{error:?} {error}"),
        };
        assert_no_secret(&text, &marker);
        let _ = result.expect_err("legacy screen must refuse");
    }

    // Benign legacy reason retains the bounded screen semantics: decline
    // accepts and records it; no provider traffic.
    let row = waiting(&shared, agent, account, "req-legacy-benign");
    let benign = "operator verified this decline";
    must(
        rpc(
            &rig,
            "agent_respond",
            json!({"alias": agent, "request": row.request,
                "decision": "decline", "reason": benign}),
        ),
        "benign legacy decline refused",
    );
    let durable = effect(&shared, &row.effect_id);
    assert_eq!(durable.state, "declined");
    assert_eq!(
        durable
            .decision
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some(benign)
    );
    assert_eq!(
        rig.smtp_barrier().expect("smtp barrier").tcp_sessions,
        sessions_before,
        "a reason path opened provider traffic"
    );

    drop(shared);
    rig.finish().expect("rig teardown failed");
}
