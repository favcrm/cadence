//! Independent CAD-1374 acceptance for hosted-session activity query durability.
//! The platform assertion, board HTTP routes, daemon authorization and SQLite
//! observations are real; only the paired Worker/D1 producer is fixture-backed.
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

const API_DIR: &str = "/home/ubuntu/Project/agenticos-stack/worktrees/agenticos-v2-aos-218-browser-authority/apps/api";
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
    platform_port: u16,
    pm_dir: PathBuf,
    home_dir: PathBuf,
    host: String,
    issuer: String,
    company: String,
    board_stop: Arc<AtomicBool>,
    board: Option<thread::JoinHandle<()>>,
    daemon_stop: Arc<AtomicBool>,
    daemon: Option<thread::JoinHandle<cadence_agent::Result<()>>>,
}
impl Fx {
    fn new() -> Self {
        let root = Builder::new()
            .prefix("c1374-aq-")
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
                issuer: issuer.clone(),
                company: company.clone(),
            }),
            ..Default::default()
        };
        let (dir, bpm) = (state, pm.clone());
        let board = thread::spawn(move || {
            let _ = cadence_agent::ui::serve(&dir, &bpm, &bo);
        });
        rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
        Self {
            root,
            _env: env,
            platform,
            port,
            platform_port: fport,
            pm_dir: pm,
            home_dir: home,
            host,
            issuer,
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
    fn session_rows(&self, kind: &str) -> Vec<(i64, String, Value)> {
        let c = self.db();
        let mut s = c
            .prepare("SELECT seq,alias,payload FROM events WHERE kind=? ORDER BY seq")
            .unwrap();
        let rows = s
            .query_map([kind], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?))
            })
            .unwrap();
        rows.map(|r| {
            let (seq, alias, payload) = r.unwrap();
            (seq, alias, serde_json::from_str(&payload).unwrap())
        })
        .collect()
    }
    fn restart(&mut self) {
        self.board_stop.store(true, SeqCst);
        self.board
            .take()
            .expect("owned board handle")
            .join()
            .expect("owned board thread joined");
        self.daemon_stop.store(true, SeqCst);
        self.daemon
            .take()
            .expect("owned daemon handle")
            .join()
            .expect("owned daemon thread joined")
            .expect("owned daemon shutdown");
        let state = self.state();
        let provider_env = ProviderEnv::refusing_providers();
        provider_env.set("CADENCE_PM_DIR", self.pm_dir.to_string_lossy().to_string());
        provider_env.set("HOME", self.home_dir.to_string_lossy().to_string());
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
            format!("http://127.0.0.1:{}", self.platform_port),
        );
        let ds = state.clone();
        let daemon = thread::spawn(move || daemon::serve_with(&ds, opts));
        let until = Instant::now() + WAIT;
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(1)).is_err()
            || cadence_agent::test_seam::Seam::token_at(&state).is_none()
        {
            if daemon.is_finished() {
                panic!("restarted daemon exited: {:?}", daemon.join().unwrap());
            }
            assert!(Instant::now() < until, "restarted daemon startup timeout");
            thread::sleep(Duration::from_millis(50));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let board_stop = Arc::new(AtomicBool::new(false));
        let bo = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port: self.port,
            stop: Some(board_stop.clone()),
            startup: Some(tx),
            test_seam: true,
            allow_hosts: vec![self.host.clone()],
            allow_origins: vec![format!("http://{}", self.host)],
            public: Some(cadence_agent::ui::PublicBoard {
                host: self.host.clone(),
                authorize_url: format!("{}/v2/board/authorize", self.issuer),
                issuer: self.issuer.clone(),
                company: self.company.clone(),
            }),
            ..Default::default()
        };
        let (dir, bpm) = (state, self.pm_dir.clone());
        let board = thread::spawn(move || {
            let _ = cadence_agent::ui::serve(&dir, &bpm, &bo);
        });
        rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
        self.board_stop = board_stop;
        self.board = Some(board);
        self.daemon_stop = daemon_stop;
        self.daemon = Some(daemon);
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

