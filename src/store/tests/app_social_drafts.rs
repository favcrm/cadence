use crate::store::{
    app_records::RecordStore,
    app_social_drafts::{DraftSource, EffectStage, GenerationStart, SocialDraftEdit},
};
use serde_json::json;

#[test]
fn social_drafts_are_context_scoped_revisioned_and_idempotent() {
    let root = tempfile::tempdir().unwrap();
    let store = RecordStore::open(root.path(), "install-social").unwrap();
    let source = DraftSource::ToolReceipt {
        receipt_id: "receipt-1".into(),
        post_id: Some("post-9".into()),
    };
    let created = store
        .app_social_draft_create(
            "ctx-one",
            "A truthful caption",
            &source,
            None,
            "request-create",
            "session:test",
        )
        .unwrap();
    assert_eq!(created["revision"], 1);
    let id = created["draft_id"].as_str().unwrap().to_owned();
    assert_eq!(
        store
            .app_social_draft_create(
                "ctx-one",
                "A truthful caption",
                &source,
                None,
                "request-create",
                "session:test"
            )
            .unwrap()["draft_id"],
        id
    );
    assert!(store.app_social_draft_show("ctx-two", &id).is_err());
    let updated = store
        .app_social_draft_update(
            "ctx-one",
            &id,
            &SocialDraftEdit {
                expected: 1,
                caption: "An edited caption",
                asset_id: Some(None),
                request_id: "request-update",
                actor: "session:test",
            },
        )
        .unwrap();
    assert_eq!(updated["revision"], 2);
    assert!(store
        .app_social_draft_update(
            "ctx-one",
            &id,
            &SocialDraftEdit {
                expected: 1,
                caption: "Stale edit",
                asset_id: None,
                request_id: "request-stale",
                actor: "session:test",
            },
        )
        .is_err());
    drop(store);
    let reload = RecordStore::open(root.path(), "install-social").unwrap();
    assert_eq!(
        reload.app_social_draft_show("ctx-one", &id).unwrap()["caption"],
        "An edited caption"
    );
    assert_eq!(
        reload.app_social_draft_revision("ctx-one", &id, 1).unwrap()["source"]["post_id"],
        "post-9"
    );
    let listed = reload.app_social_draft_list("ctx-one").unwrap();
    assert_eq!(listed["drafts"].as_array().unwrap().len(), 1);
}

#[test]
fn social_source_profiles_normalize_and_compare_and_swap() {
    let root = tempfile::tempdir().unwrap();
    let store = RecordStore::open(root.path(), "install-social").unwrap();
    assert_eq!(
        store.app_social_sources_show("ctx-one").unwrap()["revision"],
        0
    );
    let saved = store
        .app_social_sources_save(
            "ctx-one",
            0,
            &["@Kiva.Wood".into(), "brill__".into()],
            "source-request",
        )
        .unwrap();
    assert_eq!(saved["handles"], json!(["kiva.wood", "brill__"]));
    assert!(store
        .app_social_sources_save("ctx-one", 0, &[], "stale-request")
        .is_err());
    assert!(store
        .app_social_sources_save(
            "ctx-one",
            1,
            &["same".into(), "@same".into()],
            "bad-duplicate"
        )
        .is_err());
    assert_eq!(
        store
            .app_social_sources_save(
                "ctx-one",
                0,
                &["Kiva.Wood".into(), "brill__".into()],
                "source-request"
            )
            .unwrap()["revision"],
        1
    );
    store
        .app_social_freshness_save("ctx-one", "kiva.wood", "receipt-old", 10.0)
        .unwrap();
    store
        .app_social_freshness_save("ctx-one", "kiva.wood", "receipt-new", 20.0)
        .unwrap();
    store
        .app_social_freshness_save("ctx-one", "brill__", "receipt-other", 15.0)
        .unwrap();
    store
        .app_social_freshness_save("ctx-one", "kiva.wood", "receipt-stale", 12.0)
        .unwrap();
    assert_eq!(
        store.app_social_freshness_receipts("ctx-one").unwrap(),
        vec!["receipt-new", "receipt-other"]
    );
    assert!(store
        .app_social_freshness_receipts("ctx-two")
        .unwrap()
        .is_empty());
}

