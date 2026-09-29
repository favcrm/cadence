//! CAD-786 over the board: the send verbs ride operator-proof
//! POSTs/GETs through the strict HTTP peer; the recipient's
//! `/unsubscribe/<token>` page + POST answer without any session —
//! the token is the credential; replayed sessions from an agent pane
//! get 403 on every send route.
//!
//! The isolated TLS rig proves the whole path end to end over HTTP:
//! prepare → approve → two accepted deliveries → one-click
//! unsubscribe suppresses the customer.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, TestDaemon};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

const SECRET: &str = "3vT8-qN5r-Xp7w-Kz2m-44";
const PROFILE_A: &str = r#"{"schema":1,"display_name":"Amina Diallo","email":"amina@example.com","tags":[],"consent":{"email":"granted"}}"#;
const PROFILE_C: &str = r#"{"schema":1,"display_name":"Cleo Boone","email":"cleo@example.com","tags":[],"consent":{"email":"granted"}}"#;

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
        opts.crm_send_interval_ms = 10;
        opts.unsubscribe_origin = Some("http://localhost".into());
        let daemon = TestDaemon::start_opts(opts);
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
        (code, text)
    }

    fn value(&self, method: &str, path: &str, body: Value) -> Value {
        let encoded = body.to_string();
        let (code, text) = self.operator(method, path, &encoded);
        assert_eq!(code, 200, "operator {method} {path}: {text}");
        serde_json::from_str(&text).unwrap()
    }

    fn setup(&self, port: u16) -> (String, String) {
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
        for (id, profile) in [("customer-a", PROFILE_A), ("customer-c", PROFILE_C)] {
            self.daemon
                .operator_rpc(
                    "app_record_create",
                    json!({"install_id": install, "context_id": context, "record_id": id,
                        "profile": serde_json::from_str::<Value>(profile).unwrap()}),
                )
                .unwrap();
        }
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
        self.daemon
            .operator_rpc(
                "app_content_approve",
                json!({"install_id": install, "context_id": context, "campaign_id": "launch-1", "expected_revision": 1}),
            )
            .unwrap();
        self.daemon
            .operator_rpc(
                "app_audience_prepare",
                json!({"install_id": install, "context_id": context, "freeze_id": "freeze-1",
                    "base": {"mode": "all"}, "max_recipients": 50}),
            )
            .unwrap();
        let connection = self.value(
            "POST",
            "/api/connections",
            json!({"provider": "smtp", "account": "send-http", "shape": "smtp",
                    "host": "localhost", "port": port, "tls_mode": "implicit",
                    "username": "smtp-user", "secret": SECRET,
                    "sender": "news@example.com", "sender_name": "CRM News",
                    "scopes": ["email:send"], "accept_same_uid_risk": true}),
        )["connection"]
            .clone();
        let id = connection["id"].as_str().unwrap().to_string();
        self.value(
            "POST",
            "/api/crm-smtp/bind",
            json!({"install_id": install, "context_id": context,
                "connection_id": id, "request_id": "bind-1"}),
        );
        self.value(
            "POST",
            "/api/crm-smtp/test-send",
            json!({"install_id": install, "context_id": context,
                "campaign_id": "launch-1", "to_email": "operator@example.com"}),
        );
        (install, context)
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
        "/CN=cadence-send-http-ca",
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
    let certs: Vec<_> = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = rustls_pemfile::private_key(&mut key_pem.as_slice())
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
    rcpts: Arc<Mutex<Vec<String>>>,
    unsubscribes: Arc<Mutex<Vec<String>>>,
    _dir: tempfile::TempDir,
}

