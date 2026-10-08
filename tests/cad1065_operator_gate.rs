#![cfg(all(feature = "test-seam", target_os = "linux"))]

use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon, issue, operator_auth, platform, reaper, store::Store};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const PASSWORD: &str = "cad1065-synthetic-only-password";
const SMTP_RIG: &str = r#"
import base64, json, pathlib, socket, ssl, sys
root = pathlib.Path(sys.argv[1])
tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
tls.load_cert_chain(root / "certificate.pem", root / "key.pem")
listener = socket.socket()
listener.bind(("127.0.0.1", 0))
listener.listen(8)
(root / "port").write_text(str(listener.getsockname()[1]))
def record(kind):
    with (root / "observed").open("a") as out:
        out.write(kind + "\n")
while True:
    raw, _ = listener.accept()
    record("CONNECT")
    raw.settimeout(25)
    try:
        with tls.wrap_socket(raw, server_side=True) as secure:
            with secure.makefile("rwb", buffering=0) as wire:
                wire.write(b"220 localhost synthetic SMTP\r\n")
                login = 0
                while True:
                    line = wire.readline(4097)
                    if not line or len(line) > 4096:
                        break
                    if login:
                        supplied = base64.b64decode(line.strip(), validate=True)
                        if login == 1:
                            assert supplied == b"sender@example.test"
                            login = 2
                            wire.write(b"334 UGFzc3dvcmQ6\r\n")
                        else:
                            assert supplied == b"cad1065-synthetic-only-password"
                            login = 0
                            wire.write(b"235 2.7.0 authenticated\r\n")
                        continue
                    parts = line.rstrip(b"\r\n").split(b" ", 2)
                    command = parts[0].decode("ascii").upper()
                    record(command)
                    if command == "EHLO":
                        wire.write(b"250-localhost\r\n250 AUTH PLAIN LOGIN\r\n")
                    elif command == "AUTH" and parts[1].upper() == b"PLAIN":
                        if len(parts) < 3:
                            wire.write(b"504 initial response required\r\n")
                        else:
                            fields = base64.b64decode(parts[2], validate=True).split(b"\0")
                            assert fields[-2:] == [b"sender@example.test", b"cad1065-synthetic-only-password"]
                            wire.write(b"235 2.7.0 authenticated\r\n")
                    elif command == "AUTH" and parts[1].upper() == b"LOGIN":
                        login = 1
                        wire.write(b"334 VXNlcm5hbWU6\r\n")
                    elif command == "QUIT":
                        wire.write(b"221 2.0.0 goodbye\r\n")
                        break
                    else:
                        wire.write(b"550 forbidden command\r\n")
                        break
    except Exception:
        record("RIG_FAILURE")
"#;

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

struct CredentialReads(OwnedFd);

impl CredentialReads {
    fn watch(dir: &Path) -> Self {
        let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        assert!(raw >= 0, "credential observation setup failed");
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let name = CString::new(dir.as_os_str().as_encoded_bytes()).unwrap();
        let watched = unsafe {
            libc::inotify_add_watch(
                fd.as_raw_fd(),
                name.as_ptr(),
                libc::IN_OPEN | libc::IN_ACCESS,
            )
        };
        assert!(watched >= 0, "credential observation watch failed");
        Self(fd)
    }

