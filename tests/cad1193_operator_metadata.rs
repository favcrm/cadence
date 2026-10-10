//! Independent CAD-1193 negative acceptance: courtesy metadata is bounded,
//! but transport failure is neither operator authority nor proof of sign-out.
//! No caller seam, cached role, invented RPC response, external login or provider.
#![cfg(target_os = "linux")]

use cadence_agent::{adapter, client, issue, operator_auth, reaper};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");
const HEALTHY: u8 = 0;
const MISSING_IDENTITY: u8 = 1;
const STALLED_IDENTITY: u8 = 2;
const STALLED_SESSION: u8 = 3;
// Ticket target: five seconds, plus one second scheduling tolerance. The
// HTTP timeout is a watchdog, NOT the implementation's timeout mechanism.
const METADATA_LIMIT: Duration = Duration::from_secs(6);

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

/// Transport-only fault injection. Every non-faulted frame goes unchanged
/// to the REAL daemon, and its reply is copied verbatim. A fault either
/// closes the socket or holds it open without answering; it never supplies
/// a session, PID, agent list, success, or denial on behalf of production.
struct FaultWire {
    mode: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
    observed: Arc<Mutex<Vec<String>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl FaultWire {
    fn install(state: &Path) -> Self {
        let socket = client::socket_path(state);
        let backend = state.join("actual.sock");
        std::fs::rename(&socket, &backend).unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mode = Arc::new(AtomicU8::new(HEALTHY));
        let stop = Arc::new(AtomicBool::new(false));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let (m, s, o) = (mode.clone(), stop.clone(), observed.clone());
        let thread = std::thread::spawn(move || {
            let mut workers = Vec::new();
            while !s.load(SeqCst) {
                match listener.accept() {
                    Ok((mut peer, _)) => {
                        let (backend, m, s, o) = (backend.clone(), m.clone(), s.clone(), o.clone());
                        workers.push(std::thread::spawn(move || {
                            peer.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
                            peer.set_write_timeout(Some(Duration::from_secs(8)))
                                .unwrap();
                            let mut line = String::new();
                            if BufReader::new(&peer).read_line(&mut line).is_err() {
                                return;
                            }
                            let frame: Value = serde_json::from_str(&line).unwrap();
                            let method = frame["method"].as_str().unwrap();
                            // Observe method names only: never record the synthetic credentials.
                            o.lock().unwrap().push(method.to_string());
                            let fault = m.load(SeqCst);
                            if method == "agent_list" && fault == MISSING_IDENTITY {
                                return;
                            }
                            if (method == "agent_list" && fault == STALLED_IDENTITY)
                                || (method == "operator_session_check" && fault == STALLED_SESSION)
                            {
                                while !s.load(SeqCst) && m.load(SeqCst) == fault {
                                    std::thread::sleep(Duration::from_millis(10));
                                }
                                return;
                            }
                            let Ok(mut actual) = UnixStream::connect(backend) else {
                                return;
                            };
                            actual
                                .set_read_timeout(Some(Duration::from_secs(8)))
                                .unwrap();
                            actual
                                .set_write_timeout(Some(Duration::from_secs(8)))
                                .unwrap();
                            if actual.write_all(line.as_bytes()).is_err() {
                                return;
                            }
                            let mut reply = String::new();
                            if BufReader::new(actual).read_line(&mut reply).is_ok() {
                                let _ = peer.write_all(reply.as_bytes());
                            }
                        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("owned fault socket: {e}"),
                }
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self {
            mode,
            stop,
            observed,
            thread: Some(thread),
        }
    }
    fn count(&self, method: &str) -> usize {
        self.observed
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.as_str() == method)
            .count()
    }
}
impl Drop for FaultWire {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        self.thread.take().unwrap().join().unwrap();
    }
}

struct Reply {
    status: u16,
    body: Value,
    set_cookie: bool,
    elapsed: Duration,
}

fn request(port: u16, route: &str, credentials: Option<(&str, &str)>, post: bool) -> Reply {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(7)))
        .build()
        .into();
    let host = format!("cadence-{port}.localhost:{port}");
    let url = format!("http://127.0.0.1:{port}{route}");
    let start = Instant::now();
    let mut response = if post {
        let mut req = agent
            .post(&url)
            .header("Host", &host)
            .header("Origin", format!("http://{host}"))
            .header("X-Cadence-Board", "1")
            .header("Content-Type", "application/json");
        if let Some((cookie, key)) = credentials {
            req = req
                .header("Cookie", cookie)
                .header("X-Cadence-Session", key);
        }
        req.send("{}")
            .expect("real HTTP write must refuse, not hang")
    } else {
        let mut req = agent.get(&url).header("Host", &host);
        if let Some((cookie, key)) = credentials {
            req = req
                .header("Cookie", cookie)
                .header("X-Cadence-Session", key);
        }
        req.call()
            .expect("real HTTP read must answer within metadata watchdog")
    };
    let status = response.status().as_u16();
    let set_cookie = response.headers().contains_key("set-cookie");
    let body = serde_json::from_str(&response.body_mut().read_to_string().unwrap()).unwrap();
    Reply {
        status,
        body,
        set_cookie,
        elapsed: start.elapsed(),
    }
}

fn fixture_command(root: &Path, state: &Path, pm: &Path) -> Command {
    // This is the existing safety-floor pattern: only owned subprocesses
    // get a fresh fixture environment. Nothing changes this caller's env.
    let mut cmd = Command::new(BINARY);
    cmd.env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", root.join("bin").display()),
        )
        .env("HOME", root.join("home"))
        .env("XDG_STATE_HOME", root.join("xdg"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("TMPDIR", root.join("tmp"))
        .env("CADENCE_PM_DIR", pm)
        .env("CADENCE_SUITE_LOCK", root.join("suite.lock"))
        .args(["--state-dir", state.to_str().unwrap()])
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for name in adapter::PROVIDER_COMMAND_VARS {
        cmd.env(name, adapter::REFUSED_COMMAND);
    }
    for name in ["CARGO_HOME", "RUSTUP_HOME"] {
        if let Some(value) = std::env::var_os(name) {
            cmd.env(name, value);
        }
    }
    cmd
}

fn assert_unknown_meta(reply: &Reply) {
    assert_eq!(
        reply.status, 200,
        "metadata must describe unavailable access"
    );
    assert!(
        reply.elapsed <= METADATA_LIMIT,
        "metadata exceeded ticket budget: {:?}",
        reply.elapsed
    );
    assert!(
        reply.body.get("operator").is_some_and(Value::is_null),
        "unavailable proof is unknown, never true or a resolved negative: {}",
        reply.body
    );
    assert_ne!(
        reply.body["tab_signed_out"], true,
        "dependency failure is not sign-out"
    );
    assert!(
        !reply.set_cookie,
        "metadata failure must not clear a valid cookie"
    );
}

#[test]
fn cad1193_native_http_missing_identity_never_grants_and_timeout_is_not_signout() {
    let artifacts = Path::new("/tmp/e8qa/cad1193-acceptance");
    std::fs::create_dir_all(artifacts).unwrap();
    let root = tempfile::Builder::new()
        .prefix("host-")
        .tempdir_in(artifacts)
        .unwrap();
    for dir in [
        "state", "pm", "home", "xdg", "config", "data", "tmp", "bin", "dist",
    ] {
        std::fs::create_dir(root.path().join(dir)).unwrap();
    }
    // Prevent the board's background delivery sync from calling real gh.
    std::os::unix::fs::symlink("/bin/false", root.path().join("bin/gh")).unwrap();
    let state = root.path().join("state");
    let pm = root.path().join("pm");
    issue::Pm::init(&pm).unwrap();
    // Native auth component creates ONLY an isolated synthetic session;
    // the actual daemon loads it and validates its exact cookie/key pair.
    // This is fixture data, not a seam assertion or a bypass of any HTTP guard.
    let now = issue::time::now_epoch();
    let mut auth = operator_auth::Auth::load(&state);
    let nonce = auth.mint(operator_auth::Origin::Loopback, now).unwrap();
    let session = auth
        .open(
            &nonce,
            operator_auth::Origin::Loopback,
            "CAD1193 isolated acceptance",
            now,
        )
        .unwrap()
        .unwrap();
    let mut daemon = OwnedChild(
        reaper::spawn(fixture_command(root.path(), &state, &pm).args(["daemon", "run"])).unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(25);
    // Direct private socket avoids any inherited CADENCE_SOCKET override.
    while client::rpc_private_timeout(&state, "daemon_info", json!({}), Duration::from_millis(200))
        .is_err()
    {
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "owned daemon exited at startup"
        );
        assert!(
            Instant::now() < deadline,
            "owned daemon did not become ready"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(
        state.join("cadence.sqlite3").exists(),
        "identity proof must consult real agent store"
    );
    let port = (3110..=3199)
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .unwrap();
    let mut board = OwnedChild(
        reaper::spawn(
            fixture_command(root.path(), &state, &pm)
                .args([
                    "ui",
                    "run",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &port.to_string(),
                    "--dist",
                ])
                .arg(root.path().join("dist")),
        )
        .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(25);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            board.0.try_wait().unwrap().is_none(),
            "owned board exited at startup"
        );
        assert!(
            Instant::now() < deadline,
            "owned board did not bind isolated port"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    let cookie = format!("cadence_operator_{port}={}", session.token);
    let credentials = Some((cookie.as_str(), session.key.as_str()));
    let wire = FaultWire::install(&state);
    let control = request(port, "/api/meta", credentials, false);
    assert_eq!(control.status, 200);
    assert_eq!(
        control.body["signed_in"], true,
        "REAL session validation positive control"
    );
    let wrong_key = operator_auth::random_credential().unwrap();
    let invalid = request(port, "/api/meta", Some((&cookie, &wrong_key)), false);
    assert_eq!(
        invalid.body["signed_in"], false,
        "REAL session validation rejects wrong tab key"
    );

    // First prove the REAL guard refuses a missing required identity read,
    // on both its protected HTTP read and a mutation, before any handler RPC.
    wire.mode.store(MISSING_IDENTITY, SeqCst);
    let before = wire.count("agent_list");
    for (route, post) in [
        ("/api/master/permissions", false),
        ("/api/plans/CAD-1193/approve", true),
    ] {
        let denied = request(port, route, credentials, post);
        assert_eq!(
            denied.status, 403,
            "unavailable identity MUST forbid: {}",
            denied.body
        );
        assert_eq!(
            denied.body["check"], "caller_identity",
            "must exercise actual attribution refusal"
        );
    }
    assert!(
        wire.count("agent_list") >= before + 2,
        "both guards must consult required identity"
    );
    assert_eq!(
        wire.count("master_permission_list"),
        0,
        "read handler ran without authority"
    );
    assert_eq!(
        wire.count("plan_approve"),
        0,
        "mutation handler ran without authority"
    );
    eprintln!("CAD1193 actual HTTP read+write refused caller_identity; handlers not reached");

    wire.mode.store(STALLED_IDENTITY, SeqCst);
    let before = wire.count("agent_list");
    let unavailable = request(port, "/api/meta?operator=1", credentials, false);
    assert!(
        wire.count("agent_list") > before,
        "must stall the REAL required identity RPC"
    );
    assert_unknown_meta(&unavailable);
    assert_eq!(
        unavailable.body["signed_in"], true,
        "verified session survives unavailable role proof"
    );
    eprintln!(
        "CAD1193 identity stall: metadata unknown in {:?}",
        unavailable.elapsed
    );

    wire.mode.store(STALLED_SESSION, SeqCst);
    let before = wire.count("operator_session_check");
    let unavailable = request(port, "/api/meta?operator=1", credentials, false);
    assert!(
        wire.count("operator_session_check") > before,
        "must stall the REAL session validator"
    );
    assert_unknown_meta(&unavailable);
    assert!(
        unavailable
            .body
            .get("signed_in")
            .is_some_and(Value::is_null),
        "timeout cannot manufacture a false signed-out verdict: {}",
        unavailable.body
    );
    eprintln!(
        "CAD1193 session stall: unknown, not signed-out, in {:?}",
        unavailable.elapsed
    );

    wire.mode.store(HEALTHY, SeqCst);
    let recovered = request(port, "/api/meta", credentials, false);
    assert_eq!(
        recovered.body["signed_in"], true,
        "same exact session remains valid after timeouts"
    );
    assert_ne!(recovered.body["tab_signed_out"], true);
    // Kill ONLY our two Child handles, then drain the owned wire threads.
    drop(board);
    drop(daemon);
    drop(wire);
}
