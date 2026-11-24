//! CAD-1128 operator cached capability-return — GENUINE executable checks.
//! Owned by cad1128-confidentiality-swe; UNCOMPILED/UNEXECUTED this phase.
//! Real daemon (`Rig`) + real `Store` run/step/message parents + real
//! `app_capability_results` row (nonempty marker + asset) + the REAL operator
//! RPC socket — no placeholders, no canned outcomes.
//!
//! Why a direct snapshot injection: an SMTP (encrypted-sqlite-v1) connection
//! can NEVER be bound to a capability slot through real verbs — the app's
//! capability slots need `social.read`/`media.generate`/`text.publish`
//! (agenticos_external/local only), and `app_run_create_with_capabilities`
//! rejects an smtp `send`-effect binding via `binding_current_in` + effect
//! checks. The only honest reproduction of "a stored run whose frozen
//! capability config resolves to encrypted" is the crafted/residual window:
//! a real run row whose `snapshot.capabilities` we UPDATE to carry an smtp
//! BindingProof — exactly like seeding a stale-prepared send. It is a setup
//! seed, not a canned outcome; the refusal/red is produced by the real guard.
#![cfg(all(unix, feature = "test-seam"))]

use crate::daemon::cad1128_canary_fx as fx;
use crate::daemon::custody_acceptance_fixture::Rig;
use crate::error::Result;
use crate::store::app_bindings::BindingProof;
use crate::store::app_capabilities::AppCapabilityRecord;
use crate::store::app_runs::{material_digest, LocalRunRequest, LocalWorkflow};
use crate::store::{NewAgent, Take};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;
use tempfile::TempDir;

const A: &str = "cad1128-custody-a";
const B: &str = "cad1128-custody-b";
/// The marker payload the encrypted-refusal case asserts never returns.
const MARKER: &str = "cad1128-cached-return-marker-9f3a";

fn rig(dir: &TempDir) -> Rig {
    let rig = Rig::start(dir.path(), A.as_bytes(), B.as_bytes()).expect("rig start failed");
    fx::unlock_vault(&rig);
    rig
}

fn rpc(rig: &Rig, method: &str, params: Value) -> Result<Value> {
    crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc_private_timeout(&rig.state_dir, method, params, Duration::from_secs(30))
    })
}

/// A `local.text.produce` workflow — one capability-free step producing a real
/// `app_run` row + a dispatchable step (real `app_run_steps`/`messages` parents
/// for the result row's FK). Mirrors `src/store/tests/app_runs::runtime_fixture`.
const WORKFLOW: &str = "---\ntitle: Local\ngoal: R\n---\n## Write\nagent: writer\naction: local.text.produce\n\nWrite.\n\n### Acceptance\n- [ ] ok\n";

