//! CAD-785 submission over the isolated synthetic SMTP rig.
//!
//! A loopback-only fake sender speaks SMTP the way the production
//! gate demands: implicit-TLS (465-style) and mandatory-STARTTLS
//! (587-style) sessions with certificates minted by a test CA the
//! fixture daemon pins — plus three hostile variants (no STARTTLS,
//! an untrusted certificate, a wrong password). The positive tests
//! prove authenticated encrypted submission of the exact CAD-782
//! frozen bytes with the verified sender; the refusal tests prove
//! downgrade, certificate and authentication failures stop the send
//! before any message byte moves. No live network, no customer data:
//! every address is synthetic and every socket is loopback.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::daemon_opts;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

// Synthetic passwords with no 8-character overlap with any enrolled
// metadata (account, host, username, sender, scopes): the custody
// leak screens refuse enrollment when the password echoes metadata.
const SECRET: &str = "9fK2-qW7z-Xm4p-Lv8t-63";
const WRONG_SECRET: &str = "3tY8-bN5r-Kp2w-Qz6m-77";
const USERNAME: &str = "smtp-user";
const SENDER: &str = "news@example.com";
const RECIPIENT: &str = "operator@example.com";

fn blocks() -> Value {
    json!([
        {"type": "heading", "text": "Hello {{first_name|friend}}"},
        {"type": "paragraph", "text": "A calm first line."},
        {"type": "button", "label": "Read more", "url": "https://example.com/posts/welcome"},
    ])
}

struct Crm {
    _root: tempfile::TempDir,
    _pm: Pm,
    daemon: common::TestDaemon,
}

impl Crm {
    fn new(ca_pem: &[u8]) -> Self {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        Self::copy_source(&root.path().join("source"), "blog-post");
        let mut opts = daemon_opts();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        cadence_agent::platform::smtp::attach(&mut opts);
        opts.smtp_test_ca_pem = Some(ca_pem.to_vec());
        let daemon = common::TestDaemon::start_opts(opts);
        Self {
            _root: root,
            _pm: pm,
            daemon,
        }
    }

    fn copy_source(into: &Path, app: &str) {
        for name in [
            "app.md",
            "workflows/blog-post.md",
            "rubrics/blog.md",
            "templates/brief.md",
            "templates/post.md",
        ] {
            let destination = into.join(name);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::copy(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("apps/blog-post")
                    .join(name),
                &destination,
            )
            .unwrap();
        }
        if app != "blog-post" {
            let manifest = into.join("app.md");
            let text = std::fs::read_to_string(&manifest).unwrap();
            std::fs::write(
                manifest,
                text.replace("app: blog-post", &format!("app: {app}")),
            )
            .unwrap();
        }
    }

    fn setup(&self, port: u16, tls_mode: &str, secret: &str) -> (String, String) {
        let install = self
            .daemon
            .operator_rpc(
                "app_workspace_install",
                json!({"source": self._root.path().join("source")}),
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
                    "subject": "Spring launch", "preheader": "News", "blocks": blocks()}),
            )
            .unwrap();
        let connection = self
            .daemon
            .operator_rpc(
                "connection_create",
                json!({"provider": "smtp", "account": "isolated", "shape": "smtp",
                    "host": "localhost", "port": port, "tls_mode": tls_mode,
                    "username": USERNAME, "secret": secret,
                    "sender": SENDER, "sender_name": "CRM News",
                    "scopes": ["email:send"], "accept_same_uid_risk": true}),
            )
            .unwrap()["connection"]
            .clone();
        let id = connection["id"].as_str().unwrap().to_string();
        self.daemon
            .operator_rpc(
                "crm_smtp_bind",
                json!({"install_id": install, "context_id": context,
                    "connection_id": id, "request_id": "bind-1"}),
            )
            .unwrap();
        (install, context)
    }

    fn test_send(&self, install: &str, context: &str) -> Result<Value, String> {
        self.daemon
            .operator_rpc(
                "crm_smtp_test_send",
                json!({"install_id": install, "context_id": context,
                    "campaign_id": "launch-1", "to_email": RECIPIENT}),
            )
            .map_err(|e| e.to_string())
    }
}

