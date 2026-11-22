//! Independently authored CAD-2584 acceptance check; implementers must not
//! edit or weaken it. Written from the ticket, not the implementation.

use super::delivery::deliver_kickoff_with_adapter;
use super::reporting::{
    agent_credential_path, hosted_binding_target, private_app_package_path, store_kickoff_binding,
    store_private_app_package,
};
use super::Kickoff;
use crate::error::Error;
use crate::mcp_agent::{invoke_private_app_tool_with, static_tool_names};
use crate::remote_cli::{call_verb, AgentSlot, CliIdentityPin, RemoteTarget};
use base64::Engine as _;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ORG_SLUG: &str = "ws-test";
const ORG_ID: &str = "org_test_1";
const SUBJECT_ID: &str = "usr_test_1";
const ISSUER: &str = "https://issuer.test";
const ISSUE_ID: &str = "CAD-2584";
const ASSIGNMENT_ID: &str = "hla_1111111111111111";
const CLAIM_ID: &str = "hlc_1111111111111111";
const MESSAGE_ID: &str = "msg-cad2584-01";
const ALIAS: &str = "agent-updater";
const ARTIFACT_ID: &str = "art-0123456789abcdef0123456789abcdef";
const REVIEW_ID: &str = "par_0123456789abcdef0123456789abcdef";
const ACCESS_TOKEN: &str = "hct_0123456789abcdefghijklmnopqrstuvwxyzABCDEFG";
const JWS_ENVELOPE: &str = "eyJ0eXAiOiJKV1QifQ.eyJzdWIiOiJ1c3JfdGVzdF8xIn0.c2ln";

#[cfg(any(debug_assertions, feature = "test-seam"))]
fn reset_managed_session() {
    crate::remote_cli::unbind_managed_session();
}

#[cfg(not(any(debug_assertions, feature = "test-seam")))]
fn reset_managed_session() {}

fn minimal_zip_bytes(tag: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(64 + tag.len());
    bytes.extend_from_slice(b"PK\x03\x04\x14\x00\x00\x00\x00\x00\x00\x00\x00\x00");
    bytes.extend_from_slice(b"\x00\x00\x00\x00\x04\x00\x00\x00\x04\x00\x00\x00\x05\x00\x00\x00");
    bytes.extend_from_slice(b"a.txt");
    bytes.extend_from_slice(tag);
    bytes.extend_from_slice(
        b"PK\x05\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
    );
    bytes
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_secs()
}

fn write_private_json(path: &Path, value: &Value) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).expect("chmod parent");
    }
    let bytes = serde_json::to_vec(value).expect("serialize json");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .expect("open private json");
    file.write_all(&bytes).expect("write private json");
    file.sync_all().expect("sync private json");
}

struct IsolatedEnv {
    _tmp: tempfile::TempDir,
    state_dir: PathBuf,
    auth_dir: PathBuf,
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    _lock: MutexGuard<'static, ()>,
}

impl IsolatedEnv {
    fn new() -> Self {
        let lock = crate::home::CAD1922_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reset_managed_session();
        let tmp = tempfile::tempdir().expect("tempdir");
        let names = [
            "HOME",
            "CADENCE_HOME",
            "CADENCE_STATE_DIR",
            "CADENCE_PM_DIR",
            "CADENCE_PROFILE",
            "CADENCE_SANDBOX_ROOT",
            "XDG_STATE_HOME",
            "XDG_CONFIG_HOME",
            "CADENCE_ALIAS",
        ];
        let saved = names
            .iter()
            .map(|&name| (name, std::env::var_os(name)))
            .collect();
        for name in names {
            std::env::remove_var(name);
        }
        std::env::set_var("HOME", tmp.path());
        crate::home::note_state_dir_flag(None);

        let state_dir = tmp.path().join("state");
        fs::create_dir_all(&state_dir).expect("create state dir");
        fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700)).expect("chmod state");

        let auth_dir = crate::remote_auth::auth_dir(None).expect("auth_dir");
        fs::create_dir_all(&auth_dir).expect("create auth dir");
        fs::set_permissions(&auth_dir, fs::Permissions::from_mode(0o700)).expect("chmod auth");

        Self {
            _tmp: tmp,
            state_dir,
            auth_dir,
            saved,
            _lock: lock,
        }
    }

    fn write_cli_login(&self, endpoint: &str, capabilities: &[&str]) {
        let login_path = self.auth_dir.join(format!("cli-{ORG_SLUG}.json"));
        write_private_json(
            &login_path,
            &json!({
                "version": "hosted-cadence-auth.v1",
                "organization_id": ORG_ID,
                "organization_slug": ORG_SLUG,
                "audience": endpoint,
                "issuer": ISSUER,
                "subject_id": SUBJECT_ID,
                "capabilities": capabilities,
                "access_token": ACCESS_TOKEN,
                "expires_at": unix_now() + 3600,
            }),
        );
    }

    fn write_cached_agent_credential(&self, endpoint: &str) {
        let cred_path =
            agent_credential_path(&self.state_dir, MESSAGE_ID).expect("agent credential path");
        write_private_json(
            &cred_path,
            &json!({
                "version": "cadence-agent-credential.v1",
                "assignment_id": ASSIGNMENT_ID,
                "claim_id": CLAIM_ID,
                "generation": 1,
                "organization_id": ORG_ID,
                "audience": endpoint,
                "capabilities": ["cli.read", "cli.write"],
                "access_token": ACCESS_TOKEN,
                "expires_at": unix_now() + 600,
            }),
        );
    }
}

impl Drop for IsolatedEnv {
    fn drop(&mut self) {
        reset_managed_session();
        crate::home::note_state_dir_flag(None);
        for (name, value) in &self.saved {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

struct ParsedHttpRequest {
    path: String,
    body: Vec<u8>,
}

fn read_http_request(stream: &mut TcpStream) -> Option<ParsedHttpRequest> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).ok()? == 0 {
        return None;
    }
    let path = request_line.split_whitespace().nth(1)?.to_string();
    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            break;
        }
        if header == "\r\n" || header == "\n" {
            break;
        }
        if let Some((name, val)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = val.trim().parse::<usize>().ok().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).ok()?;
    }
    Some(ParsedHttpRequest { path, body })
}

