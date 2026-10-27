//! CAD-1128 `execute_effect` CONSUMER proof — the accepted-decision step
//! that `effect_lifecycle.rs`'s Store-only CAS/state tests cannot reach.
//! Mount as a private child of `daemon::effect_rpc` (the module that owns
//! the private `execute_effect`), beside `boundary_floor_tests`:
//!
//!     #[cfg(all(test, unix, feature = "test-seam"))]
//!     #[path = "effect_rpc/cad1128_execute_claim_tests.rs"]
//!     mod cad1128_execute_claim_tests;
//!
//! Independently authored test-only checks (not product implementation,
//! registered code review or operator approval). No production file was
//! edited; the manager mounts this file.
//!
//! WHAT IS REAL HERE
//! - `Rig` (the existing CAD-1128 fixture): real in-process daemon, real
//!   core SQLite, real `Store::effect_stage`/`effect_decide`/`effect_close`
//!   lifecycle, real legacy/encrypted custody. Rows are seeded through the
//!   real Store API, never raw SQL.
//! - `fx` (`cad1128_canary_fx`): real operator `connection_create`
//!   enrollment and the live `Arc<Shared>`.
//! - The injected `smtp` adapter delegates its reviewed table, descriptor,
//!   registration and preview to the real `SmtpAdapter`; only `execute`
//!   and `read_back` are replaced by deterministic in-memory values with
//!   atomic counters, so the tests observe the consumer's callbacks without
//!   network/delivery/credentials. TEST-ONLY and synthetic by design.
//!
//! THE CURRENT CONTRACT (source, `effect_rpc.rs`):
//! `execute_effect` → `refuse_encrypted_target` (early use-site guard,
//! `:795`) → adapter lookup → `load_credential` (default loader, ALWAYS,
//! before any claim) → `Store::effect_executing` (`:864`, the affected-row
//! CAS claim) → `adapter.execute` → `read_back` → `Store::effect_outcome`.
//! The early guard is what makes an encrypted target refuse before the
//! loader; the loader is a second, independent backstop.
//!
//! THREE ORACLES
//! 1. POSITIVE: a valid legacy decided row executes through the real
//!    consumer: `effect_executing` claims it, the counted adapter's
//!    `execute`/`read_back` fire once, and `effect_outcome` records `done`
//!    with the adapter result.
//! 2. CLOSED CLONE: an eligible legacy decided row is saved (`clone`), then
//!    durably closed via `Store::effect_close_decided`. `execute_effect` on
//!    the saved clone refuses `effect_not_claimable` from the affected-row
//!    claim, with NO new `execute`/`read_back`, NO outcome write and the
//!    durable row still `closed`.
//! 3. FRESH ENCRYPTED: a really-enrolled encrypted `(smtp, account)`
//!    decided row refuses `unsupported_custody_use` at the early use-site
//!    guard with zero adapter callbacks, zero claim (`state` stays
//!    `decided`) and no outcome. Baseline oracle only; see the mutation
//!    recipe below.
//!
//! MEANINGFUL MUTATION RECIPES (manager applies/restores; never this file)
//! (a) Closed legacy clone: remove the `if n != 1 { ... effect_not_claimable
//!     ... }` affected-row guard in `Store::effect_executing`
//!     (`src/store/effects.rs:467-472`) but keep the UPDATE + identity
//!     re-read. The closed clone then passes the claim, reaches the counted
//!     adapter (`execute`/`read_back` advance), and `effect_outcome` emits
//!     its event even though its `state='executing'` UPDATE matches nothing.
//!     This test then fails on the counters/durable-state oracle.
//! (b) Fresh encrypted: removing ONLY the early use-site guard makes the
//!     default `platform::load_credential` backstop refuse
//!     `unsupported_custody_use` before any claim — NOT a qualified
//!     traffic/claim/outcome red. The coherent unsupported-policy
//!     counterexample admits the generic purpose at BOTH the early guard
//!     (`effect_rpc.rs:795`) AND the default loader backstop
//!     (`src/platform/mod.rs:690-728`), the latter by a TEMPORARY actual
//!     exact vault read under the UNLOCKED real manager (authentic
//!     AAD/fingerprint checks, no canned bytes, cache or new public
//!     interface), mirroring the r2 policy counterexample. Then the
//!     encrypted decided row claims `executing` and reaches the counted
//!     adapter, and this test fails on callbacks/claim/outcome.

