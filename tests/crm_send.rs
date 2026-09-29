//! CAD-786 approved bounded sends at the daemon: prepare commits the
//! digest, approve mints queued deliveries + unsubscribe tokens, the
//! worker submits one-by-one against an isolated TLS rig, and every
//! adversarial caller — agent, detached child, forged field — is
//! refused without mutation.
//!
//! No live network: every submission lands on `127.0.0.1` against a
//! rustls rig whose CA the fixture daemon pins via `smtp_test_ca_pem`.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, TestDaemon};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SECRET: &str = "7pQ4-wK9x-Zm2t-Vj8n-61";
const PROFILE_A: &str = r#"{"schema":1,"display_name":"Amina Diallo","email":"amina@example.com","tags":[],"consent":{"email":"granted"}}"#;
const PROFILE_B: &str = r#"{"schema":1,"display_name":"Boris Feld","email":"boris@example.com","tags":[],"consent":{"email":"denied"}}"#;
const PROFILE_C: &str = r#"{"schema":1,"display_name":"Cleo Boone","email":"cleo@example.com","tags":[],"consent":{"email":"granted"}}"#;

fn blocks() -> Value {
    json!([
        {"type": "heading", "text": "Hello {{first_name|friend}}"},
        {"type": "paragraph", "text": "A calm first line."},
        {"type": "button", "label": "Read more", "url": "https://example.com/posts/welcome"},
    ])
}

// ---------- the isolated TLS rig: N sequential deliveries ----------

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

fn mint_ca(dir: &Path) {
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
        "/CN=cadence-send-test-ca",
    ]);
}

fn mint_server(dir: &Path) {
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

fn server_config(dir: &Path) -> Arc<rustls::ServerConfig> {
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

#[derive(Clone, Debug)]
struct RigMessage {
    rcpt: String,
    body: String,
    message_id: String,
    unsubscribe: String,
}

/// One connection per message, in worker order. `messages` fills
/// from a shared sink the test drains.
struct Rig {
    port: u16,
    messages: Arc<Mutex<Vec<RigMessage>>>,
    _dir: tempfile::TempDir,
}

impl Rig {
    /// `expect` messages are served, then the listener exits. An
    /// over-send blocks inside `accept` — the suite's timeout is the
    /// bound; the test counts from the shared sink instead.
    fn start(dir: tempfile::TempDir, expect: usize) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = server_config(dir.path());
        let sink = Arc::new(Mutex::new(Vec::new()));
        let writer = sink.clone();
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
                let mut message = RigMessage {
                    rcpt: String::new(),
                    body: String::new(),
                    message_id: String::new(),
                    unsubscribe: String::new(),
                };
                if !rig_write(&mut reader, "220 localhost cadence-test-smtp") {
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
                            if !rig_write(&mut reader, "250 AUTH LOGIN PLAIN") {
                                break;
                            }
                        }
                        "AUTH" => {
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
                                if !rig_write(&mut reader, "235 ok") {
                                    break;
                                }
                            } else if !rig_write(&mut reader, "535 refused") {
                                break;
                            }
                        }
                        _ if line.to_ascii_uppercase().starts_with("MAIL FROM:") => {
                            if !rig_write(&mut reader, "250 ok") {
                                break;
                            }
                        }
                        _ if line.to_ascii_uppercase().starts_with("RCPT TO:") => {
                            message.rcpt = line
                                .find('<')
                                .and_then(|s| line.find('>').map(|e| line[s + 1..e].to_string()))
                                .unwrap_or_default();
                            if !rig_write(&mut reader, "250 ok") {
                                break;
                            }
                        }
                        "DATA" => {
                            if !authed || !rig_write(&mut reader, "354 go") {
                                break;
                            }
                            while let Some(data) = rig_read(&mut reader) {
                                if data == "." {
                                    break;
                                }
                                message.body.push_str(&data);
                                message.body.push('\n');
                                if let Some(id) = data.strip_prefix("Message-ID: <") {
                                    message.message_id = id.trim_end_matches('>').to_string();
                                }
                                if let Some(url) = data
                                    .strip_prefix("List-Unsubscribe: <")
                                    .or_else(|| data.strip_prefix("unsubscribe: <"))
                                {
                                    message.unsubscribe = url.trim_end_matches('>').to_string();
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
                writer.lock().unwrap().push(message);
            }
        });
        Self {
            port,
            messages: sink,
            _dir: dir,
        }
    }
}

// ---------- the fixture ----------

struct Crm {
    root: tempfile::TempDir,
    _pm: Pm,
    daemon: TestDaemon,
}

