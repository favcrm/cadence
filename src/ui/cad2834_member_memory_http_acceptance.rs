//! CAD-2834 independent HTTP acceptance tests for member-facing personal
//! assistant memory endpoints:
//! - `POST /api/member-assistant/memory-get`
//! - `POST /api/member-assistant/memory-edit`
//! - `POST /api/member-assistant/memory-undo`
//! - Durable `context_receipt` round-trip via `POST /api/member-assistant/history`
//!   + `memory-undo`
//! - Direct `member_assist_memory_edit` mid-session delta preservation on
//!   `MemberContextTracker.delivered`
//!
//! Authored strictly against the CAD-2834 endpoint & security contract using
//! the real daemon + real board + real loopback JWKS/authority test rig.

use super::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

const BOARD_AUTH_VERSION: &str = "hosted-cadence-board-assistant.v1";

static TURN_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_turn_msg_id(prefix: &str) -> String {
    let n = TURN_SEQ.fetch_add(1, AtomicOrdering::Relaxed);
    format!("{prefix}-{n}")
}

#[derive(Clone, Debug)]
struct SeenBoardAuth {
    method: String,
    authorization: String,
    cookie_present: bool,
    body: Value,
}

fn sso_assertion(rig: &Rig, sub: &str) -> String {
    let now = crate::issue::time::now_epoch();
    let header = json!({"alg": "EdDSA", "typ": "JWT", "kid": KID});
    let claims = json!({
        "iss": rig.issuer,
        "aud": rig.public_host,
        "sub": sub,
        "email": format!("{sub}@cad2834.example.test"),
        "name": sub,
        "company": ORG,
        "role": "member",
        "iat": now,
        "exp": now + 60,
        "jti": format!("cad2834-sso-{}", uuid::Uuid::new_v4()),
    });
    let signed = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap()),
    );
    format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(rig.key.sign(signed.as_bytes()).as_ref()),
    )
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .build(),
    )
}

fn post_with_headers(
    rig: &Rig,
    path: &str,
    headers: &[(&str, &str)],
    body: &Value,
) -> (u16, Value, String) {
    let url = format!("http://127.0.0.1:{}{path}", rig.board_port);
    let mut request = http_agent()
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Host", &rig.public_host);
    for (name, val) in headers {
        request = request.header(*name, *val);
    }
    let mut resp = request
        .send_json(body)
        .expect("real board HTTP request failed");
    let status = resp.status().as_u16();
    let raw = resp.body_mut().read_to_string().unwrap_or_default();
    let parsed = serde_json::from_str(&raw).unwrap_or(Value::Null);
    (status, parsed, raw)
}

fn member_post(rig: &Rig, path: &str, cookie: Option<&str>, body: &Value) -> (u16, Value, String) {
    let origin = format!("http://{}", rig.public_host);
    let mut headers = vec![
        ("Origin", origin.as_str()),
        ("Sec-Fetch-Site", "same-origin"),
        ("X-Cadence-Board", "1"),
    ];
    if let Some(c) = cookie {
        headers.push(("Cookie", c));
    }
    post_with_headers(rig, path, &headers, body)
}

fn open_member_session(rig: &Rig, sub: &str) -> (String, String) {
    let origin = format!("http://{}", rig.public_host);
    let url = format!("http://127.0.0.1:{}/__platform/session", rig.board_port);
    let resp = http_agent()
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Host", &rig.public_host)
        .header("Origin", &origin)
        .header("Sec-Fetch-Site", "same-origin")
        .header("X-Cadence-Board", "1")
        .send_json(json!({"assertion": sso_assertion(rig, sub)}))
        .expect("SSO exchange HTTP request failed");
    assert_eq!(resp.status().as_u16(), 200, "SSO exchange refused");
    let cookie = resp
        .headers()
        .get("set-cookie")
        .expect("SSO returned no public cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    assert!(
        cookie.starts_with("__Host-aos-board-session="),
        "unexpected cookie prefix"
    );
    let token = cookie.split_once('=').unwrap().1.to_string();
    drop(resp);
    let checked = crate::client::rpc(
        rig._dir.path(),
        "board_session_check",
        json!({"token": token}),
    )
    .unwrap();
    assert_eq!(checked["valid"], true, "real public session is not live");
    assert_eq!(checked["session"]["user"]["sub"], sub);
    assert_eq!(checked["session"]["origin"], "public");
    (cookie, token)
}

/// Install a live `/__platform/assistant/board-authorize` responder that mints a
/// fresh single-use signed envelope for `target_sub` on each call.
fn install_board_authorize(
    rig: &Rig,
    target_sub: &str,
) -> (Arc<Mutex<Vec<SeenBoardAuth>>>, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let minted = Arc::new(Mutex::new(Vec::new()));
    let seen_cb = seen.clone();
    let minted_cb = minted.clone();

    let key = Ed25519KeyPair::from_seed_unchecked(&[140; 32]).unwrap();
    let issuer = rig.issuer.clone();
    let public_host = rig.public_host.clone();
    let sub = target_sub.to_string();

    let cb = Box::new(move |mut request: tiny_http::Request| {
        let mut bytes = Vec::new();
        request
            .as_reader()
            .take(64 * 1024)
            .read_to_end(&mut bytes)
            .unwrap();
        seen_cb.lock().unwrap().push(SeenBoardAuth {
            method: request.method().as_str().to_string(),
            authorization: request
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.as_str().to_string())
                .unwrap_or_default(),
            cookie_present: request.headers().iter().any(|h| h.field.equiv("Cookie")),
            body: serde_json::from_slice(&bytes).expect("authority request must be JSON"),
        });

        let now = crate::issue::time::now_epoch();
        let h = json!({"alg": "EdDSA", "typ": "agenticos-assistant-actor/1", "kid": KID});
        let c = json!({
            "iss": issuer,
            "aud": format!("http://{public_host}"),
            "sub": sub,
            "actor": format!("users/{sub}"),
            "principal_kind": "user",
            "role_at_issue": "member",
            "organization_id": ORG,
            "assistant_id": format!("assist_{sub}"),
            "scope": ["assist.read", "assist.write", "assist.jobs"],
            "credential_id": format!("cred-{sub}"),
            "iat": now,
            "exp": now + 120,
            "jti": format!("cad2834-jti-{sub}-{}", uuid::Uuid::new_v4()),
        });
        let signed = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&h).unwrap()),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&c).unwrap()),
        );
        let envelope = format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(key.sign(signed.as_bytes()).as_ref()),
        );
        minted_cb.lock().unwrap().push(envelope.clone());

        let response = Response::from_string(
            json!({
                "version": BOARD_AUTH_VERSION,
                "envelope": envelope,
                "expires_at": now + 120,
            })
            .to_string(),
        )
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
        request.respond(response).unwrap();
    });

    let mut auth = rig.authority.lock().unwrap();
    auth.board_authorize = None;
    auth.board_authorize_fn = Some(cb);
    (seen, minted)
}

