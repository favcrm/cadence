use crate::store::{
    app_records::RecordStore,
    app_social_drafts::{
        source_cursor_decode, source_cursor_encode, DraftSource, EffectStage, GenerationStart,
        SocialDraftEdit, SourceCursor,
    },
};
use rusqlite::params;
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
    store
        .app_social_tool_receipt_attach("ctx-one", "receipt-new", 20.0, "social.source.posts")
        .unwrap();
    store
        .app_social_tool_receipt_attach("ctx-one", "receipt-old", 10.0, "social.source.posts")
        .unwrap();
    store
        .app_social_tool_receipt_attach("ctx-two", "receipt-other", 15.0, "social.source.posts")
        .unwrap();
    // SOC-54: membership is per context and install — the screen library
    // resolves source reads host-side and keeps only what this context holds.
    assert!(store
        .app_social_tool_receipt_contains("ctx-one", "receipt-old")
        .unwrap());
    assert!(store
        .app_social_tool_receipt_contains("ctx-one", "receipt-new")
        .unwrap());
    assert!(!store
        .app_social_tool_receipt_contains("ctx-two", "receipt-new")
        .unwrap());
    assert!(store
        .app_social_tool_receipt_contains("ctx-two", "receipt-other")
        .unwrap());
    // The freshness tier has its own ordered index: the fallback must not sort
    // a context's whole historical freshness inventory either.
    let fresh_plan: Vec<String> = store
        .conn()
        .prepare(
            "EXPLAIN QUERY PLAN SELECT receipt_id FROM app_social_freshness
              WHERE install_id=? AND context_id=? ORDER BY completed_at DESC, handle LIMIT ?",
        )
        .unwrap()
        .query_map(params!["install-social", "ctx-one", 500i64], |r| {
            r.get::<_, String>(3)
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        fresh_plan
            .iter()
            .any(|line| line.contains("USING INDEX app_social_freshness_source_order")),
        "the freshness fallback walks its index: {fresh_plan:?}"
    );
    assert!(
        !fresh_plan.iter().any(|line| line.contains("TEMP B-TREE")),
        "the freshness fallback needs no sort: {fresh_plan:?}"
    );
    // The plan, not just the index: the candidate query must walk this order.
    let plan: Vec<String> = store
        .conn()
        .prepare(
            "EXPLAIN QUERY PLAN SELECT receipt_id FROM app_social_tool_receipts
              WHERE install_id=? AND context_id=? AND kind='social.source.posts'
              ORDER BY completed_at DESC, receipt_id DESC LIMIT ?",
        )
        .unwrap()
        .query_map(params!["install-social", "ctx-one", 500i64], |r| {
            r.get::<_, String>(3)
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    // The kind equality keeps the ordered walk in the covering index: the
    // window is read in completed_at order, never sorted and never scanned.
    assert!(
        plan.iter()
            .any(|line| line.contains("USING COVERING INDEX app_social_tool_receipts_source_order")),
        "the candidate window is read through its kind index: {plan:?}"
    );
    assert!(
        !plan.iter().any(|line| line
            .trim_start()
            .starts_with("SCAN app_social_tool_receipts")),
        "no full scan of the context's attachments: {plan:?}"
    );
    assert!(
        !plan.iter().any(|line| line.contains("TEMP B-TREE")),
        "the ordered walk needs no sort: {plan:?}"
    );
    // The candidate query orders by completed_at; the schema must carry the
    // index that makes that order a walk rather than a sort of the context's
    // whole history, including on a database opened before this index existed.
    assert!(
        store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='app_social_tool_receipts_source_order'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap()
            == 1
    );
    // SOC-54: an upload cannot evict a source read from the candidate window.
    // Three newer uploads and one older source, window 3 — the source must
    // still be a candidate, because the window selects on kind.
    store
        .app_social_tool_receipt_attach("ctx-one", "up-1", 31.0, "social.uploaded_image")
        .unwrap();
    store
        .app_social_tool_receipt_attach("ctx-one", "up-2", 32.0, "social.uploaded_image")
        .unwrap();
    store
        .app_social_tool_receipt_attach("ctx-one", "up-3", 33.0, "social.uploaded_image")
        .unwrap();
    assert_eq!(
        store.app_social_tool_receipt_ids("ctx-one", 2).unwrap(),
        vec!["receipt-new", "receipt-old"]
    );
    // The candidate window the screen read is bounded by: this context's own
    // attachments, newest first, and never another context's.
    // Typed attachments first, then the saved handles' newest read.
    assert_eq!(
        store.app_social_tool_receipt_ids("ctx-one", 500).unwrap(),
        vec!["receipt-new", "receipt-old", "receipt-other"]
    );
    assert_eq!(
        store.app_social_tool_receipt_ids("ctx-two", 500).unwrap(),
        vec!["receipt-other"]
    );
}

#[test]
fn soc54_receipt_kind_migrates_an_existing_database() {
    let root = tempfile::tempdir().unwrap();
    // A store from before the kind column and its index.
    let store = RecordStore::open(root.path(), "install-old").unwrap();
    store
        .app_social_tool_receipt_attach("ctx-one", "legacy-source", 2.0, "social.source.posts")
        .unwrap();
    store
        .app_social_sources_save("ctx-one", 0, &["brandhk".to_owned()], "src-old")
        .unwrap();
    store
        .app_social_freshness_save("ctx-one", "brandhk", "legacy-source", 2.0)
        .unwrap();
    store
        .conn()
        .execute_batch(
            "DROP INDEX app_social_tool_receipts_source_order;
             ALTER TABLE app_social_tool_receipts DROP COLUMN kind;",
        )
        .unwrap();
    drop(store);

    // Reopening must add the column and only then the index — the order that
    // failed when the index was created inside the shared DDL batch.
    let reopened = RecordStore::open(root.path(), "install-old").unwrap();
    assert_eq!(
        reopened
            .app_social_tool_receipt_ids("ctx-one", 500)
            .unwrap(),
        vec!["legacy-source"]
    );
    reopened
        .app_social_tool_receipt_attach("ctx-one", "new-source", 3.0, "social.source.posts")
        .unwrap();
    assert_eq!(
        reopened
            .app_social_tool_receipt_ids("ctx-one", 500)
            .unwrap(),
        vec!["new-source", "legacy-source"]
    );
    // Typed rows come first; unknown legacy rows take only the remaining room,
    // so an upgraded install keeps older retained reads behind its typed ones.
    reopened
        .app_social_tool_receipt_attach("ctx-one", "legacy-upload", 1.0, "")
        .unwrap();
    // Even when unknown rows are newer than the initialised receipt, the
    // newest read keeps its place: window 2 is the typed read + freshness.
    reopened
        .app_social_tool_receipt_attach("ctx-one", "legacy-newer", 9.0, "")
        .unwrap();
    assert_eq!(
        reopened.app_social_tool_receipt_ids("ctx-one", 2).unwrap(),
        vec!["new-source", "legacy-source"]
    );
    assert_eq!(
        reopened.app_social_tool_receipt_ids("ctx-one", 5).unwrap(),
        vec![
            "new-source",
            "legacy-source",
            "legacy-newer",
            "legacy-upload"
        ]
    );
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

/// CAD-1303: a soft discard hides the draft, keeps the record, and refuses
/// stale revisions and any draft that has an approved, sending or posted effect.
#[test]
fn social_draft_discard_is_soft_revision_checked_and_refused_while_in_use() {
    let root = tempfile::tempdir().unwrap();
    let store = RecordStore::open(root.path(), "install-discard").unwrap();
    let source = DraftSource::ToolReceipt {
        receipt_id: "receipt-1".into(),
        post_id: Some("post-9".into()),
    };
    let make = |request: &str| {
        store
            .app_social_draft_create("ctx", "A caption", &source, None, request, "session:test")
            .unwrap()["draft_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let stage = |draft: &str, effect: &str| {
        let frozen = json!({"draft_id": draft, "revision": 1});
        store
            .app_social_effect_stage(
                "ctx",
                &EffectStage {
                    draft,
                    revision: 1,
                    request: effect,
                    effect_id: effect,
                    frozen: &frozen,
                    approval: "approval",
                },
            )
            .unwrap()["effect"]["digest"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    // Plain discard: hidden from list/show/update, record and revisions kept.
    let plain = make("r-plain");
    let err = store
        .app_social_draft_discard("ctx", &plain, 2)
        .unwrap_err();
    assert_eq!(err.code(), Some("stale_revision"));
    assert_eq!(err.revision(), Some(1));
    assert_eq!(
        store.app_social_draft_discard("ctx", &plain, 1).unwrap(),
        json!({"draft_id": plain, "state": "discarded", "revision": 1})
    );
    assert!(store.app_social_draft_list("ctx").unwrap()["drafts"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(store.app_social_draft_show("ctx", &plain).is_err());
    assert!(store.app_social_draft_revision("ctx", &plain, 1).is_ok());
    let again = store
        .app_social_draft_discard("ctx", &plain, 1)
        .unwrap_err();
    assert_eq!(again.code(), Some("draft_not_found"));
    let unknown = store
        .app_social_draft_discard("ctx", "sdr-none", 1)
        .unwrap_err();
    assert_eq!(unknown.code(), Some("draft_not_found"));

    // A waiting effect does not block, and is declined so it cannot be approved later.
    let waiting = make("r-waiting");
    let digest = stage(&waiting, "eff-waiting");
    assert!(
        store.app_social_effect_list_page("ctx", 0).unwrap()["effects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|effect| effect["effect_id"] == "eff-waiting")
    );
    assert!(
        store.app_social_effect_list_page("ctx", 100).unwrap()["effects"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        store.app_social_effect_list_install_page(100).unwrap()["effects"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    store.app_social_draft_discard("ctx", &waiting, 1).unwrap();
    assert!(store
        .app_social_effect_decide("eff-waiting", &digest, true)
        .is_err());
    assert!(store.app_social_effect_list("ctx").unwrap()["effects"]
        .as_array()
        .unwrap()
        .is_empty());

    // Approved, sending and posted effects each refuse the discard.
    let used = make("r-used");
    let digest = stage(&used, "eff-used");
    store
        .app_social_effect_decide("eff-used", &digest, true)
        .unwrap();
    for step in ["approved", "sending", "posted"] {
        match step {
            "sending" => {
                store
                    .app_social_effect_claim_send("eff-used", &digest)
                    .unwrap();
            }
            "posted" => {
                store
                    .app_social_effect_finish("eff-used", "posted", &json!({}))
                    .unwrap();
            }
            _ => {}
        }
        let refused = store.app_social_draft_discard("ctx", &used, 1).unwrap_err();
        assert_eq!(refused.code(), Some("draft_in_use"), "{step}");
        assert!(store.app_social_draft_show("ctx", &used).is_ok(), "{step}");
    }
}

#[test]
fn manual_source_persists_without_receipts() {
    let root = tempfile::tempdir().unwrap();
    let store = RecordStore::open(root.path(), "install-social").unwrap();
    let created = store
        .app_social_draft_create(
            "ctx-one",
            "A post written from scratch",
            &DraftSource::Manual,
            None,
            "request-manual",
            "session:test",
        )
        .unwrap();
    assert_eq!(created["source"]["kind"], "manual");
    assert!(created["source"].get("receipt_id").is_none());
    let id = created["draft_id"].as_str().unwrap().to_owned();
    drop(store);
    let reload = RecordStore::open(root.path(), "install-social").unwrap();
    let shown = reload.app_social_draft_show("ctx-one", &id).unwrap();
    assert_eq!(shown["source"]["kind"], "manual");
    assert_eq!(shown["caption"], "A post written from scratch");
}

#[test]
fn uploaded_receipt_is_context_scoped_and_image_updates_are_revision_checked() {
    let root = tempfile::tempdir().unwrap();
    let store = RecordStore::open(root.path(), "install-social").unwrap();
    store
        .app_social_tool_receipt_attach("ctx-one", "up-image", 1.0, "social.uploaded_image")
        .unwrap();
    assert!(store
        .app_social_tool_receipt_contains("ctx-one", "up-image")
        .unwrap());
    assert!(!store
        .app_social_tool_receipt_contains("ctx-two", "up-image")
        .unwrap());
    let created = store
        .app_social_draft_create(
            "ctx-one",
            "A manual post",
            &DraftSource::Manual,
            None,
            "request-manual-image",
            "session:test",
        )
        .unwrap();
    let id = created["draft_id"].as_str().unwrap().to_owned();
    let with_image = store
        .app_social_draft_update(
            "ctx-one",
            &id,
            &SocialDraftEdit {
                expected: created["revision"].as_i64().unwrap(),
                caption: "A manual post",
                asset_id: Some(Some("up-image")),
                request_id: "request-attach-image",
                actor: "session:test",
            },
        )
        .unwrap();
    assert_eq!(with_image["asset_id"], "up-image");
    assert_eq!(with_image["source"]["kind"], "manual");
    let stale = store.app_social_draft_update(
        "ctx-one",
        &id,
        &SocialDraftEdit {
            expected: created["revision"].as_i64().unwrap(),
            caption: "A manual post",
            asset_id: Some(Some("up-image")),
            request_id: "request-stale-image",
            actor: "session:test",
        },
    );
    assert!(stale.is_err());
    let removed = store
        .app_social_draft_update(
            "ctx-one",
            &id,
            &SocialDraftEdit {
                expected: with_image["revision"].as_i64().unwrap(),
                caption: "A manual post",
                asset_id: Some(None),
                request_id: "request-remove-image",
                actor: "session:test",
            },
        )
        .unwrap();
    assert!(removed["asset_id"].is_null());
    assert_eq!(removed["source"]["kind"], "manual");
}

/// SOC-55: the page cursor is opaque, bounded and bound to the host's
/// verified scope. A cursor from another install/context, a malformed one or
/// one with an out-of-bounds position is refused, never applied.
#[test]
fn soc55_source_cursor_is_bounded_and_scope_bound() {
    let cursor = SourceCursor {
        revision: 7,
        completed_at: 1_790_467_200.25,
        receipt_id: "app-tool-0123abcd".into(),
        post: 23,
        run_fence: Some(42),
    };
    let raw = source_cursor_encode("install-a", "ctx-a", &cursor);
    assert!(raw.len() <= 560, "cursor is bounded: {}", raw.len());
    assert_eq!(
        source_cursor_decode("install-a", "ctx-a", &raw).unwrap(),
        cursor
    );
    let bare = SourceCursor {
        run_fence: None,
        ..cursor.clone()
    };
    assert_eq!(
        source_cursor_decode(
            "install-a",
            "ctx-a",
            &source_cursor_encode("install-a", "ctx-a", &bare)
        )
        .unwrap(),
        bare
    );
    for (install, context) in [("install-a", "ctx-b"), ("install-b", "ctx-a")] {
        assert!(
            source_cursor_decode(install, context, &raw).is_err(),
            "a cursor minted for another scope must not advance this one"
        );
    }
    for bad in [
        String::new(),
        "not-base64!".to_string(),
        "a".repeat(400),
        "e30".to_string(),
    ] {
        assert!(source_cursor_decode("install-a", "ctx-a", &bad).is_err());
    }
    use base64::Engine;
    let encode =
        |text: String| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text.as_bytes());
    let text = String::from_utf8(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(raw.as_bytes())
            .unwrap(),
    )
    .unwrap();
    let parts: Vec<&str> = text.split(':').collect();
    assert_eq!(parts.len(), 7);
    let tampered = |version: &str, at: &str, receipt: &str, post: &str| {
        encode(format!(
            "{version}:{}:{}:{at}:{receipt}:{post}:{}",
            parts[1], parts[2], parts[6]
        ))
    };
    let rejected = |cursor: String| {
        assert!(
            source_cursor_decode("install-a", "ctx-a", &cursor).is_err(),
            "cursor {cursor:?} must be refused"
        );
    };
    rejected(tampered("v2", parts[2], parts[3], parts[4]));
    rejected(tampered("v1", "nothex", parts[3], parts[4]));
    rejected(tampered("v1", "7ff8000000000000", parts[3], parts[4]));
    rejected(tampered("v1", "fff0000000000000", parts[3], parts[4]));
    rejected(tampered("v1", parts[2], "bad id", parts[4]));
    rejected(tampered("v1", parts[2], parts[3], "257"));
    rejected(tampered("v1", parts[2], parts[3], "-1"));
}

/// SOC-55: paging reaches past a hundred retained reads with a keyset cursor,
/// orders equal timestamps deterministically, and a concurrent newer receipt
/// never shifts or skips an older row the scan had not reached yet.
#[test]
fn soc55_source_pages_reach_past_one_hundred_and_keep_equal_timestamps() {
    let root = tempfile::tempdir().unwrap();
    let store = RecordStore::open(root.path(), "install-page").unwrap();
    for index in 0..130u32 {
        // Pairs share a completion time, so the tie-break must be the id.
        let at = 1_000.0 + f64::from(index / 2);
        store
            .app_social_tool_receipt_attach(
                "ctx-page",
                &format!("app-tool-{index:04}"),
                at,
                "social.source.posts",
            )
            .unwrap();
    }
    // A newer non-source attachment can never consume the scan.
    store
        .app_social_tool_receipt_attach("ctx-page", "up-newest", 9_999.0, "social.uploaded_image")
        .unwrap();
    let mut seen: Vec<String> = Vec::new();
    let mut after: Option<(f64, String)> = None;
    let mut pages = 0;
    loop {
        let page = store
            .app_social_source_candidates(
                "ctx-page",
                after.as_ref().map(|(at, id)| (*at, id.as_str())),
                25,
            )
            .unwrap();
        pages += 1;
        let Some(last) = page.last() else { break };
        assert!(
            page.windows(2).all(|pair| {
                pair[0].completed_at > pair[1].completed_at
                    || (pair[0].completed_at == pair[1].completed_at
                        && pair[0].receipt_id > pair[1].receipt_id)
            }),
            "page is newest-first with an id tie-break: {page:?}"
        );
        seen.extend(page.iter().map(|row| row.receipt_id.clone()));
        after = Some((last.completed_at, last.receipt_id.clone()));
        if pages == 1 {
            // A concurrent fetch lands at the head; the rest of the walk must
            // still reach every older retained read exactly once.
            store
                .app_social_tool_receipt_attach(
                    "ctx-page",
                    "app-tool-newest",
                    20_000.0,
                    "social.source.posts",
                )
                .unwrap();
        }
        if page.len() < 25 {
            break;
        }
    }
    assert_eq!(pages, 6, "130 reads page in six bounded reads");
    assert_eq!(seen.len(), 130, "every retained read is reached: {seen:?}");
    assert_eq!(seen[0], "app-tool-0129");
    assert_eq!(seen[129], "app-tool-0000");
    assert!(
        !seen.contains(&"app-tool-newest".to_owned()),
        "a receipt newer than the cursor is not replayed mid-scan"
    );
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), seen.len(), "no retained read is repeated");
    let fresh = store
        .app_social_source_candidates("ctx-page", None, 2)
        .unwrap();
    assert_eq!(
        fresh
            .iter()
            .map(|row| row.receipt_id.as_str())
            .collect::<Vec<_>>(),
        vec!["app-tool-newest", "app-tool-0129"],
        "a restarted scan sees the concurrent read first"
    );
}

/// SOC-55: a legacy source read that never reached the attachment table is
/// still this context's attributable receipt, joins the page exactly once, and
/// survives its handle leaving Settings.
#[test]
fn soc55_legacy_freshness_reads_join_the_page_once() {
    let root = tempfile::tempdir().unwrap();
    let store = RecordStore::open(root.path(), "install-fresh").unwrap();
    store
        .app_social_sources_save(
            "ctx-fresh",
            0,
            &["brandhk".to_owned(), "legacyhk".to_owned()],
            "src-fresh",
        )
        .unwrap();
    store
        .app_social_sources_save("ctx-other", 0, &["brandhk".to_owned()], "src-other")
        .unwrap();
    store
        .app_social_tool_receipt_attach(
            "ctx-fresh",
            "app-tool-attached",
            20.0,
            "social.source.posts",
        )
        .unwrap();
    store
        .app_social_freshness_save("ctx-fresh", "brandhk", "app-tool-attached", 20.0)
        .unwrap();
    store
        .app_social_freshness_save("ctx-fresh", "legacyhk", "app-tool-legacy", 10.0)
        .unwrap();
    store
        .app_social_freshness_save("ctx-other", "brandhk", "app-tool-other", 30.0)
        .unwrap();
    // The handles leave Settings; the receipts stay attributable.
    store
        .app_social_sources_save("ctx-fresh", 1, &[], "src-clear")
        .unwrap();

    let attached = store
        .app_social_source_candidates("ctx-fresh", None, 10)
        .unwrap();
    assert_eq!(
        attached
            .iter()
            .map(|row| row.receipt_id.as_str())
            .collect::<Vec<_>>(),
        vec!["app-tool-attached"],
        "an attached read is served by the attachment arm only"
    );
    let legacy = store
        .app_social_freshness_candidates("ctx-fresh", None, 10)
        .unwrap();
    assert_eq!(
        legacy
            .iter()
            .map(|row| row.receipt_id.as_str())
            .collect::<Vec<_>>(),
        vec!["app-tool-legacy"],
        "a freshness-only read is not lost, and an attached one is not repeated"
    );
    let after = (20.0, "app-tool-attached".to_owned());
    let resumed = store
        .app_social_freshness_candidates("ctx-fresh", Some((after.0, after.1.as_str())), 10)
        .unwrap();
    assert_eq!(
        resumed
            .iter()
            .map(|row| row.receipt_id.as_str())
            .collect::<Vec<_>>(),
        vec!["app-tool-legacy"],
        "the keyset resumes the legacy arm"
    );
    assert!(store
        .app_social_freshness_contains("ctx-fresh", "app-tool-legacy")
        .unwrap());
    assert!(!store
        .app_social_freshness_contains("ctx-fresh", "app-tool-other")
        .unwrap());
    assert!(!store
        .app_social_freshness_contains("ctx-other", "app-tool-attached")
        .unwrap());
}

/// SOC-55: the snapshot fence moves for every candidate mutation — including
/// equal timestamps with lexicographically earlier/later ids and a backdated
/// insert — but not for an upload or a replayed attachment.
#[test]
fn soc55_source_revision_fences_every_candidate_mutation() {
    let root = tempfile::tempdir().unwrap();
    let store = RecordStore::open(root.path(), "install-fence").unwrap();
    let revision = |context: &str| store.app_social_source_revision(context).unwrap();
    assert_eq!(revision("ctx-fence"), 0);
    store
        .app_social_tool_receipt_attach("ctx-fence", "app-tool-a", 10.0, "social.source.posts")
        .unwrap();
    assert_eq!(revision("ctx-fence"), 1);
    // A replay of the same attachment is not a mutation.
    store
        .app_social_tool_receipt_attach("ctx-fence", "app-tool-a", 10.0, "social.source.posts")
        .unwrap();
    assert_eq!(revision("ctx-fence"), 1);
    // An upload is not a source candidate.
    store
        .app_social_tool_receipt_attach("ctx-fence", "up-a", 99.0, "social.uploaded_image")
        .unwrap();
    assert_eq!(revision("ctx-fence"), 1);
    // Equal timestamp, lexicographically earlier then later id.
    store
        .app_social_tool_receipt_attach("ctx-fence", "app-tool-0000", 10.0, "social.source.posts")
        .unwrap();
    assert_eq!(revision("ctx-fence"), 2);
    store
        .app_social_tool_receipt_attach("ctx-fence", "app-tool-zzzz", 10.0, "social.source.posts")
        .unwrap();
    assert_eq!(revision("ctx-fence"), 3);
    // A backdated insert.
    store
        .app_social_tool_receipt_attach("ctx-fence", "app-tool-back", 1.0, "")
        .unwrap();
    assert_eq!(revision("ctx-fence"), 4);
    // A freshness insert moves it; an ignored older freshness does not.
    store
        .app_social_sources_save("ctx-fence", 0, &["brandhk".to_owned()], "src-fence")
        .unwrap();
    store
        .app_social_freshness_save("ctx-fence", "brandhk", "app-tool-a", 10.0)
        .unwrap();
    assert_eq!(revision("ctx-fence"), 5);
    store
        .app_social_freshness_save("ctx-fence", "brandhk", "app-tool-older", 5.0)
        .unwrap();
    assert_eq!(revision("ctx-fence"), 5);
    // Another context has its own counter.
    assert_eq!(revision("ctx-other"), 0);
}

/// SOC-55: run-bound source receipts page in the same newest-first order and
/// keep the same post id attributable under each run's own receipt.
#[test]
fn soc55_run_source_receipts_page_with_their_run_ids() {
    let root = tempfile::tempdir().unwrap();
    let db_path = root.path().join("cadence.sqlite3");
    let store = crate::store::Store::open(&db_path).unwrap();
    // A fixture connection, not the store's authorized handle: these rows are
    // the run-bound receipts the page scans, and the product API only records
    // them through a full broker turn.
    let db = rusqlite::Connection::open(&db_path).unwrap();
    // app_runs.context_id references app_contexts(id); create the parent the
    // fixture runs belong to, exactly as a real mount would.
    db.execute(
        "INSERT INTO app_contexts(id,install_id,request_id,revision,state,config_json,config_digest,created,updated) VALUES(?,?,?,1,'active',?,?,?,?)",
        params![
            "ctx-run",
            "install-run",
            "req-ctx-run",
            json!({"schema":1,"label":"Run","input_defaults":{}}).to_string(),
            "d",
            0.0,
            0.0
        ],
    )
    .unwrap();
    let insert = |run: &str, receipt: &str, created: f64| {
        db
            .execute(
                "INSERT INTO app_runs(id,install_id,epoch,bundle_digest,snapshot,snapshot_digest,project_link,owner_pm,request_id,state,approved_digest,created,updated,context_id) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                params![run, "install-run", 0i64, "d", "{}", "d", Option::<String>::None, "lead", format!("req-{run}"), "succeeded", "d", created, created, "ctx-run"],
            )
            .unwrap();
        db
            .execute(
                "INSERT INTO app_capability_results(id,run_id,step_id,message_id,turn_id,slot,request_id,binding_digest,input_digest,result,result_digest,created,receipt_schema,install_id,context_id,source_post_read) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,2,?,?,1)",
                params![receipt, run, "s1", "m1", "t1", "source", format!("req-{run}"), "b", "i",
                    json!({"kind":"social.source.posts","handle":"brandhk","posts":[{"id":"post-1","caption":"Same","permalink":"https://www.instagram.com/p/AbCd123/","published_at_unix":1790467200,"media_kind":"image"}]}).to_string(),
                    "digest", created, "install-run", "ctx-run"],
            )
            .unwrap();
    };
    insert("run-old", "cap-run-old", 10.0);
    insert("run-new", "cap-run-new", 20.0);
    let fence = store.app_source_run_watermark().unwrap();
    let page = store
        .app_source_run_candidates("install-run", "ctx-run", None, fence, 10)
        .unwrap();
    assert_eq!(
        page.iter()
            .map(|(receipt, _, run)| (receipt.as_str(), run.as_str()))
            .collect::<Vec<_>>(),
        vec![("cap-run-new", "run-new"), ("cap-run-old", "run-old")],
        "run receipts page newest first with their own run ids"
    );
    let after = (page[0].1, page[0].0.as_str());
    let resumed = store
        .app_source_run_candidates("install-run", "ctx-run", Some(after), fence, 10)
        .unwrap();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].0, "cap-run-old");
    // Later inserts — equal timestamp with lexically smaller and larger ids,
    // plus a backdated row — are outside the scan's rowid fence.
    insert("run-aaa", "cap-run-aaa", 20.0);
    insert("run-zzz", "cap-run-zzz", 20.0);
    insert("run-back", "cap-run-back", 5.0);
    let frozen = store
        .app_source_run_candidates("install-run", "ctx-run", None, fence, 10)
        .unwrap();
    assert_eq!(
        frozen
            .iter()
            .map(|(receipt, _, _)| receipt.as_str())
            .collect::<Vec<_>>(),
        vec!["cap-run-new", "cap-run-old"],
        "the scan's rowid fence excludes every later insert, whatever its clock"
    );
    let moved = store.app_source_run_watermark().unwrap();
    assert!(moved > fence);
    let fresh = store
        .app_source_run_candidates("install-run", "ctx-run", None, moved, 10)
        .unwrap();
    assert_eq!(
        fresh
            .iter()
            .map(|(receipt, _, _)| receipt.as_str())
            .collect::<Vec<_>>(),
        vec![
            "cap-run-zzz",
            "cap-run-new",
            "cap-run-aaa",
            "cap-run-old",
            "cap-run-back"
        ],
        "a restarted scan sees them with the id tie-break"
    );
    // Another context never sees them, and a receipt outside this context is
    // not provable as a source.
    assert!(store
        .app_source_run_candidates("install-run", "ctx-other", None, moved, 10)
        .unwrap()
        .is_empty());
    assert!(store
        .app_source_run_receipt("install-run", "ctx-other", "cap-run-new")
        .is_none());
}

/// SOC-55: the read resolver proves the native host executor's receipt
/// provenance too — empty message/turn, terminal approved run, exact
/// dispatched step and matching immutable claim — and still rejects a broker
/// receipt through the host proof.
#[test]
fn soc55_host_executed_run_receipt_is_recoverable_for_reads() {
    let root = tempfile::tempdir().unwrap();
    let db_path = root.path().join("cadence.sqlite3");
    let store = crate::store::Store::open(&db_path).unwrap();
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute(
        "INSERT INTO app_contexts(id,install_id,request_id,revision,state,config_json,config_digest,created,updated) VALUES(?,?,?,1,'active',?,?,?,?)",
        params!["ctx-host", "install-host", "req-ctx-host", json!({"schema":1,"label":"Host","input_defaults":{}}).to_string(), "d", 0.0, 0.0],
    ).unwrap();
    db.execute("INSERT INTO jobs(id,spec_path,pm_alias,state,created,updated) VALUES('job-host','spec','lead','done',0,0)", []).unwrap();
    db.execute("INSERT INTO tasks(id,job_id,state,created,updated) VALUES('task-host','job-host','succeeded',0,0)", []).unwrap();
    db.execute(
        "INSERT INTO app_runs(id,install_id,epoch,bundle_digest,snapshot,snapshot_digest,project_link,owner_pm,request_id,state,approved_digest,created,updated,context_id) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params!["run-host", "install-host", 0i64, "d", "{}", "d", Option::<String>::None, "lead", "req-run-host", "succeeded", "d", 0.0, 0.0, "ctx-host"],
    ).unwrap();
    db.execute(
        "INSERT INTO app_run_steps(run_id,step_id,task_id,spec,identity_digest,state,message_id) VALUES('run-host','s1','task-host','spec','id','succeeded',NULL)",
        [],
    ).unwrap();
    db.execute(
        "INSERT INTO app_capability_claims(run_id,slot,step_id,request_id,binding_digest,input_digest,call_id,created) VALUES('run-host','source','s1','req','b','i','cap-host',0)",
        [],
    ).unwrap();
    db.execute(
        "INSERT INTO app_capability_results(id,run_id,step_id,message_id,turn_id,slot,request_id,binding_digest,input_digest,result,result_digest,created,receipt_schema) VALUES('cap-host','run-host','s1','','','source','req','b','i',?,'d',0,2)",
        [json!({"kind":"social.source.posts","handle":"brandhk","posts":[]}).to_string()],
    ).unwrap();
    let run =
        json!({"id":"run-host","state":"succeeded","approved_digest":"d","snapshot_digest":"d"});
    let host = json!({"id":"cap-host","run_id":"run-host","message_id":""});
    crate::store::app_capabilities::source_host_receipt_recoverable_in(&store.conn(), &run, &host)
        .expect("a host receipt is proven by its approved step and claim");
    let broker = json!({"id":"cap-host","run_id":"run-host","message_id":"m1"});
    assert!(
        crate::store::app_capabilities::source_host_receipt_recoverable_in(
            &store.conn(),
            &run,
            &broker
        )
        .is_err(),
        "the host proof does not accept a broker receipt"
    );
    db.execute(
        "DELETE FROM app_capability_claims WHERE call_id='cap-host'",
        [],
    )
    .unwrap();
    assert!(
        crate::store::app_capabilities::source_host_receipt_recoverable_in(
            &store.conn(),
            &run,
            &host
        )
        .is_err(),
        "a host receipt without its immutable claim is not provable"
    );
}
