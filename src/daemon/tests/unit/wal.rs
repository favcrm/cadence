use super::*;

#[test]
fn wal_pass_checkpoints_idle_provider() {
    let (dir, shared) = shared();
    let root = dir.path().join("codex");
    std::fs::create_dir_all(&root).unwrap();
    let (db, _writer) = wal_db(&root, "state_1.sqlite");
    let before = wal_size(&db);
    assert!(before > 1, "fixture must produce a real WAL");
    let roots = [crate::doctor::host::WalRoot {
        provider: "codex",
        label: "codex store",
        root,
    }];
    let mut watch = WalWatch::default();
    pass(&roots, &HashSet::new(), 1, &shared, &mut watch);
    assert_eq!(wal_size(&db), 0, "TRUNCATE frees the wal");
    let events = shared.store.events_tail(DAEMON_ALIAS, 10).unwrap();
    let ev = events
        .iter()
        .find(|e| e.kind == "wal_checkpointed")
        .expect("wal_checkpointed event");
    assert_eq!(ev.payload["provider"], "codex");
    assert_eq!(ev.payload["store"], "codex store");
    assert_eq!(ev.payload["wal_bytes_before"], before);
    assert_eq!(ev.payload["wal_bytes_after"], 0);
}

#[test]
fn wal_pass_skips_busy_provider() {
    let (dir, shared) = shared();
    shared
        .store
        .register_agent(&NewAgent {
            alias: "op1",
            provider: "codex",
            endpoint_kind: "pty",
            role: "worker",
            cwd: dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    shared
        .store
        .enqueue("op1", "do work", None, "m-wal-1", "test")
        .unwrap();
    let Take::Message(msg) = shared.store.take_queued("op1").unwrap() else {
        panic!("queued message must be taken");
    };
    shared.store.mark_running(&msg.id, "pty-1-wal").unwrap();
    // The alias has an actor — the daemon's owned set (CAD-250).
    let live: HashSet<String> = ["op1".to_string()].into();
    let busy = shared.store.busy_providers(&live, epoch_secs()).unwrap();
    assert!(busy.contains("codex"), "{busy:?}");

    let root = dir.path().join("codex");
    std::fs::create_dir_all(&root).unwrap();
    let (db, _writer) = wal_db(&root, "state_1.sqlite");
    let before = wal_size(&db);
    let roots = [crate::doctor::host::WalRoot {
        provider: "codex",
        label: "codex store",
        root,
    }];
    let mut watch = WalWatch::default();
    pass(&roots, &busy, 1, &shared, &mut watch);
    assert_eq!(wal_size(&db), before, "busy provider is untouched");
    assert!(shared
        .store
        .events_tail(DAEMON_ALIAS, 10)
        .unwrap()
        .iter()
        .all(|e| e.kind != "wal_checkpointed"));
    // Once the turn ends the next pass checkpoints.
    shared
        .store
        .finish(&msg, "completed", &json!({"status": "completed"}), None)
        .unwrap();
    let busy = shared.store.busy_providers(&live, epoch_secs()).unwrap();
    assert!(!busy.contains("codex"), "{busy:?}");
    pass(&roots, &busy, 1, &shared, &mut watch);
    assert_eq!(wal_size(&db), 0);
}

#[test]
fn wal_pass_ignores_stale_awaiting_report_rows() {
    let (dir, shared) = shared();
    shared
        .store
        .register_agent(&NewAgent {
            alias: "dv1",
            provider: "devin",
            endpoint_kind: "pty",
            role: "worker",
            cwd: dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: Some(r#"{"report_timeout_secs": 3600}"#),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    shared
        .store
        .enqueue("dv1", "do work", None, "m-stale-1", "test")
        .unwrap();
    let Take::Message(msg) = shared.store.take_queued("dv1").unwrap() else {
        panic!("queued message must be taken");
    };
    shared.store.mark_running(&msg.id, "pty-1-stale").unwrap();
    let msg = shared.store.message(&msg.id).unwrap().unwrap();
    shared.store.mark_submitted(&msg).unwrap();
    let msg = shared.store.message(&msg.id).unwrap().unwrap();
    assert!(msg.awaiting_report(), "{msg:?}");
    let delivered = msg.started.unwrap();

    let live: HashSet<String> = ["dv1".to_string()].into();
    let none = HashSet::new();
    let within = delivered + 60.0;
    let past = delivered + 3601.0;
    // Live actor, inside the bound: busy.
    let busy = shared.store.busy_providers(&live, within).unwrap();
    assert!(busy.contains("devin"), "{busy:?}");
    // No actor owns the alias: the row is stale, not busy.
    let busy = shared.store.busy_providers(&none, within).unwrap();
    assert!(!busy.contains("devin"), "{busy:?}");
    // Past the report bound: stale even with an actor.
    let busy = shared.store.busy_providers(&live, past).unwrap();
    assert!(!busy.contains("devin"), "{busy:?}");

    // And the checkpoint decision follows: a stale row never defers.
    let root = dir.path().join("devin");
    std::fs::create_dir_all(&root).unwrap();
    let (db, _writer) = wal_db(&root, "sessions.db");
    assert!(wal_size(&db) > 0);
    let roots = [crate::doctor::host::WalRoot {
        provider: "devin",
        label: "devin sessions",
        root,
    }];
    let mut watch = WalWatch::default();
    let live_busy = shared.store.busy_providers(&live, within).unwrap();
    pass(&roots, &live_busy, 1, &shared, &mut watch);
    assert!(wal_size(&db) > 0, "a live turn defers the checkpoint");
    let stale_busy = shared.store.busy_providers(&live, past).unwrap();
    pass(&roots, &stale_busy, 1, &shared, &mut watch);
    assert_eq!(wal_size(&db), 0, "an overdue row does not defer it");
}

#[test]
fn wal_pass_busy_reader_retries_next_tick() {
    let (dir, shared) = shared();
    let root = dir.path().join("devin");
    std::fs::create_dir_all(&root).unwrap();
    let (db, _writer) = wal_db(&root, "sessions.db");
    let before = wal_size(&db);
    // A second connection holding an open read transaction pins
    // the WAL — TRUNCATE reports busy.
    let reader = rusqlite::Connection::open(&db).unwrap();
    reader
        .execute_batch("BEGIN; SELECT count(*) FROM t;")
        .unwrap();
    let roots = [crate::doctor::host::WalRoot {
        provider: "devin",
        label: "devin store",
        root,
    }];
    let mut watch = WalWatch::default();
    pass(&roots, &HashSet::new(), 1, &shared, &mut watch);
    assert_eq!(wal_size(&db), before, "busy wal is left alone");
    assert!(shared
        .store
        .events_tail(DAEMON_ALIAS, 10)
        .unwrap()
        .iter()
        .all(|e| e.kind != "wal_checkpointed"));
    // Reader finishes — the retry completes the checkpoint.
    reader.execute_batch("END").unwrap();
    drop(reader);
    pass(&roots, &HashSet::new(), 1, &shared, &mut watch);
    assert_eq!(wal_size(&db), 0);
}

#[test]
fn wal_pass_ignores_small_wals() {
    let (dir, shared) = shared();
    let root = dir.path().join("claude");
    std::fs::create_dir_all(&root).unwrap();
    let (db, _writer) = wal_db(&root, "sess.db");
    let before = wal_size(&db);
    let roots = [crate::doctor::host::WalRoot {
        provider: "claude",
        label: "claude store",
        root,
    }];
    let before_events = shared.store.events_tail(DAEMON_ALIAS, 1).unwrap();
    let mut watch = WalWatch::default();
    pass(&roots, &HashSet::new(), u64::MAX, &shared, &mut watch);
    assert_eq!(wal_size(&db), before);
    assert_eq!(
        shared
            .store
            .events_tail(DAEMON_ALIAS, 1)
            .unwrap()
            .last()
            .map(|event| event.seq),
        before_events.last().map(|event| event.seq),
        "a small WAL emits no new event"
    );
}

#[test]
fn wal_pass_defers_to_quiet_window() {
    let (dir, shared) = shared();
    let root = dir.path().join("devin");
    std::fs::create_dir_all(&root).unwrap();
    let (db, _writer) = wal_db(&root, "sessions.db");
    let before = wal_size(&db);
    let roots = [crate::doctor::host::WalRoot {
        provider: "devin",
        label: "devin store",
        root,
    }];
    let mut watch = WalWatch::default();
    // quiet_secs=60, wal mtime=now → deferred, untouched, no event.
    wal_pass(
        &roots,
        &HashSet::new(),
        1,
        60,
        false,
        &shared.store,
        &mut watch,
    );
    assert_eq!(wal_size(&db), before);
    assert!(shared
        .store
        .events_tail(DAEMON_ALIAS, 10)
        .unwrap()
        .iter()
        .all(|e| e.kind != "wal_checkpointed"));
}

#[test]
fn wal_pass_never_creates_a_db() {
    let (dir, shared) = shared();
    let root = dir.path().join("codex");
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("ghost.sqlite");
    std::fs::write(root.join("ghost.sqlite-wal"), vec![0u8; 4096]).unwrap();
    let roots = [crate::doctor::host::WalRoot {
        provider: "codex",
        label: "codex store",
        root,
    }];
    let mut watch = WalWatch::default();
    pass(&roots, &HashSet::new(), 1, &shared, &mut watch);
    assert!(!db.exists(), "orphan -wal must not conjure a db");
}

#[test]
fn wal_pass_stale_wal_is_reclaimed() {
    let (dir, shared) = shared();
    let root = dir.path().join("devin");
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("sessions.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1);")
        .unwrap();
    drop(conn);
    std::fs::write(root.join("sessions.db-wal"), vec![0u8; 8192]).unwrap();
    let roots = [crate::doctor::host::WalRoot {
        provider: "devin",
        label: "devin store",
        root,
    }];
    let mut watch = WalWatch::default();
    pass(&roots, &HashSet::new(), 1, &shared, &mut watch);
    assert_eq!(wal_size(&db), 0, "stale wal is reclaimed");
    let events = shared.store.events_tail(DAEMON_ALIAS, 10).unwrap();
    let ev = events
        .iter()
        .find(|e| e.kind == "wal_checkpointed")
        .expect("stale wal reclaim is a real freeing event");
    assert_eq!(ev.payload["wal_bytes_before"], 8192);
    assert_eq!(ev.payload["wal_bytes_after"], 0);
}

#[test]
fn wal_pass_skips_symlinked_wal() {
    let (dir, shared) = shared();
    let root = dir.path().join("codex");
    std::fs::create_dir_all(&root).unwrap();
    let (db, _writer) = wal_db(&root, "state_1.sqlite");
    let real_wal = crate::doctor::host::wal_sibling(&db).unwrap();
    let staged = dir.path().join("real-wal-copy");
    std::fs::rename(&real_wal, &staged).unwrap();
    std::os::unix::fs::symlink(&staged, &real_wal).unwrap();
    let roots = [crate::doctor::host::WalRoot {
        provider: "codex",
        label: "codex store",
        root,
    }];
    let mut watch = WalWatch::default();
    pass(&roots, &HashSet::new(), 1, &shared, &mut watch);
    assert!(
        std::fs::symlink_metadata(&real_wal).unwrap().is_symlink(),
        "symlink wal must be left in place"
    );
    assert!(shared
        .store
        .events_tail(DAEMON_ALIAS, 10)
        .unwrap()
        .iter()
        .all(|e| e.kind != "wal_checkpointed"));
}

#[test]
fn wal_pass_dry_run_events_once() {
    let (dir, shared) = shared();
    let root = dir.path().join("devin");
    std::fs::create_dir_all(&root).unwrap();
    let (db, _writer) = wal_db(&root, "sessions.db");
    let before = wal_size(&db);
    let roots = [crate::doctor::host::WalRoot {
        provider: "devin",
        label: "devin store",
        root,
    }];
    let mut watch = WalWatch::default();
    wal_pass(
        &roots,
        &HashSet::new(),
        1,
        0,
        true,
        &shared.store,
        &mut watch,
    );
    wal_pass(
        &roots,
        &HashSet::new(),
        1,
        0,
        true,
        &shared.store,
        &mut watch,
    );
    assert_eq!(wal_size(&db), before, "dry run never writes");
    let events = shared.store.events_tail(DAEMON_ALIAS, 10).unwrap();
    let pending: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "wal_checkpoint_pending")
        .collect();
    assert_eq!(pending.len(), 1, "one pending event, not one per tick");
    assert_eq!(pending[0].payload["wal_bytes_before"], before);
    // Back under the limit clears the dedupe — a later crossing
    // reports again.
    checkpoint_wal(&db);
    wal_pass(
        &roots,
        &HashSet::new(),
        1,
        0,
        true,
        &shared.store,
        &mut watch,
    );
    let n: usize = shared
        .store
        .events_tail(DAEMON_ALIAS, 10)
        .unwrap()
        .iter()
        .filter(|e| e.kind == "wal_checkpoint_pending")
        .count();
    assert_eq!(n, 1, "still one — under the limit emitted nothing");
}

#[test]
fn wal_pass_under_a_sandbox_profile_is_observe_only() {
    assert!(!wal_observe_only(false, None));
    assert!(wal_observe_only(true, None));
    let (dir, shared) = shared();
    let root = dir.path().join("devin");
    std::fs::create_dir_all(&root).unwrap();
    let (db, _writer) = wal_db(&root, "sessions.db");
    let before = wal_size(&db);
    let roots = [crate::doctor::host::WalRoot {
        provider: "devin",
        label: "devin store",
        root,
    }];
    let mut watch = WalWatch::default();
    wal_pass(
        &roots,
        &HashSet::new(),
        1,
        0,
        wal_observe_only(false, Some("x")),
        &shared.store,
        &mut watch,
    );
    assert_eq!(wal_size(&db), before, "a sandbox never checkpoints");
    let events = shared.store.events_tail(DAEMON_ALIAS, 10).unwrap();
    assert!(events.iter().any(|e| e.kind == "wal_checkpoint_pending"));
    assert!(events.iter().all(|e| e.kind != "wal_checkpointed"));
}
