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
use cadence_agent::daemon::ServeOptions;
use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_member_pane, LaneShell, TestDaemon};
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
const PROFILE_D: &str = r#"{"schema":1,"display_name":"Dana Cole","email":"dana@example.com","tags":[],"consent":{"email":"granted"}}"#;
const PROFILE_E: &str = r#"{"schema":1,"display_name":"Erin Vale","email":"erin@example.com","tags":[],"consent":{"email":"granted"}}"#;

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

/// Scripted per-recipient behaviour: the act for a connection is
/// popped from `script[rcpt]` once RCPT TO arrives; a missing or
/// empty queue means `Accept`.
#[derive(Clone, Copy, Debug)]
enum Act {
    /// 250 at end-of-data.
    Accept,
    /// A 4xx answer at end-of-data — deferred, retryable.
    Defer(u16),
    /// A 5xx answer at end-of-data — permanently rejected.
    Reject(u16),
    /// The connection dies before the terminator is written — the
    /// message was never submitted.
    DropEarly,
    /// The connection dies after `.` without answering — delivery
    /// is genuinely unknown.
    DropLate,
}

/// One connection per message, in worker order; the accept loop
/// runs for the test's lifetime. `messages` records every DATA
/// block that arrived; `rcpt_log` records every RCPT TO — the
/// submission count, even for dropped connections.
struct Rig {
    port: u16,
    messages: Arc<Mutex<Vec<RigMessage>>>,
    rcpt_log: Arc<Mutex<Vec<String>>>,
    _dir: tempfile::TempDir,
}

impl Rig {
    fn start(
        dir: tempfile::TempDir,
        script: std::collections::HashMap<String, std::collections::VecDeque<Act>>,
    ) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = server_config(dir.path());
        let sink = Arc::new(Mutex::new(Vec::new()));
        let rcpt_log = Arc::new(Mutex::new(Vec::new()));
        let script = Arc::new(Mutex::new(script));
        let (writer, rcpts) = (sink.clone(), rcpt_log.clone());
        std::thread::spawn(move || loop {
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
                continue;
            }
            let mut act = Act::Accept;
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
                                if !rig_write(&mut reader, "504 unrecognized") {
                                    break;
                                }
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
                        rcpts.lock().unwrap().push(message.rcpt.clone());
                        act = script
                            .lock()
                            .unwrap()
                            .get_mut(&message.rcpt)
                            .and_then(|queue| queue.pop_front())
                            .unwrap_or(Act::Accept);
                        if !rig_write(&mut reader, "250 ok") {
                            break;
                        }
                    }
                    "DATA" => {
                        if !authed || matches!(act, Act::DropEarly) {
                            break; // connection dies before the data terminator
                        }
                        if !rig_write(&mut reader, "354 go") {
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
                            if let Some(url) = data.strip_prefix("List-Unsubscribe: <") {
                                message.unsubscribe = url.trim_end_matches('>').to_string();
                            }
                        }
                        match act {
                            Act::Accept => {
                                if !rig_write(&mut reader, "250 accepted") {
                                    break;
                                }
                            }
                            Act::Defer(code) => {
                                if !rig_write(&mut reader, &format!("{code} busy")) {
                                    break;
                                }
                            }
                            Act::Reject(code) => {
                                if !rig_write(&mut reader, &format!("{code} refused")) {
                                    break;
                                }
                            }
                            Act::DropLate => break,
                            Act::DropEarly => unreachable!(),
                        }
                        writer.lock().unwrap().push(message.clone());
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
        });
        Self {
            port,
            messages: sink,
            rcpt_log,
            _dir: dir,
        }
    }

    /// Every 43-char bearer token the rig observed in
    /// `List-Unsubscribe` headers.
    fn tokens(&self) -> Vec<String> {
        self.messages
            .lock()
            .unwrap()
            .iter()
            .map(|m| m.unsubscribe.rsplit('/').next().unwrap().to_string())
            .filter(|t| t.len() == 43)
            .collect()
    }
}

// ---------- the fixture ----------

