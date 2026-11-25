//! Independent client refusal acceptance suite for CAD-1946.
//!
//! Exercises the real CLI dispatcher (`cli::remote::run`), `delivery::hosted_gate`
//! public API, and `remote_cli` HTTP transport/classifier against unreachable
//! endpoints and ephemeral `std::net::TcpListener` HTTP stubs.

use super::*;
use crate::cli::audit::{AuditAction, IdentitiesAction, ReviewersAction};
use crate::cli::commands::Commands;
use crate::cli::delivery::DeliveryAction;
use cadence_agent::delivery::hosted_gate::{
    approve, approve_delegated_project, approve_scope_project, delivery_check, delivery_enqueue,
    revoke_project, GateBinding,
};
use cadence_agent::issue::cli::{IssueAction, ProjectAction};
use cadence_agent::remote_cli::{CliIdentityPin, RemoteTarget};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

const TEST_ORG: &str = "acme";
const TEST_ORG_ID: &str = "org_synthetic";
const TEST_PROJECT: &str = "proj";
const TEST_ISSUE: &str = "CAD-1";
const TEST_REPO: &str = "owner/repo";
const TEST_PR: u64 = 42;
const HEAD_A: &str = "0123456789abcdef0123456789abcdef01234567";
const HEAD_A_UPPER: &str = "0123456789ABCDEF0123456789ABCDEF01234567";
const HEAD_B: &str = "fedcba9876543210fedcba9876543210fedcba98";
const TEST_ISSUER: &str = "https://issuer.example/synthetic";
const TEST_SUBJECT: &str = "usr_agent";

/// RAII environment isolation locking `RESOLVE_ENV`, wrapping `EnvGuard::bare()`,
/// and restoring any additional managed/grant variables on drop even if a test panics.
struct ScopedEnv {
    extra_saved: HashMap<&'static str, Option<std::ffi::OsString>>,
    _base: crate::cli::org::tests::EnvGuard,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl ScopedEnv {
    fn new() -> Self {
        let lock = crate::cli::org::tests::RESOLVE_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let base = crate::cli::org::tests::EnvGuard::bare();
        let mut extra_saved = HashMap::new();
        for key in [
            "CADENCE_GRANT_TOKEN",
            "CADENCE_MANAGED",
            "CADENCE_AGENT_TOKEN",
        ] {
            extra_saved.insert(key, std::env::var_os(key));
            std::env::remove_var(key);
        }
        Self {
            extra_saved,
            _base: base,
            _lock: lock,
        }
    }

    fn set_var(&mut self, key: &'static str, value: &str) {
        self.extra_saved
            .entry(key)
            .or_insert_with(|| std::env::var_os(key));
        std::env::set_var(key, value);
    }

    fn remove_var(&mut self, key: &'static str) {
        self.extra_saved
            .entry(key)
            .or_insert_with(|| std::env::var_os(key));
        std::env::remove_var(key);
    }

    fn xdg_config_home(&self) -> PathBuf {
        PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").expect("XDG_CONFIG_HOME set"))
    }

    fn xdg_state_home(&self) -> PathBuf {
        PathBuf::from(std::env::var_os("XDG_STATE_HOME").expect("XDG_STATE_HOME set"))
    }

    fn home_dir(&self) -> PathBuf {
        PathBuf::from(std::env::var_os("HOME").expect("HOME set"))
    }

    fn write_credential(
        &self,
        endpoint: &str,
        subject_id: &str,
        issuer: &str,
        capabilities: &[&str],
    ) {
        let dir = self.xdg_config_home().join("cadence").join("remote-auth");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join(format!("cli-{TEST_ORG}.json"));
        let body = json!({
            "version": "hosted-cadence-auth.v1",
            "organization_id": TEST_ORG_ID,
            "organization_slug": TEST_ORG,
            "audience": endpoint,
            "issuer": issuer,
            "subject_id": subject_id,
            "capabilities": capabilities,
            "access_token": format!("hct_{}", "a".repeat(43)),
            "expires_at": 4_000_000_000u64,
        });
        std::fs::write(&path, serde_json::to_vec(&body).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        for (key, value) in self.extra_saved.drain() {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[derive(Clone)]
struct HttpReply {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl HttpReply {
    fn json(status: u16, value: Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: serde_json::to_vec(&value).unwrap(),
        }
    }

    fn text(status: u16, text: &'static str) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: text.as_bytes().to_vec(),
        }
    }
}

type RelayHandler = Arc<dyn Fn(&str, &Value) -> HttpReply + Send + Sync>;

struct StubRelayServer {
    addr: SocketAddr,
    requests: Arc<AtomicUsize>,
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl StubRelayServer {
    fn start(authorize_reply: Option<HttpReply>, on_call: RelayHandler) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let req_counter = Arc::clone(&requests);
        let calls_log = Arc::clone(&calls);
        let stop_flag = Arc::clone(&stop);

        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop_flag.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = stream else {
                    break;
                };
                if stop_flag.load(Ordering::SeqCst) {
                    break;
                }
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                let Some((path, body_bytes)) = read_http_request(&mut stream) else {
                    continue;
                };
                req_counter.fetch_add(1, Ordering::SeqCst);
                let parsed: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
                let reply = if path == "/__platform/cli/authorize" {
                    authorize_reply.clone().unwrap_or_else(|| {
                        HttpReply::json(
                            200,
                            json!({
                                "version": "hosted-cadence-cli.v1",
                                "envelope": "aaaa.bbbb.cccc",
                            }),
                        )
                    })
                } else if path == "/__platform/cli/call" {
                    let verb = parsed["verb"].as_str().unwrap_or_default().to_string();
                    let args = parsed["arguments"].clone();
                    calls_log.lock().unwrap().push((verb.clone(), args.clone()));
                    on_call(&verb, &args)
                } else {
                    HttpReply::json(404, json!({"ok": false, "error": {"code": "not_found"}}))
                };
                write_http_reply(&mut stream, &reply);
            }
        });

        Self {
            addr,
            requests,
            calls,
            stop,
            handle: Some(handle),
        }
    }

    fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }

    fn request_count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn reset_counts(&self) {
        self.requests.store(0, Ordering::SeqCst);
        self.calls.lock().unwrap().clear();
    }

    fn take_calls(&self) -> Vec<(String, Value)> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
}

impl Drop for StubRelayServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn read_http_request(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).ok()? == 0 {
        return None;
    }
    let path = request_line.split_whitespace().nth(1)?.to_string();
    let mut content_length: usize = 0;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().ok()?;
            } else if name.eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
            {
                chunked = true;
            }
        }
    }
    let body = if chunked {
        let mut out = Vec::new();
        loop {
            let mut size_line = String::new();
            if reader.read_line(&mut size_line).ok()? == 0 {
                break;
            }
            let hex = size_line.trim().split(';').next().unwrap_or("");
            let len = usize::from_str_radix(hex, 16).ok()?;
            if len == 0 {
                let mut trailer = String::new();
                let _ = reader.read_line(&mut trailer);
                break;
            }
            let mut chunk = vec![0u8; len];
            reader.read_exact(&mut chunk).ok()?;
            out.extend_from_slice(&chunk);
            let mut crlf = [0u8; 2];
            reader.read_exact(&mut crlf).ok()?;
        }
        out
    } else {
        let mut buf = vec![0u8; content_length];
        if content_length > 0 {
            reader.read_exact(&mut buf).ok()?;
        }
        buf
    };
    Some((path, body))
}

