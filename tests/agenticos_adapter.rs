//! CAD-501: the AgenticOS adapter against a deterministic fake of the
//! runtime door. Covers the hosted path (no credential), the scoped-grant
//! bearer stub, refusal, pending, receipt mismatch and idempotent retry.
//! The daemon-path test drives two identical `platform_call`s through
//! `execute_immediate`, which mints a fresh `call-<uuid>` each time.

// The daemon-path test plants a bash pane. A test binary never runs the
// CAD-308 reaper, so that spawn does not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cadence_agent::contract_fixture::{classify_call, Effect, Verified};
use cadence_agent::platform::agenticos::{publish_content_digest, AgenticosAdapter};
use cadence_agent::platform::PlatformAdapter;
use serde_json::{json, Value};

#[derive(Clone, Copy)]
enum Script {
    Pending,
    Declined,
    Posted,
    Mismatch,
}

struct Rec {
    digest: String,
    data: Value,
}

struct State {
    script: Script,
    records: HashMap<String, Rec>,
    /// New send rows. A repeat of an existing key does not increment.
    writes: AtomicUsize,
    /// Every accepted `POST /publish`, including an idempotent repeat.
    posts: AtomicUsize,
    last_auth: Mutex<Option<String>>,
    last_key: Mutex<Option<String>>,
    last_digest: Mutex<Option<String>>,
}

struct Door {
    base: String,
    stop: Arc<AtomicBool>,
    state: Arc<Mutex<State>>,
    join: Option<thread::JoinHandle<()>>,
}

impl Drop for Door {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let host = self.base.trim_start_matches("http://");
        let _ = std::net::TcpStream::connect(host);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn header<'a>(req: &'a tiny_http::Request, name: &'static str) -> Option<&'a str> {
    req.headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str())
}

fn respond(req: tiny_http::Request, status: u16, body: Value) {
    let header =
        tiny_http::Header::from_bytes(&b"content-type"[..], &b"application/json"[..]).unwrap();
    let resp = tiny_http::Response::from_string(body.to_string())
        .with_status_code(status)
        .with_header(header);
    let _ = req.respond(resp);
}

fn start(script: Script) -> (Door, AgenticosAdapter) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let server = tiny_http::Server::http(format!("127.0.0.1:{port}")).unwrap();
    let state = Arc::new(Mutex::new(State {
        script,
        records: HashMap::new(),
        writes: AtomicUsize::new(0),
        posts: AtomicUsize::new(0),
        last_auth: Mutex::new(None),
        last_key: Mutex::new(None),
        last_digest: Mutex::new(None),
    }));
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = Arc::clone(&stop);
    let state_t = Arc::clone(&state);
    let join = thread::spawn(move || {
        while !stop_t.load(Ordering::SeqCst) {
            let req = match server.recv_timeout(Duration::from_millis(200)) {
                Ok(Some(req)) => req,
                Ok(None) => continue,
                Err(_) => break,
            };
            if stop_t.load(Ordering::SeqCst) {
                break;
            }
            handle(req, &state_t);
        }
    });
    let base = format!("http://127.0.0.1:{port}");
    let adapter = AgenticosAdapter::new(&base).unwrap();
    (
        Door {
            base,
            stop,
            state,
            join: Some(join),
        },
        adapter,
    )
}

