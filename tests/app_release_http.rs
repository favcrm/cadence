//! Real reviewed artifacts and operator HTTP release authority.
#![allow(clippy::disallowed_methods)]
mod common;
use common::{
    app_release::{Release, A},
    plant_member_pane, LaneShell,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
struct Board {
    release: Release,
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
}
impl Board {
    fn new() -> Self {
        let release = Release::new();
        let port = (3110..3200)
            .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let mut board = Self {
            release,
            port,
            stop,
            thread: None,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                Instant::now() < deadline,
                "release board startup deadline exhausted"
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
            let state = board.release.daemon.state.clone();
            let pm_dir = board.release.root.path().join("pm");
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
                    Ok(Err(error)) => eprintln!("release board startup contention: {error}"),
                    unexpected => panic!("release board bind failure returned {unexpected:?}"),
                }
                board.port = board
                    .port
                    .checked_add(1)
                    .filter(|port| *port < 3200)
                    .expect("release board startup exhausted permitted ports");
                board.stop.store(false, Ordering::SeqCst);
            } else {
                panic!("release board startup notification {notification:?}; worker {result:?}");
            }
        }
    }
    fn bindings(&self) -> String {
        format!(
            "/api/app-installations/{}/bindings",
            self.release.install["install_id"].as_str().unwrap()
        )
    }
    fn operator(&self, method: &str, path: &str, body: &str) -> (u16, String) {
        let session = common::op::sign_in(
            env!("CARGO_BIN_EXE_cadence"),
            &self.release.daemon.state,
            self.port,
        );
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
            "populated release operator {method} {path}: {result}"
        );
        serde_json::from_str(&result).unwrap()
    }
    fn populate(&self) -> (Value, Value, Value) {
        let context = self.release.context("HTTP Client", A, "http-context");
        let binding = self.value("POST", &self.bindings(), json!({"context_id":context["id"],"slot":"publication","connection_id":self.release.connection,"request_id":"http-binding"}))["binding"].clone();
        let run = self.release.complete(&context, "http-run");
        let stage = format!("/api/app-runs/{}/effects", run["id"].as_str().unwrap());
        let effect = self.value("POST", &stage, json!({"artifact_id":run["artifacts"][0]["id"],"slot":"publication","request_id":"http-stage","title":"HTTP reviewed draft"}))["effect"].clone();
        (binding, run, effect)
    }
    fn persisted(&self) -> Value {
        let bindings = self.value("GET", &self.bindings(), json!({}));
        let effects = self.value("GET", "/api/app-effects", json!({}));
        let db =
            rusqlite::Connection::open(self.release.daemon.state.join("cadence.sqlite3")).unwrap();
        let grants: i64 = db
            .query_row("SELECT count(*) FROM platform_grants", [], |r| r.get(0))
            .unwrap();
        json!({"bindings":bindings,"effects":effects,"items":self.release.items(),"grants":grants})
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
                eprintln!("release board worker cleanup after primary panic: {result:?}");
            }
        } else {
            result.unwrap().unwrap();
        }
    }
}

