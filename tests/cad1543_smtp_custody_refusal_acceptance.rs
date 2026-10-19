//! CAD-1543 refusal acceptance (reviewer-written): host SMTP custody is
//! offered only to an install whose live bundle declares it.
//!
//! A real in-process hosted daemon and a loopback fake `smtp.internal`.
//! The operator binds, test-sends and prepares a campaign send for an
//! install whose screen package lists `smtp.bind`. The same operator
//! verbs, against the same connection and an otherwise identical install
//! that declares nothing (no screen package) or only a neighbouring SMTP
//! operation (`smtp.show`), are refused by the declaration gate before
//! anything is stored or sent, and the install receipt reports
//! `smtp_custody: false`. Agent and unproven callers stay refused on
//! both. Every address and the credential are synthetic.
#![cfg(feature = "test-seam")]

use cadence_agent::platform::smtp_internal::SmtpInternal;
use cadence_agent::test_seam::{scoped, Asserted};
use cadence_agent::{client, daemon, store::Store};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, Instant};

const RECIPIENT: &str = "operator@example.com";
const SENDER: &str = "owner@restaurant.example";
const REFUSAL: &str = "does not declare SMTP sending (smtp.bind)";

// ---------- a loopback smtp.internal that accepts everything ----------

struct Fake {
    url: String,
    sends: Arc<AtomicUsize>,
}

impl Fake {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let sends = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&sends);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let count = Arc::clone(&count);
                std::thread::spawn(move || serve(stream, &count));
            }
        });
        Self { url, sends }
    }
    fn sends(&self) -> usize {
        self.sends.load(SeqCst)
    }
}

fn serve(mut stream: TcpStream, sends: &AtomicUsize) {
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
    let body: Value =
        serde_json::from_slice(&buf[head_end..head_end + length]).unwrap_or(Value::Null);
    let path = head.lines().next().and_then(|l| l.split(' ').nth(1));
    let reply = if path == Some("/v1/send") {
        sends.fetch_add(1, SeqCst);
        json!({"ok": true, "data": {"accepted": body["envelope"]["to"], "rejected": [], "server_reply": "250 2.0.0 OK"}})
    } else {
        json!({"ok": true, "data": {"verified": true}})
    };
    let text = reply.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 200 X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
}

// ---------- one hosted daemon with one blog-post install ----------

/// What the install's bundle says about SMTP.
#[derive(Clone, Copy, Debug)]
enum Screen {
    /// A screen package for the app listing `smtp.bind`.
    Declares,
    /// A valid screen package that lists only `smtp.show`.
    NeighbourOnly,
    /// No screen package at all.
    None,
}

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

fn write_screen(source: &std::path::Path, operations: &[&str]) {
    let screen = source.join("screens/main");
    std::fs::create_dir_all(&screen).unwrap();
    let js = "(()=>{})();";
    std::fs::write(screen.join("client.js"), js).unwrap();
    let sha = |bytes: &[u8]| {
        use sha2::{Digest, Sha256};
        format!("sha256:{:x}", Sha256::digest(bytes))
    };
    let declaration = json!({
        "contract": "app-screens/v2", "app": "blog-post", "entry": "client.js",
        "host_contract": "screen-records.v1", "may": ["records.call", "records.slot"],
        "operations": operations, "tools": {},
        "assets": [{"name": "client.js", "media_type": "text/javascript", "size": js.len(), "sha256": sha(js.as_bytes())}],
        "provenance": {"source_digest": sha(b"c1543-source"), "sdk_digest": sha(b"c1543-sdk"), "toolchain_digest": sha(b"c1543-toolchain")}
    });
    std::fs::write(screen.join("screens.json"), declaration.to_string()).unwrap();
}