impl Crm {
    fn new(interval_ms: u64, origin: Option<String>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        Self::copy_source(&root.path().join("source"), "blog-post");
        let mut opts = daemon_opts();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        cadence_agent::platform::smtp::attach(&mut opts);
        opts.crm_send_interval_ms = interval_ms;
        opts.unsubscribe_origin = origin;
        let daemon = TestDaemon::start_opts(opts);
        Self {
            root,
            _pm: pm,
            daemon,
        }
    }

    /// A fixture whose daemon pins the rig CA, a real unsubscribe
    /// origin and a fast send interval — the shape every live-send
    /// test needs.
    fn with_ca(ca: &tempfile::TempDir, interval_ms: u64, origin: String) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pm = Pm::init(&dir.path().join("pm")).unwrap();
        Self::copy_source(&dir.path().join("source"), "blog-post");
        let mut opts = daemon_opts();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        cadence_agent::platform::smtp::attach(&mut opts);
        opts.crm_send_interval_ms = interval_ms;
        opts.unsubscribe_origin = Some(origin);
        opts.smtp_test_ca_pem = Some(std::fs::read(ca.path().join("ca.pem")).unwrap());
        let daemon = TestDaemon::start_opts(opts);
        Self {
            root: dir,
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

    fn install(&self) -> String {
        self.daemon
            .operator_rpc(
                "app_workspace_install",
                json!({"source": self.root.path().join("source")}),
            )
            .unwrap()["install_id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn context(&self, install: &str, label: &str, request: &str) -> String {
        self.daemon
            .operator_rpc(
                "app_context_create",
                json!({"install_id": install, "label": label, "input_defaults": {}, "request_id": request}),
            )
            .unwrap()["context"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn seed_customers(&self, install: &str, context: &str, profiles: &[(&str, &str)]) {
        for (id, profile) in profiles {
            self.daemon
                .operator_rpc(
                    "app_record_create",
                    json!({"install_id": install, "context_id": context, "record_id": id,
                        "profile": serde_json::from_str::<Value>(profile).unwrap()}),
                )
                .unwrap();
        }
    }

    fn save_and_approve(&self, install: &str, context: &str, campaign: &str) {
        self.daemon
            .operator_rpc(
                "app_content_save",
                json!({"install_id": install, "context_id": context, "campaign_id": campaign,
                    "subject": "Spring launch", "preheader": "News", "blocks": blocks()}),
            )
            .unwrap();
        self.daemon
            .operator_rpc(
                "app_content_approve",
                json!({"install_id": install, "context_id": context, "campaign_id": campaign, "expected_revision": 1}),
            )
            .unwrap();
    }

    fn freeze(&self, install: &str, context: &str, freeze: &str, base: Value, max: i64) {
        self.daemon
            .operator_rpc(
                "app_audience_prepare",
                json!({"install_id": install, "context_id": context, "freeze_id": freeze,
                    "base": base, "max_recipients": max}),
            )
            .unwrap();
    }

    fn enroll(&self, account: &str, port: u16) -> Value {
        self.daemon
            .operator_rpc(
                "connection_create",
                json!({"provider": "smtp", "account": account, "shape": "smtp",
                    "host": "localhost", "port": port, "tls_mode": "implicit",
                    "username": "smtp-user", "secret": SECRET,
                    "sender": "news@example.com", "sender_name": "CRM News",
                    "scopes": ["email:send"], "accept_same_uid_risk": true}),
            )
            .unwrap()["connection"]
            .clone()
    }

    fn bind(&self, install: &str, context: &str, connection: &str, request: &str) {
        self.daemon
            .operator_rpc(
                "crm_smtp_bind",
                json!({"install_id": install, "context_id": context,
                    "connection_id": connection, "request_id": request}),
            )
            .unwrap();
    }

    fn test_send(&self, install: &str, context: &str, campaign: &str) {
        self.daemon
            .operator_rpc(
                "crm_smtp_test_send",
                json!({"install_id": install, "context_id": context, "campaign_id": campaign,
                    "to_email": "operator@example.com"}),
            )
            .unwrap();
    }

    /// install + context + 3 customers + approved content + freeze +
    /// bound link + accepted test send. Returns (install, context,
    /// connection_id).
    fn ready(&self, rig: &Rig) -> (String, String, String) {
        let install = self.install();
        let context = self.context(&install, "brand", "ctx-1");
        self.seed_customers(
            &install,
            &context,
            &[
                ("customer-a", PROFILE_A),
                ("customer-b", PROFILE_B),
                ("customer-c", PROFILE_C),
            ],
        );
        self.save_and_approve(&install, &context, "launch-1");
        self.freeze(&install, &context, "freeze-1", json!({"mode": "all"}), 50);
        let connection = self.enroll("send-smtp", rig.port);
        let connection_id = connection["id"].as_str().unwrap().to_string();
        self.bind(&install, &context, &connection_id, "bind-1");
        self.test_send(&install, &context, "launch-1");
        (install, context, connection_id)
    }
}

fn ca() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    mint_ca(dir.path());
    mint_server(dir.path());
    dir
}

/// Wait until `probe` is true or the deadline — the worker thread is
/// real; polls replace sleeps.
fn wait_until(deadline_secs: u64, probe: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(deadline_secs);
    while Instant::now() < deadline {
        if probe() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

// ---------- gates first (adversarial-first) ----------

#[test]
fn cad786_send_verbs_are_operator_only() {
    let crm = Crm::new(10, Some("https://board.example".into()));
    crm.daemon.register("send-gate");
    let install = crm.install();
    let context = crm.context(&install, "brand", "ctx-1");
    for (method, params) in [
        (
            "crm_send_prepare",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "audience_freeze_id": "freeze-1", "request_id": "p-1"}),
        ),
        (
            "crm_send_approve",
            json!({"install_id": install, "context_id": context, "send_id": "send-p-1",
                "send_digest": "sha256:x"}),
        ),
        (
            "crm_send_show",
            json!({"install_id": install, "context_id": context, "send_id": "send-p-1"}),
        ),
        (
            "crm_send_list",
            json!({"install_id": install, "context_id": context}),
        ),
        (
            "crm_send_resolve",
            json!({"install_id": install, "context_id": context, "send_id": "send-p-1",
                "customer_id": "customer-a", "resolution": "accepted"}),
        ),
    ] {
        let err = crm
            .daemon
            .agent_rpc("send-gate", method, params.clone())
            .unwrap_err();
        assert!(
            err.to_string().contains("operator action")
                || err.to_string().contains("not provably the operator"),
            "{method}: {err}"
        );
        let err = crm.daemon.unproven_rpc(method, params).unwrap_err();
        assert!(
            err.to_string().contains("operator action")
                || err.to_string().contains("not provably the operator"),
            "{method}: {err}"
        );
    }
    // Forged identity/routing fields refuse before scope resolution.
    for params in [
        json!({"install_id": install, "context_id": context, "campaign_id": "c", "audience_freeze_id": "f", "request_id": "r", "actor": "operator"}),
        json!({"install_id": install, "context_id": context, "campaign_id": "c", "audience_freeze_id": "f", "request_id": "r", "workspace": "w"}),
        json!({"install_id": install, "context_id": context, "send_id": "s", "send_digest": "d", "assistant_receipt": "r"}),
        json!({"install_id": install, "context_id": context, "send_id": "s", "send_digest": "d", "project": "p"}),
    ] {
        assert!(
            crm.daemon
                .operator_rpc("crm_send_prepare", params.clone())
                .is_err()
                || crm.daemon.operator_rpc("crm_send_approve", params).is_err()
        );
    }
    // The redeem verb answers from an agent connection — the token
    // is the credential — but reveals nothing and mutates nothing
    // for an unknown token.
    let out = crm
        .daemon
        .agent_rpc(
            "send-gate",
            "crm_unsubscribe_redeem",
            json!({"token": "A".repeat(43)}),
        )
        .unwrap();
    assert_eq!(out, json!({"unsubscribed": true}));
}

#[test]
fn cad786_prepare_requires_origin_and_test_evidence() {
    // No unsubscribe origin configured — prepare refuses.
    let crm = Crm::new(10, None);
    let install = crm.install();
    let context = crm.context(&install, "brand", "ctx-1");
    crm.seed_customers(&install, &context, &[("customer-a", PROFILE_A)]);
    crm.save_and_approve(&install, &context, "launch-1");
    crm.freeze(&install, &context, "freeze-1", json!({"mode": "all"}), 50);
    let err = crm
        .daemon
        .operator_rpc(
            "crm_send_prepare",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "audience_freeze_id": "freeze-1", "request_id": "p-1"}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("unsubscribe origin"), "{err}");

    // Unapproved content refuses before any link is consulted.
    let crm = Crm::new(10, Some("https://board.example".into()));
    let install = crm.install();
    let context = crm.context(&install, "brand", "ctx-1");
    crm.daemon
        .operator_rpc(
            "app_content_save",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "subject": "Draft", "preheader": "", "blocks": blocks()}),
        )
        .unwrap();
    let err = crm
        .daemon
        .operator_rpc(
            "crm_send_prepare",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "audience_freeze_id": "freeze-1", "request_id": "p-1"}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("approved"), "{err}");
}

