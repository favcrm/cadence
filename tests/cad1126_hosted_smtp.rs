//! CAD-1126 acceptance: hosted tenant SMTP goes only through
//! `smtp.internal`, and the credential never leaves the daemon.
//!
//! A real in-process daemon with the hosted pass-through set, a loopback
//! fake `smtp.internal` shaped exactly like contract v2, and a real CRM
//! install with an approved campaign. Every address and the credential
//! are synthetic; the credential is built at runtime.
#![cfg(feature = "test-seam")]

use base64::{engine::general_purpose::STANDARD, Engine};
use cadence_agent::platform::smtp_internal::SmtpInternal;
use cadence_agent::test_seam::{scoped, Asserted};
use cadence_agent::{client, daemon, store::Store};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const RECIPIENT: &str = "operator@example.com";
const SENDER: &str = "owner@restaurant.example";

// ---------- the fake smtp.internal ----------

#[derive(Clone)]
struct Seen {
    path: String,
    body: Value,
}

struct Fake {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Fake {
    fn start(password: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (log, password) = (Arc::clone(&log), password.clone());
                std::thread::spawn(move || serve(stream, log, &password));
            }
        });
        Self { url, seen }
    }
    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

fn reply(stream: &mut TcpStream, status: u16, body: &Value) {
    let text = body.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
}

fn serve(mut stream: TcpStream, log: Arc<Mutex<Vec<Seen>>>, password: &str) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let length = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
        })
        .unwrap_or(0);
    while buf.len() < head_end + length {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body: Value = serde_json::from_slice(&buf[head_end..head_end + length]).unwrap_or(Value::Null);
    let path = head
        .lines()
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .unwrap_or("")
        .to_string();
    log.lock().unwrap().push(Seen {
        path: path.clone(),
        body: body.clone(),
    });
    // The contract: strict server block, only 465 implicit or 587 starttls.
    let server = &body["server"];
    let port = server["port"].as_u64().unwrap_or(0);
    let mode = server["tls_mode"].as_str().unwrap_or("");
    let shape_ok = matches!((port, mode), (465, "implicit") | (587, "starttls"))
        && server["host"].as_str().is_some_and(|h| !h.is_empty());
    if !shape_ok || !matches!(path.as_str(), "/v1/send" | "/v1/verify") {
        return reply(
            &mut stream,
            400,
            &json!({"ok": false, "error": {"code": "invalid", "step": "validate", "message": "x"}}),
        );
    }
    if server["password"].as_str() != Some(password) {
        // A real server's banner can echo anything; the daemon must not.
        return reply(
            &mut stream,
            401,
            &json!({"ok": false, "error": {"code": "auth", "step": "auth",
                "message": format!("535 5.7.8 rejected {}", server["password"].as_str().unwrap_or(""))}}),
        );
    }
    if path == "/v1/verify" {
        return reply(&mut stream, 200, &json!({"ok": true, "data": {"verified": true}}));
    }
    let to = body["envelope"]["to"].clone();
    reply(
        &mut stream,
        200,
        &json!({"ok": true, "data": {"accepted": to, "rejected": [], "server_reply": "250 2.0.0 OK queued"}}),
    );
}

// ---------- the hosted daemon with a real CRM campaign ----------

