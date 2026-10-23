//! CAD-1778 recording-rule acceptance, independently authored by cc13-rev-sol.
//! Register as a child of cad1258_worker_tests: reuse its real enrolled Unix
//! endpoint/turn fixture, not an asserted worker or a direct recording call.
//! Release setup is adapted from the independent CAD-1258 fixture. Only the
//! provider boundary and private fixture storage are fault-injected; dispatch,
//! claim, lock-2 validation and failure recording are the production paths.
#![cfg(all(test, unix, feature = "test-seam"))]

use super::{setup, HostFx};
use crate::contract_fixture::{ToolTable, Verified};
use crate::daemon::{ServeOptions, Shared};
use crate::platform::connections::ProviderDescriptor;
use crate::platform::smtp::test_artifact_adapter::SmtpArtifactAdapter;
use crate::platform::{
    AppArtifactError, AppCapabilityError, AppCapabilityOutput, AppCapabilityQuote, PlatformAdapter,
};
use crate::store::StoreConn;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// Synthetic, deliberately unrelated to public fixture IDs (8-char screening).
const SMTP_SECRET: &str = "S9xQ2pW7nR4vK8dH6cT3mB5f";
const MEDIA_SECRET: &str = "synthetic-cad1365-gen-token";
const WRITE_CAUSE: &str = "cad1778 receipt storage unavailable";

struct Hook;
impl Drop for Hook {
    fn drop(&mut self) {
        crate::issue::lockseam::set_hook(None);
    }
}

/// Same adapter metadata, quote and successful execution as the live loopback
/// fixture. Select an explicit provider error only AFTER admission at the
/// existing provider-call seam. Implement both ABIs so the IDENTICAL test also
/// exercises main's lossy String ABI, rather than failing for fixture reasons.
struct ProviderOutcome {
    inner: Arc<dyn PlatformAdapter>,
    mode: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
}
impl ProviderOutcome {
    fn inject(fx: &mut HostFx) -> (Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let mode = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let platforms = &mut Arc::get_mut(&mut fx.shared).unwrap().platforms;
        let key = crate::platform::agenticos_external::PLATFORM;
        let inner = platforms.get(key).unwrap().clone();
        platforms.insert(
            key.into(),
            Arc::new(Self {
                inner,
                mode: mode.clone(),
                calls: calls.clone(),
            }),
        );
        (mode, calls)
    }
}
impl PlatformAdapter for ProviderOutcome {
    fn table(&self) -> &ToolTable {
        self.inner.table()
    }
    fn connection_descriptor(&self) -> Option<ProviderDescriptor> {
        self.inner.connection_descriptor()
    }
    fn connection_registration(&self) -> Option<String> {
        self.inner.connection_registration()
    }
    fn reported_manifest_version(&self) -> Option<String> {
        self.inner.reported_manifest_version()
    }
    fn direct_call(&self, tool: &str) -> bool {
        self.inner.direct_call(tool)
    }
    fn preview(&self, account: &str, tool: &str, input: &Value) -> String {
        self.inner.preview(account, tool, input)
    }
    fn execute(
        &self,
        _credential: &[u8],
        _tool: &str,
        _input: &Value,
        _key: &str,
        _hash: Option<&str>,
    ) -> Result<Value, String> {
        panic!("run-bound execution must not use generic execute")
    }
    fn quote_app_capability(
        &self,
        credential: &[u8],
        binding: &Value,
    ) -> Result<AppCapabilityQuote, String> {
        self.inner.quote_app_capability(credential, binding)
    }
    fn execute_app_capability(
        &self,
        credential: &[u8],
        authority: &Value,
        input: &Value,
        key: &str,
    ) -> Result<AppCapabilityOutput, String> {
        self.execute_app_capability_outcome(credential, authority, input, key)
            .map_err(|error| error.reason().to_owned())
    }
    fn execute_app_capability_outcome(
        &self,
        credential: &[u8],
        authority: &Value,
        input: &Value,
        key: &str,
    ) -> Result<AppCapabilityOutput, AppCapabilityError> {
        assert_eq!(credential, MEDIA_SECRET.as_bytes());
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mode = self.mode.load(Ordering::SeqCst);
        let reason = if mode >= 3 {
            format!("provider diagnostic contains {MEDIA_SECRET}")
        } else {
            "cad1778 provider rejected or lost its acknowledgement".into()
        };
        match mode {
            1 | 3 => Err(AppCapabilityError::Refused(reason)),
            2 | 4 => Err(AppCapabilityError::Uncertain(reason)),
            0 => self
                .inner
                .execute_app_capability_outcome(credential, authority, input, key),
            _ => panic!("unknown fixture outcome"),
        }
    }
    fn read_back(&self, tool: &str, input: &Value) -> Verified {
        self.inner.read_back(tool, input)
    }
    fn source_hash(&self, agent: &str, source: &str) -> Option<String> {
        self.inner.source_hash(agent, source)
    }
}