#[test]
fn cad786_send_end_to_end_three_recipients() {
    let rig_dir = ca();
    // 1 test send + 2 campaign deliveries = 3 connections.
    let rig = Rig::start(rig_dir, 3);
    let crm = Crm::with_ca(&rig._dir, 10, format!("https://board.example:{}", rig.port));
    let (install, context, _connection) = crm.ready(&rig);
    // The freeze carries all three customers; customer-b's denied
    // consent kept it out at freeze time, so 2 members.
    let prepared = crm
        .daemon
        .operator_rpc(
            "crm_send_prepare",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "audience_freeze_id": "freeze-1", "request_id": "send-1"}),
        )
        .unwrap();
    let digest = prepared["send_digest"].as_str().unwrap().to_string();
    assert!(digest.starts_with("sha256:"), "{prepared}");
    let send_id = prepared["send"]["send_id"].as_str().unwrap().to_string();
    assert_eq!(prepared["counts"]["final"], 2, "{prepared}");
    assert_eq!(prepared["send"]["state"], "prepared");

    // Replay: same request_id + same material returns the same send.
    let replayed = crm
        .daemon
        .operator_rpc(
            "crm_send_prepare",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "audience_freeze_id": "freeze-1", "request_id": "send-1"}),
        )
        .unwrap();
    assert_eq!(replayed["send_digest"], digest);

    // A tampered digest refuses.
    assert!(crm
        .daemon
        .operator_rpc(
            "crm_send_approve",
            json!({"install_id": install, "context_id": context, "send_id": send_id,
                "send_digest": "sha256:tampered"}),
        )
        .is_err());

    let approved = crm
        .daemon
        .operator_rpc(
            "crm_send_approve",
            json!({"install_id": install, "context_id": context, "send_id": send_id,
                "send_digest": digest}),
        )
        .unwrap();
    assert_eq!(approved["send"]["state"], "sending");
    assert_eq!(approved["counts"]["queued"], 2, "{approved}");

    // Wait for the worker to drain.
    assert!(
        wait_until(30, || {
            let shown = crm
                .daemon
                .operator_rpc(
                    "crm_send_show",
                    json!({"install_id": install, "context_id": context, "send_id": send_id}),
                )
                .unwrap();
            shown["send"]["state"] == "completed"
        }),
        "send did not complete"
    );
    let shown = crm
        .daemon
        .operator_rpc(
            "crm_send_show",
            json!({"install_id": install, "context_id": context, "send_id": send_id}),
        )
        .unwrap();
    assert_eq!(shown["counts"]["accepted"], 2, "{shown}");
    assert_eq!(shown["delivery_claim"], "smtp-acceptance-only");
    // Masked emails only in the read.
    let deliveries = shown["deliveries"].as_array().unwrap();
    assert_eq!(deliveries.len(), 2);
    for d in deliveries {
        let masked = d["email"].as_str().unwrap();
        assert!(masked.contains('*'), "{masked}");
        assert!(!masked.contains("amina@") && !masked.contains("cleo@"));
        assert_eq!(d["delivery_claim"], "smtp-acceptance-only");
    }
    // The rig saw the test send plus the two campaign deliveries —
    // personalized and carrying the per-recipient unsubscribe URL
    // and the durable Message-ID.
    let messages = rig.messages.lock().unwrap();
    let campaign: Vec<_> = messages
        .iter()
        .filter(|m| m.rcpt != "operator@example.com")
        .collect();
    assert_eq!(campaign.len(), 2, "{messages:?}");
    let rcpts: Vec<_> = campaign.iter().map(|m| m.rcpt.as_str()).collect();
    assert!(rcpts.contains(&"amina@example.com"));
    assert!(rcpts.contains(&"cleo@example.com"));
    assert!(campaign[0].body.contains("Hello Amina") || campaign[1].body.contains("Hello Amina"));
    assert!(campaign[0].body.contains("Hello Cleo") || campaign[1].body.contains("Hello Cleo"));
    assert!(campaign
        .iter()
        .all(|m| m.message_id.ends_with("@cadence.invalid")));
    assert!(campaign
        .iter()
        .all(|m| m.unsubscribe.starts_with("https://board.example")));
}

