//! CAD-2317 independent guard-check acceptance test (authored from the ticket
//! specification, separate from the implementer):
//! 1. Operator-only mutation & approval boundary across Daemon RPC and the real
//!    HTTP Board (`PublicBoard` signed Ed25519 `owner` vs `member` sessions,
//!    unauthenticated HTTP, `Asserted::Agent`, and `Asserted::Unproven`), with
//!    zero durable side effects on refusal paired with positive controls.
//! 2. Digest & revision gate (`sha256` script + `fnv1a:` Context Markdown)
//!    across `markdown`, `script`, and `script_then_markdown` routines, proving
//!    unapproved, forged, and post-approval tampered scripts/playbooks refuse
//!    across both RPC and HTTP with zero side effects until re-approved.
//! 3. Connections isolation: shared connections refuse `external_event`
//!    triggers while `allow_external_triggers == false`, admit them once enabled
//!    by the owner, and scrub the enrolled shared credential first when
//!    switching `visibility` from `shared` to `personal`.

#![cfg(all(test, feature = "test-seam"))]

use super::*;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tiny_http::{Header, Response, Server};

use crate::test_seam::{scoped, Asserted};

const ORG: &str = "cad2317-synthetic-org";
const KID: &str = "cad2317-synthetic-key";
const WORKSPACE_ACCOUNT_1: &str = "ws_11111111-1111-4111-8111-111111111111";
const WORKSPACE_ACCOUNT_2: &str = "ws_22222222-2222-4222-8222-222222222222";

struct Rig {
    _dir: tempfile::TempDir,
    state: PathBuf,
    pm: PathBuf,
    board_port: u16,
    public_host: String,
    issuer: String,
    key: Ed25519KeyPair,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// Block SIGCHLD on the test thread (inherited by spawned daemon/board threads)
/// so child `git` exits cannot interrupt timed `ureq` socket reads with EINTR.
fn block_sigchld() {
    // SAFETY: local signal set applied to the calling thread's mask.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGCHLD);
        assert_eq!(
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()),
            0
        );
    }
}

fn start() -> Rig {
    block_sigchld();
    let dir = tempfile::Builder::new()
        .prefix("c2317-guard")
        .tempdir()
        .unwrap();
    let state = dir.path().to_path_buf();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    let pm = state.join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    std::fs::set_permissions(&pm, std::fs::Permissions::from_mode(0o700)).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();

    let mut opts = crate::daemon::ServeOptions {
        test_seam: true,
        stop: Some(stop.clone()),
        provider_deployments: Some(
            crate::platform::deployments::DeploymentMetadata::parse(
                br#"{"schema":1,"providers":[]}"#,
            )
            .expect("isolated deployment metadata"),
        ),
        ..crate::daemon::ServeOptions::default()
    };
    crate::platform::agenticos_external::register_with_deployment_pin(
        &mut opts,
        "https://api.example.test",
        None,
    )
    .expect("token fixture registration");
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let daemon_state = state.clone();
    threads.push(std::thread::spawn(move || {
        crate::daemon::serve_with(&daemon_state, opts).unwrap()
    }));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !state.join("cadence.sock").exists() || crate::test_seam::Seam::token_at(&state).is_none()
    {
        assert!(Instant::now() < deadline, "synthetic daemon did not start");
        std::thread::sleep(Duration::from_millis(50));
    }

    let key = Ed25519KeyPair::from_seed_unchecked(&[231; 32]).unwrap();
    let public_key = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let platform = (3110..=3199)
        .find_map(|port| Server::http(("127.0.0.1", port)).ok())
        .expect("no free synthetic platform port in 3110..=3199");
    let platform_port = match platform.server_addr() {
        tiny_http::ListenAddr::IP(addr) => addr.port(),
        _ => panic!("synthetic platform did not bind TCP"),
    };
    let public_host = format!("localhost:{platform_port}");
    let issuer = format!("http://{public_host}");
    let platform_stop = stop.clone();
    threads.push(std::thread::spawn(move || {
        while !platform_stop.load(Ordering::SeqCst) {
            if let Ok(Some(request)) = platform.recv_timeout(Duration::from_millis(100)) {
                let valid = request.url() == "/.well-known/agenticos-board-jwks.json";
                let body = if valid {
                    json!({"keys":[{"kty":"OKP","crv":"Ed25519","kid":KID,"x":public_key}]})
                } else {
                    json!({"error":"not_found"})
                };
                let response = Response::from_string(body.to_string())
                    .with_status_code(if valid { 200 } else { 404 })
                    .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                let _ = request.respond(response);
            }
        }
    }));

    let mut board_port = 0;
    for port in 3110..=3199 {
        if port == platform_port {
            continue;
        }
        let (startup, ready) = std::sync::mpsc::channel();
        let board = ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.clone()),
            startup: Some(startup),
            test_seam: true,
            allow_hosts: vec![public_host.clone()],
            public: Some(PublicBoard {
                host: public_host.clone(),
                issuer: issuer.clone(),
                company: ORG.into(),
                authorize_url: format!("{issuer}/authorize"),
            }),
            ..ServeOpts::default()
        };
        let (board_state, board_pm) = (state.clone(), pm.clone());
        let thread =
            std::thread::spawn(move || drop(crate::ui::serve(&board_state, &board_pm, &board)));
        match ready.recv_timeout(Duration::from_secs(30)).unwrap() {
            Ok(()) => {
                board_port = port;
                threads.push(thread);
                break;
            }
            Err(_) => {
                thread.join().unwrap();
            }
        }
    }
    assert!(board_port > 0, "synthetic board could not bind");
    Rig {
        _dir: dir,
        state,
        pm,
        board_port,
        public_host,
        issuer,
        key,
        stop,
        threads,
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .build(),
    )
}

