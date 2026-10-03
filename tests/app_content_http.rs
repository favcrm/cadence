//! CAD-782 live board HTTP peer for versioned email content.
//!
//! Adversarial-first against a real board: an authorized operator
//! gets bounded content save/show/list/render, proposal
//! propose/apply/discard, approval and test/final-send preparation
//! over HTTP with receipts matching daemon RPC; an agent caller, a
//! detached child, forged actor/install/context/project fields,
//! cross-install/context probes, stale/concurrent writes and a wrong
//! verb/path matrix are all refused without mutation or leak. The
//! board is at least as strict as daemon RPC. No SMTP send happens
//! anywhere.
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
                json!({"install_id": install, "label": "Client", "input_defaults": {}, "request_id": "ctx-content-http-1"}),
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

    fn rpc_show(&self, campaign: &str) -> Value {
        self.daemon
            .operator_rpc(
                "app_content_show",
                json!({"install_id": self.install, "context_id": self.context_id, "campaign_id": campaign}),
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
                eprintln!("content board worker cleanup after primary panic: {result:?}");
            }
        } else {
            result.unwrap().unwrap();
        }
    }
}

#[test]
fn cad782_http_content_roundtrip_matches_rpc() {
    let b = Board::new();
    let base = b.base();
    let saved = b.value(
        "POST",
        &format!("{base}/campaigns"),
        json!({"campaign_id": "launch-1", "subject": "Spring launch", "preheader": "News", "blocks": blocks()}),
    );
    assert_eq!(saved["content"]["revision"], 1);
    let shown = b.value("GET", &format!("{base}/campaigns/launch-1"), json!({}));
    assert_eq!(shown["content"], saved["content"]);
    let listed = b.value("GET", &format!("{base}/campaigns/list"), json!({}));
    assert_eq!(listed["contents"].as_array().unwrap().len(), 1);

    // The HTTP receipt matches the daemon RPC receipt exactly.
    assert_eq!(b.rpc_show("launch-1")["content"], saved["content"]);

    // Render over HTTP matches RPC render byte for byte.
    let rendered = b.value(
        "POST",
        &format!("{base}/campaigns/launch-1/render"),
        json!({"sample_first_name": "Amina"}),
    );
    let via_rpc = b
        .daemon
        .operator_rpc(
            "app_content_render",
            json!({"install_id": b.install, "context_id": b.context_id, "campaign_id": "launch-1", "sample_first_name": "Amina"}),
        )
        .unwrap();
    assert_eq!(rendered["render"], via_rpc["render"]);
    assert!(rendered["render"]["html"]
        .as_str()
        .unwrap()
        .contains("Hello Amina"));

    // Saved sender material stays preview-only over HTTP too: the row
    // round-trips, test preparation behind it is labelled preview-only,
    // and final-send preparation refuses — including behind a
    // fictitious connection_id, which is operator text, not authority.
    let binding = b.value(
        "POST",
        &format!("{base}/sender-bindings"),
        json!({"binding_id": "bind-1", "sender_name": "News", "sender_address": "news@example.com", "unsubscribe_base": "https://example.com/unsub"}),
    );
    assert_eq!(binding["binding"]["preview_only"], true);
    let bound = b.value("GET", &format!("{base}/sender-bindings/bind-1"), json!({}));
    assert_eq!(bound["binding"], binding["binding"]);
    let listed = b.value("GET", &format!("{base}/sender-bindings/list"), json!({}));
    assert_eq!(listed["bindings"].as_array().unwrap().len(), 1);

    // Proposal propose/apply over HTTP with Apply/Discard semantics.
    // Operator-submitted drafts read as operator work.
    let proposed = b.value(
        "POST",
        &format!("{base}/proposals"),
        json!({"campaign_id": "launch-1", "proposal_id": "prop-1", "subject": "Spring launch, new", "blocks": blocks()}),
    );
    assert_eq!(proposed["proposal"]["state"], "pending");
    assert_eq!(proposed["proposal"]["actor"], "operator");
    assert_eq!(proposed["proposal"]["origin"], "operator-direct");
    let applied = b.value(
        "POST",
        &format!("{base}/proposals/prop-1/apply"),
        json!({"expected_revision": 1}),
    );
    assert_eq!(applied["content"]["revision"], 2);
    let approved = b.value(
        "POST",
        &format!("{base}/campaigns/launch-1/approve"),
        json!({"expected_revision": 2}),
    );
    assert_eq!(approved["content"]["approval"]["valid"], true);

    // Send preparation without a binding refuses at the transport.
    assert_ne!(
        b.operator(
            "POST",
            &format!("{base}/campaigns/launch-1/send-prepare"),
            &json!({}).to_string()
        )
        .0,
        200,
        "binding-less send-prepare admitted"
    );
    // Test-send preparation behind the named binding shares the content
    // hash and stays labelled preview-only. Final-send preparation
    // refuses at the daemon — with a saved binding, a fictitious
    // connection, and a missing binding alike — until CAD-785/786
    // supply host-verified evidence.
    let test = b.value(
        "POST",
        &format!("{base}/campaigns/launch-1/test-prepare"),
        json!({"to_email": "op@example.com", "binding_id": "bind-1"}),
    );
    assert_eq!(test["test_send"]["preview_only"], true);
    assert_eq!(test["test_send"]["send_ready"], false);
    for body in [
        json!({"binding_id": "bind-1"}),
        json!({"binding_id": "bind-missing"}),
    ] {
        assert_ne!(
            b.operator(
                "POST",
                &format!("{base}/campaigns/launch-1/send-prepare"),
                &body.to_string()
            )
            .0,
            200,
            "send-prepare admitted: {body}"
        );
    }
    let fiction = b.value(
        "POST",
        &format!("{base}/sender-bindings"),
        json!({"binding_id": "bind-fiction", "sender_name": "News", "sender_address": "news@example.com", "unsubscribe_base": "https://example.com/unsub", "connection_id": "conn-no-such-connection"}),
    );
    assert_eq!(fiction["binding"]["preview_only"], true);
    assert_ne!(
        b.operator(
            "POST",
            &format!("{base}/campaigns/launch-1/send-prepare"),
            &json!({"binding_id": "bind-fiction"}).to_string()
        )
        .0,
        200,
        "send prepared behind a fictitious connection"
    );
    // Render over HTTP matches RPC render byte for byte, including
    // the binding snapshot — always preview-only, never send-ready.
    let rendered_bound = b.value(
        "POST",
        &format!("{base}/campaigns/launch-1/render"),
        json!({"binding_id": "bind-1"}),
    );
    assert_eq!(rendered_bound["render"]["preview_only"], true);
    assert_eq!(rendered_bound["render"]["send_ready"], false);
}

