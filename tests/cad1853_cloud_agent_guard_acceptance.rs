//! CAD-1853 independent negative acceptance — authored from the original
//! ticket acceptance and the published transport shape, not from the
//! implementer's code. The implementer must not edit or weaken this file.
//!
//! Real guards exercised:
//! - the daemon's own caller rule and every `cloud_agent_*` RPC dispatch;
//! - the saved hosted-cadence credential check (`cloud_agent.read`/`write`)
//!   before any transport call;
//! - the explicit local consent grant and the connection state;
//! - the published HTTP transport against a real local board origin, with the
//!   request/response shapes the native consumer actually sends;
//! - the board's operator gate on `POST /api/cloud-agents/<id>/messages` and
//!   `GET /api/cloud-agents`.
//!
//! The model/effect boundary is a real local HTTP server implementing the
//! published contract. It counts every call, so a refusal that reaches it is
//! visible and a duplicate model turn is impossible to hide. No refusal is
//! mocked and no request-echo stands in for a guard.
//!
//! Honest boundary: a peer POSITIVE control still needs a stable producer and an
//! enrolled cloud-purpose peer credential; the peer cases here are the refusals
//! that must hold regardless: peers cannot connect, cannot use the operator's
//! connection, cannot capture a re-registered alias, and cannot reach the
//! human-only CLI/wiki family.
//!
//! Run in an admitted test slot:
//! CARGO_BUILD_JOBS=4 cargo test --features test-seam \
//!   --test suite cad1853_cloud_agent_guard_acceptance -- --test-threads 2
#![cfg(all(feature = "test-seam", target_os = "linux"))]

use cadence_agent::adapter::ProviderEnv;
use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon, issue::Pm, operator_auth, store::Store, ui};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const ORG: &str = "stub-org";
const ORG_ID: &str = "ws-stub-org";
const ALIAS: &str = "cloud-a";
const ALIAS_B: &str = "cloud-b";
const OPERATION: &str = "operation-00000001";
const TEXT: &str = "a real request";
const CHANGED: &str = "same operation, different body";
const CONTRACT_PATH: &str = "/__platform/native-agent/call";

/// These fixtures mutate process environment and share the daemon's socket
/// path, so one test runs at a time even when Cargo is asked for two threads.
static FIXTURE: Mutex<()> = Mutex::new(());

// ---------- the published contract, implemented by a real local server ----------

struct Seen {
    action: String,
    body: Value,
    authorization: String,
}

struct Script {
    seen: Vec<Seen>,
    endpoint: String,
    agent_organization: Option<String>,
    capabilities: Option<Value>,
    provenance: Option<Value>,
    submit_status: u16,
    submit_state: String,
    submit_text: Option<String>,
    submit_accepted: bool,
    read_state: String,
    read_text: String,
    read_status: u16,
    /// Drop a readback connection without an answer, as an unreachable
    /// platform does: the published transport maps that to an unknown
    /// outcome rather than to a refusal.
    read_drop: bool,
    submit_delay_ms: u64,
    slow_read_operation: Option<String>,
    slow_read_ms: u64,
    cancel_status: u16,
}

impl Script {
    fn new(endpoint: String) -> Self {
        Self {
            seen: Vec::new(),
            endpoint,
            agent_organization: None,
            capabilities: None,
            provenance: None,
            submit_status: 0,
            submit_state: "queued".into(),
            submit_text: None,
            submit_accepted: true,
            read_state: "completed".into(),
            read_text: "genuine stub reply".into(),
            read_status: 0,
            read_drop: false,
            submit_delay_ms: 0,
            slow_read_operation: None,
            slow_read_ms: 0,
            cancel_status: 501,
        }
    }
    fn calls(&self, action: &str) -> usize {
        self.seen.iter().filter(|s| s.action == action).count()
    }
}

struct Stub {
    url: String,
    script: Arc<Mutex<Script>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
    }
}

impl Stub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let script = Arc::new(Mutex::new(Script::new(url.clone())));
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, halted) = (Arc::clone(&script), Arc::clone(&stop));
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                if halted.load(SeqCst) {
                    return;
                }
                let script = Arc::clone(&shared);
                thread::spawn(move || serve(stream, script));
            }
        });
        Self { url, script, stop }
    }
    fn set(&self, change: impl FnOnce(&mut Script)) {
        change(&mut self.script.lock().unwrap_or_else(|e| e.into_inner()));
    }
    fn calls(&self, action: &str) -> usize {
        self.script
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .calls(action)
    }
    fn seen(&self) -> Vec<Value> {
        self.script
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .seen
            .iter()
            .map(|s| s.body.clone())
            .collect()
    }
    fn auths(&self) -> Vec<String> {
        self.script
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .seen
            .iter()
            .map(|seen| seen.authorization.clone())
            .collect()
    }
}

fn reply(stream: &mut TcpStream, status: u16, body: &Value) {
    let text = body.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
}

fn agent_object(script: &Script) -> Value {
    json!({
        "agent_id": "nca_stubagent000000000000000000000000",
        "organization_id": script
            .agent_organization
            .clone()
            .unwrap_or_else(|| ORG_ID.to_string()),
        "audience": script.endpoint.clone(),
        "provenance": script
            .provenance
            .clone()
            .unwrap_or_else(|| json!({
                "kind": "user",
                "invoker_id": "inv_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
                "peer": null,
            })),
    })
}

fn conversation_object(conversation: &str) -> Value {
    let mut name = String::from("ns_");
    for byte in conversation.bytes().filter(u8::is_ascii_alphanumeric) {
        if name.len() == 35 {
            break;
        }
        name.push(byte as char);
    }
    while name.len() < 35 {
        name.push('x');
    }
    json!({
        "conversation_id": conversation,
        "remote_session_id": name,
        "provider_session_id": "1",
    })
}

fn truthful_capabilities() -> Value {
    json!({
        "conversation": true,
        "tools": false,
        "mcp": false,
        "skills": false,
        "approvals": false,
        "cancellation": false,
    })
}

fn serve(mut stream: TcpStream, script: Arc<Mutex<Script>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let length = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
        })
        .unwrap_or(0);
    while buf.len() < head_end + length {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body: Value =
        serde_json::from_slice(&buf[head_end..head_end + length]).unwrap_or(Value::Null);
    let path = head
        .lines()
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or_default();
    if path != CONTRACT_PATH {
        reply(
            &mut stream,
            404,
            &json!({"ok": false, "error": {"code": "not_found"}}),
        );
        return;
    }
    let action = body["action"].as_str().unwrap_or_default().to_string();
    let arguments = body["arguments"].clone();
    let conversation = arguments["conversation_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let operation = arguments["operation_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    {
        let mut recorded = script.lock().unwrap_or_else(|e| e.into_inner());
        recorded.seen.push(Seen {
            action: action.clone(),
            body: body.clone(),
            authorization: head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("authorization:")
                        .map(|value| value.trim().to_string())
                })
                .unwrap_or_default(),
        });
    }
    let delay = {
        let script = script.lock().unwrap_or_else(|e| e.into_inner());
        script.submit_delay_ms
    };
    if action == "submit" && delay > 0 {
        thread::sleep(Duration::from_millis(delay));
    }
    if action == "readback" {
        let (slow_operation, slow_ms) = {
            let script = script.lock().unwrap_or_else(|e| e.into_inner());
            (script.slow_read_operation.clone(), script.slow_read_ms)
        };
        if slow_ms > 0 && slow_operation.as_deref() == Some(operation.as_str()) {
            thread::sleep(Duration::from_millis(slow_ms));
        }
    }
    let (status, answer) = match action.as_str() {
        "agent" => {
            let script = script.lock().unwrap_or_else(|e| e.into_inner());
            (
                200,
                json!({
                    "ok": true,
                    "agent": agent_object(&script),
                    "conversation": conversation_object(&conversation),
                    "capabilities": script
                        .capabilities
                        .clone()
                        .unwrap_or_else(truthful_capabilities),
                    "runtime": {
                        "kind": "cloudflare-worker-pi-durable",
                        "model_label": "stub-model",
                    },
                }),
            )
        }
        "submit" => {
            let script = script.lock().unwrap_or_else(|e| e.into_inner());
            if script.submit_status != 0 {
                (
                    script.submit_status,
                    json!({"ok": false, "error": {"code": "internal", "message": "stub failure"}}),
                )
            } else {
                (
                    200,
                    json!({
                        "ok": true,
                        "agent": agent_object(&script),
                        "conversation": conversation_object(&conversation),
                        "operation": {
                            "operation_id": operation,
                            "conversation_id": conversation,
                            "status": script.submit_state,
                            "accepted": script.submit_accepted,
                            "invoker_id": "inv_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
                            "text": script.submit_text,
                            "reason": null,
                        },
                    }),
                )
            }
        }
        "readback" => {
            let drop = {
                let script = script.lock().unwrap_or_else(|e| e.into_inner());
                script.read_drop
            };
            if drop {
                // An unreachable platform: the socket closes with no answer.
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return;
            }
            let script = script.lock().unwrap_or_else(|e| e.into_inner());
            if script.read_status != 0 {
                (
                    script.read_status,
                    json!({"ok": false, "error": {"code": "forbidden", "message": "read authority revoked upstream"}}),
                )
            } else {
                let completed = script.read_state == "completed";
                (
                    200,
                    json!({
                        "ok": true,
                        "operation": {
                            "operation_id": operation,
                            "conversation_id": conversation,
                            "status": script.read_state,
                            "text": completed.then(|| script.read_text.clone()),
                            "reason": (script.read_state == "failed").then(|| "unanswered".to_string()),
                            "terminal": completed || script.read_state == "failed",
                        },
                    }),
                )
            }
        }
        "cancel" => {
            let script = script.lock().unwrap_or_else(|e| e.into_inner());
            (
                script.cancel_status,
                json!({"ok": false, "error": {"code": "cancel_unsupported", "message": "unsupported"}}),
            )
        }
        other => (
            400,
            json!({"ok": false, "error": {"code": "invalid_request", "message": other}}),
        ),
    };
    reply(&mut stream, status, &answer);
}

// ---------- isolated daemon + board fixture ----------

struct EnvRestore(Vec<(&'static str, Option<OsString>)>);

impl EnvRestore {
    fn install(root: &Path) -> Self {
        let entries = [
            ("HOME", root.join("home")),
            ("XDG_CONFIG_HOME", root.join("config")),
            ("XDG_STATE_HOME", root.join("xdg-state")),
            ("XDG_DATA_HOME", root.join("data")),
            ("XDG_CACHE_HOME", root.join("cache")),
            ("TMPDIR", root.to_path_buf()),
            ("CADENCE_SUITE_LOCK", root.join("locks/suite")),
            ("CADENCE_PM_DIR", root.join("pm")),
        ];
        for (key, path) in &entries {
            let dir = if *key == "CADENCE_SUITE_LOCK" {
                path.parent().unwrap()
            } else {
                path.as_path()
            };
            std::fs::create_dir_all(dir).unwrap();
        }
        let saved = entries
            .iter()
            .map(|(key, _)| (*key, std::env::var_os(key)))
            .collect();
        for (key, value) in entries {
            std::env::set_var(key, value);
        }
        Self(saved)
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (key, value) in self.0.drain(..) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn login_path(org: &str) -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .expect("HOME or XDG_CONFIG_HOME");
    base.join("cadence/remote-auth")
        .join(format!("cli-{org}.json"))
}

struct LoginSpec<'a> {
    org: &'a str,
    org_id: &'a str,
    endpoint: &'a str,
    capabilities: &'a [&'a str],
    expires: u64,
    subject: &'a str,
    issuer: &'a str,
    token_char: &'a str,
}

fn write_login(org: &str, org_id: &str, endpoint: &str, capabilities: &[&str], expires: u64) {
    write_login_owner(LoginSpec {
        org,
        org_id,
        endpoint,
        capabilities,
        expires,
        subject: "stub-subject",
        issuer: "https://issuer.example.test",
        token_char: "a",
    });
}

fn write_login_owner(spec: LoginSpec<'_>) {
    let LoginSpec {
        org,
        org_id,
        endpoint,
        capabilities,
        expires,
        subject,
        issuer,
        token_char,
    } = spec;
    let path = login_path(org);
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir).unwrap();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let token = format!("hct_{}", token_char.repeat(43));
    let body = json!({
        "version": "hosted-cadence-auth.v1",
        "organization_id": org_id,
        "organization_slug": org,
        "audience": endpoint,
        "issuer": issuer,
        "subject_id": subject,
        "capabilities": capabilities,
        "access_token": token,
        "expires_at": expires,
    });
    std::fs::write(&path, serde_json::to_vec(&body).unwrap()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// Serve one state dir with the same isolated, seam-armed options every
/// fixture daemon uses, and wait until it answers. The returned flag is the
/// one the daemon obeys, so a caller can stop it without a `Fx`.
fn spawn_daemon_at(
    state: &Path,
    pm: &Path,
) -> (Arc<AtomicBool>, JoinHandle<cadence_agent::Result<()>>) {
    let stop = Arc::new(AtomicBool::new(false));
    let env = ProviderEnv::refusing_providers();
    env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let opts = daemon::ServeOptions {
        provider_env: env,
        stop: Some(Arc::clone(&stop)),
        test_seam: true,
        slots: Some(Default::default()),
        lease: Some(Default::default()),
        auto_stop: Some(daemon::AutoStopSetting::off()),
        agent_gc: Some(Default::default()),
        report_router: Some(0),
        checkup: Some(0),
        ..Default::default()
    };
    let owned = state.to_path_buf();
    let handle = thread::spawn(move || daemon::serve_with(&owned, opts));
    let deadline = Instant::now() + Duration::from_secs(30);
    while client::rpc_timeout(state, "health", json!({}), Duration::from_secs(1)).is_err()
        || Seam::token_at(state).is_none()
    {
        assert!(
            !handle.is_finished() && Instant::now() < deadline,
            "daemon never started at {}",
            state.display()
        );
        thread::sleep(Duration::from_millis(50));
    }
    (stop, handle)
}

struct Fx {
    _env: EnvRestore,
    root: tempfile::TempDir,
    state: PathBuf,
    pm: PathBuf,
    stop: Arc<AtomicBool>,
    daemon: Option<JoinHandle<cadence_agent::Result<()>>>,
    stub: Stub,
    board_port: u16,
    board: Option<JoinHandle<()>>,
    cookie: String,
    session_key: String,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(handle) = self.daemon.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.board.take() {
            let _ = handle.join();
        }
    }
}

impl Fx {
    fn start() -> Self {
        assert!(
            std::env::var_os("CADENCE_SOCKET").is_none(),
            "fixture BLOCKED: inherited socket override"
        );
        let lock = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::Builder::new()
            .prefix("c1853acc-")
            .tempdir_in("/tmp")
            .unwrap();
        let env = EnvRestore::install(root.path());
        let pm = root.path().join("pm");
        Pm::init(&pm).unwrap();
        let stub = Stub::start();
        let expires = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 600;
        write_login(
            ORG,
            ORG_ID,
            &stub.url,
            &["cloud_agent.read", "cloud_agent.write"],
            expires,
        );
        let mut fx = Self {
            _env: env,
            state: root.path().join("state"),
            pm,
            root,
            stop: Arc::new(AtomicBool::new(false)),
            daemon: None,
            stub,
            board_port: 0,
            board: None,
            cookie: String::new(),
            session_key: String::new(),
            _lock: lock,
        };
        fx.spawn_daemon();
        fx
    }

    fn spawn_daemon(&mut self) {
        let (stop, handle) = spawn_daemon_at(&self.state, &self.pm);
        self.stop = stop;
        self.daemon = Some(handle);
    }

    /// Stop the daemon without starting another one, so a case can act on the
    /// durable store exactly as a later restart will see it.
    fn stop_daemon(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(handle) = self.daemon.take() {
            let _ = handle.join();
        }
    }

    fn restart(&mut self) {
        self.stop_daemon();
        self.spawn_daemon();
    }

    fn seam_token(&self) -> String {
        Seam::token_at(&self.state).expect("seam token")
    }

    fn store(&self) -> Store {
        Store::open(&self.state.join("cadence.sqlite3")).unwrap()
    }

    fn cwd(&self) -> String {
        self.root.path().to_string_lossy().to_string()
    }

    fn call(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(who, || client::rpc(&self.state, method, params))
    }

    fn connect(&self) -> Value {
        self.call(
            Asserted::Operator,
            "cloud_agent_connect",
            json!({
                "org": ORG,
                "org_id": ORG_ID,
                "endpoint": self.stub.url,
                "alias": ALIAS,
                "accept_assistant_consent": true,
            }),
        )
        .unwrap_or_else(|e| panic!("cloud_agent_connect refused: {e}"))
    }

    fn start_board(&mut self) {
        if self.board.is_some() {
            return;
        }
        let (started, ready) = std::sync::mpsc::channel();
        for port in (3110..3200).filter(|port| *port != 3113) {
            let opts = ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(Arc::clone(&self.stop)),
                startup: Some(started.clone()),
                test_seam: true,
                ..Default::default()
            };
            let (state, pm) = (self.state.clone(), self.pm.clone());
            let handle = thread::spawn(move || {
                let _ = ui::serve(&state, &pm, &opts);
            });
            match ready.recv_timeout(Duration::from_secs(20)) {
                Ok(Ok(())) => {
                    self.board_port = port;
                    self.board = Some(handle);
                    break;
                }
                _ => {
                    let _ = handle.join();
                }
            }
        }
        assert_ne!(self.board_port, 0, "no isolated board port in 3110-3199");
        operator_auth::ensure_secret(&self.state).unwrap();
        let secret = operator_auth::read_secret(&self.state).unwrap();
        let nonce = self
            .call(
                Asserted::Operator,
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )
            .expect("operator link mint")["nonce"]
            .clone();
        let (status, body, cookie) = self.http(
            "operator",
            "/api/session",
            Some(json!({"nonce": nonce})),
            None,
        );
        assert_eq!(status, 200, "operator session exchange failed: {body}");
        self.cookie = cookie.expect("session cookie");
        self.session_key = body["session_key"].as_str().unwrap().to_string();
    }

    fn post(&self, who: &str, route: &str, body: Value, session: bool) -> (u16, Value) {
        let (status, body, _) = self.http(
            who,
            route,
            Some(body),
            session.then_some((self.cookie.as_str(), self.session_key.as_str())),
        );
        (status, body)
    }

    fn get(&self, who: &str, route: &str, session: bool) -> (u16, Value) {
        let host = format!("cadence-{}.localhost:{}", self.board_port, self.board_port);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .build()
            .into();
        let mut request = agent
            .get(format!("http://127.0.0.1:{}{route}", self.board_port))
            .header("Host", &host)
            .header(AS_HEADER, who)
            .header(TOKEN_HEADER, &self.seam_token());
        if session {
            request = request
                .header("Cookie", &self.cookie)
                .header("X-Cadence-Session", &self.session_key);
        }
        let mut response = request.call().unwrap();
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(json!({"raw": text})),
        )
    }

    fn http(
        &self,
        who: &str,
        route: &str,
        body: Option<Value>,
        session: Option<(&str, &str)>,
    ) -> (u16, Value, Option<String>) {
        let host = format!("cadence-{}.localhost:{}", self.board_port, self.board_port);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .build()
            .into();
        let url = format!("http://127.0.0.1:{}{route}", self.board_port);
        let mut request = agent
            .post(&url)
            .header("Host", &host)
            .header("Origin", format!("http://{host}"))
            .header("X-Cadence-Board", "1")
            .header("Content-Type", "application/json")
            .header(AS_HEADER, who)
            .header(TOKEN_HEADER, &self.seam_token());
        if let Some((cookie, key)) = session {
            request = request
                .header("Cookie", cookie)
                .header("X-Cadence-Session", key);
        }
        let mut response = request
            .send(body.unwrap_or_else(|| json!({})).to_string())
            .unwrap();
        let status = response.status().as_u16();
        let cookie = response
            .headers()
            .get("set-cookie")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap().to_string());
        let text = response.body_mut().read_to_string().unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(json!({"raw": text})),
            cookie,
        )
    }
}

