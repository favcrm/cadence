//! CAD-1128 `rpc_platform_call` boundary-floor — the CONFIRMED current
//! defect, real-RPC executable (own `.tmp` only). Mount as a child of
//! `effect_rpc::tests` (same module as `rpc_platform_call` —
//! `scan_effect_sources`/`effect_by_request` + seed SQL co-reachable). Real
//! `Rig`, real socket, real `connection_create` smtp enroll — NO forged tag,
//! no API widening. Owned by cad1128-confidentiality-swe.
//!
//! DEFECT (effect_rpc.rs:70-135): `check_grant` → `scan_effect_sources` →
//! `stage_send` (`implied_source`:303 / `source_hash`:308) all run BEFORE
//! `platform::load_credential` (:314). A `platform_call` on a REAL enrolled
//! `smtp`/`account` whose `platform_credentials.custody` carries the
//! `encrypted-sqlite-v1:` tag reaches those source callbacks + the cancel
//! scan before the late loader refuses.
//!
//! OBSERVABLE, NO COUNTING SEAM: `scan_effect_sources` calls the REAL
//! `a.source_hash(row.agent, source)` on every `waiting` `platform_effects`
//! row with a `source_name`. The real `SmtpAdapter::source_hash` returns
//! `None`; a `waiting` smtp row with `source_name` + `source_hash=Some(..)`
//! is therefore `effect_close`d (`source_changed`) + emits
//! `request_closed` — durable/public callback-driven residue proving the call
//! ran the pre-loader stage even while the call itself refused. This oracle
//! proves the scan's `source_hash` callback + the resulting durable/public
//! residue ran pre-loader; it does NOT claim a direct zero-count for every
//! `implied_source`/`preview`/`execute` callback (no adapter observer exists).
#![cfg(all(unix, feature = "test-seam"))]

use crate::daemon::cad1128_canary_fx as fx;
use rusqlite::{params, Connection, OpenFlags};
use serde_json::json;

const A: &str = "cad1128-custody-a";
const B: &str = "cad1128-custody-b";

/// Register a real agent + grant `email:send` on `(smtp, account)` so
/// `check_grant` passes and `platform_call` reaches `scan_effect_sources`.
fn smtp_agent(rig: &crate::daemon::custody_acceptance_fixture::Rig, agent: &str, account: &str) {
    let shared = fx::shared(rig);
    let cwd = rig.state_dir.clone();
    shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: agent,
            provider: "codex",
            endpoint_kind: "managed",
            role: "worker",
            cwd: cwd.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .expect("register agent failed");
    shared
        .store
        .platform_grant_add(
            agent,
            "smtp",
            account,
            &["email:send".to_string()],
            "operator",
        )
        .expect("smtp grant failed");
}

/// Seed a `waiting` `platform_effects` row on `(smtp, account)` for `agent`
/// with `source_name` + `source_hash=Some`. The real `SmtpAdapter::source_hash`
/// returns `None`, so `scan_effect_sources` sees a changed source and
/// `effect_close`s the row — the durable residue under test. This is a
/// CRAFTED unsupported-history row (a send staged while the connection's
/// custody was live before rotation — never a legacy→encrypted migration
/// path); it is an explicit precondition, not a supported encrypted route.
fn seed_waiting_smtp_effect(
    rig: &crate::daemon::custody_acceptance_fixture::Rig,
    agent: &str,
    account: &str,
    request: &str,
) {
    Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .expect("core db")
        .execute(
            "INSERT INTO platform_effects
             (effect_id,request,agent,platform,account,tool,label,input,input_summary,
              preview,source_name,source_hash,scopes,task,state,close_reason,decision,
              outcome,needs_you,staged_at,updated_at)
             VALUES (?1,?2,?3,'smtp',?4,'email.send',NULL,'{}','send','prev',
                     'pin-stale','sha:STALE','[\"email:send\"]',NULL,'waiting',
                     NULL,NULL,NULL,0,0,0)",
            params![
                format!("eff-{}", uuid::Uuid::new_v4().simple()),
                request,
                agent,
                account
            ],
        )
        .expect("seed waiting smtp effect failed");
}

