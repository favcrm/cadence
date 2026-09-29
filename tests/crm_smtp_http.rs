//! CAD-785 over the board: operator SMTP enrollment, binding and
//! test sends through the strict HTTP peer; stolen sessions replayed
//! from an agent pane (plain and detached) get 403 on every route.
//!
//! The isolated TLS rig from `crm_smtp_send.rs` is reused in
//! miniature: one implicit-TLS sender proves the operator test-send
//! path end to end over HTTP. No live network, no customer data.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_member_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::Duration;

const SECRET: &str = "4mD8-wQ2x-Zk9p-Vj5t-71";

struct Board {
    root: tempfile::TempDir,
    daemon: TestDaemon,
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
}

impl Board {
    fn new(ca_pem: Option<Vec<u8>>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let mut opts = daemon_opts();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        opts.platforms.insert(
            "fixture".into(),
            Arc::new(cadence_agent::contract_fixture::FakePlatform::standard()),
        );
        cadence_agent::platform::smtp::attach(&mut opts);
        opts.smtp_test_ca_pem = ca_pem;
        let daemon = TestDaemon::start_opts(opts);
        // Install the CRM source app for installation/context scope.
        let source = root.path().join("source");
        for name in [
            "app.md",
            "workflows/blog-post.md",
            "rubrics/blog.md",
            "templates/brief.md",
            "templates/post.md",
        ] {
            let destination = source.join(name);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::copy(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("apps/blog-post")
                    .join(name),
                &destination,
            )
            .unwrap();
        }
        let stop = Arc::new(AtomicBool::new(false));
        for _ in 0..20 {
            let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = probe.local_addr().unwrap().port();
            drop(probe);
            let (startup, ready) = std::sync::mpsc::channel();
            let opts = cadence_agent::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(stop.clone()),
                test_seam: cfg!(feature = "test-seam"),
                startup: Some(startup),
                ..Default::default()
            };
            let state = daemon.state.clone();
            let pm_dir = pm.dir.clone();
            let thread =
                std::thread::spawn(move || cadence_agent::ui::serve(&state, &pm_dir, &opts));
            match ready
                .recv_timeout(Duration::from_secs(10))
                .expect("board startup timed out")
            {
                Ok(()) => {
                    return Self {
                        root,
                        daemon,
                        port,
                        stop,
                        thread: Some(thread),
                    };
                }
                Err(std::io::ErrorKind::AddrInUse) => {
                    thread.join().unwrap().unwrap_err();
                }
                Err(kind) => panic!("board failed to start on {port}: {kind}"),
            }
        }
        panic!("board could not reserve an ephemeral port after 20 attempts")
    }

    fn operator(&self, method: &str, path: &str, body: &str) -> (u16, String) {
        let session =
            common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &self.daemon.state, self.port);
        let (code, _, text) = common::op::raw(self.port, &session.request(method, path, body));
        assert!(!text.contains(SECRET), "credential leaked in HTTP response");
        (code, text)
    }

    fn value(&self, method: &str, path: &str, body: Value) -> Value {
        let encoded = body.to_string();
        let (code, text) = self.operator(method, path, &encoded);
        assert_eq!(code, 200, "operator {method} {path}: {text}");
        serde_json::from_str(&text).unwrap()
    }

    fn setup(&self, port: u16) -> (String, String, String) {
        let install = self
            .daemon
            .operator_rpc(
                "app_workspace_install",
                json!({"source": self.root.path().join("source")}),
            )
            .unwrap()["install_id"]
            .as_str()
            .unwrap()
            .to_string();
        let context = self
            .daemon
            .operator_rpc(
                "app_context_create",
                json!({"install_id": install, "label": "brand", "input_defaults": {}, "request_id": "ctx-1"}),
            )
            .unwrap()["context"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        self.daemon
            .operator_rpc(
                "app_content_save",
                json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                    "subject": "Spring launch", "preheader": "News", "blocks": [
                        {"type": "heading", "text": "Hello {{first_name|friend}}"},
                        {"type": "paragraph", "text": "A calm first line."},
                        {"type": "button", "label": "Read more", "url": "https://example.com/posts/welcome"},
                    ]}),
            )
            .unwrap();
        let connection = self.value(
            "POST",
            "/api/connections",
            json!({"provider": "smtp", "account": "http-smtp", "shape": "smtp",
                    "host": "localhost", "port": port, "tls_mode": "implicit",
                    "username": "smtp-user", "secret": SECRET,
                    "sender": "news@example.com", "sender_name": "CRM News",
                    "scopes": ["email:send"], "accept_same_uid_risk": true}),
        )["connection"]
            .clone();
        assert_eq!(connection["smtp"]["sender"], "news@example.com");
        assert!(connection.get("secret").is_none());
        let id = connection["id"].as_str().unwrap().to_string();
        (install, context, id)
    }
}

