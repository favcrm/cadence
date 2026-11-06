//! CAD-2207 mixed-version update display acceptance.
//!
//! The real `updates::get` / `updates::banner_get` consumers talk over the
//! real Unix-socket RPC client to a synthetic daemon. The old daemon answers
//! `update_status` with a personal alias/message id and omits the
//! `fleet_protocol` capability from `daemon_info`; the board must neither
//! render that identity nor present an empty/idle update state. The
//! current-compatible daemon includes the protocol capability and a generic
//! waiter row, and the same consumers must keep showing the pending update.

use std::io::{BufRead, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

use super::{banner_get, get, HttpResp};

const OLD_ALIAS: &str = "assist-agent-legacy-private";
const OLD_MESSAGE: &str = "legacy-private-message";
const OLD_BODY: &str = "legacy private turn body";

struct SyntheticDaemon {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SyntheticDaemon {
    fn start(state: &Path, current: bool) -> Self {
        Self::start_with(state, current, current)
    }

    /// `daemon_protocol` and `status_protocol` are independent so a test can
    /// model a daemon replacement: safe `daemon_info` first, then an old
    /// unmarked `update_status` answer.
    fn start_with(state: &Path, daemon_protocol: bool, status_protocol: bool) -> Self {
        Self::start_with_error(state, daemon_protocol, status_protocol, None)
    }

    fn start_with_error(
        state: &Path,
        daemon_protocol: bool,
        status_protocol: bool,
        status_error: Option<&str>,
    ) -> Self {
        let status_error = status_error.map(str::to_string);
        let socket = crate::client::socket_path(state);
        if socket.exists() {
            std::fs::remove_file(&socket).unwrap();
        }
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }
                    Err(_) => break,
                };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(5)));
                let mut line = String::new();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                    continue;
                }
                let request: Value = match serde_json::from_str(&line) {
                    Ok(request) => request,
                    Err(_) => continue,
                };
                let method = request["method"].as_str().unwrap_or_default();
                let result = match method {
                    "daemon_info" => {
                        let mut info = json!({
                            "build_commit": "legacy-build",
                            "build_time": "legacy",
                            "started_at": 1.0,
                            "pid": std::process::id(),
                        });
                        if daemon_protocol {
                            info["fleet_protocol"] = json!(crate::proto::CLIENT_PROTOCOL);
                        }
                        info
                    }
                    "update_status" => update_status(status_protocol),
                    _ => json!({}),
                };
                let error_frame = if method == "update_status" {
                    status_error.as_deref()
                } else {
                    None
                };
                let marked = error_frame.is_none()
                    && ((method == "update_status" && status_protocol)
                        || (method == "daemon_info" && daemon_protocol));
                let mut envelope = match error_frame {
                    Some(message) => json!({
                        "ok": false,
                        "error": {"kind": "rejected", "message": message},
                    }),
                    None => json!({"ok": true, "result": result}),
                };
                if marked {
                    envelope["fleet_protocol"] = json!(crate::proto::CLIENT_PROTOCOL);
                }
                let _ = writeln!(stream, "{envelope}");
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

fn update_status(current: bool) -> Value {
    let waiting = if current {
        json!([{
            "alias": "unattended work",
            "message": "pending",
            "state": "running",
            "age_secs": 0
        }])
    } else {
        json!([{
            "alias": OLD_ALIAS,
            "message": OLD_MESSAGE,
            "body": OLD_BODY,
            "state": "running",
            "age_secs": 3
        }])
    };
    let status = json!({
        "pending_update": {
            "phase": "draining",
            "target": "v2",
            "from": "v1",
            "by": "operator",
            "since": 1.0
        },
        "waiting": waiting,
        "waiting_count": 1
    });
    // The capability marker is on the response envelope, not this result:
    // the atomic update_status check must prove the same answer it is about to
    // consume.
    status
}

fn response_parts(response: HttpResp) -> (u16, Value) {
    let status = response.status_code().0;
    let mut raw = Vec::new();
    response.into_reader().read_to_end(&mut raw).unwrap();
    let body = serde_json::from_slice(&raw)
        .unwrap_or_else(|_| json!({"raw": String::from_utf8_lossy(&raw)}));
    (status, body)
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(rows) => !rows.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
        Value::Null => false,
    }
}

