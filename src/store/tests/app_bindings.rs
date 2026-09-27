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
