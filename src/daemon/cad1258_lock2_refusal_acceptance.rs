//! CAD-1258 acceptance item 2, independently authored by cc13-rev-sol.
//! Register as a child of cad1365_generation_acceptance to reuse its live
//! native/loopback Fx without editing or weakening its checks. The release
//! driver is adapted from the sealed downstream fixture; its provider reply
//! is a counted synthetic success. No production state/network.
//!
//! A second thread revokes through the real operator RPC after the provider
//! has succeeded, at the real before-record seam. Check durable outcomes,
//! not just RPC errors; replay must not send again. No-change controls prove
//! both fixtures can actually retain successful results. Upgrade is already
//! refused during an unresolved release/nonterminal run by upgrade_ready;
//! revoke is the reachable concurrent authority change used here.
#![cfg(all(test, unix, feature = "test-seam"))]

use super::Fx as HostFx;
use crate::contract_fixture::{ToolTable, Verified};
use crate::daemon::Shared;
use crate::platform::connections::ProviderDescriptor;
use crate::platform::smtp::test_artifact_adapter::SmtpArtifactAdapter;
use crate::platform::{AppArtifactError, PlatformAdapter};
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

// Adapted from cad1156_downstream_fixture, kept inline because that sealed
// driver's types are parent-private. Loading its file a second time triggers
// duplicate_mod; do not disable that lint or widen the sealed check's API.
struct ReleaseFx {
    dir: tempfile::TempDir,
    shared: Arc<Shared>,
    adapter: Arc<OutcomeAdapter>,
    claims: Arc<std::sync::Mutex<ClaimObservation>>,
}
impl ReleaseFx {
    fn new(adapter: OutcomeAdapter) -> Self {
        use crate::daemon::ServeOptions;
        let dir = tempfile::Builder::new()
            .prefix("c1258rel")
            .tempdir()
            .unwrap();
        crate::issue::Pm::init(&dir.path().join("pm")).unwrap();
        let claims = Arc::new(std::sync::Mutex::new(ClaimObservation::default()));
        let decided = claims.clone();
        let claimed = claims.clone();
        let mut opts = ServeOptions {
            provider_env: crate::adapter::ProviderEnv::refusing_providers(),
            test_seam: true,
            effect_execute_gate: Some(Arc::new(move |row| {
                assert_eq!(row.state, "decided");
                assert!(row.decision.is_some() && row.outcome.is_none());
                let mut log = decided.lock().unwrap();
                log.decisions += 1;
                log.decided = row.decision.clone();
                true
            })),
            app_release_claim_gate: Some(Arc::new(move |row| {
                assert_eq!(row.state, "executing");
                assert!(row.decision.is_some() && row.outcome.is_none());
                let mut log = claimed.lock().unwrap();
                log.claims += 1;
                log.claimed = row.decision.clone();
                true
            })),
            ..ServeOptions::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", dir.path().join("pm").to_str().unwrap());
        crate::platform::smtp::attach(&mut opts);
        let adapter = Arc::new(adapter);
        opts.platforms.insert("smtp".into(), adapter.clone());
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
        Self {
            dir,
            shared,
            adapter,
            claims,
        }
    }
    fn call(&self, method: &str, params: Value) -> crate::error::Result<Value> {
        scoped(Asserted::Operator, || {
            self.shared.dispatch(method, &params, std::process::id())
        })
    }
    fn op(&self, method: &str, params: Value) -> Value {
        checked(self.call(method, params), "release fixture RPC refused")
    }
    fn enroll_smtp(&self, account: &str, secret: &str) -> String {
        let enrollment = crate::platform::smtp::SmtpEnrollment {
            host: "localhost".into(),
            port: 465,
            tls_mode: "implicit".into(),
            username: "fixture-user".into(),
            secret: secret.as_bytes().to_vec(),
            sender: "fixture@fixture.cadence".into(),
            sender_name: "Fixture".into(),
        };
        let bytes = zeroize::Zeroizing::new(checked(
            crate::platform::smtp::custody_bytes(&enrollment),
            "SMTP encoding failed",
        ));
        let connection = format!("conn-{}", uuid::Uuid::new_v4().simple());
        let _custody = self.shared.platform_custody_lock.lock().unwrap();
        let record = crate::store::CredentialRecord {
            connection_id: connection.clone(),
            credential_revision: 1,
            platform: "smtp".into(),
            account: account.into(),
            scopes: vec!["email:send".into()],
            fingerprint: crate::secret::fingerprint(&bytes),
            custody: self.shared.platform_custody.tag().into(),
            exchange: "smtp".into(),
            enrolled_at: 1.0,
            by: "operator".into(),
        };
        checked(
            self.shared.platform_custody.put(
                &crate::platform::custody::Key {
                    platform: "smtp",
                    account,
                },
                &bytes,
            ),
            "fixture custody write failed",
        );
        checked(
            self.shared.store.platform_enroll(&record, false, None),
            "fixture enroll failed",
        );
        connection
    }
    fn install_social(&self, connection: &str) -> (String, String) {
        let installed = self.op(
            "app_workspace_install",
            json!({"source":format!(
            "{}/tests/fixtures/apps/social-content", env!("CARGO_MANIFEST_DIR"))}),
        );
        let install = installed["install_id"].as_str().unwrap().to_owned();
        let bundle = installed["digest"].as_str().unwrap().to_owned();
        self.op(
            "app_local_install_approve",
            json!({"install_id":install,"digest":bundle}),
        );
        self.op(
            "app_binding_create",
            json!({"install_id":install,"slot":"publication",
            "connection_id":connection,"request_id":"lock2-binding"}),
        );
        (install, bundle)
    }
    fn publication_run(&self, install: &str, bundle: &str) -> (String, String) {
        let created = self.op("app_run_create", json!({"install_id":install,"workflow":"facebook",
            "request_id":"lock2-publication","owner_pm":"lead","inputs":{"subject":"Fixture caption",
                "source":"fixture source facts","writer":"writer","reviewer":"reviewer"}}));
        let run_id = created["id"].as_str().unwrap().to_owned();
        self.op(
            "app_run_approve",
            json!({"run_id":run_id,"digest":created["snapshot_digest"]}),
        );
        let store = &self.shared.store;
        let body = "# Post\nReviewed fixture copy.";
        let finish = |run: &Value, step: usize, alias: &str, reply: Value| {
            let message = required(run["steps"][step]["message_id"].as_str(), "turn missing");
            let taken = match store
                .take_queued_app_proven(alias, Some((message, bundle)))
                .unwrap()
            {
                crate::store::Take::Message(taken) => taken,
                _ => panic!("app turn not claimed"),
            };
            let token = crate::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint("g1");
            store.mark_running(&taken.id, &token).unwrap();
            let taken = store.message(&taken.id).unwrap().unwrap();
            if step == 1 {
                store
                    .app_artifact_with_digest(
                        run["artifacts"][0]["id"].as_str().unwrap(),
                        Some((&taken.id, taken.turn_id.as_deref().unwrap())),
                        bundle,
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
        let run = store.app_run_dispatch(&run_id, bundle).unwrap();
        finish(
            &run,
            0,
            "writer",
            json!({"schema":1,"kind":"produce_text","run_id":run_id,
            "step_id":"s1","revision":1,"outcome":"succeeded","artifacts":[{"media_type":"text/markdown","text":body}]}),
        );
        let run = store.app_run_dispatch(&run_id, bundle).unwrap();
        let artifact = run["artifacts"][0]["id"].as_str().unwrap().to_owned();
        finish(
            &run,
            1,
            "reviewer",
            json!({"schema":1,"kind":"review_text","run_id":run_id,
            "step_id":"s2","revision":1,"producer_step_id":"s1","producer_revision":1,
            "artifact_sha256":crate::store::app_runs::artifact_digest(body.as_bytes()),
            "decision":"approve","rationale":"Checked exact artifact."}),
        );
        assert_eq!(store.app_run_show(&run_id).unwrap()["state"], "succeeded");
        let material = store
            .app_publication_material(&run_id, &artifact, bundle, "publication")
            .unwrap();
        assert_eq!(material["artifact"]["id"], artifact);
        assert_eq!(material["artifact"]["text"], body);
        assert_eq!(
            material["producer_receipt"]["material"]["outcome"],
            "succeeded"
        );
        assert_eq!(
            material["review_receipt"]["material"]["decision"],
            "approve"
        );
        (run_id, artifact)
    }
}

fn checked<T, E>(result: std::result::Result<T, E>, message: &'static str) -> T {
    match result {
        Ok(value) => value,
        Err(_) => panic!("{message}"),
    }
}
fn required<T>(value: Option<T>, message: &'static str) -> T {
    value.expect(message)
}

#[derive(Default)]
struct ClaimObservation {
    decisions: u64,
    claims: u64,
    decided: Option<Value>,
    claimed: Option<Value>,
}

/// Preserve the sealed SMTP descriptor/preparation while counting the actual
/// external-effect entry point, not a prepare/preview or success-record write.
#[derive(Default)]
struct OutcomeAdapter {
    stage: SmtpArtifactAdapter,
    calls: AtomicU64,
}
impl PlatformAdapter for OutcomeAdapter {
    fn table(&self) -> &ToolTable {
        self.stage.table()
    }
    fn connection_descriptor(&self) -> Option<ProviderDescriptor> {
        self.stage.connection_descriptor()
    }
    fn connection_registration(&self) -> Option<String> {
        self.stage.connection_registration()
    }
    fn reported_manifest_version(&self) -> Option<String> {
        self.stage.reported_manifest_version()
    }
    fn prepare_app_text(
        &self,
        title: &str,
        body: &str,
        provenance: &Value,
    ) -> std::result::Result<Value, String> {
        self.stage.prepare_app_text(title, body, provenance)
    }
    fn preview(&self, account: &str, tool: &str, input: &Value) -> String {
        self.stage.preview(account, tool, input)
    }
    fn execute(
        &self,
        _credential: &[u8],
        _tool: &str,
        _input: &Value,
        _key: &str,
        _hash: Option<&str>,
    ) -> std::result::Result<Value, String> {
        panic!("release must use execute_app_artifact, never generic execute")
    }
    fn execute_app_artifact(
        &self,
        credential: &[u8],
        _tool: &str,
        _input: &Value,
        _key: &str,
        _hash: Option<&str>,
    ) -> std::result::Result<Value, AppArtifactError> {
        let (envelope, _) = crate::platform::smtp::custody_decode(credential).unwrap();
        assert!(envelope.secret() == PASSWORD.as_bytes());
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"receipt":"lock2-provider-success"}))
    }
    fn read_back(&self, _tool: &str, _input: &Value) -> Verified {
        Verified::True
    }
    fn source_hash(&self, _agent: &str, _source: &str) -> Option<String> {
        None
    }
}
const PASSWORD: &str = "synthetic-cad1258-lock2-password";

/// Clear even on assertion unwind: a failed case must not leak its hook into
/// another test sharing this libtest thread.
struct HookReset;
impl Drop for HookReset {
    fn drop(&mut self) {
        crate::issue::lockseam::set_hook(None);
    }
}

fn before_record(
    shared: Arc<Shared>,
    install: String,
    digest: String,
    point: &'static str,
    revoke: bool,
    provider_calls: impl Fn() -> usize + 'static,
) -> (HookReset, Rc<Cell<usize>>) {
    let seen = Rc::new(Cell::new(0));
    let count = seen.clone();
    crate::issue::lockseam::set_hook(Some(Box::new(move |at| {
        if at != point {
            return;
        }
        count.set(count.get() + 1);
        assert_eq!(count.get(), 1, "record seam must run once");
        assert_eq!(provider_calls(), 1, "provider must really succeed first");
        let pm = crate::issue::Pm::at(&shared.pm_dir().unwrap()).unwrap();
        assert!(pm.try_lock().unwrap().is_some(), "PM lock held at {at}");
        assert!(shared.platform_custody_lock.try_lock().is_ok());
        assert!(shared.app_release_lock.try_lock().is_ok());
        assert_eq!(
            shared
                .store
                .app_capability_status(&install, &digest)
                .unwrap()["state"],
            "approved"
        );
        if revoke {
            let target = shared.clone();
            let params = json!({"install_id":install,"digest":digest});
            std::thread::spawn(move || {
                scoped(Asserted::Operator, || {
                    target.dispatch("app_local_install_revoke", &params, std::process::id())
                })
                .expect("concurrent revoke must commit before lock 2");
            })
            .join()
            .unwrap();
            assert_eq!(
                shared
                    .store
                    .app_capability_status(&install, &digest)
                    .unwrap()["state"],
                "revoked"
            );
        }
    })));
    (HookReset, seen)
}

fn release_case(revoke: bool) {
    let fx = ReleaseFx::new(OutcomeAdapter::default());
    assert!(fx.dir.path().starts_with(std::env::temp_dir()));
    let connection = fx.enroll_smtp("lock2", PASSWORD);
    let (install, bundle) = fx.install_social(&connection);
    let (run, artifact) = fx.publication_run(&install, &bundle);
    let stage = fx.op(
        "app_effect_stage",
        json!({"run_id":run,"artifact_id":artifact,"slot":"publication",
            "request_id":"lock2-release","title":"Reviewed caption"}),
    );
    assert_eq!(stage["effect"]["state"], "waiting");
    let id = stage["effect"]["effect_id"].as_str().unwrap().to_owned();
    let digest = stage["effect"]["digest"].as_str().unwrap().to_owned();
    let adapter = fx.adapter.clone();
    let (hook, seen) = before_record(
        fx.shared.clone(),
        install,
        bundle,
        "app-release-before-record",
        revoke,
        move || adapter.calls.load(Ordering::SeqCst) as usize,
    );
    let result = fx.call(
        "app_effect_decide",
        json!({"effect_id":id,"digest":digest,"decision":"accept"}),
    );
    drop(hook);
    assert_eq!(
        seen.get(),
        1,
        "release must reach lock 2 after the real provider call"
    );
    assert_eq!(fx.adapter.calls.load(Ordering::SeqCst), 1);
    let row = fx.shared.store.effect_by_id(&id).unwrap().unwrap();
    let outcome = row
        .outcome
        .as_ref()
        .expect("release outcome must be durable");
    if revoke {
        assert_eq!(
            row.state, "reconcile",
            "stale release must not record success"
        );
        assert_eq!(outcome["kind"], "uncertain");
        assert!(row.needs_you);
        assert!(outcome["error"].as_str().is_some_and(|s| !s.is_empty()));
        assert_eq!(outcome["verified"], "unknown");
    } else {
        assert_eq!(
            outcome["kind"], "released",
            "unchanged release control must succeed"
        );
        assert_eq!(outcome["result"]["receipt"], "lock2-provider-success");
    }
    assert!(
        result.is_ok(),
        "release must return its honest durable outcome"
    );
    let observation = fx.claims.lock().unwrap();
    assert_eq!((observation.decisions, observation.claims), (1, 1));
    assert_eq!(observation.claimed, observation.decided);
    drop(observation);
    let stable = fx.shared.store.app_effect_show(&id).unwrap();
    let _retry = fx.call(
        "app_effect_decide",
        json!({"effect_id":id,"digest":digest,"decision":"accept"}),
    );
    assert_eq!(
        fx.adapter.calls.load(Ordering::SeqCst),
        1,
        "release retry must not resend"
    );
    assert_eq!(fx.shared.store.app_effect_show(&id).unwrap(), stable);
    println!(
        "CAD1258 release revoke={revoke}: provider=1 durable_kind={} retry_provider=1",
        outcome["kind"]
    );
}

fn host_case(revoke: bool) {
    let fx = HostFx::new();
    let expected = json!({"image":fx.quote()});
    let installed = fx
        .operator("app_workspace_show", json!({"install_id":fx.install}))
        .unwrap();
    let digest = installed["digest"].as_str().unwrap().to_owned();
    let traffic = fx.seen.clone();
    let (hook, seen) = before_record(
        fx.shared.clone(),
        fx.install.clone(),
        digest,
        "app-host-before-record",
        revoke,
        move || {
            traffic
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.method == "POST")
                .count()
        },
    );
    let result = fx.start(
        "create-image",
        json!({"subject":"Customer follow-up","source":"Acme CRM helps small teams track customers",
            "image_prompt":"Use a calm editorial palette"}),
        expected.clone(),
        "lock2-host",
    );
    drop(hook);
    assert_eq!(
        seen.get(),
        1,
        "host must reach lock 2 after the real provider call"
    );
    let listed = fx.shared.store.app_run_list(Some(&fx.install)).unwrap();
    let rows = listed["runs"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let run = &rows[0];
    let run_id = run["id"].as_str().unwrap();
    let receipt = fx
        .shared
        .store
        .app_capability_result_for_slot(run_id, "image")
        .unwrap();
    let (catalog, _) = fx.shared.store.app_media_catalog(20, None).unwrap();
    if revoke {
        assert!(result.is_err(), "stale host call must refuse success");
        assert!(
            receipt.is_none(),
            "lock 2 must not retain the stale provider success"
        );
        assert!(
            catalog.is_empty(),
            "stale success must not enter the media catalog"
        );
        assert_eq!(
            run["state"], "failed",
            "host uncertainty must be durable, not a silent RPC error"
        );
        assert_eq!(run["failure"]["kind"], "uncertain");
        assert!(run["failure"]["reason"]
            .as_str()
            .is_some_and(|s| !s.is_empty()));
    } else {
        assert!(result.is_ok());
        assert_eq!(run["state"], "succeeded");
        assert_eq!(receipt.unwrap()["result"]["kind"], "media.generated.image");
        assert_eq!(catalog.len(), 1);
    }
    let _dispatch_retry = fx.operator("app_run_dispatch", json!({"run_id":run_id}));
    let _start_retry = fx.start(
        "create-image",
        json!({"subject":"Customer follow-up","source":"Acme CRM helps small teams track customers",
            "image_prompt":"Use a calm editorial palette"}),
        expected,
        "lock2-host",
    );
    assert_eq!(fx.shared.store.app_run_show(run_id).unwrap(), *run);
    assert_eq!(
        fx.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == "POST")
            .count(),
        1,
        "host retries must not submit a second job"
    );
    println!(
        "CAD1258 host revoke={revoke}: provider=1 durable_state={} retry_provider=1",
        run["state"]
    );
}