#![cfg(all(unix, feature = "test-seam"))]

use crate::contract_fixture::{ToolTable, Verified};
use crate::daemon::cad1128_canary_fx as fx;
use crate::daemon::custody_acceptance_fixture::Rig;
use crate::daemon::Shared;
use crate::platform::deployments::DeploymentMetadata;
use crate::platform::smtp::SmtpAdapter;
use crate::platform::PlatformAdapter;
use crate::store::{EffectRow, NewAgent};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

/// The rig's synthetic credential halves (only the real TLS peer accepts
/// them; these checks never submit to it).
const A: &str = "cad1128-custody-a";
const B: &str = "cad1128-custody-b";
/// Bound on every paused-clone release/wait/join in the row200 test.
const RESUME_WAIT: Duration = Duration::from_secs(30);

/// TEST-ONLY counted `smtp` adapter: the reviewed table/descriptor/
/// registration/preview are the REAL `SmtpAdapter` values (so the real
/// `connection_create` descriptor/admission path is unchanged); only the
/// execution hooks are deterministic in-memory simulations. No SMTP
/// connection, delivery, credential use or provider admission occurs.
struct CountedExecSmtp {
    inner: SmtpAdapter,
    execute_calls: Arc<AtomicUsize>,
    read_back_calls: Arc<AtomicUsize>,
}

impl PlatformAdapter for CountedExecSmtp {
    fn table(&self) -> &ToolTable {
        self.inner.table()
    }
    fn connection_descriptor(&self) -> Option<crate::platform::connections::ProviderDescriptor> {
        self.inner.connection_descriptor()
    }
    fn connection_registration(&self) -> Option<String> {
        self.inner.connection_registration()
    }
    fn reported_manifest_version(&self) -> Option<String> {
        self.inner.reported_manifest_version()
    }
    fn preview(&self, account: &str, tool: &str, input: &Value) -> String {
        self.inner.preview(account, tool, input)
    }
    fn execute(
        &self,
        _credential: &[u8],
        _tool: &str,
        _input: &Value,
        _idempotency_key: &str,
        _expected_hash: Option<&str>,
    ) -> std::result::Result<Value, String> {
        self.execute_calls.fetch_add(1, Ordering::SeqCst);
        // Deliberately shorter than the real leak screen's 8-byte window and
        // sharing no credential-envelope fragment (the SMTP JSON contains
        // `"username":"fixture-user"`, so even `"fixture` is unsafe).
        Ok(Value::String("zzqq".into()))
    }
    fn read_back(&self, _tool: &str, _input: &Value) -> Verified {
        self.read_back_calls.fetch_add(1, Ordering::SeqCst);
        Verified::True
    }
    fn source_hash(&self, agent: &str, source: &str) -> Option<String> {
        self.inner.source_hash(agent, source)
    }
}

