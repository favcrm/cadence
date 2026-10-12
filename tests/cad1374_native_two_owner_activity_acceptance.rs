//! Independent CAD-1374 acceptance for two native SDK-backed owners in one company.
//! Platform assertions and D1 membership controls come from the paired Worker fixture;
//! board routes, daemon authorization, audit projection and SQLite receipts are real.
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
const OWNER_A: &str = "cad1374-paired-subject";
const OWNER_B: &str = "cad1374-paired-foreign";
const WAIT: Duration = Duration::from_secs(90);
type Http = (u16, String, Option<String>);

struct RestoreEnv(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl RestoreEnv {
    fn set(values: &[(&'static str, String)]) -> Self {
        let saved = values
            .iter()
            .map(|(key, _)| (*key, std::env::var_os(key)))
            .collect();
        for (key, value) in values {
            std::env::set_var(key, value);
        }
        Self(saved)
    }
}
impl Drop for RestoreEnv {
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
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).expect("fixture loopback");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(stream, "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Cad1374-Fixture-Token: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", self.port, self.token, body.len(), body).unwrap();
        read_http(stream)
    }
    fn stop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", self.port)) {
                let _ = write!(stream, "POST /__fixture__/shutdown HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Cad1374-Fixture-Token: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 2\r\n\r\n{{}}", self.port, self.token);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
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
            .prefix("c1374-two-owner-")
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
        let fixture_port = free_port_excluding(port);
        let host = format!("cad1374-two-owner.board.localhost:{port}");
        let token = format!("cad1374-two-owner-{}", uuid::Uuid::new_v4());
        let mut platform = Platform::start(fixture_port, &host, &ready, token);
        let deadline = Instant::now() + WAIT;
        while !ready.exists() {
            if let Some(status) = platform.child.try_wait().unwrap() {
                panic!("platform fixture exited: {status}");
            }
            assert!(
                Instant::now() < deadline,
                "platform fixture readiness timeout"
            );
            thread::sleep(Duration::from_millis(50));
        }
        let descriptor: Value = serde_json::from_slice(&std::fs::read(&ready).unwrap()).unwrap();
        assert_eq!(descriptor["publicHost"], host);
        let company = descriptor["company"].as_str().unwrap().to_owned();
        let instance = descriptor["instance"].as_str().unwrap().to_owned();
        let issuer = descriptor["publicIssuer"].as_str().unwrap().to_owned();
        let proxy = format!("http://127.0.0.1:{fixture_port}");
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
            format!("http://127.0.0.1:{fixture_port}"),
        );
        let daemon_state = state.clone();
        let daemon = thread::spawn(move || daemon::serve_with(&daemon_state, opts));
        let deadline = Instant::now() + WAIT;
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(1)).is_err()
            || cadence_agent::test_seam::Seam::token_at(&state).is_none()
        {
            if daemon.is_finished() {
                panic!("daemon exited: {:?}", daemon.join().unwrap());
            }
            assert!(Instant::now() < deadline, "daemon startup timeout");
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
                company: company.clone(),
            }),
            ..Default::default()
        };
        let (board_state, board_pm) = (state, pm);
        let board = thread::spawn(move || {
            let _ = cadence_agent::ui::serve(&board_state, &board_pm, &board_opts);
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
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(stream,"{method} {path} HTTP/1.1\r\nHost: {}\r\nOrigin: http://{}\r\nSec-Fetch-Site: same-origin\r\nX-Cadence-Board: 1\r\nContent-Type: application/json\r\nConnection: close\r\n{}Content-Length: {}\r\n\r\n{body}",self.host,self.host,cookie.map(|c|format!("Cookie: {c}\r\n")).unwrap_or_default(),body.len()).unwrap();
        read_http(stream)
    }
    fn signin(&self, sub: &str) -> String {
        let (status, body, _) = self
            .platform
            .post("/__fixture__/signin", &json!({"sub":sub}));
        assert_eq!(status, 200, "real D1 assertion producer refused sign-in");
        serde_json::from_str::<Value>(&body).unwrap()["assertion"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    fn login(&self, sub: &str) -> (String, String) {
        let assertion = self.signin(sub);
        let (status, body, cookie) = self.http(
            "POST",
            "/__platform/session",
            None,
            &json!({"assertion":assertion}).to_string(),
        );
        assert_eq!(
            status, 200,
            "real signed owner session exchange refused: {body}"
        );
        (cookie.expect("server-held board session cookie"), assertion)
    }
    fn db(&self) -> Connection {
        Connection::open_with_flags(
            self.state().join("cadence.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }
    fn rows(&self) -> Vec<(i64, Value)> {
        let db = self.db();
        let mut statement=db.prepare("SELECT seq,payload FROM events WHERE kind IN ('board_session_mutation_prepared','board_session_mutation_finished') ORDER BY seq").unwrap();
        let mapped = statement
            .query_map([], |row| {
                let payload: String = row.get(1)?;
                Ok((
                    row.get(0)?,
                    serde_json::from_str::<Value>(&payload).unwrap(),
                ))
            })
            .unwrap();
        mapped.map(Result::unwrap).collect()
    }
    fn last(&self, kind: &str) -> Value {
        let db = self.db();
        let mut statement = db
            .prepare("SELECT payload FROM events WHERE kind=? ORDER BY seq DESC LIMIT 1")
            .unwrap();
        serde_json::from_str(
            &statement
                .query_row([kind], |row| row.get::<_, String>(0))
                .unwrap(),
        )
        .unwrap()
    }
    fn receipts(&self, kind: &str) -> Vec<Value> {
        let db = self.db();
        let mut statement = db
            .prepare("SELECT payload FROM events WHERE kind=? ORDER BY seq")
            .unwrap();
        let receipts = statement
            .query_map([kind], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|row| serde_json::from_str(&row.unwrap()).unwrap())
            .collect();
        receipts
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
        self.platform.stop();
    }
}
fn free_port() -> u16 {
    (3110..3200)
        .find(|port| TcpListener::bind(("127.0.0.1", *port)).is_ok())
        .expect("isolated board port")
}
fn free_port_excluding(excluded: u16) -> u16 {
    (3110..3200)
        .filter(|port| *port != excluded)
        .find(|port| TcpListener::bind(("127.0.0.1", *port)).is_ok())
        .expect("isolated platform port")
}
fn read_http(mut stream: TcpStream) -> Http {
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
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
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    let cookie = text.lines().find_map(|line| {
        line.strip_prefix("Set-Cookie: ")
            .or_else(|| line.strip_prefix("set-cookie: "))
            .map(|value| value.split(';').next().unwrap().to_owned())
    });
    (status, body, cookie)
}

#[test]
fn two_native_owners_keep_activity_identity_and_authority_separate() {
    let fx = Fx::new();
    // Promote the fixture's second, distinct subject through its real D1 control,
    // then obtain a fresh signed assertion and board session for that owner.
    let (role_status, _, _) = fx
        .platform
        .post("/__fixture__/role", &json!({"sub":OWNER_B,"role":"owner"}));
    assert_eq!(
        role_status, 200,
        "real paired D1 control promotes the second fixture subject"
    );
    let (cookie_a, assertion_a) = fx.login(OWNER_A);
    let (cookie_b, assertion_b) = fx.login(OWNER_B);
    assert_ne!(OWNER_A, OWNER_B);
    assert!(
        cookie_a != cookie_b,
        "each native owner has a distinct named session cookie"
    );
    assert!(
        assertion_a != assertion_b,
        "each owner received an independently signed assertion"
    );
    let meta_a: Value =
        serde_json::from_str(&fx.http("GET", "/api/meta", Some(&cookie_a), "").1).unwrap();
    let meta_b: Value =
        serde_json::from_str(&fx.http("GET", "/api/meta", Some(&cookie_b), "").1).unwrap();
    let session_a = meta_a["session"]["id"]
        .as_str()
        .expect("safe owner A session id")
        .to_owned();
    let session_b = meta_b["session"]["id"]
        .as_str()
        .expect("safe owner B session id")
        .to_owned();
    assert_eq!(meta_a["session"]["user"]["role"], "operator");
    assert_eq!(meta_b["session"]["user"]["role"], "operator");
    assert_ne!(
        session_a, session_b,
        "native sessions cannot collapse to one actor session"
    );
    let rows = fx.rows();
    assert_eq!(
        rows.len(),
        4,
        "two actual signed-ins each produce prepared and finished canonical rows"
    );
    let mut request_by_owner = std::collections::BTreeMap::<String, (String, String)>::new();
    let mut physical_ids = Vec::new();
    for (seq, receipt) in &rows {
        let sub = receipt["human"]["sub"]
            .as_str()
            .expect("trusted signed actor subject")
            .to_owned();
        assert!(
            sub == OWNER_A || sub == OWNER_B,
            "receipt actor is one of the two actual signed owners"
        );
        assert_eq!(receipt["human"]["company"], fx.company);
        assert_eq!(receipt["company"], fx.company);
        assert_eq!(
            receipt["human"]["role"], "operator",
            "receipt records the captured membership snapshot"
        );
        assert_eq!(receipt["actor_kind"], "human");
        assert_eq!(receipt["action"], "board_session_opened");
        let expected_session = if sub == OWNER_A {
            &session_a
        } else {
            &session_b
        };
        assert_eq!(receipt["human"]["session"], expected_session.as_str());
        let request = receipt["request_id"]
            .as_str()
            .expect("request correlation id")
            .to_owned();
        let correlation = receipt["correlation_id"]
            .as_str()
            .expect("correlation id")
            .to_owned();
        assert_eq!(request, correlation);
        request_by_owner
            .entry(sub.clone())
            .or_insert_with(|| (request.clone(), correlation.clone()));
        let pair = request_by_owner.get(&sub).unwrap();
        assert_eq!(
            request, pair.0,
            "both lifecycle phases for one owner share the operation identity"
        );
        assert_eq!(correlation, pair.1);
        physical_ids.push(format!("event-{seq:020}"));
    }
    assert_eq!(
        request_by_owner.len(),
        2,
        "distinct immutable source actors"
    );
    assert_ne!(
        request_by_owner[OWNER_A].0, request_by_owner[OWNER_B].0,
        "owners' operations have separate request identities"
    );
    let query_path = "/api/activity?action=board_session_opened&limit=100";
    let (query_status, query_body, _) = fx.http("GET", query_path, Some(&cookie_a), "");
    assert_eq!(
        query_status, 200,
        "owner activity query authorized by signed session: {query_body}"
    );
    let query: Value = serde_json::from_str(&query_body).unwrap();
    assert_eq!(query["company"], fx.company);
    let events = query["events"]
        .as_array()
        .expect("projected activity events");
    assert_eq!(
        events.len(),
        4,
        "query returns exactly the four physical phase rows"
    );
    let mut queried_ids = events
        .iter()
        .map(|event| event["event_id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    queried_ids.sort();
    physical_ids.sort();
    assert_eq!(
        queried_ids, physical_ids,
        "query projects each canonical SQLite row exactly once"
    );
    for sub in [OWNER_A, OWNER_B] {
        let attributed = events
            .iter()
            .filter(|event| event["human"]["sub"] == sub)
            .collect::<Vec<_>>();
        assert_eq!(
            attributed.len(),
            2,
            "both phase rows remain attributed to their actual actor"
        );
        for event in attributed {
            assert_eq!(event["human"]["company"], fx.company);
            assert_eq!(event["human"]["role"], "operator");
            assert_eq!(event["receipt"]["role_basis"], "signed_session_snapshot");
            assert_eq!(event["receipt"]["request_id"], request_by_owner[sub].0);
            assert_eq!(event["receipt"]["correlation_id"], request_by_owner[sub].1);
        }
    }
    let filtered_path =
        format!("/api/activity?sub={OWNER_A}&action=board_session_opened&limit=100");
    let (filtered_status, filtered_body, _) = fx.http("GET", &filtered_path, Some(&cookie_b), "");
    assert_eq!(
        filtered_status, 200,
        "owner B may query owner A's activity within the shared company"
    );
    let filtered: Value = serde_json::from_str(&filtered_body).unwrap();
    let actor_a = filtered["events"].as_array().unwrap();
    assert_eq!(
        actor_a.len(),
        2,
        "sub filter selects the acting owner A, not owner B or target"
    );
    assert!(actor_a
        .iter()
        .all(|event| event["human"]["sub"] == OWNER_A && event["human"]["role"] == "operator"));
    let own_query_path =
        format!("/api/activity?sub={OWNER_B}&action=board_session_opened&limit=100");
    let (own_query_status, own_query_body, _) =
        fx.http("GET", &own_query_path, Some(&cookie_b), "");
    assert_eq!(own_query_status, 200, "owner B can query its own activity");
    let own_query: Value = serde_json::from_str(&own_query_body).unwrap();
    assert_eq!(own_query["events"].as_array().unwrap().len(), 2);
    assert!(own_query["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["human"]["sub"] == OWNER_B));
    let export_path =
        format!("/api/activity/export?sub={OWNER_A}&action=board_session_opened&limit=100");
    let (export_status, export_body, _) = fx.http("GET", &export_path, Some(&cookie_b), "");
    assert_eq!(
        export_status, 200,
        "owner B may export owner A's activity in the shared company"
    );
    let export: Value = serde_json::from_str(&export_body).unwrap();
    assert_eq!(export["company"], fx.company);
    assert_eq!(export["events"].as_array().unwrap().len(), 2);
    assert!(export["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["human"]["sub"] == OWNER_A && event["human"]["role"] == "operator"));
    let own_export_path =
        format!("/api/activity/export?sub={OWNER_B}&action=board_session_opened&limit=100");
    let (own_export_status, own_export_body, _) =
        fx.http("GET", &own_export_path, Some(&cookie_b), "");
    assert_eq!(
        own_export_status, 200,
        "owner B can export its own activity"
    );
    let own_export: Value = serde_json::from_str(&own_export_body).unwrap();
    assert_eq!(own_export["events"].as_array().unwrap().len(), 2);
    assert!(own_export["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["human"]["sub"] == OWNER_B));
    let export_receipt = fx.last("activity_export_prepared");
    assert_eq!(
        export_receipt["human"]["sub"], OWNER_B,
        "export intent attributes the authenticated exporter, not filtered subject"
    );
    assert_eq!(export_receipt["human"]["company"], fx.company);
    assert_eq!(export_receipt["human"]["role"], "operator");

    // Demotion updates actual paired D1 membership. The retained A cookie must
    // lose access, while B remains an owner and can still read A's old snapshot.
    let (demote_status, _, _) = fx
        .platform
        .post("/__fixture__/role", &json!({"sub":OWNER_A,"role":"member"}));
    assert_eq!(demote_status, 200, "real paired D1 control demotes owner A");
    for path in [
        query_path.to_owned(),
        "/api/activity/export?action=board_session_opened&limit=100".to_owned(),
    ] {
        let (status, body, _) = fx.http("GET", &path, Some(&cookie_a), "");
        assert!(
            !(200..300).contains(&status),
            "demoted owner A session retained activity authority"
        );
        assert!(
            !body.contains(&session_a) && !body.contains(OWNER_A),
            "denial does not disclose session or activity identity"
        );
    }
    let denials = fx.receipts("activity_denied");
    assert_eq!(
        denials.len(),
        2,
        "query and export each persist the real refusal"
    );
    assert!(
        denials
            .iter()
            .all(|receipt| receipt["human"]["sub"] == OWNER_A),
        "refusal attribution stays with known demoted actor A"
    );
    assert!(denials
        .iter()
        .all(|receipt| receipt["human"]["company"] == fx.company));
    let (still_owner_status, still_owner_body, _) =
        fx.http("GET", &filtered_path, Some(&cookie_b), "");
    assert_eq!(
        still_owner_status, 200,
        "owner B remains authorized after owner A demotion: {still_owner_body}"
    );
    let still_owner: Value = serde_json::from_str(&still_owner_body).unwrap();
    let still_a = still_owner["events"].as_array().unwrap();
    assert_eq!(still_a.len(), 2);
    for event in still_a {
        assert_eq!(
            event["human"]["sub"], OWNER_A,
            "historical actor is not rewritten to current requester"
        );
        assert_eq!(
            event["human"]["role"], "operator",
            "historical verified role snapshot is immutable after demotion"
        );
        assert_eq!(event["receipt"]["request_id"], request_by_owner[OWNER_A].0);
        assert_eq!(
            event["receipt"]["correlation_id"],
            request_by_owner[OWNER_A].1
        );
    }
    let (still_export_status, still_export_body, _) =
        fx.http("GET", &export_path, Some(&cookie_b), "");
    assert_eq!(
        still_export_status, 200,
        "owner B export authority is independent of filtered actor"
    );
    let still_export: Value = serde_json::from_str(&still_export_body).unwrap();
    assert_eq!(still_export["events"].as_array().unwrap().len(), 2);
    assert!(still_export["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["human"]["sub"] == OWNER_A && event["human"]["role"] == "operator"));
    let last_export = fx.last("activity_export_prepared");
    assert_eq!(
        last_export["human"]["sub"], OWNER_B,
        "demotion does not confuse export actor with subject filter"
    );
    for body in [
        &query_body,
        &filtered_body,
        &own_query_body,
        &export_body,
        &own_export_body,
        &still_owner_body,
        &still_export_body,
    ] {
        for credential in [&cookie_a, &cookie_b, &assertion_a, &assertion_b] {
            assert!(
                !body.contains(credential),
                "response exposed a native session cookie or signed assertion"
            );
        }
    }
}
