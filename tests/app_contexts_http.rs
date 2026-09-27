//! Populated operator controls and real native HTTP peer context authority.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_member_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
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
    install: String,
}
impl Board {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let mut opts = daemon_opts();
        opts.test_seam = false;
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let daemon = TestDaemon::start_opts(opts);
        let source = root.path().join("bundle");
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        let original = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("apps/local-content");
        std::fs::copy(original.join("app.md"), source.join("app.md")).unwrap();
        let text = std::fs::read_to_string(original.join("workflows/draft.md")).unwrap();
        let text = text.replace("source: { ask:", "source: { context_default: true, ask:");
        assert!(text.contains("context_default: true"));
        std::fs::write(source.join("workflows/draft.md"), text).unwrap();
        let installed = daemon
            .operator_rpc("app_workspace_install", json!({"source":source}))
            .unwrap();
        let install = installed["install_id"].as_str().unwrap().to_owned();
        let port = (3110..3200)
            .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let opts = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.clone()),
            test_seam: false,
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
            install,
        }
    }
    fn path(&self) -> String {
        format!("/api/app-installations/{}/contexts", self.install)
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
            "populated context operator request {method} {path}: {result}"
        );
        serde_json::from_str(&result).unwrap()
    }
    fn create(&self) -> Value {
        self.value("POST", &self.path(), json!({"label":"Client A", "input_defaults":{"source":"cad690-client-a-private-facts"},"request_id":"context-http-a"}))["context"].clone()
    }
    fn persisted(&self) -> Value {
        let db = rusqlite::Connection::open(self.daemon.state.join("cadence.sqlite3")).unwrap();
        let mut counts = serde_json::Map::new();
        for table in [
            "app_contexts",
            "app_runs",
            "app_grants",
            "platform_grants",
            "platform_effects",
            "events",
        ] {
            let count: i64 = db
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            counts.insert(table.into(), json!(count));
        }
        json!({"counts":counts,"contexts":self.value("GET",&self.path(),json!({}))})
    }
}
impl Drop for Board {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap().unwrap();
    }
}

#[test]
fn cad690_http_context_controls_are_populated_strict_and_revision_pinned() {
    let b = Board::new();
    let first = b.create();
    let id = first["id"].as_str().unwrap();
    assert_eq!(first["install_id"], b.install);
    assert_eq!(first["revision"], 1);
    let path = format!("{}/{id}", b.path());
    assert_eq!(b.value("GET", &path, json!({}))["context"], first);
    assert_eq!(b.value("GET", &b.path(), json!({}))["contexts"][0], first);
    let before = b.persisted();
    let create = json!({"label":"Forged", "input_defaults":{"source":"allowed-content"},"request_id":"bad-request","operator":true});
    let cases = vec![
        (b.path(), create.to_string()),
        (
            b.path(),
            r#"{"label":"Bad","input_defaults":{"source":1},"request_id":"bad-type"}"#.into(),
        ),
        (
            b.path(),
            r#"{"label":"Bad","label":"Duplicate","input_defaults":{},"request_id":"duplicate"}"#
                .into(),
        ),
        (
            format!("{path}/update"),
            r#"{"expected_revision":null,"label":"Bad","input_defaults":{}}"#.into(),
        ),
        (
            format!("{path}/archive"),
            r#"{"expected_revision":1,"context_id":"another"}"#.into(),
        ),
    ];
    for (path, body) in cases {
        assert_eq!(
            b.operator("POST", &path, &body).0,
            400,
            "strict schema accepted {path}"
        );
    }
    assert_eq!(
        b.operator("GET", &format!("{}?context_id=other", b.path()), "")
            .0,
        400
    );
    assert_eq!(
        b.persisted(),
        before,
        "invalid HTTP request changed persisted state"
    );
    let update = json!({"expected_revision":1,"label":"Client A updated","input_defaults":{"source":"cad690-client-a-revised-facts"}});
    let second = b.value("POST", &format!("{path}/update"), update.clone())["context"].clone();
    assert_eq!(second["id"], id);
    assert_eq!(second["revision"], 2);
    assert_ne!(second["digest"], first["digest"]);
    assert_ne!(
        b.operator("POST", &format!("{path}/update"), &update.to_string())
            .0,
        200
    );
    assert_eq!(b.value("GET", &path, json!({}))["context"], second);
    let archived = b.value(
        "POST",
        &format!("{path}/archive"),
        json!({"expected_revision":2}),
    )["context"]
        .clone();
    assert_eq!(archived["id"], id);
    assert_eq!(archived["state"], "archived");
    assert_eq!(archived["revision"], 3);
    assert_eq!(b.value("GET", &path, json!({}))["context"], archived);
    assert!(
        cadence_agent::issue::project::list(&b.root.path().join("pm"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn cad690_actual_agent_and_setsid_http_contexts_refuse_stolen_operator_session() {
    let b = Board::new();
    let populated = b.create();
    let path = format!("{}/{}", b.path(), populated["id"].as_str().unwrap());
    assert_eq!(b.value("GET", &path, json!({}))["context"], populated);
    let before = b.persisted();
    let mut lane = LaneShell::spawn(b.root.path());
    plant_member_pane(&b.daemon, "context-http-worker", "claude", None, lane.pid());
    let create = json!({"label":"Native forged", "input_defaults":{"source":"agent-written-facts"},"request_id":"agent-context"});
    let cases=vec![
        ("GET",b.path(),String::new()),
        ("GET",path.clone(),String::new()),
        ("POST",b.path(),create.to_string()),
        ("POST",b.path(),json!({"label":"Forged", "input_defaults":{},"request_id":"forged","agent":"operator","operator":true}).to_string()),
        ("POST",format!("{path}/update"),json!({"expected_revision":1,"label":"Overwritten","input_defaults":{}}).to_string()),
        ("POST",format!("{path}/archive"),json!({"expected_revision":1}).to_string()),
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
            let (rc,response)=lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));s.sendall(open(sys.argv[2],\"rb\").read());print(s.makefile().readline())' {} {}",b.port,file.display()));
            assert_eq!(rc, 0, "HTTP native process failed");
            let status = response.split_whitespace().nth(1).unwrap_or("missing");
            eprintln!("context HTTP peer prefix={prefix:?} {method} {path}: {status}");
            if status != "403" {
                failures.push(format!("{prefix:?} {method} {path}: {status}"));
            }
        }
    }
    assert_eq!(
        b.persisted(),
        before,
        "rejected native peer changed context state"
    );
    assert!(
        failures.is_empty(),
        "context HTTP operator peer guard failed: {failures:?}"
    );
}
