// Independently authored CAD-1085 native source nonce/claim acceptance.
use super::{CredentialVerb, NewCredentialRequest};
use crate::store::native_model_turn::{self as source, SourceRow};
use crate::store::Store;

const SESSION: &str = "native-source-entry-session";
const CONTEXT: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

fn insert_source(
    store: &Store,
    workspace: &str,
    provider: &str,
    state: &str,
    revision: i64,
) -> SourceRow {
    let row = SourceRow {
        source_id: uuid::Uuid::new_v4().to_string(),
        workspace_id: workspace.to_string(),
        provider: provider.to_string(),
        account: uuid::Uuid::new_v4().to_string(),
        revision,
        origin: "https://example.com".to_string(),
        origin_kind: source::ORIGIN_PINNED.to_string(),
        catalog_revision: 1,
        model_uid_map: "{}".to_string(),
        vault_reference: "opaque-metadata-only-reference".to_string(),
        key_id: vec![0; 32],
        state: state.to_string(),
        created_ms: 1000,
        updated_ms: 1000,
    };
    store
        .write_tx(|tx| source::insert_source_in(tx, &row))
        .unwrap();
    row
}

fn intent(target: &str, expected: Option<i64>, verb: CredentialVerb) -> NewCredentialRequest {
    NewCredentialRequest {
        provider: "devin".to_string(),
        verb,
        connection_id: None,
        shape: Some("native-model-api-key".to_string()),
        scopes: Vec::new(),
        expected_credential_revision: expected,
        expected_authorization_revision: None,
        requester_kind: "operator_ui".to_string(),
        requester_participant: None,
        conversation_id: None,
        target_agent_uid: None,
        custody_domain: "local".to_string(),
        custody_epoch: 1,
        context_digest: CONTEXT.to_string(),
        target_id_override: Some(target.to_string()),
    }
}