impl Drop for Board {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            if !std::thread::panicking() {
                result.unwrap().unwrap();
            }
        }
    }
}

// ---------- miniature isolated TLS rig ----------

fn openssl(args: &[&str]) {
    let status = std::process::Command::new("openssl")
        .args(args)
        .output()
        .expect("openssl is required for the isolated SMTP rig");
    assert!(
        status.status.success(),
        "openssl {args:?} failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}

fn mint_ca(dir: &std::path::Path) {
    openssl(&[
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        dir.join("ca-key.pem").to_str().unwrap(),
        "-out",
        dir.join("ca.pem").to_str().unwrap(),
        "-days",
        "2",
        "-subj",
        "/CN=cadence-smtp-test-ca",
    ]);
}

fn mint_server(dir: &std::path::Path) {
    std::fs::write(
        dir.join("ext.cnf"),
        "subjectAltName=DNS:localhost,IP:127.0.0.1\n",
    )
    .unwrap();
    openssl(&[
        "req",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        dir.join("srv-key.pem").to_str().unwrap(),
        "-out",
        dir.join("srv.csr").to_str().unwrap(),
        "-subj",
        "/CN=localhost",
    ]);
    openssl(&[
        "x509",
        "-req",
        "-in",
        dir.join("srv.csr").to_str().unwrap(),
        "-CA",
        dir.join("ca.pem").to_str().unwrap(),
        "-CAkey",
        dir.join("ca-key.pem").to_str().unwrap(),
        "-CAcreateserial",
        "-days",
        "2",
        "-out",
        dir.join("srv-cert.pem").to_str().unwrap(),
        "-extfile",
        dir.join("ext.cnf").to_str().unwrap(),
    ]);
}

fn server_config(dir: &std::path::Path) -> Arc<rustls::ServerConfig> {
    let cert_pem = std::fs::read(dir.join("srv-cert.pem")).unwrap();
    let key_pem = std::fs::read(dir.join("srv-key.pem")).unwrap();
    let certs: Vec<_> = rustls_pemfile::certs(&mut cert_pem.as_ref())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = rustls_pemfile::private_key(&mut key_pem.as_ref())
        .unwrap()
        .unwrap();
    Arc::new(
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap(),
    )
}

type RigReader = BufReader<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>;

fn rig_write(reader: &mut RigReader, text: &str) -> bool {
    reader
        .get_mut()
        .write_all(format!("{text}\r\n").as_bytes())
        .and_then(|()| reader.get_mut().flush())
        .is_ok()
}

fn rig_read(reader: &mut RigReader) -> Option<String> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => None,
        Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_string()),
        Err(_) => None,
    }
}

struct Rig {
    port: u16,
    done: mpsc::Receiver<RigOutcome>,
}

struct RigOutcome {
    mail_from: Option<String>,
    rcpts: usize,
    bytes: usize,
}

