//! CAD-787 operator-only HTTP relay for frozen publish intents.
//!
//! The board relays schedule/cancel/show/list to the daemon
//! `social_publish_*` RPC. claim_due/reconcile/report stay off-board
//! (404). Backend refusal codes pass through verbatim — the relay never
//! re-validates. No live provider call: tests run on the test-seam board
//! against the real store with no publish fixtures, so schedule attempts
//! refuse with backend codes. No real post, no paid call.
#![allow(clippy::disallowed_methods)]
mod common;
use common::{daemon_opts, plant_member_pane, test_port, LaneShell, PortLease, TestDaemon};
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
    _port_lease: PortLease,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
}
impl Board {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let pm = cadence_agent::issue::Pm::init(&root.path().join("pm")).unwrap();
        let opts = daemon_opts();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let daemon = TestDaemon::start_opts(opts);
        let lease = test_port();
        let port = lease.port;
        let stop = Arc::new(AtomicBool::new(false));
        let mut board = Self {
            root,
            daemon,
            port,
            _port_lease: lease,
            stop,
            thread: None,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                Instant::now() < deadline,
                "board startup deadline exhausted"
            );
            let (startup, ready) = std::sync::mpsc::channel();
            let opts = cadence_agent::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port: board.port,
                stop: Some(board.stop.clone()),
                startup: Some(startup),
                test_seam: cfg!(feature = "test-seam"),
                ..Default::default()
            };
            let state = board.daemon.state.clone();
            let pm_dir = pm.dir.clone();
            board.thread = Some(std::thread::spawn(move || {
                cadence_agent::ui::serve(&state, &pm_dir, &opts)
            }));
            let notification = match deadline.checked_duration_since(Instant::now()) {
                Some(remaining) if !remaining.is_zero() => ready.recv_timeout(remaining),
                _ => Err(std::sync::mpsc::RecvTimeoutError::Timeout),
            };
            if matches!(notification, Ok(Ok(()))) {
                return board;
            }
            board.stop.store(true, Ordering::SeqCst);
            let result = board.thread.take().unwrap().join();
            if result.is_err() {
                panic!("board startup notification {notification:?}; worker {result:?}");
            }
            board.stop.store(false, Ordering::SeqCst);
        }
    }
    fn operator(&self, method: &str, path: &str, body: &str) -> (u16, String) {
        let session =
            common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &self.daemon.state, self.port);
        let (code, _, body) = common::op::raw(self.port, &session.request(method, path, body));
        (code, body)
    }
    fn anonymous(&self, method: &str, path: &str, body: &str) -> (u16, String) {
        // Host carries the port: without it the board answers 421 before
        // any session check, which would prove nothing about gating.
        let host = format!("127.0.0.1:{}", self.port);
        let wire = if body.is_empty() {
            format!("{method} {path} HTTP/1.1\r\nhost: {host}\r\nconnection: close\r\n\r\n")
        } else {
            format!(
                "{method} {path} HTTP/1.1\r\nhost: {host}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
        };
        let (code, _, text) = common::op::raw(self.port, &wire);
        (code, text)
    }
    fn intent_rows(&self) -> i64 {
        let db = rusqlite::Connection::open(self.daemon.state.join("cadence.sqlite3")).unwrap();
        db.query_row("SELECT count(*) FROM social_publish_intents", [], |row| {
            row.get(0)
        })
        .unwrap()
    }
}
impl Drop for Board {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let Some(thread) = self.thread.take() else {
            return;
        };
        let result = thread.join();
        if std::thread::panicking() {
            if !matches!(&result, Ok(Ok(()))) {
                eprintln!("board worker cleanup after primary panic: {result:?}");
            }
        } else {
            result.unwrap().unwrap();
        }
    }
}

const ROUTES: [(&str, &str, &str); 4] = [
    ("POST", "/api/social-publishes", "{}"),
    ("POST", "/api/social-publishes/intent-a/cancel", "{}"),
    ("GET", "/api/social-publishes/intent-a", ""),
    ("GET", "/api/social-publishes?install_id=install-a", ""),
];

fn schedule_body() -> String {
    json!({
        "request_id": "request-a", "install_id": "install-a",
        "run_id": "run-a", "effect_id": "effect-a",
        "artifact_id": "artifact-a", "bundle_digest": "bundle-a",
        "slot": "publication", "destination_id": "17841400008460056",
        "toolkit": "instagram", "grant_id": "grant-a", "approval_id": "op-a",
        "due_epoch": 1790601000, "timezone": "Asia/Hong_Kong",
    })
    .to_string()
}

#[test]
fn cad787_relay_operator_list_empty_and_show_missing() {
    let b = Board::new();
    let (status, body) = b.operator("GET", "/api/social-publishes?install_id=install-a", "");
    assert_eq!(status, 200, "operator list route missing: {body}");
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("list returns JSON");
    assert_eq!(
        parsed,
        json!({"intents": []}),
        "empty scope lists no intents: {body}"
    );
    let (status, body) = b.operator("GET", "/api/social-publishes/nope", "");
    assert_eq!(status, 400, "missing intent did not refuse: {body}");
    assert!(
        body.contains("does not exist"),
        "backend message lost: {body}"
    );
}

