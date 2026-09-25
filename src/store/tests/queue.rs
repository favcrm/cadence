use super::*;

    #[test]
    fn enqueue_idempotency_and_conflict() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg(&s, "a1", &cwd);
        let (dup, _) = s.enqueue("a1", "hello", None, "m1", "user").unwrap();
        assert!(!dup);
        let (dup, state) = s.enqueue("a1", "hello", None, "m1", "user").unwrap();
        assert!(dup && state == "queued");
        assert!(s.enqueue("a1", "different", None, "m1", "user").is_err());
    }

    /// CAD-468: `enqueue_daemon` admits exactly the pairs the daemon
    /// mints — `sys-<source>-…` for a daemon source (`wake`, and `nudge`
    /// for the silent-end report reminder) — and refuses every crossing:
    /// a caller id under a daemon source, a daemon id under a caller
    /// source, a daemon id whose kind segment is not the source, and a
    /// routed source. Callers stay fenced the other way by
    /// `proto::caller_message` — no `sys-` id, no `wake` source.
    #[test]
    fn enqueue_daemon_admits_only_matching_daemon_pairs() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg(&s, "a1", &cwd);
        // The reminder pair the daemon mints.
        let (dup, _) = s
            .enqueue_daemon("a1", "report it", "sys-nudge-0123456789abcdef", "nudge")
            .unwrap();
        assert!(!dup);
        let (dup, _) = s
            .enqueue_daemon("a1", "report it", "sys-nudge-0123456789abcdef", "nudge")
            .unwrap();
        assert!(dup, "the same daemon id dedupes");
        // And the wake pair.
        s.enqueue_daemon("a1", "wake", "sys-wake-0123456789abcdef", "wake")
            .unwrap();
        // And the answer pair (CAD-447's routed reply lives on the
        // daemon lane like wake).
        s.enqueue_daemon("a1", "answer", "sys-answer-0123456789abcdef", "answer")
            .unwrap();
        for (id, source) in [
            ("m-1", "nudge"),             // caller id + daemon source
            ("m-1", "wake"),              // caller id + daemon source
            ("m-1", "answer"),            // caller id + daemon source
            ("sys-nudge-x", "user"),      // daemon id + caller source
            ("sys-wake-x", "nudge"),      // crossed daemon pair
            ("sys-nudge-x", "wake"),      // crossed daemon pair
            ("sys-answer-x", "nudge"),    // crossed daemon pair
            ("sys-nudge-x", "answer"),    // crossed daemon pair
            ("sys-x-x", "worker_notice"), // routed source can't go daemon
        ] {
            assert!(
                s.enqueue_daemon("a1", "x", id, source).is_err(),
                "{id}/{source} must be refused"
            );
        }
        // The caller side of the same fence.
        assert!(crate::proto::caller_message("sys-nudge-x", "nudge").is_err());
        assert!(crate::proto::caller_message("m-1", "wake").is_err());
        assert!(crate::proto::caller_message("m-1", "answer").is_err());
        assert!(crate::proto::caller_message("m-1", "nudge").is_ok());
    }

    #[test]
    fn fifo_take_marks_submitting() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg(&s, "a1", &cwd);
        s.enqueue("a1", "one", None, "m1", "user").unwrap();
        s.enqueue("a1", "two", None, "m2", "user").unwrap();
        match s.take_queued("a1").unwrap() {
            Take::Message(m) => assert_eq!(m.id, "m1"),
            _ => panic!("expected a message"),
        }
        match s.take_queued("a1").unwrap() {
            Take::Message(m) => assert_eq!(m.id, "m2"),
            _ => panic!("expected a message"),
        }
        assert!(matches!(s.take_queued("a1").unwrap(), Take::Empty));
    }

    /// CAD-250: while a delivered turn awaits its report, the actor's
    /// next claim is held — the second turn stays `queued` (never
    /// refused) while routed notifications still pass. The bound moves
    /// the overdue turn to `unknown` exactly once, with one notice to
    /// its `reply_to`; an ack restarts the clock; a report that already
    /// landed wins over the expiry.
    #[test]
    fn unreported_turn_holds_the_queue_until_bounded() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg(&s, "pm", &cwd);
        reg(&s, "w2", &cwd);
        reg_pty(&s, "w1", &cwd);
        s.enqueue("w1", "one", Some("pm"), "m1", "user").unwrap();
        s.enqueue("w1", "two", Some("pm"), "m2", "user").unwrap();
        let Take::Message(m1) = s.take_queued("w1").unwrap() else {
            panic!("m1 must be claimed");
        };
        s.mark_running(&m1.id, "pty-g-m1").unwrap();
        let m1 = s.message("m1").unwrap().unwrap();
        // Between turn start and the submitted marker the row is
        // `running` but not yet `awaiting_report` — it still holds.
        assert!(!m1.awaiting_report());
        assert!(matches!(s.take_queued("w1").unwrap(), Take::Empty));
        s.mark_submitted(&m1).unwrap();
        let m1 = s.message("m1").unwrap().unwrap();
        assert!(m1.awaiting_report());
        assert_eq!(m1.to_json()["awaiting_report"], true);
        assert!(matches!(s.take_queued("w1").unwrap(), Take::Empty));
        assert_eq!(s.message("m2").unwrap().unwrap().state, "queued");
        assert_eq!(s.queued_turns("w1").unwrap(), 1);
        assert_eq!(
            s.awaiting_reports("w1")
                .unwrap()
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            ["m1"]
        );

        // A routed notification overtakes the held turn.
        s.enqueue("w2", "side", Some("w1"), "x1", "user").unwrap();
        let Take::Message(x1) = s.take_queued("w2").unwrap() else {
            panic!("x1 must be claimed");
        };
        s.finish(
            &x1,
            "completed",
            &json!({"status": "completed", "text": "ok"}),
            None,
        )
        .unwrap();
        let Take::Message(routed) = s.take_queued("w1").unwrap() else {
            panic!("the routed result must pass the hold");
        };
        assert_eq!(routed.source, "worker_result");
        assert_eq!(s.message("m2").unwrap().unwrap().state, "queued");

        // An ack restarts the clock; the bound counts from it.
        let delivered = m1.report_clock().unwrap();
        s.mark_ack(&m1, Some("on it")).unwrap();
        let m1 = s.message("m1").unwrap().unwrap();
        let acked = m1.report_clock().unwrap();
        assert!(acked >= delivered);
        assert!(m1.awaiting_report(), "an ack does not finish the turn");
        assert!(!m1.report_overdue(0, acked + 1e9), "0 disables the bound");
        assert!(!s
            .expire_awaiting_report("m1", Some((10, acked + 5.0)), "r")
            .unwrap());
        assert_eq!(s.message("m1").unwrap().unwrap().state, "running");

        // Past the bound: unknown, one notice, never a result.
        assert!(s
            .expire_awaiting_report("m1", Some((10, acked + 11.0)), "bound ran out")
            .unwrap());
        let m1 = s.message("m1").unwrap().unwrap();
        assert_eq!(m1.state, "unknown");
        assert_eq!(m1.result.as_ref().unwrap()["via"], "report_timeout");
        assert!(!s
            .expire_awaiting_report("m1", Some((10, acked + 99.0)), "again")
            .unwrap());
        let notices: Vec<Message> = s
            .messages("pm")
            .unwrap()
            .into_iter()
            .filter(|m| m.source == "worker_notice" && m.body.contains("\"m1\""))
            .collect();
        assert_eq!(notices.len(), 1, "exactly one notice: {notices:?}");
        assert!(!s
            .messages("pm")
            .unwrap()
            .iter()
            .any(|m| m.source == "worker_result" && m.body.contains("\"m1\"")));

        // The hold lifts once the owed turn is resolved.
        let Take::Message(next) = s.take_queued("w1").unwrap() else {
            panic!("m2 must be claimed once m1 resolved");
        };
        assert_eq!(next.id, "m2");

        // A report that lands before the expiry wins.
        s.mark_running("m2", "pty-g-m2").unwrap();
        let m2 = s.message("m2").unwrap().unwrap();
        s.mark_submitted(&m2).unwrap();
        let m2 = s.message("m2").unwrap().unwrap();
        s.finish(
            &m2,
            "completed",
            &json!({"status": "completed", "text": "done"}),
            None,
        )
        .unwrap();
        assert!(!s
            .expire_awaiting_report("m2", Some((1, m2.report_clock().unwrap() + 9.0)), "late")
            .unwrap());
        assert!(!s.expire_awaiting_report("m2", None, "late").unwrap());
        assert_eq!(s.message("m2").unwrap().unwrap().state, "completed");
    }

    /// CAD-250 nudges: claimed past a held turn, never holding it; an
    /// unconfirmed nudge's `unknown` fences nothing; a nudge still queued
    /// at restart is cancelled with an event, never replayed.
    #[test]
    fn nudge_passes_the_hold_never_fences_and_never_replays() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg_pty(&s, "w1", &cwd);
        s.enqueue("w1", "task", None, "t1", "user").unwrap();
        s.enqueue("w1", "next", None, "t2", "user").unwrap();
        s.enqueue("w1", "steer", None, "n1", NUDGE_SOURCE).unwrap();
        s.enqueue("w1", "steer again", None, "n2", NUDGE_SOURCE)
            .unwrap();
        let Take::Message(t1) = s.take_queued("w1").unwrap() else {
            panic!("t1 must be claimed");
        };
        s.mark_running(&t1.id, "pty-g-t1").unwrap();
        let Take::Message(n1) = s.take_queued("w1").unwrap() else {
            panic!("the nudge must pass the held turn");
        };
        assert_eq!(n1.id, "n1");
        assert!(n1.is_nudge() && n1.to_json()["nudge"] == true);
        // An unconfirmed nudge: unknown, yet no fence and no unfence item.
        s.finish(
            &n1,
            "unknown",
            &json!({"status": "unknown"}),
            Some("unconfirmed"),
        )
        .unwrap();
        assert!(!s.has_unknown("w1").unwrap());
        assert!(s.unknown_messages("w1").unwrap().is_empty());
        assert_eq!(s.message("t2").unwrap().unwrap().state, "queued");
        // Restart: n2 (still queued) is cancelled with an event; t2 stays.
        drop(s);
        let s = Store::open(&dir.path().join("t.sqlite3")).unwrap();
        let n2 = s.message("n2").unwrap().unwrap();
        assert_eq!(n2.state, "cancelled");
        assert_eq!(n2.result.as_ref().unwrap()["via"], "restart_cancelled");
        assert_eq!(s.message("t2").unwrap().unwrap().state, "queued");
        assert!(s
            .events("w1", 0, 500)
            .unwrap()
            .iter()
            .any(|e| e.kind == "nudge_cancelled" && e.payload["message"] == "n2"));
        // The crash path fences the held turn — and only it: the
        // unconfirmed nudge is still no unfence item.
        assert_eq!(s.unknown_messages("w1").unwrap(), ["t1"]);
    }

    /// CAD-250 F1: a worker's report reads `running`, the report bound's
    /// expiry commits `unknown` (+ one notice) before the report writes —
    /// the report's guarded finish then refuses: the row stays `unknown`
    /// and exactly one routed message (the notice) exists, no result.
    #[test]
    fn report_racing_the_expiry_never_both_win() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg(&s, "pm", &cwd);
        reg_pty(&s, "w1", &cwd);
        s.enqueue("w1", "task", Some("pm"), "m1", "user").unwrap();
        let Take::Message(m1) = s.take_queued("w1").unwrap() else {
            panic!("m1 must be claimed");
        };
        s.mark_running(&m1.id, "pty-g-m1").unwrap();
        let m1 = s.message("m1").unwrap().unwrap();
        s.mark_submitted(&m1).unwrap();
        // The report's read: `running`.
        let seen = s.message("m1").unwrap().unwrap();
        assert_eq!(seen.state, "running");
        // The expiry commits in between.
        let clock = seen.report_clock().unwrap();
        assert!(s
            .expire_awaiting_report("m1", Some((10, clock + 11.0)), "bound ran out")
            .unwrap());
        // The report's write: refused, judged against the current row.
        let stored = json!({"status": "completed", "text": "done", "via": "pty_report"});
        let outcome = s.finish_running("m1", "completed", &stored, None).unwrap();
        let current = outcome.expect_err("the report must not win after the expiry");
        assert_eq!(current.unwrap().state, "unknown");
        assert_eq!(s.message("m1").unwrap().unwrap().state, "unknown");
        let routed: Vec<String> = s
            .messages("pm")
            .unwrap()
            .into_iter()
            .map(|m| m.source)
            .collect();
        assert_eq!(routed, ["worker_notice"], "one notice, no result");
        // And the other order: a report first wins, the expiry refuses.
        s.enqueue("w1", "task 2", Some("pm"), "m2", "user").unwrap();
        let Take::Message(m2) = s.take_queued("w1").unwrap() else {
            panic!("m2 must be claimed");
        };
        s.mark_running(&m2.id, "pty-g-m2").unwrap();
        assert!(s
            .finish_running("m2", "completed", &stored, None)
            .unwrap()
            .is_ok());
        assert!(!s.expire_awaiting_report("m2", None, "late").unwrap());
        assert_eq!(s.message("m2").unwrap().unwrap().state, "completed");
    }

    /// CAD-250 F2: a pty row that holds the turn without the `submitted`
    /// marker (adopted between `mark_running` and `mark_submitted`) is
    /// bounded from `started` like any other — the hold and the bound
    /// share one predicate — and stops deferring checkpoints past it.
    #[test]
    fn unmarked_running_row_is_bounded_too() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg_pty(&s, "w1", &cwd);
        s.enqueue("w1", "task", None, "m1", "user").unwrap();
        s.enqueue("w1", "next", None, "m2", "user").unwrap();
        let Take::Message(m1) = s.take_queued("w1").unwrap() else {
            panic!("m1 must be claimed");
        };
        s.mark_running(&m1.id, "pty-g-m1").unwrap();
        let m1 = s.message("m1").unwrap().unwrap();
        assert!(!m1.awaiting_report() && m1.holds_turn());
        assert!(matches!(s.take_queued("w1").unwrap(), Take::Empty));
        let started = m1.report_clock().unwrap();
        assert_eq!(Some(started), m1.started);
        assert_eq!(s.held_turns("w1").unwrap().len(), 1);
        let live: HashSet<String> = ["w1".to_string()].into();
        assert!(!s.busy_providers(&live, started + 60.0).unwrap().is_empty());
        let past = started + DEFAULT_REPORT_TIMEOUT_SECS as f64 + 1.0;
        assert!(s.busy_providers(&live, past).unwrap().is_empty());
        assert!(!s
            .expire_awaiting_report(
                "m1",
                Some((DEFAULT_REPORT_TIMEOUT_SECS, started + 60.0)),
                "r"
            )
            .unwrap());
        assert!(s
            .expire_awaiting_report("m1", Some((DEFAULT_REPORT_TIMEOUT_SECS, past)), "r")
            .unwrap());
        assert_eq!(s.message("m1").unwrap().unwrap().state, "unknown");
    }

    /// CAD-250 N3: a nudge still queued past its TTL is cancelled with a
    /// `nudge_cancelled` event (reason `ttl`); a younger one and a
    /// non-nudge are untouched. The clock is injected — no sleeps.
    #[test]
    fn queued_nudge_expires_after_its_ttl() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg_pty(&s, "w1", &cwd);
        s.enqueue("w1", "steer", None, "n1", NUDGE_SOURCE).unwrap();
        s.enqueue("w1", "task", None, "t1", "user").unwrap();
        let created = s.message("n1").unwrap().unwrap().created;
        assert!(s
            .expire_queued_nudges(created + 899.0, 900.0)
            .unwrap()
            .is_empty());
        assert_eq!(s.message("n1").unwrap().unwrap().state, "queued");
        let closed = s.expire_queued_nudges(created + 901.0, 900.0).unwrap();
        assert_eq!(closed, [("n1".to_string(), "w1".to_string())]);
        let n1 = s.message("n1").unwrap().unwrap();
        assert_eq!(n1.state, "cancelled");
        assert_eq!(n1.result.as_ref().unwrap()["via"], "ttl_cancelled");
        assert_eq!(s.message("t1").unwrap().unwrap().state, "queued");
        let ev = s
            .last_event_of("w1", &["nudge_cancelled"])
            .unwrap()
            .unwrap();
        assert_eq!(ev.payload["reason"], "ttl");
        assert!(s
            .expire_queued_nudges(created + 9e9, 900.0)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn restart_marks_inflight_unknown() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg(&s, "a1", &cwd);
        s.enqueue("a1", "one", None, "m1", "user").unwrap();
        let _ = s.take_queued("a1").unwrap();
        drop(s);
        let s2 = Store::open(&dir.path().join("t.sqlite3")).unwrap();
        let m = s2.message("m1").unwrap().unwrap();
        assert_eq!(m.state, "unknown");
        assert!(s2.has_unknown("a1").unwrap());
    }

    /// Acceptance 1 + 3: one call cancels every named still-queued row
    /// and queues the new one; each row keeps its history as
    /// `cancelled`, reason "superseded by <new>", with the caller. Each
    /// distinct `reply_to` hears ONE notice naming every replaced id and
    /// the new one (PR #252 QA N4) — and the caller's own reply_to hears
    /// nothing: it did the superseding.
    #[test]
    fn supersede_replaces_queued_messages_in_one_transaction() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg(&s, "pm", &cwd);
        reg(&s, "w1", &cwd);
        reg(&s, "qa", &cwd);
        s.enqueue("w1", "old fix", Some("qa"), "m1", "user")
            .unwrap();
        s.enqueue("w1", "old push", Some("qa"), "m2", "user")
            .unwrap();
        s.enqueue("w1", "old preview", None, "m3", "user").unwrap();
        s.enqueue("w1", "unrelated", None, "m4", "user").unwrap();
        s.enqueue("w1", "old status", Some("pm"), "m5", "user")
            .unwrap();
        let named = strings(&["m1", "m2", "m3", "m5"]);
        let receipt = send_steered(
            &s,
            "w1",
            Some("pm"),
            "new1",
            &steer_as_pm(Priority::Normal, &named),
        )
        .unwrap();
        assert_eq!(receipt, (false, "queued".to_string()));
        for (id, body) in [
            ("m1", "old fix"),
            ("m2", "old push"),
            ("m3", "old preview"),
            ("m5", "old status"),
        ] {
            let m = s.message(id).unwrap().unwrap();
            assert_eq!(m.state, "cancelled", "{id}");
            assert_eq!(m.body, body, "history kept");
            let r = m.result.unwrap();
            assert_eq!(r["via"], "supersede");
            assert_eq!(r["reason"], "superseded by new1");
            assert_eq!(r["superseded_by"], "new1");
            assert_eq!(
                (r["by"].as_str(), r["by_kind"].as_str()),
                (Some("pm"), Some("agent"))
            );
        }
        assert_eq!(s.message("m4").unwrap().unwrap().state, "queued");
        assert_eq!(s.message("new1").unwrap().unwrap().state, "queued");
        let events = s.events("w1", 0, 100).unwrap();
        let cancelled: Vec<_> = events
            .iter()
            .filter(|e| e.kind == "cancelled")
            .map(|e| e.payload["superseded_by"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(cancelled, ["new1", "new1", "new1", "new1"]);
        let steered = events.iter().find(|e| e.kind == "steered").unwrap();
        assert_eq!(
            steered.payload["supersedes"],
            json!(["m1", "m2", "m3", "m5"])
        );
        assert_eq!(steered.payload["by"], "pm");
        // One notice for qa's two rows, naming both and the new id; m3
        // had no reply_to, and m5's reply_to is the caller itself.
        let heard = notices(&s, "qa");
        assert_eq!(heard.len(), 1, "{heard:?}");
        let body = &heard[0].1;
        assert!(
            body.contains("m1, m2 were superseded by message new1"),
            "{body}"
        );
        assert!(notices(&s, "pm").is_empty(), "the caller is not notified");
        // A retry of the same envelope dedupes and notifies nobody again;
        // the same id naming another set is a conflict.
        let again = send_steered(
            &s,
            "w1",
            Some("pm"),
            "new1",
            &steer_as_pm(Priority::Normal, &strings(&["m5", "m3", "m2", "m1"])),
        )
        .unwrap();
        assert_eq!(again, (true, "queued".to_string()));
        assert_eq!(notices(&s, "qa").len(), 1);
        assert!(notices(&s, "pm").is_empty());
        let err = send_steered(
            &s,
            "w1",
            Some("pm"),
            "new1",
            &steer_as_pm(Priority::Normal, &strings(&["m1"])),
        )
        .unwrap_err();
        assert!(err.to_string().contains("different content"), "{err}");
        // The replacement waits its turn behind older normal work.
        let order: Vec<String> = (0..2)
            .map(|_| match s.take_queued("w1").unwrap() {
                Take::Message(m) => m.id,
                _ => panic!("expected a message"),
            })
            .collect();
        assert_eq!(order, ["m4", "new1"]);
    }

    /// PR #252 QA N2: superseding a `--task` follow-up records its
    /// cancellation on the task's (and job's) stream, like its `queued`
    /// event — not only on the agent's.
    #[test]
    fn superseding_a_task_follow_up_is_recorded_on_the_task() {
        let (dir, s) = store();
        let kickoff = seeded_task(&s, &dir.path().join("w"));
        s.enqueue_task("w1", "old detail", None, "f1", "user", Some("t1"))
            .unwrap();
        s.enqueue("w1", "unbound", None, "u1", "user").unwrap();
        send_steered(
            &s,
            "w1",
            None,
            "new1",
            &steer_as_pm(Priority::Normal, &strings(&["f1", "u1"])),
        )
        .unwrap();
        let on_task: Vec<Event> = s
            .job_events("j1", 0, 500)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "cancelled")
            .collect();
        assert_eq!(on_task.len(), 1, "{on_task:?}");
        assert_eq!(on_task[0].payload["message"], "f1");
        assert_eq!(on_task[0].payload["superseded_by"], "new1");
        assert_eq!(on_task[0].task_id.as_deref(), Some("t1"));
        // The kickoff itself is never supersedable.
        let err = send_steered(
            &s,
            "w1",
            None,
            "new2",
            &steer_as_pm(Priority::Normal, &strings(&[&kickoff])),
        )
        .unwrap_err();
        assert!(err.to_string().contains("kickoff"), "{err}");
    }

    /// Acceptance 2: a named message that is not still queued — per
    /// state, unknown, another agent's, a routed notice or a task
    /// kickoff — refuses the whole call naming it, and nothing changes.
    #[test]
    fn supersede_refuses_anything_not_still_queued_and_changes_nothing() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg(&s, "pm", &cwd);
        reg(&s, "w1", &cwd);
        reg(&s, "w2", &cwd);
        s.enqueue("w1", "stale", Some("pm"), "ok", "user").unwrap();
        let set_state = |id: &str, state: &str, result: Option<Value>| {
            s.conn()
                .execute(
                    "UPDATE messages SET state=?, result=? WHERE id=?",
                    params![state, result.map(|r| r.to_string()), id],
                )
                .unwrap();
        };
        let mut cases: Vec<(String, String)> = Vec::new();
        for state in [
            "submitting",
            "running",
            "submitted",
            "completed",
            "failed",
            "interrupted",
            "cancelled",
            "unknown",
        ] {
            let id = format!("x-{state}");
            s.enqueue("w1", "x", Some("pm"), &id, "user").unwrap();
            if state == "submitted" {
                // Delivered to a pty pane, report owed.
                set_state(&id, "running", Some(json!({"status": "submitted"})));
                cases.push((id, "is running".to_string()));
            } else {
                set_state(&id, state, None);
                cases.push((id, format!("is {state}")));
            }
        }
        cases.push(("nope".to_string(), "is unknown".to_string()));
        s.enqueue("w2", "theirs", None, "theirs", "user").unwrap();
        cases.push(("theirs".to_string(), "is agent 'w2''s".to_string()));
        s.enqueue("w1", "notice", None, "routed", "worker_notice")
            .unwrap();
        cases.push(("routed".to_string(), "routed worker_notice".to_string()));
        s.enqueue("w1", "kickoff", None, "kick", "job_dispatch")
            .unwrap();
        cases.push(("kick".to_string(), "kickoff".to_string()));
        let events_before = s.events("w1", 0, 1000).unwrap().len();
        for (n, (bad, why)) in cases.iter().enumerate() {
            let new = format!("new-{n}");
            let err = send_steered(
                &s,
                "w1",
                Some("pm"),
                &new,
                &steer_as_pm(Priority::Urgent, &strings(&["ok", bad])),
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains(&format!("message '{bad}'")), "{err}");
            assert!(err.contains(why), "{bad}: {err}");
            assert!(err.contains("nothing changed"), "{err}");
            assert!(s.message(&new).unwrap().is_none(), "{new} enqueued");
        }
        assert_eq!(s.message("ok").unwrap().unwrap().state, "queued");
        assert_eq!(s.events("w1", 0, 1000).unwrap().len(), events_before);
        assert!(notices(&s, "pm").is_empty());
    }

    /// Acceptance 4: urgent is claimed ahead of every queued normal row,
    /// FIFO within a rank — but only at the next safe boundary: a
    /// running turn (including one parked on an open approval) holds
    /// it like any other turn-owning delivery.
    #[test]
    fn urgent_goes_first_at_the_next_safe_boundary_fifo_within_rank() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg(&s, "pm", &cwd);
        reg(&s, "w2", &cwd);
        reg_pty(&s, "w1", &cwd);
        s.enqueue("w1", "running turn", None, "m0", "user").unwrap();
        let Take::Message(m0) = s.take_queued("w1").unwrap() else {
            panic!("m0 must be claimed");
        };
        s.mark_running(&m0.id, "pty-g-m0").unwrap();
        s.set_agent_state("w1", "waiting_input", None).unwrap();
        let none: &[String] = &[];
        for (id, priority) in [
            ("n1", Priority::Normal),
            ("n2", Priority::Normal),
            ("u1", Priority::Urgent),
            ("n3", Priority::Normal),
            ("u2", Priority::Urgent),
        ] {
            send_steered(&s, "w1", None, id, &steer_as_pm(priority, none)).unwrap();
        }
        // Never interrupts the running turn or its open approval.
        assert!(matches!(s.take_queued("w1").unwrap(), Take::Empty));
        assert_eq!(s.message("m0").unwrap().unwrap().state, "running");
        assert_eq!(s.queued_head("w1").unwrap().unwrap().id, "u1");
        // A routed notice still passes the hold, as before.
        s.enqueue("w2", "side", Some("w1"), "x1", "user").unwrap();
        let Take::Message(x1) = s.take_queued("w2").unwrap() else {
            panic!("x1 must be claimed");
        };
        s.finish(
            &x1,
            "completed",
            &json!({"status": "completed", "text": "ok"}),
            None,
        )
        .unwrap();
        let Take::Message(routed) = s.take_queued("w1").unwrap() else {
            panic!("the routed result passes the hold");
        };
        assert_eq!(routed.source, "worker_result");
        s.finish(&routed, "completed", &json!({"status": "completed"}), None)
            .unwrap();
        // The turn ends: the boundary.
        s.finish_running("m0", "completed", &json!({"status": "completed"}), None)
            .unwrap()
            .unwrap();
        let mut order = Vec::new();
        while let Take::Message(m) = s.take_queued("w1").unwrap() {
            s.mark_running(&m.id, &format!("pty-g-{}", m.id)).unwrap();
            s.finish_running(&m.id, "completed", &json!({"status": "completed"}), None)
                .unwrap()
                .unwrap();
            order.push(m.id);
        }
        assert_eq!(order, ["u1", "u2", "n1", "n2", "n3"]);
        assert_eq!(
            s.message("u1").unwrap().unwrap().to_json()["priority"],
            "urgent"
        );
        assert!(s
            .message("n1")
            .unwrap()
            .unwrap()
            .to_json()
            .get("priority")
            .is_none());
        // Same id, another rank: a conflict, not a duplicate.
        let err =
            send_steered(&s, "w1", None, "n1", &steer_as_pm(Priority::Urgent, none)).unwrap_err();
        assert!(err.to_string().contains("different content"), "{err}");
    }

    /// Acceptance 6 — already delivered before CAD-158: a routed result
    /// or notice has a deterministic id per source message (and per
    /// notice kind), so repeated reports of the same (message, turn_id)
    /// — the "worker_result delivered 3x with identical turn_id" case —
    /// and a late adapter finish route exactly one delivery, which is
    /// claimed once.
    #[test]
    fn routed_result_and_notice_per_message_turn_are_delivered_once() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg(&s, "pm", &cwd);
        reg_pty(&s, "w1", &cwd);
        s.enqueue("w1", "work", Some("pm"), "m1", "user").unwrap();
        let Take::Message(m1) = s.take_queued("w1").unwrap() else {
            panic!("m1 must be claimed");
        };
        s.mark_running(&m1.id, "pty-g-t1").unwrap();
        let running = s.message("m1").unwrap().unwrap();
        s.mark_submitted(&running).unwrap();
        let stored = json!({"status": "completed", "text": "done",
                            "turn_id": "pty-g-t1", "via": "pty_report"});
        assert!(s
            .finish_running("m1", "completed", &stored, None)
            .unwrap()
            .is_ok());
        for _ in 0..2 {
            let again = s.finish_running("m1", "completed", &stored, None).unwrap();
            assert_eq!(again.unwrap_err().unwrap().state, "completed");
        }
        // A late second finish of the same turn routes nothing new.
        s.finish(&running, "completed", &stored, None).unwrap();
        let routed: Vec<Message> = s
            .messages("pm")
            .unwrap()
            .into_iter()
            .filter(|m| m.source == "worker_result")
            .collect();
        assert_eq!(routed.len(), 1, "{routed:?}");
        let Take::Message(delivery) = s.take_queued("pm").unwrap() else {
            panic!("the one routed result is claimed");
        };
        assert_eq!(delivery.id, routed[0].id);
        s.finish(
            &delivery,
            "completed",
            &json!({"status": "completed"}),
            None,
        )
        .unwrap();
        assert!(matches!(s.take_queued("pm").unwrap(), Take::Empty));
        // Notices: one per (kind, message) however often it fires.
        s.enqueue("w1", "work 2", Some("pm"), "m2", "user").unwrap();
        let Take::Message(m2) = s.take_queued("w1").unwrap() else {
            panic!("m2 must be claimed");
        };
        s.mark_running(&m2.id, "pty-g-t2").unwrap();
        assert!(s.expire_awaiting_report("m2", None, "bound").unwrap());
        let m2 = s.message("m2").unwrap().unwrap();
        s.finish(&m2, "unknown", &json!({"status": "unknown"}), Some("again"))
            .unwrap();
        assert_eq!(notices(&s, "pm").len(), 1);
    }
