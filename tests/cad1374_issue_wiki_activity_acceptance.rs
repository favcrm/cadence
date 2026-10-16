//! Independent CAD-1374 issue/wiki acceptance. Exercises the real board,
//! signed hosted sessions, daemon preparation guard, tracker Git receipts,
//! and owner activity query; no product helpers are mocked.
#![cfg(feature = "test-seam")]

use base64::Engine;
use cadence_agent::test_seam::{scoped, Asserted};
use cadence_agent::{client, daemon, master};
use ring::signature::KeyPair;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::{Builder, TempDir};

const COMPANY: &str = "co_cad1374";
const KID: &str = "cad1374-issue-wiki-key";

type Session = String;

struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl EnvRestore {
    fn install(root: &Path) -> Self {
        let entries = [
            ("HOME", root.join("home")),
            ("XDG_CONFIG_HOME", root.join("xdg-config")),
            ("XDG_CACHE_HOME", root.join("xdg-cache")),
            ("TMPDIR", root.to_path_buf()),
        ];
        for (_, path) in &entries {
            std::fs::create_dir_all(path).unwrap();
        }
        let saved = entries
            .iter()
            .map(|(key, _)| (*key, std::env::var_os(key)))
            .collect();
        for (key, value) in entries {
            std::env::set_var(key, value);
        }
        Self(saved)
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (key, value) in self.0.drain(..) {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
    }
}

static FIXTURE_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Fx {
    root: TempDir,
    _env: EnvRestore,
    issuer: String,
    signer: ring::signature::Ed25519KeyPair,
    daemon_stop: Arc<AtomicBool>,
    daemon: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    board_stop: Arc<AtomicBool>,
    board: Option<std::thread::JoinHandle<()>>,
    port: u16,
    jwks: Option<(Arc<tiny_http::Server>, std::thread::JoinHandle<()>)>,
    _serial: std::sync::MutexGuard<'static, ()>,
}

impl Fx {
    fn new(start_master: bool) -> Self {
        let serial = FIXTURE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let root = Builder::new()
            .prefix("c1374iw-")
            .tempdir_in("/tmp")
            .unwrap();
        let env = EnvRestore::install(root.path());
        let state = root.path().join("state");
        let pm = root.path().join("pm");
        std::fs::create_dir_all(&state).unwrap();
        cadence_agent::issue::Pm::init(&pm).unwrap();
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(&pm)
            .args(["config", "--local", "user.name", "CAD1374 acceptance"]);
        let output = cadence_agent::reaper::output(&mut command).unwrap();
        assert!(
            output.status.success(),
            "fixture git user.name: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut command = Command::new("git");
        command.arg("-C").arg(&pm).args([
            "config",
            "--local",
            "user.email",
            "cad1374-acceptance@example.invalid",
        ]);
        let output = cadence_agent::reaper::output(&mut command).unwrap();
        assert!(
            output.status.success(),
            "fixture git user.email: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        // A local synthetic project makes the HTTP-created issue resolve
        // through the normal project selector without consulting a checkout.
        let project = pm.join("cad1374");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("project.yaml"),
            "key: cad1374\nprefix: C1374\n",
        )
        .unwrap();
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(&pm)
            .args(["add", "cad1374/project.yaml"]);
        let output = cadence_agent::reaper::output(&mut command).unwrap();
        assert!(
            output.status.success(),
            "fixture project git add: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(&pm)
            .args(["commit", "-qm", "fixture project"]);
        let output = cadence_agent::reaper::output(&mut command).unwrap();
        assert!(
            output.status.success(),
            "fixture project commit: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut yaml = std::fs::read_to_string(pm.join("pm.yaml")).unwrap();
        yaml.push_str("\npi:\n  models:\n    allow: [\"fake/model-1\"]\n    default: {master: \"fake/model-1\", worker: \"fake/model-1\"}\n");
        std::fs::write(pm.join("pm.yaml"), yaml).unwrap();

        let signer = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[0x74; 32]).unwrap();
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let jwks = json!({"keys":[{"kty":"OKP","crv":"Ed25519","kid":KID,"x":b64.encode(signer.public_key().as_ref())}]}).to_string();
        let (jwks_port, jwks_server) = (3110..3200)
            .find_map(|port| {
                tiny_http::Server::http(format!("127.0.0.1:{port}"))
                    .ok()
                    .map(|server| (port, Arc::new(server)))
            })
            .expect("free isolated JWKS loopback port");
        let issuer = format!("http://127.0.0.1:{jwks_port}");
        let jwks_thread_server = Arc::clone(&jwks_server);
        let jwks_thread = std::thread::spawn(move || {
            for request in jwks_thread_server.incoming_requests() {
                let _ = request.respond(tiny_http::Response::from_string(jwks.clone()));
            }
        });
        let port = (3110..3200)
            .find(|port| *port != jwks_port && TcpListener::bind(("127.0.0.1", *port)).is_ok())
            .expect("free isolated board loopback port");
        let host = format!("issue-wiki.board.localhost:{port}");
        cadence_agent::board_identity::write_config(
            &state,
            &cadence_agent::board_identity::Config {
                host: host.clone(),
                issuer: issuer.clone(),
                company: COMPANY.into(),
            },
        )
        .unwrap();

        let provider_env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        provider_env.set("CADENCE_PM_DIR", pm.to_string_lossy().to_string());
        provider_env.set(
            "HOME",
            root.path().join("home").to_string_lossy().to_string(),
        );
        provider_env.set(
            "CADENCE_PI_COMMAND",
            format!(
                "python3 {}",
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/pi-gateway.py")
                    .display()
            ),
        );
        provider_env.set(master::TEST_NO_LANDLOCK, "1");
        let daemon_stop = Arc::new(AtomicBool::new(false));
        let mut daemon_opts = daemon::ServeOptions {
            provider_env,
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
        cadence_agent::platform::local::register_at(
            &state,
            &mut daemon_opts,
            state.join("outbox"),
            format!("http://127.0.0.1:{port}"),
        );
        let daemon_dir = state.clone();
        let daemon = std::thread::spawn(move || daemon::serve_with(&daemon_dir, daemon_opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(1)).is_err()
            || cadence_agent::test_seam::Seam::token_at(&state).is_none()
        {
            if daemon.is_finished() {
                panic!("daemon exited during startup: {:?}", daemon.join().unwrap());
            }
            assert!(
                std::time::Instant::now() < deadline,
                "daemon startup timeout"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        if start_master {
            scoped(Asserted::Operator, || {
                client::rpc(
                    &state,
                    "master_start",
                    json!({"provider":"pi","unconfined":true}),
                )
            })
            .unwrap();
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let board_stop = Arc::new(AtomicBool::new(false));
        let board_opts = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(board_stop.clone()),
            startup: Some(tx),
            test_seam: true,
            allow_hosts: vec![host.clone()],
            allow_origins: vec![format!("http://{host}")],
            public: Some(cadence_agent::ui::PublicBoard {
                host: host.clone(),
                authorize_url: format!("{issuer}/v2/board/authorize"),
                issuer: issuer.clone(),
                company: COMPANY.into(),
            }),
            ..Default::default()
        };
        let (dir, board_pm) = (state, pm);
        let board = std::thread::spawn(move || {
            let _ = cadence_agent::ui::serve(&dir, &board_pm, &board_opts);
        });
        rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
        Self {
            root,
            _env: env,
            issuer,
            signer,
            daemon_stop,
            daemon: Some(daemon),
            board_stop,
            board: Some(board),
            port,
            jwks: Some((jwks_server, jwks_thread)),
            _serial: serial,
        }
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("state")
    }
    fn pm(&self) -> PathBuf {
        self.root.path().join("pm")
    }
    fn host(&self) -> String {
        format!("issue-wiki.board.localhost:{}", self.port)
    }
    fn rpc(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(Asserted::Operator, || {
            client::rpc(&self.state(), method, params)
        })
    }
    fn http(&self, method: &str, path: &str, cookie: Option<&str>, body: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: {}\r\nOrigin: http://{}\r\nSec-Fetch-Site: same-origin\r\nX-Cadence-Board: 1\r\nContent-Type: application/json\r\nConnection: close\r\n{}Content-Length: {}\r\n\r\n{body}", self.host(), self.host(), cookie.map(|c| format!("Cookie: {c}\r\n")).unwrap_or_default(), body.len()).unwrap();
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
        (status, response)
    }
    fn assertion(&self, sub: &str, role: &str, jti: &str) -> String {
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let claims = json!({"iss":self.issuer,"aud":self.host(),"sub":sub,"email":format!("{sub}@example.test"),"name":format!("label-{sub}"),"company":COMPANY,"role":role,"iat":now-1,"exp":now+59,"jti":jti});
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
    fn login(&self, sub: &str, role: &str) -> Session {
        self.login_with_jti(sub, role, &format!("jti-{sub}"))
    }
    fn login_with_jti(&self, sub: &str, role: &str, jti: &str) -> Session {
        let assertion = self.assertion(sub, role, jti);
        let (status, response) = self.http(
            "POST",
            "/__platform/session",
            None,
            &json!({"assertion":assertion}).to_string(),
        );
        assert_eq!(status, 200, "signed session open: {response}");
        response
            .lines()
            .find_map(|line| {
                line.strip_prefix("Set-Cookie: ")
                    .or_else(|| line.strip_prefix("set-cookie: "))
                    .map(|s| s.split(';').next().unwrap().to_string())
            })
            .expect("session cookie")
    }
    fn proof(cookie: &str) -> String {
        cookie.split_once('=').unwrap().1.to_string()
    }
    fn page(&self, cookie: &str, sub: &str) -> Value {
        let (status, response) = self.http(
            "GET",
            &format!("/api/activity?sub={sub}&limit=200"),
            Some(cookie),
            "",
        );
        assert_eq!(status, 200, "owner activity query: {response}");
        serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap()
    }
    fn transfer(&self, cookie: &str, writing: bool, path: &str, transfer: Value) -> (u16, Value) {
        let (status, response) = self.http(
            if writing { "PUT" } else { "POST" },
            if writing {
                "/api/wiki/transfer"
            } else {
                "/api/wiki/job_status"
            },
            Some(cookie),
            &json!({"path": path, "transfer": transfer}).to_string(),
        );
        let body = response.split_once("\r\n\r\n").unwrap().1;
        (
            status,
            serde_json::from_str(body).expect("transfer JSON response"),
        )
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

fn git(pm: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    command.arg("-C").arg(pm).args(args);
    let output = cadence_agent::reaper::output(&mut command).unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn core_events(fx: &Fx, kind: &str) -> Vec<Value> {
    let conn = Connection::open(fx.state().join("cadence.sqlite3")).unwrap();
    let mut stmt = conn
        .prepare("SELECT payload FROM events WHERE kind=? ORDER BY seq")
        .unwrap();
    stmt.query_map([kind], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|raw| serde_json::from_str(&raw.unwrap()).unwrap())
        .collect()
}

fn conversation_count(fx: &Fx, install: &str) -> i64 {
    Connection::open(fx.state().join("cadence.sqlite3"))
        .unwrap()
        .query_row(
            "SELECT count(*) FROM threads WHERE alias='master' AND install_id=?",
            [install],
            |row| row.get(0),
        )
        .unwrap()
}

fn assert_commit_receipt(fx: &Fx, revision: &str, sub: &str, path: &str, action: &str) {
    let shown = git(
        &fx.pm(),
        &["show", "--format=%H%n%B", "--name-only", revision],
    );
    assert!(
        shown.contains(path),
        "expected changed path {path} in commit {revision}: {shown}"
    );
    let line = shown
        .lines()
        .find_map(|line| line.strip_prefix("Activity-Receipt: "))
        .expect("same Git commit must carry its Activity-Receipt trailer");
    let receipt: Value = serde_json::from_str(line).expect("canonical single-line receipt JSON");
    assert!(
        receipt["action"].as_str().is_some_and(|value| {
            if action == "wiki_" {
                value.starts_with(action)
            } else {
                value == action
            }
        }),
        "unexpected receipt action: {}",
        receipt["action"]
    );
    let human = &receipt["human"];
    assert_eq!(human["company"], COMPANY);
    assert_eq!(human["sub"], sub);
    assert!(human["session"].as_str().is_some());
    assert!(human["role"].as_str().is_some());
    assert!(receipt["digest"].as_str().is_some());
    assert!(receipt["request_id"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
    assert_eq!(receipt["request_id"], receipt["correlation_id"]);
    assert!(
        !line.contains("private-issue-canary")
            && !line.contains("owner-B-comment-canary")
            && !line.contains("private-wiki-canary")
    );
}

#[test]
fn issue_and_wiki_http_mutations_have_prepared_intent_and_same_commit_receipts() {
    let fx = Fx::new(true);
    let owner_a = fx.login("user-owner-a", "owner");
    let owner_b = fx.login("user-owner-b", "owner");
    let member = fx.login("user-member", "member");
    let proof_a = Fx::proof(&owner_a);
    let proof_b = Fx::proof(&owner_b);
    let proof_member = Fx::proof(&member);

    // Install only repository-owned synthetic app files. This gives the
    // conversation route a real daemon-proven installation without external
    // package resolution or customer data.
    let source = fx.root.path().join("synthetic-blog-app");
    for name in [
        "app.md",
        "workflows/blog-post.md",
        "rubrics/blog.md",
        "templates/brief.md",
        "templates/post.md",
    ] {
        let from = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/apps/blog-post")
            .join(name);
        let to = source.join(name);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(from, to).unwrap();
    }
    let install = fx
        .rpc("app_workspace_install", json!({"source":source}))
        .unwrap()["install_id"]
        .as_str()
        .expect("synthetic install id")
        .to_string();

    // The issue itself is a local fixture. The tested update and comment go
    // through the real authenticated HTTP writer.
    let pm = cadence_agent::issue::Pm::at(&fx.pm()).unwrap();
    let created = cadence_agent::issue::write::new_issue(
        &pm,
        &fx.pm(),
        Some("cad1374"),
        "Acceptance fixture",
        None,
        None,
        &[],
        None,
        None,
        &[],
        Some("C1374-1"),
        Some("fixture body"),
        "fixture",
    )
    .unwrap();
    assert_eq!(created["id"], "C1374-1");
    drop(pm);
    let before = git(&fx.pm(), &["rev-parse", "HEAD"]);
    let issue_file = fx.pm().join("cad1374/C1374-1/issue.md");
    let issue_before = std::fs::read(&issue_file).unwrap();
    let (forged_status, forged_response) = fx.http("PATCH", "/api/issues/C1374-1", Some(&owner_a), &json!({
        "title":"Owner A revised title", "body":"private-issue-canary", "actor":"forged-owner-b",
        "role":"member", "company":"forged-company", "public_session":"forged-session"
    }).to_string());
    assert_eq!(
        forged_status, 400,
        "issue DTO must refuse unknown identity fields: {forged_response}"
    );
    assert_eq!(
        git(&fx.pm(), &["rev-parse", "HEAD"]),
        before,
        "forged issue request moved Git HEAD"
    );
    assert_eq!(
        std::fs::read(&issue_file).unwrap(),
        issue_before,
        "forged issue request changed issue state"
    );

    let (status, response) = fx.http(
        "PATCH",
        "/api/issues/C1374-1",
        Some(&owner_a),
        &json!({
            "title":"Owner A revised title", "body":"private-issue-canary"
        })
        .to_string(),
    );
    assert_eq!(
        status, 200,
        "signed owner issue patch positive control: {response}"
    );
    let patch_head = git(&fx.pm(), &["rev-parse", "HEAD"]);
    assert_ne!(patch_head, before, "issue writer did not commit");

    let comments_dir = fx.pm().join("cad1374/C1374-1/comments");
    let comments_before = || {
        let mut entries = std::fs::read_dir(&comments_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        entries.sort();
        entries
    };
    let comments_before_forgery = comments_before();
    let before_forged_comment_head = git(&fx.pm(), &["rev-parse", "HEAD"]);
    let (forged_status, forged_response) = fx.http("POST", "/api/issues/C1374-1/comments", Some(&owner_b), &json!({
        "body":"owner-B-comment-canary", "actor":"user-owner-a", "role":"owner", "company":"elsewhere"
    }).to_string());
    assert_eq!(
        forged_status, 400,
        "comment DTO must refuse unknown identity fields: {forged_response}"
    );
    assert_eq!(
        git(&fx.pm(), &["rev-parse", "HEAD"]),
        before_forged_comment_head,
        "forged comment request moved Git HEAD"
    );
    assert_eq!(
        comments_before(),
        comments_before_forgery,
        "forged comment request changed comments"
    );

    let (status, response) = fx.http(
        "POST",
        "/api/issues/C1374-1/comments",
        Some(&owner_b),
        &json!({
            "body":"owner-B-comment-canary"
        })
        .to_string(),
    );
    assert_eq!(
        status, 200,
        "signed owner issue comment positive control: {response}"
    );
    let comment_head = git(&fx.pm(), &["rev-parse", "HEAD"]);
    assert_ne!(comment_head, patch_head, "issue comment did not commit");

    // Preserve the existing documented member comment capability; the new
    // owner-only mutation preparation must not remove it or misattribute it.
    let (member_status, member_response) = fx.http(
        "POST",
        "/api/issues/C1374-1/comments",
        Some(&member),
        &json!({"body":"member comment"}).to_string(),
    );
    assert_eq!(
        member_status, 200,
        "existing member comment permission regressed: {member_response}"
    );

    // A verified member may write shared wiki and its own private prefix.
    // The named claim must still equal the live signed handle.
    let (status, response) = fx.http(
        "PUT",
        "/api/wiki/file",
        Some(&member),
        &json!({
            "path":"users/user-member/cad1374.md", "text":"private-wiki-canary",
            "wiki_as":"user:user-member", "actor":"user-owner-a", "company":"elsewhere"
        })
        .to_string(),
    );
    assert_eq!(
        status, 200,
        "signed member own-prefix wiki write: {response}"
    );
    let wiki_head = git(&fx.pm(), &["rev-parse", "HEAD"]);
    assert_ne!(wiki_head, comment_head, "wiki write did not commit");
    assert!(
        !git(&fx.pm(), &["status", "--short", "--", "pm.yaml"]).is_empty(),
        "foreign fixture config was unexpectedly staged/committed"
    );
    let (wrong_claim, wrong_reply) = fx.http("PUT", "/api/wiki/file", Some(&member), &json!({
        "path":"users/user-owner-a/forged.md", "text":"must not land", "wiki_as":"user:user-owner-a"
    }).to_string());
    assert_ne!(
        wrong_claim, 200,
        "member proof wrote as another live handle: {wrong_reply}"
    );
    let (shared_write, shared_reply) = fx.http("PUT", "/api/wiki/file", Some(&member), &json!({
        "path":"global/cad1374-member.md", "text":"shared-wiki-canary", "wiki_as":"user:user-member"
    }).to_string());
    assert_eq!(
        shared_write, 200,
        "signed member shared wiki write: {shared_reply}"
    );
    let shared_head = git(&fx.pm(), &["rev-parse", "HEAD"]);
    assert_ne!(
        shared_head, wiki_head,
        "member shared wiki write did not commit"
    );
    let (private_denied, private_reply) = fx.http("PUT", "/api/wiki/file", Some(&member), &json!({
        "path":"users/user-owner-a/cross-member.md", "text":"must not land", "wiki_as":"user:user-member"
    }).to_string());
    assert_ne!(
        private_denied, 200,
        "member wrote another user's private prefix: {private_reply}"
    );
    assert_eq!(git(&fx.pm(), &["rev-parse", "HEAD"]), shared_head);
    assert!(!fx
        .pm()
        .join("wiki/users/user-owner-a/cross-member.md")
        .exists());

    // Verify durable prepared intents through the daemon store and their
    // fail-closed ordering at the real issue and wiki HTTP guards.
    let events = core_events(&fx, "pm_mutation_prepared");
    assert!(events.iter().any(|event| event["action"] == "issue_updated"
        && event["resource"]
            .as_str()
            .unwrap_or_default()
            .contains("C1374-1")));
    assert!(events
        .iter()
        .any(|event| event["action"] == "issue_comment_added"
            && event["resource"]
                .as_str()
                .unwrap_or_default()
                .contains("C1374-1")));
    assert!(events.iter().any(|event| event["resource"]
        .as_str()
        .unwrap_or_default()
        .contains("users/user-member/cad1374.md")));
    assert!(events
        .iter()
        .any(|event| event["human"]["sub"] == "user-member"
            && event["resource"]
                .as_str()
                .is_some_and(|path| path.contains("global/cad1374-member.md"))));
    for (action, sub) in [
        ("issue_updated", "user-owner-a"),
        ("issue_comment_added", "user-owner-b"),
    ] {
        let event = events
            .iter()
            .find(|event| event["action"] == action)
            .expect("prepared issue intent");
        assert_eq!(
            event["human"]["sub"], sub,
            "forged body identity changed attribution"
        );
        assert_eq!(event["human"]["company"], COMPANY);
    }
    for event in &events {
        let human = &event["human"];
        assert_eq!(human["company"], COMPANY);
        assert!(human["sub"].as_str().is_some());
        assert!(human["session"].as_str().is_some());
        assert!(human["role"].as_str().is_some());
        assert!(
            !event.to_string().contains(&proof_a)
                && !event.to_string().contains(&proof_b)
                && !event.to_string().contains(&proof_member)
        );
        assert!(
            !event.to_string().contains("private-issue-canary")
                && !event.to_string().contains("private-wiki-canary")
                && !event.to_string().contains("shared-wiki-canary")
        );
    }
    assert_commit_receipt(
        &fx,
        &patch_head,
        "user-owner-a",
        "cad1374/C1374-1/issue.md",
        "issue_updated",
    );
    assert_commit_receipt(
        &fx,
        &comment_head,
        "user-owner-b",
        "cad1374/C1374-1/comments/",
        "issue_comment_added",
    );
    assert_commit_receipt(
        &fx,
        &wiki_head,
        "user-member",
        "wiki/users/user-member/cad1374.md",
        "wiki_",
    );
    assert_commit_receipt(
        &fx,
        &shared_head,
        "user-member",
        "wiki/global/cad1374-member.md",
        "wiki_",
    );

    // The owner-only query must merge the actual Git receipts, with distinct
    // server-verified identities and actual commit revisions.
    let page_a = fx.page(&owner_a, "user-owner-a");
    let page_b = fx.page(&owner_b, "user-owner-b");
    let rows_a = page_a["events"].as_array().unwrap();
    let rows_b = page_b["events"].as_array().unwrap();
    assert!(rows_a
        .iter()
        .any(|event| event["human"]["sub"] == "user-owner-a"
            && event["revision_or_digest"]
                .as_str()
                .is_some_and(|rev| rev.contains(&patch_head))));
    assert!(rows_b
        .iter()
        .any(|event| event["human"]["sub"] == "user-owner-b"
            && event["revision_or_digest"]
                .as_str()
                .is_some_and(|rev| rev.contains(&comment_head))));
    assert!(
        !page_a.to_string().contains("user-owner-b")
            && !page_b.to_string().contains("user-owner-a")
    );
    assert!(
        !page_a.to_string().contains("private-issue-canary")
            && !page_b.to_string().contains("owner-B-comment-canary")
    );
    let member_rows = fx.page(&owner_a, "user-member")["events"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        member_rows
            .iter()
            .any(|event| event["human"]["sub"] == "user-member"),
        "permitted member comment lost verified attribution: {member_rows:?}"
    );

    // Activity preparation storage failure must stop a real HTTP mutation
    // before either the issue file or Git HEAD changes. Removing the fault is
    // a positive control on the same route.
    let issue_file = fx.pm().join("cad1374/C1374-1/issue.md");
    let original_file = std::fs::read(&issue_file).unwrap();
    let original_head = git(&fx.pm(), &["rev-parse", "HEAD"]);
    let conn = Connection::open(fx.state().join("cadence.sqlite3")).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_pm_mutation_prepared BEFORE INSERT ON events WHEN NEW.kind='pm_mutation_prepared' BEGIN SELECT RAISE(ABORT,'acceptance: reject prepared PM intent'); END;").unwrap();
    let (failed_status, failed_reply) = fx.http(
        "PATCH",
        "/api/issues/C1374-1",
        Some(&owner_a),
        &json!({"title":"must not persist"}).to_string(),
    );
    assert_ne!(
        failed_status, 200,
        "mutation passed when durable prepared intent INSERT aborted: {failed_reply}"
    );
    assert_eq!(
        std::fs::read(&issue_file).unwrap(),
        original_file,
        "failed prepare changed issue file"
    );
    assert_eq!(
        git(&fx.pm(), &["rev-parse", "HEAD"]),
        original_head,
        "failed prepare moved tracker HEAD"
    );
    conn.execute_batch("DROP TRIGGER reject_pm_mutation_prepared;")
        .unwrap();
    drop(conn);
    let (positive_status, positive_reply) = fx.http(
        "PATCH",
        "/api/issues/C1374-1",
        Some(&owner_a),
        &json!({"title":"positive control"}).to_string(),
    );
    assert_eq!(
        positive_status, 200,
        "valid issue route failed after removing storage fault: {positive_reply}"
    );

    // Owner trail cannot be read/exported by a member; denial does not
    // poison the owner path. A daemon agent connection cannot become a human
    // by presenting the owner's valid session proof.
    for operation in ["activity_query", "activity_export"] {
        assert!(
            fx.rpc(operation, json!({"public_session":proof_member,"limit":10}))
                .is_err(),
            "member accessed {operation}"
        );
        let path = if operation == "activity_export" {
            "/api/activity/export?limit=10"
        } else {
            "/api/activity?limit=10"
        };
        let (status, response) = fx.http("GET", path, Some(&member), "");
        assert_ne!(status, 200, "member HTTP {operation} access: {response}");
    }
    assert!(fx.page(&owner_a, "user-owner-a")["events"]
        .as_array()
        .is_some());

    // The existing operator-only conversation-create capability remains
    // gated, but a verified owner browser can relay it as the owner. Fresh
    // always inserts a new conversation; no idempotent-reopen event is
    // inferred. Two owners remain distinct and member callers are refused.
    let conversation = |cookie: &str| {
        let (status, response) = fx.http(
            "POST",
            "/api/threads/master/conversations",
            Some(cookie),
            &json!({"install_id":install}).to_string(),
        );
        assert_eq!(
            status, 200,
            "verified owner conversation create: {response}"
        );
        serde_json::from_str::<Value>(response.split_once("\r\n\r\n").unwrap().1).unwrap()
    };
    let before_conversations = conversation_count(&fx, &install);
    let before_conversation_events = core_events(&fx, "conversation_created").len();
    let owner_a_conversation = conversation(&owner_a);
    let owner_b_conversation = conversation(&owner_b);
    assert_eq!(owner_a_conversation["created"], true);
    assert_eq!(owner_b_conversation["created"], true);
    let conversation_a = owner_a_conversation["conversation"]["id"].as_str().unwrap();
    let conversation_b = owner_b_conversation["conversation"]["id"].as_str().unwrap();
    assert_ne!(
        conversation_a, conversation_b,
        "Fresh create is intentionally not idempotent"
    );
    assert_eq!(conversation_count(&fx, &install), before_conversations + 2);
    let conversation_events = core_events(&fx, "conversation_created");
    assert_eq!(
        conversation_events.len(),
        before_conversation_events + 2,
        "Fresh creates each emit one transaction event"
    );
    let recent = conversation_events[before_conversation_events..]
        .iter()
        .rev()
        .collect::<Vec<_>>();
    for (event, expected_sub, expected_id) in [
        (recent[0], "user-owner-b", conversation_b),
        (recent[1], "user-owner-a", conversation_a),
    ] {
        assert_eq!(event["schema_version"], 1);
        assert_eq!(event["human"]["company"], COMPANY);
        assert_eq!(event["human"]["sub"], expected_sub);
        assert!(event["human"]["session"].as_str().is_some());
        assert!(event["human"]["role"].as_str().is_some());
        assert_eq!(event["outcome"], "created");
        assert_eq!(event["reason"], "conversation_created");
        assert_eq!(event["thread"], expected_id);
        let request_id = event["request_id"].as_str().expect("request UUID");
        assert!(
            uuid::Uuid::parse_str(request_id).is_ok(),
            "request id is not a UUID: {request_id}"
        );
        assert_eq!(event["request_id"], event["correlation_id"]);
        assert!(!event.to_string().contains(&proof_a) && !event.to_string().contains(&proof_b));
    }
    let (member_conversation, member_conversation_reply) = fx.http(
        "POST",
        "/api/threads/master/conversations",
        Some(&member),
        &json!({"install_id":install}).to_string(),
    );
    assert_ne!(member_conversation, 200, "member gained the existing operator-only conversation capability: {member_conversation_reply}");
    assert_eq!(conversation_count(&fx, &install), before_conversations + 2);

    let count_before_fault = conversation_count(&fx, &install);
    let conn = Connection::open(fx.state().join("cadence.sqlite3")).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_conversation_receipt BEFORE INSERT ON events WHEN NEW.kind='conversation_created' BEGIN SELECT RAISE(ABORT,'acceptance: reject conversation receipt'); END;").unwrap();
    let (failed_status, failed_reply) = fx.http(
        "POST",
        "/api/threads/master/conversations",
        Some(&owner_a),
        &json!({"install_id":install}).to_string(),
    );
    assert_ne!(
        failed_status, 200,
        "conversation returned despite event INSERT abort: {failed_reply}"
    );
    assert_eq!(
        conversation_count(&fx, &install),
        count_before_fault,
        "conversation row escaped failed event transaction"
    );
    conn.execute_batch("DROP TRIGGER reject_conversation_receipt;")
        .unwrap();
    drop(conn);
    let positive = conversation(&owner_a);
    assert_eq!(
        positive["created"], true,
        "positive control after event fault removal"
    );
    assert_eq!(conversation_count(&fx, &install), count_before_fault + 1);
    let projected = fx.page(&owner_a, "user-owner-a");
    assert!(
        projected["events"].as_array().unwrap().iter().any(|row| {
            row["target"] == format!("alias:master/conversation:{conversation_a}")
                && row["source"] == "core"
                && row["revision_or_digest"].is_null()
        }),
        "owner projection omitted the actual conversation-created receipt: {}",
        projected["events"]
    );

    let agent = scoped(Asserted::Agent("master".into()), || {
        client::rpc(
            &fx.state(),
            "pm_activity_prepare",
            json!({
                "action":"issue_updated", "resource":"issue:C1374-1", "public_session":proof_a
            }),
        )
    });
    assert!(
        agent.is_err(),
        "managed agent became a human mutation caller using a valid hosted proof"
    );
}

#[test]
fn cad914_public_session_transfer_has_subject_custody_and_canonical_receipts() {
    use sha2::Digest;
    let fx = Fx::new(false);
    let sub = "transfer-owner-a";
    let owner_a = fx.login(sub, "owner");
    let owner_b = fx.login("transfer-owner-b", "owner");
    let root = "global/public-transfer";
    let text = b"synthetic source revision\n";
    let blob = b"\x00\x01\xfe\xffsynthetic original";
    let manifest = json!({
        "version": 1, "source_id": "synthetic-public-transfer", "root": root,
        "entries": [
            {"id":"text","path":"page.md","kind":"text","size":text.len(),
             "sha256":format!("{:x}",sha2::Sha256::digest(text)),"if_rev":"none"},
            {"id":"blob","path":"original.bin","kind":"blob","size":blob.len(),
             "sha256":format!("{:x}",sha2::Sha256::digest(blob)),"if_rev":"none"},
            {"id":"dir","path":"empty","kind":"dir","size":0,"sha256":null,"if_rev":"none"}
        ]
    });
    let (status, begun) = fx.transfer(
        &owner_a,
        true,
        root,
        json!({"op":"begin","manifest":manifest}),
    );
    assert_eq!(status, 200, "{begun}");
    let job = begun["job_id"].as_str().expect("durable job id");
    let (status, denied) = fx.transfer(&owner_b, false, root, json!({"op":"status","job_id":job}));
    assert_eq!(status, 400, "{denied}");
    assert!(
        denied["job_id"].is_null(),
        "another subject received a job receipt"
    );
    assert!(
        fx.rpc(
            "wiki_transfer",
            json!({"op":"status","job_id":job,"root":root})
        )
        .is_err(),
        "unbound local operator inherited a public subject's job"
    );
    let owner_a = fx.login_with_jti(sub, "owner", "transfer-owner-a-renewed");
    let (status, resumed) = fx.transfer(&owner_a, false, root, json!({"op":"status","job_id":job}));
    assert_eq!(status, 200, "{resumed}");
    assert_eq!(
        resumed["digest"], begun["digest"],
        "session renewal changed job custody"
    );
    for (id, bytes, relative, action) in [
        ("text", text.as_slice(), "page.md", "wiki_written"),
        (
            "blob",
            blob.as_slice(),
            "original.bin",
            "wiki_blob_uploaded",
        ),
        ("dir", &[][..], "empty", "wiki_directory_created"),
    ] {
        if id != "dir" {
            let (status, receipt) = fx.transfer(
                &owner_a,
                true,
                root,
                json!({
                    "op":"chunk","job_id":job,"entry_id":id,"offset":0,
                    "data_base64":base64::engine::general_purpose::STANDARD.encode(bytes)
                }),
            );
            assert_eq!(status, 200, "{receipt}");
        }
        let (status, applied) = fx.transfer(
            &owner_a,
            true,
            root,
            json!({"op":"apply","job_id":job,"entry_id":id}),
        );
        assert_eq!(status, 200, "{applied}");
        let revision = git(&fx.pm(), &["rev-parse", "HEAD"]);
        assert_commit_receipt(
            &fx,
            revision.trim(),
            sub,
            &format!("wiki/{root}/{relative}"),
            action,
        );
        if id != "dir" {
            let path = format!("{root}/{relative}");
            let (status, exported) = fx.transfer(
                &owner_a,
                false,
                &path,
                json!({"op":"export","path":path,"offset":0,"bytes":16384}),
            );
            assert_eq!(status, 200, "{exported}");
            let actual = base64::engine::general_purpose::STANDARD
                .decode(exported["data_base64"].as_str().expect("original bytes"))
                .unwrap();
            assert_eq!(actual, bytes, "public-session original changed");
        }
    }
    let (status, finished) = fx.transfer(&owner_a, true, root, json!({"op":"finish","job_id":job}));
    assert_eq!(status, 200, "{finished}");
    assert_eq!(finished["state"], "complete");
    let empty_root = "global/public-transfer-empty";
    let (status, empty) = fx.transfer(
        &owner_a,
        true,
        empty_root,
        json!({
            "op":"begin","manifest":{"version":1,"source_id":"synthetic-empty",
                "root":empty_root,"entries":[]}
        }),
    );
    assert_eq!(status, 200, "{empty}");
    let (status, finished) = fx.transfer(
        &owner_a,
        true,
        empty_root,
        json!({"op":"finish","job_id":empty["job_id"]}),
    );
    assert_eq!(status, 200, "{finished}");
    assert_eq!(finished["state"], "complete");
    let revision = git(&fx.pm(), &["rev-parse", "HEAD"]);
    assert_commit_receipt(
        &fx,
        revision.trim(),
        sub,
        &format!("wiki/{empty_root}/.gitkeep"),
        "wiki_directory_created",
    );
    let (status, removed) = fx.http(
        "POST",
        "/api/wiki/rm",
        Some(&owner_a),
        &json!({"path":empty_root}).to_string(),
    );
    assert_eq!(status, 200, "{removed}");
    let removed_head = git(&fx.pm(), &["rev-parse", "HEAD"]);
    let (status, stale) = fx.transfer(
        &owner_a,
        false,
        empty_root,
        json!({"op":"status","job_id":empty["job_id"]}),
    );
    assert_eq!(status, 200, "{stale}");
    assert_eq!(stale["state"], "complete", "{stale}");
    assert_eq!(stale["index"], "superseded", "{stale}");
    assert_eq!(
        git(&fx.pm(), &["rev-parse", "HEAD"]),
        removed_head,
        "reading a completed receipt recreated a removed root"
    );
    println!("OBSERVED_OK completed empty-root HTTP receipt after authorized removal: {stale}");
}