fn code(error: &cadence_agent::Error) -> Option<&str> {
    error.code()
}

fn thread_of(connect: &Value) -> String {
    connect["agent"]["thread"]["id"]
        .as_str()
        .expect("connected agent thread")
        .to_string()
}

fn cloud_agent_id(connect: &Value) -> String {
    connect["agent"]["id"]
        .as_str()
        .expect("cloud agent id")
        .to_string()
}

// ---------- acceptance ----------

#[test]
fn connect_requires_explicit_assistant_consent_before_transport() {
    let fx = Fx::start();
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_connect",
            json!({"org": ORG, "org_id": ORG_ID, "endpoint": fx.stub.url, "alias": ALIAS}),
        )
        .unwrap_err();
    assert_eq!(
        code(&error),
        Some("cloud_agent_consent_required"),
        "{error}"
    );
    assert_eq!(fx.stub.calls("agent"), 0);
    assert!(fx.store().cloud_agents().unwrap().is_empty());
}

#[test]
fn connect_refuses_a_saved_login_without_the_explicit_scope() {
    let fx = Fx::start();
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 600;
    write_login(ORG, ORG_ID, &fx.stub.url, &["cloud_agent.read"], expires);
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_connect",
            json!({
                "org": ORG, "org_id": ORG_ID, "endpoint": fx.stub.url,
                "alias": ALIAS, "accept_assistant_consent": true,
            }),
        )
        .unwrap_err();
    assert_eq!(code(&error), Some("cloud_agent_scope_revoked"), "{error}");
    assert_eq!(fx.stub.calls("agent"), 0);

    // Legacy cli.write is not the cloud scope either.
    write_login(
        ORG,
        ORG_ID,
        &fx.stub.url,
        &["cli.read", "cli.write"],
        expires,
    );
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_connect",
            json!({
                "org": ORG, "org_id": ORG_ID, "endpoint": fx.stub.url,
                "alias": ALIAS, "accept_assistant_consent": true,
            }),
        )
        .unwrap_err();
    assert_eq!(code(&error), Some("cloud_agent_scope_revoked"), "{error}");
    assert_eq!(fx.stub.calls("agent"), 0);
}

#[test]
fn forged_identity_fields_are_refused_before_transport() {
    let fx = Fx::start();
    for field in [
        "sender",
        "role",
        "owner",
        "tenant",
        "session_id",
        "remote_agent_id",
        "principal",
    ] {
        let mut params = json!({
            "org": ORG, "org_id": ORG_ID, "endpoint": fx.stub.url,
            "alias": ALIAS, "accept_assistant_consent": true,
        });
        params[field] = json!("forged");
        let error = fx
            .call(Asserted::Operator, "cloud_agent_connect", params)
            .unwrap_err();
        assert_eq!(
            code(&error),
            Some("cloud_agent_identity_field"),
            "{field}: {error}"
        );
    }
    assert_eq!(fx.stub.calls("agent"), 0);

    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let thread = thread_of(&connect);
    for field in [
        "sender",
        "role",
        "owner",
        "tenant",
        "session_id",
        "remote_agent_id",
    ] {
        let mut params = json!({
            "cloud_agent_id": id, "thread_id": thread,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        });
        params[field] = json!("forged");
        let error = fx
            .call(Asserted::Operator, "cloud_agent_send", params)
            .unwrap_err();
        assert_eq!(
            code(&error),
            Some("cloud_agent_identity_field"),
            "{field}: {error}"
        );
    }
    assert_eq!(fx.stub.calls("submit"), 0);
}

#[test]
fn connect_refuses_a_foreign_tenant_or_a_false_capability_claim() {
    let fx = Fx::start();
    fx.stub.set(|script| {
        script.agent_organization = Some("ws-other-tenant".into());
    });
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_connect",
            json!({
                "org": ORG, "org_id": ORG_ID, "endpoint": fx.stub.url,
                "alias": ALIAS, "accept_assistant_consent": true,
            }),
        )
        .unwrap_err();
    assert_eq!(code(&error), Some("cloud_agent_wrong_tenant"), "{error}");
    assert!(fx.store().cloud_agents().unwrap().is_empty());

    fx.stub.set(|script| {
        script.agent_organization = None;
        script.capabilities = Some(json!({
            "conversation": true, "tools": true, "mcp": false,
            "skills": false, "approvals": false, "cancellation": false,
        }));
    });
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_connect",
            json!({
                "org": ORG, "org_id": ORG_ID, "endpoint": fx.stub.url,
                "alias": ALIAS, "accept_assistant_consent": true,
            }),
        )
        .unwrap_err();
    assert_eq!(
        code(&error),
        Some("cloud_agent_capability_refused"),
        "{error}"
    );

    fx.stub.set(|script| {
        script.capabilities = None;
        script.provenance = Some(json!({
            "kind": "local_peer",
            "peer": {"agent_id": "hca_forged", "client_agent_id": null, "role": "cloud_agent"},
        }));
    });
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_connect",
            json!({
                "org": ORG, "org_id": ORG_ID, "endpoint": fx.stub.url,
                "alias": ALIAS, "accept_assistant_consent": true,
            }),
        )
        .unwrap_err();
    assert_eq!(
        code(&error),
        Some("cloud_agent_provenance_mismatch"),
        "{error}"
    );
    assert!(fx.store().cloud_agents().unwrap().is_empty());
}

#[test]
fn operator_connect_send_and_read_round_trip_through_the_real_transport() {
    let fx = Fx::start();
    let connect = fx.connect();
    let agent = &connect["agent"];
    assert_eq!(agent["provenance"]["kind"], json!("user"));
    assert_eq!(agent["state"], json!("connected"));
    assert_eq!(agent["capabilities"]["tools"], json!(false));
    assert_eq!(agent["local_execution"], json!(false));
    assert!(agent["thread"]["id"].is_string());
    assert_eq!(fx.stub.calls("agent"), 1);

    let thread = thread_of(&connect);
    let id = cloud_agent_id(&connect);
    let sent = fx
        .call(
            Asserted::Operator,
            "cloud_agent_send",
            json!({
                "cloud_agent_id": id, "thread_id": thread,
                "text": TEXT, "operation_id": OPERATION, "wake": true,
            }),
        )
        .unwrap();
    assert_eq!(sent["operation"]["operation_id"], json!(OPERATION));
    assert_eq!(sent["operation"]["status"], json!("queued"));
    assert_eq!(sent["operation"]["source"]["kind"], json!("user"));
    assert_eq!(sent["operation"]["source"]["label"], json!("operator"));
    assert_eq!(fx.stub.calls("submit"), 1);
    let seen = fx.stub.seen();
    assert_eq!(seen[1]["version"], json!("hosted-cadence-native-agent.v1"));
    assert_eq!(seen[1]["action"], json!("submit"));
    assert_eq!(seen[1]["arguments"]["text"], json!(TEXT));
    assert_eq!(seen[1]["arguments"]["operation_id"], json!(OPERATION));

    let read = fx
        .call(
            Asserted::Operator,
            "cloud_agent_read",
            json!({"cloud_agent_id": id, "operation_id": OPERATION}),
        )
        .unwrap();
    assert_eq!(read["operation"]["status"], json!("completed"));
    assert_eq!(
        read["operation"]["reply"]["text"],
        json!("genuine stub reply")
    );
    assert_eq!(fx.stub.calls("submit"), 1);

    // The canonical Threads history holds the request and the reply.
    let rows = fx.store().thread_entries(ALIAS, 0, 100).unwrap();
    let bodies: Vec<&str> = rows.iter().map(|row| row.text.as_str()).collect();
    assert!(bodies.contains(&TEXT), "{bodies:?}");
    assert!(bodies.contains(&"genuine stub reply"), "{bodies:?}");

    // The ORIGINAL criterion: ordinary message_read and the canonical work
    // projection see the durable request and the automatic result.
    let request_id = sent["operation"]["message_id"]
        .as_str()
        .expect("request message id");
    let reply_id = read["operation"]["reply"]["message_id"]
        .as_str()
        .expect("reply message id");
    let request = fx
        .store()
        .message(request_id)
        .unwrap()
        .expect("request message row");
    assert_eq!(request.body, TEXT);
    // Canonical completion records the reply as the operation's thread result;
    // the operation id is the deterministic report id, not a second message row.
    assert_eq!(reply_id, request_id);
    let reply_entries: Vec<_> = fx
        .store()
        .thread_entries(ALIAS, 0, 100)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.text == "genuine stub reply")
        .collect();
    assert_eq!(reply_entries.len(), 1, "{reply_entries:?}");
    // The same canonical work-conversation projection the board/CLI reads.
    let link = fx
        .call(Asserted::Operator, "thread_read", json!({"alias": ALIAS}))
        .unwrap();
    let projected: Vec<&str> = link["entries"]
        .as_array()
        .expect("thread entries")
        .iter()
        .filter_map(|entry| entry["text"].as_str())
        .collect();
    assert!(projected.contains(&TEXT), "{projected:?}");
    assert!(projected.contains(&"genuine stub reply"), "{projected:?}");
}

#[test]
fn an_alias_already_owning_a_local_runtime_cannot_be_reused_for_a_cloud_agent() {
    let fx = Fx::start();
    fx.connect();
    let error = fx
        .call(
            Asserted::Operator,
            "agent_register",
            json!({
                "alias": ALIAS, "provider": "fake",
                "endpoint_kind": "fake", "cwd": fx.cwd(),
            }),
        )
        .unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("alias"),
        "alias reuse was not refused: {error}"
    );
    assert_eq!(fx.stub.calls("agent"), 1);
}

#[test]
fn changed_body_reuse_conflicts_and_same_body_never_re_submits() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let thread = thread_of(&connect);
    let send = |text: &str| {
        fx.call(
            Asserted::Operator,
            "cloud_agent_send",
            json!({
                "cloud_agent_id": id, "thread_id": thread,
                "text": text, "operation_id": OPERATION, "wake": true,
            }),
        )
    };
    send(TEXT).unwrap();
    assert_eq!(fx.stub.calls("submit"), 1);

    let error = send(CHANGED).unwrap_err();
    assert_eq!(code(&error), Some("cloud_agent_replay_conflict"), "{error}");
    assert_eq!(fx.stub.calls("submit"), 1);

    // Same id + same body is resolved by readback, never a second submit.
    let replay = send(TEXT).unwrap();
    assert_eq!(replay["operation"]["status"], json!("completed"));
    assert_eq!(fx.stub.calls("submit"), 1);
    assert_eq!(fx.stub.calls("readback"), 1);
}

#[test]
fn an_uncertain_submit_is_resolved_by_readback_without_a_duplicate_turn() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let thread = thread_of(&connect);
    fx.stub.set(|script| script.submit_status = 500);
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_send",
            json!({
                "cloud_agent_id": id, "thread_id": thread,
                "text": TEXT, "operation_id": OPERATION, "wake": true,
            }),
        )
        .unwrap_err();
    assert_eq!(error.kind(), "unknown", "{error}");
    assert_eq!(fx.stub.calls("submit"), 1);

    fx.stub.set(|script| {
        script.submit_status = 0;
        script.read_state = "completed".into();
    });
    let read = fx
        .call(
            Asserted::Operator,
            "cloud_agent_read",
            json!({"cloud_agent_id": id, "operation_id": OPERATION}),
        )
        .unwrap();
    assert_eq!(read["operation"]["status"], json!("completed"));
    assert_eq!(fx.stub.calls("submit"), 1);
    assert_eq!(fx.stub.calls("readback"), 1);
}

#[test]
fn an_unknown_readback_keeps_reconciling_and_never_resubmits() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let thread = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": thread,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    assert_eq!(fx.stub.calls("submit"), 1);

    // The claim exists locally but the remote has no record yet: honest unknown.
    fx.stub.set(|script| script.read_state = "unknown".into());
    let first = fx
        .call(
            Asserted::Operator,
            "cloud_agent_read",
            json!({"cloud_agent_id": id, "operation_id": OPERATION}),
        )
        .unwrap();
    assert_eq!(first["operation"]["status"], json!("unknown"));
    assert_eq!(first["operation"]["terminal"], json!(false));
    let row = fx
        .store()
        .cloud_agent_operation(&id, OPERATION)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "unknown");

    // The original Pi record becomes visible; reconciliation must observe it
    // and must not enqueue a second turn.
    fx.stub.set(|script| {
        script.read_state = "completed".into();
        script.read_text = "original result".into();
    });
    let second = fx
        .call(
            Asserted::Operator,
            "cloud_agent_read",
            json!({"cloud_agent_id": id, "operation_id": OPERATION}),
        )
        .unwrap();
    assert_eq!(second["operation"]["status"], json!("completed"));
    assert_eq!(
        second["operation"]["reply"]["text"],
        json!("original result")
    );
    assert_eq!(fx.stub.calls("submit"), 1);
    assert_eq!(fx.stub.calls("readback"), 2);
}

#[test]
fn restart_resumes_by_readback_and_never_resubmits() {
    let mut fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let thread = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": thread,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    assert_eq!(fx.stub.calls("submit"), 1);

    fx.restart();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let operation = fx
            .store()
            .cloud_agent_operation(&id, OPERATION)
            .unwrap()
            .expect("operation row");
        if operation.status == "completed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "resume never settled: {operation:?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(fx.stub.calls("submit"), 1);
    assert_eq!(fx.stub.calls("readback"), 1);
}

#[test]
fn revocation_refuses_a_queued_wake_and_a_completed_readback() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 600;

    // Complete and read one turn on the canonical work conversation while the
    // grant is live. Settling it first keeps the server's one-unsettled-turn
    // rule out of the later revocation assertions.
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": "completed before revocation", "operation_id": "operation-00000002", "wake": true,
        }),
    )
    .unwrap();
    fx.call(
        Asserted::Operator,
        "cloud_agent_read",
        json!({"cloud_agent_id": id, "operation_id": "operation-00000002"}),
    )
    .unwrap();

    // A queued-but-not-woken turn: revoking the write scope refuses the wake
    // before any transport call.
    write_login(ORG, ORG_ID, &fx.stub.url, &["cloud_agent.read"], expires);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": "queued after revocation", "operation_id": "operation-00000009", "wake": false,
        }),
    )
    .unwrap();
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_send",
            json!({
                "cloud_agent_id": id, "thread_id": home,
                "text": "queued after revocation", "operation_id": "operation-00000009", "wake": true,
            }),
        )
        .unwrap_err();
    assert_eq!(code(&error), Some("cloud_agent_scope_revoked"), "{error}");
    assert_eq!(
        fx.stub.calls("submit"),
        1,
        "the queued wake never reached the model"
    );

    // Revoking read authority hides the already-durable reply.
    write_login(ORG, ORG_ID, &fx.stub.url, &["cloud_agent.write"], expires);
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_read",
            json!({"cloud_agent_id": id, "operation_id": "operation-00000002"}),
        )
        .unwrap_err();
    assert_eq!(code(&error), Some("cloud_agent_scope_revoked"), "{error}");
}