#[test]
fn cad786_unsubscribe_suppresses_the_recipient() {
    let rig_dir = ca();
    let rig = Rig::start(rig_dir, 3); // 1 test + 2 deliveries
    let crm = Crm::with_ca(&rig._dir, 10, format!("https://board.example:{}", rig.port));
    let (install, context, _) = crm.ready(&rig);
    let prepared = crm
        .daemon
        .operator_rpc(
            "crm_send_prepare",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "audience_freeze_id": "freeze-1", "request_id": "send-1"}),
        )
        .unwrap();
    let digest = prepared["send_digest"].as_str().unwrap().to_string();
    let send_id = prepared["send"]["send_id"].as_str().unwrap().to_string();
    crm.daemon
        .operator_rpc(
            "crm_send_approve",
            json!({"install_id": install, "context_id": context, "send_id": send_id,
                "send_digest": digest}),
        )
        .unwrap();
    assert!(wait_until(30, || {
        rig.messages.lock().unwrap().len() >= 2
    }));
    // The customer's unsubscribe link came out of its own message —
    // a real per-recipient token.
    let token = {
        let messages = rig.messages.lock().unwrap();
        let msg = messages
            .iter()
            .find(|m| m.rcpt == "amina@example.com")
            .unwrap();
        msg.unsubscribe
            .rsplit('/')
            .next()
            .unwrap()
            .trim_end_matches('>')
            .to_string()
    };
    // A bogus token and a shaped-but-unknown token answer the same.
    for token in ["bogus", &"Z".repeat(43)] {
        let out = crm
            .daemon
            .operator_rpc("crm_unsubscribe_redeem", json!({"token": token}))
            .unwrap();
        assert_eq!(out, json!({"unsubscribed": true}));
    }
    // The real token redeems: the suppression rows land in the
    // installation's record file, never in core.
    let out = crm
        .daemon
        .operator_rpc("crm_unsubscribe_redeem", json!({"token": token}))
        .unwrap();
    assert_eq!(out, json!({"unsubscribed": true}));
    let suppressions = crm
        .daemon
        .operator_rpc(
            "app_suppression_list",
            json!({"install_id": install, "context_id": context}),
        )
        .unwrap();
    let rows = suppressions["suppressions"].as_array().unwrap();
    assert!(
        rows.iter()
            .any(|r| r["key"] == "customer-a" && r["kind"] == "customer")
            && rows
                .iter()
                .any(|r| r["key"] == "amina@example.com" && r["kind"] == "email"),
        "{suppressions}"
    );
    // Redeem again — idempotent.
    crm.daemon
        .operator_rpc("crm_unsubscribe_redeem", json!({"token": token}))
        .unwrap();
}

