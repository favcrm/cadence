//! Independent CAD-1374 acceptance for operator and stolen-token revocation.
//! The signed native session producer, board, daemon Auth, and SQLite receipts
//! are real; only the paired Worker/D1 identity producer is fixture-backed.
#![cfg(feature = "test-seam")]

use cadence_agent::adapter::ProviderEnv;
use cadence_agent::test_seam::{scoped, Asserted};
use cadence_agent::{client, daemon, master, operator_auth};
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
const SECOND_SUBJECT: &str = "cad1374-paired-foreign";
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
                let _ = write!(s, "POST /__fixture__/shutdown HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Cad1374-Fixture-Token: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 2\r\n\r\n{{}}", self.port, self.token);
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
            .prefix("c1374-revoke-")
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
        let host = format!("cad1374-revoke.board.localhost:{port}");
        let token = format!("cad1374-revoke-{}", uuid::Uuid::new_v4());
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
        let daemon_stop = Arc::new(AtomicBool::new(false));
        // Build the refusing provider environment without starting or calling a provider.
        let mut opts = daemon::ServeOptions {
            provider_env: ProviderEnv::refusing_providers(),
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
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_string_lossy().to_string());
        opts.provider_env
            .set("HOME", home.to_string_lossy().to_string());
        opts.provider_env.set(
            "CADENCE_PI_COMMAND",
            format!(
                "python3 {}",
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/pi-gateway.py")
                    .display()
            ),
        );
        opts.provider_env.set(master::TEST_NO_LANDLOCK, "1");
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
    fn assertion_for(&self, sub: &str) -> String {
        let (s, b, _) = self
            .platform
            .post("/__fixture__/signin", &json!({"sub":sub}));
        assert_eq!(s, 200, "real D1 owner assertion refused");
        serde_json::from_str::<Value>(&b).unwrap()["assertion"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    fn open_as(&self, sub: &str) -> (String, String) {
        let a = self.assertion_for(sub);
        let (s, b, c) = self.http(
            "POST",
            "/__platform/session",
            None,
            &json!({"assertion":a}).to_string(),
        );
        assert_eq!(s, 200, "signed native owner session refused: {b}");
        (c.expect("real HTTP session cookie"), a)
    }
    fn open(&self) -> (String, String) {
        self.open_as(SUBJECT)
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
    fn rows(&self, kind: &str) -> Vec<Value> {
        let c = self.db();
        let mut s = c
            .prepare("SELECT payload FROM events WHERE kind=? ORDER BY seq")
            .unwrap();
        let rows = s
            .query_map([kind], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
            .collect();
        rows
    }
    fn protected(&self, cookie: &str) -> u16 {
        self.http("GET", "/api/meta", Some(cookie), "").0
    }
    fn session_id(&self, cookie: &str) -> String {
        let (s, b, _) = self.http("GET", "/api/meta", Some(cookie), "");
        assert_eq!(s, 200);
        serde_json::from_str::<Value>(&b).unwrap()["session"]["id"]
            .as_str()
            .unwrap()
            .to_owned()
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
fn raw_cookie(cookie: &str) -> &str {
    cookie.split_once('=').expect("named cookie").1
}
fn rpc(state: &Path, verb: &str, params: Value) -> cadence_agent::Result<Value> {
    scoped(Asserted::Operator, || client::rpc(state, verb, params))
}
fn trigger(db: &Connection) {
    db.execute_batch("CREATE TRIGGER cad1374_reject_session_prepared BEFORE INSERT ON events WHEN NEW.kind='board_session_mutation_prepared' BEGIN SELECT RAISE(ABORT,'acceptance prepared receipt fault'); END;").unwrap();
}
fn untrigger(db: &Connection) {
    db.execute_batch("DROP TRIGGER cad1374_reject_session_prepared;")
        .unwrap();
}
fn assert_audit_pair(fx: &Fx, start: usize, action: &str, reason: &str, targets: &[(&str, &str)]) {
    let prepared = fx.rows("board_session_mutation_prepared");
    let finished = fx.rows("board_session_mutation_finished");
    let p = &prepared[start];
    let f = &finished[start];
    for r in [p, f] {
        assert_eq!(r["action"], action);
        assert_eq!(r["reason"], reason);
        assert_eq!(r["actor_kind"], "operator");
        assert!(
            r["human"].is_null(),
            "local operator performer is not the browser-session target"
        );
        assert_eq!(r["company"], fx.company);
        assert_eq!(r["agent"], Value::Null);
        assert_eq!(r["request_id"], r["correlation_id"]);
        assert_eq!(
            r["request_id"], p["request_id"],
            "prepared and terminal receipts correlate"
        );
        assert_eq!(p["outcome"], "prepared");
        assert_eq!(f["outcome"], "completed");
        let ts = r["targets"].as_array().expect("curated target snapshots");
        let mut got: Vec<(String, String)> = ts
            .iter()
            .map(|t| {
                (
                    t["session"].as_str().unwrap().to_owned(),
                    t["sub"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        got.sort();
        let mut expected: Vec<(String, String)> = targets
            .iter()
            .map(|(session, sub)| (session.to_string(), sub.to_string()))
            .collect();
        expected.sort();
        assert_eq!(
            got, expected,
            "receipt names the actual affected rows and subjects"
        );
        for t in ts {
            assert_eq!(t["company"], fx.company);
            assert!(targets.iter().any(|(_, sub)| t["sub"] == *sub));
            assert_eq!(t["role"], "operator");
            assert_eq!(t["role_basis"], "signed_session_snapshot");
        }
    }
}

#[test]
fn operator_revocation_rotation_and_stolen_token_are_durable_and_guarded() {
    let fx = Fx::new();
    let secret =
        operator_auth::read_secret(&fx.state()).expect("read own isolated fixture operator secret");
    let auth_file = fx.state().join("operator/sessions.json");
    let secret_file = fx.state().join("operator/secret");

    // Explicit safe-ID revoke: a prepared-receipt refusal leaves Auth intact,
    // then the same real guarded operation succeeds after dropping our trigger.
    let (cookie, _) = fx.open();
    let id = fx.session_id(&cookie);
    let auth_before = std::fs::read(&auth_file).unwrap();
    let p0 = fx.count("board_session_mutation_prepared");
    let f0 = fx.count("board_session_mutation_finished");
    let db = Connection::open(fx.state().join("cadence.sqlite3")).unwrap();
    trigger(&db);
    assert!(
        rpc(
            &fx.state(),
            "operator_sessions",
            json!({"secret":secret,"revoke":id})
        )
        .is_err(),
        "prepared receipt failure must refuse explicit revoke"
    );
    assert!(
        std::fs::read(&auth_file).unwrap() == auth_before,
        "refused ID revoke changed Auth bytes"
    );
    assert_eq!(fx.protected(&cookie), 200);
    assert_eq!(fx.count("board_session_mutation_prepared"), p0);
    assert_eq!(fx.count("board_session_mutation_finished"), f0);
    untrigger(&db);
    drop(db);
    let prepared_len = fx.rows("board_session_mutation_prepared").len();
    let out = rpc(
        &fx.state(),
        "operator_sessions",
        json!({"secret":secret,"revoke":id}),
    )
    .unwrap();
    assert_eq!(out["revoked"], 1);
    assert!(!(200..300).contains(&fx.protected(&cookie)));
    assert_audit_pair(
        &fx,
        prepared_len,
        "board_session_revoked",
        "operator_revoke",
        &[(&id, SUBJECT)],
    );

    // Revoking all existing sessions is independently prepared before Auth mutation.
    let (s, _b, _) = fx.platform.post(
        "/__fixture__/role",
        &json!({"sub":SECOND_SUBJECT,"role":"owner"}),
    );
    assert_eq!(s, 200, "real D1 second owner membership refused");
    let (all_cookie, _) = fx.open();
    let all_id = fx.session_id(&all_cookie);
    let (second_cookie, _) = fx.open_as(SECOND_SUBJECT);
    let second_id = fx.session_id(&second_cookie);
    assert_ne!(all_id, second_id, "revoke-all sessions must be distinct");
    let before = std::fs::read(&auth_file).unwrap();
    let db = Connection::open(fx.state().join("cadence.sqlite3")).unwrap();
    trigger(&db);
    assert!(
        rpc(
            &fx.state(),
            "operator_sessions",
            json!({"secret":secret,"revoke_all":true})
        )
        .is_err(),
        "prepared receipt failure must refuse revoke-all"
    );
    assert!(
        std::fs::read(&auth_file).unwrap() == before,
        "refused revoke-all changed Auth bytes"
    );
    assert_eq!(fx.protected(&all_cookie), 200);
    assert_eq!(fx.protected(&second_cookie), 200);
    untrigger(&db);
    drop(db);
    let prepared_len = fx.rows("board_session_mutation_prepared").len();
    let out = rpc(
        &fx.state(),
        "operator_sessions",
        json!({"secret":secret,"revoke_all":true}),
    )
    .unwrap();
    assert_eq!(out["revoked"], 2);
    assert!(!(200..300).contains(&fx.protected(&all_cookie)));
    assert!(!(200..300).contains(&fx.protected(&second_cookie)));
    assert_audit_pair(
        &fx,
        prepared_len,
        "board_session_revoked",
        "operator_revoke_all",
        &[(&all_id, SUBJECT), (&second_id, SECOND_SUBJECT)],
    );

    // Rotation prepares its target group before changing the secret or sessions.
    let (rotate_cookie, _) = fx.open();
    let rotate_id = fx.session_id(&rotate_cookie);
    let secret_before = std::fs::read(&secret_file).unwrap();
    let sessions_before = std::fs::read(&auth_file).unwrap();
    let db = Connection::open(fx.state().join("cadence.sqlite3")).unwrap();
    trigger(&db);
    assert!(
        rpc(
            &fx.state(),
            "operator_secret_rotate",
            json!({"secret":secret})
        )
        .is_err(),
        "receipt fault must refuse rotation"
    );
    assert!(
        std::fs::read(&secret_file).unwrap() == secret_before,
        "failed prepare rotated secret"
    );
    assert!(
        std::fs::read(&auth_file).unwrap() == sessions_before,
        "failed prepare revoked sessions"
    );
    assert_eq!(fx.protected(&rotate_cookie), 200);
    untrigger(&db);
    drop(db);
    let prepared_len = fx.rows("board_session_mutation_prepared").len();
    let rotated = rpc(
        &fx.state(),
        "operator_secret_rotate",
        json!({"secret":secret}),
    )
    .unwrap();
    assert_eq!(rotated["rotated"], true);
    assert_eq!(rotated["revoked"], 1);
    assert!(!(200..300).contains(&fx.protected(&rotate_cookie)));
    assert!(
        std::fs::read(&secret_file).unwrap() != secret_before,
        "successful rotate changes secret bytes"
    );
    assert_audit_pair(
        &fx,
        prepared_len,
        "board_session_revoked",
        "operator_secret_rotate",
        &[(&rotate_id, SUBJECT)],
    );
    assert!(
        rpc(&fx.state(), "operator_sessions", json!({"secret":secret})).is_err(),
        "old secret is rejected after rotation"
    );

    // Wrong secret and caller-supplied actor are rejected by the real local
    // operator gate; neither changes the secret, live cookie, nor Auth file.
    let new_secret = operator_auth::read_secret(&fx.state()).unwrap();
    let (guard_cookie, _) = fx.open();
    let guard_id = fx.session_id(&guard_cookie);
    let secret_now = std::fs::read(&secret_file).unwrap();
    let auth_now = std::fs::read(&auth_file).unwrap();
    assert!(rpc(
        &fx.state(),
        "operator_sessions",
        json!({"secret":"wrong-secret","revoke_all":true})
    )
    .is_err());
    assert!(rpc(
        &fx.state(),
        "operator_sessions",
        json!({"secret":new_secret,"revoke_all":true,"actor":"forged","company":"forged"})
    )
    .is_err());
    assert!(
        std::fs::read(&secret_file).unwrap() == secret_now,
        "guard refusal changed secret bytes"
    );
    assert!(
        std::fs::read(&auth_file).unwrap() == auth_now,
        "guard refusal changed Auth bytes"
    );
    assert_eq!(fx.protected(&guard_cookie), 200);
    let _ = guard_id;

    // Stolen-token report uses peer-derived actor identity, not its legacy
    // body label. A failed receipt preserves the token; success revokes it.
    let (stolen_cookie, _) = fx.open();
    let stolen_id = fx.session_id(&stolen_cookie);
    let stolen_token = raw_cookie(&stolen_cookie).to_owned();
    let db = Connection::open(fx.state().join("cadence.sqlite3")).unwrap();
    trigger(&db);
    assert!(
        rpc(
            &fx.state(),
            "operator_session_stolen",
            json!({"token":stolen_token,"agent":"forged-agent-label"})
        )
        .is_err(),
        "prepared receipt failure must refuse stolen-token revocation"
    );
    assert_eq!(fx.protected(&stolen_cookie), 200);
    untrigger(&db);
    drop(db);
    let prepared_len = fx.rows("board_session_mutation_prepared").len();
    let result = rpc(
        &fx.state(),
        "operator_session_stolen",
        json!({"token":stolen_token,"agent":"forged-agent-label"}),
    )
    .unwrap();
    assert_eq!(result["revoked"], true);
    assert!(!(200..300).contains(&fx.protected(&stolen_cookie)));
    let prepared = fx.rows("board_session_mutation_prepared");
    let finished = fx.rows("board_session_mutation_finished");
    for r in [&prepared[prepared_len], &finished[prepared_len]] {
        assert_eq!(r["action"], "board_session_revoked");
        assert_eq!(r["reason"], "presented_by_agent");
        assert_eq!(r["actor_kind"], "unknown");
        assert!(r["human"].is_null());
        assert_eq!(r["agent"], Value::Null);
        assert_eq!(r["company"], fx.company);
        assert_eq!(r["targets"][0]["session"], stolen_id);
        assert_eq!(r["targets"][0]["sub"], SUBJECT);
        assert_eq!(r["targets"][0]["role"], "operator");
        assert_eq!(r["request_id"], r["correlation_id"]);
        assert_eq!(
            r["request_id"], prepared[prepared_len]["request_id"],
            "stolen-token receipts share one request and correlation"
        );
    }
    assert_eq!(prepared[prepared_len]["outcome"], "prepared");
    assert_eq!(finished[prepared_len]["outcome"], "completed");
    assert!(!prepared[prepared_len]
        .to_string()
        .contains("forged-agent-label"));
    assert!(!finished[prepared_len]
        .to_string()
        .contains("forged-agent-label"));
    assert!(!prepared[prepared_len].to_string().contains(&stolen_token));
}
