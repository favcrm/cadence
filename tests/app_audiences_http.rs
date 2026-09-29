//! CAD-780 live board HTTP peer for saved segments and frozen audiences.
//!
//! Adversarial-first against a real board: an authorized operator
//! gets bounded segment/exclusion/suppression/audience actions over
//! HTTP with receipts matching daemon RPC; an agent caller, a
//! detached child, forged actor/install/context/project fields,
//! cross-install/context probes, stale/concurrent writes and a wrong
//! verb/path matrix are all refused without mutation or leak. The
//! board is at least as strict as daemon RPC.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_member_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

const PROFILE_A: &str = r#"{"schema":1,"display_name":"Amina Diallo","email":"amina@example.com","tags":["vip"],"consent":{"email":"granted"}}"#;
const PROFILE_B: &str = r#"{"schema":1,"display_name":"Boris Feld","email":"boris@example.com","tags":[],"consent":{"email":"denied"}}"#;
const PROFILE_C: &str = r#"{"schema":1,"display_name":"Cleo Boone","email":"cleo@example.com","tags":[],"consent":{"email":"granted"}}"#;

const VIP: &str = r#"[{"field":"tag","op":"eq","value":"vip"}]"#;

struct Board {
    root: tempfile::TempDir,
    pm_dir: PathBuf,
    daemon: TestDaemon,
    port: u16,
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
                json!({"install_id": install, "label": "Client", "input_defaults": {}, "request_id": "ctx-aud-http-1"}),
            )
            .unwrap()["context"]
            .clone();
        let context_id = context["id"].as_str().unwrap().to_owned();
        for (id, profile) in [
            ("customer-a", PROFILE_A),
            ("customer-b", PROFILE_B),
            ("customer-c", PROFILE_C),
        ] {
            let profile: Value = serde_json::from_str(profile).unwrap();
            daemon
                .operator_rpc(
                    "app_record_create",
                    json!({"install_id": install, "context_id": context_id, "record_id": id, "profile": profile}),
                )
                .unwrap();
        }
        let port = (3110..3200)
            .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let mut board = Self {
            root,
            pm_dir,
            daemon,
            port,
            stop,
            thread: None,
            install,
            context_id,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                Instant::now() < deadline,
                "audience board startup deadline exhausted"
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
                    Ok(Err(error)) => eprintln!("audience board startup contention: {error}"),
                    unexpected => panic!("audience board bind failure returned {unexpected:?}"),
                }
                board.port = board
                    .port
                    .checked_add(1)
                    .filter(|port| *port < 3200)
                    .expect("audience board startup exhausted permitted ports");
                board.stop.store(false, Ordering::SeqCst);
            } else {
                panic!("audience board startup notification {notification:?}; worker {result:?}");
            }
        }
    }

    fn base(&self) -> String {
        format!(
            "/api/app-installations/{}/contexts/{}",
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
            "operator audience request {method} {path}: {result}"
        );
        serde_json::from_str(&result).unwrap()
    }

    fn predicates() -> Value {
        serde_json::from_str(VIP).unwrap()
    }

    fn rpc_preview(&self, base: Value) -> Value {
        self.daemon
            .operator_rpc(
                "app_audience_preview",
                json!({"install_id": self.install, "context_id": self.context_id, "base": base}),
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
                eprintln!("audience board worker cleanup after primary panic: {result:?}");
            }
        } else {
            result.unwrap().unwrap();
        }
    }
}

#[test]
fn cad780_http_audience_roundtrip_matches_rpc() {
    let b = Board::new();
    let base = b.base();
    let saved = b.value(
        "POST",
        &format!("{base}/segments"),
        json!({"segment_id": "seg-vip", "name": "VIP", "predicates": Board::predicates()}),
    );
    assert_eq!(saved["segment"]["revision"], 1);
    let shown = b.value("GET", &format!("{base}/segments/seg-vip"), json!({}));
    assert_eq!(shown["segment"], saved["segment"]);
    let listed = b.value("GET", &format!("{base}/segments/list"), json!({}));
    assert_eq!(listed["segments"].as_array().unwrap().len(), 1);

    // The HTTP receipt matches the daemon RPC receipt exactly.
    let via_rpc = b
        .daemon
        .operator_rpc(
            "app_segment_show",
            json!({"install_id": b.install, "context_id": b.context_id, "segment_id": "seg-vip"}),
        )
        .unwrap();
    assert_eq!(via_rpc["segment"], saved["segment"]);

    let excl = b.value(
        "POST",
        &format!("{base}/exclusions"),
        json!({"list_id": "ex-hold", "name": "Hold", "member_ids": ["customer-c"]}),
    );
    assert_eq!(excl["exclusion"]["revision"], 1);

    let preview = b.value(
        "POST",
        &format!("{base}/audience/preview"),
        json!({"base": {"mode": "all"}}),
    );
    // Three rows minus denied = two; sample stays bounded.
    assert_eq!(preview["base_count"], 3);
    assert_eq!(preview["final_count"], 2);
    assert!(preview["sample"].as_array().unwrap().len() <= 10);
    assert_eq!(preview, b.rpc_preview(json!({"mode": "all"})));

    let prepared = b.value(
        "POST",
        &format!("{base}/audience/prepares"),
        json!({"freeze_id": "freeze-1", "base": {"mode": "all"}, "max_recipients": 50}),
    );
    assert_eq!(prepared["freeze"]["final_count"], 2);
    let frozen = b.value(
        "GET",
        &format!("{base}/audience/prepares/freeze-1"),
        json!({}),
    );
    assert_eq!(frozen["valid"], true);
    assert_eq!(frozen["freeze"]["digest"], prepared["freeze"]["digest"]);
}