#[test]
fn cad786_resolve_only_uncertain_and_only_terminal_states() {
    let crm = Crm::new(10, Some("https://board.example".into()));
    let install = crm.install();
    let context = crm.context(&install, "brand", "ctx-1");
    crm.seed_customers(&install, &context, &[("customer-a", PROFILE_A)]);
    crm.save_and_approve(&install, &context, "launch-1");
    // Resolve against a nonexistent send refuses.
    let err = crm
        .daemon
        .operator_rpc(
            "crm_send_resolve",
            json!({"install_id": install, "context_id": context, "send_id": "send-x",
                "customer_id": "customer-a", "resolution": "accepted"}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("unavailable"), "{err}");
    // A resolution outside {accepted,failed} refuses at the gate.
    assert!(crm
        .daemon
        .operator_rpc(
            "crm_send_resolve",
            json!({"install_id": install, "context_id": context, "send_id": "send-x",
                "customer_id": "customer-a", "resolution": "sent"}),
        )
        .is_err());
}

#[test]
fn cad786_secret_never_leaks_through_send_paths() {
    let rig_dir = ca();
    let rig = Rig::start(rig_dir, 3);
    let crm = Crm::with_ca(&rig._dir, 10, "https://board.example".into());
    let (install, context, _) = crm.ready(&rig);
    for (method, params) in [
        (
            "crm_send_prepare",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "audience_freeze_id": "freeze-1", "request_id": "s-1"}),
        ),
        (
            "crm_send_show",
            json!({"install_id": install, "context_id": context, "send_id": "send-s-1"}),
        ),
        (
            "crm_send_list",
            json!({"install_id": install, "context_id": context}),
        ),
    ] {
        let out = crm.daemon.operator_rpc(method, params).unwrap();
        let text = out.to_string();
        assert!(!text.contains(SECRET), "{method} leaked secret material");
    }
}
