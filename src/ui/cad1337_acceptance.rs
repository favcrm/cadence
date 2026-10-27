//! CAD-1337 HTTP-peer companion to `daemon::cad1337_acceptance`.
//!
//! This drives the actual board listener, authentication/session parsing,
//! multipart handler, relay and native daemon. The test-seam operator claim
//! is only the explicitly allowed isolated premise for unchanged caller
//! attribution; it says nothing about production kernel/operator or browser
//! execution. User sessions are signed with an ephemeral test issuer and go
//! through the real production `board_session_open` verification path.

#![cfg(feature = "test-seam")]

use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted, Seam};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const COMPANY: &str = "cad1337-http-fixture";
const PUBLIC_HOST: &str = "cad1337-http.board.localhost";
const KEY_ID: &str = "cad1337-http-ephemeral";
const KEY_SEED: [u8; 32] = [0x38; 32];
const FILE_BYTES: &[u8] = b"actual HTTP upload bytes; operational workspace only\n";

struct Stop(Arc<AtomicBool>, Vec<std::thread::JoinHandle<()>>);

impl Drop for Stop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
        for thread in self.1.drain(..).rev() {
            let _ = thread.join();
        }
    }
}

struct Fx {
    _stop: Stop,
    root: tempfile::TempDir,
    issuer: String,
    port: u16,
    seam_token: String,
}

