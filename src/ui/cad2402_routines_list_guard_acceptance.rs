//! CAD-2402 independent refusal check for the `routines.list` screen
//! operation (authored from the ticket and the public surfaces, separate from
//! the implementer). Drives real installed packages through the real daemon
//! (`app_screen_mint`, `app_screen_consume`, `app_screen_service_call`) and the
//! real board for the member path:
//! 1. A package that declares `routines.list` (with `records.call`) is served
//!    `{routines: [...]}` to an operator session; a package that does not
//!    declare it is refused the same call.
//! 2. A member session is refused, with no rows: over the board and over the
//!    daemon's own session proof. A member mounts read-only, and
//!    `routines.list` is not a read-allowlist operation.
//! 3. Any arguments other than `{}` are refused.
//! Every refusal carries no routine data, and each refusal case has a
//! positive control on the same mount so it cannot pass on a blanket failure.

#![cfg(all(test, feature = "test-seam"))]

use super::*;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tiny_http::{Header, Response, Server};

use crate::test_seam::{scoped, Asserted};

const ORG: &str = "cad2402-synthetic-org";
const KID: &str = "cad2402-synthetic-key";
const WORKFLOW: &str = "---\ntitle: \"Post: {{topic}}\"\ngoal: \"Publish {{topic}}\"\ninputs:\n  topic: { ask: \"About what?\" }\n---\n\nWhy.\n\n## Research {{topic}}\nagent: dev-1\nsize: S\n\nDo it.\n\n### Acceptance\n- [ ] brief written\n";
const SECRET_BODY: &str = "# Triage\nReview new alerts: cad2402-secret-playbook-text.\n";
const WIKI_PATH: &str = "global/playbooks/triage.md";

