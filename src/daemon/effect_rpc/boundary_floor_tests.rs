//! CAD-1128 early-boundary floor — the ORIGINAL remaining controls, real
//! in-crate executable (no lane writes). Mount as a child of
//! `effect_rpc::tests` — the only tree where `pub(super)` `respond_effect`,
//! `refuse_encrypted_target`, `scan_effect_sources` and the private
//! `stage_send` are reachable (same module as `rpc_platform_call`). Reuses
//! `cad1128_canary_fx` + real `Rig`. Owned by cad1128-confidentiality-swe.
//!
//! Three controls, each an encrypted authoritative target refusing
//! `unsupported_custody_use` BEFORE the first callback/claim/token/receipt/
//! durable-decision write — plus a legacy positive that proves the path works.
#![cfg(all(unix, feature = "test-seam"))]

use crate::daemon::cad1128_canary_fx as fx;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::json;

const A: &str = "cad1128-custody-a";
const B: &str = "cad1128-custody-b";

/// A `platform_effects` row's `state`/`decision` for the given request —
/// `Ok(Some(state, has_decision))`, `Ok(None)` if absent.
fn effect_state(
    rig: &crate::daemon::custody_acceptance_fixture::Rig,
    request: &str,
) -> crate::error::Result<Option<(String, bool)>> {
    Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?
        .query_row(
            "SELECT state, decision IS NOT NULL FROM platform_effects WHERE request=?",
            [request],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)),
        )
        .optional()
        .map_err(|e| crate::error::Error::internal(e.to_string()))
}

// ==========================================================================
// CONTROL A — app quote: `app_capability_quote` refuses the encrypted
// `BindingProof` BEFORE the provider quote / claim / receipt. Mount-point is
// `app_capabilities_rpc::tests` (pub(super) there) — see integration.md for
// the verbatim case; this module carries the GENERIC accept + stage controls.
// ==========================================================================

// ==========================================================================
// CONTROL B — generic effect accept: `respond_effect` on a `waiting`
// `platform_effects` row whose `platform`/`account` is encrypted refuses
// `unsupported_custody_use` BEFORE `effect_decide` — the durable decision is
// the observable. Removing `refuse_encrypted_target` flips `decision`/`state`
// even though the later execution backstop also refuses — a real red.
// ==========================================================================

/// Seed a `waiting` `platform_effects` row pinned to an encrypted
/// `platform`/`account` — a crafted historical stage (same pattern as the
/// stale-send window). Returns the `request` handle. The row is `waiting`
/// with `decision` NULL — a valid press target.
fn seed_waiting_encrypted_effect(
    rig: &crate::daemon::custody_acceptance_fixture::Rig,
    agent: &str,
    platform: &str,
    account: &str,
    request: &str,
) {
    let conn = Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .expect("core db");
    conn.execute(
        "INSERT INTO platform_effects
         (effect_id,request,agent,platform,account,tool,label,input,input_summary,
          preview,source_name,source_hash,scopes,task,state,close_reason,decision,
          outcome,needs_you,staged_at,updated_at)
         VALUES (?1,?2,?3,?4,?5,'email.send',NULL,'{}','send','prev',NULL,NULL,
                 '[]',NULL,'waiting',NULL,NULL,NULL,0,0,0)",
        rusqlite::params![
            format!("eff-{}", uuid::Uuid::new_v4().simple()),
            request,
            agent,
            platform,
            account
        ],
    )
    .expect("seed waiting effect failed");
}