/// A real Unix client child, enrolled as the native managed endpoint. Serve
/// each request on this test thread so the thread-local record hook exercises
/// the actual handle_conn/SO_PEERCRED/strict-identity path without a fake pid.
struct NativeWorker {
    child: std::process::Child,
    listener: std::os::unix::net::UnixListener,
}
impl NativeWorker {
    fn new(root: &std::path::Path) -> Self {
        let socket = root.join("worker.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let child = std::process::Command::new("python3")
            .args([
                "-u",
                "-c",
                r#"
import socket, sys
for line in sys.stdin:
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(30)
        s.connect(sys.argv[1])
        s.sendall(line.encode())
        s.shutdown(socket.SHUT_WR)
        print(s.makefile().readline(), end='', flush=True)
"#,
            ])
            .arg(socket)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        Self { child, listener }
    }
    fn call(&mut self, shared: Arc<Shared>, body: &Value) -> crate::error::Result<Value> {
        use std::io::{BufRead, Write};
        writeln!(
            self.child.stdin.as_mut().unwrap(),
            "{}",
            crate::proto::request("app_run_capability_call", body.clone())
        )
        .unwrap();
        self.child.stdin.as_mut().unwrap().flush().unwrap();
        let start = std::time::Instant::now();
        let stream = loop {
            match self.listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        start.elapsed() < std::time::Duration::from_secs(10),
                        "worker never connected"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => panic!("worker accept: {e}"),
            }
        };
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(30)))
            .unwrap();
        crate::daemon::serve::handle_conn(shared, stream);
        let mut response = String::new();
        std::io::BufReader::new(self.child.stdout.as_mut().unwrap())
            .read_line(&mut response)
            .unwrap();
        crate::proto::unwrap(serde_json::from_str(&response).unwrap())
    }
}
impl Drop for NativeWorker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The worker RPC is reachable on this head: a real assigned text-producing
/// turn may call its workflow's frozen image capability. No actor is launched;
/// the store's normal queued/running transition and managed turn token are used.
fn worker_case(revoke: bool) {
    let fx = HostFx::new();
    let mut worker = NativeWorker::new(fx._dir.path());
    for (alias, role) in [("lock2-lead", "pm"), ("lock2-writer", "worker")] {
        fx.shared
            .store
            .register_agent(&crate::store::NewAgent {
                alias,
                provider: "claude",
                endpoint_kind: "managed",
                role,
                cwd: fx._dir.path().to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: Some(r#"{"upstream":"lock2-lead"}"#),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        fx.shared
            .store
            .set_identity(
                alias,
                &crate::adapter::Identity {
                    thread_id: "fixture-thread".into(),
                    session_id: "fixture-session".into(),
                    model: None,
                    effort: None,
                    pid: worker.child.id(),
                    endpoint: None,
                    generation: Some("g1".into()),
                    attach: None,
                },
            )
            .unwrap();
    }
    fx.shared.enroll_endpoint("lock2-writer");
    let app = fx._dir.path().join("worker-app");
    std::fs::create_dir_all(app.join("workflows")).unwrap();
    std::fs::write(
        app.join("app.md"),
        super::APP_MD.replace("designer-fixture", "lock2-worker-fixture"),
    )
    .unwrap();
    let workflow = super::WORKFLOW_MD
        .replace("execution: host", "execution: agent")
        .replace("size: S", "agent: lock2-writer\nsize: S")
        .replace(
            "action: local.capability.call",
            "action: local.text.produce",
        );
    std::fs::write(app.join("workflows/worker-image.md"), workflow).unwrap();
    let installed = fx
        .operator("app_workspace_install", json!({"source":app}))
        .unwrap();
    let install = installed["install_id"].as_str().unwrap().to_owned();
    let bundle = installed["digest"].as_str().unwrap().to_owned();
    fx.operator(
        "app_local_install_approve",
        json!({"install_id":install,"digest":bundle}),
    )
    .unwrap();
    fx.operator(
        "app_binding_create",
        json!({"install_id":install,"slot":"image",
        "connection_id":"gen1365","request_id":"lock2-worker-binding"}),
    )
    .unwrap();
    let created = fx.operator("app_run_create", json!({"install_id":install,
        "workflow":"worker-image","request_id":"lock2-worker","owner_pm":"lock2-lead",
        "inputs":{"subject":"Customer follow-up","source":"Acme CRM helps small teams track customers",
            "image_prompt":"Use a calm editorial palette"}})).unwrap();
    let run_id = created["id"].as_str().unwrap();
    fx.operator(
        "app_run_approve",
        json!({"run_id":run_id,"digest":created["snapshot_digest"]}),
    )
    .unwrap();
    // As in the sealed publication driver, dispatch/claim the native store turn
    // directly so no background actor resume or real model launch is possible.
    let dispatched = fx.shared.store.app_run_dispatch(run_id, &bundle).unwrap();
    let message = dispatched["steps"][0]["message_id"].as_str().unwrap();
    let taken = match fx
        .shared
        .store
        .take_queued_app_proven("lock2-writer", Some((message, &bundle)))
        .unwrap()
    {
        crate::store::Take::Message(taken) => taken,
        _ => panic!("worker must claim the real queued app turn"),
    };
    let token = crate::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint("g1");
    fx.shared.store.mark_running(&taken.id, &token).unwrap();
    let body = json!({"message":taken.id,"token":token,"slot":"image",
        "request_id":"lock2-worker-call","input":{}});
    let traffic = fx.seen.clone();
    let (hook, seen) = before_record(
        fx.shared.clone(),
        install,
        bundle,
        "app-capability-before-record",
        revoke,
        move || {
            traffic
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.method == "POST")
                .count()
        },
    );
    // Use the real enrolled strict endpoint proof; Asserted::Agent would be
    // only a pane proof and the native worker gate correctly refuses it.
    let result = worker.call(fx.shared.clone(), &body);
    drop(hook);
    assert_eq!(
        seen.get(),
        1,
        "worker must reach lock 2 after actual provider success: {result:?}"
    );
    let receipt = fx
        .shared
        .store
        .app_capability_result_for_slot(run_id, "image")
        .unwrap();
    let _retry = worker.call(fx.shared.clone(), &body);
    assert_eq!(
        fx.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == "POST")
            .count(),
        1,
        "worker retry must not submit another job"
    );
    let run = fx.shared.store.app_run_show(run_id).unwrap();
    if revoke {
        let error = result.expect_err("stale worker must refuse success");
        assert!(!error.to_string().is_empty());
        assert!(receipt.is_none(), "worker must not retain stale success");
        println!("CAD1258 worker revoke=true: provider=1 receipt=none retry_provider=1 state={} failure={} RPC={error}", run["state"], run["failure"]);
        assert_eq!(
            run["state"], "failed",
            "worker uncertainty must be durable, not a silent RPC error"
        );
        assert!(run["failure"]["reason"]
            .as_str()
            .is_some_and(|s| !s.is_empty()));
    } else {
        assert!(
            result.is_ok(),
            "unchanged worker control must retain its result"
        );
        assert_eq!(receipt.unwrap()["result"]["kind"], "media.generated.image");
        println!("CAD1258 worker revoke=false: provider=1 receipt=retained retry_provider=1");
    }
}

/// ONE acceptance check, with real entry points and no-change controls.
#[test]
fn cad1258_lock2_refuses_concurrent_revoke_without_reexecuting() {
    for revoke in [false, true] {
        release_case(revoke);
        host_case(revoke);
    }
    for revoke in [false, true] {
        worker_case(revoke);
    }
}
