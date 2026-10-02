    // CAD-1015: `begin_native_nudge` / `finish_native_nudge`. Every test
    // builds its own `Store` in a tempdir — no daemon, no socket, no
    // cadence identity needed, so these run root-safe under `cargo test`.
    //
    // The adversarial cases are written first and against the durable
    // state transitions, not against the function existing: each one
    // names the guard that must hold — atomic bind, terminal
    // preservation, dedupe identity, generation/session drift.

    /// The `&Steer` a native nudge carries: normal rank, nothing
    /// superseded, caller identity for the event record.
    fn caller_steer<'a>() -> Steer<'a> {
        Steer {
            priority: Priority::Normal,
            supersedes: &[],
            by: "pm",
            by_kind: "agent",
        }
    }

    /// The `&Steer` an unattributed (operator-connection) caller carries.
    fn unattributed_steer<'a>() -> Steer<'a> {
        Steer::NONE
    }

    /// `(alias, agent)` registered, opened to a live endpoint at
    /// `generation`/`session`/`thread`, holding one running
    /// report-owing turn whose token is `turn`.
    fn live_agent(s: &Store, alias: &str, cwd: &std::path::Path, turn: &str) -> Agent {
        s.register_agent(&NewAgent {
            alias,
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: cwd.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
        s.set_identity(
            alias,
            &crate::adapter::Identity {
                thread_id: format!("thread-{alias}"),
                session_id: format!("session-{alias}"),
                model: None,
                effort: None,
                pid: 4242,
                endpoint: Some(format!("native://{alias}")),
                generation: Some(format!("gen-{alias}")),
                attach: None,
            },
        )
        .unwrap();
        s.enqueue(alias, "kickoff", None, &format!("k-{alias}"), "user")
            .unwrap();
        let Take::Message(m) = s.take_queued(alias).unwrap() else {
            panic!("{alias}: kickoff must be claimed");
        };
        s.mark_running(&m.id, turn).unwrap();
        s.agent(alias).unwrap()
    }

    /// `live_agent` with no running turn — idle endpoint only.
    fn idle_agent(s: &Store, alias: &str, cwd: &std::path::Path) -> Agent {
        s.register_agent(&NewAgent {
            alias,
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: cwd.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
        s.set_identity(
            alias,
            &crate::adapter::Identity {
                thread_id: format!("thread-{alias}"),
                session_id: format!("session-{alias}"),
                model: None,
                effort: None,
                pid: 4242,
                endpoint: Some(format!("native://{alias}")),
                generation: Some(format!("gen-{alias}")),
                attach: None,
            },
        )
        .unwrap();
        s.agent(alias).unwrap()
    }

    /// The queued nudge never enters `take_queued`'s claim path: a nudge
    /// is turnless, and a native nudge is only ever `submitting` or
    /// terminal after `begin` — so the actor cannot take it.
    #[test]
    fn idle_skip_and_no_take_queued() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let agent = idle_agent(&s, "w1", &cwd);
        // No running turn: the nudge must go straight to terminal
        // `cancelled`/`skipped_inactive`, never linger in `queued`.
        let (dup, state, turn) = s
            .begin_native_nudge(
                &agent,
                "steer",
                "n1",
                &Sender::Unattributed,
                &caller_steer(),
            )
            .unwrap();
        assert!(!dup && state == "cancelled" && turn.is_none());
        let row = s.message("n1").unwrap().unwrap();
        assert_eq!(row.state, "cancelled");
        assert_eq!(row.result.as_ref().unwrap()["via"], "skipped_inactive");
        assert_eq!(row.result.as_ref().unwrap()["status"], "skipped");
        assert_eq!(
            row.result.as_ref().unwrap()["delivery"],
            "native_turn_steering"
        );
        // `take_queued` cannot claim a terminal row — and nothing else
        // is queued, so the actor drains empty.
        assert!(matches!(s.take_queued("w1").unwrap(), Take::Empty));
        assert!(s
            .last_event_of("w1", &["nudge_cancelled"])
            .unwrap()
            .is_some_and(|e| e.payload["message"] == "n1"));
    }

    /// The whole point of the design: with a live turn the new row is
    /// `submitting` with its `turn_id` bound in the same transaction, so
    /// no later claim can take it and no sweep can see a bare `queued`.
    #[test]
    fn atomic_submitting_bind_is_unclaimable() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let agent = live_agent(&s, "w1", &cwd, "turn-1");
        let (dup, state, turn) = s
            .begin_native_nudge(
                &agent,
                "steer",
                "n1",
                &Sender::Unattributed,
                &caller_steer(),
            )
            .unwrap();
        assert!(!dup && state == "submitting" && turn.as_deref() == Some("turn-1"));
        let row = s.message("n1").unwrap().unwrap();
        assert_eq!(row.state, "submitting");
        assert_eq!(row.turn_id.as_deref(), Some("turn-1"));
        assert!(row.started.is_some());
        // The actor's claim path reads only `queued`: the bound
        // `submitting` row is already out of reach.
        assert!(matches!(s.take_queued("w1").unwrap(), Take::Empty));
        let event = s
            .last_event_of("w1", &["native_steer_submitting"])
            .unwrap()
            .expect("the submit event must exist");
        assert_eq!(event.payload["message"], "n1");
        assert_eq!(event.payload["turn"], "turn-1");
        assert_eq!(event.payload["by"], "pm");
        assert_eq!(event.payload["by_kind"], "agent");
    }

    /// Envelope dedupe is idempotent: a retry of the same id returns
    /// the stored state and the bound turn, never a second row and
    /// never a re-aim at a different turn.
    #[test]
    fn duplicate_never_enqueues_a_second_row() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let agent = live_agent(&s, "w1", &cwd, "turn-1");
        let (d1, _, t1) = s
            .begin_native_nudge(
                &agent,
                "steer",
                "n1",
                &Sender::Unattributed,
                &caller_steer(),
            )
            .unwrap();
        let (d2, state, t2) = s
            .begin_native_nudge(
                &agent,
                "steer",
                "n1",
                &Sender::Unattributed,
                &caller_steer(),
            )
            .unwrap();
        assert!(!d1 && d2, "the retry must dedupe");
        assert_eq!(state, "submitting");
        assert_eq!(t1.as_deref(), t2.as_deref());
        assert_eq!(t2.as_deref(), Some("turn-1"));
        // Still exactly one row, still the first bind.
        assert_eq!(
            s.messages("w1")
                .unwrap()
                .iter()
                .filter(|m| m.id == "n1")
                .count(),
            1
        );
        // No second native_steer_submitting event.
        let submitting_events = s
            .events("w1", 0, 100)
            .unwrap()
            .iter()
            .filter(|e| e.kind == "native_steer_submitting")
            .count();
        assert_eq!(submitting_events, 1);
    }

    /// The same id with a different body is the enqueue conflict it
    /// always was — refused, nothing written.
    #[test]
    fn changed_body_conflicts_not_replaces() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let agent = live_agent(&s, "w1", &cwd, "turn-1");
        s.begin_native_nudge(
            &agent,
            "steer",
            "n1",
            &Sender::Unattributed,
            &caller_steer(),
        )
        .unwrap();
        let err = s
            .begin_native_nudge(
                &agent,
                "different",
                "n1",
                &Sender::Unattributed,
                &caller_steer(),
            )
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("already used with different content"),
            "{err}"
        );
        // One row, unchanged.
        let row = s.message("n1").unwrap().unwrap();
        assert_eq!(row.body, "steer");
        assert_eq!(row.state, "submitting");
    }

    /// The caller's `target` is a snapshot; the durable row decides.
    /// After a restart the agent re-opens at a new generation/session —
    /// steering against the old snapshot is refused before any write.
    #[test]
    fn generation_and_session_replacement_refuses() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let agent = live_agent(&s, "w1", &cwd, "turn-1");
        // Re-open moves generation + session + endpoint: the snapshot is stale.
        s.set_identity(
            "w1",
            &crate::adapter::Identity {
                thread_id: "thread-w1".into(),
                session_id: "session-w1-v2".into(),
                model: None,
                effort: None,
                pid: 4242,
                endpoint: Some("native://w1-v2".into()),
                generation: Some("gen-w1-v2".into()),
                attach: None,
            },
        )
        .unwrap();
        let err = s
            .begin_native_nudge(
                &agent,
                "steer",
                "n1",
                &Sender::Unattributed,
                &caller_steer(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("no longer matches"), "{err}");
        // Nothing was enqueued.
        assert!(s.message("n1").unwrap().is_none());
    }

    /// A disabled agent refuses before any message write — the
    /// envelope's conflict/dedupe path can never hide a dead steer.
    #[test]
    fn disabled_refuses_before_any_write() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let agent = idle_agent(&s, "w1", &cwd);
        s.set_enabled("w1", false).unwrap();
        let err = s
            .begin_native_nudge(
                &agent,
                "steer",
                "n1",
                &Sender::Unattributed,
                &caller_steer(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("disabled"), "{err}");
        assert!(s.message("n1").unwrap().is_none());
    }

    /// `finish_native_nudge` does not touch the agent's state and does
    /// not disturb the kickoff it ran beside: the turn's lifecycle owns
    /// `agents.state`, and the held turn stays held.
    #[test]
    fn finish_never_moves_agent_or_kickoff() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let agent = live_agent(&s, "w1", &cwd, "turn-1");
        let before = s.agent("w1").unwrap().state;
        let kickoff = s.message("k-w1").unwrap().unwrap();
        s.begin_native_nudge(
            &agent,
            "steer",
            "n1",
            &Sender::Unattributed,
            &caller_steer(),
        )
        .unwrap();
        s.finish_native_nudge("n1", "queued", None).unwrap();
        let after = s.agent("w1").unwrap();
        // The nudge owned no turn: the agent's state is exactly what
        // `take_queued`/`mark_running` left it, and the kickoff is
        // still the running turn.
        assert_eq!(after.state, before);
        assert_eq!(after.state, "busy");
        let kickoff_after = s.message("k-w1").unwrap().unwrap();
        assert_eq!(kickoff_after.state, "running");
        assert_eq!(kickoff_after.turn_id, kickoff.turn_id);
        // The nudge row itself is `completed`/`accepted`/`queued`.
        let row = s.message("n1").unwrap().unwrap();
        assert_eq!(row.state, "completed");
        assert_eq!(row.result.as_ref().unwrap()["status"], "accepted");
        assert_eq!(row.result.as_ref().unwrap()["outcome"], "queued");
        assert_eq!(row.result.as_ref().unwrap()["application"], "unconfirmed");
        assert_eq!(row.result.as_ref().unwrap()["target_message"], "k-w1");
    }

    /// Stop/restart's nudge sweep owns the in-flight row: a late
    /// `finish` for a `submitting` nudge that lost the race must not
    /// overwrite the terminal the sweep already wrote.
    #[test]
    fn late_finish_never_overwrites_a_terminal() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let agent = live_agent(&s, "w1", &cwd, "turn-1");
        s.begin_native_nudge(
            &agent,
            "steer",
            "n1",
            &Sender::Unattributed,
            &caller_steer(),
        )
        .unwrap();
        // The daemon's stop path cancels in-flight nudges first.
        let closed = s.cancel_nudges_for("w1", "stopped").unwrap();
        assert_eq!(closed, [("n1".to_string(), "w1".to_string())]);
        let row = s.message("n1").unwrap().unwrap();
        assert_eq!(row.state, "unknown"); // submitting -> stopped_unconfirmed
                                          // The provider's late answer lands only on the event stream.
        s.finish_native_nudge("n1", "queued", Some("provider replied after stop"))
            .unwrap();
        let row = s.message("n1").unwrap().unwrap();
        assert_eq!(row.state, "unknown", "terminal evidence preserved");
        assert_eq!(row.result.as_ref().unwrap()["via"], "stopped_unconfirmed");
        // The late disposition is recorded as an event, not as model application.
        let event = s
            .last_event_of("w1", &["native_steer_disposition"])
            .unwrap()
            .expect("the disposition event must exist");
        assert_eq!(event.payload["message"], "n1");
        assert_eq!(event.payload["disposition"], "queued");
        assert_eq!(event.payload["recorded"], false);
    }

    /// Every disposition lands on the allowlisted terminal; anything
    /// else is refused before any write.
    #[test]
    fn finish_disposition_allowlist() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let agent = live_agent(&s, "w1", &cwd, "turn-1");
        for (id, disp, want_state, want_status) in [
            ("n-q", "queued", "completed", "accepted"),
            ("n-s", "skipped_inactive", "cancelled", "skipped"),
            ("n-r", "rejected", "failed", "failed"),
            ("n-u", "unknown", "unknown", "unknown"),
        ] {
            s.begin_native_nudge(
                &agent,
                "steer",
                id,
                &Sender::Unattributed,
                &unattributed_steer(),
            )
            .unwrap();
            s.finish_native_nudge(id, disp, Some("probe")).unwrap();
            let row = s.message(id).unwrap().unwrap();
            assert_eq!(row.state, want_state, "{disp}");
            assert_eq!(
                row.result.as_ref().unwrap()["status"],
                want_status,
                "{disp}"
            );
            assert_eq!(
                row.result.as_ref().unwrap()["via"],
                "native_turn_steering",
                "{disp}"
            );
        }
        // A made-up disposition refuses before it can write.
        s.begin_native_nudge(
            &agent,
            "steer",
            "n-x",
            &Sender::Unattributed,
            &unattributed_steer(),
        )
        .unwrap();
        let err = s.finish_native_nudge("n-x", "hacked", None).unwrap_err();
        assert!(err.to_string().contains("disposition"), "{err}");
        assert_eq!(s.message("n-x").unwrap().unwrap().state, "submitting");
        // And a non-nudge message can never take a disposition.
        let err = s.finish_native_nudge("k-w1", "queued", None).unwrap_err();
        assert!(err.to_string().contains("source"), "{err}");
    }
