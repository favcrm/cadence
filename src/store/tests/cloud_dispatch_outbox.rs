use super::*;

fn draft_task() -> (TempDir, Store) {
    let (dir, s) = store();
    let cwd = dir.path().join("w");
    seeded_job(&s, &cwd);
    reg_pty(&s, "w1", &cwd);
    s.create_task("j1", "t1", None, Some("w1"), None, None, None, None, None)
        .unwrap();
    (dir, s)
}

fn count(s: &Store) -> i64 {
    s.conn()
        .query_row("SELECT COUNT(*) FROM cloud_dispatch_outbox", [], |r| {
            r.get(0)
        })
        .unwrap()
}

#[test]
fn ordinary_local_dispatch_never_creates_a_cloud_claim_candidate() {
    let (_dir, s) = draft_task();
    let (_, mid, duplicate, _) = s.dispatch_task("t1", None, None, "operator").unwrap();
    assert!(!duplicate);
    assert_eq!(
        count(&s),
        0,
        "local work must not accumulate cloud candidates"
    );
    assert!(s.claim_cloud_dispatch_turn(&mid).is_err());
    let (_, same, duplicate, _) = s.dispatch_task("t1", None, None, "operator").unwrap();
    assert!(duplicate);
    assert_eq!(same, mid);
    assert_eq!(s.task("t1").unwrap().state, "dispatched");
}

#[test]
fn a_cloud_outbox_insert_failure_does_not_rollback_local_dispatch() {
    let (_dir, s) = draft_task();
    s.fixture_write(|c| {
        c.execute_batch(
            "CREATE TRIGGER deny_cloud_outbox BEFORE INSERT ON cloud_dispatch_outbox \
             BEGIN SELECT RAISE(ABORT, 'outbox unavailable'); END;",
        )
        .map_err(Into::into)
    })
    .unwrap();
    let (_, mid, duplicate, _) = s.dispatch_task("t1", None, None, "operator").unwrap();
    assert!(!duplicate);
    assert_eq!(count(&s), 0);
    assert!(s.claim_cloud_dispatch_turn(&mid).is_err());
}

#[test]
fn ordinary_local_plain_dispatch_never_creates_cloud_candidate() {
    let (dir, s) = store();
    let cwd = dir.path().join("w");
    reg(&s, "w1", &cwd);
    let (duplicate, _) = s
        .enqueue_steered(
            "w1",
            "plain dispatch",
            None,
            "plain-1",
            "dispatch",
            None,
            Some("CAD-720"),
            Some(cwd.to_str().unwrap()),
            &Sender::Unattributed,
            &Steer::NONE,
            None,
            None,
            None,
        )
        .unwrap();
    assert!(!duplicate);
    assert_eq!(count(&s), 0);
    assert!(s.claim_cloud_dispatch_turn("plain-1").is_err());
    let (duplicate, _) = s
        .enqueue_steered(
            "w1",
            "plain dispatch",
            None,
            "plain-1",
            "dispatch",
            None,
            Some("CAD-720"),
            Some(cwd.to_str().unwrap()),
            &Sender::Unattributed,
            &Steer::NONE,
            None,
            None,
            None,
        )
        .unwrap();
    assert!(duplicate);
    assert_eq!(count(&s), 0);
}

/// Rebuild the exact pre-fix v26 columns around a committed local dispatch.
/// This models a disk snapshot restored after the fix has been installed.
fn restored_v26_local_row(claimed: bool) -> (TempDir, Store, String) {
    let (dir, s) = draft_task();
    let (_, mid, ..) = s.dispatch_task("t1", None, None, "operator").unwrap();
    drop(s);
    let db = dir.path().join("t.sqlite3");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "DROP TRIGGER cloud_dispatch_source_immutable;
         DROP TRIGGER cloud_dispatch_eligibility_immutable;
         DROP TABLE cloud_dispatch_outbox;
         CREATE TABLE cloud_dispatch_outbox(
           cursor INTEGER PRIMARY KEY AUTOINCREMENT,
           message_id TEXT NOT NULL UNIQUE REFERENCES messages(id),
           source TEXT NOT NULL CHECK(source IN ('dispatch','job_dispatch')),
           task_id TEXT,
           task_revision INTEGER,
           audience_agent TEXT NOT NULL,
           expected_head TEXT,
           payload_digest TEXT NOT NULL,
           organization_id TEXT,
           remote_turn_id TEXT UNIQUE,
           created REAL NOT NULL,
           claimed REAL
         );
         UPDATE schema_version SET version=26;",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO cloud_dispatch_outbox(message_id,source,task_id,task_revision,
         audience_agent,payload_digest,created) VALUES (?1,'job_dispatch','t1',1,'w1','sha256:legacy',1)",
        [&mid],
    )
    .unwrap();
    if claimed {
        conn.execute(
            "UPDATE cloud_dispatch_outbox SET organization_id='old-org',
             remote_turn_id='remote-old',claimed=1 WHERE message_id=?1",
            [&mid],
        )
        .unwrap();
    }
    drop(conn);
    let restored = Store::open_for_schema_tests(&db).unwrap();
    (dir, restored, mid)
}

