//! CAD-2215: independent bad-case acceptance for HTTP monitor control.
//!
//! The signed board's `/api/monitors/<monitor>/alerts/<seq>/ack` route is the
//! operator's numeric acknowledgement. This check drives the REAL board
//! (`/api/overview`, `/api/monitors/...`) in front of the REAL daemon and a real
//! platform-authority login, and proves, by observable effect rather than by
//! matching copy:
//!
//! 1. a named member — a live signed member session — cannot read a hidden
//!    monitor's alert, and cannot acknowledge it (or even a readable alert) by
//!    numeric position: the refusal happens at the board's operator-only gate
//!    (403 `member_role`, never a 404 route-shape answer) and the stored alert
//!    is still `open`;
//! 2. a proven operator's numeric acknowledgement still succeeds and persists;
//! 3. at the daemon's own boundary, a read-relay proof (`public_session`,
//!    `read_as`, `cli_envelope`) cannot authorize the acknowledgement even when
//!    the connection peer is a proven operator — the refusal names the relayed
//!    read identity, so it is not a malformed-token answer — and the alert
//!    stays open;
//! 4. the native paths still work: a native authorized agent's opaque ack token
//!    acknowledges its readable alert, and a native operator's numeric ack
//!    persists.
//!
//! The fixture is the shared CAD-2215 signed-board one (`seed`), so the hidden
//! monitor covers a task in the private project. Nothing here establishes OS
//! isolation; it is HTTP/daemon authorization over real fixtures.

use super::*;
use crate::test_seam::{scoped, Asserted};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const VISIBLE_MONITOR: &str = "cad2215-visible-monitor";
const HIDDEN_MONITOR: &str = "cad2215-secret-monitor";
const VISIBLE_ALERT_SEQ: i64 = 221501;
const HIDDEN_ALERT_SEQ: i64 = 221502;
const WORKER: &str = "cad2215-worker";

fn alert_state(rig: &Rig, seq: i64) -> String {
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    db.query_row(
        "SELECT state FROM monitor_alerts WHERE seq=?1",
        [seq],
        |row| row.get(0),
    )
    .unwrap()
}

/// One guarded board write, carrying exactly the browser headers the shared
/// write guard requires (Content-Type, Origin, Sec-Fetch-Site, X-Cadence-Board)
/// plus the caller's session cookie. The body is empty: the route takes none.
fn ack(rig: &Rig, cookie: &str, monitor: &str, seq: i64) -> (u16, Value) {
    let url = format!(
        "http://127.0.0.1:{}/api/monitors/{monitor}/alerts/{seq}/ack",
        rig.board_port
    );
    let mut response = http_agent()
        .post(&url)
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("X-Cadence-Board", "1")
        .header("Content-Type", "application/json")
        .header("Cookie", cookie)
        .send_empty()
        .expect("real board monitor ack failed");
    let status = response.status().as_u16();
    let body = response.body_mut().read_json().unwrap_or(Value::Null);
    (status, body)
}

fn monitoring(overview: &Value) -> &Value {
    overview
        .get("monitoring")
        .filter(|value| value["available"] == json!(true))
        .unwrap_or_else(|| panic!("monitoring is unavailable: {overview}"))
}

fn native(rig: &Rig, who: Asserted, method: &str, params: &Value) -> crate::Result<Value> {
    let state = rig._dir.path().to_path_buf();
    scoped(who, || crate::client::rpc(&state, method, params.clone()))
}

