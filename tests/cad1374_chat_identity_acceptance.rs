//! Independent CAD-1374 bounded security acceptance for hosted chat identity.
//! Uses the real board_session_open/JWKS verifier, daemon thread_send guard,
//! public HTTP relay, and SQLite message/event persistence. The test-seam is
//! solely the board-to-daemon transport caller binding; it does not create or
//! stand in for hosted identity.
#![cfg(feature = "test-seam")]

use base64::Engine;
use cadence_agent::test_seam::{scoped, Asserted};
use cadence_agent::{client, daemon, master, store::Store};
use ring::signature::KeyPair;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::{Builder, TempDir};

const COMPANY: &str = "co_cad1374";
const KID: &str = "cad1374-test-key";
type Session = String;

struct Fx {
    root: TempDir,
    issuer: String,
    signer: ring::signature::Ed25519KeyPair,
    daemon_stop: Arc<AtomicBool>,
    daemon: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    board_stop: Arc<AtomicBool>,
    board: Option<std::thread::JoinHandle<()>>,
    port: u16,
    jwks: Option<(Arc<tiny_http::Server>, std::thread::JoinHandle<()>)>,
}

impl Fx {
    fn new() -> Self {
        let tmp = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let root = Builder::new().prefix("c1374acc-").tempdir_in(tmp).unwrap();
        let state = root.path().join("state");
        let pm = root.path().join("pm");
        let home = root.path().join("home");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        cadence_agent::issue::Pm::init(&pm).unwrap();
        let mut yaml = std::fs::read_to_string(pm.join("pm.yaml")).unwrap();
        yaml.push_str("\npi:\n  models:\n    allow: [\"fake/model-1\"]\n    default: {master: \"fake/model-1\", worker: \"fake/model-1\"}\n");
        std::fs::write(pm.join("pm.yaml"), yaml).unwrap();

        let signer = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[0x37; 32]).unwrap();
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let jwks = json!({"keys":[{"kty":"OKP","crv":"Ed25519","kid":KID,"x":b64.encode(signer.public_key().as_ref())}]}).to_string();
        let (jwks_port, stub) = (3110..3200)
            .find_map(|port| {
                tiny_http::Server::http(format!("127.0.0.1:{port}"))
                    .ok()
                    .map(|server| (port, Arc::new(server)))
            })
            .expect("free isolated JWKS port");
        let issuer = format!("http://127.0.0.1:{jwks_port}");
        let jwks_server = Arc::clone(&stub);
        let jwks_thread = std::thread::spawn(move || {
            for request in jwks_server.incoming_requests() {
                let _ = request.respond(tiny_http::Response::from_string(jwks.clone()));
            }
        });