#[test]
fn submit_http_503_after_acceptance_is_uncertain_and_reconciles_the_original_turn() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.stub.set(|script| script.submit_status = 503);
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_send",
            json!({
                "cloud_agent_id": id, "thread_id": home,
                "text": TEXT, "operation_id": OPERATION, "wake": true,
            }),
        )
        .unwrap_err();
    assert_eq!(code(&error), Some("cloud_agent_uncertain"), "{error}");
    fx.stub.set(|script| {
        script.submit_status = 0;
        script.read_state = "completed".into();
    });
    let read = fx
        .call(
            Asserted::Operator,
            "cloud_agent_read",
            json!({"cloud_agent_id": id, "operation_id": OPERATION}),
        )
        .unwrap();
    assert_eq!(read["operation"]["status"], json!("completed"));
    assert_eq!(fx.stub.calls("submit"), 1);
}

#[test]
fn queued_turn_reaches_the_canonical_completion_without_manual_cloud_read_or_restart() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let replies = fx
            .store()
            .thread_entries(ALIAS, 0, 100)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.text == "genuine stub reply")
            .count();
        if replies == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the queued turn never reached canonical completion/report: {replies}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(fx.stub.calls("submit"), 1);
}

#[test]
fn a_stale_expected_recipient_uid_refuses_before_the_cloud_call() {
    let fx = Fx::start();
    let connect = fx.connect();
    let _ = cloud_agent_id(&connect);
    let _ = thread_of(&connect);
    let error = fx
        .call(
            Asserted::Operator,
            "thread_send",
            json!({
                "alias": ALIAS, "text": "stale recipient", "message": "t1853-stale",
                "expected_recipient_uid": "another-agent-uid",
            }),
        )
        .unwrap_err();
    assert!(
        !error.to_string().is_empty(),
        "stale expected UID was not refused: {error}"
    );
    assert_eq!(fx.stub.calls("submit"), 0);
}

#[test]
fn concurrent_and_repeated_completion_record_exactly_one_reply() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    let state = fx.state.clone();
    let one = {
        let state = state.clone();
        let id = id.clone();
        thread::spawn(move || {
            client::rpc(
                &state,
                "cloud_agent_read",
                json!({"cloud_agent_id": id, "operation_id": OPERATION}),
            )
        })
    };
    let two = {
        let state = state.clone();
        let id = id.clone();
        thread::spawn(move || {
            client::rpc(
                &state,
                "cloud_agent_read",
                json!({"cloud_agent_id": id, "operation_id": OPERATION}),
            )
        })
    };
    let _ = one.join().unwrap();
    let _ = two.join().unwrap();
    let _ = fx.call(
        Asserted::Operator,
        "cloud_agent_read",
        json!({"cloud_agent_id": id, "operation_id": OPERATION}),
    );
    let replies = fx
        .store()
        .thread_entries(ALIAS, 0, 100)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.text == "genuine stub reply")
        .count();
    assert_eq!(replies, 1, "duplicate canonical replies: {replies}");
    assert_eq!(fx.stub.calls("submit"), 1);
}

#[test]
fn a_never_woken_row_is_not_automatically_polled() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": false,
        }),
    )
    .unwrap();
    assert_eq!(fx.stub.calls("submit"), 0);
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        fx.stub.calls("readback"),
        0,
        "a never-submitted no-wake row must not be polled"
    );
}

#[test]
fn same_uid_peer_enrollment_cannot_reach_the_cloud_target() {
    use cadence_agent::store::cloud_agent::{CloudSource, Owner};

    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "agent_register",
        json!({"alias": "peer", "provider": "fake", "endpoint_kind": "fake", "cwd": fx.cwd()}),
    )
    .unwrap();
    let owner = Owner::new("https://issuer.example.test", "stub-subject");
    let peer = fx.store().agent("peer").unwrap();
    let stamp = cadence_agent::cloud_agent::registration_epoch(peer.created);
    let store = fx.store();
    let owner_for_file = owner.clone();
    store
        .cloud_agent_source_grant(&CloudSource {
            cloud_agent_id: id.clone(),
            kind: "local_peer".into(),
            uid: peer.uid.clone(),
            registration_epoch: stamp,
            owner,
            label: "peer".into(),
            scope: "cloud_agent.write".into(),
            granted_by: "operator".into(),
            granted_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs_f64(),
            invoker_id: None,
            state: "active".into(),
        })
        .unwrap();
    let cloud = store.cloud_agent(&id).unwrap().expect("cloud agent");
    cadence_agent::remote_cli::cloud_agent_save_peer(
        &cadence_agent::remote_cli::RemoteTarget {
            org: cloud.org_slug.clone(),
            endpoint: cloud.endpoint.clone(),
            org_id: cloud.organization_id.clone(),
            identity: None,
        },
        &owner_for_file,
        &peer.uid,
        stamp,
        &json!({
            "agent_id": "hca_native_guard_peer",
            "credential_id": "hcp_native_guard_peer",
            "token": format!("hct_{}", "p".repeat(43)),
            "expires_at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 600,
            "role": "cloud_agent",
            "sender": {"uid": peer.uid, "registration_epoch": stamp},
        }),
    )
    .unwrap();
    let error = fx
        .call(
            Asserted::Agent("peer".into()),
            "cloud_agent_send",
            json!({
                "cloud_agent_id": id, "thread_id": home,
                "text": "peer request", "operation_id": OPERATION, "wake": true,
            }),
        )
        .unwrap_err();
    assert!(
        matches!(
            code(&error),
            Some("cloud_agent_peer_isolation_required")
                | Some("cloud_agent_owner_mismatch")
                | Some("cloud_agent_consent_required")
        ),
        "same-uid peer was not stopped before cloud access: {error}"
    );
    assert_eq!(fx.stub.calls("submit"), 0);
}

#[test]
fn admitted_peer_source_settles_canonical_result_and_uid_bound_report() {
    use cadence_agent::cloud_agent::{
        self, CloudTransport, RemoteAgent, RemoteOperation, RemoteStatus, Sender, Source,
        SubmitRequest,
    };
    use cadence_agent::store::cloud_agent::{CloudSource, Owner};

    struct PeerTransport {
        read: Option<(RemoteStatus, Option<&'static str>)>,
    }
    impl CloudTransport for PeerTransport {
        fn connect(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &str,
        ) -> cadence_agent::Result<RemoteAgent> {
            Err(cadence_agent::Error::rejected("connect unused"))
        }
        fn submit(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            request: &SubmitRequest<'_>,
        ) -> cadence_agent::Result<RemoteOperation> {
            Ok(RemoteOperation {
                status: RemoteStatus::Queued,
                remote_operation_id: Some(request.operation_id.to_string()),
                remote_session_id: None,
                provider_session_id: None,
                reply: None,
                error: None,
                invoker_id: Some("inv_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".into()),
            })
        }
        fn read(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &str,
            operation: &str,
            _: &Source,
        ) -> cadence_agent::Result<RemoteOperation> {
            let (status, text) = self.read.expect("read mode configured for this step");
            Ok(RemoteOperation {
                status,
                remote_operation_id: Some(operation.to_string()),
                remote_session_id: Some("ns_peer_session_000000000000000000".into()),
                provider_session_id: Some("1".into()),
                reply: text.map(str::to_string),
                error: None,
                invoker_id: Some("inv_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".into()),
            })
        }
        fn cancel(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &str,
            _: &str,
            _: &Source,
        ) -> cadence_agent::Result<()> {
            Err(cadence_agent::Error::rejected("cancel unused"))
        }
    }

    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "agent_register",
        json!({"alias": "peer", "provider": "fake", "endpoint_kind": "fake", "cwd": fx.cwd()}),
    )
    .unwrap();

    let owner = Owner::new("https://issuer.example.test", "stub-subject");
    let peer = fx.store().agent("peer").unwrap();
    let stamp = cadence_agent::cloud_agent::registration_epoch(peer.created);
    let store = fx.store();
    store
        .cloud_agent_source_grant(&CloudSource {
            cloud_agent_id: id.clone(),
            kind: "local_peer".into(),
            uid: peer.uid.clone(),
            registration_epoch: stamp,
            owner: owner.clone(),
            label: "peer".into(),
            scope: "cloud_agent.write".into(),
            granted_by: "operator".into(),
            granted_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs_f64(),
            invoker_id: None,
            state: "active".into(),
        })
        .unwrap();
    let cloud = store.cloud_agent(&id).unwrap().expect("cloud agent");
    let target = cadence_agent::remote_cli::RemoteTarget {
        org: cloud.org_slug.clone(),
        endpoint: cloud.endpoint.clone(),
        org_id: cloud.organization_id.clone(),
        identity: None,
    };
    let source = Source::registered(&peer, owner.clone());
    let sender = Sender::Agent("peer".into());

    // The downstream canonical enqueue/finish path for an already-admitted
    // peer source (the daemon's split-UID front door is exercised separately).
    let queued = PeerTransport { read: None };
    cloud_agent::send(
        &store,
        &queued,
        &cloud,
        &target,
        &home,
        &sender,
        &source,
        OPERATION,
        "peer request",
        true,
        None,
        || Ok(()),
    )
    .unwrap();

    // Intermediate unknown writes no final result/report.
    let unknown = PeerTransport {
        read: Some((RemoteStatus::Unknown, None)),
    };
    cloud_agent::read_operation(
        &store,
        &unknown,
        &cloud,
        &target,
        OPERATION,
        &source,
        || Ok(()),
    )
    .unwrap();
    let message = store.message(OPERATION).unwrap().unwrap();
    assert_eq!(message.state, "running", "unknown wrote a final result");
    assert_eq!(
        store
            .thread_entries(ALIAS, 0, 100)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.text == "genuine stub reply")
            .count(),
        0
    );
    assert!(store
        .messages("peer")
        .unwrap()
        .iter()
        .all(|row| !row.body.contains(OPERATION)));

    // The later real completion settles one canonical result and one
    // UID-bound worker_result that names the deterministic operation id.
    let completed = PeerTransport {
        read: Some((RemoteStatus::Completed, Some("genuine stub reply"))),
    };
    cloud_agent::read_operation(
        &store,
        &completed,
        &cloud,
        &target,
        OPERATION,
        &source,
        || Ok(()),
    )
    .unwrap();
    let results: Vec<_> = store
        .thread_entries(ALIAS, 0, 100)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.text == "genuine stub reply")
        .collect();
    assert_eq!(results.len(), 1, "duplicate canonical result");
    assert_eq!(results[0].message_id.as_deref(), Some(OPERATION));
    let reports: Vec<_> = store
        .messages("peer")
        .unwrap()
        .into_iter()
        .filter(|row| row.body.contains(OPERATION))
        .collect();
    assert_eq!(reports.len(), 1, "duplicate/missing peer worker_result");
    let shown = fx
        .call(
            Asserted::Operator,
            "cloud_agent_show",
            json!({"cloud_agent_id": id}),
        )
        .unwrap();
    let peer_session = shown["agent"]["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|session| session["source"]["kind"] == json!("local_peer"))
        .expect("peer session metadata");
    let link = peer_session["thread"]["link"]
        .as_str()
        .expect("session link");
    let pair = link
        .strip_prefix("/threads?c=")
        .expect("peer history must use the work-conversation selector");
    let listed = fx
        .call(
            Asserted::Operator,
            "work_conversation_list",
            json!({"include_system": true}),
        )
        .unwrap();
    assert!(
        listed["conversations"]
            .as_array()
            .expect("work conversations")
            .iter()
            .any(|row| row["id"] == json!(pair)),
        "the peer link is not a real work conversation: {listed}"
    );
    let read_back = fx
        .call(
            Asserted::Operator,
            "work_conversation_read",
            json!({"conversation": pair, "include_system": true}),
        )
        .unwrap();
    let peer_bodies: Vec<&str> = read_back["messages"]
        .as_array()
        .expect("peer conversation messages")
        .iter()
        .filter_map(|message| message["body"].as_str())
        .collect();
    assert!(
        peer_bodies.contains(&"peer request"),
        "peer request missing from the canonical conversation: {peer_bodies:?}"
    );
    assert!(
        peer_bodies
            .iter()
            .any(|body| body.contains(OPERATION) && body.contains("completed")),
        "peer worker_result report missing the original operation/result: {peer_bodies:?}"
    );
    assert!(
        !link.contains("?h="),
        "peer session exposed the private human thread view"
    );

    // Same native uid+stamp, different parent issuer/subject: the settled
    // history is not disclosed and no transport is invoked. The transport
    // answers and counts, so the refusal can only be the source/owner check.
    struct WouldAnswer(Arc<AtomicUsize>);
    impl CloudTransport for WouldAnswer {
        fn connect(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &str,
        ) -> cadence_agent::Result<RemoteAgent> {
            Err(cadence_agent::Error::rejected("connect unused"))
        }
        fn submit(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &SubmitRequest<'_>,
        ) -> cadence_agent::Result<RemoteOperation> {
            Err(cadence_agent::Error::rejected("submit unused"))
        }
        fn read(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &str,
            operation: &str,
            _: &Source,
        ) -> cadence_agent::Result<RemoteOperation> {
            self.0.fetch_add(1, SeqCst);
            Ok(RemoteOperation {
                status: RemoteStatus::Completed,
                remote_operation_id: Some(operation.to_string()),
                remote_session_id: None,
                provider_session_id: None,
                reply: Some("genuine stub reply".into()),
                error: None,
                invoker_id: Some("inv_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".into()),
            })
        }
        fn cancel(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &str,
            _: &str,
            _: &Source,
        ) -> cadence_agent::Result<()> {
            Err(cadence_agent::Error::rejected("cancel unused"))
        }
    }
    let foreign = Source::registered(
        &peer,
        Owner::new("https://other-issuer.example.test", "other-parent"),
    );
    let foreign_reads = Arc::new(AtomicUsize::new(0));
    let error = cloud_agent::read_operation(
        &store,
        &WouldAnswer(Arc::clone(&foreign_reads)),
        &cloud,
        &target,
        OPERATION,
        &foreign,
        || Ok(()),
    )
    .unwrap_err();
    assert_eq!(
        code(&error),
        Some("cloud_agent_source_mismatch"),
        "a foreign owner was stopped by something other than the source check: {error}"
    );
    assert_eq!(
        foreign_reads.load(SeqCst),
        0,
        "a foreign owner reached the transport"
    );
    // Same human subject, different issuer is the additional old-bug catch.
    let foreign_issuer = Source::operator(Owner::new(
        "https://other-issuer.example.test",
        "stub-subject",
    ));
    let error = cloud_agent::read_operation(
        &store,
        &WouldAnswer(Arc::clone(&foreign_reads)),
        &cloud,
        &target,
        OPERATION,
        &foreign_issuer,
        || Ok(()),
    )
    .unwrap_err();
    assert_eq!(
        code(&error),
        Some("cloud_agent_source_mismatch"),
        "a different issuer was stopped by something other than the source check: {error}"
    );
    assert_eq!(
        foreign_reads.load(SeqCst),
        0,
        "a different issuer reached the transport"
    );
}