#[test]
fn generation_intent_keeps_one_key_while_pending_but_allows_new_terminal_intent() {
    let root = tempfile::tempdir().unwrap();
    let store = RecordStore::open(root.path(), "install-social").unwrap();
    let scope = json!({"operation":"caption","draft_id":"draft-a","revision":2});
    let intent = "scoped-intent-digest";
    assert_eq!(
        store
            .app_social_generation_begin_with(
                "ctx-one",
                intent,
                &GenerationStart {
                    alias: "text.generate",
                    tool: "generate_text",
                    caller_input_digest: "input-a",
                    input: &json!({"prompt":"input-a"}),
                    scope: &scope,
                    request: "request-a",
                }
            )
            .unwrap()["mode"],
        "start"
    );
    let attached = store
        .app_social_generation_begin_with(
            "ctx-one",
            intent,
            &GenerationStart {
                alias: "text.generate",
                tool: "generate_text",
                caller_input_digest: "input-a",
                input: &json!({"prompt":"input-a"}),
                scope: &scope,
                request: "request-b",
            },
        )
        .unwrap();
    assert_eq!(attached["mode"], "existing");
    assert_eq!(attached["intent"]["request_id"], "request-a");
    assert!(store
        .app_social_generation_begin_with(
            "ctx-one",
            intent,
            &GenerationStart {
                alias: "text.generate",
                tool: "generate_text",
                caller_input_digest: "changed-input",
                input: &json!({"prompt":"changed-input"}),
                scope: &scope,
                request: "request-c",
            }
        )
        .is_err());
    assert_eq!(
        store
            .app_social_generation_begin_with(
                "ctx-one",
                intent,
                &GenerationStart {
                    alias: "text.generate",
                    tool: "generate_text",
                    caller_input_digest: "input-a",
                    input: &json!({"prompt":"input-a"}),
                    scope: &scope,
                    request: "request-a",
                }
            )
            .unwrap()["mode"],
        "retry"
    );
    store
        .app_social_generation_finish(
            "ctx-one",
            intent,
            "request-a",
            "pending",
            None,
            Some("upstream_idempotency_in_progress"),
        )
        .unwrap();
    assert_eq!(
        store
            .app_social_generation_begin_with(
                "ctx-one",
                intent,
                &GenerationStart {
                    alias: "text.generate",
                    tool: "generate_text",
                    caller_input_digest: "input-a",
                    input: &json!({"prompt":"input-a"}),
                    scope: &scope,
                    request: "request-a",
                }
            )
            .unwrap()["mode"],
        "retry"
    );
    store
        .app_social_generation_finish(
            "ctx-one",
            intent,
            "request-a",
            "completed",
            Some("tool-receipt"),
            None,
        )
        .unwrap();
    let settled = store.app_social_generation_intents("ctx-one").unwrap();
    assert_eq!(settled[0]["state"], "completed");
    assert_eq!(settled[0]["receipt_id"], "tool-receipt");
    assert_eq!(
        store
            .app_social_generation_begin_with(
                "ctx-one",
                intent,
                &GenerationStart {
                    alias: "text.generate",
                    tool: "generate_text",
                    caller_input_digest: "input-a",
                    input: &json!({"prompt":"input-a"}),
                    scope: &scope,
                    request: "request-next",
                }
            )
            .unwrap()["mode"],
        "start"
    );
    let pending = store.app_social_generation_intents("ctx-one").unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["input"]["prompt"], "input-a");
    assert_eq!(pending[0]["request_id"], "request-next");
}

#[test]
fn social_draft_effect_is_immutable_approved_once_and_uncertainty_never_reclaims() {
    let root = tempfile::tempdir().unwrap();
    let store = RecordStore::open(root.path(), "install-social").unwrap();
    let source = DraftSource::ToolReceipt {
        receipt_id: "receipt-1".into(),
        post_id: Some("p1".into()),
    };
    let draft = store
        .app_social_draft_create(
            "ctx-one",
            "Caption",
            &source,
            None,
            "create-1",
            "session:test",
        )
        .unwrap();
    let frozen = json!({"source":{"kind":"social_draft","draft_id":draft["draft_id"],"revision":1},"caption":"Caption","caption_digest":"digest"});
    let id = "sfx_696e7374616c6c2d736f6369616c_00000000000000000000000000000001";
    let staged = store
        .app_social_effect_stage(
            "ctx-one",
            &EffectStage {
                draft: draft["draft_id"].as_str().unwrap(),
                revision: 1,
                request: "effect-request",
                effect_id: id,
                frozen: &frozen,
                approval: "approval-1",
            },
        )
        .unwrap();
    assert_eq!(staged["effect"]["state"], "waiting");
    let digest = staged["effect"]["digest"].as_str().unwrap();
    let approved = store
        .app_social_effect_decide(id, digest, true)
        .unwrap()
        .unwrap();
    assert_eq!(approved["effect"]["state"], "approved");
    store.app_social_effect_media_key(id, None).unwrap();
    assert!(store
        .app_social_effect_claim_send(id, digest)
        .unwrap()
        .is_some());
    assert!(store
        .app_social_effect_claim_send(id, digest)
        .unwrap()
        .is_none());
    let posted = store
        .app_social_effect_finish(id, "posted", &json!({"verified":true}))
        .unwrap();
    assert_eq!(posted["effect"]["state"], "posted");
    assert!(store
        .app_social_effect_claim_send(id, digest)
        .unwrap()
        .is_none());
}

#[test]
fn uncertain_generation_intent_is_never_restarted_under_a_new_request_id() {
    let root = tempfile::tempdir().unwrap();
    let store = RecordStore::open(root.path(), "install-social").unwrap();
    let scope = json!({"operation":"caption","draft_id":"draft-a","revision":2});
    let begin = |request: &'static str| {
        store
            .app_social_generation_begin_with(
                "ctx-one",
                "scoped-intent-digest",
                &GenerationStart {
                    alias: "text.generate",
                    tool: "generate_text",
                    caller_input_digest: "input-a",
                    input: &json!({"prompt":"input-a"}),
                    scope: &scope,
                    request,
                },
            )
            .unwrap()
    };
    assert_eq!(begin("request-a")["mode"], "start");
    // The provider answered `uncertain`: the intent stays unresolved.
    store
        .app_social_generation_finish(
            "ctx-one",
            "scoped-intent-digest",
            "request-a",
            "uncertain",
            None,
            Some("upstream_outcome_uncertain"),
        )
        .unwrap();
    // A different request id for the same scope attaches; it never starts a
    // second dispatch (and so never a second provider hold).
    let attached = begin("request-b");
    assert_eq!(attached["mode"], "existing");
    assert_eq!(attached["intent"]["request_id"], "request-a");
    assert_eq!(attached["intent"]["state"], "uncertain");
}
