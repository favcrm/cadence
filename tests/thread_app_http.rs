//! CAD-802 HTTP peer for the shell chat's verified App binding.
//!
//! Adversarial-first: the operator's `POST /api/threads/<alias>/messages`
//! may carry `app: {install_id, context_id}`; the daemon proves both
//! against its store and stamps the verified binding on the entry. An
//! agent caller, a detached child with a stolen session, forged
//! bindings (browser `verified`, actor/routing claims, unknown or
//! archived contexts, bad segments), and concurrent same-message sends
//! are all refused or deduplicated without an unverified stamp.
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
        daemon
            .fixture_rpc(
                "agent_register",
                json!({"alias": "shellpeer", "provider": "inbox", "endpoint_kind": "inbox", "role": "pm", "cwd": daemon.dir.path()}),
            )
            .unwrap();
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
                json!({"install_id": install, "label": "Client", "input_defaults": {}, "request_id": "ctx-thread-http-1"}),
            )
            .unwrap()["context"]
            .clone();
        let context_id = context["id"].as_str().unwrap().to_owned();
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
                "thread app board startup deadline exhausted"
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
                    Ok(Err(error)) => eprintln!("thread app board startup contention: {error}"),
                    unexpected => panic!("thread app board bind failure returned {unexpected:?}"),
                }
                board.port = board
                    .port
                    .checked_add(1)
                    .filter(|port| *port < 3200)
                    .expect("thread app board startup exhausted permitted ports");
                board.stop.store(false, Ordering::SeqCst);
            } else {
                panic!("thread app board startup notification {notification:?}; worker {result:?}");
            }
        }
    }

    fn operator(&self, method: &str, path: &str, body: &str) -> (u16, String) {
        let session =
            common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &self.daemon.state, self.port);
        let (code, _, body) = common::op::raw(self.port, &session.request(method, path, body));
        (code, body)
    }

    fn entries(&self) -> Vec<Value> {
        self.daemon
            .operator_rpc(
                "thread_read",
                json!({"alias": "shellpeer", "after": 0, "limit": 200}),
            )
            .unwrap()["entries"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn app(&self) -> Value {
        json!({"install_id": self.install, "context_id": self.context_id})
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
                eprintln!("thread app board worker cleanup after primary panic: {result:?}");
            }
        } else {
            result.unwrap().unwrap();
        }
    }
}

