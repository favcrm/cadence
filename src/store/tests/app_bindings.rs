use super::*;

#[test]
fn cad692_binding_configuration_cas_preserves_scope_and_never_issues_grants() {
    let (_dir, s) = store();
    s.conn()
        .execute_batch(crate::store::app_bindings::SCHEMA)
        .unwrap();
    let config = json!({"schema":1,"install_id":"install-a","context":null,"connection_id":"builtin-local","registration_digest":"receipt-one"});
    let first = s
        .app_binding_create("install-a", None, "publication", &config, "binding-request")
        .unwrap();
    let binding = &first["binding"];
    assert_eq!(binding["state"], "configured");
    assert_eq!(binding["revision"], 1);
    assert_eq!(
        s.app_binding_create("install-a", None, "publication", &config, "binding-request")
            .unwrap(),
        first
    );
    assert!(s
        .app_binding_show("install-b", binding["id"].as_str().unwrap())
        .is_err());
    let mut changed = config.clone();
    changed["registration_digest"] = json!("receipt-two");
    assert!(s
        .app_binding_create(
            "install-a",
            None,
            "publication",
            &changed,
            "binding-request"
        )
        .is_err());
    let id = binding["id"].as_str().unwrap();
    let second = s.app_binding_update("install-a", id, 1, &changed).unwrap();
    assert_eq!(second["binding"]["revision"], 2);
    assert_ne!(second["binding"]["digest"], binding["digest"]);
    assert!(s.app_binding_update("install-a", id, 1, &config).is_err());
    let mut forged = changed.clone();
    forged["install_id"] = json!("install-b");
    assert!(s.app_binding_update("install-a", id, 2, &forged).is_err());
    assert_eq!(s.app_binding_show("install-a", id).unwrap(), second);
    let revoked = s.app_binding_revoke("install-a", id, 2).unwrap();
    assert_eq!(revoked["binding"]["state"], "revoked");
    assert!(s.app_binding_update("install-a", id, 3, &config).is_err());
    assert!(s.app_binding_revoke("install-a", id, 2).is_err());
    assert_eq!(
        s.conn()
            .query_row("SELECT count(*) FROM platform_grants", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn cad743_new_version_can_bind_after_one_hundred_old_version_rows() {
    let (_dir, store) = store();
    let old_digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let new_digest = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let mut first_old_id = String::new();
    for index in 0..100 {
        let context = format!("brand-{index:03}");
        let config = json!({"schema":1,"install_id":"install-a","context":{"id":context},
            "bundle_digest":old_digest,"connection_id":"local-id"});
        let row = store
            .app_binding_create(
                "install-a",
                Some(&context),
                "publication",
                &config,
                &format!("old-request-{index:03}"),
            )
            .unwrap();
        if index == 0 {
            first_old_id = row["binding"]["id"].as_str().unwrap().to_string();
        }
    }
    let new_config = json!({"schema":1,"install_id":"install-a","context":null,
        "bundle_digest":new_digest,"connection_id":"local-id"});
    let new_binding = store
        .app_binding_create("install-a", None, "publication", &new_config, "new-request")
        .unwrap();
    assert_eq!(
        new_binding["binding"]["config"]["bundle_digest"],
        new_digest
    );
    let current = store
        .app_binding_list_preferred("install-a", None, Some(new_digest))
        .unwrap();
    assert_eq!(current["bindings"].as_array().unwrap().len(), 100);
    assert_eq!(current["bindings"][0]["id"], new_binding["binding"]["id"]);
    assert_eq!(current["truncated"], true);
    assert!(
        store
            .app_binding_create(
                "install-a",
                Some("another-brand"),
                "publication",
                &json!({"schema":1,"install_id":"install-a","context":{"id":"another-brand"},
                "bundle_digest":old_digest,"connection_id":"local-id"}),
                "overflow-old-request",
            )
            .is_err(),
        "old version remains bounded at one hundred configured bindings"
    );
    store
        .app_binding_revoke("install-a", &first_old_id, 1)
        .unwrap();
    store
        .app_binding_create(
            "install-a",
            Some("another-brand"),
            "publication",
            &json!({"schema":1,"install_id":"install-a","context":{"id":"another-brand"},
            "bundle_digest":old_digest,"connection_id":"local-id"}),
            "replacement-old-request",
        )
        .unwrap();
    let returned = store
        .app_binding_list_preferred("install-a", None, Some(old_digest))
        .unwrap();
    assert_eq!(returned["bindings"].as_array().unwrap().len(), 100);
    assert!(returned["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["config"]["bundle_digest"] == old_digest));
}