impl Fx {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("cad1337-http-")
            .tempdir()
            .unwrap();
        let state = root.path().join("state");
        let pm = root.path().join("tracker");
        crate::issue::Pm::init(&pm).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let server = (3110..=3199)
            .find_map(|port| tiny_http::Server::http(("127.0.0.1", port)).ok())
            .expect("no free isolated issuer port in 3110..3199");
        let issuer = format!("http://{}", server.server_addr());
        let key = Ed25519KeyPair::from_seed_unchecked(&KEY_SEED).unwrap();
        let x = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
        // Keep ownership of every fixture thread during construction so
        // panics clean them up before TempDir removes their state/socket.
        let stop = Arc::new(AtomicBool::new(false));
        let mut stop_guard = Stop(stop.clone(), Vec::new());
        let issuer_stop = stop.clone();
        stop_guard.1.push(std::thread::spawn(move || {
            while !issuer_stop.load(Ordering::SeqCst) {
                let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(100)) else {
                    continue;
                };
                let body = json!({"keys":[{"kty":"OKP","crv":"Ed25519","kid":KEY_ID,"x":x}]});
                let response = tiny_http::Response::from_string(body.to_string()).with_header(
                    tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                );
                let _ = request.respond(response);
            }
        }));
        // The real `master` alias is reserved from the RPC registration
        // surface. Seed only its existing fixture metadata through Store;
        // all target conversations and upload/session guards below still
        // use the production dispatcher and HTTP path.
        let fixture_store = crate::store::Store::open(&crate::rollout::db_file(&state)).unwrap();
        let cwd = root.path().to_str().unwrap();
        fixture_store
            .register_agent(&NewAgent {
                alias: "master",
                provider: "pi",
                endpoint_kind: "managed",
                role: "worker",
                cwd,
                sandbox: "read-only",
                instructions: None,
                params: Some(r#"{"model":"fixture/model"}"#),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        crate::test_seam::seed_pi_fixture_policy(&fixture_store, "fixture/model").unwrap();
        drop(fixture_store);
        crate::board_identity::write_config(
            &state,
            &crate::board_identity::Config {
                host: PUBLIC_HOST.into(),
                issuer: issuer.clone(),
                company: COMPANY.into(),
            },
        )
        .unwrap();

        let daemon_opts = crate::daemon::ServeOptions {
            test_seam: true,
            stop: Some(stop.clone()),
            ..Default::default()
        };
        daemon_opts
            .provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        daemon_opts
            .provider_env
            .set("CADENCE_HOME", root.path().to_str().unwrap());
        let daemon_state = state.clone();
        stop_guard.1.push(std::thread::spawn(move || {
            crate::daemon::serve_with(&daemon_state, daemon_opts).unwrap()
        }));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "isolated CAD-1337 daemon did not start"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        let seam_token = Seam::token_at(&state).unwrap();
        let mut port = 3110 + (std::process::id() % 80) as u16;
        loop {
            let (startup, ready) = std::sync::mpsc::channel();
            let board_opts = crate::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(stop.clone()),
                startup: Some(startup),
                test_seam: true,
                public: Some(crate::ui::PublicBoard {
                    host: PUBLIC_HOST.into(),
                    issuer: issuer.clone(),
                    company: COMPANY.into(),
                    authorize_url: format!("{issuer}/authorize"),
                }),
                allow_hosts: vec![PUBLIC_HOST.into()],
                allow_origins: vec![format!("http://{PUBLIC_HOST}")],
                ..Default::default()
            };
            let board_state = state.clone();
            let board_pm = pm.clone();
            let thread = std::thread::spawn(move || {
                drop(crate::ui::serve(&board_state, &board_pm, &board_opts))
            });
            match ready.recv_timeout(Duration::from_secs(30)).unwrap() {
                Ok(()) => {
                    stop_guard.1.push(thread);
                    break;
                }
                Err(std::io::ErrorKind::AddrInUse) if port < 3199 => {
                    thread.join().unwrap();
                    port += 1;
                }
                other => {
                    stop_guard.1.push(thread);
                    panic!("isolated board startup failed: {other:?}");
                }
            }
        }
        let fx = Self {
            _stop: stop_guard,
            root,
            issuer,
            port,
            seam_token,
        };
        // Build two genuine target Home threads through the production
        // operator dispatcher. The worker target is deliberately not
        // Master Home and must never be accepted as its upload target.
        fx.rpc(
            "agent_register",
            json!({"alias":"worker","provider":"fake","endpoint_kind":"fake","cwd":fx.root.path().to_str().unwrap()}),
        )
        .unwrap();
        fx.rpc("thread_send", json!({"alias":"master","text":"create Master Home","message":"cad1337-http-master-home"})).unwrap();
        fx.rpc("thread_send", json!({"alias":"worker","text":"create worker Home","message":"cad1337-http-other-home"})).unwrap();
        fx
    }

    fn state(&self) -> std::path::PathBuf {
        self.root.path().join("state")
    }
    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn host_header(&self) -> String {
        PUBLIC_HOST.into()
    }
    fn rpc(&self, method: &str, params: Value) -> crate::Result<Value> {
        scoped(Asserted::Operator, || {
            crate::client::rpc(&self.state(), method, params)
        })
    }
    fn session(&self, subject: &str, role: &str) -> (String, String) {
        let key = Ed25519KeyPair::from_seed_unchecked(&KEY_SEED).unwrap();
        let now = crate::issue::time::now_epoch();
        let part = |v: Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(&v).unwrap());
        let signed = format!(
            "{}.{}",
            part(json!({"alg":"EdDSA","typ":"JWT","kid":KEY_ID})),
            part(json!({
                "iss":self.issuer,"aud":PUBLIC_HOST,"sub":subject,
                "email":format!("{subject}@example.test"),"name":format!("Fixture {subject}"),
                "company":COMPANY,"role":role,"iat":now,"exp":now+30,
                "jti":format!("cad1337-http-{subject}-{}",uuid::Uuid::new_v4()),
            }))
        );
        let assertion = format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(key.sign(signed.as_bytes()).as_ref())
        );
        let opened = self
            .rpc("board_session_open", json!({"assertion":assertion}))
            .unwrap();
        (
            opened["token"].as_str().unwrap().to_owned(),
            opened["session"]["account_id"]
                .as_str()
                .expect("production auth issues account binding")
                .to_owned(),
        )
    }
    fn row_count(&self) -> i64 {
        rusqlite::Connection::open(crate::rollout::db_file(&self.state()))
            .unwrap()
            .query_row("SELECT count(*) FROM chat_files", [], |r| r.get(0))
            .unwrap()
    }
    fn post_multipart(
        &self,
        token: &str,
        fields: &[(&str, &str)],
        extra: Option<(&str, &str)>,
    ) -> (u16, String) {
        let boundary = format!("cad1337-{}", uuid::Uuid::new_v4().simple());
        let mut body = String::new();
        for (name, value) in fields {
            body.push_str(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"));
        }
        if let Some((name, value)) = extra {
            body.push_str(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"));
        }
        body.push_str(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"http.txt\"\r\nContent-Type: text/plain\r\n\r\n{}\r\n--{boundary}--\r\n", String::from_utf8_lossy(FILE_BYTES)));
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let response = agent
            .post(&format!("{}/api/chat/upload", self.base()))
            .header("Host", &self.host_header())
            .header("Origin", format!("http://{PUBLIC_HOST}"))
            .header("X-Cadence-Board", "1")
            .header(crate::test_seam::AS_HEADER, "operator")
            .header(crate::test_seam::TOKEN_HEADER, &self.seam_token)
            .header("Cookie", format!("__Host-aos-board-session={token}"))
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .send(body)
            .unwrap();
        let status = response.status().as_u16();
        let mut response = response;
        let text = response
            .body_mut()
            .read_to_string()
            .expect("read HTTP upload response body");
        (status, text)
    }
}