// ---------- isolated certificate authority (openssl) ----------

fn openssl(args: &[&str]) {
    let status = std::process::Command::new("openssl")
        .args(args)
        .output()
        .unwrap_or_else(|_| panic!("openssl is required for the isolated SMTP rig"));
    assert!(
        status.status.success(),
        "openssl {args:?} failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}

struct TestCa {
    dir: tempfile::TempDir,
}

impl TestCa {
    fn mint() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let ca_key = dir.path().join("ca-key.pem");
        let ca = dir.path().join("ca.pem");
        openssl(&[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            ca_key.to_str().unwrap(),
            "-out",
            ca.to_str().unwrap(),
            "-days",
            "2",
            "-subj",
            "/CN=cadence-smtp-test-ca",
        ]);
        Self { dir }
    }

    fn ca_pem(&self) -> Vec<u8> {
        std::fs::read(self.dir.path().join("ca.pem")).unwrap()
    }

    /// A server certificate for `localhost` signed by this CA.
    fn server(&self, name: &str) -> (Vec<u8>, Vec<u8>) {
        let key = self.dir.path().join(format!("{name}-key.pem"));
        let csr = self.dir.path().join(format!("{name}.csr"));
        let cert = self.dir.path().join(format!("{name}-cert.pem"));
        let ext = self.dir.path().join(format!("{name}-ext.cnf"));
        std::fs::write(&ext, "subjectAltName=DNS:localhost,IP:127.0.0.1\n").unwrap();
        openssl(&[
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            key.to_str().unwrap(),
            "-out",
            csr.to_str().unwrap(),
            "-subj",
            "/CN=localhost",
        ]);
        openssl(&[
            "x509",
            "-req",
            "-in",
            csr.to_str().unwrap(),
            "-CA",
            self.dir.path().join("ca.pem").to_str().unwrap(),
            "-CAkey",
            self.dir.path().join("ca-key.pem").to_str().unwrap(),
            "-CAcreateserial",
            "-days",
            "2",
            "-out",
            cert.to_str().unwrap(),
            "-extfile",
            ext.to_str().unwrap(),
        ]);
        (std::fs::read(cert).unwrap(), std::fs::read(key).unwrap())
    }
}

// ---------- the fake sender ----------

#[derive(Clone, Copy, PartialEq, Eq)]
enum RigMode {
    /// TLS immediately, like port 465.
    ImplicitTls,
    /// Plaintext greeting, mandatory STARTTLS upgrade, like 587.
    Starttls,
    /// Port 587 shape with no STARTTLS advertisement: the client
    /// must refuse before AUTH.
    NoStarttls,
}

struct Submission {
    auth_user: Option<String>,
    auth_ok: bool,
    mail_from: Option<String>,
    rcpt_tos: Vec<String>,
    data: Option<String>,
    saw_auth: bool,
    saw_data: bool,
    upgraded: bool,
}

struct Rig {
    port: u16,
    submissions: mpsc::Receiver<Submission>,
    _thread: std::thread::JoinHandle<()>,
}

fn server_config(mut cert_pem: &[u8], mut key_pem: &[u8]) -> std::sync::Arc<rustls::ServerConfig> {
    let certs: Vec<_> = rustls_pemfile::certs(&mut cert_pem)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = rustls_pemfile::private_key(&mut key_pem).unwrap().unwrap();
    std::sync::Arc::new(
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap(),
    )
}

enum ServerWire {
    Plain(BufReader<TcpStream>),
    Tls(Box<BufReader<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>>),
}

struct ServerSession {
    wire: Option<ServerWire>,
    upgraded: bool,
}

impl ServerSession {
    fn wire(&mut self) -> &mut ServerWire {
        self.wire.as_mut().expect("SMTP rig wire is set")
    }

    fn reader(&mut self) -> &mut dyn BufRead {
        match self.wire() {
            ServerWire::Plain(reader) => reader,
            ServerWire::Tls(reader) => reader,
        }
    }

    fn writer(&mut self) -> &mut dyn Write {
        match self.wire() {
            ServerWire::Plain(reader) => reader.get_mut(),
            ServerWire::Tls(reader) => reader.get_mut(),
        }
    }

    fn read_line(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.reader().read_line(&mut line) {
            Ok(0) => None,
            Ok(_) if line.len() > 4096 => None,
            Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_string()),
            Err(_) => None,
        }
    }

    fn write_line(&mut self, text: &str) -> bool {
        self.writer()
            .write_all(format!("{text}\r\n").as_bytes())
            .and_then(|()| self.writer().flush())
            .is_ok()
    }

    fn upgrade(&mut self, config: &std::sync::Arc<rustls::ServerConfig>) -> bool {
        let wire = self.wire.take().expect("SMTP rig wire is set");
        match wire {
            ServerWire::Plain(reader) => {
                let stream = reader.into_inner();
                let Ok(conn) = rustls::ServerConnection::new(config.clone()) else {
                    return false;
                };
                self.wire = Some(ServerWire::Tls(Box::new(BufReader::new(
                    rustls::StreamOwned::new(conn, stream),
                ))));
                self.upgraded = true;
                true
            }
            other => {
                self.wire = Some(other);
                false
            }
        }
    }
}