struct Fx {
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn blocks() -> Value {
    json!([
        {"type": "heading", "text": "Hello {{first_name|friend}}"},
        {"type": "paragraph", "text": "A calm first line."},
        {"type": "button", "label": "Read more", "url": "https://example.com/posts/welcome"},
    ])
}

impl Fx {
    fn start(relay: Option<&Fake>) -> Self {
        let root = tempfile::Builder::new().prefix("c1126").tempdir().unwrap();
        let pm = root.path().join("pm");
        cadence_agent::issue::Pm::init(&pm).unwrap();
        for name in [
            "app.md",
            "workflows/blog-post.md",
            "rubrics/blog.md",
            "templates/brief.md",
            "templates/post.md",
        ] {
            let to = root.path().join("source").join(name);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("apps/blog-post")
                    .join(name),
                to,
            )
            .unwrap();
        }
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let mut opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(stop.clone()),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            crm_send_interval_ms: 10,
            unsubscribe_origin: Some("https://board.example".into()),
            ..Default::default()
        };
        cadence_agent::platform::smtp::attach(&mut opts);
        if let Some(relay) = relay {
            opts.smtp_internal = Some(SmtpInternal::new(&relay.url).unwrap());
        }
        let state = root.path().join("s");
        let thread = std::thread::spawn({
            let state = state.clone();
            move || daemon::serve_with(&state, opts).unwrap()
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(2)).is_err()
            || cadence_agent::test_seam::Seam::token_at(&state).is_none()
        {
            assert!(Instant::now() < deadline, "daemon never started");
            std::thread::sleep(Duration::from_millis(50));
        }
        let fx = Self {
            root,
            stop,
            thread: Some(thread),
        };
        fx.team();
        fx
    }
    fn state(&self) -> PathBuf {
        self.root.path().join("s")
    }
    /// A registered agent, so an agent-asserted caller is a real one.
    fn team(&self) {
        let store = Store::open(&self.state().join("cadence.sqlite3")).unwrap();
        let cwd = self.root.path().to_str().unwrap();
        store
            .register_agent(&cadence_agent::store::NewAgent {
                alias: "writer",
                provider: "claude",
                endpoint_kind: "managed",
                role: "worker",
                cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
    }
    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let state = self.state();
        scoped(who, || client::rpc(&state, method, params))
    }
    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }
    fn campaign(&self) -> (String, String) {
        let install = self.op(
            "app_workspace_install",
            json!({"source": self.root.path().join("source")}),
        )["install_id"]
            .as_str()
            .unwrap()
            .to_string();
        let context = self.op(
            "app_context_create",
            json!({"install_id": install, "label": "brand", "input_defaults": {}, "request_id": "ctx-1"}),
        )["context"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        self.op(
            "app_record_create",
            json!({"install_id": install, "context_id": context, "record_id": "customer-a",
                "profile": {"schema":1,"display_name":"Amina Diallo","email":"amina@example.com","tags":[],"consent":{"email":"granted"}}}),
        );
        self.op(
            "app_content_save",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "subject": "Spring launch", "preheader": "News", "blocks": blocks()}),
        );
        let revision = self.op(
            "app_content_show",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1"}),
        )["content"]["revision"]
            .as_u64()
            .unwrap();
        self.op(
            "app_content_approve",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1", "expected_revision": revision}),
        );
        self.op(
            "app_audience_prepare",
            json!({"install_id": install, "context_id": context, "freeze_id": "freeze-1",
                "base": {"mode": "all"}, "max_recipients": 50}),
        );
        (install, context)
    }
}

fn smtp_params(host: &str, secret: &str) -> Value {
    json!({
        "provider": "smtp", "account": "owner", "shape": "smtp",
        "host": host, "port": 465, "tls_mode": "implicit",
        "username": SENDER, "secret": secret, "sender": SENDER,
        "sender_name": "Restaurant", "scopes": ["email:send"],
        "accept_same_uid_risk": true,
    })
}

fn wait_until(secs: u64, probe: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if probe() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

/// Every file under `dir` except the custody directory, as bytes.
fn scan_for(dir: &Path, needle: &[u8], hits: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|n| n == "custody") {
            continue;
        }
        if path.is_dir() {
            scan_for(&path, needle, hits);
        } else if let Ok(bytes) = std::fs::read(&path) {
            if bytes.windows(needle.len()).any(|w| w == needle) {
                hits.push(path);
            }
        }
    }
}

