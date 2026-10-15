//! Independent SEC-002 acceptance. After a legitimate same-account re-login,
//! the native host dispatch and the native worker capability call must refuse
//! the frozen quote before any renewal, price or provider connection. The
//! durable binding stays. Stub HTTP only; custody, Store and both entries are real.

use super::*;
use crate::remote_cli::cad1459_account_image_acceptance as login;
use crate::remote_cli::{account_image_identity, RemoteTarget};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicUsize;
use std::thread;
use std::time::Instant;

const TEST: &str = "daemon::cad1365_original_transfer_acceptance::cad1459_native_lineage_entry_acceptance::cad1459_native_host_and_worker_refuse_frozen_lineage_before_network";
const CHILD: &str = "CAD1459_NATIVE_LINEAGE_ROOT";
const WORKER_FLAG: &str = "CAD1459_LINEAGE_WORKER";
const BOARD: &str = "https://lc.cadencecloud.app";
const WORKSPACE: &str = "ws_cad1459_selected";
const GENERATION: &str = "0123456789abcdef0123456789abcdef";

fn worker_main() {
    let socket = PathBuf::from(std::env::var_os("CAD1459_WORKER_SOCKET").unwrap());
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.unwrap();
        if line == "quit" {
            break;
        }
        let mut stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream.write_all(line.as_bytes()).unwrap();
        stream.write_all(b"\n").unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut reader = BufReader::new(stream);
        let mut reply = String::new();
        reader.read_line(&mut reply).unwrap();
        print!("REPLY:{reply}");
        let _ = std::io::stdout().flush();
    }
}

fn inputs() -> Value {
    json!({
        "subject": "Customer follow-up",
        "source": "Acme CRM helps small teams track customers",
        "image_prompt": "Use a calm editorial palette"
    })
}

fn timed(fx: &Fx, method: &str, params: Value) -> crate::error::Result<Value> {
    scoped(Asserted::Operator, || {
        crate::client::rpc_timeout(&fx.state, method, params, Duration::from_secs(25))
    })
}

fn create_run(fx: &Fx, workflow: &str, request: &str, owner: Option<&str>) -> Value {
    let (token, key) = fx.session();
    let mut params = json!({
        "install_id": fx.install,
        "workflow": workflow,
        "inputs": inputs(),
        "request_id": request,
        "token": token,
        "key": key,
        "origin": "loopback"
    });
    if let Some(owner) = owner {
        params["owner_pm"] = json!(owner);
    }
    timed(fx, "app_run_create", params)
        .unwrap_or_else(|error| panic!("{workflow} create failed: {error}"))
}

fn approve(fx: &Fx, created: &Value) -> String {
    let id = created["id"].as_str().unwrap().to_string();
    timed(
        fx,
        "app_run_approve",
        json!({"run_id": id, "digest": created["snapshot_digest"]}),
    )
    .unwrap_or_else(|error| panic!("approve {id} failed: {error}"));
    id
}

