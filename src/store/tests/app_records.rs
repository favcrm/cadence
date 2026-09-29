use super::super::app_records::{
    record_db_path, CsvAction, CsvDecision, CustomerProfile, RecordStore,
};
use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

fn record_file(dir: &TempDir, install: &str) -> RecordStore {
    RecordStore::open(dir.path(), install).unwrap()
}

fn customer(name: &str, email: &str) -> CustomerProfile {
    CustomerProfile::parse(&json!({"schema": 1, "display_name": name, "email": email, "tags": [], "consent": {"email": "granted"}})).unwrap()
}

#[test]
fn cad753_record_files_are_physical_per_installation() {
    let dir = TempDir::new().unwrap();
    let a = record_file(&dir, "install-a");
    let b = record_file(&dir, "install-b");
    let path_a = record_db_path(dir.path(), "install-a").unwrap();
    let path_b = record_db_path(dir.path(), "install-b").unwrap();
    assert_ne!(path_a, path_b);
    assert!(path_a.is_file() && path_b.is_file());
    // Two installations are two inodes, not two views of one file.
    assert_ne!(
        std::fs::metadata(&path_a).unwrap().ino(),
        std::fs::metadata(&path_b).unwrap().ino()
    );

    a.app_record_create(
        "ctx-1",
        "customer-1",
        &customer("Amina", "amina@example.com"),
    )
    .unwrap();
    b.app_record_create(
        "ctx-1",
        "customer-1",
        &customer("Boris", "boris@example.com"),
    )
    .unwrap();
    assert_eq!(
        a.app_record_show("ctx-1", "customer-1").unwrap()["record"]["profile"]["display_name"],
        "Amina"
    );
    assert_eq!(
        b.app_record_show("ctx-1", "customer-1").unwrap()["record"]["profile"]["display_name"],
        "Boris"
    );
    // Context scoping lives inside each file: the same record ID in
    // another context of the same file is absent.
    assert!(
        a.app_record_show("ctx-2", "customer-1").is_err()
            && a.app_record_list("ctx-2").unwrap()["records"]
                .as_array()
                .unwrap()
                .is_empty()
    );

    // CAS, digest and attributed history, as before.
    let created = a.app_record_show("ctx-1", "customer-1").unwrap();
    assert!(a
        .app_record_update(
            "ctx-1",
            "customer-1",
            9,
            &customer("Stale", "stale@example.com")
        )
        .is_err());
    assert_eq!(
        a.app_record_show("ctx-1", "customer-1").unwrap()["record"],
        created["record"]
    );
    let updated = a
        .app_record_update(
            "ctx-1",
            "customer-1",
            1,
            &customer("Amina B", "amina@example.com"),
        )
        .unwrap();
    assert_eq!(updated["record"]["revision"], 2);
    assert_ne!(updated["record"]["digest"], created["record"]["digest"]);
    assert_eq!(updated["record"]["history"].as_array().unwrap().len(), 2);
    assert_eq!(updated["record"]["history"][1]["actor"], "operator");
    // The sibling file is untouched by the CAS above.
    assert_eq!(
        b.app_record_show("ctx-1", "customer-1").unwrap()["record"]["revision"],
        1
    );
    // Duplicate create with the same body is idempotent; a different
    // body under the same ID is refused, never merged.
    assert_eq!(
        a.app_record_create(
            "ctx-1",
            "customer-1",
            &customer("Amina B", "amina@example.com")
        )
        .unwrap()["record"]["revision"],
        2
    );
    assert!(a
        .app_record_create(
            "ctx-1",
            "customer-1",
            &customer("Other", "other@example.com")
        )
        .is_err());
}