/// Build a REAL schema-1 run on `rig`'s daemon store with NO capability
/// injection — a normal `local.text` run through real typed approval,
/// dispatch and a live running turn. The frozen capability `BindingProof` is
/// injected into `snapshot.capabilities` AFTER execution (see
/// `inject_capability_binding`) — a crafted historical snapshot, never an
/// execution or grant bypass: the run is created/approved/dispatched on the
/// REAL schema-1 snapshot, so `approved_digest`/`state`/`app_run_steps`/
/// `messages` are all genuine. Returns `(run_id, step_id, message_id,
/// turn_id)` for the claim+record FKs.
fn seed_schema1_run(rig: &Rig, install: &str) -> (String, String, String, String) {
    let shared = fx::shared(rig);
    let s = &shared.store;
    for (alias, role) in [("lead", "pm"), ("writer", "worker"), ("reviewer", "worker")] {
        // Tolerate a daemon-registered agent with the same alias.
        let _ = s.register_agent(&NewAgent {
            alias,
            provider: "claude",
            endpoint_kind: "managed",
            role,
            cwd: rig.state_dir.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: Some("{\"upstream\":\"lead\"}"),
            team_role: None,
            model_policy: None,
        });
        let _ = s.set_identity(
            alias,
            &crate::adapter::Identity {
                thread_id: "t".into(),
                session_id: "s".into(),
                model: None,
                effort: None,
                pid: 4242,
                endpoint: None,
                generation: Some("g1".into()),
                attach: None,
            },
        );
    }
    let inputs: BTreeMap<String, String> = BTreeMap::new();
    let workflow = LocalWorkflow::parse(WORKFLOW, &inputs).unwrap();
    s.app_capability_decide(install, "sha256:bundle", true)
        .unwrap();
    let run = s
        .app_run_create(LocalRunRequest {
            install_id: install,
            bundle_digest: "sha256:bundle",
            workflow: &workflow,
            inputs: &inputs,
            request_id: "run-req",
            owner_pm: Some("lead"),
            project_link: None,
        })
        .unwrap();
    let run_id = run["id"].as_str().unwrap().to_string();
    // Typed approve + dispatch on the REAL schema-1 snapshot — genuine
    // approved_digest==snapshot_digest and running/dispatched parents.
    s.app_run_decide(
        &run_id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    let dispatched = s.app_run_dispatch(&run_id, "sha256:bundle").unwrap();
    let message_id = dispatched["steps"][0]["message_id"]
        .as_str()
        .unwrap()
        .to_string();
    let step_id = dispatched["steps"][0]["step_id"]
        .as_str()
        .unwrap_or("s1")
        .to_string();
    let Take::Message(taken) = s
        .take_queued_app_proven("writer", Some((&message_id, "sha256:bundle")))
        .unwrap()
    else {
        panic!("app kickoff must be claimed");
    };
    let token = crate::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint("g1");
    s.mark_running(&taken.id, &token).unwrap();
    let msg = s.message(&taken.id).unwrap().unwrap();
    (run_id, step_id, taken.id.clone(), msg.turn_id.unwrap())
}

/// Inject `snapshot.capabilities[slot] = proof` into a COMPLETED real run
/// row — the crafted historical snapshot the cached-return guard reads. The
/// run was created/approved/dispatched/executed on its real schema-1
/// snapshot; this only rewrites `snapshot.capabilities` (a frozen-binding
/// view that could never be produced by a valid capability run), recomputing
/// `snapshot_digest`. NOT an execution or grant bypass — the result row was
/// already written on real parents; the read-side guard is what is under
/// test. Returns the new `snapshot_digest` (informational).
fn inject_capability_binding(rig: &Rig, run_id: &str, slot: &str, proof: &BindingProof) -> String {
    let conn = Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .expect("core db");
    let mut snapshot: Value = serde_json::from_str(
        &conn
            .query_row("SELECT snapshot FROM app_runs WHERE id=?", [run_id], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    snapshot["capabilities"][slot] = serde_json::to_value(proof).unwrap();
    let digest = material_digest(&snapshot);
    conn.execute(
        "UPDATE app_runs SET snapshot=?, snapshot_digest=? WHERE id=?",
        params![snapshot.to_string(), digest, run_id],
    )
    .unwrap();
    digest
}

/// Seed a REAL `app_capability_results` row (nonempty marker + asset bytes)
/// through `app_capability_record` against real parents. The active-turn
/// guard requires a REAL `app_capability_claim` row first (`r.state='running'
/// AND `r.approved_digest=r.snapshot_digest` AND `m.state='running'` — all
/// true after the injected-approval seed above). Returns receipt id.
fn seed_result(
    rig: &Rig,
    run_id: &str,
    step: &str,
    message: &str,
    turn: &str,
    slot: &str,
    binding_digest: &str,
) -> String {
    let shared = fx::shared(rig);
    let id = format!("rcpt-{}", uuid::Uuid::new_v4().simple());
    let input_digest = material_digest(&json!({"handle":"c"}));
    // Real claim row reserves the one paid operation for this run slot — the
    // record's active-turn guard reads it before writing the result.
    // The claim's call_id must equal the result row's id — the Store's
    // pre-call-claim check matches `call_id = <result id>` (line ~344).
    shared
        .store
        .app_capability_claim(crate::store::app_capabilities::AppCapabilityClaim {
            run: run_id,
            step,
            message,
            turn,
            slot,
            request: "req-1",
            binding_digest,
            input_digest: &input_digest,
            call_id: &id,
        })
        .expect("seed capability claim failed");
    let rec = shared
        .store
        .app_capability_record(AppCapabilityRecord {
            id: &id,
            run: run_id,
            step,
            message,
            turn,
            slot,
            request: "req-1",
            binding_digest,
            input_digest: &input_digest,
            result: &json!({"marker": MARKER}),
            asset: Some(("text/plain", b"marker-asset-bytes")),
        })
        .expect("seed result failed");
    rec["id"].as_str().unwrap_or(&id).to_string()
}

fn smtp_proof(connection_id: &str, account: &str) -> BindingProof {
    BindingProof {
        id: format!("proof-{account}"),
        revision: 1,
        digest: format!("bd-{account}"),
        config: json!({
            "provider": "smtp", "account": account, "connection_id": connection_id,
            "connection_kind": "enrolled",
            "mapping": {"capability":"email.send","tool":"email.send","effect":"send","scopes":["email:send"]},
        }),
    }
}
fn builtin_proof(rig: &Rig) -> BindingProof {
    BindingProof {
        id: "proof-builtin".into(),
        revision: 1,
        digest: "bd-builtin".into(),
        config: json!({
            "provider": "local", "account": "local",
            "connection_id": fx::builtin_connection_id(rig),
            "connection_kind": "builtin",
            "mapping": {"capability":"text.publish","tool":"publish","effect":"draft","scopes":["publish"]},
        }),
    }
}

// ============================================================================
// LEGACY/HISTORICAL CONTROL — a non-encrypted frozen binding returns the marker
// ============================================================================

/// A real run whose `snapshot.capabilities` resolves to a NON-encrypted
/// (builtin) target: the operator `app_run_capability_result`/`results`/`asset`
/// return the real stored marker — NO unnecessary re-quote, just the cached
/// read. Baseline proving the guard doesn't break a valid historical read.
#[test]
fn control_cached_return_legacy_returns_marker() {
    let dir = fx::new_root();
    let rig = rig(&dir);
    let (run_id, step, message, turn) = seed_schema1_run(&rig, &rig.install_id);
    let receipt = seed_result(
        &rig,
        &run_id,
        &step,
        &message,
        &turn,
        "source",
        "bd-builtin",
    );
    inject_capability_binding(&rig, &run_id, "source", &builtin_proof(&rig));

    let got = rpc(
        &rig,
        "app_run_capability_result",
        json!({"receipt_id": receipt}),
    )
    .expect("operator result read refused for a non-encrypted binding");
    assert_eq!(
        got["result"]["marker"], MARKER,
        "stored marker not returned"
    );
    let asset = rpc(
        &rig,
        "app_run_capability_asset",
        json!({"receipt_id": receipt}),
    )
    .expect("operator asset read refused for a non-encrypted binding");
    // app_capability_asset returns top-level {receipt_id,media_type,digest,
    // size,base64} — assert real bytes, not just a nonzero size.
    assert!(asset["size"].as_u64().unwrap() > 0, "asset size missing");
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(asset["base64"].as_str().unwrap())
        .expect("asset base64 undecodable");
    assert_eq!(
        decoded, b"marker-asset-bytes",
        "asset payload not the stored marker"
    );
    rig.finish().unwrap();
}

// ============================================================================
// ENCRYPTED — all three operator reads refuse before returning the marker
// ============================================================================

/// A real run whose frozen `snapshot.capabilities.source` names an
/// `encrypted-sqlite-v1:` SMTP record (real enrolled connection on the unlocked
/// vault): `app_run_capability_result`/`results`/`asset` all refuse
/// `unsupported_custody_use` and NEVER return the marker or asset bytes.
#[test]
fn cached_return_encrypted_binding_refuses_no_marker() {
    let dir = fx::new_root();
    let rig = rig(&dir);
    let conn = fx::enroll_smtp(&rig, "enc-cache", A);
    assert!(fx::is_encrypted(&rig, &conn).unwrap());
    let proof = smtp_proof(&conn, "enc-cache");
    let (run_id, step, message, turn) = seed_schema1_run(&rig, &rig.install_id);
    let receipt = seed_result(
        &rig,
        &run_id,
        &step,
        &message,
        &turn,
        "source",
        &proof.digest,
    );
    inject_capability_binding(&rig, &run_id, "source", &proof);

    for (method, params) in [
        ("app_run_capability_result", json!({"receipt_id": receipt})),
        ("app_run_capability_results", json!({"run_id": run_id})),
        ("app_run_capability_asset", json!({"receipt_id": receipt})),
    ] {
        let err = rpc(&rig, method, params).unwrap_err();
        assert_eq!(
            err.code().map(|c| c.to_string()).as_deref(),
            Some(fx::UNSUPPORTED),
            "{method} did not refuse encrypted binding"
        );
        assert!(
            !format!("{err}").contains(MARKER),
            "{method} leaked the marker"
        );
    }
    rig.finish().unwrap();
}

/// Stale/frozen-ID only: provider+account mismatch a credential, but the frozen
/// `connection_id` resolves to the encrypted record — `refuse_encrypted_connection`
/// arm fires. A forged/stale provider+account can't bypass target classification.
#[test]
fn cached_return_frozen_connection_id_only_refuses() {
    let dir = fx::new_root();
    let rig = rig(&dir);
    let conn = fx::enroll_smtp(&rig, "enc-id", A);
    assert!(fx::is_encrypted(&rig, &conn).unwrap());
    let proof = BindingProof {
        id: "proof-mismatch".into(),
        revision: 1,
        digest: "bd-mismatch".into(),
        config: json!({"provider":"smtp","account":"unrelated","connection_id":conn,
            "connection_kind":"enrolled",
            "mapping":{"capability":"email.send","tool":"email.send","effect":"send","scopes":["email:send"]}}),
    };
    let (run_id, step, message, turn) = seed_schema1_run(&rig, &rig.install_id);
    let receipt = seed_result(
        &rig,
        &run_id,
        &step,
        &message,
        &turn,
        "source",
        "bd-mismatch",
    );
    inject_capability_binding(&rig, &run_id, "source", &proof);
    let err = rpc(
        &rig,
        "app_run_capability_result",
        json!({"receipt_id": receipt}),
    )
    .unwrap_err();
    assert_eq!(
        err.code().map(|c| c.to_string()).as_deref(),
        Some(fx::UNSUPPORTED)
    );
    assert!(!format!("{err}").contains(MARKER));
    rig.finish().unwrap();
}

// Mutation red: this path returns stored bytes with NO `load_credential`
// backstop — removing the `refuse_encrypted_binding` loop in the admitted
// operator branch lets the marker return. Asserted by the refusal above; a
// mutation that admits is observed as the marker coming back (the control
// case proves the read path returns it when unguarded). (Plain `//` — a
// trailing `///` doc comment has no item and fails to compile.)