impl Fx {
    fn start(relay: &Fake, screen: Screen) -> Self {
        let root = tempfile::Builder::new().prefix("c1543").tempdir().unwrap();
        let pm = root.path().join("pm");
        cadence_agent::issue::Pm::init(&pm).unwrap();
        let source = root.path().join("source");
        for name in [
            "app.md",
            "workflows/blog-post.md",
            "rubrics/blog.md",
            "templates/brief.md",
            "templates/post.md",
        ] {
            let to = source.join(name);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/apps/blog-post")
                    .join(name),
                to,
            )
            .unwrap();
        }
        match screen {
            Screen::Declares => write_screen(&source, &["smtp.bind"]),
            Screen::NeighbourOnly => write_screen(&source, &["smtp.show"]),
            Screen::None => {}
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
        opts.smtp_internal = Some(SmtpInternal::new(&relay.url).unwrap());
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
        Store::open(&fx.state().join("cadence.sqlite3"))
            .unwrap()
            .register_agent(&cadence_agent::store::NewAgent {
                alias: "writer",
                provider: "claude",
                endpoint_kind: "managed",
                role: "worker",
                cwd: fx.root.path().to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        fx
    }
    fn state(&self) -> PathBuf {
        self.root.path().join("s")
    }
    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let state = self.state();
        scoped(who, || client::rpc(&state, method, params))
    }
    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }
    /// Install, context, one consenting customer, an approved campaign,
    /// a frozen audience and an enrolled SMTP connection.
    fn setup(&self) -> (String, String, String) {
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
                "subject": "Spring launch", "preheader": "News",
                "blocks": [{"type": "paragraph", "text": "A calm first line."}]}),
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
        let secret = format!("pw-{}", uuid::Uuid::new_v4().simple());
        let connection = self.op(
            "connection_create",
            json!({
                "provider": "smtp", "account": "owner", "shape": "smtp",
                "host": "smtp.gmail.com", "port": 465, "tls_mode": "implicit",
                "username": SENDER, "secret": secret, "sender": SENDER,
                "sender_name": "Restaurant", "scopes": ["email:send"],
                "accept_same_uid_risk": true,
            }),
        )["connection"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        (install, context, connection)
    }
    /// `smtp_custody` on both the show and the list receipt.
    fn custody(&self, install: &str) -> (Value, Value) {
        let shown = self.op("app_workspace_show", json!({"install_id": install}));
        let listed = self.op("app_workspace_list", json!({}));
        let row = listed
            .as_array()
            .or_else(|| listed["installations"].as_array())
            .unwrap_or_else(|| panic!("unexpected list shape: {listed}"))
            .iter()
            .find(|r| r["install_id"] == install)
            .unwrap_or_else(|| panic!("install missing from list: {listed}"))
            .clone();
        (shown["smtp_custody"].clone(), row["smtp_custody"].clone())
    }
}

fn gated(install: &str, context: &str, connection: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "crm_smtp_bind",
            json!({"install_id": install, "context_id": context, "connection_id": connection, "request_id": "bind-1"}),
        ),
        (
            "crm_smtp_test_send",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1", "to_email": RECIPIENT}),
        ),
        (
            "crm_send_prepare",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "audience_freeze_id": "freeze-1", "request_id": "send-1"}),
        ),
    ]
}

fn callers_refused(fx: &Fx, verbs: &[(&'static str, Value)]) {
    for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
        for (method, params) in verbs {
            assert!(
                fx.rpc(who.clone(), method, params.clone()).is_err(),
                "{method} must refuse {}",
                who.as_str()
            );
        }
    }
}

#[test]
fn smtp_custody_is_refused_to_an_install_that_does_not_declare_smtp_bind() {
    let relay = Fake::start();

    for screen in [Screen::None, Screen::NeighbourOnly] {
        let fx = Fx::start(&relay, screen);
        let (install, context, connection) = fx.setup();
        assert_eq!(
            fx.custody(&install),
            (json!(false), json!(false)),
            "{screen:?}: the receipt must report smtp_custody false"
        );
        let verbs = gated(&install, &context, &connection);
        callers_refused(&fx, &verbs);
        let sends = relay.sends();
        for (method, params) in &verbs {
            let refusal = fx
                .rpc(Asserted::Operator, method, params.clone())
                .expect_err(&format!("{screen:?}: operator {method} must be refused"));
            let text = format!("{refusal} {refusal:?}");
            assert!(
                text.contains(REFUSAL),
                "{screen:?}: operator {method} refused for the wrong reason: {text}"
            );
        }
        // Show is not gated, and it proves the refused bind stored nothing.
        let unbound = fx
            .rpc(
                Asserted::Operator,
                "crm_smtp_show",
                json!({"install_id": install, "context_id": context}),
            )
            .expect_err("a refused bind left a binding");
        assert!(
            unbound.to_string().contains("no SMTP sender is bound"),
            "{screen:?}: {unbound}"
        );
        assert!(
            !fx.op(
                "crm_send_list",
                json!({"install_id": install, "context_id": context})
            )
            .to_string()
            .contains("send-1"),
            "{screen:?}: a refused prepare left a send"
        );
        assert_eq!(relay.sends(), sends, "{screen:?}: a refused verb sent mail");
    }

    // The declaring install passes the same gate with the same verbs.
    let fx = Fx::start(&relay, Screen::Declares);
    let (install, context, connection) = fx.setup();
    assert_eq!(fx.custody(&install), (json!(true), json!(true)));
    let verbs = gated(&install, &context, &connection);
    callers_refused(&fx, &verbs);
    let sends = relay.sends();
    let bound = fx.op(verbs[0].0, verbs[0].1.clone());
    assert_eq!(bound["binding"]["sender"]["address"], SENDER, "{bound}");
    let sent = fx.op(verbs[1].0, verbs[1].1.clone());
    assert_eq!(sent["receipt"]["accepted"], true, "{sent}");
    assert_eq!(relay.sends(), sends + 1);
    let prepared = fx.op(verbs[2].0, verbs[2].1.clone());
    assert!(prepared["send_digest"].is_string(), "{prepared}");
}