#[test]
fn cad802_http_thread_app_binding_verified_roundtrip() {
    let b = Board::new();
    let path = "/api/threads/shellpeer/messages";
    let body =
        json!({"text": "help with this app", "message": "http-app-1", "app": b.app()}).to_string();
    let (code, receipt) = b.operator("POST", path, &body);
    assert_eq!(code, 200, "operator app send refused: {receipt}");
    let line = b
        .entries()
        .into_iter()
        .find(|e| e["text"] == json!("help with this app"))
        .expect("verified app send stored no entry");
    let bound = &line["payload"]["app"];
    assert_eq!(bound["install_id"], json!(b.install), "{line}");
    assert_eq!(bound["context_id"], json!(b.context_id), "{line}");
    assert_eq!(bound["verified"], json!(true), "{line}");
    assert!(
        bound["context_revision"].as_i64().unwrap_or(0) > 0,
        "{line}"
    );
    assert!(
        !bound["context_digest"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "{line}"
    );
    // The stamp matches the store's own proof — not the browser's words.
    let proof = b
        .daemon
        .operator_rpc(
            "app_context_show",
            json!({"install_id": b.install, "context_id": b.context_id}),
        )
        .unwrap()["context"]
        .clone();
    assert_eq!(bound["context_revision"], proof["revision"], "{line}");
    assert_eq!(bound["context_digest"], proof["digest"], "{line}");
}

#[test]
fn cad802_http_thread_app_forged_refuse_without_stamp() {
    let b = Board::new();
    let path = "/api/threads/shellpeer/messages";
    let before = b.entries().len();
    let second = b
        .daemon
        .operator_rpc(
            "app_context_create",
            json!({"install_id": b.install, "label": "Second", "input_defaults": {}, "request_id": "ctx-thread-http-2"}),
        )
        .unwrap()["context"]
        .clone();
    let second_id = second["id"].as_str().unwrap();
    b.daemon
        .operator_rpc(
            "app_context_archive",
            json!({"install_id": b.install, "context_id": second_id, "expected_revision": second["revision"]}),
        )
        .unwrap();
    // Forged bindings: browser `verified`, actor/routing claims at
    // either level, unknown or archived scope, bad shape — every one
    // refused, and none queues an entry.
    for body in [
        json!({"text": "x", "message": "http-f", "app": {"install_id": b.install, "context_id": b.context_id, "verified": true}}),
        json!({"text": "x", "message": "http-f", "app": {"install_id": b.install, "context_id": b.context_id, "actor": "operator"}}),
        json!({"text": "x", "message": "http-f", "app": {"install_id": b.install, "context_id": b.context_id, "revision": 1}}),
        json!({"text": "x", "message": "http-f", "app": {"install_id": "install-nope", "context_id": b.context_id}}),
        json!({"text": "x", "message": "http-f", "app": {"install_id": b.install, "context_id": "context-nope"}}),
        json!({"text": "x", "message": "http-f", "app": {"install_id": b.install, "context_id": second_id}}),
        json!({"text": "x", "message": "http-f", "app": {"install_id": "../escape", "context_id": b.context_id}}),
        json!({"text": "x", "message": "http-f", "app": {"install_id": b.install}}),
        json!({"text": "x", "message": "http-f", "app": "install:ctx"}),
        json!({"text": "x", "message": "http-f", "app": b.app(), "actor": "operator"}),
        json!({"text": "x", "message": "http-f", "app": b.app(), "by": "operator"}),
        json!({"text": "x", "message": "http-f", "app": b.app(), "project": "client"}),
    ] {
        let (code, text) = b.operator("POST", path, &body.to_string());
        assert_ne!(code, 200, "forged app send admitted: {body}");
        // A refusal names the bad field at most — never a server stamp.
        assert!(
            !text.contains("context_digest"),
            "refusal leaked a stamp: {text}"
        );
        assert!(
            !text.contains("context_revision"),
            "refusal leaked a stamp: {text}"
        );
    }
    assert_eq!(b.entries().len(), before, "forged app sends queued entries");
}

#[test]
fn cad802_http_thread_app_agent_and_detached_refuse() {
    let b = Board::new();
    let path = "/api/threads/shellpeer/messages";
    let before = b.entries().len();
    let mut lane = LaneShell::spawn(b.root.path());
    plant_member_pane(&b.daemon, "thread-app-worker", "claude", None, lane.pid());
    let body =
        json!({"text": "agent app claim", "message": "http-agent-1", "app": b.app()}).to_string();
    let mut failures = Vec::new();
    for prefix in ["", "setsid "] {
        let stolen = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
        let wire = stolen.request_as("POST", path, &body, "");
        assert!(!wire.contains(cadence_agent::test_seam::AS_HEADER));
        assert!(!wire.contains(cadence_agent::test_seam::TOKEN_HEADER));
        let file = lane
            .dir
            .path()
            .join(format!("thread-app-request-{}.txt", lane.seq));
        std::fs::write(&file, wire).unwrap();
        let (rc, response) = lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));s.sendall(open(sys.argv[2],\"rb\").read());print(s.makefile().readline())' {} {}", b.port, file.display()));
        assert_eq!(rc, 0, "HTTP native process failed");
        let status = response.split_whitespace().nth(1).unwrap_or("missing");
        eprintln!("thread app peer prefix={prefix:?}: {status}");
        if status != "403" {
            failures.push(format!("{prefix:?}: {status}"));
        }
    }
    assert!(
        failures.is_empty(),
        "thread app operator peer guard failed: {failures:?}"
    );
    assert_eq!(
        b.entries().len(),
        before,
        "agent/detached app sends queued entries"
    );
}

#[test]
fn cad802_http_thread_app_concurrent_duplicate() {
    let b = Board::new();
    let path = "/api/threads/shellpeer/messages".to_string();
    let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
    let body =
        json!({"text": "concurrent app question", "message": "http-app-race", "app": b.app()})
            .to_string();
    let attempts = 6;
    let results = std::thread::scope(|scope| {
        (0..attempts)
            .map(|_| {
                scope.spawn(|| {
                    let (code, _, text) =
                        common::op::raw(b.port, &session.request("POST", &path, &body));
                    (code, text)
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(
        results.iter().all(|(code, _)| *code == 200),
        "concurrent app sends refused {results:?}"
    );
    let fresh = results
        .iter()
        .filter(|(_, text)| {
            serde_json::from_str::<Value>(text)
                .ok()
                .and_then(|v| v.get("duplicate").cloned())
                != Some(json!(true))
        })
        .count();
    assert_eq!(fresh, 1, "concurrent app sends queued {fresh}");
    let matching = b
        .entries()
        .into_iter()
        .filter(|e| e["text"] == json!("concurrent app question"))
        .count();
    assert_eq!(
        matching, 1,
        "concurrent app sends stored {matching} entries"
    );
}