fn write_json_response(stream: &mut TcpStream, status: u16, body: &Value) {
    let payload = serde_json::to_vec(body).expect("serialize response");
    let reason = if status == 200 { "OK" } else { "Error" };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&payload);
    let _ = stream.flush();
}

#[derive(Clone)]
enum CliCallReply {
    Json(u16, Value),
    DropEof,
}

struct LoopbackConfig {
    frontmatter_private_app: Value,
    profile_enabled_tools: HashMap<String, Vec<String>>,
    zip_bytes: Vec<u8>,
    queued_cli_calls: Mutex<VecDeque<CliCallReply>>,
}

impl LoopbackConfig {
    fn for_board(
        frontmatter_private_app: Value,
        profile_enabled_tools: HashMap<String, Vec<String>>,
        zip_bytes: Vec<u8>,
    ) -> Self {
        Self {
            frontmatter_private_app,
            profile_enabled_tools,
            zip_bytes,
            queued_cli_calls: Mutex::new(VecDeque::new()),
        }
    }

    fn with_queued_calls(zip_bytes: Vec<u8>, replies: Vec<CliCallReply>) -> Self {
        Self {
            frontmatter_private_app: json!({}),
            profile_enabled_tools: HashMap::new(),
            zip_bytes,
            queued_cli_calls: Mutex::new(replies.into()),
        }
    }
}

struct LoopbackStats {
    total_connections: AtomicUsize,
    authorize_hits: AtomicUsize,
    artifact_read_hits: AtomicUsize,
    artifact_upload_hits: AtomicUsize,
    platform_call_hits: AtomicUsize,
    profile_show_ids: Mutex<Vec<String>>,
    recorded_cli_calls: Mutex<Vec<Value>>,
}

impl LoopbackStats {
    fn new() -> Self {
        Self {
            total_connections: AtomicUsize::new(0),
            authorize_hits: AtomicUsize::new(0),
            artifact_read_hits: AtomicUsize::new(0),
            artifact_upload_hits: AtomicUsize::new(0),
            platform_call_hits: AtomicUsize::new(0),
            profile_show_ids: Mutex::new(Vec::new()),
            recorded_cli_calls: Mutex::new(Vec::new()),
        }
    }

    fn take_cli_calls(&self) -> Vec<Value> {
        let mut guard = self
            .recorded_cli_calls
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *guard)
    }
}

struct LoopbackServer {
    endpoint: String,
    addr: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    stats: Arc<LoopbackStats>,
    handle: Option<JoinHandle<()>>,
}

impl LoopbackServer {
    fn start(config: LoopbackConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let endpoint = format!("http://{addr}");
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(LoopbackStats::new());

        let stop_bg = Arc::clone(&stop);
        let stats_bg = Arc::clone(&stats);
        let handle = std::thread::spawn(move || {
            for incoming in listener.incoming() {
                if stop_bg.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = incoming else {
                    break;
                };
                if stop_bg.load(Ordering::SeqCst) {
                    break;
                }
                stats_bg.total_connections.fetch_add(1, Ordering::SeqCst);
                let Some(req) = read_http_request(&mut stream) else {
                    continue;
                };
                match req.path.as_str() {
                    "/__platform/cli/authorize" => {
                        stats_bg.authorize_hits.fetch_add(1, Ordering::SeqCst);
                        write_json_response(
                            &mut stream,
                            200,
                            &json!({
                                "version": "hosted-cadence-cli.v1",
                                "envelope": JWS_ENVELOPE,
                            }),
                        );
                    }
                    "/__platform/wiki/authorize" => {
                        write_json_response(
                            &mut stream,
                            200,
                            &json!({
                                "version": "hosted-cadence-wiki.v1",
                                "envelope": JWS_ENVELOPE,
                            }),
                        );
                    }
                    "/__platform/wiki/call" => {
                        write_json_response(&mut stream, 200, &json!({ "entries": [] }));
                    }
                    "/__platform/artifacts/upload" => {
                        stats_bg.artifact_upload_hits.fetch_add(1, Ordering::SeqCst);
                        write_json_response(
                            &mut stream,
                            200,
                            &json!({
                                "file": {
                                    "id": ARTIFACT_ID,
                                    "version": 1,
                                    "digest": sha256_hex(&config.zip_bytes),
                                }
                            }),
                        );
                    }
                    p if p.ends_with("/agent-credential") => {
                        let now = unix_now();
                        write_json_response(
                            &mut stream,
                            200,
                            &json!({
                                "version": "hosted-cadence-local-assignment.v1",
                                "assignment_id": ASSIGNMENT_ID,
                                "claim_id": CLAIM_ID,
                                "generation": 1,
                                "principal": { "kind": "agent" },
                                "capabilities": ["cli.read", "cli.write"],
                                "credential": {
                                    "token_type": "Bearer",
                                    "renewal": "remint",
                                    "access_token": ACCESS_TOKEN,
                                    "issued_at": now,
                                    "expires_at": now + 600,
                                }
                            }),
                        );
                    }
                    "/__platform/cli/call" => {
                        let body: Value =
                            serde_json::from_slice(&req.body).unwrap_or_else(|_| json!({}));
                        stats_bg
                            .recorded_cli_calls
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(body.clone());
                        let verb = body["verb"].as_str().unwrap_or_default();
                        if verb == "platform_call" {
                            stats_bg.platform_call_hits.fetch_add(1, Ordering::SeqCst);
                        }
                        let queued = config
                            .queued_cli_calls
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .pop_front();
                        if let Some(reply) = queued {
                            match reply {
                                CliCallReply::Json(status, payload) => {
                                    write_json_response(&mut stream, status, &payload);
                                }
                                CliCallReply::DropEof => {
                                    let _ = stream.shutdown(Shutdown::Both);
                                    drop(stream);
                                }
                            }
                            continue;
                        }
                        let arguments = &body["arguments"];
                        match verb {
                            "issue_show" => {
                                write_json_response(
                                    &mut stream,
                                    200,
                                    &json!({
                                        "issue": {
                                            "id": ISSUE_ID,
                                            "title": "Private app update acceptance",
                                            "frontmatter": {
                                                "private_app_update": config.frontmatter_private_app
                                            },
                                            "artifacts": [{
                                                "id": ARTIFACT_ID,
                                                "kind": "private-app-zip",
                                                "version": 1,
                                                "name": "app.zip"
                                            }]
                                        }
                                    }),
                                );
                            }
                            "issue_history" => {
                                write_json_response(&mut stream, 200, &json!({ "events": [] }));
                            }
                            "agent_profile_show" => {
                                let id = arguments["id"].as_str().unwrap_or_default().to_string();
                                stats_bg
                                    .profile_show_ids
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .push(id.clone());
                                let tools = config
                                    .profile_enabled_tools
                                    .get(&id)
                                    .cloned()
                                    .unwrap_or_default();
                                write_json_response(
                                    &mut stream,
                                    200,
                                    &json!({
                                        "profile": {
                                            "id": id,
                                            "enabled_tools": tools,
                                        }
                                    }),
                                );
                            }
                            "artifact_read" => {
                                stats_bg.artifact_read_hits.fetch_add(1, Ordering::SeqCst);
                                let b64 = base64::engine::general_purpose::STANDARD
                                    .encode(&config.zip_bytes);
                                write_json_response(
                                    &mut stream,
                                    200,
                                    &json!({
                                        "file": {
                                            "id": ARTIFACT_ID,
                                            "kind": "private-app-zip",
                                            "version": 1,
                                            "bytes": config.zip_bytes.len(),
                                            "digest": sha256_hex(&config.zip_bytes),
                                            "name": "app.zip"
                                        },
                                        "total": config.zip_bytes.len(),
                                        "offset": 0,
                                        "content_base64": b64
                                    }),
                                );
                            }
                            "platform_call" => {
                                write_json_response(
                                    &mut stream,
                                    200,
                                    &json!({ "ok": true, "data": {} }),
                                );
                            }
                            _ => {
                                write_json_response(&mut stream, 200, &json!({}));
                            }
                        }
                    }
                    _ => {
                        write_json_response(
                            &mut stream,
                            404,
                            &json!({ "error": { "code": "not_found" } }),
                        );
                    }
                }
            }
        });

        Self {
            endpoint,
            addr,
            stop,
            stats,
            handle: Some(handle),
        }
    }
}

