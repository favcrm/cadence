//! CAD-768 HTTP peer for per-installation app customer records.
//!
//! Adversarial-first: these tests name the guard before the route
//! exists. An authorized operator gets bounded list/get/create/update
//! over HTTP with expected-revision CAS matching RPC semantics; an
//! agent caller, a detached child, forged actor/install/context/project
//! fields, cross-install/context probes, stale/concurrent writes and a
//! wrong verb/path matrix are all refused without mutation or leak.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_member_pane, test_port, LaneShell, PortLease, TestDaemon};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

const PROFILE_A: &str = r#"{"schema":1,"display_name":"Amina Diallo","email":"amina@example.com","tags":["vip"],"consent":{"email":"granted"}}"#;
const PROFILE_B: &str = r#"{"schema":1,"display_name":"Boris Feld","email":"boris@example.com","tags":[],"consent":{"email":"denied"}}"#;

struct Board {
    root: tempfile::TempDir,
    pm_dir: PathBuf,
    daemon: TestDaemon,
    port: u16,
    _port_lease: PortLease,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    install: String,
    context_id: String,
}

impl Board {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let pm_dir = pm.dir.clone();
        let mut opts = daemon_opts();
        opts.test_seam = false;
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let daemon = TestDaemon::start_opts(opts);
        let source = root.path().join("source");
        for name in [
            "app.md",
            "workflows/blog-post.md",
            "rubrics/blog.md",
            "templates/brief.md",
            "templates/post.md",
        ] {
            let destination = source.join(name);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::copy(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("apps/blog-post")
                    .join(name),
                &destination,
            )
            .unwrap();
        }
        let installed = daemon
            .operator_rpc("app_workspace_install", json!({"source": source}))
            .unwrap();
        let install = installed["install_id"].as_str().unwrap().to_owned();
        let context = daemon
            .operator_rpc(
                "app_context_create",
                json!({"install_id": install, "label": "Client", "input_defaults": {}, "request_id": "ctx-http-1"}),
            )
            .unwrap()["context"]
            .clone();
        let context_id = context["id"].as_str().unwrap().to_owned();
        let lease = test_port();
        let port = lease.port;
        let stop = Arc::new(AtomicBool::new(false));
        let mut board = Self {
            root,
            pm_dir,
            daemon,
            port,
            _port_lease: lease,
            stop,
            thread: None,
            install,
            context_id,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                Instant::now() < deadline,
                "record board startup deadline exhausted"
            );
            let (startup, ready) = std::sync::mpsc::channel();
            let opts = cadence_agent::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port: board.port,
                stop: Some(board.stop.clone()),
                startup: Some(startup),
                test_seam: false,
                ..Default::default()
            };
            let state = board.daemon.state.clone();
            let pm_dir = board.pm_dir.clone();
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
            if matches!(notification, Ok(Err(std::io::ErrorKind::AddrInUse))) {
                match result {
                    Ok(Err(error)) => eprintln!("record board startup contention: {error}"),
                    unexpected => panic!("record board bind failure returned {unexpected:?}"),
                }
                board.port = board
                    .port
                    .checked_add(1)
                    .filter(|port| *port < 3200)
                    .expect("record board startup exhausted permitted ports");
                board.stop.store(false, Ordering::SeqCst);
            } else {
                panic!("record board startup notification {notification:?}; worker {result:?}");
            }
        }
    }

    fn base(&self) -> String {
        format!(
            "/api/app-installations/{}/contexts/{}/records",
            self.install, self.context_id
        )
    }

    fn operator(&self, method: &str, path: &str, body: &str) -> (u16, String) {
        let session =
            common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &self.daemon.state, self.port);
        let (code, _, body) = common::op::raw(self.port, &session.request(method, path, body));
        (code, body)
    }

    fn value(&self, method: &str, path: &str, body: Value) -> Value {
        let encoded = if method == "GET" {
            String::new()
        } else {
            body.to_string()
        };
        let (code, result) = self.operator(method, path, &encoded);
        assert_eq!(
            code, 200,
            "operator record request {method} {path}: {result}"
        );
        serde_json::from_str(&result).unwrap()
    }

    fn profile(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    fn rpc_show(&self, install: &str, context: &str, record: &str) -> Value {
        self.daemon
            .operator_rpc(
                "app_record_show",
                json!({"install_id": install, "context_id": context, "record_id": record}),
            )
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
                eprintln!("record board worker cleanup after primary panic: {result:?}");
            }
        } else {
            result.unwrap().unwrap();
        }
    }
}

