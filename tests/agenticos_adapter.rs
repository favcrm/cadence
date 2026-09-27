//! CAD-501: the AgenticOS adapter against a deterministic fake of the
//! runtime door. Covers the hosted path (no credential), the scoped-grant
//! bearer stub, refusal, pending, receipt mismatch and idempotent retry.

use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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
    writes: AtomicUsize,
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
    assert_eq!(state.last_key.lock().unwrap().as_deref(), Some(KEY));
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
    let first = adapter
        .execute(&[], "publish_post", &input("hello"), KEY, Some(&hash))
        .unwrap();
    assert_eq!(first["ledger"], "published");
    assert_eq!(first["verified"], true);
    assert_eq!(first["permalink"], "https://example.test/p/1");
    assert_eq!(first["repeated"], false);
    assert_eq!(
        adapter.read_back("publish_post", &input("hello")),
        Verified::True
    );
    let second = adapter
        .execute(&[], "publish_post", &input("hello"), KEY, Some(&hash))
        .unwrap();
    assert_eq!(second["repeated"], true);
    assert_eq!(second["permalink"], first["permalink"]);
    assert_eq!(door.state.lock().unwrap().writes.load(Ordering::SeqCst), 1);

    let err = adapter
        .execute(&[], "publish_post", &input("other caption"), KEY, None)
        .unwrap_err();
    assert!(err.contains("digest_mismatch"), "{err}");
    assert_eq!(door.state.lock().unwrap().writes.load(Ordering::SeqCst), 1);
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