fn post(
    rig: &Rig,
    path: &str,
    cookie: Option<&str>,
    body: &Value,
) -> ureq::http::Response<ureq::Body> {
    let url = format!("http://127.0.0.1:{}{path}", rig.board_port);
    let mut request = agent()
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("X-Cadence-Board", "1");
    if let Some(cookie) = cookie {
        request = request.header("Cookie", cookie);
    }
    request.send_json(body).expect("real board POST failed")
}

fn get(rig: &Rig, path: &str, cookie: Option<&str>) -> ureq::http::Response<ureq::Body> {
    let url = format!("http://127.0.0.1:{}{path}", rig.board_port);
    let mut request = agent().get(&url).header("Host", &rig.public_host);
    if let Some(cookie) = cookie {
        request = request.header("Cookie", cookie);
    }
    request.call().expect("real board GET failed")
}

fn put(rig: &Rig, path: &str, cookie: &str, body: &Value) -> ureq::http::Response<ureq::Body> {
    let url = format!("http://127.0.0.1:{}{path}", rig.board_port);
    agent()
        .put(&url)
        .header("Content-Type", "application/json")
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("X-Cadence-Board", "1")
        .header("Cookie", cookie)
        .send_json(body)
        .expect("real board PUT failed")
}

fn status_and_json(mut response: ureq::http::Response<ureq::Body>) -> (u16, Value) {
    let status = response.status().as_u16();
    let raw = response.body_mut().read_to_string().unwrap_or_default();
    let parsed = serde_json::from_str(&raw).unwrap_or_else(|_| json!({"raw": raw}));
    (status, parsed)
}

