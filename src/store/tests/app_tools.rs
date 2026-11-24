use super::*;
use crate::store::app_tools::{AppToolClaim, AppToolRecord};

static WORKSPACE: std::sync::LazyLock<crate::issue::app_catalog::workspace::RuntimeSnapshot<'static>> =
    std::sync::LazyLock::new(crate::issue::app_catalog::workspace::RuntimeSnapshot::store_fixture);

fn fresh() -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_for_schema_tests(&dir.path().join("t.sqlite3")).unwrap();
    (dir, store)
}

fn claim<'a>(request: &'a str) -> AppToolClaim<'a> {
    AppToolClaim {
        workspace: &WORKSPACE,
        context: None,
        request,
        install: "install1",
        alias: "instagram.read",
        slot: "source",
        binding_digest: "sha256:bind",
        input_digest: "sha256:in",
        call_id: "app-tool-call-1",
    }
}

#[test]
fn cad1177_tool_claim_reserves_one_intent_and_refuses_recycled_id() {
    let (_d, store) = fresh();
    store.app_tool_claim(claim("req-1")).unwrap();
    // A byte-identical repeat (the uncertain-transport retry) is admitted.
    store.app_tool_claim(claim("req-1")).unwrap();
    // Any difference — install, alias, binding or input — refuses so a
    // recycled id can never run a second or changed operation.
    for mutate in [
        AppToolClaim {
            install: "install2",
            ..claim("req-1")
        },
        AppToolClaim {
            alias: "image.generate",
            ..claim("req-1")
        },
        AppToolClaim {
            binding_digest: "sha256:other",
            ..claim("req-1")
        },
        AppToolClaim {
            input_digest: "sha256:other",
            ..claim("req-1")
        },
        AppToolClaim {
            call_id: "app-tool-other",
            ..claim("req-1")
        },
    ] {
        assert!(store.app_tool_claim(mutate).is_err());
    }
    // A different request id is a new intent and claims independently.
    store.app_tool_claim(claim("req-2")).unwrap();
}

#[test]
fn cad1177_tool_record_retains_one_receipt_and_replays_it() {
    let (_d, store) = fresh();
    // A result with no pre-call claim refuses — claim must precede I/O.
    let input = json!({"profile_handle": "juicysuite_crm"});
    let result = json!({"kind": "social.source.posts", "posts": []});
    fn mk<'a>(request: &'a str, input: &'a Value, result: &'a Value) -> AppToolRecord<'a> {
        AppToolRecord {
            id: "app-tool-call-1",
            request,
            install: "install1",
            alias: "instagram.read",
            slot: "source",
            binding_digest: "sha256:bind",
            input_digest: "sha256:in",
            input,
            result,
            asset: None,
        }
    }
    assert!(store.app_tool_record(mk("req-1", &input, &result)).is_err());
    // Claim then record.
    store.app_tool_claim(claim("req-1")).unwrap();
    let receipt = store.app_tool_record(mk("req-1", &input, &result)).unwrap();
    assert_eq!(receipt["request_id"], "req-1");
    assert_eq!(receipt["install_id"], "install1");
    assert_eq!(receipt["alias"], "instagram.read");
    // A second record for the same request returns the SAME receipt
    // (idempotent replay), not a new row.
    let replay = store.app_tool_record(mk("req-1", &input, &result)).unwrap();
    assert_eq!(replay["id"], receipt["id"]);
    // Listing by install returns it.
    let listed = store.app_tool_results("install1").unwrap();
    assert_eq!(listed["results"].as_array().unwrap().len(), 1);
    assert!(store.app_tool_results("other").unwrap()["results"]
        .as_array()
        .unwrap()
        .is_empty());
    // For-request lookup finds it; a tampered digest on disk refuses.
    assert!(store
        .app_tool_result_for_request("req-1")
        .unwrap()
        .is_some());
    assert!(store
        .app_tool_result_for_request("absent")
        .unwrap()
        .is_none());
}