fn error_code(body: &Value) -> Option<&str> {
    body.get("code").and_then(Value::as_str).or_else(|| {
        body.get("error")
            .and_then(|e| e.get("code"))
            .and_then(Value::as_str)
    })
}

fn shared_for(rig: &Rig) -> Arc<crate::daemon::Shared> {
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap()
}

fn snapshot_dir(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&path)
                    .map(|p| p.to_string_lossy().into_owned().into_bytes())
                    .unwrap_or_default();
                out.insert(format!("symlink:{rel}"), target);
            } else if meta.is_dir() {
                out.insert(format!("dir:{rel}/"), Vec::new());
                walk(base, &path, out);
            } else {
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn assert_board_auth_wire(seen: &[SeenBoardAuth], token: &str) {
    assert!(!seen.is_empty(), "expected board-authorize call");
    for call in seen {
        assert_eq!(call.method, "POST");
        assert!(
            call.authorization == format!("BoardSession {token}"),
            "wrong board-authorize possession proof"
        );
        assert!(
            !call.cookie_present,
            "browser Cookie header leaked to platform board-authorize"
        );
        assert_eq!(call.body["version"], BOARD_AUTH_VERSION);
    }
}

fn finish_running_turns(shared: &crate::daemon::Shared, alias: &str) {
    for msg in shared.store.messages(alias).unwrap() {
        if msg.state == "running" {
            let _ = shared.store.finish(
                &msg,
                "completed",
                &json!({"status": "completed", "text": "done"}),
                None,
            );
            if shared
                .store
                .message(&msg.id)
                .unwrap()
                .is_some_and(|cur| cur.state == "running")
            {
                shared
                    .store
                    .fixture_write(|tx| {
                        tx.execute(
                            "UPDATE messages SET state='done' WHERE id=?",
                            rusqlite::params![msg.id],
                        )?;
                        Ok(())
                    })
                    .unwrap();
            }
        }
    }
}

fn start_active_member_turn(
    shared: &crate::daemon::Shared,
    sub: &str,
    alias: &str,
    body: &str,
) -> (String, String) {
    finish_running_turns(shared, alias);
    let binding = shared
        .store
        .assistant_binding(ORG, sub)
        .unwrap()
        .expect("assistant binding missing");
    let gen_hex = "0123456789abcdef0123456789abcdef";
    shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE agents SET generation=?, state='idle', params=? WHERE alias=?",
                rusqlite::params![
                    gen_hex,
                    json!({"report_timeout_secs": 3600}).to_string(),
                    alias
                ],
            )?;
            Ok(())
        })
        .unwrap();
    let turn_token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(gen_hex);
    let msg_id = next_turn_msg_id("cad2834-turn");
    shared
        .store
        .enqueue_sent(
            alias,
            body,
            None,
            &msg_id,
            &format!("member-{sub}"),
            None,
            &crate::store::Sender::VerifiedMember(crate::store::MemberAuthority {
                org: ORG.to_string(),
                sub: sub.to_string(),
                assistant_id: binding.assistant_id,
                credential_id: format!("cred-{sub}"),
            }),
        )
        .unwrap();
    shared.store.mark_running(&msg_id, &turn_token).unwrap();
    (msg_id, turn_token)
}

#[test]
fn cad2834_member_memory_http_refuses_unauthenticated_and_csrf_guards() {
    let _flag = AssistantFlagOn::arm();
    let rig = start(&["alice"]);
    let (alice_cookie, alice_token) = open_member_session(&rig, "alice");
    let (seen, _) = install_board_authorize(&rig, "alice");

    let (status, state_body, _) = member_post(
        &rig,
        "/api/member-assistant/state",
        Some(&alice_cookie),
        &json!({}),
    );
    assert_eq!(status, 200, "alice state init failed: {state_body}");
    let alias = state_body["assistant"]["alias"]
        .as_str()
        .expect("missing assistant alias")
        .to_string();

    // Positive control: authenticated memory-get returns 200.
    let (status, get_body, _) = member_post(
        &rig,
        "/api/member-assistant/memory-get",
        Some(&alice_cookie),
        &json!({}),
    );
    assert_eq!(
        status, 200,
        "positive control memory-get refused: {get_body}"
    );
    assert_board_auth_wire(&seen.lock().unwrap(), &alice_token);

    let shared = shared_for(&rig);
    let alice_cwd = PathBuf::from(shared.store.agent(&alias).unwrap().cwd);
    let pm_before = tree(&rig.pm);
    let cwd_before = snapshot_dir(&alice_cwd);

    // 1. Unauthenticated (missing session cookie) on memory-get / memory-edit / memory-undo -> 401.
    seen.lock().unwrap().clear();
    for (path, payload) in [
        ("/api/member-assistant/memory-get", json!({})),
        (
            "/api/member-assistant/memory-edit",
            json!({"target": "MEMORY.md", "content": "unauthenticated write"}),
        ),
        (
            "/api/member-assistant/memory-undo",
            json!({"history_id": "h-unauth"}),
        ),
    ] {
        let (status, body, _) = member_post(&rig, path, None, &payload);
        assert!(
            status == 401 || status == 403,
            "unauthenticated {path} was not refused 401/403: {status} {body}"
        );
        if status == 403 {
            assert_eq!(
                body["check"], "board_session_required",
                "unexpected 403 check on unauthenticated {path}: {body}"
            );
        }
    }
    assert!(
        seen.lock().unwrap().is_empty(),
        "unauthenticated request reached board-authorize"
    );
    assert_eq!(tree(&rig.pm), pm_before, "unauthenticated call mutated PM");
    assert_eq!(
        snapshot_dir(&alice_cwd),
        cwd_before,
        "unauthenticated call mutated private files"
    );

    // 2. Missing Origin, cross-site Origin, and missing X-Cadence-Board: 1 -> 400/403.
    let same_origin = format!("http://{}", rig.public_host);
    for (path, payload) in [
        ("/api/member-assistant/memory-get", json!({})),
        (
            "/api/member-assistant/memory-edit",
            json!({"target": "MEMORY.md", "content": "csrf write"}),
        ),
        (
            "/api/member-assistant/memory-undo",
            json!({"history_id": "h-csrf"}),
        ),
    ] {
        // Missing Origin
        let (status, body, _) = post_with_headers(
            &rig,
            path,
            &[("Cookie", &alice_cookie), ("X-Cadence-Board", "1")],
            &payload,
        );
        assert!(
            status == 400 || status == 403,
            "missing Origin on {path} was not refused 400/403: {status} {body}"
        );

        // Cross-site Origin
        let (status, body, _) = post_with_headers(
            &rig,
            path,
            &[
                ("Cookie", &alice_cookie),
                ("Origin", "https://evil.example.test"),
                ("Sec-Fetch-Site", "cross-site"),
                ("X-Cadence-Board", "1"),
            ],
            &payload,
        );
        assert!(
            status == 400 || status == 403,
            "cross-site Origin on {path} was not refused 400/403: {status} {body}"
        );

        // Missing X-Cadence-Board: 1
        let (status, body, _) = post_with_headers(
            &rig,
            path,
            &[
                ("Cookie", &alice_cookie),
                ("Origin", &same_origin),
                ("Sec-Fetch-Site", "same-origin"),
            ],
            &payload,
        );
        assert!(
            status == 400 || status == 403,
            "missing X-Cadence-Board on {path} was not refused 400/403: {status} {body}"
        );
    }
    assert!(
        seen.lock().unwrap().is_empty(),
        "CSRF-refused request reached board-authorize"
    );
    assert_eq!(tree(&rig.pm), pm_before, "CSRF-refused call mutated PM");
    assert_eq!(
        snapshot_dir(&alice_cwd),
        cwd_before,
        "CSRF-refused call mutated private files"
    );
}

