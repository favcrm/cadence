use super::*;
use crate::store::app_records::RecordStore;
use crate::store::app_works::WorksDraft;
use serde_json::json;
use tempfile::TempDir;

fn open() -> (TempDir, RecordStore) {
    let dir = TempDir::new().unwrap();
    let store = RecordStore::open(dir.path(), "install-a").unwrap();
    (dir, store)
}

fn draft(kind: &str, title: &str, content: Value) -> WorksDraft {
    WorksDraft::parse(&json!({
        "kind": kind,
        "title": title,
        "content": content,
        "tags": [],
    }))
    .unwrap()
}

fn doc(title: &str) -> WorksDraft {
    draft(
        "doc",
        title,
        json!({"blocks": [
            {"type": "h2", "text": title},
            {"type": "p", "text": "Body."},
        ]}),
    )
}

#[test]
fn cad1649_create_get_review_and_export() {
    let (_dir, store) = open();
    let created = store
        .app_works_create("ctx-1", &doc("Retention"), "alice", "create-1", None)
        .unwrap();
    assert_eq!(created["id"], "retention");
    assert_eq!(created["revision"], 1);
    assert_eq!(created["reviewState"], "draft");
    assert_eq!(created["exportState"], "blocked");
    let fetched = store.app_works_get("ctx-1", "retention").unwrap();
    assert_eq!(fetched["title"], "Retention");
    assert_eq!(fetched["contentDigest"], created["contentDigest"]);

    let requested = store
        .app_works_request_review("ctx-1", "retention", 1, "alice", None)
        .unwrap();
    assert_eq!(requested["reviewStatus"], "pending");
    assert!(store
        .app_works_review("ctx-1", "retention", 1, "approve", None, "alice", None)
        .is_err());
    let approved = store
        .app_works_review("ctx-1", "retention", 1, "approve", Some("ok"), "bob", None)
        .unwrap();
    assert_eq!(approved["reviewState"], "reviewed");
    assert_eq!(approved["exportState"], "ready");
    let receipt = store
        .app_works_export("ctx-1", "retention", 1, "html", "bob", None)
        .unwrap();
    assert_eq!(receipt["digest"], fetched["contentDigest"]);
}

#[test]
fn cad1649_cas_replay_changes_and_query() {
    let (_dir, store) = open();
    let first = store
        .app_works_create("ctx-1", &doc("Retention"), "alice", "create-1", None)
        .unwrap();
    let replay = store
        .app_works_create("ctx-1", &doc("Retention"), "alice", "create-1", None)
        .unwrap();
    assert_eq!(replay["id"], first["id"]);
    assert!(store
        .app_works_create("ctx-1", &doc("Different"), "alice", "create-1", None)
        .is_err());
    let patched = store
        .app_works_patch(
            "ctx-1",
            "retention",
            1,
            &[json!({"type": "set_title", "title": "Retention v2"})],
            "alice",
            "patch-1",
            None,
        )
        .unwrap();
    assert_eq!(patched["revision"], 2);
    let error = store
        .app_works_patch(
            "ctx-1",
            "retention",
            1,
            &[json!({"type": "set_title", "title": "Stale"})],
            "alice",
            "patch-2",
            None,
        )
        .unwrap_err();
    assert_eq!(error.code(), Some("conflict"));

    store
        .app_works_request_review("ctx-1", "retention", 2, "alice", None)
        .unwrap();
    let changed = store
        .app_works_review("ctx-1", "retention", 2, "changes", None, "bob", None)
        .unwrap();
    assert_eq!(changed["reviewStatus"], "changes_requested");
    assert_eq!(changed["exportState"], "blocked");
    let changes = store
        .app_works_query("ctx-1", &json!({"review": "changes_requested"}))
        .unwrap();
    assert_eq!(changes.as_array().unwrap().len(), 1);
    let pending = store
        .app_works_query("ctx-1", &json!({"review": "pending"}))
        .unwrap();
    assert!(pending.as_array().unwrap().is_empty());
    assert!(store
        .app_works_export("ctx-1", "retention", 2, "html", "bob", None)
        .is_err());
    store
        .app_works_request_review("ctx-1", "retention", 2, "alice", None)
        .unwrap();
    store
        .app_works_review("ctx-1", "retention", 2, "approve", None, "bob", None)
        .unwrap();
    let receipt = store
        .app_works_export("ctx-1", "retention", 2, "pdf", "bob", None)
        .unwrap();
    assert!(receipt["digest"].as_str().unwrap().starts_with("sha256:"));
    let error = store
        .app_works_export("ctx-1", "retention", 1, "html", "bob", None)
        .unwrap_err();
    assert_eq!(error.code(), Some("needs_review"));

    let all = store.app_works_query("ctx-1", &json!({})).unwrap();
    assert_eq!(all.as_array().unwrap().len(), 1);
    let docs = store
        .app_works_query("ctx-1", &json!({"kind": "doc", "text": "retention"}))
        .unwrap();
    assert_eq!(docs.as_array().unwrap().len(), 1);
    let decks = store
        .app_works_query("ctx-1", &json!({"kind": "deck"}))
        .unwrap();
    assert!(decks.as_array().unwrap().is_empty());
}

