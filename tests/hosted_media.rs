//! CAD-868 trusted media composition at real operator/binding RPC consumers.
//! CAD-1060: the same hosted builtin also binds the source read.
//! All data is synthetic; valid price/execute calls are intentionally not sent.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::{issue::Pm, platform::deployments::DeploymentMetadata};
use common::{daemon_opts, TestDaemon};
use serde_json::{json, Value};
use std::path::Path;

#[cfg(feature = "test-seam")]
#[path = "hosted_media/native.rs"]
mod native;

#[test]
fn cad868_hosted_connection_binding_and_broker_keep_operator_and_turn_gates() {
    let root = tempfile::tempdir().unwrap();
    let pm = Pm::init(&root.path().join("pm")).unwrap();
    let mut opts = daemon_opts();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
    opts.provider_deployments = Some(DeploymentMetadata::parse(br#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@2","transport":"hosted-media-lease@1"}]}"#).unwrap());
    let daemon = TestDaemon::start_opts(opts);
    daemon.register_inbox("hosted-media-outsider");
    let connections = daemon.operator_rpc("connection_list", json!({})).unwrap();
    let connection = connections["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["provider"] == "agenticos_external")
        .unwrap();
    assert_eq!(connection["kind"], "builtin");
    assert_eq!(connection["account"], "hosted");
    assert_eq!(
        connection["descriptor"]["builtin_accounts"],
        json!(["hosted"])
    );
    let ids = |key: &str, field: &str| -> Vec<Value> {
        connection["descriptor"][key]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row[field].clone())
            .collect()
    };
    assert_eq!(
        ids("capabilities", "id"),
        vec![json!("social.read"), json!("media.generate")]
    );
    assert_eq!(
        ids("action_mappings", "action"),
        vec![json!("list_posts"), json!("generate_image")]
    );
    assert_eq!(connection["status"]["network_checked"], false);
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("workspace-apps/social-content");
    let install = daemon
        .operator_rpc("app_workspace_install", json!({"source":source}))
        .unwrap();
    daemon
        .operator_rpc(
            "app_local_install_approve",
            json!({"install_id":install["install_id"],"digest":install["digest"]}),
        )
        .unwrap();
    let bind = json!({"install_id":install["install_id"],"slot":"image","connection_id":connection["id"],"request_id":"hosted-image-binding"});
    assert!(daemon
        .agent_rpc("hosted-media-outsider", "app_binding_create", bind.clone())
        .is_err());
    let binding = daemon
        .operator_rpc("app_binding_create", bind.clone())
        .unwrap();
    assert_eq!(binding["binding"]["config"]["connection_kind"], "builtin");
    assert_eq!(
        binding["binding"]["config"]["mapping"]["capability"],
        "media.generate"
    );
    let mut publication = bind.clone();
    publication["slot"] = json!("publication");
    publication["request_id"] = json!("hosted-publication");
    assert!(daemon
        .operator_rpc("app_binding_create", publication)
        .is_err());
    let mut source = bind.clone();
    source["slot"] = json!("source");
    source["request_id"] = json!("hosted-source");
    assert!(daemon
        .agent_rpc(
            "hosted-media-outsider",
            "app_binding_create",
            source.clone()
        )
        .is_err());
    let bound = daemon.operator_rpc("app_binding_create", source).unwrap();
    let config = &bound["binding"]["config"];
    assert_eq!(config["connection_kind"], "builtin");
    assert_eq!(config["account"], "hosted");
    assert_eq!(config["mapping"]["capability"], "social.read");
    assert_eq!(config["mapping"]["action"], "list_posts");
    for slot in ["image", "source"] {
        let quote = json!({"install_id":install["install_id"],"slot":slot});
        assert!(daemon
            .agent_rpc("hosted-media-outsider", "app_binding_quote", quote.clone())
            .is_err());
        for (field, value) in [
            ("transport", json!("hosted-media-lease@1")),
            ("connection_kind", json!("builtin")),
            ("account", json!("hosted")),
            ("company", json!("other-company")),
            ("origin", json!("http://api.internal")),
            ("credential", json!("")),
            ("operator", json!(true)),
        ] {
            let mut forged = quote.clone();
            forged[field] = value;
            assert!(daemon.operator_rpc("app_binding_quote", forged).is_err());
        }
        let call = json!({"message":"not-assigned","token":"forged-turn","slot":slot,"request_id":"hosted-call","input":{}});
        assert!(daemon
            .operator_rpc("app_run_capability_call", call.clone())
            .is_err());
        assert!(daemon
            .agent_rpc("hosted-media-outsider", "app_run_capability_call", call)
            .is_err());
    }
    assert!(daemon.operator_rpc("connection_create", json!({"provider":"agenticos_external","account":"hosted","shape":"token","token":"synthetic-placeholder","scopes":["provider.draft"],"accept_same_uid_risk":true})).is_err());
    let listed = daemon.operator_rpc("connection_list", json!({})).unwrap();
    assert_eq!(
        listed["connections"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["provider"] == "agenticos_external")
            .count(),
        1
    );
}