fn write_http_reply(stream: &mut TcpStream, reply: &HttpReply) {
    let reason = match reply.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        503 => "Service Unavailable",
        _ => "Status",
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reply.status,
        reason,
        reply.content_type,
        reply.body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&reply.body);
    let _ = stream.flush();
}

fn target_with(endpoint: &str, identity: Option<CliIdentityPin>) -> RemoteTarget {
    RemoteTarget {
        org: TEST_ORG.into(),
        endpoint: endpoint.into(),
        org_id: TEST_ORG_ID.into(),
        identity,
    }
}

fn agent_pin() -> CliIdentityPin {
    CliIdentityPin {
        subject_id: TEST_SUBJECT.into(),
        issuer: TEST_ISSUER.into(),
    }
}

fn test_binding() -> GateBinding {
    GateBinding::new(TEST_PROJECT, TEST_REPO, TEST_ISSUE, TEST_PR, HEAD_A).unwrap()
}

fn run_cmd(target: RemoteTarget, command: Commands) -> Result<i32> {
    run(target, command, 5, None, None)
}

fn cmd_delivery_enqueue() -> Commands {
    Commands::Delivery {
        action: DeliveryAction::Enqueue {
            project: Some(TEST_PROJECT.into()),
            issue: Some(TEST_ISSUE.into()),
            repo: Some(TEST_REPO.into()),
            pr: TEST_PR,
            head: HEAD_A.into(),
            browser_qa: false,
            approve: None,
        },
    }
}

fn cmd_delivery_check() -> Commands {
    Commands::Delivery {
        action: DeliveryAction::Check {
            project: Some(TEST_PROJECT.into()),
            issue: Some(TEST_ISSUE.into()),
            repo: Some(TEST_REPO.into()),
            pr: TEST_PR,
            head: Some(HEAD_A.into()),
            browser_qa: false,
        },
    }
}

fn cmd_audit_approve_merge() -> Commands {
    Commands::Audit {
        action: Some(AuditAction::Approve {
            pr: Some(TEST_PR),
            head: Some(HEAD_A.into()),
            issue: Some(TEST_ISSUE.into()),
            project: Some(TEST_PROJECT.into()),
            delegated: false,
            scope: None,
            source: "owner in chat".into(),
            repo: Some(TEST_REPO.into()),
            action: "merge".into(),
            id: None,
        }),
        since: None,
        class: None,
        project: None,
        json: false,
        limit: None,
        repo: None,
        notes_dir: None,
        merge_report: None,
    }
}

fn cmd_audit_approve_scope() -> Commands {
    Commands::Audit {
        action: Some(AuditAction::Approve {
            pr: None,
            head: None,
            issue: Some(TEST_ISSUE.into()),
            project: Some(TEST_PROJECT.into()),
            delegated: false,
            scope: None,
            source: "owner in chat".into(),
            repo: None,
            action: "scope".into(),
            id: Some("scope-1".into()),
        }),
        since: None,
        class: None,
        project: None,
        json: false,
        limit: None,
        repo: None,
        notes_dir: None,
        merge_report: None,
    }
}

fn cmd_audit_approve_delegated() -> Commands {
    Commands::Audit {
        action: Some(AuditAction::Approve {
            pr: Some(TEST_PR),
            head: Some(HEAD_A.into()),
            issue: Some(TEST_ISSUE.into()),
            project: Some(TEST_PROJECT.into()),
            delegated: true,
            scope: Some("scope-1".into()),
            source: "designated worker".into(),
            repo: Some(TEST_REPO.into()),
            action: "merge".into(),
            id: None,
        }),
        since: None,
        class: None,
        project: None,
        json: false,
        limit: None,
        repo: None,
        notes_dir: None,
        merge_report: None,
    }
}

fn cmd_audit_designate(revoke: bool) -> Commands {
    Commands::Audit {
        action: Some(AuditAction::Designate {
            alias: "worker-1".into(),
            project: TEST_PROJECT.into(),
            source: "owner in chat".into(),
            revoke,
        }),
        since: None,
        class: None,
        project: None,
        json: false,
        limit: None,
        repo: None,
        notes_dir: None,
        merge_report: None,
    }
}

fn cmd_audit_revoke() -> Commands {
    Commands::Audit {
        action: Some(AuditAction::Revoke {
            id: "appr-1".into(),
            project: Some(TEST_PROJECT.into()),
            source: None,
            reason: "superseded".into(),
        }),
        since: None,
        class: None,
        project: None,
        json: false,
        limit: None,
        repo: None,
        notes_dir: None,
        merge_report: None,
    }
}

fn valid_stub_reply(verb: &str) -> HttpReply {
    match verb {
        "delivery_check" => HttpReply::json(
            200,
            json!({
                "state": "pass",
                "ok": true,
                "project": TEST_PROJECT,
                "issue": TEST_ISSUE,
                "repo": TEST_REPO,
                "pr": TEST_PR,
                "head": HEAD_A,
                "reasons": [],
            }),
        ),
        "audit_approve_delegated_project" => HttpReply::json(
            200,
            json!({
                "state": "recorded",
                "duplicate": false,
                "approval": {
                    "approval_id": "appr-delegated-1",
                    "project": TEST_PROJECT,
                    "issue": TEST_ISSUE,
                    "head_sha": HEAD_A,
                    "scope": {
                        "repo": TEST_REPO,
                        "pr": TEST_PR,
                    },
                },
            }),
        ),
        _ => HttpReply::json(200, json!({"state": "unexpected_verb", "verb": verb})),
    }
}

fn assert_managed_refused_pre_network(err: &Error, label: &str) {
    let msg = err.to_string();
    assert!(
        msg.contains("managed caller") && msg.contains("Nothing was sent"),
        "{label}: expected pre-network managed caller refusal, got: {msg}"
    );
}

// ---- Scenario 1: Managed caller pre-network refusal ----