    fn assert_no_read(&self) {
        let mut buffer = [0u8; 8192];
        let count =
            unsafe { libc::read(self.0.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
        assert_eq!(count, -1, "denied verification accessed credential storage");
        assert_eq!(
            std::io::Error::last_os_error().kind(),
            std::io::ErrorKind::WouldBlock,
            "credential observation failed"
        );
    }

    fn assert_read_observed(&self) {
        let mut buffer = [0u8; 8192];
        let count =
            unsafe { libc::read(self.0.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
        assert!(count > 0, "credential observation positive control failed");
    }
}

/// `platform::smtp::direct_dial_count` is a process-global counter shared
/// by every fixture daemon this binary runs in parallel under
/// `--test-threads 2`. A test that snapshots the counter must own its
/// observation window: while its fixture lives, no sibling fixture's
/// legitimate `verify_login`/send dial may interleave, or the snapshot
/// assertion observes a dial it did not cause. This witness serializes
/// fixture lifetimes (never the requests inside them), holding from
/// before any setup until after the daemon and board threads are joined
/// and the owned rig child is reaped. The lock carries no shared state,
/// so a poisoned guard from a panicking fixture test is still safe to
/// take: `Drop` always runs its teardown before releasing.
static SMTP_COUNTER_WITNESS: Mutex<()> = Mutex::new(());

struct Fixture {
    _rig: OwnedChild,
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    port: u16,
    board_port: u16,
    // LAST field: after `Drop::drop` joins the server threads, fields are
    // dropped in declaration order, so the witness is released only once
    // every owned thread and the rig child are gone — no late daemon dial
    // can leak into the next fixture's counter snapshot.
    _smtp_counter_witness: MutexGuard<'static, ()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        for thread in self.threads.drain(..).rev() {
            let _ = thread.join();
        }
    }
}

impl Fixture {
    fn start() -> Self {
        // Acquire the witness before any setup or dial: counter snapshots
        // in this process are only meaningful while no other fixture's
        // server can run.
        let smtp_counter_witness = SMTP_COUNTER_WITNESS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = tempfile::Builder::new().prefix("c1065-").tempdir().unwrap();
        let certificate = root.path().join("certificate.pem");
        let key = root.path().join("key.pem");
        let ca = root.path().join("ca.pem");
        let ca_key = root.path().join("ca-key.pem");
        let request = root.path().join("request.pem");
        let result = reaper::output(
            Command::new("openssl")
                .args([
                    "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                ])
                .args(["-subj", "/CN=CAD1065 synthetic CA"])
                .args(["-addext", "basicConstraints=critical,CA:TRUE"])
                .arg("-keyout")
                .arg(&ca_key)
                .arg("-out")
                .arg(&ca)
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap();
        assert!(result.status.success(), "isolated TLS CA setup failed");
        let result = reaper::output(
            Command::new("openssl")
                .args(["req", "-new", "-newkey", "rsa:2048", "-nodes"])
                .args([
                    "-subj",
                    "/CN=localhost",
                    "-addext",
                    "subjectAltName=DNS:localhost",
                ])
                .args(["-addext", "basicConstraints=critical,CA:FALSE"])
                .args(["-addext", "extendedKeyUsage=serverAuth"])
                .arg("-keyout")
                .arg(&key)
                .arg("-out")
                .arg(&request)
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap();
        assert!(result.status.success(), "isolated TLS request setup failed");
        let result = reaper::output(
            Command::new("openssl")
                .args([
                    "x509",
                    "-req",
                    "-days",
                    "1",
                    "-copy_extensions",
                    "copy",
                    "-set_serial",
                    "2",
                ])
                .arg("-in")
                .arg(&request)
                .arg("-CA")
                .arg(&ca)
                .arg("-CAkey")
                .arg(&ca_key)
                .arg("-out")
                .arg(&certificate)
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap();
        assert!(result.status.success(), "isolated TLS fixture setup failed");
        let rig = OwnedChild(
            reaper::spawn(
                Command::new("python3")
                    .args(["-c", SMTP_RIG])
                    .arg(root.path())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            )
            .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        while !root.path().join("port").exists() {
            assert!(Instant::now() < deadline, "SMTP fixture did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        let port = std::fs::read_to_string(root.path().join("port"))
            .unwrap()
            .parse()
            .unwrap();
        let mut fixture = Self {
            root,
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
            _rig: rig,
            port,
            board_port: 0,
            _smtp_counter_witness: smtp_counter_witness,
        };
        issue::Pm::init(&fixture.pm()).unwrap();
        assert!(
            matches!(
                platform::custody::Custody::open(&fixture.state()).unwrap(),
                platform::custody::Custody::File(_)
            ),
            "run this owned gate in its isolated File-custody environment"
        );
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", fixture.pm().to_str().unwrap());
        let mut opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(fixture.stop.clone()),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            smtp_test_ca_pem: Some(std::fs::read(ca).unwrap()),
            ..Default::default()
        };
        platform::smtp::attach(&mut opts);
        let state = fixture.state();
        fixture.threads.push(std::thread::spawn(move || {
            daemon::serve_with(&state, opts).unwrap();
        }));
        let deadline = Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(
            &fixture.state(),
            "health",
            json!({}),
            Duration::from_secs(2),
        )
        .is_err()
            || Seam::token_at(&fixture.state()).is_none()
        {
            assert!(Instant::now() < deadline, "fixture daemon did not start");
            std::thread::sleep(Duration::from_millis(30));
        }
        Store::open(&fixture.state().join("cadence.sqlite3"))
            .unwrap()
            .register_agent(&cadence_agent::store::NewAgent {
                alias: "gate-agent",
                provider: "claude",
                endpoint_kind: "managed",
                role: "worker",
                cwd: fixture.root.path().to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        for port in 3110..3200 {
            let (startup, ready) = std::sync::mpsc::channel();
            let opts = cadence_agent::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(fixture.stop.clone()),
                startup: Some(startup),
                test_seam: true,
                ..Default::default()
            };
            let (state, pm) = (fixture.state(), fixture.pm());
            let thread = std::thread::spawn(move || {
                let _ = cadence_agent::ui::serve(&state, &pm, &opts);
            });
            match ready.recv_timeout(Duration::from_secs(20)).unwrap() {
                Ok(()) => {
                    fixture.board_port = port;
                    fixture.threads.push(thread);
                    break;
                }
                Err(_) => thread.join().unwrap(),
            }
        }
        assert_ne!(fixture.board_port, 0, "no isolated board port available");
        fixture
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("s")
    }
    fn pm(&self) -> PathBuf {
        self.root.path().join("pm")
    }

    fn rpc(&self, who: Asserted, params: Value) -> cadence_agent::Result<Value> {
        scoped(who, || {
            client::rpc_timeout(
                &self.state(),
                "connection_test",
                params,
                Duration::from_secs(30),
            )
        })
    }

    fn operator(&self, method: &str, params: Value) -> Value {
        scoped(Asserted::Operator, || {
            client::rpc(&self.state(), method, params)
        })
        .unwrap()
    }

    fn http(
        &self,
        who: &str,
        route: &str,
        body: Value,
        session: Option<(&str, &str)>,
    ) -> (u16, Value, Option<String>) {
        let host = format!("cadence-{}.localhost:{}", self.board_port, self.board_port);
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build();
        let agent: ureq::Agent = config.into();
        let token = Seam::token_at(&self.state()).unwrap();
        let mut request = agent
            .post(format!("http://127.0.0.1:{}{route}", self.board_port))
            .header("Host", &host)
            .header("Origin", format!("http://{host}"))
            .header("X-Cadence-Board", "1")
            .header("Content-Type", "application/json")
            .header(AS_HEADER, who)
            .header(TOKEN_HEADER, token);
        if let Some((cookie, key)) = session {
            request = request
                .header("Cookie", cookie)
                .header("X-Cadence-Session", key);
        }
        let mut response = request.send(body.to_string()).unwrap();
        let status = response.status().as_u16();
        let cookie = response
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap().to_string());
        let text = response.body_mut().read_to_string().unwrap();
        (status, serde_json::from_str(&text).unwrap(), cookie)
    }

    fn session(&self) -> (String, String) {
        operator_auth::ensure_secret(&self.state()).unwrap();
        let secret = operator_auth::read_secret(&self.state()).unwrap();
        let nonce = self.operator(
            "operator_link_mint",
            json!({"secret":secret,"origin":"loopback"}),
        )["nonce"]
            .clone();
        let (status, body, cookie) =
            self.http("operator", "/api/session", json!({"nonce":nonce}), None);
        assert_eq!(status, 200, "actual operator session exchange failed");
        (
            cookie.unwrap(),
            body["session_key"].as_str().unwrap().to_string(),
        )
    }

    fn observed(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.path().join("observed"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn state_snapshot(&self) -> Vec<(String, Vec<String>)> {
        let connection = Connection::open_with_flags(
            self.state().join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut tables = connection.prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        ).unwrap();
        let names: Vec<String> = tables
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        names
            .into_iter()
            .filter(|name| {
                [
                    "connection",
                    "credential",
                    "grant",
                    "binding",
                    "campaign",
                    "smtp",
                ]
                .iter()
                .any(|part| name.contains(part))
            })
            .map(|name| {
                let quoted = name.replace('"', "\"\"");
                let mut statement = connection
                    .prepare(&format!("SELECT * FROM \"{quoted}\""))
                    .unwrap();
                let width = statement.column_count();
                let mut rows: Vec<String> = statement
                    .query_map([], |row| {
                        let values: Vec<rusqlite::types::Value> = (0..width)
                            .map(|i| row.get(i))
                            .collect::<rusqlite::Result<_>>(
                        )?;
                        Ok(format!("{values:?}"))
                    })
                    .unwrap()
                    .map(Result::unwrap)
                    .collect();
                rows.sort();
                (name, rows)
            })
            .collect()
    }
}

fn snapshot_state(state: &Path) -> Vec<(String, Vec<String>)> {
    let connection = Connection::open_with_flags(
        state.join("cadence.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut tables = connection.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    ).unwrap();
    let names: Vec<String> = tables
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    names
        .into_iter()
        .filter(|name| {
            [
                "connection",
                "credential",
                "grant",
                "binding",
                "campaign",
                "smtp",
            ]
            .iter()
            .any(|part| name.contains(part))
        })
        .map(|name| {
            let quoted = name.replace('"', "\"\"");
            let mut statement = connection
                .prepare(&format!("SELECT * FROM \"{quoted}\""))
                .unwrap();
            let width = statement.column_count();
            let mut rows: Vec<String> = statement
                .query_map([], |row| {
                    let values: Vec<rusqlite::types::Value> = (0..width)
                        .map(|i| row.get(i))
                        .collect::<rusqlite::Result<_>>()?;
                    Ok(format!("{values:?}"))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            rows.sort();
            (name, rows)
        })
        .collect()
}

fn assert_receipt(value: &Value, row: &Value) {
    let result = &value["verification"];
    assert_eq!(result["schema"], 1);
    assert_eq!(result["operation"], "smtp-login-no-send-v1");
    assert_eq!(result["connection_id"], row["id"]);
    assert_eq!(result["revision"], row["revision"]);
    assert_eq!(result["registration_digest"], row["registration_digest"]);
    assert_eq!(result["status"], "success");
    assert_eq!(result["network_attempted"], true);
    assert_eq!(result["authentication_verified"], true);
    assert!(result["failure"].is_null());
    for key in [
        "email_sent",
        "delivery_verified",
        "sender_entitlement_verified",
        "execution_authority",
    ] {
        assert_eq!(result[key], false);
    }
    assert!(
        !value.to_string().contains(PASSWORD),
        "receipt published synthetic credential"
    );
}

#[test]
fn cad1065_actual_rpc_and_http_refuse_nonoperator_before_credentials_or_traffic() {
    let fixture = Fixture::start();
    let row = fixture.operator(
        "connection_create",
        json!({
            "provider":"smtp", "account":"gate-sender", "shape":"smtp",
            "host":"localhost", "port":fixture.port, "tls_mode":"implicit",
            "username":"sender@example.test", "secret":PASSWORD, "sender":"sender@example.test",
            "scopes":["email:send"], "accept_same_uid_risk":true,
        }),
    )["connection"]
        .clone();
    assert!(
        row["revision"].as_u64().is_some(),
        "operator enrollment positive control failed"
    );
    let params = json!({
        "connection_id":row["id"], "expected_revision":row["revision"],
        "expected_registration_digest":row["registration_digest"],
    });
    let body = json!({
        "expected_revision":row["revision"], "expected_registration_digest":row["registration_digest"],
    });
    let route = format!("/api/connections/{}/test", row["id"].as_str().unwrap());
    let initial = fixture.state_snapshot();
    let reads = CredentialReads::watch(&fixture.state().join("custody"));
    let dials = platform::smtp::direct_dial_count();
    for who in [Asserted::Agent("gate-agent".into()), Asserted::Unproven] {
        let error = fixture
            .rpc(who, params.clone())
            .expect_err("nonoperator reached verification result");
        assert_eq!(error.kind(), "rejected", "wrong refusal surface");
        assert!(
            error.to_string().contains("operator"),
            "refusal did not exercise operator authority"
        );
        assert!(
            fixture.observed().is_empty(),
            "nonoperator caused provider traffic"
        );
        reads.assert_no_read();
    }
    for who in ["agent:gate-agent", "unproven"] {
        let (status, _, _) = fixture.http(who, &route, body.clone(), None);
        assert_eq!(status, 403, "HTTP nonoperator reached verification handler");
        assert!(
            fixture.observed().is_empty(),
            "HTTP nonoperator caused provider traffic"
        );
        reads.assert_no_read();
    }
    let mut forged = params.clone();
    forged["actor"] = json!("operator");
    assert!(fixture
        .rpc(Asserted::Agent("gate-agent".into()), forged)
        .is_err());
    let (cookie, key) = fixture.session();
    let (status, _, _) = fixture.http(
        "agent:gate-agent",
        &route,
        body.clone(),
        Some((&cookie, &key)),
    );
    assert_eq!(status, 403, "agent inherited operator session authority");
    reads.assert_no_read();
    assert_eq!(
        platform::smtp::direct_dial_count(),
        dials,
        "denied caller dialed SMTP"
    );
    assert!(
        fixture.observed().is_empty(),
        "denied caller contacted provider"
    );
    assert!(
        initial == fixture.state_snapshot(),
        "denied caller changed connection authority/state"
    );
    let answer = fixture.rpc(Asserted::Operator, params).unwrap();
    assert_receipt(&answer, &row);
    reads.assert_read_observed();
    drop(reads);
    let (cookie, key) = fixture.session();
    let (status, answer, _) = fixture.http("operator", &route, body, Some((&cookie, &key)));
    assert_eq!(status, 200, "operator HTTP positive control failed");
    assert_receipt(&answer, &row);
    let observed = fixture.observed();
    assert_eq!(observed.iter().filter(|v| *v == "CONNECT").count(), 2);
    assert_eq!(observed.iter().filter(|v| *v == "AUTH").count(), 2);
    assert_eq!(observed.iter().filter(|v| *v == "QUIT").count(), 2);
    assert!(!observed
        .iter()
        .any(|v| ["MAIL", "RCPT", "DATA", "RIG_FAILURE"].contains(&v.as_str())));
    assert!(
        initial == fixture.state_snapshot(),
        "verification mutated connection authority/state"
    );
}

fn assert_admission_receipt(value: &Value, row: &Value, status: &str, code: &str) {
    assert_eq!(
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        ["verification"].into_iter().collect(),
        "verification response has extension fields"
    );
    let receipt = &value["verification"];
    let expected: std::collections::BTreeSet<&str> = [
        "schema",
        "operation",
        "connection_id",
        "revision",
        "registration_digest",
        "started_at",
        "completed_at",
        "status",
        "network_attempted",
        "authentication_verified",
        "email_sent",
        "delivery_verified",
        "sender_entitlement_verified",
        "execution_authority",
        "failure",
    ]
    .into_iter()
    .collect();
    let actual: std::collections::BTreeSet<&str> = receipt
        .as_object()
        .expect("typed receipt is absent")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(actual, expected, "receipt has missing or extension fields");
    assert_eq!(receipt["schema"], 1);
    assert_eq!(receipt["operation"], "smtp-login-no-send-v1");
    assert_eq!(receipt["connection_id"], row["id"]);
    assert_eq!(receipt["revision"], row["revision"]);
    assert_eq!(receipt["registration_digest"], row["registration_digest"]);
    assert_eq!(receipt["status"], status);
    assert_eq!(receipt["failure"]["code"], code);
    assert!(
        ["admission", "configuration"]
            .contains(&receipt["failure"]["step"].as_str().unwrap_or_default()),
        "wrong pre-network failure stage"
    );
    assert_eq!(
        receipt["failure"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        ["code", "step"].into_iter().collect()
    );
    for key in ["started_at", "completed_at"] {
        assert!(
            receipt[key].as_str().is_some_and(|value| !value.is_empty()),
            "required daemon timestamp is missing"
        );
    }
    for key in [
        "network_attempted",
        "authentication_verified",
        "email_sent",
        "delivery_verified",
        "sender_entitlement_verified",
        "execution_authority",
    ] {
        assert_eq!(receipt[key], false, "admission failure made a false claim");
    }
    assert!(
        !value.to_string().contains(PASSWORD),
        "admission receipt published the synthetic credential"
    );
}

#[test]
fn cad1065_actual_rpc_http_strict_request_and_exact_cas_before_secret_or_traffic() {
    let fixture = Fixture::start();
    let row = fixture.operator(
        "connection_create",
        json!({
            "provider":"smtp", "account":"gate-sender", "shape":"smtp",
            "host":"localhost", "port":fixture.port, "tls_mode":"implicit",
            "username":"sender@example.test", "secret":PASSWORD, "sender":"sender@example.test",
            "scopes":["email:send"], "accept_same_uid_risk":true,
        }),
    )["connection"]
        .clone();
    let builtin = fixture.operator("connection_list", json!({}))["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["provider"] == "local" && row["kind"] == "builtin")
        .expect("credentialless positive-control row is absent")
        .clone();
    let (cookie, key) = fixture.session();
    let initial = fixture.state_snapshot();
    let reads = CredentialReads::watch(&fixture.state().join("custody"));
    let dials = platform::smtp::direct_dial_count();
    let route = format!("/api/connections/{}/test", row["id"].as_str().unwrap());
    let current = json!({
        "connection_id":row["id"], "expected_revision":row["revision"],
        "expected_registration_digest":row["registration_digest"],
    });
    let malformed = [
        ("expected_revision", None),
        ("expected_registration_digest", None),
        ("expected_revision", Some(json!(0))),
        ("expected_revision", Some(json!(-1))),
        ("expected_revision", Some(json!(1.0))),
        (
            "expected_revision",
            Some(json!("cad1065-input-echo-canary")),
        ),
        ("expected_revision", Some(json!(true))),
        ("expected_registration_digest", Some(json!([]))),
        (
            "expected_registration_digest",
            Some(json!("cad1065-input-echo-canary")),
        ),
        (
            "cad1065-field-echo-canary",
            Some(json!("cad1065-input-echo-canary")),
        ),
    ];
    for (field, replacement) in malformed {
        let mut request = current.clone();
        match replacement {
            Some(value) => request[field] = value,
            None => {
                request.as_object_mut().unwrap().remove(field);
            }
        }
        let error = fixture
            .rpc(Asserted::Operator, request.clone())
            .expect_err("malformed request reached verification");
        assert_eq!(error.kind(), "rejected");
        let diagnostic = error.to_string();
        assert!(
            !diagnostic.contains("cad1065-input-echo-canary")
                && !diagnostic.contains("cad1065-field-echo-canary"),
            "RPC malformed diagnostic reflected request data"
        );
        request.as_object_mut().unwrap().remove("connection_id");
        let (status, diagnostic, _) =
            fixture.http("operator", &route, request, Some((&cookie, &key)));
        assert_eq!(status, 400, "HTTP admitted a malformed verification body");
        let diagnostic = diagnostic.to_string();
        assert!(
            !diagnostic.contains("cad1065-input-echo-canary")
                && !diagnostic.contains("cad1065-field-echo-canary"),
            "HTTP malformed diagnostic reflected request data"
        );
        reads.assert_no_read();
        assert_eq!(platform::smtp::direct_dial_count(), dials);
        assert!(fixture.observed().is_empty());
    }
    let (status, _, _) = fixture.http(
        "operator",
        &format!("{route}?cad1065-query-echo-canary=1"),
        json!({
            "expected_revision":row["revision"],
            "expected_registration_digest":row["registration_digest"],
        }),
        Some((&cookie, &key)),
    );
    assert_eq!(status, 400, "HTTP admitted an unapproved query option");
    reads.assert_no_read();
    assert_eq!(platform::smtp::direct_dial_count(), dials);
    assert!(fixture.observed().is_empty());
    let wrong_digest = json!(format!("sha256:{}", "0".repeat(64)));
    assert_ne!(wrong_digest, row["registration_digest"]);
    let stale = [
        (
            json!(row["revision"].as_u64().unwrap().checked_add(1).unwrap()),
            row["registration_digest"].clone(),
        ),
        (Value::Null, row["registration_digest"].clone()),
        (row["revision"].clone(), Value::Null),
        (row["revision"].clone(), wrong_digest.clone()),
        (Value::Null, Value::Null),
    ];
    for (revision, digest) in stale {
        let request = json!({
            "connection_id":row["id"], "expected_revision":revision,
            "expected_registration_digest":digest,
        });
        let result = fixture.rpc(Asserted::Operator, request).unwrap();
        assert_admission_receipt(&result, &row, "stale", "stale_connection");
        let (status, result, _) = fixture.http(
            "operator",
            &route,
            json!({"expected_revision":revision,"expected_registration_digest":digest}),
            Some((&cookie, &key)),
        );
        assert_eq!(status, 200);
        assert_admission_receipt(&result, &row, "stale", "stale_connection");
        reads.assert_no_read();
        assert_eq!(platform::smtp::direct_dial_count(), dials);
        assert!(fixture.observed().is_empty());
    }
    for (revision, expected_status, expected_code) in [
        (
            builtin["revision"].clone(),
            "unsupported",
            "unsupported_provider",
        ),
        (json!(1), "stale", "stale_connection"),
    ] {
        let result = fixture
            .rpc(
                Asserted::Operator,
                json!({
                    "connection_id":builtin["id"], "expected_revision":revision,
                    "expected_registration_digest":builtin["registration_digest"],
                }),
            )
            .unwrap();
        assert_admission_receipt(&result, &builtin, expected_status, expected_code);
        let builtin_route = format!("/api/connections/{}/test", builtin["id"].as_str().unwrap());
        let (status, result, _) = fixture.http(
            "operator",
            &builtin_route,
            json!({
                "expected_revision":revision,
                "expected_registration_digest":builtin["registration_digest"],
            }),
            Some((&cookie, &key)),
        );
        assert_eq!(status, 200);
        assert_admission_receipt(&result, &builtin, expected_status, expected_code);
        reads.assert_no_read();
        assert_eq!(platform::smtp::direct_dial_count(), dials);
        assert!(fixture.observed().is_empty());
    }
    assert!(
        initial == fixture.state_snapshot(),
        "pre-admission calls changed connection authority or state"
    );
    let successful = fixture.rpc(Asserted::Operator, current).unwrap();
    assert_receipt(&successful, &row);
    reads.assert_read_observed();
    let observed = fixture.observed();
    assert_eq!(
        observed.iter().filter(|value| *value == "CONNECT").count(),
        1
    );
    assert_eq!(observed.iter().filter(|value| *value == "AUTH").count(), 1);
    assert_eq!(observed.iter().filter(|value| *value == "QUIT").count(), 1);
    assert!(!observed
        .iter()
        .any(|value| ["MAIL", "RCPT", "DATA", "RIG_FAILURE"].contains(&value.as_str())));
    assert!(
        initial == fixture.state_snapshot(),
        "positive verification changed connection authority or state"
    );
}

fn fixture_with_peer(script: &str) -> Fixture {
    let mut fixture = Fixture::start();
    fixture._rig.0.kill().unwrap();
    fixture._rig.0.wait().unwrap();
    std::fs::remove_file(fixture.root.path().join("port")).unwrap();
    fixture._rig = OwnedChild(
        reaper::spawn(
            Command::new("python3")
                .args(["-c", script])
                .arg(fixture.root.path())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    while !fixture.root.path().join("port").exists() {
        assert!(
            Instant::now() < deadline,
            "controlled SMTP peer did not start"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    fixture.port = std::fs::read_to_string(fixture.root.path().join("port"))
        .unwrap()
        .parse()
        .unwrap();
    fixture
}

#[test]
fn cad1065_actual_secure_probe_checks_quit_and_does_not_publish_peer_echo() {
    for (script, code, step) in [
        (
            SMTP_RIG.replace(
                r#"wire.write(b"221 2.0.0 goodbye\r\n")"#,
                r#"wire.write(b"500 cad1065-synthetic-only-password only-password peer-echo-canary\r\n")"#,
            ),
            "quit_failed",
            "quit",
        ),
        (
            SMTP_RIG.replace(
                r#"wire.write(b"235 2.7.0 authenticated\r\n")"#,
                r#"wire.write(b"535 cad1065-synthetic-only-password only-password peer-echo-canary\r\n")"#,
            ),
            "auth_failed",
            "auth",
        ),
    ] {
        assert_ne!(script, SMTP_RIG, "controlled peer variant was not applied");
        let fixture = fixture_with_peer(&script);
        let row = fixture.operator(
            "connection_create",
            json!({
                "provider":"smtp", "account":"gate-sender", "shape":"smtp",
                "host":"localhost", "port":fixture.port, "tls_mode":"implicit",
                "username":"sender@example.test", "secret":PASSWORD, "sender":"sender@example.test",
                "scopes":["email:send"], "accept_same_uid_risk":true,
            }),
        )["connection"]
            .clone();
        let initial = fixture.state_snapshot();
        let reads = CredentialReads::watch(&fixture.state().join("custody"));
        let result = fixture
            .rpc(
                Asserted::Operator,
                json!({
                    "connection_id":row["id"], "expected_revision":row["revision"],
                    "expected_registration_digest":row["registration_digest"],
                }),
            )
            .unwrap();
        let receipt = &result["verification"];
        assert_eq!(receipt["schema"], 1);
        assert_eq!(receipt["operation"], "smtp-login-no-send-v1");
        assert_eq!(receipt["connection_id"], row["id"]);
        assert_eq!(receipt["revision"], row["revision"]);
        assert_eq!(receipt["registration_digest"], row["registration_digest"]);
        assert_eq!(receipt["status"], "failed");
        assert_eq!(receipt["network_attempted"], true);
        assert_eq!(receipt["failure"]["code"], code);
        assert_eq!(receipt["failure"]["step"], step);
        for field in [
            "authentication_verified",
            "email_sent",
            "delivery_verified",
            "sender_entitlement_verified",
            "execution_authority",
        ] {
            assert_eq!(receipt[field], false);
        }
        let public = result.to_string();
        for canary in [PASSWORD, "only-password", "peer-echo-canary"] {
            assert!(!public.contains(canary), "provider bytes escaped the probe");
        }
        reads.assert_read_observed();
        let observed = fixture.observed();
        assert!(observed.iter().any(|command| command == "CONNECT"));
        assert!(observed.iter().any(|command| command == "AUTH"));
        if step == "quit" {
            assert!(observed.iter().any(|command| command == "QUIT"));
        }
        assert!(!observed
            .iter()
            .any(|value| ["MAIL", "RCPT", "DATA", "RIG_FAILURE"].contains(&value.as_str())));
        assert!(
            initial == fixture.state_snapshot(),
            "failed probe changed connection authority or state"
        );
    }
}

#[test]
fn cad1065_actual_short_coincident_password_remains_login_verifiable() {
    let script = SMTP_RIG.replace(PASSWORD, "success");
    assert_ne!(script, SMTP_RIG);
    let fixture = fixture_with_peer(&script);
    let row = fixture.operator(
        "connection_create",
        json!({
            "provider":"smtp", "account":"gate-sender", "shape":"smtp",
            "host":"localhost", "port":fixture.port, "tls_mode":"implicit",
            "username":"sender@example.test", "secret":"success", "sender":"sender@example.test",
            "scopes":["email:send"], "accept_same_uid_risk":true,
        }),
    )["connection"]
        .clone();
    let initial = fixture.state_snapshot();
    let result = fixture
        .rpc(
            Asserted::Operator,
            json!({
                "connection_id":row["id"], "expected_revision":row["revision"],
                "expected_registration_digest":row["registration_digest"],
            }),
        )
        .unwrap();
    assert_receipt(&result, &row);
    let observed = fixture.observed();
    assert_eq!(
        observed.iter().filter(|value| *value == "CONNECT").count(),
        1
    );
    assert_eq!(observed.iter().filter(|value| *value == "AUTH").count(), 1);
    assert_eq!(observed.iter().filter(|value| *value == "QUIT").count(), 1);
    assert!(!observed
        .iter()
        .any(|value| ["MAIL", "RCPT", "DATA", "RIG_FAILURE"].contains(&value.as_str())));
    assert!(
        initial == fixture.state_snapshot(),
        "short-password probe changed connection authority or state"
    );
}

fn bounds_row(fixture: &Fixture) -> Value {
    fixture.operator(
        "connection_create",
        json!({
            "provider":"smtp", "account":"gate-sender", "shape":"smtp",
            "host":"localhost", "port":fixture.port, "tls_mode":"implicit",
            "username":"sender@example.test", "secret":PASSWORD, "sender":"sender@example.test",
            "scopes":["email:send"], "accept_same_uid_risk":true,
        }),
    )["connection"]
        .clone()
}

fn bounds_request(row: &Value) -> Value {
    json!({
        "connection_id":row["id"], "expected_revision":row["revision"],
        "expected_registration_digest":row["registration_digest"],
    })
}

#[test]
fn cad1065_unknown_identity_is_not_promoted_to_a_trusted_receipt() {
    let fixture = Fixture::start();
    let _row = bounds_row(&fixture);
    let (cookie, key) = fixture.session();
    let unknown = "cad1065-unregistered-input-echo-canary";
    assert!(
        !fixture.operator("connection_list", json!({}))["connections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == unknown)
    );
    let initial = fixture.state_snapshot();
    let reads = CredentialReads::watch(&fixture.state().join("custody"));
    let dials = platform::smtp::direct_dial_count();
    let error = fixture
        .rpc(
            Asserted::Operator,
            json!({
                "connection_id":unknown, "expected_revision":null,
                "expected_registration_digest":null,
            }),
        )
        .expect_err("unregistered request identity was declassified as server-trusted metadata");
    assert_eq!(error.kind(), "rejected");
    assert!(!error.to_string().contains(unknown));
    let (status, result, _) = fixture.http(
        "operator",
        &format!("/api/connections/{unknown}/test"),
        json!({"expected_revision":null,"expected_registration_digest":null}),
        Some((&cookie, &key)),
    );
    assert_eq!(status, 409);
    assert!(!result.to_string().contains(unknown));
    assert!(result.get("verification").is_none());
    reads.assert_no_read();
    assert_eq!(platform::smtp::direct_dial_count(), dials);
    assert!(fixture.observed().is_empty());
    assert!(initial == fixture.state_snapshot());
}

#[test]
fn cad1065_reply_total_bound_and_utf8_refusal_leave_verifier_usable() {
    let greeting = r#"wire.write(b"220 localhost synthetic SMTP\r\n")"#;
    for replacement in [
        r#"wire.write((b"220-" + b"x" * 1024 + b"\r\n") * 70 + b"220 ready\r\n")"#,
        r#"wire.write("\U0001f4a5\r\n".encode("utf-8"))"#,
    ] {
        let conditional = format!(
            "if not (root / \"bad-reply-sent\").exists():\n                    (root / \"bad-reply-sent\").write_text(\"1\")\n                    {replacement}\n                else:\n                    {greeting}"
        );
        let script = SMTP_RIG.replace(greeting, &conditional);
        assert_ne!(script, SMTP_RIG);
        let fixture = fixture_with_peer(&script);
        let row = bounds_row(&fixture);
        let initial = fixture.state_snapshot();
        let result = fixture
            .rpc(Asserted::Operator, bounds_request(&row))
            .expect(
                "malformed provider reply crashed the request instead of returning a typed failure",
            );
        let receipt = &result["verification"];
        assert_eq!(receipt["status"], "failed");
        assert_eq!(receipt["network_attempted"], true);
        assert_eq!(receipt["authentication_verified"], false);
        assert_eq!(receipt["email_sent"], false);
        let observed = fixture.observed();
        assert!(!observed
            .iter()
            .any(|command| ["AUTH", "MAIL", "RCPT", "DATA"].contains(&command.as_str())));
        let successful = fixture
            .rpc(Asserted::Operator, bounds_request(&row))
            .expect("malformed reply left the custody guard poisoned");
        assert_receipt(&successful, &row);
        let observed = fixture.observed();
        assert_eq!(
            observed.iter().filter(|value| *value == "CONNECT").count(),
            2
        );
        assert_eq!(observed.iter().filter(|value| *value == "AUTH").count(), 1);
        assert_eq!(observed.iter().filter(|value| *value == "QUIT").count(), 1);
        assert!(!observed
            .iter()
            .any(|command| ["MAIL", "RCPT", "DATA"].contains(&command.as_str())));
        assert!(initial == fixture.state_snapshot());
    }
}

#[test]
fn cad1065_tls_drip_cannot_reset_the_total_operation_budget() {
    let script = r#"
import pathlib, socket, ssl, sys, time
root = pathlib.Path(sys.argv[1])
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(root / "certificate.pem", root / "key.pem")
listener = socket.socket()
listener.bind(("127.0.0.1", 0))
listener.listen(8)
(root / "port").write_text(str(listener.getsockname()[1]))
while True:
    raw, _ = listener.accept()
    with (root / "observed").open("a") as out:
        out.write("CONNECT\n")
    raw.settimeout(5)
    try:
        incoming, outgoing = ssl.MemoryBIO(), ssl.MemoryBIO()
        peer = ctx.wrap_bio(incoming, outgoing, server_side=True)
        packet = b""
        while not packet:
            data = raw.recv(65536)
            assert data
            incoming.write(data)
            try:
                peer.do_handshake()
            except ssl.SSLWantReadError:
                pass
            packet = outgoing.read()
        started = time.monotonic()
        index = 0
        while time.monotonic() - started < 30:
            raw.sendall(packet[index:index + 1])
            index = (index + 1) % len(packet)
            time.sleep(1)
    except (BrokenPipeError, ConnectionResetError):
        pass
    finally:
        raw.close()
"#;
    let fixture = fixture_with_peer(script);
    let row = bounds_row(&fixture);
    let initial = fixture.state_snapshot();
    let started = Instant::now();
    let result = fixture
        .rpc(Asserted::Operator, bounds_request(&row))
        .expect("TLS drip did not return an operation timeout receipt");
    assert!(
        started.elapsed() < Duration::from_secs(25),
        "TLS progress reset the fixed twenty-second budget"
    );
    let receipt = &result["verification"];
    assert_eq!(receipt["status"], "failed");
    assert_eq!(receipt["failure"]["code"], "timeout");
    assert_eq!(receipt["failure"]["step"], "tls");
    assert_eq!(receipt["network_attempted"], true);
    for field in [
        "authentication_verified",
        "email_sent",
        "delivery_verified",
        "sender_entitlement_verified",
        "execution_authority",
    ] {
        assert_eq!(receipt[field], false);
    }
    assert_eq!(fixture.observed(), vec!["CONNECT"]);
    assert!(initial == fixture.state_snapshot());
}

// Keep process-environment changes out of the rest of this integration-test
// binary: the outer invocation re-execs only this test with an isolated
// secret-tool and D-Bus marker in its environment.
#[test]
fn cad1065_libsecret_cleanup_pending_is_bounded_and_fences_verification() {
    const CHILD: &str = "CAD1065_CLEANUP_GATE_CHILD";
    if std::env::var_os(CHILD).is_some() {
        cad1065_libsecret_cleanup_pending_child();
        return;
    }

    let tools = tempfile::Builder::new()
        .prefix("c1065-cleanup-tool-")
        .tempdir()
        .unwrap();
    let tool = tools.path().join("secret-tool");
    let script = r#"#!/bin/sh
case "$1" in
  store)
    /usr/bin/python3 -c 'import os; open(os.environ["CAD1065_CLEANUP_TOOL_DIR"] + "/stored", "wb").write(os.read(0, 4096))'
    exit 0
    ;;
  lookup)
    echo lookup >> "$CAD1065_CLEANUP_TOOL_DIR/calls"
    if [ ! -e "$CAD1065_CLEANUP_TOOL_DIR/start-test" ] || [ -e "$CAD1065_CLEANUP_TOOL_DIR/allow-custody-conflict" ]; then
      cat "$CAD1065_CLEANUP_TOOL_DIR/stored"
      exit 0
    fi
    echo "$$" > "$CAD1065_CLEANUP_TOOL_DIR/lookup.pid"
    while [ ! -e "$CAD1065_CLEANUP_TOOL_DIR/release" ]; do /bin/sleep 0.01; done
    /usr/bin/head -c 20000 /dev/zero
    /bin/sleep 60
    ;;
  clear)
    rm -f "$CAD1065_CLEANUP_TOOL_DIR/stored"
    exit 0
    ;;
  *) exit 1 ;;
esac
"#;
    std::fs::write(&tool, script).unwrap();
    std::fs::write(tools.path().join("calls"), "").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tool, PermissionsExt::from_mode(0o755)).unwrap();

    let path = format!(
        "{}:{}",
        tools.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut isolated = std::process::Command::new(std::env::current_exe().unwrap());
    isolated
        .args([
            "--exact",
            "cad1065_libsecret_cleanup_pending_is_bounded_and_fences_verification",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env("CAD1065_CLEANUP_TOOL_DIR", tools.path())
        .env("PATH", path)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/cad1065-test-bus");
    let output =
        reaper::status(&mut isolated).expect("could not start isolated cleanup-gate process");
    assert!(
        output.success(),
        "isolated libsecret cleanup gate failed: {output}"
    );
}

fn cad1065_libsecret_cleanup_pending_child() {
    let tool_dir = PathBuf::from(std::env::var_os("CAD1065_CLEANUP_TOOL_DIR").unwrap());
    let root = tempfile::Builder::new()
        .prefix("c1065-cleanup-")
        .tempdir()
        .unwrap();
    let state = root.path().join("state");
    let pm = root.path().join("pm");
    issue::Pm::init(&pm).unwrap();
    assert!(matches!(
        platform::custody::Custody::open(&state).unwrap(),
        platform::custody::Custody::Libsecret(_)
    ));

    let provider_env = cadence_agent::adapter::ProviderEnv::refusing_providers();
    provider_env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let mut opts = daemon::ServeOptions {
        provider_env,
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
    platform::smtp::attach(&mut opts);
    let daemon_state = state.clone();
    let daemon_thread = std::thread::spawn(move || {
        daemon::serve_with(&daemon_state, opts).unwrap();
    });
    let _daemon_guard = CleanupGateDaemon {
        stop,
        thread: Some(daemon_thread),
    };
    let startup_deadline = Instant::now() + Duration::from_secs(30);
    while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(2)).is_err()
        || Seam::token_at(&state).is_none()
    {
        assert!(
            Instant::now() < startup_deadline,
            "cleanup-gate daemon did not start"
        );
        std::thread::sleep(Duration::from_millis(30));
    }

    let row = scoped(Asserted::Operator, || {
        client::rpc(
            &state,
            "connection_create",
            json!({
                "provider":"smtp", "account":"cleanup-gate-sender", "shape":"smtp",
                "host":"mail.example.com", "port":465, "tls_mode":"implicit",
                "username":"sender@example.test", "secret":PASSWORD,
                "sender":"sender@example.test", "scopes":["email:send"],
                "accept_same_uid_risk":true,
            }),
        )
    })
    .unwrap()["connection"]
        .clone();
    let params = json!({
        "connection_id":row["id"], "expected_revision":row["revision"],
        "expected_registration_digest":row["registration_digest"],
    });
    let initial = snapshot_state(&state);
    let lookups_before_test = std::fs::read_to_string(tool_dir.join("calls"))
        .unwrap_or_default()
        .lines()
        .count();
    assert!(
        lookups_before_test > 0,
        "connection projection never read libsecret"
    );
    let request_state = state.clone();
    let request_params = params.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let request_started = Instant::now();
    std::fs::write(tool_dir.join("start-test"), b"go").unwrap();
    let request = std::thread::spawn(move || {
        let answer = scoped(Asserted::Operator, || {
            client::rpc_timeout(
                &request_state,
                "connection_test",
                request_params,
                Duration::from_secs(30),
            )
        });
        let _ = tx.send(answer);
    });

    let pid_path = tool_dir.join("lookup.pid");
    let startup_deadline = Instant::now() + Duration::from_secs(5);
    while !pid_path.exists() {
        assert!(
            Instant::now() < startup_deadline,
            "secret-tool lookup did not start"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid: libc::pid_t = std::fs::read_to_string(&pid_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(pid > 0, "secret-tool published an invalid pid");
    let mut trace_guard = CleanupGateTrace {
        pid,
        tool_dir: tool_dir.clone(),
        armed: true,
    };
    let seized = unsafe {
        libc::ptrace(
            libc::PTRACE_SEIZE,
            pid,
            std::ptr::null_mut::<libc::c_void>(),
            libc::PTRACE_O_TRACEEXIT as usize as *mut libc::c_void,
        )
    };
    assert_eq!(
        seized, 0,
        "could not hold the owned secret-tool child for cleanup"
    );
    std::fs::write(tool_dir.join("release"), b"go").unwrap();

    // The stand-in emits more than the bounded material cap and then stays
    // alive. ptrace holds its SIGKILL exit stop so cleanup cannot be observed
    // until this test releases it; this exercises the actual owned-child path.
    let result = rx.recv_timeout(Duration::from_secs(23));
    let elapsed = request_started.elapsed();
    if result.is_err() {
        // Let a broken/unbounded implementation finish so the test process
        // does not strand the daemon request or its child.
        unsafe {
            libc::ptrace(
                libc::PTRACE_CONT,
                pid,
                std::ptr::null_mut::<libc::c_void>(),
                std::ptr::null_mut::<libc::c_void>(),
            );
        }
        let _ = rx.recv_timeout(Duration::from_secs(5));
        let _ = request.join();
        panic!("cleanup-pending verification did not return within the shared budget");
    }
    let tracee_path = Path::new("/proc").join(pid.to_string());
    assert!(
        tracee_path.exists(),
        "cleanup returned without observing the held child"
    );
    assert!(
        elapsed < Duration::from_secs(23),
        "cleanup-pending verification exceeded the shared deadline"
    );
    assert!(request.join().is_ok());
    let first = result
        .unwrap()
        .expect_err("cleanup-pending lookup returned a typed receipt");
    assert_eq!(
        first.kind(),
        "busy",
        "cleanup-pending result was not fixed busy"
    );
    assert!(
        first
            .to_string()
            .contains("connection test is fenced pending credential cleanup"),
        "cleanup-pending error was not the fixed fence refusal"
    );
    assert!(
        !first.to_string().contains(PASSWORD),
        "busy refusal echoed the credential"
    );
    assert!(
        !first.to_string().contains("verification"),
        "cleanup-pending result exposed a typed receipt"
    );

    // The fence persists: a second verifier is refused without another
    // lookup. This alone does not prove custody serialization, so also
    // start an actual credential revoke while the owned lookup is still
    // held at its exit stop.
    let second = scoped(Asserted::Operator, || {
        client::rpc_timeout(
            &state,
            "connection_test",
            params.clone(),
            Duration::from_secs(2),
        )
    })
    .expect_err("cleanup fence allowed a later verifier call");
    assert_eq!(second.kind(), "busy");
    assert!(second
        .to_string()
        .contains("connection test is fenced pending credential cleanup"));
    let calls_path = tool_dir.join("calls");
    assert_eq!(
        std::fs::read_to_string(&calls_path)
            .unwrap()
            .lines()
            .count(),
        lookups_before_test + 1,
        "fenced follow-up spawned another secret-tool lookup"
    );

    // If revoke bypasses the retained serialization, its lookup can return
    // promptly and the test observes completion while cleanup is unobserved.
    // Allow that lookup only after this point; the over-cap child remains
    // independently ptrace-held until the explicit continue below.
    std::fs::write(tool_dir.join("allow-custody-conflict"), b"go").unwrap();
    let conflict_state = state.clone();
    let conflict_id = params["connection_id"].clone();
    let (conflict_started_tx, conflict_started_rx) = std::sync::mpsc::channel();
    let (conflict_tx, conflict_rx) = std::sync::mpsc::channel();
    let conflict = std::thread::spawn(move || {
        let _ = conflict_started_tx.send(());
        let result = scoped(Asserted::Operator, || {
            client::rpc_timeout(
                &conflict_state,
                "connection_revoke",
                json!({"connection_id": conflict_id}),
                Duration::from_secs(8),
            )
        });
        let _ = conflict_tx.send(result);
    });
    conflict_started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("conflicting revoke thread did not start");
    match conflict_rx.recv_timeout(Duration::from_millis(500)) {
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        Ok(result) => panic!(
            "queued credential revoke completed or failed before cleanup was observed: {result:?}"
        ),
        Err(error) => panic!("queued credential revoke disconnected before release: {error}"),
    }
    assert_eq!(
        std::fs::read_to_string(&calls_path)
            .unwrap()
            .lines()
            .count(),
        lookups_before_test + 1,
        "conflicting revoke reached custody while the lookup was unobserved"
    );
    assert!(
        initial == snapshot_state(&state),
        "conflicting revoke mutated credential state while cleanup was unobserved"
    );

    // Continue and detach from the ptrace-held exit. Releasing the tracer
    // lets the cleanup owner observe the terminal wait status, reap this
    // child, and release custody serialization.
    let continued = unsafe {
        libc::ptrace(
            libc::PTRACE_DETACH,
            pid,
            std::ptr::null_mut::<libc::c_void>(),
            std::ptr::null_mut::<libc::c_void>(),
        )
    };
    assert_eq!(
        continued, 0,
        "could not detach and release the cleanup-held child"
    );
    trace_guard.armed = false;

    let reap_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match std::fs::metadata(&tracee_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Ok(_) => {
                let stat = std::fs::read_to_string(tracee_path.join("stat")).unwrap_or_default();
                let state = stat.rsplit_once(") ").map(|(_, rest)| rest.chars().next());
                assert!(
                    Instant::now() < reap_deadline,
                    "cleanup owner did not reap the released child; /proc state: {state:?}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("could not observe cleanup child in procfs: {error}"),
        }
    }

    // The same revoke that was queued during cleanup must now acquire custody
    // and finish, proving serialization is released after positive reaping.
    let revoked = conflict_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("queued custody revoke stayed blocked after cleanup reap")
        .expect("credential revoke failed after cleanup reap");
    assert_eq!(revoked["revoked"], true);
    assert!(conflict.join().is_ok());
    assert_ne!(
        initial,
        snapshot_state(&state),
        "completed revoke did not mutate connection/credential state"
    );

    // Even after the cleanup child is released, the timed-out verification
    // is fenced and cannot manufacture a typed receipt or spawn another
    // secret-tool lookup.
    let fenced_followup = scoped(Asserted::Operator, || {
        client::rpc_timeout(&state, "connection_test", params, Duration::from_secs(2))
    })
    .expect_err("cleanup fence allowed a later verifier call");
    assert_eq!(fenced_followup.kind(), "busy");
    assert!(fenced_followup
        .to_string()
        .contains("connection test is fenced pending credential cleanup"));
    assert_eq!(
        std::fs::read_to_string(&calls_path)
            .unwrap()
            .lines()
            .count(),
        lookups_before_test + 2,
        "queued revoke or fenced follow-up spawned an unexpected secret-tool lookup"
    );
}

struct CleanupGateTrace {
    pid: libc::pid_t,
    tool_dir: PathBuf,
    armed: bool,
}

impl Drop for CleanupGateTrace {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = std::fs::write(self.tool_dir.join("release"), b"go");
        unsafe {
            let _ = libc::ptrace(
                libc::PTRACE_CONT,
                self.pid,
                std::ptr::null_mut::<libc::c_void>(),
                std::ptr::null_mut::<libc::c_void>(),
            );
        }
    }
}

struct CleanupGateDaemon {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for CleanupGateDaemon {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
fn cad1065_reply_wire_framing_counts_toward_total_bound() {
    let greeting = r#"wire.write(b"220 localhost synthetic SMTP\r\n")"#;
    let oversized = r#"wire.write((b"220-" + b" " * 4090 + b"\r\n") * 99 + b"220 ready\r\n")"#;
    let conditional = format!(
        "if not (root / \"oversized-greeting-sent\").exists():\n                    (root / \"oversized-greeting-sent\").write_text(\"1\")\n                    {oversized}\n                else:\n                    {greeting}"
    );
    let script = SMTP_RIG.replace(greeting, &conditional);
    assert_ne!(
        script, SMTP_RIG,
        "oversized greeting variant was not applied"
    );
    let fixture = fixture_with_peer(&script);
    let row = bounds_row(&fixture);
    let initial = fixture.state_snapshot();

    // 99 individually legal 4096-byte continuation lines plus a short final
    // line exceed the 64 KiB wire-reply cap. Leading spaces are framing that
    // normalization removes, so the guard must count raw bytes before parsing
    // or trimming the SMTP payload.
    let refused = fixture
        .rpc(Asserted::Operator, bounds_request(&row))
        .expect("framing-only oversized greeting was accepted");
    let receipt = &refused["verification"];
    assert_eq!(receipt["status"], "failed");
    assert_eq!(receipt["network_attempted"], true);
    assert_eq!(receipt["authentication_verified"], false);
    assert_eq!(receipt["failure"]["code"], "tls_failed");
    assert_eq!(receipt["failure"]["step"], "tls");
    for field in [
        "email_sent",
        "delivery_verified",
        "sender_entitlement_verified",
        "execution_authority",
    ] {
        assert_eq!(receipt[field], false);
    }
    assert_eq!(
        fixture.observed(),
        vec!["CONNECT"],
        "oversized greeting was accepted far enough to send EHLO/AUTH"
    );

    // The malformed greeting is refused without poisoning subsequent checks.
    let successful = fixture
        .rpc(Asserted::Operator, bounds_request(&row))
        .expect("oversized reply left the verifier unusable");
    assert_receipt(&successful, &row);
    let observed = fixture.observed();
    assert_eq!(
        observed.iter().filter(|value| *value == "CONNECT").count(),
        2
    );
    assert_eq!(observed.iter().filter(|value| *value == "EHLO").count(), 1);
    assert_eq!(observed.iter().filter(|value| *value == "AUTH").count(), 1);
    assert_eq!(observed.iter().filter(|value| *value == "QUIT").count(), 1);
    assert!(!observed
        .iter()
        .any(|command| ["MAIL", "RCPT", "DATA"].contains(&command.as_str())));
    assert!(initial == fixture.state_snapshot());
}
struct Cad1185CompetingFixtureResult {
    row: Value,
    receipt: Value,
    observed: Vec<String>,
}

#[test]
fn cad1185_competing_fixture_waits_for_zero_dial_measurement_owner() {
    let fixture = Fixture::start();
    let row = bounds_row(&fixture);
    let route = format!("/api/connections/{}/test", row["id"].as_str().unwrap());
    let reads = CredentialReads::watch(&fixture.state().join("custody"));
    let dials = platform::smtp::direct_dial_count();
    let initial = fixture.state_snapshot();

    let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let competing = std::thread::spawn(move || {
        let _ = attempt_tx.send(());
        let fixture = Fixture::start();
        let _ = ready_tx.send(());
        let row = bounds_row(&fixture);
        let answer = fixture
            .rpc(Asserted::Operator, bounds_request(&row))
            .unwrap();
        let outcome = Cad1185CompetingFixtureResult {
            row,
            receipt: answer,
            observed: fixture.observed(),
        };
        drop(fixture);
        let _ = done_tx.send(outcome);
    });

    let attempt_observed = attempt_rx.recv_timeout(Duration::from_secs(5)).is_ok();
    // A timeout here is only a provisional observation: after releasing A we
    // still require B's explicit ready signal and successful SMTP completion.
    // On the unfixed lifecycle B's real ready signal arrives during this
    // window, before the measurement owner is released.
    let became_ready_while_owned = ready_rx.recv_timeout(Duration::from_secs(20)).is_ok();

    let rpc_refused = fixture
        .rpc(Asserted::Unproven, json!({"connection_id":row["id"]}))
        .is_err_and(|error| error.kind() == "rejected");
    let (http_status, _, _) = fixture.http(
        "unproven",
        &route,
        json!({"unexpected_field":"malformed"}),
        None,
    );
    let no_credential_read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        reads.assert_no_read();
    }))
    .is_ok();
    let no_provider_traffic = fixture.observed().is_empty();
    let dial_count_unchanged = platform::smtp::direct_dial_count() == dials;
    let state_unchanged = fixture.state_snapshot() == initial;
    drop(reads);
    drop(fixture);

    // Always release A and reap B before making assertions, including the
    // expected RED on the old lifecycle when B announced readiness too soon.
    let competing_result = done_rx.recv_timeout(Duration::from_secs(90));
    let worker_joined = competing.join().is_ok();
    let completed_after_release = competing_result.is_ok();
    let result = competing_result.ok();

    assert!(
        attempt_observed,
        "competing fixture did not announce its attempt"
    );
    assert!(
        completed_after_release && worker_joined,
        "competing fixture did not start and finish after owner release"
    );
    assert!(
        !became_ready_while_owned,
        "competing fixture became ready while the zero-dial owner was active"
    );
    assert!(rpc_refused, "A's malformed RPC request was not rejected");
    assert_eq!(
        http_status, 403,
        "A's malformed HTTP request was not rejected"
    );
    assert!(
        no_credential_read,
        "A's denied requests accessed credential storage"
    );
    assert!(
        no_provider_traffic,
        "A's denied requests contacted the SMTP rig"
    );
    assert!(
        dial_count_unchanged,
        "A's denied requests incremented SMTP dials"
    );
    assert!(
        state_unchanged,
        "A's denied requests changed connection state"
    );

    let result = result.unwrap();
    assert_receipt(&result.receipt, &result.row);
    assert_eq!(
        result
            .observed
            .iter()
            .filter(|value| *value == "CONNECT")
            .count(),
        1
    );
    assert_eq!(
        result
            .observed
            .iter()
            .filter(|value| *value == "AUTH")
            .count(),
        1
    );
    assert_eq!(
        result
            .observed
            .iter()
            .filter(|value| *value == "QUIT")
            .count(),
        1
    );
    assert!(!result
        .observed
        .iter()
        .any(|value| ["MAIL", "RCPT", "DATA", "RIG_FAILURE"].contains(&value.as_str())));
}