/// The daemon's own keyring over fixture-owned key files: the only setup
/// operation is the real operator `scoped_cursor_initialize` RPC. Key files
/// must live outside the state dir and the tracker, so they get their own
/// private directory; the returned guard keeps it alive for the test.
fn initialize_keyring(rig: &Rig) -> tempfile::TempDir {
    let state = rig._dir.path();
    let keys = tempfile::tempdir().unwrap();
    std::fs::set_permissions(keys.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let write = |name: &str, byte: u8| {
        use std::io::Write as _;
        let path = keys.path().join(name);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(&[byte; 32]).unwrap();
        drop(file);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        path
    };
    let identity = write("identity.key", 0x41);
    let cursor = write("cursor.key", 0x52);
    let id = "0c".repeat(16);
    let config = json!({
        "version": 1,
        "identity_key_file": identity.to_str().unwrap(),
        "active_key_id": id.clone(),
        "keys": [{ "id": id, "key_file": cursor.to_str().unwrap() }],
    });
    let config_path = state.join("scoped-cursors.json");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let response = native(
        rig,
        Asserted::Operator,
        "scoped_cursor_initialize",
        &json!({}),
    )
    .expect("the operator must initialize the keyring");
    assert_eq!(response["initialized"], json!(true), "{response}");
    keys
}

fn envelope(rig: &Rig, sub: &str) -> String {
    let now = crate::issue::time::now_epoch();
    let header = json!({"alg": "EdDSA", "typ": "agenticos-wiki-actor/1", "kid": KID});
    let claims = json!({
        "iss": rig.issuer,
        "aud": format!("http://{}", rig.public_host),
        "sub": sub,
        "actor": format!("users/{sub}"),
        "principal_kind": "user",
        "role_at_issue": "member",
        "organization_id": ORG,
        "scope": ["cli.read"],
        "credential_id": format!("cad2215-monitor-{sub}"),
        "iat": now,
        "exp": now + 120,
        "jti": format!("cad2215-monitor-{}", uuid::Uuid::new_v4()),
    });
    let signed = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap()),
    );
    format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(rig.key.sign(signed.as_bytes()).as_ref())
    )
}

#[test]
fn named_member_cannot_acknowledge_a_hidden_or_visible_alert_numerically() {
    let rig = start(&[]);
    let _keys = initialize_queue_keyring(&rig);
    seed(&rig);
    let member = login(&rig, "cad2215-monitor-member", "member");
    let owner = login(&rig, "cad2215-monitor-owner", "owner");

    // Read boundary: the member cannot see the private project's monitor or
    // its alert while the operator can. Both reads are the real HTTP routes.
    let member_overview = get(&rig, &member, "/api/overview");
    let owner_overview = get(&rig, &owner, "/api/overview");
    let member_view = monitoring(&member_overview).to_string();
    let owner_view = monitoring(&owner_overview).to_string();
    assert!(
        member_view.contains(VISIBLE_MONITOR),
        "member lost the readable monitor"
    );
    assert!(
        member_view.contains("cad2215-visible-payload"),
        "member lost the readable alert"
    );
    for hidden in [
        HIDDEN_MONITOR,
        "cad2215-secret-payload",
        "cad2215-secret-task",
    ] {
        assert!(
            !member_view.contains(hidden),
            "member monitoring leaked {hidden}"
        );
        assert!(owner_view.contains(hidden), "operator lost {hidden}");
    }

    // A named member's numeric acknowledgement of the hidden alert is refused
    // by the board's operator-only gate — not by the route-shape 404 — and the
    // stored alert is untouched.
    let (status, body) = ack(&rig, &member, HIDDEN_MONITOR, HIDDEN_ALERT_SEQ);
    assert_eq!(
        status, 403,
        "member numeric ack of a hidden alert was not refused: {body}"
    );
    assert_eq!(
        body["check"],
        json!("member_role"),
        "refusal was not the member-role gate: {body}"
    );
    assert_eq!(
        alert_state(&rig, HIDDEN_ALERT_SEQ),
        "open",
        "the member's refused ack changed the hidden alert"
    );

    // Even a readable alert is the operator's decision; the gate is the same,
    // so a member cannot acknowledge anything by numeric position.
    let (status, body) = ack(&rig, &member, VISIBLE_MONITOR, VISIBLE_ALERT_SEQ);
    assert_eq!(
        status, 403,
        "member numeric ack of a readable alert was not refused: {body}"
    );
    assert_eq!(body["check"], json!("member_role"), "{body}");
    assert_eq!(alert_state(&rig, VISIBLE_ALERT_SEQ), "open");

    // The operator's numeric control still succeeds through the same route.
    let (status, body) = ack(&rig, &owner, HIDDEN_MONITOR, HIDDEN_ALERT_SEQ);
    assert_eq!(status, 200, "operator numeric ack failed: {body}");
    assert_eq!(body["ok"], json!(true), "{body}");
    assert_eq!(
        alert_state(&rig, HIDDEN_ALERT_SEQ),
        "acknowledged",
        "operator numeric ack did not persist"
    );
}

