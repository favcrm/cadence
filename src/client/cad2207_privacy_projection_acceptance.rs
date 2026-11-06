//! CAD-2207 client privacy-projection acceptance — independently authored.
//!
//! The wire contract: every daemon response carries the capability marker
//! `fleet_protocol` on the response ENVELOPE (beside `ok`/`result`/`error`),
//! not inside `result`. A client may consume a daemon's fleet projection —
//! `agent_list` / `agent_operational` — or any other private-adjacent read
//! only when the answer that carries it is itself marked. A prior
//! `daemon_info` reply cannot authenticate a later frame: the daemon may have
//! been replaced between the two requests, and a long-lived connection may
//! carry an old capability forward.
//!
//! These checks drive the REAL client transports over a REAL Unix socket to a
//! synthetic daemon, and cover the failure the review found:
//!
//! - an old daemon answering with a personal alias/row/body/error and no
//!   marker: `rpc`, `rpc_deadline`, `MetaBudget::read`,
//!   `rpc_private_timeout`, `rpc_answer`, `RpcSession::answer` and
//!   `rpc_relay_timeout` must all refuse, and no refusal may echo the private
//!   data;
//! - a SAFE marked `daemon_info` followed by an UNMARKED fleet answer (the
//!   swapped-fleet case): the fleet read must still refuse;
//! - a marker placed only inside `result` is not a marker: the fleet read
//!   must refuse;
//! - a current daemon marking the SAME fleet answer it returns keeps the
//!   ordinary list usable;
//! - legacy (unmarked) `health` keeps only safe liveness — state, pid,
//!   agent_uid, protocol — plus `fleet_status_unavailable`, and never forwards
//!   its private waiter/fleet fields.
//!
//! Reviewer follow-up pinned here: a malformed legacy health (`result: null`,
//! a non-object result, or no POSITIVE numeric pid) must be a FAILURE, never
//! a successful sanitized "already running/ready" liveness answer. Only a
//! well-formed legacy health with a positive pid may yield the safe liveness
//! projection.
//!
//! Not exercised here (no honest socket seam): the `CADENCE_SOCKET` override
//! path (`rpc_socket_path`) — process-global env mutation would cross-test
//! contaminate under a threaded runner — and the sandbox-pinned `rpc_state`
//! path; both feed the same `rpc_frame_on` protector covered below. Remote
//! CLI/HTTP relay transports and the daemon-side marker injection live
//! outside this client module (the board-side acceptance covers the latter).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::{
    rpc, rpc_after_fleet_preflight, rpc_answer, rpc_deadline, rpc_private_timeout,
    rpc_relay_timeout, socket_path, MetaBudget, RpcSession,
};
use crate::proto;

/// `script(method, params)` answers one synthetic-daemon request.
type Script = Arc<dyn Fn(&str, &Value) -> Value + Send + Sync>;
/// `script(connection, method, params)` answers one request, or closes it.
type RecordingScript = Arc<dyn Fn(usize, &str, &Value) -> Option<Value> + Send + Sync>;
/// `(accepted connection index, method)` in arrival order.
type MethodLog = Arc<Mutex<Vec<(usize, String)>>>;

const PRIVATE_ALIAS: &str = "assist-agent-legacy-private";
const PRIVATE_MESSAGE: &str = "legacy-private-message";
const PRIVATE_BODY: &str = "legacy private turn body";
const ORDINARY_ALIAS: &str = "cad2207-ordinary-worker";

fn private_fleet() -> Value {
    json!({"agents": [{
        "alias": PRIVATE_ALIAS,
        "state": "idle",
        "provider": "pi",
        "endpoint_kind": "managed",
        "pid": 4242,
        "_private_body": PRIVATE_BODY,
    }]})
}

fn error_frame(message: &str) -> Value {
    json!({"ok": false, "error": {"kind": "rejected", "message": message}})
}

/// The scripted daemon answers each request line with a whole envelope; a
/// response is "current" only when the test itself puts `fleet_protocol` on
/// that envelope.
struct Scripted {
    replies: BTreeMap<&'static str, Value>,
}

impl Scripted {
    fn new() -> Self {
        Self {
            replies: BTreeMap::new(),
        }
    }

