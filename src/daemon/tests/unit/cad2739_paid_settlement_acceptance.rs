//! Independent acceptance for paid image settlement across a restart.
use crate::store::app_records::RecordStore;
use crate::store::app_social_drafts::ImageSettlement;
use crate::store::app_tools::AppToolClaim;
use crate::store::Store;
use serde_json::json;

#[test]
fn stale_job_cannot_retain_or_attach_and_reproved_job_settles_retained_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let core = dir.path().join("core.sqlite3");
    let store = Store::open_for_schema_tests(&core).unwrap();
    let records = RecordStore::open(dir.path(), "install-paid").unwrap();
    let input = json!({"prompt": "a red kite"});
    let plan = json!({"subject": "a red kite", "size": "square"});
    let input_digest = crate::store::app_runs::material_digest(&input);
    let config =
        json!({"schema": 1, "install_id": "install-paid", "context": null, "provider": "image"});
    let binding = crate::store::app_runs::material_digest(&json!({
        "kind": "app-binding-v1",
        "install_id": "install-paid",
        "context_id": null,
        "slot": "image",
        "config": config,
    }));
    let spec = json!({
        "alias": "image.generate", "slot": "image",
        "binding_digest": binding, "input_digest": input_digest, "input": input,
        "authority": {
            "install_id": "install-paid", "context_id": "context-paid", "slot": "image",
            "binding": {"digest": binding, "config": config}, "inputs": plan,
            "call_id": "call-paid", "invocation_id": "call-paid"
        }
    });
    records
        .app_social_generation_begin(
            "context-paid",
            "intent-paid",
            "image.generate",
            "image",
            &input_digest,
            &plan,
            &json!({"operation": "image"}),
            "request-paid",
        )
        .unwrap();
    let claim = store
        .app_tool_claim_receipt(AppToolClaim {
            request: "request-paid",
            install: "install-paid",
            alias: "image.generate",
            slot: "image",
            binding_digest: &binding,
            input_digest: &input_digest,
            call_id: "call-paid",
        })
        .unwrap();
    records
        .app_social_image_job_start(
            "context-paid",
            "intent-paid",
            "request-paid",
            "call-paid",
            &spec,
            &claim,
            9999999999.0,
        )
        .unwrap();
    let stale = records
        .app_social_image_job_authority("call-paid")
        .unwrap()
        .unwrap();
    let retained = records
        .app_social_image_record_result(
            &store,
            &stale,
            &json!({"kind": "image", "marker": "original"}),
            None,
        )
        .unwrap();
    assert_eq!(retained["id"], "call-paid");

    // An unresolved attempt is resumed under the same key but a new authority
    // epoch. The old worker must not retain, replay, or attach its result.
    records
        .app_social_image_job_settle_authorized(
            &stale,
            ImageSettlement::Uncertain {
                reason: "interrupted",
            },
        )
        .unwrap();
    assert!(records
        .app_social_image_job_resume("call-paid", 9999999999.0)
        .unwrap());
    assert!(records
        .app_social_image_record_result(
            &store,
            &stale,
            &json!({"kind": "image", "marker": "replacement"}),
            None,
        )
        .is_err());
    assert!(records
        .app_social_image_result_for_job(&store, &stale)
        .is_err());
    assert!(records.app_social_image_complete(&store, &stale).is_err());
    assert!(!records
        .app_social_tool_receipt_contains("context-paid", "call-paid")
        .unwrap());
    assert_eq!(
        records
            .app_social_image_job("call-paid")
            .unwrap()
            .unwrap()
            .state,
        "active"
    );
    assert_eq!(
        records
            .app_social_generation_intents("context-paid")
            .unwrap()[0]["state"],
        "pending"
    );

    // No installation or provider is created. Reopening both durable stores
    // models restart after receipt retention, before receipt attachment.
    drop(records);
    drop(store);
    let store = Store::open_for_schema_tests(&core).unwrap();
    let records = RecordStore::open(dir.path(), "install-paid").unwrap();
    let current = records
        .app_social_image_job_authority("call-paid")
        .unwrap()
        .unwrap();
    let replay = records
        .app_social_image_result_for_job(&store, &current)
        .unwrap()
        .unwrap();
    assert_eq!(replay["id"], "call-paid");
    assert_eq!(replay["result"]["marker"], "original");
    records.app_social_image_complete(&store, &current).unwrap();
    assert!(records
        .app_social_tool_receipt_contains("context-paid", "call-paid")
        .unwrap());
    assert_eq!(
        records
            .app_social_image_job("call-paid")
            .unwrap()
            .unwrap()
            .state,
        "succeeded"
    );
    let intents = records
        .app_social_generation_intents("context-paid")
        .unwrap();
    assert_eq!(intents[0]["state"], "completed");
    assert_eq!(intents[0]["receipt_id"], "call-paid");
}
