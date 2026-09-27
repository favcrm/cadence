//! Operator connection management: real TCP peers, stolen sessions, no app grants.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::{contract_fixture::FakePlatform, issue::Pm};
use common::{daemon_opts, plant_member_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

const SECRET: &str = concat!("cadp_conn_fixture_", "b1c2d3e4f5g6");
struct Board {
    root: tempfile::TempDir,
    daemon: TestDaemon,
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
}
impl Board {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let mut opts = daemon_opts();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        opts.platforms
            .insert("fixture".into(), Arc::new(FakePlatform::standard()));
        let daemon = TestDaemon::start_opts(opts);
        let port = (3110..3200)
            .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let opts = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.clone()),
            test_seam: cfg!(feature = "test-seam"),
            ..Default::default()
        };
        let state = daemon.state.clone();
        let thread = std::thread::spawn(move || cadence_agent::ui::serve(&state, &pm.dir, &opts));
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
        Self {
            root,
            daemon,
            port,
            stop,
            thread: Some(thread),
        }
    }
    fn operator(&self, method: &str, path: &str, body: &str) -> (u16, String) {
        let session =
            common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &self.daemon.state, self.port);
        let (code, _, body) = common::op::raw(self.port, &session.request(method, path, body));
        assert!(!body.contains(SECRET), "credential leaked in HTTP response");
        (code, body)
    }
    fn value(&self, method: &str, path: &str, body: Value) -> Value {
        let encoded = body.to_string();
        let (code, text) = self.operator(method, path, if method == "GET" { "" } else { &encoded });
        assert_eq!(code, 200, "operator {method} {path}: {text}");
        serde_json::from_str(&text).unwrap()
    }
    fn create(&self, account: &str) -> Value {
        self.value("POST","/api/connections",json!({"provider":"fixture","account":account,"shape":"token","token":SECRET,"scopes":["widgets:read"],"accept_same_uid_risk":true}))["connection"].clone()
    }
    fn counts(&self) -> Vec<i64> {
        let db = rusqlite::Connection::open(self.daemon.state.join("cadence.sqlite3")).unwrap();
        ["platform_grants", "app_runs", "platform_effects", "jobs"]
            .iter()
            .map(|t| {
                db.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
                    .unwrap()
            })
            .collect()
    }
}
impl Drop for Board {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap().unwrap();
    }
}

