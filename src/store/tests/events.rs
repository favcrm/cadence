
    #[test]
    fn approval_evidence_is_idempotent_and_separate_from_delivery() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg(&s, "a1", &cwd);
        let via = "operator-connection";
        let op = "chris via chat";
        assert!(
            s.record_approval(&approval("ap-1", op, SHA40_A, 84), via)
                .unwrap()
                .0
        );
        // Identical retry dedupes; the same id naming another head,
        // source or PR is refused.
        assert!(
            !s.record_approval(&approval("ap-1", op, SHA40_A, 84), via)
                .unwrap()
                .0
        );
        for conflicting in [
            approval("ap-1", op, SHA40_B, 84),
            approval("ap-1", "someone else", SHA40_A, 84),
            approval("ap-1", op, SHA40_A, 85),
        ] {
            let err = s.record_approval(&conflicting, via).unwrap_err();
            assert!(err.to_string().contains("different evidence"), "{err}");
        }
        assert!(
            s.record_approval(&approval("ap-2", op, SHA40_B, 70), via)
                .unwrap()
                .0
        );
        assert!(s.revoke_approval("ap-1", op, "head changed", via).unwrap());
        assert!(!s.revoke_approval("ap-1", op, "head changed", via).unwrap());
        assert!(s.revoke_approval("ap-1", op, "other reason", via).is_err());
        // A revoked id is never reused — not even for identical evidence.
        let err = s
            .record_approval(&approval("ap-1", op, SHA40_A, 84), via)
            .unwrap_err();
        assert!(
            err.to_string().contains("was revoked") && err.to_string().contains("--id"),
            "{err}"
        );
        assert!(s.revoke_approval("missing", op, "no record", via).is_err());
        // `user`/`daemon` are delivery/stream identities, not operators;
        // abbreviated heads, bad scopes and bad ids are refused.
        assert!(s
            .record_approval(&approval("ap-3", "user", SHA40_A, 1), via)
            .is_err());
        assert!(s
            .record_approval(&approval("ap-3", "Daemon", SHA40_A, 1), via)
            .is_err());
        assert!(s
            .record_approval(&approval("ap-3", op, "aaaaaaa", 1), via)
            .is_err());
        assert!(s
            .record_approval(&approval("ap-3", op, SHA40_A, 0), via)
            .is_err());
        assert!(s
            .record_approval(&approval("AP 3", op, SHA40_A, 1), via)
            .is_err());
        let mut bad_repo = approval("ap-3", op, SHA40_A, 1);
        bad_repo.repo = "no-slash";
        assert!(s.record_approval(&bad_repo, via).is_err());

        // A cancelled delivery carrying an approval phrase writes nothing
        // to the approval stream, and the daemon stream's retention prune
        // does not reach it.
        s.enqueue(
            "a1",
            &format!("OPERATOR APPROVED #84 at {SHA40_A}"),
            None,
            "m-cancel",
            "user",
        )
        .unwrap();
        s.cancel("m-cancel", "operator", Some("delivery withdrawn"))
            .unwrap();
        s.prune_stream(Store::DAEMON_STREAM, 0).unwrap();
        assert_eq!(
            approval_rows(&s),
            vec![
                (APPROVAL_RECORDED_EVENT.to_string(), "ap-1".to_string()),
                (APPROVAL_RECORDED_EVENT.to_string(), "ap-2".to_string()),
                (APPROVAL_REVOKED_EVENT.to_string(), "ap-1".to_string()),
            ]
        );
        // The stream name can never be an agent alias, so `agent rm`'s
        // per-alias event delete cannot reach it either.
        assert!(identifier(APPROVAL_STREAM, "alias").is_err());
    }

    /// CAD-217 review: re-approving after a revoke with the default id
    /// records a FRESH approval (`<base>-2`), never a silent duplicate of
    /// the revoked one; identical re-sends of the live one still dedupe.
    #[test]
    fn approval_default_id_counts_past_a_revoke() {
        let (_dir, s) = store();
        let via = "operator-connection";
        let mut a = approval("unused", "op", SHA40_A, 84);
        a.id = None;
        let base = default_approval_id("merge", 84, SHA40_A);
        assert_eq!(base, "merge-pr84-aaaaaaaaaaaa");
        assert_eq!(s.record_approval(&a, via).unwrap(), (true, base.clone()));
        assert_eq!(s.record_approval(&a, via).unwrap(), (false, base.clone()));
        assert!(s.revoke_approval(&base, "op", "head moved", via).unwrap());
        let fresh = format!("{base}-2");
        assert_eq!(s.record_approval(&a, via).unwrap(), (true, fresh.clone()));
        assert_eq!(s.record_approval(&a, via).unwrap(), (false, fresh.clone()));
        // A different source for the same head takes the next free id
        // instead of colliding with the default.
        let mut b = approval("unused", "someone else", SHA40_A, 84);
        b.id = None;
        assert_eq!(
            s.record_approval(&b, via).unwrap(),
            (true, format!("{base}-3"))
        );
        assert_eq!(
            approval_rows(&s),
            vec![
                (APPROVAL_RECORDED_EVENT.to_string(), base.clone()),
                (APPROVAL_REVOKED_EVENT.to_string(), base),
                (APPROVAL_RECORDED_EVENT.to_string(), fresh),
                (
                    APPROVAL_RECORDED_EVENT.to_string(),
                    "merge-pr84-aaaaaaaaaaaa-3".to_string()
                ),
            ]
        );
    }

    // ---------- CAD-316: delivery-event rollup ----------

    const DAY: f64 = 86_400.0;

    /// A message row in `state`, and one raw event per `(kind, age)`
    /// naming it — `at` backdated so the rollup's age cut applies.
    fn delivery(s: &Store, alias: &str, id: &str, state: &str, events: &[(&str, f64)]) {
        s.enqueue(alias, "work", None, id, "user").unwrap();
        let conn = s.conn();
        conn.execute(
            "UPDATE messages SET state=?1 WHERE id=?2",
            params![state, id],
        )
        .unwrap();
        for (kind, age) in events {
            conn.execute(
                "INSERT INTO events(alias,kind,payload,at) VALUES(?1,?2,?3,?4)",
                params![alias, kind, json!({"message": id}).to_string(), now() - age],
            )
            .unwrap();
        }
    }

    /// Every event row, oldest first: `(seq, alias, kind, payload, at)`.
    fn event_rows(s: &Store) -> Vec<(i64, String, String, String, f64)> {
        let conn = s.conn();
        let mut st = conn
            .prepare("SELECT seq, alias, kind, payload, at FROM events ORDER BY seq")
            .unwrap();
        st.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    fn kinds_of(s: &Store, alias: &str) -> Vec<String> {
        event_rows(s)
            .into_iter()
            .filter(|row| row.1 == alias)
            .map(|row| row.2)
            .collect()
    }

    /// Acceptance 1: `submitting`/`submitted` rows older than seven days
    /// become one per-alias row of counts; younger rows stay rows, and a
    /// later pass folds into the same row instead of adding another.
    #[test]
    fn delivery_events_older_than_seven_days_roll_up_into_counts() {
        let (dir, s) = store();
        reg(&s, "a1", &dir.path().join("w"));
        reg(&s, "a2", &dir.path().join("w"));
        delivery(
            &s,
            "a1",
            "m1",
            "completed",
            &[("submitting", 9.0 * DAY), ("submitted", 9.0 * DAY)],
        );
        delivery(
            &s,
            "a1",
            "m2",
            "failed",
            &[("submitting", 8.0 * DAY), ("submitted", 6.0 * DAY)],
        );
        delivery(&s, "a2", "m3", "cancelled", &[("submitting", 10.0 * DAY)]);
        let cutoff = now() - EVENT_ROLLUP_AGE_SECS;
        assert_eq!(EVENT_ROLLUP_AGE_SECS, 7.0 * DAY);

        assert_eq!(
            s.roll_up_delivery_events(cutoff, EVENT_ROLLUP_BATCH)
                .unwrap(),
            4
        );
        let rollup = |alias: &str| -> Vec<Value> {
            event_rows(&s)
                .into_iter()
                .filter(|row| row.1 == alias && row.2 == DELIVERY_ROLLUP_EVENT)
                .map(|row| serde_json::from_str(&row.3).unwrap())
                .collect()
        };
        // The six-day-old `submitted` stays a row; the rest is counts.
        let rest: Vec<String> = kinds_of(&s, "a1")
            .into_iter()
            .filter(|k| ROLLABLE_EVENT_KINDS.contains(&k.as_str()) || k == DELIVERY_ROLLUP_EVENT)
            .collect();
        assert_eq!(rest, vec![DELIVERY_ROLLUP_EVENT, "submitted"]);
        let a1 = rollup("a1");
        assert_eq!(a1.len(), 1, "{a1:?}");
        assert_eq!(a1[0]["counts"], json!({"submitting": 2, "submitted": 1}));
        assert!(a1[0]["last_at"].as_f64().unwrap() < cutoff);
        assert!(a1[0]["first_at"].as_f64().unwrap() <= a1[0]["last_at"].as_f64().unwrap());
        assert_eq!(
            rollup("a2")[0]["counts"],
            json!({"submitting": 1, "submitted": 0})
        );

        // Nothing new is old enough: a second pass is a no-op.
        let before = event_rows(&s);
        assert_eq!(
            s.roll_up_delivery_events(cutoff, EVENT_ROLLUP_BATCH)
                .unwrap(),
            0
        );
        assert_eq!(event_rows(&s), before);

        // A week later the six-day row ages out and folds into the same
        // row — still one rollup per alias, counts summed.
        assert_eq!(
            s.roll_up_delivery_events(now() - DAY, EVENT_ROLLUP_BATCH)
                .unwrap(),
            1
        );
        let a1 = rollup("a1");
        assert_eq!(a1.len(), 1, "{a1:?}");
        assert_eq!(a1[0]["counts"], json!({"submitting": 2, "submitted": 2}));
        assert!(!kinds_of(&s, "a1").iter().any(|k| k == "submitted"));
    }

    /// Acceptance 2: retention deletes by allowlist, never by denylist.
    /// Every other kind — semantic events and any kind added later —
    /// survives however old, and so do delivery rows that are still
    /// evidence: scoped to a job, or naming a message that is live or
    /// `unknown` (unreconciled).
    #[test]
    fn rollup_never_deletes_a_semantic_event() {
        assert_eq!(ROLLABLE_EVENT_KINDS, ["submitting", "submitted"]);
        let (dir, s) = store();
        reg(&s, "a1", &dir.path().join("w"));
        delivery(&s, "a1", "m-done", "completed", &[]);
        let old = now() - 30.0 * DAY;
        {
            let conn = s.conn();
            for kind in [
                "turn_started",
                "turn_finished",
                "turn_unknown",
                "attention",
                "acknowledged",
                "paste_not_rendered",
                "verdict",
                "job_event",
                "task_dispatched",
                "approval_recorded",
                "agent_gc_removed",
                "a_kind_added_later",
            ] {
                conn.execute(
                    "INSERT INTO events(alias,kind,payload,at) VALUES('a1',?1,?2,?3)",
                    params![kind, json!({"message": "m-done"}).to_string(), old],
                )
                .unwrap();
            }
            // A delivery row a job view reads stays in that view.
            conn.execute(
                "INSERT INTO events(alias,kind,payload,job_id,task_id,at)
                 VALUES('a1','submitted',?1,'job-1','job-1-impl',?2)",
                params![json!({"message": "m-done"}).to_string(), old],
            )
            .unwrap();
        }
        for (id, state) in [
            ("m-queued", "queued"),
            ("m-submitting", "submitting"),
            ("m-running", "running"),
            ("m-unknown", "unknown"),
        ] {
            delivery(
                &s,
                "a1",
                id,
                state,
                &[("submitting", 30.0 * DAY), ("submitted", 30.0 * DAY)],
            );
        }
        let before = event_rows(&s);
        assert_eq!(
            s.roll_up_delivery_events(now(), EVENT_ROLLUP_BATCH)
                .unwrap(),
            0
        );
        assert_eq!(event_rows(&s), before);
    }

    /// QA revise (item 3): one pass folds at most `limit` rows, oldest
    /// first, and the next pass continues into the same rollup rows —
    /// a long-lived store's backlog drains in bounded lock holds.
    #[test]
    fn rollup_pass_is_capped_and_the_next_pass_continues() {
        let (dir, s) = store();
        reg(&s, "a1", &dir.path().join("w"));
        reg(&s, "a2", &dir.path().join("w"));
        for i in 0..4 {
            let age = (20 - i) as f64 * DAY;
            delivery(
                &s,
                "a1",
                &format!("m1-{i}"),
                "completed",
                &[("submitting", age), ("submitted", age)],
            );
            delivery(
                &s,
                "a2",
                &format!("m2-{i}"),
                "completed",
                &[("submitting", age)],
            );
        }
        let cutoff = now() - EVENT_ROLLUP_AGE_SECS;
        let delivery_left = |s: &Store| {
            event_rows(s)
                .into_iter()
                .filter(|row| ROLLABLE_EVENT_KINDS.contains(&row.2.as_str()))
                .count()
        };
        assert_eq!(delivery_left(&s), 12);
        assert_eq!(s.roll_up_delivery_events(cutoff, 5).unwrap(), 5);
        assert_eq!(delivery_left(&s), 7);
        // Oldest first: the first pass took the 20- and 19-day rows.
        let oldest_left = event_rows(&s)
            .into_iter()
            .filter(|row| ROLLABLE_EVENT_KINDS.contains(&row.2.as_str()))
            .map(|row| row.4)
            .fold(f64::INFINITY, f64::min);
        assert!(oldest_left > now() - 19.0 * DAY - 60.0, "{oldest_left}");
        assert_eq!(s.roll_up_delivery_events(cutoff, 5).unwrap(), 5);
        assert_eq!(s.roll_up_delivery_events(cutoff, 5).unwrap(), 2);
        assert_eq!(s.roll_up_delivery_events(cutoff, 5).unwrap(), 0);
        assert_eq!(delivery_left(&s), 0);
        let counts = |alias: &str| -> Vec<Value> {
            event_rows(&s)
                .into_iter()
                .filter(|row| row.1 == alias && row.2 == DELIVERY_ROLLUP_EVENT)
                .map(|row| serde_json::from_str::<Value>(&row.3).unwrap()["counts"].clone())
                .collect()
        };
        assert_eq!(counts("a1"), vec![json!({"submitting": 4, "submitted": 4})]);
        assert_eq!(counts("a2"), vec![json!({"submitting": 4, "submitted": 0})]);
    }

    fn publish_grant() -> Vec<super::platform::Derived> {
        vec![(
            "dev-1".to_string(),
            "local".to_string(),
            "local".to_string(),
            vec!["publish".to_string()],
        )]
    }

    /// A reconcile that still holds the pre-revoke digest must not put
    /// the grant back (CAD-577 review 344, note 3).
    #[test]
    fn stale_reconcile_after_revoke_does_not_restore_the_grant() {
        let (_dir, s) = store();
        let grants = publish_grant();
        s.record_app_approval(json!({
            "project": "demo", "name": "roles",
            "digest": "sha256:abc", "by": "operator",
        }))
        .unwrap();
        s.app_grants_set("demo/roles", &grants, "operator").unwrap();
        s.app_revoke_with_record(
            json!({
                "project": "demo", "name": "roles",
                "digest": Value::Null, "revoked": true, "by": "operator",
            }),
            "demo/roles",
            "operator",
        )
        .unwrap();
        s.app_grants_reconcile("demo/roles", Some("sha256:abc"), &grants, "operator")
            .unwrap();
        assert!(s
            .platform_grant("dev-1", "local", "local")
            .unwrap()
            .is_none());
    }

    /// Revoke and reconcile share the write lock. Whichever runs second
    /// sees the other's commit, so the grant does not survive.
    #[test]
    fn concurrent_revoke_and_reconcile_leave_the_grant_revoked() {
        use std::sync::Arc;
        use std::thread;
        let (_dir, s) = store();
        let s = Arc::new(s);
        let grants = publish_grant();
        for _ in 0..32 {
            s.record_app_approval(json!({
                "project": "demo", "name": "roles",
                "digest": "sha256:abc", "by": "operator",
            }))
            .unwrap();
            s.app_grants_set("demo/roles", &grants, "operator").unwrap();
            let a = Arc::clone(&s);
            let g = grants.clone();
            let t1 = thread::spawn(move || {
                a.app_grants_reconcile("demo/roles", Some("sha256:abc"), &g, "operator")
                    .unwrap();
            });
            let b = Arc::clone(&s);
            let t2 = thread::spawn(move || {
                b.app_revoke_with_record(
                    json!({
                        "project": "demo", "name": "roles",
                        "digest": Value::Null, "revoked": true, "by": "operator",
                    }),
                    "demo/roles",
                    "operator",
                )
                .unwrap();
            });
            t1.join().unwrap();
            t2.join().unwrap();
            assert!(
                s.platform_grant("dev-1", "local", "local")
                    .unwrap()
                    .is_none(),
                "a reconcile that raced a revoke must not leave the grant"
            );
        }
    }
