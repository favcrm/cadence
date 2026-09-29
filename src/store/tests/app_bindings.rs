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

/// CAD-796 adversarial-first: a material rebind or revoke must withdraw the
/// installation's prior approval and its derived grants; an identical
/// re-save must keep both. RED without the guard: the approval row stays
/// `approved` across the rebind.
#[test]
fn cad796_rebind_and_revoke_withdraw_approval_unchanged_resave_keeps_it() {
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
    // Identical re-save: the revision still bumps and waiting effects still
    // close (CAD-692), but the installation approval and derived grants are
    // untouched — no approval churn for an unchanged binding.
    let same = s.app_binding_update("install-a", &id, 1, &config).unwrap();
    assert_eq!(same["binding"]["revision"], 2);
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
    assert!(s.app_binding_update("install-a", &id, 2, &forged).is_err());
    assert!(s.app_binding_show("install-b", &id).is_err());
    assert_eq!(approval_of(), Some("approved".into()));
    // Material rebind: new connection incarnation withdraws approval and
    // the derived grant, and closes nothing it does not own.
    let mut changed = config.clone();
    changed["connection_id"] = json!("conn-b");
    let second = s.app_binding_update("install-a", &id, 2, &changed).unwrap();
    assert_eq!(second["binding"]["revision"], 3);
    assert_eq!(approval_of(), Some("revoked".into()));
    let grant = s
        .platform_grant("worker-a", "fixture", "work")
        .unwrap()
        .unwrap();
    assert!(!grant.scopes.contains(&"widgets:write".to_string()));
    assert!(grant.scopes.contains(&"widgets:read".to_string()));
    // Operator re-approves the changed binding: authority returns.
    s.app_capability_decide("install-a", digest, true).unwrap();
    assert_eq!(approval_of(), Some("approved".into()));
    // Revoke withdraws again; a concurrent stale revision never lands.
    assert!(s.app_binding_revoke("install-a", &id, 2).is_err());
    s.app_binding_revoke("install-a", &id, 3).unwrap();
    assert_eq!(approval_of(), Some("revoked".into()));
}

/// CAD-796: revoking or rotating the credential under a configured binding
/// withdraws every affected installation's approval in the same transaction.
#[test]
fn cad796_credential_revoke_and_rotate_withdraw_bound_approvals() {
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
    s.app_binding_create("install-a", None, "publication", &config, "cad796-cred-req")
        .unwrap();
    // Rotate: revision bump stales the binding and withdraws approval.
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
        "revoked"
    );
    // Re-approve, then revoke the credential: approval withdraws again.
    s.app_capability_decide("install-a", digest, true).unwrap();
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
        "revoked"
    );
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