#[test]
fn soc54_source_reads_are_selected_before_the_history_bound() {
    let (_d, store) = fresh();
    let input = json!({});
    let source = json!({"kind": "social.source.posts", "handle": "brandhk", "posts": []});
    let upload = json!({"kind": "social.upload.image"});
    // Owned so the &str fields in the records outlive the calls.
    // The record id IS the claimed call id; the claim must precede the row.
    let rows: Vec<(String, String)> = (0..3)
        .map(|i| (format!("req-src-{i}"), format!("tool-src-{i}")))
        .chain((0..5).map(|i| (format!("req-up-{i}"), format!("zz-up-{i}"))))
        .collect();
    for (i, (request, id)) in rows.iter().enumerate() {
        let is_source = i < 3;
        store
            .app_tool_claim(AppToolClaim {
                workspace: &WORKSPACE,
                context: None,
                request,
                install: "install1",
                alias: if is_source {
                    "instagram.read"
                } else {
                    "social.image"
                },
                slot: if is_source { "source" } else { "image" },
                binding_digest: "sha256:bind",
                input_digest: "sha256:in",
                call_id: id,
            })
            .unwrap();
        store
            .app_tool_record(AppToolRecord {
                id,
                request,
                install: "install1",
                alias: if is_source {
                    "instagram.read"
                } else {
                    "social.image"
                },
                slot: if is_source { "source" } else { "image" },
                binding_digest: "sha256:bind",
                input_digest: "sha256:in",
                input: &input,
                result: if is_source { &source } else { &upload },
                asset: None,
            })
            .unwrap();
    }
    // Five newer non-source receipts sit ahead of the three source reads. A
    // bound applied before the kind filter would return nothing here; the
    // filter runs first, so the newest two source reads come back.
    let all: Vec<String> = rows.iter().map(|(_, id)| id.clone()).collect();
    let ids = |rows: Vec<Value>| -> Vec<String> {
        rows.iter()
            .map(|row| row["id"].as_str().unwrap().to_owned())
            .collect()
    };
    // Stage two at PRODUCTION shape: 500 candidates, LIMIT 100. The id list
    // must drive an index probe; an install-first plan would cost history for a
    // sparse candidate set.
    let wide: Vec<String> = (0..500).map(|i| format!("cand-{i}")).collect();
    let placeholders = vec!["?"; wide.len()].join(",");
    let sql = format!(
        "EXPLAIN QUERY PLAN SELECT id FROM app_tool_results
          WHERE id IN ({placeholders})
            AND json_extract(result,'$.kind')='social.source.posts'
          ORDER BY created DESC, id DESC LIMIT ?"
    );
    let mut binds: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(wide.len() + 1);
    for id in &wide {
        binds.push(id);
    }
    let limit = 100i64;
    binds.push(&limit);
    let plan: Vec<String> = store
        .conn()
        .prepare(&sql)
        .unwrap()
        .query_map(binds.as_slice(), |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|line| line.contains("USING INDEX sqlite_autoindex_app_tool_results_1")),
        "the id probe uses the primary key at production cardinality: {plan:?}"
    );
    assert!(
        !plan
            .iter()
            .any(|line| line.contains("app_tool_results_install")),
        "the plan must not prefer the installation index: {plan:?}"
    );
    assert_eq!(
        ids(store
            .app_tool_source_results_for("install1", &all, 2)
            .unwrap()),
        vec!["tool-src-2", "tool-src-1"]
    );
    // The bound is the caller's window: passing only the oldest candidate
    // still returns it, and an empty candidate set costs no query at all.
    assert_eq!(
        ids(store
            .app_tool_source_results_for("install1", &all[0..1], 2)
            .unwrap()),
        vec!["tool-src-0"]
    );
    assert!(store
        .app_tool_source_results_for("install1", &[], 2)
        .unwrap()
        .is_empty());
    assert!(store
        .app_tool_source_results_for("other", &all, 2)
        .unwrap()
        .is_empty());
}

#[test]
fn cad1177_tool_receipt_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.sqlite3");
    let store = Store::open_for_schema_tests(&path).unwrap();
    store.app_tool_claim(claim("req-keep")).unwrap();
    store
        .app_tool_record(AppToolRecord {
            id: "app-tool-call-1",
            request: "req-keep",
            install: "install1",
            alias: "instagram.read",
            slot: "source",
            binding_digest: "sha256:bind",
            input_digest: "sha256:in",
            input: &json!({}),
            result: &json!({"posts": [{"id": "p1"}]}),
            asset: None,
        })
        .unwrap();
    drop(store);
    let reopened = Store::open_for_schema_tests(&path).unwrap();
    let receipt = reopened
        .app_tool_result_for_request("req-keep")
        .unwrap()
        .unwrap();
    assert_eq!(receipt["result"]["posts"][0]["id"], "p1");
}