impl Drop for LoopbackServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn remote_target(endpoint: &str) -> RemoteTarget {
    RemoteTarget {
        org: ORG_SLUG.to_string(),
        endpoint: endpoint.to_string(),
        org_id: ORG_ID.to_string(),
        identity: Some(CliIdentityPin {
            subject_id: SUBJECT_ID.to_string(),
            issuer: ISSUER.to_string(),
        }),
    }
}

fn make_kickoff<'a>(worktree: &'a str, profile_id: Option<&'a str>) -> Kickoff<'a> {
    Kickoff {
        alias: ALIAS,
        issue_id: ISSUE_ID,
        assignment_id: ASSIGNMENT_ID,
        worktree,
        message_id: MESSAGE_ID,
        claim_id: CLAIM_ID,
        generation: 1,
        organization_id: ORG_ID,
        audience: "http://127.0.0.1",
        subject_id: Some(SUBJECT_ID),
        issuer: Some(ISSUER),
        profile_id,
    }
}

fn assert_no_custody_and_no_send(state_dir: &Path, server: &LoopbackServer, send_hits: usize) {
    let bind_path = state_dir
        .join("hosted-assignments")
        .join(format!("{MESSAGE_ID}.bind"));
    let zip_path = private_app_package_path(state_dir, MESSAGE_ID).expect("zip path");
    assert!(
        !bind_path.exists(),
        "refused kickoff must not write .bind at {}",
        bind_path.display()
    );
    assert!(
        !zip_path.exists(),
        "refused kickoff must not write .zip at {}",
        zip_path.display()
    );
    assert_eq!(send_hits, 0, "refused kickoff must not invoke send()");
    assert_eq!(
        server.stats.artifact_read_hits.load(Ordering::SeqCst),
        0,
        "refused kickoff must fail before downloading artifact bytes"
    );
}

fn run_kickoff_case(
    frontmatter_private_app: Value,
    profile_enabled_tools: HashMap<String, Vec<String>>,
    adapter_name: Option<&str>,
    claimed_profile_id: Option<&str>,
) -> (Result<Value, Error>, usize, LoopbackServer, IsolatedEnv) {
    let env = IsolatedEnv::new();
    let config = LoopbackConfig::for_board(
        frontmatter_private_app,
        profile_enabled_tools,
        minimal_zip_bytes(b"good"),
    );
    let server = LoopbackServer::start(config);
    env.write_cli_login(&server.endpoint, &["cli.read", "cli.write", "wiki.read"]);

    let target = remote_target(&server.endpoint);
    let worktree = env._tmp.path().to_string_lossy().to_string();
    let kickoff = make_kickoff(&worktree, claimed_profile_id);
    let send_hits = AtomicUsize::new(0);
    let adapter_owned = adapter_name.map(str::to_string);

    let outcome = deliver_kickoff_with_adapter(
        &target,
        &env.state_dir,
        &kickoff,
        10,
        &|| adapter_owned.clone(),
        &|_| {
            send_hits.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        },
    );
    (outcome, send_hits.load(Ordering::SeqCst), server, env)
}

#[test]
fn cad2584_refuses_personal_assistant_updater_before_download_and_send() {
    for fm in [
        json!({
            "artifact_id": ARTIFACT_ID,
            "updater_agent": "service/personal_assistant",
        }),
        json!({
            "artifact_id": ARTIFACT_ID,
            "personal_assistant_requested": true,
        }),
    ] {
        let (outcome, send_hits, server, env) =
            run_kickoff_case(fm, HashMap::new(), Some("codex/managed"), None);
        let err = outcome.expect_err("Personal Assistant must never become updater");
        assert_eq!(err.code(), Some("private_app_readiness_refused"));
        assert!(
            err.to_string().contains("personal_assistant_excluded"),
            "unexpected refusal: {err}"
        );
        assert_no_custody_and_no_send(&env.state_dir, &server, send_hits);
    }
}