#[test]
fn cad768_http_record_cas_roundtrip_matches_rpc() {
    let b = Board::new();
    let base = b.base();
    let created = b.value(
        "POST",
        &base,
        json!({"record_id": "customer-1", "profile": Board::profile(PROFILE_A)}),
    );
    assert_eq!(created["record"]["revision"], 1);
    assert_eq!(created["record"]["profile"]["display_name"], "Amina Diallo");
    assert!(created["record"]["digest"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(created["record"]["history"].as_array().unwrap().len(), 1);
    assert_eq!(created["record"]["history"][0]["actor"], "operator");

    let show_path = format!("{base}/customer-1");
    let shown = b.value("GET", &show_path, json!({}));
    assert_eq!(shown["record"], created["record"]);

    let listed = b.value("GET", &base, json!({}));
    assert_eq!(listed["records"].as_array().unwrap().len(), 1);
    assert_eq!(listed["records"][0], created["record"]);

    // The HTTP receipt matches the daemon RPC receipt exactly.
    let via_rpc = b.rpc_show(&b.install, &b.context_id, "customer-1");
    assert_eq!(via_rpc["record"], created["record"]);

    let update_path = format!("{show_path}/update");
    let updated = b.value(
        "POST",
        &update_path,
        json!({"expected_revision": 1, "profile": Board::profile(PROFILE_B)}),
    );
    assert_eq!(updated["record"]["revision"], 2);
    assert_ne!(updated["record"]["digest"], created["record"]["digest"]);
    assert_eq!(updated["record"]["history"].as_array().unwrap().len(), 2);
    assert_eq!(updated["record"]["profile"]["display_name"], "Boris Feld");
    let via_rpc2 = b.rpc_show(&b.install, &b.context_id, "customer-1");
    assert_eq!(via_rpc2["record"], updated["record"]);
    assert_eq!(
        b.value("GET", &show_path, json!({}))["record"],
        updated["record"]
    );
}

#[test]
fn cad768_http_forged_fields_refuse_without_mutation_or_leak() {
    let b = Board::new();
    let base = b.base();
    let created = b.value(
        "POST",
        &base,
        json!({"record_id": "customer-1", "profile": Board::profile(PROFILE_A)}),
    );
    let before = b.value("GET", &format!("{base}/customer-1"), json!({}));
    assert_eq!(before["record"], created["record"]);
    let marker = "cad768-private-profile-marker";

    // Forged identity / discovery-link / routing fields in the body are
    // refused by the exact payload grammar — never authority.
    for body in [
        json!({"record_id": "customer-2", "profile": Board::profile(PROFILE_A), "by": "operator"}),
        json!({"record_id": "customer-2", "profile": Board::profile(PROFILE_A), "actor": "operator"}),
        json!({"record_id": "customer-2", "profile": Board::profile(PROFILE_A), "project": "client"}),
        json!({"record_id": "customer-2", "profile": Board::profile(PROFILE_A), "project_link": "client"}),
        json!({"record_id": "customer-2", "profile": Board::profile(PROFILE_A), "workspace": "default"}),
        json!({"record_id": "customer-2", "profile": Board::profile(PROFILE_A), "install_id": b.install}),
        json!({"record_id": "customer-2", "profile": Board::profile(PROFILE_A), "context_id": b.context_id}),
        json!({"record_id": "customer-1", "profile": Board::profile(PROFILE_A), "expected_revision": 1}),
    ] {
        let (code, text) = b.operator("POST", &base, &body.to_string());
        assert_eq!(code, 400, "forged create body accepted: {body}");
        assert!(
            !text.contains(marker),
            "refusal echoed customer content: {text}"
        );
    }
    for body in [
        json!({"expected_revision": 1, "profile": Board::profile(PROFILE_B), "by": "operator"}),
        json!({"expected_revision": 1, "profile": Board::profile(PROFILE_B), "actor": "operator"}),
        json!({"expected_revision": 1, "profile": Board::profile(PROFILE_B), "project": "client"}),
        json!({"expected_revision": 1, "profile": Board::profile(PROFILE_B), "project_link": "client"}),
        json!({"expected_revision": 1, "profile": Board::profile(PROFILE_B), "workspace": "default"}),
        json!({"expected_revision": 1, "profile": Board::profile(PROFILE_B), "record_id": "customer-1"}),
        json!({"expected_revision": 1, "profile": Board::profile(PROFILE_B), "install_id": b.install}),
        json!({"expected_revision": 1, "profile": Board::profile(PROFILE_B), "context_id": b.context_id}),
    ] {
        let (code, _) = b.operator(
            "POST",
            &format!("{}/customer-1/update", base),
            &body.to_string(),
        );
        assert_eq!(code, 400, "forged update body accepted: {body}");
    }
    // Query strings never carry authority.
    for path in [
        format!("{base}?context_id=other"),
        format!("{base}/customer-1?record_id=other"),
        format!("{}/customer-1/update?expected_revision=1", base),
    ] {
        let (code, _) = b.operator("GET", &path, "");
        assert_ne!(code, 200, "query-bearing path accepted: {path}");
    }
    let (code, _) = b.operator("POST", &format!("{base}?x=1"), "{}");
    assert_eq!(code, 400, "query-bearing write accepted");
    // Invalid profiles refuse without echoing the marker.
    for profile in [
        json!({"schema": 1, "display_name": marker, "email": "not-an-email", "consent": {"email": "granted"}}),
        json!({"schema": 1, "display_name": marker, "consent": {"email": "granted"}, "database_path": "/tmp/x"}),
    ] {
        let (code, text) = b.operator(
            "POST",
            &base,
            &json!({"record_id": "customer-bad", "profile": profile}).to_string(),
        );
        assert_eq!(code, 409, "bad profile accepted: {text}");
        assert!(!text.contains(marker), "profile content leaked: {text}");
    }
    // Nothing above mutated the file.
    assert_eq!(
        b.value("GET", &format!("{base}/customer-1"), json!({}))["record"],
        created["record"]
    );
    assert_eq!(b.operator("GET", &format!("{base}/customer-2"), "").0, 409);
    assert_eq!(
        b.operator("GET", &format!("{base}/customer-bad"), "").0,
        409
    );
}

#[test]
fn cad768_http_cross_install_and_cross_context_refuse() {
    let b = Board::new();
    let base = b.base();
    b.value(
        "POST",
        &base,
        json!({"record_id": "customer-1", "profile": Board::profile(PROFILE_A)}),
    );
    // A sibling installation with its own context.
    let second_source = b.root.path().join("second");
    for name in [
        "app.md",
        "workflows/blog-post.md",
        "rubrics/blog.md",
        "templates/brief.md",
        "templates/post.md",
    ] {
        let destination = second_source.join(name);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("apps/blog-post")
                .join(name),
            &destination,
        )
        .unwrap();
    }
    // Distinct app name so the catalog mints a second installation.
    let manifest = second_source.join("app.md");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        manifest,
        text.replace("app: blog-post", "app: blog-post-two"),
    )
    .unwrap();
    let second = b
        .daemon
        .operator_rpc("app_workspace_install", json!({"source": second_source}))
        .unwrap();
    let install_b = second["install_id"].as_str().unwrap().to_string();
    assert_ne!(install_b, b.install);
    let ctx_b = b
        .daemon
        .operator_rpc(
            "app_context_create",
            json!({"install_id": install_b, "label": "Client B", "input_defaults": {}, "request_id": "ctx-http-b"}),
        )
        .unwrap()["context"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ctx_a2 = b
        .daemon
        .operator_rpc(
            "app_context_create",
            json!({"install_id": b.install, "label": "Client A2", "input_defaults": {}, "request_id": "ctx-http-a2"}),
        )
        .unwrap()["context"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let other_base = format!("/api/app-installations/{install_b}/contexts/{ctx_b}/records");

    // Cross-install reads/writes fail in both directions.
    for (method, path, body) in [
        ("GET", format!("{other_base}/customer-1"), String::new()),
        (
            "GET",
            format!(
                "/api/app-installations/{install_b}/contexts/{}/records/customer-1",
                b.context_id
            ),
            String::new(),
        ),
        (
            "GET",
            format!(
                "/api/app-installations/{}/contexts/{ctx_b}/records/customer-1",
                b.install
            ),
            String::new(),
        ),
    ] {
        let (code, _) = b.operator(method, &path, &body);
        assert_ne!(code, 200, "cross-install read admitted at {path}");
    }
    let (code, _) = b.operator(
        "POST",
        &format!("{other_base}/customer-1/update"),
        &json!({"expected_revision": 1, "profile": Board::profile(PROFILE_B)}).to_string(),
    );
    assert_ne!(code, 200, "cross-install write admitted");
    // Cross-context reads/writes/lists fail inside one installation.
    let sibling_show = format!(
        "/api/app-installations/{}/contexts/{ctx_a2}/records/customer-1",
        b.install
    );
    assert_ne!(b.operator("GET", &sibling_show, "").0, 200);
    assert_ne!(
        b.operator(
            "POST",
            &format!("{sibling_show}/update"),
            &json!({"expected_revision": 1, "profile": Board::profile(PROFILE_B)}).to_string(),
        )
        .0,
        200
    );
    let sibling_list = format!(
        "/api/app-installations/{}/contexts/{ctx_a2}/records",
        b.install
    );
    let listed: Value = {
        let (code, text) = b.operator("GET", &sibling_list, "");
        assert_eq!(code, 200);
        serde_json::from_str(&text).unwrap()
    };
    assert!(listed["records"].as_array().unwrap().is_empty());
    // Forged installation and context IDs fail closed.
    for path in [
        format!(
            "/api/app-installations/no-such-install/contexts/{}/records/customer-1",
            b.context_id
        ),
        format!(
            "/api/app-installations/{}/contexts/ctx-no-such-context/records/customer-1",
            b.install
        ),
    ] {
        assert_ne!(b.operator("GET", &path, "").0, 200, "forged scope admitted");
    }
    // The original record is untouched.
    assert_eq!(
        b.value("GET", &format!("{base}/customer-1"), json!({}))["record"]["revision"],
        1
    );
}

#[test]
fn cad768_http_stale_and_concurrent_cas_refuse_without_mutation() {
    let b = Board::new();
    let base = b.base();
    let created = b.value(
        "POST",
        &base,
        json!({"record_id": "customer-1", "profile": Board::profile(PROFILE_A)}),
    );
    let update_path = format!("{base}/customer-1/update");
    let stale = json!({"expected_revision": 7, "profile": Board::profile(PROFILE_B)}).to_string();
    assert_ne!(b.operator("POST", &update_path, &stale).0, 200);
    assert_eq!(
        b.value("GET", &format!("{base}/customer-1"), json!({}))["record"],
        created["record"]
    );
    // Concurrent HTTP updates at the same revision: exactly one wins.
    let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
    let attempts = 6;
    let results = std::thread::scope(|scope| {
        (0..attempts)
            .map(|_| {
                scope.spawn(|| {
                    let wire = session.request(
                        "POST",
                        &update_path,
                        &json!({"expected_revision": 1, "profile": Board::profile(PROFILE_B)})
                            .to_string(),
                    );
                    common::op::raw(b.port, &wire).0 == 200
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        results.iter().filter(|ok| **ok).count(),
        1,
        "concurrent HTTP CAS admitted {results:?}"
    );
    let shown = b.value("GET", &format!("{base}/customer-1"), json!({}));
    assert_eq!(shown["record"]["revision"], 2);
    assert_eq!(shown["record"]["history"].as_array().unwrap().len(), 2);
}

#[test]
fn cad768_http_agent_and_detached_refuse_without_mutation() {
    let b = Board::new();
    let base = b.base();
    let created = b.value(
        "POST",
        &base,
        json!({"record_id": "customer-1", "profile": Board::profile(PROFILE_A)}),
    );
    let show_path = format!("{base}/customer-1");
    let update_path = format!("{show_path}/update");
    let before: Value = b.value("GET", &show_path, json!({}));
    assert_eq!(before["record"], created["record"]);

    let mut lane = LaneShell::spawn(b.root.path());
    plant_member_pane(&b.daemon, "record-http-worker", "claude", None, lane.pid());
    let cases = vec![
        ("GET", base.clone(), String::new()),
        ("GET", show_path.clone(), String::new()),
        (
            "POST",
            base.clone(),
            json!({"record_id": "customer-2", "profile": Board::profile(PROFILE_A)}).to_string(),
        ),
        (
            "POST",
            update_path.clone(),
            json!({"expected_revision": 1, "profile": Board::profile(PROFILE_B)}).to_string(),
        ),
    ];
    let mut failures = Vec::new();
    for prefix in ["", "setsid "] {
        for (method, path, body) in &cases {
            let stolen =
                common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
            let wire = stolen.request_as(method, path, body, "");
            assert!(!wire.contains(cadence_agent::test_seam::AS_HEADER));
            assert!(!wire.contains(cadence_agent::test_seam::TOKEN_HEADER));
            let file = lane.dir.path().join(format!("request-{}.txt", lane.seq));
            std::fs::write(&file, wire).unwrap();
            let (rc, response) = lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));s.sendall(open(sys.argv[2],\"rb\").read());print(s.makefile().readline())' {} {}", b.port, file.display()));
            assert_eq!(rc, 0, "HTTP native process failed");
            let status = response.split_whitespace().nth(1).unwrap_or("missing");
            eprintln!("record HTTP peer prefix={prefix:?} {method} {path}: {status}");
            if status != "403" {
                failures.push(format!("{prefix:?} {method} {path}: {status}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "record HTTP operator peer guard failed: {failures:?}"
    );
    // A sessionless read is refused too (never the operator by default).
    let host = common::op::board_host(b.port);
    let bare = format!("GET {show_path} HTTP/1.0\r\nHost: {host}\r\nContent-Length: 0\r\n\r\n");
    let (code, _, _) = common::op::raw(b.port, &bare);
    assert_eq!(code, 403, "sessionless read admitted");
    // Nothing above mutated the record.
    assert_eq!(
        b.value("GET", &show_path, json!({}))["record"],
        created["record"]
    );
    assert_eq!(b.operator("GET", &format!("{base}/customer-2"), "").0, 409);
}

#[test]
fn cad768_http_verb_path_matrix() {
    let b = Board::new();
    let base = b.base();
    b.value(
        "POST",
        &base,
        json!({"record_id": "customer-1", "profile": Board::profile(PROFILE_A)}),
    );
    let show_path = format!("{base}/customer-1");
    let update_path = format!("{show_path}/update");
    let before = b.value("GET", &show_path, json!({}));

    // Wrong verb on a real route is 405, not a silent miss.
    assert_eq!(
        b.operator(
            "POST",
            &show_path,
            &json!({"expected_revision": 1, "profile": Board::profile(PROFILE_B)}).to_string()
        )
        .0,
        405
    );
    assert_eq!(b.operator("GET", &update_path, "").0, 405);
    // Record-shaped paths outside the installation/context contract 404.
    for (method, path, body) in [
        (
            "GET",
            format!("/api/app-installations/{}/records", b.install),
            String::new(),
        ),
        (
            "POST",
            format!("/api/app-installations/{}/records", b.install),
            "{}".to_string(),
        ),
        ("GET", "/api/app-records".to_string(), String::new()),
        ("GET", format!("{show_path}/update/extra"), String::new()),
        (
            "GET",
            format!(
                "/api/app-installations/{}/contexts/{}/records/",
                b.install, b.context_id
            ),
            String::new(),
        ),
    ] {
        let (code, _, _) = {
            let session =
                common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
            common::op::raw(b.port, &session.request(method, &path, &body))
        };
        assert_eq!(
            code, 404,
            "record path matrix missed {method} {path}: {code}"
        );
    }
    // Traversal never reaches a route: the board refuses the path.
    {
        let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
        let (code, _, _) = common::op::raw(
            b.port,
            &session.request("GET", &format!("{}/../records", base), ""),
        );
        assert_eq!(code, 400, "traversal path admitted: {code}");
    }
    // Oversized bodies never buffer whole.
    let big = "x".repeat(64 * 1024);
    let (code, _) = b.operator(
        "POST",
        &base,
        &json!({"record_id": "customer-big", "profile": {"schema": 1, "display_name": big, "consent": {"email": "granted"}}}).to_string(),
    );
    assert!(
        code == 400 || code == 409 || code == 413,
        "oversized body admitted: {code}"
    );
    assert_eq!(
        b.value("GET", &show_path, json!({}))["record"],
        before["record"]
    );
}

/// CAD-781 list search and pagination through the actual HTTP peer.
///
/// Positive search/pagination over real transport matches RPC
/// semantics; forged or out-of-bounds selectors refuse with a generic
/// error that leaks no customer content, and query strings on
/// non-list routes (including creates) refuse before any mutation.
#[test]
fn cad781_http_list_search_and_pagination_through_peer() {
    let b = Board::new();
    let base = b.base();
    let seed = [
        (
            "customer-s1",
            r#"{"schema":1,"display_name":"Search Alpha One","email":"alpha-one@example.com","tags":["alpha"],"consent":{"email":"granted"}}"#,
        ),
        (
            "customer-s2",
            r#"{"schema":1,"display_name":"Search Beta Two","email":"beta-two@example.com","tags":["beta"],"source":"import","consent":{"email":"denied"}}"#,
        ),
        (
            "customer-s3",
            r#"{"schema":1,"display_name":"Search Gamma Three","email":"gamma-three@example.com","tags":[],"consent":{"email":"unknown"}}"#,
        ),
    ];
    for (id, profile) in seed {
        b.value(
            "POST",
            &base,
            json!({"record_id": id, "profile": Board::profile(profile)}),
        );
    }

    let all = b.value("GET", &base, json!({}));
    assert_eq!(all["records"].as_array().unwrap().len(), 3);
    assert_eq!(all["truncated"], false);
    assert!(all["next_cursor"].is_null());

    // Server-side search narrows to the matching row only.
    let found = b.value("GET", &format!("{base}?query=Beta"), json!({}));
    let rows = found["records"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "customer-s2");

    // Cursor pagination walks the ordered ids without overlap.
    let first = b.value("GET", &format!("{base}?limit=2"), json!({}));
    assert_eq!(first["records"].as_array().unwrap().len(), 2);
    assert_eq!(first["truncated"], true);
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    assert_eq!(cursor, first["records"][1]["id"].as_str().unwrap());
    let second = b.value("GET", &format!("{base}?limit=2&cursor={cursor}"), json!({}));
    let tail = second["records"].as_array().unwrap();
    assert_eq!(tail.len(), 1);
    assert_eq!(second["truncated"], false);
    assert!(second["next_cursor"].is_null());
    assert_ne!(tail[0]["id"], first["records"][0]["id"]);

    // Forged and out-of-bounds selectors refuse without a leak.
    for path in [
        format!("{base}?install_id=other"),
        format!("{base}?by=operator"),
        format!("{base}?project=client"),
        format!("{base}?query=Beta&query=Beta"),
        format!("{base}?limit=0"),
        format!("{base}?limit=101"),
        format!("{base}?limit=many"),
        format!("{base}?cursor=a/b"),
        format!("{base}?query=%ZZ"),
        format!("{}/customer-s1?query=Beta", base),
        format!("{}/customer-s1/update?limit=2", base),
    ] {
        let method = if path.contains("/update?") {
            "POST"
        } else {
            "GET"
        };
        let body = if method == "POST" {
            json!({"expected_revision": 1, "profile": Board::profile(PROFILE_A)}).to_string()
        } else {
            String::new()
        };
        let (code, text) = b.operator(method, &path, &body);
        assert!(
            code == 400 || code == 405,
            "list selector misuse admitted {path}: {code}"
        );
        assert!(
            !text.contains("alpha-one@example.com") && !text.contains("Search Alpha"),
            "selector refusal leaked customer content for {path}"
        );
    }
    // A create behind a query string refuses before any row mutates.
    let (code, _) = b.operator(
        "POST",
        &format!("{base}?query=Beta"),
        &json!({"record_id": "customer-evil", "profile": Board::profile(PROFILE_A)}).to_string(),
    );
    assert_eq!(code, 400, "queried create admitted: {code}");
    assert_eq!(
        b.value("GET", &base, json!({}))["records"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn cad1053_http_consent_grant_needs_operator_and_a_method() {
    let b = Board::new();
    let base = b.base();
    let created = b.value(
        "POST",
        &base,
        json!({"record_id": "customer-1", "profile": Board::profile(PROFILE_B)}),
    );
    let show_path = format!("{base}/customer-1");
    let update_path = format!("{show_path}/update");
    let grant = Board::profile(PROFILE_A);

    // Operator, no method: refused, record unchanged.
    let (code, body) = b.operator(
        "POST",
        &update_path,
        &json!({"expected_revision": 1, "profile": grant}).to_string(),
    );
    assert_eq!(code, 409, "{body}");
    // Forged extra field inside the provenance: refused.
    let (code, _) = b.operator(
        "POST",
        &update_path,
        &json!({"expected_revision": 1, "profile": grant, "consent_provenance": {"method": "written", "actor": "operator"}}).to_string(),
    );
    assert_ne!(code, 200);
    assert_eq!(
        b.value("GET", &show_path, json!({}))["record"],
        created["record"]
    );

    // An agent (plain and detached) with a valid method is refused at
    // the operator peer; nothing is written.
    let mut lane = LaneShell::spawn(b.root.path());
    plant_member_pane(&b.daemon, "consent-http-worker", "claude", None, lane.pid());
    let body = json!({"expected_revision": 1, "profile": grant, "consent_provenance": {"method": "in_person"}}).to_string();
    for prefix in ["", "setsid "] {
        let stolen = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
        let wire = stolen.request_as("POST", &update_path, &body, "");
        let file = lane.dir.path().join(format!("request-{}.txt", lane.seq));
        std::fs::write(&file, wire).unwrap();
        let (rc, response) = lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));s.sendall(open(sys.argv[2],\"rb\").read());print(s.makefile().readline())' {} {}", b.port, file.display()));
        assert_eq!(rc, 0);
        assert_eq!(
            response.split_whitespace().nth(1),
            Some("403"),
            "{prefix:?}: {response}"
        );
    }
    assert_eq!(
        b.value("GET", &show_path, json!({}))["record"],
        created["record"]
    );

    // Operator with a method: lands, and the receipt names it.
    let updated = b.value(
        "POST",
        &update_path,
        json!({"expected_revision": 1, "profile": grant, "consent_provenance": {"method": "web_form", "note": "Footer signup"}}),
    );
    let last = updated["record"]["consent_history"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["state"], "granted");
    assert_eq!(last["method"], "web_form");
    assert_eq!(last["note"], "Footer signup");
    assert_eq!(
        b.rpc_show(&b.install, &b.context_id, "customer-1")["record"],
        updated["record"]
    );
}
