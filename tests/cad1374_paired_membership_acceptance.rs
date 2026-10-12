//! Paired CAD-1374/AOS-218 browser-session membership acceptance.
//! The platform side is an owned Vitest Worker/D1 child fixture; all board
//! sign-in, HTTP requests, daemon RPCs, session storage and queue inspection
//! use Cadence's real daemon and board handlers. This does not prove deployed
//! Cloudflare SDK-origin custody or external-action rollback.
#![cfg(feature = "test-seam")]

use cadence_agent::adapter::ProviderEnv;
use cadence_agent::test_seam::{scoped, Asserted};
use cadence_agent::{client, daemon, master, store::Store};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::{Builder, TempDir};

const API_DIR: &str = "/home/ubuntu/Project/agenticos-stack/worktrees/agenticos-v2-aos-218-browser-authority/apps/api";
const DRIVER: &str = "src/runtime/cad1374-paired-membership.fixture.test.ts";
const WAIT: Duration = Duration::from_secs(90);
const BODY_CAP: usize = 1024 * 1024;

type HttpResult = (u16, String, Option<String>);

struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl EnvRestore {
    fn install(values: &[(&'static str, String)]) -> Self {
        let old = values
            .iter()
            .map(|(name, _)| (*name, std::env::var_os(name)))
            .collect();
        for (name, value) in values {
            std::env::set_var(name, value);
        }
        Self(old)
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (name, value) in self.0.drain(..) {
            if let Some(value) = value {
                std::env::set_var(name, value);
            } else {
                std::env::remove_var(name);
            }
        }
    }
}

struct Driver {
    child: Child,
    port: u16,
    token: String,
}

impl Driver {
    fn start(port: u16, public_host: &str, ready: &Path, token: String) -> Self {
        let mut command = Command::new(Path::new(API_DIR).join("node_modules/.bin/vitest"));
        command
            .args([
                "run",
                DRIVER,
                "--pool=threads",
                "--maxWorkers=1",
                "--no-file-parallelism",
            ])
            .current_dir(API_DIR)
            .env("CAD1374_FIXTURE_PORT", port.to_string())
            .env("CAD1374_FIXTURE_READY", ready)
            .env("CAD1374_PUBLIC_HOST", public_host)
            .env("CAD1374_FIXTURE_TOKEN", &token)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cadence_agent::reaper::spawn(&mut command)
            .expect("start owned paired-membership Vitest child");
        Self { child, port, token }
    }

    fn request_shutdown(&self) {
        let Ok(mut stream) = TcpStream::connect(("127.0.0.1", self.port)) else {
            return;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let payload = "{}";
        let _ = write!(
            stream,
            "POST /__fixture__/shutdown HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Cad1374-Fixture-Token: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 2\r\n\r\n{}",
            self.port, self.token, payload
        );
        let _ = read_http(stream);
    }

    fn control(&self, path: &str, body: &Value) -> HttpResult {
        let payload = body.to_string();
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).expect("fixture loopback");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(
            stream,
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Cad1374-Fixture-Token: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            self.port,
            self.token,
            payload.len(),
            payload
        )
        .unwrap();
        read_http(stream)
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            self.request_shutdown();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match self.child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(50))
                    }
                    _ => {
                        let _ = self.child.kill();
                        break;
                    }
                }
            }
        }
        let _ = self.child.wait();
    }
}

struct Fx {
    root: TempDir,
    driver: Driver,
    daemon_stop: Arc<AtomicBool>,
    daemon: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    board_stop: Arc<AtomicBool>,
    board: Option<std::thread::JoinHandle<()>>,
    board_port: u16,
    host: String,
    _env_restore: EnvRestore,
}