struct Rig {
    _dir: tempfile::TempDir,
    state: PathBuf,
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

fn start() -> Rig {
    block_sigchld();
    let dir = tempfile::Builder::new()
        .prefix("c2402-guard")
        .tempdir()
        .unwrap();
    let state = dir.path().to_path_buf();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    let pm = state.join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    std::fs::set_permissions(&pm, std::fs::Permissions::from_mode(0o700)).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();

    let key = Ed25519KeyPair::from_seed_unchecked(&[233; 32]).unwrap();
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
                let path = request.url().split('?').next().unwrap_or("");
                let (status_code, body) = if path == "/.well-known/agenticos-board-jwks.json" {
                    (
                        200,
                        json!({"keys":[{"kty":"OKP","crv":"Ed25519","kid":KID,"x":public_key}]}),
                    )
                } else {
                    (404, json!({"ok": false, "error": {"code": "not_found"}}))
                };
                let response = Response::from_string(body.to_string())
                    .with_status_code(status_code)
                    .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                let _ = request.respond(response);
            }
        }
    }));

    let opts = crate::daemon::ServeOptions {
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
        "jti": format!("cad2402-{sub}-{}", uuid::Uuid::new_v4()),
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

/// The loopback operator session `(token, key)` the daemon proves natively.
fn operator_session(rig: &Rig) -> (String, String) {
    crate::operator_auth::ensure_secret(&rig.state).unwrap();
    let secret = crate::operator_auth::read_secret(&rig.state).unwrap();
    let nonce = rpc_as(
        rig,
        Asserted::Operator,
        "operator_link_mint",
        json!({"secret": secret, "origin": "loopback"}),
    )
    .unwrap()["nonce"]
        .clone();
    let opened = rpc_as(
        rig,
        Asserted::Operator,
        "operator_session_open",
        json!({"nonce": nonce, "origin": "loopback"}),
    )
    .unwrap();
    (
        opened["token"].as_str().unwrap().to_string(),
        opened["key"].as_str().unwrap().to_string(),
    )
}

fn cookie_token(cookie: &str) -> &str {
    cookie.split_once('=').expect("name=value cookie").1
}

/// Two routines owned by two agents, so a refusal that returned rows would be
/// visible. Returns the names the refusals must never echo.
fn seed_routines(rig: &Rig, owner: &str) -> [&'static str; 2] {
    let (status, body) = status_and_json(put(
        rig,
        "/api/wiki/file",
        owner,
        &json!({"path": WIKI_PATH, "text": SECRET_BODY}),
    ));
    assert_eq!(status, 200, "owner wiki write failed: {body}");
    for (alias, name) in [("worker-b", "Zeta sweep"), ("worker-a", "Alpha review")] {
        rpc_as(
            rig,
            Asserted::Operator,
            "agent_register",
            json!({"alias": alias, "provider": "inbox", "endpoint_kind": "inbox"}),
        )
        .expect("register fixture agent");
        rpc_as(
            rig,
            Asserted::Operator,
            "routine_create",
            json!({"name": name, "agent_alias": alias, "trigger_kind": "schedule",
                "interval_mins": 15, "timezone": "UTC",
                "target_kind": "markdown", "markdown_path": WIKI_PATH}),
        )
        .expect("operator creates a routine");
    }
    ["Zeta sweep", "Alpha review"]
}

/// Install and approve a one-screen `screen-records.v1` package declaring
/// exactly `operations`, with `records.call`, and create its context.
fn install_app(rig: &Rig, app: &str, operations: &[&str]) -> (String, String) {
    let source = tempfile::tempdir().unwrap();
    let dir = source.path().join(app);
    std::fs::create_dir_all(dir.join("workflows")).unwrap();
    std::fs::create_dir_all(dir.join("screens/main")).unwrap();
    std::fs::write(
        dir.join("app.md"),
        format!(
            "---\napp: {app}\ntitle: Routines fixture\nversion: '1'\nneeds: {{connections: [], \
             requires: {{contracts: {{app-screens: [1], screen-records: [1]}}}}}}\n---\n\nGuide.\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("workflows/do.md"), WORKFLOW).unwrap();
    let js = "(()=>{})();";
    std::fs::write(dir.join("screens/main/client.js"), js).unwrap();
    let sha = |bytes: &[u8]| format!("sha256:{:x}", Sha256::digest(bytes));
    let declaration = json!({
        "contract": "app-screens/v2", "app": app, "entry": "client.js",
        "host_contract": "screen-records.v1", "may": ["records.call"],
        "operations": operations, "tools": {},
        "assets": [{"name": "client.js", "media_type": "text/javascript",
            "size": js.len(), "sha256": sha(js.as_bytes())}],
        "provenance": {"source_digest": sha(b"cad2402-source"),
            "sdk_digest": sha(b"cad2402-sdk"), "toolchain_digest": sha(b"cad2402-toolchain")}
    });
    std::fs::write(
        dir.join("screens/main/screens.json"),
        declaration.to_string(),
    )
    .unwrap();
    let installed = rpc_as(
        rig,
        Asserted::Operator,
        "app_workspace_install",
        json!({"source": dir.to_str().unwrap()}),
    )
    .expect("install the fixture package");
    let install = installed["install_id"].as_str().unwrap().to_string();
    rpc_as(
        rig,
        Asserted::Operator,
        "app_local_install_approve",
        json!({"install_id": install, "digest": installed["digest"]}),
    )
    .expect("approve the fixture package");
    let created = rpc_as(
        rig,
        Asserted::Operator,
        "app_context_create",
        json!({"install_id": install, "label": app,
            "input_defaults": {}, "request_id": format!("{app}-context")}),
    )
    .expect("create the mount context");
    let context = created["context"]["id"]
        .as_str()
        .or_else(|| created["id"].as_str())
        .unwrap()
        .to_string();
    (install, context)
}

fn consume(rig: &Rig, receipt: &Value) -> String {
    let nonce = receipt["mount"]
        .as_str()
        .unwrap()
        .strip_prefix("/api/app-screen/")
        .unwrap();
    rpc_as(
        rig,
        Asserted::Operator,
        "app_screen_consume",
        json!({"nonce": nonce}),
    )
    .expect("consume the mount");
    receipt["action_token"]
        .as_str()
        .expect("the mount minted an action context")
        .to_string()
}

/// An operator mount over the loopback session; returns its action token.
fn operator_mount(rig: &Rig, session: &(String, String), install: &str, context: &str) -> String {
    let minted = rpc_as(
        rig,
        Asserted::Operator,
        "app_screen_mint",
        json!({"install_id": install, "context_id": context, "tag": "main",
            "generation": 1, "token": session.0, "key": session.1, "origin": "loopback"}),
    )
    .expect("operator mint");
    consume(rig, &minted)
}

fn call_rpc(
    rig: &Rig,
    action: &str,
    credentials: (&str, &str, &str),
    args: Value,
) -> Result<Value> {
    rpc_as(
        rig,
        Asserted::Operator,
        "app_screen_service_call",
        json!({"action_token": action, "operation": "routines.list", "args": args,
            "token": credentials.0, "key": credentials.1, "origin": credentials.2}),
    )
}

/// Nothing a routine owns may appear in a refusal.
fn assert_no_routine_data(text: &str, names: &[&str], what: &str) {
    for needle in names.iter().copied().chain([
        "worker-a",
        "worker-b",
        "cad2402-secret-playbook-text",
        WIKI_PATH,
    ]) {
        assert!(!text.contains(needle), "{what} leaked {needle:?}: {text}");
    }
}

fn assert_served(reply: &Value, names: &[&str]) {
    let keys = |value: &Value| -> Vec<String> {
        let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    };
    assert_eq!(keys(reply), ["routines"], "reply shape: {reply}");
    let rows = reply["routines"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "both routines are listed: {reply}");
    assert_eq!(rows[0]["agent"], "worker-a", "ordered by agent: {reply}");
    assert_eq!(rows[1]["agent"], "worker-b");
    for row in rows {
        let mut got = keys(row);
        got.retain(|key| key != "next_run_at");
        assert_eq!(
            got,
            [
                "agent",
                "asks_first",
                "id",
                "name",
                "schedule_text",
                "status"
            ],
            "row key set: {row}"
        );
        assert!(matches!(
            row["status"].as_str(),
            Some("on" | "paused" | "repoint")
        ));
        assert!(row["asks_first"].is_boolean());
    }
    let text = reply.to_string();
    for name in names {
        assert!(text.contains(name), "{name} missing from {text}");
    }
    for leaked in [
        "cad2402-secret-playbook-text",
        "global/playbooks",
        "markdown",
    ] {
        assert!(!text.contains(leaked), "{leaked} leaked into {text}");
    }
}

#[test]
fn cad2402_declared_package_is_served_to_an_operator_and_an_undeclared_one_is_refused() {
    let rig = start();
    let owner = session(&rig, "owner", "owner");
    let names = seed_routines(&rig, &owner);
    let operator = operator_session(&rig);
    let creds = (operator.0.as_str(), operator.1.as_str(), "loopback");

    let (install, context) = install_app(&rig, "cad2402-declared", &["routines.list"]);
    let declared = operator_mount(&rig, &operator, &install, &context);
    let reply = call_rpc(&rig, &declared, creds, json!({})).expect("declared operator call");
    assert_served(&reply, &names);

    // Same operator, same call, a package that does not declare it.
    let (install, context) = install_app(&rig, "cad2402-undeclared", &["records.list"]);
    let undeclared = operator_mount(&rig, &operator, &install, &context);
    let error = call_rpc(&rig, &undeclared, creds, json!({}))
        .expect_err("an undeclared operation must be refused");
    assert_no_routine_data(&error.to_string(), &names, "undeclared refusal");
    // The declared mount still serves: the refusal was not a blanket failure.
    assert!(call_rpc(&rig, &declared, creds, json!({})).is_ok());
}

#[test]
fn cad2402_any_argument_other_than_empty_object_is_refused_with_no_rows() {
    let rig = start();
    let owner = session(&rig, "owner", "owner");
    let names = seed_routines(&rig, &owner);
    let operator = operator_session(&rig);
    let creds = (operator.0.as_str(), operator.1.as_str(), "loopback");
    let (install, context) = install_app(&rig, "cad2402-args", &["routines.list"]);
    let action = operator_mount(&rig, &operator, &install, &context);

    assert_served(
        &call_rpc(&rig, &action, creds, json!({})).expect("positive control"),
        &names,
    );
    for bad in [
        json!({"agent": "x"}),
        json!({"agent": "worker-a"}),
        json!({"limit": 5}),
        json!({"routines": []}),
        json!({"": 1}),
        json!({"agent": null}),
    ] {
        let error = call_rpc(&rig, &action, creds, bad.clone())
            .expect_err(&format!("arguments {bad} must be refused"));
        assert_no_routine_data(&error.to_string(), &names, &format!("{bad} refusal"));
    }
    // The mount is intact after the refusals.
    assert_served(&call_rpc(&rig, &action, creds, json!({})).unwrap(), &names);
}

#[test]
fn cad2402_member_session_is_refused_with_no_rows_over_the_board_and_the_daemon() {
    let rig = start();
    let owner = session(&rig, "owner", "owner");
    let member = session(&rig, "member", "member");
    let names = seed_routines(&rig, &owner);
    let (install, context) = install_app(&rig, "cad2402-member", &["routines.list"]);

    // A member mounts read-only, so the declared operation is not even
    // advertised on its mount.
    let (status, minted) = status_and_json(post(
        &rig,
        &format!("/api/app-installations/{install}/screens/main/mount"),
        Some(&member),
        &json!({"context_id": context}),
    ));
    assert_eq!(status, 200, "member mount failed: {minted}");
    assert!(
        !minted["operations"].to_string().contains("routines.list"),
        "a member mount must not advertise routines.list: {minted}"
    );
    let action = consume(&rig, &minted);

    let (status, body) = status_and_json(post(
        &rig,
        "/api/app-screen-services/call",
        Some(&member),
        &json!({"action_token": action, "operation": "routines.list", "args": {}}),
    ));
    assert!(
        matches!(status, 401 | 403),
        "member call over the board must be refused, got {status}: {body}"
    );
    assert_no_routine_data(&body.to_string(), &names, "member board refusal");

    // The daemon's own session proof refuses the same member credential.
    let error = call_rpc(
        &rig,
        &action,
        (cookie_token(&member), "", "public"),
        json!({}),
    )
    .expect_err("the daemon must refuse a member session");
    assert_no_routine_data(&error.to_string(), &names, "member daemon refusal");
}

#[test]
fn cad2402_owner_session_over_the_board_is_the_positive_control_for_the_member_refusal() {
    let rig = start();
    let owner = session(&rig, "owner", "owner");
    let names = seed_routines(&rig, &owner);
    let (install, context) = install_app(&rig, "cad2402-owner", &["routines.list"]);
    let (status, minted) = status_and_json(post(
        &rig,
        &format!("/api/app-installations/{install}/screens/main/mount"),
        Some(&owner),
        &json!({"context_id": context}),
    ));
    assert_eq!(status, 200, "owner mount failed: {minted}");
    let action = consume(&rig, &minted);
    let (status, body) = status_and_json(post(
        &rig,
        "/api/app-screen-services/call",
        Some(&owner),
        &json!({"action_token": action, "operation": "routines.list", "args": {}}),
    ));
    assert_eq!(status, 200, "owner call over the board failed: {body}");
    assert_served(&body, &names);
}
