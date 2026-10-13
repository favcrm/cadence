//! Independent CAD-1374 focused acceptance for hosted app decisions.
//! Uses the real board/daemon and the existing real Worker/D1 membership
//! producer; no product helper or membership callback is substituted.
#![cfg(feature = "test-seam")]

use cadence_agent::adapter::ProviderEnv;
use cadence_agent::test_seam::{scoped, Asserted};
use cadence_agent::{client, daemon, master, store::Store};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, Instant};
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
const DRIVER_TEST: &str = "src/runtime/cad1374-paired-membership.fixture.test.ts";
const SUBJECT: &str = "cad1374-paired-subject";
const WAIT: Duration = Duration::from_secs(90);
type HttpResult = (u16, String, Option<String>);

struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl EnvRestore {
    fn install(values: &[(&'static str, String)]) -> Self {
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
                DRIVER_TEST,
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
            cadence_agent::reaper::spawn(&mut command).expect("start owned real Worker/D1 fixture");
        Self { child, port, token }
    }
    fn post(&self, path: &str, body: &Value) -> HttpResult {
        let bytes = body.to_string();
        let mut stream =
            TcpStream::connect(("127.0.0.1", self.port)).expect("owned fixture loopback");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(stream, "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Cad1374-Fixture-Token: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", self.port, self.token, bytes.len(), bytes).unwrap();
        read_http(stream)
    }
    fn stop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            // Shutdown is cleanup, not a fixture assertion: the server may
            // already have closed its listener while its owned child exits.
            if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", self.port)) {
                let body = "{}";
                let _ = write!(stream, "POST /__fixture__/shutdown HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Cad1374-Fixture-Token: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", self.port, self.token, body.len(), body);
            }
        }
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
    _env: EnvRestore,
    platform: Platform,
    board_port: u16,
    host: String,
    company: String,
    board_stop: Arc<AtomicBool>,
    board: Option<std::thread::JoinHandle<()>>,
    daemon_stop: Arc<AtomicBool>,
    daemon: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    passive_workers: Vec<Child>,
}
impl Fx {
    fn new() -> Self {
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let root = Builder::new()
            .prefix("cad1374-app-")
            .tempdir_in(base)
            .unwrap();
        let state = root.path().join("state");
        let pm = root.path().join("pm");
        let home = root.path().join("home");
        let ready = root.path().join("platform-ready.json");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        cadence_agent::issue::Pm::init(&pm).unwrap();
        let mut yaml = std::fs::read_to_string(pm.join("pm.yaml")).unwrap();
        yaml.push_str("\npi:\n  models:\n    allow: [\"fake/model-1\"]\n    default: {master: \"fake/model-1\", worker: \"fake/model-1\"}\n");
        std::fs::write(pm.join("pm.yaml"), yaml).unwrap();
        let board_port = free_port();
        let fixture_port = free_port_excluding(board_port);
        let host = format!("cad1374-app.board.localhost:{board_port}");
        let token = format!("cad1374-app-{}", uuid::Uuid::new_v4());
        let mut platform = Platform::start(fixture_port, &host, &ready, token);
        let until = Instant::now() + WAIT;
        while !ready.exists() {
            if let Some(status) = platform.child.try_wait().unwrap() {
                panic!("platform fixture exited before ready: {status}");
            }
            assert!(
                Instant::now() < until,
                "real platform fixture readiness timeout"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let descriptor: Value = serde_json::from_slice(&std::fs::read(&ready).unwrap()).unwrap();
        assert_eq!(descriptor["publicHost"], host);
        let company = descriptor["company"].as_str().unwrap().to_owned();
        let instance = descriptor["instance"].as_str().unwrap().to_owned();
        let issuer = descriptor["publicIssuer"].as_str().unwrap().to_owned();
        let proxy = format!("http://127.0.0.1:{fixture_port}");
        let env = EnvRestore::install(&[
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
        let daemon = std::thread::spawn(move || daemon::serve_with(&daemon_state, opts));
        let deadline = Instant::now() + WAIT;
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(1)).is_err()
            || cadence_agent::test_seam::Seam::token_at(&state).is_none()
        {
            if daemon.is_finished() {
                panic!("daemon exited during startup: {:?}", daemon.join().unwrap());
            }
            assert!(Instant::now() < deadline, "daemon startup timeout");
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
                company: company.clone(),
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
            platform,
            board_port,
            host,
            company,
            board_stop,
            board: Some(board),
            daemon_stop,
            daemon: Some(daemon),
            passive_workers: Vec::new(),
        }
    }
    fn state(&self) -> PathBuf {
        self.root.path().join("state")
    }
    fn spawn_passive_worker(&mut self) -> u32 {
        let mut command = Command::new("/bin/sleep");
        command
            .arg("300")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cadence_agent::reaper::spawn(&mut command)
            .expect("start owned passive worker identity fixture");
        let pid = child.id();
        self.passive_workers.push(child);
        pid
    }
    fn stop_passive_workers(&mut self) {
        for mut child in self.passive_workers.drain(..) {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
    fn install_profiles(&self) {
        let pm_dir = self.root.path().join("pm");
        for (key, value) in [
            ("user.name", "CAD-1374 fixture"),
            ("user.email", "cad1374-fixture@invalid"),
        ] {
            let mut command = Command::new("git");
            command
                .arg("-C")
                .arg(&pm_dir)
                .args(["config", "--local", key, value]);
            let status = cadence_agent::reaper::status(&mut command)
                .expect("configure synthetic PM repository identity");
            assert!(status.success(), "configure local PM git {key}");
        }
        let pm = cadence_agent::issue::Pm::at(&pm_dir).unwrap();
        for alias in ["lead", "writer"] {
            cadence_agent::master::write_file(
                &pm,
                alias,
                "SOUL.md",
                "Synthetic acceptance fixture profile.\n",
                "operator",
            )
            .unwrap();
            cadence_agent::master::write_file(
                &pm,
                alias,
                "AGENT.md",
                "Use only the named local workflow assignment.\n",
                "operator",
            )
            .unwrap();
        }
    }
    fn event_payloads(&self, stream: &str) -> Vec<Value> {
        let conn = Connection::open_with_flags(
            self.state().join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut statement = conn
            .prepare("SELECT kind, payload FROM events WHERE alias=?1 ORDER BY seq")
            .unwrap();
        let rows = statement
            .query_map([stream], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap();
        rows.map(|row| {
            let (kind, payload) = row.expect("read event kind and payload");
            let payload: Value = serde_json::from_str(&payload).expect("event payload JSON");
            json!({"kind": kind, "payload": payload})
        })
        .collect()
    }
    fn http(&self, method: &str, path: &str, cookie: Option<&str>, body: &str) -> HttpResult {
        let mut stream = TcpStream::connect(("127.0.0.1", self.board_port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: {}\r\nOrigin: http://{}\r\nSec-Fetch-Site: same-origin\r\nX-Cadence-Board: 1\r\nContent-Type: application/json\r\nConnection: close\r\n{}Content-Length: {}\r\n\r\n{body}", self.host, self.host, cookie.map(|c| format!("Cookie: {c}\r\n")).unwrap_or_default(), body.len()).unwrap();
        read_http(stream)
    }
    fn login(&self) -> String {
        let (status, minted, _) = self
            .platform
            .post("/__fixture__/signin", &json!({"sub":SUBJECT}));
        assert_eq!(status, 200, "real D1 owner assertion: {minted}");
        let assertion = serde_json::from_str::<Value>(&minted).unwrap()["assertion"]
            .as_str()
            .unwrap()
            .to_owned();
        let (status, response, cookie) = self.http(
            "POST",
            "/__platform/session",
            None,
            &json!({"assertion":assertion}).to_string(),
        );
        assert_eq!(status, 200, "real signed session: {response}");
        cookie.expect("board session cookie")
    }
    fn op(&self, method: &str, params: Value) -> Value {
        scoped(Asserted::Operator, || {
            client::rpc(&self.state(), method, params)
        })
        .unwrap_or_else(|e| panic!("{method}: {e}"))
    }
    fn try_op(&self, method: &str, params: Value) -> Result<Value, String> {
        scoped(Asserted::Operator, || {
            client::rpc(&self.state(), method, params)
        })
        .map_err(|e| e.to_string())
    }
    fn db(&self) -> Connection {
        Connection::open(self.state().join("cadence.sqlite3")).unwrap()
    }
}
impl Drop for Fx {
    fn drop(&mut self) {
        self.board_stop.store(true, SeqCst);
        if let Some(handle) = self.board.take() {
            let _ = handle.join();
        }
        self.daemon_stop.store(true, SeqCst);
        if let Some(handle) = self.daemon.take() {
            let _ = handle.join();
        }
        self.platform.stop();
        self.stop_passive_workers();
    }
}
fn free_port() -> u16 {
    (3110..3200)
        .find(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("free isolated loopback port")
}
fn free_port_excluding(exclude: u16) -> u16 {
    (3110..3200)
        .filter(|p| *p != exclude)
        .find(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("free second isolated loopback port")
}
fn assert_canonical_receipt(
    payload: &Value,
    action: &str,
    resource: &str,
    outcome: &str,
    owner_cookie: &str,
) {
    assert_eq!(payload["schema_version"], 1);
    assert_eq!(payload["action"], action);
    assert_eq!(payload["resource"], resource);
    assert_eq!(payload["outcome"], outcome);
    assert_eq!(payload["reason"], action);
    let request_id = payload["request_id"]
        .as_str()
        .expect("canonical request UUID");
    assert!(
        uuid::Uuid::parse_str(request_id).is_ok(),
        "request_id must be a UUID: {request_id}"
    );
    assert_eq!(payload["correlation_id"], request_id);
    assert_eq!(payload["causation_id"], Value::Null);
    assert_eq!(payload["human"]["role"], "operator");
    let serialized = payload.to_string();
    assert!(
        !serialized.contains(owner_cookie),
        "audit receipt leaked owner cookie"
    );
    assert!(
        !serialized.contains("cad1374-private-input-sentinel"),
        "audit receipt leaked private input"
    );
}

fn read_http(mut stream: TcpStream) -> HttpResult {
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
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    let cookie = text.lines().find_map(|line| {
        line.strip_prefix("Set-Cookie: ")
            .or_else(|| line.strip_prefix("set-cookie: "))
            .map(|s| s.split(';').next().unwrap().to_owned())
    });
    (status, body, cookie)
}

const APP: &str = "---\napp: cad1374-decision\ntitle: Decision Acceptance\nversion: '1.0.0'\nsummary: Synthetic local workflow.\nneeds:\n  connections: []\n---\n";
const WORKFLOW: &str = "---\ntitle: Retain text\ngoal: Retain text\ninputs:\n  private: { ask: \"Private text\", optional: true }\n---\n## Write\nagent: writer\naction: local.text.produce\n\nWrite Markdown.\n\n### Acceptance\n- [ ] Markdown exists\n";

#[test]
fn hosted_app_decisions_are_attributed_and_fail_closed_before_dispatch() {
    let mut fx = Fx::new();
    let owner = fx.login();
    fx.install_profiles();
    let source = fx.root.path().join("decision-app");
    std::fs::create_dir_all(source.join("workflows")).unwrap();
    std::fs::write(source.join("app.md"), APP).unwrap();
    std::fs::write(source.join("workflows/write.md"), WORKFLOW).unwrap();
    let installed = fx.op("app_workspace_install", json!({"source":source}));
    let install_id = installed["install_id"].as_str().unwrap();
    let (status, response, _) = fx.http(
        "POST",
        &format!("/api/app-installations/{install_id}/approve"),
        Some(&owner),
        &json!({"digest":installed["digest"]}).to_string(),
    );
    assert_eq!(
        status, 200,
        "signed owner records capability consent through HTTP: {response}"
    );
    let approval: Value =
        serde_json::from_str(&response).expect("capability approval response JSON");
    assert_eq!(approval["install_id"], install_id);
    assert_eq!(approval["digest"], installed["digest"]);
    assert_eq!(approval["approved"], true);
    let consent_epoch = approval["epoch"]
        .as_i64()
        .expect("server capability approval epoch");
    let consent_events = fx.event_payloads(Store::DAEMON_STREAM);
    let consent = consent_events
        .iter()
        .find(|event| {
            event["kind"] == "app_install_capability_approved"
                && event["payload"]["install_id"] == install_id
                && event["payload"]["epoch"].as_i64() == Some(consent_epoch)
        })
        .expect("durable capability consent receipt for the HTTP approval epoch");
    assert_eq!(consent["payload"]["human"]["sub"], SUBJECT);
    assert_eq!(consent["payload"]["human"]["company"], fx.company);
    assert!(consent["payload"]["human"]["session"].as_str().is_some());
    assert_eq!(consent["payload"]["human"]["role"], "operator");
    assert_canonical_receipt(
        &consent["payload"],
        "app_install_capability_approved",
        &format!("install:{install_id}"),
        "approved",
        &owner,
    );

    // Install a real named worker assignment. The app run remains queued for
    // the actual daemon worker path; this acceptance does not fabricate a
    // capability callback or claim provider execution.
    // These owned sleepers provide distinct live process identities only;
    // they are not provider workers and do not qualify execution.
    let worker_pids = [
        ("lead", fx.spawn_passive_worker()),
        ("writer", fx.spawn_passive_worker()),
    ];
    let store = Store::open(&fx.state().join("cadence.sqlite3")).unwrap();
    for (alias, role) in [("lead", "pm"), ("writer", "worker")] {
        store
            .register_agent(&cadence_agent::store::NewAgent {
                alias,
                provider: "claude",
                endpoint_kind: "managed",
                role,
                cwd: fx.root.path().to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: Some("{\"upstream\":\"lead\"}"),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        store
            .set_identity(
                alias,
                &cadence_agent::adapter::Identity {
                    thread_id: format!("{alias}-thread"),
                    session_id: format!("{alias}-session"),
                    model: None,
                    effort: None,
                    pid: worker_pids
                        .iter()
                        .find(|(worker, _)| *worker == alias)
                        .unwrap()
                        .1,
                    endpoint: None,
                    generation: Some("g1".into()),
                    attach: None,
                },
            )
            .unwrap();
    }
    drop(store);
    let create_body = json!({"install_id":install_id,"workflow":"write","inputs":{"private":"cad1374-private-input-sentinel"},"request_id":"cad1374-created","owner_pm":"lead"});
    let (status, response, _) = fx.http(
        "POST",
        "/api/app-runs",
        Some(&owner),
        &create_body.to_string(),
    );
    assert_eq!(
        status, 200,
        "signed owner creates app run through HTTP: {response}"
    );
    let run: Value = serde_json::from_str(&response).unwrap();
    let run_id = run["id"].as_str().unwrap();
    let digest = run["snapshot_digest"].as_str().unwrap();

    // The strict HTTP DTO rejects body claims instead of letting them select
    // company, role or actor. No second run may be persisted by the attempt.
    let before = fx
        .db()
        .query_row("SELECT count(*) FROM app_runs", [], |r| r.get::<_, i64>(0))
        .unwrap();
    let forged = json!({"install_id":install_id,"workflow":"write","inputs":{},"request_id":"cad1374-forged","owner_pm":"lead","actor":"other-owner","role":"owner","company":"other-company","human":{"sub":"other-owner"}});
    let (status, body, _) = fx.http("POST", "/api/app-runs", Some(&owner), &forged.to_string());
    assert_eq!(status, 400, "forged identity fields are refused: {body}");
    assert_eq!(
        fx.db()
            .query_row("SELECT count(*) FROM app_runs", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        before
    );

    // Durable creation record must preserve the authenticated stable identity,
    // company, session reference and effective role, not request fields.
    let events = fx.event_payloads(Store::DAEMON_STREAM);
    let created = events
        .iter()
        .find(|e| e["kind"] == "app_run_created" && e["payload"]["run_id"] == run_id)
        .expect("created lifecycle receipt");
    assert_eq!(created["payload"]["human"]["sub"], SUBJECT);
    assert_eq!(created["payload"]["human"]["company"], fx.company);
    assert!(created["payload"]["human"]["session"].as_str().is_some());
    assert_eq!(created["payload"]["human"]["role"], "operator");
    assert_canonical_receipt(
        &created["payload"],
        "app_run_created",
        &format!("app-run:{run_id}"),
        "created",
        &owner,
    );

    let approve = |body: Value| {
        fx.http(
            "POST",
            &format!("/api/app-runs/{run_id}/approve"),
            Some(&owner),
            &body.to_string(),
        )
    };
    let (status, response, _) = approve(json!({"digest":digest}));
    assert_eq!(status, 200, "owner approves exact run digest: {response}");
    let (status, response, _) = fx.http(
        "POST",
        &format!("/api/app-runs/{run_id}/dispatch"),
        Some(&owner),
        "{}",
    );
    assert_eq!(
        status, 200,
        "owner dispatches approved run through real board: {response}"
    );
    let dispatched: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(dispatched["state"], "running");
    let message_id = dispatched["steps"][0]["message_id"]
        .as_str()
        .expect("queued app worker message");
    let queued_message: i64 = fx
        .db()
        .query_row(
            "SELECT count(*) FROM messages WHERE id=?1 AND source='app_run_dispatch'",
            [message_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        queued_message, 1,
        "dispatch response must identify the real queued app worker message"
    );
    let approval = fx
        .event_payloads("app-run-approvals")
        .into_iter()
        .find(|event| event["kind"] == "app_run_approved" && event["payload"]["run_id"] == run_id)
        .expect("durable approval receipt");
    assert_eq!(approval["payload"]["human"]["sub"], SUBJECT);
    assert_eq!(approval["payload"]["human"]["company"], fx.company);
    assert!(approval["payload"]["human"]["session"].as_str().is_some());
    assert_eq!(approval["payload"]["human"]["role"], "operator");
    assert_canonical_receipt(
        &approval["payload"],
        "app_run_approved",
        &format!("app-run:{run_id}"),
        "approved",
        &owner,
    );
    let dispatch = fx
        .event_payloads(Store::DAEMON_STREAM)
        .into_iter()
        .find(|event| {
            event["kind"] == "app_run_step_dispatched" && event["payload"]["run_id"] == run_id
        })
        .expect("durable dispatch receipt");
    assert_eq!(dispatch["payload"]["human"]["sub"], SUBJECT);
    assert_eq!(dispatch["payload"]["human"]["company"], fx.company);
    assert!(dispatch["payload"]["human"]["session"].as_str().is_some());
    assert_eq!(dispatch["payload"]["human"]["role"], "operator");
    let dispatch_resource = format!(
        "app-run:{run_id}/step:{}/outcome",
        dispatch["payload"]["step_id"]
            .as_str()
            .expect("dispatched step id")
    );
    assert_canonical_receipt(
        &dispatch["payload"],
        "app_run_step_dispatched",
        &dispatch_resource,
        "dispatched",
        &owner,
    );

    // Storage rejection is a real SQLite BEFORE INSERT trigger. The failed
    // decision must not change run state; removing the trigger is the control.
    let pending = fx.op("app_run_create", json!({"install_id":install_id,"workflow":"write","inputs":{},"request_id":"cad1374-trigger","owner_pm":"lead"}));
    let pending_id = pending["id"].as_str().unwrap();
    let pending_digest = pending["snapshot_digest"].as_str().unwrap();
    let conn = fx.db();
    conn.execute_batch("CREATE TRIGGER cad1374_reject_approval BEFORE INSERT ON events WHEN NEW.kind='app_run_approved' BEGIN SELECT RAISE(ABORT,'cad1374 acceptance receipt fault'); END;").unwrap();
    let (status, body, _) = fx.http(
        "POST",
        &format!("/api/app-runs/{pending_id}/approve"),
        Some(&owner),
        &json!({"digest":pending_digest}).to_string(),
    );
    assert_ne!(
        status, 200,
        "approval succeeded despite its lifecycle receipt INSERT abort: {body}"
    );
    assert_eq!(
        fx.op("app_run_show", json!({"run_id":pending_id}))["state"],
        "awaiting_approval"
    );
    conn.execute_batch("DROP TRIGGER cad1374_reject_approval;")
        .unwrap();
    drop(conn);
    let (status, body, _) = fx.http(
        "POST",
        &format!("/api/app-runs/{pending_id}/approve"),
        Some(&owner),
        &json!({"digest":pending_digest}).to_string(),
    );
    assert_eq!(
        status, 200,
        "approval positive control after trigger removal: {body}"
    );

    let cancelled_run = json!({"install_id":install_id,"workflow":"write","inputs":{},"request_id":"cad1374-cancel","owner_pm":"lead"});
    let (status, response, _) = fx.http(
        "POST",
        "/api/app-runs",
        Some(&owner),
        &cancelled_run.to_string(),
    );
    assert_eq!(status, 200, "owner creates run to cancel: {response}");
    let cancelled_id = serde_json::from_str::<Value>(&response).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, response, _) = fx.http(
        "POST",
        &format!("/api/app-runs/{cancelled_id}/cancel"),
        Some(&owner),
        "{}",
    );
    assert_eq!(status, 200, "signed owner cancels pending run: {response}");
    assert_eq!(
        fx.op("app_run_show", json!({"run_id":cancelled_id}))["state"],
        "cancelled"
    );
    let cancel = fx
        .event_payloads(Store::DAEMON_STREAM)
        .into_iter()
        .find(|event| {
            event["kind"] == "app_run_cancelled" && event["payload"]["run_id"] == cancelled_id
        })
        .expect("durable cancellation receipt");
    assert_eq!(cancel["payload"]["human"]["sub"], SUBJECT);
    assert_eq!(cancel["payload"]["human"]["company"], fx.company);
    assert!(cancel["payload"]["human"]["session"].as_str().is_some());
    assert_eq!(cancel["payload"]["human"]["role"], "operator");
    assert_canonical_receipt(
        &cancel["payload"],
        "app_run_cancelled",
        &format!("app-run:{cancelled_id}"),
        "cancelled",
        &owner,
    );

    // The actual D1 producer now demotes the same subject. The pre-existing
    // signed browser cookie may not dispatch another approved run. Membership
    // outage is separately fail-closed, without asserting queued work cancelled.
    let demotion_run = json!({"install_id":install_id,"workflow":"write","inputs":{},"request_id":"cad1374-demotion","owner_pm":"lead"});
    let (status, response, _) = fx.http(
        "POST",
        "/api/app-runs",
        Some(&owner),
        &demotion_run.to_string(),
    );
    assert_eq!(
        status, 200,
        "signed owner creates run for demotion check: {response}"
    );
    let created: Value = serde_json::from_str(&response).unwrap();
    let next_id = created["id"].as_str().expect("created run id").to_owned();
    let next_digest = created["snapshot_digest"]
        .as_str()
        .expect("created run digest")
        .to_owned();
    let (status, response, _) = fx.http(
        "POST",
        &format!("/api/app-runs/{next_id}/approve"),
        Some(&owner),
        &json!({"digest":next_digest}).to_string(),
    );
    assert_eq!(
        status, 200,
        "signed owner approves run for demotion check: {response}"
    );
    let approval = fx
        .event_payloads("app-run-approvals")
        .into_iter()
        .rfind(|event| event["kind"] == "app_run_approved" && event["payload"]["run_id"] == next_id)
        .expect("durable human approval receipt for demotion run");
    assert_eq!(approval["payload"]["human"]["sub"], SUBJECT);
    assert_eq!(approval["payload"]["human"]["company"], fx.company);
    assert!(approval["payload"]["human"]["session"].as_str().is_some());
    assert_eq!(approval["payload"]["human"]["role"], "operator");
    assert_canonical_receipt(
        &approval["payload"],
        "app_run_approved",
        &format!("app-run:{next_id}"),
        "approved",
        &owner,
    );
    assert_eq!(
        fx.platform
            .post("/__fixture__/role", &json!({"sub":SUBJECT,"role":"member"}))
            .0,
        200
    );
    let (status, body, _) = fx.http(
        "POST",
        &format!("/api/app-runs/{next_id}/dispatch"),
        Some(&owner),
        "{}",
    );
    assert_ne!(
        status, 200,
        "existing owner cookie dispatched after real D1 demotion: {body}"
    );
    assert!(
        fx.try_op("app_run_dispatch", json!({"run_id":next_id}))
            .is_err(),
        "retained-approval daemon RPC dispatched after actual D1 demotion"
    );
    assert_eq!(
        fx.op("app_run_show", json!({"run_id":next_id}))["state"],
        "approved",
        "membership failure must not cancel or poison retained approval"
    );
    let count: i64 = fx
        .db()
        .query_row(
            "SELECT count(*) FROM messages WHERE source='app_run_dispatch'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "refused demoted-owner dispatch queued another worker message"
    );
    assert_eq!(
        fx.platform
            .post("/__fixture__/available", &json!({"available":false}))
            .0,
        200
    );
    let (status, body, _) = fx.http(
        "POST",
        &format!("/api/app-runs/{next_id}/dispatch"),
        Some(&owner),
        "{}",
    );
    assert_ne!(
        status, 200,
        "membership outage allowed stale owner dispatch: {body}"
    );
    assert!(
        fx.try_op("app_run_dispatch", json!({"run_id":next_id}))
            .is_err(),
        "retained-approval daemon RPC dispatched during membership proof outage"
    );
    assert_eq!(
        fx.op("app_run_show", json!({"run_id":next_id}))["state"],
        "approved",
        "proof outage must not persist cancellation"
    );
    let count: i64 = fx
        .db()
        .query_row(
            "SELECT count(*) FROM messages WHERE source='app_run_dispatch'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "membership proof outage queued another worker message"
    );
}