#[test]
fn credential_replaced_after_authorization_never_spends_under_the_new_owner_at_the_wire() {
    use cadence_agent::cloud_agent::wire::HostedCloudTransport;
    use cadence_agent::cloud_agent::{self, Sender, Source};
    use cadence_agent::store::Owner;

    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    let store = fx.store();
    let cloud = store.cloud_agent(&id).unwrap().expect("cloud agent");
    let target = cadence_agent::remote_cli::RemoteTarget {
        org: cloud.org_slug.clone(),
        endpoint: cloud.endpoint.clone(),
        org_id: cloud.organization_id.clone(),
        identity: None,
    };
    let owner_a = Owner::new("https://issuer.example.test", "stub-subject");
    let source = Source::operator(owner_a.clone());
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 600;
    let token_a = format!("hct_{}", "a".repeat(43));
    let token_b = format!("hct_{}", "b".repeat(43));

    // Authorization validates owner A; the existing authorize closure then
    // replaces the saved credential with owner B immediately before the paid
    // HTTP submit. The wire must either use the validated A snapshot or fail
    // closed — never spend under B.
    let result = cloud_agent::send(
        &store,
        &HostedCloudTransport,
        &cloud,
        &target,
        &home,
        &Sender::Operator,
        &source,
        OPERATION,
        TEXT,
        true,
        None,
        || {
            write_login_owner(LoginSpec {
                org: ORG,
                org_id: ORG_ID,
                endpoint: &fx.stub.url,
                capabilities: &["cloud_agent.read", "cloud_agent.write"],
                expires,
                subject: "other-subject",
                issuer: "https://other-issuer.example.test",
                token_char: "b",
            });
            Ok(())
        },
    );
    let auths = fx.stub.auths();
    assert!(
        auths.iter().all(|header| !header.contains(&token_b)),
        "a request was sent under the replacement owner B: {auths:?}"
    );
    match result {
        Ok(operation) => {
            assert_eq!(operation.owner.subject, "stub-subject");
            assert!(
                auths.iter().any(|header| header.contains(&token_a)),
                "the validated A credential never reached the wire: {auths:?}"
            );
        }
        Err(error) => {
            assert!(
                !error.to_string().is_empty(),
                "the owner race produced no typed refusal"
            );
            assert!(auths.iter().all(|header| !header.contains(&token_b)));
        }
    }
    let stored = store.cloud_agent_operation(&id, OPERATION).unwrap();
    if let Some(operation) = stored {
        assert_eq!(
            operation.owner.subject, "stub-subject",
            "the operation changed owner"
        );
    }

    // Restored A remains a working positive control on the same connection.
    write_login_owner(LoginSpec {
        org: ORG,
        org_id: ORG_ID,
        endpoint: &fx.stub.url,
        capabilities: &["cloud_agent.read", "cloud_agent.write"],
        expires,
        subject: "stub-subject",
        issuer: "https://issuer.example.test",
        token_char: "a",
    });
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": "operation-00000015", "wake": true,
        }),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let operation = fx
            .store()
            .cloud_agent_operation(&id, "operation-00000015")
            .unwrap()
            .unwrap();
        if operation.status == "completed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "restored A did not work: {operation:?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_late_inflight_readback_cannot_reopen_a_completed_receipt() {
    use cadence_agent::cloud_agent::{
        self, CloudTransport, RemoteAgent, RemoteOperation, RemoteStatus, Source, SubmitRequest,
    };
    use cadence_agent::store::Owner;

    struct Gate {
        state: std::sync::Mutex<(usize, bool)>,
        ready: std::sync::Condvar,
    }
    struct OrderedTransport(std::sync::Arc<Gate>);
    impl CloudTransport for OrderedTransport {
        fn connect(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &str,
        ) -> cadence_agent::Result<RemoteAgent> {
            Err(cadence_agent::Error::rejected("connect unused"))
        }
        fn submit(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &SubmitRequest<'_>,
        ) -> cadence_agent::Result<RemoteOperation> {
            Err(cadence_agent::Error::rejected("submit unused"))
        }
        fn read(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &str,
            operation: &str,
            _: &Source,
        ) -> cadence_agent::Result<RemoteOperation> {
            let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            state.0 += 1;
            let call = state.0;
            self.0.ready.notify_all();
            if call == 1 {
                let deadline = std::time::Instant::now() + Duration::from_secs(20);
                while !state.1 {
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    assert!(!left.is_zero(), "the stale read was never released");
                    state = self.0.ready.wait_timeout(state, left).unwrap().0;
                }
                return Ok(RemoteOperation {
                    status: RemoteStatus::Unknown,
                    remote_operation_id: Some(operation.to_string()),
                    remote_session_id: None,
                    provider_session_id: None,
                    reply: None,
                    error: None,
                    invoker_id: Some("inv_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".into()),
                });
            }
            Ok(RemoteOperation {
                status: RemoteStatus::Completed,
                remote_operation_id: Some(operation.to_string()),
                remote_session_id: None,
                provider_session_id: None,
                reply: Some("genuine stub reply".into()),
                error: None,
                invoker_id: Some("inv_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".into()),
            })
        }
        fn cancel(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &str,
            _: &str,
            _: &Source,
        ) -> cadence_agent::Result<()> {
            Err(cadence_agent::Error::rejected("cancel unused"))
        }
    }

    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    let store = fx.store();
    let cloud = store.cloud_agent(&id).unwrap().expect("cloud agent");
    let target = cadence_agent::remote_cli::RemoteTarget {
        org: cloud.org_slug.clone(),
        endpoint: cloud.endpoint.clone(),
        org_id: cloud.organization_id.clone(),
        identity: None,
    };
    let source = Source::operator(Owner::new("https://issuer.example.test", "stub-subject"));
    let gate = std::sync::Arc::new(Gate {
        state: std::sync::Mutex::new((0, false)),
        ready: std::sync::Condvar::new(),
    });
    let first = {
        let gate = std::sync::Arc::clone(&gate);
        let state = fx.state.clone();
        let cloud = cloud.clone();
        let target = target.clone();
        let source = source.clone();
        thread::spawn(move || {
            let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
            cloud_agent::read_operation(
                &store,
                &OrderedTransport(gate),
                &cloud,
                &target,
                OPERATION,
                &source,
                || Ok(()),
            )
        })
    };
    // The first remote read is genuinely in flight before the second runs.
    {
        let mut state = gate.state.lock().unwrap_or_else(|e| e.into_inner());
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while state.0 == 0 {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(
                !left.is_zero(),
                "the first read never reached the transport"
            );
            state = gate.ready.wait_timeout(state, left).unwrap().0;
        }
    }
    let completed = cloud_agent::read_operation(
        &store,
        &OrderedTransport(std::sync::Arc::clone(&gate)),
        &cloud,
        &target,
        OPERATION,
        &source,
        || Ok(()),
    )
    .unwrap();
    assert_eq!(completed.status, "completed");
    assert_eq!(
        store.message(OPERATION).unwrap().unwrap().state,
        "completed"
    );
    {
        let mut state = gate.state.lock().unwrap_or_else(|e| e.into_inner());
        state.1 = true;
        gate.ready.notify_all();
    }
    let stale = first.join().unwrap().unwrap();
    // The late unknown response is adopted into the already-settled receipt,
    // never reopening it.
    assert_eq!(stale.status, "completed");
    assert_eq!(
        fx.store()
            .cloud_agent_operation(&id, OPERATION)
            .unwrap()
            .unwrap()
            .status,
        "completed",
        "the stale readback reopened the receipt"
    );
    let replies = fx
        .store()
        .thread_entries(ALIAS, 0, 100)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.text == "genuine stub reply")
        .count();
    assert_eq!(replies, 1, "duplicate canonical result");
}

#[test]
fn busy_refusal_does_not_retain_the_refused_turn_and_a_can_still_complete() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": false,
        }),
    )
    .unwrap();
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_send",
            json!({
                "cloud_agent_id": id, "thread_id": home,
                "text": "second body", "operation_id": "operation-00000012", "wake": true,
            }),
        )
        .unwrap_err();
    assert!(
        matches!(
            code(&error),
            Some("cloud_agent_session_busy") | Some("cloud_agent_replay_conflict")
        ),
        "busy was not refused: {error}"
    );
    assert!(
        fx.store()
            .cloud_agent_operation(&id, "operation-00000012")
            .unwrap()
            .is_none(),
        "the refused turn retained a blocking queue claim"
    );
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let operation = fx
            .store()
            .cloud_agent_operation(&id, OPERATION)
            .unwrap()
            .unwrap();
        if operation.status == "completed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "A never completed: {operation:?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(fx.stub.calls("submit"), 1);
}

#[test]
fn definitive_forbidden_submit_settles_the_canonical_message_as_failed() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.stub.set(|script| script.submit_status = 403);
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_send",
            json!({
                "cloud_agent_id": id, "thread_id": home,
                "text": TEXT, "operation_id": OPERATION, "wake": true,
            }),
        )
        .unwrap_err();
    assert_eq!(code(&error), Some("cloud_agent_scope_revoked"), "{error}");
    let message = fx.store().message(OPERATION).unwrap().unwrap();
    assert_eq!(message.state, "failed", "canonical message stayed running");
    assert!(
        message
            .result
            .as_ref()
            .is_some_and(|result| result.get("error").is_some()),
        "no honest error on the terminal message: {message:?}"
    );
    assert_eq!(fx.stub.calls("submit"), 1);
}

#[test]
fn explicit_read_of_a_never_woken_turn_keeps_it_queued_and_still_wakes() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": false,
        }),
    )
    .unwrap();
    let read = fx
        .call(
            Asserted::Operator,
            "cloud_agent_read",
            json!({"cloud_agent_id": id, "operation_id": OPERATION}),
        )
        .unwrap();
    assert_eq!(read["operation"]["status"], json!("queued"));
    assert_eq!(
        fx.stub.calls("readback"),
        0,
        "a queue-only turn was read upstream"
    );
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let operation = fx
            .store()
            .cloud_agent_operation(&id, OPERATION)
            .unwrap()
            .unwrap();
        if operation.status == "completed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "woken turn did not submit: {operation:?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(fx.stub.calls("submit"), 1);
}

#[test]
fn a_cross_target_message_id_collision_leaves_no_blocking_claim() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "agent_register",
        json!({"alias": "other", "provider": "fake", "endpoint_kind": "fake", "cwd": fx.cwd()}),
    )
    .unwrap();
    // The operation id is also the canonical global message id.
    fx.call(
        Asserted::Operator,
        "thread_send",
        json!({
            "alias": "other", "text": "collision owner",
            "message": "operation-00000013",
        }),
    )
    .unwrap();
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_send",
            json!({
                "cloud_agent_id": id, "thread_id": home,
                "text": TEXT, "operation_id": "operation-00000013", "wake": true,
            }),
        )
        .unwrap_err();
    assert!(
        !error.to_string().is_empty(),
        "a global message id collision was not refused"
    );
    assert!(
        fx.store()
            .cloud_agent_operation(&id, "operation-00000013")
            .unwrap()
            .is_none(),
        "the refused turn retained a blocking claim"
    );
    assert_eq!(fx.stub.calls("submit"), 0);
    // A later independent turn is not blocked by the refusal.
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": "operation-00000014", "wake": true,
        }),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let operation = fx
            .store()
            .cloud_agent_operation(&id, "operation-00000014")
            .unwrap()
            .unwrap();
        if operation.status == "completed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "later turn blocked: {operation:?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn eight_old_unknown_rows_do_not_starve_the_ninth_accepted_turn() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    let store = fx.store();

    // One real settled turn supplies a valid CloudOperation template.
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": "operation-00000010", "wake": true,
        }),
    )
    .unwrap();
    fx.call(
        Asserted::Operator,
        "cloud_agent_read",
        json!({"cloud_agent_id": id, "operation_id": "operation-00000010"}),
    )
    .unwrap();
    let template = store
        .cloud_agent_operation(&id, "operation-00000010")
        .unwrap()
        .expect("template operation");

    // Eight unbudging heads on EIGHT DISTINCT provider sessions. They carry a
    // remote id and unknown status, but no canonical message, so completion
    // must fail and they stay first in the worklist.
    for index in 1..=8 {
        let mut draft = template.clone();
        draft.operation_id = format!("stale-operation-{index:08}");
        draft.conversation_id = format!("stale-conversation-{index:08}");
        draft.remote_session_id = format!("ns_stale_session_{index:08}");
        draft.source_uid = format!("stale-uid-{index}");
        draft.registration_epoch = index as i64;
        draft.remote_operation_id = None;
        draft.message_id = None;
        draft.reply_text = None;
        draft.reply_message_id = None;
        store.cloud_agent_operation_begin(&draft).unwrap();
        store
            .cloud_agent_operation_set(
                &id,
                &draft.operation_id,
                "unknown",
                Some(&format!("remote-stale-{index:08}")),
                None,
                None,
                None,
            )
            .unwrap();
    }

    // Ninth accepted turn on its own provider session.
    fx.stub.set(|script| {
        script.read_state = "completed".into();
        script.read_text = "genuine stub reply".into();
    });
    let ninth = "ninth-operation-0009";
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": "accepted after eight stale rows", "operation_id": ninth, "wake": true,
        }),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let operation = fx
            .store()
            .cloud_agent_operation(&id, ninth)
            .unwrap()
            .unwrap();
        if operation.status == "completed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the ninth accepted turn was starved by eight old rows: {operation:?}"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn intermediate_unknown_does_not_consume_the_final_report_id() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    fx.stub.set(|script| script.read_state = "unknown".into());
    let first = fx
        .call(
            Asserted::Operator,
            "cloud_agent_read",
            json!({"cloud_agent_id": id, "operation_id": OPERATION}),
        )
        .unwrap();
    assert_eq!(first["operation"]["status"], json!("unknown"));
    let rows_after_unknown = fx.store().thread_entries(ALIAS, 0, 100).unwrap();
    assert!(
        rows_after_unknown
            .iter()
            .all(|entry| entry.text != "genuine stub reply"),
        "an intermediate unknown wrote a final result"
    );

    fx.stub.set(|script| {
        script.read_state = "completed".into();
        script.read_text = "genuine stub reply".into();
    });
    let second = fx
        .call(
            Asserted::Operator,
            "cloud_agent_read",
            json!({"cloud_agent_id": id, "operation_id": OPERATION}),
        )
        .unwrap();
    assert_eq!(second["operation"]["status"], json!("completed"));
    let replies: Vec<_> = fx
        .store()
        .thread_entries(ALIAS, 0, 100)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.text == "genuine stub reply")
        .collect();
    assert_eq!(replies.len(), 1, "duplicate final result/report");
    assert_eq!(
        replies[0].message_id.as_deref(),
        Some(OPERATION),
        "the final report did not use the deterministic operation id"
    );
    assert_eq!(fx.stub.calls("submit"), 1);
}

#[test]
fn a_foreign_owner_cannot_read_the_settled_history() {
    use cadence_agent::cloud_agent::{
        self, CloudTransport, RemoteAgent, RemoteOperation, RemoteStatus, Source, SubmitRequest,
    };
    use cadence_agent::remote_cli::RemoteTarget;
    use cadence_agent::store::Owner;

    /// A transport that WOULD answer with a genuine settled reply and counts
    /// every call. A foreign-owner refusal must come from the admission
    /// decision itself, so a deleted source check cannot hide behind a
    /// transport that also refuses: this one refuses nothing, and the only
    /// way the read is stopped is the source/owner comparison.
    struct WouldAnswer(Arc<AtomicUsize>);
    impl CloudTransport for WouldAnswer {
        fn connect(&self, _: &RemoteTarget, _: &str) -> cadence_agent::Result<RemoteAgent> {
            Err(cadence_agent::Error::rejected("connect unused"))
        }
        fn submit(
            &self,
            _: &RemoteTarget,
            _: &SubmitRequest<'_>,
        ) -> cadence_agent::Result<RemoteOperation> {
            Err(cadence_agent::Error::rejected("submit unused"))
        }
        fn read(
            &self,
            _: &RemoteTarget,
            _: &str,
            operation: &str,
            _: &Source,
        ) -> cadence_agent::Result<RemoteOperation> {
            self.0.fetch_add(1, SeqCst);
            Ok(RemoteOperation {
                status: RemoteStatus::Completed,
                remote_operation_id: Some(operation.to_string()),
                remote_session_id: None,
                provider_session_id: None,
                reply: Some("genuine stub reply".into()),
                error: None,
                invoker_id: Some("inv_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".into()),
            })
        }
        fn cancel(
            &self,
            _: &RemoteTarget,
            _: &str,
            _: &str,
            _: &Source,
        ) -> cadence_agent::Result<()> {
            Err(cadence_agent::Error::rejected("cancel unused"))
        }
    }

    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    fx.call(
        Asserted::Operator,
        "cloud_agent_read",
        json!({"cloud_agent_id": id, "operation_id": OPERATION}),
    )
    .unwrap();

    let store = fx.store();
    let agent = store.cloud_agent(&id).unwrap().expect("cloud agent");
    let target = RemoteTarget {
        org: agent.org_slug.clone(),
        endpoint: agent.endpoint.clone(),
        org_id: agent.organization_id.clone(),
        identity: None,
    };
    let foreign = Source::operator(Owner::new(
        "https://other-issuer.example.test",
        "other-subject",
    ));
    // Nothing but the source/owner comparison may stop this: the transport
    // answers and is counted, and the platform is never reached.
    let foreign_reads = Arc::new(AtomicUsize::new(0));
    let readbacks_before_foreign = fx.stub.calls("readback");
    let foreign_error = cloud_agent::read_operation(
        &store,
        &WouldAnswer(Arc::clone(&foreign_reads)),
        &agent,
        &target,
        OPERATION,
        &foreign,
        || Ok(()),
    )
    .unwrap_err();
    assert_eq!(
        code(&foreign_error),
        Some("cloud_agent_source_mismatch"),
        "a foreign owner was stopped by something other than the source check: {foreign_error}"
    );
    assert_eq!(
        foreign_reads.load(SeqCst),
        0,
        "a foreign owner reached the transport"
    );
    assert_eq!(
        fx.stub.calls("readback"),
        readbacks_before_foreign,
        "a foreign owner reached the upstream readback"
    );
    assert!(
        !foreign_error.to_string().contains("genuine stub reply"),
        "the foreign refusal carried the cached reply: {foreign_error}"
    );

    // An admitted settled read is only served after the platform's own
    // readback authorizes it; the reply is the one that readback returned.
    fx.stub
        .set(|script| script.read_text = "fresh readback reply".into());
    let readbacks_before_admitted = fx.stub.calls("readback");
    let admitted = fx
        .call(
            Asserted::Operator,
            "cloud_agent_read",
            json!({"cloud_agent_id": id, "operation_id": OPERATION}),
        )
        .expect("the original owner must read its settled history");
    assert_eq!(
        fx.stub.calls("readback"),
        readbacks_before_admitted + 1,
        "the settled read was served without an upstream readback"
    );
    assert_eq!(
        admitted["operation"]["reply"]["text"],
        json!("fresh readback reply")
    );
}

