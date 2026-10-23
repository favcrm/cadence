//! CAD-1258 (implementer): the worker `app_run_capability_call` path. The
//! provider call runs with the PM lock free, and a second identical request
//! makes no provider call and does not fail the run. Uses the real
//! handle_conn on the test thread (the lockseam hook is thread-local).
#![cfg(all(test, unix, feature = "test-seam"))]

#[path = "cad1778_failure_record_acceptance.rs"]
mod cad1778_failure_record_acceptance;

use super::Fx as HostFx;
use crate::daemon::Shared;
use crate::store::StoreConn;
use serde_json::{json, Value};
use std::sync::Arc;

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

struct Setup {
    fx: HostFx,
    worker: NativeWorker,
    install: String,
    run_id: String,
    message: String,
    token: String,
    bundle: String,
}

fn setup() -> Setup {
    let fx = HostFx::new();
    let worker = NativeWorker::new(fx._dir.path());
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
    Setup {
        fx,
        worker,
        install,
        run_id: run_id.to_string(),
        message: taken.id.clone(),
        token,
        bundle,
    }
}

fn posts(fx: &HostFx) -> usize {
    fx.seen
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.method == "POST")
        .count()
}

#[test]
fn cad1258_worker_provider_call_runs_outside_the_pm_lock() {
    let mut s = setup();
    let pm_dir = s.fx.shared.pm_dir().unwrap();
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let log = seen.clone();
    crate::issue::lockseam::set_hook(Some(Box::new(move |point| {
        let free = crate::issue::Pm::at(&pm_dir)
            .unwrap()
            .try_lock()
            .unwrap()
            .is_some();
        assert!(free, "PM lock held during {point}");
        log.borrow_mut().push(point.to_string());
    })));
    let body = json!({"message":s.message,"token":s.token,"slot":"image",
        "request_id":"c1258-worker","input":{}});
    let result = s.worker.call(s.fx.shared.clone(), &body);
    crate::issue::lockseam::set_hook(None);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        *seen.borrow(),
        [
            "app-capability-provider-call",
            "app-capability-before-record"
        ]
    );
    assert_eq!(posts(&s.fx), 1);
}

/// A claim already held (the first call still in flight) makes a second
/// identical request refuse with no provider call and no run failure.
#[test]
fn cad1258_worker_second_identical_request_makes_no_provider_call() {
    let mut s = setup();
    let (_, step, proof) =
        s.fx.shared
            .store
            .app_capability_turn("lock2-writer", &s.message, &s.token, "image", &s.bundle)
            .unwrap();
    let call_id = format!(
        "app-call-{}",
        uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            format!("{}:image", s.run_id).as_bytes()
        )
        .simple()
    );
    assert!(s
        .fx
        .shared
        .store
        .app_capability_claim(crate::store::app_capabilities::AppCapabilityClaim {
            run: &s.run_id,
            step: &step,
            message: &s.message,
            turn: &s.token,
            slot: "image",
            request: "c1258-dup",
            binding_digest: &proof.digest,
            input_digest: &crate::store::app_runs::material_digest(&json!({})),
            call_id: &call_id,
        })
        .unwrap());
    let body = json!({"message":s.message,"token":s.token,"slot":"image",
        "request_id":"c1258-dup","input":{}});
    let result = s.worker.call(s.fx.shared.clone(), &body);
    let error = result.expect_err("second request must be refused");
    assert_eq!(error.kind(), "unknown", "{error}");
    assert!(
        error.to_string().contains("no automatic retry was made"),
        "{error}"
    );
    assert_eq!(posts(&s.fx), 0, "no provider call for the second request");
    let run = s.fx.shared.store.app_run_show(&s.run_id).unwrap();
    assert_eq!(run["state"], "running", "{run}");
    assert!(run["failure"].is_null(), "{run}");
    let _ = s.install;
}

