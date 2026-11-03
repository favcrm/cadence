use super::*;
use crate::contract_fixture::ToolTable;

const MANIFEST: &str = "cadence-provider-tools@1";
const TOOL: &str = "search_console_report";
const CONNECTION: &str = "con_550e8400-e29b-41d4-a716-446655440000";

fn report_table(effect: &str, connected_read: bool) -> ToolTable {
    ToolTable::from_json(&json!({
        "platform": "agenticos-provider",
        "manifest_version": MANIFEST,
        "tools": [{
            "tool": TOOL,
            "effect": effect,
            "scopes": ["provider.read"],
            "connected_read": connected_read,
            "label": "Report"
        }]
    }))
    .expect("provider tool table parses")
}

#[test]
fn connected_read_requires_connection() {
    let table = report_table("read", true);
    let error = connected_read_admission(
        &table,
        Some(MANIFEST),
        TOOL,
        &json!({"max_charge_minor": 275}),
    )
    .err()
    .expect("a connected read without a connection must be an error, not Ok(None)");
    let message = error.to_string();
    assert!(message.contains("connection"), "{message}");
}

#[test]
fn connected_read_requires_max_charge_minor() {
    let table = report_table("read", true);
    let error = connected_read_admission(
        &table,
        Some(MANIFEST),
        TOOL,
        &json!({"connection": CONNECTION}),
    )
    .err()
    .expect("a connection alone must never admit a connected read");
    let message = error.to_string();
    assert!(message.contains("max_charge_minor"), "{message}");
}

#[test]
fn connected_read_refuses_draft_effect() {
    let table = report_table("draft", true);
    assert_eq!(classify_call(&table, Some(MANIFEST), TOOL), Effect::Draft);
    let error = connected_read_admission(
        &table,
        Some(MANIFEST),
        TOOL,
        &json!({"connection": CONNECTION, "max_charge_minor": 275}),
    )
    .err()
    .expect("a connected read must refuse a draft classification");
    let message = error.to_string();
    assert!(message.contains("read effect"), "{message}");
}

#[test]
fn connected_read_refuses_manifest_mismatch() {
    let table = report_table("read", true);
    let reported = Some("cadence-provider-tools@2");
    assert_eq!(classify_call(&table, reported, TOOL), Effect::Send);
    let error = connected_read_admission(
        &table,
        reported,
        TOOL,
        &json!({"connection": CONNECTION, "max_charge_minor": 275}),
    )
    .err()
    .expect("a manifest mismatch must refuse, not fall through as an ordinary call");
    let message = error.to_string();
    assert!(message.contains("read effect"), "{message}");
}

#[test]
fn ordinary_tool_refuses_connection() {
    let table = report_table("read", false);
    let error = connected_read_admission(
        &table,
        Some(MANIFEST),
        TOOL,
        &json!({"connection": CONNECTION}),
    )
    .err()
    .expect("an ordinary tool must not silently discard a connection");
    let message = error.to_string();
    assert!(message.contains("connection"), "{message}");
    assert!(message.contains("connected read"), "{message}");
}

#[test]
fn ordinary_tool_refuses_max_charge_minor() {
    let table = report_table("read", false);
    let error = connected_read_admission(
        &table,
        Some(MANIFEST),
        TOOL,
        &json!({"max_charge_minor": 275}),
    )
    .err()
    .expect("an ordinary tool must not silently discard a charge without a connection");
    let message = error.to_string();
    assert!(message.contains("max_charge_minor"), "{message}");
    assert!(message.contains("connected read"), "{message}");
}

#[test]
fn ordinary_tool_without_connected_fields_is_unattended() {
    let table = report_table("read", false);
    let admitted = connected_read_admission(&table, Some(MANIFEST), TOOL, &json!({}))
        .expect("an ordinary tool without connected-read fields remains admissible");
    assert!(admitted.is_none(), "an ordinary tool must return Ok(None)");
}

#[test]
fn connected_read_preserves_connection_and_accepted_charge() {
    let table = report_table("read", true);
    for charge in [0_u64, 275] {
        let admitted = connected_read_admission(
            &table,
            Some(MANIFEST),
            TOOL,
            &json!({"connection": CONNECTION, "max_charge_minor": charge}),
        )
        .expect("a fully specified connected read is admissible")
        .expect("a connected read must not fall through to the ordinary path");
        assert_eq!(admitted.connection, CONNECTION);
        assert_eq!(admitted.max_charge_minor, charge);
    }
}

#[test]
fn provider_connection_id_preserves_real_handle() {
    assert_eq!(
        provider_connection_id(CONNECTION).expect("provider con_<uuid> handles are valid"),
        CONNECTION
    );
}

#[test]
fn provider_connection_id_refuses_empty() {
    let error = provider_connection_id("").expect_err("empty connection IDs must refuse");
    let message = error.to_string();
    assert!(message.contains("connection"), "{message}");
}

#[test]
fn provider_connection_id_refuses_overlong() {
    let overlong = "a".repeat(129);
    let error = provider_connection_id(&overlong)
        .expect_err("connection IDs longer than 128 characters must refuse");
    let message = error.to_string();
    assert!(message.contains("connection"), "{message}");
    assert!(message.contains("128"), "{message}");
}

#[test]
fn provider_connection_id_refuses_embedded_spaces() {
    let error = provider_connection_id("con_550e8400 e29b-41d4-a716-446655440000")
        .expect_err("space-bearing connection IDs must refuse");
    let message = error.to_string();
    assert!(message.contains("connection"), "{message}");
}