#[test]
fn revoked_upstream_read_refuses_a_locally_settled_reply() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    fx.call(
        Asserted::Operator,
        "cloud_agent_read",
        json!({"cloud_agent_id": id, "operation_id": OPERATION}),
    )
    .unwrap();

    // Revoke READ at the contract/upstream only; the local saved credential
    // and the settled local row are untouched.
    fx.stub.set(|script| script.read_status = 403);
    let before = fx.stub.calls("readback");
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_read",
            json!({"cloud_agent_id": id, "operation_id": OPERATION}),
        )
        .unwrap_err();
    assert_eq!(code(&error), Some("cloud_agent_scope_revoked"), "{error}");
    assert_eq!(
        fx.stub.calls("readback"),
        before + 1,
        "the revoked read was served from the local cache"
    );
    assert!(
        !error.to_string().contains("genuine stub reply"),
        "the revoked read returned the reply: {error}"
    );

    // Positive control: the same fixture with read authority restored still
    // yields the settled reply.
    fx.stub.set(|script| script.read_status = 0);
    let admitted = fx
        .call(
            Asserted::Operator,
            "cloud_agent_read",
            json!({"cloud_agent_id": id, "operation_id": OPERATION}),
        )
        .unwrap();
    assert_eq!(admitted["operation"]["status"], json!("completed"));
    assert_eq!(
        admitted["operation"]["reply"]["text"],
        json!("genuine stub reply")
    );
}

#[test]
fn an_expired_enrolled_peer_record_is_refused_locally_before_transport() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "agent_register",
        json!({"alias": "peer", "provider": "fake", "endpoint_kind": "fake", "cwd": fx.cwd()}),
    )
    .unwrap();
    let store = fx.store();
    let peer = store.agent("peer").unwrap();
    let cloud = store.cloud_agent(&id).unwrap().expect("cloud agent");
    let target = cadence_agent::remote_cli::RemoteTarget {
        org: cloud.org_slug.clone(),
        endpoint: cloud.endpoint.clone(),
        org_id: cloud.organization_id.clone(),
        identity: None,
    };
    let owner = cadence_agent::store::Owner::new("https://issuer.example.test", "stub-subject");
    let stamp = cadence_agent::cloud_agent::registration_epoch(peer.created);
    let expired = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 60;
    cadence_agent::remote_cli::cloud_agent_save_peer(
        &target,
        &owner,
        &peer.uid,
        stamp,
        &json!({
            "agent_id": "hca_native_guard_peer",
            "credential_id": "hcp_native_guard_peer",
            "token": format!("hct_{}", "p".repeat(43)),
            "expires_at": expired,
            "role": "cloud_agent",
            "sender": {"uid": peer.uid, "registration_epoch": stamp},
        }),
    )
    .unwrap();
    let before = fx.stub.calls("readback");
    let error = fx
        .call(
            Asserted::Agent("peer".into()),
            "cloud_agent_read",
            json!({"cloud_agent_id": id, "operation_id": OPERATION}),
        )
        .unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("expire"),
        "an expired peer record was not refused by expiry: {error}"
    );
    assert_eq!(
        fx.stub.calls("readback"),
        before,
        "an expired peer reached the upstream readback"
    );
    let _ = home;
}

#[test]
fn generic_readers_do_not_expose_a_cloud_reply_to_an_unenrolled_peer() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    fx.call(
        Asserted::Operator,
        "cloud_agent_read",
        json!({"cloud_agent_id": id, "operation_id": OPERATION}),
    )
    .unwrap();
    fx.call(
        Asserted::Operator,
        "agent_register",
        json!({"alias": "peer", "provider": "fake", "endpoint_kind": "fake", "cwd": fx.cwd()}),
    )
    .unwrap();

    // The peer has no cloud enrollment. Refusal or redaction is accepted, but
    // the reply must never cross these generic projections.
    for (method, params) in [
        ("thread_read", json!({"alias": ALIAS})),
        ("agent_show", json!({"alias": ALIAS})),
        ("agent_events", json!({"alias": ALIAS, "after": 0})),
    ] {
        if let Ok(value) = fx.call(Asserted::Agent("peer".into()), method, params) {
            assert!(
                !value.to_string().contains("genuine stub reply"),
                "{method} exposed the cloud reply to an unenrolled peer: {value}"
            );
        }
    }

    // The operator still sees the reply; the UID-bound peer report positive
    // remains the dedicated admitted-peer component case.
    let operator = fx
        .call(Asserted::Operator, "thread_read", json!({"alias": ALIAS}))
        .unwrap();
    assert!(operator.to_string().contains("genuine stub reply"));
}

#[test]
fn regrant_does_not_revive_queued_work_and_renewal_continues() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": false,
        }),
    )
    .unwrap();
    fx.call(
        Asserted::Operator,
        "cloud_agent_disconnect",
        json!({"cloud_agent_id": id}),
    )
    .unwrap();
    fx.connect();
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_send",
            json!({
                "cloud_agent_id": id, "thread_id": home,
                "text": TEXT, "operation_id": OPERATION, "wake": true,
            }),
        )
        .unwrap_err();
    assert!(
        !error.to_string().is_empty(),
        "a regrant revived queued work"
    );
    assert_eq!(fx.stub.calls("submit"), 0, "a regrant submitted old work");
    let queued = fx
        .store()
        .cloud_agent_operation(&id, OPERATION)
        .unwrap()
        .unwrap();
    assert_ne!(
        queued.status, "completed",
        "old queued work completed after regrant"
    );

    // Positive control: a fresh operation after the regrant submits once.
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": "fresh after regrant", "operation_id": "regrant-fresh-0001", "wake": true,
        }),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let operation = fx
            .store()
            .cloud_agent_operation(&id, "regrant-fresh-0001")
            .unwrap()
            .unwrap();
        if operation.status == "completed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "fresh after regrant did not submit: {operation:?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(fx.stub.calls("submit"), 1);

    // Ordinary credential renewal (same subject/issuer, no revocation) keeps
    // its own later queued work admissible.
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": "renewal queued", "operation_id": "renewal-queued-0001", "wake": false,
        }),
    )
    .unwrap();
    write_login_owner(LoginSpec {
        org: ORG,
        org_id: ORG_ID,
        endpoint: &fx.stub.url,
        capabilities: &["cloud_agent.read", "cloud_agent.write"],
        expires: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 600,
        subject: "stub-subject",
        issuer: "https://issuer.example.test",
        token_char: "r",
    });
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": "renewal queued", "operation_id": "renewal-queued-0001", "wake": true,
        }),
    )
    .expect("ordinary credential renewal must let its own queued work proceed");
    assert_eq!(fx.stub.calls("submit"), 2);
}

#[test]
fn canonical_enqueue_refuses_a_replaced_alias_source_without_transport_or_delivery() {
    use cadence_agent::cloud_agent::{
        self, CloudTransport, RemoteAgent, RemoteOperation, Sender, Source, SubmitRequest,
    };
    use cadence_agent::store::Owner;

    struct NoTransport(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl CloudTransport for NoTransport {
        fn connect(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &str,
        ) -> cadence_agent::Result<RemoteAgent> {
            Err(cadence_agent::Error::rejected(
                "no transport in this bad case",
            ))
        }
        fn submit(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &SubmitRequest<'_>,
        ) -> cadence_agent::Result<RemoteOperation> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(cadence_agent::Error::rejected(
                "no transport in this bad case",
            ))
        }
        fn read(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &str,
            _: &str,
            _: &Source,
        ) -> cadence_agent::Result<RemoteOperation> {
            Err(cadence_agent::Error::rejected(
                "no transport in this bad case",
            ))
        }
        fn cancel(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &str,
            _: &str,
            _: &Source,
        ) -> cadence_agent::Result<()> {
            Err(cadence_agent::Error::rejected(
                "no transport in this bad case",
            ))
        }
    }

    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    let store = fx.store();
    let agent = store.cloud_agent(&id).unwrap().expect("cloud agent");
    let target = cadence_agent::remote_cli::RemoteTarget {
        org: agent.org_slug.clone(),
        endpoint: agent.endpoint.clone(),
        org_id: agent.organization_id.clone(),
        identity: None,
    };
    // The operation was verified for an older registered source; the alias now
    // resolves to a different UID/registration. The canonical enqueue must
    // compare the immutable source, not the alias it re-resolves.
    let stale = Source {
        kind: "local_peer",
        uid: "stale-peer-uid".into(),
        epoch: 1,
        owner: Owner::new("https://issuer.example.test", "stub-subject"),
    };
    let transport_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let transport = NoTransport(std::sync::Arc::clone(&transport_calls));
    let error = cloud_agent::send(
        &store,
        &transport,
        &agent,
        &target,
        &home,
        &Sender::Agent("peer".into()),
        &stale,
        "operation-00000011",
        "must not be delivered to a replaced alias",
        true,
        None,
        || Ok(()),
    )
    .unwrap_err();
    assert!(
        !error.to_string().is_empty(),
        "a replaced alias source was not refused"
    );
    assert_eq!(
        transport_calls.load(Ordering::SeqCst),
        0,
        "transport was invoked"
    );
    let rows = store.thread_entries(ALIAS, 0, 100).unwrap();
    assert!(
        rows.iter()
            .all(|entry| entry.text != "must not be delivered to a replaced alias"),
        "a request was attributed/delivered for the replaced alias"
    );
}

#[test]
fn restart_keeps_the_cloud_connection_actorless_and_truthful() {
    let mut fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    fx.restart();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let agent = fx.store().cloud_agent(&id).unwrap().expect("cloud agent");
        if agent.state == "connected" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "cloud agent state after restart: {agent:?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let agent = fx.store().agent(ALIAS).unwrap();
    assert_eq!(agent.provider, "agenticos-cloud");
}

#[test]
fn crash_before_submit_returns_reconciles_the_persisted_intent_without_resubmit() {
    let mut fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    // The HTTP stub holds the submit response open so the daemon is stopped
    // while the durable submit intent exists (running, no remote id) but no
    // native send return has happened.
    fx.stub.set(|script| script.submit_delay_ms = 4_000);
    let state = fx.state.clone();
    let sender = {
        let state = state.clone();
        let id = id.clone();
        let home = home.clone();
        thread::spawn(move || {
            client::rpc(
                &state,
                "cloud_agent_send",
                json!({
                    "cloud_agent_id": id, "thread_id": home,
                    "text": TEXT, "operation_id": OPERATION, "wake": true,
                }),
            )
        })
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let operation = fx.store().cloud_agent_operation(&id, OPERATION).unwrap();
        if operation
            .as_ref()
            .is_some_and(|op| op.status == "running" && op.remote_operation_id.is_none())
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "submit intent was not persisted before HTTP: {operation:?}"
        );
        thread::sleep(Duration::from_millis(20));
    }
    fx.restart();
    let _ = sender.join().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let operation = fx
            .store()
            .cloud_agent_operation(&id, OPERATION)
            .unwrap()
            .unwrap();
        if operation.status == "completed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the persisted submit intent was not reconciled by readback: {operation:?}"
        );
        thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(fx.stub.calls("submit"), 1, "the intent was resubmitted");
    assert_eq!(fx.stub.calls("readback"), 1, "the intent was not read back");
}

#[test]
fn a_deadline_exhausting_head_does_not_starve_the_next_accepted_turn() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    let store = fx.store();
    // Template row from one real settled turn.
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": "operation-00000010", "wake": true,
        }),
    )
    .unwrap();
    fx.call(
        Asserted::Operator,
        "cloud_agent_read",
        json!({"cloud_agent_id": id, "operation_id": "operation-00000010"}),
    )
    .unwrap();
    let template = store
        .cloud_agent_operation(&id, "operation-00000010")
        .unwrap()
        .expect("template operation");
    // Exactly eight pending heads on distinct sessions; the FIRST is
    // authoritative but its HTTP read consumes the tick's wall deadline.
    for index in 1..=8 {
        let mut draft = template.clone();
        draft.operation_id = format!("deadline-operation-{index:04}");
        draft.conversation_id = format!("deadline-conversation-{index:04}");
        draft.remote_session_id = format!("ns_deadline_{index:04}");
        draft.source_uid = "stub-subject".into();
        draft.registration_epoch = 0;
        draft.remote_operation_id = None;
        draft.message_id = None;
        draft.reply_text = None;
        draft.reply_message_id = None;
        store.cloud_agent_operation_begin(&draft).unwrap();
        store
            .cloud_agent_operation_set(
                &id,
                &draft.operation_id,
                "unknown",
                Some(&format!("remote-deadline-{index:04}")),
                None,
                None,
                None,
            )
            .unwrap();
    }
    fx.stub.set(|script| {
        script.slow_read_operation = Some("remote-deadline-0001".into());
        script.slow_read_ms = 25_000;
        script.read_state = "completed".into();
        script.read_text = "genuine stub reply".into();
    });
    // The ninth turn is accepted on its own session while the head is unknown.
    let ninth = "deadline-accepted-0009";
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": "accepted after a deadline-exhausting head", "operation_id": ninth, "wake": true,
        }),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let operation = fx
            .store()
            .cloud_agent_operation(&id, ninth)
            .unwrap()
            .unwrap();
        if operation.status == "completed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the deadline-exhausting head starved the next accepted turn: {operation:?}"
        );
        thread::sleep(Duration::from_millis(200));
    }
    let head = fx
        .store()
        .cloud_agent_operation(&id, "deadline-operation-0001")
        .unwrap()
        .unwrap();
    assert_ne!(
        head.status, "completed",
        "the slow head unexpectedly settled"
    );
}

