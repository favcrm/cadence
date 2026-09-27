//! Provider-owned mapping and app-text contract; no app grant or fake project.
use cadence_agent::platform::{local, PlatformAdapter};
use serde_json::json;

fn adapter() -> (tempfile::TempDir, std::sync::Arc<dyn PlatformAdapter>) {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = cadence_agent::daemon::ServeOptions::default();
    local::register_at(
        dir.path(),
        &mut opts,
        dir.path().join("outbox"),
        "http://127.0.0.1:3188".into(),
    );
    (dir, opts.platforms.remove("local").unwrap())
}

#[test]
fn reviewed_text_mapping_is_provider_owned_exact_and_app_only() {
    let (_dir, adapter) = adapter();
    let descriptor = adapter.connection_descriptor().unwrap();
    descriptor.validate(adapter.table()).unwrap();
    let mapping = descriptor
        .resolve_action("text.publish", 1, "publish", "connection_account")
        .unwrap();
    assert_eq!(mapping.tool, "publish_app_text");
    assert_eq!(mapping.scopes, ["publish"]);
    assert_eq!(mapping.effect, "send");
    assert_eq!(mapping.input_contract, "text.publish.input@1");
    assert!(adapter.app_artifact_tool("publish_app_text"));
    assert!(!adapter.app_artifact_tool("publish"));
    for (capability, version, action, resource) in [
        ("text.publish", 2, "publish", "connection_account"),
        ("text.publish", 1, "read", "connection_account"),
        ("text.publish", 1, "publish", "project"),
        ("invented", 1, "publish", "connection_account"),
    ] {
        assert!(descriptor
            .resolve_action(capability, version, action, resource)
            .is_err());
    }
    for field in ["tool", "scope", "effect", "duplicate"] {
        let mut invalid = descriptor.clone();
        match field {
            "tool" => invalid.action_mappings[0].tool = "invented".into(),
            "scope" => invalid.action_mappings[0].scopes.push("other".into()),
            "effect" => invalid.action_mappings[0].effect = "draft".into(),
            _ => invalid
                .action_mappings
                .push(invalid.action_mappings[0].clone()),
        }
        assert!(invalid.validate(adapter.table()).is_err(), "{field}");
    }
}

#[test]
fn app_text_preparation_preserves_exact_text_and_never_accepts_files() {
    let (_dir, adapter) = adapter();
    let provenance = json!({"artifact_id":"artifact-example", "sink_registration":adapter.connection_registration().unwrap()});
    let input = adapter
        .prepare_app_text("Draft", "first line\nsecond line\n", &provenance)
        .unwrap();
    assert_eq!(
        input,
        json!({"schema":1,"title":"Draft","body":"first line\nsecond line\n","provenance":provenance})
    );
    assert!(!input.as_object().unwrap().contains_key("project"));
    assert!(adapter
        .preview("local", "publish_app_text", &input)
        .contains("first line\nsecond line\n"));
    assert!(adapter
        .prepare_app_text("bad\ntitle", "body", &provenance)
        .is_err());
    assert!(adapter
        .prepare_app_text("title", &"x".repeat(64 * 1024), &provenance)
        .is_err());
    assert!(adapter
        .prepare_app_text("title", "body", &json!(null))
        .is_err());
    let mut forged = input.clone();
    forged["attachments"] = json!(["/etc/passwd"]);
    assert!(adapter
        .execute(&[], "publish_app_text", &forged, "eid-example", None)
        .is_err());
}

#[test]
fn local_app_text_requires_persisted_executing_child_before_any_write() {
    use sha2::Digest;
    let (dir, adapter) = adapter();
    cadence_agent::store::Store::open(&dir.path().join("cadence.sqlite3")).unwrap();
    let digest = format!("sha256:{:x}", sha2::Sha256::digest(b"server artifact"));
    let provenance = json!({"artifact_id":"artifact-example", "artifact_digest":digest, "sink_registration":adapter.connection_registration().unwrap()});
    let input = adapter
        .prepare_app_text("Draft", "server artifact", &provenance)
        .unwrap();
    assert!(adapter
        .execute(
            &[],
            "publish_app_text",
            &input,
            "eid-without-child",
            Some(&digest)
        )
        .is_err());
    assert!(!dir.path().join("outbox").exists());
}

#[test]
fn local_sink_receipt_distinguishes_actual_non_utf8_destination_bytes() {
    use std::os::unix::ffi::OsStringExt;
    let dir = tempfile::tempdir().unwrap();
    let first = dir
        .path()
        .join(std::ffi::OsString::from_vec(vec![b'x', 0xff]));
    let second = dir
        .path()
        .join(std::ffi::OsString::from_vec(vec![b'x', 0xfe]));
    assert_eq!(first.to_string_lossy(), second.to_string_lossy());
    let mut a = cadence_agent::daemon::ServeOptions::default();
    let mut b = cadence_agent::daemon::ServeOptions::default();
    local::register_at(dir.path(), &mut a, first, "http://127.0.0.1:3188".into());
    local::register_at(dir.path(), &mut b, second, "http://127.0.0.1:3188".into());
    assert_ne!(
        a.platforms["local"].connection_registration(),
        b.platforms["local"].connection_registration()
    );
}