#[test]
fn uploaded_image_receipt_retains_a_bounded_asset() {
    let (_d, store) = fresh();
    let bytes = b"not-a-real-png-but-the-store-does-not-sniff";
    let result = json!({
        "kind": "social.uploaded_image",
        "media_type": "image/png",
        "width": 800,
        "height": 1000,
        "name": "photo.png",
        "digest": "sha256:test",
    });
    let receipt = store
        .app_tool_upload_record(crate::store::app_tools::AppToolUpload {
            id: "up-receipt",
            request: "request-upload",
            install: "install1",
            alias: "social.draft",
            slot: "image",
            result: &result,
            media_type: "image/png",
            bytes,
        })
        .unwrap();
    assert_eq!(receipt["result"]["kind"], "social.uploaded_image");
    assert_eq!(receipt["asset"]["media_type"], "image/png");
    assert_eq!(receipt["install_id"], "install1");
    let (header, digest, stored) = store
        .app_tool_asset_bytes("install1", "up-receipt")
        .unwrap();
    assert_eq!(header, "image/png");
    assert_eq!(stored, bytes);
    assert_eq!(digest, crate::store::app_runs::artifact_digest(bytes));
    // Another install cannot read this receipt or its bytes.
    assert!(store.app_tool_result("up-receipt").is_ok());
    assert!(store
        .app_tool_asset_bytes("install2", "up-receipt")
        .is_err());
    assert!(store
        .app_tool_upload_record(crate::store::app_tools::AppToolUpload {
            id: "up-empty",
            request: "request-empty",
            install: "install1",
            alias: "social.draft",
            slot: "image",
            result: &result,
            media_type: "image/png",
            bytes: b"",
        })
        .is_err());
}

#[test]
fn paid_image_result_requires_the_persisted_active_job_and_claim_after_reopen() {
    use crate::store::app_records::RecordStore;
    use crate::store::app_social_drafts::ImageSettlement;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("core.sqlite3");
    let store = Store::open_for_schema_tests(&path).unwrap();
    let records = RecordStore::open(dir.path(), "install1").unwrap();
    let input = json!({"prompt": "a blue vase"});
    let plan = json!({"prompt": "a blue vase", "size": "square"});
    let input_digest = crate::store::app_runs::material_digest(&input);
    let config =
        json!({"schema": 1, "install_id": "install1", "context": null, "provider": "image"});
    let binding = crate::store::app_bindings::config_digest("install1", None, "image", &config);
    let spec = json!({
        "alias": "image.generate", "slot": "image",
        "binding_digest": binding, "input_digest": input_digest,
        "input": input,
        "authority": {
            "install_id": "install1", "context_id": "context1", "slot": "image",
            "binding": {"digest": binding, "config": config}, "inputs": plan,
            "call_id": "call-1", "invocation_id": "call-1"
        }
    });
    records
        .app_social_generation_begin(
            "context1",
            "intent-1",
            "image.generate",
            "image",
            &input_digest,
            &plan,
            &json!({"operation": "image"}),
            "request-1",
        )
        .unwrap();
    let claim = store
        .app_tool_claim_receipt(AppToolClaim {
            request: "request-1",
            install: "install1",
            alias: "image.generate",
            slot: "image",
            binding_digest: &binding,
            input_digest: &input_digest,
            call_id: "call-1",
        })
        .unwrap();
    records
        .app_social_image_job_start(
            "context1",
            "intent-1",
            "request-1",
            "call-1",
            &spec,
            &claim,
            9999999999.0,
        )
        .unwrap();
    drop(records);
    drop(store);

    // There is deliberately no live installation in core: the persisted
    // pre-call evidence is enough to finish after a restart/removal.
    let store = Store::open_for_schema_tests(&path).unwrap();
    let records = RecordStore::open(dir.path(), "install1").unwrap();
    let authority = records
        .app_social_image_job_authority("call-1")
        .unwrap()
        .unwrap();
    let alien = RecordStore::open(dir.path(), "install2").unwrap();
    assert!(alien
        .app_social_image_record_result(&store, &authority, &json!({"kind": "image"}), None,)
        .is_err());
    records
        .app_social_image_job_settle_authorized(
            &authority,
            ImageSettlement::Uncertain {
                reason: "uncertain",
            },
        )
        .unwrap();
    assert!(records
        .app_social_image_job_resume("call-1", 9999999999.0)
        .unwrap());
    assert!(records
        .app_social_image_record_result(&store, &authority, &json!({"kind": "image"}), None,)
        .is_err());
    let authority = records
        .app_social_image_job_authority("call-1")
        .unwrap()
        .unwrap();
    let receipt = records
        .app_social_image_record_result(&store, &authority, &json!({"kind": "image"}), None)
        .unwrap();
    assert_eq!(receipt["id"], "call-1");
    // Crash after retention, before settlement: replay reads the same
    // checked receipt without a provider call.
    drop(records);
    drop(store);
    let store = Store::open_for_schema_tests(&path).unwrap();
    let records = RecordStore::open(dir.path(), "install1").unwrap();
    let authority = records
        .app_social_image_job_authority("call-1")
        .unwrap()
        .unwrap();
    let replay = records
        .app_social_image_result_for_job(&store, &authority)
        .unwrap()
        .unwrap();
    assert_eq!(replay["id"], "call-1");
    records
        .app_social_image_complete(&store, &authority)
        .unwrap();
    assert!(records
        .app_social_tool_receipt_contains("context1", "call-1")
        .unwrap());
    assert!(records
        .app_social_image_record_result(&store, &authority, &json!({"kind": "other"}), None,)
        .is_err());
    assert!(records
        .app_social_image_result_for_job(&store, &authority)
        .is_err());
}

