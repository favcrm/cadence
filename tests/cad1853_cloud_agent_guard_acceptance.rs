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
use std::sync::atomic::{AtomicBool, Ordering, Ordering::SeqCst};
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
    submit_accepted: bool,
    read_state: String,
    read_text: String,
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
            submit_accepted: true,
            read_state: "completed".into(),
            read_text: "genuine stub reply".into(),
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
                            "text": null,
                            "reason": null,
                        },
                    }),
                )
            }
        }
        "readback" => {
            let script = script.lock().unwrap_or_else(|e| e.into_inner());
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
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = Arc::clone(&stop);
        let state = self.state.clone();
        let pm = self.pm.clone();
        let env = ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(stop),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        self.daemon = Some(thread::spawn(move || daemon::serve_with(&state, opts)));
        let deadline = Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&self.state, "health", json!({}), Duration::from_secs(1)).is_err()
            || Seam::token_at(&self.state).is_none()
        {
            assert!(
                !self.daemon.as_ref().unwrap().is_finished() && Instant::now() < deadline,
                "fixture daemon never started"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn restart(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(handle) = self.daemon.take() {
            let _ = handle.join();
        }
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
    // history is not disclosed and no transport is invoked.
    struct NoTransport;
    impl CloudTransport for NoTransport {
        fn connect(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &str,
        ) -> cadence_agent::Result<RemoteAgent> {
            Err(cadence_agent::Error::rejected("no transport"))
        }
        fn submit(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &SubmitRequest<'_>,
        ) -> cadence_agent::Result<RemoteOperation> {
            Err(cadence_agent::Error::rejected("no transport"))
        }
        fn read(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &str,
            _: &str,
            _: &Source,
        ) -> cadence_agent::Result<RemoteOperation> {
            Err(cadence_agent::Error::rejected("no transport"))
        }
        fn cancel(
            &self,
            _: &cadence_agent::remote_cli::RemoteTarget,
            _: &str,
            _: &str,
            _: &Source,
        ) -> cadence_agent::Result<()> {
            Err(cadence_agent::Error::rejected("no transport"))
        }
    }
    let foreign = Source::registered(
        &peer,
        Owner::new("https://other-issuer.example.test", "other-parent"),
    );
    let error = cloud_agent::read_operation(
        &store,
        &NoTransport,
        &cloud,
        &target,
        OPERATION,
        &foreign,
        || Ok(()),
    )
    .unwrap_err();
    assert!(
        !error.to_string().is_empty(),
        "foreign owner read settled history"
    );
    // Same human subject, different issuer is the additional old-bug catch.
    let foreign_issuer = Source::operator(Owner::new(
        "https://other-issuer.example.test",
        "stub-subject",
    ));
    let error = cloud_agent::read_operation(
        &store,
        &NoTransport,
        &cloud,
        &target,
        OPERATION,
        &foreign_issuer,
        || Ok(()),
    )
    .unwrap_err();
    assert!(
        !error.to_string().is_empty(),
        "different issuer read settled history"
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
        self, CloudTransport, RemoteAgent, RemoteOperation, Source, SubmitRequest,
    };
    use cadence_agent::remote_cli::RemoteTarget;
    use cadence_agent::store::Owner;

    struct NoTransport;
    impl CloudTransport for NoTransport {
        fn connect(&self, _: &RemoteTarget, _: &str) -> cadence_agent::Result<RemoteAgent> {
            Err(cadence_agent::Error::rejected(
                "no transport in this bad case",
            ))
        }
        fn submit(
            &self,
            _: &RemoteTarget,
            _: &SubmitRequest<'_>,
        ) -> cadence_agent::Result<RemoteOperation> {
            Err(cadence_agent::Error::rejected(
                "no transport in this bad case",
            ))
        }
        fn read(
            &self,
            _: &RemoteTarget,
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
            _: &RemoteTarget,
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
    let owner = Owner::new("https://issuer.example.test", "stub-subject");
    let foreign = Source::operator(Owner::new(
        "https://other-issuer.example.test",
        "other-subject",
    ));
    let foreign_error = cloud_agent::read_operation(
        &store,
        &NoTransport,
        &agent,
        &target,
        OPERATION,
        &foreign,
        || Ok(()),
    )
    .unwrap_err();
    assert!(
        !foreign_error.to_string().is_empty(),
        "a foreign owner read the settled history"
    );

    // Admitted positive control: the original owner still reads it, with no
    // transport call because the operation is already settled.
    let admitted = cloud_agent::read_operation(
        &store,
        &NoTransport,
        &agent,
        &target,
        OPERATION,
        &Source::operator(owner),
        || Ok(()),
    )
    .expect("the original owner must read its settled history");
    assert_eq!(admitted.reply_text.as_deref(), Some("genuine stub reply"));
    assert_eq!(fx.stub.calls("readback"), 1);
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