#[test]
fn cad2834_member_memory_http_refuses_identity_injection_and_cross_member_mismatch() {
    let _flag = AssistantFlagOn::arm();
    let rig = start(&["alice", "bob"]);

    // Seed Bob's private memory via Bob's own real session & edit.
    let (bob_cookie, _) = open_member_session(&rig, "bob");
    let (_, _) = install_board_authorize(&rig, "bob");
    let (status, bob_state, _) = member_post(
        &rig,
        "/api/member-assistant/state",
        Some(&bob_cookie),
        &json!({}),
    );
    assert_eq!(status, 200, "bob state failed: {bob_state}");
    let bob_alias = bob_state["assistant"]["alias"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, bob_edit, _) = member_post(
        &rig,
        "/api/member-assistant/memory-edit",
        Some(&bob_cookie),
        &json!({
            "target": "MEMORY.md",
            "content": "# Bob Private\n- CAD2834 bob confidential note\n",
        }),
    );
    assert_eq!(status, 200, "bob positive memory-edit failed: {bob_edit}");
    let bob_hist_id = bob_edit["history_id"].as_str().unwrap().to_string();

    // Provision Alice and positive-control her own memory-get.
    let (alice_cookie, alice_token) = open_member_session(&rig, "alice");
    let (seen, _) = install_board_authorize(&rig, "alice");
    let (status, alice_state, _) = member_post(
        &rig,
        "/api/member-assistant/state",
        Some(&alice_cookie),
        &json!({}),
    );
    assert_eq!(status, 200, "alice state failed: {alice_state}");
    let alice_alias = alice_state["assistant"]["alias"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, alice_get, _) = member_post(
        &rig,
        "/api/member-assistant/memory-get",
        Some(&alice_cookie),
        &json!({}),
    );
    assert_eq!(status, 200, "alice positive memory-get failed: {alice_get}");

    let shared = shared_for(&rig);
    let alice_cwd = PathBuf::from(shared.store.agent(&alice_alias).unwrap().cwd);
    let bob_cwd = PathBuf::from(shared.store.agent(&bob_alias).unwrap().cwd);
    let pm_before = tree(&rig.pm);
    let alice_before = snapshot_dir(&alice_cwd);
    let bob_before = snapshot_dir(&bob_cwd);

    // 3. Identity-field injection -> 400 identity_field_refused.
    seen.lock().unwrap().clear();
    let bob_env = rig.envelope("bob", json!({}), json!({}));
    for field in ["sub", "org", "assistant_id", "member_envelope", "actor"] {
        let injected_val = if field == "member_envelope" {
            json!(bob_env.clone())
        } else {
            json!("bob")
        };

        let mut get_payload = json!({});
        get_payload[field] = injected_val.clone();
        let (status, body, _) = member_post(
            &rig,
            "/api/member-assistant/memory-get",
            Some(&alice_cookie),
            &get_payload,
        );
        assert_eq!(
            status, 400,
            "identity field {field} on memory-get not 400: {body}"
        );
        assert_eq!(
            error_code(&body),
            Some("identity_field_refused"),
            "wrong code for {field} on memory-get: {body}"
        );

        let mut edit_payload = json!({"target": "MEMORY.md", "content": "injected"});
        edit_payload[field] = injected_val.clone();
        let (status, body, _) = member_post(
            &rig,
            "/api/member-assistant/memory-edit",
            Some(&alice_cookie),
            &edit_payload,
        );
        assert_eq!(
            status, 400,
            "identity field {field} on memory-edit not 400: {body}"
        );
        assert_eq!(
            error_code(&body),
            Some("identity_field_refused"),
            "wrong code for {field} on memory-edit: {body}"
        );

        let mut undo_payload = json!({"history_id": bob_hist_id});
        undo_payload[field] = injected_val;
        let (status, body, _) = member_post(
            &rig,
            "/api/member-assistant/memory-undo",
            Some(&alice_cookie),
            &undo_payload,
        );
        assert_eq!(
            status, 400,
            "identity field {field} on memory-undo not 400: {body}"
        );
        assert_eq!(
            error_code(&body),
            Some("identity_field_refused"),
            "wrong code for {field} on memory-undo: {body}"
        );
    }
    assert!(
        seen.lock().unwrap().is_empty(),
        "identity-injected request reached board-authorize"
    );
    assert_eq!(tree(&rig.pm), pm_before);
    assert_eq!(snapshot_dir(&alice_cwd), alice_before);
    assert_eq!(snapshot_dir(&bob_cwd), bob_before);

    // 4. Cross-member envelope mismatch: Alice's cookie + platform board-authorize
    //    returning a valid signed Bob envelope -> 403 member_mismatch, Bob untouched.
    let (bob_seen, bob_minted) = install_board_authorize(&rig, "bob");
    let key = crate::board_identity::PublicKey {
        kid: KID.to_string(),
        x: rig.key.public_key().as_ref().try_into().unwrap(),
    };
    let config = crate::board_identity::Config {
        host: rig.public_host.clone(),
        issuer: rig.issuer.clone(),
        company: ORG.to_string(),
    };

    for (path, payload) in [
        ("/api/member-assistant/memory-get", json!({})),
        (
            "/api/member-assistant/memory-edit",
            json!({"target": "MEMORY.md", "content": "cross-member overwrite"}),
        ),
        (
            "/api/member-assistant/memory-undo",
            json!({"history_id": bob_hist_id}),
        ),
    ] {
        let (status, body, raw) = member_post(&rig, path, Some(&alice_cookie), &payload);
        assert_eq!(
            status, 403,
            "cross-member mismatch on {path} not refused 403: {body}"
        );
        assert_eq!(
            error_code(&body),
            Some("member_mismatch"),
            "wrong code on cross-member mismatch {path}: {body}"
        );
        assert!(
            !raw.contains(&alice_token),
            "session token leaked in {path} refusal"
        );
        assert!(
            !raw.contains("CAD2834 bob confidential note"),
            "Bob's private memory leaked in {path} refusal"
        );
        let latest_env = bob_minted.lock().unwrap().last().cloned().unwrap();
        assert!(!raw.contains(&latest_env), "Bob envelope leaked in {path}");
        let verified = crate::assistant_actor::verify(
            &latest_env,
            &config,
            &key,
            crate::issue::time::now_epoch(),
        )
        .expect("Bob fixture envelope must be cryptographically valid");
        assert_eq!(verified.sub, "bob");
    }
    assert_board_auth_wire(&bob_seen.lock().unwrap(), &alice_token);
    assert_eq!(tree(&rig.pm), pm_before);
    assert_eq!(snapshot_dir(&alice_cwd), alice_before);
    assert_eq!(snapshot_dir(&bob_cwd), bob_before);
}

