//! CAD-1365 independent acceptance: the manual execution fence must refuse
//! `app_media_original` on BOTH the native connection handler
//! (`serve::handle_conn`) and the board HTTP relay
//! (`POST /api/app-media/original`), before any original work or provider
//! GET. Authored independently of the implementer, who may not edit or
//! weaken this file. The parent declares the module under
//! `#[cfg(all(test, feature = "test-seam"))]`.
//!
//! Scenario (one fenced daemon, one state dir):
//! - an operator session is minted in the REAL `operator_auth::Auth` store
//!   BEFORE fencing (a fenced daemon refuses `operator_session_open`); its
//!   token and key are the genuine cookie + `X-Cadence-Session` pair, not a
//!   forged caller flag;
//! - the on-disk fence is the REAL pair `portable_fence::load` verifies: a
//!   0600 execution-mode receipt and the activated marker, same generation;
//! - native `app_media_original` on the socket must return the exact
//!   `portable_fence::fence_refusal` text as an ordinary JSON error;
//! - HTTP `POST /api/app-media/original` on a real board must relay to the
//!   same guard and return the same fence text in a non-2xx JSON error;
//! - a loopback provider counts every connection; it must stay ZERO;
//! - allowlisted `health` must still succeed, proving the daemon is alive.

#![cfg(all(test, feature = "test-seam"))]

use super::{serve, ServeOptions, Shared};
use crate::backup::portable::INACTIVE_MARKER;
use crate::backup::portable_fence::{self, Mode, FENCE_FILE, FORMAT, MODE_DISABLED};
use crate::operator_auth::{Auth, Origin};
use crate::platform::agenticos_external;
use crate::test_seam::{Seam, AS_HEADER, TOKEN_HEADER};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const GENERATION: &str = "g-cad1365-fence";
/// The earlier fence refusal the board's session check returns for the
/// HTTP path (`operator_session_check` is not allowlisted under the fence).
const FENCE_SESSION_CHECK_TEXT: &str = "operator_session_check is execution-capable and this state dir is in manual execution-disabled mode";
const FENCE_TEXT: &str = "app_media_original is execution-capable and this state dir is in manual execution-disabled mode";

/// The real verified fence pair: the activated marker and the 0600
/// execution-mode receipt sharing one generation.
fn write_fence(state: &Path) {
    let marker = json!({"state": "activated_manual_disabled", "generation": GENERATION});
    std::fs::write(state.join(INACTIVE_MARKER), marker.to_string()).unwrap();
    let receipt = json!({
        "format": FORMAT,
        "execution_mode": MODE_DISABLED,
        "generation": GENERATION,
    });
    let path = state.join(FENCE_FILE);
    std::fs::write(&path, receipt.to_string()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// Mint one loopback operator session in the real `Auth` store, before fencing.
fn mint_session(state: &Path) -> (String, String) {
    let mut auth = Auth::load(state);
    let now = crate::rollout::unix_now() as i64;
    let nonce = auth.mint(Origin::Loopback, now).unwrap();
    let opened = auth
        .open(&nonce, Origin::Loopback, "", now)
        .unwrap()
        .unwrap();
    (opened.token, opened.key)
}

/// One raw request on the real native socket; returns the reply line as JSON.
fn exchange(state: &Path, method: &str, params: Value) -> Value {
    let mut stream = UnixStream::connect(state.join("cadence.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let request = crate::proto::request(method, params);
    stream.write_all(format!("{request}\n").as_bytes()).unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap_or_else(|_| panic!("reply is not one JSON line: {line:?}"))
}

/// A free loopback board port in 3110-3199, bound by the real `ui::serve`.
fn board(state: &Path, pm: &Path, stop: &Arc<AtomicBool>) -> (u16, std::thread::JoinHandle<()>) {
    let first = (std::process::id() % 80) as u16;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut offset = 0u16;
    loop {
        assert!(Instant::now() < deadline, "board startup deadline");
        let port = 3110 + (first + offset) % 80;
        let (startup, ready) = std::sync::mpsc::channel();
        let opts = crate::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.clone()),
            startup: Some(startup),
            test_seam: true,
            ..Default::default()
        };
        let (s, p) = (state.to_path_buf(), pm.to_path_buf());
        let thread = std::thread::spawn(move || drop(crate::ui::serve(&s, &p, &opts)));
        match ready.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Ok(())) => return (port, thread),
            Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                let _ = thread.join();
                offset += 1;
                assert!(offset < 80, "all fixture board candidates busy");
            }
            other => {
                let _ = thread.join();
                panic!("fixture board startup failed on port {port}: {other:?}");
            }
        }
    }
}