fn b64_decode(text: &str) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    STANDARD
        .decode(text.trim())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

fn angle(value: &str) -> Option<String> {
    let start = value.find('<')?;
    let end = value.find('>')?;
    if end > start {
        Some(value[start + 1..end].to_string())
    } else {
        None
    }
}

impl Rig {
    fn start(mode: RigMode, cert: Vec<u8>, key: Vec<u8>, expected_secret: String) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let config = server_config(&cert, &key);
            // Sequential sessions until the test ends: the loop
            // outlives any single send so rotation probes can dial
            // again; the thread is reaped at process exit.
            for accepted in listener.incoming() {
                let Ok(stream) = accepted else {
                    break;
                };
                stream.set_read_timeout(Some(Duration::from_secs(20))).ok();
                stream.set_write_timeout(Some(Duration::from_secs(20))).ok();
                let outcome = serve_one(&config, mode, stream, &expected_secret);
                if tx.send(outcome).is_err() {
                    break;
                }
            }
        });
        Self {
            port,
            submissions: rx,
            _thread: thread,
        }
    }

    fn submission(self) -> Submission {
        self.submissions
            .recv_timeout(Duration::from_secs(90))
            .expect("fake sender saw no session")
    }

    fn try_submission(&self, timeout: Duration) -> Option<Submission> {
        self.submissions.recv_timeout(timeout).ok()
    }
}

fn serve_one(
    config: &std::sync::Arc<rustls::ServerConfig>,
    mode: RigMode,
    stream: TcpStream,
    expected_secret: &str,
) -> Submission {
    let mut submission = Submission {
        auth_user: None,
        auth_ok: false,
        mail_from: None,
        rcpt_tos: Vec::new(),
        data: None,
        saw_auth: false,
        saw_data: false,
        upgraded: false,
    };
    let mut session = match mode {
        RigMode::ImplicitTls => {
            let conn = rustls::ServerConnection::new(config.clone()).unwrap();
            ServerSession {
                wire: Some(ServerWire::Tls(Box::new(BufReader::new(
                    rustls::StreamOwned::new(conn, stream),
                )))),
                upgraded: true,
            }
        }
        RigMode::Starttls | RigMode::NoStarttls => ServerSession {
            wire: Some(ServerWire::Plain(BufReader::new(stream))),
            upgraded: false,
        },
    };
    serve_one_inner(config, mode, &mut session, &mut submission, expected_secret);
    submission
}

