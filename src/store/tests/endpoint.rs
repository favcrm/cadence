use super::*;

fn digest() -> String {
    format!("sha256:{}", "a".repeat(64))
}

#[test]
fn endpoint_defaults_disabled_and_requires_verify_before_enable() {
    let (dir, store) = store();
    reg(&store, "a1", &dir.path().join("w"));

    let endpoint = store
        .endpoint_add("a1", "email", "billing", "proj", "invoice", &digest())
        .unwrap();
    assert_eq!(endpoint.state, "disabled");
    assert!(!endpoint.verified);
    assert_eq!(endpoint.agent_alias.as_deref(), Some("a1"));
    assert_eq!(endpoint.project, "proj");
    assert_eq!(endpoint.workflow, "invoice");
    assert_eq!(endpoint.digest, digest());

    let refused = store.endpoint_set_enabled(&endpoint.id, true).unwrap_err();
    assert!(refused.to_string().contains("not verified"), "{refused}");

    let verified = store.endpoint_verify(&endpoint.id).unwrap();
    assert!(verified.verified);
    assert_eq!(verified.state, "disabled");

    assert_eq!(
        store
            .endpoint_set_enabled(&endpoint.id, true)
            .unwrap()
            .state,
        "enabled"
    );
    assert_eq!(
        store
            .endpoint_set_enabled(&endpoint.id, false)
            .unwrap()
            .state,
        "disabled"
    );
    assert_eq!(store.endpoints(Some("a1"), None).unwrap().len(), 1);
    assert!(store.endpoints(None, Some("enabled")).unwrap().is_empty());

    assert_eq!(store.endpoint_remove(&endpoint.id).unwrap().id, endpoint.id);
    assert!(store.endpoint(&endpoint.id).is_err());
}

#[test]
fn endpoint_refuses_duplicate_unknown_agent_and_bad_values() {
    let (dir, store) = store();
    reg(&store, "a1", &dir.path().join("w"));
    reg(&store, "a2", &dir.path().join("w"));

    store
        .endpoint_add("a1", "email", "billing", "proj", "invoice", &digest())
        .unwrap();
    let duplicate = store
        .endpoint_add("a2", "email", "billing", "proj", "invoice", &digest())
        .unwrap_err();
    assert!(
        duplicate.to_string().contains("already exists"),
        "{duplicate}"
    );

    let unknown = store
        .endpoint_add("missing", "email", "other", "proj", "invoice", &digest())
        .unwrap_err();
    assert!(
        unknown.to_string().contains("Unknown managed agent"),
        "{unknown}"
    );

    assert!(store
        .endpoint_add("a1", "sms", "other", "proj", "invoice", &digest())
        .is_err());
    assert!(store
        .endpoint_add("a1", "email", "Bad_Slug", "proj", "invoice", &digest())
        .is_err());
    assert!(store
        .endpoint_add("a1", "email", "x", "proj", "invoice", "sha256:abc")
        .is_err());
}

#[test]
fn endpoint_stays_bound_to_uid_when_the_alias_row_changes() {
    let (dir, store) = store();
    reg(&store, "a1", &dir.path().join("w"));
    let endpoint = store
        .endpoint_add("a1", "webhook", "orders", "proj", "invoice", &digest())
        .unwrap();
    let original_uid = endpoint.agent_uid.clone();

    // Simulate a re-registered alias: the agent row's uid changes while the
    // endpoint row is untouched.
    store
        .write_tx(|tx| {
            tx.execute("UPDATE agents SET uid='agt_replaced' WHERE alias='a1'", [])?;
            Ok(())
        })
        .unwrap();

    let moved = store.endpoint(&endpoint.id).unwrap();
    assert_eq!(moved.agent_uid, original_uid);
    assert!(moved.agent_alias.is_none());
}
