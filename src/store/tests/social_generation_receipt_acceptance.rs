// Independent CAD-1177 acceptance owned by ef-social-pm-01a114fb.
// Implementers must not edit or weaken this check. This exercises the actual
// RecordStore validator used by app_social_draft_list, not a mirror matcher.
use crate::store::{app_records::RecordStore, app_runs::material_digest};
use serde_json::json;

#[test]
fn social_generation_receipt_acceptance_keeps_caller_and_frozen_plan_distinct() {
    let root = tempfile::tempdir().unwrap();
    let records = RecordStore::open(root.path(), "install-independent").unwrap();
    let caller = json!({});
    let caller_digest = material_digest(&caller);
    let plan = json!({"inputs":{"source":"Verified public food facts","subject":"Food","brand_voice":"Clear","image_prompt":"Editorial"},"source":{"receipt_id":"retained-source","post_digest":"retained-post-digest","post":{"id":"post-one","caption":"Verified public food facts","permalink":"https://www.instagram.com/p/example/"}}});
    let plan_digest = material_digest(&plan);
    assert_ne!(caller_digest, plan_digest);
    let scope =
        json!({"operation":"image","draft_id":"sdr-00000000000000000000000000000001","revision":1});
    records
        .app_social_generation_begin(
            "ctx-independent",
            "scoped-image-intent",
            "social.draft",
            "generate_image",
            &caller_digest,
            &plan,
            &scope,
            "request-independent",
        )
        .unwrap();
    let receipt = json!({"id":"retained-image","install_id":"install-independent","request_id":"request-independent","alias":"social.draft","input_digest":caller_digest,"input":caller});
    // A genuine caller hash need not equal the independently frozen plan hash.
    records
        .app_social_generation_validate_receipt("ctx-independent", "request-independent", &receipt)
        .unwrap();
    for (key, value) in [
        ("input_digest", json!(plan_digest)),
        (
            "input_digest",
            json!(material_digest(&json!({"source":"forged"}))),
        ),
        ("install_id", json!("install-foreign")),
        ("request_id", json!("request-foreign")),
        ("alias", json!("caption.generate")),
    ] {
        let mut forged = receipt.clone();
        forged[key] = value;
        assert!(
            records
                .app_social_generation_validate_receipt(
                    "ctx-independent",
                    "request-independent",
                    &forged
                )
                .is_err(),
            "forged {key} was accepted"
        );
    }
    assert!(records
        .app_social_generation_validate_receipt("ctx-foreign", "request-independent", &receipt)
        .is_err());
    assert!(records
        .app_social_generation_validate_receipt("ctx-independent", "request-foreign", &receipt)
        .is_err());
    // The persisted plan must also remain authentic; a matching caller
    // hash cannot conceal corrupted frozen factual material.
    records
        .conn()
        .execute(
            "UPDATE app_social_generation_intents SET input_json='{}' WHERE request_id=?",
            ["request-independent"],
        )
        .unwrap();
    assert!(records
        .app_social_generation_validate_receipt("ctx-independent", "request-independent", &receipt)
        .is_err());
}
