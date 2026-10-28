// Independently authored CAD-1085 acceptance for the production attempt guard.
use super::{EntryContextV1, VerifiedHumanAttemptV1};

#[test]
fn claimed_credential_attempt_refuses_a_different_management_context() {
    let attempt = VerifiedHumanAttemptV1 {
        request_id: "00000000-0000-4000-8000-000000000001".to_string(),
        attempt_id: "00000000-0000-4000-8000-000000000002".to_string(),
        verb: "enroll".to_string(),
        provider: "smtp".to_string(),
        connection_id: None,
        target_id: "acct-context-acceptance".to_string(),
        shape: Some("smtp".to_string()),
        scopes: Vec::new(),
        expected_credential_revision: None,
        expected_authorization_revision: None,
        checked_context: EntryContextV1 {
            workspace_id: "workspace-context-acceptance".to_string(),
            custody_domain: "local".to_string(),
            custody_epoch: 3,
            owner_epoch: 7,
            supervisor_generation: "11111111111111111111111111111111".to_string(),
            loaded_registry_digest: None,
            target_agent_uid: None,
        },
        verification_request_id: None,
        entry_session_binding: "entry-context-acceptance".to_string(),
    };

    attempt
        .require_management_context(
            "workspace-context-acceptance",
            "local",
            3,
            7,
            "11111111111111111111111111111111",
        )
        .expect("the exact claimed context must remain usable");

    let changed_contexts = [
        (
            "owner restart with unchanged workspace and database",
            "workspace-context-acceptance",
            "local",
            3,
            8,
            "11111111111111111111111111111111",
        ),
        (
            "different database epoch",
            "workspace-context-acceptance",
            "local",
            4,
            7,
            "11111111111111111111111111111111",
        ),
        (
            "different workspace",
            "workspace-other",
            "local",
            3,
            7,
            "11111111111111111111111111111111",
        ),
        (
            "different supervisor generation",
            "workspace-context-acceptance",
            "local",
            3,
            7,
            "22222222222222222222222222222222",
        ),
        (
            "different custody domain",
            "workspace-context-acceptance",
            "hosted",
            3,
            7,
            "11111111111111111111111111111111",
        ),
    ];

    for (case, workspace, domain, database_epoch, owner_epoch, generation) in changed_contexts {
        let error = attempt
            .require_management_context(workspace, domain, database_epoch, owner_epoch, generation)
            .expect_err(case);
        assert_eq!(error.code(), Some("request_changed"), "{case}");
    }
}