/// `platform_effects` `(state, close_reason)` for `request`.
fn effect_state(
    rig: &crate::daemon::custody_acceptance_fixture::Rig,
    request: &str,
) -> crate::error::Result<Option<(String, Option<String>)>> {
    use rusqlite::OptionalExtension;
    Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?
        .query_row(
            "SELECT state, close_reason FROM platform_effects WHERE request=?",
            [request],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(|e| crate::error::Error::internal(e.to_string()))
}

/// Count `events` rows — `request_closed`/`request_opened`/`platform_called`
/// land here (`event_public` writes the `events` table).
fn event_count(rig: &crate::daemon::custody_acceptance_fixture::Rig) -> i64 {
    Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("core db")
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .expect("event count query failed")
}

/// Count `request_closed` `source_changed` events for `request` on `agent`'s
/// stream — the exact per-agent/per-request event `emit_request_closed`
/// writes (`kind='request_closed'`, `payload.reason='source_changed'`).
/// `Ok(count)`, never a disguised `0` on a read error.
fn source_changed_event_count(
    rig: &crate::daemon::custody_acceptance_fixture::Rig,
    agent: &str,
    request: &str,
) -> i64 {
    Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("core db")
        .query_row(
            "SELECT COUNT(*) FROM events WHERE alias=? AND kind='request_closed'
             AND json_extract(payload,'$.request')=?
             AND json_extract(payload,'$.reason')='source_changed'",
            params![agent, request],
            |r| r.get(0),
        )
        .expect("source_changed event count query failed")
}

/// `platform_call {platform:"smtp", account, tool:"email.send"}` by `agent`
/// over the real socket — caller is the connection-derived agent.
fn smtp_call(
    rig: &crate::daemon::custody_acceptance_fixture::Rig,
    agent: &str,
    account: &str,
    request: &str,
) -> crate::error::Result<serde_json::Value> {
    let alias = agent.to_string();
    let acct = account.to_string();
    let req = request.to_string();
    crate::test_seam::scoped(crate::test_seam::Asserted::Agent(alias), move || {
        crate::client::rpc_private_timeout(
            &rig.state_dir,
            "platform_call",
            json!({"platform": "smtp", "account": acct, "request": req,
                   "tool": "email.send", "input": {"to": "r@e.test"}}),
            std::time::Duration::from_secs(30),
        )
    })
}

/// POSITIVE — `platform_call` on a NON-encrypted `smtp`/`account` by a
/// granted agent reaches `scan_effect_sources`: a `waiting` row with a
/// `source_name` the real `SmtpAdapter::source_hash` returns `None` for is
/// `effect_close`d (`source_changed`). Proves the residue is produced by the
/// scan/callback, not by setup — the eligible legacy twin.
#[test]
fn platform_call_legacy_smtp_scans_and_closes_stale_row() {
    let dir = fx::new_root();
    // Legacy rig — non-encrypted custody so `load_credential` does not refuse.
    let rig = crate::daemon::custody_acceptance_fixture::Rig::start_legacy(
        dir.path(),
        A.as_bytes(),
        B.as_bytes(),
    )
    .expect("rig start failed");
    // File custody never locks — no `unlock_vault` needed; the enroll writes
    // a plaintext `platform_credentials` row.
    let account = "legacy-acct";
    let conn = fx::enroll_smtp(&rig, account, A);
    assert!(
        !fx::is_encrypted(&rig, &conn).unwrap(),
        "legacy enroll must be unencrypted"
    );
    smtp_agent(&rig, "agent-p", account);
    seed_waiting_smtp_effect(&rig, "agent-p", account, "req-stale");
    // The call itself must STAGE successfully — a generic `email.send`
    // legitimately stages on an unencrypted `smtp` (no provider execute
    // required); `stage_send` answers `{"result":"staged"}`. The `input`
    // carries a `to` field; no `source`/`input.source` is needed.
    let reply = smtp_call(&rig, "agent-p", account, "req-pos")
        .expect("legacy smtp platform_call did not stage");
    assert_eq!(
        reply["result"], "staged",
        "legacy call did not stage: {reply}"
    );
    let (staged_state, _) = effect_state(&rig, "req-pos")
        .unwrap()
        .expect("staged row missing");
    assert_eq!(staged_state, "waiting", "staged row not waiting");
    // AND the scan closed the stale row — the callback-driven residue proving
    // the pre-loader stage ran.
    let (state, close_reason) = effect_state(&rig, "req-stale")
        .unwrap()
        .expect("stale row vanished");
    assert_eq!(
        (state.as_str(), close_reason.as_deref()),
        ("closed", Some("source_changed")),
        "legacy smtp platform_call did not trigger the source-scan close"
    );
    // The scan also emitted the exact `request_closed`/`source_changed` event
    // on this agent/request — the public callback-driven residue.
    assert!(
        source_changed_event_count(&rig, "agent-p", "req-stale") >= 1,
        "legacy smtp platform_call did not emit the source_changed close event"
    );
    rig.finish().unwrap();
}

/// NEGATIVE (CURRENT-DEFECT RED → GREEN after fix) — `platform_call` on a
/// REAL enrolled `smtp`/`account` whose `platform_credentials.custody` is
/// `encrypted-sqlite-v1:`: the call must refuse `unsupported_custody_use` and
/// the stale `waiting` row must NOT be closed — the early classification runs
/// BEFORE `scan_effect_sources`/`implied_source`/`source_hash`.
///
/// CURRENT-DEFECT RED: absent an early `refuse_encrypted_target` gate before
/// `check_grant`, `scan_effect_sources` runs → the real
/// `SmtpAdapter::source_hash` returns `None` ≠ the row's `sha:STALE` →
/// `effect_close` (`source_changed`) + `request_closed` — residue >0 even
/// while `load_credential` later refuses. That close/event IS the real
/// behavioral red; the durable row state + emitted event are the oracle.
#[test]
fn platform_call_encrypted_smtp_refuses_before_source_scan() {
    let dir = fx::new_root();
    let rig = crate::daemon::custody_acceptance_fixture::Rig::start(
        dir.path(),
        A.as_bytes(),
        B.as_bytes(),
    )
    .expect("rig start failed");
    fx::unlock_vault(&rig);
    // Real `connection_create` smtp enroll on ENCRYPTED custody — the genuine
    // authoritative encrypted target (real init+unlock, real connection_id).
    let account = "enc-acct";
    let conn = fx::enroll_smtp(&rig, account, A);
    assert!(fx::is_encrypted(&rig, &conn).unwrap());
    smtp_agent(&rig, "agent-e", account);
    seed_waiting_smtp_effect(&rig, "agent-e", account, "req-stale-enc");
    let events_before = event_count(&rig);
    let r = smtp_call(&rig, "agent-e", account, "req-enc-call");
    let code = match r {
        Err(e) => e.code().map(|c| c.to_string()),
        Ok(v) => panic!("encrypted smtp platform_call unexpectedly succeeded: {v}"),
    };
    assert_eq!(
        code.as_deref(),
        Some(fx::UNSUPPORTED),
        "encrypted smtp platform_call must refuse unsupported_custody_use"
    );
    let (state, close_reason) = effect_state(&rig, "req-stale-enc")
        .unwrap()
        .expect("stale row vanished");
    assert_eq!(
        state, "waiting",
        "encrypted platform_call closed the stale row"
    );
    assert_eq!(
        close_reason, None,
        "encrypted platform_call wrote a close_reason"
    );
    assert_eq!(
        event_count(&rig),
        events_before,
        "encrypted platform_call emitted a request_closed/platform event"
    );
    // And specifically no `request_closed`/`source_changed` event on this
    // agent/request — the scan's callback residue never ran.
    assert_eq!(
        source_changed_event_count(&rig, "agent-e", "req-stale-enc"),
        0,
        "encrypted platform_call emitted a source_changed close event"
    );
    // The call's own staged row must never exist — no `req-enc-call` effect
    // landed before the refusal.
    assert!(
        effect_state(&rig, "req-enc-call").unwrap().is_none(),
        "encrypted platform_call staged an effect row"
    );
    rig.finish().unwrap();
}
