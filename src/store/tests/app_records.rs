use super::super::app_contexts::ContextConfig;
use super::super::app_records::CustomerProfile;
use super::*;

fn record_store() -> (TempDir, Store) {
    let dir = TempDir::new().unwrap();
    let store = Store::open(&dir.path().join("t.sqlite3")).unwrap();
    (dir, store)
}

fn seeded_context(store: &Store, install: &str, request: &str) -> String {
    let config = ContextConfig::new("Client", std::collections::BTreeMap::new()).unwrap();
    let row = store.app_context_create(install, &config, request).unwrap();
    row["context"]["id"].as_str().unwrap().to_string()
}

fn customer(name: &str, email: &str) -> CustomerProfile {
    CustomerProfile::parse(&json!({"schema": 1, "display_name": name, "email": email, "tags": [], "consent": {"email": "granted"}})).unwrap()
}

#[test]
fn cad753_record_cas_digest_and_history_at_store_level() {
    let (_dir, store) = record_store();
    let context = seeded_context(&store, "install-1", "ctx-1");
    let created = store
        .app_record_create(
            "install-1",
            &context,
            "customer-1",
            &customer("Amina", "amina@example.com"),
        )
        .unwrap();
    assert_eq!(created["record"]["revision"], 1);
    let digest = created["record"]["digest"].as_str().unwrap().to_string();
    assert!(digest.starts_with("sha256:"));
    assert_eq!(created["record"]["history"].as_array().unwrap().len(), 1);

    // The same record ID under another installation or context is a
    // different row, not a collision.
    let other_context = seeded_context(&store, "install-2", "ctx-2");
    store
        .app_record_create(
            "install-2",
            &other_context,
            "customer-1",
            &customer("Boris", "boris@example.com"),
        )
        .unwrap();
    assert_eq!(
        store
            .app_record_show("install-2", &other_context, "customer-1")
            .unwrap()["record"]["profile"]["display_name"],
        "Boris"
    );
    assert!(
        store
            .app_record_show("install-2", &other_context, "customer-9")
            .is_err()
            && store
                .app_record_show("install-1", &other_context, "customer-1")
                .is_err()
            && store
                .app_record_show("install-2", &context, "customer-1")
                .is_err(),
        "cross-install or cross-context read reached a record"
    );

    // Stale CAS refuses and leaves revision, digest and history alone.
    assert!(store
        .app_record_update(
            "install-1",
            &context,
            "customer-1",
            9,
            &customer("Stale", "stale@example.com")
        )
        .is_err());
    let kept = store
        .app_record_show("install-1", &context, "customer-1")
        .unwrap();
    assert_eq!(kept["record"], created["record"]);

    let updated = store
        .app_record_update(
            "install-1",
            &context,
            "customer-1",
            1,
            &customer("Amina B", "amina@example.com"),
        )
        .unwrap();
    assert_eq!(updated["record"]["revision"], 2);
    assert_ne!(updated["record"]["digest"].as_str().unwrap(), digest);
    assert_eq!(updated["record"]["history"].as_array().unwrap().len(), 2);
    assert_eq!(updated["record"]["history"][1]["actor"], "operator");

    // Duplicate create with the same body is idempotent; a different
    // body under the same ID is refused, never merged.
    let repeat = store
        .app_record_create(
            "install-1",
            &context,
            "customer-1",
            &customer("Amina B", "amina@example.com"),
        )
        .unwrap();
    assert_eq!(repeat["record"]["revision"], 2);
    assert!(store
        .app_record_create(
            "install-1",
            &context,
            "customer-1",
            &customer("Other", "other@example.com")
        )
        .is_err());
}

#[test]
fn cad753_record_profile_validation_never_echoes_content() {
    let (_dir, store) = record_store();
    let context = seeded_context(&store, "install-1", "ctx-1");
    let marker = "cad753-store-private-marker";
    for body in [
        json!({"schema": 1, "display_name": "", "consent": {"email": "granted"}}),
        json!({"schema": 1, "display_name": marker, "email": "not-an-email", "consent": {"email": "granted"}}),
        json!({"schema": 1, "display_name": marker, "consent": {"email": "maybe"}}),
        json!({"schema": 1, "display_name": marker, "tags": ["ok", "no spaces allowed!!"], "consent": {"email": "granted"}}),
        json!({"schema": 1, "display_name": marker, "consent": {"email": "granted"}, "raw_sql": "SELECT 1"}),
        json!({"display_name": marker, "consent": {"email": "granted"}}),
    ] {
        // The refusal is a unit struct by construction, so it cannot
        // echo the marker; assert both the refusal and the silence.
        let error = CustomerProfile::parse(&body).unwrap_err();
        assert!(
            !error.to_string().contains(marker),
            "profile content leaked in refusal"
        );
    }
    // Records require a live context in the same installation.
    assert!(store
        .app_record_create(
            "install-1",
            "ctx-no-such",
            "customer-1",
            &customer("A", "a@example.com")
        )
        .is_err());
    assert!(store
        .app_record_create(
            "install-9",
            &context,
            "customer-1",
            &customer("A", "a@example.com")
        )
        .is_err());
}

#[test]
fn cad753_record_migration_is_atomic_and_preserves_existing_state() {
    let (dir, store) = record_store();
    let context = seeded_context(&store, "install-1", "ctx-1");
    let created = store
        .app_record_create(
            "install-1",
            &context,
            "customer-1",
            &customer("Amina", "amina@example.com"),
        )
        .unwrap();
    let before_contexts = store.app_context_list("install-1").unwrap();
    let db = dir.path().join("t.sqlite3");
    drop(store);
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "DROP TABLE app_record_revisions; DROP TABLE app_records;
         UPDATE schema_version SET version=27;
         CREATE TRIGGER reject_record_schema BEFORE UPDATE ON schema_version WHEN NEW.version=28 BEGIN SELECT RAISE(ABORT,'migration denied'); END;",
    )
    .unwrap();
    assert!(Store::open_for_schema_tests(&db).is_err());
    assert_eq!(
        conn.query_row("SELECT version FROM schema_version", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        27
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name IN ('app_records','app_record_revisions')",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    conn.execute_batch("DROP TRIGGER reject_record_schema;")
        .unwrap();
    drop(conn);
    let migrated = Store::open_for_schema_tests(&db).unwrap();
    assert_eq!(
        migrated.app_context_list("install-1").unwrap(),
        before_contexts
    );
    // Fresh tables accept new records; the pre-migration rows were
    // installation data under the old schema contract and are not
    // resurrected as different rows.
    let fresh = seeded_context(&migrated, "install-9", "ctx-9");
    let recreated = migrated
        .app_record_create(
            "install-9",
            &fresh,
            "customer-1",
            &customer("Amina", "amina@example.com"),
        )
        .unwrap();
    assert_eq!(recreated["record"]["revision"], 1);
    assert_eq!(
        migrated
            .app_record_show("install-9", &fresh, "customer-1")
            .unwrap()["record"],
        recreated["record"]
    );
    drop(migrated);
    let reopened = Store::open(&db).unwrap();
    assert_eq!(
        reopened
            .app_record_show("install-9", &fresh, "customer-1")
            .unwrap()["record"],
        recreated["record"]
    );
    drop(created);
}