#[test]
fn cad2584_refuses_chat_default_app_agent_fallback_before_download_and_send() {
    let mut tools = HashMap::new();
    tools.insert(
        "ap_chat_default".to_string(),
        vec!["cadence_writes".to_string()],
    );
    let fm = json!({
        "artifact_id": ARTIFACT_ID,
        "default_app_agent": "ap_chat_default",
    });
    let (outcome, send_hits, server, env) =
        run_kickoff_case(fm, tools, Some("codex/managed"), Some("ap_chat_default"));
    let err = outcome.expect_err("CAD-2434 chat-only default_app_agent must not become updater");
    assert_eq!(err.code(), Some("private_app_readiness_refused"));
    assert!(
        err.to_string().contains("chat_default_not_updater"),
        "unexpected refusal: {err}"
    );
    assert_no_custody_and_no_send(&env.state_dir, &server, send_hits);
}

#[test]
fn cad2584_refuses_is_chat_default_only_frontmatter_before_download_and_send() {
    let mut tools = HashMap::new();
    tools.insert(
        "ap_chat_only".to_string(),
        vec!["cadence_writes".to_string()],
    );
    let fm = json!({
        "artifact_id": ARTIFACT_ID,
        "updater_agent": "agents/ap_chat_only",
        "is_chat_default_only": true,
        "server_readiness": {
            "ready": true,
            "actor": "agents/ap_chat_only",
            "reasons": []
        }
    });
    let (outcome, send_hits, server, env) =
        run_kickoff_case(fm, tools, Some("codex/managed"), Some("ap_chat_only"));
    let err = outcome.expect_err("is_chat_default_only=true must refuse");
    assert_eq!(err.code(), Some("private_app_readiness_refused"));
    assert!(
        err.to_string().contains("chat_default_not_updater"),
        "unexpected refusal: {err}"
    );
    assert_no_custody_and_no_send(&env.state_dir, &server, send_hits);
}

#[test]
fn cad2584_refuses_server_readiness_actor_mismatch_and_service_actor() {
    for (server_actor, expected_reasons) in [
        (
            "service/personal_assistant",
            vec!["actor_mismatch", "personal_assistant_excluded"],
        ),
        ("agents/ap_other", vec!["actor_mismatch"]),
    ] {
        let mut tools = HashMap::new();
        tools.insert("ap_updater".to_string(), vec!["cadence_writes".to_string()]);
        let fm = json!({
            "artifact_id": ARTIFACT_ID,
            "updater_agent": "agents/ap_updater",
            "server_readiness": {
                "ready": true,
                "actor": server_actor,
                "reasons": []
            }
        });
        let (outcome, send_hits, server, env) =
            run_kickoff_case(fm, tools, Some("codex/managed"), Some("ap_updater"));
        let err = outcome.expect_err("server_readiness actor mismatch must refuse");
        assert_eq!(err.code(), Some("private_app_readiness_refused"));
        let msg = err.to_string();
        for reason in expected_reasons {
            assert!(
                msg.contains(reason),
                "expected '{reason}' in refusal '{msg}' for server_actor={server_actor}"
            );
        }
        assert_no_custody_and_no_send(&env.state_dir, &server, send_hits);
    }
}

#[test]
fn cad2584_refuses_claimed_profile_mismatch_before_download_and_send() {
    let mut tools = HashMap::new();
    tools.insert("ap_updater".to_string(), vec!["cadence_writes".to_string()]);
    let fm = json!({
        "artifact_id": ARTIFACT_ID,
        "updater_agent": "agents/ap_updater",
        "server_readiness": {
            "ready": true,
            "actor": "agents/ap_updater",
            "reasons": []
        }
    });
    let (outcome, send_hits, server, env) =
        run_kickoff_case(fm, tools, Some("codex/managed"), Some("ap_claimed_other"));
    let err = outcome.expect_err("mismatched claimed kickoff.profile_id must refuse");
    assert_eq!(err.code(), Some("private_app_readiness_refused"));
    assert!(
        err.to_string().contains("actor_mismatch"),
        "unexpected refusal: {err}"
    );
    assert_no_custody_and_no_send(&env.state_dir, &server, send_hits);
}

#[test]
fn cad2584_dispatcher_cli_write_cannot_mask_missing_profile_cadence_writes() {
    // Dispatcher saved CLI login has cli.write, but updater profile lacks cadence_writes.
    let mut tools = HashMap::new();
    tools.insert("ap_updater".to_string(), vec![]);
    let fm = json!({
        "artifact_id": ARTIFACT_ID,
        "updater_agent": "agents/ap_updater",
        "server_readiness": {
            "ready": true,
            "actor": "agents/ap_updater",
            "reasons": []
        }
    });
    let (outcome, send_hits, server, env) =
        run_kickoff_case(fm, tools, Some("codex/managed"), Some("ap_updater"));
    let err = outcome.expect_err("missing cadence_writes on updater profile must refuse");
    assert_eq!(err.code(), Some("private_app_readiness_refused"));
    assert!(
        err.to_string().contains("missing_write_scope"),
        "unexpected refusal: {err}"
    );
    let probed = server
        .stats
        .profile_show_ids
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    assert!(
        probed.iter().any(|id| id == "ap_updater"),
        "dispatcher must inspect updater profile via agent_profile_show, got {probed:?}"
    );
    assert_no_custody_and_no_send(&env.state_dir, &server, send_hits);
}

#[test]
fn cad2584_unknown_server_not_ready_reason_fails_closed() {
    for reasons in [json!(["policy_locked"]), json!([])] {
        let mut tools = HashMap::new();
        tools.insert("ap_updater".to_string(), vec!["cadence_writes".to_string()]);
        let fm = json!({
            "artifact_id": ARTIFACT_ID,
            "updater_agent": "agents/ap_updater",
            "server_readiness": {
                "ready": false,
                "actor": "agents/ap_updater",
                "reasons": reasons
            }
        });
        let (outcome, send_hits, server, env) =
            run_kickoff_case(fm, tools, Some("codex/managed"), Some("ap_updater"));
        let err = outcome.expect_err("!server.ready must fail closed");
        assert_eq!(err.code(), Some("private_app_readiness_refused"));
        assert!(
            err.to_string().contains("server_not_ready"),
            "unexpected refusal: {err}"
        );
        assert_no_custody_and_no_send(&env.state_dir, &server, send_hits);
    }
}

