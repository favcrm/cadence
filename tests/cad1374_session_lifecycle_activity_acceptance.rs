//! Independent CAD-1374 acceptance for critical hosted-session lifecycle audit.
//! Signed platform assertions, the board HTTP handlers, daemon Auth and its
//! SQLite receipt guard are real; the platform producer is the paired Worker/D1 fixture.
#![cfg(feature = "test-seam")]

use cadence_agent::adapter::ProviderEnv;
use cadence_agent::test_seam::{scoped, Asserted};
use cadence_agent::{client, daemon, master};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering::SeqCst},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use tempfile::{Builder, TempDir};

fn paired_api_dir() -> std::path::PathBuf {
    let path = std::env::var_os("CAD1374_PAIRED_API_DIR")
        .map(std::path::PathBuf::from)
        .expect("CAD1374_PAIRED_API_DIR must point to the pinned AgenticOS apps/api checkout");
    assert!(
        path.is_absolute(),
        "CAD1374_PAIRED_API_DIR must be absolute"
    );
    assert!(path.is_dir(), "CAD1374_PAIRED_API_DIR must be a directory");
    assert!(
        path.join("node_modules/.bin/vitest").is_file(),
        "CAD1374_PAIRED_API_DIR must contain the installed Vitest CLI"
    );
    path
}
const DRIVER: &str = "src/runtime/cad1374-paired-membership.fixture.test.ts";
const SUBJECT: &str = "cad1374-paired-subject";
const WAIT: Duration = Duration::from_secs(90);
type Http = (u16, String, Option<String>);

