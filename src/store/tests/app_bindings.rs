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

/// CAD-796 adversarial-first: a material rebind or revoke must drop the
/// installation's derived grants; an identical re-save must keep them.
/// CAD-1119 (operator direction: install = consent, binding = consent for
/// the slot): neither the rebind nor the revoke withdraws the
/// installation's approval any more. The revoked binding itself stops the
/// slot.
#[test]
fn cad796_rebind_and_revoke_drop_grants_and_keep_install_consent() {
    let (_dir, s) = store();
    s.conn()
        .execute_batch(crate::store::app_bindings::SCHEMA)
        .unwrap();
    let digest = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let approval_of = || {
        s.conn()
            .query_row(
                "SELECT state FROM app_install_capabilities WHERE install_id='install-a'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .unwrap()
    };
    s.app_capability_decide("install-a", digest, true).unwrap();
    assert_eq!(approval_of(), Some("approved".into()));
    // One derived grant row for this installation, plus a hand-made grant
    // on the same triple that must survive the withdrawal.
    s.app_grants_set(
        "project/app",
        "install-a",
        &[(
            "worker-a".into(),
            "fixture".into(),
            "work".into(),
            vec!["widgets:write".into()],
        )],
        "operator",
    )
    .unwrap();
    s.platform_grant_add(
        "worker-a",
        "fixture",
        "work",
        &["widgets:read".into()],
        "operator",
    )
    .unwrap_or_else(|_| {
        // `platform_grant_add` refuses unenrolled accounts: enroll first.
        s.platform_enroll(
            &crate::store::CredentialRecord {
                connection_id: "conn-seed".into(),
                credential_revision: 1,
                platform: "fixture".into(),
                account: "work".into(),
                scopes: vec!["widgets:read".into(), "widgets:write".into()],
                fingerprint: "seed".into(),
                custody: "file".into(),
                exchange: "token".into(),
                enrolled_at: 1.0,
                by: "operator".into(),
            },
            false,
            None,
        )
        .unwrap();
        // Re-derive after the credential exists so the grant lands.
        s.app_grants_set(
            "project/app",
            "install-a",
            &[(
                "worker-a".into(),
                "fixture".into(),
                "work".into(),
                vec!["widgets:write".into()],
            )],
            "operator",
        )
        .unwrap();
        s.platform_grant_add(
            "worker-a",
            "fixture",
            "work",
            &["widgets:read".into()],
            "operator",
        )
        .unwrap()
    });
    let config = json!({"schema":1,"install_id":"install-a","context":null,
        "bundle_digest":digest,"workspace_id":"ws","connection_id":"conn-a",
        "provider":"fixture","account":"work","connection_kind":"enrolled",
        "connection_revision":1,"registration_digest":"reg-1","descriptor_revision":1,
        "mapping":{"effect":"send","scopes":["widgets:write"]},"declaration":{}});
    let first = s
        .app_binding_create("install-a", None, "publication", &config, "cad796-req-1")
        .unwrap();
    let id = first["binding"]["id"].as_str().unwrap().to_string();
    // Identical re-save: a complete no-op — same revision and receipt,
    // open effects, standing approval and intact derived grants.
    let same = s.app_binding_update("install-a", &id, 1, &config).unwrap();
    assert_eq!(same["binding"], first["binding"]);
    assert_eq!(approval_of(), Some("approved".into()));
    assert!(s
        .platform_grant("worker-a", "fixture", "work")
        .unwrap()
        .unwrap()
        .scopes
        .contains(&"widgets:write".to_string()));
    // Forged installation scope never reaches the guard: it refuses first.
    let mut forged = config.clone();
    forged["install_id"] = json!("install-b");
    assert!(s.app_binding_update("install-a", &id, 1, &forged).is_err());
    assert!(s.app_binding_show("install-b", &id).is_err());
    assert_eq!(approval_of(), Some("approved".into()));
    // Material rebind: the operator's rebind is the slot's consent. It
    // drops the derived grant and closes nothing it does not own; the
    // installation's approval stays in force.
    let mut changed = config.clone();
    changed["connection_id"] = json!("conn-b");
    let second = s.app_binding_update("install-a", &id, 1, &changed).unwrap();
    assert_eq!(second["binding"]["revision"], 2);
    assert_eq!(approval_of(), Some("approved".into()));
    let grant = s
        .platform_grant("worker-a", "fixture", "work")
        .unwrap()
        .unwrap();
    assert!(!grant.scopes.contains(&"widgets:write".to_string()));
    assert!(grant.scopes.contains(&"widgets:read".to_string()));
    // Revoke: a concurrent stale revision never lands; the binding is
    // revoked and the slot has no configured binding left.
    assert!(s.app_binding_revoke("install-a", &id, 1).is_err());
    s.app_binding_revoke("install-a", &id, 2).unwrap();
    assert_eq!(
        s.app_binding_show("install-a", &id).unwrap()["binding"]["state"],
        "revoked"
    );
    assert!(s
        .app_binding_for_slot("install-a", None, "publication", digest)
        .unwrap()
        .is_none());
    assert_eq!(approval_of(), Some("approved".into()));
}

/// CAD-796: revoking or rotating the credential under a configured binding
/// stops that binding in the same transaction. CAD-1119: the slot stops at
/// the binding receipt, which no longer proves current, while the
/// installation's approval (the install consent) stays.
#[test]
fn cad796_credential_revoke_and_rotate_stop_the_bound_slot() {
    let (_dir, s) = store();
    s.conn()
        .execute_batch(crate::store::app_bindings::SCHEMA)
        .unwrap();
    let digest = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    s.platform_enroll(
        &crate::store::CredentialRecord {
            connection_id: "conn-cred".into(),
            credential_revision: 1,
            platform: "fixture".into(),
            account: "bound".into(),
            scopes: vec!["widgets:write".into()],
            fingerprint: "cred-one".into(),
            custody: "file".into(),
            exchange: "token".into(),
            enrolled_at: 1.0,
            by: "operator".into(),
        },
        false,
        None,
    )
    .unwrap();
    s.app_capability_decide("install-a", digest, true).unwrap();
    let config = json!({"schema":1,"install_id":"install-a","context":null,
        "bundle_digest":digest,"workspace_id":"ws","connection_id":"conn-cred",
        "provider":"fixture","account":"bound","connection_kind":"enrolled",
        "connection_revision":1,"registration_digest":"reg-1","descriptor_revision":1,
        "mapping":{"effect":"send","scopes":["widgets:write"]},"declaration":{}});
    let created = s
        .app_binding_create("install-a", None, "publication", &config, "cad796-cred-req")
        .unwrap();
    let proof: crate::store::app_bindings::BindingProof =
        serde_json::from_value(json!({"id": created["binding"]["id"],
            "revision": created["binding"]["revision"],
            "digest": created["binding"]["digest"],
            "config": created["binding"]["config"]}))
        .unwrap();
    s.conn()
        .execute(
            "INSERT OR REPLACE INTO connection_metadata(singleton, workspace_id) VALUES(1, 'ws')",
            [],
        )
        .unwrap();
    let current = || {
        crate::store::app_bindings::binding_current_in(
            &s.conn(),
            "install-a",
            None,
            "publication",
            &proof,
        )
        .unwrap()
    };
    assert!(current(), "the fresh binding is current");
    // Rotate: the revision bump stales the binding; approval stays.
    s.platform_enroll(
        &crate::store::CredentialRecord {
            connection_id: "conn-cred".into(),
            credential_revision: 2,
            platform: "fixture".into(),
            account: "bound".into(),
            scopes: vec!["widgets:write".into()],
            fingerprint: "cred-two".into(),
            custody: "file".into(),
            exchange: "token".into(),
            enrolled_at: 2.0,
            by: "operator".into(),
        },
        true,
        None,
    )
    .unwrap();
    assert_eq!(
        s.conn()
            .query_row(
                "SELECT state FROM app_install_capabilities WHERE install_id='install-a'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
        "approved"
    );
    assert!(!current(), "a rotated credential stops the bound slot");
    // Revoke the credential: the slot stays stopped.
    s.platform_revoke(
        "fixture",
        "bound",
        "operator",
        Some("cad796 test revoke"),
        &[],
    )
    .unwrap()
    .expect("credential record must exist");
    assert_eq!(
        s.conn()
            .query_row(
                "SELECT state FROM app_install_capabilities WHERE install_id='install-a'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
        "approved"
    );
    assert!(!current(), "a revoked credential stops the bound slot");
}

/// CAD-796 adversarial: a corrupt derived-grant row must fail the rebind
/// closed — the whole transaction rolls back — instead of committing a
/// partial cleanup. RED without error propagation: the update succeeds,
/// the approval is withdrawn, and cleanup silently skipped the bad row.
#[test]
fn cad796_corrupt_grant_row_rolls_back_rebind() {
    let (_dir, s) = store();
    s.conn()
        .execute_batch(crate::store::app_bindings::SCHEMA)
        .unwrap();
    let digest = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    s.app_capability_decide("install-a", digest, true).unwrap();
    s.app_grants_set(
        "project/app",
        "install-a",
        &[(
            "worker-a".into(),
            "fixture".into(),
            "work".into(),
            vec!["widgets:write".into()],
        )],
        "operator",
    )
    .unwrap();
    // A second derivation row for the same installation with malformed
    // scope JSON — the rebind must refuse rather than skip it.
    s.conn()
        .execute(
            "INSERT INTO app_grants(app, agent, platform, account, scopes, granted_at, by, install_id) VALUES(?,?,?,?,?,?,?,?)",
            params!["project/legacy", "worker-a", "fixture", "work", "not-json", 1.0, "operator", "install-a"],
        )
        .unwrap();
    let config = json!({"schema":1,"install_id":"install-a","context":null,
        "bundle_digest":digest,"workspace_id":"ws","connection_id":"conn-a",
        "provider":"fixture","account":"work","connection_kind":"enrolled",
        "connection_revision":1,"registration_digest":"reg-1","descriptor_revision":1,
        "mapping":{"effect":"send","scopes":["widgets:write"]},"declaration":{}});
    let first = s
        .app_binding_create(
            "install-a",
            None,
            "publication",
            &config,
            "cad796-corrupt-req",
        )
        .unwrap();
    let id = first["binding"]["id"].as_str().unwrap().to_string();
    let mut changed = config.clone();
    changed["connection_id"] = json!("conn-b");
    assert!(s.app_binding_update("install-a", &id, 1, &changed).is_err());
    // Nothing committed: same revision, approval still in force.
    assert_eq!(
        s.app_binding_show("install-a", &id).unwrap()["binding"]["revision"],
        1
    );
    assert_eq!(
        s.conn()
            .query_row(
                "SELECT state FROM app_install_capabilities WHERE install_id='install-a'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
        "approved"
    );
}