#[derive(Clone, Copy, Debug)]
enum Fault {
    Write,
    SecretWrite,
    Authority,
    ConflictingReceipt,
    SnapshotRead,
}

fn clean(reason: &str) {
    for secret in [SMTP_SECRET, MEDIA_SECRET] {
        // Match the production promise of withholding even 8-character pieces.
        for fragment in secret.as_bytes().windows(8) {
            assert!(
                !reason.contains(std::str::from_utf8(fragment).unwrap()),
                "recorded reason contains credential text (withheld from assertion)"
            );
        }
    }
}

fn check_reason(reason: &str, fault: Fault) {
    clean(reason);
    assert!(!reason.is_empty(), "failure reason must be durable");
    match fault {
        Fault::Authority => assert!(
            reason.contains("App authority changed"),
            "a real authority change must retain the requested authority-change message: {reason}"
        ),
        Fault::Write => {
            assert!(
                reason.contains(WRITE_CAUSE),
                "storage failure reason must name the actual storage cause: {reason}"
            );
            assert!(!reason.to_lowercase().contains("authority changed"));
        }
        Fault::SecretWrite => {
            assert!(!reason.to_lowercase().contains("authority changed"));
            assert!(
                reason.contains("withheld") || reason.contains("retained"),
                "screened reason must still identify retention failure"
            );
        }
        Fault::ConflictingReceipt => {
            assert!(
                reason.contains("already has its one result"),
                "an existing-receipt conflict is not an authority change: {reason}"
            );
            assert!(!reason.to_lowercase().contains("authority changed"));
        }
        Fault::SnapshotRead => {
            assert!(
                reason.contains("snapshot") || reason.contains("expected value"),
                "snapshot read failure must name that cause: {reason}"
            );
            assert!(!reason.to_lowercase().contains("authority changed"));
        }
    }
}