fn session(rig: &Rig, sub: &str, role: &str) -> String {
    let now = crate::issue::time::now_epoch();
    let header = json!({"alg":"EdDSA","typ":"JWT","kid":KID});
    let claims = json!({
        "iss": rig.issuer,
        "aud": rig.public_host,
        "company": ORG,
        "sub": sub,
        "email": format!("{sub}@example.test"),
        "name": sub,
        "role": role,
        "iat": now,
        "exp": now + 60,
        "jti": format!("cad2317-{sub}-{}", uuid::Uuid::new_v4()),
    });
    let signed = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap()),
    );
    let assertion = format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(rig.key.sign(signed.as_bytes()).as_ref()),
    );
    let response = post(
        rig,
        "/__platform/session",
        None,
        &json!({"assertion": assertion}),
    );
    assert_eq!(
        response.status().as_u16(),
        200,
        "signed fixture SSO was refused"
    );
    response
        .headers()
        .get("set-cookie")
        .expect("public session has no cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

fn rpc_as(rig: &Rig, who: Asserted, method: &str, params: Value) -> Result<Value> {
    scoped(who, || crate::client::rpc(&rig.state, method, params))
}

fn register_inbox_agent(rig: &Rig, alias: &str) {
    rpc_as(
        rig,
        Asserted::Operator,
        "agent_register",
        json!({
            "alias": alias,
            "provider": "inbox",
            "endpoint_kind": "inbox",
        }),
    )
    .expect("operator registers fixture inbox agent");
}

fn inbox_queued(rig: &Rig, alias: &str) -> i64 {
    let list = rpc_as(rig, Asserted::Operator, "agent_list", json!({})).unwrap();
    list["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["alias"] == alias)
        .and_then(|row| row["inbox"]["queued"].as_i64())
        .unwrap_or(0)
}

/// FNV-1a content revision matching `crate::issue::write::issue_rev` / `wiki`.
fn fnv1a_rev(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        h ^= u64::from(*byte);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("fnv1a:{h:016x}")
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Write an executable script atomically (close write fd before `rename`) so
/// concurrent `fork`/`execve` on Linux never hits `ETXTBSY`.
fn write_executable_script(path: &Path, bytes: &[u8]) {
    let parent = path.parent().expect("script parent dir");
    std::fs::create_dir_all(parent).unwrap();
    let tmp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name().unwrap().to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .unwrap();
    file.write_all(bytes).unwrap();
    file.set_permissions(std::fs::Permissions::from_mode(0o755))
        .unwrap();
    file.sync_all().unwrap();
    drop(file);
    std::fs::rename(&tmp, path).unwrap();
}

fn write_wiki_playbook(rig: &Rig, owner_cookie: &str, wiki_path: &str, text: &str) -> String {
    let (status, body) = status_and_json(put(
        rig,
        "/api/wiki/file",
        owner_cookie,
        &json!({"path": wiki_path, "text": text}),
    ));
    assert_eq!(
        status, 200,
        "owner PUT /api/wiki/file failed for {wiki_path}: {body}"
    );
    assert!(
        rig.pm.join("wiki").join(wiki_path).is_file(),
        "wiki file should land under <pm>/wiki/{wiki_path}"
    );
    let expected = fnv1a_rev(text.as_bytes());
    if let Some(returned_rev) = body["rev"].as_str() {
        assert_eq!(returned_rev, expected, "wiki rev matches FNV-1a token");
    }
    expected
}

fn routine_create_payload(
    name: &str,
    agent_alias: &str,
    trigger_kind: &str,
    target_kind: &str,
    markdown_path: Option<&str>,
    script_path: Option<&str>,
    connection_account: Option<&str>,
) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("name".into(), json!(name));
    map.insert("agent_alias".into(), json!(agent_alias));
    map.insert("trigger_kind".into(), json!(trigger_kind));
    map.insert("interval_mins".into(), json!(60));
    map.insert("timezone".into(), json!("UTC"));
    if trigger_kind == "event" {
        map.insert("event_source".into(), json!("agenticos_external"));
    }
    map.insert("target_kind".into(), json!(target_kind));
    if let Some(md) = markdown_path {
        map.insert("markdown_path".into(), json!(md));
    }
    if let Some(sh) = script_path {
        map.insert("script_path".into(), json!(sh));
    }
    if let Some(conn) = connection_account {
        map.insert("connection_account".into(), json!(conn));
    }
    Value::Object(map)
}

fn enroll_shared_connection(rig: &Rig, account: &str, token: &str) -> String {
    let enrolled = rpc_as(
        rig,
        Asserted::Operator,
        "connection_create",
        json!({
            "provider": "agenticos_external",
            "account": account,
            "shape": "token",
            "token": token,
            "scopes": ["provider.read"],
            "accept_same_uid_risk": true,
        }),
    )
    .expect("operator enrolls agenticos_external connection");
    enrolled["connection"]["id"]
        .as_str()
        .expect("enrolled connection id")
        .to_string()
}

fn assert_http_refused_auth(status: u16, context: &str, body: &Value) {
    assert!(
        matches!(status, 401 | 403),
        "{context} must refuse with HTTP 401/403, got {status}: {body}"
    );
}

#[test]
fn operator_only_boundary_refuses_unauthorized_callers_across_rpc_and_http_with_zero_side_effects()
{
    let rig = start();
    let owner = session(&rig, "owner", "owner");
    let member = session(&rig, "member", "member");
    register_inbox_agent(&rig, "worker");

    let wiki_rel = "global/playbooks/triage.md";
    let md_rev = write_wiki_playbook(&rig, &owner, wiki_rel, "# Triage\nReview new alerts.\n");
    let script_bytes = b"#!/bin/sh\nprintf '{\"items\":[]}\\n'\n";
    write_executable_script(&rig.pm.join("scripts/check.sh"), script_bytes);
    let script_sha = sha256_hex(script_bytes);

    let conn_id = enroll_shared_connection(&rig, WORKSPACE_ACCOUNT_1, "shared-secret-token-xyz");

    // 1. `routine_create` / `POST /api/routines`
    let create_req = routine_create_payload(
        "Hourly triage",
        "worker",
        "schedule",
        "script_then_markdown",
        Some("wiki:global/playbooks/triage.md"),
        Some("scripts/check.sh"),
        None,
    );
    for caller in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        assert!(
            rpc_as(&rig, caller.clone(), "routine_create", create_req.clone()).is_err(),
            "routine_create must refuse {caller:?}"
        );
    }
    for (label, cookie) in [("unauthenticated", None), ("member", Some(member.as_str()))] {
        let (status, body) = status_and_json(post(&rig, "/api/routines", cookie, &create_req));
        assert_http_refused_auth(status, &format!("{label} POST /api/routines"), &body);
    }

    let (list_status, listed) = status_and_json(get(&rig, "/api/routines", Some(&owner)));
    assert_eq!(list_status, 200, "owner GET /api/routines failed: {listed}");
    assert!(
        listed["routines"].as_array().unwrap().is_empty(),
        "refused routine_create calls must leave zero routines: {listed}"
    );

    // Positive controls for `routine_create` (RPC + HTTP)
    let created_rpc = rpc_as(&rig, Asserted::Operator, "routine_create", create_req)
        .expect("operator routine_create succeeds");
    assert_eq!(created_rpc["ok"], true);
    assert_eq!(created_rpc["routine"]["status"], "waiting_approval");
    let rtn_rpc = created_rpc["routine"]["id"]
        .as_str()
        .expect("rpc routine id")
        .to_string();

    let create_http_req = routine_create_payload(
        "HTTP triage",
        "worker",
        "schedule",
        "script_then_markdown",
        Some("wiki:global/playbooks/triage.md"),
        Some("scripts/check.sh"),
        None,
    );
    let (create_status, created_http) =
        status_and_json(post(&rig, "/api/routines", Some(&owner), &create_http_req));
    assert_eq!(
        create_status, 200,
        "owner POST /api/routines failed: {created_http}"
    );
    assert_eq!(created_http["routine"]["status"], "waiting_approval");
    let rtn_http = created_http["routine"]["id"]
        .as_str()
        .expect("http routine id")
        .to_string();

    // 2. `routine_approve` / `POST /api/routines/<id>/approve`
    let approve_rpc_req = json!({
        "id": rtn_rpc,
        "expect_rev": md_rev,
        "digest": script_sha,
    });
    for caller in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        assert!(
            rpc_as(
                &rig,
                caller.clone(),
                "routine_approve",
                approve_rpc_req.clone()
            )
            .is_err(),
            "routine_approve must refuse {caller:?}"
        );
    }
    let approve_http_body = json!({
        "expect_rev": md_rev,
        "digest": script_sha,
    });
    for (label, cookie) in [("unauthenticated", None), ("member", Some(member.as_str()))] {
        let (status, body) = status_and_json(post(
            &rig,
            &format!("/api/routines/{rtn_http}/approve"),
            cookie,
            &approve_http_body,
        ));
        assert_http_refused_auth(
            status,
            &format!("{label} POST /api/routines/<id>/approve"),
            &body,
        );
    }

    for id in [&rtn_rpc, &rtn_http] {
        let shown = rpc_as(&rig, Asserted::Operator, "routine_show", json!({"id": id})).unwrap();
        assert_eq!(
            shown["routine"]["status"], "waiting_approval",
            "refused approvals must not change routine {id}: {shown}"
        );
        assert!(shown["routine"]["approved_rev"].is_null());
        assert!(shown["routine"]["approved_sha256"].is_null());
    }

    // Positive controls for `routine_approve` (RPC + HTTP)
    let approved_rpc = rpc_as(&rig, Asserted::Operator, "routine_approve", approve_rpc_req)
        .expect("operator routine_approve succeeds");
    assert_eq!(approved_rpc["ok"], true);
    assert_eq!(approved_rpc["routine"]["status"], "on");

    let (approve_status, approved_http) = status_and_json(post(
        &rig,
        &format!("/api/routines/{rtn_http}/approve"),
        Some(&owner),
        &approve_http_body,
    ));
    assert_eq!(
        approve_status, 200,
        "owner POST /api/routines/<id>/approve failed: {approved_http}"
    );
    assert_eq!(approved_http["routine"]["status"], "on");

    // 3. `routine_run` / `POST /api/routines/<id>/run`
    let run_rpc_req = json!({"id": rtn_rpc, "trigger_source": "manual"});
    for caller in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        assert!(
            rpc_as(&rig, caller.clone(), "routine_run", run_rpc_req.clone()).is_err(),
            "routine_run must refuse {caller:?}"
        );
    }
    let run_http_body = json!({"trigger_source": "manual"});
    for (label, cookie) in [("unauthenticated", None), ("member", Some(member.as_str()))] {
        let (status, body) = status_and_json(post(
            &rig,
            &format!("/api/routines/{rtn_http}/run"),
            cookie,
            &run_http_body,
        ));
        assert_http_refused_auth(
            status,
            &format!("{label} POST /api/routines/<id>/run"),
            &body,
        );
    }

    for id in [&rtn_rpc, &rtn_http] {
        let shown = rpc_as(&rig, Asserted::Operator, "routine_show", json!({"id": id})).unwrap();
        assert!(
            shown["routine"]["runs"].as_array().unwrap().is_empty(),
            "refused routine_run must record zero runs for {id}: {shown}"
        );
    }
    assert_eq!(
        inbox_queued(&rig, "worker"),
        0,
        "refused routine_run must queue zero messages"
    );

    // Positive controls for `routine_run` (RPC + HTTP)
    let ran_rpc = rpc_as(&rig, Asserted::Operator, "routine_run", run_rpc_req)
        .expect("operator routine_run succeeds");
    assert_eq!(ran_rpc["ok"], true);
    assert!(ran_rpc["run"]["id"].as_str().is_some());

    let (run_status, ran_http) = status_and_json(post(
        &rig,
        &format!("/api/routines/{rtn_http}/run"),
        Some(&owner),
        &run_http_body,
    ));
    assert_eq!(
        run_status, 200,
        "owner POST /api/routines/<id>/run failed: {ran_http}"
    );
    assert!(ran_http["run"]["id"].as_str().is_some());

    // 4. `connection_policy_set` / `POST /api/connections/<id>/policy`
    let hostile_policy_rpc = json!({
        "connection_id": conn_id,
        "visibility": "personal",
        "write_mode": "ask_approval",
        "allow_external_triggers": true,
    });
    for caller in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        assert!(
            rpc_as(
                &rig,
                caller.clone(),
                "connection_policy_set",
                hostile_policy_rpc.clone(),
            )
            .is_err(),
            "connection_policy_set must refuse {caller:?}"
        );
    }
    let hostile_policy_http = json!({
        "visibility": "personal",
        "write_mode": "ask_approval",
        "allow_external_triggers": true,
    });
    for (label, cookie) in [("unauthenticated", None), ("member", Some(member.as_str()))] {
        let (status, body) = status_and_json(post(
            &rig,
            &format!("/api/connections/{conn_id}/policy"),
            cookie,
            &hostile_policy_http,
        ));
        assert_http_refused_auth(
            status,
            &format!("{label} POST /api/connections/<id>/policy"),
            &body,
        );
    }

    // Zero side effects: the shared credential is still enrolled and external triggers remain off.
    let shown_conn = rpc_as(
        &rig,
        Asserted::Operator,
        "connection_show",
        json!({"connection_id": conn_id}),
    )
    .expect("refused policy writes must not wipe enrolled shared credential");
    assert_eq!(shown_conn["connection"]["kind"], "enrolled");
    let policy_before = rpc_as(
        &rig,
        Asserted::Operator,
        "connection_policy_get",
        json!({"connection_id": conn_id}),
    )
    .expect("operator reads connection policy");
    assert_ne!(policy_before["policy"]["visibility"], "personal");
    assert_eq!(policy_before["policy"]["allow_external_triggers"], false);

    // Positive controls for `connection_policy_set` (RPC + HTTP)
    let set_rpc = rpc_as(
        &rig,
        Asserted::Operator,
        "connection_policy_set",
        json!({
            "connection_id": conn_id,
            "visibility": "shared",
            "write_mode": "read_only",
            "allow_external_triggers": false,
        }),
    )
    .expect("operator connection_policy_set succeeds");
    assert_eq!(set_rpc["ok"], true);
    assert_eq!(set_rpc["policy"]["write_mode"], "read_only");
    assert_eq!(set_rpc["shared_credential_cleared"], false);

    let (policy_status, set_http) = status_and_json(post(
        &rig,
        &format!("/api/connections/{conn_id}/policy"),
        Some(&owner),
        &json!({
            "visibility": "shared",
            "write_mode": "ask_approval",
            "allow_external_triggers": true,
        }),
    ));
    assert_eq!(
        policy_status, 200,
        "owner POST /api/connections/<id>/policy failed: {set_http}"
    );
    assert_eq!(set_http["policy"]["write_mode"], "ask_approval");
    assert_eq!(set_http["policy"]["allow_external_triggers"], true);
    assert_eq!(set_http["shared_credential_cleared"], false);
}