#[test]
fn cad2584_unsupported_adapter_claude_managed_refuses_before_download_and_send() {
    let mut tools = HashMap::new();
    tools.insert("ap_updater".to_string(), vec!["cadence_writes".to_string()]);
    let fm = json!({
        "artifact_id": ARTIFACT_ID,
        "updater_agent": "agents/ap_updater",
        "server_readiness": {
            "ready": true,
            "actor": "agents/ap_updater",
            "reasons": []
        }
    });
    let (outcome, send_hits, server, env) =
        run_kickoff_case(fm, tools, Some("claude/managed"), Some("ap_updater"));
    let err = outcome.expect_err("claude/managed adapter must refuse");
    assert_eq!(err.code(), Some("private_app_readiness_refused"));
    assert!(
        err.to_string().contains("adapter_upload_unsupported"),
        "unexpected refusal: {err}"
    );
    assert_no_custody_and_no_send(&env.state_dir, &server, send_hits);
}

#[test]
fn cad2584_workspace_mcp_unpaired_refuses_and_paired_succeeds() {
    for paired in [Some(false), None] {
        let mut tools = HashMap::new();
        tools.insert("ap_updater".to_string(), vec!["cadence_writes".to_string()]);
        let mut fm = json!({
            "artifact_id": ARTIFACT_ID,
            "updater_agent": "agents/ap_updater",
            "server_readiness": {
                "ready": true,
                "actor": "agents/ap_updater",
                "reasons": []
            }
        });
        if let Some(p) = paired {
            fm["workspace_mcp_paired"] = json!(p);
        }
        let (outcome, send_hits, server, env) =
            run_kickoff_case(fm, tools, Some("workspace_mcp/hosted"), Some("ap_updater"));
        let err = outcome.expect_err("unpaired WorkspaceMcp must refuse pre-dispatch");
        assert_eq!(err.code(), Some("private_app_readiness_refused"));
        assert!(
            err.to_string().contains("unpaired_workspace_mcp"),
            "expected unpaired_workspace_mcp for paired={paired:?}, got: {err}"
        );
        assert_no_custody_and_no_send(&env.state_dir, &server, send_hits);
    }

    let mut tools = HashMap::new();
    tools.insert("ap_updater".to_string(), vec!["cadence_writes".to_string()]);
    let fm = json!({
        "artifact_id": ARTIFACT_ID,
        "updater_agent": "agents/ap_updater",
        "workspace_mcp_paired": true,
        "server_readiness": {
            "ready": true,
            "actor": "agents/ap_updater",
            "reasons": []
        }
    });
    let (outcome, send_hits, server, env) =
        run_kickoff_case(fm, tools, Some("workspace_mcp/hosted"), Some("ap_updater"));
    let sent = outcome.expect("paired WorkspaceMcp updater must succeed");
    assert_eq!(send_hits, 1);
    assert_eq!(server.stats.artifact_read_hits.load(Ordering::SeqCst), 1);
    assert!(sent.get("note").is_some());
    assert!(hosted_binding_target(&env.state_dir, MESSAGE_ID)
        .expect("read binding")
        .and_then(|b| b.private_app_package)
        .is_some());
    assert!(private_app_package_path(&env.state_dir, MESSAGE_ID)
        .expect("zip path")
        .is_file());
}

#[test]
fn cad2584_ready_updater_positive_control_stages_bind_zip_and_sends() {
    let mut tools = HashMap::new();
    tools.insert("ap_updater".to_string(), vec!["cadence_writes".to_string()]);
    let fm = json!({
        "artifact_id": ARTIFACT_ID,
        "updater_agent": "agents/ap_updater",
        "server_readiness": {
            "ready": true,
            "actor": "agents/ap_updater",
            "reasons": []
        }
    });
    let (outcome, send_hits, server, env) =
        run_kickoff_case(fm, tools, Some("codex/managed"), Some("ap_updater"));
    let sent = outcome.expect("ready updater positive control must succeed");
    assert_eq!(send_hits, 1);
    assert_eq!(server.stats.artifact_read_hits.load(Ordering::SeqCst), 1);
    assert!(sent.get("note").is_some());

    let binding = hosted_binding_target(&env.state_dir, MESSAGE_ID)
        .expect("read binding")
        .expect("binding exists");
    let custody = binding.private_app_package.expect("custody attached");
    assert_eq!(custody.artifact_ref, ARTIFACT_ID);
    assert_eq!(
        custody.zip_sha256,
        format!("sha256:{}", sha256_hex(&minimal_zip_bytes(b"good")))
    );
    let zip_path = private_app_package_path(&env.state_dir, MESSAGE_ID).expect("zip path");
    assert!(zip_path.is_file(), "staged ZIP must exist");
}

#[test]
fn cad2584_static_tool_names_expose_only_agent_lifecycle_tools() {
    let names = static_tool_names();
    for expected in [
        "private_app_prepare",
        "private_app_status",
        "private_app_apply",
        "private_app_reconcile",
    ] {
        assert!(
            names.contains(&expected),
            "missing required agent private app tool '{expected}' in {names:?}"
        );
    }
    for forbidden in [
        "private_app_authorize",
        "private_app_review_authorize",
        "private_app_update_policy",
        "private_app_review_request",
        "private_app_review_status",
        "private_app_review_apply",
        "private_app_review_reconcile",
        "actor",
        "org",
        "alias",
        "path",
        "principal",
        "subject_id",
        "issuer",
    ] {
        assert!(
            !names.contains(&forbidden),
            "forbidden owner tool or identity field '{forbidden}' exposed in static_tool_names"
        );
    }
}

