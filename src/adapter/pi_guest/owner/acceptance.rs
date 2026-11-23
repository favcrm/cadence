//! CAD-1160 independently authored acceptance (aos159-constructor-guard).
//! Implementers must not edit/weaken this file. Calls the SAME production scope
//! guard used by opaque LaunchPermit::arm/consume, not a client/parser mirror.
//! Public descriptions below model scope only: NO LaunchPermit, Proof, trusted
//! key, elected graph, service caller or positive launch authority is created.
//! Current/phase/replay/owned-kernel-dispatch acceptance remains pending the
//! coordinator's genuine private-origin baseline, to extend this SAME ONE test.
use crate::protected_pi_profile::authority::{
    Authorized, GraphFile, ImageProfile, Role, Selection, GUEST_UID, SUPERVISOR_UID,
};
use sha2::{Digest, Sha256};

#[test]
fn owned_launch_scope_rejects_syntax_valid_caller_substitution() {
    let alias = "scope-guard";
    let selection = Selection {
        alias_sha256: Sha256::digest(alias.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        generation: "a".repeat(32),
        role: Role::Worker,
        model: "guard-provider/guard-model".into(),
    };
    let expected = Authorized {
        version: 1,
        selection: selection.clone(),
        alias: alias.into(),
        operation: "b".repeat(32),
        supervisor: SUPERVISOR_UID,
        guest: GUEST_UID,
        guest_gid: GUEST_UID,
        shared_gid: 21002,
        image: ImageProfile {
            helper_sha256: [1; 32],
            node_sha256: [2; 32],
            cli: "pi/cli.js".into(),
            extensions: vec!["extensions/platform.js".into()],
            files: vec![
                GraphFile {
                    path: "extensions/platform.js".into(),
                    size: 1,
                    mode: 0o644,
                    sha256: [3; 32],
                },
                GraphFile {
                    path: "node".into(),
                    size: 1,
                    mode: 0o755,
                    sha256: [2; 32],
                },
                GraphFile {
                    path: "pi/cli.js".into(),
                    size: 1,
                    mode: 0o644,
                    sha256: [4; 32],
                },
            ],
        },
    };
    expected.validate(&selection).unwrap();
    // This pure comparison succeeds for matching scope. It does not bypass
    // OwnerProfile::from_constructor or create any authenticated permit.
    super::require_binding(&expected, &selection, None).unwrap();
    super::require_binding(&expected, &selection, Some(&expected.operation)).unwrap();

    let mut wrong_alias = selection.clone();
    wrong_alias.alias_sha256 = "c".repeat(64);
    let mut wrong_generation = selection.clone();
    wrong_generation.generation = "c".repeat(32);
    let mut wrong_role = selection.clone();
    wrong_role.role = Role::Master;
    let mut wrong_model = selection.clone();
    wrong_model.model = "guard-provider/other-model".into();
    let wrong_operation = "c".repeat(32);
    for (case, requested, operation) in [
        ("alias", wrong_alias, None),
        ("generation", wrong_generation, None),
        ("role", wrong_role, None),
        ("model", wrong_model, None),
        (
            "operation",
            selection.clone(),
            Some(wrong_operation.as_str()),
        ),
    ] {
        // Every adversarial selection is accepted by the actual finite grammar.
        // Refusal must therefore come from REAL retained-scope comparison, not
        // an early UID, cold endpoint or syntax guard giving a false PASS.
        requested.validate().unwrap();
        match super::require_binding(&expected, &requested, operation) {
            Err(crate::Error::Rejected(message)) => assert_eq!(
                message, "protected Pi owner scope/current/provenance is UNKNOWN or refused",
                "{case} did not reach the actual scope refusal"
            ),
            _ => panic!("caller substituted {case} across the production scope guard"),
        }
    }
}