#[test]
fn digest_and_revision_gate_blocks_unapproved_forged_and_tampered_routines_with_zero_side_effects()
{
    let rig = start();
    let owner = session(&rig, "owner", "owner");
    register_inbox_agent(&rig, "worker");

    let wiki_rel = "global/playbooks/triage.md";
    let rev_v1 = write_wiki_playbook(&rig, &owner, wiki_rel, "# Triage v1\nInitial playbook.\n");

    let script_path = rig.pm.join("scripts/check.sh");
    let pre_approval_witness = rig.pm.join("pre_approval.witness");
    let script_v1 = format!(
        "#!/bin/sh\nprintf 'ran\\n' > '{}'\nprintf '{{\"items\":[]}}\\n'\n",
        pre_approval_witness.display()
    );
    write_executable_script(&script_path, script_v1.as_bytes());
    let sha_v1 = sha256_hex(script_v1.as_bytes());

    // Create routines across all three target kinds: `markdown`, `script`, `script_then_markdown`.
    let created_md = rpc_as(
        &rig,
        Asserted::Operator,
        "routine_create",
        routine_create_payload(
            "Markdown only",
            "worker",
            "schedule",
            "markdown",
            Some("wiki:global/playbooks/triage.md"),
            None,
            None,
        ),
    )
    .expect("create markdown routine");
    let rtn_md = created_md["routine"]["id"].as_str().unwrap().to_string();
    assert_eq!(created_md["routine"]["status"], "waiting_approval");
    assert_eq!(created_md["routine"]["current_rev"], rev_v1);
    assert!(created_md["routine"]["approved_rev"].is_null());
    assert!(created_md["routine"]["approved_sha256"].is_null());

    let created_sh = rpc_as(
        &rig,
        Asserted::Operator,
        "routine_create",
        routine_create_payload(
            "Script only",
            "worker",
            "schedule",
            "script",
            None,
            Some("scripts/check.sh"),
            None,
        ),
    )
    .expect("create script routine");
    let rtn_sh = created_sh["routine"]["id"].as_str().unwrap().to_string();
    assert_eq!(created_sh["routine"]["status"], "waiting_approval");
    assert_eq!(created_sh["routine"]["current_sha256"], sha_v1);
    assert!(created_sh["routine"]["approved_rev"].is_null());
    assert!(created_sh["routine"]["approved_sha256"].is_null());

    let (both_status, created_both) = status_and_json(post(
        &rig,
        "/api/routines",
        Some(&owner),
        &routine_create_payload(
            "Script then markdown",
            "worker",
            "schedule",
            "script_then_markdown",
            Some("wiki:global/playbooks/triage.md"),
            Some("scripts/check.sh"),
            None,
        ),
    ));
    assert_eq!(
        both_status, 200,
        "create script_then_markdown failed: {created_both}"
    );
    let rtn_both = created_both["routine"]["id"].as_str().unwrap().to_string();
    assert_eq!(created_both["routine"]["status"], "waiting_approval");
    assert_eq!(created_both["routine"]["current_rev"], rev_v1);
    assert_eq!(created_both["routine"]["current_sha256"], sha_v1);
    assert!(created_both["routine"]["approved_rev"].is_null());
    assert!(created_both["routine"]["approved_sha256"].is_null());

    // Running any unapproved routine via RPC or HTTP is refused before spawning or queuing.
    for id in [&rtn_md, &rtn_sh, &rtn_both] {
        assert!(
            rpc_as(
                &rig,
                Asserted::Operator,
                "routine_run",
                json!({"id": id, "trigger_source": "manual"}),
            )
            .is_err(),
            "unapproved routine_run must refuse {id}"
        );
        let (status, body) = status_and_json(post(
            &rig,
            &format!("/api/routines/{id}/run"),
            Some(&owner),
            &json!({"trigger_source": "manual"}),
        ));
        assert!(
            !(200..300).contains(&status),
            "unapproved HTTP run must refuse {id}, got {status}: {body}"
        );
    }
    assert!(
        !pre_approval_witness.exists(),
        "unapproved script routine must never execute check.sh"
    );
    assert_eq!(
        inbox_queued(&rig, "worker"),
        0,
        "unapproved routine must never queue messages"
    );

    // Forged / mismatched `expect_rev` or `digest` must refuse on both RPC and HTTP.
    let forged_rev = "fnv1a:0000000000000000";
    let forged_sha = "0".repeat(64);
    assert!(
        rpc_as(
            &rig,
            Asserted::Operator,
            "routine_approve",
            json!({"id": rtn_md, "expect_rev": forged_rev}),
        )
        .is_err(),
        "forged expect_rev must refuse markdown approval"
    );
    assert!(
        rpc_as(
            &rig,
            Asserted::Operator,
            "routine_approve",
            json!({"id": rtn_sh, "digest": forged_sha}),
        )
        .is_err(),
        "forged digest must refuse script approval"
    );
    for bad_body in [
        json!({"expect_rev": forged_rev, "digest": sha_v1}),
        json!({"expect_rev": rev_v1, "digest": forged_sha}),
    ] {
        let (status, body) = status_and_json(post(
            &rig,
            &format!("/api/routines/{rtn_both}/approve"),
            Some(&owner),
            &bad_body,
        ));
        assert!(
            !(200..300).contains(&status),
            "forged approval on HTTP must fail, got {status}: {body}"
        );
    }

    // Exact live `expect_rev` / `digest` transitions status to `"on"`.
    let ok_md = rpc_as(
        &rig,
        Asserted::Operator,
        "routine_approve",
        json!({"id": rtn_md, "expect_rev": rev_v1}),
    )
    .expect("approve markdown routine");
    assert_eq!(ok_md["routine"]["status"], "on");

    let ok_sh = rpc_as(
        &rig,
        Asserted::Operator,
        "routine_approve",
        json!({"id": rtn_sh, "digest": sha_v1}),
    )
    .expect("approve script routine");
    assert_eq!(ok_sh["routine"]["status"], "on");

    let (ok_both_status, ok_both) = status_and_json(post(
        &rig,
        &format!("/api/routines/{rtn_both}/approve"),
        Some(&owner),
        &json!({"expect_rev": rev_v1, "digest": sha_v1}),
    ));
    assert_eq!(
        ok_both_status, 200,
        "approve script_then_markdown failed: {ok_both}"
    );
    assert_eq!(ok_both["routine"]["status"], "on");

    // Positive baseline run after exact approval succeeds.
    let (base_run_status, base_run) = status_and_json(post(
        &rig,
        &format!("/api/routines/{rtn_both}/run"),
        Some(&owner),
        &json!({"trigger_source": "manual"}),
    ));
    assert_eq!(
        base_run_status, 200,
        "approved baseline run failed: {base_run}"
    );
    assert!(
        pre_approval_witness.exists(),
        "approved baseline run should execute script v1"
    );

    // Tamper with `scripts/check.sh` on disk after approval so it would drop `tampered.witness`.
    let tampered_witness = rig.pm.join("tampered.witness");
    let script_tampered = format!(
        "#!/bin/sh\nprintf 'tampered\\n' > '{}'\nprintf '{{\"items\":[]}}\\n'\n",
        tampered_witness.display()
    );
    write_executable_script(&script_path, script_tampered.as_bytes());
    let sha_v2 = sha256_hex(script_tampered.as_bytes());
    assert_ne!(sha_v1, sha_v2);

    let runs_both_before = rpc_as(
        &rig,
        Asserted::Operator,
        "routine_show",
        json!({"id": rtn_both}),
    )
    .unwrap()["routine"]["runs"]
        .as_array()
        .unwrap()
        .len();
    let runs_sh_before = rpc_as(
        &rig,
        Asserted::Operator,
        "routine_show",
        json!({"id": rtn_sh}),
    )
    .unwrap()["routine"]["runs"]
        .as_array()
        .unwrap()
        .len();
    let queued_before = inbox_queued(&rig, "worker");

    // Both RPC `routine_run` AND HTTP `POST /api/routines/<id>/run` MUST refuse tampered script.
    for id in [&rtn_sh, &rtn_both] {
        assert!(
            rpc_as(
                &rig,
                Asserted::Operator,
                "routine_run",
                json!({"id": id, "trigger_source": "manual"}),
            )
            .is_err(),
            "tampered script routine_run via RPC must refuse {id}"
        );
        let (status, body) = status_and_json(post(
            &rig,
            &format!("/api/routines/{id}/run"),
            Some(&owner),
            &json!({"trigger_source": "manual"}),
        ));
        assert!(
            !(200..300).contains(&status),
            "tampered script run via HTTP must refuse {id}, got {status}: {body}"
        );
    }

    // Zero side effects on script tampering + drift surfaced on both RPC and HTTP.
    assert!(
        !tampered_witness.exists(),
        "tampered.witness must NOT exist — modified script must never be spawned before re-approval"
    );
    assert_eq!(
        inbox_queued(&rig, "worker"),
        queued_before,
        "tampered script refusal must queue zero messages"
    );

    let shown_both_rpc = rpc_as(
        &rig,
        Asserted::Operator,
        "routine_show",
        json!({"id": rtn_both}),
    )
    .unwrap();
    assert_eq!(
        shown_both_rpc["routine"]["runs"].as_array().unwrap().len(),
        runs_both_before,
        "tampered script refusal must record zero routine_runs rows"
    );
    assert_eq!(shown_both_rpc["routine"]["status"], "waiting_approval");
    assert_eq!(shown_both_rpc["routine"]["approval_drift"], true);

    let (shown_http_status, shown_both_http) = status_and_json(get(
        &rig,
        &format!("/api/routines/{rtn_both}"),
        Some(&owner),
    ));
    assert_eq!(shown_http_status, 200);
    assert_eq!(shown_both_http["routine"]["status"], "waiting_approval");
    assert_eq!(shown_both_http["routine"]["approval_drift"], true);

    let shown_sh_rpc = rpc_as(
        &rig,
        Asserted::Operator,
        "routine_show",
        json!({"id": rtn_sh}),
    )
    .unwrap();
    assert_eq!(
        shown_sh_rpc["routine"]["runs"].as_array().unwrap().len(),
        runs_sh_before
    );
    assert_eq!(shown_sh_rpc["routine"]["status"], "waiting_approval");
    assert_eq!(shown_sh_rpc["routine"]["approval_drift"], true);

    // Owner re-approves the new SHA-256 digest -> HTTP run now succeeds and proves witness works.
    let (reapprove_sh_status, reapproved_both) = status_and_json(post(
        &rig,
        &format!("/api/routines/{rtn_both}/approve"),
        Some(&owner),
        &json!({"expect_rev": rev_v1, "digest": sha_v2}),
    ));
    assert_eq!(
        reapprove_sh_status, 200,
        "owner re-approve after script edit failed: {reapproved_both}"
    );
    assert_eq!(reapproved_both["routine"]["status"], "on");

    let (rerun_status, rerun_body) = status_and_json(post(
        &rig,
        &format!("/api/routines/{rtn_both}/run"),
        Some(&owner),
        &json!({"trigger_source": "manual"}),
    ));
    assert_eq!(
        rerun_status, 200,
        "run after script re-approval failed: {rerun_body}"
    );
    assert!(
        tampered_witness.exists(),
        "re-approved script v2 executes and writes witness"
    );
    std::fs::remove_file(&tampered_witness).unwrap();

    // Now modify the Context Markdown playbook (`PUT /api/wiki/file`) after approval.
    let rev_v2 = write_wiki_playbook(
        &rig,
        &owner,
        wiki_rel,
        "# Triage v2\nModified playbook requiring fresh approval.\n",
    );
    assert_ne!(rev_v1, rev_v2);

    let runs_md_before = rpc_as(
        &rig,
        Asserted::Operator,
        "routine_show",
        json!({"id": rtn_md}),
    )
    .unwrap()["routine"]["runs"]
        .as_array()
        .unwrap()
        .len();
    let runs_both_md_before = rpc_as(
        &rig,
        Asserted::Operator,
        "routine_show",
        json!({"id": rtn_both}),
    )
    .unwrap()["routine"]["runs"]
        .as_array()
        .unwrap()
        .len();
    let queued_md_before = inbox_queued(&rig, "worker");

    for id in [&rtn_md, &rtn_both] {
        assert!(
            rpc_as(
                &rig,
                Asserted::Operator,
                "routine_run",
                json!({"id": id, "trigger_source": "manual"}),
            )
            .is_err(),
            "modified markdown routine_run via RPC must refuse {id}"
        );
        let (status, body) = status_and_json(post(
            &rig,
            &format!("/api/routines/{id}/run"),
            Some(&owner),
            &json!({"trigger_source": "manual"}),
        ));
        assert!(
            !(200..300).contains(&status),
            "modified markdown run via HTTP must refuse {id}, got {status}: {body}"
        );
    }

    assert!(
        !tampered_witness.exists(),
        "script_then_markdown must not spawn script when markdown revision drifted"
    );
    assert_eq!(
        inbox_queued(&rig, "worker"),
        queued_md_before,
        "markdown drift refusal must queue zero messages"
    );

    let (md_drift_status, md_drift_http) =
        status_and_json(get(&rig, &format!("/api/routines/{rtn_md}"), Some(&owner)));
    assert_eq!(md_drift_status, 200);
    assert_eq!(md_drift_http["routine"]["status"], "waiting_approval");
    assert_eq!(md_drift_http["routine"]["approval_drift"], true);
    assert_eq!(
        md_drift_http["routine"]["runs"].as_array().unwrap().len(),
        runs_md_before
    );

    let both_md_drift = rpc_as(
        &rig,
        Asserted::Operator,
        "routine_show",
        json!({"id": rtn_both}),
    )
    .unwrap();
    assert_eq!(both_md_drift["routine"]["status"], "waiting_approval");
    assert_eq!(both_md_drift["routine"]["approval_drift"], true);
    assert_eq!(
        both_md_drift["routine"]["runs"].as_array().unwrap().len(),
        runs_both_md_before
    );

    // Owner re-approves `rev_v2` -> both `rtn_md` and `rtn_both` run cleanly again.
    let (reapprove_md_status, reapproved_md) = status_and_json(post(
        &rig,
        &format!("/api/routines/{rtn_md}/approve"),
        Some(&owner),
        &json!({"expect_rev": rev_v2}),
    ));
    assert_eq!(
        reapprove_md_status, 200,
        "owner re-approve markdown failed: {reapproved_md}"
    );
    assert_eq!(reapproved_md["routine"]["status"], "on");

    let (run_md_status, run_md_body) = status_and_json(post(
        &rig,
        &format!("/api/routines/{rtn_md}/run"),
        Some(&owner),
        &json!({"trigger_source": "manual"}),
    ));
    assert_eq!(
        run_md_status, 200,
        "markdown run after re-approval failed: {run_md_body}"
    );

    let (reapprove_both_md_status, reapproved_both_md) = status_and_json(post(
        &rig,
        &format!("/api/routines/{rtn_both}/approve"),
        Some(&owner),
        &json!({"expect_rev": rev_v2, "digest": sha_v2}),
    ));
    assert_eq!(
        reapprove_both_md_status, 200,
        "owner re-approve script_then_markdown after markdown edit failed: {reapproved_both_md}"
    );
    assert_eq!(reapproved_both_md["routine"]["status"], "on");

    let (run_both_md_status, run_both_md_body) = status_and_json(post(
        &rig,
        &format!("/api/routines/{rtn_both}/run"),
        Some(&owner),
        &json!({"trigger_source": "manual"}),
    ));
    assert_eq!(
        run_both_md_status, 200,
        "script_then_markdown run after markdown re-approval failed: {run_both_md_body}"
    );
}

