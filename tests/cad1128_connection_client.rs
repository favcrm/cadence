#![cfg(all(unix, feature = "test-seam"))]
// Drafted real-client actor/CAS controls for the landed CAD-1128
// connection management surface — UNCOMPILED/UNEXECUTED. Mount as a new
// integration test file `tests/cad1128_connection_client.rs` (a sibling of
// cad1126_hosted_smtp.rs). These drive the REAL spawned `serve_with` daemon
// through `client::rpc` under `scoped(Asserted)` — the same operator/agent/
// unproven actor seam cad1126 uses — so an operator-gate or CAS failure is
// a live behavioral result, not a schema or parse assertion.
//
// Each check pairs a working positive control (an operator call that
// succeeds) with the counterexample that must refuse; no test asserts red
// from setup/compile failure. The `Fixture`/`Asserted`/`scoped`/`client`
// imports below are the real ones — nothing is a placeholder.
//
// Surfaces under test (all live over the socket):
//   connection_custody_{initialize,unlock,lock,status}  — daemon/custody_rpc.rs
//   connection_revoke / connection_rotate expected-CAS  — daemon/connections_rpc.rs
//   the CLI preflight shape (expected_identity) is exercised indirectly:
//   the daemon's own CAS is the authority and is what's proven here.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cadence_agent::client;
use cadence_agent::daemon::{self, ServeOptions};
use cadence_agent::store::{NewAgent, Store};
use cadence_agent::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use tempfile::TempDir;

// ---------- fixture: a real spawned daemon + one registered agent ----------