#[test]
fn relayed_read_proofs_cannot_acknowledge_and_native_paths_still_do() {
    let rig = start(&[]);
    seed(&rig);
    let member = login(&rig, "cad2215-relay-member", "member");
    let owner = login(&rig, "cad2215-relay-owner", "owner");
    let member_token = member
        .split_once('=')
        .expect("member session cookie")
        .1
        .to_string();
    let owner_token = owner
        .split_once('=')
        .expect("owner session cookie")
        .1
        .to_string();
    let _keys = initialize_keyring(&rig);
    let cli = envelope(&rig, "cad2215-relay-member");

    // A read-relay proof is not write authority, even when the peer is a
    // proven operator (the `scoped` assertion) and even for an operator-role
    // session. Each refusal names the relayed identity, so it cannot be a
    // malformed-token answer in disguise, and the alert stays open.
    for (label, proof) in [
        ("public_session", json!(member_token)),
        ("public_session (operator role)", json!(owner_token)),
        ("read_as", json!("users/cad2215-relay-member")),
        ("cli_envelope", json!(cli)),
    ] {
        let mut params = json!({
            "monitor": HIDDEN_MONITOR,
            "alert": HIDDEN_ALERT_SEQ,
            "by": "operator",
        });
        params[proof_key(label)] = proof;
        let error = native(&rig, Asserted::Operator, "monitor_alert_ack", &params)
            .expect_err(&format!("{label} authorized a monitor acknowledgement"));
        let text = format!("{error:?}");
        assert!(
            text.contains("relayed read identity cannot authorize a write"),
            "{label} was refused for the wrong reason: {text}"
        );
        assert_eq!(
            alert_state(&rig, HIDDEN_ALERT_SEQ),
            "open",
            "{label} changed the hidden alert"
        );
    }

    // Native authorized agent: an opaque ack token for its readable monitor
    // still acknowledges.
    let page = native(
        &rig,
        Asserted::Agent(WORKER.to_string()),
        "monitor_alerts",
        &json!({"monitor": VISIBLE_MONITOR, "cursor_format": "opaque-v1"}),
    )
    .expect("the native worker must read its readable monitor");
    let token = page["alerts"]
        .as_array()
        .unwrap_or_else(|| panic!("scoped monitor page has no alerts: {page}"))
        .iter()
        .find(|alert| alert["payload"]["marker"] == json!("cad2215-visible-payload"))
        .unwrap_or_else(|| panic!("the readable alert is missing: {page}"))["ack"]
        .as_str()
        .filter(|token| !token.is_empty())
        .unwrap_or_else(|| panic!("the readable alert has no ack token: {page}"))
        .to_string();
    let acknowledged = native(
        &rig,
        Asserted::Agent(WORKER.to_string()),
        "monitor_alert_ack",
        &json!({"monitor": VISIBLE_MONITOR, "ack": token, "by": WORKER}),
    )
    .expect("the native worker's own ack token must acknowledge its alert");
    assert_eq!(acknowledged["alert"]["task"], json!("cad2215-visible-task"));
    assert_eq!(alert_state(&rig, VISIBLE_ALERT_SEQ), "acknowledged");

    // Native operator numeric control, without any relay proof, persists.
    let acknowledged = native(
        &rig,
        Asserted::Operator,
        "monitor_alert_ack",
        &json!({"monitor": HIDDEN_MONITOR, "alert": HIDDEN_ALERT_SEQ, "by": "operator"}),
    )
    .expect("the native operator's numeric ack must succeed");
    assert_eq!(acknowledged["alert"]["seq"], json!(HIDDEN_ALERT_SEQ));
    assert_eq!(alert_state(&rig, HIDDEN_ALERT_SEQ), "acknowledged");
}

/// The wire key each proof travels under. Kept separate from the label so the
/// refusal list reads as behaviour, not as a spelling trick.
fn proof_key(label: &str) -> &'static str {
    match label {
        "public_session" | "public_session (operator role)" => "public_session",
        "read_as" => "read_as",
        "cli_envelope" => "cli_envelope",
        other => panic!("unknown proof label {other}"),
    }
}