fn setup_mcp_custody_fixture(
    env: &IsolatedEnv,
    endpoint: &str,
    zip_bytes: &[u8],
    install_id: Option<&str>,
    expected_digest: Option<&str>,
    expected_generation: Option<&str>,
) -> (RemoteTarget, AgentSlot) {
    env.write_cli_login(endpoint, &["cli.read", "cli.write"]);
    env.write_cached_agent_credential(endpoint);
    let target = remote_target(endpoint);
    let worktree = env._tmp.path().to_string_lossy().to_string();
    let kickoff = make_kickoff(&worktree, Some("ap_updater"));
    store_kickoff_binding(
        &env.state_dir,
        ORG_SLUG,
        endpoint,
        &kickoff,
        "Private app update test kickoff",
    )
    .expect("store kickoff binding");
    store_private_app_package(
        &env.state_dir,
        MESSAGE_ID,
        ARTIFACT_ID,
        "app.zip",
        zip_bytes,
        install_id,
        expected_digest,
        expected_generation,
    )
    .expect("store private app package");
    let cred_path =
        agent_credential_path(&env.state_dir, MESSAGE_ID).expect("agent credential path");
    let slot = AgentSlot::new(
        cred_path,
        &target,
        ASSIGNMENT_ID.to_string(),
        CLAIM_ID.to_string(),
        1,
    );
    (target, slot)
}

#[test]
fn cad2584_private_app_prepare_rejects_model_supplied_path_org_alias_before_rpc() {
    let env = IsolatedEnv::new();
    let zip_bytes = minimal_zip_bytes(b"good");
    let server =
        LoopbackServer::start(LoopbackConfig::with_queued_calls(zip_bytes.clone(), vec![]));
    let (target, slot) =
        setup_mcp_custody_fixture(&env, &server.endpoint, &zip_bytes, None, None, None);

    for (tool, args) in [
        (
            "private_app_prepare",
            json!({ "package_ref": ARTIFACT_ID, "path": "/etc/passwd" }),
        ),
        (
            "private_app_prepare",
            json!({ "package_ref": ARTIFACT_ID, "org": "other-org" }),
        ),
        (
            "private_app_prepare",
            json!({ "package_ref": ARTIFACT_ID, "alias": "other-alias" }),
        ),
        (
            "private_app_prepare",
            json!({ "package_ref": ARTIFACT_ID, "actor": "agents/ap_other" }),
        ),
        (
            "private_app_status",
            json!({ "id": REVIEW_ID, "org": "other-org" }),
        ),
        (
            "private_app_apply",
            json!({
                "id": REVIEW_ID,
                "digest": format!("sha256:{}", sha256_hex(&zip_bytes)),
                "zip_sha256": format!("sha256:{}", sha256_hex(&zip_bytes)),
                "alias": "other-alias"
            }),
        ),
        (
            "private_app_reconcile",
            json!({ "id": REVIEW_ID, "actor": "service/personal_assistant" }),
        ),
    ] {
        let err = invoke_private_app_tool_with(
            &env.auth_dir,
            &env.state_dir,
            MESSAGE_ID,
            &target,
            &slot,
            tool,
            &args,
            5,
        )
        .expect_err("forged identity/path argument must be rejected");
        assert!(
            !err.to_string().is_empty(),
            "rejection error should explain failure for {tool} {args}"
        );
    }
    assert_eq!(
        server.stats.total_connections.load(Ordering::SeqCst),
        0,
        "forged args must fail locally before opening any TCP/RPC connection"
    );
}

#[test]
fn cad2584_private_app_prepare_fails_closed_on_mismatched_ref_and_tampered_zip_before_platform_call(
) {
    let env = IsolatedEnv::new();
    let zip_bytes = minimal_zip_bytes(b"good");
    let server =
        LoopbackServer::start(LoopbackConfig::with_queued_calls(zip_bytes.clone(), vec![]));
    let (target, slot) =
        setup_mcp_custody_fixture(&env, &server.endpoint, &zip_bytes, None, None, None);

    // 1. Mismatched package_ref (not matching bound custody ARTIFACT_ID)
    let mismatched = invoke_private_app_tool_with(
        &env.auth_dir,
        &env.state_dir,
        MESSAGE_ID,
        &target,
        &slot,
        "private_app_prepare",
        &json!({ "package_ref": "art-99999999999999999999999999999999" }),
        5,
    );
    assert!(
        mismatched.is_err(),
        "mismatched package_ref must fail closed"
    );
    assert_eq!(
        server.stats.platform_call_hits.load(Ordering::SeqCst),
        0,
        "mismatched package_ref must not reach platform_call RPC"
    );
    assert_eq!(
        server.stats.artifact_upload_hits.load(Ordering::SeqCst),
        0,
        "mismatched package_ref must not upload artifact"
    );

    // 2. Tamper on-disk .zip bytes while keeping matching package_ref
    let zip_path = private_app_package_path(&env.state_dir, MESSAGE_ID).expect("zip path");
    let tampered = minimal_zip_bytes(b"evil");
    fs::write(&zip_path, &tampered).expect("overwrite zip");
    fs::set_permissions(&zip_path, fs::Permissions::from_mode(0o600)).expect("chmod zip");

    let tampered_outcome = invoke_private_app_tool_with(
        &env.auth_dir,
        &env.state_dir,
        MESSAGE_ID,
        &target,
        &slot,
        "private_app_prepare",
        &json!({ "package_ref": ARTIFACT_ID }),
        5,
    );
    assert!(
        tampered_outcome.is_err(),
        "tampered on-disk .zip digest must fail closed"
    );
    assert_eq!(
        server.stats.platform_call_hits.load(Ordering::SeqCst),
        0,
        "tampered .zip must not reach platform_call RPC"
    );
    assert_eq!(
        server.stats.artifact_upload_hits.load(Ordering::SeqCst),
        0,
        "tampered .zip must not upload artifact"
    );
}