fn serve_one_inner(
    config: &std::sync::Arc<rustls::ServerConfig>,
    mode: RigMode,
    session: &mut ServerSession,
    submission: &mut Submission,
    expected_secret: &str,
) {
    submission.upgraded = session.upgraded;
    if !session.write_line("220 localhost cadence-test-smtp") {
        return;
    }
    while let Some(line) = session.read_line() {
        let head = line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        match head.as_str() {
            "EHLO" | "HELO" => {
                if !session.upgraded && mode == RigMode::Starttls {
                    session.write_line("250-localhost");
                    session.write_line("250-STARTTLS");
                    session.write_line("250 AUTH LOGIN PLAIN");
                } else {
                    session.write_line("250-localhost");
                    session.write_line("250 AUTH LOGIN PLAIN");
                }
            }
            "STARTTLS" => {
                if !session.upgraded && mode == RigMode::Starttls {
                    if !session.write_line("220 ready") {
                        break;
                    }
                    if session.upgrade(config) {
                        submission.upgraded = true;
                    } else {
                        break;
                    }
                } else {
                    session.write_line("502 unimplemented");
                }
            }
            "AUTH" => {
                submission.saw_auth = true;
                let rest = line[4..].trim();
                let mut parts = rest.splitn(2, ' ');
                let mechanism = parts.next().unwrap_or("").to_ascii_uppercase();
                let initial = parts.next().unwrap_or("");
                let (user, pass) = match mechanism.as_str() {
                    "LOGIN" => {
                        if !session.write_line("334 VXNlcm5hbWU6") {
                            break;
                        }
                        let Some(user) = session.read_line() else {
                            break;
                        };
                        if !session.write_line("334 UGFzc3dvcmQ6") {
                            break;
                        }
                        let Some(pass) = session.read_line() else {
                            break;
                        };
                        (b64_decode(&user), b64_decode(&pass))
                    }
                    "PLAIN" => {
                        let decoded = b64_decode(initial);
                        let mut fields = decoded.split('\0');
                        let _ = fields.next();
                        (
                            fields.next().unwrap_or("").to_string(),
                            fields.next().unwrap_or("").to_string(),
                        )
                    }
                    _ => {
                        session.write_line("504 unrecognized");
                        continue;
                    }
                };
                submission.auth_user = Some(user.clone());
                if user == USERNAME && pass == expected_secret {
                    submission.auth_ok = true;
                    session.write_line("235 ok");
                } else {
                    session.write_line("535 refused");
                }
            }
            "MAIL" => {
                if !submission.auth_ok {
                    session.write_line("530 authentication required");
                } else if let Some(from) = angle(&line) {
                    submission.mail_from = Some(from);
                    session.write_line("250 ok");
                } else {
                    session.write_line("501 malformed");
                }
            }
            "RCPT" => {
                if !submission.auth_ok {
                    session.write_line("530 authentication required");
                } else if let Some(to) = angle(&line) {
                    submission.rcpt_tos.push(to);
                    session.write_line("250 ok");
                } else {
                    session.write_line("501 malformed");
                }
            }
            "DATA" => {
                if !submission.auth_ok || submission.rcpt_tos.is_empty() {
                    session.write_line("503 bad sequence");
                } else if !session.write_line("354 go") {
                    break;
                } else {
                    let mut lines = Vec::new();
                    while let Some(data) = session.read_line() {
                        if data == "." {
                            break;
                        }
                        // Undo exactly one dot-stuffing layer:
                        // the client prefixes every
                        // dot-leading line with one dot.
                        lines.push(data.strip_prefix('.').unwrap_or(&data).to_string());
                    }
                    submission.saw_data = true;
                    submission.data = Some(lines.join("\r\n"));
                    session.write_line("250 accepted");
                }
            }
            "RSET" | "NOOP" => {
                session.write_line("250 ok");
            }
            "QUIT" => {
                session.write_line("221 bye");
                break;
            }
            _ => {
                session.write_line("502 unimplemented");
            }
        }
    }
}