#[test]
fn paid_image_job_refuses_alien_frozen_identity_and_mismatched_claims() {
    use crate::store::app_records::RecordStore;

    for field in [
        "install",
        "context",
        "call",
        "binding",
        "config",
        "input",
        "plan",
        "claim",
        "claim_request",
        "claim_call",
        "claim_install",
        "claim_input",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_for_schema_tests(&dir.path().join("core.sqlite3")).unwrap();
        let records = RecordStore::open(dir.path(), "install1").unwrap();
        let input = json!({"prompt": "vase"});
        let plan = json!({"subject": "vase"});
        let digest = crate::store::app_runs::material_digest(&input);
        let config =
            json!({"schema": 1, "install_id": "install1", "context": null, "provider": "image"});
        let binding = crate::store::app_bindings::config_digest("install1", None, "image", &config);
        let mut spec = json!({
            "alias": "image.generate", "slot": "image",
            "binding_digest": binding, "input_digest": digest, "input": input,
            "authority": {
                "install_id": "install1", "context_id": "context1", "slot": "image",
                "binding": {"digest": binding, "config": config}, "inputs": plan,
                "call_id": "call-1", "invocation_id": "call-1"
            }
        });
        match field {
            "install" => spec["authority"]["install_id"] = json!("install2"),
            "context" => spec["authority"]["context_id"] = json!("context2"),
            "call" => spec["authority"]["call_id"] = json!("call-2"),
            "binding" => spec["authority"]["binding"]["digest"] = json!("binding-2"),
            "config" => spec["authority"]["binding"]["config"]["provider"] = json!("other"),
            "input" => spec["input"] = json!({"prompt": "other"}),
            "plan" => spec["authority"]["inputs"] = json!({"subject": "other"}),
            _ => {}
        }
        records
            .app_social_generation_begin(
                "context1",
                "intent-1",
                "image.generate",
                "image",
                &digest,
                &plan,
                &json!({"operation": "image"}),
                "request-1",
            )
            .unwrap();
        let claim = store
            .app_tool_claim_receipt(AppToolClaim {
                request: if field == "claim_request" {
                    "request-2"
                } else {
                    "request-1"
                },
                install: if field == "claim_install" {
                    "install2"
                } else {
                    "install1"
                },
                alias: "image.generate",
                slot: "image",
                binding_digest: if field == "claim" {
                    "binding-2"
                } else {
                    &binding
                },
                input_digest: if field == "claim_input" {
                    "other"
                } else {
                    &digest
                },
                call_id: if field == "claim_call" {
                    "call-2"
                } else {
                    "call-1"
                },
            })
            .unwrap();
        assert!(
            records
                .app_social_image_job_start(
                    "context1",
                    "intent-1",
                    "request-1",
                    "call-1",
                    &spec,
                    &claim,
                    9999999999.0,
                )
                .is_err(),
            "{field}"
        );
        assert!(records
            .app_social_image_job_authority("call-1")
            .unwrap()
            .is_none());
    }
}

#[test]
fn paid_tool_claim_receipt_requires_a_persisted_claim_and_exact_input() {
    let (_dir, store) = fresh();
    let input = json!({"prompt": "vase"});
    let digest = crate::store::app_runs::material_digest(&input);
    let claim = AppToolClaim {
        request: "request-1",
        install: "install1",
        alias: "image.generate",
        slot: "image",
        binding_digest: "binding-1",
        input_digest: &digest,
        call_id: "call-1",
    };
    let authority = store.app_tool_claim_receipt(claim).unwrap();
    assert!(store
        .app_tool_claim_receipt(AppToolClaim {
            request: "request-2",
            ..claim
        })
        .is_err());
    assert!(authority
        .record(&store, &json!({"prompt": "different"}), &json!({}), None)
        .is_err());
    let receipt = authority
        .record(&store, &input, &json!({"kind": "image"}), None)
        .unwrap();
    assert_eq!(receipt["id"], "call-1");
    assert_eq!(
        authority
            .record(&store, &input, &json!({"kind": "image"}), None)
            .unwrap()["id"],
        "call-1"
    );
}