#[test]
fn cad2584_private_app_apply_missing_owner_consent_refuses_without_replay() {
    let env = IsolatedEnv::new();
    let zip_bytes = minimal_zip_bytes(b"good");
    let sha = format!("sha256:{}", sha256_hex(&zip_bytes));
    let server = LoopbackServer::start(LoopbackConfig::with_queued_calls(
        zip_bytes.clone(),
        vec![CliCallReply::Json(
            409,
            json!({
                "ok": false,
                "error": {
                    "code": "owner_consent_required",
                    "message": "owner consent required"
                }
            }),
        )],
    ));
    let (target, slot) =
        setup_mcp_custody_fixture(&env, &server.endpoint, &zip_bytes, None, None, None);

    let err = invoke_private_app_tool_with(
        &env.auth_dir,
        &env.state_dir,
        MESSAGE_ID,
        &target,
        &slot,
        "private_app_apply",
        &json!({
            "id": REVIEW_ID,
            "digest": sha,
            "zip_sha256": sha
        }),
        5,
    )
    .expect_err("missing owner consent HTTP 409 must fail closed");
    assert_eq!(err.kind(), "rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("owner_consent_required"),
        "expected owner_consent_required in refusal: {msg}"
    );
    assert!(
        !msg.contains("installed"),
        "refusal must not report installed: {msg}"
    );
    let calls = server.stats.take_cli_calls();
    assert_eq!(calls.len(), 1, "must send exactly one apply POST");
    assert_eq!(
        calls[0].pointer("/arguments/tool").and_then(Value::as_str),
        Some("private_app_review_apply")
    );
}

#[test]
fn cad2584_private_app_prepare_and_apply_stale_installation_refuses_without_replay() {
    let env = IsolatedEnv::new();
    let zip_bytes = minimal_zip_bytes(b"good");
    let sha = format!("sha256:{}", sha256_hex(&zip_bytes));
    let stale_body = json!({
        "ok": false,
        "error": {
            "code": "installation_stale",
            "message": "installed digest or generation changed"
        }
    });
    let server = LoopbackServer::start(LoopbackConfig::with_queued_calls(
        zip_bytes.clone(),
        vec![
            CliCallReply::Json(409, stale_body.clone()),
            CliCallReply::Json(409, stale_body),
        ],
    ));
    let (target, slot) = setup_mcp_custody_fixture(
        &env,
        &server.endpoint,
        &zip_bytes,
        Some("install_app_1"),
        Some(&sha),
        Some("gen_1"),
    );

    let prep_err = invoke_private_app_tool_with(
        &env.auth_dir,
        &env.state_dir,
        MESSAGE_ID,
        &target,
        &slot,
        "private_app_prepare",
        &json!({ "package_ref": ARTIFACT_ID }),
        5,
    )
    .expect_err("stale installation on prepare must fail closed");
    assert_eq!(prep_err.kind(), "rejected");
    assert!(
        prep_err.to_string().contains("installation_stale"),
        "expected installation_stale in prepare refusal: {prep_err}"
    );

    let apply_err = invoke_private_app_tool_with(
        &env.auth_dir,
        &env.state_dir,
        MESSAGE_ID,
        &target,
        &slot,
        "private_app_apply",
        &json!({
            "id": REVIEW_ID,
            "digest": sha,
            "zip_sha256": sha
        }),
        5,
    )
    .expect_err("stale installation on apply must fail closed");
    assert_eq!(apply_err.kind(), "rejected");
    assert!(
        apply_err.to_string().contains("installation_stale"),
        "expected installation_stale in apply refusal: {apply_err}"
    );

    let calls = server.stats.take_cli_calls();
    assert_eq!(calls.len(), 2, "each stale 409 must send only 1 POST");
    assert_eq!(
        calls[0].pointer("/arguments/tool").and_then(Value::as_str),
        Some("private_app_review_request")
    );
    assert_eq!(
        calls[1].pointer("/arguments/tool").and_then(Value::as_str),
        Some("private_app_review_apply")
    );
}

#[test]
fn cad2584_private_app_prepare_eof_returns_outcome_unknown_and_retry_reuses_same_request_id() {
    let env = IsolatedEnv::new();
    let zip_bytes = minimal_zip_bytes(b"good");
    let server = LoopbackServer::start(LoopbackConfig::with_queued_calls(
        zip_bytes.clone(),
        vec![
            CliCallReply::DropEof,
            CliCallReply::Json(
                200,
                json!({
                    "ok": true,
                    "data": {
                        "id": REVIEW_ID,
                        "state": "pending_owner_review"
                    }
                }),
            ),
        ],
    ));
    let (target, slot) =
        setup_mcp_custody_fixture(&env, &server.endpoint, &zip_bytes, None, None, None);

    let err = invoke_private_app_tool_with(
        &env.auth_dir,
        &env.state_dir,
        MESSAGE_ID,
        &target,
        &slot,
        "private_app_prepare",
        &json!({ "package_ref": ARTIFACT_ID }),
        5,
    )
    .expect_err("EOF on private_app_prepare must return OutcomeUnknown");
    assert!(
        matches!(err, Error::OutcomeUnknown(_)),
        "expected OutcomeUnknown on prepare EOF, got: {err:?}"
    );
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("parq_"),
        "OutcomeUnknown prepare message must name deterministic parq_ request_id: {err_msg}"
    );
    let first_calls = server.stats.take_cli_calls();
    assert_eq!(
        first_calls.len(),
        1,
        "timed-out prepare must send at most 1 POST"
    );
    let first_req_id = first_calls[0]
        .pointer("/arguments/body/request_id")
        .and_then(Value::as_str)
        .expect("request_id in first prepare call")
        .to_string();
    assert!(first_req_id.starts_with("parq_"));
    assert!(
        err_msg.contains(&first_req_id),
        "error message '{err_msg}' must cite exact request_id '{first_req_id}'"
    );

    // Retrying with the exact same bound package_ref must reuse the identical request_id.
    let ok_json = invoke_private_app_tool_with(
        &env.auth_dir,
        &env.state_dir,
        MESSAGE_ID,
        &target,
        &slot,
        "private_app_prepare",
        &json!({ "package_ref": ARTIFACT_ID }),
        5,
    )
    .expect("prepare retry with same package_ref must succeed");
    assert!(ok_json.contains(REVIEW_ID));

    let retry_calls = server.stats.take_cli_calls();
    assert_eq!(retry_calls.len(), 1);
    let retry_req_id = retry_calls[0]
        .pointer("/arguments/body/request_id")
        .and_then(Value::as_str)
        .expect("request_id in retry prepare call");
    assert_eq!(
        retry_req_id, first_req_id,
        "retrying prepare on the same package_ref must reuse the exact deterministic request_id"
    );
}