#[test]
fn cad780_http_forged_bodies_refuse_without_mutation_or_leak() {
    let b = Board::new();
    let base = b.base();
    let created = b.value(
        "POST",
        &format!("{base}/segments"),
        json!({"segment_id": "seg-vip", "name": "VIP", "predicates": Board::predicates()}),
    );
    let before = b.value("GET", &format!("{base}/segments/seg-vip"), json!({}));
    assert_eq!(before["segment"], created["segment"]);
    let marker = "cad780-private-segment-marker";

    // Forged identity / discovery-link / routing / scope fields in
    // the body refuse at the transport grammar — never authority.
    for body in [
        json!({"segment_id": "seg-2", "name": "X", "predicates": Board::predicates(), "by": "operator"}),
        json!({"segment_id": "seg-2", "name": "X", "predicates": Board::predicates(), "actor": "operator"}),
        json!({"segment_id": "seg-2", "name": "X", "predicates": Board::predicates(), "project": "client"}),
        json!({"segment_id": "seg-2", "name": "X", "predicates": Board::predicates(), "project_link": "client"}),
        json!({"segment_id": "seg-2", "name": "X", "predicates": Board::predicates(), "workspace": "default"}),
        json!({"segment_id": "seg-2", "name": "X", "predicates": Board::predicates(), "install_id": b.install}),
        json!({"segment_id": "seg-2", "name": "X", "predicates": Board::predicates(), "context_id": b.context_id}),
    ] {
        let (code, text) = b.operator("POST", &format!("{base}/segments"), &body.to_string());
        assert_eq!(code, 400, "forged segment body accepted: {body}");
        assert!(!text.contains(marker), "refusal echoed content: {text}");
    }
    // A revision naming an unknown segment passes the transport
    // grammar and refuses at the daemon — without echoing the name.
    let (code, text) = b.operator(
        "POST",
        &format!("{base}/segments"),
        &json!({"segment_id": "seg-2", "name": marker, "predicates": Board::predicates(), "expected_revision": 1}).to_string(),
    );
    assert_eq!(code, 409, "unknown-segment revision accepted: {text}");
    assert!(!text.contains(marker), "refusal echoed content: {text}");
    for body in [
        json!({"freeze_id": "freeze-1", "base": {"mode": "all"}, "max_recipients": 50, "by": "operator"}),
        json!({"freeze_id": "freeze-1", "base": {"mode": "all"}, "max_recipients": 50, "actor": "operator"}),
        json!({"freeze_id": "freeze-1", "base": {"mode": "all"}, "max_recipients": 50, "project": "client"}),
        json!({"freeze_id": "freeze-1", "base": {"mode": "all"}, "max_recipients": 50, "install_id": b.install}),
        json!({"freeze_id": "freeze-1", "base": {"mode": "all"}, "max_recipients": 50, "context_id": b.context_id}),
    ] {
        let (code, _) = b.operator(
            "POST",
            &format!("{base}/audience/prepares"),
            &body.to_string(),
        );
        assert_eq!(code, 400, "forged prepare body accepted: {body}");
    }
    // Query strings never carry authority.
    for path in [
        format!("{base}/segments/list?context_id=other"),
        format!("{base}/segments/seg-vip?segment_id=other"),
        format!("{base}/audience/prepares/freeze-1?freeze_id=other"),
    ] {
        let (code, _) = b.operator("GET", &path, "");
        assert_ne!(code, 200, "query-bearing path accepted: {path}");
    }
    let (code, _) = b.operator("POST", &format!("{base}/segments?x=1"), "{}");
    assert_eq!(code, 400, "query-bearing write accepted");
    // An unsupported predicate refuses without echoing the marker.
    let (code, text) = b.operator(
        "POST",
        &format!("{base}/segments"),
        &json!({"segment_id": "seg-bad", "name": marker, "predicates": [{"field": "body", "op": "eq", "value": marker}]}).to_string(),
    );
    assert_eq!(code, 409, "bad predicate accepted: {text}");
    assert!(!text.contains(marker), "predicate content leaked: {text}");
    // Nothing above mutated the file.
    assert_eq!(
        b.value("GET", &format!("{base}/segments/seg-vip"), json!({}))["segment"],
        created["segment"]
    );
    assert_eq!(
        b.operator("GET", &format!("{base}/segments/seg-2"), "").0,
        409
    );
}