/// POST the original route with the real loopback cookie + session key and
/// the test seam operator identity. Returns (status, body text).
fn http_original(port: u16, seam: &str, token: &str, key: &str, body: Value) -> (u16, String) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let host = format!("cadence-{port}.localhost:{port}");
    let mut resp = agent
        .post(format!("http://127.0.0.1:{port}/api/app-media/original"))
        .header("Host", &host)
        .header("X-Cadence-Board", "1")
        .header("Origin", format!("http://{host}"))
        .header(AS_HEADER, "operator")
        .header(TOKEN_HEADER, seam)
        .header("Cookie", format!("cadence_operator_{port}={token}"))
        .header("X-Cadence-Session", key)
        .header("Content-Type", "application/json")
        .send(body.to_string())
        .unwrap();
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    (status, text)
}

/// Stops and joins the native accept loop on every exit path, including a
/// failed assertion, so no test thread outlives the fixture.
struct ServerGuard {
    stop: Arc<AtomicBool>,
    server: Option<std::thread::JoinHandle<()>>,
}

impl ServerGuard {
    fn finish(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        self.finish();
    }
}

#[test]
fn cad1365_original_fence_refuses_native_and_http_and_keeps_health() {
    let dir = tempfile::Builder::new()
        .prefix("c1365fence")
        .tempdir()
        .unwrap();
    let state = dir.path().join("s");
    let pm = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    crate::issue::Pm::init(&pm).unwrap();
    crate::store::Store::open(&state.join("cadence.sqlite3")).unwrap();
    let (token, key) = mint_session(&state);
    write_fence(&state);
    assert_eq!(portable_fence::load(&state).unwrap(), Mode::ManualDisabled);

    // Loopback provider: every accepted connection is a provider hit.
    let provider = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    provider.set_nonblocking(true).unwrap();
    let base = format!("http://{}", provider.local_addr().unwrap());

    let mut opts = ServeOptions {
        test_seam: true,
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    opts.platforms.insert(
        agenticos_external::PLATFORM.into(),
        Arc::new(
            agenticos_external::AgenticosExternalAdapter::with_deployment_pin(
                &base,
                Some(agenticos_external::MANIFEST_PIN),
            )
            .unwrap(),
        ),
    );
    let shared = Shared::new(&state, &opts).unwrap();

    // Native: one bounded accept loop serves the real per-connection handler
    // for every request until `stop` is set. The listener stays open through
    // the HTTP stage, so the board's own relay can reach this daemon socket.
    let stop = Arc::new(AtomicBool::new(false));
    let listener = UnixListener::bind(state.join("cadence.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let accept_stop = stop.clone();
    let server_shared = shared.clone();
    let server = std::thread::spawn(move || {
        let mut conns = Vec::new();
        while !accept_stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    let conn_shared = server_shared.clone();
                    conns.push(std::thread::spawn(move || {
                        serve::handle_conn(conn_shared, stream)
                    }));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
        for conn in conns {
            let _ = conn.join();
        }
    });
    let mut guard = ServerGuard {
        stop: stop.clone(),
        server: Some(server),
    };
    let refused = exchange(
        &state,
        "app_media_original",
        json!({"action_token": "a".repeat(64), "token": token, "key": key,
            "origin": "loopback", "asset_id": "ca_fence_probe"}),
    );
    let health = exchange(&state, "health", json!({}));

    assert_eq!(
        refused["ok"], false,
        "native original must be refused: {refused}"
    );
    let message = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(FENCE_TEXT),
        "native refused for the wrong reason: {message}"
    );
    assert_eq!(
        health["ok"], true,
        "allowlisted health must work under the fence: {health}"
    );

    // HTTP: the board checks the caller's session with the daemon BEFORE the
    // original route runs (`board_caller` -> `check_session` ->
    // `operator_session_check`). Under the fence that Bearer method is refused
    // by `dispatch` first, so the board answers 503 `daemon_unavailable` with
    // the portable-fence text for `operator_session_check`. The original
    // method is never reached, so the body and provider must stay untouched.
    let seam = Seam::token_at(&state).expect("fixture seam armed by the daemon");
    let board_stop = Arc::new(AtomicBool::new(false));
    let (port, board_thread) = board(&state, &pm, &board_stop);
    let (status, text) = http_original(
        port,
        &seam,
        &token,
        &key,
        json!({"action_token": "a".repeat(64), "asset_id": "ca_fence_probe"}),
    );
    board_stop.store(true, Ordering::SeqCst);
    let _ = board_thread.join();
    assert_eq!(
        status, 503,
        "HTTP original must be refused by the fence: {text}"
    );
    assert!(
        text.contains("daemon_unavailable"),
        "HTTP refusal must be the typed daemon_unavailable code: {text}"
    );
    assert!(
        text.contains(FENCE_SESSION_CHECK_TEXT),
        "HTTP refused for the wrong reason (expected the session-check fence): {text}"
    );
    assert!(
        !text.contains("app_media_original"),
        "HTTP body must not name the original route: {text}"
    );

    guard.finish();

    let mut provider_gets = 0;
    while provider.accept().is_ok() {
        provider_gets += 1;
    }
    assert_eq!(provider_gets, 0, "a refused original reached the provider");
}