#[test]
fn managed_caller_refuses_owner_gate_writes_before_http_and_permits_delegated_and_check() {
    let mut env = ScopedEnv::new();
    let server = StubRelayServer::start(None, Arc::new(|verb, _args| valid_stub_reply(verb)));
    let endpoint = server.endpoint();
    env.write_credential(
        &endpoint,
        TEST_SUBJECT,
        TEST_ISSUER,
        &["cli.read", "cli.write", "audit.approve"],
    );

    let owner_only = [
        ("delivery enqueue", cmd_delivery_enqueue as fn() -> Commands),
        ("audit approve", cmd_audit_approve_merge),
        ("audit approve --action scope", cmd_audit_approve_scope),
        ("audit designate", || cmd_audit_designate(false)),
        ("audit designate --revoke", || cmd_audit_designate(true)),
        ("audit revoke", cmd_audit_revoke),
    ];

    // Check every managed-caller signal: pinned agent identity, CADENCE_ALIAS (+ CADENCE_MANAGED=1),
    // and CADENCE_GRANT_TOKEN.
    enum Mode {
        IdentityPin,
        Alias,
        GrantToken,
    }
    for mode in [Mode::IdentityPin, Mode::Alias, Mode::GrantToken] {
        env.remove_var("CADENCE_ALIAS");
        env.remove_var("CADENCE_GRANT_TOKEN");
        env.remove_var("CADENCE_MANAGED");
        let pin = match mode {
            Mode::IdentityPin => Some(agent_pin()),
            Mode::Alias => {
                env.set_var("CADENCE_ALIAS", "synthetic-worker");
                env.set_var("CADENCE_MANAGED", "1");
                None
            }
            Mode::GrantToken => {
                env.set_var("CADENCE_GRANT_TOKEN", "grant_synthetic");
                None
            }
        };

        server.reset_counts();
        for (label, build_cmd) in owner_only {
            let err = run_cmd(target_with(&endpoint, pin.clone()), build_cmd()).unwrap_err();
            assert_managed_refused_pre_network(&err, label);
        }
        assert_eq!(
            server.request_count(),
            0,
            "managed caller owner-only commands must send zero HTTP requests"
        );

        // Permitted paths under the same managed caller: `audit approve --delegated` and `delivery check`.
        server.reset_counts();
        assert_eq!(
            run_cmd(
                target_with(&endpoint, pin.clone()),
                cmd_audit_approve_delegated()
            )
            .unwrap(),
            0
        );
        assert_eq!(
            run_cmd(target_with(&endpoint, pin), cmd_delivery_check()).unwrap(),
            0
        );
        let calls = server.take_calls();
        let verbs: Vec<&str> = calls.iter().map(|(v, _)| v.as_str()).collect();
        assert_eq!(
            verbs,
            vec!["audit_approve_delegated_project", "delivery_check"]
        );
    }

    // Also prove against an unreachable endpoint (`https://127.0.0.1:1`) that owner commands return
    // the local managed-caller refusal while `delegated` and `delivery check` pass `refuse_managed`
    // and fail only at credential/transport.
    env.remove_var("CADENCE_GRANT_TOKEN");
    env.set_var("CADENCE_ALIAS", "synthetic-worker");
    env.set_var("CADENCE_MANAGED", "1");
    env.write_credential(
        "https://127.0.0.1:1",
        TEST_SUBJECT,
        TEST_ISSUER,
        &["cli.read", "cli.write", "audit.approve"],
    );
    for (label, build_cmd) in owner_only {
        let err = run_cmd(
            target_with("https://127.0.0.1:1", Some(agent_pin())),
            build_cmd(),
        )
        .unwrap_err();
        assert_managed_refused_pre_network(&err, label);
    }
    for (label, build_cmd) in [
        (
            "audit approve --delegated",
            cmd_audit_approve_delegated as fn() -> Commands,
        ),
        ("delivery check", cmd_delivery_check),
    ] {
        let err = run_cmd(
            target_with("https://127.0.0.1:1", Some(agent_pin())),
            build_cmd(),
        )
        .unwrap_err();
        assert!(
            !err.to_string().contains("managed caller"),
            "{label} must pass refuse_managed and reach transport: {err}"
        );
    }
}

#[test]
fn managed_producer_source_is_refused_before_any_http_request() {
    let mut env = ScopedEnv::new();
    let server = StubRelayServer::start(None, Arc::new(|verb, _| valid_stub_reply(verb)));
    let endpoint = server.endpoint();
    env.write_credential(
        &endpoint,
        TEST_SUBJECT,
        TEST_ISSUER,
        &["cli.read", "cli.write", "audit.approve"],
    );
    env.set_var("CADENCE_ALIAS", "synthetic-worker");
    env.set_var("CADENCE_MANAGED", "1");

    let command = Commands::Audit {
        action: Some(AuditAction::Producer {
            project: TEST_PROJECT.into(),
            repo: TEST_REPO.into(),
            issue: TEST_ISSUE.into(),
            pr: TEST_PR,
            head: HEAD_A.into(),
            source: Some("owner in chat".into()),
            human_only: true,
            profile: vec![],
        }),
        since: None,
        class: None,
        project: None,
        json: false,
        limit: None,
        repo: None,
        notes_dir: None,
        merge_report: None,
    };
    let err = run_cmd(target_with(&endpoint, None), command).unwrap_err();
    assert_managed_refused_pre_network(&err, "audit producer --source");
    assert_eq!(server.request_count(), 0);
    assert!(server.take_calls().is_empty());
}

#[test]
fn stale_delivery_apply_sends_exact_fence_once_and_returns_conflict() {
    let env = ScopedEnv::new();
    // This relay models a board conflict; the actual board's no-write behavior
    // must be established independently by the AOS-744 server fixture.
    let server = StubRelayServer::start(
        None,
        Arc::new(|_, _| {
            HttpReply::json(
                409,
                json!({
                    "ok": false,
                    "error": {"code": "revision_conflict", "message": "Tenant remote call refused."},
                }),
            )
        }),
    );
    let endpoint = server.endpoint();
    env.write_credential(
        &endpoint,
        TEST_SUBJECT,
        TEST_ISSUER,
        &["cli.read", "cli.write", "audit.approve"],
    );
    let digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let command = Commands::Issue {
        action: IssueAction::Project {
            action: ProjectAction::Delivery {
                key: TEST_PROJECT.into(),
                repos: vec![TEST_REPO.into()],
                delivery_file: None,
                assigned_reviews_file: None,
                show: false,
                preview: false,
                expect_rev: Some(7),
                preview_digest: Some(digest.into()),
            },
        },
    };
    let err = run_cmd(target_with(&endpoint, None), command).unwrap_err();
    let message = err.to_string();
    assert_eq!(err.kind(), "rejected", "{message}");
    assert!(message.contains("revision_conflict"), "{message}");
    let calls = server.take_calls();
    assert_eq!(
        calls,
        vec![(
            "delivery_policy_set".into(),
            json!({
                "project": TEST_PROJECT,
                "repos": [TEST_REPO],
                "expected_revision": 7,
                "preview_digest": digest,
            }),
        )],
        "the CLI must not retry the mutation without the fence"
    );
    assert_eq!(
        server.request_count(),
        2,
        "one authorization and one mutation"
    );
}

#[test]
fn unfenced_profile_policy_is_refused_but_legacy_policy_still_dispatches() {
    let env = ScopedEnv::new();
    let server = StubRelayServer::start(
        None,
        Arc::new(|_, _| {
            HttpReply::json(
                409,
                json!({
                    "ok": false,
                    "error": {"code": "revision_conflict", "message": "fixture refusal"},
                }),
            )
        }),
    );
    let endpoint = server.endpoint();
    env.write_credential(
        &endpoint,
        TEST_SUBJECT,
        TEST_ISSUER,
        &["cli.read", "cli.write", "audit.approve"],
    );
    let target = target_with(&endpoint, None);
    let repos = vec![TEST_REPO.to_string()];
    let profile_policy = "delivery:\n  reviews:\n    standards: {kind: agent, focus: standards, profile: ap_standards-123}\n  risk:\n    - require: [standards]\n";
    let err = cadence_agent::delivery::hosted_gate::set_delivery(
        &target,
        5,
        TEST_PROJECT,
        &repos,
        Some(profile_policy),
        None,
        None,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains(
            "profile-backed delivery policy requires --expect-rev and --preview-digest from --preview"
        ),
        "{err}"
    );
    assert_eq!(server.request_count(), 0);
    assert!(server.take_calls().is_empty());

    let legacy_policy = "delivery:\n  reviews:\n    standards: {kind: agent, focus: standards}\n  risk:\n    - require: [standards]\n";
    let err = cadence_agent::delivery::hosted_gate::set_delivery(
        &target,
        5,
        TEST_PROJECT,
        &repos,
        Some(legacy_policy),
        None,
        None,
    )
    .unwrap_err();
    assert!(err.to_string().contains("revision_conflict"), "{err}");
    assert_eq!(
        server.take_calls(),
        vec![(
            "delivery_policy_set".into(),
            json!({
                "project": TEST_PROJECT,
                "repos": [TEST_REPO],
                "delivery": legacy_policy,
            }),
        )]
    );
    assert_eq!(server.request_count(), 2);
}