impl Rig {
    fn start(dir: tempfile::TempDir) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = server_config(dir.path());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _dir = dir;
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
            stream.set_write_timeout(Some(Duration::from_secs(30))).ok();
            let conn = rustls::ServerConnection::new(config).unwrap();
            let tls = rustls::StreamOwned::new(conn, stream);
            let mut reader = BufReader::new(tls);
            let mut outcome = RigOutcome {
                mail_from: None,
                rcpts: 0,
                bytes: 0,
            };
            if !rig_write(&mut reader, "220 localhost cadence-test-smtp") {
                let _ = tx.send(outcome);
                return;
            }
            let mut authed = false;
            while let Some(line) = rig_read(&mut reader) {
                let head = line
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_ascii_uppercase();
                match head.as_str() {
                    "EHLO" | "HELO" => {
                        rig_write(&mut reader, "250-localhost");
                        rig_write(&mut reader, "250 AUTH LOGIN PLAIN");
                    }
                    "AUTH" => {
                        // AUTH LOGIN <user-b64> is not offered; the
                        // client uses the two-step challenge form or
                        // PLAIN with an initial response.
                        let rest = line[4..].trim();
                        let mut parts = rest.splitn(2, ' ');
                        let mechanism = parts.next().unwrap_or("").to_ascii_uppercase();
                        let initial = parts.next().unwrap_or("");
                        use base64::{engine::general_purpose::STANDARD, Engine};
                        let decode = |text: &str| {
                            STANDARD
                                .decode(text.trim())
                                .ok()
                                .and_then(|bytes| String::from_utf8(bytes).ok())
                                .unwrap_or_default()
                        };
                        let (user, pass) = match mechanism.as_str() {
                            "LOGIN" => {
                                if !rig_write(&mut reader, "334 VXNlcm5hbWU6") {
                                    break;
                                }
                                let Some(user) = rig_read(&mut reader) else {
                                    break;
                                };
                                if !rig_write(&mut reader, "334 UGFzc3dvcmQ6") {
                                    break;
                                }
                                let Some(pass) = rig_read(&mut reader) else {
                                    break;
                                };
                                (decode(&user), decode(&pass))
                            }
                            "PLAIN" => {
                                let decoded = decode(initial);
                                let mut fields = decoded.split('\0');
                                let _ = fields.next();
                                (
                                    fields.next().unwrap_or("").to_string(),
                                    fields.next().unwrap_or("").to_string(),
                                )
                            }
                            _ => {
                                rig_write(&mut reader, "504 unrecognized");
                                continue;
                            }
                        };
                        if user == "smtp-user" && pass == SECRET {
                            authed = true;
                            rig_write(&mut reader, "235 ok");
                        } else {
                            rig_write(&mut reader, "535 refused");
                        }
                    }
                    _ if line.to_ascii_uppercase().starts_with("MAIL FROM:") => {
                        if !authed {
                            rig_write(&mut reader, "530 authentication required");
                        } else {
                            outcome.mail_from = line
                                .find('<')
                                .and_then(|s| line.find('>').map(|e| line[s + 1..e].to_string()));
                            rig_write(&mut reader, "250 ok");
                        }
                    }
                    _ if line.to_ascii_uppercase().starts_with("RCPT TO:") => {
                        if !authed {
                            rig_write(&mut reader, "530 authentication required");
                        } else {
                            outcome.rcpts += 1;
                            rig_write(&mut reader, "250 ok");
                        }
                    }
                    "DATA" => {
                        if !authed || outcome.rcpts == 0 {
                            rig_write(&mut reader, "503 bad sequence");
                        } else if !rig_write(&mut reader, "354 go") {
                            break;
                        } else {
                            while let Some(data) = rig_read(&mut reader) {
                                if data == "." {
                                    break;
                                }
                                outcome.bytes += data.len();
                            }
                            rig_write(&mut reader, "250 accepted");
                        }
                    }
                    "QUIT" => {
                        rig_write(&mut reader, "221 bye");
                        break;
                    }
                    "RSET" | "NOOP" => {
                        rig_write(&mut reader, "250 ok");
                    }
                    _ => {
                        rig_write(&mut reader, "502 unimplemented");
                    }
                }
            }
            let _ = tx.send(outcome);
        });
        Self { port, done: rx }
    }

    fn outcome(self) -> RigOutcome {
        self.done
            .recv_timeout(Duration::from_secs(90))
            .expect("fake sender saw no session")
    }
}