#[test]
fn cad688_http_operator_management_is_populated_strict_and_grant_free() {
    let b = Board::new();
    let providers = b.value("GET", "/api/connection-providers", json!({}));
    assert!(providers["providers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["provider"] == "fixture"));
    let listed = b.value("GET", "/api/connections", json!({}));
    assert!(listed["connections"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["provider"] == "local" && c["account"] == "local"));
    let before = b.counts();
    let c = b.create("http-account");
    let id = c["id"].as_str().unwrap();
    assert_eq!(c["provider"], "fixture");
    assert_eq!(c["account"], "http-account");
    assert!(!id.is_empty());
    assert_eq!(
        b.value("GET", &format!("/api/connections/{id}"), json!({}))["connection"]["id"],
        id
    );
    let checked = b.value("POST", &format!("/api/connections/{id}/status"), json!({}));
    assert_eq!(checked["connection"]["id"], id);
    for (path, body) in [
        (
            "/api/connections".to_string(),
            json!({"provider":"fixture","account":"forged","shape":"token","token":SECRET,"scopes":["widgets:read"],"operator":true}),
        ),
        (
            format!("/api/connections/{id}/rotate"),
            json!({"token":SECRET,"provider":"fixture","account":"other"}),
        ),
        (
            format!("/api/connections/{id}/revoke"),
            json!({"connection_id":"other"}),
        ),
        (
            format!("/api/connections/{id}/status"),
            json!({"agent":"operator"}),
        ),
    ] {
        assert_eq!(
            b.operator("POST", &path, &body.to_string()).0,
            400,
            "unknown fields admitted"
        );
    }
    for path in [
        "/api/connections?provider=fixture",
        "/api/connections?x=1&x=2",
        "/api/connection-providers?provider=fixture",
    ] {
        assert_eq!(
            b.operator("GET", path, "").0,
            400,
            "unsupported query admitted"
        );
    }
    let duplicate = format!("{{\"token\":\"{}\",\"token\":\"{}\"}}", SECRET, SECRET);
    assert_eq!(
        b.operator("POST", &format!("/api/connections/{id}/rotate"), &duplicate)
            .0,
        400
    );
    for body in [
        json!({"token":SECRET,"scopes":null}).to_string(),
        json!({"token":SECRET,"accept_same_uid_risk":null}).to_string(),
        format!("{{\"{}\":\"x\"}}", SECRET),
    ] {
        let (code, text) = b.operator("POST", &format!("/api/connections/{id}/rotate"), &body);
        assert_eq!(code, 400, "invalid credential schema admitted");
        assert!(
            !text.contains(SECRET),
            "schema diagnostics reflected credential text"
        );
    }
    let rotated = b.value(
        "POST",
        &format!("/api/connections/{id}/rotate"),
        json!({"token":SECRET}),
    )["connection"]
        .clone();
    assert_eq!(rotated["id"], id);
    assert!(rotated["revision"].as_u64().unwrap() > c["revision"].as_u64().unwrap());
    assert_eq!(
        b.value("POST", &format!("/api/connections/{id}/revoke"), json!({}))["revoked"],
        true
    );
    let replacement = b.create("http-account");
    assert_ne!(replacement["id"], id);
    for verb in ["rotate", "revoke", "status"] {
        let body = if verb == "rotate" {
            json!({"token":SECRET})
        } else {
            json!({})
        };
        let (code, text) = b.operator(
            "POST",
            &format!("/api/connections/{id}/{verb}"),
            &body.to_string(),
        );
        assert!(code >= 400, "stale ID affected replacement: {text}");
    }
    assert_eq!(
        b.value(
            "GET",
            &format!("/api/connections/{}", replacement["id"].as_str().unwrap()),
            json!({})
        )["connection"],
        replacement
    );
    assert_eq!(
        b.counts(),
        before,
        "management created grants, jobs, effects or runs"
    );
}

#[test]
fn cad688_actual_agent_and_setsid_http_management_refuses_stolen_session() {
    let b = Board::new();
    let c = b.create("native-http");
    let id = c["id"].as_str().unwrap();
    b.value("GET", "/api/connection-providers", json!({}));
    b.value("GET", "/api/connections", json!({}));
    b.value("GET", &format!("/api/connections/{id}"), json!({}));
    b.value("POST", &format!("/api/connections/{id}/status"), json!({}));
    let before = b.counts();
    let mut lane = LaneShell::spawn(b.root.path());
    plant_member_pane(
        &b.daemon,
        "connection-http-worker",
        "claude",
        None,
        lane.pid(),
    );
    let cases=vec![
        ("GET","/api/connection-providers".to_string(),String::new()),
        ("GET","/api/connections".to_string(),String::new()),
        ("GET",format!("/api/connections/{id}"),String::new()),
        ("POST","/api/connections".to_string(),json!({"provider":"fixture","account":"stolen","shape":"token","token":SECRET,"scopes":["widgets:read"],"accept_same_uid_risk":true}).to_string()),
        ("POST",format!("/api/connections/{id}/rotate"),json!({"token":SECRET}).to_string()),
        ("POST",format!("/api/connections/{id}/revoke"),"{}".into()),
        ("POST",format!("/api/connections/{id}/status"),"{}".into()),
        ("POST",format!("/api/connections/{id}/status"),json!({"operator":true,"agent":"operator"}).to_string()),
    ];
    let mut failures = Vec::new();
    for prefix in ["", "setsid "] {
        for (method, path, body) in &cases {
            let stolen =
                common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
            let wire = stolen.request_as(method, path, body, "");
            assert!(!wire.contains(cadence_agent::test_seam::AS_HEADER));
            assert!(!wire.contains(cadence_agent::test_seam::TOKEN_HEADER));
            assert!(wire.contains(&stolen.cookie));
            assert!(wire.contains(&stolen.key));
            let file = lane
                .dir
                .path()
                .join(format!("connection-request-{}.txt", lane.seq));
            std::fs::write(&file, wire).unwrap();
            let (rc,response)=lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));s.sendall(open(sys.argv[2],\"rb\").read());print(s.makefile().readline())' {} {}",b.port,file.display()));
            assert_eq!(rc, 0);
            let code = response
                .split_whitespace()
                .nth(1)
                .unwrap_or("missing")
                .to_string();
            eprintln!("native HTTP {prefix}{method} {path}: {code}");
            if code != "403" {
                failures.push(format!("{prefix}{method} {path}: {code}"));
            }
        }
    }
    assert_eq!(
        b.value("GET", &format!("/api/connections/{id}"), json!({}))["connection"],
        c
    );
    assert_eq!(b.counts(), before);
    assert!(
        failures.is_empty(),
        "operator peer guard failed: {failures:?}"
    );
}

#[test]
fn cad688_concurrent_http_rotate_revoke_cannot_resurrect_connection() {
    let b = Board::new();
    let c = b.create("concurrent-http");
    let id = c["id"].as_str().unwrap();
    let a = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
    let z = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
    let rotate = a.request(
        "POST",
        &format!("/api/connections/{id}/rotate"),
        &json!({"token":SECRET}).to_string(),
    );
    let revoke = z.request("POST", &format!("/api/connections/{id}/revoke"), "{}");
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let results = std::thread::scope(|scope| {
        let one = barrier.clone();
        let two = barrier.clone();
        let port = b.port;
        let r = scope.spawn(move || {
            one.wait();
            common::op::raw(port, &rotate)
        });
        let v = scope.spawn(move || {
            two.wait();
            common::op::raw(port, &revoke)
        });
        (r.join().unwrap(), v.join().unwrap())
    });
    assert!(results.0 .0 == 200 || results.0 .0 >= 400);
    assert_eq!(
        results.1 .0, 200,
        "revoke must succeed irrespective of rotation order"
    );
    assert!(!results.0 .2.contains(SECRET));
    assert!(!results.1 .2.contains(SECRET));
    assert!(b.operator("GET", &format!("/api/connections/{id}"), "").0 >= 400);
    let replacement = b.create("concurrent-http");
    assert_ne!(replacement["id"], id);
    assert!(
        b.operator(
            "POST",
            &format!("/api/connections/{id}/rotate"),
            &json!({"token":SECRET}).to_string()
        )
        .0 >= 400
    );
    assert_eq!(
        b.value(
            "GET",
            &format!("/api/connections/{}", replacement["id"].as_str().unwrap()),
            json!({})
        )["connection"],
        replacement
    );
}