#[test]
fn assigned_review_policy_apply_refuses_legacy_preview_and_preserves_explicit_disable() {
    let env = ScopedEnv::new();
    let digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    for supported in [false, true] {
        let server = StubRelayServer::start(
            None,
            Arc::new(move |verb, _| {
                if verb == "delivery_policy" {
                    let mut proposal = json!({"repos": [TEST_REPO], "delivery": null});
                    if supported {
                        proposal["assigned_reviews"] = Value::Null;
                    }
                    HttpReply::json(
                        200,
                        json!({
                            "state": "preview", "project": TEST_PROJECT,
                            "expected_revision": 7, "preview_digest": digest, "proposal": proposal,
                        }),
                    )
                } else {
                    HttpReply::json(
                        200,
                        json!({
                            "state": "recorded", "project": TEST_PROJECT, "repos": [TEST_REPO],
                            "delivery": null, "revision": 8, "assigned_reviews": null,
                        }),
                    )
                }
            }),
        );
        let endpoint = server.endpoint();
        env.write_credential(
            &endpoint,
            TEST_SUBJECT,
            TEST_ISSUER,
            &["cli.read", "cli.write", "audit.approve"],
        );
        let result = cadence_agent::delivery::hosted_gate::set_delivery(
            &target_with(&endpoint, None),
            5,
            TEST_PROJECT,
            &[TEST_REPO.into()],
            None,
            Some((7, digest)),
            Some(&Value::Null),
        );
        let mut expected = vec![(
            "delivery_policy".to_owned(),
            json!({"project": TEST_PROJECT, "proposal": {"repos": [TEST_REPO], "assigned_reviews": null}}),
        )];
        if supported {
            assert_eq!(result.unwrap().get("assigned_reviews"), Some(&Value::Null));
            expected.push((
                "delivery_policy_set".to_owned(),
                json!({"project": TEST_PROJECT, "repos": [TEST_REPO], "assigned_reviews": null,
                    "expected_revision": 7, "preview_digest": digest}),
            ));
        } else {
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("did not preview these exact"));
        }
        assert_eq!(server.take_calls(), expected);
    }
}

// ---- Scenario 2: Foreign/forged HTTP 200 receipt rejection ----

fn call_with_single_reply(reply: Value, f: impl FnOnce(&RemoteTarget) -> Result<()>) -> Result<()> {
    let env = ScopedEnv::new();
    let payload = Arc::new(reply);
    let server = StubRelayServer::start(
        None,
        Arc::new(move |_verb, _args| HttpReply::json(200, (*payload).clone())),
    );
    let endpoint = server.endpoint();
    env.write_credential(
        &endpoint,
        TEST_SUBJECT,
        TEST_ISSUER,
        &["cli.read", "cli.write", "audit.approve"],
    );
    let target = target_with(&endpoint, None);
    f(&target)
}

#[test]
fn forged_http_200_receipts_for_audit_approve_are_rejected_as_unknown() {
    let binding = test_binding();
    let valid = json!({
        "state": "recorded",
        "action": "merge",
        "project": TEST_PROJECT,
        "issue": TEST_ISSUE,
        "head_sha": HEAD_A,
        "scope": {"repo": TEST_REPO, "pr": TEST_PR},
        "source": "owner in chat",
        "approval_id": "appr-1",
        "recorded_via": "cli",
        "duplicate": false,
    });
    call_with_single_reply(valid.clone(), |t| {
        approve(t, 5, &binding, "owner in chat").map(|_| ())
    })
    .expect("baseline valid approve receipt must succeed");

    let forged_cases: Vec<(&str, Value)> = vec![
        ("wrong project", {
            let mut v = valid.clone();
            v["project"] = json!("other-proj");
            v
        }),
        ("project case mismatch", {
            let mut v = valid.clone();
            v["project"] = json!("PROJ");
            v
        }),
        ("wrong issue", {
            let mut v = valid.clone();
            v["issue"] = json!("CAD-999");
            v
        }),
        ("issue case mismatch", {
            let mut v = valid.clone();
            v["issue"] = json!("cad-1");
            v
        }),
        ("wrong repo", {
            let mut v = valid.clone();
            v["scope"]["repo"] = json!("other/repo");
            v
        }),
        ("wrong pr", {
            let mut v = valid.clone();
            v["scope"]["pr"] = json!(999);
            v
        }),
        ("wrong head_sha", {
            let mut v = valid.clone();
            v["head_sha"] = json!(HEAD_B);
            v
        }),
        ("head_sha case mismatch", {
            let mut v = valid.clone();
            v["head_sha"] = json!(HEAD_A_UPPER);
            v
        }),
        ("wrong source", {
            let mut v = valid.clone();
            v["source"] = json!("forged source");
            v
        }),
        ("wrong state", {
            let mut v = valid.clone();
            v["state"] = json!("pending");
            v
        }),
        ("empty approval_id", {
            let mut v = valid.clone();
            v["approval_id"] = json!("");
            v
        }),
    ];

    for (label, payload) in forged_cases {
        let err = call_with_single_reply(payload, |t| {
            approve(t, 5, &binding, "owner in chat").map(|_| ())
        })
        .unwrap_err();
        assert_eq!(
            err.kind(),
            "unknown",
            "audit_approve_project ({label}) must fail as unknown, got {}: {err}",
            err.kind()
        );
    }
}

