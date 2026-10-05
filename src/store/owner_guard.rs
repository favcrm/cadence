//! CAD-1161 independently authored bad-case guard (aos159-constructor-guard).
//! Implementers must not edit/weaken this file. Exercises the actual production
//! in-transaction incarnation and fixed-path guards with real SQLite, not a
//! client-side mirror. Fixed-path refusal creates no authority or service peer.
//! Binding is a public selector, NEVER a forged positive StoreOwnerGrant.
//! Grant issuance/one-use replay and protected-open/close/witness integration
//! remain to be exercised once the constructor's real relay contract exists.
use super::owner;
use rusqlite::{params, Connection, TransactionBehavior};
use std::path::Path;

fn database_files(path: &Path) -> Vec<Option<Vec<u8>>> {
    ["", "-wal", "-shm", "-journal"]
        .into_iter()
        .map(
            |suffix| match std::fs::read(format!("{}{suffix}", path.display())) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => panic!("could not inspect isolated database: {error}"),
            },
        )
        .collect()
}

fn refused_without_mutation(conn: &mut Connection, path: &Path, binding: &owner::Binding) {
    // Exclude malformed selector/timeout false positives: this candidate is
    // valid public syntax and must reach the real database identity comparison.
    binding.validate().unwrap();
    let files = database_files(path);
    let before: (String, String, i64) = conn
        .query_row(
            "SELECT database_id,incarnation,epoch FROM store_incarnation WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    match owner::check_identity(&tx, binding, false) {
        Err(crate::Error::Rejected(message)) => assert_eq!(
            message,
            "Store owner permit is bound to a different database incarnation/epoch"
        ),
        _ => panic!("wrong/replaced incarnation did not reach the real refusal"),
    }
    let after: (String, String, i64) = tx
        .query_row(
            "SELECT database_id,incarnation,epoch FROM store_incarnation WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(after, before, "refused selector changed durable identity");
    assert_eq!(
        tx.query_row("SELECT value FROM canary WHERE id=1", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        159,
        "refusal changed business state"
    );
    tx.rollback().unwrap();
    assert_eq!(
        database_files(path),
        files,
        "incarnation refusal mutated the database or SQLite sidefiles"
    );
}

#[test]
fn store_owner_wrong_or_replaced_incarnation_refuses_without_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cad1161.sqlite3");
    let mut conn = Connection::open(&path).unwrap();
    conn.execute_batch(owner::IDENTITY_SCHEMA).unwrap();
    conn.execute_batch(
        "CREATE TABLE canary(id INTEGER PRIMARY KEY,value INTEGER NOT NULL);
         INSERT INTO canary VALUES(1,159);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO store_incarnation VALUES(1,?1,?2,?3,?4,?5)",
        params![
            "cad1161-db",
            "incarnation-one",
            1,
            "open-one",
            "artifact-one"
        ],
    )
    .unwrap();
    let binding = owner::Binding {
        database_id: "cad1161-db".into(),
        incarnation: "incarnation-one".into(),
        database_epoch: 1,
        operation: "open-one".into(),
        purpose: owner::Purpose::Open,
        path: path.to_str().unwrap().into(),
        challenge: vec![0x61; 32],
        attempt: "attempt-one".into(),
        artifact: "artifact-one".into(),
        deadline_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 60,
        source: None,
    };
    binding.validate().unwrap();
    // A shape-valid, matching local row reaches this real comparison. This
    // read-only success does NOT mint an opening/maintenance/restore permit.
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    owner::check_identity(&tx, &binding, false).unwrap();
    tx.rollback().unwrap();

    // The matching local identity above remains a valid pure comparison, but
    // neither an existing alternate database nor an absent caller-chosen file
    // may be elected as the fixed protected database. These absolute paths are
    // entirely in our isolated directory: never inspect the real enclave.
    let absent_path = dir.path().join("must-not-be-created.sqlite3");
    let files_before_path_refusal = database_files(&path);
    for requested in [path.as_path(), absent_path.as_path()] {
        assert!(requested.is_absolute());
        assert_ne!(requested, Path::new(owner::DATABASE_PATH));
        let candidate_files = database_files(requested);
        match owner::check_path(requested) {
            Err(crate::Error::Rejected(message)) => assert_eq!(
                message, "Store owner path is not the exact canonical regular database",
                "alternate path did not reach the actual fixed-path refusal"
            ),
            _ => panic!("caller elected an alternate protected database path"),
        }
        assert_eq!(
            database_files(requested),
            candidate_files,
            "path refusal created or changed the candidate database/sidefiles"
        );
        assert_eq!(
            database_files(&path),
            files_before_path_refusal,
            "path refusal changed the existing database/sidefiles"
        );
    }
    assert_eq!(
        conn.query_row("SELECT value FROM canary WHERE id=1", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        159,
        "path refusal changed business state"
    );

    let mut wrong_database = binding.clone();
    wrong_database.database_id = "different-database".into();
    refused_without_mutation(&mut conn, &path, &wrong_database);
    let mut wrong_epoch = binding.clone();
    wrong_epoch.database_epoch = 2;
    refused_without_mutation(&mut conn, &path, &wrong_epoch);

    // Advance ONLY isolated setup state. Reusing the old incarnation binding
    // now refuses despite the same database/path; this is stale-incarnation
    // refusal, NOT proof of an opaque consumed permit's same-epoch replay gate.
    conn.execute(
        "UPDATE store_incarnation SET incarnation='incarnation-two',epoch=2,
         operation='open-two',artifact='artifact-two' WHERE id=1",
        [],
    )
    .unwrap();
    refused_without_mutation(&mut conn, &path, &binding);
}
