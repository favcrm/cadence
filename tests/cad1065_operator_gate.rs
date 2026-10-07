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
use std::sync::Arc;
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

struct Fixture {
    _rig: OwnedChild,
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    port: u16,
    board_port: u16,
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