// ---------- the tests ----------

fn positive_assertions(receipt: &Value, seen: &Submission) {
    assert_eq!(receipt["accepted"], true);
    assert_eq!(receipt["smtp_code"], 250);
    assert_eq!(receipt["delivery_claim"], "smtp-acceptance-only");
    assert_eq!(receipt["kind"], "test");
    // Authenticated: the server verified the password.
    assert!(seen.saw_auth, "server saw no AUTH");
    assert!(seen.auth_ok, "server rejected the credential");
    assert_eq!(seen.auth_user.as_deref(), Some(USERNAME));
    // Verified sender and exactly one operator recipient.
    assert_eq!(seen.mail_from.as_deref(), Some(SENDER));
    assert_eq!(seen.rcpt_tos, vec![RECIPIENT.to_string()]);
    // The frozen bytes rode multipart/alternative.
    let data = seen.data.as_deref().expect("server saw no DATA");
    assert!(data.contains("multipart/alternative"), "not multipart");
    assert!(data.contains("Content-Type: text/plain"), "no text part");
    assert!(data.contains("Content-Type: text/html"), "no HTML part");
    assert!(data.contains("Hello"), "heading missing");
    assert!(data.contains("friend"), "fallback substitution missing");
    assert!(
        data.contains("https://example.com/posts/welcome"),
        "button URL missing"
    );
    assert!(data.contains(SENDER), "verified sender missing from body");
    assert!(
        data.contains("List-Unsubscribe:"),
        "unsubscribe header missing"
    );
    assert!(data.contains("Spring launch"), "subject missing");
    assert!(receipt["multipart"]["html_bytes"].as_u64().unwrap() > 100);
    assert!(receipt["multipart"]["text_bytes"].as_u64().unwrap() > 20);
    assert_eq!(
        receipt["unsubscribe_authority"],
        "test send (no unsubscribe token)"
    );
    assert!(receipt.to_string().contains("smtp-acceptance-only"));
    // Nothing secret rode along in cleartext metadata.
    assert!(!data.contains(SECRET));
    assert!(!receipt.to_string().contains(SECRET));
}

#[test]
fn cad785_implicit_tls_test_send_submits_frozen_bytes() {
    let ca = TestCa::mint();
    let (cert, key) = ca.server("srv465");
    let rig = Rig::start(RigMode::ImplicitTls, cert, key, SECRET.to_string());
    let port = rig.port;
    let app = Crm::new(&ca.ca_pem());
    let (install, context) = app.setup(port, "implicit", SECRET);
    let receipt = app
        .test_send(&install, &context)
        .expect("isolated implicit-TLS send must be accepted")["receipt"]
        .clone();
    let seen = rig.submission();
    positive_assertions(&receipt, &seen);
    assert_eq!(receipt["transport"]["tls_mode"], "implicit");
    assert_eq!(receipt["transport"]["port"], port);
}

#[test]
fn cad785_starttls_test_send_upgrades_before_auth() {
    let ca = TestCa::mint();
    let (cert, key) = ca.server("srv587");
    let rig = Rig::start(RigMode::Starttls, cert, key, SECRET.to_string());
    let port = rig.port;
    let app = Crm::new(&ca.ca_pem());
    let (install, context) = app.setup(port, "starttls", SECRET);
    let receipt = app
        .test_send(&install, &context)
        .expect("isolated STARTTLS send must be accepted")["receipt"]
        .clone();
    let seen = rig.submission();
    positive_assertions(&receipt, &seen);
    assert!(seen.upgraded, "STARTTLS upgrade never happened");
    assert_eq!(receipt["transport"]["tls_mode"], "starttls");
}