/// Install a storage fault after the actual provider call. Fixture writes use
/// the existing sealed test-only owner surface: no production authorizer or
/// foreign-key constraint is disabled. Abort ONLY the success receipt write;
/// the fallback failure transaction must remain writable.
fn record_hook(
    shared: Arc<Shared>,
    install: String,
    bundle: String,
    run: String,
    point: &'static str,
    fault: Fault,
    release: bool,
) -> (Hook, Rc<Cell<usize>>) {
    let seen = Rc::new(Cell::new(0));
    let count = seen.clone();
    crate::issue::lockseam::set_hook(Some(Box::new(move |at| {
        if at != point {
            return;
        }
        count.set(count.get() + 1);
        assert_eq!(count.get(), 1);
        let pm = crate::issue::Pm::at(&shared.pm_dir().unwrap()).unwrap();
        assert!(pm.try_lock().unwrap().is_some(), "seam must be outside PM");
        match fault {
            Fault::Authority => {
                let target = shared.clone();
                let params = json!({"install_id": install, "digest": bundle});
                std::thread::spawn(move || {
                    scoped(Asserted::Operator, || {
                        target.dispatch("app_local_install_revoke", &params, std::process::id())
                    })
                    .expect("real concurrent revoke must complete before lock 2");
                })
                .join()
                .unwrap();
            }
            Fault::Write | Fault::SecretWrite => {
                let cause = if matches!(fault, Fault::SecretWrite) {
                    if release {
                        SMTP_SECRET
                    } else {
                        MEDIA_SECRET
                    }
                } else {
                    WRITE_CAUSE
                };
                let sql = if release {
                    format!("CREATE TRIGGER cad1778_write_failure BEFORE UPDATE OF outcome ON platform_effects WHEN json_extract(NEW.outcome,'$.kind')='released' BEGIN SELECT RAISE(ABORT,'{cause}'); END;")
                } else {
                    format!("CREATE TRIGGER cad1778_write_failure BEFORE INSERT ON app_capability_results BEGIN SELECT RAISE(ABORT,'{cause}'); END;")
                };
                shared
                    .store
                    .fixture_write(|c| {
                        c.execute_batch(&sql)?;
                        Ok(())
                    })
                    .unwrap();
            }
            Fault::ConflictingReceipt => {
                shared.store.fixture_write(|c| {
                    c.execute("INSERT INTO app_capability_results(id,run_id,step_id,message_id,turn_id,slot,request_id,binding_digest,input_digest,result,result_digest,created,receipt_schema)
                        SELECT 'cad1778-old-receipt',c.run_id,c.step_id,s.message_id,m.turn_id,c.slot,'different-previous-request',c.binding_digest,c.input_digest,'{}','fixture-old-digest',0,2
                        FROM app_capability_claims c JOIN app_run_steps s ON s.run_id=c.run_id AND s.step_id=c.step_id JOIN messages m ON m.id=s.message_id WHERE c.run_id=?", [&run])?;
                    Ok(())
                }).unwrap();
            }
            Fault::SnapshotRead => {
                // Equivalent to the brief's missing run row, but retain FK
                // integrity and raw run/step rows so terminality is observable.
                shared.store.fixture_write(|c| {
                    assert_eq!(c.execute("UPDATE app_runs SET snapshot='cad1778 malformed snapshot' WHERE id=?", [&run])?, 1);
                    Ok(())
                }).unwrap();
            }
        }
    })));
    (Hook, seen)
}

fn check_run(shared: &Shared, run: &str, fault: Option<Fault>, expected_kind: &str) {
    // Raw durable rows, deliberately NOT app_run_show: that is precisely the
    // read which can fail in the after-entered regression case.
    let (state, steps, failures, receipts) = shared
        .store
        .read_tx(|c| {
            let state: String =
                c.query_row("SELECT state FROM app_runs WHERE id=?", [run], |r| r.get(0))?;
            let steps: Vec<String> = c.query_vec(
                "SELECT state FROM app_run_steps WHERE run_id=?",
                [run],
                |r| r.get(0),
            )?;
            let failures: Vec<(String, String)> = c.query_vec(
                "SELECT kind,reason FROM app_run_failures WHERE run_id=?",
                [run],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let receipts: i64 = c.query_row(
                "SELECT count(*) FROM app_capability_results WHERE run_id=?",
                [run],
                |r| r.get(0),
            )?;
            Ok((state, steps, failures, receipts))
        })
        .unwrap();
    for (_, reason) in &failures {
        clean(reason);
    }
    println!(
        "CAD1778 durable run={state} steps={steps:?} failures={} receipts={receipts}",
        failures.len()
    );
    assert_eq!(state, "failed", "must not remain running or record success");
    assert!(!steps.is_empty());
    assert_eq!(
        failures.len(),
        1,
        "one durable failure, never a silent drop"
    );
    assert_eq!(failures[0].0, expected_kind);
    if let Some(fault) = fault {
        check_reason(&failures[0].1, fault);
    } else {
        assert!(!failures[0].1.is_empty());
    }
    if fault.is_some_and(|fault| !matches!(fault, Fault::Authority)) {
        assert!(
            steps.iter().all(|s| s == "failed"),
            "record failure must not leave a dispatched step"
        );
    }
    let old_receipts = i64::from(matches!(fault, Some(Fault::ConflictingReceipt)));
    assert_eq!(receipts, old_receipts, "no new success receipt");
    let (catalog, _) = shared.store.app_media_catalog(20, None).unwrap();
    assert!(
        catalog.is_empty(),
        "failed provider work must not enter the catalog"
    );
}

fn worker_case(fault: Option<Fault>, provider_mode: usize) {
    let mut s = setup();
    let (mode, calls) = ProviderOutcome::inject(&mut s.fx);
    let body = json!({"message": s.message, "token": s.token, "slot": "image",
        "request_id": "cad1778-worker", "input": {}});
    let (hook, seen) = if let Some(fault) = fault {
        record_hook(
            s.fx.shared.clone(),
            s.install.clone(),
            s.bundle.clone(),
            s.run_id.clone(),
            "app-capability-before-record",
            fault,
            false,
        )
    } else {
        let seen = Rc::new(Cell::new(0));
        let count = seen.clone();
        let shared = s.fx.shared.clone();
        crate::issue::lockseam::set_hook(Some(Box::new(move |at| {
            if at == "app-capability-provider-call" {
                count.set(count.get() + 1);
                let pm = crate::issue::Pm::at(&shared.pm_dir().unwrap()).unwrap();
                assert!(pm.try_lock().unwrap().is_some());
                mode.store(provider_mode, Ordering::SeqCst);
            }
        })));
        (Hook, seen)
    };
    let result = s.worker.call(s.fx.shared.clone(), &body);
    drop(hook);
    assert_eq!(seen.get(), 1, "must traverse the actual requested seam");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(result.is_err(), "no success RPC in a failure case");
    let kind = if matches!(provider_mode, 1 | 3) {
        "refused"
    } else {
        "uncertain"
    };
    check_run(&s.fx.shared, &s.run_id, fault, kind);
    let _retry = s.worker.call(s.fx.shared.clone(), &body);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "retained claim prevents resend"
    );
}

fn host_case(fault: Fault) {
    let mut fx = HostFx::new();
    let (_, calls) = ProviderOutcome::inject(&mut fx);
    let created = fx.create("cad1778-host").unwrap();
    let run = created["id"].as_str().unwrap().to_owned();
    let bundle = created["snapshot"]["bundle_digest"]
        .as_str()
        .unwrap()
        .to_owned();
    let (hook, seen) = record_hook(
        fx.shared.clone(),
        fx.install.clone(),
        bundle,
        run.clone(),
        "app-host-before-record",
        fault,
        false,
    );
    let result = fx.approve_dispatch(&created);
    drop(hook);
    assert_eq!(seen.get(), 1, "must reach host lock 2");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "provider really executed");
    assert!(result.is_err(), "no success RPC in a failure case");
    check_run(&fx.shared, &run, Some(fault), "uncertain");
}

