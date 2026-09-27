//! CAD-631 operator-only HTTP run surfaces; private daemon and native peers.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, pi_policy_pm, plant_member_pane, LaneShell, TestDaemon};
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
struct BoardSetup {
    root: tempfile::TempDir,
    pm: Pm,
    daemon: TestDaemon,
}
impl BoardSetup {
    fn new(pi: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let opts = daemon_opts();
        if pi {
            pi_policy_pm(&pm.dir);
            let fixture =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/app-run-pi.py");
            opts.provider_env.set(
                "CADENCE_PI_COMMAND",
                format!("python3 {}", fixture.display()),
            );
        }
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let daemon = TestDaemon::start_opts(opts);
        Self { root, pm, daemon }
    }
}
impl Board {
    fn new() -> Self {
        Self::start(false)
    }
    fn start(pi: bool) -> Self {
        Self::start_prepared(BoardSetup::new(pi), None, |_| {})
    }
    fn start_prepared(
        setup: BoardSetup,
        forced_port: Option<u16>,
        after_probe: impl FnOnce(u16),
    ) -> Self {
        let BoardSetup { root, pm, daemon } = setup;
        let port = forced_port.unwrap_or_else(|| {
            (3110..3200)
                .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
                .unwrap()
        });
        assert!((3110..3200).contains(&port));
        after_probe(port);
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
        let board = Self {
            root,
            daemon,
            port,
            stop,
            thread: Some(thread),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
        board
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
        let result = self.thread.take().unwrap().join();
        if std::thread::panicking() {
            if !matches!(&result, Ok(Ok(()))) {
                eprintln!("board worker cleanup after primary panic: {result:?}");
            }
        } else {
            result.unwrap().unwrap();
        }
    }
}

#[test]
fn cad691_board_readiness_requires_its_own_bound_worker() {
    // Prepare both token domains before releasing the probed port. The hook
    // occupies it with a real board, not a fabricated HTTP response.
    let contender_setup = BoardSetup::new(false);
    let own_setup = BoardSetup::new(false);
    let mut contender = None;
    let b = Board::start_prepared(own_setup, None, |port| {
        let other = Board::start_prepared(contender_setup, Some(port), |_| {});
        assert_eq!(other.port, port);
        assert_eq!(other.operator("GET", "/api/app-runs", "").0, 200);
        eprintln!("CAD-691: owned contender serves its own operator on probed port {port}");
        contender = Some(other);
    });
    let contender = contender.expect("probe hook did not establish its owned contender");
    // Keep this positive first: old TCP-only readiness returns a Board whose
    // own sign-in reaches the contender and fails with HTTP403, before any
    // distinct-port/live-worker assertion can obscure that primary failure.
    assert_eq!(b.operator("GET", "/api/app-runs", "").0, 200);
    assert_ne!(b.port, contender.port, "readiness borrowed another board");
    assert!(
        !b.thread.as_ref().unwrap().is_finished(),
        "readiness returned after its own board worker exited"
    );
    assert_eq!(contender.operator("GET", "/api/app-runs", "").0, 200);
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

#[test]
fn cad631_operator_http_run_management_keeps_approval_explicit() {
    let b = Board::new();
    b.daemon.fixture_rpc("agent_register", json!({"alias":"local-pm","provider":"fake","endpoint_kind":"fake","cwd":b.root.path(),"role":"pm"})).unwrap();
    b.daemon.register_member("local-writer", "local-pm");
    b.daemon.register_member("local-reviewer", "local-pm");
    let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("apps/local-content");
    let installed = b
        .daemon
        .operator_rpc("app_workspace_install", json!({"source":source}))
        .unwrap();
    let install = installed["install_id"].as_str().unwrap();
    let digest = installed["digest"].as_str().unwrap();
    let approval_path = format!("/api/app-installations/{install}/approve");
    let decision = json!({"digest":digest}).to_string();
    let (status, body) = b.operator("POST", &approval_path, &decision);
    assert_eq!(status, 200, "capability approval: {body}");
    let created_body = json!({"install_id":install,"workflow":"draft","inputs":{"subject":"Lunch menu","source":"Lunch is served noon to 3pm.","writer":"local-writer","reviewer":"local-reviewer"},"request_id":"http-local-1","owner_pm":"local-pm"});
    let (status, body) = b.operator("POST", "/api/app-runs", &created_body.to_string());
    assert_eq!(status, 200, "create local run: {body}");
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = created["id"].as_str().unwrap();
    assert_eq!(created["state"], "awaiting_approval");
    assert!(created["project_link"].is_null());
    assert!(
        cadence_agent::issue::project::list(&b.root.path().join("pm"))
            .unwrap()
            .is_empty()
    );
    let db = rusqlite::Connection::open(b.daemon.state.join("cadence.sqlite3")).unwrap();
    let grants: i64 = db
        .query_row("SELECT count(*) FROM app_grants", [], |row| row.get(0))
        .unwrap();
    assert_eq!(grants, 0, "local run created provider grants");
    let kickoffs: i64 = db
        .query_row(
            "SELECT count(*) FROM messages WHERE source IN ('job_dispatch','app_run_dispatch')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        kickoffs, 0,
        "create dispatched without explicit execution approval"
    );
    let (status, repeated) = b.operator("POST", "/api/app-runs", &created_body.to_string());
    assert_eq!(status, 200, "idempotent create: {repeated}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&repeated).unwrap()["id"],
        id
    );
    let (status, shown) = b.operator("GET", &format!("/api/app-runs/{id}"), "");
    assert_eq!(status, 200, "show: {shown}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&shown).unwrap()["snapshot_digest"],
        created["snapshot_digest"]
    );
    let (status, listed) = b.operator("GET", &format!("/api/app-runs?install_id={install}"), "");
    assert_eq!(status, 200, "filtered list: {listed}");
    let listed: serde_json::Value = serde_json::from_str(&listed).unwrap();
    assert_eq!(listed["runs"].as_array().unwrap().len(), 1);
    assert_eq!(listed["runs"][0]["id"], id);
    let mut forged = created_body.clone();
    forged["operator"] = json!(true);
    assert_eq!(
        b.operator("POST", "/api/app-runs", &forged.to_string()).0,
        400
    );
    let (status, body) = b.operator(
        "POST",
        &format!("/api/app-runs/{id}/approve"),
        &json!({"digest":created["snapshot_digest"]}).to_string(),
    );
    assert_eq!(status, 200, "execution approval: {body}");
    let (status, body) = b.operator("POST", &format!("/api/app-runs/{id}/cancel"), "{}");
    assert_eq!(status, 200, "cancel: {body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["state"],
        "cancelled"
    );
    let (status, body) = b.operator(
        "POST",
        &format!("/api/app-installations/{install}/revoke"),
        &decision,
    );
    assert_eq!(status, 200, "revoke: {body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["approved"],
        false
    );
}

#[test]
fn cad631_completed_pi_artifact_http_is_json_and_native_peer_scoped() {
    use sha2::{Digest, Sha256};
    const OWNER: &str = "http-local-pm";
    const WRITER: &str = "http-local-writer";
    const REVIEWER: &str = "http-local-reviewer";
    const DRAFT: &str = "Lunch is served from noon to 3pm.";
    let b = Board::start(true);
    b.daemon.fixture_rpc("agent_register", json!({"alias":OWNER,"provider":"inbox","endpoint_kind":"inbox","role":"pm","cwd":b.daemon.dir.path()})).unwrap();
    for alias in [WRITER, REVIEWER] {
        b.daemon.register_pi(
            alias,
            json!({"upstream":OWNER,"model":"fake/model-1","effort":"high"}),
        );
        b.daemon.wait_agent(alias, "idle", 20);
    }
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("apps/local-content");
    let installed = b
        .daemon
        .operator_rpc("app_workspace_install", json!({"source":source}))
        .unwrap();
    let install = installed["install_id"].as_str().unwrap();
    b.daemon
        .operator_rpc(
            "app_local_install_approve",
            json!({"install_id":install,"digest":installed["digest"]}),
        )
        .unwrap();
    let created = b.daemon.operator_rpc("app_run_create", json!({"install_id":install,"workflow":"draft","inputs":{"subject":"Lunch menu","source":DRAFT,"writer":WRITER,"reviewer":REVIEWER},"request_id":"http-artifact-1","owner_pm":OWNER})).unwrap();
    let run_id = created["id"].as_str().unwrap();
    b.daemon
        .operator_rpc(
            "app_run_approve",
            json!({"run_id":run_id,"digest":created["snapshot_digest"]}),
        )
        .unwrap();
    b.daemon
        .operator_rpc("app_run_dispatch", json!({"run_id":run_id}))
        .unwrap();
    std::fs::write(b.daemon.state.join("app-run-release-writer"), "release").unwrap();
    let deadline = Instant::now() + Duration::from_secs(40);
    let completed = loop {
        let run = b
            .daemon
            .operator_rpc("app_run_show", json!({"run_id":run_id}))
            .unwrap();
        if run["state"] == "succeeded" {
            break run;
        }
        assert!(
            !matches!(run["state"].as_str(), Some("failed" | "cancelled")),
            "provider run failed: {run}"
        );
        assert!(
            Instant::now() < deadline,
            "provider run did not complete: {run}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let artifact_id = completed["artifacts"][0]["id"].as_str().unwrap();
    let path = format!("/api/app-run-artifacts/{artifact_id}");
    let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
    let (status, headers, body) = common::op::raw(b.port, &session.request("GET", &path, ""));
    assert_eq!(status, 200, "completed artifact HTTP: {body}");
    let headers = headers.to_ascii_lowercase();
    assert!(
        headers.contains("content-type: application/json"),
        "unsafe artifact content type: {headers}"
    );
    assert!(
        headers.contains("x-content-type-options: nosniff"),
        "artifact lacks nosniff: {headers}"
    );
    let receipt: serde_json::Value =
        serde_json::from_str(&body).expect("artifact must be valid JSON");
    assert_eq!(receipt["id"], artifact_id);
    assert_eq!(receipt["text"], DRAFT);
    assert_eq!(receipt["media_type"], "text/markdown");
    assert_eq!(receipt["size"], DRAFT.len());
    assert_eq!(
        receipt["digest"],
        format!("sha256:{:x}", Sha256::digest(DRAFT.as_bytes()))
    );
    assert_eq!(receipt["digest"], completed["artifacts"][0]["digest"]);
    b.daemon
        .operator_rpc(
            "app_local_install_revoke",
            json!({"install_id":install,"digest":installed["digest"]}),
        )
        .unwrap();
    let workflow = b
        .root
        .path()
        .join("pm/.apps/installations")
        .join(install)
        .join("bundle/workflows/draft.md");
    let mut text = std::fs::read_to_string(&workflow).unwrap();
    text.push_str("\nHistorical outputs remain audit evidence after this valid edit.\n");
    std::fs::write(workflow, text).unwrap();
    let current = b
        .daemon
        .operator_rpc("app_workspace_show", json!({"install_id":install}))
        .unwrap();
    assert_ne!(current["digest"], installed["digest"]);
    let (status, body) = b.operator("GET", &path, "");
    assert_eq!(
        status, 200,
        "operator historical artifact HTTP must survive revoke and bundle replacement: {body}"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        receipt
    );
    std::fs::remove_dir_all(
        b.root
            .path()
            .join("pm/.apps/installations")
            .join(install)
            .join("bundle"),
    )
    .unwrap();
    let (status, body) = b.operator("GET", &path, "");
    assert_eq!(
        status, 200,
        "operator historical artifact HTTP must survive bundle removal: {body}"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        receipt
    );

    // Test authority against an existing completed object, not an invented ID.
    let mut lane = LaneShell::spawn(b.root.path());
    plant_member_pane(
        &b.daemon,
        "completed-artifact-http-worker",
        "claude",
        None,
        lane.pid(),
    );
    // The board has no capture/probe relay: never invent a file/session fallback.
    // Public metadata/chat reads must use the daemon's redacted read surfaces.
    let surfaces = vec![
        (path.clone(), 403),
        (format!("/api/agents/{WRITER}/capture"), 400),
        (format!("/api/agents/{WRITER}/probe"), 400),
        (format!("/api/agents/{WRITER}"), 200),
        ("/api/agents".to_string(), 200),
        (format!("/api/threads/{WRITER}?tail=1"), 200),
    ];
    for (surface, expected) in &surfaces[1..] {
        let (status, body) = b.operator("GET", surface, "");
        assert_eq!(
            status, *expected,
            "board transcript surface {surface}: {body}"
        );
        assert!(
            !body.contains(DRAFT),
            "board metadata exposed private app material: {surface}"
        );
    }
    let mut failures = Vec::new();
    for prefix in ["", "setsid "] {
        for (surface, expected) in &surfaces {
            let stolen =
                common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
            let wire = stolen.request_as("GET", surface, "", "");
            assert!(!wire.contains(cadence_agent::test_seam::AS_HEADER));
            assert!(!wire.contains(cadence_agent::test_seam::TOKEN_HEADER));
            assert!(wire.contains(&stolen.cookie));
            assert!(wire.contains(&stolen.key));
            let file = lane.dir.path().join(format!("artifact-{}.txt", lane.seq));
            std::fs::write(&file, wire).unwrap();
            let (rc, response) = lane.run(&format!("{prefix}python3 -c 'import json,socket,sys;s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));s.settimeout(10);s.sendall(open(sys.argv[2],\"rb\").read());print(json.dumps(s.makefile().read()))' {} {}", b.port, file.display()));
            assert_eq!(rc, 0);
            let response: String = serde_json::from_str(response.trim()).unwrap();
            let status = response.split_whitespace().nth(1).unwrap_or("missing");
            eprintln!("completed native HTTP prefix={prefix:?} path={surface} status={status}");
            if status != expected.to_string() {
                failures.push(format!(
                    "{prefix:?} {surface}: {status}, expected {expected}"
                ));
            }
            if response.contains(DRAFT) {
                failures.push(format!(
                    "{prefix:?} {surface}: disclosed private app material"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "completed artifact native access failures: {failures:?}"
    );
    let retained = b
        .daemon
        .operator_rpc("app_run_show", json!({"run_id":run_id}))
        .unwrap();
    assert_eq!(retained["state"], "succeeded");
    assert_eq!(retained["artifacts"], completed["artifacts"]);
}