#[test]
fn cad785_http_operator_enrolls_binds_and_test_sends() {
    let dir = tempfile::tempdir().unwrap();
    mint_ca(dir.path());
    mint_server(dir.path());
    let ca_pem = std::fs::read(dir.path().join("ca.pem")).unwrap();
    let rig = Rig::start(dir);
    let port = rig.port;
    let b = Board::new(Some(ca_pem));
    let (install, context, id) = b.setup(port);
    // Token-shape SMTP enrollment refuses over HTTP too.
    assert_eq!(
        b.operator(
            "POST",
            "/api/connections",
            &json!({"provider": "smtp", "account": "bad-shape", "shape": "smtp",
                "token": SECRET, "scopes": ["email:send"],
                "accept_same_uid_risk": true})
            .to_string(),
        )
        .0,
        409
    );
    let binding = b.value(
        "POST",
        "/api/crm-smtp/bind",
        json!({"install_id": install, "context_id": context,
            "connection_id": id, "request_id": "bind-1"}),
    )["binding"]
        .clone();
    assert_eq!(binding["auth_revision"], 1);
    let shown = b.value(
        "POST",
        "/api/crm-smtp/show",
        json!({"install_id": install, "context_id": context}),
    )["binding"]
        .clone();
    assert_eq!(shown["digest"], binding["digest"]);
    assert!(shown.to_string().contains("news@example.com"));
    let receipt = b.value(
        "POST",
        "/api/crm-smtp/test-send",
        json!({"install_id": install, "context_id": context,
            "campaign_id": "launch-1", "to_email": "operator@example.com"}),
    )["receipt"]
        .clone();
    assert_eq!(receipt["accepted"], true);
    assert_eq!(receipt["delivery_claim"], "smtp-acceptance-only");
    let seen = rig.outcome();
    assert_eq!(seen.mail_from.as_deref(), Some("news@example.com"));
    assert_eq!(seen.rcpts, 1);
    assert!(seen.bytes > 500, "multipart body missing: {}", seen.bytes);
    assert!(!receipt.to_string().contains(SECRET));
}

#[test]
fn cad785_http_peer_is_as_strict_as_the_daemon() {
    let b = Board::new(None);
    let (install, context, id) = b.setup(465);
    b.value(
        "POST",
        "/api/crm-smtp/bind",
        json!({"install_id": install, "context_id": context,
            "connection_id": id, "request_id": "bind-1"}),
    );
    // Forged identity/receipt/routing/bulk fields refuse at the peer.
    for (path, body) in [
        (
            "/api/crm-smtp/bind",
            json!({"install_id": install, "context_id": context, "connection_id": id,
                "request_id": "x", "by": "operator"}),
        ),
        (
            "/api/crm-smtp/rebind",
            json!({"install_id": install, "context_id": context, "connection_id": id,
                "expected_revision": 1, "workspace": "w"}),
        ),
        (
            "/api/crm-smtp/test-send",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "to_email": "operator@example.com", "to_emails": ["x@example.com"]}),
        ),
        (
            "/api/crm-smtp/test-send",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "to_email": "operator@example.com", "assistant_receipt": "r"}),
        ),
        (
            "/api/crm-smtp/revoke",
            json!({"install_id": install, "context_id": context, "expected_revision": 1,
                "connection_id": "other"}),
        ),
    ] {
        assert_eq!(
            b.operator("POST", path, &body.to_string()).0,
            400,
            "forged field admitted at {path}"
        );
    }
    // Query strings refuse; unknown sub-paths 404.
    assert_eq!(b.operator("POST", "/api/crm-smtp/show?x=1", "{}").0, 400);
    assert_eq!(
        b.operator(
            "POST",
            "/api/crm-smtp/show",
            &json!({"install_id": install, "context_id": context, "connection_id": "x"})
                .to_string(),
        )
        .0,
        400
    );
    // CRM SMTP has no read section: a GET on a known route falls
    // through unserved, and an unknown route is 404 — neither serves.
    assert_eq!(b.operator("GET", "/api/crm-smtp/show", "").0, 404);
    assert_eq!(b.operator("POST", "/api/crm-smtp/notify", "{}").0, 404);
    // A revoked binding refuses the test send over HTTP.
    b.value(
        "POST",
        "/api/crm-smtp/revoke",
        json!({"install_id": install, "context_id": context, "expected_revision": 1}),
    );
    assert_eq!(
        b.operator(
            "POST",
            "/api/crm-smtp/test-send",
            &json!({"install_id": install, "context_id": context,
                "campaign_id": "launch-1", "to_email": "operator@example.com"})
            .to_string(),
        )
        .0,
        409
    );
}