struct RestoreEnv(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl RestoreEnv {
    fn set(values: &[(&'static str, String)]) -> Self {
        let saved = values
            .iter()
            .map(|(k, _)| (*k, std::env::var_os(k)))
            .collect();
        for (k, v) in values {
            std::env::set_var(k, v);
        }
        Self(saved)
    }
}
impl Drop for RestoreEnv {
    fn drop(&mut self) {
        for (k, v) in self.0.drain(..) {
            if let Some(v) = v {
                std::env::set_var(k, v);
            } else {
                std::env::remove_var(k);
            }
        }
    }
}

struct Platform {
    child: Child,
    port: u16,
    token: String,
}
impl Platform {
    fn start(port: u16, host: &str, ready: &Path, token: String) -> Self {
        let api_dir = paired_api_dir();
        let mut command = Command::new(api_dir.join("node_modules/.bin/vitest"));
        command
            .args([
                "run",
                "--config",
                "vitest.cad1374-paired.config.ts",
                DRIVER,
                "--pool=threads",
                "--maxWorkers=1",
                "--no-file-parallelism",
            ])
            .current_dir(&api_dir)
            .env("CAD1374_FIXTURE_PORT", port.to_string())
            .env("CAD1374_FIXTURE_READY", ready)
            .env("CAD1374_PUBLIC_HOST", host)
            .env("CAD1374_FIXTURE_TOKEN", &token)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child =
            cadence_agent::reaper::spawn(&mut command).expect("start owned Worker/D1 fixture");
        Self { child, port, token }
    }
    fn post(&self, path: &str, value: &Value) -> Http {
        let body = value.to_string();
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).expect("owned fixture loopback");
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(s, "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Cad1374-Fixture-Token: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", self.port, self.token, body.len(), body).unwrap();
        read_http(s)
    }
    fn stop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            if let Ok(mut s) = TcpStream::connect(("127.0.0.1", self.port)) {
                let b = "{}";
                let _ = write!(s, "POST /__fixture__/shutdown HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Cad1374-Fixture-Token: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 2\r\n\r\n{}", self.port, self.token, b);
            }
        }
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
impl Drop for Platform {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Fx {
    root: TempDir,
    _env: RestoreEnv,
    platform: Platform,
    port: u16,
    host: String,
    company: String,
    board_stop: Arc<AtomicBool>,
    board: Option<thread::JoinHandle<()>>,
    daemon_stop: Arc<AtomicBool>,
    daemon: Option<thread::JoinHandle<cadence_agent::Result<()>>>,
}
impl Fx {
    fn new() -> Self {
        let root = Builder::new()
            .prefix("c1374-sa-")
            .tempdir_in(std::env::temp_dir())
            .unwrap();
        let state = root.path().join("state");
        let pm = root.path().join("pm");
        let home = root.path().join("home");
        let ready = root.path().join("ready.json");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        cadence_agent::issue::Pm::init(&pm).unwrap();
        let mut yaml = std::fs::read_to_string(pm.join("pm.yaml")).unwrap();
        yaml.push_str("\npi:\n  models:\n    allow: [\"fake/model-1\"]\n    default: {master: \"fake/model-1\", worker: \"fake/model-1\"}\n");
        std::fs::write(pm.join("pm.yaml"), yaml).unwrap();
        let port = free_port();
        let fport = free_port_excluding(port);
        let host = format!("cad1374-session.board.localhost:{port}");
        let token = format!("cad1374-session-{}", uuid::Uuid::new_v4());
        let mut platform = Platform::start(fport, &host, &ready, token);
        let until = Instant::now() + WAIT;
        while !ready.exists() {
            if let Some(status) = platform.child.try_wait().unwrap() {
                panic!("platform fixture exited: {status}");
            }
            assert!(Instant::now() < until, "platform fixture readiness timeout");
            thread::sleep(Duration::from_millis(50));
        }
        let d: Value = serde_json::from_slice(&std::fs::read(&ready).unwrap()).unwrap();
        assert_eq!(d["publicHost"], host);
        let company = d["company"].as_str().unwrap().to_owned();
        let instance = d["instance"].as_str().unwrap().to_owned();
        let issuer = d["publicIssuer"].as_str().unwrap().to_owned();
        let proxy = format!("http://127.0.0.1:{fport}");
        let env = RestoreEnv::set(&[
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
                company: company.clone(),
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
            format!("http://127.0.0.1:{fport}"),
        );
        let ds = state.clone();
        let daemon = thread::spawn(move || daemon::serve_with(&ds, opts));
        let until = Instant::now() + WAIT;
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(1)).is_err()
            || cadence_agent::test_seam::Seam::token_at(&state).is_none()
        {
            if daemon.is_finished() {
                panic!("daemon exited: {:?}", daemon.join().unwrap());
            }
            assert!(Instant::now() < until, "daemon startup timeout");
            thread::sleep(Duration::from_millis(50));
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
        let bo = cadence_agent::ui::ServeOpts {
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
                issuer,
                company: company.clone(),
            }),
            ..Default::default()
        };
        let (dir, bpm) = (state, pm);
        let board = thread::spawn(move || {
            let _ = cadence_agent::ui::serve(&dir, &bpm, &bo);
        });
        rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
        Self {
            root,
            _env: env,
            platform,
            port,
            host,
            company,
            board_stop,
            board: Some(board),
            daemon_stop,
            daemon: Some(daemon),
        }
    }
    fn state(&self) -> PathBuf {
        self.root.path().join("state")
    }
    fn http(&self, method: &str, path: &str, cookie: Option<&str>, body: &str) -> Http {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(s,"{method} {path} HTTP/1.1\r\nHost: {}\r\nOrigin: http://{}\r\nSec-Fetch-Site: same-origin\r\nX-Cadence-Board: 1\r\nContent-Type: application/json\r\nConnection: close\r\n{}Content-Length: {}\r\n\r\n{body}",self.host,self.host,cookie.map(|c|format!("Cookie: {c}\r\n")).unwrap_or_default(),body.len()).unwrap();
        read_http(s)
    }
    fn assertion(&self) -> String {
        let (status, b, _) = self
            .platform
            .post("/__fixture__/signin", &json!({"sub":SUBJECT}));
        assert_eq!(status, 200, "real D1 owner assertion producer refused");
        serde_json::from_str::<Value>(&b).unwrap()["assertion"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    fn open(&self, assertion: &str) -> Http {
        self.http(
            "POST",
            "/__platform/session",
            None,
            &json!({"assertion":assertion}).to_string(),
        )
    }
    fn db(&self) -> Connection {
        Connection::open_with_flags(
            self.state().join("cadence.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }
    fn count(&self, kind: &str) -> i64 {
        self.db()
            .query_row("SELECT count(*) FROM events WHERE kind=?", [kind], |r| {
                r.get(0)
            })
            .unwrap()
    }
    fn last(&self, kind: &str) -> Value {
        let c = self.db();
        let mut s = c
            .prepare("SELECT payload FROM events WHERE kind=? ORDER BY seq DESC LIMIT 1")
            .unwrap();
        let t: String = s.query_row([kind], |r| r.get(0)).unwrap();
        serde_json::from_str(&t).unwrap()
    }
    fn protected(&self, cookie: &str) -> u16 {
        self.http("GET", "/api/meta", Some(cookie), "").0
    }
}
impl Drop for Fx {
    fn drop(&mut self) {
        self.board_stop.store(true, SeqCst);
        if let Some(h) = self.board.take() {
            let _ = h.join();
        }
        self.daemon_stop.store(true, SeqCst);
        if let Some(h) = self.daemon.take() {
            let _ = h.join();
        }
        self.platform.stop();
    }
}
fn free_port() -> u16 {
    (3110..3200)
        .find(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("isolated board port")
}
fn free_port_excluding(x: u16) -> u16 {
    (3110..3200)
        .filter(|p| *p != x)
        .find(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("isolated platform port")
}
fn assert_lifecycle_contract(receipt: &Value, trusted_company: &str) {
    assert_eq!(
        receipt["schema_version"], 1,
        "lifecycle receipt schema version"
    );
    assert_eq!(
        receipt["company"], trusted_company,
        "tenant scope comes from trusted board configuration"
    );
    assert_eq!(
        receipt["actor_kind"], "human",
        "verified browser actor is recorded as human"
    );
    let at = receipt["at"]
        .as_f64()
        .expect("numeric UTC epoch on lifecycle receipt");
    assert!(
        at.is_finite() && at > 0.0,
        "lifecycle receipt time is a finite UTC epoch"
    );
    assert!(
        receipt
            .as_object()
            .is_some_and(|o| o.contains_key("revision_or_digest")),
        "nullable revision field is explicit"
    );
    assert!(
        receipt["revision_or_digest"].is_null(),
        "session lifecycle has no state revision"
    );
    assert!(
        receipt
            .as_object()
            .is_some_and(|o| o.contains_key("causation_id")),
        "causation field is explicit"
    );
    assert_eq!(
        receipt["causation_id"],
        Value::Null,
        "lifecycle has no parent action"
    );
    assert_eq!(
        receipt["human"]["company"], trusted_company,
        "company comes from trusted board configuration"
    );
    assert_eq!(
        receipt["human"]["sub"], SUBJECT,
        "actor subject comes from the signed platform assertion"
    );
    let request = receipt["request_id"].as_str().expect("server request UUID");
    let correlation = receipt["correlation_id"]
        .as_str()
        .expect("server correlation UUID");
    for value in [request, correlation] {
        let id = uuid::Uuid::parse_str(value).expect("server lifecycle identifier is a UUID");
        assert_eq!(
            id.to_string(),
            value,
            "lifecycle UUID is canonical lowercase hyphenated form"
        );
    }
    assert_eq!(
        request, correlation,
        "request and correlation share the canonical server UUID"
    );
}

fn read_http(mut s: TcpStream) -> Http {
    let mut bytes = Vec::new();
    s.read_to_end(&mut bytes).unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
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
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    let cookie = text.lines().find_map(|l| {
        l.strip_prefix("Set-Cookie: ")
            .or_else(|| l.strip_prefix("set-cookie: "))
            .map(|s| s.split(';').next().unwrap().to_owned())
    });
    (status, body, cookie)
}

#[test]
fn critical_hosted_session_open_and_logout_fail_closed_on_missing_prepared_receipt() {
    let fx = Fx::new();
    let auth_file = fx.state().join("operator/sessions.json");
    let initial_assertion = fx.assertion();
    let (status, body, initial_cookie) = fx.open(&initial_assertion);
    assert_eq!(
        status, 200,
        "real signed owner session exchange refused: {body}"
    );
    let initial_cookie = initial_cookie.expect("HTTP session cookie");
    let initial_meta: Value =
        serde_json::from_str(&fx.http("GET", "/api/meta", Some(&initial_cookie), "").1).unwrap();
    let initial_view = initial_meta["session"].clone();
    assert!(
        initial_view["id"].as_str().is_some(),
        "safe session view exposes session id"
    );
    assert!(
        fx.protected(&initial_cookie) == 200,
        "initial browser session can access protected owner read"
    );

    // A fault limited to prepared lifecycle inserts must not spend the JTI,
    // alter the actual Auth persistence, or invalidate the existing session.
    let before =
        std::fs::read(&auth_file).expect("Auth constructor's actual sessions persistence file");
    let prep_before = fx.count("board_session_mutation_prepared");
    let finish_before = fx.count("board_session_mutation_finished");
    let db = Connection::open(fx.state().join("cadence.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER cad1374_reject_session_prepared BEFORE INSERT ON events WHEN NEW.kind='board_session_mutation_prepared' BEGIN SELECT RAISE(ABORT,'acceptance prepared receipt fault'); END;").unwrap();
    let fresh = fx.assertion();
    let (status, _, cookie) = fx.open(&fresh);
    assert!(
        !(200..300).contains(&status),
        "critical public session opened without prepared receipt"
    );
    assert!(
        cookie.is_none(),
        "refused session exchange set a browser credential"
    );
    assert!(
        std::fs::read(&auth_file).unwrap() == before,
        "failed open changed Auth persistence bytes"
    );
    assert_eq!(
        fx.count("board_session_mutation_prepared"),
        prep_before,
        "failed open inserted prepared receipt"
    );
    assert_eq!(
        fx.count("board_session_mutation_finished"),
        finish_before,
        "failed open inserted terminal receipt"
    );
    assert_eq!(
        fx.protected(&initial_cookie),
        200,
        "receipt failure invalidated existing session"
    );
    // The direct daemon peer is subject to the same durable receipt rule;
    // this uses the existing authenticated local RPC client, not a fake guard.
    let direct_before = std::fs::read(&auth_file).unwrap();
    let direct = scoped(Asserted::Operator, || {
        client::rpc(
            &fx.state(),
            "board_session_open",
            json!({"assertion":fresh}),
        )
    });
    assert!(
        direct.is_err(),
        "direct daemon RPC opened a session without its prepared receipt"
    );
    assert!(
        std::fs::read(&auth_file).unwrap() == direct_before,
        "failed direct RPC changed Auth persistence bytes"
    );
    assert_eq!(
        fx.count("board_session_mutation_prepared"),
        prep_before,
        "failed direct RPC inserted prepared receipt"
    );
    db.execute_batch("DROP TRIGGER cad1374_reject_session_prepared;")
        .unwrap();
    drop(db);

    // Retrying the very same platform JTI is the no-consumption control.
    let (status, body, new_cookie) = fx.open(&fresh);
    assert_eq!(
        status, 200,
        "same genuine assertion was consumed by refused attempt: {body}"
    );
    assert_eq!(
        fx.count("board_session_mutation_prepared"),
        prep_before + 1,
        "successful open has one durable prepared receipt"
    );
    assert_eq!(
        fx.count("board_session_mutation_finished"),
        finish_before + 1,
        "successful open has one terminal receipt"
    );
    let new_cookie = new_cookie.expect("successful exchange sets session cookie");
    assert_eq!(
        fx.protected(&new_cookie),
        200,
        "new owner session is usable"
    );
    let (meta_status, new_meta, _) = fx.http("GET", "/api/meta", Some(&new_cookie), "");
    assert_eq!(meta_status, 200, "new session metadata read");
    let new_view: Value = serde_json::from_str::<Value>(&new_meta).unwrap()["session"].clone();
    assert_ne!(
        new_view["id"], initial_view["id"],
        "replacement has a distinct safe session reference"
    );
    assert_ne!(
        fx.protected(&initial_cookie),
        200,
        "successful replacement leaves previous public subject session live"
    );
    let opened = fx.last("board_session_mutation_prepared");
    let opened_finished = fx.last("board_session_mutation_finished");
    assert_lifecycle_contract(&opened, &fx.company);
    assert_lifecycle_contract(&opened_finished, &fx.company);
    assert_eq!(opened["action"], "board_session_opened");
    assert_eq!(opened_finished["action"], "board_session_opened");
    assert_eq!(opened["human"]["sub"], SUBJECT);
    assert_eq!(opened["human"]["company"], fx.company);
    assert_eq!(opened["human"]["session"], new_view["id"]);
    assert_eq!(opened_finished["human"]["session"], new_view["id"]);
    assert_eq!(opened_finished["request_id"], opened["request_id"]);
    assert_eq!(opened_finished["correlation_id"], opened["correlation_id"]);
    let request = opened["request_id"].as_str().expect("server request UUID");
    let correlation = opened["correlation_id"]
        .as_str()
        .expect("server correlation UUID");
    assert_eq!(
        request, correlation,
        "server request and correlation IDs are canonical shared identity"
    );
    let serialized = opened.to_string();
    assert!(!serialized.contains(&initial_cookie));
    assert!(!serialized.contains(&new_cookie));
    assert!(!serialized.contains("assertion"));

    // A replay of a valid signed assertion is a real pre-auth refusal. Its
    // denial carries no client-named actor and no assertion/cookie material.
    let denial_before = fx.count("board_session_security_denied");
    let (replay_status, replay_body, _) = fx.open(&initial_assertion);
    assert!(
        !(200..300).contains(&replay_status),
        "replayed signed assertion opened another session"
    );
    assert_eq!(
        fx.count("board_session_security_denied"),
        denial_before + 1,
        "replay refusal has its durable pre-auth denial"
    );
    let denial = fx.last("board_session_security_denied");
    assert_eq!(denial["reason"], "assertion_replayed");
    assert!(
        denial["human"].is_null(),
        "pre-auth replay must not invent an authenticated actor"
    );
    let denial_json = denial.to_string();
    assert!(!denial_json.contains(&initial_assertion));
    assert!(!denial_json.contains(&initial_cookie));
    assert!(!replay_body.contains(&initial_assertion));

    // Logout is self-service. It remains available with only the cookie and
    // must leave the live credential untouched if its prepared receipt fails.
    let auth_before_logout = std::fs::read(&auth_file).unwrap();
    let prepared_before_logout = fx.count("board_session_mutation_prepared");
    let finished_before_logout = fx.count("board_session_mutation_finished");
    let db = Connection::open(fx.state().join("cadence.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER cad1374_reject_session_prepared BEFORE INSERT ON events WHEN NEW.kind='board_session_mutation_prepared' BEGIN SELECT RAISE(ABORT,'acceptance prepared receipt fault'); END;").unwrap();
    let (status, _, cookie) = fx.http("POST", "/api/session/logout", Some(&new_cookie), "{}");
    assert!(
        !(200..300).contains(&status),
        "logout revoked the session without a prepared receipt"
    );
    assert!(cookie.is_none(), "failed logout returned a cookie mutation");
    assert!(
        std::fs::read(&auth_file).unwrap() == auth_before_logout,
        "failed logout changed Auth state"
    );
    assert_eq!(
        fx.protected(&new_cookie),
        200,
        "failed logout revoked the live session"
    );
    assert_eq!(
        fx.count("board_session_mutation_prepared"),
        prepared_before_logout
    );
    assert_eq!(
        fx.count("board_session_mutation_finished"),
        finished_before_logout
    );
    db.execute_batch("DROP TRIGGER cad1374_reject_session_prepared;")
        .unwrap();
    drop(db);
    let prepared_count_before_success = fx.count("board_session_mutation_prepared");
    let finished_count_before_success = fx.count("board_session_mutation_finished");
    let (status, _, _) = fx.http("POST", "/api/session/logout", Some(&new_cookie), "{}");
    assert_eq!(status, 204, "cookie-only self logout refused");
    assert_ne!(
        fx.protected(&new_cookie),
        200,
        "logout left protected browser session live"
    );

    // Each operation has its own server UUID; only its own prepared/finished
    // pair shares it. A finish receipt does not imply external exactly-once.
    assert_eq!(
        fx.count("board_session_mutation_prepared"),
        prepared_count_before_success + 1
    );
    assert_eq!(
        fx.count("board_session_mutation_finished"),
        finished_count_before_success + 1
    );
    let logout_prepared = fx.last("board_session_mutation_prepared");
    let terminal = fx.last("board_session_mutation_finished");
    assert_lifecycle_contract(&logout_prepared, &fx.company);
    assert_lifecycle_contract(&terminal, &fx.company);
    assert_eq!(logout_prepared["action"], "board_session_logged_out");
    assert_eq!(terminal["action"], "board_session_logged_out");
    assert_eq!(logout_prepared["human"]["sub"], SUBJECT);
    assert_eq!(terminal["human"]["sub"], SUBJECT);
    assert_eq!(logout_prepared["human"]["company"], fx.company);
    assert_eq!(terminal["human"]["company"], fx.company);
    assert_eq!(logout_prepared["human"]["session"], new_view["id"]);
    assert_eq!(terminal["human"]["session"], new_view["id"]);
    assert_eq!(terminal["request_id"], logout_prepared["request_id"]);
    assert_eq!(
        terminal["correlation_id"],
        logout_prepared["correlation_id"]
    );
    let logout_request = logout_prepared["request_id"]
        .as_str()
        .expect("logout server request UUID");
    let logout_correlation = logout_prepared["correlation_id"]
        .as_str()
        .expect("logout server correlation UUID");
    assert_eq!(
        logout_request, logout_correlation,
        "logout request and correlation IDs share its server UUID"
    );
    assert_ne!(
        logout_request, request,
        "open and logout are distinct server operations"
    );
    let terminal_json = terminal.to_string();
    assert!(!terminal_json.contains(&initial_cookie));
    assert!(!terminal_json.contains(&new_cookie));
}