#[test]
fn cad2584_private_app_apply_eof_returns_outcome_unknown_then_reconcile_succeeds_without_replay() {
    let env = IsolatedEnv::new();
    let zip_bytes = minimal_zip_bytes(b"good");
    let sha = format!("sha256:{}", sha256_hex(&zip_bytes));
    let server = LoopbackServer::start(LoopbackConfig::with_queued_calls(
        zip_bytes.clone(),
        vec![
            CliCallReply::DropEof,
            CliCallReply::Json(
                200,
                json!({
                    "ok": true,
                    "data": {
                        "id": REVIEW_ID,
                        "state": "installed",
                        "applied_install_id": "install_42"
                    }
                }),
            ),
        ],
    ));
    let (target, slot) =
        setup_mcp_custody_fixture(&env, &server.endpoint, &zip_bytes, None, None, None);

    let err = invoke_private_app_tool_with(
        &env.auth_dir,
        &env.state_dir,
        MESSAGE_ID,
        &target,
        &slot,
        "private_app_apply",
        &json!({
            "id": REVIEW_ID,
            "digest": sha,
            "zip_sha256": sha
        }),
        5,
    )
    .expect_err("EOF on private_app_apply must return OutcomeUnknown");
    assert!(
        matches!(err, Error::OutcomeUnknown(_)),
        "expected OutcomeUnknown on apply EOF, got: {err:?}"
    );
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("private_app_reconcile") && err_msg.contains(REVIEW_ID),
        "apply OutcomeUnknown must direct private_app_reconcile on {REVIEW_ID}: {err_msg}"
    );
    let apply_calls = server.stats.take_cli_calls();
    assert_eq!(apply_calls.len(), 1, "apply must send at most 1 POST");
    assert_eq!(
        apply_calls[0]
            .pointer("/arguments/tool")
            .and_then(Value::as_str),
        Some("private_app_review_apply")
    );

    let reconciled = invoke_private_app_tool_with(
        &env.auth_dir,
        &env.state_dir,
        MESSAGE_ID,
        &target,
        &slot,
        "private_app_reconcile",
        &json!({ "id": REVIEW_ID }),
        5,
    )
    .expect("private_app_reconcile after uncertain apply must succeed");
    assert!(reconciled.contains("installed"));
    assert!(reconciled.contains("install_42"));

    let reconcile_calls = server.stats.take_cli_calls();
    assert_eq!(reconcile_calls.len(), 1);
    assert_eq!(
        reconcile_calls[0]
            .pointer("/arguments/tool")
            .and_then(Value::as_str),
        Some("private_app_review_reconcile")
    );
}

#[test]
fn cad2584_remote_cli_private_app_read_vs_mutation_eof_classification() {
    let env = IsolatedEnv::new();
    let server = LoopbackServer::start(LoopbackConfig::with_queued_calls(
        minimal_zip_bytes(b"bundle"),
        vec![
            CliCallReply::DropEof,
            CliCallReply::DropEof,
            CliCallReply::DropEof,
            CliCallReply::DropEof,
        ],
    ));
    env.write_cli_login(&server.endpoint, &["cli.read", "cli.write"]);
    let target = remote_target(&server.endpoint);
    let fake_sha = format!("sha256:{}", sha256_hex(b"bundle"));

    // Read-classified platform_call tools: EOF must NOT classify as OutcomeUnknown.
    for (tool, input) in [
        ("private_app_review_status", json!({ "id": "par_1" })),
        ("private_app_review_reconcile", json!({ "id": "par_1" })),
    ] {
        let mut args = Map::new();
        args.insert("operation".into(), json!("call"));
        args.insert("tool".into(), json!(tool));
        args.insert("input".into(), input);

        let deadline = Instant::now() + Duration::from_secs(5);
        let err = call_verb(&target, "platform_call", args, deadline, 5, |_| {})
            .expect_err("EOF without HTTP reply must return an error");
        assert!(
            !matches!(err, Error::OutcomeUnknown(_)),
            "{tool} must be Effect::Read, not Error::OutcomeUnknown: {err:?}"
        );
        assert_ne!(
            err.kind(),
            "unknown",
            "{tool} must not return kind 'unknown': {err:?}"
        );
        assert_eq!(
            server.stats.take_cli_calls().len(),
            1,
            "{tool} must send exactly one /__platform/cli/call"
        );
    }

    // Mutating platform_call tools: EOF after request reached server MUST be OutcomeUnknown
    // and MUST make at most one POST to /__platform/cli/call (no blind retries).
    for (tool, input) in [
        (
            "private_app_review_request",
            json!({
                "url": "https://github.com/example/app/releases/download/v1.0.0/app.zip",
                "request_id": "req-cad2584-1"
            }),
        ),
        (
            "private_app_review_apply",
            json!({
                "id": "par_1",
                "digest": fake_sha,
                "zip_sha256": fake_sha
            }),
        ),
    ] {
        let mut args = Map::new();
        args.insert("operation".into(), json!("call"));
        args.insert("tool".into(), json!(tool));
        args.insert("input".into(), input);

        let deadline = Instant::now() + Duration::from_secs(5);
        let err = call_verb(&target, "platform_call", args, deadline, 5, |_| {})
            .expect_err("EOF on mutating tool must fail as OutcomeUnknown");
        assert!(
            matches!(err, Error::OutcomeUnknown(_)),
            "{tool} must classify EOF after send as Error::OutcomeUnknown, got: {err:?}"
        );
        assert_eq!(
            err.kind(),
            "unknown",
            "{tool} error kind must be 'unknown', got: {err:?}"
        );
        assert_eq!(
            server.stats.take_cli_calls().len(),
            1,
            "{tool} must make at most one POST to /__platform/cli/call (no blind retries)"
        );
    }

    assert!(
        server.stats.authorize_hits.load(Ordering::SeqCst) >= 4,
        "expected /__platform/cli/authorize to be exercised for all 4 calls"
    );
}