#[test]
fn cad692_http_release_is_populated_strict_and_requires_exact_digest() {
    let b = Board::new();
    let (binding, run, effect) = b.populate();
    assert_eq!(effect["state"], "waiting");
    assert!(b.release.items().as_array().unwrap().is_empty());
    let binding_path = format!("{}/{}", b.bindings(), binding["id"].as_str().unwrap());
    let effect_path = format!("/api/app-effects/{}", effect["effect_id"].as_str().unwrap());
    assert_eq!(b.value("GET", &binding_path, json!({}))["binding"], binding);
    assert_eq!(b.value("GET", &effect_path, json!({}))["effect"], effect);
    let before = b.persisted();
    let stage = format!("/api/app-runs/{}/effects", run["id"].as_str().unwrap());
    for (path, body) in [
        (b.bindings(), r#"{"slot":"publication","connection_id":"a","request_id":"bad","context_id":null}"#.to_owned()),
        (format!("{binding_path}/revoke"), r#"{"expected_revision":1,"expected_revision":2}"#.to_owned()),
        (format!("{binding_path}/update"), r#"{"expected_revision":1,"connection_id":"a","operator":true}"#.to_owned()),
        (stage.clone(), json!({"artifact_id":run["artifacts"][0]["id"],"slot":"publication","request_id":"bad","title":"Title","body":"caller supplied content"}).to_string()),
        (format!("{effect_path}/decide"), json!({"digest":effect["digest"],"decision":"accept","effect_id":"another"}).to_string()),
    ] { assert_eq!(b.operator("POST", &path, &body).0, 400, "strict request accepted {path}"); }
    assert_eq!(
        b.operator("GET", "/api/app-effects?install_id=other", "").0,
        400
    );
    assert_ne!(
        b.operator(
            "POST",
            &format!("{effect_path}/decide"),
            &json!({"digest":"wrong","decision":"accept"}).to_string()
        )
        .0,
        200
    );
    assert_eq!(
        b.persisted(),
        before,
        "refused HTTP call changed effect/binding/outbox/grants"
    );
    let done = b.value(
        "POST",
        &format!("{effect_path}/decide"),
        json!({"digest":effect["digest"],"decision":"accept"}),
    );
    assert_eq!(done["effect"]["state"], "done");
    assert_eq!(b.release.items().as_array().unwrap().len(), 1);
    let artifact = b.release.artifact(&run);
    assert!(artifact["text"].as_str().unwrap().contains(A));
    assert_eq!(
        b.value("GET", &effect_path, json!({}))["effect"]["state"],
        "done"
    );
    assert!(
        cadence_agent::issue::project::list(&b.release.root.path().join("pm"))
            .unwrap()
            .is_empty()
    );
}
#[test]
fn cad692_actual_agent_and_setsid_http_release_refuse_stolen_operator_sessions() {
    let b = Board::new();
    let (binding, run, effect) = b.populate();
    let before = b.persisted();
    let binding_path = format!("{}/{}", b.bindings(), binding["id"].as_str().unwrap());
    let effect_path = format!("/api/app-effects/{}", effect["effect_id"].as_str().unwrap());
    let mut lane = LaneShell::spawn(b.release.root.path());
    plant_member_pane(
        &b.release.daemon,
        "release-http-worker",
        "claude",
        None,
        lane.pid(),
    );
    let cases=vec![
        ("GET",b.bindings(),String::new()),
        ("GET",binding_path.clone(),String::new()),
        ("GET","/api/app-effects".to_owned(),String::new()),
        ("GET",effect_path.clone(),String::new()),
        ("POST",b.bindings(),json!({"context_id":binding["context_id"],"slot":"publication","connection_id":b.release.connection,"request_id":"stolen"}).to_string()),
        ("POST",format!("{binding_path}/update"),json!({"expected_revision":1,"connection_id":b.release.connection}).to_string()),
        ("POST",format!("{binding_path}/revoke"),json!({"expected_revision":1}).to_string()),
        ("POST",format!("/api/app-runs/{}/effects",run["id"].as_str().unwrap()),json!({"artifact_id":run["artifacts"][0]["id"],"slot":"publication","request_id":"forged","title":"Forged","operator":true}).to_string()),
        ("POST",format!("{effect_path}/decide"),json!({"digest":effect["digest"],"decision":"accept"}).to_string()),
    ];
    let mut failures = Vec::new();
    for prefix in ["", "setsid "] {
        for (method, path, body) in &cases {
            let stolen = common::op::sign_in(
                env!("CARGO_BIN_EXE_cadence"),
                &b.release.daemon.state,
                b.port,
            );
            let wire = stolen.request_as(method, path, body, "");
            assert!(!wire.contains(cadence_agent::test_seam::AS_HEADER));
            assert!(!wire.contains(cadence_agent::test_seam::TOKEN_HEADER));
            let file = lane
                .dir
                .path()
                .join(format!("release-http-{}.txt", lane.seq));
            std::fs::write(&file, wire).unwrap();
            let (rc,response)=lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));s.sendall(open(sys.argv[2],\"rb\").read());print(s.makefile().readline())' {} {}",b.port,file.display()));
            assert_eq!(rc, 0, "native HTTP request process failed");
            let status = response.split_whitespace().nth(1).unwrap_or("missing");
            if status != "403" {
                failures.push(format!("{prefix:?} {method} {path}: {status}"));
            }
        }
    }
    assert_eq!(
        b.persisted(),
        before,
        "native HTTP changed private release authority"
    );
    assert!(
        failures.is_empty(),
        "release HTTP operator peer guard failed: {failures:?}"
    );
}
