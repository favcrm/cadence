use super::*;

fn draft_task() -> (TempDir, Store) {
    let (dir, s) = store();
    let cwd = dir.path().join("w");
    seeded_job(&s, &cwd);
    reg(&s, "w1", &cwd);
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
        .query_row("SELECT COUNT(*) FROM cloud_dispatch_outbox", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
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