#[test]
fn cad2834_member_memory_http_refuses_agent_peer_disallowed_targets_and_secrets() {
    let _flag = AssistantFlagOn::arm();
    let rig = start(&["alice"]);
    let (alice_cookie, _) = open_member_session(&rig, "alice");
    let (seen, _) = install_board_authorize(&rig, "alice");

    let (status, state_body, _) = member_post(
        &rig,
        "/api/member-assistant/state",
        Some(&alice_cookie),
        &json!({}),
    );
    assert_eq!(status, 200, "alice state failed: {state_body}");
    let alias = state_body["assistant"]["alias"]
        .as_str()
        .unwrap()
        .to_string();

    // Positive control.
    let (status, get_body, _) = member_post(
        &rig,
        "/api/member-assistant/memory-get",
        Some(&alice_cookie),
        &json!({}),
    );
    assert_eq!(status, 200, "alice positive control failed: {get_body}");

    let shared = shared_for(&rig);
    let alice_cwd = PathBuf::from(shared.store.agent(&alias).unwrap().cwd);
    let cwd_before = snapshot_dir(&alice_cwd);

    // 5a. Attributed agent peer presenting Alice's session cookie -> refused, files untouched.
    //     First test Alice's own assistant peer (refused 403/503 before reaching
    //     board-authorize), then test a non-assistant worker peer with a fresh live
    //     session cookie per request (`403 session_from_agent`, session revoked on use).
    shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "worker-cad2834",
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: rig._dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let pm_before = tree(&rig.pm);
    seen.lock().unwrap().clear();
    let seam_token = Seam::token_at(rig._dir.path()).expect("missing test seam token");
    let origin = format!("http://{}", rig.public_host);
    for (agent_alias, expect_403_only) in [(alias.as_str(), false), ("worker-cad2834", true)] {
        let agent_as = format!("agent:{agent_alias}");
        for (path, payload) in [
            ("/api/member-assistant/memory-get", json!({})),
            (
                "/api/member-assistant/memory-edit",
                json!({"target": "MEMORY.md", "content": "stolen session edit"}),
            ),
            (
                "/api/member-assistant/memory-undo",
                json!({"history_id": "h-stolen"}),
            ),
        ] {
            let cookie_for_req = if expect_403_only {
                open_member_session(&rig, "alice").0
            } else {
                alice_cookie.clone()
            };
            let (status, body, _) = post_with_headers(
                &rig,
                path,
                &[
                    ("Cookie", &cookie_for_req),
                    ("Origin", &origin),
                    ("Sec-Fetch-Site", "same-origin"),
                    ("X-Cadence-Board", "1"),
                    (crate::test_seam::AS_HEADER, &agent_as),
                    (crate::test_seam::TOKEN_HEADER, &seam_token),
                ],
                &payload,
            );
            if expect_403_only {
                assert_eq!(
                    status, 403,
                    "attributed worker peer on {path} was not refused 403: {body}"
                );
                assert_eq!(
                    body["check"], "session_from_agent",
                    "unexpected 403 check for worker peer on {path}: {body}"
                );
            } else {
                assert!(
                    status == 403 || status == 503,
                    "attributed assist peer on {path} was not refused 403/503: {status} {body}"
                );
            }
        }
    }
    assert!(
        seen.lock().unwrap().is_empty(),
        "agent-attributed session call reached board-authorize"
    );
    assert_eq!(tree(&rig.pm), pm_before);
    assert_eq!(snapshot_dir(&alice_cwd), cwd_before);

    // 5b. POST /api/assist/memory_* without a valid member envelope -> refused, files untouched.
    let other_key = Ed25519KeyPair::from_seed_unchecked(&[9; 32]).unwrap();
    let forged = {
        let now = crate::issue::time::now_epoch();
        let h = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(
                &json!({"alg": "EdDSA", "typ": "agenticos-assistant-actor/1", "kid": KID}),
            )
            .unwrap(),
        );
        let c = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "iss": rig.issuer, "aud": format!("http://{}", rig.public_host),
                "sub": "alice", "actor": "users/alice", "principal_kind": "user",
                "role_at_issue": "member", "organization_id": ORG,
                "assistant_id": "assist_alice", "scope": ["assist.read", "assist.write"],
                "credential_id": "cred-alice", "iat": now, "exp": now + 120, "jti": "cad2834-forged"
            }))
            .unwrap(),
        );
        let signed = format!("{h}.{c}");
        format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(other_key.sign(signed.as_bytes()).as_ref())
        )
    };
    let wrong_org = rig.envelope("alice", json!({}), json!({"organization_id": "other-org"}));
    let now = crate::issue::time::now_epoch();
    let expired = rig.envelope(
        "alice",
        json!({}),
        json!({"iat": now - 400, "exp": now - 100}),
    );
    let read_only = rig.envelope("alice", json!({}), json!({"scope": ["assist.read"]}));

    for (verb, env, args) in [
        ("memory_get", &forged, json!({})),
        (
            "memory_edit",
            &forged,
            json!({"target": "MEMORY.md", "content": "forged"}),
        ),
        (
            "memory_edit",
            &wrong_org,
            json!({"target": "MEMORY.md", "content": "wrong-org"}),
        ),
        (
            "memory_edit",
            &expired,
            json!({"target": "MEMORY.md", "content": "expired"}),
        ),
        (
            "memory_edit",
            &read_only,
            json!({"target": "MEMORY.md", "content": "read-only"}),
        ),
        ("memory_undo", &read_only, json!({"history_id": "h-1"})),
    ] {
        let (status, body) = rig.call(verb, env, &args);
        assert!(
            matches!(status, 401 | 403 | 404),
            "untrusted /api/assist/{verb} call was not refused: {status} {body}"
        );
    }
    assert_eq!(tree(&rig.pm), pm_before);
    assert_eq!(snapshot_dir(&alice_cwd), cwd_before);

    // Re-authenticate Alice with a fresh session for target/secret checks.
    let (fresh_cookie, _) = open_member_session(&rig, "alice");
    let (_, _) = install_board_authorize(&rig, "alice");
    let (status, body, _) = member_post(
        &rig,
        "/api/member-assistant/memory-get",
        Some(&fresh_cookie),
        &json!({}),
    );
    assert_eq!(status, 200, "fresh Alice positive control failed: {body}");

    // 6a. Disallowed targets -> 400/403, files unchanged.
    for target in [
        "AGENTS.md",
        "IDENTITY.md",
        "BOOTSTRAP.md",
        "../escape.md",
        "memory/../escape.md",
        "memory/Bad_Slug.md",
        "",
        "/etc/passwd",
    ] {
        let (status, body, _) = member_post(
            &rig,
            "/api/member-assistant/memory-edit",
            Some(&fresh_cookie),
            &json!({"target": target, "content": "disallowed target"}),
        );
        assert!(
            status == 400 || status == 403,
            "disallowed target '{target}' was not refused 400/403: {status} {body}"
        );
    }
    assert_eq!(tree(&rig.pm), pm_before);
    assert_eq!(snapshot_dir(&alice_cwd), cwd_before);

    // 6b. Secret-bearing content (synthetic ghp_ token) -> 400/403, files unchanged.
    let synthetic_ghp = format!("ghp_{}", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8");
    for target in ["MEMORY.md", "SOUL.md", "USER.md", "memory/prefs.md"] {
        let (status, body, _) = member_post(
            &rig,
            "/api/member-assistant/memory-edit",
            Some(&fresh_cookie),
            &json!({
                "target": target,
                "content": format!("# Secret test\n- token={synthetic_ghp}\n"),
            }),
        );
        assert!(
            status == 400 || status == 403,
            "secret-bearing content on '{target}' was not refused 400/403: {status} {body}"
        );
    }
    assert!(!alice_cwd.join("memory/prefs.md").exists());
    assert_eq!(tree(&rig.pm), pm_before);
    assert_eq!(snapshot_dir(&alice_cwd), cwd_before);
}