impl Rig {
    fn start(dir: tempfile::TempDir, expect: usize) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = server_config(dir.path());
        let rcpts = Arc::new(Mutex::new(Vec::new()));
        let unsubscribes = Arc::new(Mutex::new(Vec::new()));
        let (rcpt_w, unsub_w) = (rcpts.clone(), unsubscribes.clone());
        std::thread::spawn(move || {
            for _ in 0..expect {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                stream.set_read_timeout(Some(Duration::from_secs(60))).ok();
                stream.set_write_timeout(Some(Duration::from_secs(60))).ok();
                let Ok(conn) = rustls::ServerConnection::new(config.clone()) else {
                    return;
                };
                let tls = rustls::StreamOwned::new(conn, stream);
                let mut reader = BufReader::new(tls);
                let mut rcpt = String::new();
                if !rig_write(&mut reader, "220 localhost cadence-test-smtp") {
                    return;
                }
                while let Some(line) = rig_read(&mut reader) {
                    let head = line
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase();
                    match head.as_str() {
                        "EHLO" | "HELO" => {
                            rig_write(&mut reader, "250-localhost");
                            if !rig_write(&mut reader, "250 AUTH LOGIN PLAIN") {
                                break;
                            }
                        }
                        "AUTH" => {
                            let rest = line[4..].trim();
                            let mechanism =
                                rest.split(' ').next().unwrap_or("").to_ascii_uppercase();
                            match mechanism.as_str() {
                                "LOGIN" => {
                                    if !rig_write(&mut reader, "334 VXNlcm5hbWU6") {
                                        break;
                                    }
                                    if rig_read(&mut reader).is_none() {
                                        break;
                                    }
                                    if !rig_write(&mut reader, "334 UGFzc3dvcmQ6") {
                                        break;
                                    }
                                    if rig_read(&mut reader).is_none() {
                                        break;
                                    }
                                    if !rig_write(&mut reader, "235 ok") {
                                        break;
                                    }
                                }
                                "PLAIN" => {
                                    if !rig_write(&mut reader, "235 ok") {
                                        break;
                                    }
                                }
                                _ => {
                                    if !rig_write(&mut reader, "504 unrecognized") {
                                        break;
                                    }
                                }
                            }
                        }
                        _ if line.to_ascii_uppercase().starts_with("MAIL FROM:") => {
                            if !rig_write(&mut reader, "250 ok") {
                                break;
                            }
                        }
                        _ if line.to_ascii_uppercase().starts_with("RCPT TO:") => {
                            rcpt = line
                                .find('<')
                                .and_then(|s| line.find('>').map(|e| line[s + 1..e].to_string()))
                                .unwrap_or_default();
                            if !rig_write(&mut reader, "250 ok") {
                                break;
                            }
                        }
                        "DATA" => {
                            if !rig_write(&mut reader, "354 go") {
                                break;
                            }
                            while let Some(data) = rig_read(&mut reader) {
                                if data == "." {
                                    break;
                                }
                                if let Some(url) = data.strip_prefix("List-Unsubscribe: <") {
                                    unsub_w
                                        .lock()
                                        .unwrap()
                                        .push(url.trim_end_matches('>').to_string());
                                }
                            }
                            if !rig_write(&mut reader, "250 accepted") {
                                break;
                            }
                        }
                        "QUIT" => {
                            let _ = rig_write(&mut reader, "221 bye");
                            break;
                        }
                        _ => {
                            if !rig_write(&mut reader, "250 ok") {
                                break;
                            }
                        }
                    }
                }
                rcpt_w.lock().unwrap().push(rcpt);
            }
        });
        Self {
            port,
            rcpts,
            unsubscribes,
            _dir: dir,
        }
    }
}