#[test]
fn cad753_record_file_reopens_with_its_contents() {
    let dir = TempDir::new().unwrap();
    let created = record_file(&dir, "install-a")
        .app_record_create(
            "ctx-1",
            "customer-1",
            &customer("Amina", "amina@example.com"),
        )
        .unwrap();
    drop(created);
    // No daemon handle is held between these opens: persistence is the
    // file itself.
    let reopened = RecordStore::open(dir.path(), "install-a").unwrap();
    assert_eq!(
        reopened.app_record_show("ctx-1", "customer-1").unwrap()["record"]["revision"],
        1
    );
    reopened
        .app_record_update(
            "ctx-1",
            "customer-1",
            1,
            &customer("Amina B", "amina@example.com"),
        )
        .unwrap();
    drop(reopened);
    assert_eq!(
        RecordStore::open(dir.path(), "install-a")
            .unwrap()
            .app_record_show("ctx-1", "customer-1")
            .unwrap()["record"]["revision"],
        2
    );
}

#[test]
fn cad753_record_file_identity_and_corruption_refuse_without_deletion() {
    let dir = TempDir::new().unwrap();
    record_file(&dir, "install-a")
        .app_record_create(
            "ctx-1",
            "customer-1",
            &customer("Amina", "amina@example.com"),
        )
        .unwrap();
    let path = record_db_path(dir.path(), "install-a").unwrap();
    // A file planted at another installation's path — the same bytes
    // claimed by another name — is refused by its identity row, and
    // the planted bytes are preserved.
    let planted = record_db_path(dir.path(), "install-b").unwrap();
    std::fs::copy(&path, &planted).unwrap();
    let Err(error) = RecordStore::open(dir.path(), "install-b") else {
        panic!("planted identity accepted");
    };
    let error = error.to_string();
    assert!(
        error.contains("identity"),
        "identity mismatch unclear: {error}"
    );
    assert_eq!(
        std::fs::read(&planted).unwrap(),
        std::fs::read(&path).unwrap()
    );
    // Corrupt bytes refuse and are preserved, never healed in place.
    let victim = record_db_path(dir.path(), "install-victim").unwrap();
    RecordStore::open(dir.path(), "install-victim").unwrap();
    std::fs::write(&victim, b"not a database at all").unwrap();
    let Err(error) = RecordStore::open(dir.path(), "install-victim") else {
        panic!("corrupt file accepted");
    };
    let error = error.to_string();
    assert!(
        error.contains("corrupt") || error.contains("backup"),
        "corruption refusal unclear: {error}"
    );
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"not a database at all",
        "corrupt file was modified"
    );
    // The intact installation still opens with its contents.
    assert_eq!(
        RecordStore::open(dir.path(), "install-a")
            .unwrap()
            .app_record_show("ctx-1", "customer-1")
            .unwrap()["record"]["profile"]["display_name"],
        "Amina"
    );
}

