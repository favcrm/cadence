//! CAD-1121 acceptance check, written from the ticket's policy by
//! someone other than the implementer: a hosted daemon never binds or
//! sends through a raw-SMTP connection; hosted mail goes only through
//! the platform email door.
//!
//! One real daemon and one real board (HTTP), a loopback implicit-TLS
//! SMTP rig that counts every connection, the real CRM installation, a
//! real SMTP connection enrolled and bound while SELF-HOSTED. The
//! daemon is then flipped hosted at runtime (the test-seam
//! `hosted_workspace` handle) and every raw-SMTP path must fail closed
//! with the typed `hosted_smtp_unsupported` code while the rig sees no
//! new connection.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::test_seam::{scoped, Asserted, Seam, SendRowGate};

const CODE: &str = "hosted_smtp_unsupported";
const SMTP_USER: &str = "smtp-user";

fn openssl(args: &[&str]) {
    let out = std::process::Command::new("openssl")
        .args(args)
        .output()
        .expect("openssl is required for the isolated SMTP rig");
    assert!(
        out.status.success(),
        "openssl {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A test CA and a `localhost` server certificate.
struct Pki {
    dir: tempfile::TempDir,
}

impl Pki {
    fn mint() -> Self {
        let dir = tempfile::Builder::new().prefix("c1121p").tempdir().unwrap();
        let p = |n: &str| dir.path().join(n).to_str().unwrap().to_string();
        openssl(&[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            &p("ca-key.pem"),
            "-out",
            &p("ca.pem"),
            "-days",
            "2",
            "-subj",
            "/CN=cadence-1121-ca",
        ]);
        std::fs::write(
            dir.path().join("ext.cnf"),
            "subjectAltName=DNS:localhost,IP:127.0.0.1\n",
        )
        .unwrap();
        openssl(&[
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            &p("srv-key.pem"),
            "-out",
            &p("srv.csr"),
            "-subj",
            "/CN=localhost",
        ]);
        openssl(&[
            "x509",
            "-req",
            "-in",
            &p("srv.csr"),
            "-CA",
            &p("ca.pem"),
            "-CAkey",
            &p("ca-key.pem"),
            "-CAcreateserial",
            "-days",
            "2",
            "-out",
            &p("srv-cert.pem"),
            "-extfile",
            &p("ext.cnf"),
        ]);
        Self { dir }
    }

    fn read(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.dir.path().join(name)).unwrap()
    }

    fn server_config(&self) -> Arc<rustls::ServerConfig> {
        let cert = self.read("srv-cert.pem");
        let key = self.read("srv-key.pem");
        let certs: Vec<_> = rustls_pemfile::certs(&mut cert.as_slice())
            .collect::<Result<_, _>>()
            .unwrap();
        let key = rustls_pemfile::private_key(&mut key.as_slice())
            .unwrap()
            .unwrap();
        Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .unwrap(),
        )
    }
}

/// A loopback implicit-TLS submission server that accepts everything
/// and counts every TCP connection it is dialled with.
struct Rig {
    port: u16,
    connections: Arc<AtomicUsize>,
    recipients: Arc<Mutex<Vec<String>>>,
}

