//! Independent acceptance: the public signed member-history relay must not
//! forward a genuine daemon page whose scoped acknowledgement or opaque
//! position has been lost/corrupted. The fault switch runs only after the
//! real daemon's member proof, live authority and history selection.

use super::*;
use std::sync::atomic::{AtomicU8, Ordering};

const FIRST: &str = "CAD2215_HISTORY_HTTP_FIRST";
const SECOND: &str = "CAD2215_HISTORY_HTTP_SECOND";

fn assert_opaque_page(page: &Value, expected: &str) -> (String, String, String) {
    assert!(
        page.get("_reader_scope").is_none(),
        "daemon ack reached browser: {page}"
    );
    let conversation = page["conversation"]
        .as_object()
        .expect("conversation object");
    assert_eq!(
        conversation.len(),
        2,
        "extra conversation metadata reached browser: {page}"
    );
    for field in ["id", "alias"] {
        assert!(
            conversation
                .get(field)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty()),
            "missing conversation {field}: {page}"
        );
    }
    let rows = page["entries"].as_array().expect("history entries");
    assert_eq!(rows.len(), 1, "expected one selected real row: {page}");
    let row = rows[0].as_object().expect("history row object");
    assert_eq!(row.get("text"), Some(&json!(expected)), "wrong row: {page}");
    assert!(
        !row.contains_key("seq"),
        "global position reached browser: {page}"
    );
    let id = row
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.parse::<i64>().is_err())
        .expect("stable opaque row id")
        .to_string();
    let cursor = page["cursor"]
        .as_str()
        .filter(|token| !token.is_empty() && token.parse::<i64>().is_err())
        .expect("opaque forward cursor")
        .to_string();
    let before = page["before_cursor"]
        .as_str()
        .filter(|token| !token.is_empty() && token.parse::<i64>().is_err())
        .expect("opaque backward cursor")
        .to_string();
    (id, cursor, before)
}

#[test]
fn signed_history_http_refuses_unacknowledged_or_numeric_daemon_pages() {
    let _flag = AssistantFlagOn::arm();
    let fault = Arc::new(AtomicU8::new(0));
    let rig = start_with_history_fault(&["ana"], None, true, Some(fault.clone()));
    let _keys = initialize_queue_keyring(&rig);

    // The real signed member-state call provisions the actual assistant and
    // its bound conversation via the daemon's live-authority guard.
    let state_envelope = rig.envelope("ana", json!({}), json!({}));
    let (status, state) = rig.call("assist_state", &state_envelope, &json!({}));
    assert_eq!(status, 200, "real member runtime setup refused: {state}");
    let thread = state["conversation_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .expect("member conversation id")
        .to_string();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    for text in [FIRST, SECOND] {
        shared
            .store
            .thread_append_to(
                &thread,
                crate::store::NewEntry {
                    role: "agent",
                    kind: "assistant_text",
                    text,
                    payload: None,
                    message_id: None,
                },
            )
            .unwrap()
            .expect("real member thread fixture row");
    }

    // Every request gets a freshly signed envelope with its own JTI.
    let read = |args: Value| {
        let envelope = rig.envelope("ana", json!({}), json!({}));
        rig.call("assist_history", &envelope, &args)
    };
    let (status, first) = read(json!({"limit":1}));
    assert_eq!(status, 200, "genuine acknowledged page refused: {first}");
    let (first_id, forward, _) = assert_opaque_page(&first, FIRST);
    let (status, same) = read(json!({"limit":1}));
    assert_eq!(status, 200, "second genuine page refused: {same}");
    assert_eq!(
        assert_opaque_page(&same, FIRST).0,
        first_id,
        "row id was not stable"
    );

    let (status, next) = read(json!({"after":forward, "limit":1}));
    assert_eq!(status, 200, "opaque forward continuation refused: {next}");
    let (_, _, before_second) = assert_opaque_page(&next, SECOND);
    let (status, older) = read(json!({"before":before_second, "limit":1}));
    assert_eq!(status, 200, "opaque backward continuation refused: {older}");
    assert_eq!(assert_opaque_page(&older, FIRST).0, first_id);

    // Each fault changes the actual, already-selected genuine daemon page.
    // Mode 2 leaves the real ack on the page but corrupts cursor to numeric.
    for (mode, label) in [
        (1, "missing ack"),
        (2, "numeric cursor"),
        (3, "wrong ack"),
        (4, "extra conversation updated metadata"),
    ] {
        fault.store(mode, Ordering::Release);
        let (status, body) = read(json!({"limit":1}));
        assert!(
            !(200..300).contains(&status),
            "{label} was relayed as success: {body}"
        );
        assert!(
            !body.to_string().contains(FIRST),
            "{label} leaked selected row: {body}"
        );
        assert!(
            body.get("entries").is_none(),
            "{label} returned a page: {body}"
        );
        assert!(
            body.get("cursor").is_none(),
            "{label} returned a continuation: {body}"
        );
    }
    fault.store(0, Ordering::Release);
    let (status, restored) = read(json!({"limit":1}));
    assert_eq!(status, 200, "restored genuine page refused: {restored}");
    assert_eq!(assert_opaque_page(&restored, FIRST).0, first_id);
}
