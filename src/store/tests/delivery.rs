
    #[test]
    fn finish_routes_result_in_one_transaction() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        for alias in ["pm", "w1"] {
            reg(&s, alias, &cwd);
        }
        s.enqueue("w1", "do it", Some("pm"), "m1", "user").unwrap();
        let m = match s.take_queued("w1").unwrap() {
            Take::Message(m) => m,
            _ => panic!(),
        };
        s.mark_running("m1", "turn-1").unwrap();
        s.finish(
            &m,
            "completed",
            &json!({"status":"completed","text":"done"}),
            None,
        )
        .unwrap();
        let pm_msgs = s.messages("pm").unwrap();
        assert_eq!(pm_msgs.len(), 1);
        assert_eq!(pm_msgs[0].source, "worker_result");
        assert!(pm_msgs[0].reply_to.is_none());
        // Deterministic delivery id: resending produces the same id.
        let expected = Uuid::new_v5(&Uuid::NAMESPACE_URL, b"cadence-result:m1")
            .simple()
            .to_string();
        assert_eq!(pm_msgs[0].id, expected);
    }

    #[test]
    fn finish_route_identity_survives_timestamp_json_ulp() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        for alias in ["pm", "w1"] {
            reg(&s, alias, &cwd);
        }
        s.enqueue("w1", "do it", Some("pm"), "m1", "user").unwrap();

        // A JSON number at epoch scale can round the same SQLite REAL to
        // the adjacent f64 when it is serialized and parsed again. Recreate
        // that harmless presentation drift in the queued binding; the exact
        // created_bits proof must still admit the original recipient.
        let (seq, payload): (i64, String) = {
            let conn = s.conn();
            conn.query_row(
                "SELECT seq,payload FROM events
                 WHERE alias='w1' AND kind='queued' ORDER BY seq DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
        };
        let mut payload: Value = serde_json::from_str(&payload).unwrap();
        let created = payload["recipient_identity"]["created"].as_f64().unwrap();
        payload["recipient_identity"]["created"] =
            json!(f64::from_bits(created.to_bits().wrapping_add(1)));
        {
            let conn = s.conn();
            conn.execute(
                "UPDATE events SET payload=? WHERE seq=?",
                rusqlite::params![payload.to_string(), seq],
            )
            .unwrap();
        }

        let m = match s.take_queued("w1").unwrap() {
            Take::Message(m) => m,
            _ => panic!("expected worker message"),
        };
        s.mark_running("m1", "turn-1").unwrap();
        s.finish(
            &m,
            "completed",
            &json!({"status": "completed", "text": "done"}),
            None,
        )
        .unwrap();
        assert_eq!(s.messages("pm").unwrap().len(), 1);
    }

    /// CAD-304 S2: `--force` finishes open work through the normal
    /// paths — a running kickoff ends `interrupted` with its result
    /// routed to `reply_to`, a queued message is cancelled with a
    /// `cancelled` notice to its `reply_to` — and names who it told.
    #[test]
    fn forced_remove_finishes_open_work_through_the_finish_path() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let kickoff = seeded_task(&s, &cwd);
        run_kickoff(&s, &kickoff);
        s.enqueue("w1", "later", Some("pm"), "m-queued", "user")
            .unwrap();
        s.set_state_detached("w1", "stopped", None).unwrap();

        let notified = s
            .remove_agent("w1", true, &json!({"by": "pm", "by_kind": "agent"}))
            .unwrap();
        assert_eq!(notified, vec!["pm".to_string()]);
        let k = s.message(&kickoff).unwrap().unwrap();
        assert_eq!(k.state, "interrupted");
        let result = k.result.clone().unwrap();
        assert_eq!(result["via"], "agent_remove_forced", "{result}");
        assert_eq!(result["by"], "pm", "{result}");
        // The unattached queued row is pruned with the rest of w1's
        // unreferenced history; its notice already reached pm.
        assert!(s.message("m-queued").unwrap().is_none());
        let pm = s.messages("pm").unwrap();
        let routed = |id: String| pm.iter().find(|m| m.id == id).cloned();
        let result_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("cadence-result:{kickoff}").as_bytes(),
        )
        .simple()
        .to_string();
        let r = routed(result_id).expect("interrupted result routed to pm");
        assert_eq!(r.source, "worker_result");
        assert!(r.body.contains("interrupted"), "{}", r.body);
        let notice_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, b"cadence-notice:cancelled:m-queued")
            .simple()
            .to_string();
        let n = routed(notice_id).expect("cancelled notice routed to pm");
        assert_eq!(n.source, "worker_notice");
        let events = s.events(Store::DAEMON_STREAM, 0, 100).unwrap();
        let forced = events
            .iter()
            .find(|e| e.kind == "agent_remove_forced")
            .unwrap();
        assert_eq!(forced.payload["notified"], json!(["pm"]));
        assert_eq!(forced.payload["unassigned"], json!(["t1"]));
        assert_eq!(forced.payload["by"], "pm");
        // The task keeps its state and revision; only the assignee goes.
        let t = s.task("t1").unwrap();
        assert_eq!(t.assignee, None);
        assert_eq!(t.state, "running");
        assert_eq!(forced.payload["by_kind"], "agent");
        let removed = events.iter().find(|e| e.kind == "agent_removed").unwrap();
        assert_eq!(removed.payload["alias"], "w1");
        assert_eq!(removed.payload["force"], true);
        assert_eq!(removed.payload["by"], "pm");
    }

    /// CAD-304 S1 (qa nit): an unconfirmed nudge left `unknown` refuses
    /// `--force` like any unknown, and the remedy names only `message
    /// reconcile` — `agent unfence` does not reconcile a nudge.
    #[test]
    fn forced_remove_refuses_unknown_and_names_a_working_remedy() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg(&s, "w1", &cwd);
        s.enqueue("w1", "look", None, "m-nudge", "nudge").unwrap();
        s.enqueue("w1", "task", None, "m-work", "user").unwrap();
        s.set_state_detached("w1", "stopped", None).unwrap();
        s.conn()
            .execute("UPDATE messages SET state='unknown' WHERE id='m-nudge'", [])
            .unwrap();
        let err = s
            .remove_agent("w1", true, &operator_by())
            .unwrap_err()
            .to_string();
        assert!(err.contains("message reconcile m-nudge"), "{err}");
        assert!(!err.contains("agent unfence"), "{err}");
        assert_eq!(s.message("m-nudge").unwrap().unwrap().state, "unknown");
        assert_eq!(s.message("m-work").unwrap().unwrap().state, "queued");
        // A fencing unknown also offers unfence.
        s.conn()
            .execute("UPDATE messages SET state='unknown' WHERE id='m-work'", [])
            .unwrap();
        let err = s
            .remove_agent("w1", true, &operator_by())
            .unwrap_err()
            .to_string();
        assert!(err.contains("agent unfence w1 --no-resume"), "{err}");
        assert!(s.agent("w1").is_ok());
    }

    /// CAD-304 S4: an alias registered again after `agent remove` does
    /// not list, gate on or fence on the rows job history kept under
    /// the old alias — while `message`/`job` reads still resolve them.
    #[test]
    fn re_registered_alias_does_not_inherit_kept_rows() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let kickoff = seeded_task(&s, &cwd);
        let m = run_kickoff(&s, &kickoff);
        s.finish(
            &m,
            "completed",
            &json!({"status": "completed", "text": "done", "sha": SHA40_A}),
            None,
        )
        .unwrap();
        s.set_state_detached("w1", "stopped", None).unwrap();
        // The task is in review — open work, so the removal is forced.
        s.remove_agent("w1", true, &operator_by()).unwrap();
        assert_eq!(s.message(&kickoff).unwrap().unwrap().state, "completed");

        s.register_agent(&NewAgent {
            alias: "w1",
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: cwd.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: Some(&json!({"upstream": "pm"}).to_string()),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
        assert!(s.messages("w1").unwrap().is_empty());
        assert!(!s.has_unknown("w1").unwrap());
        // Its task was unassigned by the forced removal (review round-2
        // ruling): the new agent carries no task, so nothing gates on it
        // — and the PM was told which task to reassign.
        assert!(s.tasks_for_assignee("w1").unwrap().is_empty());
        let t = s.task("t1").unwrap();
        assert_eq!((t.assignee.as_deref(), t.state.as_str()), (None, "review"));
        assert!(s
            .messages("pm")
            .unwrap()
            .iter()
            .any(|m| m.source == "job_event" && m.body.contains("is unassigned")));
        let kinds: Vec<String> = s
            .job_events("j1", 0, 100)
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert!(kinds.iter().any(|k| k == "task_unassigned"), "{kinds:?}");
        // The old worker cannot verdict the revision it produced, even
        // unassigned.
        let err = s
            .record_verdict("t1", SHA40_A, "pass", "w1", None, None, None, None, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("ran this revision's kickoff"), "{err}");
        assert_eq!(s.queued_count("w1").unwrap(), 0);
        assert!(matches!(s.take_queued("w1").unwrap(), Take::Empty));
        // The kept kickoff still resolves by id.
        assert_eq!(s.message(&kickoff).unwrap().unwrap().alias, "w1");
        // The new agent's own traffic lists as usual.
        s.enqueue("w1", "fresh", None, "fresh-1", "user").unwrap();
        let ids: Vec<String> = s
            .messages("w1")
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, vec!["fresh-1".to_string()]);
        // Nothing old refuses a plain removal of the new agent.
        s.set_state_detached("w1", "stopped", None).unwrap();
        s.conn()
            .execute(
                "UPDATE messages SET state='completed' WHERE id='fresh-1'",
                [],
            )
            .unwrap();
        s.remove_agent("w1", false, &operator_by()).unwrap();
    }

    /// `agent_remove_forced.notified` names only recipients a notice
    /// actually reached (review N4): a `reply_to` whose agent is gone
    /// gets a `handoff_unresolved` instead, and is not listed.
    #[test]
    fn forced_remove_notified_lists_only_delivered_recipients() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg(&s, "w1", &cwd);
        reg(&s, "gone", &cwd);
        s.enqueue("w1", "later", Some("gone"), "m-q", "user")
            .unwrap();
        s.set_state_detached("gone", "stopped", None).unwrap();
        s.remove_agent("gone", false, &operator_by()).unwrap();
        s.set_state_detached("w1", "stopped", None).unwrap();
        let notified = s.remove_agent("w1", true, &operator_by()).unwrap();
        assert!(notified.is_empty(), "{notified:?}");
        let events = s.events(Store::DAEMON_STREAM, 0, 100).unwrap();
        let forced = events
            .iter()
            .find(|e| e.kind == "agent_remove_forced")
            .unwrap();
        assert_eq!(forced.payload["notified"], json!([]));
    }

    #[test]
    fn finish_survives_removed_recipient_across_restart_and_dedupes_failure() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let kickoff = seeded_task(&s, &cwd);
        let m = run_kickoff(&s, &kickoff);

        s.set_state_detached("pm", "stopped", None).unwrap();
        s.remove_agent(
            "pm",
            false,
            &json!({"by": "operator", "by_kind": "operator"}),
        )
        .unwrap();
        let result = json!({"status": "completed", "text": "done", "sha": SHA40_A});
        s.finish(&m, "completed", &result, None).unwrap();
        // A repeated completion is an idempotent replay: it must not add a
        // second unresolved route event or roll back the terminal write.
        s.finish(&m, "completed", &result, None).unwrap();

        assert_eq!(s.message(&kickoff).unwrap().unwrap().state, "completed");
        assert_eq!(s.task("t1").unwrap().state, "review");
        let unresolved: Vec<_> = s
            .job_events("j1", 0, 100)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == "handoff_unresolved")
            .collect();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].payload["reason"], "recipient_missing");
        assert_eq!(unresolved[0].task_id.as_deref(), Some("t1"));

        drop(s);
        let s2 = Store::open(&dir.path().join("t.sqlite3")).unwrap();
        reg(&s2, "pm", &cwd);
        // Re-registering an alias never replays a sensitive old result.
        assert!(s2.messages("pm").unwrap().is_empty());
        let after_restart: Vec<_> = s2
            .job_events("j1", 0, 100)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == "handoff_unresolved")
            .collect();
        assert_eq!(after_restart.len(), 1);
    }

    #[test]
    fn finish_refuses_re_registered_alias_with_changed_identity() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let kickoff = seeded_task(&s, &cwd);
        let m = run_kickoff(&s, &kickoff);

        s.set_state_detached("pm", "stopped", None).unwrap();
        s.remove_agent(
            "pm",
            false,
            &json!({"by": "operator", "by_kind": "operator"}),
        )
        .unwrap();
        reg(&s, "pm", &cwd);
        s.finish(
            &m,
            "completed",
            &json!({"status": "completed", "text": "done", "sha": SHA40_A}),
            None,
        )
        .unwrap();

        assert!(s.messages("pm").unwrap().is_empty());
        let event = s
            .job_events("j1", 0, 100)
            .unwrap()
            .into_iter()
            .find(|event| event.kind == "handoff_unresolved")
            .expect("alias reuse must leave an unresolved route record");
        assert_eq!(event.payload["reason"], "recipient_identity_changed");
        assert_ne!(
            event.payload["expected_identity"]["created"],
            event.payload["current_identity"]["created"]
        );
        assert_eq!(s.task("t1").unwrap().state, "review");
    }

    #[test]
    fn queued_routed_result_refuses_endpoint_generation_change() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg(&s, "pm", &cwd);
        reg(&s, "w1", &cwd);
        s.set_identity(
            "pm",
            &crate::adapter::Identity {
                thread_id: "thread-1".into(),
                session_id: "session-1".into(),
                model: Some("model-1".into()),
                effort: None,
                pid: 1,
                endpoint: Some("fake://one".into()),
                generation: Some("generation-1".into()),
                attach: None,
            },
        )
        .unwrap();
        s.enqueue("w1", "do it", Some("pm"), "m1", "user").unwrap();
        let m = match s.take_queued("w1").unwrap() {
            Take::Message(m) => m,
            _ => panic!("expected worker message"),
        };
        s.mark_running("m1", "turn-1").unwrap();
        s.finish(
            &m,
            "completed",
            &json!({"status": "completed", "text": "done"}),
            None,
        )
        .unwrap();
        let delivery = Uuid::new_v5(&Uuid::NAMESPACE_URL, b"cadence-result:m1")
            .simple()
            .to_string();
        assert_eq!(s.message(&delivery).unwrap().unwrap().state, "queued");

        s.set_identity(
            "pm",
            &crate::adapter::Identity {
                thread_id: "thread-2".into(),
                session_id: "session-2".into(),
                model: Some("model-1".into()),
                effort: None,
                pid: 2,
                endpoint: Some("fake://two".into()),
                generation: Some("generation-2".into()),
                attach: None,
            },
        )
        .unwrap();
        assert!(matches!(s.take_queued("pm").unwrap(), Take::Empty));
        let failed = s.message(&delivery).unwrap().unwrap();
        assert_eq!(failed.state, "failed");
        assert_eq!(failed.result.unwrap()["via"], "handoff_unresolved");
        assert!(s.events("daemon", 0, 100).unwrap().iter().any(|event| {
            event.kind == "handoff_unresolved"
                && event.payload["reason"] == "recipient_identity_changed"
        }));
    }

    #[test]
    fn missing_job_event_recipient_is_durable_and_not_replayed() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let _kickoff = seeded_task(&s, &cwd);
        s.set_state_detached("pm", "stopped", None).unwrap();
        s.remove_agent(
            "pm",
            false,
            &json!({"by": "operator", "by_kind": "operator"}),
        )
        .unwrap();

        s.job_notice("t1", "running", "stall:1", "worker is quiet")
            .unwrap();
        let event = s
            .job_events("j1", 0, 100)
            .unwrap()
            .into_iter()
            .find(|event| event.kind == "handoff_unresolved")
            .expect("missing PM notification must be durable");
        assert_eq!(event.payload["source"], "job_event");
        assert_eq!(event.payload["reason"], "recipient_missing");
        assert_eq!(event.task_id.as_deref(), Some("t1"));

        reg(&s, "pm", &cwd);
        s.job_notice("t1", "running", "stall:1", "worker is quiet")
            .unwrap();
        assert!(s.messages("pm").unwrap().is_empty());
    }

    #[test]
    fn reconcile_completed_behaves_like_finish() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let kickoff = seeded_task(&s, &cwd);
        let m = run_kickoff(&s, &kickoff);
        // Fence it, then operator-reconcile to completed with --sha.
        s.finish(&m, "unknown", &json!({"status": "unknown"}), Some("lost"))
            .unwrap();
        assert_eq!(s.task("t1").unwrap().state, "running"); // untouched
        s.reconcile(
            &kickoff,
            "completed",
            Some("verified by hand"),
            "operator",
            Some(SHA40_A),
        )
        .unwrap();
        let t = s.task("t1").unwrap();
        assert_eq!(t.state, "review");
        assert_eq!(t.head_sha.as_deref(), Some(SHA40_A));
    }

    #[test]
    fn held_unknown_routes_a_live_notice() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg(&s, "w1", &cwd);
        reg(&s, "pm", &cwd);
        s.enqueue("w1", "work", Some("pm"), "m-held", "user")
            .unwrap();
        let held = run_kickoff(&s, "m-held");
        s.finish(
            &held,
            "unknown",
            &json!({"status": "unknown", "text": "", "error": "poll held", "held": true}),
            Some("poll held"),
        )
        .unwrap();
        assert!(s
            .events("w1", 0, 40)
            .unwrap()
            .iter()
            .any(|event| event.kind == "notice_routed"));
        let notices = s.messages("pm").unwrap();
        assert!(
            notices.iter().any(|message| {
                message.body.contains("held") && message.body.contains("not fenced")
            }),
            "{notices:?}"
        );
        assert!(notices
            .iter()
            .all(|message| !message.body.contains("the worker is fenced")));
        s.enqueue("w1", "again", Some("pm"), "m-fence", "user")
            .unwrap();
        let fenced = run_kickoff(&s, "m-fence");
        s.finish(
            &fenced,
            "unknown",
            &json!({"status": "unknown", "text": "", "error": "lost"}),
            Some("lost"),
        )
        .unwrap();
        assert!(s
            .messages("pm")
            .unwrap()
            .iter()
            .any(|message| message.body.contains("the worker is fenced")));
    }

    #[test]
    fn unknown_event_reason_redacts_bounds_and_drops_controls() {
        let secret = format!("{:032x}", 0x9f8e7d6c5b4a3f2e1d0c9b8a7f6e5d4c_u128);
        let redacted = unknown_event_reason(
            Some(&format!("provider dropped --token={secret} mid-turn")),
            &json!({}),
        );
        assert!(!redacted.contains(secret.as_str()), "{redacted}");
        assert!(redacted.contains("[REDACTED]"), "{redacted}");
        assert!(!redacted.chars().any(char::is_control), "{redacted}");

        let noisy =
            unknown_event_reason(Some(&format!("lost\u{1}connection {secret}")), &json!({}));
        assert!(!noisy.contains('\u{1}'), "{noisy}");
        assert!(!noisy.contains(secret.as_str()), "{noisy}");

        assert_eq!(
            unknown_event_reason(Some("   "), &json!({"error": "  "})),
            crate::daemon::UNKNOWN_GENERIC_REASON
        );
        assert_eq!(
            unknown_event_reason(None, &json!({})),
            crate::daemon::UNKNOWN_GENERIC_REASON
        );
        let from_result =
            unknown_event_reason(None, &json!({"error": format!("see --token={secret}")}));
        assert!(!from_result.contains(secret.as_str()), "{from_result}");
        assert!(from_result.contains("[REDACTED]"), "{from_result}");

        let bounded = unknown_event_reason(Some(&"a".repeat(600)), &json!({}));
        assert!(bounded.ends_with('…'), "{bounded}");
        assert_eq!(bounded.chars().count(), UNKNOWN_EVENT_REASON_CHARS + 1);
    }

    #[test]
    fn finish_unknown_emits_one_scoped_event_and_skips_unscoped() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let kickoff = seeded_task(&s, &cwd);
        let message = run_kickoff(&s, &kickoff);
        let secret = format!("{:032x}", 0x9f8e7d6c5b4a3f2e1d0c9b8a7f6e5d4c_u128);
        let reason = format!("dropped --token={secret}");
        s.finish(
            &message,
            "unknown",
            &json!({"status": "unknown", "text": "", "error": reason}),
            Some(&reason),
        )
        .unwrap();
        assert_eq!(s.task("t1").unwrap().state, "running");
        assert!(s.task("t1").unwrap().head_sha.is_none());
        assert_eq!(s.message(&kickoff).unwrap().unwrap().state, "unknown");
        let unknown: Vec<_> = s
            .job_events("j1", 0, 50)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == "turn_unknown")
            .collect();
        assert_eq!(unknown.len(), 1, "{unknown:?}");
        assert_eq!(unknown[0].job_id.as_deref(), Some("j1"));
        assert_eq!(unknown[0].task_id.as_deref(), Some("t1"));
        assert_eq!(unknown[0].payload["message"], kickoff);
        assert_eq!(unknown[0].payload["owner"], "operator");
        let stored_reason = unknown[0].payload["reason"].as_str().unwrap();
        assert!(!stored_reason.contains(secret.as_str()), "{stored_reason}");
        assert!(stored_reason.contains("[REDACTED]"), "{stored_reason}");
        assert!(unknown[0].payload["next_action"]
            .as_str()
            .unwrap()
            .contains(&kickoff));
        assert!(s
            .events("w1", 0, 80)
            .unwrap()
            .iter()
            .any(|event| event.kind == "turn_finished"));

        s.create_task("j1", "t2", None, Some("w1"), None, None, None, None, None)
            .unwrap();
        let (_, kick2, ..) = s.dispatch_task("t2", None, None, "test").unwrap();
        let completed = run_kickoff(&s, &kick2);
        s.finish(
            &completed,
            "completed",
            &json!({"status": "completed", "text": "done", "sha": SHA40_A}),
            None,
        )
        .unwrap();
        assert_eq!(s.task("t2").unwrap().state, "review");
        assert_eq!(
            s.job_events("j1", 0, 80)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "turn_unknown")
                .count(),
            1
        );

        s.enqueue("w1", "plain", None, "plain", "user").unwrap();
        let plain = run_kickoff(&s, "plain");
        s.finish(
            &plain,
            "unknown",
            &json!({"status": "unknown", "error": "no task"}),
            Some("no task"),
        )
        .unwrap();
        s.enqueue("w1", "dangling", None, "dangling", "user")
            .unwrap();
        let mut dangling = run_kickoff(&s, "dangling");
        dangling.task_id = Some("missing-task".into());
        s.finish(
            &dangling,
            "unknown",
            &json!({"status": "unknown", "error": "missing task"}),
            Some("missing task"),
        )
        .unwrap();
        assert_eq!(
            s.job_events("j1", 0, 80)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "turn_unknown")
                .count(),
            1
        );
        assert_eq!(s.task("t1").unwrap().state, "running");
    }

    #[test]
    fn interrupted_and_failed_kickoffs_legalize_redispatch() {
        for status in ["interrupted", "failed"] {
            let (dir, s) = store();
            let cwd = dir.path().join("w");
            let kickoff = seeded_task(&s, &cwd);
            let m = run_kickoff(&s, &kickoff);
            s.finish(&m, status, &json!({"status": status}), Some("x"))
                .unwrap();
            // Task untouched (still running), dispatch starts r2.
            assert_eq!(s.task("t1").unwrap().state, "running", "{status}");
            let (t, k2, dup, _) = s.dispatch_task("t1", None, None, "test").unwrap();
            assert!(!dup, "{status}");
            assert_eq!(t.revision, 2, "{status}");
            assert_ne!(k2, kickoff);
            // Old rows preserved: r1 kickoff + events intact.
            assert!(s.message(&kickoff).unwrap().is_some());
            let evs = s.job_events("j1", 0, 50).unwrap();
            assert!(evs.iter().filter(|e| e.kind == "task_dispatched").count() >= 2);
        }
    }

    /// CAD-284: the timer prunes like `agent remove` — the kickoff a
    /// task references and job-scoped events outlive the row, an
    /// unattached completed message does not.
    #[test]
    fn timer_gc_remove_keeps_job_history() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let kickoff = seeded_task(&s, &cwd);
        let m = run_kickoff(&s, &kickoff);
        s.finish(
            &m,
            "completed",
            &json!({"status": "completed", "text": "done", "sha": SHA40_A}),
            None,
        )
        .unwrap();
        s.enqueue("w1", "chat", None, "m-chat", "user").unwrap();
        s.conn()
            .execute(
                "UPDATE messages SET state='completed' WHERE id='m-chat'",
                [],
            )
            .unwrap();
        s.conn()
            .execute(
                "UPDATE agents SET state='stopped', enabled=0, endpoint=NULL,
                 updated=? WHERE alias='w1'",
                params![now() - 30.0 * 86_400.0],
            )
            .unwrap();

        assert!(s.timer_gc_remove("w1", 86_400.0).unwrap().is_some());
        assert!(s.agent_opt("w1").unwrap().is_none());
        assert_eq!(s.message(&kickoff).unwrap().unwrap().state, "completed");
        assert!(s.message("m-chat").unwrap().is_none());
        assert!(s
            .job_events("j1", 0, 100)
            .unwrap()
            .iter()
            .any(|e| e.alias == "w1" && e.kind == "task_running"));
    }

    /// CAD-199: the timer's guarded removal — the manual candidate rule
    /// plus not-enabled and no open-or-unknown message, re-checked in
    /// the removing transaction, with one `agent_gc_removed` event per
    /// removed row on the daemon stream and none for a kept row.
    #[test]
    fn timer_gc_remove_keeps_open_unknown_enabled_and_young_rows() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let week = 7.0 * 86_400.0;
        let aged = now() - 30.0 * 86_400.0;
        for alias in [
            "old", "done", "queued", "running", "unknown", "future", "enabled", "young", "live",
        ] {
            reg(&s, alias, &cwd);
            s.conn()
                .execute(
                    "UPDATE agents SET state='stopped', enabled=0, endpoint=NULL,
                     updated=? WHERE alias=?",
                    params![aged, alias],
                )
                .unwrap();
        }
        // Terminal history does not keep a row; every other state does,
        // including one the store has never heard of.
        for (alias, id, state) in [
            ("done", "m-done", "completed"),
            ("queued", "m-q", "queued"),
            ("running", "m-r", "running"),
            ("unknown", "m-u", "unknown"),
            ("future", "m-f", "awaiting_report"),
        ] {
            s.enqueue(alias, "work", None, id, "user").unwrap();
            s.conn()
                .execute("UPDATE messages SET state=? WHERE id=?", params![state, id])
                .unwrap();
        }
        let c = s.conn();
        c.execute("UPDATE agents SET enabled=1 WHERE alias='enabled'", [])
            .unwrap();
        c.execute(
            "UPDATE agents SET updated=? WHERE alias='young'",
            params![now() - 2.0 * 86_400.0],
        )
        .unwrap();
        c.execute("UPDATE agents SET endpoint='sock' WHERE alias='live'", [])
            .unwrap();
        drop(c);

        let mut removed = Vec::new();
        for agent in s.gc_candidates(Some(week)).unwrap() {
            if let Some(gone) = s.timer_gc_remove(&agent.alias, week).unwrap() {
                removed.push(gone.alias);
            }
        }
        removed.sort();
        assert_eq!(removed, vec!["done", "old"]);
        for kept in [
            "queued", "running", "unknown", "future", "enabled", "young", "live",
        ] {
            assert!(s.agent_opt(kept).unwrap().is_some(), "{kept} was removed");
        }
        // An alias that is gone (or never existed) is simply not eligible.
        assert!(s.timer_gc_remove("old", week).unwrap().is_none());

        let events: Vec<Event> = s
            .events(Store::DAEMON_STREAM, 0, 50)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "agent_gc_removed")
            .collect();
        let mut aliases: Vec<&str> = events
            .iter()
            .map(|e| e.payload["alias"].as_str().unwrap())
            .collect();
        aliases.sort();
        assert_eq!(aliases, vec!["done", "old"]);
        for e in &events {
            assert_eq!(e.payload["records_only"], true, "{}", e.payload);
            assert_eq!(e.payload["older_than_secs"], week, "{}", e.payload);
            assert!(e.payload["age_secs"].as_f64().unwrap() >= 30.0 * 86_400.0 - 60.0);
            let reason = e.payload["reason"].as_str().unwrap();
            assert!(reason.contains("state stopped"), "{reason}");
            let note = e.payload["note"].as_str().unwrap();
            assert!(note.contains("frees no memory and no disk"), "{note}");
            assert!(note.contains("can no longer be resumed"), "{note}");
        }
    }

    // ---- CAD-158: priority steering and supersession ----