#[test]
fn cad1649_trash_restore_and_context_scope() {
    let (_dir, store) = open();
    let created = store
        .app_works_create("ctx-1", &doc("Retention"), "alice", "create-1", None)
        .unwrap();
    assert_eq!(created["revision"], 1);
    let trashed = store
        .app_works_trash("ctx-1", "retention", 1, "alice", "trash-1", None)
        .unwrap();
    assert_eq!(trashed["trashed"], true);
    let error = store.app_works_get("ctx-1", "retention").unwrap_err();
    assert_eq!(error.code(), Some("not_found"));
    let restored = store
        .app_works_restore("ctx-1", "retention", 1, "alice", "restore-1", None)
        .unwrap();
    assert_eq!(restored["revision"], 1);
    assert_eq!(
        store.app_works_get("ctx-1", "retention").unwrap()["revision"],
        1
    );
    assert!(store.app_works_get("ctx-2", "retention").is_err());
    assert!(store
        .app_works_query("ctx-2", &json!({}))
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn cad1649_invalid_content_refuses_without_write() {
    let (_dir, store) = open();
    assert!(WorksDraft::parse(&json!({
        "kind": "other",
        "title": "X",
        "content": {"blocks": [{"type": "h2", "text": "X"}]},
        "tags": [],
    }))
    .is_err());
    assert!(WorksDraft::parse(&json!({
        "kind": "doc",
        "title": "X",
        "content": {"blocks": [{"type": "bogus", "text": "X"}]},
        "tags": [],
    }))
    .is_err());
    assert!(store.app_works_get("ctx-1", "x").is_err());
}

#[test]
fn cad1649_review_requires_two_verified_principals() {
    let (_dir, store) = open();
    store
        .app_works_create("ctx-1", &doc("Retention"), "operator", "create-1", None)
        .unwrap();
    store
        .app_works_request_review("ctx-1", "retention", 1, "operator", None)
        .unwrap();
    let error = store
        .app_works_review("ctx-1", "retention", 1, "approve", None, "bob", None)
        .unwrap_err();
    assert_eq!(error.code(), Some("forbidden"));

    store
        .app_works_create("ctx-1", &doc("Other"), "alice", "create-2", None)
        .unwrap();
    store
        .app_works_request_review("ctx-1", "other", 1, "alice", None)
        .unwrap();
    let error = store
        .app_works_review("ctx-1", "other", 1, "approve", None, "operator", None)
        .unwrap_err();
    assert_eq!(error.code(), Some("forbidden"));
    store
        .app_works_review("ctx-1", "other", 1, "approve", None, "bob", None)
        .unwrap();
}

#[test]
fn cad1649_content_cannot_shadow_metadata() {
    let (_dir, store) = open();
    let forged = json!({
        "blocks": [{"type": "h2", "text": "Retention"}],
        "title": "forged",
        "author": "mallory",
        "reviewStatus": "approved",
        "reviewedRevision": 1,
        "exportState": "ready",
    });
    assert!(WorksDraft::parse(&json!({
        "kind": "doc",
        "title": "Retention",
        "content": forged.clone(),
        "tags": [],
    }))
    .is_err());
    assert!(WorksDraft::parse(&json!({
        "kind": "doc",
        "title": "Retention",
        "content": {"blocks": [{"type": "h2", "text": "Retention", "author": "mallory"}]},
        "tags": [],
    }))
    .is_err());
    store
        .app_works_create("ctx-1", &doc("Retention"), "alice", "create-1", None)
        .unwrap();
    assert!(store
        .app_works_patch(
            "ctx-1",
            "retention",
            1,
            &[json!({"type": "set_content", "content": forged})],
            "alice",
            "patch-1",
            None,
        )
        .is_err());
    let fetched = store.app_works_get("ctx-1", "retention").unwrap();
    assert_eq!(fetched["title"], "Retention");
    assert_eq!(fetched["author"], "alice");
    assert_eq!(fetched["reviewStatus"], "none");
    assert_eq!(fetched["exportState"], "blocked");
}

#[test]
fn cad1649_long_title_is_addressable() {
    let (_dir, store) = open();
    let title = "A".repeat(240);
    let created = store
        .app_works_create("ctx-1", &doc(&title), "alice", "create-1", None)
        .unwrap();
    let id = created["id"].as_str().unwrap();
    assert!(id.len() <= 48, "{id}");
    let fetched = store.app_works_get("ctx-1", id).unwrap();
    assert_eq!(fetched["title"], title);
}

#[test]
fn cad1649_audit_rows_are_written() {
    let (_dir, store) = open();
    store
        .app_works_create("ctx-1", &doc("Retention"), "alice", "create-1", None)
        .unwrap();
    store
        .app_works_patch(
            "ctx-1",
            "retention",
            1,
            &[json!({"type": "set_title", "title": "Retention v2"})],
            "alice",
            "patch-1",
            None,
        )
        .unwrap();
    let conn = store.conn();
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM app_works_audit WHERE context_id='ctx-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
    let actions: String = conn
        .query_row(
            "SELECT group_concat(action, ',') FROM app_works_audit WHERE context_id='ctx-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(actions.contains("create"));
    assert!(actions.contains("patch"));
}

// CAD-1662 audit transaction: a Works mutation whose audit row cannot be
// written must roll back entirely - no item/revision change and no
// idempotency replay row. Fail-injection is a SQLite trigger on the real
// audit table, so the production write path is unchanged.
#[test]
fn cad1662_audit_failure_rolls_back_item_revision_and_replay() {
    let (_dir, store) = open();
    store
        .app_works_create("ctx-1", &doc("Rollback"), "alice", "create-rb", None)
        .unwrap();
    store
        .conn()
        .execute_batch(
            "CREATE TRIGGER cad1662_fail_audit BEFORE INSERT ON app_works_audit
             BEGIN SELECT RAISE(ABORT, 'synthetic audit failure'); END;",
        )
        .unwrap();
    let before_rev = store.app_works_get("ctx-1", "rollback").unwrap()["revision"].clone();

    let patch = store.app_works_patch(
        "ctx-1",
        "rollback",
        1,
        &[json!({"type": "set_title", "title": "Rollback v2"})],
        "alice",
        "patch-rb",
        None,
    );
    assert!(patch.is_err(), "patch must fail when its audit row fails");

    let after = store.app_works_get("ctx-1", "rollback").unwrap();
    assert_eq!(after["revision"], before_rev, "revision must not advance");
    assert_eq!(after["title"], "Rollback", "title must not change");
    let replays: i64 = store
        .conn()
        .query_row(
            "SELECT count(*) FROM app_works_idempotency WHERE context_id='ctx-1' AND key='patch-rb'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(replays, 0, "no replay row may persist after audit failure");
}