#[test]
fn user_cli_cloud_agent_connect_uses_workspace_and_creates_one_real_connection() {
    let mut fx = Fx::start();
    // The real CLI discovers a saved remote login through the owned org
    // registry, the same file `cadence org list` reads.
    let registry = fx.root.path().join("config/cadence/orgs.json");
    std::fs::write(
        &registry,
        json!({
            "connections": [{
                "selection": {"org": ORG},
                "destination": {"mode": "remote", "endpoint": fx.stub.url, "org_id": ORG_ID},
            }],
        })
        .to_string(),
    )
    .unwrap();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    command
        .arg("--state-dir")
        .arg(&fx.state)
        .args([
            "cloud-agent",
            "connect",
            "--workspace",
            ORG,
            "--alias",
            "cloud-qa",
            "--accept-assistant-consent",
        ])
        .env("CADENCE_TEST_SEAM", "1")
        .env("CADENCE_TEST_AS", "operator");
    let output =
        cadence_agent::reaper::output(&mut command).expect("run cadence cloud-agent connect");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "CLI cloud-agent connect failed: {status:?}\nstdout: {stdout}\nstderr: {stderr}",
        status = output.status
    );
    assert!(
        !stdout.contains("hct_") && !stderr.contains("hct_"),
        "the CLI exported the hosted credential: {stdout}\n{stderr}"
    );
    assert_eq!(
        fx.stub.calls("agent"),
        1,
        "the CLI did not create one connection"
    );
    assert_eq!(fx.stub.calls("submit"), 0, "the CLI invoked a model turn");

    let listed = fx
        .call(Asserted::Operator, "cloud_agent_list", json!({}))
        .unwrap();
    let agent = listed["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .find(|agent| agent["alias"] == json!("cloud-qa"))
        .expect("cloud-qa connection");
    assert_eq!(agent["kind"], json!("cloud"));
    assert_eq!(agent["state"], json!("connected"));
    assert_eq!(agent["organization_id"], json!(ORG_ID));
    assert_eq!(
        agent["remote_agent_id"],
        json!("nca_stubagent000000000000000000000000")
    );
    assert_eq!(agent["provenance"]["kind"], json!("user"));
    assert!(agent["thread"]["id"].is_string());
    let link = agent["thread"]["link"].as_str().expect("thread link");
    let conversation = link
        .strip_prefix("/threads?c=")
        .expect("operator-agent history must use the work-conversation selector");
    let listed = fx
        .call(
            Asserted::Operator,
            "work_conversation_list",
            json!({"include_system": true}),
        )
        .unwrap();
    assert!(
        listed["conversations"]
            .as_array()
            .expect("work conversations")
            .iter()
            .any(|row| row["id"] == json!(conversation)),
        "the linked conversation is not in the real work-conversation list: {listed}"
    );
    // `cloud_qa` has no turn yet, so the metadata link must at least be a
    // real listed work conversation, and no model call has happened.
    assert_eq!(fx.stub.calls("submit"), 0);
    let _ = conversation;

    // The actual board consumer payload: a cloud connection is a normal idle
    // worker, never a mailbox, and the header totals must not claim an inbox.
    fx.start_board();
    let (status, payload) = fx.get("operator", "/api/agents", true);
    assert_eq!(status, 200, "{payload}");
    let row = payload["agents"]
        .as_array()
        .expect("agents payload")
        .iter()
        .find(|row| row["alias"] == json!("cloud-qa"))
        .expect("cloud-qa row");
    assert_eq!(
        row["state"],
        json!("idle"),
        "cloud misprojected as a mailbox: {row}"
    );
    assert_ne!(
        row["inbox"],
        json!(true),
        "cloud row marked as an inbox: {row}"
    );
    assert_eq!(payload["totals"]["inboxes"], json!(0), "{payload}");
    assert_eq!(row["endpoint_kind"], json!("cloud"));
    assert_eq!(row["provider"], json!("agenticos-cloud"));
    if let Some(location) = row.get("location") {
        assert_eq!(location["kind"], json!("cloud"), "{row}");
    }

    // After the FIRST EXPLICIT turn and completion, the operator metadata link
    // must resolve through the real work-conversation list and read.
    let listed = fx
        .call(Asserted::Operator, "cloud_agent_list", json!({}))
        .unwrap();
    let cloud_id = listed["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .find(|entry| entry["alias"] == json!("cloud-qa"))
        .and_then(|entry| entry["id"].as_str())
        .expect("cloud-qa id")
        .to_string();
    let thread = listed["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["alias"] == json!("cloud-qa"))
        .and_then(|entry| entry["thread"]["id"].as_str())
        .expect("cloud-qa thread")
        .to_string();
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": cloud_id, "thread_id": thread,
            "text": "cli explicit turn", "operation_id": "cli-turn-00000001", "wake": true,
        }),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let operation = fx
            .store()
            .cloud_agent_operation(&cloud_id, "cli-turn-00000001")
            .unwrap()
            .unwrap();
        if operation.status == "completed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "cli turn did not complete: {operation:?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let shown = fx
        .call(
            Asserted::Operator,
            "cloud_agent_show",
            json!({"cloud_agent_id": cloud_id}),
        )
        .unwrap();
    let link = shown["agent"]["thread"]["link"]
        .as_str()
        .expect("thread link");
    let conversation = link
        .strip_prefix("/threads?c=")
        .expect("operator history must use the work-conversation selector");
    // Operator-agent home threads use the existing `?c=` thread selector.
    let read_back = fx
        .call(
            Asserted::Operator,
            "thread_read",
            json!({"alias": "cloud-qa", "conversation": conversation}),
        )
        .unwrap();
    let bodies: Vec<&str> = read_back["entries"]
        .as_array()
        .expect("thread entries")
        .iter()
        .filter_map(|entry| entry["text"].as_str())
        .collect();
    assert!(bodies.contains(&"cli explicit turn"), "{bodies:?}");
    assert!(bodies.contains(&"genuine stub reply"), "{bodies:?}");
}

#[test]
fn cancel_is_explicitly_unsupported_and_never_reports_a_stopped_turn() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let thread = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": thread,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_cancel",
            json!({"cloud_agent_id": id, "operation_id": OPERATION}),
        )
        .unwrap_err();
    assert_eq!(
        code(&error),
        Some("cloud_agent_cancel_unsupported"),
        "{error}"
    );
    let operation = fx
        .store()
        .cloud_agent_operation(&id, OPERATION)
        .unwrap()
        .unwrap();
    assert_eq!(operation.status, "queued");
    assert!(!matches!(
        operation.status.as_str(),
        "cancelled" | "stopped"
    ));
    assert_eq!(fx.stub.calls("submit"), 1);
}

#[test]
fn a_peer_cannot_connect_or_use_the_operators_connection() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let thread = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "agent_register",
        json!({
            "alias": "peer", "provider": "fake",
            "endpoint_kind": "fake", "cwd": fx.cwd(),
        }),
    )
    .unwrap();

    let error = scoped(Asserted::Agent("peer".into()), || {
        client::rpc(
            &fx.state,
            "cloud_agent_connect",
            json!({
                "org": ORG, "org_id": ORG_ID, "endpoint": fx.stub.url,
                "alias": ALIAS_B, "accept_assistant_consent": true,
            }),
        )
    })
    .unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("operator"),
        "peer was not stopped by the operator gate: {error}"
    );
    assert_eq!(fx.stub.calls("agent"), 1);

    let error = scoped(Asserted::Agent("peer".into()), || {
        client::rpc(
            &fx.state,
            "cloud_agent_send",
            json!({
                "cloud_agent_id": id, "thread_id": thread,
                "text": TEXT, "operation_id": OPERATION, "wake": true,
            }),
        )
    })
    .unwrap_err();
    assert!(
        matches!(
            code(&error),
            Some("cloud_agent_peer_isolation_required")
                | Some("cloud_agent_owner_mismatch")
                | Some("cloud_agent_consent_required")
        ) || error
            .to_string()
            .contains("no enrolled cloud-peer credential"),
        "peer was not refused by the cloud source gate: {error}"
    );
    assert_eq!(fx.stub.calls("submit"), 0);

    let error = scoped(Asserted::Unproven, || {
        client::rpc(
            &fx.state,
            "cloud_agent_send",
            json!({
                "cloud_agent_id": id, "thread_id": thread,
                "text": TEXT, "operation_id": OPERATION, "wake": true,
            }),
        )
    })
    .unwrap_err();
    assert!(
        error.to_string().contains("derives no agent identity"),
        "unproven caller not refused by the caller rule: {error}"
    );
    assert_eq!(fx.stub.calls("submit"), 0);
}

#[test]
fn the_board_http_peer_refuses_a_peer_forged_or_unproven_caller() {
    let mut fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let thread = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "agent_register",
        json!({
            "alias": "peer", "provider": "fake",
            "endpoint_kind": "fake", "cwd": fx.cwd(),
        }),
    )
    .unwrap();
    fx.start_board();
    let route = format!("/api/cloud-agents/{id}/messages");
    let body = json!({"thread_id": thread, "operation_id": OPERATION, "text": TEXT});

    let (status, _) = fx.post("unproven", &route, body.clone(), false);
    assert!(
        status == 401 || status == 403,
        "unproven HTTP caller was not refused: {status}"
    );
    assert_eq!(fx.stub.calls("submit"), 0);

    let (status, refusal) = fx.post("agent:peer", &route, body.clone(), false);
    assert!(status == 401 || status == 403, "{status} {refusal}");
    assert!(
        refusal["check"] == json!("operator_only")
            || refusal["code"] == json!("operator_session_required"),
        "{refusal}"
    );
    assert_eq!(fx.stub.calls("submit"), 0);

    let mut forged = body.clone();
    forged["sender"] = json!("operator");
    let (status, _) = fx.post("operator", &route, forged, true);
    assert!(
        status == 400 || status == 401,
        "a forged sender field reached the daemon: {status}"
    );
    assert_eq!(fx.stub.calls("submit"), 0);

    let (status, refusal) = fx.get("agent:peer", "/api/cloud-agents", false);
    assert!(status == 401 || status == 403, "{status} {refusal}");
    assert_eq!(fx.stub.calls("submit"), 0);

    // The admitted operator path is the positive control over the same route.
    let (status, accepted) = fx.post("operator", &route, body, true);
    assert_eq!(status, 200, "{accepted}");
    assert_eq!(accepted["operation"]["operation_id"], json!(OPERATION));
    assert_eq!(fx.stub.calls("submit"), 1);
}

// ---------- round three: the claim, projection, readback and portable guards ----------

/// A submission claim is exclusive and permanent. Two identical wakes race;
/// the winner claims and settles, READ authority is then gone, and the loser
/// is still told only that the operation was claimed — never handed the
/// settled reply, never granted a readback and never allowed a second turn.
#[test]
fn a_lost_submit_claim_returns_the_conflict_without_content_or_a_second_submit() {
    use cadence_agent::cloud_agent::{
        self, Authority, CloudTransport, RemoteAgent, RemoteOperation, RemoteStatus, Sender,
        Source, SubmitRequest, SCOPE_READ, SCOPE_WRITE,
    };
    use cadence_agent::remote_cli::RemoteTarget;
    use cadence_agent::store::Owner;

    const WINNER: &str = "the reply the winner settled";

    /// Answers anything it is asked and counts it, so "the loser never sent"
    /// is a counted effect rather than an inference from an error.
    struct Counted {
        submits: Arc<AtomicUsize>,
        reads: Arc<AtomicUsize>,
        reply: &'static str,
    }
    impl CloudTransport for Counted {
        fn connect(&self, _: &RemoteTarget, _: &str) -> cadence_agent::Result<RemoteAgent> {
            Err(cadence_agent::Error::rejected("connect unused"))
        }
        fn submit(
            &self,
            _: &RemoteTarget,
            request: &SubmitRequest<'_>,
        ) -> cadence_agent::Result<RemoteOperation> {
            self.submits.fetch_add(1, SeqCst);
            Ok(RemoteOperation {
                status: RemoteStatus::Completed,
                remote_operation_id: Some(request.operation_id.to_string()),
                remote_session_id: None,
                provider_session_id: None,
                reply: Some(self.reply.to_string()),
                error: None,
                invoker_id: Some("inv_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".into()),
            })
        }
        fn read(
            &self,
            _: &RemoteTarget,
            _: &str,
            operation: &str,
            _: &Source,
        ) -> cadence_agent::Result<RemoteOperation> {
            self.reads.fetch_add(1, SeqCst);
            Ok(RemoteOperation {
                status: RemoteStatus::Completed,
                remote_operation_id: Some(operation.to_string()),
                remote_session_id: None,
                provider_session_id: None,
                reply: Some(self.reply.to_string()),
                error: None,
                invoker_id: Some("inv_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".into()),
            })
        }
        fn cancel(
            &self,
            _: &RemoteTarget,
            _: &str,
            _: &str,
            _: &Source,
        ) -> cadence_agent::Result<()> {
            Err(cadence_agent::Error::rejected("cancel unused"))
        }
    }

    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    // Queue without waking: the row is `queued` and has never been claimed.
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": false,
        }),
    )
    .unwrap();
    assert_eq!(fx.stub.calls("submit"), 0);
    let queued = fx
        .store()
        .cloud_agent_operation(&id, OPERATION)
        .unwrap()
        .expect("queued operation");
    assert_eq!(queued.status, "queued");
    assert_eq!(queued.claimed_at, 0.0);

    // READ scope is removed from the saved login; WRITE authority — the
    // submission gate — remains, exactly as a real partial revocation leaves
    // it.
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 600;
    write_login(ORG, ORG_ID, &fx.stub.url, &["cloud_agent.write"], expires);

    let store = fx.store();
    let cloud = store.cloud_agent(&id).unwrap().expect("cloud agent");
    let target = RemoteTarget {
        org: cloud.org_slug.clone(),
        endpoint: cloud.endpoint.clone(),
        org_id: cloud.organization_id.clone(),
        identity: None,
    };
    let source = Sender::Operator
        .source_for(&store, &target)
        .expect("the saved login still proves the operator's owner");
    assert_eq!(
        source.owner,
        Owner::new("https://issuer.example.test", "stub-subject")
    );

    let (parked_tx, parked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let submit_authority_calls = Arc::new(AtomicUsize::new(0));
    let read_authority_calls = Arc::new(AtomicUsize::new(0));
    let loser_submits = Arc::new(AtomicUsize::new(0));
    let loser_reads = Arc::new(AtomicUsize::new(0));
    let loser = {
        let state = fx.state.clone();
        let thread_id = home.clone();
        let (cloud, target, source) = (cloud.clone(), target.clone(), source.clone());
        let (submit_calls, read_calls) = (
            Arc::clone(&submit_authority_calls),
            Arc::clone(&read_authority_calls),
        );
        let submits = Arc::clone(&loser_submits);
        let reads = Arc::clone(&loser_reads);
        thread::spawn(move || {
            let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
            // The daemon's own two gates against the fixture's real
            // credential: submission authority passes, read authority fails.
            let submit = || {
                submit_calls.fetch_add(1, SeqCst);
                let proven = cadence_agent::remote_cli::cloud_agent_credential_owned(
                    &target,
                    SCOPE_WRITE,
                    Some(&source.owner),
                );
                parked_tx.send(()).expect("the winning caller never ran");
                // The winner claims and settles while this caller is parked
                // exactly here: past authorization, before the claim.
                let _ = release_rx.recv_timeout(Duration::from_secs(30));
                proven
            };
            let read = || {
                read_calls.fetch_add(1, SeqCst);
                cadence_agent::remote_cli::cloud_agent_credential_owned(
                    &target,
                    SCOPE_READ,
                    Some(&source.owner),
                )
            };
            cloud_agent::send_authorized(
                &store,
                &Counted {
                    submits,
                    reads,
                    reply: "the loser never sends",
                },
                &cloud,
                &target,
                cloud_agent::SendRequest {
                    thread_id: &thread_id,
                    sender: &Sender::Operator,
                    source: &source,
                    operation_id: OPERATION,
                    text: TEXT,
                    wake: true,
                    expected_recipient_uid: None,
                },
                &Authority {
                    submit: &submit,
                    read: &read,
                },
            )
        })
    };
    parked_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the losing caller never reached its submission authority");
    // The winner is the daemon's own send route under the same write-only
    // login: it claims the queued row and settles it in one accepted turn.
    fx.stub.set(|script| {
        script.submit_state = "completed".into();
        script.submit_text = Some(WINNER.to_string());
    });
    let won = fx
        .call(
            Asserted::Operator,
            "cloud_agent_send",
            json!({
                "cloud_agent_id": id, "thread_id": home,
                "text": TEXT, "operation_id": OPERATION, "wake": true,
            }),
        )
        .expect("the winner must claim and submit the queued request");
    assert_eq!(won["operation"]["status"], json!("completed"));
    assert_eq!(won["operation"]["reply"]["text"], json!(WINNER));
    assert_eq!(fx.stub.calls("submit"), 1, "the winner did not submit once");
    assert_eq!(
        fx.stub.calls("readback"),
        0,
        "the winner needed no readback"
    );
    let settled = fx
        .store()
        .cloud_agent_operation(&id, OPERATION)
        .unwrap()
        .expect("settled operation");
    assert_eq!(settled.status, "completed");
    assert!(
        settled.claimed_at > 0.0,
        "the winning claim was not recorded"
    );
    assert_eq!(settled.reply_text.as_deref(), Some(WINNER));

    let _ = release_tx.send(());
    let error = loser
        .join()
        .expect("the losing caller panicked")
        .expect_err("the loser must not be handed the settled operation");
    assert_eq!(
        code(&error),
        Some("cloud_agent_operation_claimed"),
        "{error}"
    );
    assert_eq!(error.kind(), "conflict", "{error}");
    assert!(
        !error.to_string().contains(WINNER),
        "the losing claim returned the settled reply: {error}"
    );
    assert_eq!(
        submit_authority_calls.load(SeqCst),
        1,
        "the losing branch never proved submission authority"
    );
    assert_eq!(
        read_authority_calls.load(SeqCst),
        0,
        "the losing branch consulted read authority"
    );
    assert_eq!(
        loser_submits.load(SeqCst),
        0,
        "the loser crossed the submission boundary"
    );
    assert_eq!(
        loser_reads.load(SeqCst),
        0,
        "the loser performed a readback"
    );
    assert_eq!(
        fx.stub.calls("submit"),
        1,
        "a second model turn was submitted"
    );
    assert_eq!(
        fx.stub.calls("readback"),
        0,
        "the losing claim performed a readback"
    );
    let texts: Vec<String> = fx
        .store()
        .thread_entries(ALIAS, 0, 100)
        .unwrap()
        .into_iter()
        .map(|entry| entry.text)
        .collect();
    assert_eq!(
        texts.iter().filter(|text| *text == WINNER).count(),
        1,
        "{texts:?}"
    );
    assert_eq!(
        texts.iter().filter(|text| *text == TEXT).count(),
        1,
        "{texts:?}"
    );
}