/// Any truthful unavailability signal. A 200 with an empty waiting list and
/// no signal is the false-idle answer this acceptance forbids.
fn indicates_unavailable(status: u16, body: &Value) -> bool {
    if status != 200 {
        return true;
    }
    fn scan(value: &Value) -> bool {
        match value {
            Value::Object(fields) => fields.iter().any(|(key, value)| {
                let key = key.to_ascii_lowercase();
                let named = key.contains("unavailable")
                    || key.contains("error")
                    || key.contains("unsupported")
                    || key.contains("upgrade")
                    || (key.contains("protocol") && key != "fleet_protocol");
                (named && truthy(value)) || scan(value)
            }),
            Value::Array(rows) => rows.iter().any(scan),
            _ => false,
        }
    }
    scan(body)
}

fn assert_no_old_identity(body: &Value, what: &str) {
    let text = body.to_string();
    for secret in [OLD_ALIAS, OLD_MESSAGE, OLD_BODY] {
        assert!(
            !text.contains(secret),
            "{what} exposed the old daemon's {secret}: {body}"
        );
    }
}

#[test]
fn old_daemon_waiter_is_unavailable_and_never_rendered() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = SyntheticDaemon::start(dir.path(), false);

    let (status, card) = response_parts(get(dir.path()));
    assert_no_old_identity(&card, "/api/update");
    assert!(
        indicates_unavailable(status, &card),
        "/api/update presented an old daemon's waiter as available/idle: {status} {card}"
    );

    let (status, banner) = response_parts(banner_get(dir.path()));
    assert_no_old_identity(&banner, "/api/update/banner");
    assert!(
        indicates_unavailable(status, &banner),
        "/api/update/banner presented an old daemon's waiter as available/idle: {status} {banner}"
    );
}

#[test]
fn compatible_daemon_still_shows_pending_update_and_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = SyntheticDaemon::start(dir.path(), true);

    let (status, card) = response_parts(get(dir.path()));
    assert_eq!(status, 200, "compatible /api/update failed: {card}");
    assert!(
        !indicates_unavailable(status, &card),
        "compatible /api/update claimed unavailability: {card}"
    );
    assert!(
        card["waiting"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()),
        "compatible /api/update lost the current waiter: {card}"
    );
    assert_no_old_identity(&card, "/api/update");

    let (status, banner) = response_parts(banner_get(dir.path()));
    assert_eq!(status, 200, "compatible banner failed: {banner}");
    assert!(
        !indicates_unavailable(status, &banner),
        "compatible banner claimed unavailability: {banner}"
    );
    assert!(
        banner["pending"].is_object(),
        "compatible banner lost its pending update: {banner}"
    );
    assert_no_old_identity(&banner, "/api/update/banner");
}

/// A safe daemon_info answer followed by an old unmarked update_status (a
/// daemon replacement between calls) must not bless the old payload.
#[test]
fn safe_daemon_info_does_not_bless_an_unmarked_update_status() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = SyntheticDaemon::start_with(dir.path(), true, false);

    let (status, card) = response_parts(get(dir.path()));
    assert_no_old_identity(&card, "/api/update");
    assert!(
        indicates_unavailable(status, &card),
        "/api/update trusted daemon_info over an unmarked update_status: {status} {card}"
    );

    let (status, banner) = response_parts(banner_get(dir.path()));
    assert_no_old_identity(&banner, "/api/update/banner");
    assert!(
        indicates_unavailable(status, &banner),
        "/api/update/banner trusted daemon_info over an unmarked update_status: {status} {banner}"
    );
}

/// A legacy daemon can answer update_status with an ERROR frame whose message
/// carries private identity. The board must not echo it or treat the error as
/// a clean no-update state.
#[test]
fn legacy_error_frame_identity_is_not_echoed_and_remains_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let message = format!("legacy update_status failed for {OLD_ALIAS}/{OLD_MESSAGE}: {OLD_BODY}");
    let _daemon = SyntheticDaemon::start_with_error(dir.path(), true, false, Some(&message));

    let (status, card) = response_parts(get(dir.path()));
    assert_no_old_identity(&card, "/api/update");
    assert!(
        indicates_unavailable(status, &card),
        "/api/update turned a legacy error into a clean no-update state: {status} {card}"
    );

    let (status, banner) = response_parts(banner_get(dir.path()));
    assert_no_old_identity(&banner, "/api/update/banner");
    assert!(
        indicates_unavailable(status, &banner),
        "/api/update/banner turned a legacy error into a clean no-update state: {status} {banner}"
    );
}