/// CAD-1778: a worker call that failed after its claim marks its own step
/// failed; the still-running worker message must still close, whichever way
/// it ends.
#[test]
fn cad1778_message_closes_after_a_failed_worker_call() {
    for how in ["completed", "failed", "unknown", "expire"] {
        let s = setup();
        let (_, step, _) =
            s.fx.shared
                .store
                .app_capability_turn("lock2-writer", &s.message, &s.token, "image", &s.bundle)
                .unwrap();
        s.fx.shared
            .store
            .app_run_call_failed(&s.run_id, &step, "uncertain", "boom")
            .unwrap();
        if how == "expire" {
            assert!(s
                .fx
                .shared
                .store
                .expire_awaiting_report(&s.message, None, "timed out")
                .unwrap());
        } else {
            let done = s.fx.shared.store.finish_running(
                &s.message,
                how,
                &json!({"status":how,"text":""}),
                None,
            );
            assert!(matches!(done, Ok(Ok(_))), "{how}: {done:?}");
        }
        let message = s.fx.shared.store.message(&s.message).unwrap().unwrap();
        assert_ne!(message.state, "running", "{how}");
    }
}

/// CAD-1778: the tracker's interrupted-write fence is a `Rejected` error too.
/// At lock 2 it is recorded uncertain naming the fence, never as an
/// authority change.
#[test]
fn cad1778_fence_at_lock_two_is_not_an_authority_change() {
    let mut s = setup();
    let dir = s.fx.shared.pm_dir().unwrap();
    crate::issue::lockseam::set_hook(Some(Box::new(move |point| {
        if point != "app-capability-before-record" {
            return;
        }
        // A writer that crashed mid-write: the CAD-852 helper (this test
        // binary re-executed) takes the lock, leaves an untracked file and
        // is SIGKILLed.
        let ready = std::env::temp_dir().join(format!("c1778-ready-{}", std::process::id()));
        let _ = std::fs::remove_file(&ready);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "issue::lock_tests::lock_helper", "--nocapture"])
            .env("CAD852_ROLE", "hold_untracked")
            .env("CAD852_PM", &dir)
            .env("CAD852_READY", &ready)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let start = std::time::Instant::now();
        while !ready.exists() {
            assert!(start.elapsed().as_secs() < 20, "writer not ready");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        child.kill().unwrap();
        child.wait().unwrap();
    })));
    let body = json!({"message":s.message,"token":s.token,"slot":"image",
        "request_id":"c1778-fence","input":{}});
    let result = s.worker.call(s.fx.shared.clone(), &body);
    crate::issue::lockseam::set_hook(None);
    assert!(result.is_err(), "{result:?}");
    let run = s.fx.shared.store.app_run_show(&s.run_id).unwrap();
    assert_eq!(run["state"], "failed", "{run}");
    let failure = run["failure"].to_string();
    assert!(failure.contains("uncertain"), "{failure}");
    assert!(failure.contains("interrupted"), "{failure}");
    assert!(
        !failure.to_lowercase().contains("authority changed"),
        "{failure}"
    );
}

/// CAD-1778: a worker lock-2 snapshot read failure still ends with a durable
/// failure and a terminal step.
#[test]
fn cad1778_worker_unreadable_snapshot_still_records_the_failure() {
    let mut s = setup();
    let (shared, run) = (s.fx.shared.clone(), s.run_id.clone());
    crate::issue::lockseam::set_hook(Some(Box::new(move |point| {
        if point == "app-capability-before-record" {
            shared
                .store
                .fixture_write(|c| {
                    c.execute(
                        "UPDATE app_runs SET snapshot='cad1778 malformed snapshot' WHERE id=?",
                        [&run],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
    })));
    let body = json!({"message":s.message,"token":s.token,"slot":"image",
        "request_id":"c1778-unreadable","input":{}});
    let result = s.worker.call(s.fx.shared.clone(), &body);
    crate::issue::lockseam::set_hook(None);
    assert!(result.is_err(), "{result:?}");
    let rows: (String, Vec<String>, i64) =
        s.fx.shared
            .store
            .read_tx(|c| {
                let state: String =
                    c.query_row("SELECT state FROM app_runs WHERE id=?", [&s.run_id], |r| {
                        r.get(0)
                    })?;
                let steps: Vec<String> = c.query_vec(
                    "SELECT state FROM app_run_steps WHERE run_id=?",
                    [&s.run_id],
                    |r| r.get(0),
                )?;
                let failures: i64 = c.query_row(
                    "SELECT count(*) FROM app_run_failures WHERE run_id=?",
                    [&s.run_id],
                    |r| r.get(0),
                )?;
                Ok((state, steps, failures))
            })
            .unwrap();
    assert_eq!(rows.0, "failed");
    assert!(rows.1.iter().all(|x| x == "failed"), "{rows:?}");
    assert_eq!(rows.2, 1);
}