/// The claim is durable eligibility, not just `status='queued'`: an accepted
/// queued answer that names no remote operation id leaves the row `queued`,
/// and a delayed identical wake still may not submit it. Restart recovery
/// remains readback-only for that row.
#[test]
fn an_accepted_queued_claim_cannot_re_fire_and_restart_reconciles_by_readback() {
    use cadence_agent::cloud_agent::{
        self, Authority, CloudTransport, RemoteAgent, RemoteOperation, RemoteStatus, Sender,
        Source, SubmitRequest,
    };
    use cadence_agent::remote_cli::RemoteTarget;

    /// The contract's accepted-queued answer: the turn was accepted, but no
    /// remote operation id came back. Every call is counted.
    struct AcceptedQueued(Arc<AtomicUsize>);
    impl CloudTransport for AcceptedQueued {
        fn connect(&self, _: &RemoteTarget, _: &str) -> cadence_agent::Result<RemoteAgent> {
            Err(cadence_agent::Error::rejected("connect unused"))
        }
        fn submit(
            &self,
            _: &RemoteTarget,
            _: &SubmitRequest<'_>,
        ) -> cadence_agent::Result<RemoteOperation> {
            self.0.fetch_add(1, SeqCst);
            Ok(RemoteOperation {
                status: RemoteStatus::Queued,
                remote_operation_id: None,
                remote_session_id: None,
                provider_session_id: None,
                reply: None,
                error: None,
                invoker_id: Some("inv_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".into()),
            })
        }
        fn read(
            &self,
            _: &RemoteTarget,
            _: &str,
            _: &str,
            _: &Source,
        ) -> cadence_agent::Result<RemoteOperation> {
            Err(cadence_agent::Error::rejected(
                "this fake never reads back; the daemon owns recovery",
            ))
        }
        fn cancel(
            &self,
            _: &RemoteTarget,
            _: &str,
            _: &str,
            _: &Source,
        ) -> cadence_agent::Result<()> {
            Err(cadence_agent::Error::rejected("cancel unused"))
        }
    }

    let mut fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    // The daemon is stopped so nothing but this case can touch the row
    // before the deliberate delayed wake.
    fx.stop_daemon();
    let store = fx.store();
    let cloud = store.cloud_agent(&id).unwrap().expect("cloud agent");
    let target = RemoteTarget {
        org: cloud.org_slug.clone(),
        endpoint: cloud.endpoint.clone(),
        org_id: cloud.organization_id.clone(),
        identity: None,
    };
    let source = Sender::Operator
        .source_for(&store, &target)
        .expect("operator source");
    let submits = Arc::new(AtomicUsize::new(0));
    cloud_agent::send(
        &store,
        &AcceptedQueued(Arc::clone(&submits)),
        &cloud,
        &target,
        &home,
        &Sender::Operator,
        &source,
        OPERATION,
        TEXT,
        true,
        None,
        || Ok(()),
    )
    .expect("an accepted-queued answer is a success");
    assert_eq!(submits.load(SeqCst), 1, "the first wake did not submit");
    let claimed = store
        .cloud_agent_operation(&id, OPERATION)
        .unwrap()
        .expect("operation");
    assert_eq!(
        claimed.status, "queued",
        "an accepted-queued answer must stay nonterminal"
    );
    assert_eq!(claimed.remote_operation_id, None);
    assert!(
        claimed.claimed_at > 0.0,
        "the accepted-queued submit recorded no claim"
    );
    assert!(claimed.reply_text.is_none());

    // A delayed identical wake: eligibility is durable, so no second submit.
    let second = Arc::new(AtomicUsize::new(0));
    let allow = || -> cadence_agent::Result<()> { Ok(()) };
    let error = cloud_agent::send_authorized(
        &store,
        &AcceptedQueued(Arc::clone(&second)),
        &cloud,
        &target,
        cloud_agent::SendRequest {
            thread_id: &home,
            sender: &Sender::Operator,
            source: &source,
            operation_id: OPERATION,
            text: TEXT,
            wake: true,
            expected_recipient_uid: None,
        },
        &Authority {
            submit: &allow,
            read: &allow,
        },
    )
    .unwrap_err();
    assert_eq!(
        code(&error),
        Some("cloud_agent_operation_claimed"),
        "{error}"
    );
    assert_eq!(
        second.load(SeqCst),
        0,
        "an already-claimed turn was submitted again"
    );
    let after = store
        .cloud_agent_operation(&id, OPERATION)
        .unwrap()
        .expect("operation");
    assert_eq!(after.status, claimed.status);
    assert_eq!(after.claimed_at, claimed.claimed_at);
    assert!(after.reply_text.is_none());
    assert_eq!(
        fx.stub.calls("submit"),
        0,
        "a fake claim reached the real model boundary"
    );

    // Restart recovery must reconcile the claimed-queued row by READBACK and
    // never by submitting it again.
    fx.restart();
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let current = fx
            .store()
            .cloud_agent_operation(&id, OPERATION)
            .unwrap()
            .expect("operation");
        if current.status == "completed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "restart never reconciled the claimed queued row: {current:?}"
        );
        thread::sleep(Duration::from_millis(100));
    }
    let settled = fx
        .store()
        .cloud_agent_operation(&id, OPERATION)
        .unwrap()
        .expect("operation");
    assert_eq!(settled.reply_text.as_deref(), Some("genuine stub reply"));
    assert_eq!(
        fx.stub.calls("readback"),
        1,
        "restart recovery did not read the row back"
    );
    assert_eq!(
        fx.stub.calls("submit"),
        0,
        "restart recovery resubmitted the claimed turn"
    );
    let replies = fx
        .store()
        .thread_entries(ALIAS, 0, 100)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.text == "genuine stub reply")
        .count();
    assert_eq!(replies, 1, "restart recovery recorded {replies} results");
}

/// Every generic projection of a cloud alias demands CURRENT read authority,
/// not bare connection membership: a write-only operator gets no stored reply
/// from `thread_read`, `agent_show` or `agent_events`.
#[test]
fn a_write_only_operator_cannot_read_cloud_content_through_generic_projections() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    fx.call(
        Asserted::Operator,
        "cloud_agent_read",
        json!({"cloud_agent_id": id, "operation_id": OPERATION}),
    )
    .unwrap();
    assert_eq!(fx.stub.calls("readback"), 1);

    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 600;
    write_login(ORG, ORG_ID, &fx.stub.url, &["cloud_agent.write"], expires);
    for (method, params) in [
        ("thread_read", json!({"alias": ALIAS})),
        ("agent_show", json!({"alias": ALIAS})),
        ("agent_events", json!({"alias": ALIAS, "after": 0})),
    ] {
        let error = fx
            .call(Asserted::Operator, method, params.clone())
            .unwrap_err();
        assert_eq!(
            code(&error),
            Some("cloud_agent_scope_revoked"),
            "{method}: {error}"
        );
        assert!(
            !error.to_string().contains("genuine stub reply"),
            "{method} disclosed the reply: {error}"
        );
    }

    // Positive control: the same three projections serve the durable reply
    // again once the saved login carries read authority.
    write_login(
        ORG,
        ORG_ID,
        &fx.stub.url,
        &["cloud_agent.read", "cloud_agent.write"],
        expires,
    );
    for (method, params) in [
        ("thread_read", json!({"alias": ALIAS})),
        ("agent_show", json!({"alias": ALIAS})),
        ("agent_events", json!({"alias": ALIAS, "after": 0})),
    ] {
        let value = fx
            .call(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("{method} refused the owner's restored read: {e}"));
        assert!(
            value.to_string().contains("genuine stub reply"),
            "{method} did not serve the reply: {value}"
        );
    }
}

/// The projection binds each row to the source its operation was admitted
/// for: another source's request and reply never appear in the operator's
/// view of the same cloud alias.
#[test]
fn a_cloud_projection_shows_only_the_callers_own_source_rows() {
    use cadence_agent::cloud_agent::{
        self, CloudTransport, RemoteAgent, RemoteOperation, RemoteStatus, Sender, Source,
        SubmitRequest,
    };
    use cadence_agent::remote_cli::RemoteTarget;
    use cadence_agent::store::cloud_agent::{CloudSource, Owner};

    struct PeerReply;
    impl CloudTransport for PeerReply {
        fn connect(&self, _: &RemoteTarget, _: &str) -> cadence_agent::Result<RemoteAgent> {
            Err(cadence_agent::Error::rejected("connect unused"))
        }
        fn submit(
            &self,
            _: &RemoteTarget,
            request: &SubmitRequest<'_>,
        ) -> cadence_agent::Result<RemoteOperation> {
            Ok(RemoteOperation {
                status: RemoteStatus::Completed,
                remote_operation_id: Some(request.operation_id.to_string()),
                remote_session_id: None,
                provider_session_id: None,
                reply: Some("peer-only reply".into()),
                error: None,
                invoker_id: Some("inv_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".into()),
            })
        }
        fn read(
            &self,
            _: &RemoteTarget,
            _: &str,
            _: &str,
            _: &Source,
        ) -> cadence_agent::Result<RemoteOperation> {
            Err(cadence_agent::Error::rejected("read unused"))
        }
        fn cancel(
            &self,
            _: &RemoteTarget,
            _: &str,
            _: &str,
            _: &Source,
        ) -> cadence_agent::Result<()> {
            Err(cadence_agent::Error::rejected("cancel unused"))
        }
    }

    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    // The caller's OWN settled turn.
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    fx.call(
        Asserted::Operator,
        "cloud_agent_read",
        json!({"cloud_agent_id": id, "operation_id": OPERATION}),
    )
    .unwrap();

    // A second source admitted on the same connection, with its own settled
    // turn on the same canonical work conversation.
    fx.call(
        Asserted::Operator,
        "agent_register",
        json!({"alias": "peer", "provider": "fake", "endpoint_kind": "fake", "cwd": fx.cwd()}),
    )
    .unwrap();
    let store = fx.store();
    let cloud = store.cloud_agent(&id).unwrap().expect("cloud agent");
    let peer = store.agent("peer").unwrap();
    let stamp = cadence_agent::cloud_agent::registration_epoch(peer.created);
    let owner = Owner::new("https://issuer.example.test", "stub-subject");
    store
        .cloud_agent_source_grant(&CloudSource {
            cloud_agent_id: id.clone(),
            kind: "local_peer".into(),
            uid: peer.uid.clone(),
            registration_epoch: stamp,
            owner: owner.clone(),
            label: "peer".into(),
            scope: "cloud_agent.write".into(),
            granted_by: "operator".into(),
            granted_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs_f64(),
            invoker_id: None,
            state: "active".into(),
        })
        .unwrap();
    let target = RemoteTarget {
        org: cloud.org_slug.clone(),
        endpoint: cloud.endpoint.clone(),
        org_id: cloud.organization_id.clone(),
        identity: None,
    };
    // The enrolled peer record is what `source_for` reads; with it present,
    // the only thing left to stop the peer is the split-UID boundary.
    cadence_agent::remote_cli::cloud_agent_save_peer(
        &target,
        &owner,
        &peer.uid,
        stamp,
        &json!({
            "agent_id": "hca_native_guard_peer",
            "credential_id": "hcp_native_guard_peer",
            "token": format!("hct_{}", "p".repeat(43)),
            "expires_at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 600,
            "role": "cloud_agent",
            "sender": {"uid": peer.uid, "registration_epoch": stamp},
        }),
    )
    .unwrap();
    let source = Source::registered(&peer, owner);
    cloud_agent::send(
        &store,
        &PeerReply,
        &cloud,
        &target,
        &home,
        &Sender::Agent("peer".into()),
        &source,
        "peer-operation-0001",
        "peer request body",
        true,
        None,
        || Ok(()),
    )
    .expect("the admitted peer source settles its own turn");
    let peer_operation = store
        .cloud_agent_operation(&id, "peer-operation-0001")
        .unwrap()
        .expect("peer operation");
    assert_eq!(peer_operation.status, "completed");

    for (method, params) in [
        ("thread_read", json!({"alias": ALIAS})),
        ("agent_show", json!({"alias": ALIAS})),
        ("agent_events", json!({"alias": ALIAS, "after": 0})),
    ] {
        let value = fx
            .call(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("{method} refused the owner: {e}"));
        let text = value.to_string();
        assert!(
            text.contains("genuine stub reply"),
            "{method} lost the caller's own result: {value}"
        );
        assert!(
            !text.contains("peer-only reply"),
            "{method} projected another source's reply: {value}"
        );
        assert!(
            !text.contains("peer request body"),
            "{method} projected another source's request: {value}"
        );
    }

    // This fixture runs same-UID, where a peer's cloud projection is refused
    // before the row filter by the split-UID boundary itself. The enrolled
    // peer therefore reads nothing (and certainly not the operator's reply).
    let error = fx
        .call(
            Asserted::Agent("peer".into()),
            "thread_read",
            json!({"alias": ALIAS}),
        )
        .unwrap_err();
    assert_eq!(
        code(&error),
        Some("cloud_agent_peer_isolation_required"),
        "{error}"
    );
    assert!(!error.to_string().contains("genuine stub reply"));
}

/// The cloud projection guard must not over-refuse: a non-cloud alias and an
/// ordinary UID-bound peer report keep projecting for the operator and for
/// the posting agent.
#[test]
fn a_non_cloud_alias_and_a_uid_bound_peer_report_still_project() {
    let fx = Fx::start();
    fx.connect();
    fx.call(
        Asserted::Operator,
        "agent_register",
        json!({"alias": "worker", "provider": "fake", "endpoint_kind": "fake", "cwd": fx.cwd()}),
    )
    .unwrap();
    fx.call(
        Asserted::Operator,
        "agent_register",
        json!({"alias": "peer", "provider": "fake", "endpoint_kind": "fake", "cwd": fx.cwd()}),
    )
    .unwrap();
    // An ordinary UID-bound peer report into a NON-cloud alias, over the
    // agents' own send route (`cadence send`, not `thread_send`).
    fx.call(
        Asserted::Agent("peer".into()),
        "agent_send",
        json!({
            "alias": "worker", "text": "uid-bound peer report",
            "message": "m-peer-report-0001",
        }),
    )
    .unwrap();
    let store = fx.store();
    let peer = store.agent("peer").unwrap();
    let report = store
        .message("m-peer-report-0001")
        .unwrap()
        .expect("peer report row");
    assert_eq!(
        report.sender_uid.as_deref(),
        Some(peer.uid.as_str()),
        "the peer report lost its UID attribution"
    );

    // `agent_show` projects the alias' messages: the peer's report is there
    // for the operator AND for the posting peer, with its UID attribution.
    for who in [Asserted::Operator, Asserted::Agent("peer".into())] {
        let shown = fx
            .call(who, "agent_show", json!({"alias": "worker"}))
            .unwrap_or_else(|e| panic!("agent_show refused ordinary local work: {e}"));
        assert!(
            shown.to_string().contains("uid-bound peer report"),
            "agent_show lost the peer report: {shown}"
        );
    }
    // `thread_read` projects thread entries, and this untargeted send wrote
    // none; what matters is that the cloud projection guard does not refuse a
    // non-cloud alias for either caller.
    for who in [Asserted::Operator, Asserted::Agent("peer".into())] {
        let thread = fx
            .call(who, "thread_read", json!({"alias": "worker"}))
            .expect("thread_read must keep working on a non-cloud alias");
        assert!(
            thread["entries"].is_array(),
            "thread_read broke on a non-cloud alias: {thread}"
        );
    }
}

/// Cloud content whose four authority tables do not resolve fails closed:
/// the projection refuses rather than treating the alias as an ordinary
/// local agent.
#[test]
fn cloud_content_without_resolvable_authority_fails_closed() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    fx.call(
        Asserted::Operator,
        "cloud_agent_read",
        json!({"cloud_agent_id": id, "operation_id": OPERATION}),
    )
    .unwrap();

    // Remove exactly what a restored coordination package never carries: the
    // connection, grant, session and receipt authority. The request and
    // result rows stay, which is the state that must fail closed.
    {
        let conn = rusqlite::Connection::open(fx.state.join("cadence.sqlite3")).unwrap();
        conn.busy_timeout(Duration::from_secs(10)).unwrap();
        for sql in [
            "DELETE FROM cloud_agent_operations WHERE cloud_agent_id=?1",
            "DELETE FROM cloud_agent_sessions WHERE cloud_agent_id=?1",
            "DELETE FROM cloud_agent_sources WHERE cloud_agent_id=?1",
            "DELETE FROM cloud_agents WHERE id=?1",
        ] {
            conn.execute(sql, [id.as_str()]).unwrap();
        }
    }
    let store = fx.store();
    assert!(
        store.cloud_agents_for_alias(ALIAS).unwrap().is_empty(),
        "the fixture still resolves cloud authority"
    );
    assert!(
        store.alias_has_cloud_content(ALIAS).unwrap(),
        "the fixture lost the cloud content it asserts about"
    );

    for (method, params) in [
        ("thread_read", json!({"alias": ALIAS})),
        ("agent_show", json!({"alias": ALIAS})),
        ("agent_events", json!({"alias": ALIAS, "after": 0})),
    ] {
        let error = fx.call(Asserted::Operator, method, params).unwrap_err();
        assert_eq!(
            code(&error),
            Some("cloud_agent_authority_missing"),
            "{method}: {error}"
        );
        assert!(
            !error.to_string().contains("genuine stub reply"),
            "{method} disclosed the reply: {error}"
        );
    }
}