#[test]
fn restored_v26_local_rows_remain_ineligible_with_later_org_or_old_claim() {
    for claimed in [false, true] {
        let (_dir, s, mid) = restored_v26_local_row(claimed);
        let eligible: i64 = s
            .conn()
            .query_row(
                "SELECT cloud_eligible FROM cloud_dispatch_outbox WHERE message_id=?1",
                [&mid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(eligible, 0);
        assert!(s.claim_cloud_dispatch_turn(&mid).is_err());
        assert!(s
            .conn()
            .execute(
                "UPDATE cloud_dispatch_outbox SET cloud_eligible=1,
                 organization_id='later-org' WHERE message_id=?1",
                [&mid],
            )
            .is_err());
        assert_eq!(s.task("t1").unwrap().state, "dispatched");
    }
}

#[test]
fn concurrent_claims_cannot_turn_legacy_local_work_into_cloud_work() {
    let (_dir, s, mid) = restored_v26_local_row(false);
    let s = std::sync::Arc::new(s);
    let mut threads = Vec::new();
    for _ in 0..8 {
        let s = s.clone();
        let mid = mid.clone();
        threads.push(std::thread::spawn(move || {
            s.claim_cloud_dispatch_turn(&mid).is_err()
        }));
    }
    assert!(threads.into_iter().all(|thread| thread.join().unwrap()));
    let turn: Option<String> = s
        .conn()
        .query_row(
            "SELECT remote_turn_id FROM cloud_dispatch_outbox WHERE message_id=?1",
            [&mid],
            |r| r.get(0),
        )
        .unwrap();
    assert!(turn.is_none());
}

#[test]
fn v27_migration_converges_when_version_was_rolled_back_after_column_commit() {
    let (dir, s) = store();
    drop(s);
    let db = dir.path().join("t.sqlite3");
    let conn = Connection::open(&db).unwrap();
    conn.execute("UPDATE schema_version SET version=26", [])
        .unwrap();
    drop(conn);
    Store::open_for_schema_tests(&db).unwrap();
    let conn = Connection::open(&db).unwrap();
    let version: i64 = conn
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, crate::rollout::SCHEMA_VERSION);
}

#[test]
fn v27_replay_replaces_permissive_column_and_fake_eligibility_trigger() {
    let (dir, s) = draft_task();
    let (_, mid, ..) = s.dispatch_task("t1", None, None, "operator").unwrap();
    drop(s);
    let db = dir.path().join("t.sqlite3");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "DROP TRIGGER cloud_dispatch_source_immutable;
         DROP TRIGGER cloud_dispatch_eligibility_immutable;
         DROP TABLE cloud_dispatch_outbox;
         CREATE TABLE cloud_dispatch_outbox(
           cursor INTEGER PRIMARY KEY AUTOINCREMENT,
           message_id TEXT NOT NULL UNIQUE REFERENCES messages(id),
           source TEXT NOT NULL,
           task_id TEXT,
           task_revision INTEGER,
           audience_agent TEXT NOT NULL,
           expected_head TEXT,
           payload_digest TEXT NOT NULL,
           organization_id TEXT,
           remote_turn_id TEXT UNIQUE,
           created REAL NOT NULL,
           claimed REAL,
           cloud_eligible INTEGER NOT NULL DEFAULT 0
         );
         CREATE TRIGGER cloud_dispatch_eligibility_immutable
         BEFORE UPDATE ON cloud_dispatch_outbox
         WHEN 0 BEGIN SELECT RAISE(ABORT, 'never runs'); END;
         UPDATE schema_version SET version=26;",
    )
    .unwrap();
    drop(conn);
    Store::open_for_schema_tests(&db).unwrap();
    let conn = Connection::open(&db).unwrap();
    assert!(conn
        .execute(
            "INSERT INTO cloud_dispatch_outbox(message_id,source,audience_agent,
             payload_digest,created,cloud_eligible)
             VALUES (?1,'dispatch','w1','sha256:legacy',1,1)",
            [&mid],
        )
        .is_err());
    conn.execute(
        "INSERT INTO cloud_dispatch_outbox(message_id,source,audience_agent,
         payload_digest,created,cloud_eligible)
         VALUES (?1,'dispatch','w1','sha256:legacy',1,0)",
        [&mid],
    )
    .unwrap();
    assert!(conn
        .execute(
            "UPDATE cloud_dispatch_outbox SET cloud_eligible=1 WHERE message_id=?1",
            [&mid],
        )
        .is_err());
}

#[test]
fn cloud_outbox_v26_migration_is_atomic() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("t.sqlite3");
    Store::open(&db).unwrap();
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "DROP TRIGGER cloud_dispatch_source_immutable;
         DROP TRIGGER cloud_dispatch_eligibility_immutable;
         DROP TABLE cloud_dispatch_outbox;
         UPDATE schema_version SET version=25;",
    )
    .unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_cloud_schema BEFORE UPDATE ON schema_version \
         WHEN NEW.version=26 BEGIN SELECT RAISE(ABORT,'migration denied'); END;",
    )
    .unwrap();
    assert!(Store::open_for_schema_tests(&db).is_err());
    let table: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='cloud_dispatch_outbox'",
            [],
            |r| r.get(0),
        )
        .optional()
        .unwrap();
    assert!(table.is_none(), "failed migration left a partial outbox");
    conn.execute_batch("DROP TRIGGER fail_cloud_schema;")
        .unwrap();
    Store::open_for_schema_tests(&db).unwrap();
    let version: i64 = conn
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, crate::rollout::SCHEMA_VERSION);
}