fn worker_call(
    stdin: &mut impl Write,
    stdout: &mut BufReader<std::process::ChildStdout>,
    message: &str,
    turn: &str,
    request: &str,
    slot: &str,
) -> Value {
    let frame = crate::proto::request(
        "app_run_capability_call",
        json!({
            "message": message,
            "token": turn,
            "slot": slot,
            "request_id": request,
            "input": {}
        }),
    );
    writeln!(stdin, "{frame}").unwrap();
    stdin.flush().unwrap();
    let fd = stdout.get_ref().as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    let mut acc = Vec::new();
    let started = Instant::now();
    let value = loop {
        if let Some(body) = reply_body(&acc) {
            break body;
        }
        if started.elapsed() > Duration::from_secs(25) {
            break json!({
                "raw": "worker reply timed out",
                "seen": String::from_utf8_lossy(&acc),
            });
        }
        let mut buf = [0u8; 1024];
        match stdout.get_mut().read(&mut buf) {
            Ok(0) => {
                break json!({"raw": "worker closed stdout", "seen": String::from_utf8_lossy(&acc)});
            }
            Ok(n) => acc.extend_from_slice(&buf[..n]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => break json!({"raw": error.to_string()}),
        }
    };
    unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    value
}

fn reply_body(acc: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(acc).ok()?;
    let start = text.rfind("REPLY:")?;
    let after = &text[start + "REPLY:".len()..];
    let (line, rest) = after.split_once('\n')?;
    let _ = rest;
    Some(serde_json::from_str(line.trim()).unwrap_or_else(|_| json!({"raw": line})))
}

fn claim(fx: &Fx, turn: &str) -> crate::store::Message {
    let taken = fx
        .shared
        .take_app_aware("image-worker")
        .unwrap_or_else(|error| panic!("claim failed: {error}"));
    let crate::store::Take::Message(message) = taken else {
        panic!("image-worker had no queued app turn");
    };
    fx.shared
        .store
        .mark_running(&message.id, turn)
        .unwrap_or_else(|error| panic!("mark running failed: {error}"));
    *message
}

#[test]
fn cad1459_native_host_and_worker_refuse_frozen_lineage_before_network() {
    if std::env::var_os(WORKER_FLAG).is_some() {
        worker_main();
        return;
    }
    let Some(root) = std::env::var_os(CHILD).map(PathBuf::from) else {
        let root = tempfile::Builder::new()
            .prefix("c1459n-")
            .tempdir()
            .unwrap();
        for name in ["home", "config", "data", "state", "tmp", "locks"] {
            login::private_dir(&root.path().join(name));
        }
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--test-threads", "1"])
            .env(CHILD, root.path())
            .env("HOME", root.path().join("home"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env("TMPDIR", root.path().join("tmp"))
            .env("CADENCE_SUITE_LOCK", root.path().join("locks/suite"))
            .stdin(Stdio::null())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "isolated native lineage entry acceptance failed"
        );
        return;
    };
    let auth = root.join("config/cadence/remote-auth");
    login::private_dir(&auth);
    login::private_write(
        &auth.join("trusted-issuer"),
        b"https://issuer.board.localhost",
    );
    let original_login = login::normal_login();
    login::save_login(&auth, &original_login);
    login::select(&root, "lc");

    let original_bytes = noisy_square_png();
    let digest = format!("{:x}", Sha256::digest(&original_bytes));
    let artifact = json!({"ref": format!("{JOB_ID}.{digest}"), "digest": digest,
        "bytes": original_bytes.len(), "mime": "image/png"});
    let price = json!({"slug":"generate_image","chargeMinor":31_500,"currency":"USD","version":"2026-09-29T00:00:00.000Z"});
    let submits = Arc::new(AtomicUsize::new(0));
    let submits_reply = submits.clone();
    let provider_bytes = original_bytes.clone();
    let pki = root.join("wire-lineage");
    login::private_dir(&pki);
    let wire = login::Wire::media(
        &pki,
        Arc::new(move |request| {
            let body = if request == "GET /__platform/media/price/image HTTP/1.1\r\n" {
                json!({"ok":true,"data":{"kind":"image","model":"openai/gpt-image-2.5","price":price}})
            } else if request == "POST /__platform/media/image HTTP/1.1\r\n" {
                submits_reply.fetch_add(1, Ordering::SeqCst);
                json!({"ok":true,"data":{"job":{"id":JOB_ID,"kind":"image","status":"succeeded",
                    "model":"openai/gpt-image-2.5","provider":"upstream-fixture","providerTaskId":null,
                    "artifacts":[artifact],"artifactError":null,"usage":{"providerCredits":null},
                    "price":price,"repeated":false,"cached":false,"stale":false}}})
            } else if request.starts_with("GET /__platform/media/artifacts/") {
                return ("image/png".into(), provider_bytes.clone());
            } else {
                panic!("unexpected synthetic account media operation: {request}");
            };
            ("application/json".into(), body.to_string().into_bytes())
        }),
    );
    let _trust = crate::issuer_transport::test_fixture::install(wire.trust());
    let fx = Fx::boot_configured(original_bytes, None, false, Some(wire.trust()), false, true);

    let mut worker = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--test-threads", "1", "--nocapture"])
        .env(WORKER_FLAG, "1")
        .env("CAD1459_WORKER_SOCKET", fx.state.join("cadence.sock"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let worker_pid = worker.id();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "lead",
            provider: "fake",
            endpoint_kind: "fake",
            role: "pm",
            cwd: fx.state.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "image-worker",
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: fx.state.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: Some(r#"{"upstream":"lead"}"#),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    fx.shared
        .store
        .set_identity(
            "image-worker",
            &crate::adapter::Identity {
                thread_id: "cad1459-worker".into(),
                session_id: "cad1459-session".into(),
                model: None,
                effort: None,
                pid: worker_pid,
                endpoint: None,
                generation: Some(GENERATION.into()),
                attach: None,
            },
        )
        .unwrap();
    fx.shared.enroll_endpoint("image-worker");

    let created = create_run(&fx, "create-image", "cad1459-host-ok", None);
    let host_ok = approve(&fx, &created);
    let dispatched = timed(&fx, "app_run_dispatch", json!({"run_id": host_ok}))
        .unwrap_or_else(|error| panic!("current-lineage host dispatch failed: {error}"));
    assert_eq!(
        dispatched["state"], "succeeded",
        "positive host control: {dispatched}"
    );
    assert_eq!(
        submits.load(Ordering::SeqCst),
        1,
        "positive host reaches one provider submit"
    );

    let stale_host = approve(
        &fx,
        &create_run(&fx, "create-image", "cad1459-host-stale", None),
    );

    let worker_ok_created = create_run(&fx, "worker-image", "cad1459-worker-ok", Some("lead"));
    let _worker_ok = approve(&fx, &worker_ok_created);
    timed(
        &fx,
        "app_run_dispatch",
        json!({"run_id": worker_ok_created["id"].as_str().unwrap()}),
    )
    .unwrap_or_else(|error| panic!("current-lineage worker dispatch failed: {error}"));
    let current_turn = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(GENERATION);
    let running = claim(&fx, &current_turn);
    let mut stdin = worker.stdin.take().unwrap();
    let mut stdout = BufReader::new(worker.stdout.take().unwrap());
    let positive = worker_call(
        &mut stdin,
        &mut stdout,
        &running.id,
        &current_turn,
        "cad1459-worker-ok",
        "image",
    );
    assert!(
        positive.get("error").is_none() && positive.get("result").is_some(),
        "positive worker control must execute the current quote: {positive}"
    );
    assert_eq!(
        submits.load(Ordering::SeqCst),
        2,
        "positive worker reaches one provider submit"
    );
    let finished = fx
        .shared
        .store
        .message(&running.id)
        .unwrap()
        .expect("positive worker message remains");
    fx.shared
        .complete(
            &finished,
            crate::adapter::TurnResult {
                turn_id: current_turn,
                status: "completed".into(),
                text: String::new(),
                stop_reason: None,
                error: None,
            },
        )
        .unwrap_or_else(|error| panic!("positive worker turn did not finish: {error}"));

    let worker_stale_created =
        create_run(&fx, "worker-image", "cad1459-worker-stale", Some("lead"));
    let _worker_stale = approve(&fx, &worker_stale_created);
    timed(
        &fx,
        "app_run_dispatch",
        json!({"run_id": worker_stale_created["id"].as_str().unwrap()}),
    )
    .unwrap_or_else(|error| panic!("pre-relogin worker dispatch failed: {error}"));
    let stale_token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(GENERATION);
    let stale_turn = claim(&fx, &stale_token);

    let connections = wire.connection_count();
    let submits_before = submits.load(Ordering::SeqCst);
    let bindings = timed(
        &fx,
        "app_binding_list",
        json!({"install_id": fx.install, "context_id": fx.context}),
    )
    .unwrap();
    let target = RemoteTarget {
        identity: None,
        org: "lc".into(),
        endpoint: BOARD.into(),
        org_id: WORKSPACE.into(),
    };
    let before = account_image_identity(&target).unwrap();
    let login_path = auth.join("cli-lc.json");
    let mut relogin: Value = serde_json::from_slice(&std::fs::read(&login_path).unwrap()).unwrap();
    let access = relogin["access_token"].clone();
    relogin["cli_session"]["session_id"] = json!("hcs_99999999-9999-4999-8999-999999999999");
    let absolute = relogin["cli_session"]["absolute_expires_at"]
        .as_u64()
        .unwrap()
        + 3600;
    relogin["cli_session"]["absolute_expires_at"] = json!(absolute);
    login::private_write(&login_path, &serde_json::to_vec(&relogin).unwrap());
    let current = account_image_identity(&target).unwrap();
    assert_ne!(
        before.session_id, current.session_id,
        "re-login must change execution lineage"
    );
    assert_eq!(before.subject_id, current.subject_id);
    assert_eq!(before.organization_id, current.organization_id);

    let host_refusal = timed(&fx, "app_run_dispatch", json!({"run_id": stale_host}));
    let host_error = host_refusal.expect_err("host dispatch must refuse the frozen lineage");
    assert_ne!(
        host_error.kind(),
        "unknown",
        "host refusal must be definite: {host_error}"
    );
    let worker_refusal = worker_call(
        &mut stdin,
        &mut stdout,
        &stale_turn.id,
        &stale_token,
        "cad1459-worker-stale",
        "image",
    );
    assert!(
        worker_refusal.get("error").is_some(),
        "worker entry must refuse the frozen lineage: {worker_refusal}"
    );
    assert_ne!(
        worker_refusal["error"]["kind"].as_str(),
        Some("unknown"),
        "worker refusal must be definite: {worker_refusal}"
    );
    let undeclared = worker_call(
        &mut stdin,
        &mut stdout,
        &stale_turn.id,
        &stale_token,
        "cad1459-worker-undeclared",
        "notes",
    );
    assert!(
        undeclared.get("error").is_some(),
        "undeclared slot must refuse the frozen lineage: {undeclared}"
    );
    assert_ne!(
        undeclared["error"]["kind"].as_str(),
        Some("unknown"),
        "undeclared slot refusal must be definite: {undeclared}"
    );
    assert_eq!(
        wire.connection_count(),
        connections,
        "frozen lineage must refuse before renewal, price or provider connections"
    );
    assert_eq!(
        submits.load(Ordering::SeqCst),
        submits_before,
        "no provider submit after re-login"
    );
    let bindings_after = timed(
        &fx,
        "app_binding_list",
        json!({"install_id": fx.install, "context_id": fx.context}),
    )
    .unwrap();
    assert_eq!(
        bindings_after, bindings,
        "durable binding is unchanged by re-login"
    );
    let saved: Value = serde_json::from_slice(&std::fs::read(&login_path).unwrap()).unwrap();
    assert_eq!(
        saved["access_token"], access,
        "refusal must not renew the access token"
    );
    let fd = stdout.get_ref().as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) };
    let drain = thread::spawn(move || {
        let mut rest = Vec::new();
        let _ = stdout.get_mut().read_to_end(&mut rest);
    });
    writeln!(stdin, "quit").ok();
    drop(stdin);
    let status = worker.wait().expect("lineage worker wait");
    let _ = drain.join();
    assert!(status.success(), "lineage worker exited {status}");
}