fn wait_until(secs: u64, mut probe: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if probe() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

#[test]
fn cad786_send_routes_are_operator_gated() {
    let board = Board::new(None);
    let session = common::op::sign_in(
        env!("CARGO_BIN_EXE_cadence"),
        &board.daemon.state,
        board.port,
    );
    for (method, path) in [
        ("POST", "/api/crm-send/prepare"),
        ("POST", "/api/crm-send/approve"),
        ("POST", "/api/crm-send/resolve"),
        (
            "GET",
            "/api/crm-send/show?install_id=i&context_id=c&send_id=s",
        ),
        ("GET", "/api/crm-send/list?install_id=i&context_id=c"),
    ] {
        // A session replayed by an agent caller gets 403 — the
        // seam asserts `agent:` so the proof fails the same way a
        // planted pane does.
        if method == "POST" {
            let replay = session.request_as(
                method,
                path,
                "{}",
                &common::op::seam_headers(&board.daemon.state, "agent:w1"),
            );
            let (code, _, _) = common::op::raw(board.port, &replay);
            assert_eq!(code, 403, "agent-replayed {method} {path}");
        }
    }
    // A sessionless POST refuses outright — the RecipientToken class
    // must never widen to API routes.
    for path in ["/api/crm-send/prepare", "/api/crm-send/origin"] {
        let bare = common::op::request(
            "POST",
            path,
            &format!("127.0.0.1:{}", board.port),
            None,
            None,
            "{}",
        );
        let (code, _, _) = common::op::raw(board.port, &bare);
        assert_eq!(code, 403, "sessionless write admitted: {path}");
    }
    // Agent-replayed session on the origin write too.
    let replay = session.request_as(
        "POST",
        "/api/crm-send/origin",
        &json!({"unsubscribe_origin": "https://unsub.example.com"}).to_string(),
        &common::op::seam_headers(&board.daemon.state, "agent:w1"),
    );
    let (code, _, _) = common::op::raw(board.port, &replay);
    assert_eq!(code, 403, "agent-replayed origin write admitted");
    // Unknown fields on the write bodies refuse at the schema.
    let (code, _) = board.operator(
        "POST",
        "/api/crm-send/prepare",
        &json!({"install_id": "i", "context_id": "c", "campaign_id": "k",
            "audience_freeze_id": "f", "request_id": "r", "actor": "operator"})
        .to_string(),
    );
    assert_eq!(code, 400, "forged field must refuse");
    // POSTs to read routes are 405.
    let (code, _) = board.operator("POST", "/api/crm-send/show", "{}");
    assert_eq!(code, 405);
}

#[test]
fn cad786_unsubscribe_route_answers_without_session() {
    let board = Board::new(None);
    // GET: the confirmation page, no cookie needed.
    let token = "A".repeat(43);
    let request = common::op::request(
        "GET",
        &format!("/unsubscribe/{token}"),
        &format!("127.0.0.1:{}", board.port),
        None,
        None,
        "",
    );
    let (code, _, body) = common::op::raw(board.port, &request);
    assert_eq!(code, 200, "{body}");
    assert!(body.contains("Unsubscribe"));
    // A non-token path refuses.
    let request = common::op::request(
        "GET",
        "/unsubscribe/nope",
        &format!("127.0.0.1:{}", board.port),
        None,
        None,
        "",
    );
    let (code, _, _) = common::op::raw(board.port, &request);
    assert_eq!(code, 404);
    // POST: the constant confirmation page — unknown token or not.
    let request = common::op::request(
        "POST",
        &format!("/unsubscribe/{token}"),
        &format!("127.0.0.1:{}", board.port),
        None,
        None,
        "",
    );
    let (code, _, body) = common::op::raw(board.port, &request);
    assert_eq!(code, 200, "{body}");
    assert!(body.contains("Unsubscribed"));
}

#[test]
fn cad786_end_to_end_over_http() {
    let rig_dir = tempfile::tempdir().unwrap();
    mint_ca(rig_dir.path());
    mint_server(rig_dir.path());
    // 1 test send + 2 deliveries.
    let rig = Rig::start(rig_dir, 3);
    let board = Board::new(Some(std::fs::read(rig._dir.path().join("ca.pem")).unwrap()));
    let (install, context) = board.setup(rig.port);

    let prepared = board.value(
        "POST",
        "/api/crm-send/prepare",
        json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
            "audience_freeze_id": "freeze-1", "request_id": "send-1"}),
    );
    let digest = prepared["send_digest"].as_str().unwrap().to_string();
    let send_id = prepared["send"]["send_id"].as_str().unwrap().to_string();
    assert_eq!(prepared["send"]["state"], "prepared");

    let approved = board.value(
        "POST",
        "/api/crm-send/approve",
        json!({"install_id": install, "context_id": context, "send_id": send_id,
            "send_digest": digest}),
    );
    assert_eq!(approved["send"]["state"], "sending");
    assert_eq!(approved["counts"]["queued"], 2, "{approved}");

    let mut last = Value::Null;
    let done = wait_until(30, || {
        let (code, text) = board.operator(
            "GET",
            &format!(
                "/api/crm-send/show?install_id={install}&context_id={context}&send_id={send_id}"
            ),
            "",
        );
        if code == 200 {
            last = serde_json::from_str(&text).unwrap();
        }
        last["send"]["state"] == "completed"
    });
    assert!(done, "send did not complete: {last}");
    // The rig saw the test send plus the two campaign deliveries.
    assert_eq!(rig.rcpts.lock().unwrap().len(), 3);
    // GET list returns the send too.
    let (code, text) = board.operator(
        "GET",
        &format!("/api/crm-send/list?install_id={install}&context_id={context}"),
        "",
    );
    assert_eq!(code, 200, "{text}");
    let listed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(listed["sends"].as_array().unwrap().len(), 1);
    // The origin routes over HTTP: GET shows the configured value,
    // POST round-trips a change, a bad shape refuses with 400.
    let (code, text) = board.operator("GET", "/api/crm-send/origin", "");
    assert_eq!(code, 200, "{text}");
    let (code, text): (u16, String) = board.operator(
        "POST",
        "/api/crm-send/origin",
        &json!({"unsubscribe_origin": "https://board.unsub.example"}).to_string(),
    );
    assert_eq!(code, 200, "{text}");
    let (code, text) = board.operator("GET", "/api/crm-send/origin", "");
    assert_eq!(code, 200, "{text}");
    let shown: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(shown["unsubscribe_origin"], "https://board.unsub.example");
    let (code, _) = board.operator(
        "POST",
        "/api/crm-send/origin",
        &json!({"unsubscribe_origin": "http://example.com/nope"}).to_string(),
    );
    assert_eq!(code, 400);
    // Restore the rig origin for the unsubscribe flow below.
    board.operator(
        "POST",
        "/api/crm-send/origin",
        &json!({"unsubscribe_origin": "http://localhost"}).to_string(),
    );
    // The real unsubscribe token from a campaign message redeems
    // over HTTP — sessionless, exactly as a recipient's browser (or
    // a one-click mail client) would send it.
    let token = {
        let unsubs = rig.unsubscribes.lock().unwrap();
        unsubs
            .iter()
            .map(|u| u.rsplit('/').next().unwrap().to_string())
            .find(|t| t.len() == 43)
            .expect("a campaign unsubscribe URL with a real token")
    };
    let suppressions = |board: &Board| -> usize {
        board
            .daemon
            .operator_rpc(
                "app_suppression_list",
                json!({"install_id": install, "context_id": context}),
            )
            .unwrap()["suppressions"]
            .as_array()
            .unwrap()
            .len()
    };
    // GET renders a confirmation page and mutates nothing.
    let before = suppressions(&board);
    let request = common::op::request(
        "GET",
        &format!("/unsubscribe/{token}"),
        &format!("127.0.0.1:{}", board.port),
        None,
        None,
        "",
    );
    let (code, _, body) = common::op::raw(board.port, &request);
    assert_eq!(code, 200, "{body}");
    assert!(body.contains("Unsubscribe"));
    assert_eq!(suppressions(&board), before, "the GET mutated");
    // POST with the RFC 8058 one-click body redeems the token.
    let request = common::op::request(
        "POST",
        &format!("/unsubscribe/{token}"),
        &format!("127.0.0.1:{}", board.port),
        None,
        None,
        "List-Unsubscribe=One-Click",
    );
    let (code, _, body) = common::op::raw(board.port, &request);
    assert_eq!(code, 200, "{body}");
    assert!(body.contains("Unsubscribed"));
    let suppressions = board
        .daemon
        .operator_rpc(
            "app_suppression_list",
            json!({"install_id": install, "context_id": context}),
        )
        .unwrap();
    assert!(suppressions["suppressions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["kind"] == "customer"));
}
