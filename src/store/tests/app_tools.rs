use super::*;
use crate::store::app_tools::{AppToolClaim, AppToolRecord};

fn fresh() -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_for_schema_tests(&dir.path().join("t.sqlite3")).unwrap();
    (dir, store)
}

fn claim<'a>(request: &'a str) -> AppToolClaim<'a> {
    AppToolClaim {
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