#[test]
fn cad785_missing_starttls_refuses_before_auth() {
    let ca = TestCa::mint();
    let (cert, key) = ca.server("srvplain");
    let rig = Rig::start(RigMode::NoStarttls, cert, key, SECRET.to_string());
    let app = Crm::new(&ca.ca_pem());
    let (install, context) = app.setup(rig.port, "starttls", SECRET);
    let error = app.test_send(&install, &context).unwrap_err();
    assert!(error.contains("STARTTLS"), "wrong refusal: {error}");
    assert!(!error.contains(SECRET), "refusal leaked the secret");
    let seen = rig.submission();
    assert!(!seen.saw_auth, "AUTH crossed plaintext");
    assert!(!seen.saw_data, "DATA crossed plaintext");
    assert!(!seen.upgraded);
}

#[test]
fn cad785_untrusted_certificate_refuses_before_auth() {
    // The server's certificate chains to a *different* CA than the
    // one the fixture daemon pins: verification must fail.
    let pinned = TestCa::mint();
    let hostile = TestCa::mint();
    let (cert, key) = hostile.server("srvrogue");
    let rig = Rig::start(RigMode::ImplicitTls, cert, key, SECRET.to_string());
    let app = Crm::new(&pinned.ca_pem());
    let (install, context) = app.setup(rig.port, "implicit", SECRET);
    let error = app.test_send(&install, &context).unwrap_err();
    assert!(
        error.contains("TLS") || error.contains("transport") || error.contains("refused"),
        "wrong refusal: {error}"
    );
    assert!(!error.contains(SECRET), "refusal leaked the secret");
    // The TLS handshake fails server-side too, so the rig may never
    // reach the SMTP dialog — either way no AUTH and no DATA.
    if let Some(seen) = rig.try_submission(Duration::from_secs(5)) {
        assert!(!seen.saw_auth, "AUTH crossed an unverified tunnel");
        assert!(!seen.saw_data, "DATA crossed an unverified tunnel");
    }
}

#[test]
fn cad785_wrong_password_refuses_without_sending() {
    let ca = TestCa::mint();
    let (cert, key) = ca.server("srvwrong");
    let rig = Rig::start(RigMode::ImplicitTls, cert, key, SECRET.to_string());
    let app = Crm::new(&ca.ca_pem());
    // Enrolled with the wrong password: AUTH fails, DATA never runs.
    let (install, context) = app.setup(rig.port, "implicit", WRONG_SECRET);
    let error = app.test_send(&install, &context).unwrap_err();
    assert!(error.contains("authentication"), "wrong refusal: {error}");
    assert!(!error.contains(WRONG_SECRET), "refusal leaked the secret");
    let seen = rig.submission();
    assert!(seen.saw_auth, "server saw no AUTH attempt");
    assert!(!seen.auth_ok, "wrong password accepted");
    assert!(!seen.saw_data, "DATA ran after refused AUTH");
}

#[test]
fn cad785_rotate_stales_authorization_until_rebind() {
    let ca = TestCa::mint();
    let (cert, key) = ca.server("srvrotate");
    let rig = Rig::start(RigMode::ImplicitTls, cert, key, SECRET.to_string());
    let port = rig.port;
    // Keep the rig alive past one session: the daemon dials fresh
    // sockets per send, but this test sends once, so one session
    // suffices — the rig thread ends after the first send.
    let app = Crm::new(&ca.ca_pem());
    let (install, context) = app.setup(port, "implicit", SECRET);
    let link = app
        .daemon
        .operator_rpc(
            "crm_smtp_show",
            json!({"install_id": install, "context_id": context}),
        )
        .unwrap()["binding"]
        .clone();
    assert_eq!(link["auth_revision"], 1);
    // Rotation bumps the credential revision; the link still pins 1.
    let rotated = app
        .daemon
        .operator_rpc(
            "connection_rotate",
            json!({"connection_id": link["connection_id"], "secret": WRONG_SECRET}),
        )
        .unwrap();
    assert_eq!(rotated["connection"]["revision"], 2);
    // The stale authorization refuses without touching the network:
    // the rig already served its one session, so any socket attempt
    // would fail differently — the refusal must name staleness.
    let error = app.test_send(&install, &context).unwrap_err();
    assert!(
        error.contains("stale"),
        "stale authorization admitted: {error}"
    );
    // Rebind under CAS adopts revision 2 — and the next send then
    // refuses at AUTH, because the rig expects the original secret.
    // That AUTH refusal proves the rebind took effect with the NEW
    // secret on the wire.
    let rebound = app
        .daemon
        .operator_rpc(
            "crm_smtp_rebind",
            json!({"install_id": install, "context_id": context,
                "connection_id": link["connection_id"], "expected_revision": 1}),
        )
        .unwrap()["binding"]
        .clone();
    assert_eq!(rebound["auth_revision"], 2);
    assert_eq!(rebound["link_revision"], 2);
    let error = app.test_send(&install, &context).unwrap_err();
    assert!(error.contains("authentication"), "rebound send: {error}");
    let seen = rig.submission();
    assert!(seen.saw_auth, "rebound send never attempted AUTH");
    assert!(!seen.auth_ok, "wrong password accepted after rebind");
    assert!(!seen.saw_data, "DATA ran after refused AUTH");
}