#[test]
fn forged_http_200_receipts_for_delivery_enqueue_are_rejected_as_unknown() {
    let valid = json!({
        "state": "enqueued",
        "ok": true,
        "project": TEST_PROJECT,
        "issue": TEST_ISSUE,
        "repo": TEST_REPO,
        "pr": TEST_PR,
        "head": HEAD_A,
        "evidence_id": "ev_123",
        "queued": true,
        "auto_merge_enabled": true,
    });
    call_with_single_reply(valid.clone(), |t| {
        delivery_enqueue(
            t,
            5,
            Some(TEST_PROJECT),
            Some(TEST_ISSUE),
            TEST_REPO,
            TEST_PR,
            HEAD_A,
            false,
            None,
        )
        .map(|_| ())
    })
    .expect("baseline valid delivery_enqueue receipt must succeed");

    let forged_cases: Vec<(&str, Value)> = vec![
        ("refused state with queued=false", {
            let mut v = valid.clone();
            v["state"] = json!("refused");
            v["ok"] = json!(false);
            v["queued"] = json!(false);
            v["auto_merge_enabled"] = json!(false);
            v
        }),
        ("ok=false", {
            let mut v = valid.clone();
            v["ok"] = json!(false);
            v
        }),
        ("wrong project", {
            let mut v = valid.clone();
            v["project"] = json!("other-proj");
            v
        }),
        ("missing project", {
            let mut v = valid.clone();
            v.as_object_mut().unwrap().remove("project");
            v
        }),
        ("wrong issue", {
            let mut v = valid.clone();
            v["issue"] = json!("CAD-999");
            v
        }),
        ("missing issue", {
            let mut v = valid.clone();
            v.as_object_mut().unwrap().remove("issue");
            v
        }),
        ("wrong repo", {
            let mut v = valid.clone();
            v["repo"] = json!("evil/repo");
            v
        }),
        ("wrong pr", {
            let mut v = valid.clone();
            v["pr"] = json!(77);
            v
        }),
        ("wrong head", {
            let mut v = valid.clone();
            v["head"] = json!(HEAD_B);
            v
        }),
        ("missing evidence_id", {
            let mut v = valid.clone();
            v.as_object_mut().unwrap().remove("evidence_id");
            v
        }),
        ("empty evidence_id", {
            let mut v = valid.clone();
            v["evidence_id"] = json!("   ");
            v
        }),
        ("non-bool queued", {
            let mut v = valid.clone();
            v["queued"] = json!("yes");
            v
        }),
    ];

    for (label, payload) in forged_cases {
        let err = call_with_single_reply(payload, |t| {
            delivery_enqueue(
                t,
                5,
                Some(TEST_PROJECT),
                Some(TEST_ISSUE),
                TEST_REPO,
                TEST_PR,
                HEAD_A,
                false,
                None,
            )
            .map(|_| ())
        })
        .unwrap_err();
        assert_eq!(
            err.kind(),
            "unknown",
            "delivery_enqueue ({label}) must fail as unknown, got {}: {err}",
            err.kind()
        );
    }
}

#[test]
fn forged_http_200_receipts_for_scope_delegated_and_revoke_are_rejected_as_unknown() {
    // 1. audit_approve_scope_project
    let valid_scope = json!({
        "state": "recorded",
        "duplicate": false,
        "project": TEST_PROJECT,
        "issue": TEST_ISSUE,
        "source": "owner in chat",
        "approval_id": "scope-1",
        "scope_digest": "sha256:abc",
        "recorded_via": "cli",
    });
    call_with_single_reply(valid_scope.clone(), |t| {
        approve_scope_project(
            t,
            5,
            TEST_PROJECT,
            TEST_ISSUE,
            "owner in chat",
            Some("scope-1"),
        )
        .map(|_| ())
    })
    .expect("baseline scope approval receipt must succeed");

    for (label, payload) in [
        ("wrong project", {
            let mut v = valid_scope.clone();
            v["project"] = json!("other-proj");
            v
        }),
        ("wrong issue", {
            let mut v = valid_scope.clone();
            v["issue"] = json!("CAD-99");
            v
        }),
        ("wrong id", {
            let mut v = valid_scope.clone();
            v["approval_id"] = json!("scope-other");
            v
        }),
        ("wrong state", {
            let mut v = valid_scope.clone();
            v["state"] = json!("pending");
            v
        }),
    ] {
        let err = call_with_single_reply(payload, |t| {
            approve_scope_project(
                t,
                5,
                TEST_PROJECT,
                TEST_ISSUE,
                "owner in chat",
                Some("scope-1"),
            )
            .map(|_| ())
        })
        .unwrap_err();
        assert_eq!(err.kind(), "unknown", "scope ({label})");
    }

    // 2. audit_approve_delegated_project
    let valid_delegated = json!({
        "state": "recorded",
        "duplicate": false,
        "approval": {
            "approval_id": "appr-del-1",
            "project": TEST_PROJECT,
            "issue": TEST_ISSUE,
            "head_sha": HEAD_A,
            "scope": {"repo": TEST_REPO, "pr": TEST_PR},
        },
    });
    call_with_single_reply(valid_delegated.clone(), |t| {
        approve_delegated_project(
            t,
            5,
            Some(TEST_PROJECT),
            Some(TEST_ISSUE),
            TEST_REPO,
            TEST_PR,
            HEAD_A,
            Some("scope-1"),
            "designated worker",
        )
        .map(|_| ())
    })
    .expect("baseline delegated approval receipt must succeed");

    for (label, payload) in [
        ("wrong project", {
            let mut v = valid_delegated.clone();
            v["approval"]["project"] = json!("other-proj");
            v
        }),
        ("wrong issue", {
            let mut v = valid_delegated.clone();
            v["approval"]["issue"] = json!("CAD-99");
            v
        }),
        ("wrong repo", {
            let mut v = valid_delegated.clone();
            v["approval"]["scope"]["repo"] = json!("other/repo");
            v
        }),
        ("wrong pr", {
            let mut v = valid_delegated.clone();
            v["approval"]["scope"]["pr"] = json!(99);
            v
        }),
        ("wrong head_sha", {
            let mut v = valid_delegated.clone();
            v["approval"]["head_sha"] = json!(HEAD_B);
            v
        }),
        ("empty approval_id", {
            let mut v = valid_delegated.clone();
            v["approval"]["approval_id"] = json!("");
            v
        }),
        ("wrong state", {
            let mut v = valid_delegated.clone();
            v["state"] = json!("refused");
            v
        }),
    ] {
        let err = call_with_single_reply(payload, |t| {
            approve_delegated_project(
                t,
                5,
                Some(TEST_PROJECT),
                Some(TEST_ISSUE),
                TEST_REPO,
                TEST_PR,
                HEAD_A,
                Some("scope-1"),
                "designated worker",
            )
            .map(|_| ())
        })
        .unwrap_err();
        assert_eq!(err.kind(), "unknown", "delegated ({label})");
    }

    // 3. audit_revoke_project
    let valid_revoke = json!({
        "state": "revoked",
        "project": TEST_PROJECT,
        "approval_id": "appr-1",
        "revoked_at": "2026-11-15T00:00:00Z",
    });
    call_with_single_reply(valid_revoke.clone(), |t| {
        revoke_project(t, 5, Some(TEST_PROJECT), "appr-1", "superseded").map(|_| ())
    })
    .expect("baseline revoke receipt must succeed");

    for (label, payload) in [
        ("wrong project", {
            let mut v = valid_revoke.clone();
            v["project"] = json!("other-proj");
            v
        }),
        ("wrong id", {
            let mut v = valid_revoke.clone();
            v["approval_id"] = json!("appr-other");
            v
        }),
        ("wrong state", {
            let mut v = valid_revoke.clone();
            v["state"] = json!("in-force");
            v
        }),
        ("missing revoked_at", {
            let mut v = valid_revoke.clone();
            v.as_object_mut().unwrap().remove("revoked_at");
            v
        }),
    ] {
        let err = call_with_single_reply(payload, |t| {
            revoke_project(t, 5, Some(TEST_PROJECT), "appr-1", "superseded").map(|_| ())
        })
        .unwrap_err();
        assert_eq!(err.kind(), "unknown", "revoke ({label})");
    }
}

