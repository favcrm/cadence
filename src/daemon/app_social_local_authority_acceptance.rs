//! Bounded CAD-1177 acceptance for the closed local social-draft method boundary.
//!
//! This calls the production method classifier directly. It does not prove
//! full RPC admission, operator/session/mount/binding proofs, CAS behavior,
//! custody, or installed-board/HTTP acceptance.

#[test]
fn local_social_draft_classifier_is_an_exact_closed_allowlist() {
    // These methods do not require draft-effect permission.
    for method in [
        "app_social_draft_list",
        "app_social_draft_show",
        "app_social_sources_show",
    ] {
        assert!(
            !super::local_social_draft_write(method).unwrap(),
            "{method} must remain a local read"
        );
    }
    // `app_social_draft_asset` only reads bytes, but the original RPC
    // classifies it as a write for draft-effect slot authorization. Preserve
    // that permission boundary; this boolean is not an IO-mutability claim.
    for method in [
        "app_social_draft_create",
        "app_social_draft_update",
        "app_social_draft_asset",
        "app_social_sources_save",
    ] {
        assert!(
            super::local_social_draft_write(method).unwrap(),
            "{method} must remain a local metadata write"
        );
    }

    // Effect/provider/generation operations must not enter the local branch.
    for method in [
        "app_effect_stage",
        "app_social_effect_stage",
        "app_tool_invoke",
        "app_tool_asset",
        "app_social_generation_start",
        "app_social_generate",
        "app_social_draft_create_extra",
        "unknown_method",
    ] {
        assert!(
            super::local_social_draft_write(method).is_err(),
            "{method} must be refused by the closed local-method classifier"
        );
    }
}