struct Crm {
    root: tempfile::TempDir,
    _pm: Pm,
    daemon: TestDaemon,
    /// The options the daemon started with — `restart` reuses them.
    opts: ServeOptions,
    /// State dirs of replaced daemons, kept so their contents survive
    /// a restart.
    holds: Vec<tempfile::TempDir>,
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
        let daemon = TestDaemon::start_opts(opts.clone());
        Self {
            root,
            _pm: pm,
            daemon,
            opts,
            holds: Vec::new(),
        }
    }

    /// A fixture whose daemon pins the rig CA, a real unsubscribe
    /// origin and a fast send interval — the shape every live-send
    /// test needs.
    fn with_ca(ca: &tempfile::TempDir, interval_ms: u64, origin: String) -> Self {
        Self::with_ca_opt(ca, interval_ms, Some(origin))
    }

    /// Same, with the serve-opt origin unset when `None`.
    fn with_ca_opt(ca: &tempfile::TempDir, interval_ms: u64, origin: Option<String>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pm = Pm::init(&dir.path().join("pm")).unwrap();
        Self::copy_source(&dir.path().join("source"), "blog-post");
        let mut opts = daemon_opts();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        cadence_agent::platform::smtp::attach(&mut opts);
        opts.crm_send_interval_ms = interval_ms;
        opts.unsubscribe_origin = origin;
        opts.smtp_test_ca_pem = Some(std::fs::read(ca.path().join("ca.pem")).unwrap());
        let daemon = TestDaemon::start_opts(opts.clone());
        Self {
            root: dir,
            _pm: pm,
            daemon,
            opts,
            holds: Vec::new(),
        }
    }

    /// Same, with a per-row budget gate parked into the send worker.
    /// `test-seam` builds only — the type does not exist elsewhere.
    #[cfg(feature = "test-seam")]
    fn with_ca_gated(
        ca: &tempfile::TempDir,
        interval_ms: u64,
        origin: Option<String>,
        gate: Option<Arc<cadence_agent::test_seam::SendRowGate>>,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pm = Pm::init(&dir.path().join("pm")).unwrap();
        Self::copy_source(&dir.path().join("source"), "blog-post");
        let mut opts = daemon_opts();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        cadence_agent::platform::smtp::attach(&mut opts);
        opts.crm_send_interval_ms = interval_ms;
        opts.unsubscribe_origin = origin;
        opts.smtp_test_ca_pem = Some(std::fs::read(ca.path().join("ca.pem")).unwrap());
        #[cfg(feature = "test-seam")]
        {
            opts.crm_send_row_gate = gate;
        }
        let daemon = TestDaemon::start_opts(opts.clone());
        Self {
            root: dir,
            _pm: pm,
            daemon,
            opts,
            holds: Vec::new(),
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
        self.install_as("blog-post")
    }

    /// Restart the daemon on the same state dir — crash-recovery
    /// coverage for the send reconciler. `TestDaemon.state` lives
    /// inside its `dir` TempDir, which Drop deletes — so the dir is
    /// swapped out and kept alive for the fixture's lifetime before
    /// the old daemon is dropped (shutdown + join releases the
    /// state flock `await_singleton_released` waits on).
    fn restart(&mut self) {
        let state = self.daemon.state.clone();
        let dummy_dir = tempfile::tempdir().unwrap();
        let dummy_state = dummy_dir.path().join("state");
        let mut old = std::mem::replace(
            &mut self.daemon,
            TestDaemon {
                dir: dummy_dir,
                state: dummy_state,
                handle: None,
                process: None,
            },
        );
        self.holds.push(std::mem::replace(
            &mut old.dir,
            tempfile::tempdir().unwrap(),
        ));
        drop(old);
        self.daemon = TestDaemon::start_on_opts(state, self.opts.clone());
    }

    /// A fresh install of the same source under a new app name — a
    /// second `blog-post` install refuses, so each `ready` lane of a
    /// multi-case test names its own app.
    fn install_as(&self, app: &str) -> String {
        let source = if app == "blog-post" {
            self.root.path().join("source")
        } else {
            let into = self.root.path().join(format!("source-{app}"));
            Self::copy_source(&into, app);
            into
        };
        self.daemon
            .operator_rpc("app_workspace_install", json!({"source": source}))
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

    fn save_draft(&self, install: &str, context: &str, campaign: &str, subject: &str) {
        // The observed revision is required on existing content.
        let current = self
            .daemon
            .operator_rpc(
                "app_content_show",
                json!({"install_id": install, "context_id": context, "campaign_id": campaign}),
            )
            .ok();
        let revision = current
            .as_ref()
            .and_then(|v| v["content"]["revision"].as_u64())
            .unwrap_or(0);
        let mut params = json!({"install_id": install, "context_id": context, "campaign_id": campaign,
            "subject": subject, "preheader": "News", "blocks": blocks()});
        if revision > 0 {
            params["expected_revision"] = json!(revision);
        }
        self.daemon
            .operator_rpc("app_content_save", params)
            .unwrap();
    }

    fn save_and_approve(&self, install: &str, context: &str, campaign: &str) {
        self.save_draft(install, context, campaign, "Spring launch");
        let revision = self
            .daemon
            .operator_rpc(
                "app_content_show",
                json!({"install_id": install, "context_id": context, "campaign_id": campaign}),
            )
            .unwrap()["content"]["revision"]
            .as_u64()
            .unwrap();
        self.daemon
            .operator_rpc(
                "app_content_approve",
                json!({"install_id": install, "context_id": context, "campaign_id": campaign, "expected_revision": revision}),
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

    fn freeze_excl(
        &self,
        install: &str,
        context: &str,
        freeze: &str,
        base: Value,
        exclusion: &str,
        max: i64,
    ) {
        self.daemon
            .operator_rpc(
                "app_audience_prepare",
                json!({"install_id": install, "context_id": context, "freeze_id": freeze,
                    "base": base, "exclusion_list_id": exclusion, "max_recipients": max}),
            )
            .unwrap();
    }

    fn exclusion(&self, install: &str, context: &str, list: &str, members: &[&str]) {
        self.daemon
            .operator_rpc(
                "app_exclusion_save",
                json!({"install_id": install, "context_id": context, "list_id": list,
                    "name": "Skip", "member_ids": members}),
            )
            .unwrap();
    }

    fn prepare(
        &self,
        install: &str,
        context: &str,
        campaign: &str,
        freeze: &str,
        request: &str,
    ) -> Value {
        self.daemon
            .operator_rpc(
                "crm_send_prepare",
                json!({"install_id": install, "context_id": context, "campaign_id": campaign,
                    "audience_freeze_id": freeze, "request_id": request}),
            )
            .unwrap()
    }

    fn approve(&self, install: &str, context: &str, send_id: &str, digest: &str) -> Value {
        self.daemon
            .operator_rpc(
                "crm_send_approve",
                json!({"install_id": install, "context_id": context, "send_id": send_id,
                    "send_digest": digest}),
            )
            .unwrap()
    }

    fn show(&self, install: &str, context: &str, send_id: &str) -> Value {
        self.daemon
            .operator_rpc(
                "crm_send_show",
                json!({"install_id": install, "context_id": context, "send_id": send_id}),
            )
            .unwrap()
    }

    #[cfg(feature = "test-seam")]
    fn suppress_customer(&self, install: &str, context: &str, customer: &str) {
        self.daemon
            .operator_rpc(
                "app_suppression_add",
                json!({"install_id": install, "context_id": context, "customer_id": customer,
                    "reason": "unsubscribe"}),
            )
            .unwrap();
    }

    /// install + context + the given customers + approved content +
    /// a freeze of `base` (+optional exclusion list) + bound link +
    /// accepted test send. Returns (install, context, connection_id).
    fn ready_custom(
        &self,
        rig: &Rig,
        profiles: &[(&str, &str)],
        base: Value,
        exclusion: Option<&str>,
        max: i64,
        request: &str,
    ) -> (String, String, String) {
        let install = self.install_as(request);
        let context = self.context(&install, "brand", &format!("{request}-ctx"));
        self.seed_customers(&install, &context, profiles);
        self.save_and_approve(&install, &context, "launch-1");
        if let Some(list) = exclusion {
            self.freeze_excl(&install, &context, "freeze-1", base, list, max);
        } else {
            self.freeze(&install, &context, "freeze-1", base, max);
        }
        let connection = self.enroll("send-smtp", rig.port);
        let connection_id = connection["id"].as_str().unwrap().to_string();
        self.bind(&install, &context, &connection_id, "bind-1");
        self.test_send(&install, &context, "launch-1");
        (install, context, connection_id)
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
        self.ready_named(rig, "blog-post")
    }

    fn ready_named(&self, rig: &Rig, app: &str) -> (String, String, String) {
        let install = self.install_as(app);
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
        let connection = self.enroll(&format!("smtp-{app}"), rig.port);
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
    let rig = Rig::start(rig_dir, Default::default());
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
    let rig = Rig::start(rig_dir, Default::default()); // 1 test + 2 deliveries
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
    let rig = Rig::start(rig_dir, Default::default());
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

// ---------- the detached-child gate (B1) ----------

fn native(
    lane: &mut LaneShell,
    state: &Path,
    detached: bool,
    method: &str,
    params: Value,
) -> Value {
    let frame = json!({"method": method, "params": params});
    let request = lane.dir.path().join(format!("send-{}.json", lane.seq));
    std::fs::write(&request, frame.to_string()).unwrap();
    let prefix = if detached { "setsid " } else { "" };
    let (rc, text) = lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.socket(socket.AF_UNIX);s.settimeout(10);s.connect(sys.argv[1]);s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");print(s.makefile().readline())' {} {}", state.join("cadence.sock").display(), request.display()));
    assert_eq!(rc, 0, "native socket process failed: {text}");
    serde_json::from_str(text.trim()).unwrap()
}

#[test]
fn cad786_detached_child_cannot_reach_send_verbs() {
    let crm = Crm::new(10, Some("https://board.example".into()));
    let install = crm.install();
    let context = crm.context(&install, "brand", "ctx-1");
    let mut lane = LaneShell::spawn(crm.daemon.dir.path());
    plant_member_pane(&crm.daemon, "send-peer", "claude", None, lane.pid());
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
            "crm_send_resolve",
            json!({"install_id": install, "context_id": context, "send_id": "send-p-1",
                "customer_id": "customer-a", "resolution": "accepted"}),
        ),
    ] {
        let frame = native(&mut lane, &crm.daemon.state, true, method, params);
        assert_eq!(frame["ok"], false, "{method}: {frame}");
        assert!(
            frame["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("operator action"),
            "{method}: {frame}"
        );
    }
}

// ---------- stale inputs refuse with zero rows, zero traffic (B2) ----------

#[test]
fn cad786_stale_inputs_refuse_zero_rows_zero_traffic() {
    let rig_dir = ca();
    let rig = Rig::start(rig_dir, Default::default());
    let crm = Crm::with_ca(&rig._dir, 10, "https://board.example".into());

    let rig_submissions = |rig: &Rig| -> usize {
        rig.rcpt_log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| *r != "operator@example.com")
            .count()
    };
    let deliveries_of = |crm: &Crm, install: &str, context: &str, send_id: &str| -> usize {
        crm.daemon
            .operator_rpc(
                "crm_send_show",
                json!({"install_id": install, "context_id": context, "send_id": send_id}),
            )
            .map(|v| v["deliveries"].as_array().unwrap().len())
            .unwrap_or(0)
    };

    // 1. Content edited (and re-approved) after prepare → new digest.
    {
        let (install, context, _) = crm.ready_named(&rig, "staleone");
        let prepared = crm.prepare(&install, &context, "launch-1", "freeze-1", "edit-1");
        let digest = prepared["send_digest"].as_str().unwrap().to_string();
        let send_id = prepared["send"]["send_id"].as_str().unwrap().to_string();
        let before = rig_submissions(&rig);
        crm.save_and_approve(&install, &context, "launch-1"); // revision 2
        assert!(crm
            .daemon
            .operator_rpc(
                "crm_send_approve",
                json!({"install_id": install, "context_id": context, "send_id": send_id,
                    "send_digest": digest}),
            )
            .is_err());
        assert_eq!(deliveries_of(&crm, &install, &context, &send_id), 0);
        assert_eq!(rig_submissions(&rig), before);
    }
    // 2. Approval cleared: saved revision 2, never re-approved.
    {
        let (install, context, _) = crm.ready_named(&rig, "staletwo");
        let prepared = crm.prepare(&install, &context, "launch-1", "freeze-1", "edit-2");
        let digest = prepared["send_digest"].as_str().unwrap().to_string();
        let send_id = prepared["send"]["send_id"].as_str().unwrap().to_string();
        let before = rig_submissions(&rig);
        crm.save_draft(&install, &context, "launch-1", "Changed"); // sits draft — approval cleared
        assert!(crm
            .daemon
            .operator_rpc(
                "crm_send_approve",
                json!({"install_id": install, "context_id": context, "send_id": send_id,
                    "send_digest": digest}),
            )
            .is_err());
        assert_eq!(deliveries_of(&crm, &install, &context, &send_id), 0);
        assert_eq!(rig_submissions(&rig), before);
    }
    // 3. Audience drift: consent revoked after the freeze.
    {
        let (install, context, _) = crm.ready_named(&rig, "stalethree");
        let prepared = crm.prepare(&install, &context, "launch-1", "freeze-1", "drift-3");
        let digest = prepared["send_digest"].as_str().unwrap().to_string();
        let send_id = prepared["send"]["send_id"].as_str().unwrap().to_string();
        let before = rig_submissions(&rig);
        crm.daemon
            .operator_rpc(
                "app_record_update",
                json!({"install_id": install, "context_id": context, "record_id": "customer-a",
                    "expected_revision": 1,
                    "profile": {"schema": 1, "display_name": "Amina Diallo", "email": "amina@example.com",
                        "tags": [], "consent": {"email": "denied"}}}),
            )
            .unwrap();
        assert!(crm
            .daemon
            .operator_rpc(
                "crm_send_approve",
                json!({"install_id": install, "context_id": context, "send_id": send_id,
                    "send_digest": digest}),
            )
            .is_err());
        assert_eq!(deliveries_of(&crm, &install, &context, &send_id), 0);
        assert_eq!(rig_submissions(&rig), before);
    }
    // 4. Credential rotated: the prepared auth_revision is stale.
    {
        let (install, context, connection) = crm.ready_named(&rig, "stalefour");
        let prepared = crm.prepare(&install, &context, "launch-1", "freeze-1", "rot-4");
        let digest = prepared["send_digest"].as_str().unwrap().to_string();
        let send_id = prepared["send"]["send_id"].as_str().unwrap().to_string();
        let before = rig_submissions(&rig);
        crm.daemon
            .operator_rpc(
                "connection_rotate",
                json!({"connection_id": connection, "secret": concat!("9wK4-zT7m", "-Qx3p-Vn8r-52")}),
            )
            .unwrap();
        assert!(crm
            .daemon
            .operator_rpc(
                "crm_send_approve",
                json!({"install_id": install, "context_id": context, "send_id": send_id,
                    "send_digest": digest}),
            )
            .is_err());
        assert_eq!(deliveries_of(&crm, &install, &context, &send_id), 0);
        assert_eq!(rig_submissions(&rig), before);
    }
    // 5. Link rebound: link_revision moved past the prepared pin.
    {
        let (install, context, connection) = crm.ready_named(&rig, "stalefive");
        let prepared = crm.prepare(&install, &context, "launch-1", "freeze-1", "reb-5");
        let digest = prepared["send_digest"].as_str().unwrap().to_string();
        let send_id = prepared["send"]["send_id"].as_str().unwrap().to_string();
        let before = rig_submissions(&rig);
        crm.daemon
            .operator_rpc(
                "crm_smtp_rebind",
                json!({"install_id": install, "context_id": context,
                    "connection_id": connection, "expected_revision": 1}),
            )
            .unwrap();
        assert!(crm
            .daemon
            .operator_rpc(
                "crm_send_approve",
                json!({"install_id": install, "context_id": context, "send_id": send_id,
                    "send_digest": digest}),
            )
            .is_err());
        assert_eq!(deliveries_of(&crm, &install, &context, &send_id), 0);
        assert_eq!(rig_submissions(&rig), before);
    }
    // 6. Link revoked entirely.
    {
        let (install, context, _) = crm.ready_named(&rig, "stalesix");
        let prepared = crm.prepare(&install, &context, "launch-1", "freeze-1", "rev-6");
        let digest = prepared["send_digest"].as_str().unwrap().to_string();
        let send_id = prepared["send"]["send_id"].as_str().unwrap().to_string();
        let before = rig_submissions(&rig);
        crm.daemon
            .operator_rpc(
                "crm_smtp_revoke",
                json!({"install_id": install, "context_id": context, "expected_revision": 1}),
            )
            .unwrap();
        assert!(crm
            .daemon
            .operator_rpc(
                "crm_send_approve",
                json!({"install_id": install, "context_id": context, "send_id": send_id,
                    "send_digest": digest}),
            )
            .is_err());
        assert_eq!(deliveries_of(&crm, &install, &context, &send_id), 0);
        assert_eq!(rig_submissions(&rig), before);
    }
    // 7. No accepted test send for this campaign's content+link:
    //    launch-2 was approved but never test-sent.
    {
        let (install, context, _) = crm.ready_named(&rig, "staleseven");
        crm.save_and_approve(&install, &context, "launch-2");
        let before = rig_submissions(&rig);
        assert!(crm
            .daemon
            .operator_rpc(
                "crm_send_prepare",
                json!({"install_id": install, "context_id": context, "campaign_id": "launch-2",
                    "audience_freeze_id": "freeze-1", "request_id": "evid-7"}),
            )
            .is_err());
        assert_eq!(rig_submissions(&rig), before);
    }
    // 8. Context archived: the scope proof refuses everything.
    {
        let (install, context, _) = crm.ready_named(&rig, "staleeight");
        let prepared = crm.prepare(&install, &context, "launch-1", "freeze-1", "arch-8");
        let digest = prepared["send_digest"].as_str().unwrap().to_string();
        let send_id = prepared["send"]["send_id"].as_str().unwrap().to_string();
        let before = rig_submissions(&rig);
        let revision = crm
            .daemon
            .operator_rpc(
                "app_context_show",
                json!({"install_id": install, "context_id": context}),
            )
            .unwrap()["context"]["revision"]
            .as_u64()
            .unwrap();
        crm.daemon
            .operator_rpc(
                "app_context_archive",
                json!({"install_id": install, "context_id": context, "expected_revision": revision}),
            )
            .unwrap();
        assert!(crm
            .daemon
            .operator_rpc(
                "crm_send_approve",
                json!({"install_id": install, "context_id": context, "send_id": send_id,
                    "send_digest": digest}),
            )
            .is_err());
        assert_eq!(rig_submissions(&rig), before);
    }
}

// ---------- concurrent approve: exactly one winner (B3) ----------

#[test]
fn cad786_concurrent_approves_one_wins_once() {
    let rig_dir = ca();
    let rig = Rig::start(rig_dir, Default::default());
    let mut crm = Crm::with_ca(&rig._dir, 10, "https://board.example".into());
    let (install, context, _) = crm.ready(&rig);
    let prepared = crm.prepare(&install, &context, "launch-1", "freeze-1", "send-race");
    let digest = prepared["send_digest"].as_str().unwrap().to_string();
    let send_id = prepared["send"]["send_id"].as_str().unwrap().to_string();

    let mut results = Vec::new();
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..6 {
            let digest = digest.clone();
            let send_id = send_id.clone();
            let daemon = &crm.daemon;
            let install = &install;
            let context = &context;
            handles.push(scope.spawn(move || {
                daemon.operator_rpc(
                    "crm_send_approve",
                    json!({"install_id": install, "context_id": context, "send_id": send_id,
                        "send_digest": digest}),
                )
            }));
        }
        for handle in handles {
            results.push(handle.join().unwrap());
        }
    });
    let wins = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(wins, 1, "expected exactly one winning approve: {results:?}");

    assert!(wait_until(30, || {
        crm.show(&install, &context, &send_id)["send"]["state"] == "completed"
    }));
    let shown = crm.show(&install, &context, &send_id);
    assert_eq!(shown["counts"]["accepted"], 2, "{shown}");
    // Exactly two delivery rows — a losing approve minted nothing.
    assert_eq!(shown["deliveries"].as_array().unwrap().len(), 2);
    // The core intent is the winner's, completed — a loser must not
    // have closed or rewritten it.
    let core = rusqlite::Connection::open(crm.daemon.state.join("cadence.sqlite3")).unwrap();
    let (core_state, core_install): (String, String) = core
        .query_row(
            "SELECT state, install_id FROM crm_sends WHERE send_id=?",
            rusqlite::params![send_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(core_state, "completed", "core send row: {core_state}");
    assert_eq!(core_install, install);
    // A losing approve's `crm_send_open` hit the same PK — exactly
    // one core row exists for this send.
    let core_rows: i64 = core
        .query_row(
            "SELECT COUNT(*) FROM crm_sends WHERE send_id=?",
            rusqlite::params![send_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(core_rows, 1, "losing approves left stray core rows");
    drop(core);
    // A restart reconciles nothing: the core intent is completed,
    // so no second worker spawns and no row is touched.
    crm.restart();
    let after = crm.show(&install, &context, &send_id);
    assert_eq!(after["send"]["state"], "completed");
    assert_eq!(after["counts"]["accepted"], 2, "{after}");
    assert_eq!(after["deliveries"].as_array().unwrap().len(), 2);
    let rcpts = rig.rcpt_log.lock().unwrap().clone();
    assert_eq!(
        rcpts
            .iter()
            .filter(|r| *r != "operator@example.com")
            .count(),
        2,
        "restart resent a recipient"
    );
    // Exactly one submission per recipient at the rig.
    let log = rig.rcpt_log.lock().unwrap();
    for email in ["amina@example.com", "cleo@example.com"] {
        assert_eq!(
            log.iter().filter(|r| *r == email).count(),
            1,
            "{email} submitted more than once: {log:?}"
        );
    }
}

// ---------- mid-send suppression and link revoke (B4) ----------

#[cfg(feature = "test-seam")]
#[test]
fn cad786_mid_send_suppression_and_revoke() {
    let rig_dir = ca();
    let rig = Rig::start(rig_dir, Default::default());
    // The row gate parks the worker between recipients — the test
    // mutates state while the worker is provably stopped, never on
    // a wall-clock interval.
    let gate = cadence_agent::test_seam::SendRowGate::new();
    let crm = Crm::with_ca_gated(
        &rig._dir,
        10,
        Some("https://board.example".into()),
        Some(gate.clone()),
    );
    let (install, context, _) = crm.ready_custom(
        &rig,
        &[
            ("customer-a", PROFILE_A),
            ("customer-c", PROFILE_C),
            ("customer-d", PROFILE_D),
        ],
        json!({"mode": "all"}),
        None,
        50,
        "mid-1",
    );
    let prepared = crm.prepare(&install, &context, "launch-1", "freeze-1", "mid-1");
    let digest = prepared["send_digest"].as_str().unwrap().to_string();
    let send_id = prepared["send"]["send_id"].as_str().unwrap().to_string();
    crm.approve(&install, &context, &send_id, &digest);

    // Budget 0: the worker parked on its first iteration — nothing
    // can submit until the test admits a row.
    gate.allow(1);
    assert!(
        wait_until(30, || {
            rig.rcpt_log
                .lock()
                .unwrap()
                .contains(&"amina@example.com".to_string())
        }),
        "first row never submitted"
    );

    // The worker is parked again. Suppress the second recipient and
    // revoke the third's consent before either is claimed.
    crm.suppress_customer(&install, &context, "customer-c");
    crm.daemon
        .operator_rpc(
            "app_record_update",
            json!({"install_id": install, "context_id": context, "record_id": "customer-d",
                "expected_revision": 1,
                "profile": {"schema": 1, "display_name": "Dana Cole", "email": "dana@example.com",
                    "tags": [], "consent": {"email": "denied"}}}),
        )
        .unwrap();
    // Two rows + one exit-check iteration.
    gate.allow(3);
    assert!(wait_until(30, || {
        crm.show(&install, &context, &send_id)["send"]["state"] == "completed"
    }));
    let shown = crm.show(&install, &context, &send_id);
    assert_eq!(shown["counts"]["accepted"], 1, "{shown}");
    assert_eq!(shown["counts"]["suppressed"], 2, "{shown}");
    // Neither suppressed row ever touched the wire.
    let log = rig.rcpt_log.lock().unwrap();
    assert_eq!(log.iter().filter(|r| *r == "cleo@example.com").count(), 0);
    assert_eq!(log.iter().filter(|r| *r == "dana@example.com").count(), 0);
    drop(log);

    // A second send, link revoked mid-flight: the parked worker's
    // next row dies at the authority check — remaining rows close,
    // the send and the core intent close, nothing else submits.
    crm.freeze(&install, &context, "freeze-2", json!({"mode": "all"}), 50);
    let prepared = crm.prepare(&install, &context, "launch-1", "freeze-2", "mid-2");
    let digest = prepared["send_digest"].as_str().unwrap().to_string();
    let send_id2 = prepared["send"]["send_id"].as_str().unwrap().to_string();
    crm.approve(&install, &context, &send_id2, &digest);
    let rigged = rig.rcpt_log.lock().unwrap().len();
    crm.daemon
        .operator_rpc(
            "crm_smtp_revoke",
            json!({"install_id": install, "context_id": context, "expected_revision": 1}),
        )
        .unwrap();
    gate.allow(1); // one row: claim → authority dead → close all
    assert!(
        wait_until(30, || {
            crm.show(&install, &context, &send_id2)["send"]["state"] == "closed"
        }),
        "send never closed"
    );
    let shown = crm.show(&install, &context, &send_id2);
    assert_eq!(shown["counts"]["accepted"], 0, "{shown}");
    assert_eq!(
        rig.rcpt_log.lock().unwrap().len(),
        rigged,
        "rig saw post-revoke traffic"
    );
}

// ---------- crash reconciliation (B5) ----------

#[test]
fn cad786_crash_marks_submitting_uncertain_never_resent() {
    let rig_dir = ca();
    let rig = Rig::start(rig_dir, Default::default());
    let ca_pem = std::fs::read(rig._dir.path().join("ca.pem")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let pm = Pm::init(&dir.path().join("pm")).unwrap();
    Crm::copy_source(&dir.path().join("source"), "blog-post");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let mut opts = daemon_opts();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
    cadence_agent::platform::smtp::attach(&mut opts);
    opts.crm_send_interval_ms = 60000; // worker sleeps between rows
    opts.unsubscribe_origin = Some("https://board.example".into());
    opts.smtp_test_ca_pem = Some(ca_pem.clone());
    let daemon = TestDaemon::start_on_opts(state.clone(), opts);

    // Drive the fixture by hand — Crm owns a daemon, so borrow it
    // into a thin wrapper.
    let install = daemon
        .operator_rpc(
            "app_workspace_install",
            json!({"source": dir.path().join("source")}),
        )
        .unwrap()["install_id"]
        .as_str()
        .unwrap()
        .to_string();
    let context = daemon
        .operator_rpc(
            "app_context_create",
            json!({"install_id": install, "label": "brand", "input_defaults": {}, "request_id": "ctx-1"}),
        )
        .unwrap()["context"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    for (id, profile) in [
        ("customer-a", PROFILE_A),
        ("customer-c", PROFILE_C),
        ("customer-d", PROFILE_D),
    ] {
        daemon
            .operator_rpc(
                "app_record_create",
                json!({"install_id": install, "context_id": context, "record_id": id,
                    "profile": serde_json::from_str::<Value>(profile).unwrap()}),
            )
            .unwrap();
    }
    daemon
        .operator_rpc(
            "app_content_save",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "subject": "Spring launch", "preheader": "News", "blocks": blocks()}),
        )
        .unwrap();
    daemon
        .operator_rpc(
            "app_content_approve",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1", "expected_revision": 1}),
        )
        .unwrap();
    daemon
        .operator_rpc(
            "app_audience_prepare",
            json!({"install_id": install, "context_id": context, "freeze_id": "freeze-1",
                "base": {"mode": "all"}, "max_recipients": 50}),
        )
        .unwrap();
    let connection = daemon
        .operator_rpc(
            "connection_create",
            json!({"provider": "smtp", "account": "send-smtp", "shape": "smtp",
                "host": "localhost", "port": rig.port, "tls_mode": "implicit",
                "username": "smtp-user", "secret": SECRET,
                "sender": "news@example.com", "sender_name": "CRM News",
                "scopes": ["email:send"], "accept_same_uid_risk": true}),
        )
        .unwrap()["connection"]
        .clone();
    daemon
        .operator_rpc(
            "crm_smtp_bind",
            json!({"install_id": install, "context_id": context,
                "connection_id": connection["id"].as_str().unwrap(), "request_id": "bind-1"}),
        )
        .unwrap();
    daemon
        .operator_rpc(
            "crm_smtp_test_send",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "to_email": "operator@example.com"}),
        )
        .unwrap();
    let prepared = daemon
        .operator_rpc(
            "crm_send_prepare",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "audience_freeze_id": "freeze-1", "request_id": "crash-1"}),
        )
        .unwrap();
    let digest = prepared["send_digest"].as_str().unwrap().to_string();
    let send_id = prepared["send"]["send_id"].as_str().unwrap().to_string();
    daemon
        .operator_rpc(
            "crm_send_approve",
            json!({"install_id": install, "context_id": context, "send_id": send_id,
                "send_digest": digest}),
        )
        .unwrap();
    // The first row submits; the worker then sleeps 60s mid-queue.
    assert!(wait_until(30, || {
        rig.rcpt_log
            .lock()
            .unwrap()
            .contains(&"amina@example.com".to_string())
    }));
    // Daemon "crashes" mid-queue; make one queued row look claimed.
    drop(daemon);
    let record_db = state.join("app-records").join(format!("{install}.sqlite3"));
    {
        let conn = rusqlite::Connection::open(&record_db).unwrap();
        conn.execute(
            "UPDATE app_campaign_deliveries SET state='submitting', attempts=1 WHERE context_id=? AND send_id=? AND customer_id='customer-c'",
            rusqlite::params![context, send_id],
        )
        .unwrap();
    }
    // Restart on the same state: reconciliation must mark the
    // interrupted row uncertain and finish the rest exactly once.
    let mut opts = daemon_opts();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
    cadence_agent::platform::smtp::attach(&mut opts);
    opts.crm_send_interval_ms = 10;
    opts.unsubscribe_origin = Some("https://board.example".into());
    opts.smtp_test_ca_pem = Some(ca_pem);
    let daemon = TestDaemon::start_on_opts(state, opts);
    assert!(wait_until(60, || {
        daemon
            .operator_rpc(
                "crm_send_show",
                json!({"install_id": install, "context_id": context, "send_id": send_id}),
            )
            .map(|v| v["send"]["state"] == "completed")
            .unwrap_or(false)
    }));
    let shown = daemon
        .operator_rpc(
            "crm_send_show",
            json!({"install_id": install, "context_id": context, "send_id": send_id}),
        )
        .unwrap();
    let state_of = |shown: &Value, customer: &str| {
        shown["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["customer_id"] == customer)
            .map(|d| {
                (
                    d["state"].as_str().unwrap().to_string(),
                    d["attempts"].as_i64().unwrap(),
                )
            })
            .unwrap()
    };
    assert_eq!(state_of(&shown, "customer-a"), ("accepted".into(), 1));
    assert_eq!(
        state_of(&shown, "customer-c"),
        ("uncertain".into(), 1),
        "{shown}"
    );
    assert_eq!(state_of(&shown, "customer-d"), ("accepted".into(), 1));
    // The uncertain row never resubmits; dana submitted exactly once.
    let log = rig.rcpt_log.lock().unwrap();
    assert_eq!(log.iter().filter(|r| *r == "cleo@example.com").count(), 0);
    assert_eq!(log.iter().filter(|r| *r == "dana@example.com").count(), 1);
    drop(log);
    // Operator resolves the uncertain row: no SMTP traffic.
    let resolved = daemon
        .operator_rpc(
            "crm_send_resolve",
            json!({"install_id": install, "context_id": context, "send_id": send_id,
                "customer_id": "customer-c", "resolution": "accepted"}),
        )
        .unwrap();
    assert_eq!(resolved["delivery"]["resolved_by"], "operator");
    assert_eq!(
        rig.rcpt_log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| *r == "cleo@example.com")
            .count(),
        0
    );
}

