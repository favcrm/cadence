//! CAD-868 actual discovery, binding receipt and broker credential consumers.
use super::*;
use crate::platform::{agenticos_external, deployments::DeploymentMetadata, Custody, Key};
use std::collections::BTreeMap;

fn fixture() -> (
    tempfile::TempDir,
    Arc<Shared>,
    Value,
    BTreeMap<String, String>,
    BindingProof,
) {
    let dir = tempfile::tempdir().unwrap();
    let metadata = DeploymentMetadata::parse(br#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@2","transport":"hosted-media-lease@1"}]}"#).unwrap();
    let mut opts = ServeOptions::default();
    opts.platforms.insert(
        agenticos_external::PLATFORM.into(),
        Arc::new(
            agenticos_external::AgenticosExternalAdapter::hosted_media(
                metadata.hosted_media().unwrap(),
            )
            .unwrap(),
        ),
    );
    let mut shared = Shared::new(dir.path(), &opts).unwrap();
    // Synthetic file custody only; never consult an installed host keychain.
    Arc::get_mut(&mut shared).unwrap().platform_custody =
        Custody::File(dir.path().join("fixture-custody"));
    let connections = shared.connection_list_locked().unwrap();
    let connection = connections
        .iter()
        .find(|row| row["provider"] == "agenticos_external")
        .unwrap();
    assert_eq!(connection["kind"], "builtin");
    assert_eq!(connection["account"], "hosted");
    assert_eq!(connection["status"]["manifest_status"], "matched");
    assert_eq!(connection["status"]["custody_available"], true);
    assert_eq!(connection["status"]["network_checked"], false);
    let bundle = json!({"digest":"sha256:fixture-bundle"});
    let files = BTreeMap::from([(
        "app.md".into(),
        include_str!("../../../workspace-apps/social-content/app.md").into(),
    )]);
    let config = shared
        .app_binding_config(
            "install868",
            None,
            "image",
            connection["id"].as_str().unwrap(),
            &bundle,
            &files,
        )
        .unwrap();
    shared
        .store
        .app_binding_create("install868", None, "image", &config, "bind868")
        .unwrap();
    let proof = shared
        .store
        .app_binding_for_slot(
            "install868",
            None,
            "image",
            bundle["digest"].as_str().unwrap(),
        )
        .unwrap()
        .unwrap();
    (dir, shared, bundle, files, proof)
}

#[test]
fn cad868_builtin_discovery_binding_and_app_credential_are_registration_dependent() {
    let (_dir, shared, bundle, files, proof) = fixture();
    shared
        .app_binding_receipt_current("install868", None, "image", &proof, &bundle, &files)
        .unwrap();
    assert!(shared
        .app_capability_credential(&proof.config)
        .unwrap()
        .is_empty());
    assert!(shared.store.platform_credentials().unwrap().is_empty());
    // Neither discovery nor app admission silently enrolls bytes or opens a
    // credentialless legacy grant/default route.
    assert!(!crate::platform::is_builtin("agenticos_external", "hosted"));
    assert!(crate::platform::load_credential(
        &shared.store,
        &shared.platform_custody,
        "agenticos_external",
        "hosted"
    )
    .is_err());
    let connection = proof.config["connection_id"].as_str().unwrap();
    assert!(shared
        .app_binding_config("install868", None, "source", connection, &bundle, &files)
        .is_err());
    assert!(shared
        .app_binding_config(
            "install868",
            None,
            "publication",
            connection,
            &bundle,
            &files
        )
        .is_err());
}

#[test]
fn cad868_broker_quote_refuses_forged_missing_and_stale_builtin_receipts() {
    let (_dir, shared, _bundle, _files, proof) = fixture();
    for (field, value) in [
        ("schema", json!(2)),
        ("workspace_id", json!("other-workspace")),
        ("provider", json!("unregistered")),
        ("account", json!("other")),
        ("connection_kind", json!("enrolled")),
        ("connection_kind", Value::Null),
        ("connection_id", json!("missing")),
        ("connection_id", Value::Null),
        ("connection_revision", json!(1)),
        ("registration_digest", json!("sha256:forged")),
        ("registration_digest", Value::Null),
        ("sink_registration", json!("sha256:forged")),
        ("sink_registration", Value::Null),
        ("descriptor_revision", json!("external-mode")),
        ("reviewed_pin", Value::Null),
        ("reported_pin", json!("stale")),
    ] {
        let mut forged = proof.clone();
        forged.config[field] = value;
        assert!(
            shared.app_capability_credential(&forged.config).is_err(),
            "{field}"
        );
        // The real broker quote consumer must reject before media price I/O.
        assert!(shared.app_capability_quote(&forged).is_err(), "{field}");
    }
}

#[test]
fn cad868_adapter_disappearance_or_mode_switch_stales_existing_binding() {
    let (_dir, mut shared, bundle, files, proof) = fixture();
    Arc::get_mut(&mut shared)
        .unwrap()
        .platforms
        .remove("agenticos_external");
    assert!(shared
        .app_binding_receipt_current("install868", None, "image", &proof, &bundle, &files)
        .is_err());
    assert!(shared.app_capability_credential(&proof.config).is_err());
    assert!(shared.app_capability_quote(&proof).is_err());
    Arc::get_mut(&mut shared).unwrap().platforms.insert(
        "agenticos_external".into(),
        Arc::new(
            agenticos_external::AgenticosExternalAdapter::with_deployment_pin(
                "https://external.example.test",
                Some(agenticos_external::MANIFEST_PIN),
            )
            .unwrap(),
        ),
    );
    assert!(!shared
        .connection_list_locked()
        .unwrap()
        .iter()
        .any(|row| row["provider"] == "agenticos_external"));
    assert!(shared
        .app_binding_receipt_current("install868", None, "image", &proof, &bundle, &files)
        .is_err());
    assert!(shared.app_capability_credential(&proof.config).is_err());
    assert!(shared.app_capability_quote(&proof).is_err());
}

#[test]
fn cad868_enrolled_external_account_named_hosted_never_upgrades_to_builtin() {
    let (_dir, mut shared, bundle, files, proof) = fixture();
    let token = b"synthetic-cad868-external-token";
    let custody = shared
        .platform_custody
        .put(
            &Key {
                platform: "agenticos_external",
                account: "hosted",
            },
            token,
        )
        .unwrap();
    shared
        .store
        .platform_enroll(
            &crate::store::CredentialRecord {
                connection_id: "enrolled868".into(),
                credential_revision: 1,
                platform: "agenticos_external".into(),
                account: "hosted".into(),
                scopes: vec!["provider.draft".into()],
                fingerprint: crate::secret::fingerprint(token),
                custody: custody.into(),
                exchange: "token".into(),
                enrolled_at: 1.0,
                by: "operator".into(),
            },
            false,
            None,
        )
        .unwrap();
    let mut enrolled = proof.clone();
    enrolled.config["connection_kind"] = json!("enrolled");
    enrolled.config["connection_id"] = json!("enrolled868");
    enrolled.config["connection_revision"] = json!(1);
    assert!(shared.app_capability_credential(&enrolled.config).is_err());
    assert!(shared.app_capability_quote(&enrolled).is_err());
    // The separate builtin still holds no bytes; the enrolled custody is not
    // overwritten, reclassified or read as a fake lease credential.
    assert!(shared
        .app_capability_credential(&proof.config)
        .unwrap()
        .is_empty());
    assert_eq!(
        crate::platform::load_credential(
            &shared.store,
            &shared.platform_custody,
            "agenticos_external",
            "hosted"
        )
        .unwrap(),
        token
    );
    Arc::get_mut(&mut shared).unwrap().platforms.insert(
        "agenticos_external".into(),
        Arc::new(
            agenticos_external::AgenticosExternalAdapter::with_deployment_pin(
                "https://external.example.test",
                Some(agenticos_external::MANIFEST_PIN),
            )
            .unwrap(),
        ),
    );
    assert_eq!(
        shared.app_capability_credential(&enrolled.config).unwrap(),
        token
    );
    assert!(shared.app_capability_credential(&proof.config).is_err());
    assert!(shared
        .app_binding_receipt_current("install868", None, "image", &proof, &bundle, &files)
        .is_err());
}