    fn on(mut self, method: &'static str, envelope: Value) -> Self {
        self.replies.insert(method, envelope);
        self
    }

    fn into_fn(self) -> impl Fn(&str, &Value) -> Value + Send + Sync + 'static {
        move |method, _params| {
            self.replies
                .get(method)
                .cloned()
                .unwrap_or_else(|| error_frame(&format!("unscripted method {method}")))
        }
    }
}

fn marked(mut envelope: Value) -> Value {
    envelope["fleet_protocol"] = json!(proto::CLIENT_PROTOCOL);
    envelope
}

struct SyntheticDaemon {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SyntheticDaemon {
    fn start(state: &Path, script: impl Fn(&str, &Value) -> Value + Send + Sync + 'static) -> Self {
        let socket = socket_path(state);
        if socket.exists() {
            std::fs::remove_file(&socket).expect("remove stale fixture socket");
        }
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent).expect("fixture socket dir");
        }
        let listener = UnixListener::bind(&socket).expect("bind fixture socket");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let script: Script = Arc::new(script);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                let reader = match stream.try_clone() {
                    Ok(clone) => clone,
                    Err(_) => continue,
                };
                let mut reader = BufReader::new(reader);
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                    if line.trim().is_empty() {
                        continue;
                    }
                    let request: Value = match serde_json::from_str(&line) {
                        Ok(request) => request,
                        Err(_) => break,
                    };
                    let method = request["method"].as_str().unwrap_or_default().to_string();
                    let params = request["params"].clone();
                    let envelope = script(&method, &params);
                    if writeln!(stream, "{envelope}")
                        .and_then(|()| stream.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for SyntheticDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn assert_no_private(text: &str, what: &str) {
    for secret in [PRIVATE_ALIAS, PRIVATE_MESSAGE, PRIVATE_BODY] {
        assert!(
            !text.contains(secret),
            "{what} exposed the old daemon's private data ({secret}): {text}"
        );
    }
}

fn assert_value_has_no_private(value: &Value, what: &str) {
    assert_no_private(&value.to_string(), what);
}

#[test]
fn old_daemon_private_answers_are_refused_by_every_client_transport() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = SyntheticDaemon::start(
        dir.path(),
        Scripted::new()
            // An old daemon: no envelope marker anywhere, and its result
            // still carries the private projection.
            .on(
                "daemon_info",
                proto::ok(json!({"build_commit": "legacy", "pid": 7})),
            )
            .on("agent_operational", proto::ok(private_fleet()))
            .on("agent_list", proto::ok(private_fleet()))
            .on(
                "thread_read",
                proto::ok(json!({"alias": PRIVATE_ALIAS, "body": PRIVATE_BODY})),
            )
            .on(
                "agent_events",
                error_frame(&format!(
                    "agent '{PRIVATE_ALIAS}' failed: {PRIVATE_MESSAGE}"
                )),
            )
            .into_fn(),
    );

    // `rpc`/`rpc_answer` prove the fleet contract first: an unmarked
    // `daemon_info` denies the read before the fleet answer is even consumed.
    let error = rpc(dir.path(), "agent_operational", json!({}))
        .expect_err("an unmarked fleet answer must not be served");
    assert_no_private(&error.to_string(), "rpc agent_operational");
    let error =
        rpc(dir.path(), "agent_list", json!({})).expect_err("an unmarked fleet list served");
    assert_no_private(&error.to_string(), "rpc agent_list");
    let error = rpc_answer(dir.path(), "agent_operational", json!({}))
        .expect_err("the fleet pre-gate must refuse before the answer transport");
    assert_no_private(&error.to_string(), "rpc_answer agent_operational pre-gate");
    // `rpc_answer` is the transport/answer split: a non-fleet sensitive read
    // reaches the frame protector and comes back as an INNER refusal.
    let error = rpc_answer(dir.path(), "thread_read", json!({}))
        .expect("the answer transport itself must answer")
        .expect_err("an unmarked thread read must be the inner refusal");
    assert_no_private(&error.to_string(), "rpc_answer thread_read");

    // The deadline, private, relay and session transports have no
    // `daemon_info` pre-gate: the per-frame protector must refuse them.
    let budget = Duration::from_secs(5);
    let error = rpc_deadline(dir.path(), "agent_operational", json!({}), budget)
        .expect_err("the deadline transport served an unmarked fleet answer");
    assert_no_private(&error.to_string(), "rpc_deadline agent_operational");
    let error = MetaBudget::fresh(budget)
        .read(dir.path(), "agent_operational", json!({}))
        .expect_err("the metadata deadline transport served an unmarked fleet answer");
    assert_no_private(&error.to_string(), "MetaBudget::read agent_operational");
    let error = rpc_private_timeout(dir.path(), "agent_operational", json!({}), budget)
        .expect_err("the private-socket transport served an unmarked fleet answer");
    assert_no_private(&error.to_string(), "rpc_private_timeout agent_operational");
    let error = rpc_relay_timeout(dir.path(), "agent_operational", json!({}), budget)
        .expect_err("the relay transport served an unmarked fleet answer");
    assert_no_private(&error.to_string(), "rpc_relay_timeout agent_operational");
    let mut session = RpcSession::connect(dir.path()).expect("session transport");
    let error = session
        .answer("agent_operational", json!({}))
        .expect("the session transport must answer")
        .expect_err("the session transport served an unmarked fleet answer");
    assert_no_private(&error.to_string(), "RpcSession::answer agent_operational");

    // A sensitive read that is not pre-gated still refuses per frame, and a
    // legacy ERROR answer is replaced rather than forwarded.
    let error = rpc(dir.path(), "thread_read", json!({}))
        .expect_err("an unmarked thread read must not be served");
    assert_no_private(&error.to_string(), "rpc thread_read");
    let error = rpc_deadline(dir.path(), "thread_read", json!({}), budget)
        .expect_err("the deadline transport served an unmarked thread read");
    assert_no_private(&error.to_string(), "rpc_deadline thread_read");
    let error = rpc(dir.path(), "agent_events", json!({}))
        .expect_err("a legacy error answer must be replaced, not forwarded");
    assert_no_private(&error.to_string(), "rpc agent_events");
}

#[test]
fn marked_daemon_info_cannot_authenticate_an_unmarked_fleet_answer() {
    let unmarked_fleet = |dir: &Path| {
        SyntheticDaemon::start(
            dir,
            Scripted::new()
                .on(
                    "daemon_info",
                    proto::ok(json!({"fleet_protocol": proto::CLIENT_PROTOCOL, "pid": 7})),
                )
                .on("agent_operational", proto::ok(private_fleet()))
                .on("agent_list", proto::ok(private_fleet()))
                .into_fn(),
        )
    };

    let dir = tempfile::tempdir().unwrap();
    let _daemon = unmarked_fleet(dir.path());
    for method in ["agent_operational", "agent_list"] {
        let error = rpc(dir.path(), method, json!({}))
            .expect_err("a marked daemon_info cannot authenticate an unmarked fleet answer");
        assert_no_private(&error.to_string(), method);
    }
    // The answer variant splits transport from daemon refusal: the pre-gate
    // now passes (marked daemon_info), so the fleet frame itself is the inner
    // refusal.
    let error = rpc_answer(dir.path(), "agent_operational", json!({}))
        .expect("the answer transport must answer")
        .expect_err("the swapped unmarked fleet answer must be the inner refusal");
    assert_no_private(&error.to_string(), "rpc_answer swapped fleet");

    // The same swap with an unmarked ERROR answer that echoes an alias.
    let dir = tempfile::tempdir().unwrap();
    let _daemon = SyntheticDaemon::start(
        dir.path(),
        Scripted::new()
            .on(
                "daemon_info",
                proto::ok(json!({"fleet_protocol": proto::CLIENT_PROTOCOL, "pid": 7})),
            )
            .on(
                "agent_operational",
                error_frame(&format!(
                    "agent '{PRIVATE_ALIAS}' is private: {PRIVATE_MESSAGE}"
                )),
            )
            .into_fn(),
    );
    let error = rpc(dir.path(), "agent_operational", json!({}))
        .expect_err("a swapped unmarked error answer must refuse");
    assert_no_private(&error.to_string(), "swapped agent_operational error");

    // The marker is the response ENVELOPE: the same value inside `result`
    // is not a proof and must be refused.
    let dir = tempfile::tempdir().unwrap();
    let _daemon = SyntheticDaemon::start(
        dir.path(),
        Scripted::new()
            .on(
                "daemon_info",
                proto::ok(json!({"fleet_protocol": proto::CLIENT_PROTOCOL, "pid": 7})),
            )
            .on(
                "agent_operational",
                proto::ok(json!({
                    "agents": private_fleet()["agents"],
                    "fleet_protocol": proto::CLIENT_PROTOCOL,
                })),
            )
            .into_fn(),
    );
    let error = rpc(dir.path(), "agent_operational", json!({}))
        .expect_err("a result-level marker is not an envelope marker");
    assert_no_private(&error.to_string(), "result-level marker");
}

#[test]
fn marked_same_response_keeps_the_ordinary_list_usable() {
    let dir = tempfile::tempdir().unwrap();
    let ordinary = json!({"agents": [{"alias": ORDINARY_ALIAS, "state": "idle"}]});
    let _daemon = SyntheticDaemon::start(
        dir.path(),
        Scripted::new()
            .on(
                "daemon_info",
                proto::ok(json!({"fleet_protocol": proto::CLIENT_PROTOCOL, "pid": 7})),
            )
            .on("agent_operational", marked(proto::ok(ordinary.clone())))
            .on("agent_list", marked(proto::ok(ordinary)))
            .into_fn(),
    );

    for method in ["agent_operational", "agent_list"] {
        let answer = rpc(dir.path(), method, json!({}))
            .unwrap_or_else(|e| panic!("a marked current answer was refused by {method}: {e}"));
        assert_eq!(
            answer["agents"][0]["alias"], ORDINARY_ALIAS,
            "{method} lost the ordinary row: {answer}"
        );
        assert_value_has_no_private(&answer, method);
    }
}

/// Run one legacy `health` answer (unmarked envelope unless `current`) and
/// return the client's result.
fn health_answer(result: Value, current: bool) -> crate::error::Result<Value> {
    let dir = tempfile::tempdir().unwrap();
    let envelope = if current {
        marked(proto::ok(result))
    } else {
        proto::ok(result)
    };
    let _daemon =
        SyntheticDaemon::start(dir.path(), Scripted::new().on("health", envelope).into_fn());
    rpc(dir.path(), "health", json!({}))
}

#[test]
fn legacy_health_keeps_only_safe_liveness() {
    // A well-formed legacy health: ready + positive pid + private waiter.
    let value = health_answer(
        json!({
            "state": "ready",
            "pid": 4242,
            "agent_uid": 1001,
            "protocol": proto::PROTOCOL_VERSION,
            "waiting": [{
                "alias": PRIVATE_ALIAS,
                "message": PRIVATE_MESSAGE,
                "body": PRIVATE_BODY,
            }],
        }),
        false,
    )
    .expect("a well-formed legacy health must still prove liveness");
    assert_eq!(value["fleet_status_unavailable"], true, "{value}");
    assert_eq!(value["state"], "ready", "{value}");
    assert_eq!(value["pid"].as_u64(), Some(4242), "{value}");
    assert_eq!(value["agent_uid"].as_u64(), Some(1001), "{value}");
    assert_eq!(
        value["protocol"].as_u64(),
        Some(u64::from(proto::PROTOCOL_VERSION)),
        "{value}"
    );
    assert!(value.get("waiting").is_none(), "{value}");
    assert_value_has_no_private(&value, "legacy health");

    // Reviewer follow-up: a malformed legacy health must be a FAILURE, never
    // a successful sanitized liveness answer that reads as "already running".
    let unusable = [
        ("null result", Value::Null),
        ("non-object result", json!("ready")),
        ("absent pid", json!({"state": "ready"})),
        (
            "zero pid",
            json!({"state": "ready", "pid": 0, "waiting": [{"alias": PRIVATE_ALIAS}]}),
        ),
        ("negative pid", json!({"state": "ready", "pid": -1})),
        ("string pid", json!({"state": "ready", "pid": "4242"})),
    ];
    for (label, result) in unusable {
        let error = health_answer(result, false)
            .expect_err("a legacy health with no positive numeric pid must not prove liveness");
        assert_no_private(&error.to_string(), label);
        assert!(
            !error
                .to_string()
                .to_ascii_lowercase()
                .contains("already running"),
            "{label} manufactured a running daemon: {error}"
        );
    }

    // The same malformed case through the deadline transport refuses too.
    let dir = tempfile::tempdir().unwrap();
    let _daemon = SyntheticDaemon::start(
        dir.path(),
        Scripted::new()
            .on("health", proto::ok(Value::Null))
            .into_fn(),
    );
    let error = rpc_deadline(dir.path(), "health", json!({}), Duration::from_secs(5))
        .expect_err("the deadline transport accepted a null legacy health");
    assert_no_private(&error.to_string(), "rpc_deadline health");

    // A result-level marker is not an envelope marker: this stays legacy and
    // is sanitized while keeping the safe pid.
    let value = health_answer(
        json!({
            "state": "ready",
            "pid": 4242,
            "fleet_protocol": proto::CLIENT_PROTOCOL,
            "waiting": [{"alias": PRIVATE_ALIAS}],
        }),
        false,
    )
    .expect("a result-level marker does not make health current");
    assert_eq!(value["fleet_status_unavailable"], true, "{value}");
    assert_eq!(value["pid"].as_u64(), Some(4242), "{value}");
    assert!(value.get("waiting").is_none(), "{value}");
    assert_value_has_no_private(&value, "result-level health");

    // A current daemon marks its own answer on the envelope; the client
    // trusts that answer (the daemon sanitizes it server-side).
    let value = health_answer(
        json!({"state": "ready", "pid": 4242, "agent_uid": 1001}),
        true,
    )
    .expect("a marked current health must be accepted");
    assert_eq!(value["pid"].as_u64(), Some(4242), "{value}");
}

/// A scripted daemon that also records which METHODS arrived on which
/// ACCEPTED CONNECTION, so a test can prove a preflight and its mutation
/// shared one socket and that a failure never reconnects or replays.
struct RecordingDaemon {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    log: MethodLog,
    accepts: Arc<AtomicUsize>,
}

impl RecordingDaemon {
    /// `script(connection, method, params)` answers `Some(envelope)`, or
    /// returns `None` to close that connection without a reply.
    fn start(
        state: &Path,
        script: impl Fn(usize, &str, &Value) -> Option<Value> + Send + Sync + 'static,
    ) -> Self {
        let socket = socket_path(state);
        if socket.exists() {
            std::fs::remove_file(&socket).expect("remove stale fixture socket");
        }
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent).expect("fixture socket dir");
        }
        let listener = UnixListener::bind(&socket).expect("bind fixture socket");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let script: RecordingScript = Arc::new(script);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let log = Arc::new(Mutex::new(Vec::new()));
        let accepts = Arc::new(AtomicUsize::new(0));
        let thread_log = Arc::clone(&log);
        let thread_accepts = Arc::clone(&accepts);
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let (stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                let connection = thread_accepts.fetch_add(1, Ordering::SeqCst);
                let script = Arc::clone(&script);
                let log = Arc::clone(&thread_log);
                // One handler per connection: a reconnect must be accepted
                // (and recorded) instead of hiding behind the first client.
                std::thread::spawn(move || serve_connection(stream, connection, script, log));
            }
        });
        Self {
            stop,
            thread: Some(thread),
            log,
            accepts,
        }
    }

    fn methods(&self) -> Vec<(usize, String)> {
        self.log.lock().unwrap().clone()
    }

    fn accepts(&self) -> usize {
        self.accepts.load(Ordering::SeqCst)
    }
}