#[test]
fn hosted_session_activity_query_export_and_owner_guard_are_durable_and_tenant_scoped() {
    let mut fx = Fx::new();
    let assertion = fx.assertion();
    let (status, body, cookie) = fx.open(&assertion);
    assert_eq!(
        status, 200,
        "real signed owner session exchange refused: {body}"
    );
    let cookie = cookie.expect("server-held owner session cookie");
    let meta: Value =
        serde_json::from_str(&fx.http("GET", "/api/meta", Some(&cookie), "").1).unwrap();
    let session_id = meta["session"]["id"]
        .as_str()
        .expect("safe session view id")
        .to_owned();
    assert_eq!(meta["session"]["user"]["role"], "operator");
    let prepared = fx.last("board_session_mutation_prepared");
    let finished = fx.last("board_session_mutation_finished");
    for receipt in [&prepared, &finished] {
        assert_eq!(receipt["action"], "board_session_opened");
        assert_eq!(receipt["human"]["company"], fx.company);
        assert_eq!(receipt["human"]["sub"], SUBJECT);
        assert_eq!(receipt["human"]["session"], session_id);
        assert_eq!(receipt["human"]["role"], "operator");
        assert_eq!(receipt["actor_kind"], "human");
        let at = receipt["at"].as_f64().expect("UTC event timestamp");
        assert!(at.is_finite() && at > 0.0);
    }
    assert_eq!(prepared["request_id"], finished["request_id"]);
    assert_eq!(prepared["correlation_id"], finished["correlation_id"]);
    let source_rows = fx.session_rows("board_session_mutation_prepared");
    let finish_rows = fx.session_rows("board_session_mutation_finished");
    assert_eq!(
        source_rows.len(),
        1,
        "one actual prepared frame from platform sign-in"
    );
    assert_eq!(
        finish_rows.len(),
        1,
        "one actual completed frame from platform sign-in"
    );
    assert_eq!(source_rows[0].1, "daemon");
    assert_eq!(finish_rows[0].1, "daemon");
    let event_ids = [source_rows[0].0, finish_rows[0].0].map(|seq| format!("event-{seq:020}"));
    assert_ne!(
        event_ids[0], event_ids[1],
        "physical prepared and completed receipts have distinct stable row IDs"
    );
    let times = [
        prepared["at"].as_f64().unwrap(),
        finished["at"].as_f64().unwrap(),
    ];
    let from_value = times.into_iter().fold(f64::INFINITY, f64::min) - 1.0;
    let until_value = times.into_iter().fold(f64::NEG_INFINITY, f64::max) + 1.0;
    let from = from_value.to_string();
    let until = until_value.to_string();
    let query_base = format!(
        "/api/activity?sub={SUBJECT}&action=board_session_opened&from={from}&until={until}&limit=1"
    );
    let db = Connection::open(fx.state().join("cadence.sqlite3")).unwrap();
    let alias: String = db
        .query_row(
            "SELECT alias FROM events WHERE seq=?1",
            [source_rows[0].0],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        alias, "daemon",
        "test input uses the actual canonical source alias"
    );
    let mut foreign = prepared.clone();
    let foreign_company = "co_cad1374_synthetic_foreign";
    foreign["company"] = json!(foreign_company);
    foreign["human"]["company"] = json!(foreign_company);
    for target in foreign["targets"]
        .as_array_mut()
        .expect("canonical target list")
    {
        target["company"] = json!(foreign_company);
    }
    db.execute(
        "INSERT INTO events(alias,kind,payload,at) VALUES(?1,?2,?3,?4)",
        rusqlite::params![
            "cad1374-synthetic-alias",
            "board_session_mutation_prepared",
            prepared.to_string(),
            prepared["at"].as_f64().unwrap()
        ],
    )
    .unwrap();
    let alias_seq: i64 = db
        .query_row("SELECT last_insert_rowid()", [], |r| r.get(0))
        .unwrap();
    db.execute(
        "INSERT INTO events(alias,kind,payload,at) VALUES(?1,?2,?3,?4)",
        rusqlite::params![
            alias,
            "board_session_mutation_prepared",
            foreign.to_string(),
            prepared["at"].as_f64().unwrap()
        ],
    )
    .unwrap();
    let foreign_seq: i64 = db
        .query_row("SELECT last_insert_rowid()", [], |r| r.get(0))
        .unwrap();
    drop(db);
    let synthetic_ids = [alias_seq, foreign_seq].map(|seq| format!("event-{seq:020}"));

    let (query_status, query_body, _) = fx.http("GET", &query_base, Some(&cookie), "");
    assert_eq!(query_status, 200, "owner activity query: {query_body}");
    let page: Value = serde_json::from_str(&query_body).unwrap();
    assert_eq!(page["schema_version"], 1);
    assert_eq!(page["company"], fx.company);
    assert_eq!(page["interval"]["from"], json!(from_value));
    assert_eq!(page["interval"]["until"], json!(until_value));
    assert_eq!(page["row_count"], 1);
    assert_eq!(
        page["complete"], false,
        "first of two matching receipt rows is incomplete"
    );
    assert_eq!(
        page["events"][0]["human"]["sub"], SUBJECT,
        "sub filters the acting human, not the target"
    );
    assert_eq!(
        page["events"][0]["human"]["role"], "operator",
        "role is the captured verified snapshot"
    );
    assert_eq!(
        page["events"][0]["receipt"]["role_basis"],
        "signed_session_snapshot"
    );
    assert_eq!(
        page["events"][0]["receipt"]["request_id"],
        prepared["request_id"]
    );
    assert_eq!(
        page["events"][0]["receipt"]["correlation_id"],
        prepared["correlation_id"]
    );
    assert_eq!(prepared["correlation_id"], prepared["request_id"]);
    let cursor = page["next_cursor"]
        .as_str()
        .expect("continuation cursor for prepared and finished rows");
    let page2_path = format!("{query_base}&cursor={cursor}");
    let (query2_status, query2_body, _) = fx.http("GET", &page2_path, Some(&cookie), "");
    assert_eq!(query2_status, 200, "activity cursor page: {query2_body}");
    let page2: Value = serde_json::from_str(&query2_body).unwrap();
    assert_eq!(
        page2["interval"], page["interval"],
        "cursor holds the captured UTC interval"
    );
    assert_eq!(page2["row_count"], 1);
    assert_eq!(page2["complete"], true);
    assert!(page2["next_cursor"].is_null());
    assert_eq!(page2["events"][0]["human"]["sub"], SUBJECT);
    assert_eq!(page2["events"][0]["human"]["session"], session_id);
    assert_eq!(
        page2["events"][0]["receipt"]["request_id"],
        prepared["request_id"]
    );
    assert_eq!(
        page2["events"][0]["receipt"]["correlation_id"],
        prepared["correlation_id"]
    );
    let target_filter=format!("/api/activity?sub={session_id}&action=board_session_opened&from={from}&until={until}&limit=1");
    let (target_status, target_body, _) = fx.http("GET", &target_filter, Some(&cookie), "");
    assert_eq!(
        target_status, 200,
        "subject filter against session target: {target_body}"
    );
    let target_page: Value = serde_json::from_str(&target_body).unwrap();
    assert_eq!(
        target_page["row_count"], 0,
        "subject filter matches actor, never the target session id"
    );
    assert_eq!(target_page["complete"], true);
    let mut queried_ids = vec![
        page["events"][0]["event_id"].as_str().unwrap().to_owned(),
        page2["events"][0]["event_id"].as_str().unwrap().to_owned(),
    ];
    assert_ne!(queried_ids[0], queried_ids[1]);
    queried_ids.sort();
    let mut expected_ids = event_ids.to_vec();
    expected_ids.sort();
    assert_eq!(
        queried_ids, expected_ids,
        "all canonical phase rows appear once and only once"
    );
    for id in &synthetic_ids {
        assert!(
            !queried_ids.contains(id),
            "noncanonical alias or foreign-company event crossed projection"
        );
    }
    assert_eq!(page["events"][0]["target"], format!("session:{session_id}"));

    let export_base=format!("/api/activity/export?sub={SUBJECT}&action=board_session_opened&from={from}&until={until}&limit=1");
    let (export_status, export_body, _) = fx.http("GET", &export_base, Some(&cookie), "");
    assert_eq!(export_status, 200, "owner activity export: {export_body}");
    let export: Value = serde_json::from_str(&export_body).unwrap();
    assert_eq!(export["schema_version"], 1);
    assert_eq!(export["company"], fx.company);
    assert_eq!(export["interval"]["from"], json!(from_value));
    assert_eq!(export["interval"]["until"], json!(until_value));
    assert_eq!(export["row_count"], 1);
    assert_eq!(export["complete"], false);
    let export_cursor = export["next_cursor"]
        .as_str()
        .expect("export pagination cursor");
    let export2_path = format!("{export_base}&cursor={export_cursor}");
    let (export2_status, export2_body, _) = fx.http("GET", &export2_path, Some(&cookie), "");
    assert_eq!(export2_status, 200, "second export page: {export2_body}");
    let export2: Value = serde_json::from_str(&export2_body).unwrap();
    assert_eq!(export2["interval"], export["interval"]);
    assert_eq!(export2["row_count"], 1);
    assert_eq!(export2["complete"], true);
    assert!(export2["next_cursor"].is_null());
    let export_ids = [
        export["events"][0]["event_id"].as_str().unwrap(),
        export2["events"][0]["event_id"].as_str().unwrap(),
    ];
    assert_eq!(export_ids[0], queried_ids[0]);
    assert_eq!(export_ids[1], queried_ids[1]);
    let export_receipt = fx.last("activity_export_prepared");
    assert_eq!(export_receipt["operation"], "activity_export");
    assert_eq!(export_receipt["outcome"], "prepared");
    assert_eq!(export_receipt["company"], fx.company);
    assert_eq!(export_receipt["human"]["sub"], SUBJECT);
    assert_eq!(export_receipt["interval"]["from"], json!(from_value));
    assert_eq!(export_receipt["interval"]["until"], json!(until_value));
    assert_eq!(
        export_receipt["row_count"], 1,
        "prepared intent describes the actual second page"
    );
    for body in [&query_body, &query2_body, &export_body, &export2_body] {
        assert!(!body.contains(&cookie), "export included session cookie");
        assert!(
            !body.contains(&assertion),
            "export included signed assertion"
        );
        let doc: Value = serde_json::from_str(body).unwrap();
        for event in doc["events"].as_array().unwrap() {
            let rendered = event.to_string().to_ascii_lowercase();
            for field in [
                "token",
                "assertion",
                "client_payload",
                "user_agent",
                "credential_sha",
                "jti",
            ] {
                assert!(
                    event.get(field).is_none() && event["receipt"].get(field).is_none(),
                    "projected event contains forbidden field {field}"
                );
            }
            assert!(!rendered.contains(&assertion.to_ascii_lowercase()));
        }
    }

    // The same public cookie and query remain valid after actual owned board
    // and daemon threads stop and restart over the unchanged state directory.
    for path in [&query_base[..], &page2_path[..]] {
        let (status, body, _) = fx.http("GET", path, Some(&cookie), "");
        assert_eq!(status, 200, "pre-restart page was available: {body}");
    }
    fx.restart();
    let (meta_status, meta_after, _) = fx.http("GET", "/api/meta", Some(&cookie), "");
    assert_eq!(
        meta_status, 200,
        "same owner cookie remains valid after daemon/Auth reload"
    );
    let meta_after: Value = serde_json::from_str(&meta_after).unwrap();
    assert_eq!(
        meta_after["session"]["id"], session_id,
        "safe session reference survives restart"
    );
    let (restart1_status, restart1_body, _) = fx.http("GET", &query_base, Some(&cookie), "");
    assert_eq!(restart1_status, 200, "query after restart: {restart1_body}");
    let restart1: Value = serde_json::from_str(&restart1_body).unwrap();
    let restart_cursor = restart1["next_cursor"]
        .as_str()
        .expect("same persisted page continues after restart");
    assert_eq!(
        restart1["events"][0]["event_id"],
        page["events"][0]["event_id"]
    );
    let restart2_path = format!("{query_base}&cursor={restart_cursor}");
    let (restart2_status, restart2_body, _) = fx.http("GET", &restart2_path, Some(&cookie), "");
    assert_eq!(
        restart2_status, 200,
        "continued query after restart: {restart2_body}"
    );
    let restart2: Value = serde_json::from_str(&restart2_body).unwrap();
    assert_eq!(
        restart2["events"][0]["event_id"],
        page2["events"][0]["event_id"]
    );

    for path in [&query_base[..], &export_base[..]] {
        let (forged, body, _) = fx.http(
            "GET",
            &format!("{path}&company=co_forged"),
            Some(&cookie),
            "",
        );
        assert_eq!(
            forged, 400,
            "HTTP allowlist refused caller-selected company: {body}"
        );
    }
    let bearer = cookie
        .split_once('=')
        .expect("named board session cookie")
        .1;
    for method in ["activity_query", "activity_export"] {
        let result = scoped(Asserted::Operator, || {
            client::rpc(
                &fx.state(),
                method,
                json!({
                    "sub":SUBJECT,"action":"board_session_opened","from":from_value,"until":until_value,
                    "public_session":bearer,"company":"co_forged","actor":"forged-user"
                }),
            )
        });
        assert!(
            result.is_err(),
            "daemon accepted caller-named company/actor on {method}"
        );
    }
    // The paired Worker/D1 control demotes the actual owner while preserving
    // the already-open browser cookie. Both HTTP paths must refuse and audit.
    let denials_before = fx.count("activity_denied");
    let (role_status, role_body, _) = fx
        .platform
        .post("/__fixture__/role", &json!({"sub":SUBJECT,"role":"member"}));
    assert_eq!(
        role_status, 200,
        "paired D1 owner-to-member control: {role_body}"
    );
    for path in [&query_base[..], &export_base[..]] {
        let (denied, reply, _) = fx.http("GET", path, Some(&cookie), "");
        assert!(
            !(200..300).contains(&denied),
            "demoted session accessed {path}: {reply}"
        );
        assert!(
            !reply.contains(&queried_ids[0]) && !reply.contains(&queried_ids[1]),
            "demoted refusal disclosed a receipt"
        );
    }
    assert_eq!(
        fx.count("activity_denied"),
        denials_before + 2,
        "both real owner guards durably record refusal"
    );
    let last_denial = fx.last("activity_denied");
    assert_eq!(last_denial["outcome"], "denied");
    assert_eq!(
        last_denial["human"]["sub"], SUBJECT,
        "known demoted actor remains attributed without authority"
    );
    assert_eq!(last_denial["human"]["company"], fx.company);
}
