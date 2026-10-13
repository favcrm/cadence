//! Independent CAD-1374 acceptance: daemon-stream retention must not erase a
//! live workspace-effect intent while the serialized filesystem effect waits.
//! Noise rows below are explicitly synthetic; the intent and refusal are
//! produced by the real signed board/daemon route.
#![cfg(feature = "test-seam")]

use cadence_agent::adapter::ProviderEnv;
use cadence_agent::test_seam::{scoped, Asserted};
use cadence_agent::{client, daemon, master, store::Store};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
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
                std::env::set_var(k, v)
            } else {
                std::env::remove_var(k)
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
            cadence_agent::reaper::spawn(&mut command).expect("start owned real Worker/D1 fixture");
        Self { child, port, token }
    }
    fn post(&self, path: &str, value: &Value) -> Http {
        let body = value.to_string();
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(s,"POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Cad1374-Fixture-Token: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",self.port,self.token,body.len(),body).unwrap();
        read_http(s)
    }
    fn stop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            if let Ok(mut s) = TcpStream::connect(("127.0.0.1", self.port)) {
                let b = "{}";
                let _=write!(s,"POST /__fixture__/shutdown HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Cad1374-Fixture-Token: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",self.port,self.token,b.len(),b);
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
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let root = Builder::new()
            .prefix("cad1374-ret-")
            .tempdir_in(base)
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
        let host = format!("cad1374-ret.board.localhost:{port}");
        let token = format!("cad1374-ret-{}", uuid::Uuid::new_v4());
        let mut platform = Platform::start(fport, &host, &ready, token);
        let until = Instant::now() + WAIT;
        while !ready.exists() {
            if let Some(status) = platform.child.try_wait().unwrap() {
                panic!("platform fixture exited: {status}")
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
                panic!("daemon exited: {:?}", daemon.join().unwrap())
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
    fn login(&self) -> String {
        let (status, b, _) = self
            .platform
            .post("/__fixture__/signin", &json!({"sub":SUBJECT}));
        assert_eq!(status, 200, "D1 signed-in owner");
        let assertion = serde_json::from_str::<Value>(&b).unwrap()["assertion"]
            .as_str()
            .unwrap()
            .to_owned();
        let (status, _, c) = self.http(
            "POST",
            "/__platform/session",
            None,
            &json!({"assertion":assertion}).to_string(),
        );
        assert_eq!(status, 200, "signed board session");
        c.expect("board cookie")
    }
    fn profiles(&self) {
        let dir = self.root.path().join("pm");
        for (k, v) in [
            ("user.name", "CAD-1374 fixture"),
            ("user.email", "cad1374-fixture@invalid"),
        ] {
            let mut command = Command::new("git");
            command
                .arg("-C")
                .arg(&dir)
                .args(["config", "--local", k, v]);
            assert!(cadence_agent::reaper::status(&mut command)
                .unwrap()
                .success());
        }
        let pm = cadence_agent::issue::Pm::at(&dir).unwrap();
        for alias in ["lead", "writer"] {
            cadence_agent::master::write_file(
                &pm,
                alias,
                "SOUL.md",
                "Synthetic local acceptance profile.\n",
                "operator",
            )
            .unwrap();
            cadence_agent::master::write_file(
                &pm,
                alias,
                "AGENT.md",
                "Local workflow fixture.\n",
                "operator",
            )
            .unwrap();
        }
    }
    fn db(&self) -> Connection {
        Connection::open_with_flags(
            self.state().join("cadence.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }
    fn install(&self, cookie: &str) -> String {
        let (status, b, _) = self.http("GET", "/api/app-catalog", Some(cookie), "");
        assert_eq!(status, 200, "catalog");
        let list: Value = serde_json::from_str(&b).unwrap();
        let row = list["catalog"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["source_kind"] == "builtin" && r["state"] == "available")
            .expect("available builtin");
        let id = row["id"].as_str().unwrap().to_owned();
        let (status, b, _) = self.http(
            "POST",
            "/api/app-catalog/install",
            Some(cookie),
            &json!({"catalog_id":id,"expected_digest":row["digest"]}).to_string(),
        );
        assert_eq!(status, 200, "install owned builtin");
        serde_json::from_str::<Value>(&b).unwrap()["install_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    fn prepared(&self) -> Value {
        let c = self.db();
        let mut s=c.prepare("SELECT payload FROM events WHERE alias='daemon' AND kind='app_workspace_mutation_prepared' ORDER BY seq DESC LIMIT 1").unwrap();
        let payload: String = s.query_row([], |r| r.get(0)).unwrap();
        serde_json::from_str(&payload).unwrap()
    }
    fn capability(&self, install: &str) -> Option<(i64, String, String)> {
        self.db()
            .query_row(
                "SELECT epoch,digest,state FROM app_install_capabilities WHERE install_id=?1",
                [install],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .unwrap()
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
        .expect("isolated fixture port")
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

fn add_noise(store: &Store, start: usize, count: usize) {
    for n in start..start + count {
        store
            .event_public(
                Store::DAEMON_STREAM,
                "cad1374_retention_noise",
                json!({"fixture":"synthetic ordinary WAL noise","ordinal":n}),
            )
            .unwrap();
    }
}
fn rows(fx: &Fx, kind: &str) -> Vec<(i64, Value)> {
    let c = fx.db();
    let mut s = c
        .prepare("SELECT seq,payload FROM events WHERE alias='daemon' AND kind=?1 ORDER BY seq")
        .unwrap();
    let rows = s
        .query_map([kind], |r| {
            let p: String = r.get(1)?;
            Ok((r.get(0)?, serde_json::from_str::<Value>(&p).unwrap()))
        })
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

#[test]
fn daemon_retention_preserves_unmatched_live_intent_but_bounds_noise_and_completed_history() {
    let fx = Fx::new();
    let owner = fx.login();
    fx.profiles();
    let install = fx.install(&owner);
    let (status, preview, _) = fx.http(
        "POST",
        &format!("/api/app-installations/{install}/remove-preview"),
        Some(&owner),
        "{}",
    );
    assert_eq!(status, 200, "remove preview");
    let preview: Value = serde_json::from_str(&preview).unwrap();
    let remove = json!({"expected_generation":preview["generation"],"expected_digest":preview["digest"],"request_id":uuid::Uuid::new_v4().to_string()});
    let (status, _remove_body, _) = fx.http(
        "POST",
        &format!("/api/app-installations/{install}/remove"),
        Some(&owner),
        &remove.to_string(),
    );
    assert_eq!(status, 200, "real owner removes builtin");
    let record = fx
        .root
        .path()
        .join("pm/.apps/installations")
        .join(&install)
        .join("record.yaml");
    let removed_record = std::fs::read(&record).unwrap();
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(fx.root.path().join("pm"))
        .args(["rev-parse", "HEAD"]);
    let head_before_refusal = cadence_agent::reaper::output(&mut command).unwrap();
    assert!(head_before_refusal.status.success());
    let head_before_refusal = head_before_refusal.stdout;
    let capability_before_refusal = fx.capability(&install);
    let pm = cadence_agent::issue::Pm::at(&fx.root.path().join("pm")).unwrap();
    let lock = pm.lock().unwrap();
    let before = rows(&fx, "app_workspace_mutation_prepared").len();
    let cookie = owner.clone();
    let port = fx.port;
    let host = fx.host.clone();
    let restore_install = install.clone();
    let done = Arc::new(AtomicBool::new(false));
    let thread_done = done.clone();
    let join = thread::spawn(move || {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let path = format!("/api/app-installations/{restore_install}/restore");
        write!(s,"POST {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: http://{host}\r\nSec-Fetch-Site: same-origin\r\nX-Cadence-Board: 1\r\nCookie: {cookie}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 2\r\n\r\n{{}}").unwrap();
        let r = read_http(s);
        thread_done.store(true, SeqCst);
        r
    });
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        if rows(&fx, "app_workspace_mutation_prepared").len() > before {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        rows(&fx, "app_workspace_mutation_prepared").len() > before,
        "real HTTP restore did not persist its intent while PM lock held"
    );
    let prep = fx.prepared();
    assert_eq!(prep["action"], "app_workspace_restored");
    assert_eq!(prep["resource"], format!("install:{install}"));
    assert_eq!(prep["human"]["sub"], SUBJECT);
    assert_eq!(prep["human"]["company"], fx.company);
    assert_eq!(prep["human"]["role"], "operator");
    assert_eq!(prep["schema_version"], 1);
    assert_eq!(prep["correlation_id"], prep["request_id"]);
    assert!(
        !done.load(SeqCst),
        "restore completed while real PM lock held"
    );
    // Side-open explicitly avoids daemon startup/recovery generation reset;
    // pruning runs on the real DB while the genuine HTTP intent is in flight.
    let maintenance = Store::open_side(&fx.state().join("cadence.sqlite3"))
        .expect("non-recovering writer for isolated live daemon DB");
    add_noise(&maintenance, 0, 230);
    let prep_request = prep["request_id"]
        .as_str()
        .expect("canonical request UUID")
        .to_owned();
    let prep_seq = rows(&fx, "app_workspace_mutation_prepared")
        .last()
        .unwrap()
        .0;
    // Synthetic mismatched terminal: exact action/resource/company but wrong
    // request id. It is not a daemon-produced terminal and must not close intent.
    let mut wrong_uuid = prep.clone();
    wrong_uuid["request_id"] = json!(uuid::Uuid::new_v4().to_string());
    wrong_uuid["correlation_id"] = wrong_uuid["request_id"].clone();
    wrong_uuid["outcome"] = json!("refused");
    maintenance
        .event_public(
            Store::DAEMON_STREAM,
            "app_workspace_mutation_finished",
            wrong_uuid,
        )
        .unwrap();
    let mut wrong_company = prep.clone();
    wrong_company["outcome"] = json!("refused");
    wrong_company["human"]["company"] = json!("synthetic-foreign-company");
    wrong_company["company"] = json!("synthetic-foreign-company");
    maintenance
        .event_public(
            Store::DAEMON_STREAM,
            "app_workspace_mutation_finished",
            wrong_company,
        )
        .unwrap();
    maintenance.prune_stream(Store::DAEMON_STREAM, 200).unwrap();
    let intents = rows(&fx, "app_workspace_mutation_prepared");
    let still_active = intents
        .iter()
        .find(|(seq, _)| *seq == prep_seq)
        .expect("active genuine prepared intent was pruned by ordinary-WAL retention");
    assert_eq!(
        still_active.1, prep,
        "retention changed actual active intent metadata"
    );
    assert_eq!(still_active.1["request_id"], prep_request);
    let noise = rows(&fx, "cad1374_retention_noise");
    assert!(
        noise.len() <= 200,
        "ordinary noise exceeded cap: {}",
        noise.len()
    );
    assert!(
        noise.iter().any(|(_, p)| p["ordinal"] == 229),
        "newest ordinary noise was not retained"
    );
    assert!(
        !noise.iter().any(|(_, p)| p["ordinal"] == 0),
        "old ordinary noise escaped bounded retention"
    );
    assert!(
        rows(&fx, "app_workspace_mutation_finished")
            .iter()
            .any(|(_, p)| p["request_id"] != prep_request && p["resource"] == prep["resource"]),
        "synthetic wrong-UUID terminal was not retained"
    );
    assert!(
        rows(&fx, "app_workspace_mutation_finished")
            .iter()
            .any(|(_, p)| p["request_id"] == prep_request
                && p["human"]["company"] == "synthetic-foreign-company"),
        "synthetic wrong-company terminal was not retained"
    );
    let (demotion, _, _) = fx
        .platform
        .post("/__fixture__/role", &json!({"sub":SUBJECT,"role":"member"}));
    assert_eq!(demotion, 200, "real D1 role demotion after durable intent");
    drop(lock);
    let (status, _private_body, _) = join.join().expect("real restore HTTP thread");
    assert!(
        !(200..300).contains(&status),
        "demoted user restore returned a 2xx status"
    );
    let record_after_refusal = std::fs::read(&record).unwrap();
    assert!(
        record_after_refusal == removed_record,
        "refused restore changed physical installation record"
    );
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(fx.root.path().join("pm"))
        .args(["rev-parse", "HEAD"]);
    let head_after_refusal = cadence_agent::reaper::output(&mut command).unwrap();
    assert!(head_after_refusal.status.success());
    assert_eq!(
        head_after_refusal.stdout, head_before_refusal,
        "refused restore changed owned PM HEAD"
    );
    assert_eq!(
        fx.capability(&install),
        capability_before_refusal,
        "refused restore changed installation capability"
    );
    let terminals = rows(&fx, "app_workspace_mutation_finished");
    let terminal = terminals
        .iter()
        .find(|(_, p)| {
            p["request_id"] == prep_request
                && p["correlation_id"] == prep["correlation_id"]
                && p["human"]["company"] == fx.company
                && p["schema_version"] == prep["schema_version"]
                && p["action"] == prep["action"]
                && p["resource"] == prep["resource"]
                && p["outcome"] == "refused"
        })
        .expect("real daemon matching refusal terminal, not synthetic wrong-company frame");
    assert_eq!(terminal.1["outcome"], "refused");
    assert_eq!(terminal.1["action"], prep["action"]);
    assert_eq!(terminal.1["resource"], prep["resource"]);
    assert_eq!(terminal.1["human"]["company"], fx.company);
    assert_eq!(terminal.1["schema_version"], prep["schema_version"]);
    assert_eq!(terminal.1["correlation_id"], prep["correlation_id"]);
    assert_eq!(terminal.1["request_id"], prep["request_id"]);
    // Completed history remains ordinary bounded history, not an audit promise.
    add_noise(&maintenance, 230, 230);
    maintenance.prune_stream(Store::DAEMON_STREAM, 200).unwrap();
    assert!(
        rows(&fx, "app_workspace_mutation_prepared")
            .iter()
            .all(|(_, p)| p["request_id"] != prep_request
                && p["correlation_id"] != prep["correlation_id"]),
        "completed prepared history became permanently exempt from retention"
    );
    assert!(
        rows(&fx, "app_workspace_mutation_finished")
            .iter()
            .all(|(_, p)| p["request_id"] != prep_request
                && p["correlation_id"] != prep["correlation_id"]),
        "completed terminal history became permanently exempt from retention"
    );
    let noise = rows(&fx, "cad1374_retention_noise");
    assert!(
        noise.len() <= 200,
        "second prune did not bound ordinary WAL"
    );
    assert!(noise.iter().any(|(_, p)| p["ordinal"] == 459));
    drop(maintenance);
}