#[test]
fn cad780_http_cross_install_and_cross_context_refuse() {
    let b = Board::new();
    let base = b.base();
    b.value(
        "POST",
        &format!("{base}/segments"),
        json!({"segment_id": "seg-vip", "name": "VIP", "predicates": Board::predicates()}),
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
            json!({"install_id": install_b, "label": "Client B", "input_defaults": {}, "request_id": "ctx-aud-http-b"}),
        )
        .unwrap()["context"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let other_show =
        format!("/api/app-installations/{install_b}/contexts/{ctx_b}/segments/seg-vip");

    // Cross-install reads/writes fail: the row lives under A only.
    assert_ne!(b.operator("GET", &other_show, "").0, 200);
    let (code, _) = b.operator(
        "POST",
        &format!("/api/app-installations/{install_b}/contexts/{ctx_b}/segments"),
        &json!({"segment_id": "seg-vip", "name": "VIP", "predicates": Board::predicates()})
            .to_string(),
    );
    // Saving under B's own scope is legitimate and isolated — it must
    // not see or touch A's segment.
    assert_eq!(code, 200);
    assert_eq!(
        b.operator(
            "GET",
            &format!(
                "/api/app-installations/{}/contexts/{}/segments/seg-vip",
                b.install, b.context_id
            ),
            ""
        )
        .0,
        200,
        "installation A lost its segment"
    );
    // Mixed scopes fail: A's install with B's context and vice versa.
    for path in [
        format!(
            "/api/app-installations/{install_b}/contexts/{}/segments/seg-vip",
            b.context_id
        ),
        format!(
            "/api/app-installations/{}/contexts/{ctx_b}/segments/seg-vip",
            b.install
        ),
        format!(
            "/api/app-installations/no-such-install/contexts/{}/segments/seg-vip",
            b.context_id
        ),
        format!(
            "/api/app-installations/{}/contexts/ctx-no-such-context/segments/seg-vip",
            b.install
        ),
    ] {
        assert_ne!(b.operator("GET", &path, "").0, 200, "forged scope admitted");
    }
    // Cross-install preview writes refuse as well.
    let (code, _) = b.operator(
        "POST",
        &format!(
            "/api/app-installations/{}/contexts/{ctx_b}/audience/preview",
            b.install
        ),
        &json!({"base": {"mode": "all"}}).to_string(),
    );
    assert_ne!(code, 200, "cross-install preview admitted");
    // The original segment is untouched.
    assert_eq!(
        b.value("GET", &format!("{base}/segments/seg-vip"), json!({}))["segment"]["revision"],
        1
    );
}

#[test]
fn cad780_http_stale_and_concurrent_cas_refuse_without_mutation() {
    let b = Board::new();
    let base = b.base();
    let created = b.value(
        "POST",
        &format!("{base}/segments"),
        json!({"segment_id": "seg-vip", "name": "VIP", "predicates": Board::predicates()}),
    );
    let stale = json!({"segment_id": "seg-vip", "name": "Racer", "predicates": Board::predicates(), "expected_revision": 7}).to_string();
    assert_ne!(
        b.operator("POST", &format!("{base}/segments"), &stale).0,
        200
    );
    assert_eq!(
        b.value("GET", &format!("{base}/segments/seg-vip"), json!({}))["segment"],
        created["segment"]
    );
    // Concurrent HTTP saves at the same revision: exactly one wins.
    let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
    let save_path = format!("{base}/segments");
    let attempts = 6;
    let results = std::thread::scope(|scope| {
        (0..attempts)
            .map(|_| {
                scope.spawn(|| {
                    let wire = session.request(
                        "POST",
                        &save_path,
                        &json!({"segment_id": "seg-vip", "name": "Racer", "predicates": Board::predicates(), "expected_revision": 1}).to_string(),
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
        "concurrent HTTP segment CAS admitted {results:?}"
    );
    let settled = b.value("GET", &format!("{base}/segments/seg-vip"), json!({}));
    assert_eq!(settled["segment"]["revision"], 2);
}

#[test]
fn cad780_http_agent_and_detached_refuse_without_mutation() {
    let b = Board::new();
    let base = b.base();
    let created = b.value(
        "POST",
        &format!("{base}/segments"),
        json!({"segment_id": "seg-vip", "name": "VIP", "predicates": Board::predicates()}),
    );
    let show_path = format!("{base}/segments/seg-vip");
    let before: Value = b.value("GET", &show_path, json!({}));
    assert_eq!(before["segment"], created["segment"]);

    let mut lane = LaneShell::spawn(b.root.path());
    plant_member_pane(
        &b.daemon,
        "audience-http-worker",
        "claude",
        None,
        lane.pid(),
    );
    let cases = vec![
        ("GET", format!("{base}/segments/list"), String::new()),
        ("GET", show_path.clone(), String::new()),
        (
            "POST",
            format!("{base}/segments"),
            json!({"segment_id": "seg-evil", "name": "E", "predicates": Board::predicates()})
                .to_string(),
        ),
        (
            "POST",
            format!("{base}/audience/preview"),
            json!({"base": {"mode": "all"}}).to_string(),
        ),
        (
            "POST",
            format!("{base}/audience/prepares"),
            json!({"freeze_id": "freeze-evil", "base": {"mode": "all"}, "max_recipients": 50})
                .to_string(),
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
            eprintln!("audience HTTP peer prefix={prefix:?} {method} {path}: {status}");
            if status != "403" {
                failures.push(format!("{prefix:?} {method} {path}: {status}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "audience HTTP operator peer guard failed: {failures:?}"
    );
    // A sessionless read is refused too (never the operator by default).
    let host = common::op::board_host(b.port);
    let bare = format!("GET {show_path} HTTP/1.0\r\nHost: {host}\r\nContent-Length: 0\r\n\r\n");
    let (code, _, _) = common::op::raw(b.port, &bare);
    assert_eq!(code, 403, "sessionless read admitted");
    // Nothing above mutated the segment.
    assert_eq!(
        b.value("GET", &show_path, json!({}))["segment"],
        created["segment"]
    );
    assert_eq!(
        b.operator("GET", &format!("{base}/segments/seg-evil"), "")
            .0,
        409
    );
}

#[test]
fn cad780_http_verb_path_matrix() {
    let b = Board::new();
    let base = b.base();
    let created = b.value(
        "POST",
        &format!("{base}/segments"),
        json!({"segment_id": "seg-vip", "name": "VIP", "predicates": Board::predicates()}),
    );
    let show_path = format!("{base}/segments/seg-vip");
    let before = b.value("GET", &show_path, json!({}));

    // Wrong verb on a real route is 405, never a silent read or write.
    assert_eq!(
        b.operator(
            "POST",
            &show_path,
            &json!({"segment_id": "seg-vip", "name": "X", "predicates": Board::predicates(), "expected_revision": 1}).to_string()
        )
        .0,
        405
    );
    assert_eq!(b.operator("GET", &format!("{base}/segments"), "").0, 405);
    assert_eq!(
        b.operator("GET", &format!("{base}/audience/preview"), "").0,
        405
    );
    assert_eq!(
        b.operator("POST", &format!("{base}/segments/list"), "{}").0,
        405
    );
    // Audience-shaped paths outside the contract 404.
    for (method, path, body) in [
        (
            "GET",
            format!("{base}/segments/seg-vip/extra"),
            String::new(),
        ),
        ("GET", format!("{base}/audience"), String::new()),
        (
            "GET",
            format!("{base}/audience/prepares/freeze-1/extra"),
            String::new(),
        ),
        ("GET", "/api/app-audiences".to_string(), String::new()),
    ] {
        let (code, _, _) = {
            let session =
                common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
            common::op::raw(b.port, &session.request(method, &path, &body))
        };
        assert_eq!(
            code, 404,
            "audience path matrix missed {method} {path}: {code}"
        );
    }
    // Traversal never reaches a route.
    {
        let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
        let (code, _, _) = common::op::raw(
            b.port,
            &session.request("GET", &format!("{}/../segments", base), ""),
        );
        assert_eq!(code, 400, "traversal path admitted: {code}");
    }
    assert_eq!(
        b.value("GET", &show_path, json!({}))["segment"],
        before["segment"]
    );
    assert_eq!(before["segment"], created["segment"]);
}