// ---------- SMTP classification end to end (B6) ----------

#[test]
fn cad786_classification_end_to_end() {
    let rig_dir = ca();
    let mut script: std::collections::HashMap<String, std::collections::VecDeque<Act>> =
        Default::default();
    // amina: 4xx then accept → attempts=2, accepted.
    script.insert(
        "amina@example.com".into(),
        [Act::Defer(450), Act::Accept].into(),
    );
    // cleo: 5xx → failed, never retried.
    script.insert("cleo@example.com".into(), [Act::Reject(550)].into());
    // dana: drop before DATA three times → failed at the bound.
    script.insert(
        "dana@example.com".into(),
        [Act::DropEarly, Act::DropEarly, Act::DropEarly].into(),
    );
    // erin: drop after the terminator → uncertain, never resent.
    script.insert("erin@example.com".into(), [Act::DropLate].into());
    let rig = Rig::start(rig_dir, script);
    let crm = Crm::with_ca(&rig._dir, 10, "https://board.example".into());
    let (install, context, _) = crm.ready_custom(
        &rig,
        &[
            ("customer-a", PROFILE_A),
            ("customer-c", PROFILE_C),
            ("customer-d", PROFILE_D),
            ("customer-e", PROFILE_E),
        ],
        json!({"mode": "all"}),
        None,
        50,
        "cls-1",
    );
    let prepared = crm.prepare(&install, &context, "launch-1", "freeze-1", "cls-1");
    let digest = prepared["send_digest"].as_str().unwrap().to_string();
    let send_id = prepared["send"]["send_id"].as_str().unwrap().to_string();
    crm.approve(&install, &context, &send_id, &digest);

    assert!(wait_until(60, || {
        crm.show(&install, &context, &send_id)["send"]["state"] == "completed"
    }));
    let shown = crm.show(&install, &context, &send_id);
    let row = |customer: &str| -> Value {
        shown["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["customer_id"] == customer)
            .cloned()
            .unwrap()
    };
    assert_eq!(row("customer-a")["state"], "accepted");
    assert_eq!(row("customer-a")["attempts"], 2, "{}", row("customer-a"));
    assert_eq!(row("customer-a")["smtp_code"], 250);
    assert_eq!(row("customer-c")["state"], "failed");
    assert_eq!(row("customer-c")["attempts"], 1);
    assert_eq!(row("customer-c")["smtp_code"], 550);
    assert_eq!(row("customer-d")["state"], "failed");
    assert_eq!(row("customer-d")["attempts"], 3, "bounded retries");
    assert_eq!(row("customer-e")["state"], "uncertain");
    assert_eq!(row("customer-e")["attempts"], 1);

    let log = rig.rcpt_log.lock().unwrap();
    assert_eq!(log.iter().filter(|r| *r == "amina@example.com").count(), 2);
    assert_eq!(log.iter().filter(|r| *r == "cleo@example.com").count(), 1);
    assert_eq!(log.iter().filter(|r| *r == "dana@example.com").count(), 3);
    assert_eq!(log.iter().filter(|r| *r == "erin@example.com").count(), 1);
    drop(log);

    // Uncertain resolves — both directions — with zero SMTP traffic.
    let resolved = crm
        .daemon
        .operator_rpc(
            "crm_send_resolve",
            json!({"install_id": install, "context_id": context, "send_id": send_id,
                "customer_id": "customer-e", "resolution": "accepted"}),
        )
        .unwrap();
    assert_eq!(resolved["delivery"]["resolved_by"], "operator");
    assert_eq!(
        rig.rcpt_log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| *r == "erin@example.com")
            .count(),
        1,
        "resolve must never resend"
    );
    // A terminal row refuses a second resolution.
    assert!(crm
        .daemon
        .operator_rpc(
            "crm_send_resolve",
            json!({"install_id": install, "context_id": context, "send_id": send_id,
                "customer_id": "customer-e", "resolution": "failed"}),
        )
        .is_err());
}