struct Fx {
    root: TempDir,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Fx {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let state = root.path().join("s");
        std::fs::create_dir_all(&state).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let mut opts = ServeOptions {
            provider_env: cadence_agent::adapter::ProviderEnv::refusing_providers(),
            stop: Some(stop.clone()),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        cadence_agent::platform::smtp::attach(&mut opts);
        Self::serve(root, state, stop, opts)
    }
    /// The same fixture with a `token`-shape provider registered —
    /// `agenticos_external` is the only in-tree adapter carrying
    /// `enrollment_shapes:["token"]`. The base URL is never contacted for a
    /// scoped-token enroll; registration is the only requirement.
    fn with_token_provider() -> Self {
        let root = TempDir::new().unwrap();
        let state = root.path().join("s");
        std::fs::create_dir_all(&state).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let mut opts = ServeOptions {
            provider_env: cadence_agent::adapter::ProviderEnv::refusing_providers(),
            stop: Some(stop.clone()),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        cadence_agent::platform::smtp::attach(&mut opts);
        cadence_agent::platform::agenticos_external::register_with_deployment_pin(
            &mut opts,
            "https://api.example.test",
            None,
        )
        .unwrap();
        Self::serve(root, state, stop, opts)
    }
    fn serve(root: TempDir, state: PathBuf, stop: Arc<AtomicBool>, opts: ServeOptions) -> Self {
        // The daemon resolves `pm_dir` from this per-instance provider env
        // during encrypted-config load and Shared startup (CAD-1210 guard),
        // regardless of the verb under test. Keep the fixture off the real
        // home tracker without touching process-global env.
        let pm = root.path().join("pm");
        cadence_agent::issue::Pm::init(&pm).expect("fixture PM init");
        opts.provider_env.set(
            "CADENCE_PM_DIR",
            pm.to_str().expect("fixture PM path encoding"),
        );
        let thread = std::thread::spawn({
            let state = state.clone();
            move || daemon::serve_with(&state, opts).unwrap()
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(2)).is_err()
            || cadence_agent::test_seam::Seam::token_at(&state).is_none()
        {
            assert!(Instant::now() < deadline, "daemon never started");
            std::thread::sleep(Duration::from_millis(50));
        }
        let fx = Self {
            root,
            stop,
            thread: Some(thread),
        };
        fx.team();
        fx
    }
    fn state(&self) -> PathBuf {
        self.root.path().join("s")
    }
    /// One registered agent so `Asserted::Agent` is a real caller, not a
    /// fabricated identity.
    fn team(&self) {
        let store = Store::open(&self.state().join("cadence.sqlite3")).unwrap();
        let cwd = self.root.path().to_str().unwrap();
        store
            .register_agent(&NewAgent {
                alias: "writer",
                provider: "fake",
                endpoint_kind: "managed",
                role: "worker",
                cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
    }
    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let state = self.state();
        scoped(who, || client::rpc(&state, method, params))
    }
    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// An SMTP-shape connection enrollment the operator performs — the record
/// the CAS and actor checks then operate on. `smtp` is the only non-builtin
/// provider the fixture daemon registers (`platform::smtp::attach`); its
/// `enrollment_shapes` is `["smtp"]` and the record lands `custody:"file"`
/// — a legacy eligible target, NOT masked by the encrypted-unsupported
/// policy. A refusing provider env means no real socket is dialled.
fn enroll(fx: &Fx, account: &str) -> String {
    let created = fx.op(
        "connection_create",
        json!({"provider":"smtp","account":account,"shape":"smtp",
               "host":"smtp.example","port":465,"tls_mode":"implicit",
               "username":"u","secret":"app-password","sender":"sender@example.test",
               "sender_name":"S","scopes":["email:send"],
               "accept_same_uid_risk":true}),
    );
    created["connection"]["id"].as_str().unwrap().to_string()
}

/// The live (id, revision) the daemon's `connection_show` reports — the
/// same pair the CLI's `expected_identity` reads once per invocation.
fn live_identity(fx: &Fx, id: &str) -> (String, u64) {
    let shown = fx.op("connection_show", json!({"connection_id": id}));
    let conn = &shown["connection"];
    (
        conn["id"].as_str().unwrap().to_string(),
        conn["revision"].as_u64().unwrap(),
    )
}

// =====================================================================
// 1. ACTOR controls — management verbs are operator-gated over the socket.
// =====================================================================

/// A registered agent and an unproven caller get ZERO management effect on
/// every custody verb and on revoke — no key/store/core effect. The
/// positive control (an operator status) proves the gate admits a real
/// operator so the refusals are actor-specific, not a broken daemon.
#[test]
fn custody_management_verbs_refuse_non_operator() {
    let fx = Fx::new();
    // Positive control: the operator reads status (encrypted may be
    // unconfigured → a `configured:false` status, still a success).
    let status = fx.op("connection_custody_status", json!({}));
    assert!(status["custody"].is_object(), "{status}");

    for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
        for (method, params) in [
            (
                "connection_custody_initialize",
                json!({"accept_same_uid_risk": true}),
            ),
            (
                "connection_custody_unlock",
                json!({"accept_same_uid_risk": true}),
            ),
            ("connection_custody_lock", json!({})),
            ("connection_custody_status", json!({})),
        ] {
            let refusal = fx.rpc(who.clone(), method, params);
            assert!(refusal.is_err(), "{method} must refuse {}", who.as_str());
        }
    }
}

/// An agent/unproven caller cannot revoke or rotate a connection — the
/// operator-gate precedes any credential or core write.
#[test]
fn revoke_and_rotate_refuse_non_operator() {
    let fx = Fx::new();
    let id = enroll(&fx, "victim");
    for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
        for (method, params) in [
            ("connection_revoke", json!({"connection_id": id})),
            (
                "connection_rotate",
                json!({"connection_id": id, "token": "tok-2"}),
            ),
        ] {
            let refusal = fx.rpc(who.clone(), method, params);
            assert!(refusal.is_err(), "{method} must refuse {}", who.as_str());
        }
    }
    // The record is untouched — a refused caller retired nothing.
    let after = fx.op("connection_show", json!({"connection_id": id}));
    assert_eq!(after["connection"]["id"], id);
}

/// A forged operator-assertion inside `params` does not become authority:
/// actor proof comes from the connection, never a payload field. A caller
/// that adds `{"by":"operator"}`/`{"actor":"operator"}` to its params must
/// still be refused — the field is not read as identity.
#[test]
fn forged_actor_field_does_not_grant_management() {
    let fx = Fx::new();
    let id = enroll(&fx, "forged");
    // An agent asserting operator authority via a request field must still
    // fail the real caller gate.
    for forged in [
        json!({"connection_id": id, "by": "operator"}),
        json!({"connection_id": id, "actor": "operator"}),
        json!({"connection_id": id, "caller": "operator"}),
        json!({"connection_id": id, "operator": true}),
    ] {
        let refusal = fx.rpc(
            Asserted::Agent("writer".into()),
            "connection_revoke",
            forged,
        );
        assert!(refusal.is_err(), "forged actor field must not grant revoke");
    }
    // Custody is the same: a forged field is not an operator proof.
    let refusal = fx.rpc(
        Asserted::Agent("writer".into()),
        "connection_custody_initialize",
        json!({"accept_same_uid_risk": true, "actor": "operator"}),
    );
    assert!(
        refusal.is_err(),
        "forged actor field must not grant custody init"
    );
}

// =====================================================================
// 2. CAS controls — expected_connection_id + expected_revision binding.
// =====================================================================

/// The exact expected pair the operator supplies must match the live
/// record; a stale revision refuses `stale_revision` and retires nothing.
/// The test ROTATES first so a genuine stale value exists — at
/// `credential_revision=1` the naive `saturating_sub(1).max(1)` collapses
/// to a match and would revoke. The stale value is the real pre-rotate
/// revision. On a legacy `custody:"file"` record the fields are advisory —
/// but the daemon still runs the CAS, so a WRONG expected value refuses
/// even on legacy. Positive control (separate test): a matching pair
/// succeeds.
#[test]
fn revoke_with_stale_expected_revision_refuses() {
    let fx = Fx::new();
    let id = enroll(&fx, "cas-rev");
    // Bump the revision once (a real rotate) so a genuine STALE value
    // exists — at rev 1 `saturating_sub(1).max(1)` collapses to a MATCH.
    // Capture the pre-rotate pair, rotate, then hand back the OLD value.
    let (expected_id, old_revision) = live_identity(&fx, &id);
    fx.op(
        "connection_rotate",
        json!({"connection_id": expected_id, "secret": "rotated-secret",
               "accept_same_uid_risk": true}),
    );
    let (live_id, live_revision) = live_identity(&fx, &id);
    assert!(
        live_revision > old_revision,
        "rotate must bump the revision before a stale case is meaningful"
    );
    // Now `old_revision` is genuinely behind — the daemon CAS refuses it.
    let refusal = fx.rpc(
        Asserted::Operator,
        "connection_revoke",
        json!({"connection_id": live_id,
               "expected_connection_id": live_id,
               "expected_revision": old_revision}),
    );
    assert!(refusal.is_err(), "a genuinely stale revision must refuse");
    let still = fx.op("connection_show", json!({"connection_id": id}));
    assert_eq!(
        still["connection"]["revision"], live_revision,
        "a refused revoke must not bump or remove the record"
    );
}

/// A matching expected pair revokes cleanly — the CAS is a gate, not a
/// blanket denial. Proves the refusals above are CAS-specific.
#[test]
fn revoke_with_matching_expectation_succeeds() {
    let fx = Fx::new();
    let id = enroll(&fx, "cas-ok");
    let (expected_id, revision) = live_identity(&fx, &id);
    let out = fx.op(
        "connection_revoke",
        json!({"connection_id": expected_id,
               "expected_connection_id": expected_id,
               "expected_revision": revision}),
    );
    assert_eq!(out["revoked"], true, "{out}");
    // A second revoke on the same incarnation is now absent.
    let again = fx.rpc(
        Asserted::Operator,
        "connection_revoke",
        json!({"connection_id": expected_id,
               "expected_connection_id": expected_id,
               "expected_revision": revision}),
    );
    assert!(again.is_err(), "re-revoke must not double-retire");
}

/// A mismatched `expected_connection_id` (old incarnation after re-enroll)
/// refuses — the CAS binds the exact incarnation, not just the account.
#[test]
fn revoke_refuses_mismatched_expected_connection_id() {
    let fx = Fx::new();
    // A real OTHER incarnation's id — the product forwards the explicit
    // `expected_connection_id` into `revoke_inner_expected` (or the current
    // id when omitted), so the forged value must be a plausible real id the
    // CAS can compare, not a sentinel string it might normalize away.
    let other_id = enroll(&fx, "cas-id-other");
    let id = enroll(&fx, "cas-id");
    let (_, revision) = live_identity(&fx, &id);
    let refusal = fx.rpc(
        Asserted::Operator,
        "connection_revoke",
        json!({"connection_id": id,
               "expected_connection_id": other_id,
               "expected_revision": revision}),
    );
    assert!(refusal.is_err(), "mismatched incarnation must refuse");
    let still = fx.op("connection_show", json!({"connection_id": id}));
    assert_eq!(still["connection"]["id"], id);
}

// =====================================================================
// 3. Custody verb semantics — ack required, status is the only read.
// =====================================================================

/// initialize/unlock without `accept_same_uid_risk:true` refuse
/// `custody_unprotected` — an omitted or false ack never enables the key.
/// On an unconfigured daemon initialize/unlock also refuse custody
/// unavailability; the point is the ACK gate is reached/enforced, not a
/// silent enable.
#[test]
fn custody_initialize_unlock_require_explicit_ack() {
    let fx = Fx::new();
    for (method, params) in [
        ("connection_custody_initialize", json!({})), // omitted
        (
            "connection_custody_initialize",
            json!({"accept_same_uid_risk": false}),
        ), // false
        ("connection_custody_unlock", json!({})),
        (
            "connection_custody_unlock",
            json!({"accept_same_uid_risk": false}),
        ),
    ] {
        let refusal = fx.rpc(Asserted::Operator, method, params);
        assert!(
            refusal.is_err(),
            "{method} must refuse without explicit ack"
        );
    }
}

/// `connection_custody_status` is the one non-mutating verb — an operator
/// reads it without an ack and gets a `custody` object; lock is a mutation
/// that succeeds only when custody is configured (unconfigured → refuses,
/// but never silently pretends to lock).
#[test]
fn custody_status_is_operator_readable_without_ack() {
    let fx = Fx::new();
    let status = fx.op("connection_custody_status", json!({}));
    assert!(status["custody"].is_object(), "{status}");
    // Unconfigured daemon: lock must refuse rather than pretend.
    let lock = fx.rpc(Asserted::Operator, "connection_custody_lock", json!({}));
    assert!(lock.is_err(), "lock on unconfigured custody must refuse");
}

/// A caller-supplied secret/key field on any custody verb is rejected —
/// the allowlist carries only `accept_same_uid_risk`, never key material.
#[test]
fn custody_verbs_reject_secret_and_key_fields() {
    let fx = Fx::new();
    for (method, params) in [
        (
            "connection_custody_initialize",
            json!({"accept_same_uid_risk": true, "key_file": "/x"}),
        ),
        (
            "connection_custody_unlock",
            json!({"accept_same_uid_risk": true, "secret": "s"}),
        ),
        ("connection_custody_status", json!({"token": "t"})),
        ("connection_custody_lock", json!({"key": "k"})),
    ] {
        let refusal = fx.rpc(Asserted::Operator, method, params);
        assert!(
            refusal.is_err(),
            "{method} must reject non-allowlisted field"
        );
    }
}

// =====================================================================
// 4. Legacy TOKEN rotate CAS — the writer's `enroll_inner_expected` fix.
// =====================================================================
// A `shape:"token"` record on `agenticos_external` (the only in-tree
// `enrollment_shapes:["token"]` provider). The writer fixed the token
// branch to forward `expected_connection_id`/`expected_revision` into
// `enroll_inner_expected` — before, the supplied pair was ignored and a
// stale caller could rotate the live incarnation. These checks exercise
// the real legacy-token rotate, NOT the encrypted or SMTP paths.

/// Enroll a scoped-token record on the token provider; returns the id.
fn enroll_token(fx: &Fx, account: &str) -> String {
    let created = fx.op(
        "connection_create",
        json!({"provider":"agenticos_external","account":account,"shape":"token",
               "token":"scoped-token-1","scopes":["provider.read"],
               "accept_same_uid_risk":true}),
    );
    created["connection"]["id"].as_str().unwrap().to_string()
}

/// A stale `expected_revision` on a token rotate refuses `stale_revision`
/// and changes NOTHING — the pair is forwarded into `enroll_inner_expected`,
/// not ignored. A matching pair rotates cleanly (the CAS is a gate).
#[test]
fn token_rotate_stale_expected_revision_refuses() {
    let fx = Fx::with_token_provider();
    let id = enroll_token(&fx, "ws_550e8400-e29b-41d4-a716-446655440000");
    let (expected_id, rev) = live_identity(&fx, &id);
    assert!(rev > 0);
    // Stale expected revision — a forged value the live revision does not
    // match. The rotate must refuse before any credential write.
    let refusal = fx.rpc(
        Asserted::Operator,
        "connection_rotate",
        json!({"connection_id": expected_id,
               "token": "scoped-token-2",
               "expected_connection_id": expected_id,
               "expected_revision": rev + 99}),
    );
    assert!(refusal.is_err(), "a forged expected_revision must refuse");
    let still = fx.op("connection_show", json!({"connection_id": id}));
    assert_eq!(
        still["connection"]["revision"].as_u64().unwrap(),
        rev,
        "a refused rotate must not bump or change the record"
    );
}

/// A matching expected pair rotates the token incarnation — the CAS is a
/// gate, not a blanket denial. Proves the refusal above is CAS-specific.
#[test]
fn token_rotate_matching_expectation_succeeds() {
    let fx = Fx::with_token_provider();
    let id = enroll_token(&fx, "ws_550e8400-e29b-41d4-a716-446655440000");
    let (expected_id, rev) = live_identity(&fx, &id);
    let out = fx.op(
        "connection_rotate",
        json!({"connection_id": expected_id,
               "token": "scoped-token-2",
               "expected_connection_id": expected_id,
               "expected_revision": rev}),
    );
    // A successful rotate bumps the revision.
    let still = fx.op("connection_show", json!({"connection_id": id}));
    assert!(
        still["connection"]["revision"].as_u64().unwrap() > rev,
        "matching rotate must bump revision: {out}"
    );
}

/// A stale `expected_connection_id` (a real OTHER incarnation's id) refuses
/// — the CAS binds the exact incarnation, not just the account.
#[test]
fn token_rotate_refuses_mismatched_expected_connection_id() {
    let fx = Fx::with_token_provider();
    let other_id = enroll_token(&fx, "ws_6ba7b810-9dad-11d1-80b4-00c04fd430c8");
    let id = enroll_token(&fx, "ws_550e8400-e29b-41d4-a716-446655440000");
    let (_, rev) = live_identity(&fx, &id);
    let refusal = fx.rpc(
        Asserted::Operator,
        "connection_rotate",
        json!({"connection_id": id,
               "token": "scoped-token-2",
               "expected_connection_id": other_id,
               "expected_revision": rev}),
    );
    assert!(
        refusal.is_err(),
        "a real mismatched incarnation must refuse"
    );
    let still = fx.op("connection_show", json!({"connection_id": id}));
    assert_eq!(still["connection"]["id"], id);
}
