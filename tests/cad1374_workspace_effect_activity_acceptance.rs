//! Independent CAD-1374 acceptance for builtin workspace effects.
//! The HTTP board session and D1 membership are real signed fixture flows;
//! no fake role callback or daemon guard is used.
#![cfg(feature = "test-seam")]

use cadence_agent::adapter::ProviderEnv;
use cadence_agent::test_seam::{scoped, Asserted};
use cadence_agent::{client, daemon, master};
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
            cadence_agent::reaper::spawn(&mut command).expect("start owned real Worker/D1 fixture");
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
                let _ = write!(s, "POST /__fixture__/shutdown HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Cad1374-Fixture-Token: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", self.port, self.token, b.len(), b);
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
            .prefix("cad1374-wse-")
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
        let host = format!("cad1374-wse.board.localhost:{port}");
        let token = format!("cad1374-wse-{}", uuid::Uuid::new_v4());
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
        assert_eq!(status, 200, "D1 signed-in owner: {b}");
        let assertion = serde_json::from_str::<Value>(&b).unwrap()["assertion"]
            .as_str()
            .unwrap()
            .to_owned();
        let (status, b, c) = self.http(
            "POST",
            "/__platform/session",
            None,
            &json!({"assertion":assertion}).to_string(),
        );
        assert_eq!(status, 200, "signed board session: {b}");
        c.expect("board cookie")
    }
    fn db(&self) -> Connection {
        Connection::open(self.state().join("cadence.sqlite3")).unwrap()
    }
    fn prepared_count(&self) -> i64 {
        let c = Connection::open_with_flags(
            self.state().join("cadence.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        c.query_row(
            "SELECT count(*) FROM events WHERE kind='app_workspace_mutation_prepared'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }
    fn prepared(&self) -> Value {
        let c = Connection::open_with_flags(
            self.state().join("cadence.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut s=c.prepare("SELECT payload FROM events WHERE kind='app_workspace_mutation_prepared' ORDER BY seq DESC LIMIT 1").unwrap();
        let t: String = s.query_row([], |r| r.get(0)).unwrap();
        serde_json::from_str(&t).unwrap()
    }
    fn install_capability(&self, install_id: &str) -> Option<(i64, String, String)> {
        let c = Connection::open_with_flags(
            self.state().join("cadence.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        c.query_row(
            "SELECT epoch,digest,state FROM app_install_capabilities WHERE install_id=?",
            [install_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .unwrap()
    }
    fn install_builtin(&self, cookie: &str) -> String {
        let (status, b, _) = self.http("GET", "/api/app-catalog", Some(cookie), "");
        assert_eq!(status, 200, "current catalog: {b}");
        let list: Value = serde_json::from_str(&b).unwrap();
        let row = list["catalog"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["source_kind"] == "builtin" && r["state"] == "available")
            .expect("catalog provides an available builtin");
        let id = row["id"].as_str().unwrap().to_owned();
        let digest = row["digest"].as_str().unwrap().to_owned();
        let (status, b, _) = self.http(
            "POST",
            "/api/app-catalog/install",
            Some(cookie),
            &json!({"catalog_id":id,"expected_digest":digest}).to_string(),
        );
        assert_eq!(
            status, 200,
            "install current builtin by catalog digest: {b}"
        );
        let v: Value = serde_json::from_str(&b).unwrap();
        v["install_id"].as_str().expect("installed ID").to_owned()
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
fn workspace_effect_rechecks_signed_owner_after_pm_lock_and_requires_prepared_receipt() {
    let fx = Fx::new();
    let owner = fx.login();
    fx.profiles();
    let install_id = fx.install_builtin(&owner);
    let (status, preview, _) = fx.http(
        "POST",
        &format!("/api/app-installations/{install_id}/remove-preview"),
        Some(&owner),
        "{}",
    );
    assert_eq!(status, 200, "remove preview: {preview}");
    let preview: Value = serde_json::from_str(&preview).unwrap();
    let expected = json!({"expected_generation":preview["generation"],"expected_digest":preview["digest"],"request_id":uuid::Uuid::new_v4().to_string()});
    let record_path = fx
        .root
        .path()
        .join("pm/.apps/installations")
        .join(&install_id)
        .join("record.yaml");
    let live_record =
        std::fs::read(&record_path).expect("live installation record before receipt failure");
    let git = |args: &[&str]| {
        let mut command = Command::new("git");
        command.arg("-C").arg(fx.root.path().join("pm")).args(args);
        cadence_agent::reaper::output(&mut command).unwrap()
    };
    let git_head = || git(&["rev-parse", "HEAD"]).stdout;
    let head_before = git_head();
    let pm_yaml_head_before = git(&["show", "HEAD:pm.yaml"]);
    assert!(
        pm_yaml_head_before.status.success(),
        "initialized PM commit contains pm.yaml"
    );
    let pm_yaml_head_before = pm_yaml_head_before.stdout;
    let capability_before_refused = fx.install_capability(&install_id);
    let prepared_before_remove = fx.prepared_count();

    // A narrowly scoped INSERT failure proves the effect cannot pass without
    // its actual durable intent. The same operation succeeds after removal.
    let conn = fx.db();
    conn.execute_batch("CREATE TRIGGER cad1374_reject_workspace_prepared BEFORE INSERT ON events WHEN NEW.kind='app_workspace_mutation_prepared' BEGIN SELECT RAISE(ABORT,'acceptance prepared receipt fault'); END;").unwrap();
    let (status, body, _) = fx.http(
        "POST",
        &format!("/api/app-installations/{install_id}/remove"),
        Some(&owner),
        &expected.to_string(),
    );
    assert_ne!(
        status, 200,
        "remove proceeded when its prepared receipt INSERT was refused: {body}"
    );
    assert_eq!(
        std::fs::read(&record_path).unwrap(),
        live_record,
        "failed prepared receipt changed the install record"
    );
    assert_eq!(
        fx.install_capability(&install_id),
        capability_before_refused,
        "failed prepared INSERT changed installation capability consent"
    );
    assert_eq!(
        fx.prepared_count(),
        prepared_before_remove,
        "failed prepared INSERT produced a receipt"
    );
    assert_eq!(
        git_head(),
        head_before,
        "failed prepared INSERT committed PM changes"
    );
    assert_eq!(
        git(&["show", "HEAD:pm.yaml"]).stdout,
        pm_yaml_head_before,
        "failed prepared INSERT changed committed PM config"
    );
    conn.execute_batch("DROP TRIGGER cad1374_reject_workspace_prepared;")
        .unwrap();
    drop(conn);
    let (status, body, _) = fx.http(
        "POST",
        &format!("/api/app-installations/{install_id}/remove"),
        Some(&owner),
        &expected.to_string(),
    );
    assert_eq!(
        status, 200,
        "real signed owner remove positive control: {body}"
    );
    assert_ne!(
        git_head(),
        head_before,
        "successful HTTP removal did not commit the PM change"
    );
    let committed_record = git(&[
        "show",
        &format!("HEAD:.apps/installations/{install_id}/record.yaml"),
    ]);
    assert!(
        committed_record.status.success(),
        "successful removal commit contains the owned installation record"
    );
    assert_eq!(
        committed_record.stdout,
        std::fs::read(&record_path).unwrap(),
        "committed owned record matches the live removed record"
    );
    assert_eq!(
        git(&["show", "HEAD:pm.yaml"]).stdout,
        pm_yaml_head_before,
        "workspace effect commit did not include foreign PM config"
    );
    let revoked_capability = fx
        .install_capability(&install_id)
        .expect("soft remove retains its capability row");
    assert_eq!(
        revoked_capability.2, "revoked",
        "soft remove revokes installation capability"
    );
    assert_eq!(
        fx.prepared_count(),
        prepared_before_remove + 1,
        "one real prepared intent for successful remove"
    );
    let intent = fx.prepared();
    assert_eq!(intent["action"], "app_workspace_removed");
    assert_eq!(intent["resource"], format!("install:{install_id}"));
    assert_eq!(intent["human"]["sub"], SUBJECT);
    assert_eq!(intent["human"]["company"], fx.company);
    assert_eq!(intent["human"]["role"], "operator");
    assert!(intent["human"]["session"].as_str().is_some());
    assert_eq!(intent["schema_version"], 1);
    let request_id = intent["request_id"].as_str().expect("server request UUID");
    let correlation_id = intent["correlation_id"]
        .as_str()
        .expect("server correlation UUID");
    let canonical_uuid =
        |text: &str| uuid::Uuid::parse_str(text).is_ok_and(|id| id.to_string() == text);
    assert!(
        canonical_uuid(request_id) && canonical_uuid(correlation_id),
        "receipt IDs are canonical plain UUIDs"
    );
    assert_ne!(
        request_id,
        expected["request_id"].as_str().unwrap(),
        "receipt request ID is not the client-supplied remove ID"
    );
    assert_eq!(
        request_id, correlation_id,
        "receipt request and correlation IDs share the server mutation UUID"
    );
    assert!(
        intent
            .as_object()
            .is_some_and(|o| o.contains_key("causation_id")),
        "receipt contains its causation field"
    );
    assert_eq!(intent["causation_id"], Value::Null);
    let serialized = intent.to_string();
    assert!(!serialized.contains(&owner));
    assert!(!serialized.contains("cookie"));
    let c = Connection::open_with_flags(
        fx.state().join("cadence.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut s=c.prepare("SELECT payload FROM events WHERE kind='app_workspace_mutation_finished' ORDER BY seq DESC LIMIT 1").unwrap();
    let payload: String = s.query_row([], |r| r.get(0)).unwrap();
    let removed: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(removed["action"], "app_workspace_removed");
    assert_eq!(removed["resource"], format!("install:{install_id}"));
    assert_eq!(removed["outcome"], "removed");
    assert_eq!(removed["reason"], "app_workspace_removed");
    assert_eq!(removed["request_id"], intent["request_id"]);
    assert_eq!(removed["correlation_id"], intent["correlation_id"]);
    let digest = removed["revision_or_digest"]
        .as_str()
        .expect("server-computed effect digest");
    assert!(digest.strip_prefix("sha256:").is_some_and(|h| h.len() == 64
        && h.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))));
    assert_eq!(removed["human"]["sub"], SUBJECT);
    assert_eq!(removed["human"]["company"], fx.company);
    assert_eq!(removed["human"]["role"], "operator");
    assert_eq!(removed["causation_id"], Value::Null);
    drop(s);
    drop(c);
    let record_path = fx
        .root
        .path()
        .join("pm/.apps/installations")
        .join(&install_id)
        .join("record.yaml");
    let removed_record = std::fs::read(&record_path).expect("soft-removed installation record");

    // Take the public PM serialization lock, start restore through the real
    // signed HTTP relay, wait for its durable intent, demote the subject in
    // the real D1 producer, then release. The owner guard must re-prove role
    // inside the serialized effect before any restore/consent write.
    let head_after_remove = git_head();
    let pm = cadence_agent::issue::Pm::at(&fx.root.path().join("pm")).unwrap();
    let lock = pm.lock().unwrap();
    let before = fx.prepared_count();
    let cookie = owner.clone();
    let port = fx.port;
    let host = fx.host.clone();
    let restore_id = install_id.clone();
    let done = Arc::new(AtomicBool::new(false));
    let thread_done = done.clone();
    let join = thread::spawn(move || {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let path = format!("/api/app-installations/{restore_id}/restore");
        write!(s,"POST {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: http://{host}\r\nSec-Fetch-Site: same-origin\r\nX-Cadence-Board: 1\r\nCookie: {cookie}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 2\r\n\r\n{{}}").unwrap();
        let r = read_http(s);
        thread_done.store(true, SeqCst);
        r
    });
    let until = Instant::now() + Duration::from_secs(20);
    let mut prepared_while_locked = false;
    while Instant::now() < until {
        if fx.prepared_count() > before {
            prepared_while_locked = true;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let completed_while_locked = done.load(SeqCst);
    let (demotion_status, role_body, _) = if prepared_while_locked {
        fx.platform
            .post("/__fixture__/role", &json!({"sub":SUBJECT,"role":"member"}))
    } else {
        (0, String::new(), None)
    };
    drop(lock);
    let (status, body, _) = join.join().expect("restore HTTP worker");
    assert!(
        prepared_while_locked,
        "restore did not durably prepare while PM lock held; HTTP result {status}: {body}"
    );
    assert!(
        !completed_while_locked,
        "restore completed while its PM lock was held"
    );
    assert_eq!(
        demotion_status, 200,
        "real D1 membership demotion: {role_body}"
    );
    assert_ne!(
        status, 200,
        "restore succeeded after actual post-intent D1 demotion: {body}"
    );
    assert_eq!(
        fx.prepared_count(),
        before + 1,
        "refusal must have its single genuine prepared intent"
    );
    let c = Connection::open_with_flags(
        fx.state().join("cadence.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let pending:i64=c.query_row("SELECT count(*) FROM events WHERE kind='app_workspace_mutation_finished' AND json_extract(payload,'$.outcome')='refused'",[],|r|r.get(0)).unwrap();
    assert_eq!(
        pending, 1,
        "the actual daemon workspace guard records a refused terminal effect"
    );
    assert_eq!(
        std::fs::read(&record_path).unwrap(),
        removed_record,
        "demoted restore changed the installation record"
    );
    assert_eq!(
        fx.install_capability(&install_id),
        Some(revoked_capability),
        "demoted restore changed revoked installation capability consent"
    );
    assert_eq!(
        git(&["show", "HEAD:pm.yaml"]).stdout,
        pm_yaml_head_before,
        "demoted restore changed committed PM config"
    );
    assert_eq!(
        git_head(),
        head_after_remove,
        "post-lock role denial committed PM changes"
    );
    let mut s=c.prepare("SELECT payload FROM events WHERE kind='app_workspace_mutation_finished' ORDER BY seq DESC LIMIT 1").unwrap();
    let payload: String = s.query_row([], |r| r.get(0)).unwrap();
    let finished: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(finished["action"], "app_workspace_restored");
    assert_eq!(finished["resource"], format!("install:{install_id}"));
    assert_eq!(finished["outcome"], "refused");
    assert_eq!(finished["human"]["sub"], SUBJECT);
    assert_eq!(finished["human"]["company"], fx.company);
    assert_eq!(finished["human"]["role"], "operator");
    assert!(finished["human"]["session"].as_str().is_some());
    assert_eq!(finished["request_id"], finished["correlation_id"]);
    assert_eq!(finished["causation_id"], Value::Null);
    assert!(
        finished["revision_or_digest"].is_null()
            || finished["revision_or_digest"].as_str().is_some_and(|d| d
                .strip_prefix("sha256:")
                .is_some_and(|h| h.len() == 64
                    && h.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))))
    );
}