/// POSITIVE — a `waiting` effect on a NON-encrypted (builtin/legacy)
/// `platform`/`account`: `respond_effect(accept)` must NOT refuse
/// `unsupported_custody_use`. It is operator-only to accept — the real
/// operator caller presses; on a non-encrypted row the press lands a real
/// `decision` (proving the path works), never the custody refusal.
#[test]
fn waiting_effect_accept_legacy_decides() {
    let dir = fx::new_root();
    let rig = crate::daemon::custody_acceptance_fixture::Rig::start(
        dir.path(),
        A.as_bytes(),
        B.as_bytes(),
    )
    .expect("rig start failed");
    let shared = fx::shared(&rig);
    // Real agent + a `waiting` effect on the builtin local outbox account —
    // not encrypted. `named_alias` must equal `row.agent`.
    let cwd = rig.state_dir.clone();
    shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "agent-b",
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
    // A real `platform_grants` row on `local`/`local` — the post-decision
    // `execute` grant re-check (`check_scopes`) requires a grant even for an
    // empty-scope row; absent it `execute_effect` `grant_revoked`s the row
    // and `effect_close` clears `decision`, so the accept would never leave
    // a durable decision to observe.
    shared
        .store
        .platform_grant_add("agent-b", "local", "local", &[], "operator")
        .expect("local grant failed");
    let req = "req-legacy-accept";
    seed_waiting_encrypted_effect(
        &rig,
        "agent-b",
        "local",
        crate::platform::BUILTIN_LOCAL_ACCOUNT,
        req,
    );
    // Fetch the staged row, then press accept as the operator over the real
    // socket — `agent_caller` resolves Operator when the peer is the
    // operator connection.
    // Drive the real `agent_respond` over the operator socket — `respond_effect`
    // resolves the presser from `agent_caller(peer_pid)`; the test seams the
    // operator caller explicitly, so the presser is a genuine operator.
    // A non-encrypted accept must NOT refuse for custody — the decision lands
    // or the refusal is a different reason, never `unsupported_custody_use`.
    let r = crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc_private_timeout(
            &rig.state_dir,
            "agent_respond",
            json!({"alias": "agent-b", "request": req, "decision": "accept"}),
            std::time::Duration::from_secs(30),
        )
    });
    eprintln!("CAD-1128 legacy accept RPC result: {:?}", r);
    // The only forbidden outcome is `unsupported_custody_use`; on a
    // non-encrypted target the operator accept LANDS the durable decision
    // (state leaves `waiting` / `decision` set) — the path works end-to-end.
    if let Err(e) = &r {
        assert_ne!(
            e.code(),
            Some(fx::UNSUPPORTED),
            "legacy waiting accept refused for encrypted custody — guard over-fires"
        );
    }
    let (_state, decided) = effect_state(&rig, req)
        .unwrap()
        .expect("legacy waiting row vanished");
    assert!(
        decided,
        "legacy accept wrote no durable decision — path broken"
    );
    rig.finish().unwrap();
}

/// NEGATIVE — a `waiting` `platform_effects` row whose `platform`/`account`
/// resolves to an encrypted connection refuses `unsupported_custody_use` at
/// `respond_effect` BEFORE `effect_decide`. Asserts the row stays `waiting`
/// with `decision` NULL — the durable decision is the forbidden residue.
/// MEANINGFUL MUTATION: remove `refuse_encrypted_target(&row.platform,
/// &row.account)` in `respond_effect` → `effect_decide` flips the row to
/// `decided` even though the post-decision execution backstop also refuses;
/// `state`/`decision` change IS the real red.
#[test]
fn waiting_effect_accept_encrypted_refuses_no_decision() {
    let dir = fx::new_root();
    let rig = crate::daemon::custody_acceptance_fixture::Rig::start(
        dir.path(),
        A.as_bytes(),
        B.as_bytes(),
    )
    .expect("rig start failed");
    fx::unlock_vault(&rig);
    let conn_id = fx::enroll_smtp(&rig, "enc-acc", A);
    assert!(fx::is_encrypted(&rig, &conn_id).unwrap());
    let shared = fx::shared(&rig);
    let cwd = rig.state_dir.clone();
    shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "agent-e",
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
    // The waiting row's platform/account must be the ENCRYPTED credential —
    // the enrolled connection's provider+account. `refuse_encrypted_target`
    // looks up `platform_credentials` by (provider,account); the encrypted
    // tag rides `connection_credential`. Seed it pinned to the enrolled
    // provider+account so the target-class resolves.
    let req = "req-enc-accept";
    seed_waiting_encrypted_effect(&rig, "agent-e", "smtp", "enc-acc", req);
    // `agent_respond` over the real operator socket reaches `respond_effect`'s
    // `refuse_encrypted_target(&row.platform,&row.account)` under the held
    // custody section — before `effect_decide`. `fx::refused` wraps the call
    // in `scoped(Asserted::Operator)` and returns the refusal code.
    let code = fx::refused(
        &rig,
        "agent_respond",
        json!({"alias": "agent-e", "request": req, "decision": "accept"}),
    );
    assert_eq!(
        code.as_deref(),
        Some(fx::UNSUPPORTED),
        "encrypted waiting accept must refuse unsupported_custody_use before decide"
    );
    let (state, decided) = effect_state(&rig, req)
        .unwrap()
        .expect("waiting effect row vanished");
    assert_eq!(state, "waiting", "encrypted accept left the row waiting");
    assert!(!decided, "encrypted accept wrote a durable decision");
    rig.finish().unwrap();
}

// ==========================================================================
// CONTROL C — app-artifact stage: `stage_app_artifact` calls
// `refuse_encrypted_binding(&proof.config)` BEFORE `prepare_app_artifact` /
// `preview` / `app_effect_stage`. A run frozen with an encrypted-capability
// binding must refuse with zero `platform_effects`/callback residue. This is
// the residual-window seed (the binding turned encrypted after the run's
// snapshot froze) — the stored proof is genuine, the target is now encrypted.
// Reachable only inside `app_effects_rpc::tests` — see integration.md for the
// verbatim case + the real-run/material seed it requires (the
// `app_binding_receipt_current` receipt precedes the custody check, so the
// run's frozen binding must be current, not forged).
// ==========================================================================
