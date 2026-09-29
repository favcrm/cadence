//! CAD-813 live board HTTP peer for verified assistant email proposals.
//!
//! ADVERSARIAL-FIRST (RED): the board exposes no assistant-mint
//! route — the browser can request or display a verified proposal but
//! can never mint its provenance. A proposal the daemon recorded
//! behind a host-verified chat turn lists over HTTP with its
//! assistant receipt intact; operator Apply/Discard over HTTP keep
//! their operator proof (an agent caller and a sessionless caller are
//! refused without mutation), receipt-shaped fields refuse at the
//! transport grammar, and unknown assistant paths 404. The board is
//! at least as strict as daemon RPC. No SMTP send happens anywhere.
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

fn blocks() -> Value {
    json!([
        {"type": "heading", "text": "Hello {{first_name|friend}}"},
        {"type": "paragraph", "text": "A calm first line."},
        {"type": "button", "label": "Read more", "url": "https://example.com/posts/welcome"},
    ])
}

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
                json!({"install_id": install, "label": "Client", "input_defaults": {}, "request_id": "ctx-813-http-1"}),
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
                "content board startup deadline exhausted"
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
                    Ok(Err(error)) => eprintln!("content board startup contention: {error}"),
                    unexpected => panic!("content board bind failure returned {unexpected:?}"),
                }
                board.port = board
                    .port
                    .checked_add(1)
                    .filter(|port| *port < 3200)
                    .expect("content board startup exhausted permitted ports");
                board.stop.store(false, Ordering::SeqCst);
            } else {
                panic!("content board startup notification {notification:?}; worker {result:?}");
            }
        }
    }

    fn base(&self) -> String {
        format!(
            "/api/app-installations/{}/contexts/{}/content",
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
            "operator content request {method} {path}: {result}"
        );
        serde_json::from_str(&result).unwrap()
    }

    /// A scoped chat turn for `agent`: the operator's `thread_send`
    /// carries the verified App binding, then the message is claimed
    /// running under a turn token current for the planted pane.
    fn chat_turn(&self, agent: &str, message: &str) -> String {
        self.daemon
            .operator_rpc(
                "thread_send",
                json!({"alias": agent, "text": "help draft the launch email", "message": message,
                       "app": {"install_id": self.install, "context_id": self.context_id}}),
            )
            .unwrap();
        let token = format!("pty-planted-{}", uuid::Uuid::new_v4().simple());
        let conn = rusqlite::Connection::open(self.daemon.state.join("cadence.sqlite3")).unwrap();
        let changed = conn
            .execute(
                "UPDATE messages SET state='running',turn_id=? WHERE id=?",
                rusqlite::params![token, message],
            )
            .unwrap();
        assert_eq!(changed, 1, "chat turn message missing: {message}");
        token
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
                eprintln!("content board worker cleanup after primary panic: {result:?}");
            }
        } else {
            result.unwrap().unwrap();
        }
    }
}

#[test]
fn cad813_http_verified_proposal_lists_with_receipt() {
    let b = Board::new();
    let base = b.base();
    b.value(
        "POST",
        &format!("{base}/campaigns"),
        json!({"campaign_id": "launch-1", "subject": "Spring launch", "blocks": blocks()}),
    );

    // The assistant turn proposes over the daemon socket (its own
    // pane connection); the operator path could never mint this.
    let mut lane = LaneShell::spawn(b.root.path());
    plant_member_pane(&b.daemon, "crm-http-writer", "claude", None, lane.pid());
    let token = b.chat_turn("crm-http-writer", "chat-813-http-1");
    let frame: Value = lane.rpc(
        &b.daemon.state,
        "app_content_assistant_propose",
        json!({"install_id": b.install, "context_id": b.context_id, "campaign_id": "launch-1",
               "proposal_id": "prop-http-1", "subject": "Assistant draft over chat",
               "blocks": blocks(), "message": "chat-813-http-1", "token": token}),
    );
    assert_eq!(frame["ok"], true, "{frame}");

    // The Campaigns-page seam (CAD-784) reads it over HTTP with the
    // verified actor, origin and receipt intact.
    let listed: Value = b.value("GET", &format!("{base}/proposals/list"), json!({}));
    assert_eq!(listed["proposals"].as_array().unwrap().len(), 1);
    let proposal = &listed["proposals"][0];
    assert_eq!(proposal["actor"], "assistant");
    assert_eq!(proposal["origin"], "assistant-receipt");
    assert_eq!(
        proposal["assistant_receipt"]["message_id"],
        "chat-813-http-1"
    );
    assert_eq!(proposal["assistant_receipt"]["agent"], "crm-http-writer");
    assert_eq!(proposal["assistant_receipt"]["campaign_id"], "launch-1");
    let shown: Value = b.value("GET", &format!("{base}/proposals/prop-http-1"), json!({}));
    assert_eq!(shown["proposal"], *proposal);

    // Operator Apply over HTTP creates the new revision; the receipt
    // survives on the decided row while the draft moves on.
    let applied: Value = b.value(
        "POST",
        &format!("{base}/proposals/prop-http-1/apply"),
        json!({"expected_revision": 1}),
    );
    assert_eq!(applied["content"]["revision"], 2);
    let decided: Value = b.value("GET", &format!("{base}/proposals/prop-http-1"), json!({}));
    assert_eq!(decided["proposal"]["state"], "applied");
    assert_eq!(decided["proposal"]["actor"], "assistant");
}