#[test]
fn forged_http_200_receipt_for_delivery_check_mismatch_is_rejected() {
    let valid = json!({
        "state": "pass",
        "ok": true,
        "project": TEST_PROJECT,
        "issue": TEST_ISSUE,
        "repo": TEST_REPO,
        "pr": TEST_PR,
        "head": HEAD_A,
        "reasons": [],
    });
    call_with_single_reply(valid.clone(), |t| {
        delivery_check(
            t,
            5,
            Some(TEST_PROJECT),
            Some(TEST_ISSUE),
            TEST_REPO,
            TEST_PR,
            Some(HEAD_A),
            false,
        )
        .map(|_| ())
    })
    .expect("baseline valid delivery_check receipt must succeed");
    for (label, payload) in [
        ("wrong project", {
            let mut v = valid.clone();
            v["project"] = json!("other-proj");
            v
        }),
        ("missing project", {
            let mut v = valid.clone();
            v.as_object_mut().unwrap().remove("project");
            v
        }),
        ("wrong issue", {
            let mut v = valid.clone();
            v["issue"] = json!("CAD-999");
            v
        }),
        ("missing issue", {
            let mut v = valid.clone();
            v.as_object_mut().unwrap().remove("issue");
            v
        }),
        ("wrong repo", {
            let mut v = valid.clone();
            v["repo"] = json!("other/repo");
            v
        }),
        ("wrong pr", {
            let mut v = valid.clone();
            v["pr"] = json!(999);
            v
        }),
        ("wrong head", {
            let mut v = valid.clone();
            v["head"] = json!(HEAD_B);
            v
        }),
        ("inconsistent state/ok", {
            let mut v = valid.clone();
            v["ok"] = json!(false);
            v
        }),
    ] {
        let err = call_with_single_reply(payload, |t| {
            delivery_check(
                t,
                5,
                Some(TEST_PROJECT),
                Some(TEST_ISSUE),
                TEST_REPO,
                TEST_PR,
                Some(HEAD_A),
                false,
            )
            .map(|_| ())
        })
        .unwrap_err();
        assert_eq!(err.kind(), "rejected", "delivery_check ({label})");
    }
}

// ---- Scenario 3: Domain 409 preservation & delivery check refused exit semantics ----

#[test]
fn delivery_check_refused_returns_nonzero_or_err() {
    let env = ScopedEnv::new();
    let mode = Arc::new(Mutex::new(0usize));
    let mode_ref = Arc::clone(&mode);
    let server = StubRelayServer::start(
        None,
        Arc::new(move |_verb, _args| match *mode_ref.lock().unwrap() {
            // Contract refusal payload -> exit code 1
            0 => HttpReply::json(
                200,
                json!({
                    "state": "refused",
                    "ok": false,
                    "project": TEST_PROJECT,
                    "issue": TEST_ISSUE,
                    "repo": TEST_REPO,
                    "pr": TEST_PR,
                    "head": HEAD_A,
                    "reasons": ["need 2 reviews"],
                }),
            ),
            // Malformed `status: "refused"` without `state` -> Err
            1 => HttpReply::json(
                200,
                json!({
                    "status": "refused",
                    "ok": false,
                    "project": TEST_PROJECT,
                    "issue": TEST_ISSUE,
                    "repo": TEST_REPO,
                    "pr": TEST_PR,
                    "head": HEAD_A,
                    "reasons": ["need 2 reviews"],
                }),
            ),
            // Inconsistent `state: "refused", ok: true` -> Err
            _ => HttpReply::json(
                200,
                json!({
                    "state": "refused",
                    "ok": true,
                    "project": TEST_PROJECT,
                    "issue": TEST_ISSUE,
                    "repo": TEST_REPO,
                    "pr": TEST_PR,
                    "head": HEAD_A,
                    "reasons": [],
                }),
            ),
        }),
    );
    let endpoint = server.endpoint();
    env.write_credential(
        &endpoint,
        TEST_SUBJECT,
        TEST_ISSUER,
        &["cli.read", "cli.write"],
    );
    let target = target_with(&endpoint, None);

    *mode.lock().unwrap() = 0;
    let exit_code = run_cmd(target.clone(), cmd_delivery_check()).unwrap();
    assert_eq!(exit_code, 1, "delivery check refused must exit non-zero");

    *mode.lock().unwrap() = 1;
    assert!(
        run_cmd(target.clone(), cmd_delivery_check()).is_err(),
        "status: \"refused\" without contract state must error"
    );

    *mode.lock().unwrap() = 2;
    assert!(
        run_cmd(target, cmd_delivery_check()).is_err(),
        "state: \"refused\" with ok: true must error"
    );
}

#[test]
fn domain_409_codes_preserve_domain_refusal_instead_of_container_required() {
    let env = ScopedEnv::new();
    let next_code = Arc::new(Mutex::new("gate_refused".to_string()));
    let next_code_ref = Arc::clone(&next_code);
    let server = StubRelayServer::start(
        None,
        Arc::new(move |_verb, _args| {
            let code = next_code_ref.lock().unwrap().clone();
            HttpReply::json(
                409,
                json!({
                    "ok": false,
                    "error": {
                        "code": code,
                        "message": "domain refusal detail",
                    }
                }),
            )
        }),
    );
    let endpoint = server.endpoint();
    env.write_credential(
        &endpoint,
        TEST_SUBJECT,
        TEST_ISSUER,
        &["cli.read", "cli.write", "audit.approve"],
    );
    let target = target_with(&endpoint, None);

    type EndpointCall = fn(&RemoteTarget) -> Result<()>;
    let cases: [(&str, EndpointCall); 7] = [
        ("gate_refused", |t| {
            delivery_enqueue(
                t,
                5,
                Some(TEST_PROJECT),
                Some(TEST_ISSUE),
                TEST_REPO,
                TEST_PR,
                HEAD_A,
                false,
                None,
            )
            .map(|_| ())
        }),
        ("head_moved", |t| {
            delivery_enqueue(
                t,
                5,
                Some(TEST_PROJECT),
                Some(TEST_ISSUE),
                TEST_REPO,
                TEST_PR,
                HEAD_A,
                false,
                None,
            )
            .map(|_| ())
        }),
        ("already_revoked", |t| {
            revoke_project(t, 5, Some(TEST_PROJECT), "appr-1", "duplicate").map(|_| ())
        }),
        ("not_delegable", |t| {
            approve_delegated_project(
                t,
                5,
                Some(TEST_PROJECT),
                Some(TEST_ISSUE),
                TEST_REPO,
                TEST_PR,
                HEAD_A,
                Some("scope-1"),
                "worker",
            )
            .map(|_| ())
        }),
        ("scope_approval_required", |t| {
            approve_delegated_project(
                t,
                5,
                Some(TEST_PROJECT),
                Some(TEST_ISSUE),
                TEST_REPO,
                TEST_PR,
                HEAD_A,
                None,
                "worker",
            )
            .map(|_| ())
        }),
        ("approval_revoked", |t| {
            delivery_check(
                t,
                5,
                Some(TEST_PROJECT),
                Some(TEST_ISSUE),
                TEST_REPO,
                TEST_PR,
                Some(HEAD_A),
                false,
            )
            .map(|_| ())
        }),
        ("project_archived", |t| {
            approve_scope_project(t, 5, TEST_PROJECT, TEST_ISSUE, "owner", None).map(|_| ())
        }),
    ];

    for (code, invoke) in cases {
        *next_code.lock().unwrap() = code.to_string();
        let err = invoke(&target).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(code) && msg.contains("HTTP 409"),
            "409 {code} must preserve domain code and status, got: {msg}"
        );
        assert!(
            !msg.contains("container_required") && !msg.contains("could not serve"),
            "409 {code} must not collapse into container_required: {msg}"
        );
    }

    // Contrast: 409 `container_required` on a v2 gate verb DOES report tenant-cannot-serve.
    *next_code.lock().unwrap() = "container_required".to_string();
    let err = delivery_enqueue(
        &target,
        5,
        Some(TEST_PROJECT),
        Some(TEST_ISSUE),
        TEST_REPO,
        TEST_PR,
        HEAD_A,
        false,
        None,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("container_required") && msg.contains("could not serve"),
        "409 container_required must report tenant-cannot-serve, got: {msg}"
    );
}