#[test]
fn cad782_http_forged_bodies_refuse_without_mutation_or_leak() {
    let b = Board::new();
    let base = b.base();
    let created = b.value(
        "POST",
        &format!("{base}/campaigns"),
        json!({"campaign_id": "launch-1", "subject": "Spring launch", "blocks": blocks()}),
    );
    let before = b.value("GET", &format!("{base}/campaigns/launch-1"), json!({}));
    assert_eq!(before["content"], created["content"]);
    let marker = "cad782-private-content-marker";

    // Forged identity / discovery-link / routing / scope fields in
    // the body refuse at the transport grammar — never authority.
    for body in [
        json!({"campaign_id": "launch-2", "subject": "X", "blocks": blocks(), "by": "operator"}),
        json!({"campaign_id": "launch-2", "subject": "X", "blocks": blocks(), "actor": "operator"}),
        json!({"campaign_id": "launch-2", "subject": "X", "blocks": blocks(), "project": "client"}),
        json!({"campaign_id": "launch-2", "subject": "X", "blocks": blocks(), "project_link": "client"}),
        json!({"campaign_id": "launch-2", "subject": "X", "blocks": blocks(), "workspace": "default"}),
        json!({"campaign_id": "launch-2", "subject": "X", "blocks": blocks(), "install_id": b.install}),
        json!({"campaign_id": "launch-2", "subject": "X", "blocks": blocks(), "context_id": b.context_id}),
    ] {
        let (code, text) = b.operator("POST", &format!("{base}/campaigns"), &body.to_string());
        assert_eq!(code, 400, "forged content body accepted: {body}");
        assert!(!text.contains(marker), "refusal echoed content: {text}");
    }
    // Unsafe content passes the transport grammar and refuses at the
    // daemon — without echoing the marker.
    let (code, text) = b.operator(
        "POST",
        &format!("{base}/campaigns"),
        &json!({"campaign_id": "launch-bad", "subject": marker, "blocks": [{"type": "paragraph", "text": "<script>alert(1)</script>"}]}).to_string(),
    );
    assert_eq!(code, 409, "unsafe content accepted: {text}");
    assert!(!text.contains(marker), "content leaked: {text}");
    // Receipt-shaped fields refuse at the transport on every body
    // that could carry one — no receipt exists yet, so any present
    // receipt is forged.
    for (path, body) in [
        (
            format!("{base}/campaigns"),
            json!({"campaign_id": "launch-2", "subject": "X", "blocks": blocks(), "assistant_receipt": {"turn_id": "t-1"}}),
        ),
        (
            format!("{base}/proposals"),
            json!({"campaign_id": "launch-1", "proposal_id": "prop-x", "subject": "X", "blocks": blocks(), "turn_id": "t-1"}),
        ),
        (
            format!("{base}/proposals"),
            json!({"campaign_id": "launch-1", "proposal_id": "prop-x", "subject": "X", "blocks": blocks(), "nonce": "n-1"}),
        ),
        (
            format!("{base}/sender-bindings"),
            json!({"binding_id": "bind-x", "sender_name": "N", "sender_address": "n@example.com", "unsubscribe_base": "https://example.com/u", "assistant_receipt": {"turn_id": "t-1"}}),
        ),
        (
            format!("{base}/campaigns/launch-1/send-prepare"),
            json!({"binding_id": "bind-1", "turn_id": "t-1"}),
        ),
    ] {
        let (code, _) = b.operator("POST", &path, &body.to_string());
        assert_eq!(code, 400, "forged receipt body accepted: {body}");
    }
    // Sender binding bodies refuse forged identity/scope fields too.
    for body in [
        json!({"binding_id": "bind-2", "sender_name": "N", "sender_address": "n@example.com", "unsubscribe_base": "https://example.com/u", "actor": "operator"}),
        json!({"binding_id": "bind-2", "sender_name": "N", "sender_address": "n@example.com", "unsubscribe_base": "https://example.com/u", "install_id": b.install}),
    ] {
        let (code, _) = b.operator(
            "POST",
            &format!("{base}/sender-bindings"),
            &body.to_string(),
        );
        assert_eq!(code, 400, "forged binding body accepted: {body}");
    }
    // Query strings never carry authority.
    for path in [
        format!("{base}/campaigns/list?context_id=other"),
        format!("{base}/campaigns/launch-1?campaign_id=other"),
        format!("{base}/proposals/prop-1?proposal_id=other"),
    ] {
        let (code, _) = b.operator("GET", &path, "");
        assert_ne!(code, 200, "query-bearing path accepted: {path}");
    }
    let (code, _) = b.operator("POST", &format!("{base}/campaigns?x=1"), "{}");
    assert_eq!(code, 400, "query-bearing write accepted");
    // Nothing above mutated the file.
    assert_eq!(
        b.value("GET", &format!("{base}/campaigns/launch-1"), json!({}))["content"],
        created["content"]
    );
    assert_eq!(
        b.operator("GET", &format!("{base}/campaigns/launch-2"), "")
            .0,
        409
    );
}