#[test]
fn hosted_smtp_goes_only_through_smtp_internal_and_the_secret_never_leaves_the_daemon() {
    // Built at runtime; never a literal in the source.
    let password = format!("pw-{}-{}", std::process::id(), uuid::Uuid::new_v4().simple());
    let wrong = format!("bad-{}", uuid::Uuid::new_v4().simple());
    let relay = Fake::start(password.clone());
    let fx = Fx::start(Some(&relay));
    let dials = cadence_agent::platform::smtp::direct_dial_count();

    // A wrong password is refused at connect, in plain words, with a
    // typed code, before anything is stored; the relay saw `/v1/verify`.
    let refused = fx
        .rpc(Asserted::Operator, "connection_create", smtp_params("smtp.gmail.com", &wrong))
        .unwrap_err();
    let refused_text = format!("{refused:?} {refused}");
    assert!(refused_text.contains("check the app password"), "{refused_text}");
    assert!(refused_text.contains("smtp_auth"), "{refused_text}");
    assert!(!refused_text.contains(&wrong), "the error echoed the secret");
    assert!(
        !fx.op("connection_list", json!({}))["connections"]
            .to_string()
            .contains("\"account\":\"owner\""),
        "a refused enrolment left a connection behind"
    );
    assert_eq!(relay.seen().last().unwrap().path, "/v1/verify");

    // (b) An agent, and a caller that proves nothing, can neither enrol,
    // bind, read, nor send; the relay sees nothing more from them.
    let (install, context) = fx.campaign();
    let before = relay.count();
    let connected = fx.op("connection_create", smtp_params("smtp.gmail.com", &password));
    assert_eq!(relay.count(), before + 1, "enrolment verifies exactly once");
    let id = connected["connection"]["id"].as_str().unwrap().to_string();
    let before = relay.count();
    for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
        for (method, params) in [
            ("connection_create", smtp_params("smtp.gmail.com", &password)),
            ("connection_rotate", json!({"connection_id": id, "secret": password})),
            ("connection_show", json!({"connection_id": id})),
            ("connection_list", json!({})),
            (
                "crm_smtp_bind",
                json!({"install_id": install, "context_id": context, "connection_id": id, "request_id": "bind-x"}),
            ),
            (
                "crm_smtp_test_send",
                json!({"install_id": install, "context_id": context, "campaign_id": "launch-1", "to_email": RECIPIENT}),
            ),
        ] {
            let refusal = fx.rpc(who.clone(), method, params);
            assert!(refusal.is_err(), "{method} must refuse {}", who.as_str());
        }
    }
    assert_eq!(relay.count(), before, "a refused caller reached smtp.internal");

    // (a) The operator binds and test-sends: the relay receives the
    // RFC 5322 message and the credential, and no socket was dialled.
    let bound = fx.op(
        "crm_smtp_bind",
        json!({"install_id": install, "context_id": context, "connection_id": id, "request_id": "bind-1"}),
    );
    assert_eq!(bound["binding"]["sender"]["address"], SENDER);
    let sent = fx.op(
        "crm_smtp_test_send",
        json!({"install_id": install, "context_id": context, "campaign_id": "launch-1", "to_email": RECIPIENT}),
    );
    assert_eq!(sent["receipt"]["accepted"], true, "{sent}");
    let call = relay.seen().into_iter().rfind(|s| s.path == "/v1/send").unwrap();
    assert_eq!(call.body["server"]["host"], "smtp.gmail.com");
    assert_eq!(call.body["server"]["password"], password.as_str());
    assert_eq!(call.body["envelope"]["from"], SENDER);
    assert_eq!(call.body["envelope"]["to"], json!([RECIPIENT]));
    let message =
        String::from_utf8(STANDARD.decode(call.body["message_b64"].as_str().unwrap()).unwrap()).unwrap();
    for header in ["Date: ", "Message-ID: <", "MIME-Version: 1.0", "Subject: Spring launch"] {
        assert!(message.contains(header), "missing {header}:\n{message}");
    }
    assert!(message.contains(&format!("To: <{RECIPIENT}>")));
    assert!(!message.contains(&password));

    // An approved campaign send takes the same path, with its
    // List-Unsubscribe header, to the subscriber.
    let prepared = fx.op(
        "crm_send_prepare",
        json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
            "audience_freeze_id": "freeze-1", "request_id": "send-1"}),
    );
    let (digest, send_id) = (
        prepared["send_digest"].as_str().unwrap().to_string(),
        prepared["send"]["send_id"].as_str().unwrap().to_string(),
    );
    fx.op(
        "crm_send_approve",
        json!({"install_id": install, "context_id": context, "send_id": send_id, "send_digest": digest}),
    );
    assert!(
        wait_until(30, || relay
            .seen()
            .iter()
            .any(|s| s.body["envelope"]["to"] == json!(["amina@example.com"]))),
        "the campaign message never reached smtp.internal"
    );
    let campaign = relay
        .seen()
        .into_iter()
        .find(|s| s.body["envelope"]["to"] == json!(["amina@example.com"]))
        .unwrap();
    let campaign_message =
        String::from_utf8(STANDARD.decode(campaign.body["message_b64"].as_str().unwrap()).unwrap())
            .unwrap();
    assert!(campaign_message.contains("List-Unsubscribe: <https://board.example/unsubscribe/"));
    assert_eq!(
        cadence_agent::platform::smtp::direct_dial_count(),
        dials,
        "a hosted send opened a direct SMTP socket"
    );

    // (c) The password is in no response, event, thread entry or log:
    // nothing under the state dir, custody excepted, holds it.
    let shown = fx.op(
        "crm_send_show",
        json!({"install_id": install, "context_id": context, "send_id": send_id}),
    );
    assert!(!shown.to_string().contains(&password));
    assert!(!sent.to_string().contains(&password));
    assert!(!connected.to_string().contains(&password));
    let mut hits = Vec::new();
    scan_for(fx.root.path(), password.as_bytes(), &mut hits);
    scan_for(fx.root.path(), wrong.as_bytes(), &mut hits);
    assert!(hits.is_empty(), "secret found on disk outside custody: {hits:?}");
}