// ---------- bounded campaign + PII hygiene (B8, B9) ----------

#[test]
fn cad786_bounded_campaign_counts_and_no_pii_in_core() {
    let rig_dir = ca();
    let rig = Rig::start(rig_dir, Default::default());
    let crm = Crm::with_ca(&rig._dir, 10, "https://board.example".into());
    let profiles = [
        ("customer-a", PROFILE_A), // Amina, granted
        ("customer-b", PROFILE_B), // Boris, denied
        ("customer-c", PROFILE_C), // Cleo, granted
        ("customer-d", PROFILE_D), // Dana, granted → excluded by list
        ("customer-e", PROFILE_E), // Erin, granted
    ];
    let install = crm.install();
    let context = crm.context(&install, "brand", "ctx-1");
    crm.seed_customers(&install, &context, &profiles);
    crm.save_and_approve(&install, &context, "launch-1");
    crm.exclusion(&install, &context, "ex-skip", &["customer-d"]);
    // An audience larger than its own ceiling refuses at freeze —
    // the send bound can never see an over-large member list.
    assert!(crm
        .daemon
        .operator_rpc(
            "app_audience_prepare",
            json!({"install_id": install, "context_id": context, "freeze_id": "too-big",
                "base": {"mode": "all"}, "max_recipients": 2}),
        )
        .is_err());
    crm.freeze_excl(
        &install,
        &context,
        "freeze-1",
        json!({"mode": "all"}),
        "ex-skip",
        50,
    );
    let connection = crm.enroll("send-smtp", rig.port);
    let connection_id = connection["id"].as_str().unwrap().to_string();
    crm.bind(&install, &context, &connection_id, "bind-1");
    crm.test_send(&install, &context, "launch-1");

    let prepared = crm.prepare(&install, &context, "launch-1", "freeze-1", "bnd-1");
    let counts = &prepared["counts"];
    assert_eq!(counts["included"], 5, "{counts}");
    assert_eq!(counts["excluded"], 1, "{counts}");
    assert_eq!(counts["suppressed_now"], 1, "{counts}");
    assert_eq!(counts["final"], 3, "{counts}");
    let digest = prepared["send_digest"].as_str().unwrap().to_string();
    let send_id = prepared["send"]["send_id"].as_str().unwrap().to_string();
    crm.approve(&install, &context, &send_id, &digest);
    assert!(wait_until(60, || {
        crm.show(&install, &context, &send_id)["send"]["state"] == "completed"
    }));
    let shown = crm.show(&install, &context, &send_id);
    assert_eq!(shown["counts"]["accepted"], 3, "{shown}");

    // The rig got exactly three multipart messages, personalized,
    // each with distinct tokens and Message-IDs.
    let messages = rig.messages.lock().unwrap();
    let campaign: Vec<_> = messages
        .iter()
        .filter(|m| m.rcpt != "operator@example.com")
        .collect();
    assert_eq!(campaign.len(), 3);
    let names = ["Hello Amina", "Hello Cleo", "Hello Erin"];
    assert!(names
        .iter()
        .all(|n| campaign.iter().any(|m| m.body.contains(n))));
    let mut tokens = campaign
        .iter()
        .map(|m| m.unsubscribe.clone())
        .collect::<Vec<_>>();
    tokens.sort();
    tokens.dedup();
    assert_eq!(tokens.len(), 3, "distinct unsubscribe URLs required");
    let mut ids = campaign
        .iter()
        .map(|m| m.message_id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 3);
    assert!(campaign.iter().all(|m| m
        .body
        .contains("List-Unsubscribe-Post: List-Unsubscribe=One-Click")));
    drop(messages);

    // B9 + A-proof: raw unsubscribe tokens and the SMTP secret live
    // in neither the core DB bytes nor the installation record-file
    // bytes; customer PII is in the record file but never in core.
    let state = &crm.daemon.state;
    let core_bytes = std::fs::read(state.join("cadence.sqlite3")).unwrap();
    let record_bytes =
        std::fs::read(state.join("app-records").join(format!("{install}.sqlite3"))).unwrap();
    let hay =
        |bytes: &[u8], needle: &str| bytes.windows(needle.len()).any(|w| w == needle.as_bytes());
    for pii in [
        "amina@example.com",
        "cleo@example.com",
        "erin@example.com",
        "Amina",
        "Cleo",
        "Erin",
        SECRET,
    ] {
        assert!(!hay(&core_bytes, pii), "core DB carries {pii}");
    }
    assert!(!hay(&record_bytes, SECRET));
    // The raw bearer tokens from the delivered messages persist
    // nowhere — only their sha256 hashes do.
    for token in rig.tokens() {
        assert!(!hay(&core_bytes, &token), "core DB carries a raw token");
        assert!(
            !hay(&record_bytes, &token),
            "record file carries a raw token"
        );
        let hash = cadence_agent::store::app_sends::unsubscribe_token_hash(&token);
        let hexhash = hash.trim_start_matches("sha256:");
        assert!(
            hay(&record_bytes, hexhash),
            "record file lost the token hash"
        );
    }
    // And the real token still redeems.
    let token = rig.tokens().pop().unwrap();
    let out = crm
        .daemon
        .operator_rpc("crm_unsubscribe_redeem", json!({"token": token}))
        .unwrap();
    assert_eq!(out, json!({"unsubscribed": true}));
}