/// An unreachable readback proves nothing, so a settled operation may not be
/// answered from its local receipt; a correlated nonterminal readback returns
/// only its own state. Both hide the cached reply and both really read.
#[test]
fn an_unreachable_readback_is_uncertain_and_a_pending_answer_never_serves_the_receipt() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    fx.call(
        Asserted::Operator,
        "cloud_agent_read",
        json!({"cloud_agent_id": id, "operation_id": OPERATION}),
    )
    .unwrap();
    assert_eq!(fx.stub.calls("readback"), 1);

    // 1. The settled receipt, with an unreachable platform.
    fx.stub.set(|script| script.read_drop = true);
    let before = fx.stub.calls("readback");
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_read",
            json!({"cloud_agent_id": id, "operation_id": OPERATION}),
        )
        .unwrap_err();
    assert_eq!(code(&error), Some("cloud_agent_uncertain"), "{error}");
    assert_eq!(error.kind(), "unknown", "{error}");
    assert!(
        !error.to_string().contains("genuine stub reply"),
        "the unreachable readback served the cached reply: {error}"
    );
    assert_eq!(
        fx.stub.calls("readback"),
        before + 1,
        "the uncertainty was asserted without attempting the readback"
    );
    // The durable terminal receipt survives for reconciliation.
    let kept = fx
        .store()
        .cloud_agent_operation(&id, OPERATION)
        .unwrap()
        .expect("settled receipt");
    assert_eq!(kept.status, "completed");
    assert_eq!(kept.reply_text.as_deref(), Some("genuine stub reply"));

    // The settled send replay is the same guard, not a second path.
    let replay = fx
        .call(
            Asserted::Operator,
            "cloud_agent_send",
            json!({
                "cloud_agent_id": id, "thread_id": home,
                "text": TEXT, "operation_id": OPERATION, "wake": true,
            }),
        )
        .unwrap_err();
    assert_eq!(code(&replay), Some("cloud_agent_uncertain"), "{replay}");
    assert!(
        !replay.to_string().contains("genuine stub reply"),
        "the settled replay served the cached reply: {replay}"
    );
    assert_eq!(
        fx.stub.calls("submit"),
        1,
        "the unsettled replay submitted a second turn"
    );

    // 2. A correlated, nonterminal readback answer for a settled row.
    fx.stub.set(|script| {
        script.read_drop = false;
        script.read_state = "running".into();
    });
    let before = fx.stub.calls("readback");
    let pending = fx
        .call(
            Asserted::Operator,
            "cloud_agent_read",
            json!({"cloud_agent_id": id, "operation_id": OPERATION}),
        )
        .expect("a nonterminal readback is a normal answer");
    assert_eq!(pending["operation"]["status"], json!("running"));
    assert_eq!(pending["operation"]["terminal"], json!(false));
    assert!(
        pending["operation"]["reply"].is_null(),
        "the pending answer carried a reply: {pending}"
    );
    assert!(
        !pending.to_string().contains("genuine stub reply"),
        "the pending answer served the cached reply: {pending}"
    );
    assert_eq!(
        fx.stub.calls("readback"),
        before + 1,
        "the pending answer was served without a readback"
    );
    let kept = fx
        .store()
        .cloud_agent_operation(&id, OPERATION)
        .unwrap()
        .expect("settled receipt");
    assert_eq!(
        kept.status, "completed",
        "the pending answer reopened the receipt"
    );
    assert_eq!(kept.reply_text.as_deref(), Some("genuine stub reply"));

    // Positive control: the platform's own terminal answer is served again.
    fx.stub.set(|script| script.read_state = "completed".into());
    let admitted = fx
        .call(
            Asserted::Operator,
            "cloud_agent_read",
            json!({"cloud_agent_id": id, "operation_id": OPERATION}),
        )
        .unwrap();
    assert_eq!(admitted["operation"]["status"], json!("completed"));
    assert_eq!(
        admitted["operation"]["reply"]["text"],
        json!("genuine stub reply")
    );
}

/// A portable round trip drops the cloud request/result text together with
/// its authority rows, and the restored alias fails closed for an ordinary
/// local caller instead of reading as an ordinary local conversation.
#[test]
fn a_portable_round_trip_drops_the_cloud_reply_and_the_restored_alias_fails_closed() {
    use cadence_agent::backup::portable::{
        export_portable, restore_activate_disabled, PortableExportOptions, PortableManifest, Scope,
    };
    use cadence_agent::backup::portable_activate::required_inventory;

    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    fx.call(
        Asserted::Operator,
        "cloud_agent_read",
        json!({"cloud_agent_id": id, "operation_id": OPERATION}),
    )
    .unwrap();

    // A clean tracker (pm.yaml at its root) is what the package pins.
    let tracker = fx.root.path().join("tracker");
    std::fs::create_dir_all(&tracker).unwrap();
    std::fs::copy(fx.pm.join("pm.yaml"), tracker.join("pm.yaml")).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "pm.yaml"],
        vec!["commit", "-q", "-m", "CAD-1853 portable guard fixture"],
    ] {
        let mut command = std::process::Command::new("git");
        command
            .arg("-C")
            .arg(&tracker)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "init.defaultBranch=main",
                "-c",
                "maintenance.auto=false",
            ])
            .args(&args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "cad1853-acceptance")
            .env("GIT_AUTHOR_EMAIL", "acceptance@example.invalid")
            .env("GIT_COMMITTER_NAME", "cad1853-acceptance")
            .env("GIT_COMMITTER_EMAIL", "acceptance@example.invalid");
        let output = cadence_agent::reaper::output(&mut command).unwrap();
        assert!(
            output.status.success(),
            "fixture git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let package = fx.root.path().join("package");
    export_portable(
        &fx.state,
        &package,
        &PortableExportOptions {
            scope: Scope::Coordination,
            tracker,
            redact_history_secrets: false,
        },
    )
    .expect("a workspace holding a completed cloud turn must still export");
    let manifest: PortableManifest =
        serde_json::from_slice(&std::fs::read(package.join("manifest.json")).unwrap()).unwrap();
    for table in [
        "cloud_agents",
        "cloud_agent_sources",
        "cloud_agent_sessions",
        "cloud_agent_operations",
    ] {
        assert!(
            manifest.omitted_tables.iter().any(|row| row == table),
            "the package did not declare {table} omitted"
        );
    }
    let plan = required_inventory(&package).expect("the package's non-terminal rows");
    let restored = fx.root.path().join("restored");
    restore_activate_disabled(&package, &restored, &plan)
        .expect("the package restores with execution disabled");

    // Neither the package nor the restored copy carries the cloud reply or
    // request text, and no cloud authority row travels.
    for copy in [&package, &restored] {
        let conn = rusqlite::Connection::open_with_flags(
            copy.join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |row| row.get(0)).unwrap() };
        assert_eq!(
            count("SELECT COUNT(*) FROM messages WHERE body LIKE '%genuine stub reply%'"),
            0,
            "{}: the cloud reply text survived",
            copy.display()
        );
        assert_eq!(
            count(&format!(
                "SELECT COUNT(*) FROM messages WHERE body LIKE '%{TEXT}%'"
            )),
            0,
            "{}: the cloud request text survived",
            copy.display()
        );
        for table in [
            "cloud_agents",
            "cloud_agent_sources",
            "cloud_agent_sessions",
            "cloud_agent_operations",
        ] {
            assert_eq!(
                count(&format!("SELECT COUNT(*) FROM {table}")),
                0,
                "{}: {table} authority survived",
                copy.display()
            );
        }
        // The alias row stays inert so an explicit reconnect can reuse it.
        assert_eq!(
            count(&format!(
                "SELECT COUNT(*) FROM agents WHERE alias='{ALIAS}' \
                 AND provider='agenticos-cloud' AND endpoint IS NULL"
            )),
            1,
            "{}: the inert alias row is missing",
            copy.display()
        );
    }

    // The restored store answers an ordinary local caller: with no cloud
    // authority to resolve, the projection of the alias fails closed.
    let (stop, handle) = spawn_daemon_at(&restored, &fx.pm);
    let restored_show = scoped(Asserted::Operator, || {
        client::rpc(&restored, "agent_show", json!({"alias": ALIAS}))
    })
    .unwrap_err();
    assert_eq!(
        code(&restored_show),
        Some("cloud_agent_authority_missing"),
        "agent_show on restored data: {restored_show}"
    );
    assert!(
        !restored_show.to_string().contains("genuine stub reply"),
        "agent_show disclosed the reply: {restored_show}"
    );
    // The other two projections are execution-capable verbs, so the restored
    // root's execution-disabled fence refuses them even earlier. Either
    // refusal must stay content-free.
    for (method, params) in [
        ("thread_read", json!({"alias": ALIAS})),
        ("agent_events", json!({"alias": ALIAS, "after": 0})),
    ] {
        let error = scoped(Asserted::Operator, || {
            client::rpc(&restored, method, params)
        })
        .unwrap_err();
        assert!(
            !error.to_string().contains("genuine stub reply"),
            "{method} disclosed the reply: {error}"
        );
        assert!(
            error.to_string().contains("execution-disabled")
                || code(&error) == Some("cloud_agent_authority_missing"),
            "{method} was refused by neither the fence nor the projection: {error}"
        );
    }
    // A caller that proves nothing is refused as well: with no cloud
    // authority to resolve, the projection's own refusal comes first and a
    // caller-rule refusal would be equally content-free.
    let unproven = scoped(Asserted::Unproven, || {
        client::rpc(&restored, "agent_show", json!({"alias": ALIAS}))
    })
    .unwrap_err();
    assert!(
        unproven.to_string().contains("derives no agent identity")
            || code(&unproven) == Some("cloud_agent_authority_missing"),
        "{unproven}"
    );
    assert!(
        !unproven.to_string().contains("genuine stub reply"),
        "an unproven caller was shown the reply: {unproven}"
    );
    stop.store(true, SeqCst);
    let _ = handle.join();

    // Positive control: the same projection serves the live connection's
    // settled turn, so the restored refusal is the missing authority.
    let live = fx
        .call(Asserted::Operator, "agent_show", json!({"alias": ALIAS}))
        .expect("the live workspace still projects its own cloud turn");
    assert!(
        live.to_string().contains("genuine stub reply"),
        "the live projection lost the reply: {live}"
    );
}

/// A settled send replay discloses a reply, so a write-only credential may
/// not serve one: the replay refuses on the missing read scope, without a
/// second submit and without a new readback.
#[test]
fn a_write_only_credential_cannot_replay_a_settled_send() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    fx.call(
        Asserted::Operator,
        "cloud_agent_read",
        json!({"cloud_agent_id": id, "operation_id": OPERATION}),
    )
    .unwrap();
    assert_eq!(fx.stub.calls("submit"), 1);
    assert_eq!(fx.stub.calls("readback"), 1);

    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 600;
    write_login(ORG, ORG_ID, &fx.stub.url, &["cloud_agent.write"], expires);
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_send",
            json!({
                "cloud_agent_id": id, "thread_id": home,
                "text": TEXT, "operation_id": OPERATION, "wake": true,
            }),
        )
        .unwrap_err();
    assert_eq!(code(&error), Some("cloud_agent_scope_revoked"), "{error}");
    assert!(
        !error.to_string().contains("genuine stub reply"),
        "the write-only replay served the reply: {error}"
    );
    assert_eq!(fx.stub.calls("submit"), 1, "the replay submitted again");
    assert_eq!(
        fx.stub.calls("readback"),
        1,
        "the replay read back despite the missing read scope"
    );
    let kept = fx
        .store()
        .cloud_agent_operation(&id, OPERATION)
        .unwrap()
        .expect("settled receipt");
    assert_eq!(kept.status, "completed");
    assert_eq!(kept.reply_text.as_deref(), Some("genuine stub reply"));

    // Positive control: with read authority restored the same replay serves
    // the platform's own readback answer.
    write_login(
        ORG,
        ORG_ID,
        &fx.stub.url,
        &["cloud_agent.read", "cloud_agent.write"],
        expires,
    );
    let replayed = fx
        .call(
            Asserted::Operator,
            "cloud_agent_send",
            json!({
                "cloud_agent_id": id, "thread_id": home,
                "text": TEXT, "operation_id": OPERATION, "wake": true,
            }),
        )
        .expect("read authority serves the settled replay");
    assert_eq!(replayed["operation"]["status"], json!("completed"));
    assert_eq!(
        replayed["operation"]["reply"]["text"],
        json!("genuine stub reply")
    );
    assert_eq!(fx.stub.calls("submit"), 1);
    assert_eq!(fx.stub.calls("readback"), 2);
}

/// The normal `send` route onto a cloud alias is that same remote turn, not a
/// local queue: steering, turn correlation, a captured recipient and
/// dispatch-only lane fields are refused whole before any transport call or
/// canonical row.
#[test]
fn the_normal_send_envelope_refuses_cloud_unsupported_fields_before_transport() {
    let fx = Fx::start();
    let connect = fx.connect();
    let recipient = fx.store().agent(ALIAS).unwrap().uid;
    assert_eq!(connect["agent"]["recipient_uid"], json!(recipient));
    for (index, (field, value)) in [
        ("nudge", json!(true)),
        ("turn_id", json!("t-forged")),
        ("expected_recipient_uid", json!(recipient)),
        ("issue", json!("CAD-1853")),
        ("worktree", json!("/tmp/not-a-lane")),
    ]
    .into_iter()
    .enumerate()
    {
        let message = format!("m-envelope-{index}");
        let mut params = json!({"alias": ALIAS, "text": TEXT, "message": message});
        params[field] = value;
        let error = fx
            .call(Asserted::Operator, "agent_send", params)
            .unwrap_err();
        assert_eq!(error.kind(), "rejected", "{field}: {error}");
        let text = error.to_string();
        assert!(
            text.contains(field)
                || text.contains("not a cloud-agent send field")
                || text.contains("not steering"),
            "{field} was not refused as an unsupported cloud-send field: {error}"
        );
        assert_eq!(
            fx.stub.calls("submit"),
            0,
            "{field} reached the model boundary"
        );
        assert!(
            fx.store().message(&message).unwrap().is_none(),
            "{field} wrote a canonical message"
        );
    }

    // Positive control: the cloud-legal envelope (text + message) submits once.
    let accepted = fx
        .call(
            Asserted::Operator,
            "agent_send",
            json!({"alias": ALIAS, "text": TEXT, "message": "m-envelope-accepted"}),
        )
        .expect("the cloud-legal envelope must send");
    assert_eq!(accepted["operation"]["status"], json!("queued"));
    assert_eq!(fx.stub.calls("submit"), 1);
}

/// A reconnect refreshes consent, never identity: a refused reconnect leaves
/// the retained invoker pin and provider session byte-for-byte as they were.
#[test]
fn a_refused_reconnect_leaves_the_pin_and_the_session_unchanged() {
    let fx = Fx::start();
    let connect = fx.connect();
    let id = cloud_agent_id(&connect);
    let home = thread_of(&connect);
    fx.call(
        Asserted::Operator,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id, "thread_id": home,
            "text": TEXT, "operation_id": OPERATION, "wake": true,
        }),
    )
    .unwrap();
    fx.call(
        Asserted::Operator,
        "cloud_agent_read",
        json!({"cloud_agent_id": id, "operation_id": OPERATION}),
    )
    .unwrap();

    let store = fx.store();
    let before_sources = store.cloud_agent_sources(&id).unwrap();
    let before_sessions = store.cloud_agent_sessions(&id).unwrap();
    assert_eq!(before_sources.len(), 1);
    assert_eq!(before_sessions.len(), 1);
    let pinned = before_sources[0]
        .invoker_id
        .clone()
        .expect("the connection pinned its stable owner");

    // The contract answers with a different stable owner.
    fx.stub.set(|script| {
        script.provenance = Some(json!({
            "kind": "user",
            "invoker_id": "inv_yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy",
            "peer": null,
        }));
    });
    let calls_before = fx.stub.calls("agent");
    let error = fx
        .call(
            Asserted::Operator,
            "cloud_agent_connect",
            json!({
                "org": ORG,
                "org_id": ORG_ID,
                "endpoint": fx.stub.url,
                "alias": ALIAS,
                "accept_assistant_consent": true,
            }),
        )
        .unwrap_err();
    assert_eq!(code(&error), Some("cloud_agent_invoker_changed"), "{error}");
    assert_eq!(
        fx.stub.calls("agent"),
        calls_before + 1,
        "the refused reconnect never reached the contract"
    );

    let after_sources = fx.store().cloud_agent_sources(&id).unwrap();
    let after_sessions = fx.store().cloud_agent_sessions(&id).unwrap();
    assert_eq!(
        after_sources[0].invoker_id.as_deref(),
        Some(pinned.as_str()),
        "the refused reconnect replaced the invoker pin"
    );
    assert_eq!(
        after_sources, before_sources,
        "the refused reconnect rewrote the granted source"
    );
    assert_eq!(
        after_sessions.len(),
        before_sessions.len(),
        "the refused reconnect created or dropped a provider session"
    );
    for (after, before) in after_sessions.iter().zip(&before_sessions) {
        assert_eq!(
            after.to_json(),
            before.to_json(),
            "the refused reconnect rewrote the provider session"
        );
    }
    let agent = fx.store().cloud_agent(&id).unwrap().expect("cloud agent");
    assert_eq!(agent.state, "connected");
    let read = fx
        .call(
            Asserted::Operator,
            "cloud_agent_read",
            json!({"cloud_agent_id": id, "operation_id": OPERATION}),
        )
        .expect("the retained connection still reads its settled turn");
    assert_eq!(
        read["operation"]["reply"]["text"],
        json!("genuine stub reply")
    );

    // Positive control: the pinned owner answer reconnects cleanly.
    fx.stub.set(|script| script.provenance = None);
    let again = fx.connect();
    assert_eq!(again["agent"]["state"], json!("connected"));
    assert_eq!(again["agent"]["id"], json!(id));
}
