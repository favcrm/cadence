//! CAD688 actual peer authority and credential incarnation lifecycle.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::contract_fixture::FakePlatform;
use common::{daemon_opts, plant_member_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

fn fixture() -> TestDaemon {
    let mut opts = daemon_opts();
    opts.platforms
        .insert("fixture".into(), Arc::new(FakePlatform::standard()));
    TestDaemon::start_opts(opts)
}
fn create(account: &str, token: &str) -> Value {
    json!({"provider":"fixture","account":account,"shape":"token","token":token,"scopes":["widgets:read"],"accept_same_uid_risk":true})
}
fn native(
    lane: &mut LaneShell,
    state: &Path,
    detached: bool,
    method: &str,
    params: Value,
) -> Value {
    // Build literal two-field JSON, avoiding proto::request's optional test seam.
    let frame = json!({"method":method,"params":params});
    assert_eq!(frame.as_object().unwrap().len(), 2);
    assert!(frame.get(cadence_agent::test_seam::FRAME_FIELD).is_none());
    let wire = frame.to_string();
    assert!(!wire.contains(cadence_agent::test_seam::AS_HEADER));
    assert!(!wire.contains(cadence_agent::test_seam::TOKEN_HEADER));
    let request = lane.dir.path().join(format!("native-{}.json", lane.seq));
    std::fs::write(&request, wire).unwrap();
    let prefix = if detached { "setsid " } else { "" };
    let (rc, text) = lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.socket(socket.AF_UNIX);s.settimeout(10);s.connect(sys.argv[1]);s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");print(s.makefile().readline())' {} {}", state.join("cadence.sock").display(), request.display()));
    assert_eq!(rc, 0, "native socket process failed: {text}");
    serde_json::from_str(text.trim()).unwrap()
}

#[test]
fn cad688_native_and_setsid_connection_management_is_operator_only() {
    let d = fixture();
    let created = d
        .operator_rpc(
            "connection_create",
            create("native-account", "cadp_conn_secret_12345"),
        )
        .unwrap();
    let id = created["connection"]["id"].as_str().unwrap();
    assert_eq!(
        d.operator_rpc("connection_show", json!({"connection_id":id}))
            .unwrap(),
        created
    );
    let mut lane = LaneShell::spawn();
    plant_member_pane(&d, "connection-peer", "claude", None, lane.pid());
    let before = d.operator_rpc("connection_list", json!({})).unwrap();
    let calls = [
        ("connection_providers", json!({})),
        ("connection_list", json!({})),
        ("connection_show", json!({"connection_id":id})),
        ("connection_check", json!({"connection_id":id})),
        (
            "connection_create",
            create("forged-account", "cadp_conn_other_secret"),
        ),
        (
            "connection_rotate",
            json!({"connection_id":id,"token":"cadp_conn_rotate_secret"}),
        ),
        ("connection_revoke", json!({"connection_id":id})),
    ];
    let mut failures = Vec::new();
    for detached in [false, true] {
        for (method, params) in &calls {
            let frame = native(&mut lane, &d.state, detached, method, params.clone());
            if frame["ok"] != false
                || frame.get("result").is_some()
                || !frame["error"]["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains("operator action")
            {
                failures.push(json!({"detached":detached,"method":method,"frame":frame}));
            }
        }
        let frame = native(
            &mut lane,
            &d.state,
            detached,
            "connection_revoke",
            json!({"connection_id":id,"agent":"operator","provider":"fixture","account":"native-account"}),
        );
        assert_eq!(frame["ok"], false);
    }
    assert!(
        failures.is_empty(),
        "actual peer connection admission failed: {failures:?}"
    );
    assert_eq!(
        d.operator_rpc("connection_list", json!({})).unwrap(),
        before
    );
}
#[test]
fn cad688_rotation_preserves_id_and_stale_id_cannot_address_reenrollment() {
    let d = fixture();
    let first = d
        .operator_rpc(
            "connection_create",
            create("lifecycle", "cadp_conn_first_secret"),
        )
        .unwrap();
    let id = first["connection"]["id"].clone();
    let rotated = d
        .operator_rpc(
            "connection_rotate",
            json!({"connection_id":id,"token":"cadp_conn_second_secret"}),
        )
        .unwrap();
    assert_eq!(rotated["connection"]["id"], id);
    assert_eq!(rotated["connection"]["revision"], 2);
    d.operator_rpc("connection_revoke", json!({"connection_id":id}))
        .unwrap();
    let next = d
        .operator_rpc(
            "connection_create",
            create("lifecycle", "cadp_conn_third_secret"),
        )
        .unwrap();
    assert_ne!(next["connection"]["id"], id);
    for method in [
        "connection_show",
        "connection_check",
        "connection_rotate",
        "connection_revoke",
    ] {
        let mut params = json!({"connection_id":id});
        if method == "connection_rotate" {
            params["token"] = json!("cadp_stale_secret");
        }
        assert!(
            d.operator_rpc(method, params).is_err(),
            "stale ID admitted {method}"
        );
    }
    assert_eq!(
        d.operator_rpc(
            "connection_show",
            json!({"connection_id":next["connection"]["id"]})
        )
        .unwrap(),
        next
    );
    let db = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM platform_grants", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