// Minimal real SMTP release driver, using the reviewed descriptor and native
// run/produce/review/stage/decide chain. No generic effect or fake authority.
struct ReleaseFx {
    dir: tempfile::TempDir,
    shared: Arc<Shared>,
    calls: Arc<AtomicUsize>,
}
struct ReleaseAdapter {
    inner: SmtpArtifactAdapter,
    calls: Arc<AtomicUsize>,
}
impl PlatformAdapter for ReleaseAdapter {
    fn table(&self) -> &ToolTable {
        self.inner.table()
    }
    fn connection_descriptor(&self) -> Option<ProviderDescriptor> {
        self.inner.connection_descriptor()
    }
    fn connection_registration(&self) -> Option<String> {
        self.inner.connection_registration()
    }
    fn reported_manifest_version(&self) -> Option<String> {
        self.inner.reported_manifest_version()
    }
    fn prepare_app_text(
        &self,
        title: &str,
        body: &str,
        provenance: &Value,
    ) -> Result<Value, String> {
        self.inner.prepare_app_text(title, body, provenance)
    }
    fn preview(&self, account: &str, tool: &str, input: &Value) -> String {
        self.inner.preview(account, tool, input)
    }
    fn execute(
        &self,
        _: &[u8],
        _: &str,
        _: &Value,
        _: &str,
        _: Option<&str>,
    ) -> Result<Value, String> {
        panic!("release must use its native artifact path")
    }
    fn execute_app_artifact(
        &self,
        credential: &[u8],
        _: &str,
        _: &Value,
        _: &str,
        _: Option<&str>,
    ) -> Result<Value, AppArtifactError> {
        let (envelope, _) = crate::platform::smtp::custody_decode(credential).unwrap();
        assert_eq!(envelope.secret(), SMTP_SECRET.as_bytes());
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"receipt":"cad1778-provider-success"}))
    }
    fn read_back(&self, _: &str, _: &Value) -> Verified {
        Verified::True
    }
    fn source_hash(&self, _: &str, _: &str) -> Option<String> {
        None
    }
}
impl ReleaseFx {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("c1778rel")
            .tempdir()
            .unwrap();
        crate::issue::Pm::init(&dir.path().join("pm")).unwrap();
        let mut opts = ServeOptions {
            provider_env: crate::adapter::ProviderEnv::refusing_providers(),
            test_seam: true,
            ..ServeOptions::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", dir.path().join("pm").to_str().unwrap());
        crate::platform::smtp::attach(&mut opts);
        let calls = Arc::new(AtomicUsize::new(0));
        opts.platforms.insert(
            "smtp".into(),
            Arc::new(ReleaseAdapter {
                inner: SmtpArtifactAdapter::default(),
                calls: calls.clone(),
            }),
        );
        let shared = Shared::new(dir.path(), &opts).unwrap();
        assert_eq!(
            shared.platform_custody.tag(),
            crate::platform::custody::FILE_TAG
        );
        for (alias, role) in [("lead", "pm"), ("writer", "worker"), ("reviewer", "worker")] {
            shared
                .store
                .register_agent(&crate::store::NewAgent {
                    alias,
                    provider: "claude",
                    endpoint_kind: "managed",
                    role,
                    cwd: dir.path().to_str().unwrap(),
                    sandbox: "read-only",
                    instructions: None,
                    params: Some(r#"{"upstream":"lead"}"#),
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
            shared
                .store
                .set_identity(
                    alias,
                    &crate::adapter::Identity {
                        thread_id: "fixture-thread".into(),
                        session_id: "fixture-session".into(),
                        model: None,
                        effort: None,
                        pid: std::process::id(),
                        endpoint: None,
                        generation: Some("g1".into()),
                        attach: None,
                    },
                )
                .unwrap();
        }
        Self { dir, shared, calls }
    }
    fn call(&self, method: &str, params: Value) -> crate::error::Result<Value> {
        scoped(Asserted::Operator, || {
            self.shared.dispatch(method, &params, std::process::id())
        })
    }
    fn op(&self, method: &str, params: Value) -> Value {
        match self.call(method, params) {
            Ok(v) => v,
            Err(_) => panic!("release fixture RPC refused: {method}"),
        }
    }
    fn stage(&self) -> (String, String, Value) {
        let enrollment = crate::platform::smtp::SmtpEnrollment {
            host: "localhost".into(),
            port: 465,
            tls_mode: "implicit".into(),
            username: "fixture-user".into(),
            secret: SMTP_SECRET.as_bytes().to_vec(),
            sender: "fixture@fixture.cadence".into(),
            sender_name: "Fixture".into(),
        };
        let bytes =
            zeroize::Zeroizing::new(crate::platform::smtp::custody_bytes(&enrollment).unwrap());
        let connection = "cad1778-smtp";
        self.shared
            .platform_custody
            .put(
                &crate::platform::custody::Key {
                    platform: "smtp",
                    account: "cad1778",
                },
                &bytes,
            )
            .unwrap();
        self.shared
            .store
            .platform_enroll(
                &crate::store::CredentialRecord {
                    connection_id: connection.into(),
                    credential_revision: 1,
                    platform: "smtp".into(),
                    account: "cad1778".into(),
                    scopes: vec!["email:send".into()],
                    fingerprint: crate::secret::fingerprint(&bytes),
                    custody: self.shared.platform_custody.tag().into(),
                    exchange: "smtp".into(),
                    enrolled_at: 1.0,
                    by: "operator".into(),
                },
                false,
                None,
            )
            .unwrap();
        let installed = self.op("app_workspace_install", json!({"source":format!("{}/tests/fixtures/apps/social-content", env!("CARGO_MANIFEST_DIR"))}));
        let install = installed["install_id"].as_str().unwrap().to_owned();
        let bundle = installed["digest"].as_str().unwrap().to_owned();
        self.op(
            "app_local_install_approve",
            json!({"install_id":install,"digest":bundle}),
        );
        self.op("app_binding_create", json!({"install_id":install,"slot":"publication","connection_id":connection,"request_id":"cad1778-bind"}));
        let created = self.op("app_run_create", json!({"install_id":install,"workflow":"facebook","request_id":"cad1778-release-run","owner_pm":"lead",
            "inputs":{"subject":"Fixture caption","source":"fixture source facts","writer":"writer","reviewer":"reviewer"}}));
        let run = created["id"].as_str().unwrap();
        self.op(
            "app_run_approve",
            json!({"run_id":run,"digest":created["snapshot_digest"]}),
        );
        let store = &self.shared.store;
        let text = "# Post\nReviewed fixture copy.";
        let finish = |current: &Value, step: usize, alias: &str, reply: Value| {
            let message = current["steps"][step]["message_id"].as_str().unwrap();
            let taken = match store
                .take_queued_app_proven(alias, Some((message, &bundle)))
                .unwrap()
            {
                crate::store::Take::Message(m) => m,
                _ => panic!("real queued app turn must be claimed"),
            };
            let token = crate::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint("g1");
            store.mark_running(&taken.id, &token).unwrap();
            let taken = store.message(&taken.id).unwrap().unwrap();
            if step == 1 {
                store
                    .app_artifact_with_digest(
                        current["artifacts"][0]["id"].as_str().unwrap(),
                        Some((&taken.id, taken.turn_id.as_deref().unwrap())),
                        &bundle,
                    )
                    .unwrap();
            }
            store
                .finish(
                    &taken,
                    "completed",
                    &json!({"turn_id":taken.turn_id,"text":reply.to_string()}),
                    None,
                )
                .unwrap();
        };
        let dispatched = store.app_run_dispatch(run, &bundle).unwrap();
        finish(
            &dispatched,
            0,
            "writer",
            json!({"schema":1,"kind":"produce_text","run_id":run,"step_id":"s1","revision":1,
            "outcome":"succeeded","artifacts":[{"media_type":"text/markdown","text":text}]}),
        );
        let dispatched = store.app_run_dispatch(run, &bundle).unwrap();
        let artifact = dispatched["artifacts"][0]["id"].as_str().unwrap();
        finish(
            &dispatched,
            1,
            "reviewer",
            json!({"schema":1,"kind":"review_text","run_id":run,"step_id":"s2","revision":1,
            "producer_step_id":"s1","producer_revision":1,"artifact_sha256":crate::store::app_runs::artifact_digest(text.as_bytes()),
            "decision":"approve","rationale":"Checked exact artifact."}),
        );
        assert_eq!(store.app_run_show(run).unwrap()["state"], "succeeded");
        let staged = self.op("app_effect_stage", json!({"run_id":run,"artifact_id":artifact,"slot":"publication","request_id":"cad1778-release","title":"Reviewed caption"}));
        assert_eq!(staged["effect"]["state"], "waiting");
        (install, bundle, staged)
    }
}

fn release_case(fault: Fault) {
    let fx = ReleaseFx::new();
    assert!(fx.dir.path().starts_with(std::env::temp_dir()));
    let (install, bundle, staged) = fx.stage();
    let id = staged["effect"]["effect_id"].as_str().unwrap();
    let digest = staged["effect"]["digest"].as_str().unwrap();
    let (hook, seen) = record_hook(
        fx.shared.clone(),
        install,
        bundle,
        String::new(),
        "app-release-before-record",
        fault,
        true,
    );
    let _result = fx.call(
        "app_effect_decide",
        json!({"effect_id":id,"digest":digest,"decision":"accept"}),
    );
    drop(hook);
    assert_eq!(seen.get(), 1, "must reach release lock 2");
    assert_eq!(
        fx.calls.load(Ordering::SeqCst),
        1,
        "provider really executed"
    );
    let row = fx.shared.store.effect_by_id(id).unwrap().unwrap();
    if let Some(reason) = row.outcome.as_ref().and_then(|o| o["error"].as_str()) {
        clean(reason);
    }
    println!(
        "CAD1778 durable effect={} outcome_present={}",
        row.state,
        row.outcome.is_some()
    );
    assert_eq!(row.state, "reconcile", "never success or left executing");
    assert!(row.needs_you);
    let outcome = row.outcome.unwrap();
    assert_eq!(outcome["kind"], "uncertain");
    assert_eq!(outcome["verified"], "unknown");
    check_reason(outcome["error"].as_str().unwrap(), fault);
    let _retry = fx.call(
        "app_effect_decide",
        json!({"effect_id":id,"digest":digest,"decision":"accept"}),
    );
    assert_eq!(
        fx.calls.load(Ordering::SeqCst),
        1,
        "failure must never resend"
    );
}

/// ONE acceptance check, collecting independent cases so main's first bad
/// provider kind does not hide the recording/terminality regressions.
#[test]
fn cad1778_failure_record_acceptance() {
    let mut failures = Vec::new();
    let mut run = |name: String, case: &mut dyn FnMut()| {
        println!("CAD1778 CASE {name}");
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(case)).is_err() {
            failures.push(name.clone());
            println!("CAD1778 FAIL {name}");
        } else {
            println!("CAD1778 PASS {name}");
        }
    };
    for mode in 1..=4 {
        run(format!("worker-provider-mode-{mode}"), &mut || {
            worker_case(None, mode)
        });
    }
    for fault in [Fault::Write, Fault::SecretWrite, Fault::Authority] {
        run(format!("release-{fault:?}"), &mut || release_case(fault));
        run(format!("host-{fault:?}"), &mut || host_case(fault));
        run(format!("worker-{fault:?}"), &mut || {
            worker_case(Some(fault), 0)
        });
    }
    run("host-SnapshotRead".into(), &mut || {
        host_case(Fault::SnapshotRead)
    });
    run("worker-ConflictingReceipt".into(), &mut || {
        worker_case(Some(Fault::ConflictingReceipt), 0)
    });
    assert!(
        failures.is_empty(),
        "CAD1778 acceptance failures: {failures:?}"
    );
}