impl Rig {
    fn start(pki: &Pki) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = pki.server_config();
        let connections = Arc::new(AtomicUsize::new(0));
        let recipients = Arc::new(Mutex::new(Vec::new()));
        let (count, rcpts) = (connections.clone(), recipients.clone());
        std::thread::spawn(move || {
            for accepted in listener.incoming() {
                let Ok(stream) = accepted else { return };
                count.fetch_add(1, Ordering::SeqCst);
                stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
                let (config, rcpts) = (config.clone(), rcpts.clone());
                std::thread::spawn(move || serve(config, stream, rcpts));
            }
        });
        Self {
            port,
            connections,
            recipients,
        }
    }

    /// Campaign recipients submitted so far (test sends go to the
    /// operator's address and are not counted).
    fn rows(&self) -> Vec<String> {
        let all = self.recipients.lock().unwrap();
        all.iter()
            .filter(|r| *r != "operator@example.com")
            .cloned()
            .collect()
    }

    fn seen(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

fn serve(config: Arc<rustls::ServerConfig>, stream: TcpStream, rcpts: Arc<Mutex<Vec<String>>>) {
    let Ok(conn) = rustls::ServerConnection::new(config) else {
        return;
    };
    let mut wire = BufReader::new(rustls::StreamOwned::new(conn, stream));
    let write = |wire: &mut BufReader<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>,
                 text: &str| {
        wire.get_mut()
            .write_all(format!("{text}\r\n").as_bytes())
            .and_then(|()| wire.get_mut().flush())
            .is_ok()
    };
    if !write(&mut wire, "220 localhost cadence-1121-rig") {
        return;
    }
    let mut line = String::new();
    loop {
        line.clear();
        if !matches!(wire.read_line(&mut line), Ok(n) if n > 0) {
            return;
        }
        let text = line.trim_end().to_string();
        let upper = text.to_ascii_uppercase();
        let ok = if upper.starts_with("EHLO") || upper.starts_with("HELO") {
            write(&mut wire, "250-localhost") && write(&mut wire, "250 AUTH PLAIN")
        } else if upper.starts_with("AUTH") {
            write(&mut wire, "235 ok")
        } else if upper.starts_with("MAIL FROM:") {
            write(&mut wire, "250 ok")
        } else if upper.starts_with("RCPT TO:") {
            if let (Some(s), Some(e)) = (text.find('<'), text.find('>')) {
                rcpts.lock().unwrap().push(text[s + 1..e].to_string());
            }
            write(&mut wire, "250 ok")
        } else if upper == "DATA" {
            if !write(&mut wire, "354 go") {
                return;
            }
            loop {
                line.clear();
                if !matches!(wire.read_line(&mut line), Ok(n) if n > 0) {
                    return;
                }
                if line.trim_end() == "." {
                    break;
                }
            }
            write(&mut wire, "250 accepted")
        } else if upper == "QUIT" {
            write(&mut wire, "221 bye");
            return;
        } else {
            write(&mut wire, "250 ok")
        };
        if !ok {
            return;
        }
    }
}

struct Stop(Arc<AtomicBool>, Vec<std::thread::JoinHandle<()>>);

impl Drop for Stop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
        for thread in self.1.drain(..).rev() {
            let _ = thread.join();
        }
    }
}

/// The daemon, its board and the hosted switch.
struct Fx {
    state: std::path::PathBuf,
    hosted: Arc<AtomicBool>,
    gate: Arc<SendRowGate>,
    agent: ureq::Agent,
    base: String,
    host: String,
    token: String,
    cookie: String,
    key: String,
    _stop: Stop,
    _dir: tempfile::TempDir,
}

fn start(pki: &Pki) -> Fx {
    let dir = tempfile::Builder::new().prefix("c1121").tempdir().unwrap();
    let state = dir.path().to_path_buf();
    let pm = state.join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let mut stop = Stop(Arc::new(AtomicBool::new(false)), Vec::new());
    let hosted = Arc::new(AtomicBool::new(false));
    let gate = SendRowGate::new();
    let mut opts = crate::daemon::ServeOptions {
        test_seam: true,
        stop: Some(stop.0.clone()),
        hosted_workspace: Some(hosted.clone()),
        crm_send_row_gate: Some(gate.clone()),
        crm_send_interval_ms: 5,
        unsubscribe_origin: Some("https://board.example".into()),
        smtp_test_ca_pem: Some(pki.read("ca.pem")),
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    crate::platform::smtp::attach(&mut opts);
    let daemon_state = state.clone();
    stop.1.push(std::thread::spawn(move || {
        crate::daemon::serve_with(&daemon_state, opts).unwrap()
    }));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
        assert!(Instant::now() < deadline, "daemon never started");
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut port = 3110 + (std::process::id() % 80) as u16;
    loop {
        let (startup, ready) = std::sync::mpsc::channel();
        let board = crate::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.0.clone()),
            startup: Some(startup),
            test_seam: true,
            ..Default::default()
        };
        let (state, pm) = (state.clone(), pm.clone());
        let thread = std::thread::spawn(move || drop(crate::ui::serve(&state, &pm, &board)));
        match ready.recv_timeout(Duration::from_secs(30)).unwrap() {
            Ok(()) => break stop.1.push(thread),
            Err(_) if port < 3199 => port += 1,
            Err(kind) => panic!("board could not bind: {kind:?}"),
        }
        thread.join().unwrap();
    }
    let token = Seam::token_at(&state).unwrap();
    let host = format!("cadence-{port}.localhost:{port}");
    crate::operator_auth::ensure_secret(&state).unwrap();
    let secret = crate::operator_auth::read_secret(&state).unwrap();
    let nonce = scoped(Asserted::Operator, || {
        crate::client::rpc(
            &state,
            "operator_link_mint",
            json!({"secret": secret, "origin": "loopback"}),
        )
    })
    .unwrap()["nonce"]
        .clone();
    let config = ureq::Agent::config_builder().http_status_as_error(false);
    let agent: ureq::Agent = config.build().into();
    let base = format!("http://127.0.0.1:{port}");
    let session = agent
        .post(format!("{base}/api/session"))
        .header("Host", &host)
        .header("X-Cadence-Board", "1")
        .header("Origin", format!("http://{host}"))
        .header(crate::test_seam::AS_HEADER, "operator")
        .header(crate::test_seam::TOKEN_HEADER, &token)
        .header("Content-Type", "application/json")
        .send(json!({"nonce": nonce}).to_string())
        .unwrap();
    let set = session.headers()["set-cookie"].to_str().unwrap();
    let cookie = set[..set.find(';').unwrap()].to_owned();
    let key: Value = session.into_body().read_json().unwrap();
    Fx {
        state,
        hosted,
        gate,
        agent,
        base,
        host,
        token,
        cookie,
        key: key["session_key"].as_str().unwrap().to_string(),
        _stop: stop,
        _dir: dir,
    }
}

