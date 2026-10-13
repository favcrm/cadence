// CAD-1405 native + HTTP lifecycle refusal acceptance.
//
// PARTIAL authorship, not a frozen or completed acceptance check. It is
// registered only in a scratch snapshot, as a child module of
// `hosted_source_board`, through:
//   mod cad1405_app_lifecycle_acceptance {
//       include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/daemon/cad1405_app_lifecycle_acceptance.rs"));
//   }
// Plain `//` comments only: inner `//!` docs are rejected inside include!d modules.
//
// Scenario (one test):
// 1. Real setup as cad1294: install and bind Social Content, save the allowed
//    handle `juicysuite_crm`, mint and consume a feed mount (generation 1).
//    A harmless `instagram.read` invoke must reach the provider door (positive control).
// 2. Refusals before removal, each with unchanged tracker bytes and provider count:
//    native RPC agent and unproven removal; HTTP agent and unproven removal
//    with a valid owned board cookie/session; HTTP operator removal with a stale pin.
// 3. Positive same-route control: operator HTTP POST /api/app-installations/<id>/remove
//    with current preview pins. The old mount token must then fail, with no provider call.
// 4. Native operator restore. The old token must still fail after same-digest
//    re-consent. A fresh generation-2 mint and consume must succeed for the same handle.
//
// Gaps (none verified by a run unless the parent records one):
// - The HTTP refusal status is asserted as 401 or 403 for guard refusals. The
//   real status and body are not known until a run.
// - Retained-state checks use the tracker byte tree only. No store-level
//   queued-publish cancellation assertion.
// - The HTTP error body is read with `read_to_string`, the ureq 3 API assumed here.
// - `CALLS` is process-global; run filtered with --test-threads 2.
// - Not compiled or executed by the author. No authority check has passed.

use super::*;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// Every file under the tracker (outside `.git`) with its bytes.
fn tree(pm: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            if rel == ".git" {
                continue;
            }
            if p.is_dir() {
                out.insert(format!("{rel}/"), vec![]);
                walk(base, &p, out);
            } else {
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(pm, pm, &mut out);
    out
}

/// One RPC under an asserted caller. Refusals come back as `Err(text)`.
fn as_caller(
    state: &Path,
    who: crate::test_seam::Asserted,
    method: &str,
    params: serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    crate::test_seam::scoped(who, || crate::client::rpc(state, method, params))
        .map_err(|e| e.to_string())
}

/// Mint then consume one feed mount. Returns the mint reply.
fn mint_and_consume(
    state: &Path,
    install: &str,
    context: &str,
    token: &serde_json::Value,
    key: &serde_json::Value,
    generation: u64,
) -> serde_json::Value {
    let mint = operator(
        state,
        "app_screen_mint",
        serde_json::json!({"install_id": install, "context_id": context, "tag": "feed",
            "token": token, "key": key, "origin": "loopback", "generation": generation}),
    );
    let nonce = mint["mount"].as_str().unwrap().rsplit('/').next().unwrap();
    operator(state, "app_screen_consume", serde_json::json!({"nonce": nonce}));
    mint
}

/// The same `instagram.read` call the cad1294 fixture makes, through the mount token.
fn invoke(
    state: &Path,
    action_token: &serde_json::Value,
    token: &serde_json::Value,
    key: &serde_json::Value,
    request: &str,
) -> std::result::Result<serde_json::Value, String> {
    as_caller(
        state,
        crate::test_seam::Asserted::Operator,
        "app_tool_invoke",
        serde_json::json!({"action_token": action_token, "tool_alias": "instagram.read",
            "input": {"handle": "juicysuite_crm"}, "request_id": request,
            "token": token, "key": key, "origin": "loopback"}),
    )
}

/// The owned board: its base URL, Host, board token and the real session.
struct Board {
    base: String,
    host: String,
    token: String,
    cookie: String,
    key: String,
}

/// Start an owned board on 3110-3199, as `board_quote` does. Joins failed bind attempts.
fn boot_board(stop: &mut Stop, state: &Path, pm: &Path) -> u16 {
    let mut port = 3110 + (std::process::id() % 80) as u16;
    loop {
        let (startup, ready) = std::sync::mpsc::channel();
        let opts = crate::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.0.clone()),
            startup: Some(startup),
            test_seam: true,
            ..Default::default()
        };
        let (s, p) = (state.to_path_buf(), pm.to_path_buf());
        let thread = std::thread::spawn(move || drop(crate::ui::serve(&s, &p, &opts)));
        match ready.recv_timeout(Duration::from_secs(20)).unwrap() {
            Ok(()) => {
                stop.1.push(thread);
                return port;
            }
            Err(kind) => {
                thread.join().unwrap();
                assert!(port < 3199, "board could not bind: {kind:?}");
                port += 1;
            }
        }
    }
}

