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

#[test]
fn cloud_dispatch_outbox_is_committed_with_job_kickoff_and_retry_is_same_fact() {
    let (_dir, s) = draft_task();
    let (_, mid, duplicate, _) = s.dispatch_task("t1", None, None, "operator").unwrap();
    assert!(!duplicate);
    let row: (String, String, i64, Option<String>) = s
        .conn()
        .query_row(
            "SELECT message_id, audience_agent, task_revision, remote_turn_id \
             FROM cloud_dispatch_outbox WHERE message_id=?1",
            [&mid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(row, (mid.clone(), "w1".into(), 1, None));
    let (_, again, duplicate, _) = s.dispatch_task("t1", None, None, "operator").unwrap();
    assert!(duplicate);
    assert_eq!(again, mid);
    let count: i64 = s
        .conn()
        .query_row("SELECT COUNT(*) FROM cloud_dispatch_outbox", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    s.enqueue_steered(
        "w1",
        "forged dispatch source without a lane",
        None,
        "forged-source",
        "dispatch",
        None,
        None,
        None,
        &Sender::Unattributed,
        &Steer::NONE,
        None,
    )
    .unwrap();
    let count: i64 = s
        .conn()
        .query_row("SELECT COUNT(*) FROM cloud_dispatch_outbox", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 1, "source alone cannot mint an outbox fact");
    assert!(s
        .conn()
        .execute(
            "UPDATE cloud_dispatch_outbox SET audience_agent='forged-agent' WHERE message_id=?1",
            [&mid],
        )
        .is_err());
    let actionable: i64 = s
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM cloud_dispatch_outbox WHERE remote_turn_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(actionable, 0, "dispatch alone cannot create remote work");
}

#[test]
fn cloud_dispatch_outbox_insert_failure_rolls_back_entire_dispatch() {
    let (_dir, s) = draft_task();
    s.conn()
        .execute_batch(
            "CREATE TRIGGER deny_cloud_outbox BEFORE INSERT ON cloud_dispatch_outbox \
             BEGIN SELECT RAISE(ABORT, 'outbox unavailable'); END;",
        )
        .unwrap();
    assert!(s.dispatch_task("t1", None, None, "operator").is_err());
    let task = s.task("t1").unwrap();
    assert_eq!(task.state, "draft");
    assert_eq!(task.revision, 0);
    assert!(task.dispatch_message.is_none());
    let count: i64 = s
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE task_id='t1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn cloud_turn_claim_is_exact_idempotent_and_refuses_forged_or_stale_targets() {
    let (_dir, s) = draft_task();
    let (_, mid, ..) = s.dispatch_task("t1", None, None, "operator").unwrap();
    assert!(s.claim_cloud_dispatch_turn(&mid, "", "w1").is_err());
    assert!(s
        .claim_cloud_dispatch_turn(&mid, "org-1", "forged-agent")
        .is_err());
    let turn = s.claim_cloud_dispatch_turn(&mid, "org-1", "w1").unwrap();
    assert!(turn.starts_with("remote-"));
    assert_ne!(turn, mid);
    assert_eq!(
        s.claim_cloud_dispatch_turn(&mid, "org-1", "w1").unwrap(),
        turn
    );
    assert!(s.claim_cloud_dispatch_turn(&mid, "org-2", "w1").is_err());
    s.cancel_task("t1", "operator").unwrap();
    assert!(s.claim_cloud_dispatch_turn(&mid, "org-1", "w1").is_err());
}

#[test]
fn concurrent_cloud_claims_return_one_durable_turn() {
    let (_dir, s) = draft_task();
    let (_, mid, ..) = s.dispatch_task("t1", None, None, "operator").unwrap();
    let s = std::sync::Arc::new(s);
    let mut threads = Vec::new();
    for _ in 0..8 {
        let s = s.clone();
        let mid = mid.clone();
        threads.push(std::thread::spawn(move || {
            s.claim_cloud_dispatch_turn(&mid, "org-1", "w1").unwrap()
        }));
    }
    let turns: std::collections::HashSet<String> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(turns.len(), 1);
}

#[test]
fn cloud_plain_dispatch_has_no_task_revision_and_claims_once() {
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
        )
        .unwrap();
    assert!(!duplicate);
    let row: (Option<String>, Option<i64>) = s
        .conn()
        .query_row(
            "SELECT task_id,task_revision FROM cloud_dispatch_outbox WHERE message_id='plain-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, (None, None));
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
        )
        .unwrap();
    assert!(duplicate);
    let count: i64 = s
        .conn()
        .query_row("SELECT COUNT(*) FROM cloud_dispatch_outbox", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    let first = s
        .claim_cloud_dispatch_turn("plain-1", "org-1", "w1")
        .unwrap();
    assert_eq!(
        s.claim_cloud_dispatch_turn("plain-1", "org-1", "w1")
            .unwrap(),
        first
    );
}

#[test]
fn cloud_outbox_v26_migration_is_atomic() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("t.sqlite3");
    Store::open(&db).unwrap();
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch("DROP TABLE cloud_dispatch_outbox; UPDATE schema_version SET version=25;")
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
    assert_eq!(version, 26);
}
