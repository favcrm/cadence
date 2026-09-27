//! CAD-631 operator-only HTTP run surfaces; private daemon and native peers.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_member_pane, LaneShell, TestDaemon};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

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
        let opts = daemon_opts();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
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
        (code, body)
    }
}
impl Drop for Board {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap().unwrap();
    }
}

#[test]
fn cad631_http_operator_list_and_strict_schema() {
    let b = Board::new();
    let (status, body) = b.operator("GET", "/api/app-runs", "");
    assert_eq!(status, 200, "operator list route missing: {body}");
    assert!(serde_json::from_str::<serde_json::Value>(&body).is_ok());
    for path in [
        "/api/app-runs?install_id=a&install_id=b",
        "/api/app-runs?agent=operator",
        "/api/app-runs?install_id=",
    ] {
        assert_eq!(b.operator("GET", path, "").0, 400, "query accepted: {path}");
    }
    for body in [
        json!({"agent":"operator"}),
        json!({"install_id":"a","workflow":"w","inputs":{},"request_id":"r","owner_pm":"pm","operator":true}),
        json!({"install_id":"a","workflow":"w","inputs":{"x":1},"request_id":"r","owner_pm":"pm"}),
    ] {
        assert_eq!(
            b.operator("POST", "/api/app-runs", &body.to_string()).0,
            400,
            "forged or untyped body accepted"
        );
    }
    assert_eq!(
        b.operator(
            "POST",
            "/api/app-runs/run-a/cancel",
            "{\"run_id\":\"other\"}"
        )
        .0,
        400
    );
}

#[test]
fn cad631_actual_agent_and_setsid_http_run_routes_refuse_stolen_session() {
    let b = Board::new();
    // Operator positive before negatives rules out absent list RPC/route confidence.
    assert_eq!(b.operator("GET", "/api/app-runs", "").0, 200);
    let mut lane = LaneShell::spawn(b.root.path());
    plant_member_pane(&b.daemon, "app-http-worker", "claude", None, lane.pid());
    let digest = "0".repeat(64);
    let create = json!({"install_id":"install-a","workflow":"local","inputs":{},"request_id":"request-a","owner_pm":"pm"}).to_string();
    let cases = vec![
        ("POST", "/api/app-runs", create),
        ("POST", "/api/app-runs", json!({"install_id":"install-a","workflow":"local","inputs":{},"request_id":"forged-a","owner_pm":"pm","agent":"operator","operator":true}).to_string()),
        ("GET", "/api/app-runs", String::new()),
        ("GET", "/api/app-runs/run-a", String::new()),
        (
            "POST",
            "/api/app-runs/run-a/approve",
            json!({"digest":digest}).to_string(),
        ),
        ("POST", "/api/app-runs/run-a/cancel", "{}".into()),
        ("POST", "/api/app-runs/run-a/dispatch", "{}".into()),
        (
            "POST",
            "/api/app-installations/install-a/approve",
            json!({"digest":digest}).to_string(),
        ),
        (
            "POST",
            "/api/app-installations/install-a/revoke",
            json!({"digest":digest}).to_string(),
        ),
        ("GET", "/api/app-run-artifacts/artifact-a", String::new()),
    ];
    let db = rusqlite::Connection::open(b.daemon.state.join("cadence.sqlite3")).unwrap();
    let before: i64 = db
        .query_row("SELECT count(*) FROM app_runs", [], |row| row.get(0))
        .unwrap();
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
            let file = lane.dir.path().join(format!("request-{}.txt", lane.seq));
            std::fs::write(&file, wire).unwrap();
            let (rc, response) = lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));s.sendall(open(sys.argv[2],\"rb\").read());print(s.makefile().readline())' {} {}", b.port, file.display()));
            assert_eq!(rc, 0);
            let status = response.split_whitespace().nth(1).unwrap_or("missing");
            eprintln!("native app HTTP prefix={prefix:?} {method} {path}: {status}");
            if status != "403" {
                failures.push(format!("{prefix:?} {method} {path}: {status}"));
            }
        }
    }
    let after: i64 = db
        .query_row("SELECT count(*) FROM app_runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        before, after,
        "native rejected calls changed persisted runs"
    );
    assert!(
        failures.is_empty(),
        "native app HTTP authority failures: {failures:?}"
    );
}