fn counts(store: &Store, request: &str) -> (i64, i64, i64) {
    let conn = store.conn();
    conn.query_row(
        "SELECT
            (SELECT COUNT(*) FROM credential_request_nonces WHERE request_id=?1),
            (SELECT COUNT(*) FROM credential_request_nonces WHERE request_id=?1 AND claimed_at_ms IS NOT NULL),
            (SELECT COUNT(*) FROM credential_request_attempts WHERE request_id=?1)",
        [request],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .unwrap()
}

#[test]
fn native_rotation_nonce_binds_the_exact_live_source_and_rechecks_it_at_claim() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("source-nonce-acceptance.sqlite3")).unwrap();
    let workspace = store.connection_workspace_id().unwrap();
    // A different active account must not substitute for the exact target.
    let _other = insert_source(&store, &workspace, "devin", source::SOURCE_ACTIVE, 2);
    let target = insert_source(&store, &workspace, "devin", source::SOURCE_ACTIVE, 3);
    let request = store
        .create_credential_request(
            intent(&target.source_id, Some(3), CredentialVerb::Rotate),
            1000,
        )
        .unwrap();
    assert!(request.connection_id.is_none());
    assert_eq!(request.target_id, target.source_id);
    let (nonce, shown) = store
        .issue_credential_nonce(&request.request_id, SESSION, 1, CONTEXT, 1001)
        .expect("an exact current native source rotation must be collectable");
    assert_eq!(shown.target_id, target.source_id);
    let claimed = store
        .claim_credential_attempt(&request.request_id, &nonce, SESSION, 1, CONTEXT, 1002)
        .expect("the valid source-bound nonce must claim once");
    assert_eq!(claimed.request.target_id, target.source_id);
    assert_eq!(claimed.request.expected_credential_revision, Some(3));
    assert!(claimed.request.connection_id.is_none());
    assert_eq!(counts(&store, &request.request_id), (1, 1, 1));
    assert_eq!(
        store
            .credential_request(&request.request_id)
            .unwrap()
            .unwrap()
            .state,
        "submitting"
    );
    let replay = store
        .claim_credential_attempt(&request.request_id, &nonce, SESSION, 1, CONTEXT, 1003)
        .expect_err("a claimed native source nonce must never execute twice");
    assert_eq!(replay.code(), Some("already_submitted"));
    assert_eq!(counts(&store, &request.request_id), (1, 1, 1));

    for verb in [CredentialVerb::Rotate, CredentialVerb::Verify] {
        let error = store
            .create_credential_request(intent(&target.source_id, None, verb), 1000)
            .expect_err("native non-enrollment requests require a frozen source revision");
        assert_eq!(error.code(), Some("invalid_request"));
    }

    let foreign = insert_source(
        &store,
        "foreign-workspace",
        "devin",
        source::SOURCE_ACTIVE,
        3,
    );
    let provider_mismatch = insert_source(
        &store,
        &workspace,
        "other-provider",
        source::SOURCE_ACTIVE,
        3,
    );
    let locked = insert_source(&store, &workspace, "devin", source::SOURCE_LOCKED, 3);
    let revoked = insert_source(&store, &workspace, "devin", source::SOURCE_REVOKED, 3);
    let missing = uuid::Uuid::new_v4().to_string();
    let bad_targets = [
        ("stale revision", target.source_id.as_str(), 2),
        ("foreign workspace", foreign.source_id.as_str(), 3),
        (
            "different source provider",
            provider_mismatch.source_id.as_str(),
            3,
        ),
        ("locked source", locked.source_id.as_str(), 3),
        ("revoked source", revoked.source_id.as_str(), 3),
        ("missing source", missing.as_str(), 3),
    ];
    for (case, id, expected) in bad_targets {
        let request = store
            .create_credential_request(intent(id, Some(expected), CredentialVerb::Rotate), 1000)
            .unwrap();
        let error = store
            .issue_credential_nonce(&request.request_id, SESSION, 1, CONTEXT, 1001)
            .expect_err(case);
        assert_eq!(error.code(), Some("request_changed"), "{case}");
        assert_eq!(counts(&store, &request.request_id), (0, 0, 0), "{case}");
        assert_eq!(
            store
                .credential_request(&request.request_id)
                .unwrap()
                .unwrap()
                .state,
            "pending",
            "{case}"
        );
    }

    for change in ["rotate", "lock", "revoke"] {
        let row = insert_source(&store, &workspace, "devin", source::SOURCE_ACTIVE, 3);
        let request = store
            .create_credential_request(
                intent(&row.source_id, Some(3), CredentialVerb::Rotate),
                1000,
            )
            .unwrap();
        let (nonce, _) = store
            .issue_credential_nonce(&request.request_id, SESSION, 1, CONTEXT, 1001)
            .unwrap();
        store
            .write_tx(|tx| match change {
                "rotate" => source::rotate_source_in(
                    tx,
                    &workspace,
                    &row.source_id,
                    3,
                    &row.account,
                    &row.origin,
                    &row.origin_kind,
                    row.catalog_revision,
                    &row.model_uid_map,
                    "opaque-new-metadata-reference",
                    &row.key_id,
                    1002,
                ),
                "lock" => source::set_source_state_in(
                    tx,
                    &workspace,
                    &row.source_id,
                    source::SOURCE_LOCKED,
                    1002,
                ),
                "revoke" => source::set_source_state_in(
                    tx,
                    &workspace,
                    &row.source_id,
                    source::SOURCE_REVOKED,
                    1002,
                ),
                _ => unreachable!(),
            })
            .unwrap();
        let error = store
            .claim_credential_attempt(&request.request_id, &nonce, SESSION, 1, CONTEXT, 1003)
            .expect_err("source authority must be rechecked after nonce issuance");
        assert_eq!(error.code(), Some("request_changed"), "{change}");
        assert_eq!(counts(&store, &request.request_id), (1, 0, 0), "{change}");
        assert_eq!(
            store
                .credential_request(&request.request_id)
                .unwrap()
                .unwrap()
                .state,
            "pending",
            "{change}"
        );
    }
}