/// The real login-link exchange against the owned board, as `board_quote` does.
fn board_session(state: &Path, port: u16) -> Board {
    let token = crate::test_seam::Seam::token_at(state).unwrap();
    let host = format!("cadence-{port}.localhost:{port}");
    crate::operator_auth::ensure_secret(state).unwrap();
    let secret = crate::operator_auth::read_secret(state).unwrap();
    let nonce = operator(
        state,
        "operator_link_mint",
        serde_json::json!({"secret": secret, "origin": "loopback"}),
    )["nonce"]
        .clone();
    let config = ureq::Agent::config_builder().http_status_as_error(false);
    let agent: ureq::Agent = config.build().into();
    let base = format!("http://127.0.0.1:{port}");
    let session = agent
        .post(format!("{base}/api/session"))
        .header("Host", &host)
        .header("X-Cadence-Board", "1")
        .header("Origin", format!("http://{host}"))
        .header(crate::test_seam::AS_HEADER, "operator")
        .header(crate::test_seam::TOKEN_HEADER, &token)
        .header("Content-Type", "application/json")
        .send(serde_json::json!({"nonce": nonce}).to_string())
        .unwrap();
    let set = session.headers()["set-cookie"].to_str().unwrap();
    let cookie = set[..set.find(';').unwrap()].to_owned();
    let key: serde_json::Value = session.into_body().read_json().unwrap();
    Board {
        base,
        host,
        token,
        cookie,
        key: key["session_key"].as_str().unwrap().to_owned(),
    }
}

/// HTTP POST remove. `who` is the asserted identity header; the owned cookie
/// and session key are always attached, so only the caller guard can refuse.
fn http_remove(board: &Board, who: &str, install: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
    let config = ureq::Agent::config_builder().http_status_as_error(false);
    let agent: ureq::Agent = config.build().into();
    let mut response = agent
        .post(format!("{}/api/app-installations/{install}/remove", board.base))
        .header("Host", &board.host)
        .header("X-Cadence-Board", "1")
        .header("Origin", format!("http://{}", board.host))
        .header(crate::test_seam::AS_HEADER, who)
        .header(crate::test_seam::TOKEN_HEADER, &board.token)
        .header("Cookie", &board.cookie)
        .header("X-Cadence-Session", &board.key)
        .header("Content-Type", "application/json")
        .send(body.to_string())
        .unwrap();
    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().unwrap();
    (status, serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text)))
}