impl Fx {
    fn rpc(&self, method: &str, params: Value) -> crate::error::Result<Value> {
        scoped(Asserted::Operator, || {
            crate::client::rpc(&self.state, method, params)
        })
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        self.rpc(method, params)
            .unwrap_or_else(|e| panic!("{method}: {e}"))
    }

    /// The operator's board POST: `(status, json body)`.
    fn http(&self, path: &str, body: Value) -> (u16, Value) {
        let mut response = self
            .agent
            .post(format!("{}{path}", self.base))
            .header("Host", &self.host)
            .header("X-Cadence-Board", "1")
            .header("Origin", format!("http://{}", self.host))
            .header(crate::test_seam::AS_HEADER, "operator")
            .header(crate::test_seam::TOKEN_HEADER, &self.token)
            .header("Cookie", &self.cookie)
            .header("X-Cadence-Session", &self.key)
            .header("Content-Type", "application/json")
            .send(body.to_string())
            .unwrap();
        let status = response.status().as_u16();
        (
            status,
            response.body_mut().read_json().unwrap_or(Value::Null),
        )
    }

    fn set_hosted(&self, on: bool) {
        self.hosted.store(on, Ordering::SeqCst);
    }
}

fn coded(result: crate::error::Result<Value>, what: &str) {
    let error = result.expect_err(&format!("{what} must be refused"));
    assert_eq!(error.code(), Some(CODE), "{what}: {error}");
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

const PROFILE: fn(&str, &str) -> Value = |name, email| {
    json!({"schema": 1, "display_name": name, "email": email, "tags": [],
           "consent": {"email": "granted"}})
};

/// CAD-1121: a hosted daemon never binds or sends through raw SMTP.
///
/// Self-hosted (positive controls): a raw-SMTP connection binds (RPC and
/// board), shows `usable: true`, test-sends (RPC and board) through the
/// rig, prepares a send and a worker submits its first row.
///
/// Then the daemon is flipped hosted. (a) bind and rebind of a raw-SMTP
/// connection are refused with `hosted_smtp_unsupported` over the RPC and
/// over the board (409 with the code) and nothing is bound or changed;
/// (b) the binding made while self-hosted shows `usable: false`, and
/// test-send (RPC and board), prepare, approve and the running worker all
/// fail closed with the typed code, and the rig sees NO new connection.
///
/// Guards (see the PR report for the mutation table): `Shared::is_hosted`
/// and `hosted_smtp_refusal` (connections_rpc.rs), `crm_sender_revision`
/// and `crm_smtp_authority` (crm_smtp_rpc.rs), the show projection, the
/// connection row's `sender_unusable`, the board's typed refusal
/// (ui/crm_smtp.rs).
#[test]
fn hosted_daemon_never_binds_or_sends_through_raw_smtp() {
    let pki = Pki::mint();
    let rig = Rig::start(&pki);
    let fx = start(&pki);

    // The real CRM installation, three contexts, customers, content.
    let source = format!("{}/workspace-apps/crm", env!("CARGO_MANIFEST_DIR"));
    let installed = fx.ok("app_workspace_install", json!({"source": source}));
    fx.ok(
        "app_local_install_approve",
        json!({"install_id": installed["install_id"], "digest": installed["digest"]}),
    );
    let install = installed["install_id"].as_str().unwrap().to_string();
    let context = |label: &str| -> String {
        let made = fx.ok(
            "app_context_create",
            json!({"install_id": install, "label": label, "input_defaults": {},
                   "request_id": format!("ctx-{}", label.to_lowercase())}),
        );
        made["context"]["id"].as_str().unwrap().to_string()
    };
    let (ctx1, ctx2, ctx3) = (context("Alpha"), context("Beta"), context("Gamma"));
    for (id, name, email) in [
        ("customer-a", "Amina Diallo", "amina@example.com"),
        ("customer-b", "Boris Feld", "boris@example.com"),
    ] {
        fx.ok(
            "app_record_create",
            json!({"install_id": install, "context_id": ctx1, "record_id": id,
                   "profile": PROFILE(name, email)}),
        );
    }
    let approve_content = |campaign: &str| {
        fx.ok(
            "app_content_save",
            json!({"install_id": install, "context_id": ctx1, "campaign_id": campaign,
                   "subject": "Spring launch", "preheader": "News",
                   "blocks": [{"type": "paragraph", "text": "A calm first line."}]}),
        );
        let shown = fx.ok(
            "app_content_show",
            json!({"install_id": install, "context_id": ctx1, "campaign_id": campaign}),
        );
        fx.ok(
            "app_content_approve",
            json!({"install_id": install, "context_id": ctx1, "campaign_id": campaign,
                   "expected_revision": shown["content"]["revision"]}),
        );
    };
    approve_content("launch-1");
    approve_content("launch-2");
    fx.ok(
        "app_audience_prepare",
        json!({"install_id": install, "context_id": ctx1, "freeze_id": "freeze-1",
               "base": {"mode": "all"}, "max_recipients": 50}),
    );
    // Two raw-SMTP connections, enrolled while self-hosted.
    let password = format!("{}-{}", "9fK2qW7zXm4p", "Lv8t63Rn5");
    let enroll = |account: &str| -> String {
        fx.ok(
            "connection_create",
            json!({"provider": "smtp", "account": account, "shape": "smtp",
                   "host": "localhost", "port": rig.port, "tls_mode": "implicit",
                   "username": SMTP_USER, "secret": password,
                   "sender": "news@example.com", "sender_name": "CRM News",
                   "scopes": ["email:send"], "accept_same_uid_risk": true}),
        )["connection"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (conn1, conn2) = (enroll("send-one"), enroll("send-two"));
    let ctx = |c: &str| json!({"install_id": install, "context_id": c});
    let with = |c: &str, extra: Value| {
        let mut p = ctx(c);
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        p
    };

    // ---- (c) self-hosted positive controls --------------------------
    fx.ok(
        "crm_smtp_bind",
        with(
            &ctx1,
            json!({"connection_id": conn1, "request_id": "bind-1"}),
        ),
    );
    let shown = fx.ok("crm_smtp_show", ctx(&ctx1));
    assert_eq!(shown["binding"]["usable"], json!(true), "{shown}");
    let (status, bound3) = fx.http(
        "/api/crm-smtp/bind",
        with(
            &ctx3,
            json!({"connection_id": conn1, "request_id": "bind-3"}),
        ),
    );
    assert_eq!(status, 200, "self-hosted board bind: {bound3}");
    let rev3 = bound3["binding"]["link_revision"].as_i64().unwrap();
    let (status, rebound) = fx.http(
        "/api/crm-smtp/rebind",
        with(
            &ctx3,
            json!({"connection_id": conn2, "expected_revision": rev3}),
        ),
    );
    assert_eq!(status, 200, "self-hosted board rebind: {rebound}");
    let rev3 = rebound["binding"]["link_revision"].as_i64().unwrap();
    let test = |campaign: &str| {
        with(
            &ctx1,
            json!({"campaign_id": campaign, "to_email": "operator@example.com"}),
        )
    };
    let receipt = fx.ok("crm_smtp_test_send", test("launch-1"));
    assert_eq!(receipt["receipt"]["accepted"], json!(true), "{receipt}");
    let (status, receipt) = fx.http("/api/crm-smtp/test-send", test("launch-2"));
    assert_eq!(status, 200, "self-hosted board test-send: {receipt}");
    assert_eq!(receipt["receipt"]["accepted"], json!(true), "{receipt}");
    assert_eq!(rig.seen(), 2, "the two test sends went through the rig");
    // Two sends: S1 (launch-1) runs under the row gate, S2 (launch-2)
    // stays prepared.
    let prepare = |campaign: &str, request: &str| {
        fx.rpc(
            "crm_send_prepare",
            with(
                &ctx1,
                json!({"campaign_id": campaign, "audience_freeze_id": "freeze-1",
                       "request_id": request}),
            ),
        )
    };
    let s1 = prepare("launch-1", "r1").unwrap_or_else(|e| panic!("self-hosted prepare: {e}"));
    let s2 = prepare("launch-2", "r2").unwrap();
    let approve = |send: &Value| {
        fx.rpc(
            "crm_send_approve",
            with(
                &ctx1,
                json!({"send_id": send["send"]["send_id"],
                       "send_digest": send["send_digest"]}),
            ),
        )
    };
    approve(&s1).unwrap_or_else(|e| panic!("self-hosted approve: {e}"));
    fx.gate.allow(1);
    until("the worker's first row reaches the rig", || {
        rig.rows().len() == 1
    });
    assert_eq!(rig.seen(), 3);

    // ---- flip hosted -------------------------------------------------
    fx.set_hosted(true);
    let before = rig.seen();

    // (a) bind / rebind with a raw-SMTP connection: typed refusal, RPC
    // and board, nothing bound or changed.
    coded(
        fx.rpc(
            "crm_smtp_bind",
            with(
                &ctx2,
                json!({"connection_id": conn1, "request_id": "bind-2"}),
            ),
        ),
        "RPC bind",
    );
    let (status, body) = fx.http(
        "/api/crm-smtp/bind",
        with(
            &ctx2,
            json!({"connection_id": conn2, "request_id": "bind-2b"}),
        ),
    );
    assert_eq!(
        (status, &body["code"]),
        (409, &json!(CODE)),
        "board bind: {body}"
    );
    coded(
        fx.rpc(
            "crm_smtp_rebind",
            with(
                &ctx3,
                json!({"connection_id": conn1, "expected_revision": rev3}),
            ),
        ),
        "RPC rebind",
    );
    let (status, body) = fx.http(
        "/api/crm-smtp/rebind",
        with(
            &ctx3,
            json!({"connection_id": conn1, "expected_revision": rev3}),
        ),
    );
    assert_eq!(
        (status, &body["code"]),
        (409, &json!(CODE)),
        "board rebind: {body}"
    );
    let err = fx
        .rpc("crm_smtp_show", ctx(&ctx2))
        .expect_err("ctx2 stays unbound");
    assert!(
        err.code() != Some(CODE) && err.to_string().contains("no SMTP sender"),
        "{err}"
    );
    let shown3 = fx.ok("crm_smtp_show", ctx(&ctx3));
    assert_eq!(
        shown3["binding"]["link_revision"],
        json!(rev3),
        "rebind changed the link"
    );
    assert_eq!(shown3["binding"]["connection_id"], json!(conn2));

    // (b) the existing binding is reported unusable and never used.
    let shown = fx.ok("crm_smtp_show", ctx(&ctx1));
    assert_eq!(shown["binding"]["usable"], json!(false), "{shown}");
    assert_eq!(shown["binding"]["unusable_reason"], json!(CODE), "{shown}");
    let listed = fx.ok("connection_list", json!({}));
    let row = listed["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!(conn1))
        .unwrap();
    assert_eq!(row["sender_unusable"], json!(CODE), "{row}");
    coded(
        fx.rpc("crm_smtp_test_send", test("launch-1")),
        "RPC test-send",
    );
    let (status, body) = fx.http("/api/crm-smtp/test-send", test("launch-1"));
    assert_eq!(
        (status, &body["code"]),
        (409, &json!(CODE)),
        "board test-send: {body}"
    );
    coded(prepare("launch-1", "r3"), "prepare");
    coded(approve(&s2), "send (approve)");
    // The running worker's next row: it must close the send, not dial.
    fx.gate.allow(1);
    until("the running send closes", || {
        let shown = fx.ok(
            "crm_send_show",
            with(&ctx1, json!({"send_id": s1["send"]["send_id"]})),
        );
        shown["send"]["state"] != json!("sending")
    });
    let closed = fx.ok(
        "crm_send_show",
        with(&ctx1, json!({"send_id": s1["send"]["send_id"]})),
    );
    let reason = closed["send"]["close_reason"].as_str().unwrap_or_default();
    assert!(reason.contains("SMTP is for self-hosted"), "{closed}");
    assert_eq!(rig.rows().len(), 1, "the worker submitted a second row");
    assert_eq!(
        rig.seen(),
        before,
        "a hosted daemon opened an SMTP connection"
    );
}