impl Fx {
    fn new() -> Self {
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let root = Builder::new()
            .prefix("cad1374-paired-")
            .tempdir_in(base)
            .unwrap();
        let state = root.path().join("state");
        let pm = root.path().join("pm");
        let home = root.path().join("home");
        let ready = root.path().join("fixture-ready.json");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        cadence_agent::issue::Pm::init(&pm).unwrap();
        let mut yaml = std::fs::read_to_string(pm.join("pm.yaml")).unwrap();
        yaml.push_str("\npi:\n  models:\n    allow: [\"fake/model-1\"]\n    default: {master: \"fake/model-1\", worker: \"fake/model-1\"}\n");
        std::fs::write(pm.join("pm.yaml"), yaml).unwrap();

        let board_port = free_port();
        let fixture_port = free_port_excluding(board_port);
        let host = format!("cad1374.board.localhost:{board_port}");
        let token = format!("fixture-control-{}", uuid::Uuid::new_v4());
        let mut driver = Driver::start(fixture_port, &host, &ready, token);
        let deadline = Instant::now() + WAIT;
        while !ready.exists() {
            if let Some(status) = driver.child.try_wait().unwrap() {
                panic!("owned Vitest fixture exited before readiness ({status})");
            }
            assert!(
                Instant::now() < deadline,
                "paired Worker fixture readiness timeout"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let descriptor: Value = serde_json::from_slice(&std::fs::read(&ready).unwrap()).unwrap();
        let company = descriptor["company"].as_str().unwrap().to_owned();
        let instance = descriptor["instance"].as_str().unwrap().to_owned();
        let issuer = descriptor["publicIssuer"].as_str().unwrap().to_owned();
        assert_eq!(descriptor["publicHost"], host);
        assert!((1..=64).contains(&company.len()));
        assert!((1..=12).contains(&instance.len()));

        let proxy = format!("http://127.0.0.1:{fixture_port}");
        let env_restore = EnvRestore::install(&[
            ("HTTP_PROXY", proxy.clone()),
            ("http_proxy", proxy),
            ("NO_PROXY", "127.0.0.1,localhost".into()),
            ("no_proxy", "127.0.0.1,localhost".into()),
            ("AGENTICOS_COMPANY", company.clone()),
            ("AGENTICOS_INSTANCE", instance),
            ("AGENTICOS_BOARD_HOST", host.clone()),
            ("AGENTICOS_BOARD_ISSUER", issuer.clone()),
            ("AGENTICOS_BOARD_COMPANY", company.clone()),
        ]);
        cadence_agent::board_identity::write_config(
            &state,
            &cadence_agent::board_identity::Config {
                host: host.clone(),
                issuer: issuer.clone(),
                company,
            },
        )
        .unwrap();

        let provider_env = ProviderEnv::refusing_providers();
        provider_env.set("CADENCE_PM_DIR", pm.to_string_lossy().to_string());
        provider_env.set("HOME", home.to_string_lossy().to_string());
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
        let mut opts = daemon::ServeOptions {
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
            &mut opts,
            state.join("outbox"),
            format!("http://127.0.0.1:{fixture_port}"),
        );
        let daemon_dir = state.clone();
        let daemon = std::thread::spawn(move || daemon::serve_with(&daemon_dir, opts));
        let until = Instant::now() + WAIT;
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(1)).is_err()
            || cadence_agent::test_seam::Seam::token_at(&state).is_none()
        {
            if daemon.is_finished() {
                panic!("daemon exited during startup: {:?}", daemon.join().unwrap());
            }
            assert!(Instant::now() < until, "daemon startup timeout");
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

        let (tx, rx) = std::sync::mpsc::channel();
        let board_stop = Arc::new(AtomicBool::new(false));
        let board_opts = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port: board_port,
            stop: Some(board_stop.clone()),
            startup: Some(tx),
            test_seam: true,
            allow_hosts: vec![host.clone()],
            allow_origins: vec![format!("http://{host}")],
            public: Some(cadence_agent::ui::PublicBoard {
                host: host.clone(),
                authorize_url: format!("{issuer}/v2/board/authorize"),
                issuer,
                company: descriptor["company"].as_str().unwrap().to_owned(),
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
            driver,
            daemon_stop,
            daemon: Some(daemon),
            board_stop,
            board: Some(board),
            board_port,
            host,
            _env_restore: env_restore,
        }
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("state")
    }

    fn http(&self, method: &str, path: &str, cookie: Option<&str>, body: &str) -> HttpResult {
        let mut stream = TcpStream::connect(("127.0.0.1", self.board_port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nOrigin: http://{}\r\nSec-Fetch-Site: same-origin\r\nX-Cadence-Board: 1\r\nContent-Type: application/json\r\nConnection: close\r\n{}Content-Length: {}\r\n\r\n{body}",
            self.host,
            self.host,
            cookie.map(|c| format!("Cookie: {c}\r\n")).unwrap_or_default(),
            body.len()
        )
        .unwrap();
        read_http(stream)
    }

    fn rpc(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(Asserted::Operator, || {
            client::rpc(&self.state(), method, params)
        })
    }

    fn login(&self, sub: &str) -> (u16, String, Option<String>) {
        let minted = self
            .driver
            .control("/__fixture__/signin", &json!({"sub":sub}));
        assert_eq!(minted.0, 200);
        let assertion = serde_json::from_str::<Value>(&minted.1).unwrap()["assertion"]
            .as_str()
            .unwrap()
            .to_owned();
        self.http(
            "POST",
            "/__platform/session",
            None,
            &json!({"assertion":assertion}).to_string(),
        )
    }

    fn queued_event(&self, id: &str) -> Option<Value> {
        let store = Store::open(&self.state().join("cadence.sqlite3")).unwrap();
        store
            .events_tail("master", 500)
            .unwrap()
            .into_iter()
            .find(|event| event.kind == "queued" && event.payload["message"] == id)
            .map(|event| event.payload)
    }

    fn has_message(&self, id: &str) -> bool {
        self.queued_event(id).is_some()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.board_stop.store(true, SeqCst);
        if let Some(board) = self.board.take() {
            let _ = board.join();
        }
        self.daemon_stop.store(true, SeqCst);
        if let Some(daemon) = self.daemon.take() {
            let _ = daemon.join();
        }
        self.driver.request_shutdown();
    }
}

fn free_port() -> u16 {
    (3110..3200)
        .find(|port| TcpListener::bind(("127.0.0.1", *port)).is_ok())
        .expect("free caller-owned fixture port in 3110..3199")
}

fn free_port_excluding(excluded: u16) -> u16 {
    (3110..3200)
        .filter(|port| *port != excluded)
        .find(|port| TcpListener::bind(("127.0.0.1", *port)).is_ok())
        .expect("free distinct caller-owned fixture port in 3110..3199")
}

fn read_http(mut stream: TcpStream) -> HttpResult {
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    assert!(
        response.len() <= BODY_CAP + 32 * 1024,
        "fixture/board response exceeds cap"
    );
    let text = String::from_utf8_lossy(&response).into_owned();
    let status = text
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    let cookie = text.lines().find_map(|line| {
        line.strip_prefix("Set-Cookie: ")
            .or_else(|| line.strip_prefix("set-cookie: "))
            .map(|value| value.split(';').next().unwrap().to_string())
    });
    (status, body, cookie)
}

#[test]
fn live_platform_membership_fences_existing_public_browser_authority() {
    let fx = Fx::new();
    let subject = "cad1374-paired-subject";

    // Initial owner write is a real board-cookie request and creates one
    // durable daemon queue entry under the signed-in subject.
    let (status, reply, owner_cookie) = fx.login(subject);
    assert_eq!(status, 200, "fixture owner sign-in refused: {reply}");
    let owner_cookie = owner_cookie.expect("owner browser cookie");
    let owner_token = owner_cookie.split_once('=').unwrap().1.to_owned();
    let (status, body, _) = fx.http(
        "POST",
        "/api/threads/master/messages",
        Some(&owner_cookie),
        &json!({"text":"paired owner before demotion","message":"cad1374-paired-owner-before"})
            .to_string(),
    );
    let diagnostics = if status == 200 {
        String::new()
    } else {
        let (diagnostic_status, safe_summary, _) =
            fx.driver.control("/__fixture__/diagnostics", &json!({}));
        format!("; fixture diagnostics HTTP {diagnostic_status}: {safe_summary}")
    };
    assert_eq!(
        status, 200,
        "owner write before demotion: {body}{diagnostics}"
    );
    let persisted = fx.queued_event("cad1374-paired-owner-before").unwrap();
    assert_eq!(persisted["human"]["sub"], subject);
    let (status, _, _) = fx.http(
        "POST",
        "/api/threads/master/messages",
        Some(&owner_cookie),
        &json!({"text":"forged claims","message":"cad1374-paired-forged","actor":"cad1374-paired-foreign","role":"owner","company":"foreign"}).to_string(),
    );
    assert_ne!(
        status, 200,
        "body identity fields substituted for the authenticated subject"
    );
    assert!(fx.rpc("thread_send", json!({"alias":"master","text":"forged claims","message":"cad1374-paired-forged-rpc","public_session":owner_token,"actor":"cad1374-paired-foreign","role":"owner","company":"foreign"})).is_err());
    assert!(!fx.has_message("cad1374-paired-forged"));
    assert!(!fx.has_message("cad1374-paired-forged-rpc"));

    // An unavailable proof provider is not a valid cached owner result.
    assert_eq!(
        fx.driver
            .control("/__fixture__/available", &json!({"available":false}))
            .0,
        200
    );
    let (status, _, _) = fx.http(
        "POST",
        "/api/threads/master/messages",
        Some(&owner_cookie),
        &json!({"text":"provider unavailable","message":"cad1374-paired-unavailable"}).to_string(),
    );
    assert_ne!(status, 200, "provider outage retained owner authority");
    assert!(fx.rpc("thread_send", json!({"alias":"master","text":"provider unavailable","message":"cad1374-paired-unavailable-rpc","public_session":owner_token})).is_err());
    assert!(!fx.has_message("cad1374-paired-unavailable"));
    assert!(!fx.has_message("cad1374-paired-unavailable-rpc"));
    assert_eq!(
        fx.driver
            .control("/__fixture__/available", &json!({"available":true}))
            .0,
        200
    );

    // This updates real D1, preserving a foreign-company owner row. The
    // exact same cookie must now be reduced to member, not its cached owner.
    assert_eq!(
        fx.driver
            .control("/__fixture__/role", &json!({"sub":subject,"role":"member"}))
            .0,
        200
    );
    let (status, body, _) = fx.http(
        "POST",
        "/api/threads/master/messages",
        Some(&owner_cookie),
        &json!({"text":"after demotion","message":"cad1374-paired-owner-after"}).to_string(),
    );
    assert_ne!(
        status, 200,
        "same browser cookie retained owner HTTP write after D1 demotion: {body}"
    );
    let reduced = fx
        .rpc("board_session_check", json!({"token":owner_token}))
        .unwrap();
    assert_eq!(reduced["valid"], true);
    assert_eq!(reduced["session"]["user"]["role"], "member");
    let (meta_status, reduced_meta, _) = fx.http("GET", "/api/meta", Some(&owner_cookie), "");
    assert_eq!(meta_status, 200);
    let reduced_meta: Value = serde_json::from_str(&reduced_meta).unwrap();
    assert_eq!(reduced_meta["signed_in"], true);
    assert_eq!(reduced_meta["session"]["user"]["role"], "member");
    assert!(fx.rpc("thread_send", json!({"alias":"master","text":"after demotion","message":"cad1374-paired-owner-after-rpc","public_session":owner_token})).is_err(), "direct daemon owner write survived demotion");
    assert!(!fx.has_message("cad1374-paired-owner-after"));
    assert!(!fx.has_message("cad1374-paired-owner-after-rpc"));

    // A fresh member assertion is minted from the actual D1 member row.
    let (status, body, member_cookie) = fx.login(subject);
    assert_eq!(status, 200, "member sign-in: {body}");
    let member_cookie = member_cookie.unwrap();
    let member_token = member_cookie.split_once('=').unwrap().1.to_owned();
    let member_check = fx
        .rpc("board_session_check", json!({"token":member_token}))
        .unwrap();
    assert_eq!(
        member_check["valid"], true,
        "live member cookie was not recognized"
    );
    let (status, meta, _) = fx.http("GET", "/api/meta", Some(&member_cookie), "");
    assert_eq!(status, 200, "current member metadata request: {meta}");
    assert_eq!(
        serde_json::from_str::<Value>(&meta).unwrap()["signed_in"],
        true
    );

    // Removal affects only this company's D1 row; the subject remains an
    // owner in FOREIGN_COMPANY. That cannot substitute for this realm.
    let removed = fx
        .driver
        .control("/__fixture__/role", &json!({"sub":subject,"role":null}));
    assert_eq!(removed.0, 200);
    assert_eq!(
        serde_json::from_str::<Value>(&removed.1).unwrap()["present"],
        false
    );

    let (status, body, _) = fx.http(
        "POST",
        "/api/threads/master/messages",
        Some(&member_cookie),
        &json!({"text":"removed member","message":"cad1374-paired-removed-http"}).to_string(),
    );
    assert_ne!(
        status, 200,
        "same removed-member cookie retained protected HTTP authority: {body}"
    );
    assert_ne!(
        fx.rpc("board_session_check", json!({"token":member_token}))
            .unwrap()["valid"],
        true,
        "daemon still considered removed member valid"
    );
    let (meta_status, invalid_meta, _) = fx.http("GET", "/api/meta", Some(&member_cookie), "");
    assert_eq!(
        meta_status, 401,
        "removed-cookie metadata request was not refused: {invalid_meta}"
    );
    let invalid_meta: Value = serde_json::from_str(&invalid_meta).unwrap();
    assert_eq!(
        invalid_meta["ok"], false,
        "removed-cookie metadata refusal: {invalid_meta}"
    );
    assert_eq!(
        invalid_meta["error"]["code"], "board_session_required",
        "removed-cookie metadata refusal: {invalid_meta}"
    );
    assert!(fx.rpc("thread_send", json!({"alias":"master","text":"removed member","message":"cad1374-paired-removed-rpc","public_session":member_cookie.split_once('=').unwrap().1})).is_err());
    assert!(!fx.has_message("cad1374-paired-removed-http"));
    assert!(!fx.has_message("cad1374-paired-removed-rpc"));

    // Re-promotion and foreign-company membership must not restore the old
    // cookie. A new assertion is required to establish any later authority.
    assert_eq!(
        fx.driver
            .control("/__fixture__/role", &json!({"sub":subject,"role":"owner"}))
            .0,
        200
    );
    assert!(
        fx.rpc("board_session_check", json!({"token":member_token}))
            .unwrap()["valid"]
            != true
    );
    let (status, body, _) = fx.http("GET", "/api/meta", Some(&member_cookie), "");
    assert_eq!(
        status, 401,
        "re-promoted member's revoked cookie was not refused: {body}"
    );
    let invalidated_meta: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        invalidated_meta["ok"], false,
        "revoked-cookie metadata refusal: {invalidated_meta}"
    );
    assert_eq!(
        invalidated_meta["error"]["code"], "board_session_required",
        "revoked-cookie metadata refusal: {invalidated_meta}"
    );
    assert_eq!(
        fx.driver
            .control("/__fixture__/signin", &json!({"sub":subject}))
            .0,
        200,
        "re-promoted primary membership should permit a new assertion only"
    );

    // Local operator behavior remains the standalone local path: no public
    // cookie or claimed human identity is attached to this RPC.
    fx.rpc(
        "thread_send",
        json!({"alias":"master","text":"local operator remains","message":"cad1374-paired-local"}),
    )
    .unwrap();
    assert!(fx.has_message("cad1374-paired-local"));
}