#[test]
fn cad785_http_stolen_session_from_agent_pane_gets_403() {
    let b = Board::new(None);
    let (install, context, id) = b.setup(465);
    b.value(
        "POST",
        "/api/crm-smtp/bind",
        json!({"install_id": install, "context_id": context,
            "connection_id": id, "request_id": "bind-1"}),
    );
    let before = b.value(
        "POST",
        "/api/crm-smtp/show",
        json!({"install_id": install, "context_id": context}),
    );
    let mut lane = LaneShell::spawn(b.root.path());
    plant_member_pane(&b.daemon, "smtp-http-peer", "claude", None, lane.pid());
    let cases = [
        (
            "POST",
            "/api/connections".to_string(),
            json!({"provider": "smtp", "account": "stolen", "shape": "smtp",
                "host": "localhost", "port": 465, "tls_mode": "implicit",
                "username": "smtp-user", "secret": SECRET,
                "sender": "news@example.com", "scopes": ["email:send"],
                "accept_same_uid_risk": true})
            .to_string(),
        ),
        (
            "POST",
            "/api/crm-smtp/bind".to_string(),
            json!({"install_id": install, "context_id": context,
                "connection_id": id, "request_id": "stolen"})
            .to_string(),
        ),
        (
            "POST",
            "/api/crm-smtp/rebind".to_string(),
            json!({"install_id": install, "context_id": context,
                "connection_id": id, "expected_revision": 1})
            .to_string(),
        ),
        (
            "POST",
            "/api/crm-smtp/revoke".to_string(),
            json!({"install_id": install, "context_id": context, "expected_revision": 1})
                .to_string(),
        ),
        (
            "POST",
            "/api/crm-smtp/show".to_string(),
            json!({"install_id": install, "context_id": context}).to_string(),
        ),
        (
            "POST",
            "/api/crm-smtp/test-send".to_string(),
            json!({"install_id": install, "context_id": context,
                "campaign_id": "launch-1", "to_email": "operator@example.com"})
            .to_string(),
        ),
    ];
    let mut failures = Vec::new();
    for prefix in ["", "setsid "] {
        for (method, path, body) in &cases {
            let stolen =
                common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
            let wire = stolen.request_as(method, path, body, "");
            assert!(!wire.contains(cadence_agent::test_seam::AS_HEADER));
            assert!(!wire.contains(cadence_agent::test_seam::TOKEN_HEADER));
            assert!(wire.contains(&stolen.cookie));
            assert!(wire.contains(&stolen.key));
            let file = lane
                .dir
                .path()
                .join(format!("smtp-request-{}.txt", lane.seq));
            std::fs::write(&file, wire).unwrap();
            let (rc, response) = lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));s.sendall(open(sys.argv[2],\"rb\").read());print(s.makefile().readline())' {} {}", b.port, file.display()));
            assert_eq!(rc, 0);
            let code = response
                .split_whitespace()
                .nth(1)
                .unwrap_or("missing")
                .to_string();
            eprintln!("stolen HTTP {prefix}{method} {path}: {code}");
            if code != "403" {
                failures.push(format!("{prefix}{method} {path}: {code}"));
            }
        }
    }
    assert!(failures.is_empty(), "HTTP peer guard failed: {failures:?}");
    assert_eq!(
        b.value(
            "POST",
            "/api/crm-smtp/show",
            json!({"install_id": install, "context_id": context}),
        )["binding"],
        before["binding"]
    );
}