#[test]
fn cad813_http_browser_cannot_mint_provenance() {
    let b = Board::new();
    let base = b.base();
    b.value(
        "POST",
        &format!("{base}/campaigns"),
        json!({"campaign_id": "launch-1", "subject": "Spring launch", "blocks": blocks()}),
    );

    // Receipt/turn-shaped fields refuse at the HTTP transport grammar
    // on every body that could carry one — the browser never names a
    // turn, a token or a receipt.
    for (path, body) in [
        (
            format!("{base}/proposals"),
            json!({"campaign_id": "launch-1", "proposal_id": "prop-x", "subject": "X", "blocks": blocks(), "message": "m-1", "token": "t-1"}),
        ),
        (
            format!("{base}/proposals"),
            json!({"campaign_id": "launch-1", "proposal_id": "prop-x", "subject": "X", "blocks": blocks(), "assistant_receipt": {"turn_id": "t-1"}}),
        ),
        (
            format!("{base}/proposals"),
            json!({"campaign_id": "launch-1", "proposal_id": "prop-x", "subject": "X", "blocks": blocks(), "turn_id": "t-1"}),
        ),
        (
            format!("{base}/proposals"),
            json!({"campaign_id": "launch-1", "proposal_id": "prop-x", "subject": "X", "blocks": blocks(), "source_revision": 1}),
        ),
        (
            format!("{base}/proposals/prop-x/apply"),
            json!({"expected_revision": 1, "message": "m-1"}),
        ),
        (
            format!("{base}/campaigns"),
            json!({"campaign_id": "launch-2", "subject": "X", "blocks": blocks(), "token": "t-1"}),
        ),
    ] {
        let (code, _) = b.operator("POST", &path, &body.to_string());
        assert_eq!(code, 400, "browser-minted provenance accepted: {body}");
    }
    // There is no assistant-mint route: assistant-shaped paths 404.
    for (method, path) in [
        ("POST", format!("{base}/assistant-proposals")),
        (
            "POST",
            format!("{base}/proposals/prop-x/propose-as-assistant"),
        ),
        ("GET", format!("{base}/proposals/prop-x/receipt")),
    ] {
        let (code, _, _) = {
            let session =
                common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
            common::op::raw(b.port, &session.request(method, &path, ""))
        };
        assert_eq!(
            code, 404,
            "assistant-mint path answered {method} {path}: {code}"
        );
    }
    // Nothing above created a proposal or moved the draft.
    let listed: Value = b.value("GET", &format!("{base}/proposals/list"), json!({}));
    assert_eq!(listed["proposals"].as_array().unwrap().len(), 0);
}

#[test]
fn cad813_http_agent_and_sessionless_cannot_apply_or_discard() {
    let b = Board::new();
    let base = b.base();
    let created = b.value(
        "POST",
        &format!("{base}/campaigns"),
        json!({"campaign_id": "launch-1", "subject": "Spring launch", "blocks": blocks()}),
    );
    let mut lane = LaneShell::spawn(b.root.path());
    plant_member_pane(&b.daemon, "crm-http-agent", "claude", None, lane.pid());
    let token = b.chat_turn("crm-http-agent", "chat-813-http-2");
    let frame: Value = lane.rpc(
        &b.daemon.state,
        "app_content_assistant_propose",
        json!({"install_id": b.install, "context_id": b.context_id, "campaign_id": "launch-1",
               "proposal_id": "prop-http-2", "subject": "Assistant draft",
               "blocks": blocks(), "message": "chat-813-http-2", "token": token}),
    );
    assert_eq!(frame["ok"], true, "{frame}");
    let show_path = format!("{base}/campaigns/launch-1");

    // The agent's own HTTP request (its pane process relaying a signed
    // session) is refused at the board's operator peer guard — 403,
    // never a decision.
    let mut failures = Vec::new();
    for prefix in ["", "setsid "] {
        for (method, path, body) in [
            (
                "POST",
                format!("{base}/proposals/prop-http-2/apply"),
                json!({"expected_revision": 1}).to_string(),
            ),
            (
                "POST",
                format!("{base}/proposals/prop-http-2/discard"),
                String::new(),
            ),
        ] {
            let stolen =
                common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
            let wire = stolen.request_as(method, &path, &body, "");
            let file = lane
                .dir
                .path()
                .join(format!("request-813-{}.txt", lane.seq));
            std::fs::write(&file, wire).unwrap();
            let (rc, response) = lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));s.sendall(open(sys.argv[2],\"rb\").read());print(s.makefile().readline())' {} {}", b.port, file.display()));
            assert_eq!(rc, 0, "HTTP native process failed");
            let status = response.split_whitespace().nth(1).unwrap_or("missing");
            if status != "403" {
                failures.push(format!("{prefix:?} {method} {path}: {status}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "assistant HTTP operator peer guard failed: {failures:?}"
    );
    // A sessionless Apply/Discard is refused too.
    let host = common::op::board_host(b.port);
    for path in [
        format!("{base}/proposals/prop-http-2/apply"),
        format!("{base}/proposals/prop-http-2/discard"),
    ] {
        let bare = format!("POST {path} HTTP/1.0\r\nHost: {host}\r\nContent-Length: 0\r\n\r\n");
        let (code, _, _) = common::op::raw(b.port, &bare);
        assert_eq!(code, 403, "sessionless decision admitted: {path}");
    }
    // The proposal is still pending and the draft never moved.
    let pending: Value = b.value("GET", &format!("{base}/proposals/prop-http-2"), json!({}));
    assert_eq!(pending["proposal"]["state"], "pending");
    assert_eq!(
        b.value("GET", &show_path, json!({}))["content"],
        created["content"]
    );
}