#[test]
fn cad2834_member_memory_http_round_trip_undo_and_context_receipt() {
    let _flag = AssistantFlagOn::arm();
    let rig = start(&["alice"]);
    enroll_queue_service(&rig);
    let _keys = initialize_queue_keyring(&rig);

    let (alice_cookie, alice_token) = open_member_session(&rig, "alice");
    let (seen, _) = install_board_authorize(&rig, "alice");

    let (status, state_body, _) = member_post(
        &rig,
        "/api/member-assistant/state",
        Some(&alice_cookie),
        &json!({}),
    );
    assert_eq!(status, 200, "alice state failed: {state_body}");
    let alias = state_body["assistant"]["alias"]
        .as_str()
        .unwrap()
        .to_string();
    let shared = shared_for(&rig);
    let alice_cwd = PathBuf::from(shared.store.agent(&alias).unwrap().cwd);

    // 7a. Initial memory-get reads seeded SOUL.md, USER.md, MEMORY.md.
    let (status, initial_get, _) = member_post(
        &rig,
        "/api/member-assistant/memory-get",
        Some(&alice_cookie),
        &json!({}),
    );
    assert_eq!(status, 200, "initial memory-get failed: {initial_get}");
    assert_eq!(initial_get["applies"], "next_turn");
    let files = initial_get["files"]
        .as_array()
        .expect("memory-get missing files array");
    let find_file = |resp: &Value, target: &str| -> Value {
        resp["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["target"] == target)
            .cloned()
            .unwrap_or_else(|| panic!("missing target {target} in {resp}"))
    };
    let soul0 = find_file(&initial_get, "SOUL.md");
    let user0 = find_file(&initial_get, "USER.md");
    let mem0 = find_file(&initial_get, "MEMORY.md");
    assert!(
        files.len() >= 3
            && !soul0["text"].as_str().unwrap_or_default().is_empty()
            && !user0["text"].as_str().unwrap_or_default().is_empty()
            && soul0["line_count"].is_u64()
            && user0["line_count"].is_u64()
            && mem0["line_count"].is_u64(),
        "unexpected initial files shape: {initial_get}"
    );
    assert!(initial_get["topics"].is_array());
    assert!(initial_get["history"].is_array());

    // 7b. Edit MEMORY.md (twice to verify stale expected_sha -> 409 revision_conflict).
    let mut mem_edit1_req = json!({
        "target": "MEMORY.md",
        "content": "# Memory\n- CAD2834 round-trip baseline\n",
    });
    if let Some(sha) = mem0["sha"].as_str() {
        mem_edit1_req["expected_sha"] = json!(sha);
    }
    let (status, mem_edit1, _) = member_post(
        &rig,
        "/api/member-assistant/memory-edit",
        Some(&alice_cookie),
        &mem_edit1_req,
    );
    assert_eq!(status, 200, "MEMORY.md edit 1 failed: {mem_edit1}");
    assert_eq!(mem_edit1["target"], "MEMORY.md");
    assert_eq!(mem_edit1["applies"], "next_turn");
    let mem_sha_1 = mem_edit1["sha"].as_str().expect("missing sha").to_string();
    assert!(!mem_sha_1.is_empty());

    let mem_v2_text = "# Memory\n- CAD2834 round-trip baseline\n- CAD2834 second edit\n";
    let (status, mem_edit2, _) = member_post(
        &rig,
        "/api/member-assistant/memory-edit",
        Some(&alice_cookie),
        &json!({
            "target": "MEMORY.md",
            "content": mem_v2_text,
            "expected_sha": mem_sha_1,
        }),
    );
    assert_eq!(status, 200, "MEMORY.md edit 2 failed: {mem_edit2}");
    let mem_sha_2 = mem_edit2["sha"].as_str().unwrap().to_string();
    let mem_hist_2 = mem_edit2["history_id"].as_str().unwrap().to_string();
    assert_ne!(mem_sha_2, mem_sha_1);

    // Stale expected_sha returns 409 revision_conflict and leaves file untouched.
    let (status, conflict, _) = member_post(
        &rig,
        "/api/member-assistant/memory-edit",
        Some(&alice_cookie),
        &json!({
            "target": "MEMORY.md",
            "content": "stale overwrite",
            "expected_sha": mem_sha_1,
        }),
    );
    assert_eq!(status, 409, "stale expected_sha did not 409: {conflict}");
    assert_eq!(error_code(&conflict), Some("revision_conflict"));
    assert_eq!(
        std::fs::read_to_string(alice_cwd.join("MEMORY.md")).unwrap(),
        mem_v2_text
    );

    // Multi-line SOUL.md update (twice so we also verify stale-file undo -> 409).
    let soul_v1_text = "## Voice\n- Direct and concise.\n- Cite exact file paths.\n";
    let mut soul_edit1_req = json!({
        "target": "SOUL.md",
        "content": soul_v1_text,
    });
    if let Some(sha) = soul0["sha"].as_str() {
        soul_edit1_req["expected_sha"] = json!(sha);
    }
    let (status, soul_edit1, _) = member_post(
        &rig,
        "/api/member-assistant/memory-edit",
        Some(&alice_cookie),
        &soul_edit1_req,
    );
    assert_eq!(status, 200, "SOUL.md edit 1 failed: {soul_edit1}");
    let soul_sha_1 = soul_edit1["sha"].as_str().unwrap().to_string();
    let soul_hist_1 = soul_edit1["history_id"].as_str().unwrap().to_string();

    let soul_v2_text =
        "## Voice\n- Direct and concise.\n- Cite exact file paths.\n- Never guess.\n";
    let (status, soul_edit2, _) = member_post(
        &rig,
        "/api/member-assistant/memory-edit",
        Some(&alice_cookie),
        &json!({
            "target": "SOUL.md",
            "content": soul_v2_text,
            "expected_sha": soul_sha_1,
        }),
    );
    assert_eq!(status, 200, "SOUL.md edit 2 failed: {soul_edit2}");
    let soul_sha_2 = soul_edit2["sha"].as_str().unwrap().to_string();

    // Create memory/prefs.md topic.
    let prefs_text = "# Preferences\n- Dark mode\n- Compact diffs\n";
    let (status, prefs_edit, _) = member_post(
        &rig,
        "/api/member-assistant/memory-edit",
        Some(&alice_cookie),
        &json!({
            "target": "memory/prefs.md",
            "content": prefs_text,
        }),
    );
    assert_eq!(status, 200, "memory/prefs.md create failed: {prefs_edit}");
    assert_eq!(prefs_edit["target"], "memory/prefs.md");
    assert_eq!(prefs_edit["applies"], "next_turn");
    let prefs_sha = prefs_edit["sha"].as_str().unwrap().to_string();
    let prefs_hist = prefs_edit["history_id"].as_str().unwrap().to_string();

    // Reload memory-get: verify persisted content, topic, and history fields.
    let (status, reloaded, _) = member_post(
        &rig,
        "/api/member-assistant/memory-get",
        Some(&alice_cookie),
        &json!({}),
    );
    assert_eq!(status, 200, "reload memory-get failed: {reloaded}");
    assert_eq!(find_file(&reloaded, "MEMORY.md")["text"], mem_v2_text);
    assert_eq!(find_file(&reloaded, "MEMORY.md")["sha"], mem_sha_2);
    let reloaded_soul = find_file(&reloaded, "SOUL.md");
    let reloaded_soul_text = reloaded_soul["text"].as_str().unwrap_or_default();
    assert!(
        reloaded_soul_text.contains("# Soul")
            && reloaded_soul_text.contains("<!-- cadence:managed-begin tone -->")
            && reloaded_soul_text.contains("<!-- cadence:managed-begin soul_body -->")
            && reloaded_soul_text.contains(soul_v2_text),
        "reloaded SOUL.md missing managed envelope or edited body: {reloaded_soul_text}"
    );
    assert_eq!(
        std::fs::read_to_string(alice_cwd.join("SOUL.md")).unwrap(),
        reloaded_soul_text
    );
    assert_eq!(reloaded_soul["sha"], soul_sha_2);
    let prefs_topic = reloaded["topics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["slug"] == "prefs")
        .unwrap_or_else(|| panic!("missing prefs topic in {reloaded}"));
    assert_eq!(prefs_topic["target"], "memory/prefs.md");
    assert_eq!(prefs_topic["text"], prefs_text);
    assert_eq!(prefs_topic["sha"], prefs_sha);
    assert_eq!(prefs_topic["line_count"], 3);

    let history = reloaded["history"].as_array().unwrap();
    let mem2_entry = history
        .iter()
        .find(|h| h["id"] == mem_hist_2)
        .unwrap_or_else(|| panic!("missing history entry {mem_hist_2} in {reloaded}"));
    for required in [
        "id",
        "at",
        "op",
        "target",
        "actor",
        "turn_id",
        "before_exists",
        "before_sha",
        "after_sha",
        "added",
        "removed",
        "undone_by",
    ] {
        assert!(
            mem2_entry.get(required).is_some(),
            "history entry missing field '{required}': {mem2_entry}"
        );
    }
    assert_eq!(mem2_entry["target"], "MEMORY.md");
    assert_eq!(mem2_entry["before_exists"], true);
    assert_eq!(mem2_entry["before_sha"], mem_sha_1);
    assert_eq!(mem2_entry["after_sha"], mem_sha_2);
    assert!(mem2_entry["undone_by"].is_null());

    // 7c. Undo MEMORY.md edit 2 -> restores edit 1, marks undone_by, repeat undo 409.
    let (status, undo_mem, _) = member_post(
        &rig,
        "/api/member-assistant/memory-undo",
        Some(&alice_cookie),
        &json!({"history_id": mem_hist_2}),
    );
    assert_eq!(status, 200, "memory-undo on MEMORY.md failed: {undo_mem}");
    assert_eq!(undo_mem["target"], "MEMORY.md");
    assert_eq!(undo_mem["sha"], mem_sha_1);
    assert_eq!(undo_mem["applies"], "next_turn");
    assert!(undo_mem.get("undone").is_some());
    assert!(undo_mem.get("op").is_some());
    let undo_mem_hist_id = undo_mem["undo_history_id"]
        .as_str()
        .expect("missing undo_history_id")
        .to_string();
    assert_eq!(
        std::fs::read_to_string(alice_cwd.join("MEMORY.md")).unwrap(),
        "# Memory\n- CAD2834 round-trip baseline\n"
    );

    // Repeat undo returns 409.
    let (status, repeat_undo, _) = member_post(
        &rig,
        "/api/member-assistant/memory-undo",
        Some(&alice_cookie),
        &json!({"history_id": mem_hist_2}),
    );
    assert_eq!(status, 409, "repeat undo did not 409: {repeat_undo}");

    // Undo newly created topic removes memory/prefs.md from disk and topics.
    let (status, undo_prefs, _) = member_post(
        &rig,
        "/api/member-assistant/memory-undo",
        Some(&alice_cookie),
        &json!({"history_id": prefs_hist}),
    );
    assert_eq!(status, 200, "undo new topic failed: {undo_prefs}");
    assert_eq!(undo_prefs["target"], "memory/prefs.md");
    assert_eq!(undo_prefs["applies"], "next_turn");
    assert!(!alice_cwd.join("memory/prefs.md").exists());

    // Stale file after subsequent edit: undoing soul_hist_1 after soul_edit2 returns 409.
    let (status, stale_undo, _) = member_post(
        &rig,
        "/api/member-assistant/memory-undo",
        Some(&alice_cookie),
        &json!({"history_id": soul_hist_1}),
    );
    assert_eq!(status, 409, "stale-file undo did not 409: {stale_undo}");
    assert_eq!(
        std::fs::read_to_string(alice_cwd.join("SOUL.md")).unwrap(),
        reloaded_soul_text
    );

    let (status, after_undo_get, _) = member_post(
        &rig,
        "/api/member-assistant/memory-get",
        Some(&alice_cookie),
        &json!({}),
    );
    assert_eq!(status, 200);
    assert!(
        after_undo_get["topics"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["slug"] != "prefs"),
        "undone topic still listed: {after_undo_get}"
    );
    let mem2_after = after_undo_get["history"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["id"] == mem_hist_2)
        .unwrap();
    assert_eq!(mem2_after["undone_by"], undo_mem_hist_id);

    // 7d. Real assistant_context_update("remember") on an active interactive turn ->
    //     POST /api/member-assistant/history returns durable kind: "context_receipt"
    //     with payload.history_id & target -> POST /api/member-assistant/memory-undo succeeds.
    let (_t4_id, _) = start_active_member_turn(&shared, "alice", &alias, "remember my preference");
    let remember_fact = "CAD2834_DURABLE_RECEIPT_FACT";
    let remember_res =
        crate::test_seam::scoped(crate::test_seam::Asserted::Agent(alias.clone()), || {
            crate::client::rpc(
                rig._dir.path(),
                "assistant_context_update",
                json!({"op": "remember", "fact": remember_fact}),
            )
        })
        .expect("assistant_context_update remember over daemon socket failed");
    let remember_hist_id = remember_res["history_id"]
        .as_str()
        .expect("assistant_context_update missing history_id")
        .to_string();
    assert!(std::fs::read_to_string(alice_cwd.join("MEMORY.md"))
        .unwrap()
        .contains(remember_fact));

    let (status, hist_body, _) = member_post(
        &rig,
        "/api/member-assistant/history",
        Some(&alice_cookie),
        &json!({}),
    );
    assert_eq!(status, 200, "member-assistant/history failed: {hist_body}");
    let entries = hist_body["entries"]
        .as_array()
        .expect("history response missing entries");
    let receipt = entries
        .iter()
        .find(|e| {
            e["kind"] == "context_receipt"
                && e["payload"]["history_id"].as_str() == Some(remember_hist_id.as_str())
        })
        .unwrap_or_else(|| {
            panic!("missing durable context_receipt for {remember_hist_id} in {hist_body}")
        });
    assert_eq!(
        receipt["payload"]["target"], "MEMORY.md",
        "context_receipt missing target: {receipt}"
    );

    let (status, receipt_undo, _) = member_post(
        &rig,
        "/api/member-assistant/memory-undo",
        Some(&alice_cookie),
        &json!({"history_id": remember_hist_id}),
    );
    assert_eq!(
        status, 200,
        "memory-undo of assistant_context_update receipt failed: {receipt_undo}"
    );
    assert_eq!(receipt_undo["target"], "MEMORY.md");
    assert_eq!(receipt_undo["applies"], "next_turn");
    assert!(
        !std::fs::read_to_string(alice_cwd.join("MEMORY.md"))
            .unwrap()
            .contains(remember_fact),
        "remembered fact remained on disk after memory-undo"
    );

    let (status, final_get, _) = member_post(
        &rig,
        "/api/member-assistant/memory-get",
        Some(&alice_cookie),
        &json!({}),
    );
    assert_eq!(status, 200);
    assert!(
        !find_file(&final_get, "MEMORY.md")["text"]
            .as_str()
            .unwrap_or_default()
            .contains(remember_fact),
        "remembered fact remained in memory-get after memory-undo"
    );
    assert_board_auth_wire(&seen.lock().unwrap(), &alice_token);
    finish_running_turns(&shared, &alias);
}

fn start_with_fake_pi(
    subs: &[&str],
) -> (Rig, tempfile::TempDir, std::sync::mpsc::Receiver<String>) {
    block_sigchld();
    let dir = tempfile::Builder::new()
        .prefix("c2834-http")
        .tempdir()
        .unwrap();
    let bin_dir = tempfile::Builder::new()
        .prefix("c2834-bin")
        .tempdir()
        .unwrap();
    let state = dir.path().to_path_buf();
    let pm = bin_dir.path().join("pm");
    let home = bin_dir.path().join("home");
    let bin = bin_dir.path().join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    crate::issue::Pm::init(&pm).unwrap();
    fixture_issue_project(&pm);
    let mut pm_yaml = std::fs::read_to_string(pm.join("pm.yaml")).unwrap_or_default();
    pm_yaml.push_str(
        "\npi:\n  models:\n    allow:\n      - fake/c2834-model\n    default:\n      worker: fake/c2834-model\n",
    );
    std::fs::write(pm.join("pm.yaml"), pm_yaml).unwrap();
    for root in [&state, &pm, bin_dir.path(), &home, &bin] {
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    let prompt_listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let prompt_port = prompt_listener.local_addr().unwrap().port();
    let (prompt_tx, prompt_rx) = std::sync::mpsc::channel::<String>();
    let mut threads = Vec::new();
    threads.push(std::thread::spawn(move || {
        for stream in prompt_listener.incoming().flatten() {
            let reader = std::io::BufReader::new(stream);
            for line in std::io::BufRead::lines(reader).map_while(std::result::Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if let Some(t) = v["text"].as_str() {
                        let _ = prompt_tx.send(t.to_string());
                    }
                }
            }
        }
    }));

    let fake_pi = bin.join("fake-pi.py");
    std::fs::write(
        &fake_pi,
        format!(
            r#"#!/usr/bin/env python3
import json, socket, sys
args = sys.argv[1:]
model = args[args.index("--model") + 1] if "--model" in args else "fake/c2834-model"
provider, _, model_id = model.partition("/")
session = 0
def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
for line in sys.stdin:
    req = json.loads(line)
    kind, rid = req.get("type"), req.get("id")
    reply = {{"type": "response", "id": rid, "command": kind, "success": True}}
    if kind == "get_state":
        reply["data"] = {{"sessionId": f"s{{session}}", "thinkingLevel": "medium", "model": {{"provider": provider, "id": model_id}}}}
    elif kind == "new_session":
        session += 1
        reply["data"] = {{"sessionId": f"s{{session}}"}}
    emit(reply)
    if kind == "prompt":
        text = req.get("message", "")
        with socket.create_connection(("127.0.0.1", {prompt_port}), timeout=5) as s:
            s.sendall((json.dumps({{"text": text}}) + "\n").encode("utf-8"))
        emit({{"type": "message_end", "message": {{"role": "assistant", "content": [{{"type": "text", "text": "ok"}}], "stopReason": "stop"}}}})
        emit({{"type": "agent_settled"}})
"#
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_pi, std::fs::Permissions::from_mode(0o755)).unwrap();

    let fake_confine = bin.join("fake-confine.sh");
    std::fs::write(
        &fake_confine,
        "#!/bin/sh\nwhile [ \"$1\" != \"--\" ] && [ \"$#\" -gt 0 ]; do shift; done\n[ \"$1\" = \"--\" ] && shift\nexec \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake_confine, std::fs::Permissions::from_mode(0o755)).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    {
        let daemon_state = state.clone();
        let env = crate::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
        env.set("HOME", home.to_str().unwrap());
        env.set(
            "CADENCE_PI_COMMAND",
            format!("python3 {}", fake_pi.display()),
        );
        env.set("CADENCE_CONFINE_COMMAND", fake_confine.to_str().unwrap());
        let opts = ServeOptions {
            provider_env: env,
            test_seam: true,
            stop: Some(stop.clone()),
            checkup: Some(0),
            ..ServeOptions::default()
        };
        threads.push(std::thread::spawn(move || {
            crate::daemon::serve_with(&daemon_state, opts).unwrap()
        }));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
            assert!(Instant::now() < deadline, "daemon never started");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    let key = Ed25519KeyPair::from_seed_unchecked(&[140; 32]).unwrap();
    let public_key = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let verify_key = Ed25519KeyPair::from_seed_unchecked(&[140; 32]).unwrap();
    let verify_public = verify_key.public_key().as_ref().to_vec();
    let platform = loopback_server();
    let platform_port = match platform.server_addr() {
        tiny_http::ListenAddr::IP(a) => a.port(),
        _ => panic!("fixture platform bound an unexpected address"),
    };
    let public_host = format!("localhost:{platform_port}");
    let issuer = format!("http://{public_host}");
    let authority: Arc<Mutex<Authority>> = Arc::new(Mutex::new(Authority {
        status: 200,
        ..Authority::default()
    }));
    {
        let stop = stop.clone();
        let authority = authority.clone();
        let issuer = issuer.clone();
        let public_host = public_host.clone();
        threads.push(std::thread::spawn(move || loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(Some(request)) = platform.recv_timeout(Duration::from_millis(200)) {
                answer_platform(
                    request,
                    &authority,
                    &issuer,
                    &public_host,
                    &public_key,
                    &verify_public,
                );
            }
        }));
    }

    {
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = crate::daemon::Shared::new(&state, &opts).unwrap();
        for sub in subs {
            let assistant_id = format!("assist_{sub}");
            shared
                .store
                .assistant_bind(
                    ORG,
                    sub,
                    &assistant_id,
                    &format!("assist-agent-{sub}"),
                    Some("rev0"),
                )
                .unwrap_or_else(|e| panic!("provision {sub}: {e}"));
            authority
                .lock()
                .unwrap()
                .members
                .insert(sub.to_string(), MemberAuthority::live(sub));
        }
    }

    let mut board_port = 0u16;
    for port in 3110..=3199u16 {
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
        let (state, pm) = (state.clone(), pm.clone());
        let thread = std::thread::spawn(move || drop(crate::ui::serve(&state, &pm, &board)));
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
    assert!(board_port > 0, "board could not bind in 3110..=3199");

    (
        Rig {
            _dir: dir,
            pm,
            board_port,
            public_host,
            issuer,
            key,
            authority,
            stop,
            _threads: threads,
        },
        bin_dir,
        prompt_rx,
    )
}

#[test]
fn cad2834_member_memory_edit_preserves_mid_session_delta_across_turns() {
    let _flag = AssistantFlagOn::arm();
    let (rig, _bin_dir, prompt_rx) = start_with_fake_pi(&["alice"]);
    enroll_queue_service(&rig);
    let _keys = initialize_queue_keyring(&rig);

    let (alice_cookie, _) = open_member_session(&rig, "alice");
    let (_, _) = install_board_authorize(&rig, "alice");

    let (status, state_body, _) = member_post(
        &rig,
        "/api/member-assistant/state",
        Some(&alice_cookie),
        &json!({}),
    );
    assert_eq!(status, 200, "alice state failed: {state_body}");
    let alias = state_body["assistant"]["alias"]
        .as_str()
        .unwrap()
        .to_string();

    let wait_idle = || {
        crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
            crate::client::rpc(
                rig._dir.path(),
                "agent_wait",
                json!({"alias": alias, "until": "idle", "timeout": 20}),
            )
        })
        .expect("agent_wait idle failed");
    };

    // Turn 1 delivers initial Layer-1 context.
    let (status, send1, _) = member_post(
        &rig,
        "/api/member-assistant/send",
        Some(&alice_cookie),
        &json!({"text": "CAD2834_TURN_1_PROMPT"}),
    );
    assert_eq!(status, 200, "turn 1 send failed: {send1}");
    let t1_prompt = prompt_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("turn 1 prompt never delivered");
    assert!(
        t1_prompt.contains("CAD2834_TURN_1_PROMPT") && t1_prompt.contains("## Available skills"),
        "turn 1 missing Layer-1 context: {t1_prompt}"
    );
    wait_idle();

    // Turn 2 with no memory changes receives no redundant context.
    let (status, send2, _) = member_post(
        &rig,
        "/api/member-assistant/send",
        Some(&alice_cookie),
        &json!({"text": "CAD2834_TURN_2_PROMPT"}),
    );
    assert_eq!(status, 200, "turn 2 send failed: {send2}");
    let t2_prompt = prompt_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("turn 2 prompt never delivered");
    assert!(
        t2_prompt.contains("CAD2834_TURN_2_PROMPT")
            && !t2_prompt.contains("## Available skills")
            && !t2_prompt.contains("MEMORY.md"),
        "turn 2 unexpectedly re-delivered unchanged context: {t2_prompt}"
    );
    wait_idle();

    // Direct member memory-edit leaves MemberContextTracker.delivered behind.
    let (status, edit_body, _) = member_post(
        &rig,
        "/api/member-assistant/memory-edit",
        Some(&alice_cookie),
        &json!({
            "target": "MEMORY.md",
            "content": "# Memory\n- CAD2834_DIRECT_EDIT_DELTA_SENTINEL\n",
        }),
    );
    assert_eq!(status, 200, "direct memory-edit failed: {edit_body}");
    assert_eq!(edit_body["applies"], "next_turn");

    // Turn 3 receives the compact mid-session delta for the direct edit.
    let (status, send3, _) = member_post(
        &rig,
        "/api/member-assistant/send",
        Some(&alice_cookie),
        &json!({"text": "CAD2834_TURN_3_PROMPT"}),
    );
    assert_eq!(status, 200, "turn 3 send failed: {send3}");
    let t3_prompt = prompt_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("turn 3 prompt never delivered");
    assert!(
        t3_prompt.contains("CAD2834_TURN_3_PROMPT")
            && t3_prompt.contains("CAD2834_DIRECT_EDIT_DELTA_SENTINEL")
            && !t3_prompt.contains("## Available skills"),
        "turn 3 missed direct memory-edit delta (tracker was prematurely silenced): {t3_prompt}"
    );
    wait_idle();

    // Member history reflects the turns without fabricating a context_receipt for direct HTTP edit.
    let (status, hist_body, _) = member_post(
        &rig,
        "/api/member-assistant/history",
        Some(&alice_cookie),
        &json!({}),
    );
    assert_eq!(status, 200, "history read failed: {hist_body}");
    let entries = hist_body["entries"].as_array().unwrap();
    assert!(
        entries.iter().any(|e| e["text"] == "CAD2834_TURN_3_PROMPT"),
        "member history missing turn 3 message: {hist_body}"
    );
}