/// `(opts.platforms, isolated deployment metadata, execute counter,
/// read_back counter)` — `start_with_platforms` requires explicit
/// deployments so no host deployment file is read.
fn counted_platforms() -> (
    super::PlatformMap,
    DeploymentMetadata,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let execute_calls = Arc::new(AtomicUsize::new(0));
    let read_back_calls = Arc::new(AtomicUsize::new(0));
    let adapter = CountedExecSmtp {
        inner: SmtpAdapter::default(),
        execute_calls: execute_calls.clone(),
        read_back_calls: read_back_calls.clone(),
    };
    let mut platforms = super::PlatformMap::new();
    platforms.insert("smtp".into(), Arc::new(adapter) as Arc<dyn PlatformAdapter>);
    let deployments =
        DeploymentMetadata::parse(br#"{"schema":1,"providers":[]}"#).expect("empty deployments");
    (platforms, deployments, execute_calls, read_back_calls)
}

fn start(
    root: &Path,
    encrypted: bool,
    platforms: super::PlatformMap,
    deployments: DeploymentMetadata,
) -> Rig {
    Rig::start_with_platforms(
        root,
        A.as_bytes(),
        B.as_bytes(),
        encrypted,
        platforms,
        Some(deployments),
    )
    .expect("rig start_with_platforms failed")
}

/// Register the row's requester and give it the exact staged scope on the
/// enrolled account (`platform_grant_add` requires the credential row).
fn register_with_grant(shared: &Shared, rig: &Rig, alias: &str, account: &str) {
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
    shared
        .store
        .platform_grant_add(
            alias,
            "smtp",
            account,
            &["email:send".to_string()],
            "operator",
        )
        .expect("grant failed");
}

/// Stage a real `waiting` row through `Store::effect_stage` and press it
/// through the real `Store::effect_decide` — the returned row is `decided`,
/// the exact input `execute_effect` consumes.
fn decided_row(shared: &Shared, agent: &str, account: &str, request: &str) -> EffectRow {
    let row = EffectRow {
        effect_id: format!("eff-{}", uuid::Uuid::new_v4().simple()),
        request: request.to_string(),
        agent: agent.to_string(),
        platform: "smtp".to_string(),
        account: account.to_string(),
        tool: "email.send".to_string(),
        label: None,
        input: json!({"fixture": "cad1128-execute-claim"}),
        input_summary: "send".to_string(),
        preview: "fixture preview".to_string(),
        source_name: None,
        source_hash: None,
        scopes: vec!["email:send".to_string()],
        task: None,
        state: "waiting".to_string(),
        close_reason: None,
        decision: None,
        outcome: None,
        needs_you: false,
        staged_at: 0.0,
        updated_at: 0.0,
    };
    let (staged, _) = shared
        .store
        .effect_stage(&row)
        .expect("effect_stage failed");
    assert_eq!(staged.state, "waiting");
    let decided = shared
        .store
        .effect_decide(
            request,
            true,
            &json!({"by": {"member": "operator", "role": "operator",
                "rule": "cad1128-fixture"}, "at": 0.0}),
        )
        .expect("effect_decide failed")
        .expect("decided row missing");
    assert_eq!(decided.state, "decided");
    decided
}

/// Full durable row snapshot. `to_record` is the shared §5.4 projection and
/// omits `scopes`/`task`/`source_name`/timestamps/`needs_you` plus absent
/// optional keys; combining it with the actual row fields keeps the
/// unchanged-row oracle complete without a production or shared-schema change.
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

#[test]
fn legacy_decided_effect_executes_and_a_closed_clone_refuses_claim() {
    let dir = fx::new_root();
    let (platforms, deployments, execute_calls, read_back_calls) = counted_platforms();
    let rig = start(dir.path(), false, platforms, deployments);
    let conn = fx::enroll_smtp(&rig, "legacy-exec", A);
    assert!(
        !fx::is_encrypted(&rig, &conn).expect("custody tag read"),
        "the legacy positive must not enroll encrypted custody"
    );
    let shared = fx::shared(&rig);
    register_with_grant(&shared, &rig, "agent-less", "legacy-exec");

    // POSITIVE: the decided row is claimed and its adapter callbacks run.
    let open = decided_row(&shared, "agent-less", "legacy-exec", "req-cad1128-open");
    let done = shared
        .execute_effect(&open)
        .expect("a valid legacy decided row must execute");
    assert_eq!(done.state, "done");
    let outcome = done.outcome.as_ref().expect("executed row has no outcome");
    assert!(
        outcome.get("result").is_some(),
        "the outcome did not record an adapter result"
    );
    assert_eq!(outcome["verified"], true);
    assert_eq!(execute_calls.load(Ordering::SeqCst), 1);
    assert_eq!(read_back_calls.load(Ordering::SeqCst), 1);

    // CLOSED CLONE: save the eligible decided row, close it durably, then
    // execute the saved clone. The affected-row claim must refuse, with no
    // new callback and no outcome.
    let closed_source = decided_row(&shared, "agent-less", "legacy-exec", "req-cad1128-closed");
    let saved_clone = closed_source.clone();
    let _closed = shared
        .store
        .effect_close_decided(&saved_clone.effect_id, "cad1128_fixture_closed")
        .expect("effect_close_decided failed")
        .expect("decided row was not closeable");
    // The helper's returned row is an incidental projection; establish the
    // CLOSED precondition only from the authoritative durable row.
    let durable_closed = shared
        .store
        .effect_by_id(&saved_clone.effect_id)
        .expect("closed row read failed")
        .expect("closed row vanished");
    assert_eq!(
        durable_closed.state, "closed",
        "the durable row did not close"
    );
    assert_eq!(
        durable_closed.close_reason.as_deref(),
        Some("cad1128_fixture_closed")
    );
    assert!(durable_closed.outcome.is_none(), "closing wrote an outcome");
    let result = shared.execute_effect(&saved_clone);
    assert_eq!(
        execute_calls.load(Ordering::SeqCst),
        1,
        "a closed clone reached the adapter execute callback"
    );
    assert_eq!(
        read_back_calls.load(Ordering::SeqCst),
        1,
        "a closed clone reached the adapter read_back callback"
    );
    let durable = shared
        .store
        .effect_by_id(&saved_clone.effect_id)
        .expect("closed row read failed")
        .expect("closed row vanished");
    assert_eq!(durable.state, "closed", "the closed row changed state");
    assert!(durable.outcome.is_none(), "a closed clone wrote an outcome");
    let error = result.expect_err("a durably closed decided row must not be claimable");
    assert_eq!(
        error.code(),
        Some("effect_not_claimable"),
        "closed clone refused with an unexpected error: {error}"
    );
    drop(shared);
    rig.finish().expect("rig teardown failed");
}

#[test]
fn encrypted_decided_row_closed_by_native_revoke_cannot_resume_after_reenroll_rotation_or_restart()
{
    // r2 row200 (contract raw 200 and 83-89). The working legacy happy
    // control is the first test in this file; this combined case is
    // encrypted-only. The Store seed is the explicit historical decided-row
    // precondition, never supported encrypted staging or a migration.
    let dir = fx::new_root();
    let root = dir.path();
    let account = "exec-acct";
    let agent = "agent-row200";
    let (platforms, deployments, execute_calls, read_back_calls) = counted_platforms();
    let rig = start(root, true, platforms, deployments);
    fx::unlock_vault(&rig);
    let conn = fx::enroll_smtp(&rig, account, A);
    assert!(fx::is_encrypted(&rig, &conn).expect("custody tag read"));
    let shared = fx::shared(&rig);
    register_with_grant(&shared, &rig, agent, account);
    let granted = fx::op(
        &rig,
        "platform_grant",
        json!({"agent": agent, "platform": "smtp", "account": account,
            "scopes": ["email:send"]}),
    );
    assert_eq!(granted["state"], "granted");
    assert!(shared
        .store
        .platform_grant(agent, "smtp", account)
        .expect("grant read failed")
        .is_some_and(|grant| grant.covers("email:send")));

    // State 1: decided encrypted historical row; its accepted clone is
    // paused at the real execute entry before the native revoke.
    let historical = decided_row(&shared, agent, account, "req-row200-revoked");
    let old_clone = historical.clone();
    let sessions = rig.smtp_barrier().expect("smtp barrier").tcp_sessions;
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel::<std::result::Result<(), Option<String>>>();
    let worker_shared = Arc::clone(&shared);
    let worker_clone = old_clone.clone();
    let (new_conn, closed_before_resume, resumed) = std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            if release_rx.recv_timeout(RESUME_WAIT).is_err() {
                let _ = done_tx.send(Err(Some("paused-clone release timeout".to_string())));
                return;
            }
            let observed = worker_shared
                .execute_effect(&worker_clone)
                .map(|_| ())
                .map_err(|error| error.code().map(str::to_string));
            let _ = done_tx.send(observed);
        });
        // ACTUAL native encrypted revoke while the accepted clone is paused.
        let revoked = fx::op(
            &rig,
            "connection_revoke",
            json!({"connection_id": &conn, "expected_connection_id": &conn,
                "expected_revision": 1}),
        );
        assert_eq!(revoked["connection_id"].as_str(), Some(conn.as_str()));
        assert_eq!(revoked["revoked"], true);
        let closed = snapshot(
            &shared
                .store
                .effect_by_id(&historical.effect_id)
                .expect("row read failed")
                .expect("historical row vanished"),
        );
        assert_eq!(closed["state"], "closed");
        assert_eq!(closed["close_reason"], "credential_revoked");
        assert_eq!(closed["outcome"], Value::Null);
        assert!(
            shared
                .store
                .platform_grant(agent, "smtp", account)
                .expect("grant read failed")
                .is_none(),
            "encrypted revoke left a grant behind"
        );

        // Explicit same-account encrypted re-enroll and REAL regrant.
        let new_conn = fx::enroll_smtp(&rig, account, A);
        assert!(
            fx::is_encrypted(&rig, &new_conn).expect("custody tag read"),
            "fresh same-account incarnation is not encrypted"
        );
        let regranted = fx::op(
            &rig,
            "platform_grant",
            json!({"agent": agent, "platform": "smtp", "account": account,
                "scopes": ["email:send"]}),
        );
        assert_eq!(regranted["state"], "granted");
        assert!(shared
            .store
            .platform_grant(agent, "smtp", account)
            .expect("grant read failed")
            .is_some_and(|grant| grant.covers("email:send")));

        release_tx.send(()).expect("release paused clone");
        let observed = done_rx
            .recv_timeout(RESUME_WAIT)
            .expect("paused clone did not resume within bound");
        if worker.join().is_err() {
            panic!("paused clone worker panicked");
        }
        (new_conn, closed, observed)
    });
    assert_eq!(
        execute_calls.load(Ordering::SeqCst),
        0,
        "resumed closed clone reached the adapter execute callback"
    );
    assert_eq!(
        read_back_calls.load(Ordering::SeqCst),
        0,
        "resumed closed clone reached the adapter read_back callback"
    );
    assert_eq!(
        rig.smtp_barrier().expect("smtp barrier").tcp_sessions,
        sessions,
        "resumed closed clone opened provider traffic"
    );
    assert_eq!(
        snapshot(
            &shared
                .store
                .effect_by_id(&old_clone.effect_id)
                .expect("row read failed")
                .expect("closed row vanished")
        ),
        closed_before_resume,
        "resumed closed clone changed the durable row"
    );
    let code = resumed.expect_err("a durably closed decided clone resumed with success");
    assert!(
        matches!(
            code.as_deref(),
            Some("unsupported_custody_use") | Some("effect_not_claimable")
        ),
        "resumed closed clone refused with an unexpected code: {code:?}"
    );

    // State 2: seed+retain the PRE-ROTATION decided clone, then rotate.
    let rotated_hist = decided_row(&shared, agent, account, "req-row200-rotated");
    let rotated_clone = rotated_hist.clone();
    let rotated_before = snapshot(&rotated_hist);
    let rotated = fx::op(
        &rig,
        "connection_rotate",
        json!({"connection_id": &new_conn, "expected_connection_id": &new_conn,
            "expected_revision": 1, "secret": B}),
    );
    assert_eq!(
        rotated["connection"]["id"].as_str(),
        Some(new_conn.as_str())
    );
    let rotated_record = shared
        .store
        .connection_credential(&new_conn)
        .expect("core read failed")
        .expect("rotated record");
    assert_eq!(rotated_record.credential_revision, 2);
    assert!(rotated_record.custody.starts_with("encrypted-sqlite-v1:"));
    assert!(shared
        .store
        .platform_grant(agent, "smtp", account)
        .expect("grant read failed")
        .is_some_and(|grant| grant.covers("email:send")));
    let sessions = rig.smtp_barrier().expect("smtp barrier").tcp_sessions;
    let result = shared.execute_effect(&rotated_clone);
    assert_eq!(execute_calls.load(Ordering::SeqCst), 0);
    assert_eq!(read_back_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        rig.smtp_barrier().expect("smtp barrier").tcp_sessions,
        sessions,
        "pre-rotation clone opened provider traffic"
    );
    assert_eq!(
        snapshot(
            &shared
                .store
                .effect_by_id(&rotated_clone.effect_id)
                .expect("row read failed")
                .expect("rotated clone row vanished")
        ),
        rotated_before,
        "pre-rotation clone changed under rotation"
    );
    let error = result.expect_err("a pre-rotation clone executed after rotation");
    assert!(
        matches!(
            error.code(),
            Some("unsupported_custody_use") | Some("effect_not_claimable")
        ),
        "pre-rotation clone refused with an unexpected code: {error}"
    );

    // State 3: seed the historical clone BEFORE the encrypted restart.
    let restart_hist = decided_row(&shared, agent, account, "req-row200-restart");
    let restart_clone = restart_hist.clone();
    let closed_after_rotation = snapshot(
        &shared
            .store
            .effect_by_id(&old_clone.effect_id)
            .expect("row read failed")
            .expect("closed row vanished"),
    );
    let key_path = root.join("keys").join("key");
    let key_before = std::fs::read(&key_path).expect("fixture key bytes");
    let config_path = root
        .join("state")
        .join(crate::platform::encrypted_config::RECORD);
    let config_before = std::fs::read(&config_path).expect("encrypted config bytes");
    let workspace_before = shared
        .store
        .connection_workspace_id()
        .expect("workspace read failed");
    assert!(
        root.join("state").join("vault").is_dir(),
        "encrypted vault root vanished"
    );
    drop(shared);

    let (platforms, deployments, execute_calls, read_back_calls) = counted_platforms();
    let rig = rig
        .restart_encrypted_with_platforms(A.as_bytes(), B.as_bytes(), platforms, Some(deployments))
        .expect("encrypted restart failed");
    assert_eq!(
        std::fs::read(&key_path).expect("fixture key bytes"),
        key_before,
        "encrypted restart replaced the fixture key"
    );
    assert_eq!(
        std::fs::read(&config_path).expect("encrypted config bytes"),
        config_before,
        "encrypted restart rewrote the path-only config"
    );
    let shared = fx::shared(&rig);
    assert_eq!(
        shared
            .store
            .connection_workspace_id()
            .expect("workspace read failed"),
        workspace_before,
        "restart changed the workspace"
    );
    let status = fx::op(&rig, "connection_custody_status", json!({}));
    assert_eq!(
        status["custody"]["unlocked"], false,
        "restart session was not locked"
    );
    assert_eq!(
        snapshot(
            &shared
                .store
                .effect_by_id(&old_clone.effect_id)
                .expect("row read failed")
                .expect("closed row vanished across restart")
        ),
        closed_after_rotation,
        "restart changed the revoke-closed row"
    );
    let unlocked = fx::op(
        &rig,
        "connection_custody_unlock",
        json!({"accept_same_uid_risk": true}),
    );
    assert_eq!(unlocked["custody"]["unlocked"], true);
    assert!(shared
        .store
        .platform_grant(agent, "smtp", account)
        .expect("grant read failed")
        .is_some_and(|grant| grant.covers("email:send")));
    let retained = snapshot(
        &shared
            .store
            .effect_by_id(&restart_clone.effect_id)
            .expect("row read failed")
            .expect("restart historical row vanished"),
    );
    // Real startup recovery may reconcile an unexecuted decided row; it must
    // never close or erase it. Keep decision/outcome invariants.
    assert_ne!(
        retained["state"].as_str(),
        Some("closed"),
        "restart closed the historical unexecuted row"
    );
    assert!(
        matches!(
            retained["state"].as_str(),
            Some("decided") | Some("reconcile")
        ),
        "restart produced an unexpected historical state: {retained}"
    );
    assert!(
        retained["decision"].is_object(),
        "restart erased the accepted decision"
    );
    assert_eq!(retained["outcome"], Value::Null);
    let sessions = rig.smtp_barrier().expect("smtp barrier").tcp_sessions;
    let result = shared.execute_effect(&restart_clone);
    assert_eq!(
        execute_calls.load(Ordering::SeqCst),
        0,
        "post-restart clone reached the adapter execute callback"
    );
    assert_eq!(
        read_back_calls.load(Ordering::SeqCst),
        0,
        "post-restart clone reached the adapter read_back callback"
    );
    assert_eq!(
        rig.smtp_barrier().expect("smtp barrier").tcp_sessions,
        sessions,
        "post-restart clone opened provider traffic"
    );
    assert_eq!(
        snapshot(
            &shared
                .store
                .effect_by_id(&restart_clone.effect_id)
                .expect("row read failed")
                .expect("restart historical row vanished")
        ),
        retained,
        "post-restart clone was claimed or changed"
    );
    let error = result.expect_err("a historical clone executed after restart");
    assert!(
        matches!(
            error.code(),
            Some("unsupported_custody_use") | Some("effect_not_claimable")
        ),
        "post-restart clone refused with an unexpected code: {error}"
    );
    drop(shared);
    rig.finish().expect("rig teardown failed");
}