#[test]
fn cad787_relay_sessionless_unproven_detached_refused() {
    let b = Board::new();
    assert_eq!(
        b.operator("GET", "/api/social-publishes?install_id=install-a", "")
            .0,
        200
    );
    // Sessionless: no cookie, no key, every route.
    for (method, path, body) in ROUTES {
        let (status, text) = b.anonymous(method, path, body);
        assert_eq!(status, 403, "sessionless accepted {method} {path}: {text}");
    }
    // Unproven agent: a planted worker pane acts without operator proof.
    let mut lane = LaneShell::spawn(b.root.path());
    plant_member_pane(&b.daemon, "publish-http-worker", "claude", None, lane.pid());
    let before = b.intent_rows();
    for (method, path, body) in ROUTES {
        let stolen = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
        let wire = stolen.request_as(method, path, body, "");
        assert!(!wire.contains(cadence_agent::test_seam::AS_HEADER));
        assert!(!wire.contains(cadence_agent::test_seam::TOKEN_HEADER));
        for prefix in ["", "setsid "] {
            let file = lane.dir.path().join(format!("request-{}.txt", lane.seq));
            std::fs::write(&file, wire.clone()).unwrap();
            let (rc, response) = lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));s.sendall(open(sys.argv[2],\"rb\").read());print(s.makefile().readline())' {} {}", b.port, file.display()));
            assert_eq!(rc, 0);
            let status = response.split_whitespace().nth(1).unwrap_or("missing");
            eprintln!("native publish HTTP prefix={prefix:?} {method} {path}: {status}");
            assert_eq!(
                status, "403",
                "unproven/detached accepted {prefix:?} {method} {path}"
            );
        }
    }
    assert_eq!(b.intent_rows(), before, "rejected calls changed intents");
}

#[test]
fn cad787_relay_forged_bodies_fail_closed_with_backend_codes() {
    let b = Board::new();
    for body in [
        json!({"operator": true}),
        json!({"request_id": "r", "operator": true, "destination_id": "x"}),
        json!({"request_id": "r", "run_id": "a", "effect_id": "e", "artifact_id": "a", "bundle_digest": "b", "slot": "publication", "destination_id": "juicysuite_crm", "toolkit": "instagram", "grant_id": "g", "approval_id": "o", "due_epoch": "tomorrow", "timezone": "Asia/Hong_Kong"}),
        json!({"request_id": "r", "install_id": "i", "run_id": "a", "effect_id": "e", "artifact_id": "a", "bundle_digest": "b", "slot": "publication", "destination_id": "d", "toolkit": "instagram", "grant_id": "g", "approval_id": "o", "due_epoch": 1, "timezone": "Asia/Hong_Kong", "caption_digest": "forged"}),
    ] {
        let (status, text) = b.operator("POST", "/api/social-publishes", &body.to_string());
        assert_eq!(status, 400, "forged body accepted: {body}");
        assert!(
            text.contains("unknown field")
                || text.contains("invalid type")
                || text.contains("code"),
            "refusal unexplained: {text}"
        );
    }
    // A well-formed schedule with no fixtures reaches the backend, whose
    // refusal surfaces verbatim (run resolution precedes approval
    // validation — backend-owned order; grant_approval itself is pinned
    // adversarially at the store level and gated client-side at 120).
    let mut oversize: serde_json::Value = serde_json::from_str(&schedule_body()).unwrap();
    oversize["approval_id"] = json!("o".repeat(121));
    let (status, text) = b.operator("POST", "/api/social-publishes", &oversize.to_string());
    assert_eq!(status, 400, "oversize approval accepted");
    assert!(
        text.contains("unknown app run") && text.contains("\"code\""),
        "backend refusal not passed through: {text}"
    );
    // Cancel of a missing intent refuses with the backend message.
    let (status, text) = b.operator(
        "POST",
        "/api/social-publishes/nope/cancel",
        r#"{"install_id":"install-a"}"#,
    );
    assert_eq!(status, 400, "cancel of nothing accepted: {text}");
    assert!(
        text.contains("cancelled") || text.contains("does not exist"),
        "backend message lost: {text}"
    );
}

#[test]
fn cad787_relay_dispatch_routes_404() {
    let b = Board::new();
    for (method, path) in [
        ("POST", "/api/social-publishes/claim-due"),
        ("POST", "/api/social-publishes/reconcile"),
        ("POST", "/api/social-publishes/report"),
        ("POST", "/api/social-publishes/intent-a/report"),
        ("POST", "/api/social-publishes/intent-a/claim-due"),
        ("POST", "/api/social-publishes/intent-a/reconcile"),
    ] {
        let (status, text) = b.operator(method, path, "{}");
        assert_eq!(
            status, 404,
            "dispatch route served: {method} {path}: {text}"
        );
    }
}

#[test]
fn cad787_relay_http_rpc_parity_on_refusal() {
    let b = Board::new();
    // Same well-formed schedule through the native RPC and the HTTP door
    // must refuse identically: no fixtures exist, so the backend refuses
    // with its own code through both doors.
    let params: serde_json::Value = serde_json::from_str(&schedule_body()).unwrap();
    let native = b
        .daemon
        .fixture_rpc("social_publish_schedule", params.clone())
        .expect_err("schedule without fixtures must refuse");
    let (status, text) = b.operator("POST", "/api/social-publishes", &params.to_string());
    assert_eq!(
        status, 400,
        "HTTP door diverged from native refusal: {text}"
    );
    assert!(
        text.contains(native.code().unwrap_or("invalid_request")),
        "backend code not passed through: native={native:?} http={text}"
    );
    assert_eq!(b.intent_rows(), 0, "refused schedule stored an intent");
}