#[test]
fn cad753_record_paths_are_safe_and_private() {
    let dir = TempDir::new().unwrap();
    for bad in [
        "",
        "../escape",
        "/absolute",
        "has space",
        "UPPER",
        "ctx/x",
        "..",
        ".",
    ] {
        assert!(
            record_db_path(dir.path(), bad).is_err(),
            "unsafe installation ID accepted: {bad}"
        );
    }
    let path = record_db_path(dir.path(), "install-a").unwrap();
    assert_eq!(
        path,
        dir.path().join("app-records").join("install-a.sqlite3")
    );
    RecordStore::open(dir.path(), "install-a").unwrap();
    assert_eq!(
        std::fs::metadata(dir.path().join("app-records"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn cad753_record_profile_validation_never_echoes_content() {
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
}

#[test]
fn cad779_concurrent_creates_conflict_on_normalized_email() {
    let dir = TempDir::new().unwrap();
    let store = record_file(&dir, "install-a");
    // Two writers enter together with two IDs behind one address
    // (mixed case proves the fold). No preview layer stands between
    // the calls: the write boundary itself must admit exactly one
    // live row.
    let barrier = std::sync::Barrier::new(2);
    let (first, second) = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            store.app_record_create(
                "ctx-1",
                "customer-a",
                &customer("Racer A", "Racer@Example.com"),
            )
        });
        let b = scope.spawn(|| {
            barrier.wait();
            store.app_record_create(
                "ctx-1",
                "customer-b",
                &customer("Racer B", "racer@example.com"),
            )
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    // Exactly one create wins; the loser names the conflict without
    // echoing the address.
    let wins = [&first, &second].iter().filter(|done| done.is_ok()).count();
    assert_eq!(wins, 1, "email race planted two live rows");
    let refused = match (&first, &second) {
        (Err(error), Ok(_)) | (Ok(_), Err(error)) => error.to_string(),
        _ => unreachable!("wins == 1 proves exactly one loser"),
    };
    assert!(
        refused.contains("another record"),
        "loser refused unclearly: {refused}"
    );
    assert!(
        !refused.contains("Racer@") && !refused.contains("racer@"),
        "address leaked in refusal: {refused}"
    );
    let listed = store.app_record_list("ctx-1").unwrap();
    let records = listed["records"].as_array().unwrap();
    assert_eq!(records.len(), 1, "second live row survived");
    assert_eq!(
        records[0]["profile"]["email"]
            .as_str()
            .unwrap()
            .to_lowercase(),
        "racer@example.com"
    );
}

#[test]
fn cad779_update_cannot_move_customer_onto_another_email() {
    let dir = TempDir::new().unwrap();
    let store = record_file(&dir, "install-a");
    store
        .app_record_create("ctx-1", "customer-a", &customer("A", "held@example.com"))
        .unwrap();
    store
        .app_record_create("ctx-1", "customer-b", &customer("B", "other@example.com"))
        .unwrap();
    let before = store.app_record_show("ctx-1", "customer-b").unwrap();
    let refused = store
        .app_record_update("ctx-1", "customer-b", 1, &customer("B", "HELD@example.com"))
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("another record"),
        "email move was not refused: {refused}"
    );
    assert_eq!(
        store.app_record_show("ctx-1", "customer-b").unwrap(),
        before
    );
}

#[test]
fn cad779_corrupt_existing_profile_closes_email_create() {
    let dir = TempDir::new().unwrap();
    let store = record_file(&dir, "install-a");
    store
        .app_record_create("ctx-1", "customer-a", &customer("A", "held@example.com"))
        .unwrap();
    let path = record_db_path(dir.path(), "install-a").unwrap();
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute(
        "UPDATE app_records SET body='not-json' WHERE context_id='ctx-1' AND id='customer-a'",
        [],
    )
    .unwrap();
    drop(conn);
    let refused = store
        .app_record_create("ctx-1", "customer-b", &customer("B", "new@example.com"))
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("corrupt") || refused.contains("profile"),
        "corrupt profile was ignored during uniqueness check: {refused}"
    );
    let conn =
        rusqlite::Connection::open(record_db_path(dir.path(), "install-a").unwrap()).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM app_records WHERE context_id='ctx-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "a new row was committed past a corrupt profile");
}

#[test]
fn cad779_receipt_write_failure_cannot_leave_imported_rows() {
    let dir = TempDir::new().unwrap();
    let store = record_file(&dir, "install-a");
    let csv = "record_id,display_name,email\ncustomer-a,A,first@example.com\n";
    let preview = store.app_record_csv_preview("ctx-1", csv).unwrap();
    let token = preview["preview_token"].as_str().unwrap();
    let path = record_db_path(dir.path(), "install-a").unwrap();
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER block_receipt BEFORE INSERT ON app_record_csv_imports \
         BEGIN SELECT RAISE(ABORT, 'receipt blocked'); END;",
    )
    .unwrap();
    drop(conn);

    assert!(
        store
            .app_record_csv_import("ctx-1", csv, token, "req-receipt-failure", None)
            .is_err(),
        "an import without a durable receipt was accepted"
    );
    let listed = store.app_record_list("ctx-1").unwrap();
    assert_eq!(
        listed["records"].as_array().unwrap().len(),
        0,
        "an import planted rows before its receipt could be saved: {listed}"
    );
}

#[test]
fn cad779_csv_receipt_state_column_migrates_older_files() {
    let dir = TempDir::new().unwrap();
    // A file whose receipts predate the pending/completed state,
    // crafted by hand: only the current binary's migration may add
    // the column, and all pre-existing rows are completed receipts.
    let path = record_db_path(dir.path(), "install-a").unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let setup = rusqlite::Connection::open(&path).unwrap();
    setup
        .execute_batch(
            "CREATE TABLE record_schema(version INTEGER NOT NULL);
             INSERT INTO record_schema(version) VALUES(1);
             CREATE TABLE record_identity(install_id TEXT PRIMARY KEY, created REAL NOT NULL);
             INSERT INTO record_identity(install_id, created) VALUES('install-a', 0.0);
             CREATE TABLE app_records(context_id TEXT NOT NULL, id TEXT NOT NULL, kind TEXT NOT NULL, revision INTEGER NOT NULL, body TEXT NOT NULL, body_digest TEXT NOT NULL, created REAL NOT NULL, updated REAL NOT NULL, PRIMARY KEY(context_id, id));
             CREATE TABLE app_record_revisions(context_id TEXT NOT NULL, record_id TEXT NOT NULL, revision INTEGER NOT NULL, body TEXT NOT NULL, body_digest TEXT NOT NULL, actor TEXT NOT NULL, at REAL NOT NULL, PRIMARY KEY(context_id, record_id, revision));
             CREATE TABLE app_record_csv_imports(request_id TEXT PRIMARY KEY, context_id TEXT NOT NULL, preview_token TEXT NOT NULL, result TEXT NOT NULL, at REAL NOT NULL);",
        )
        .unwrap();
    drop(setup);
    let store = RecordStore::open(dir.path(), "install-a").unwrap();
    let csv = "record_id,display_name,email\ncustomer-a,A,a@example.com\n";
    let preview = store.app_record_csv_preview("ctx-1", csv).unwrap();
    let token = preview["preview_token"].as_str().unwrap();
    let imported = store
        .app_record_csv_import("ctx-1", csv, token, "req-migrated", None)
        .unwrap();
    assert_eq!(
        imported["summary"],
        json!({"applied": 1, "skipped": 0, "failed": 0})
    );
    let replayed = store
        .app_record_csv_import("ctx-1", csv, token, "req-migrated", None)
        .unwrap();
    assert_eq!(replayed["replayed"], true);
}

#[test]
fn cad779_invalid_csv_decision_does_not_reserve_request_id() {
    let dir = TempDir::new().unwrap();
    let store = record_file(&dir, "install-a");
    let csv = "record_id,display_name,email\ncustomer-a,A,a@example.com\n";
    let preview = store.app_record_csv_preview("ctx-1", csv).unwrap();
    let token = preview["preview_token"].as_str().unwrap();
    assert!(store
        .app_record_csv_import(
            "ctx-1",
            csv,
            token,
            "req-decision",
            Some(vec![CsvDecision {
                row: 1,
                action: CsvAction::Update,
                expected_revision: Some(1),
            }]),
        )
        .is_err());
    let imported = store
        .app_record_csv_import("ctx-1", csv, token, "req-decision", None)
        .unwrap();
    assert_eq!(imported["summary"]["applied"], 1);
}

#[test]
fn cad780_concurrent_fresh_opens_converge_on_one_file() {
    let dir = TempDir::new().unwrap();
    // Six threads open the same never-created installation at once:
    // exactly one runs the fresh initialization while the rest wait
    // out its commit instead of mistaking the half-written file for
    // corruption. Every create then lands exactly once.
    let barrier = std::sync::Barrier::new(6);
    let dir_ref = &dir;
    let barrier_ref = &barrier;
    std::thread::scope(|scope| {
        (0..6)
            .map(|index| {
                scope.spawn(move || {
                    barrier_ref.wait();
                    let store = record_file(dir_ref, "install-fresh");
                    let id = format!("customer-{index}");
                    store
                        .app_record_create(
                            "ctx-1",
                            &id,
                            &customer(
                                &format!("Racer {index}"),
                                &format!("racer{index}@example.com"),
                            ),
                        )
                        .unwrap();
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .for_each(|h| h.join().unwrap());
    });
    let listed = record_file(&dir, "install-fresh")
        .app_record_list("ctx-1")
        .unwrap();
    assert_eq!(listed["records"].as_array().unwrap().len(), 6);
}

#[test]
fn cad780_open_waits_out_a_held_write_lock_then_succeeds() {
    let dir = TempDir::new().unwrap();
    // The installation file exists and is initialized; a sibling
    // holds a write transaction on it while a loser opens. Coded
    // lock contention at the WAL pragma must ride the bounded wait
    // and converge — never refuse the file as corrupt or unavailable.
    record_file(&dir, "install-locked")
        .app_record_create(
            "ctx-1",
            "customer-1",
            &customer("Amina", "amina@example.com"),
        )
        .unwrap();
    let path = record_db_path(dir.path(), "install-locked").unwrap();
    let holder = rusqlite::Connection::open(&path).unwrap();
    holder
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO app_suppressions(context_id,kind,key,reason,at) VALUES('ctx-1','email','held@example.com','test',0.0);")
        .unwrap();
    let opened = std::thread::scope(|scope| {
        let worker = scope.spawn(|| RecordStore::open(dir.path(), "install-locked"));
        // The opener's first attempts meet the held write lock; the
        // holder commits inside the bounded wait.
        std::thread::sleep(std::time::Duration::from_millis(300));
        holder.execute_batch("COMMIT;").unwrap();
        worker.join().unwrap()
    });
    let store = opened.unwrap();
    assert_eq!(
        store.app_record_show("ctx-1", "customer-1").unwrap()["record"]["revision"],
        1
    );
    assert_eq!(
        store.app_suppression_list("ctx-1").unwrap()["suppressions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn cad780_open_sees_an_initializing_file_then_converges() {
    let dir = TempDir::new().unwrap();
    // A valid SQLite file with no record tables yet: exactly what a
    // loser sees while a sibling is mid-initialization. The version
    // read must return the in-progress signal for the bounded wait —
    // never an immediate corruption refusal — and converge once the
    // sibling commits its init.
    let path = record_db_path(dir.path(), "install-initializing").unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    rusqlite::Connection::open(&path).unwrap();
    let opened = std::thread::scope(|scope| {
        let worker = scope.spawn(|| RecordStore::open(dir.path(), "install-initializing"));
        // The opener's first reads land before any table exists; the
        // initializer commits inside the bounded wait.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let init = rusqlite::Connection::open(&path).unwrap();
        init.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE record_schema(version INTEGER NOT NULL);
             INSERT INTO record_schema(version) VALUES(1);
             CREATE TABLE record_identity(install_id TEXT PRIMARY KEY, created REAL NOT NULL);
             INSERT INTO record_identity(install_id, created) VALUES('install-initializing', 0.0);
             COMMIT;",
        )
        .unwrap();
        worker.join().unwrap()
    });
    let store = opened.unwrap();
    // The converged open migrated the half-initialized file forward.
    let rule = crate::store::Predicate::parse(
        &serde_json::json!({"field": "tag", "op": "eq", "value": "vip"}),
    )
    .unwrap();
    store
        .app_segment_save("ctx-1", "seg-vip", None, "VIP", &[rule])
        .unwrap();
}

#[test]
fn cad780_open_on_a_forever_table_less_file_refuses_corrupt() {
    let dir = TempDir::new().unwrap();
    // A valid SQLite file that never gains record tables: the
    // bounded wait must expire into the rejected CORRUPT diagnosis —
    // never the transient in-progress signal, and never silently.
    let path = record_db_path(dir.path(), "install-empty").unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    rusqlite::Connection::open(&path).unwrap();
    let Err(refused) = RecordStore::open(dir.path(), "install-empty") else {
        panic!("table-less file opened");
    };
    let refused = refused.to_string();
    assert!(
        refused.contains("corrupt") && refused.contains("backup"),
        "table-less file refused unclearly: {refused}"
    );
    assert!(
        !refused.contains("in progress"),
        "exhausted wait leaked the transient signal: {refused}"
    );
}