#[test]
fn fresh_encrypted_decided_effect_refuses_before_callback_claim_outcome() {
    let dir = fx::new_root();
    let (platforms, deployments, execute_calls, read_back_calls) = counted_platforms();
    let rig = start(dir.path(), true, platforms, deployments);
    fx::unlock_vault(&rig);
    let conn = fx::enroll_smtp(&rig, "enc-exec", A);
    assert!(
        fx::is_encrypted(&rig, &conn).expect("custody tag read"),
        "the encrypted target was not enrolled into encrypted custody"
    );
    let shared = fx::shared(&rig);
    register_with_grant(&shared, &rig, "agent-enc", "enc-exec");

    let decided = decided_row(&shared, "agent-enc", "enc-exec", "req-cad1128-encrypted");
    let result = shared.execute_effect(&decided);
    assert_eq!(
        execute_calls.load(Ordering::SeqCst),
        0,
        "the encrypted target reached the adapter execute callback"
    );
    assert_eq!(
        read_back_calls.load(Ordering::SeqCst),
        0,
        "the encrypted target reached the adapter read_back callback"
    );
    let durable = shared
        .store
        .effect_by_id(&decided.effect_id)
        .expect("encrypted row read failed")
        .expect("encrypted row vanished");
    assert_eq!(
        durable.state, "decided",
        "the encrypted target was claimed (state left decided)"
    );
    assert!(
        durable.outcome.is_none(),
        "the encrypted target wrote an outcome"
    );
    let error = result.expect_err("an encrypted decided target must not execute");
    assert_eq!(
        error.code(),
        Some(fx::UNSUPPORTED),
        "encrypted execute refused with an unexpected error: {error}"
    );
    drop(shared);
    rig.finish().expect("rig teardown failed");
}