#[test]
fn claimed_native_source_is_not_rebound_and_ambiguous_default_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(
        &root
            .path()
            .join("claimed-source-selection-acceptance.sqlite3"),
    )
    .unwrap();
    let workspace = store.connection_workspace_id().unwrap();
    assert!(store
        .read_tx(|conn| source::active_source_in(&*conn, &workspace, "devin"))
        .unwrap()
        .is_none());

    let original = insert_source(&store, &workspace, "devin", source::SOURCE_ACTIVE, 3);
    let default = store
        .read_tx(|conn| source::active_source_in(&*conn, &workspace, "devin"))
        .unwrap()
        .expect("one active account is an unambiguous default");
    assert_eq!(default.source_id, original.source_id);

    let request = store
        .create_credential_request(
            intent(&original.source_id, Some(3), CredentialVerb::Rotate),
            1000,
        )
        .unwrap();
    let (nonce, _) = store
        .issue_credential_nonce(&request.request_id, SESSION, 1, CONTEXT, 1001)
        .unwrap();
    let claimed = store
        .claim_credential_attempt(&request.request_id, &nonce, SESSION, 1, CONTEXT, 1002)
        .unwrap();
    let expected_revision = claimed.request.expected_credential_revision.unwrap();
    let selected = store
        .native_model_rotation_source(
            &claimed.request.target_id,
            &claimed.request.provider,
            expected_revision,
        )
        .expect("the exact claimed source must be selectable before replacement");
    assert_eq!(selected.source_id, original.source_id);
    assert_eq!(selected.account, original.account);

    // A second legal active account at the SAME revision must not become a
    // silent default, and ambiguity must not prevent explicit-ID rotation.
    let replacement = insert_source(&store, &workspace, "devin", source::SOURCE_ACTIVE, 3);
    let ambiguous = store
        .read_tx(|conn| source::active_source_in(&*conn, &workspace, "devin"))
        .expect_err("multiple active accounts require explicit source selection");
    assert_eq!(ambiguous.code(), Some("auth_unavailable"));
    let explicit_original = store
        .native_model_rotation_source(&original.source_id, "devin", 3)
        .unwrap();
    let explicit_replacement = store
        .native_model_rotation_source(&replacement.source_id, "devin", 3)
        .unwrap();
    assert_eq!(explicit_original.source_id, original.source_id);
    assert_eq!(explicit_replacement.source_id, replacement.source_id);

    // The nonce was already claimed for A. Revoke A so B is now the only
    // default, then invoke the SAME production selector the apply path uses.
    store
        .write_tx(|tx| {
            source::set_source_state_in(
                tx,
                &workspace,
                &original.source_id,
                source::SOURCE_REVOKED,
                1003,
            )
        })
        .unwrap();
    let default = store
        .read_tx(|conn| source::active_source_in(&*conn, &workspace, "devin"))
        .unwrap()
        .expect("B is now the only active default");
    assert_eq!(default.source_id, replacement.source_id);
    let error = store
        .native_model_rotation_source(
            &claimed.request.target_id,
            &claimed.request.provider,
            expected_revision,
        )
        .expect_err("a claimed A rotation must refuse, never select same-revision B");
    assert_eq!(error.code(), Some("request_changed"));
    assert_eq!(claimed.request.target_id, original.source_id);
    assert_eq!(counts(&store, &request.request_id), (1, 1, 1));

    let original_after = store
        .native_model_source(&workspace, &original.source_id)
        .unwrap()
        .unwrap();
    let replacement_after = store
        .native_model_source(&workspace, &replacement.source_id)
        .unwrap()
        .unwrap();
    assert_eq!(original_after.state, source::SOURCE_REVOKED);
    assert_eq!(original_after.revision, 3);
    assert_eq!(replacement_after.state, source::SOURCE_ACTIVE);
    assert_eq!(replacement_after.revision, 3);
    assert_eq!(replacement_after.account, replacement.account);
    assert_eq!(
        replacement_after.vault_reference,
        replacement.vault_reference
    );
}