/// Actual HTTP peer acceptance: an authenticated operator positive control
/// reaches the native upload guard; member, forged identity and wrong-thread
/// requests refuse without creating a metadata row. Native exact-turn and
/// private-root checks remain in the sibling daemon acceptance.
#[test]
fn cad1337_http_upload_is_authenticated_and_home_thread_bound() {
    let fx = Fx::new();
    let (owner, account_id) = fx.session("usr-cad1337-http-owner", "owner");
    let (member, _member_account) = fx.session("usr-cad1337-http-member", "member");
    let master_home = fx._home_id("master");
    let worker_home = fx._home_id("worker");

    let before = fx.row_count();
    let (status, body) = fx.post_multipart(&owner, &[("conversation", &master_home)], None);
    assert_eq!(
        status, 200,
        "real owner session + Master Home positive control: {body}"
    );
    let receipt: Value =
        serde_json::from_str(&body).expect("upload route returns daemon receipt JSON");
    let id = receipt["id"].as_str().expect("daemon-minted attachment id");
    assert_eq!(
        fx.row_count(),
        before + 1,
        "positive HTTP upload must reach the real daemon once"
    );
    let workspace = fx.root.path().join("workspace");
    let original = workspace
        .join("users")
        .join(&account_id)
        .join("threads")
        .join(&master_home)
        .join("uploads")
        .join(id)
        .join("http.txt");
    assert_eq!(
        std::fs::read(original).unwrap(),
        FILE_BYTES,
        "HTTP peer must retain original bytes under account + real Home thread"
    );

    let before_member = fx.row_count();
    let (status, body) = fx.post_multipart(&member, &[("conversation", &master_home)], None);
    assert!(
        status == 401 || status == 403,
        "ordinary member unexpectedly passed OperatorOnly upload route: {status} {body}"
    );
    assert_eq!(
        fx.row_count(),
        before_member,
        "ordinary account caused retained metadata"
    );

    for field in ["account_id", "user_id", "actor", "path", "scope"] {
        let before_forgery = fx.row_count();
        let (status, body) = fx.post_multipart(
            &owner,
            &[("conversation", &master_home)],
            Some((field, "forged-user-or-path")),
        );
        assert_eq!(
            status, 400,
            "browser-supplied {field} must be refused by HTTP parser: {body}"
        );
        assert!(
            body.contains("not accepted"),
            "{field} refused at wrong HTTP guard: {body}"
        );
        assert_eq!(
            fx.row_count(),
            before_forgery,
            "forged {field} persisted a row"
        );
    }

    let before_wrong = fx.row_count();
    let (status, body) = fx.post_multipart(&owner, &[("conversation", &worker_home)], None);
    assert!(
        !(200..300).contains(&status),
        "real but foreign Home thread reached upload: {status} {body}"
    );
    assert!(
        body.to_ascii_lowercase().contains("conversation")
            || body.to_ascii_lowercase().contains("home"),
        "wrong-thread refusal did not identify target proof: {body}"
    );
    assert_eq!(
        fx.row_count(),
        before_wrong,
        "foreign Home target retained metadata"
    );
}