fn handle(mut req: tiny_http::Request, state: &Mutex<State>) {
    let method = req.method().as_str().to_string();
    let path = req.url().to_string();
    let key_hdr = header(&req, "idempotency-key").map(str::to_string);
    let digest_hdr = header(&req, "content-digest").map(str::to_string);
    let auth = header(&req, "authorization").map(str::to_string);
    let mut body = String::new();
    let _ = req.as_reader().take(64 * 1024).read_to_string(&mut body);
    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);

    if method == "GET" {
        let key = path.rsplit('/').next().unwrap_or("");
        let guard = state.lock().unwrap();
        match guard.records.get(key) {
            Some(rec) => {
                let data = rec.data.clone();
                drop(guard);
                respond(req, 200, json!({"ok": true, "data": data}));
            }
            None => {
                drop(guard);
                respond(
                    req,
                    404,
                    json!({"ok": false, "error": {"code": "not_found", "message": "not_found"}}),
                );
            }
        }
        return;
    }

    if method != "POST" || !path.starts_with("/v1/runtime/connectors/publish") {
        respond(
            req,
            404,
            json!({"ok": false, "error": {"code": "not_found", "message": "not_found"}}),
        );
        return;
    }
    if !parsed.is_object() {
        respond(
            req,
            400,
            json!({"ok": false, "error": {"code": "invalid_request", "message": "invalid_request"}}),
        );
        return;
    }
    let Some(key) = key_hdr else {
        respond(
            req,
            400,
            json!({"ok": false, "error": {"code": "invalid_request", "message": "invalid_request"}}),
        );
        return;
    };
    let digest = digest_hdr.unwrap_or_default();
    let mut guard = state.lock().unwrap();
    guard.posts.fetch_add(1, Ordering::SeqCst);
    *guard.last_auth.lock().unwrap() = auth;
    *guard.last_key.lock().unwrap() = Some(key.clone());
    *guard.last_digest.lock().unwrap() = Some(digest.clone());
    if let Some(prev) = guard.records.get(&key) {
        if prev.digest != digest {
            drop(guard);
            respond(
                req,
                409,
                json!({"ok": false, "error": {"code": "digest_mismatch", "message": "digest_mismatch"}}),
            );
            return;
        }
        let mut data = prev.data.clone();
        data["repeated"] = json!(true);
        drop(guard);
        respond(req, 200, json!({"ok": true, "data": data}));
        return;
    }
    let script = guard.script;
    let (status, decision, post_status, permalink, receipt) = match script {
        Script::Pending => (202, "pending", "pending", Value::Null, Value::Null),
        Script::Declined => (409, "declined", "declined", Value::Null, Value::Null),
        Script::Posted => (
            200,
            "approved",
            "posted",
            json!("https://example.test/p/1"),
            json!(digest),
        ),
        Script::Mismatch => (
            200,
            "approved",
            "posted",
            json!("https://example.test/p/1"),
            json!("0000000000000000000000000000000000000000000000000000000000000000"),
        ),
    };
    let data = json!({
        "key": key,
        "decision": decision,
        "executed": post_status == "posted",
        "status": post_status,
        "permalink": permalink,
        "repeated": false,
        "contentDigest": receipt,
    });
    guard.records.insert(
        key,
        Rec {
            digest,
            data: data.clone(),
        },
    );
    guard.writes.fetch_add(1, Ordering::SeqCst);
    drop(guard);
    respond(req, status, json!({"ok": true, "data": data}));
}

fn input(caption: &str) -> Value {
    json!({"connectionId": "conn_1", "caption": caption})
}

const KEY: &str = "publish-key-1";

#[test]
fn publish_post_classifies_as_a_cadence_draft() {
    let (_door, adapter) = start(Script::Pending);
    let reported = adapter.reported_manifest_version();
    assert_eq!(
        classify_call(adapter.table(), reported.as_deref(), "publish_post"),
        Effect::Draft
    );
    assert_eq!(
        classify_call(adapter.table(), reported.as_deref(), "connections_list"),
        Effect::Read
    );
}

#[test]
fn pending_is_waiting_in_agenticos() {
    let (door, adapter) = start(Script::Pending);
    let out = adapter
        .execute(&[], "publish_post", &input("hello"), KEY, None)
        .unwrap();
    assert_eq!(out["ledger"], "waiting");
    assert_eq!(out["detail"], "waiting in AgenticOS");
    assert!(out["deep_link"].as_str().unwrap().starts_with("https://"));
    assert_eq!(out["handoff"], "draft");
    assert_eq!(
        out["content_hash"],
        format!("sha256:{}", publish_content_digest("conn_1", "hello", None))
    );
    assert_eq!(
        adapter.read_back("publish_post", &input("hello")),
        Verified::Unknown
    );
    let state = door.state.lock().unwrap();
    assert_eq!(
        state.last_key.lock().unwrap().as_deref(),
        Some(derived_key("hello").as_str())
    );
    assert_eq!(
        state.last_digest.lock().unwrap().as_deref(),
        Some(publish_content_digest("conn_1", "hello", None).as_str())
    );
    assert!(state.last_auth.lock().unwrap().is_none());
}

#[test]
fn declined_is_refused() {
    let (_door, adapter) = start(Script::Declined);
    let out = adapter
        .execute(&[], "publish_post", &input("hello"), KEY, None)
        .unwrap();
    assert_eq!(out["ledger"], "refused");
    assert_eq!(out["detail"], "refused in AgenticOS");
    assert_eq!(out["permalink"], Value::Null);
    assert_eq!(
        adapter.read_back("publish_post", &input("hello")),
        Verified::False
    );
}

#[test]
fn receipt_mismatch_fails_verification() {
    let (_door, adapter) = start(Script::Mismatch);
    let out = adapter
        .execute(&[], "publish_post", &input("hello"), KEY, None)
        .unwrap();
    assert_eq!(out["ledger"], "published");
    assert_eq!(out["verified"], false);
    assert_eq!(
        out["detail"],
        "receipt does not match the approved revision"
    );
    assert_eq!(
        adapter.read_back("publish_post", &input("hello")),
        Verified::False
    );
}