// ---- Scenario 4: Unreachable / HTTP 503 fail-closed (no local SQLite/git-notes fallback) ----

fn has_local_store_artifacts(root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if has_local_store_artifacts(&path) {
                return true;
            }
        } else if name.ends_with(".db")
            || name.ends_with(".sqlite")
            || name.ends_with(".sqlite3")
            || name.ends_with(".sock")
            || name.ends_with(".note")
        {
            return true;
        }
    }
    false
}

fn all_hosted_delivery_and_audit_commands(notes_dir: &Path) -> Vec<(&'static str, Commands)> {
    vec![
        ("delivery check", cmd_delivery_check()),
        ("delivery enqueue", cmd_delivery_enqueue()),
        (
            "audit approval",
            Commands::Audit {
                action: Some(AuditAction::Approval {
                    pr: TEST_PR,
                    head: HEAD_A.into(),
                    issue: Some(TEST_ISSUE.into()),
                    project: Some(TEST_PROJECT.into()),
                    repo: Some(TEST_REPO.into()),
                }),
                since: None,
                class: None,
                project: None,
                json: false,
                limit: None,
                repo: None,
                notes_dir: None,
                merge_report: None,
            },
        ),
        (
            "audit verdicts",
            Commands::Audit {
                action: Some(AuditAction::Verdicts {
                    issue: TEST_ISSUE.into(),
                    pr: TEST_PR,
                    head: HEAD_A.into(),
                    notes_dir: Some(notes_dir.to_path_buf()),
                    repo: Some(TEST_REPO.into()),
                    project: Some(TEST_PROJECT.into()),
                    body: false,
                }),
                since: None,
                class: None,
                project: None,
                json: false,
                limit: None,
                repo: None,
                notes_dir: None,
                merge_report: None,
            },
        ),
        (
            "audit verdict",
            Commands::Audit {
                action: Some(AuditAction::Verdict {
                    issue: TEST_ISSUE.into(),
                    pr: TEST_PR,
                    head: HEAD_A.into(),
                    kind: "combined".into(),
                    outcome: "pass".into(),
                    title_outcome: None,
                    section_outcome: None,
                    risk: "auto".into(),
                    file: None,
                    text: Some("verified cleanly".into()),
                    repo: Some(TEST_REPO.into()),
                    project: Some(TEST_PROJECT.into()),
                }),
                since: None,
                class: None,
                project: None,
                json: false,
                limit: None,
                repo: None,
                notes_dir: None,
                merge_report: None,
            },
        ),
        (
            "audit scope (project)",
            Commands::Audit {
                action: Some(AuditAction::Scope {
                    issue: TEST_ISSUE.into(),
                    project: Some(TEST_PROJECT.into()),
                }),
                since: None,
                class: None,
                project: None,
                json: false,
                limit: None,
                repo: None,
                notes_dir: None,
                merge_report: None,
            },
        ),
        (
            "audit scope (legacy)",
            Commands::Audit {
                action: Some(AuditAction::Scope {
                    issue: TEST_ISSUE.into(),
                    project: None,
                }),
                since: None,
                class: None,
                project: None,
                json: false,
                limit: None,
                repo: None,
                notes_dir: None,
                merge_report: None,
            },
        ),
        ("audit approve", cmd_audit_approve_merge()),
        ("audit approve --action scope", cmd_audit_approve_scope()),
        ("audit approve --delegated", cmd_audit_approve_delegated()),
        ("audit designate", cmd_audit_designate(false)),
        (
            "audit designations",
            Commands::Audit {
                action: Some(AuditAction::Designations {
                    project: Some(TEST_PROJECT.into()),
                }),
                since: None,
                class: None,
                project: None,
                json: false,
                limit: None,
                repo: None,
                notes_dir: None,
                merge_report: None,
            },
        ),
        (
            "audit digest",
            Commands::Audit {
                action: Some(AuditAction::Digest {
                    since: Some("24h".into()),
                    project: Some(TEST_PROJECT.into()),
                }),
                since: None,
                class: None,
                project: None,
                json: false,
                limit: None,
                repo: None,
                notes_dir: None,
                merge_report: None,
            },
        ),
        ("audit revoke", cmd_audit_revoke()),
        (
            "audit accept-revise",
            Commands::Audit {
                action: Some(AuditAction::AcceptRevise {
                    pr: TEST_PR,
                    head: HEAD_A.into(),
                    issue: TEST_ISSUE.into(),
                    project: TEST_PROJECT.into(),
                    source: "owner read review".into(),
                    repo: Some(TEST_REPO.into()),
                }),
                since: None,
                class: None,
                project: None,
                json: false,
                limit: None,
                repo: None,
                notes_dir: None,
                merge_report: None,
            },
        ),
        (
            "audit findings",
            Commands::Audit {
                action: Some(AuditAction::Findings {
                    pr: TEST_PR,
                    head: HEAD_A.into(),
                    issue: TEST_ISSUE.into(),
                    project: TEST_PROJECT.into(),
                    kind: None,
                    repo: Some(TEST_REPO.into()),
                }),
                since: None,
                class: None,
                project: None,
                json: false,
                limit: None,
                repo: None,
                notes_dir: None,
                merge_report: None,
            },
        ),
        (
            "audit reviewers",
            Commands::Audit {
                action: Some(AuditAction::Reviewers {
                    project: Some(TEST_PROJECT.into()),
                    action: None,
                }),
                since: None,
                class: None,
                project: None,
                json: false,
                limit: None,
                repo: None,
                notes_dir: None,
                merge_report: None,
            },
        ),
        (
            "audit reviewers set",
            Commands::Audit {
                action: Some(AuditAction::Reviewers {
                    project: None,
                    action: Some(ReviewersAction::Set {
                        project: TEST_PROJECT.into(),
                        reviewer: vec!["user_reviewer_1".into()],
                    }),
                }),
                since: None,
                class: None,
                project: None,
                json: false,
                limit: None,
                repo: None,
                notes_dir: None,
                merge_report: None,
            },
        ),
        (
            "audit identities",
            Commands::Audit {
                action: Some(AuditAction::Identities { action: None }),
                since: None,
                class: None,
                project: None,
                json: false,
                limit: None,
                repo: None,
                notes_dir: None,
                merge_report: None,
            },
        ),
        (
            "audit identities set",
            Commands::Audit {
                action: Some(AuditAction::Identities {
                    action: Some(IdentitiesAction::Set {
                        identity: vec!["user_reviewer_1=octocat".into()],
                    }),
                }),
                since: None,
                class: None,
                project: None,
                json: false,
                limit: None,
                repo: None,
                notes_dir: None,
                merge_report: None,
            },
        ),
    ]
}