/// Ticket-derived HTTP-peer refusals: a distinct verified owner account may
/// not enqueue another owner's attachment, and no-follow custody refuses an
/// operational account symlink. The upload/readiness and message APIs run
/// through the actual listener, session verifier, relay and native guard.
#[cfg(unix)]
#[test]
fn cad1337_http_cross_account_attachment_send_and_symlink_refuse() {
    let fx = Fx::new();
    let (owner_a, account_a) = fx.session("usr-cad1337-http-owner-a", "owner");
    let (owner_b, _account_b) = fx.session("usr-cad1337-http-owner-b", "owner");
    let home = fx._home_id("master");
    let (status, body) = fx.post_multipart(&owner_a, &[("conversation", &home)], None);
    assert_eq!(status, 200, "owner A upload positive control: {body}");
    let receipt: Value = serde_json::from_str(&body).unwrap();
    let attachment = receipt["id"]
        .as_str()
        .expect("actual daemon attachment id")
        .to_owned();

    let post_message = |token: &str, message_id: &str| {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let response = agent
            .post(&format!("{}/api/threads/master/messages", fx.base()))
            .header("Host", &fx.host_header())
            .header("Origin", format!("http://{PUBLIC_HOST}"))
            .header("X-Cadence-Board", "1")
            .header(crate::test_seam::AS_HEADER, "operator")
            .header(crate::test_seam::TOKEN_HEADER, &fx.seam_token)
            .header("Cookie", format!("__Host-aos-board-session={token}"))
            .header("Content-Type", "application/json")
            .send(
                serde_json::json!({
                    "text":"send the attached file",
                    "message":message_id,
                    "conversation":home,
                    "attachments":[{"id":attachment}],
                })
                .to_string(),
            )
            .unwrap();
        let status = response.status().as_u16();
        let mut response = response;
        let body = response.body_mut().read_to_string().unwrap();
        (status, body)
    };
    let (status, body) = post_message(&owner_a, "cad1337-http-owner-a-send");
    assert_eq!(
        status, 200,
        "owner A attachment-send positive control: {body}"
    );
    let queued: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        queued["state"], "queued",
        "owner A message was not queued: {body}"
    );

    let before_b = fx.row_count();
    let (status, body) = post_message(&owner_b, "cad1337-http-owner-b-cross-send");
    assert!(
        !(200..300).contains(&status),
        "owner B sent owner A's attachment: {body}"
    );
    assert!(
        body.contains("attachment is not bound to this authenticated account and conversation"),
        "wrong refusal layer: {body}"
    );
    assert_eq!(
        fx.row_count(),
        before_b,
        "cross-account send changed attachment metadata"
    );
    let db = rusqlite::Connection::open(crate::rollout::db_file(&fx.state())).unwrap();
    let b_messages: i64 = db
        .query_row(
            "SELECT count(*) FROM messages WHERE id=?",
            ["cad1337-http-owner-b-cross-send"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(b_messages, 0, "refused cross-account message was queued");

    // Replace only this fixture's operational account directory with a
    // symlink to an owned outside sentinel. The real upload route must refuse
    // and must not create any file or row at the symlink target.
    let users = fx.root.path().join("workspace/users");
    let account_path = users.join(&account_a);
    let saved = users.join(format!("{account_a}.saved"));
    let outside = fx.root.path().join("outside-sentinel");
    std::fs::create_dir(&outside).unwrap();
    let sentinel = outside.join("sentinel.txt");
    std::fs::write(&sentinel, b"outside fixture sentinel\n").unwrap();
    std::fs::rename(&account_path, &saved).unwrap();
    std::os::unix::fs::symlink(&outside, &account_path).unwrap();
    let before_symlink = fx.row_count();
    let (status, body) = fx.post_multipart(&owner_a, &[("conversation", &home)], None);
    assert!(
        !(200..300).contains(&status),
        "HTTP upload crossed account symlink: {body}"
    );
    assert_eq!(
        fx.row_count(),
        before_symlink,
        "symlink refusal persisted metadata"
    );
    assert_eq!(
        std::fs::read(&sentinel).unwrap(),
        b"outside fixture sentinel\n"
    );
    assert_eq!(
        std::fs::read_dir(&outside).unwrap().count(),
        1,
        "HTTP created outside bytes"
    );
}

impl Fx {
    fn _home_id(&self, alias: &str) -> String {
        let db = rusqlite::Connection::open(crate::rollout::db_file(&self.state())).unwrap();
        db.query_row(
            "SELECT id FROM threads WHERE alias=? AND install_id IS NULL",
            [alias],
            |r| r.get(0),
        )
        .unwrap()
    }
}

include!("cad1337_repair_acceptance.rs");