#[test]
fn cad782_http_cross_install_and_cross_context_refuse() {
    let b = Board::new();
    let base = b.base();
    b.value(
        "POST",
        &format!("{base}/campaigns"),
        json!({"campaign_id": "launch-1", "subject": "Spring launch", "blocks": blocks()}),
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
            json!({"install_id": install_b, "label": "Client B", "input_defaults": {}, "request_id": "ctx-content-http-b"}),
        )
        .unwrap()["context"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Mixed scopes fail: B's install with A's context and vice versa.
    for path in [
        format!(
            "/api/app-installations/{install_b}/contexts/{}/content/campaigns/launch-1",
            b.context_id
        ),
        format!(
            "/api/app-installations/{}/contexts/{ctx_b}/content/campaigns/launch-1",
            b.install
        ),
        format!(
            "/api/app-installations/no-such-install/contexts/{}/content/campaigns/launch-1",
            b.context_id
        ),
        format!(
            "/api/app-installations/{}/contexts/ctx-no-such-context/content/campaigns/launch-1",
            b.install
        ),
    ] {
        assert_ne!(b.operator("GET", &path, "").0, 200, "forged scope admitted");
    }
    // Saving under B's own scope is legitimate and isolated.
    let (code, _) = b.operator(
        "POST",
        &format!("/api/app-installations/{install_b}/contexts/{ctx_b}/content/campaigns"),
        &json!({"campaign_id": "launch-1", "subject": "B launch", "blocks": blocks()}).to_string(),
    );
    assert_eq!(code, 200);
    // The original content is untouched.
    assert_eq!(
        b.value("GET", &format!("{base}/campaigns/launch-1"), json!({}))["content"]["revision"],
        1
    );
}

#[test]
fn cad782_http_stale_and_concurrent_cas_refuse_without_mutation() {
    let b = Board::new();
    let base = b.base();
    let created = b.value(
        "POST",
        &format!("{base}/campaigns"),
        json!({"campaign_id": "launch-1", "subject": "Spring launch", "blocks": blocks()}),
    );
    let stale = json!({"campaign_id": "launch-1", "subject": "Racer", "blocks": blocks(), "expected_revision": 7}).to_string();
    assert_ne!(
        b.operator("POST", &format!("{base}/campaigns"), &stale).0,
        200
    );
    assert_eq!(
        b.value("GET", &format!("{base}/campaigns/launch-1"), json!({}))["content"],
        created["content"]
    );
    // Concurrent HTTP saves at the same revision: exactly one wins.
    let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
    let save_path = format!("{base}/campaigns");
    let attempts = 6;
    let results = std::thread::scope(|scope| {
        (0..attempts)
            .map(|_| {
                scope.spawn(|| {
                    let wire = session.request(
                        "POST",
                        &save_path,
                        &json!({"campaign_id": "launch-1", "subject": "Racer", "blocks": blocks(), "expected_revision": 1}).to_string(),
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
        "concurrent HTTP content CAS admitted {results:?}"
    );
    let settled = b.value("GET", &format!("{base}/campaigns/launch-1"), json!({}));
    assert_eq!(settled["content"]["revision"], 2);
}

#[test]
fn cad782_http_agent_and_detached_refuse_without_mutation() {
    let b = Board::new();
    let base = b.base();
    let created = b.value(
        "POST",
        &format!("{base}/campaigns"),
        json!({"campaign_id": "launch-1", "subject": "Spring launch", "blocks": blocks()}),
    );
    let show_path = format!("{base}/campaigns/launch-1");
    let before: Value = b.value("GET", &show_path, json!({}));
    assert_eq!(before["content"], created["content"]);

    let mut lane = LaneShell::spawn(b.root.path());
    plant_member_pane(&b.daemon, "content-http-worker", "claude", None, lane.pid());
    let cases = vec![
        ("GET", format!("{base}/campaigns/list"), String::new()),
        ("GET", show_path.clone(), String::new()),
        (
            "POST",
            format!("{base}/campaigns"),
            json!({"campaign_id": "launch-evil", "subject": "Evil", "blocks": blocks()})
                .to_string(),
        ),
        (
            "POST",
            format!("{show_path}/render"),
            json!({"sample_first_name": "Amina"}).to_string(),
        ),
        (
            "POST",
            format!("{base}/proposals"),
            json!({"campaign_id": "launch-1", "proposal_id": "prop-evil", "subject": "Evil", "blocks": blocks()})
                .to_string(),
        ),
        (
            "POST",
            format!("{show_path}/test-prepare"),
            json!({"to_email": "evil@example.com"}).to_string(),
        ),
        (
            "POST",
            format!("{show_path}/send-prepare"),
            json!({"binding_id": "bind-1"}).to_string(),
        ),
        (
            "POST",
            format!("{base}/sender-bindings"),
            json!({"binding_id": "bind-evil", "sender_name": "Evil", "sender_address": "evil@example.com", "unsubscribe_base": "https://example.com/u"})
                .to_string(),
        ),
        ("GET", format!("{base}/sender-bindings/list"), String::new()),
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
            eprintln!("content HTTP peer prefix={prefix:?} {method} {path}: {status}");
            if status != "403" {
                failures.push(format!("{prefix:?} {method} {path}: {status}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "content HTTP operator peer guard failed: {failures:?}"
    );
    // A sessionless read is refused too (never the operator by default).
    let host = common::op::board_host(b.port);
    let bare = format!("GET {show_path} HTTP/1.0\r\nHost: {host}\r\nContent-Length: 0\r\n\r\n");
    let (code, _, _) = common::op::raw(b.port, &bare);
    assert_eq!(code, 403, "sessionless read admitted");
    // Nothing above mutated the content.
    assert_eq!(
        b.value("GET", &show_path, json!({}))["content"],
        created["content"]
    );
    assert_eq!(
        b.operator("GET", &format!("{base}/campaigns/launch-evil"), "")
            .0,
        409
    );
}

#[test]
fn cad782_http_verb_path_matrix() {
    let b = Board::new();
    let base = b.base();
    let created = b.value(
        "POST",
        &format!("{base}/campaigns"),
        json!({"campaign_id": "launch-1", "subject": "Spring launch", "blocks": blocks()}),
    );
    let show_path = format!("{base}/campaigns/launch-1");
    let before = b.value("GET", &show_path, json!({}));

    // Wrong verb on a real route is 405, never a silent read or write.
    assert_eq!(
        b.operator(
            "POST",
            &show_path,
            &json!({"campaign_id": "launch-1", "subject": "X", "blocks": blocks()}).to_string()
        )
        .0,
        405
    );
    assert_eq!(b.operator("GET", &format!("{base}/campaigns"), "").0, 405);
    assert_eq!(b.operator("GET", &format!("{show_path}/render"), "").0, 405);
    assert_eq!(
        b.operator("POST", &format!("{base}/campaigns/list"), "{}")
            .0,
        405
    );
    assert_eq!(
        b.operator("GET", &format!("{base}/proposals/list"), "").0,
        200
    );
    assert_eq!(
        b.operator("GET", &format!("{base}/sender-bindings"), "").0,
        405
    );
    assert_eq!(
        b.operator("GET", &format!("{base}/sender-bindings/list"), "")
            .0,
        200
    );
    // Content-shaped paths outside the contract 404.
    for (method, path, body) in [
        (
            "GET",
            format!("{base}/campaigns/launch-1/extra"),
            String::new(),
        ),
        (
            "GET",
            format!("{base}/campaigns/launch-1/render/extra"),
            String::new(),
        ),
        (
            "GET",
            format!("{base}/proposals/prop-1/extra"),
            String::new(),
        ),
        (
            "GET",
            format!("{base}/sender-bindings/bind-1/extra"),
            String::new(),
        ),
        ("GET", "/api/app-content".to_string(), String::new()),
    ] {
        let (code, _, _) = {
            let session =
                common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
            common::op::raw(b.port, &session.request(method, &path, &body))
        };
        assert_eq!(
            code, 404,
            "content path matrix missed {method} {path}: {code}"
        );
    }
    // Traversal never reaches a route.
    {
        let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
        let (code, _, _) = common::op::raw(
            b.port,
            &session.request("GET", &format!("{}/../campaigns", base), ""),
        );
        assert_eq!(code, 400, "traversal path admitted: {code}");
    }
    assert_eq!(
        b.value("GET", &show_path, json!({}))["content"],
        before["content"]
    );
    assert_eq!(before["content"], created["content"]);
}

// ---- CAD-1056: the HTML/text save over the board ----

const HTTP_HTML: &str = "<h1 onclick=\"x()\">Hi {{first_name|friend}}</h1><script>alert(1)</script><a href=\"javascript:alert(1)\">bad</a><img src=\"https://t.example/p.gif\" width=\"1\" height=\"1\"><p>Body</p>";

#[test]
fn cad1056_http_html_save_matches_rpc_and_is_as_strict() {
    let b = Board::new();
    let base = b.base();
    let saved = b.value(
        "POST",
        &format!("{base}/campaigns"),
        json!({"campaign_id": "launch-h", "subject": "Own", "html": HTTP_HTML, "text": "Plain {{first_name|friend}}"}),
    );
    let doc = &saved["content"];
    assert_eq!(doc["mode"], "html");
    let stored = doc["html"].as_str().unwrap();
    for bad in ["<script", "onclick", "javascript:", "t.example"] {
        assert!(!stored.contains(bad), "{bad} stored: {stored}");
    }
    // Same bytes as the daemon RPC, and the render keeps the host footer.
    assert_eq!(b.rpc_show("launch-h")["content"], *doc);
    let rendered = b.value(
        "POST",
        &format!("{base}/campaigns/launch-h/render"),
        json!({"sample_first_name": "Amina"}),
    );
    assert!(rendered["render"]["html"]
        .as_str()
        .unwrap()
        .contains("Unsubscribe</a>"));
    assert_eq!(rendered["render"]["send_ready"], false);
    assert!(rendered["render"]["text"]
        .as_str()
        .unwrap()
        .contains("Plain Amina"));

    // Stale CAS, both-or-neither body and footer spoof refuse over HTTP
    // exactly as on the RPC, and nothing mutates.
    let refusals = [
        json!({"campaign_id": "launch-h", "subject": "x", "html": "<p>x</p>"}),
        json!({"campaign_id": "launch-h", "subject": "x", "html": "<p>x</p>", "expected_revision": 9}),
        json!({"campaign_id": "launch-h", "subject": "x", "expected_revision": 1}),
        json!({"campaign_id": "launch-h", "subject": "x", "html": "<p>x</p>", "blocks": blocks(), "expected_revision": 1}),
        json!({"campaign_id": "launch-h", "subject": "x", "html": "<a href=\"https://cadence.invalid/unsubscribe?token=RECIPIENT\">u</a>", "expected_revision": 1}),
        json!({"campaign_id": "launch-h", "subject": "x", "html": "<script>1</script>", "expected_revision": 1}),
    ];
    for body in refusals {
        let (code, text) = b.operator("POST", &format!("{base}/campaigns"), &body.to_string());
        assert!((400..500).contains(&code), "{body} admitted: {code} {text}");
    }
    // Forged identity/derived fields refuse at the transport schema.
    for forged in [
        json!({"actor": "operator"}),
        json!({"by": "operator"}),
        json!({"sanitized_html": "<p>x</p>"}),
        json!({"content_digest": "abc"}),
        json!({"install_id": "other"}),
    ] {
        let mut body = json!({"campaign_id": "launch-h", "subject": "x", "html": "<p>x</p>", "expected_revision": 1});
        for (k, v) in forged.as_object().unwrap() {
            body[k] = v.clone();
        }
        assert_eq!(
            b.operator("POST", &format!("{base}/campaigns"), &body.to_string())
                .0,
            400,
            "forged {forged}"
        );
    }
    assert_eq!(b.rpc_show("launch-h")["content"], *doc);

    // A good second save bumps the revision and resets approval.
    b.daemon
        .operator_rpc("app_content_approve", json!({"install_id": b.install, "context_id": b.context_id, "campaign_id": "launch-h", "expected_revision": 1}))
        .unwrap();
    let again = b.value(
        "POST",
        &format!("{base}/campaigns"),
        json!({"campaign_id": "launch-h", "subject": "Own", "html": "<p>Second</p>", "expected_revision": 1}),
    );
    assert_eq!(again["content"]["revision"], 2);
    assert_eq!(again["content"]["approval"]["valid"], false);
}

#[test]
fn cad1056_http_html_save_agent_and_detached_get_403() {
    let b = Board::new();
    let base = b.base();
    let created = b.value(
        "POST",
        &format!("{base}/campaigns"),
        json!({"campaign_id": "launch-1", "subject": "Spring launch", "blocks": blocks()}),
    );
    let mut lane = LaneShell::spawn(b.root.path());
    plant_member_pane(
        &b.daemon,
        "content-html-http-worker",
        "claude",
        None,
        lane.pid(),
    );
    let bodies = [
        json!({"campaign_id": "launch-1", "subject": "Evil", "html": HTTP_HTML, "expected_revision": 1}),
        json!({"campaign_id": "launch-evil", "subject": "Evil", "html": HTTP_HTML, "text": "evil"}),
    ];
    let mut failures = Vec::new();
    for prefix in ["", "setsid "] {
        for body in &bodies {
            let stolen =
                common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
            let wire =
                stolen.request_as("POST", &format!("{base}/campaigns"), &body.to_string(), "");
            let file = lane
                .dir
                .path()
                .join(format!("html-request-{}.txt", lane.seq));
            std::fs::write(&file, wire).unwrap();
            let (rc, response) = lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));s.sendall(open(sys.argv[2],\"rb\").read());print(s.makefile().readline())' {} {}", b.port, file.display()));
            assert_eq!(rc, 0);
            let status = response.split_whitespace().nth(1).unwrap_or("missing");
            if status != "403" {
                failures.push(format!("{prefix:?} {body}: {status}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "HTML save peer guard failed: {failures:?}"
    );
    // Sessionless write refuses too.
    let host = common::op::board_host(b.port);
    let raw = json!({"campaign_id": "launch-1", "subject": "x", "html": "<p>x</p>", "expected_revision": 1}).to_string();
    let bare = format!("POST {base}/campaigns HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{raw}", raw.len());
    assert_eq!(
        common::op::raw(b.port, &bare).0,
        403,
        "sessionless HTML save admitted"
    );
    assert_eq!(b.rpc_show("launch-1")["content"], created["content"]);
}