        let free = |p: &u16| *p != jwks_port && TcpListener::bind(("127.0.0.1", *p)).is_ok();
        let port = (3110..3200)
            .find(free)
            .expect("free isolated board port disjoint from JWKS");
        let public_host = format!("acme.board.localhost:{port}");
        cadence_agent::board_identity::write_config(
            &state,
            &cadence_agent::board_identity::Config {
                host: public_host.clone(),
                issuer: issuer.clone(),
                company: COMPANY.into(),
            },
        )
        .unwrap();

        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", pm.to_string_lossy().to_string());
        env.set("HOME", home.to_string_lossy().to_string());
        env.set(
            "CADENCE_PI_COMMAND",
            format!(
                "python3 {}",
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/pi-gateway.py")
                    .display()
            ),
        );
        env.set(master::TEST_NO_LANDLOCK, "1");
        let daemon_stop = Arc::new(AtomicBool::new(false));
        let mut opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(daemon_stop.clone()),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        // register_at's URL is metadata/fallback only; this acceptance never
        // dispatches a local platform action or contacts that URL.
        cadence_agent::platform::local::register_at(
            &state,
            &mut opts,
            state.join("outbox"),
            format!("http://127.0.0.1:{port}"),
        );
        let daemon_dir = state.clone();
        let daemon = std::thread::spawn(move || daemon::serve_with(&daemon_dir, opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(1)).is_err()
            || cadence_agent::test_seam::Seam::token_at(&state).is_none()
        {
            if daemon.is_finished() {
                panic!("daemon exited during startup: {:?}", daemon.join().unwrap());
            }
            assert!(std::time::Instant::now() < deadline, "daemon did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        scoped(Asserted::Operator, || {
            client::rpc(
                &state,
                "master_start",
                json!({"provider":"pi","unconfined":true}),
            )
        })
        .unwrap();
        let public = cadence_agent::ui::PublicBoard {
            host: public_host.clone(),
            authorize_url: format!("{issuer}/v2/board/authorize"),
            issuer: issuer.clone(),
            company: COMPANY.into(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let board_stop = Arc::new(AtomicBool::new(false));
        let board_opts = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(board_stop.clone()),
            startup: Some(tx),
            test_seam: true,
            allow_hosts: vec![public_host.clone()],
            allow_origins: vec![format!("http://{public_host}")],
            public: Some(public),
            ..Default::default()
        };
        let (dir, board_pm) = (state, pm);
        let board = std::thread::spawn(move || {
            let _ = cadence_agent::ui::serve(&dir, &board_pm, &board_opts);
        });
        rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
        Self {
            root,
            issuer,
            signer,
            daemon_stop,
            daemon: Some(daemon),
            board_stop,
            board: Some(board),
            port,
            jwks: Some((stub, jwks_thread)),
        }
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("state")
    }
    fn public_host(&self) -> String {
        format!("acme.board.localhost:{}", self.port)
    }

    fn http(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: &str,
    ) -> (u16, String, Option<String>) {
        let host = self.public_host();
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: http://{host}\r\nSec-Fetch-Site: same-origin\r\nX-Cadence-Board: 1\r\nContent-Type: application/json\r\nConnection: close\r\n{}
\nContent-Length: {}\r\n\r\n{body}", cookie.map(|c| format!("Cookie: {c}\r\n")).unwrap_or_default(), body.len()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let set_cookie = response
            .lines()
            .find_map(|line| {
                line.strip_prefix("Set-Cookie: ")
                    .or_else(|| line.strip_prefix("set-cookie: "))
            })
            .map(|line| line.split(';').next().unwrap().to_string());
        (status, response, set_cookie)
    }

    fn assertion(&self, sub: &str, role: &str, jti: &str, exp_delta: i64) -> String {
        self.assertion_for_company(sub, role, jti, exp_delta, COMPANY)
    }

    fn assertion_for_company(
        &self,
        sub: &str,
        role: &str,
        jti: &str,
        exp_delta: i64,
        company: &str,
    ) -> String {
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let claims = json!({"iss":self.issuer,"aud":self.public_host(),"sub":sub,"email":format!("{sub}@example.test"),"name":format!("label-{sub}"),"company":company,"role":role,"iat":now-1,"exp":now+exp_delta,"jti":jti});
        let signed = format!(
            "{}.{}",
            b64.encode(json!({"alg":"EdDSA","typ":"JWT","kid":KID}).to_string()),
            b64.encode(claims.to_string())
        );
        format!(
            "{signed}.{}",
            b64.encode(self.signer.sign(signed.as_bytes()))
        )
    }

    fn login(&self, assertion: &str) -> (u16, String, Option<Session>) {
        let (status, response, cookie) = self.http(
            "POST",
            "/__platform/session",
            None,
            &json!({"assertion":assertion}).to_string(),
        );
        (status, response, cookie)
    }

    fn rpc(&self, params: Value) -> cadence_agent::Result<Value> {
        self.rpc_method("thread_send", params)
    }

    fn rpc_method(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(Asserted::Operator, || {
            client::rpc(&self.state(), method, params)
        })
    }

    fn persisted(&self, id: &str) -> (Value, Value) {
        let store = Store::open(&self.state().join("cadence.sqlite3")).unwrap();
        let queued = store
            .events_tail("master", 500)
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "queued" && e.payload["message"] == id)
            .expect("durable queued event");
        let thread = scoped(Asserted::Operator, || {
            client::rpc(
                &self.state(),
                "thread_read",
                json!({"alias":"master","limit":500}),
            )
        })
        .unwrap();
        let entry = thread["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["message"] == id)
            .cloned()
            .expect("durable thread message entry");
        (queued.payload, entry["payload"].clone())
    }

    fn has_message(&self, id: &str) -> bool {
        let store = Store::open(&self.state().join("cadence.sqlite3")).unwrap();
        let queued = store
            .events_tail("master", 500)
            .unwrap()
            .iter()
            .any(|e| e.kind == "queued" && e.payload["message"] == id);
        let thread = scoped(Asserted::Operator, || {
            client::rpc(
                &self.state(),
                "thread_read",
                json!({"alias":"master","limit":500}),
            )
        })
        .unwrap();
        queued
            || thread["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["message"] == id)
    }

    fn assert_no_credentials_in_state(&self, ids: &[&str], credentials: &[&str]) {
        let (events, thread) = {
            let store = Store::open(&self.state().join("cadence.sqlite3")).unwrap();
            let events = store
                .events_tail("master", 500)
                .unwrap()
                .into_iter()
                .filter(|e| ids.contains(&e.payload["message"].as_str().unwrap_or_default()))
                .map(|e| serde_json::to_string(&e.payload).unwrap())
                .collect::<Vec<_>>();
            let thread = scoped(Asserted::Operator, || {
                client::rpc(
                    &self.state(),
                    "thread_read",
                    json!({"alias":"master","limit":500}),
                )
            })
            .unwrap();
            let entries = thread["entries"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| ids.contains(&e["message"].as_str().unwrap_or_default()))
                .map(|e| serde_json::to_string(&e["payload"]).unwrap())
                .collect::<Vec<_>>();
            (events, entries)
        };
        for payload in events.iter().chain(thread.iter()) {
            for credential in credentials {
                assert!(
                    !payload.contains(credential),
                    "tested event payload contains a submitted credential"
                );
            }
        }
        fn walk(dir: &std::path::Path, files: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, files);
                } else {
                    files.push(path);
                }
            }
        }
        let mut files = Vec::new();
        walk(&self.state(), &mut files);
        for file in files {
            let bytes = std::fs::read(&file).unwrap_or_default();
            for credential in credentials {
                assert!(
                    !bytes
                        .windows(credential.len())
                        .any(|window| window == credential.as_bytes()),
                    "submitted credential appears in isolated state artifact {}",
                    file.display()
                );
            }
        }
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        if let Some(board) = self.board.take() {
            self.board_stop.store(true, SeqCst);
            let _ = board.join();
        }
        self.daemon_stop.store(true, SeqCst);
        if let Some(daemon) = self.daemon.take() {
            let _ = daemon.join();
        }
        if let Some((server, thread)) = self.jwks.take() {
            server.unblock();
            let _ = thread.join();
        }
    }
}

