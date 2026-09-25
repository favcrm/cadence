use super::*;

    #[test]
    fn current_verdict_follows_the_open_revision() {
        let rows = vec![verdict(1, 2), verdict(3, 2), verdict(2, 1)];
        let current = super::current_verdict(1, &rows).unwrap();
        assert_eq!(current.seq, 2);
        assert_eq!(current.revision, 1);
        // Reopen left a higher revision in history. It is not current.
        assert!(super::current_verdict(0, &rows).is_none());
        let again = super::current_verdict(2, &rows).unwrap();
        assert_eq!(again.seq, 3);
    }

    #[test]
    fn finish_sha_binds_review_and_verdict() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let kickoff = seeded_task(&s, &cwd);
        let m = run_kickoff(&s, &kickoff);
        assert_eq!(s.task("t1").unwrap().state, "running");
        // The `message result --sha` path: explicit result.sha wins.
        s.finish(
            &m,
            "completed",
            &json!({"status": "completed", "text": "done", "sha": SHA40_A}),
            None,
        )
        .unwrap();
        let t = s.task("t1").unwrap();
        assert_eq!(t.state, "review");
        assert_eq!(t.head_sha.as_deref(), Some(SHA40_A));
        // verdict binding: wrong sha rejected, right sha passes.
        assert!(s
            .record_verdict("t1", SHA40_B, "pass", "rev", None, None, None, None, None)
            .is_err());
        let (t, v) = s
            .record_verdict("t1", SHA40_A, "pass", "rev", None, None, None, None, None)
            .unwrap();
        assert_eq!(t.state, "verified");
        assert_eq!(v.revision, 1);
    }

    #[test]
    fn finish_sha_trailer_and_missing_sha_repair() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let kickoff = seeded_task(&s, &cwd);
        let m = run_kickoff(&s, &kickoff);
        // Managed path: no result.sha — the LAST `SHA: <hex>` line wins.
        let text = format!("summary\nSHA: {SHA40_B}\nmore text\nSHA: {SHA40_A}");
        s.finish(
            &m,
            "completed",
            &json!({"status": "completed", "text": text}),
            None,
        )
        .unwrap();
        let t = s.task("t1").unwrap();
        assert_eq!(t.head_sha.as_deref(), Some(SHA40_A), "{t:?}");

        // A pull-request URL after the trailer must not steal the sha.
        s.create_task("j1", "t-pr", None, Some("w1"), None, None, None, None, None)
            .unwrap();
        let (_, kick_pr, ..) = s.dispatch_task("t-pr", None, None, "test").unwrap();
        let m_pr = run_kickoff(&s, &kick_pr);
        let trailed = format!("SHA: {SHA40_B}\nhttps://github.com/favcrm/cadence/pull/9");
        s.finish(
            &m_pr,
            "completed",
            &json!({"status": "completed", "text": trailed}),
            None,
        )
        .unwrap();
        assert_eq!(s.task("t-pr").unwrap().head_sha.as_deref(), Some(SHA40_B));

        // Second task: no sha anywhere → review with NULL, task sha repairs.
        s.create_task("j1", "t2", None, Some("w1"), None, None, None, None, None)
            .unwrap();
        let (_, kick2, ..) = s.dispatch_task("t2", None, None, "test").unwrap();
        let m2 = run_kickoff(&s, &kick2);
        s.finish(
            &m2,
            "completed",
            &json!({"status": "completed", "text": "no sha"}),
            None,
        )
        .unwrap();
        let t2 = s.task("t2").unwrap();
        assert_eq!(t2.state, "review");
        assert_eq!(t2.head_sha, None);
        assert!(s
            .record_verdict("t2", SHA40_A, "pass", "rev", None, None, None, None, None)
            .unwrap_err()
            .to_string()
            .contains("job task sha"));
        s.set_task_sha("t2", SHA40_A, "op").unwrap();
        s.record_verdict("t2", SHA40_A, "pass", "rev", None, None, None, None, None)
            .unwrap();
        assert_eq!(s.task("t2").unwrap().state, "verified");
    }

    #[test]
    fn verdict_revision_and_reviewer_guards() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let kickoff = seeded_task(&s, &cwd);
        let m = run_kickoff(&s, &kickoff);
        s.finish(
            &m,
            "completed",
            &json!({"status": "completed", "sha": SHA40_A}),
            None,
        )
        .unwrap();
        // Stale revision.
        assert!(s
            .record_verdict(
                "t1",
                SHA40_A,
                "pass",
                "rev",
                None,
                None,
                None,
                Some(9),
                None
            )
            .unwrap_err()
            .to_string()
            .contains("stale"));
        // Reviewer == assignee.
        assert!(s
            .record_verdict("t1", SHA40_A, "pass", "w1", None, None, None, None, None)
            .unwrap_err()
            .to_string()
            .contains("assignee"));
        // Verdicts are append-only across revisions: revise then pass.
        s.record_verdict("t1", SHA40_A, "revise", "rev", None, None, None, None, None)
            .unwrap();
        assert_eq!(s.task("t1").unwrap().state, "revising");
        let (_, k2, ..) = s.dispatch_task("t1", None, None, "test").unwrap();
        let m2 = run_kickoff(&s, &k2);
        s.finish(
            &m2,
            "completed",
            &json!({"status": "completed", "sha": SHA40_B}),
            None,
        )
        .unwrap();
        // The r1 sha is stale for r2.
        assert!(s
            .record_verdict("t1", SHA40_A, "pass", "rev", None, None, None, None, None)
            .is_err());
        s.record_verdict("t1", SHA40_B, "pass", "rev", None, None, None, None, None)
            .unwrap();
        let vs = s.verdicts_for_task("t1").unwrap();
        assert_eq!(vs.len(), 2);
        assert_eq!(vs[0].revision, 1);
        assert_eq!(vs[1].revision, 2);
    }

    // ---- CAD-162: adoption judges tokens by the agent's own scheme ----

    #[test]
    fn explicit_kickoff_tails_differ_per_message_id() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        seeded_job(&s, &cwd);
        reg_pty(&s, "w1", &cwd);
        s.create_task(
            "j1",
            "t1",
            None,
            Some("w1"),
            None,
            Some("green tests"),
            Some("/w/t1"),
            Some("b1"),
            Some(SHA40_A),
        )
        .unwrap();
        s.create_task(
            "j1",
            "t2",
            None,
            Some("w1"),
            None,
            Some("other work"),
            None,
            None,
            None,
        )
        .unwrap();
        let (k1, b1) = dispatch_body(&s, "t1", None);
        let (k2, b2) = dispatch_body(&s, "t2", None);
        assert_ne!(k1, k2);
        for (k, b) in [(&k1, &b1), (&k2, &b2)] {
            // The durable id is still the report target; the body ends
            // in the derived correlation — trailer first, suffix last.
            assert!(b.contains(&format!("cadence message result {k}")), "{b}");
            assert!(b.ends_with(&expected_correlation(k)), "{b}");
            let trailer = b.rfind("committed.").unwrap();
            let corr = b.rfind("Correlation:").unwrap();
            assert!(trailer < corr, "{b}");
            // The pty paste contract: one line, control-free, ≤4000 bytes.
            assert!(
                !b.contains('\n') && !b.chars().any(|c| c.is_control()),
                "{b}"
            );
            assert!(b.len() <= 4000);
        }
        // Distinct dispatches yield distinct probed tails, each holding
        // its own digest inside the 64-scalar window. The slice is
        // whitespace-stripped, so the needle must be stripped too.
        let (s1, s2) = (probe_slice(&b1), probe_slice(&b2));
        assert_ne!(s1, s2);
        let stripped = |s: String| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        assert!(s1.contains(&stripped(expected_correlation(&k1))), "{s1}");
        assert!(s2.contains(&stripped(expected_correlation(&k2))), "{s2}");
        // The pre-change world: strip the suffix and both bodies share
        // the boilerplate tail — the collision this fixes.
        let b1_shared = b1.replace(&expected_correlation(&k1), "");
        let b2_shared = b2.replace(&expected_correlation(&k2), "");
        assert_eq!(probe_slice(&b1_shared), probe_slice(&b2_shared));
        // Count model of the visible-pane rule: A's old prompt is on the
        // grid before the paste; after it scrolls off and B renders, B's
        // slice count rises 0 → 1 — a real increase, not a contains.
        let before = normalized(&b1_shared);
        assert_eq!(before.matches(&s2).count(), 0);
        let after = normalized(&b2);
        assert_eq!(after.matches(&s2).count(), 1);
        // An absent tail still proves nothing: a pane that never paints
        // B's ending contributes no occurrence of its slice.
        assert_eq!(normalized("→ draft placeholder").matches(&s2).count(), 0);
    }

    /// CAD-160: criteria that cannot fit the pty ceiling whole refuse
    /// the kickoff — the error names the ceiling and the spec file, and
    /// nothing is queued or transitioned. (Replaces the old truncation,
    /// which cut the criteria to "(truncated — full criteria in the
    /// spec file).")
    #[test]
    fn oversized_criteria_refuse_the_kickoff_and_queue_nothing() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        seeded_job(&s, &cwd);
        reg_pty(&s, "w1", &cwd);
        s.create_task(
            "j1",
            "t1",
            None,
            Some("w1"),
            None,
            Some(&"a".repeat(5000)),
            None,
            None,
            None,
        )
        .unwrap();
        let err = s
            .dispatch_task("t1", None, Some("k-long"), "test")
            .unwrap_err()
            .to_string();
        assert!(err.contains("4000-char"), "{err}");
        assert!(err.contains("spec file /s.md"), "{err}");
        assert!(err.contains("Nothing was queued"), "{err}");
        // QA N1: a kickoff has no sender text — the hint names only
        // the criteria.
        assert!(err.contains("the kickoff is"), "{err}");
        assert!(err.contains("shorten the criteria"), "{err}");
        assert!(!err.contains("text"), "{err}");
        assert!(s.message("k-long").unwrap().is_none());
        assert_eq!(s.queued_count("w1").unwrap(), 0);
        let task = s.task("t1").unwrap();
        assert_eq!(task.state, "draft");
        assert_eq!(task.revision, 0);
        assert!(task.dispatch_message.is_none());
    }

    /// CAD-160: an open task's message is the sender's text, then the
    /// objective, then only the still-unchecked criteria — CAD-300's
    /// numbered, JSON-quoted form, renumbered — on one control-free line.
    #[test]
    fn task_message_restates_objective_then_outstanding_criteria() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        seeded_job(&s, &cwd);
        reg_pty(&s, "w1", &cwd);
        s.create_task(
            "j1",
            "t1",
            Some("Ship the\nemail change"),
            Some("w1"),
            Some("/specs/t1.md"),
            Some(r#"1) [x] "done already"; 2) [ ] "use V2; [x] keep tests"; 3) [ ] "tab\there""#),
            None,
            None,
            None,
        )
        .unwrap();
        let body = s
            .compose_task_message("t1", "w1", "actually, use the V1 provider", 4000)
            .unwrap();
        assert_eq!(
            body,
            "actually, use the V1 provider — Task t1 (job j1) is still open; this message \
             amends it and does not replace it. Objective: Ship the email change. Spec: \
             /specs/t1.md. Outstanding criteria: 1) [ ] \"use V2; [x] keep tests\"; \
             2) [ ] \"tab\\there\"."
        );
        assert!(!crate::adapter::pty::has_control_chars(&body), "{body}");
        let (items, rest) = crate::issue::dispatch::parse_acceptance_listing(
            body.split_once("Outstanding criteria: ").unwrap().1,
        )
        .unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| !i.checked));
        assert_eq!(items[1].text, "tab\there");
        assert_eq!(rest, ".");
        // Free-form criteria are one outstanding item; a task with every
        // item checked still restates the objective, with none owed.
        s.create_task(
            "j1",
            "t2",
            None,
            Some("w1"),
            None,
            Some("green; tests"),
            None,
            None,
            None,
        )
        .unwrap();
        let body = s.compose_task_message("t2", "w1", "ping", 4000).unwrap();
        assert!(
            body.ends_with(
                "Objective: implement per spec. Spec: /s.md. Outstanding criteria: \
                 1) [ ] \"green; tests\"."
            ),
            "{body}"
        );
        s.create_task(
            "j1",
            "t3",
            None,
            Some("w1"),
            None,
            Some(r#"1) [x] "a""#),
            None,
            None,
            None,
        )
        .unwrap();
        let body = s.compose_task_message("t3", "w1", "ping", 4000).unwrap();
        assert!(body.ends_with("Outstanding criteria: none."), "{body}");
    }

    /// CAD-160: a terminal task's message is delivered byte-identical.
    #[test]
    fn terminal_task_message_is_unchanged() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        seeded_job(&s, &cwd);
        s.create_task(
            "j1",
            "t1",
            None,
            Some("pm"),
            None,
            Some("green"),
            None,
            None,
            None,
        )
        .unwrap();
        s.cancel_task("t1", "test").unwrap();
        let text = "  exact\ttext, trailing space ";
        assert_eq!(
            s.compose_task_message("t1", "pm", text, 4000).unwrap(),
            text
        );
    }

    /// CAD-160 (QA N2): only the task's assignee is on the hook, so a
    /// `--task` message to anyone else — a worker's note to its PM —
    /// and one bound to an unassigned task go out byte-identical.
    #[test]
    fn task_message_to_a_non_assignee_is_unchanged() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        seeded_job(&s, &cwd);
        reg_pty(&s, "w1", &cwd);
        s.create_task(
            "j1",
            "t1",
            None,
            Some("w1"),
            None,
            Some("green"),
            None,
            None,
            None,
        )
        .unwrap();
        s.create_task(
            "j1",
            "t2",
            None,
            None,
            None,
            Some("green"),
            None,
            None,
            None,
        )
        .unwrap();
        let text = "PR is up — see #1 ";
        assert_eq!(
            s.compose_task_message("t1", "pm", text, 4000).unwrap(),
            text
        );
        assert_eq!(
            s.compose_task_message("t2", "w1", text, 4000).unwrap(),
            text
        );
        assert_ne!(
            s.compose_task_message("t1", "w1", text, 4000).unwrap(),
            text
        );
    }

    /// CAD-160 (QA N3): a composed message needs an amendment — blank
    /// text is refused rather than sent as a bare restatement.
    #[test]
    fn task_message_refuses_blank_text() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        seeded_job(&s, &cwd);
        reg_pty(&s, "w1", &cwd);
        s.create_task(
            "j1",
            "t1",
            None,
            Some("w1"),
            None,
            Some("green"),
            None,
            None,
            None,
        )
        .unwrap();
        for text in ["", "  \t "] {
            let err = s
                .compose_task_message("t1", "w1", text, 4000)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("Prompt must contain 1-48000 characters"),
                "{err}"
            );
            assert!(err.contains("non-blank"), "{err}");
        }
    }

    /// CAD-160: over the ceiling the restated objective gives way and
    /// the criteria stay whole; criteria that cannot fit refuse, naming
    /// the ceiling and the spec file.
    #[test]
    fn task_message_cuts_the_objective_never_the_criteria() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        seeded_job(&s, &cwd);
        let criteria = "c".repeat(800);
        s.create_task(
            "j1",
            "t1",
            Some(&"T".repeat(3000)),
            Some("pm"),
            None,
            Some(&criteria),
            None,
            None,
            None,
        )
        .unwrap();
        let text = "p".repeat(500);
        let body = s.compose_task_message("t1", "pm", &text, 4000).unwrap();
        assert!(body.len() <= 4000, "{} bytes", body.len());
        assert!(body.starts_with(&format!("{text} — Task t1")), "{body}");
        assert!(body.contains("T…. Spec: /s.md."), "{body}");
        assert!(
            body.ends_with(&format!("Outstanding criteria: 1) [ ] \"{criteria}\".")),
            "{body}"
        );
        s.create_task(
            "j1",
            "t2",
            None,
            Some("pm"),
            None,
            Some(&"c".repeat(5000)),
            None,
            None,
            None,
        )
        .unwrap();
        let err = s
            .compose_task_message("t2", "pm", "ping", 4000)
            .unwrap_err()
            .to_string();
        assert!(err.contains("4000-char"), "{err}");
        assert!(err.contains("spec file /s.md"), "{err}");
    }

    /// CAD-160: sweep the criteria across the 4000-byte ceiling. Every
    /// body that goes out carries the criteria whole, the report
    /// contract and the correlation; near the ceiling the prose (the
    /// closing reminder, the long issue note) gives way; past it the
    /// kickoff refuses. The truncation note is gone.
    #[test]
    fn kickoff_prose_gives_way_and_criteria_stay_whole_at_the_ceiling() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        seeded_job(&s, &cwd);
        reg_pty(&s, "w1", &cwd);
        s.create_task("j1", "t1", None, Some("w1"), None, None, None, None, None)
            .unwrap();
        let mut job = s.job("j1").unwrap();
        job.issue_id = Some("CAD-9".into());
        let mut task = s.task("t1").unwrap();
        let worker = s.agent("w1").unwrap();
        let mid = "mid-0123456789abcdef";
        // sha256sum over the id bytes, computed outside this code — the
        // test pins the encoding instead of re-deriving it from the
        // function under test.
        let correlation = " Correlation: 51bb19c5d45a3b839228091e46563550.";
        let (mut saw_full, mut saw_compact, mut saw_refused) = (false, false, false);
        for len in 3300..=4000 {
            let criteria = "x".repeat(len);
            task.acceptance = Some(criteria.clone());
            match kickoff_body(&job, &task, 1, mid, &worker) {
                Ok(out) => {
                    assert!(out.len() <= 4000, "len {len}: {} bytes", out.len());
                    assert!(
                        out.contains(&format!(" Acceptance: {criteria}.")),
                        "len {len}"
                    );
                    assert!(out.ends_with(correlation), "len {len}: {out}");
                    assert!(out.contains(&format!("cadence message result {mid}")));
                    assert!(!out.chars().any(|c| c.is_control()), "len {len}");
                    assert!(!out.contains("truncated"), "len {len}: {out}");
                    if out.contains("Do not report a SHA you have not committed.") {
                        assert!(out.contains("put the header line `Issue: CAD-9`"));
                        saw_full = true;
                    } else {
                        assert!(out.contains(" Issue: CAD-9. Report when done"), "{out}");
                        saw_compact = true;
                    }
                }
                Err(e) => {
                    let e = e.to_string();
                    assert!(e.contains("4000-char") && e.contains("/s.md"), "{e}");
                    saw_refused = true;
                }
            }
        }
        assert!(
            saw_full && saw_compact && saw_refused,
            "sweep must cross both boundaries"
        );
        // Multibyte criteria are never cut mid-codepoint — they are
        // never cut at all: whole, or refused.
        task.acceptance = Some("界".repeat(1100));
        let out = kickoff_body(&job, &task, 1, mid, &worker).unwrap();
        assert!(out.contains(&"界".repeat(1100)), "{out}");
        assert!(out.ends_with(correlation), "{out}");
        task.acceptance = Some("界".repeat(1400));
        assert!(kickoff_body(&job, &task, 1, mid, &worker).is_err());
        // Same id ⇒ same body: the tail is a pure function of the
        // durable id, so a repaste of one message keeps one slice.
        task.acceptance = Some("界".repeat(1100));
        assert_eq!(out, kickoff_body(&job, &task, 1, mid, &worker).unwrap());
    }

    #[test]
    fn kickoff_body_strips_control_fields_and_keeps_pinned_tail() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        seeded_job(&s, &cwd);
        reg_pty(&s, "w1", &cwd);
        s.create_task("j1", "t1", None, Some("w1"), None, None, None, None, None)
            .unwrap();
        let job = s.job("j1").unwrap();
        let mut task = s.task("t1").unwrap();
        let worker = s.agent("w1").unwrap();
        // Newline, a C0 control and a C1 control in the free-form fields
        // — `clean` maps them to spaces before assembly, so the paste
        // stays one control-free line and still ends in the pinned
        // digest of `mid-0123456789abcdef`.
        task.acceptance = Some("line one\nline two\u{7}more\u{85}end".to_string());
        task.spec_path = Some("spec\tdir/file\nname.md".to_string());
        let out = kickoff_body(&job, &task, 1, "mid-0123456789abcdef", &worker).unwrap();
        assert!(!out.chars().any(|c| c.is_control()), "{out}");
        assert!(!out.contains('\n'), "{out}");
        assert!(out.len() <= 4000);
        assert!(
            out.ends_with(" Correlation: 51bb19c5d45a3b839228091e46563550."),
            "{out}"
        );
    }

    #[test]
    fn kickoff_correlation_bounds_long_and_unicode_ids() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        seeded_job(&s, &cwd);
        reg_pty(&s, "w1", &cwd);
        s.create_task("j1", "t1", None, Some("w1"), None, None, None, None, None)
            .unwrap();
        // A 64-char `--message` override (the longest legal id) still
        // yields the fixed-size suffix — not a 64-char tail.
        let long_id = "z".repeat(64);
        let (k, b) = dispatch_body(&s, "t1", Some(&long_id));
        assert_eq!(k, long_id);
        assert!(b.ends_with(&expected_correlation(&k)), "{b}");
        // Beyond the legal grammar the function still stays bounded and
        // control-free — the digest never leaks raw id bytes.
        let job = s.job("j1").unwrap();
        let task = s.task("t1").unwrap();
        let worker = s.agent("w1").unwrap();
        let weird = "κickoff-任务-✓".repeat(15);
        let out = kickoff_body(&job, &task, 1, &weird, &worker).unwrap();
        assert!(out.ends_with(&expected_correlation(&weird)), "{out}");
        assert!(!out.chars().any(|c| c.is_control()));
        assert_ne!(expected_correlation(&long_id), expected_correlation(&weird));
        // The suffix is 47 chars regardless of id length or alphabet.
        assert_eq!(expected_correlation(&long_id).len(), 47);
        assert_eq!(expected_correlation(&weird).len(), 47);
    }

    #[test]
    fn managed_kickoff_envelope_is_unchanged() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        seeded_job(&s, &cwd);
        // `fake/fake` reports via TurnResult — the managed envelope.
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
        s.create_task(
            "j1",
            "t1",
            None,
            Some("w1"),
            None,
            Some("green tests"),
            None,
            None,
            None,
        )
        .unwrap();
        let (_, b) = dispatch_body(&s, "t1", None);
        assert!(!b.contains("Correlation"), "{b}");
        assert!(b.contains("SHA: <40-hex>"), "{b}");
        assert!(
            b.ends_with("Do not report a SHA you have not committed."),
            "{b}"
        );
        // CAD-160: the compact managed form (prose dropped, criteria
        // whole) ends on the report contract — no suffix reserved, no
        // suffix appended — and criteria past the ceiling refuse.
        let job = s.job("j1").unwrap();
        let worker = s.agent("w1").unwrap();
        let mut task = s.task("t1").unwrap();
        task.acceptance = Some("a".into());
        let base = kickoff_body(&job, &task, 1, "m", &worker).unwrap().len() - 1;
        let criteria = "a".repeat(4000 - base + 10);
        task.acceptance = Some(criteria.clone());
        let b2 = kickoff_body(&job, &task, 1, "m", &worker).unwrap();
        assert!(b2.len() <= 4000);
        assert!(!b2.contains("Correlation"), "{b2}");
        assert!(b2.contains(&format!(" Acceptance: {criteria}.")), "{b2}");
        assert!(b2.ends_with("reported revision."), "{b2}");
        task.acceptance = Some("a".repeat(5000));
        assert!(kickoff_body(&job, &task, 1, "m", &worker).is_err());
    }