#[test]
fn cad785_revoked_credential_and_link_refuse_sends() {
    let ca = TestCa::mint();
    let app = Crm::new(&ca.ca_pem());
    let install = app
        .daemon
        .operator_rpc(
            "app_workspace_install",
            json!({"source": app._root.path().join("source")}),
        )
        .unwrap()["install_id"]
        .as_str()
        .unwrap()
        .to_string();
    let context = app
        .daemon
        .operator_rpc(
            "app_context_create",
            json!({"install_id": install, "label": "brand", "input_defaults": {}, "request_id": "ctx-1"}),
        )
        .unwrap()["context"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    app.daemon
        .operator_rpc(
            "app_content_save",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "subject": "Spring launch", "preheader": "News", "blocks": blocks()}),
        )
        .unwrap();
    let first = app
        .daemon
        .operator_rpc(
            "connection_create",
            json!({"provider": "smtp", "account": "rev-a", "shape": "smtp",
                "host": "localhost", "port": 465, "tls_mode": "implicit",
                "username": USERNAME, "secret": SECRET,
                "sender": SENDER, "scopes": ["email:send"],
                "accept_same_uid_risk": true}),
        )
        .unwrap()["connection"]
        .clone();
    let second = app
        .daemon
        .operator_rpc(
            "connection_create",
            json!({"provider": "smtp", "account": "rev-b", "shape": "smtp",
                "host": "localhost", "port": 465, "tls_mode": "implicit",
                "username": USERNAME, "secret": SECRET,
                "sender": SENDER, "scopes": ["email:send"],
                "accept_same_uid_risk": true}),
        )
        .unwrap()["connection"]
        .clone();
    app.daemon
        .operator_rpc(
            "crm_smtp_bind",
            json!({"install_id": install, "context_id": context,
                "connection_id": first["id"], "request_id": "bind-1"}),
        )
        .unwrap();
    // Revoking the credential pulls the send authority out from
    // under the live link — no socket opens.
    app.daemon
        .operator_rpc("connection_revoke", json!({"connection_id": first["id"]}))
        .unwrap();
    let error = app.test_send(&install, &context).unwrap_err();
    assert!(
        error.contains("unavailable") || error.contains("stale"),
        "revoked credential admitted: {error}"
    );
    // Rebind to the surviving connection, then revoke the link: the
    // credential lives, the binding does not.
    app.daemon
        .operator_rpc(
            "crm_smtp_rebind",
            json!({"install_id": install, "context_id": context,
                "connection_id": second["id"], "expected_revision": 1}),
        )
        .unwrap();
    app.daemon
        .operator_rpc(
            "crm_smtp_revoke",
            json!({"install_id": install, "context_id": context, "expected_revision": 2}),
        )
        .unwrap();
    let error = app.test_send(&install, &context).unwrap_err();
    assert!(
        error.contains("revoked") || error.contains("bound"),
        "revoked link admitted: {error}"
    );
}
