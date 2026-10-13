//! Independent CAD-1374 acceptance for real hosted-session receipt backup and
//! recovery. The paired Worker/D1 owner assertion is the real SDK fixture;
//! SQLite observations and backup/restore calls exercise the native store.
#![cfg(feature = "test-seam")]

use cadence_agent::adapter::ProviderEnv;
use cadence_agent::test_seam::{scoped, Asserted};
use cadence_agent::{backup, client, daemon, master};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
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
            cadence_agent::reaper::spawn(&mut command).expect("start owned Worker/D1 fixture");
        Self { child, port, token }
    }
    fn post(&self, path: &str, value: &Value) -> Http {
        let body = value.to_string();
        let mut stream =
            TcpStream::connect(("127.0.0.1", self.port)).expect("owned fixture loopback");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(stream, "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Cad1374-Fixture-Token: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", self.port, self.token, body.len(), body).unwrap();
        read_http(stream)
    }
    fn stop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", self.port)) {
                let body = "{}";
                let _ = write!(stream, "POST /__fixture__/shutdown HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Cad1374-Fixture-Token: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 2\r\n\r\n{}", self.port, self.token, body);
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
            .prefix("c1374-br-")
            .tempdir_in(std::env::temp_dir())
            .unwrap();
        let state = root.path().join("state");
        let pm = root.path().join("pm");
        let home = root.path().join("home");
        let ready = root.path().join("ready.json");
        let backup_dir = root.path().join("backup-artifacts");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&backup_dir).unwrap();
        cadence_agent::issue::Pm::init(&pm).unwrap();
        let mut yaml = std::fs::read_to_string(pm.join("pm.yaml")).unwrap();
        yaml.push_str("\npi:\n  models:\n    allow: [\"fake/model-1\"]\n    default: {master: \"fake/model-1\", worker: \"fake/model-1\"}\n");
        std::fs::write(pm.join("pm.yaml"), yaml).unwrap();
        let port = free_port();
        let platform_port = free_port_excluding(port);
        let host = format!("cad1374-backup.board.localhost:{port}");
        let token = format!("cad1374-backup-{}", uuid::Uuid::new_v4());
        let mut platform = Platform::start(platform_port, &host, &ready, token);
        let until = Instant::now() + WAIT;
        while !ready.exists() {
            if let Some(status) = platform.child.try_wait().unwrap() {
                panic!("platform fixture exited: {status}");
            }
            assert!(Instant::now() < until, "platform fixture readiness timeout");
            thread::sleep(Duration::from_millis(50));
        }
        let data: Value = serde_json::from_slice(&std::fs::read(&ready).unwrap()).unwrap();
        assert_eq!(data["publicHost"], host);
        let company = data["company"].as_str().unwrap().to_owned();
        let instance = data["instance"].as_str().unwrap().to_owned();
        let issuer = data["publicIssuer"].as_str().unwrap().to_owned();
        let proxy = format!("http://127.0.0.1:{platform_port}");
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
        let mut fx = Self {
            root,
            _env: env,
            platform,
            port,
            platform_port,
            pm_dir: pm,
            home_dir: home,
            host,
            issuer,
            company,
            board_stop: Arc::new(AtomicBool::new(false)),
            board: None,
            daemon_stop: Arc::new(AtomicBool::new(false)),
            daemon: None,
        };
        fx.start_daemon(true);
        fx.start_board();
        fx
    }
    fn state(&self) -> PathBuf {
        self.root.path().join("state")
    }
    fn backup_dir(&self) -> PathBuf {
        self.root.path().join("backup-artifacts")
    }
    fn http(&self, method: &str, path: &str, cookie: Option<&str>, body: &str) -> Http {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: {}\r\nOrigin: http://{}\r\nSec-Fetch-Site: same-origin\r\nX-Cadence-Board: 1\r\nContent-Type: application/json\r\nConnection: close\r\n{}Content-Length: {}\r\n\r\n{body}", self.host, self.host, cookie.map(|value| format!("Cookie: {value}\r\n")).unwrap_or_default(), body.len()).unwrap();
        read_http(stream)
    }
    fn assertion(&self) -> String {
        let (status, body, _) = self
            .platform
            .post("/__fixture__/signin", &json!({"sub": SUBJECT}));
        assert_eq!(status, 200, "real D1 owner assertion producer refused");
        serde_json::from_str::<Value>(&body).unwrap()["assertion"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    fn open(&self, assertion: &str) -> Http {
        self.http(
            "POST",
            "/__platform/session",
            None,
            &json!({"assertion": assertion}).to_string(),
        )
    }
    fn db(&self) -> Connection {
        Connection::open_with_flags(
            self.state().join("cadence.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }
    fn receipt_rows(&self) -> Vec<(i64, Value)> {
        let conn = self.db();
        let mut stmt = conn.prepare("SELECT seq,payload FROM events WHERE kind IN ('board_session_mutation_prepared','board_session_mutation_finished') AND alias='daemon' ORDER BY seq").unwrap();
        let mapped = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get::<_, String>(1)?)))
            .unwrap();
        mapped
            .map(|row| {
                let (seq, payload) = row.unwrap();
                (seq, serde_json::from_str(&payload).unwrap())
            })
            .collect()
    }
    fn start_daemon(&mut self, register: bool) {
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
                panic!("daemon exited: {:?}", daemon.join().unwrap());
            }
            assert!(Instant::now() < until, "daemon startup timeout");
            thread::sleep(Duration::from_millis(50));
        }
        if register {
            scoped(Asserted::Operator, || {
                client::rpc(
                    &state,
                    "master_start",
                    json!({"provider":"pi","unconfined":true}),
                )
            })
            .unwrap();
        }
        self.daemon_stop = daemon_stop;
        self.daemon = Some(daemon);
    }
    fn start_board(&mut self) {
        let (tx, rx) = std::sync::mpsc::channel();
        let board_stop = Arc::new(AtomicBool::new(false));
        let opts = cadence_agent::ui::ServeOpts {
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
        let (state, pm) = (self.state(), self.pm_dir.clone());
        let board = thread::spawn(move || {
            let _ = cadence_agent::ui::serve(&state, &pm, &opts);
        });
        rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
        self.board_stop = board_stop;
        self.board = Some(board);
    }
    fn stop_board_and_daemon(&mut self) {
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
    }
}
fn free_port() -> u16 {
    (3110..3200)
        .find(|port| TcpListener::bind(("127.0.0.1", *port)).is_ok())
        .expect("owned board port")
}
fn free_port_excluding(excluded: u16) -> u16 {
    (3110..3200)
        .filter(|port| *port != excluded)
        .find(|port| TcpListener::bind(("127.0.0.1", *port)).is_ok())
        .expect("owned platform port")
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
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn file_digest(path: &Path) -> String {
    digest(&std::fs::read(path).expect("owned auth file readable"))
}
fn manifest_for(result: &Value) -> PathBuf {
    result["manifest"]
        .as_str()
        .expect("backup API returns manifest path")
        .into()
}

#[test]
fn real_hosted_receipts_backup_refuse_live_restore_retain_and_recover_after_shutdown() {
    let mut fx = Fx::new();
    let assertion = fx.assertion();
    let (status, _, cookie) = fx.open(&assertion);
    assert_eq!(status, 200, "real signed owner session exchange refused");
    let cookie = cookie.expect("server-held owner session cookie");
    let meta: Value =
        serde_json::from_str(&fx.http("GET", "/api/meta", Some(&cookie), "").1).unwrap();
    let session_id = meta["session"]["id"]
        .as_str()
        .expect("safe session reference")
        .to_owned();
    assert_eq!(meta["session"]["user"]["role"], "operator");
    let rows = fx.receipt_rows();
    assert_eq!(
        rows.len(),
        2,
        "the signed hosted open writes both actual canonical phases"
    );
    let mut expected = Vec::new();
    for (seq, receipt) in &rows {
        assert_eq!(receipt["action"], "board_session_opened");
        assert_eq!(receipt["human"]["company"], fx.company);
        assert_eq!(receipt["human"]["sub"], SUBJECT);
        assert_eq!(receipt["human"]["session"], session_id);
        assert_eq!(receipt["human"]["role"], "operator");
        assert_eq!(receipt["actor_kind"], "human");
        assert!(receipt["at"]
            .as_f64()
            .is_some_and(|time| time.is_finite() && time > 0.0));
        expected.push((format!("event-{seq:020}"), receipt.clone()));
    }
    assert_eq!(rows[0].1["request_id"], rows[1].1["request_id"]);
    assert_eq!(rows[0].1["correlation_id"], rows[1].1["correlation_id"]);
    let (query_status, query_body, _) = fx.http(
        "GET",
        &format!("/api/activity?sub={SUBJECT}&action=board_session_opened&limit=100"),
        Some(&cookie),
        "",
    );
    assert_eq!(query_status, 200, "real owner activity query refused");
    let query: Value = serde_json::from_str(&query_body).unwrap();
    let mut queried: Vec<_> = query["events"]
        .as_array()
        .expect("activity event list")
        .iter()
        .map(|event| {
            event["event_id"]
                .as_str()
                .expect("physical event id")
                .to_owned()
        })
        .collect();
    let mut expected_ids: Vec<_> = expected.iter().map(|(id, _)| id.clone()).collect();
    queried.sort();
    expected_ids.sort();
    assert_eq!(
        queried, expected_ids,
        "HTTP exposes precisely both physical canonical receipts"
    );
    for event in query["events"].as_array().unwrap() {
        assert_eq!(event["human"]["sub"], SUBJECT);
        assert_eq!(event["human"]["role"], "operator");
        assert_eq!(event["receipt"]["role_basis"], "signed_session_snapshot");
    }

    let auth_path = fx.state().join("operator/sessions.json");
    let auth_hash = file_digest(&auth_path);
    let mut command = Command::new("git");
    command.args(["-C", fx.pm_dir.to_str().unwrap(), "rev-parse", "HEAD"]);
    let pm_head = cadence_agent::reaper::output(&mut command).unwrap();
    assert!(pm_head.status.success(), "fixture PM repository has a HEAD");
    let pm_head = String::from_utf8(pm_head.stdout).unwrap();
    let outside = fx
        .backup_dir()
        .parent()
        .unwrap()
        .join("unrelated-preserve-me");
    std::fs::write(&outside, b"outside the backup pruning scope").unwrap();

    // Core-only supported backup: credentials and the separate PM git repo
    // are outside its artifact set and are not a recovery claim.
    let first = backup::backup(&fx.state(), &fx.backup_dir(), 2, "manual").unwrap();
    let first_manifest = manifest_for(&first);
    let (first_data, first_db) = backup::verify(&first_manifest).unwrap();
    assert_eq!(first_data.kind, backup::Kind::Backup);
    assert!(
        first_data.app_records.is_empty(),
        "this fixture exercises core receipt recovery only"
    );
    assert_eq!(first_data.reason, "manual");
    let first_artifacts = std::fs::read_dir(fx.backup_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(
        first_artifacts.len(),
        2,
        "manifest plus named verified core copy"
    );
    assert!(first_artifacts
        .iter()
        .all(|path| path != &auth_path && !path.to_string_lossy().contains("sessions.json")));
    assert_eq!(
        file_digest(&auth_path),
        auth_hash,
        "backup leaves external auth bytes unchanged"
    );
    assert!(
        std::fs::read(&outside).is_ok(),
        "backup/prune is confined to its supplied directory"
    );
    let manifest_text = std::fs::read_to_string(&first_manifest).unwrap();
    assert!(!manifest_text.contains("sessions.json"));
    assert!(!manifest_text.contains(&cookie));
    assert!(!manifest_text.contains(&assertion));
    assert_eq!(std::fs::metadata(first_db).unwrap().len(), first_data.bytes);
    assert_eq!(
        receipt_snapshot(&first_data, &first_manifest),
        expected,
        "snapshot copy contains canonical source frames"
    );

    let second = backup::backup(&fx.state(), &fx.backup_dir(), 1, "manual").unwrap();
    let second_manifest = manifest_for(&second);
    let (second_data, second_db) = backup::verify(&second_manifest).unwrap();
    assert_ne!(
        first_manifest, second_manifest,
        "backup names are unique even within one timestamp second"
    );
    assert!(
        !first_manifest.exists(),
        "keep=1 removes the older same-reason verified manifest"
    );
    assert!(
        !first_db_path(&first_data, &first_manifest).exists(),
        "retention removes the old manifest's named copy"
    );
    assert!(second_manifest.is_file() && second_db.is_file());
    assert_eq!(receipt_snapshot(&second_data, &second_manifest), expected);
    assert!(
        outside.is_file(),
        "retention never prunes outside its supplied directory"
    );
    assert_eq!(file_digest(&auth_path), auth_hash);

    // A live daemon must refuse through the actual cadence.lock path before
    // any destructive replacement; no synthetic lock or fake refusal.
    let live_error = backup::restore(
        &second_manifest,
        &fx.state(),
        &backup::RestoreOptions {
            force: true,
            repos: vec![],
        },
    )
    .expect_err("restore over the live fixture daemon must refuse");
    let live_message = live_error.to_string();
    assert!(
        live_message.contains("daemon is running") && live_message.contains("cadence.lock"),
        "restore refusal identifies the held daemon lock"
    );
    assert_eq!(
        fx.receipt_rows(),
        rows,
        "live-lock refusal leaves physical receipt rows unchanged"
    );
    assert_eq!(
        file_digest(&auth_path),
        auth_hash,
        "live-lock refusal leaves external auth bytes unchanged"
    );

    // The same auth file and PM checkout intentionally remain outside the
    // core-only backup. Stop only this fixture's board/daemon, restore via
    // supported API, then boot against the persisted master without registering
    // a second one. This does not claim credential or PM-receipt recovery.
    fx.stop_board_and_daemon();
    backup::restore(
        &second_manifest,
        &fx.state(),
        &backup::RestoreOptions {
            force: true,
            repos: vec![],
        },
    )
    .unwrap();
    assert_eq!(
        receipt_snapshot(&second_data, &second_manifest),
        expected,
        "verified source still has the original frames"
    );
    assert_eq!(
        fx.receipt_rows(),
        rows,
        "restored live store has the original physical receipt rows"
    );
    assert_eq!(
        file_digest(&auth_path),
        auth_hash,
        "auth is unchanged because it was outside this backup"
    );
    let mut command = Command::new("git");
    command.args(["-C", fx.pm_dir.to_str().unwrap(), "rev-parse", "HEAD"]);
    let pm_after = cadence_agent::reaper::output(&mut command).unwrap();
    assert!(pm_after.status.success());
    assert_eq!(
        pm_after.stdout,
        pm_head.as_bytes(),
        "the external PM git HEAD is unchanged"
    );
    fx.start_daemon(false);
    fx.start_board();
    let (recovered_status, recovered_body, _) = fx.http(
        "GET",
        "/api/activity?sub=cad1374-paired-subject&action=board_session_opened&limit=100",
        Some(&cookie),
        "",
    );
    assert_eq!(recovered_status, 200, "original owner cookie remains usable because the original auth file remains outside backup");
    let recovered: Value = serde_json::from_str(&recovered_body).unwrap();
    let mut recovered_ids: Vec<_> = recovered["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["event_id"].as_str().unwrap().to_owned())
        .collect();
    recovered_ids.sort();
    assert_eq!(recovered_ids, expected_ids);
    for event in recovered["events"].as_array().unwrap() {
        let matching = expected
            .iter()
            .find(|(id, _)| id == event["event_id"].as_str().unwrap())
            .unwrap();
        assert_eq!(event["human"]["sub"], SUBJECT);
        assert_eq!(event["human"]["session"], session_id);
        assert_eq!(event["human"]["role"], "operator");
        assert_eq!(event["receipt"]["request_id"], matching.1["request_id"]);
        assert_eq!(
            event["receipt"]["correlation_id"],
            matching.1["correlation_id"]
        );
    }
}

fn receipt_snapshot(manifest: &backup::Manifest, manifest_path: &Path) -> Vec<(String, Value)> {
    let db_path = manifest_path.parent().unwrap().join(&manifest.db_file);
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut stmt = conn.prepare("SELECT seq,payload FROM events WHERE kind IN ('board_session_mutation_prepared','board_session_mutation_finished') AND alias='daemon' ORDER BY seq").unwrap();
    let snapshot: Vec<_> = stmt
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|row| {
            let (seq, payload) = row.unwrap();
            (
                format!("event-{seq:020}"),
                serde_json::from_str(&payload).unwrap(),
            )
        })
        .collect();
    snapshot
}
fn first_db_path(manifest: &backup::Manifest, manifest_path: &Path) -> PathBuf {
    manifest_path.parent().unwrap().join(&manifest.db_file)
}