#[test]
fn retry_returns_the_recorded_outcome_once() {
    let (door, adapter) = start(Script::Posted);
    let hash = format!("sha256:{}", publish_content_digest("conn_1", "hello", None));
    // Distinct gate keys. The door key is the revision, not these.
    let first = adapter
        .execute(
            &[],
            "publish_post",
            &input("hello"),
            "call-aaaaaaaa",
            Some(&hash),
        )
        .unwrap();
    assert_eq!(first["ledger"], "published");
    assert_eq!(first["verified"], true);
    assert_eq!(first["permalink"], "https://example.test/p/1");
    assert_eq!(first["repeated"], false);
    assert_eq!(first["platform_ref"], derived_key("hello"));
    assert_eq!(
        adapter.read_back("publish_post", &input("hello")),
        Verified::True
    );
    let second = adapter
        .execute(
            &[],
            "publish_post",
            &input("hello"),
            "call-bbbbbbbb",
            Some(&hash),
        )
        .unwrap();
    assert_eq!(second["repeated"], true);
    assert_eq!(second["permalink"], first["permalink"]);
    assert_eq!(second["platform_ref"], first["platform_ref"]);
    assert_eq!(door.state.lock().unwrap().writes.load(Ordering::SeqCst), 1);
    assert_eq!(door.state.lock().unwrap().posts.load(Ordering::SeqCst), 1);

    // A different caption is a different revision, so a different key
    // and a second row. The first key is never reused against new bytes.
    let other = adapter
        .execute(
            &[],
            "publish_post",
            &input("other caption"),
            "call-cccccccc",
            None,
        )
        .unwrap();
    assert_eq!(other["platform_ref"], derived_key("other caption"));
    assert_ne!(other["platform_ref"], first["platform_ref"]);
    assert_eq!(door.state.lock().unwrap().writes.load(Ordering::SeqCst), 2);
}

#[test]
fn a_wrong_expected_hash_is_not_posted() {
    let (door, adapter) = start(Script::Posted);
    let err = adapter
        .execute(
            &[],
            "publish_post",
            &input("hello"),
            KEY,
            Some("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        )
        .unwrap_err();
    assert!(err.contains("does not match"), "{err}");
    assert_eq!(door.state.lock().unwrap().writes.load(Ordering::SeqCst), 0);
}

#[test]
fn hosted_sends_no_credential_and_a_grant_sends_a_bearer() {
    let (door, adapter) = start(Script::Posted);
    adapter
        .execute(&[], "publish_post", &input("hello"), KEY, None)
        .unwrap();
    assert!(door
        .state
        .lock()
        .unwrap()
        .last_auth
        .lock()
        .unwrap()
        .is_none());

    let (door, adapter) = start(Script::Posted);
    let token = b"cadp_scoped_test";
    adapter
        .execute(
            token,
            "publish_post",
            &input("hello"),
            "publish-key-2",
            None,
        )
        .unwrap();
    assert_eq!(
        door.state
            .lock()
            .unwrap()
            .last_auth
            .lock()
            .unwrap()
            .as_deref(),
        Some("Bearer cadp_scoped_test")
    );
}

/// CompanyControl keys match `^[A-Za-z0-9_-]{8,128}$`. The derivation
/// the adapter must use: the gate's `call-<uuid>` is not this string.
fn derived_key(caption: &str) -> String {
    format!(
        "agenticos-publish-v1-{}",
        publish_content_digest("conn_1", caption, None)
    )
}

#[path = "support/operator.rs"]
mod op;

fn proc_start(pid: u32) -> Option<i64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

/// In-process daemon with the AgenticOS adapter already registered, so
/// `attach` leaves this instance in place.
struct CallDaemon {
    state: PathBuf,
    _dir: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl CallDaemon {
    fn start(adapter: Arc<AgenticosAdapter>) -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let state = dir.path().join("state");
        let pm = dir.path().join("pm");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&pm).unwrap();
        let env = cadence_agent::adapter::ProviderEnv::default();
        env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let mut platforms = HashMap::new();
        platforms.insert(
            cadence_agent::platform::agenticos::PLATFORM.to_string(),
            adapter as Arc<dyn cadence_agent::platform::PlatformAdapter>,
        );
        let opts = cadence_agent::daemon::ServeOptions {
            provider_env: env,
            report_router: Some(0),
            auto_stop: Some(cadence_agent::daemon::AutoStopSetting::off()),
            slots: Some(cadence_agent::slots::SlotConfig::default()),
            agent_gc: Some(cadence_agent::daemon::AgentGcSetting::default()),
            stop: Some(stop.clone()),
            platforms,
            test_seam: cfg!(feature = "test-seam"),
            ..Default::default()
        };
        let owned = state.clone();
        let handle = thread::spawn(move || {
            let _ = cadence_agent::daemon::serve_with(&owned, opts);
        });
        let d = Self {
            state,
            _dir: dir,
            stop,
            handle: Some(handle),
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        while cadence_agent::client::rpc(&d.state, "health", json!({})).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become healthy");
            thread::sleep(Duration::from_millis(50));
        }
        d
    }

    fn op(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let frame = op::operator_rpc(
            &cadence_agent::client::socket_path(&self.state),
            method,
            params,
        );
        cadence_agent::proto::unwrap(frame)
    }
}

impl Drop for CallDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

struct Lane {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    dir: tempfile::TempDir,
    seq: u64,
}

impl Lane {
    fn spawn(d: &CallDaemon, alias: &str) -> Self {
        let mut child = Command::new("bash")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let lane = Self {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            dir: tempfile::TempDir::new().unwrap(),
            seq: 0,
        };
        d.op(
            "agent_register",
            json!({"alias": alias, "provider": "inbox", "endpoint_kind": "inbox",
                   "cwd": d.state.to_str().unwrap()}),
        )
        .unwrap_or_else(|e| panic!("register {alias}: {e}"));
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "UPDATE agents SET endpoint_kind='pty', pid=?1, pid_start=?3, \
                enabled=0, generation='planted', session_id='planted' WHERE alias=?2",
            rusqlite::params![lane.child.id() as i64, alias, proc_start(lane.child.id())],
        )
        .unwrap();
        lane
    }

    fn rpc(&mut self, d: &CallDaemon, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let req = self.dir.path().join(format!("req-{}.json", self.seq));
        std::fs::write(
            &req,
            cadence_agent::proto::request(method, params).to_string(),
        )
        .unwrap();
        let tag = format!("__lane_rc_{}__", self.seq);
        self.seq += 1;
        writeln!(
            self.stdin,
            "{{ python3 -c 'import socket,sys;\
             s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);\
             s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");\
             print(s.makefile().readline())' {} {} ; }} 2>&1; rc=$?; echo; echo {tag}$rc",
            cadence_agent::client::socket_path(&d.state).display(),
            req.display()
        )
        .unwrap();
        self.stdin.flush().unwrap();
        let mut out = String::new();
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "lane shell exited"
            );
            if let Some(rc) = line.strip_prefix(&tag) {
                assert_eq!(rc.trim(), "0", "lane rpc failed: {out}");
                break;
            }
            out.push_str(&line);
        }
        let frame: Value = serde_json::from_str(out.trim()).unwrap();
        cadence_agent::proto::unwrap(frame)
    }
}