#[test]
fn hosted_chat_identity_is_verified_distinct_and_credential_free() {
    let fx = Fx::new();
    let assertion_a = fx.assertion("user-owner-a", "owner", "jti-owner-a", 45);
    let (a_status, a_reply, a) = fx.login(&assertion_a);
    assert_eq!(
        a_status, 200,
        "owner A must pass real board_session_open: {a_reply}"
    );
    let assertion_b = fx.assertion("user-owner-b", "owner", "jti-owner-b", 45);
    let (b_status, b_reply, b) = fx.login(&assertion_b);
    assert_eq!(
        b_status, 200,
        "owner B must pass real board_session_open: {b_reply}"
    );
    let a = a.unwrap();
    let b = b.unwrap();
    let token_canary = "secret-session-canary-cad1374";
    let proof_a = a
        .split_once('=')
        .map(|(_, value)| value.to_string())
        .unwrap_or_default();
    let proof_b = b
        .split_once('=')
        .map(|(_, value)| value.to_string())
        .unwrap_or_default();
    assert!(!proof_a.is_empty() && !proof_b.is_empty());

    let (missing, _, _) = fx.http(
        "POST",
        "/api/threads/master/messages",
        None,
        &json!({"text":"must refuse"}).to_string(),
    );
    assert_ne!(
        missing, 200,
        "public Host ingress must not fall back to anonymous/local operator chat"
    );

    let id_a = "cad1374-owner-a-message";
    let (status, reply, _) = fx.http(
        "POST",
        "/api/threads/master/messages",
        Some(&a),
        &json!({"text":"owner-A-private-body","message":id_a}).to_string(),
    );
    assert_eq!(status, 200, "owner A public chat: {reply}");
    let (queued_a, entry_a) = fx.persisted(id_a);
    assert_eq!(queued_a["human"]["sub"], "user-owner-a");
    assert_eq!(queued_a["human"]["company"], COMPANY);
    assert!(queued_a["human"]["session"].as_str().is_some());
    assert_eq!(entry_a["human"]["sub"], "user-owner-a");
    assert_eq!(entry_a["human"]["company"], COMPANY);
    assert_ne!(queued_a["human"]["sub"], "user-owner-b");

    let id_b = "cad1374-owner-b-message";
    let (status, reply, _) = fx.http(
        "POST",
        "/api/threads/master/messages",
        Some(&b),
        &json!({"text":"owner-B-private-body","message":id_b}).to_string(),
    );
    assert_eq!(status, 200, "owner B public chat: {reply}");
    let (queued_b, entry_b) = fx.persisted(id_b);
    assert_eq!(queued_b["human"]["sub"], "user-owner-b");
    assert_eq!(queued_b["human"]["company"], COMPANY);
    assert_ne!(queued_a["human"]["session"], queued_b["human"]["session"]);
    assert_eq!(entry_b["human"]["sub"], "user-owner-b");

    // Forged identity and credential fields are refused and cannot write.
    let (forged_status, forged_reply, _) = fx.http("POST", "/api/threads/master/messages", Some(&a), &json!({"text":"forged-claims","message":"cad1374-forged","actor":"user-owner-b","role":"owner","company":"other-company","token":token_canary}).to_string());
    assert_ne!(
        forged_status, 200,
        "untrusted identity/credential claims must be refused: {forged_reply}"
    );
    assert!(
        !fx.has_message("cad1374-forged"),
        "forged claims wrote a message"
    );

    // Reusing an id under a different verified human is not an idempotent retry.
    let conflict = fx.http(
        "POST",
        "/api/threads/master/messages",
        Some(&b),
        &json!({"text":"owner-A-private-body","message":id_a}).to_string(),
    );
    assert_ne!(
        conflict.0, 200,
        "same message id attributed to another owner must conflict: {}",
        conflict.1
    );
    let (after_retry, _) = fx.persisted(id_a);
    assert_eq!(after_retry["human"]["sub"], "user-owner-a");

    // Malformed and expired signed assertions are rejected by the real session opener.
    // This does not test expiration of an already-open session; that guard remains Not Run.
    let expired_assertion = fx.assertion("expired-owner", "owner", "jti-expired", -120);
    for bad in [String::from("not-a-jws"), expired_assertion.clone()] {
        let (status, response, _) = fx.login(&bad);
        assert_ne!(
            status, 200,
            "invalid/expired platform proof opened a session: {response}"
        );
    }
    let member_assertion = fx.assertion("user-member", "member", "jti-member", 45);
    let (member_status, member_reply, member) = fx.login(&member_assertion);
    assert_eq!(
        member_status, 200,
        "member must be genuinely authenticated before refusal: {member_reply}"
    );
    let member = member.unwrap();
    let member_bearer = member
        .split_once('=')
        .map(|(_, value)| value.to_string())
        .unwrap();
    let member_rpc = fx.rpc(json!({"alias":"master","text":"member-direct-refusal","message":"cad1374-member-direct","public_session":member_bearer}));
    assert!(
        member_rpc.is_err(),
        "real daemon thread_send must refuse a verified member session"
    );
    assert!(
        !fx.has_message("cad1374-member-direct"),
        "member daemon refusal wrote a message"
    );
    let (member_status, member_reply, _) = fx.http("POST", "/api/threads/master/messages", Some(&member), &json!({"text":"member-forgery","message":"cad1374-member-http","actor":"user-owner-a","role":"owner","company":COMPANY}).to_string());
    assert_ne!(
        member_status, 200,
        "forged owner claims must not elevate verified member: {member_reply}"
    );
    assert!(
        !fx.has_message("cad1374-member-http"),
        "member HTTP refusal wrote a message"
    );

    // Direct daemon calls carry only the signed-in session bearer; the daemon
    // itself must reject malformed proof values and derive identity from its row.
    for (n, malformed) in [Value::Null, json!(""), json!(17)].into_iter().enumerate() {
        let result = fx.rpc(json!({"alias":"master","text":"bad-proof","message":format!("cad1374-malformed-{n}"),"public_session":malformed}));
        assert!(result.is_err(), "malformed hosted proof accepted");
        assert!(
            !fx.has_message(&format!("cad1374-malformed-{n}")),
            "malformed proof wrote a message"
        );
    }
    let invalid_bearer = fx.rpc(json!({"alias":"master","text":"invalid-bearer","message":"cad1374-invalid-bearer","public_session":"not-a-live-session"}));
    assert!(
        invalid_bearer.is_err(),
        "invalid string bearer accepted by daemon"
    );
    assert!(
        !fx.has_message("cad1374-invalid-bearer"),
        "invalid bearer wrote a message"
    );
    let forged_rpc = fx.rpc(json!({"alias":"master","text":"forged-rpc","message":"cad1374-forged-rpc","public_session":proof_a,"actor":"user-owner-b","role":"owner","company":"elsewhere"}));
    assert!(
        forged_rpc.is_err(),
        "daemon RPC must refuse caller-supplied identity claims"
    );
    assert!(
        !fx.has_message("cad1374-forged-rpc"),
        "forged daemon identity claims wrote a message"
    );
    let direct = fx.rpc(json!({"alias":"master","text":"direct-owner-a","message":"cad1374-direct-a","public_session":proof_a})).unwrap();
    let direct_id = direct["message"].as_str().unwrap();
    let (direct_queued, direct_entry) = fx.persisted(direct_id);
    assert_eq!(direct_queued["human"]["sub"], "user-owner-a");
    assert_eq!(direct_entry["human"]["sub"], "user-owner-a");

    // Compatibility for local operator chat remains unattributed; no client
    // payload can inject a human claim when there is no public session proof.
    fx.rpc(json!({"alias":"master","text":"local-operator","message":"cad1374-local"}))
        .unwrap();
    let (local_event, local_entry) = fx.persisted("cad1374-local");
    assert!(local_event.get("human").is_none());
    assert!(local_entry.get("human").is_none());

    // The app-write checkpoint uses the same signed browser sessions, real
    // HTTP relays and daemon guards; direct store calls are used only to
    // inspect persisted receipts, never to establish hosted identity.
    let source = fx.root.path().join("app-source");
    for name in [
        "app.md",
        "workflows/blog-post.md",
        "rubrics/blog.md",
        "templates/brief.md",
        "templates/post.md",
    ] {
        let target = source.join(name);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/apps/blog-post")
                .join(name),
            target,
        )
        .unwrap();
    }
    let install = fx
        .rpc_method("app_workspace_install", json!({"source":source}))
        .unwrap()["install_id"]
        .as_str()
        .unwrap()
        .to_string();
    let context = fx
        .rpc_method(
            "app_context_create",
            json!({"install_id":install,"label":"acceptance","input_defaults":{},"request_id":"cad1374-app-context"}),
        )
        .unwrap()["context"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let profile_a = json!({"schema":1,"display_name":"Record A","email":"record-a@example.test","tags":[],"consent":{"email":"unknown"}});
    let profile_b = json!({"schema":1,"display_name":"Record B","email":"record-b@example.test","tags":[],"consent":{"email":"unknown"}});
    let record_path = format!("/api/app-installations/{install}/contexts/{context}/records");
    let content_path =
        format!("/api/app-installations/{install}/contexts/{context}/content/campaigns");
    let (status, reply, _) = fx.http(
        "POST",
        &record_path,
        Some(&a),
        &json!({"record_id":"customer-a","profile":profile_a}).to_string(),
    );
    assert_eq!(status, 200, "owner A record create: {reply}");
    let (status, reply, _) = fx.http(
        "POST",
        &record_path,
        Some(&b),
        &json!({"record_id":"customer-b","profile":profile_b}).to_string(),
    );
    assert_eq!(status, 200, "owner B record create: {reply}");
    let (status, replay_reply, _) = fx.http(
        "POST",
        &record_path,
        Some(&b),
        &json!({"record_id":"customer-a","profile":profile_a}).to_string(),
    );
    assert_eq!(status, 200, "identical create replay: {replay_reply}");
    let (status, reply, _) = fx.http(
        "POST",
        &format!("{record_path}/customer-a/update"),
        Some(&b),
        &json!({"expected_revision":1,"profile":{"schema":1,"display_name":"Record A updated","email":"record-a@example.test","tags":[],"consent":{"email":"unknown"}}}).to_string(),
    );
    assert_eq!(status, 200, "owner B record update: {reply}");
    let blocks = json!([{"type":"paragraph","text":"Owner-authored content"}]);
    for (owner, cookie, campaign, subject) in [
        ("user-owner-a", &a, "campaign-a", "Owner A content"),
        ("user-owner-b", &b, "campaign-b", "Owner B content"),
    ] {
        let (status, reply, _) = fx.http(
            "POST",
            &content_path,
            Some(cookie),
            &json!({"campaign_id":campaign,"subject":subject,"blocks":blocks}).to_string(),
        );
        assert_eq!(status, 200, "{owner} content save: {reply}");
    }
    fx.rpc_method(
        "app_content_save",
        json!({"install_id":install,"context_id":context,"campaign_id":"campaign-local","subject":"Local content","blocks":blocks}),
    )
    .unwrap();

    let csv = "record_id,display_name,email\ncsv-customer,CSV Customer,csv-customer@example.test\n";
    let (status, preview_reply, _) = fx.http(
        "POST",
        &format!("{record_path}/csv-preview"),
        Some(&a),
        &json!({"csv_text":csv}).to_string(),
    );
    assert_eq!(status, 200, "CSV preview: {preview_reply}");
    let preview_body = preview_reply.split("\r\n\r\n").nth(1).unwrap();
    let preview: Value = serde_json::from_str(preview_body).unwrap();
    let preview_token = preview["preview_token"].as_str().unwrap();
    let import =
        json!({"csv_text":csv,"preview_token":preview_token,"request_id":"cad1374-csv-import"})
            .to_string();
    let (status, reply, _) = fx.http(
        "POST",
        &format!("{record_path}/csv-import"),
        Some(&a),
        &import,
    );
    assert_eq!(status, 200, "owner A CSV import: {reply}");
    let (status, replay_reply, _) = fx.http(
        "POST",
        &format!("{record_path}/csv-import"),
        Some(&b),
        &import,
    );
    assert_eq!(status, 200, "identical CSV replay: {replay_reply}");
    let record_db = fx
        .state()
        .join("app-records")
        .join(format!("{install}.sqlite3"));
    let conn = rusqlite::Connection::open(&record_db).unwrap();
    let human = |table: &str, where_sql: &str| -> Value {
        let sql = format!("SELECT human FROM {table} WHERE {where_sql}");
        let raw: Option<String> = conn.query_row(&sql, [], |row| row.get(0)).unwrap();
        serde_json::from_str(&raw.unwrap()).unwrap()
    };
    let local_content: Option<String> = conn
        .query_row(
            "SELECT human FROM app_content_revisions WHERE campaign_id='campaign-local' AND revision=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        local_content.is_none(),
        "legacy local content must stay unattributed"
    );
    let record_a = human(
        "app_record_revisions",
        "record_id='customer-a' AND revision=1",
    );
    let record_b = human(
        "app_record_revisions",
        "record_id='customer-b' AND revision=1",
    );
    let update_b = human(
        "app_record_revisions",
        "record_id='customer-a' AND revision=2",
    );
    assert_eq!(record_a["sub"], "user-owner-a");
    assert_eq!(record_b["sub"], "user-owner-b");
    assert_eq!(update_b["sub"], "user-owner-b");
    assert_eq!(record_a["company"], COMPANY);
    assert_ne!(record_a["session"], record_b["session"]);
    for campaign in ["campaign-a", "campaign-b"] {
        let content = human(
            "app_content_revisions",
            &format!("campaign_id='{campaign}' AND revision=1"),
        );
        assert_eq!(content["company"], COMPANY);
        assert_eq!(
            content["sub"],
            if campaign == "campaign-a" {
                "user-owner-a"
            } else {
                "user-owner-b"
            }
        );
    }
    let import_human = human("app_record_csv_imports", "request_id='cad1374-csv-import'");
    assert_eq!(import_human["sub"], "user-owner-a");
    assert_eq!(
        human(
            "app_record_revisions",
            "record_id='csv-customer' AND revision=1"
        )["sub"],
        "user-owner-a"
    );
    assert_eq!(
        human(
            "app_record_revisions",
            "record_id='customer-a' AND revision=1"
        )["sub"],
        "user-owner-a",
        "identical create replay must preserve the first author's revision"
    );
    drop(conn);

    // Forged actor/company fields and verified member sessions cannot write
    // app data; rejected inputs leave no revision or CSV receipt behind.
    let (status, _, _) = fx.http(
        "POST",
        &record_path,
        Some(&a),
        &json!({"record_id":"forged-app","profile":profile_a,"actor":"user-owner-b","company":"elsewhere"}).to_string(),
    );
    assert_ne!(status, 200, "HTTP must reject forged app identity fields");
    let (status, _, _) = fx.http(
        "POST",
        &record_path,
        None,
        &json!({"record_id":"missing-session-app","profile":profile_a}).to_string(),
    );
    assert_ne!(status, 200, "public app create must require a session");
    let (status, _, _) = fx.http(
        "POST",
        &record_path,
        Some(&member),
        &json!({"record_id":"member-app","profile":profile_a}).to_string(),
    );
    assert_ne!(
        status, 200,
        "verified member must not create owner-only app records"
    );
    let (status, _, _) = fx.http(
        "POST",
        &content_path,
        Some(&member),
        &json!({"campaign_id":"member-campaign","subject":"Member content","blocks":blocks})
            .to_string(),
    );
    assert_ne!(
        status, 200,
        "verified member must not save owner-only app content"
    );
    assert!(fx
        .rpc_method(
            "app_record_show",
            json!({"install_id":install,"context_id":context,"record_id":"forged-app"})
        )
        .is_err());
    assert!(fx
        .rpc_method(
            "app_record_show",
            json!({"install_id":install,"context_id":context,"record_id":"member-app"})
        )
        .is_err());
    assert!(fx
        .rpc_method(
            "app_content_show",
            json!({"install_id":install,"context_id":context,"campaign_id":"member-campaign"})
        )
        .is_err());

    // CAD-1374 activity query/export acceptance uses the same verified
    // platform sessions and the real daemon + HTTP relay. The records above
    // provide durable chat, app-revision, and CSV-import activity to query.
    let activity_path = "/api/activity?sub=user-owner-a&limit=200";
    let (status, activity_reply, _) = fx.http("GET", activity_path, Some(&a), "");
    assert_eq!(
        status, 200,
        "same-company owner activity query: {activity_reply}"
    );
    let activity_body = activity_reply.split("\r\n\r\n").nth(1).unwrap();
    let activity: Value = serde_json::from_str(activity_body).unwrap();
    assert_eq!(activity["schema_version"], 1);
    assert_eq!(activity["company"], COMPANY);
    assert_eq!(activity["complete"], true);
    assert!(activity["events"].as_array().is_some());
    let event_ids = activity["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| event["event_id"].as_str())
        .collect::<std::collections::HashSet<_>>();
    assert!(
        event_ids.len() > 1,
        "expected persisted owner chat and app activity"
    );
    let rows = activity["events"].as_array().unwrap();
    assert!(
        rows.iter()
            .any(|event| { event["target"].as_str().unwrap_or_default().contains(id_a) }),
        "query omitted the persisted signed-owner chat event"
    );
    assert!(
        rows.iter().any(|event| {
            let target = event["target"].as_str().unwrap_or_default();
            target.contains("campaign-a")
                || target.contains("customer-a")
                || target.contains("cad1374-csv-import")
        }),
        "query omitted persisted signed-owner app activity"
    );
    let rendered = serde_json::to_string(&activity).unwrap();
    for forbidden in [
        "owner-A-private-body",
        "Owner A content",
        "CSV Customer",
        token_canary,
        proof_a.as_str(),
    ] {
        assert!(
            !rendered.contains(forbidden),
            "activity projection leaked {forbidden}"
        );
    }
    assert!(activity["coverage"]["included_sources"].is_array());
    assert!(activity["coverage"]["not_yet_covered"].is_array());

    // Stable limit-one continuation must enumerate the same filtered result
    // exactly once. Changing the filter while retaining its cursor is refused.
    let mut cursor = String::new();
    let mut paged = Vec::new();
    loop {
        let path = if cursor.is_empty() {
            "/api/activity?sub=user-owner-a&limit=1".to_string()
        } else {
            format!("/api/activity?sub=user-owner-a&limit=1&cursor={cursor}")
        };
        let (page_status, page_reply, _) = fx.http("GET", &path, Some(&a), "");
        assert_eq!(page_status, 200, "activity page: {page_reply}");
        let page: Value =
            serde_json::from_str(page_reply.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let rows = page["events"].as_array().unwrap();
        assert_eq!(
            rows.len(),
            1,
            "limit=1 must return one row per nonempty page"
        );
        paged.push(rows[0]["event_id"].as_str().unwrap().to_string());
        if page["complete"] == true {
            break;
        }
        cursor = page["next_cursor"]
            .as_str()
            .expect("continuation cursor")
            .to_string();
    }
    let page_ids = paged
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        page_ids.len(),
        paged.len(),
        "continuation duplicated an event"
    );
    let expected_ids = event_ids
        .iter()
        .map(|id| id.to_string())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        page_ids, expected_ids,
        "continuation omitted an eligible event"
    );
    let changed_scope = format!("/api/activity?sub=user-owner-b&limit=1&cursor={cursor}");
    let (changed_status, changed_reply, _) = fx.http("GET", &changed_scope, Some(&a), "");
    assert_ne!(
        changed_status, 200,
        "cursor must be bound to its frozen filter scope: {changed_reply}"
    );

    // Missing, invalid, member, forged, and a genuine second-company signed
    // assertion cannot read activity through either public HTTP or daemon RPC.
    for (cookie, label) in [
        (None, "missing"),
        (Some("cadence_session=not-a-session"), "invalid"),
    ] {
        let (denied, reply, _) = fx.http("GET", "/api/activity?limit=1", cookie, "");
        assert_ne!(denied, 200, "{label} public proof read activity: {reply}");
        assert!(
            !reply.contains(id_a),
            "{label} refusal disclosed activity payload/count"
        );
    }
    // A genuine managed-agent transport caller cannot become an operator
    // merely by presenting a valid hosted owner cookie. `master` is the
    // registered fixture agent started above; the seam asserts transport
    // provenance only and does not establish the hosted human identity.
    for operation in ["activity_query", "activity_export"] {
        let result = scoped(Asserted::Agent("master".into()), || {
            client::rpc(
                &fx.state(),
                operation,
                json!({"limit":1,"public_session":proof_a}),
            )
        });
        assert!(
            result.is_err(),
            "registered managed agent with valid owner proof was allowed to {operation}"
        );
    }
    let member_query = fx.rpc_method(
        "activity_query",
        json!({"limit":1,"public_session":member_bearer}),
    );
    assert!(
        member_query.is_err(),
        "member direct daemon activity query was allowed"
    );
    let (member_http, member_reply, _) = fx.http("GET", "/api/activity?limit=1", Some(&member), "");
    assert_ne!(
        member_http, 200,
        "member HTTP activity query was allowed: {member_reply}"
    );
    assert!(
        !member_reply.contains(id_a),
        "member refusal disclosed activity payload/count"
    );
    let forged = fx.rpc_method(
        "activity_query",
        json!({"limit":1,"public_session":proof_a,"actor":"user-owner-b","role":"owner","company":"elsewhere"}),
    );
    assert!(
        forged.is_err(),
        "daemon accepted caller-supplied identity claims for audit query"
    );
    let cross_company = fx.assertion_for_company(
        "other-company-owner",
        "owner",
        "jti-other-company",
        45,
        "co_cad1374_other",
    );
    let (cross_status, cross_reply, cross_cookie) = fx.login(&cross_company);
    assert_ne!(
        cross_status, 200,
        "second-company signed proof bound to this board: {cross_reply}"
    );
    assert!(cross_cookie.is_none());

    // Export is owner-authorized, complete as an explicit paginated document,
    // private-body/credential-free, and durably records its prepared intent.
    let (export_status, export_reply, _) = fx.http(
        "GET",
        "/api/activity/export?sub=user-owner-a&limit=1",
        Some(&a),
        "",
    );
    assert_eq!(export_status, 200, "owner activity export: {export_reply}");
    let export_bytes = export_reply.split("\r\n\r\n").nth(1).unwrap();
    let export: Value = serde_json::from_str(export_bytes).unwrap();
    assert_eq!(export["schema_version"], 1);
    assert_eq!(export["company"], COMPANY);
    assert!(export["complete"].is_boolean());
    assert!(export["row_count"].as_u64().is_some());
    assert!(export["coverage"]["included_sources"].is_array());
    assert!(export["coverage"]["not_yet_covered"].is_array());
    for forbidden in [
        "owner-A-private-body",
        "Owner A content",
        "CSV Customer",
        token_canary,
        proof_a.as_str(),
        member_bearer.as_str(),
    ] {
        assert!(
            !export_bytes.contains(forbidden),
            "export leaked sensitive content/credential"
        );
    }
    let event_store = Store::open(&fx.state().join("cadence.sqlite3")).unwrap();
    let activity_events = event_store.events_tail(Store::DAEMON_STREAM, 500).unwrap();
    let prepared = activity_events
        .iter()
        .find(|event| event.kind == "activity_export_prepared")
        .expect("successful export had no durable prepared-intent event");
    assert_eq!(prepared.payload["operation"], "activity_export");
    assert_eq!(prepared.payload["outcome"], "prepared");
    assert_eq!(prepared.payload["company"], COMPANY);
    assert_eq!(prepared.payload["human"]["sub"], "user-owner-a");
    assert_eq!(prepared.payload["human"]["company"], COMPANY);

    // Invalid continuation input must be refused before any new export
    // intent is durably prepared (the route and daemon are both exercised).
    let prepared_count = |store: &Store| {
        store
            .events_tail(Store::DAEMON_STREAM, 500)
            .unwrap()
            .iter()
            .filter(|event| event.kind == "activity_export_prepared")
            .count()
    };
    let before_bad_cursor = prepared_count(&event_store);
    let (bad_cursor_status, bad_cursor_reply, _) = fx.http(
        "GET",
        "/api/activity/export?sub=user-owner-a&limit=1&cursor=not-a-valid-cursor",
        Some(&a),
        "",
    );
    assert_ne!(
        bad_cursor_status, 200,
        "malformed export cursor unexpectedly succeeded: {bad_cursor_reply}"
    );
    assert_eq!(
        prepared_count(&event_store),
        before_bad_cursor,
        "malformed export cursor wrote a prepared-intent event"
    );

    // Fail only the durable prepared-intent insertion. A failed critical
    // receipt must refuse the export without returning its projected bytes.
    let db = rusqlite::Connection::open(fx.state().join("cadence.sqlite3")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject_activity_export_prepared
         BEFORE INSERT ON events
         WHEN NEW.kind = 'activity_export_prepared'
         BEGIN SELECT RAISE(ABORT, 'acceptance: reject prepared export intent'); END;",
    )
    .unwrap();
    let before_storage_failure = prepared_count(&event_store);
    let (storage_status, storage_reply, _) = fx.http(
        "GET",
        "/api/activity/export?sub=user-owner-a&limit=1",
        Some(&a),
        "",
    );
    assert_ne!(
        storage_status, 200,
        "export succeeded despite durable prepared-intent storage failure: {storage_reply}"
    );
    assert!(!storage_reply.contains("\"events\""));
    assert!(!storage_reply.contains("\"row_count\""));
    assert_eq!(
        prepared_count(&event_store),
        before_storage_failure,
        "failed prepared-intent storage nevertheless wrote an event"
    );
    db.execute_batch("DROP TRIGGER reject_activity_export_prepared;")
        .unwrap();
    drop(db);
    let denied_export = fx.rpc_method(
        "activity_export",
        json!({"limit":1,"public_session":member_bearer}),
    );
    assert!(
        denied_export.is_err(),
        "member direct daemon export was allowed"
    );
    let (denied_export_status, denied_export_reply, _) =
        fx.http("GET", "/api/activity/export?limit=1", Some(&member), "");
    assert_ne!(
        denied_export_status, 200,
        "member HTTP export was allowed: {denied_export_reply}"
    );
    assert!(
        !denied_export_reply.contains(id_a),
        "denied export disclosed activity bytes"
    );
    let refusal_events = Store::open(&fx.state().join("cadence.sqlite3"))
        .unwrap()
        .events_tail(Store::DAEMON_STREAM, 500)
        .unwrap()
        .into_iter()
        .map(|event| serde_json::to_string(&event.payload).unwrap())
        .collect::<Vec<_>>();
    for operation in ["activity_query", "activity_export"] {
        assert!(
            refusal_events.iter().any(|payload| {
                payload.contains("user-member")
                    && payload.contains(operation)
                    && (payload.to_ascii_lowercase().contains("denied")
                        || payload.to_ascii_lowercase().contains("refused"))
            }),
            "{operation} refusal did not durably preserve the verified member identity and reason"
        );
    }

    // Local operator activity access remains compatible with legacy rows.
    let local_activity = fx.rpc_method("activity_query", json!({"limit":200}));
    assert!(
        local_activity.is_ok(),
        "local operator activity query regressed: {local_activity:?}"
    );

    // Run the destructive pre-CAD-1374 schema migration only after activity
    // assertions have inspected the still-attributed signed-owner receipts.
    // Old/local rows remain NULL rather than receiving guessed attribution.
    fx.rpc_method(
        "app_record_create",
        json!({"install_id":install,"context_id":context,"record_id":"legacy-local","profile":{"schema":1,"display_name":"Legacy","email":"legacy@example.test","tags":[],"consent":{"email":"unknown"}}}),
    )
    .unwrap();
    let conn = rusqlite::Connection::open(&record_db).unwrap();
    let legacy_before: Option<String> = conn
        .query_row(
            "SELECT human FROM app_record_revisions WHERE record_id='legacy-local' AND revision=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        legacy_before.is_none(),
        "local legacy receipt should be NULL"
    );
    conn.execute_batch(
        "ALTER TABLE app_content_revisions DROP COLUMN human;
         ALTER TABLE app_record_revisions DROP COLUMN human;
         ALTER TABLE app_record_csv_imports DROP COLUMN human;",
    )
    .unwrap();
    drop(conn);
    let migrated =
        cadence_agent::store::app_records::RecordStore::open(&fx.state(), &install).unwrap();
    drop(migrated);
    let conn = rusqlite::Connection::open(&record_db).unwrap();
    for (table, key) in [
        ("app_content_revisions", "campaign_id='campaign-a'"),
        ("app_record_revisions", "record_id='legacy-local'"),
        ("app_record_csv_imports", "request_id='cad1374-csv-import'"),
    ] {
        let sql = format!("SELECT human FROM {table} WHERE {key}");
        let old: Option<String> = conn.query_row(&sql, [], |row| row.get(0)).unwrap();
        assert!(
            old.is_none(),
            "migration guessed historical human for {table}"
        );
    }

    // Scan persisted state only after activity, refusal, export and migration
    // writes so credentials introduced by any of those paths are covered.
    let assertion_credentials = [
        &assertion_a,
        &assertion_b,
        &member_assertion,
        &expired_assertion,
    ];
    let credential_refs = [
        token_canary,
        proof_a.as_str(),
        proof_b.as_str(),
        member_bearer.as_str(),
        assertion_credentials[0].as_str(),
        assertion_credentials[1].as_str(),
        assertion_credentials[2].as_str(),
        assertion_credentials[3].as_str(),
    ];
    fx.assert_no_credentials_in_state(
        &[
            id_a,
            id_b,
            "cad1374-forged",
            "cad1374-forged-rpc",
            "cad1374-direct-a",
            "cad1374-local",
            "cad1374-member-direct",
            "cad1374-member-http",
            "cad1374-malformed-0",
            "cad1374-malformed-1",
            "cad1374-malformed-2",
            "cad1374-invalid-bearer",
        ],
        &credential_refs,
    );
}
