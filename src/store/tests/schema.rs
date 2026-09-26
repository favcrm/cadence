
    /// A crash between ALTER and the version bump must not wedge the
    /// database: the migration is one transaction, and a half-applied
    /// state (column present, version still 1) converges on reopen.
    #[test]
    fn migration_v1_to_v2_is_atomic_and_idempotent() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("t.sqlite3");
        let cwd = dir.path().join("w");
        std::fs::create_dir(&cwd).unwrap();
        {
            let s = Store::open(&db).unwrap();
            reg(&s, "a1", &cwd);
            s.enqueue("a1", "keep me", None, "m1", "user").unwrap();
        }
        // Fabricate a genuine v1 database.
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "ALTER TABLE agents DROP COLUMN endpoint;
             UPDATE schema_version SET version=1;",
        )
        .unwrap();
        drop(conn);
        {
            // Upgrade preserves v1 rows and restores the column.
            let s = Store::open_for_schema_tests(&db).unwrap();
            let agent = s.agent("a1").unwrap();
            assert_eq!(agent.endpoint, None);
            assert_eq!(s.message("m1").unwrap().unwrap().body, "keep me");
            s.set_identity(
                "a1",
                &crate::adapter::Identity {
                    thread_id: "th".into(),
                    session_id: "s".into(),
                    model: None,
                    effort: None,
                    pid: 1,
                    endpoint: Some("ws://x".into()),
                    generation: None,
                    attach: None,
                },
            )
            .unwrap();
            assert_eq!(s.agent("a1").unwrap().endpoint.as_deref(), Some("ws://x"));
        }
        // The interrupted-upgrade state (column present, version 1)
        // converges instead of failing on a duplicate column.
        let conn = Connection::open(&db).unwrap();
        conn.execute("UPDATE schema_version SET version=1", [])
            .unwrap();
        drop(conn);
        {
            // `recover` clears runtime endpoint/pid on every open; the
            // persisted thread identity proves the row survived.
            let s = Store::open_for_schema_tests(&db).unwrap();
            let agent = s.agent("a1").unwrap();
            assert_eq!(agent.thread_id.as_deref(), Some("th"));
            assert_eq!(agent.endpoint, None);
        }
        // Reopening a current-version store is a no-op.
        let version: i64 = {
            let conn = Connection::open(&db).unwrap();
            conn.query_row("SELECT version FROM schema_version", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(version, crate::rollout::SCHEMA_VERSION);
        Store::open(&db).unwrap();
    }

    #[test]
    fn migration_v3_to_v4_converges_and_preserves_rows() {
        let dir = TempDir::new().unwrap();
        let db = v3_db(&dir);
        {
            let s = Store::open_for_schema_tests(&db).unwrap();
            // Old rows read cleanly: the pre-v4 message is unattached.
            let m = s.message("m1").unwrap().unwrap();
            assert_eq!(m.task_id, None);
            assert_eq!(m.body, "old work");
            // The new tables exist and take writes.
            s.create_job(
                "j1",
                None,
                "/s.md",
                &"0".repeat(64),
                "a1",
                Some("CAD-1"),
                None,
                None,
                2,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let v: i64 = s
                .conn()
                .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, crate::rollout::SCHEMA_VERSION);
        }
        // Half-applied: v4 objects present but version rolled back —
        // reopening must converge, not fail on duplicates.
        let conn = Connection::open(&db).unwrap();
        conn.execute("UPDATE schema_version SET version=3", [])
            .unwrap();
        drop(conn);
        {
            let s = Store::open_for_schema_tests(&db).unwrap();
            assert_eq!(s.job("j1").unwrap().id, "j1");
            assert_eq!(s.message("m1").unwrap().unwrap().task_id, None);
        }
        // Deeper partial state: a new column exists while another was
        // dropped and version is still 3 — the per-column checks heal it.
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "ALTER TABLE events DROP COLUMN task_id;
             UPDATE schema_version SET version=3;",
        )
        .unwrap();
        drop(conn);
        {
            let s = Store::open_for_schema_tests(&db).unwrap();
            // Scoped events work again → the column was re-added.
            s.create_task("j1", "j1-t9", None, None, None, None, None, None, None)
                .unwrap();
            let evs = s.job_events("j1", 0, 50).unwrap();
            assert!(evs.iter().any(|e| e.task_id.as_deref() == Some("j1-t9")));
            let v: i64 = s
                .conn()
                .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, crate::rollout::SCHEMA_VERSION);
        }
    }

    /// CAD-385 acceptance 4: v13 → v14 adds `agents.pid_start` and
    /// keeps every row; a row that carried a pid across the upgrade has
    /// no start (it fails closed until re-recorded); a half-applied v14
    /// converges, and reopening a current store is a no-op.
    #[test]
    fn migration_v13_to_v14_adds_pid_start_and_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("t.sqlite3");
        let version = |db: &Path| -> i64 {
            Connection::open(db)
                .unwrap()
                .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
                .unwrap()
        };
        let has_column = |db: &Path| -> bool {
            Connection::open(db)
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM pragma_table_info('agents') WHERE name='pid_start'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
                == 1
        };
        {
            let s = Store::open(&db).unwrap();
            reg(&s, "m1", dir.path());
        }
        // A genuine v13: no `pid_start`, and a stopped row that kept its
        // pid (recovery leaves stopped rows alone).
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "ALTER TABLE agents DROP COLUMN pid_start;
                 UPDATE agents SET state='stopped', pid=4242, generation='g0' WHERE alias='m1';
                 UPDATE schema_version SET version=13;",
            )
            .unwrap();
        assert!(!has_column(&db));
        {
            let s = Store::open_for_schema_tests(&db).unwrap();
            let a = s.agent("m1").unwrap();
            assert_eq!(
                (a.pid, a.pid_start, a.state.as_str()),
                (Some(4242), None, "stopped")
            );
        }
        assert_eq!(version(&db), crate::rollout::SCHEMA_VERSION);
        assert!(has_column(&db));
        // Half-applied: column present, version rolled back — converges.
        Connection::open(&db)
            .unwrap()
            .execute("UPDATE schema_version SET version=13", [])
            .unwrap();
        {
            let s = Store::open_for_schema_tests(&db).unwrap();
            assert_eq!(s.agent("m1").unwrap().pid, Some(4242));
        }
        assert_eq!(version(&db), crate::rollout::SCHEMA_VERSION);
        // Reopening a current store is a no-op.
        let s = Store::open(&db).unwrap();
        assert_eq!(s.agent("m1").unwrap().pid_start, None);
        assert_eq!(version(&db), crate::rollout::SCHEMA_VERSION);
    }

    /// CAD-162 acceptance 1/3, the recovery check: a shutdown-marker
    /// entry whose token is not current for the recorded generation
    /// under the AGENT'S OWN scheme is refused — a pty-shaped token on a
    /// managed agent (which the old `pty-{gen}-` literal kept) and a
    /// managed-shaped token on a pty agent — while a genuine pty entry
    /// is kept.
    #[test]
    fn adoption_refuses_a_marker_token_from_another_endpoint_kind() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("t.sqlite3");
        let cwd = dir.path().join("w");
        std::fs::create_dir(&cwd).unwrap();
        let pty_token = format!("pty-{CAD162_GEN}-{}", "a".repeat(32));
        let claude_token = format!("claude-{CAD162_GEN}-{}", "b".repeat(32));
        {
            let s = Store::open(&path).unwrap();
            cad162_turn(&s, &cwd, "mc", ("claude", "managed"), &pty_token);
            cad162_turn(&s, &cwd, "dp", ("devin", "pty"), &claude_token);
            cad162_turn(&s, &cwd, "ok", ("devin", "pty"), &pty_token);
        }
        let entry = |alias: &str, token: &str| AdoptEntry {
            alias: alias.to_string(),
            message_id: format!("m-{alias}"),
            turn_id: token.to_string(),
            generation: CAD162_GEN.to_string(),
            pane_pid: 4242,
            native_session: format!("session-{alias}"),
        };
        let s = Store::open_adopting(
            &path,
            Some(ConsumedMarker {
                entries: vec![
                    entry("mc", &pty_token),
                    entry("dp", &claude_token),
                    entry("ok", &pty_token),
                ],
                stale: None,
            }),
        )
        .unwrap();
        for alias in ["mc", "dp"] {
            assert_eq!(
                cad162_refusals(&s, alias),
                [format!("turn m-{alias} does not match recorded generation")],
                "{alias}"
            );
            assert!(s.take_adoption(alias).is_none(), "{alias} adopted");
            let m = s.message(&format!("m-{alias}")).unwrap().unwrap();
            assert_eq!(m.state, "unknown", "{alias}");
        }
        assert!(cad162_refusals(&s, "ok").is_empty());
        assert_eq!(s.take_adoption("ok").map(|e| e.len()), Some(1));
        assert_eq!(s.message("m-ok").unwrap().unwrap().state, "running");
    }

    /// CAD-162 acceptance 1/3, the shutdown-record check: a running pty
    /// turn whose token is another kind's shape under the snapshot
    /// generation is refused and named, never recorded for adoption.
    #[test]
    fn shutdown_entries_refuse_a_token_from_another_endpoint_kind() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let claude_token = format!("claude-{CAD162_GEN}-{}", "b".repeat(32));
        let pty_token = format!("pty-{CAD162_GEN}-{}", "a".repeat(32));
        cad162_turn(&s, &cwd, "dp", ("devin", "pty"), &claude_token);
        cad162_turn(&s, &cwd, "ok", ("devin", "pty"), &pty_token);
        let facts = s.pty_endpoint_facts().unwrap();
        assert_eq!(facts.len(), 2, "{facts:?}");
        let entries = s.shutdown_entries(&facts).unwrap();
        assert_eq!(
            entries.iter().map(|e| e.alias.as_str()).collect::<Vec<_>>(),
            ["ok"]
        );
        assert_eq!(
            cad162_refusals(&s, "dp"),
            ["turn token predates endpoint generation"]
        );
    }

    /// CAD-256: a panic while one caller holds the connection guard
    /// poisons the mutex. The next store call must recover it — not
    /// panic — and leave a `store_poisoned` event on the daemon stream.
    #[test]
    fn a_panic_holding_the_connection_does_not_poison_later_calls() {
        let (_dir, s) = store();
        std::thread::scope(|scope| {
            let crashed = scope
                .spawn(|| {
                    let conn = s.conn();
                    // A raw BEGIN the panic leaves open: recovery must
                    // roll it back, not commit the next caller into it.
                    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
                    panic!("store closure panicked while holding the lock");
                })
                .join();
            assert!(crashed.is_err());
        });
        assert!(s.conn.is_poisoned());

        let events = s.events(Store::DAEMON_STREAM, 0, 100).unwrap();
        assert!(!s.conn.is_poisoned());
        let poisoned: Vec<_> = events
            .iter()
            .filter(|e| e.kind == "store_poisoned")
            .collect();
        assert_eq!(poisoned.len(), 1, "{events:?}");
        assert_eq!(poisoned[0].payload["rolled_back"], true);
        // Later calls take the plain path and record nothing more.
        s.event_public("daemon", "probe", json!({})).unwrap();
        let events = s.events(Store::DAEMON_STREAM, 0, 100).unwrap();
        assert_eq!(
            events.iter().filter(|e| e.kind == "store_poisoned").count(),
            1
        );
    }

    #[test]
    fn store_connections_wait_on_a_busy_database() {
        let (dir, s) = store();
        let timeout: i64 = s
            .conn()
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(timeout, 5000);
        let reader = super::open_read_only(&dir.path().join("t.sqlite3")).unwrap();
        let timeout: i64 = reader
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(timeout, 5000);
    }

    /// Store v15 (CAD-158): an upgraded queue reads every existing row as
    /// normal and keeps its FIFO order; a half-applied v15 converges.
    #[test]
    fn migration_v14_to_v15_adds_priority_and_keeps_fifo() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("t.sqlite3");
        std::fs::create_dir(dir.path().join("w")).unwrap();
        {
            let s = Store::open(&db).unwrap();
            reg(&s, "a1", &dir.path().join("w"));
            s.enqueue("a1", "first", None, "m1", "user").unwrap();
            s.enqueue("a1", "second", None, "m2", "user").unwrap();
        }
        // A genuine v14: no priority column.
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "ALTER TABLE messages DROP COLUMN priority;
                 UPDATE schema_version SET version=14;",
            )
            .unwrap();
        let version = |db: &Path| -> i64 {
            Connection::open(db)
                .unwrap()
                .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
                .unwrap()
        };
        {
            let s = Store::open_for_schema_tests(&db).unwrap();
            assert_eq!(s.message("m1").unwrap().unwrap().priority, Priority::Normal);
            let none: &[String] = &[];
            s.enqueue_steered(
                "a1",
                "urgent",
                None,
                "u1",
                "user",
                None,
                None,
                None,
                &Sender::Unattributed,
                &steer_as_pm(Priority::Urgent, none),
                None,
            )
            .unwrap();
            let order: Vec<String> = (0..3)
                .map(|_| match s.take_queued("a1").unwrap() {
                    Take::Message(m) => m.id,
                    _ => panic!("expected a message"),
                })
                .collect();
            assert_eq!(order, ["u1", "m1", "m2"]);
        }
        assert_eq!(version(&db), crate::rollout::SCHEMA_VERSION);
        // Half-applied: column present, version rolled back.
        Connection::open(&db)
            .unwrap()
            .execute("UPDATE schema_version SET version=14", [])
            .unwrap();
        {
            let s = Store::open_for_schema_tests(&db).unwrap();
            assert_eq!(s.message("u1").unwrap().unwrap().priority, Priority::Urgent);
        }
        assert_eq!(version(&db), crate::rollout::SCHEMA_VERSION);
    }

    /// v16 adds `messages.issue`/`messages.worktree` — the dispatch
    /// lane a kickoff belongs to (CAD-467). Nullable, so a v15 store
    /// migrates in place and old rows read NULL.
    #[test]
    fn migration_v15_to_v16_adds_lane_provenance() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("t.sqlite3");
        std::fs::create_dir(dir.path().join("w")).unwrap();
        {
            let s = Store::open(&db).unwrap();
            reg(&s, "a1", &dir.path().join("w"));
            s.enqueue("a1", "kickoff", None, "k1", "user").unwrap();
        }
        // A genuine v15: no provenance columns.
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "ALTER TABLE messages DROP COLUMN issue;
                 ALTER TABLE messages DROP COLUMN worktree;
                 UPDATE schema_version SET version=15;",
            )
            .unwrap();
        let version = |db: &Path| -> i64 {
            Connection::open(db)
                .unwrap()
                .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
                .unwrap()
        };
        {
            let s = Store::open_for_schema_tests(&db).unwrap();
            let old = s.message("k1").unwrap().unwrap();
            assert_eq!(old.issue, None);
            assert_eq!(old.worktree, None);
            s.enqueue_steered(
                "a1",
                "lane kickoff",
                None,
                "k2",
                "user",
                None,
                Some("D-1"),
                Some("/lane/d-1"),
                &Sender::Unattributed,
                &Steer::NONE,
                None,
            )
            .unwrap();
            let new = s.message("k2").unwrap().unwrap();
            assert_eq!(new.issue.as_deref(), Some("D-1"));
            assert_eq!(new.worktree.as_deref(), Some("/lane/d-1"));
        }
        assert_eq!(version(&db), crate::rollout::SCHEMA_VERSION);
        // Half-applied: one column present, version rolled back.
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "ALTER TABLE messages DROP COLUMN worktree;
                 UPDATE schema_version SET version=15;",
            )
            .unwrap();
        {
            let s = Store::open_for_schema_tests(&db).unwrap();
            assert_eq!(
                s.message("k2").unwrap().unwrap().issue.as_deref(),
                Some("D-1")
            );
        }
        assert_eq!(version(&db), crate::rollout::SCHEMA_VERSION);
    }

    /// v17 adds the platform custody tables (CAD-366, ADR 0006):
    /// `platform_credentials`, `platform_grants`, `platform_defaults`
    /// — handles only, never credential bytes. `IF NOT EXISTS`, so a
    /// v16 store migrates in place and a half-applied v17 converges.
    #[test]
    fn migration_v16_to_v17_adds_platform_tables() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("t.sqlite3");
        std::fs::create_dir(dir.path().join("w")).unwrap();
        {
            let s = Store::open(&db).unwrap();
            reg(&s, "a1", &dir.path().join("w"));
        }
        // A genuine v16: no platform tables.
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "DROP TABLE platform_credentials;
                 DROP TABLE platform_grants;
                 DROP TABLE platform_defaults;
                 UPDATE schema_version SET version=16;",
            )
            .unwrap();
        let has = |db: &Path, table: &str| -> bool {
            Connection::open(db)
                .unwrap()
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get::<_, i64>(0),
                )
                .optional()
                .unwrap()
                .is_some()
        };
        {
            let s = Store::open_for_schema_tests(&db).unwrap();
            for table in [
                "platform_credentials",
                "platform_grants",
                "platform_defaults",
            ] {
                assert!(has(&db, table), "{table} missing after migrate");
            }
            assert!(s.platform_credentials().unwrap().is_empty());
            assert!(s.platform_grants(None).unwrap().is_empty());
            assert!(s.platform_defaults().unwrap().is_empty());
            // The migrated store keeps its pre-v17 rows.
            assert!(s.agent("a1").unwrap().alias == "a1");
        }
        // Half-applied: one table present, version rolled back — the
        // reopen converges.
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "DROP TABLE platform_defaults;
                 UPDATE schema_version SET version=16;",
            )
            .unwrap();
        {
            let _ = Store::open_for_schema_tests(&db).unwrap();
        }
        assert!(has(&db, "platform_defaults"));
    }

    /// v18 adds the pending-effect record and the draft log (CAD-506,
    /// ADR 0006 §5.2/§5.4): `platform_effects` — one staged send keyed
    /// by `effect_id`, its brokered handle UNIQUE — and
    /// `platform_drafts`, the information-only "ran without you" rows.
    /// `IF NOT EXISTS`, so a v17 store migrates in place and a
    /// half-applied v18 converges.
    #[test]
    fn migration_v17_to_v18_adds_effect_tables() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("t.sqlite3");
        std::fs::create_dir(dir.path().join("w")).unwrap();
        {
            let s = Store::open(&db).unwrap();
            reg(&s, "a1", &dir.path().join("w"));
        }
        // A genuine v17: platform custody tables, no effect tables.
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "DROP TABLE platform_effects;
                 DROP TABLE platform_drafts;
                 UPDATE schema_version SET version=17;",
            )
            .unwrap();
        let has = |db: &Path, table: &str| -> bool {
            Connection::open(db)
                .unwrap()
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get::<_, i64>(0),
                )
                .optional()
                .unwrap()
                .is_some()
        };
        {
            let s = Store::open_for_schema_tests(&db).unwrap();
            for table in ["platform_effects", "platform_drafts"] {
                assert!(has(&db, table), "{table} missing after migrate");
            }
            assert!(s.platform_effects(None).unwrap().is_empty());
            assert!(s.platform_drafts(None, 10).unwrap().is_empty());
            // The migrated store keeps its pre-v18 rows.
            assert!(s.agent("a1").unwrap().alias == "a1");
        }
        assert_eq!(
            crate::rollout::SCHEMA_VERSION,
            18,
            "bump? pin the new version and add its migration test"
        );
        // Half-applied: one table present, version rolled back — the
        // reopen converges.
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "DROP TABLE platform_drafts;
                 UPDATE schema_version SET version=17;",
            )
            .unwrap();
        {
            let _ = Store::open_for_schema_tests(&db).unwrap();
        }
        assert!(has(&db, "platform_drafts"));
    }