#[test]
fn connections_isolation_gates_external_triggers_and_scrubs_shared_credentials_on_personal_switch()
{
    let rig = start();
    let owner = session(&rig, "owner", "owner");
    register_inbox_agent(&rig, "worker");

    let wiki_rel = "global/playbooks/inbound.md";
    let md_rev = write_wiki_playbook(
        &rig,
        &owner,
        wiki_rel,
        "# Inbound Event Triage\nInspect verified external events.\n",
    );

    // Enroll shared connection #1 and verify policy custody stays intact under `shared`.
    let conn_id = enroll_shared_connection(&rig, WORKSPACE_ACCOUNT_1, "shared-secret-token-xyz");
    let shared_set = rpc_as(
        &rig,
        Asserted::Operator,
        "connection_policy_set",
        json!({
            "connection_id": conn_id,
            "visibility": "shared",
            "write_mode": "ask_approval",
            "allow_external_triggers": false,
        }),
    )
    .expect("set shared policy with allow_external_triggers=false");
    assert_eq!(shared_set["ok"], true);
    assert_eq!(shared_set["policy"]["visibility"], "shared");
    assert_eq!(shared_set["policy"]["allow_external_triggers"], false);
    assert_eq!(shared_set["shared_credential_cleared"], false);

    let shown = rpc_as(
        &rig,
        Asserted::Operator,
        "connection_show",
        json!({"connection_id": conn_id}),
    )
    .expect("shared policy keeps enrolled custody intact");
    assert_eq!(shown["connection"]["kind"], "enrolled");
    assert_eq!(shown["connection"]["status"]["custody_available"], true);

    // Create & approve an event routine bound to `conn_id`.
    let created = rpc_as(
        &rig,
        Asserted::Operator,
        "routine_create",
        routine_create_payload(
            "External event triage",
            "worker",
            "event",
            "markdown",
            Some("wiki:global/playbooks/inbound.md"),
            None,
            Some(&conn_id),
        ),
    )
    .expect("create connection-bound routine");
    let rtn_id = created["routine"]["id"].as_str().unwrap().to_string();

    let approved = rpc_as(
        &rig,
        Asserted::Operator,
        "routine_approve",
        json!({"id": rtn_id, "expect_rev": md_rev}),
    )
    .expect("approve connection-bound routine");
    assert_eq!(approved["routine"]["status"], "on");

    // Untrusted `external_event` trigger MUST be refused on both RPC and HTTP while
    // `visibility == "shared"` and `allow_external_triggers == false`.
    let queued_before = inbox_queued(&rig, "worker");
    assert!(
        rpc_as(
            &rig,
            Asserted::Operator,
            "routine_run",
            json!({"id": rtn_id, "trigger_source": "external_event"}),
        )
        .is_err(),
        "external_event trigger via RPC must refuse when allow_external_triggers == false"
    );
    let (ext_denied_status, ext_denied_body) = status_and_json(post(
        &rig,
        &format!("/api/routines/{rtn_id}/run"),
        Some(&owner),
        &json!({"trigger_source": "external_event"}),
    ));
    assert!(
        !(200..300).contains(&ext_denied_status),
        "external_event trigger via HTTP must refuse when allow_external_triggers == false, \
         got {ext_denied_status}: {ext_denied_body}"
    );
    let shown_rtn = rpc_as(
        &rig,
        Asserted::Operator,
        "routine_show",
        json!({"id": rtn_id}),
    )
    .unwrap();
    assert!(
        shown_rtn["routine"]["runs"].as_array().unwrap().is_empty(),
        "refused external_event trigger must record zero runs: {shown_rtn}"
    );
    assert_eq!(inbox_queued(&rig, "worker"), queued_before);

    // Owner enables `allow_external_triggers: true` via HTTP -> `external_event` now succeeds.
    let (allow_status, allow_body) = status_and_json(post(
        &rig,
        &format!("/api/connections/{conn_id}/policy"),
        Some(&owner),
        &json!({
            "visibility": "shared",
            "write_mode": "ask_approval",
            "allow_external_triggers": true,
        }),
    ));
    assert_eq!(
        allow_status, 200,
        "owner enable allow_external_triggers failed: {allow_body}"
    );
    assert_eq!(allow_body["policy"]["allow_external_triggers"], true);
    assert_eq!(allow_body["shared_credential_cleared"], false);

    let (get_pol_status, get_pol_body) = status_and_json(get(
        &rig,
        &format!("/api/connections/{conn_id}/policy"),
        Some(&owner),
    ));
    assert_eq!(get_pol_status, 200);
    assert_eq!(get_pol_body["policy"]["allow_external_triggers"], true);

    let (ext_ok_status, ext_ok_body) = status_and_json(post(
        &rig,
        &format!("/api/routines/{rtn_id}/run"),
        Some(&owner),
        &json!({"trigger_source": "external_event"}),
    ));
    assert_eq!(
        ext_ok_status, 200,
        "external_event trigger must succeed after owner opts in: {ext_ok_body}"
    );
    assert!(ext_ok_body["run"]["id"].as_str().is_some());

    // Switching `conn_id` from `shared` to `personal` via HTTP scrubs the shared credential first.
    let (personal_http_status, personal_http) = status_and_json(post(
        &rig,
        &format!("/api/connections/{conn_id}/policy"),
        Some(&owner),
        &json!({
            "visibility": "personal",
            "write_mode": "ask_approval",
            "allow_external_triggers": false,
        }),
    ));
    assert_eq!(
        personal_http_status, 200,
        "owner HTTP switch shared -> personal failed: {personal_http}"
    );
    assert_eq!(personal_http["policy"]["visibility"], "personal");
    assert_eq!(
        personal_http["shared_credential_cleared"], true,
        "HTTP shared -> personal must report shared_credential_cleared == true: {personal_http}"
    );

    let list_after_http =
        rpc_as(&rig, Asserted::Operator, "connection_list", json!({})).expect("connection_list");
    let still_enrolled_http = list_after_http["connections"]
        .as_array()
        .expect("connections array")
        .iter()
        .any(|row| row["id"] == conn_id && row["kind"] == "enrolled");
    let show_after_http = rpc_as(
        &rig,
        Asserted::Operator,
        "connection_show",
        json!({"connection_id": conn_id}),
    );
    assert!(
        !still_enrolled_http,
        "connection_list must no longer list {conn_id} as enrolled after HTTP personal switch: \
         {list_after_http}"
    );
    assert!(
        show_after_http.is_err()
            || show_after_http.as_ref().unwrap()["connection"]["kind"] != "enrolled"
            || show_after_http.as_ref().unwrap()["connection"]["status"]["custody_available"]
                == false,
        "connection_show must not report live enrolled custody after HTTP personal switch: \
         {show_after_http:?}"
    );

    // Verify the Daemon RPC path (`connection_policy_set`) enforces the identical credential scrub.
    let conn_id_2 = enroll_shared_connection(&rig, WORKSPACE_ACCOUNT_2, "shared-secret-token-two");
    rpc_as(
        &rig,
        Asserted::Operator,
        "connection_policy_set",
        json!({
            "connection_id": conn_id_2,
            "visibility": "shared",
            "write_mode": "read_only",
            "allow_external_triggers": false,
        }),
    )
    .expect("set shared policy on second connection");

    let personal_rpc = rpc_as(
        &rig,
        Asserted::Operator,
        "connection_policy_set",
        json!({
            "connection_id": conn_id_2,
            "visibility": "personal",
            "write_mode": "read_only",
            "allow_external_triggers": false,
        }),
    )
    .expect("RPC shared -> personal succeeds");
    assert_eq!(personal_rpc["ok"], true);
    assert_eq!(personal_rpc["policy"]["visibility"], "personal");
    assert_eq!(
        personal_rpc["shared_credential_cleared"], true,
        "RPC shared -> personal must report shared_credential_cleared == true: {personal_rpc}"
    );

    let list_after_rpc =
        rpc_as(&rig, Asserted::Operator, "connection_list", json!({})).expect("connection_list");
    let still_enrolled_rpc = list_after_rpc["connections"]
        .as_array()
        .expect("connections array")
        .iter()
        .any(|row| row["id"] == conn_id_2 && row["kind"] == "enrolled");
    let show_after_rpc = rpc_as(
        &rig,
        Asserted::Operator,
        "connection_show",
        json!({"connection_id": conn_id_2}),
    );
    assert!(
        !still_enrolled_rpc,
        "connection_list must no longer list {conn_id_2} as enrolled after RPC personal switch: \
         {list_after_rpc}"
    );
    assert!(
        show_after_rpc.is_err()
            || show_after_rpc.as_ref().unwrap()["connection"]["kind"] != "enrolled"
            || show_after_rpc.as_ref().unwrap()["connection"]["status"]["custody_available"]
                == false,
        "connection_show must not report live enrolled custody after RPC personal switch: \
         {show_after_rpc:?}"
    );
}