// ---------- concurrent resolve: exactly one winner ----------

#[test]
fn cad786_concurrent_resolves_one_wins() {
    let rig_dir = ca();
    let mut script: std::collections::HashMap<String, std::collections::VecDeque<Act>> =
        Default::default();
    script.insert("amina@example.com".into(), [Act::DropLate].into());
    script.insert("cleo@example.com".into(), [Act::Accept].into());
    let rig = Rig::start(rig_dir, script);
    let crm = Crm::with_ca(&rig._dir, 10, "https://board.example".into());
    let (install, context, _) = crm.ready(&rig); // a + c in the freeze
    let prepared = crm.prepare(&install, &context, "launch-1", "freeze-1", "res-race");
    let digest = prepared["send_digest"].as_str().unwrap().to_string();
    let send_id = prepared["send"]["send_id"].as_str().unwrap().to_string();
    crm.approve(&install, &context, &send_id, &digest);
    assert!(wait_until(60, || {
        crm.show(&install, &context, &send_id)["send"]["state"] == "completed"
    }));

    let mut results = Vec::new();
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..6 {
            let daemon = &crm.daemon;
            let install = &install;
            let context = &context;
            let send_id = &send_id;
            handles.push(scope.spawn(move || {
                daemon.operator_rpc(
                    "crm_send_resolve",
                    json!({"install_id": install, "context_id": context, "send_id": send_id,
                        "customer_id": "customer-a", "resolution": "accepted"}),
                )
            }));
        }
        for handle in handles {
            results.push(handle.join().unwrap());
        }
    });
    let wins = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(wins, 1, "expected exactly one winning resolve: {results:?}");
    let shown = crm.show(&install, &context, &send_id);
    let row = shown["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["customer_id"] == "customer-a")
        .unwrap();
    assert_eq!(row["state"], "accepted");
    assert_eq!(row["resolved_by"], "operator");
}