impl Drop for RecordingDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve_connection(
    mut stream: std::os::unix::net::UnixStream,
    connection: usize,
    script: RecordingScript,
    log: MethodLog,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let reader = match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    };
    let mut reader = BufReader::new(reader);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return,
            Ok(_) => {}
            Err(_) => return,
        }
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(_) => return,
        };
        let method = request["method"].as_str().unwrap_or_default().to_string();
        let params = request["params"].clone();
        log.lock().unwrap().push((connection, method.clone()));
        match script(connection, &method, &params) {
            Some(envelope) => {
                if writeln!(stream, "{envelope}")
                    .and_then(|()| stream.flush())
                    .is_err()
                {
                    return;
                }
            }
            None => return,
        }
    }
}

/// CAD-2207: the preflight and the mutation must share ONE connection. An
/// unmarked (or forged result-marker) `daemon_info` must gate the mutation
/// completely; a marked daemon must receive exactly one `agent_resume` on the
/// same accepted connection and its real result must come back; and a
/// disconnect after a marked preflight must fail WITHOUT reconnecting or
/// replaying the mutation.
#[test]
fn same_connection_fleet_preflight_gates_the_resume() {
    // ---- unmarked / forged daemon_info: no mutation may reach the daemon --
    for info in [
        proto::ok(json!({"build_commit": "legacy", "pid": 7})),
        // The marker inside `result` is a forged field, not an envelope mark.
        proto::ok(json!({"fleet_protocol": proto::CLIENT_PROTOCOL, "pid": 7})),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let daemon =
            RecordingDaemon::start(dir.path(), move |_conn, method, _params| match method {
                "daemon_info" => Some(info.clone()),
                "agent_resume" => Some(error_frame("a mutation must never arrive")),
                _ => Some(error_frame("unscripted")),
            });
        let error =
            rpc_after_fleet_preflight(dir.path(), "agent_resume", json!({"alias": ORDINARY_ALIAS}))
                .expect_err("an unmarked daemon_info must gate the mutation");
        assert!(!error.to_string().contains(PRIVATE_ALIAS), "{error}");
        // Bound the settle: a wrong client that fire-and-forgets the
        // mutation gets this long for the daemon to log it.
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            daemon.methods(),
            vec![(0, "daemon_info".to_string())],
            "an unmarked preflight let the mutation reach the daemon"
        );
        assert_eq!(
            daemon.accepts(),
            1,
            "an unmarked preflight opened another connection"
        );
    }

    // ---- marked daemon: exactly one mutation, on the preflight connection --
    let dir = tempfile::tempdir().unwrap();
    let daemon = RecordingDaemon::start(dir.path(), |_conn, method, _params| match method {
        "daemon_info" => Some(marked(proto::ok(
            json!({"build_commit": "current", "pid": 7}),
        ))),
        "agent_resume" => Some(marked(proto::ok(json!({
            "alias": ORDINARY_ALIAS,
            "state": "resumed",
        })))),
        _ => Some(error_frame("unscripted")),
    });
    let resumed =
        rpc_after_fleet_preflight(dir.path(), "agent_resume", json!({"alias": ORDINARY_ALIAS}))
            .expect("a marked daemon's preflight must pass and answer the mutation");
    assert_eq!(resumed["alias"], ORDINARY_ALIAS, "{resumed}");
    assert_eq!(resumed["state"], "resumed", "{resumed}");
    assert_eq!(
        daemon.methods(),
        vec![
            (0, "daemon_info".to_string()),
            (0, "agent_resume".to_string())
        ],
        "the mutation must be exactly one, on the preflight's own connection"
    );
    assert_eq!(daemon.accepts(), 1);

    // ---- disconnect after a marked preflight: fail, never replay ----------
    let dir = tempfile::tempdir().unwrap();
    let daemon = RecordingDaemon::start(dir.path(), |_conn, method, _params| match method {
        "daemon_info" => Some(marked(proto::ok(
            json!({"build_commit": "current", "pid": 7}),
        ))),
        // Logged, then the connection dies unanswered.
        "agent_resume" => None,
        _ => Some(error_frame("unscripted")),
    });
    let error =
        rpc_after_fleet_preflight(dir.path(), "agent_resume", json!({"alias": ORDINARY_ALIAS}))
            .expect_err("a disconnect after the preflight must fail the mutation");
    assert!(!error.to_string().contains(PRIVATE_ALIAS), "{error}");
    assert_eq!(
        daemon.methods(),
        vec![
            (0, "daemon_info".to_string()),
            (0, "agent_resume".to_string())
        ],
        "the mutation must be sent once and never replayed"
    );
    assert_eq!(
        daemon.accepts(),
        1,
        "the client reconnected and replayed the mutation"
    );
}
