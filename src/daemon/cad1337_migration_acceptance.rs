//! Independent CAD-1337 unit acceptance: malformed pre-v38 ownership-column
//! shape is refused atomically by the real Store migration. The corrupted
//! private fixture reopens through `open_for_schema_tests`, which skips only
//! the unchanged rollout-receipt admission; production preflight, SQLite
//! authorizer, schema-shape guard, and migration transaction remain real. This
//! does not prove operational upgrade authorization.

use rusqlite::{types::Value as SqlValue, Connection};
use tempfile::TempDir;

#[test]
fn v38_migration_refuses_wrong_existing_owner_column_atomically() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("migration.sqlite3");
    crate::store::Store::open(&db).unwrap();

    // Reconstruct a private v37-era metadata table with an unsupported
    // pre-existing `user_id` declaration. The open below exercises the
    // migration's real PRAGMA shape check; no guard flag or implementation
    // helper is called by this acceptance.
    let fixture = Connection::open(&db).unwrap();
    fixture
        .execute_batch(
            "BEGIN;
             DROP INDEX IF EXISTS chat_files_sha_scope;
             DROP INDEX IF EXISTS chat_files_sha_scope_legacy;
             DROP INDEX IF EXISTS chat_files_sha_owner_thread;
             DROP TABLE chat_upload_accounts;
             ALTER TABLE chat_files RENAME TO chat_files_v38_fixture;
             CREATE TABLE chat_files(
                 id TEXT PRIMARY KEY,
                 sha256 TEXT NOT NULL,
                 size INTEGER NOT NULL,
                 mime TEXT NOT NULL,
                 name TEXT NOT NULL,
                 scope TEXT NOT NULL,
                 context_id TEXT NOT NULL DEFAULT '',
                 uploader TEXT NOT NULL,
                 created REAL NOT NULL,
                 user_id INTEGER);
             INSERT INTO chat_files(id,sha256,size,mime,name,scope,context_id,uploader,created,user_id)
                 VALUES('chf-fixture','ab',2,'text/plain','note.txt','home','','legacy',1.0,NULL);
             DROP TABLE chat_files_v38_fixture;
             CREATE UNIQUE INDEX chat_files_sha_scope
                 ON chat_files(sha256,scope,context_id);
             UPDATE schema_version SET version=37;
             COMMIT;",
        )
        .unwrap();
    let row_before = file_row(&fixture);
    drop(fixture);

    let err = crate::store::Store::open_for_schema_tests(&db)
        .err()
        .expect("wrong declared owner-column type must refuse migration");
    assert!(
        err.to_string()
            .contains("chat_files.user_id has an unsupported schema shape"),
        "migration refused for the wrong reason: {err}"
    );

    let after = Connection::open(&db).unwrap();
    let version: i64 = after
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 37, "refused migration advanced the marker");
    assert_eq!(
        file_row(&after),
        row_before,
        "refusal changed legacy row values"
    );
    let registry_exists: i64 = after
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='chat_upload_accounts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(registry_exists, 0, "refused migration kept partial DDL");
    let index_exists: i64 = after
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='chat_files_sha_scope'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index_exists, 1, "refusal changed the legacy index");
}

fn file_row(conn: &Connection) -> Vec<SqlValue> {
    conn.query_row(
        "SELECT id,sha256,size,mime,name,scope,context_id,uploader,created,user_id FROM chat_files",
        [],
        |row| {
            (0..10)
                .map(|index| row.get(index))
                .collect::<rusqlite::Result<Vec<_>>>()
        },
    )
    .unwrap()
}