// ---------- the persisted unsubscribe origin (item 4) ----------

#[test]
fn cad786_origin_setting_drives_prepare_and_gates() {
    let rig_dir = ca();
    let rig = Rig::start(rig_dir, Default::default());
    // No serve-opt origin: prepare must refuse and name the route.
    let crm = Crm::with_ca_opt(&rig._dir, 10, None);
    let install = crm.install_as("orig1");
    let context = crm.context(&install, "brand", "ctx-1");
    crm.seed_customers(&install, &context, &[("customer-a", PROFILE_A)]);
    crm.save_and_approve(&install, &context, "launch-1");
    crm.freeze(&install, &context, "freeze-1", json!({"mode": "all"}), 50);
    let err = crm
        .daemon
        .operator_rpc(
            "crm_send_prepare",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "audience_freeze_id": "freeze-1", "request_id": "o-1"}),
        )
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("unsubscribe origin") && err.contains("/api/crm-send/origin"),
        "{err}"
    );

    // Agent and detached child cannot set the origin.
    crm.daemon.register("send-gate");
    assert!(crm
        .daemon
        .agent_rpc(
            "send-gate",
            "crm_send_origin_set",
            json!({"unsubscribe_origin": "https://unsub.example.com"}),
        )
        .unwrap_err()
        .to_string()
        .contains("operator action"));
    let mut lane = LaneShell::spawn(crm.daemon.dir.path());
    plant_member_pane(&crm.daemon, "send-peer", "claude", None, lane.pid());
    let frame = native(
        &mut lane,
        &crm.daemon.state,
        true,
        "crm_send_origin_set",
        json!({"unsubscribe_origin": "https://unsub.example.com"}),
    );
    assert_eq!(frame["ok"], false, "{frame}");

    // Invalid origins refuse — every shape the validator forbids.
    for bad in [
        "http://example.com",                                // non-loopback http
        "https://example.com/x",                             // path
        "https://example.com/?q=1",                          // query
        "https://user@example.com",                          // userinfo
        "https://example.com/#frag",                         // fragment
        "ftp://example.com",                                 // scheme
        &format!("https://{}.example.com", "a".repeat(300)), // >200 bytes
        "https://example.com",                               // placeholder replaced below
    ] {
        if bad == "https://example.com" {
            continue;
        }
        assert!(
            crm.daemon
                .operator_rpc("crm_send_origin_set", json!({"unsubscribe_origin": bad}),)
                .is_err(),
            "{bad} accepted"
        );
    }
    let shown = crm
        .daemon
        .operator_rpc("crm_send_origin_show", json!({}))
        .unwrap();
    assert_eq!(shown["unsubscribe_origin"], Value::Null);
    assert_eq!(shown["stored"], false);

    // Set → show reflects the normalized value; bind + test send +
    // prepare succeed against the rig.
    crm.daemon
        .operator_rpc(
            "crm_send_origin_set",
            json!({"unsubscribe_origin": "https://unsub.example.com/"}),
        )
        .unwrap();
    let shown = crm
        .daemon
        .operator_rpc("crm_send_origin_show", json!({}))
        .unwrap();
    assert_eq!(
        shown["unsubscribe_origin"], "https://unsub.example.com",
        "trailing slash normalized"
    );
    let connection = crm.enroll("smtp-orig1", rig.port);
    let connection_id = connection["id"].as_str().unwrap().to_string();
    crm.bind(&install, &context, &connection_id, "bind-1");
    crm.test_send(&install, &context, "launch-1");
    let prepared = crm.prepare(&install, &context, "launch-1", "freeze-1", "o-1");
    assert_eq!(
        prepared["send"]["unsubscribe_origin"],
        "https://unsub.example.com"
    );
    let digest = prepared["send_digest"].as_str().unwrap().to_string();
    let send_id = prepared["send"]["send_id"].as_str().unwrap().to_string();

    // Changing the origin after prepare refuses at approve — zero
    // delivery rows minted.
    crm.daemon
        .operator_rpc(
            "crm_send_origin_set",
            json!({"unsubscribe_origin": "https://elsewhere.example.com"}),
        )
        .unwrap();
    assert!(crm
        .daemon
        .operator_rpc(
            "crm_send_approve",
            json!({"install_id": install, "context_id": context, "send_id": send_id,
                "send_digest": digest}),
        )
        .is_err());
    assert_eq!(
        crm.show(&install, &context, &send_id)["deliveries"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    // Clearing returns the setting to unset — prepare refuses again.
    crm.daemon
        .operator_rpc("crm_send_origin_set", json!({"unsubscribe_origin": null}))
        .unwrap();
    assert!(crm
        .daemon
        .operator_rpc(
            "crm_send_prepare",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "audience_freeze_id": "freeze-1", "request_id": "o-2"}),
        )
        .is_err());
}