impl Drop for Lane {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Two identical `platform_call` publish_posts go through
/// `execute_immediate`, which mints a new `call-<uuid>` on every RPC.
/// The door must still see one key and one send row, and the second
/// call must return that row's mirrored status.
#[test]
fn two_platform_calls_share_one_agenticos_row() {
    let (door, adapter) = start(Script::Posted);
    let adapter = Arc::new(adapter);
    let d = CallDaemon::start(Arc::clone(&adapter));
    let mut lane = Lane::spawn(&d, "w1");
    d.op(
        "platform_grant",
        json!({"agent": "w1", "platform": "agenticos", "account": "hosted",
               "scopes": ["publish"]}),
    )
    .unwrap();

    let call = json!({"platform": "agenticos", "account": "hosted",
                      "tool": "publish_post", "input": input("hello")});
    let first = lane
        .rpc(&d, "platform_call", call.clone())
        .unwrap_or_else(|e| panic!("first publish_post: {e}"));
    let second = lane
        .rpc(&d, "platform_call", call)
        .unwrap_or_else(|e| panic!("second publish_post: {e}"));

    let key = derived_key("hello");
    let first_result = &first["platform_result"];
    let second_result = &second["platform_result"];
    assert_eq!(first["result"], "executed", "{first}");
    assert_eq!(first_result["platform_ref"], key, "{first_result}");
    assert_eq!(second_result["platform_ref"], key, "{second_result}");
    assert_eq!(second_result["ledger"], first_result["ledger"]);
    assert_eq!(second_result["status"], first_result["status"]);
    assert_eq!(second_result["permalink"], first_result["permalink"]);
    assert_eq!(second_result["repeated"], true, "{second_result}");
    assert_eq!(
        second_result["content_hash"],
        format!("sha256:{}", publish_content_digest("conn_1", "hello", None))
    );

    let state = door.state.lock().unwrap();
    assert_eq!(
        state.records.len(),
        1,
        "two platform_calls created {} AgenticOS rows: {:?}",
        state.records.len(),
        state.records.keys().collect::<Vec<_>>()
    );
    assert!(
        state.records.contains_key(&key),
        "row key was {:?}, want {key}",
        state.records.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        state.posts.load(Ordering::SeqCst),
        1,
        "a repeat publish_post posted again instead of mirroring the row"
    );
    assert_eq!(state.writes.load(Ordering::SeqCst), 1);
}