#[test]
fn cad1405_native_remove_restore_retires_mount_authority() {
    let (mut stop, root, state) = boot_daemon(recording_door);
    let pm = root.path().join("pm");
    let fixture = root.path().join("app");
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    copy_dir(
        &manifest_dir.join("tests/fixtures/apps/social-content"),
        &fixture,
    );
    copy_dir(
        &manifest_dir.join("tests/fixtures/apps/ig-tools-fixture/screens"),
        &fixture.join("screens"),
    );
    let screens = fixture.join("screens/feed/screens.json");
    let text = std::fs::read_to_string(&screens).unwrap();
    std::fs::write(&screens, text.replace("ig-tools-fixture", "social-content")).unwrap();

    let install = operator(
        &state,
        "app_workspace_install",
        serde_json::json!({"source": fixture.to_str().unwrap()}),
    );
    let install_id = install["install_id"].as_str().unwrap().to_string();
    operator(
        &state,
        "app_local_install_approve",
        serde_json::json!({"install_id": install_id, "digest": install["digest"]}),
    );
    let rows = operator(&state, "connection_list", serde_json::json!({}))["connections"].clone();
    let hosted = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["account"] == "hosted")
        .unwrap()["id"]
        .clone();
    let context = operator(
        &state,
        "app_context_create",
        serde_json::json!({"install_id": install_id, "label": "EF", "input_defaults": {}, "request_id": "ctx-1405"}),
    );
    let context = context["context"]["id"]
        .as_str()
        .or_else(|| context["id"].as_str())
        .unwrap()
        .to_string();
    operator(
        &state,
        "app_binding_create",
        serde_json::json!({"install_id": install_id, "context_id": context, "slot": "source",
            "connection_id": hosted, "request_id": "bind-1405"}),
    );
    crate::store::app_records::RecordStore::open(&state, &install_id)
        .unwrap()
        .app_social_sources_save(&context, 0, &["juicysuite_crm".to_owned()], "src-1405")
        .unwrap();

    crate::operator_auth::ensure_secret(&state).unwrap();
    let secret = crate::operator_auth::read_secret(&state).unwrap();
    let nonce = operator(
        &state,
        "operator_link_mint",
        serde_json::json!({"secret": secret, "origin": "loopback"}),
    )["nonce"]
        .clone();
    let session = operator(
        &state,
        "operator_session_open",
        serde_json::json!({"nonce": nonce, "origin": "loopback"}),
    );
    let (token, key) = (session["token"].clone(), session["key"].clone());

    CALLS.lock().unwrap().clear();
    let port = boot_board(&mut stop, &state, &pm);
    let stolen_board = board_session(&state, port);

    // Step 1: positive control. A valid mount reaches the provider door.
    let mint1 = mint_and_consume(&state, &install_id, &context, &token, &key, 1);
    let receipt = invoke(&state, &mint1["action_token"], &token, &key, "fetch-1405-a")
        .unwrap_or_else(|e| panic!("positive control invoke refused: {e}"));
    assert!(receipt.get("receipt").is_some(), "{receipt}");
    {
        let calls = CALLS.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "positive control did not reach the door: {calls:?}");
        assert_eq!(calls[0]["query"]["handle"], "juicysuite_crm");
    }

    // Step 2: refusals before removal. Each must leave the tracker and provider count unchanged.
    let preview = operator(
        &state,
        "app_workspace_remove_preview",
        serde_json::json!({"install_id": install_id}),
    );
    let pins = serde_json::json!({"expected_generation": preview["generation"],
        "expected_digest": preview["digest"]});
    let before_refused = tree(&pm);
    for who in [
        crate::test_seam::Asserted::Agent("writer".into()),
        crate::test_seam::Asserted::Unproven,
    ] {
        let label = who.as_str();
        let mut rpc = pins.clone();
        rpc["install_id"] = serde_json::json!(install_id);
        rpc["request_id"] = serde_json::json!(format!("remove-1405-rpc-{label}"));
        assert!(
            as_caller(&state, who, "app_workspace_remove", rpc).is_err(),
            "native {label} removal was accepted"
        );
    }
    // A real agent request on the operator session is refused as a stolen session.
    // This invalidates stolen_board only; the native RPC tokens above stay live.
    let mut body = pins.clone();
    body["request_id"] = serde_json::json!("remove-1405-http-agent");
    let (status, reply) = http_remove(&stolen_board, "agent:writer", &install_id, body);
    assert_eq!(status, 403, "HTTP agent removal not refused: {reply}");
    assert_eq!(reply["check"], "session_from_agent", "{reply}");

    // Fresh operator credential for the remaining HTTP cases on the same route.
    let board = board_session(&state, port);
    let stale = serde_json::json!({"expected_generation": "0".repeat(64),
        "expected_digest": preview["digest"], "request_id": "remove-1405-stale"});
    let (status, reply) = http_remove(&board, "operator", &install_id, stale);
    assert!(
        !(200..300).contains(&status) && status != 404,
        "HTTP stale-pin removal not refused: status {status} {reply}"
    );
    assert!(
        !reply.to_string().contains("session_from_agent")
            && !reply.to_string().contains("operator_session_required"),
        "stale-pin refusal came from a session check, not the generation pin: {reply}"
    );
    assert_eq!(tree(&pm), before_refused, "a refused removal changed the tracker");
    assert_eq!(
        CALLS.lock().unwrap().len(),
        1,
        "a refused removal reached the provider"
    );

    // Step 3: positive same-route control. The operator removes over HTTP with current pins.
    let mut body = pins.clone();
    body["request_id"] = serde_json::json!("remove-1405");
    let (status, reply) = http_remove(&board, "operator", &install_id, body);
    assert_eq!(status, 200, "operator HTTP removal refused: {reply}");
    assert_eq!(reply["state"], "removed", "{reply}");
    assert!(
        invoke(&state, &mint1["action_token"], &token, &key, "fetch-1405-b").is_err(),
        "old mount token authorized a read while the install is removed"
    );
    assert_eq!(
        CALLS.lock().unwrap().len(),
        1,
        "a removed install reached the provider"
    );

    // Step 4: native restore re-consents the same digest. The old token must stay denied.
    let restored = operator(
        &state,
        "app_workspace_restore",
        serde_json::json!({"install_id": install_id}),
    );
    assert_eq!(restored["state"], "live", "{restored}");
    assert!(
        invoke(&state, &mint1["action_token"], &token, &key, "fetch-1405-c").is_err(),
        "old mount token was revived by restore"
    );
    assert_eq!(
        CALLS.lock().unwrap().len(),
        1,
        "the revived old token reached the provider"
    );

    // A fresh generation-2 mount for the same allowed handle succeeds.
    let mint2 = mint_and_consume(&state, &install_id, &context, &token, &key, 2);
    invoke(&state, &mint2["action_token"], &token, &key, "fetch-1405-d")
        .unwrap_or_else(|e| panic!("fresh mount refused after restore: {e}"));
    assert_eq!(CALLS.lock().unwrap().len(), 2, "fresh mount did not reach the door");
}
