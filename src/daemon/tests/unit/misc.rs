use super::*;

#[test]
fn daemon_stream_is_pruned() {
    let (_dir, shared) = shared();
    for i in 0..5 {
        let _ = shared
            .store
            .event_public(DAEMON_ALIAS, "wal_checkpointed", json!({"i": i}));
    }
    shared.store.prune_stream(DAEMON_ALIAS, 3).unwrap();
    let events = shared.store.events_tail(DAEMON_ALIAS, 10).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events.last().unwrap().payload["i"], 4);
    assert_eq!(events.first().unwrap().payload["i"], 2);
}

#[test]
fn broken_walk_with_live_target_pane_is_refused() {
    let (dir, shared) = shared();
    register(&shared, dir.path(), "tgt");
    // Give the agent a pty endpoint with a LIVE pane pid — this
    // test process — so `pty_endpoint_facts` finds the target.
    let conn = rusqlite::Connection::open(dir.path().join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET endpoint_kind='pty', generation=1, pid=?1 \
             WHERE alias='tgt'",
        rusqlite::params![std::process::id() as i64],
    )
    .unwrap();
    drop(conn);
    // A dead peer pid: the first /proc read fails → chain is None.
    let dead = u32::MAX - 42;
    assert!(std::fs::metadata(format!("/proc/{dead}")).is_err());
    let err = shared.derived_caller("tgt", dead, "answer").unwrap_err();
    assert!(err.to_string().contains("could not be walked"), "{err}");
}