#[test]
fn unreachable_and_503_fail_closed_without_local_sqlite_or_notes_fallback() {
    let env = ScopedEnv::new();
    let scratch = tempfile::tempdir().unwrap();
    let fake_notes = scratch.path().join("notes");
    std::fs::create_dir_all(&fake_notes).unwrap();
    // Plant a local-looking review note in `--notes-dir` so any local fallback on
    // `audit verdicts` would succeed instead of failing closed.
    std::fs::write(
        fake_notes.join("001-review.md"),
        format!(
            "From: reviewer\nIssue: {TEST_ISSUE}\nPR: #{TEST_PR}\nHead: {HEAD_A}\nVerdict: PASS\n"
        ),
    )
    .unwrap();
    let empty_state = scratch.path().join("state");
    std::fs::create_dir_all(&empty_state).unwrap();

    // 1. Unreachable closed loopback port.
    let closed_port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let unreachable_endpoint = format!("http://127.0.0.1:{closed_port}");
    env.write_credential(
        &unreachable_endpoint,
        "user_owner",
        TEST_ISSUER,
        &["cli.read", "cli.write", "audit.approve"],
    );
    let unreachable_target = target_with(&unreachable_endpoint, None);
    for (label, cmd) in all_hosted_delivery_and_audit_commands(&fake_notes) {
        let res = run(
            unreachable_target.clone(),
            cmd,
            5,
            Some(empty_state.clone()),
            None,
        );
        assert!(
            res.is_err(),
            "unreachable endpoint must fail closed for {label}, got {res:?}"
        );
    }

    // 2. Live server returning 503 Service Unavailable on `/__platform/cli/call`.
    let server_503 = StubRelayServer::start(
        None,
        Arc::new(|_verb, _args| HttpReply::text(503, "Service Unavailable")),
    );
    let endpoint_503 = server_503.endpoint();
    env.write_credential(
        &endpoint_503,
        "user_owner",
        TEST_ISSUER,
        &["cli.read", "cli.write", "audit.approve"],
    );
    let target_503 = target_with(&endpoint_503, None);
    for (label, cmd) in all_hosted_delivery_and_audit_commands(&fake_notes) {
        let res = run(target_503.clone(), cmd, 5, Some(empty_state.clone()), None);
        assert!(
            res.is_err(),
            "HTTP 503 must fail closed for {label}, got {res:?}"
        );
    }

    // 3. Live server returning 503 Service Unavailable on `/__platform/cli/authorize`.
    let auth_503 = StubRelayServer::start(
        Some(HttpReply::json(
            503,
            json!({"ok": false, "error": {"code": "service_unavailable"}}),
        )),
        Arc::new(|verb, _args| valid_stub_reply(verb)),
    );
    let endpoint_auth_503 = auth_503.endpoint();
    env.write_credential(
        &endpoint_auth_503,
        "user_owner",
        TEST_ISSUER,
        &["cli.read", "cli.write", "audit.approve"],
    );
    let auth_target_503 = target_with(&endpoint_auth_503, None);
    for (label, cmd) in all_hosted_delivery_and_audit_commands(&fake_notes) {
        let res = run(
            auth_target_503.clone(),
            cmd,
            5,
            Some(empty_state.clone()),
            None,
        );
        assert!(
            res.is_err(),
            "authorize HTTP 503 must fail closed for {label}, got {res:?}"
        );
    }

    assert!(
        !has_local_store_artifacts(&empty_state),
        "no local SQLite/socket/notes store may be created in local_state"
    );
    assert!(
        !has_local_store_artifacts(&env.xdg_state_home()),
        "no local SQLite/socket/notes store may be created in XDG_STATE_HOME"
    );
    assert!(
        !has_local_store_artifacts(&env.home_dir()),
        "no local SQLite/socket/notes store may be created in HOME"
    );
}

/// CAD-2047: `delivery enqueue --approve <source>` sends `approve_source` on the
/// one enqueue call (and only when asked), and a receipt that does not say
/// whether the approval was recorded is an unknown outcome, not a success.
#[test]
fn delivery_enqueue_approve_source_travels_and_needs_its_receipt_flag() {
    let receipt = |recorded: Option<bool>| {
        let mut v = json!({
            "state": "enqueued",
            "ok": true,
            "project": TEST_PROJECT,
            "issue": TEST_ISSUE,
            "repo": TEST_REPO,
            "pr": TEST_PR,
            "head": HEAD_A,
            "evidence_id": "ev_2047",
            "queued": true,
            "auto_merge_enabled": true,
        });
        if let Some(flag) = recorded {
            v["approval_recorded"] = json!(flag);
        }
        v
    };
    let run_enqueue = |reply: Value, approve: Option<&'static str>| {
        let env = ScopedEnv::new();
        let seen: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let payload = Arc::new(reply);
        let server = StubRelayServer::start(
            None,
            Arc::new(move |verb, args| {
                log.lock().unwrap().push((verb.to_string(), args.clone()));
                HttpReply::json(200, (*payload).clone())
            }),
        );
        let endpoint = server.endpoint();
        env.write_credential(
            &endpoint,
            TEST_SUBJECT,
            TEST_ISSUER,
            &["cli.read", "cli.write", "audit.approve"],
        );
        let target = target_with(&endpoint, None);
        let result = delivery_enqueue(
            &target,
            5,
            Some(TEST_PROJECT),
            Some(TEST_ISSUE),
            TEST_REPO,
            TEST_PR,
            HEAD_A,
            false,
            approve,
        )
        .map(|_| ());
        let calls = seen.lock().unwrap().clone();
        (result, calls)
    };
    let enqueue_args = |calls: &[(String, Value)]| -> Value {
        calls
            .iter()
            .find(|(verb, _)| verb == "delivery_enqueue")
            .map(|(_, args)| args.clone())
            .expect("delivery_enqueue was called")
    };

    // With --approve: the source travels and a receipt with the flag passes.
    let (ok, calls) = run_enqueue(
        receipt(Some(true)),
        Some("operator 2026-11-16 after reviews"),
    );
    ok.expect("receipt naming approval_recorded must pass");
    assert_eq!(
        enqueue_args(&calls)["approve_source"],
        json!("operator 2026-11-16 after reviews")
    );

    // With --approve, a receipt without the flag (an older board) is unknown.
    let (err, _) = run_enqueue(receipt(None), Some("operator ok"));
    let err = err.expect_err("a receipt silent on the approval must not pass");
    assert!(
        err.to_string().contains("approval was recorded"),
        "unexpected error: {err}"
    );

    // Without --approve: nothing extra is sent and the old receipt still passes.
    let (ok, calls) = run_enqueue(receipt(None), None);
    ok.expect("plain enqueue receipt must still pass");
    assert!(enqueue_args(&calls).get("approve_source").is_none());
}
