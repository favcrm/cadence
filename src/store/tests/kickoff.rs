use super::*;

    #[test]
    fn cloud_hold_is_a_monitor_alert() {
        assert!(Store::monitor_alert_kind("cloud_hold"));
        assert!(Store::monitor_alert_kind("cloud_recover_escalated"));
    }

    #[test]
    fn cloud_kickoff_inlines_spec_without_a_host_path() {
        let dir = tempfile::tempdir().unwrap();
        let spec = dir.path().join("spec.md");
        std::fs::write(&spec, "Implement the cloud task from this text.").unwrap();
        let worktree = dir.path().join("wt");
        let job = Job {
            id: "j1".into(),
            title: None,
            spec_path: spec.display().to_string(),
            spec_sha256: None,
            pm_alias: "pm".into(),
            issue_id: None,
            repo: None,
            base_ref: None,
            state: "open".into(),
            max_revisions: 2,
            stall_secs: None,
            error: None,
            created: 0.0,
            updated: 0.0,
        };
        let task = Task {
            id: "t1".into(),
            job_id: "j1".into(),
            title: None,
            role: "worker".into(),
            assignee: Some("cloud-1".into()),
            spec_path: Some(spec.display().to_string()),
            acceptance: Some("the task is done".into()),
            worktree: Some(worktree.display().to_string()),
            branch: Some("cadence/cloud".into()),
            base_sha: Some("abc".into()),
            head_sha: None,
            state: "draft".into(),
            revision: 0,
            dispatch_message: None,
            error: None,
            created: 0.0,
            updated: 0.0,
        };
        let assignee = Agent {
            alias: "cloud-1".into(),
            provider: "devin".into(),
            endpoint_kind: "cloud".into(),
            role: "worker".into(),
            team_role: None,
            cwd: worktree.display().to_string(),
            sandbox: "read-only".into(),
            instructions: None,
            thread_id: None,
            session_id: None,
            model: None,
            effort: None,
            pid: None,
            pid_start: None,
            endpoint: None,
            params: None,
            model_selection: None,
            quota: None,
            generation: None,
            state: "starting".into(),
            enabled: true,
            error: None,
            created: 0.0,
            updated: 0.0,
        };
        let body = kickoff_body(&job, &task, 1, "m1", &assignee).unwrap();
        assert!(
            body.contains("Implement the cloud task from this text."),
            "{body}"
        );
        assert!(body.contains("SHA:"), "{body}");
        assert!(!body.contains(&spec.display().to_string()), "{body}");
        assert!(!body.contains(&worktree.display().to_string()), "{body}");
        assert!(!body.contains("cadence self"), "{body}");
    }

    #[test]
    fn cloud_kickoff_states_when_the_spec_is_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let spec = dir.path().join("spec.md");
        let mut raw = "你".repeat(20_000);
        raw.push_str("UNIQUE_TAIL_MARKER");
        std::fs::write(&spec, &raw).unwrap();
        let job = Job {
            id: "j1".into(),
            title: None,
            spec_path: spec.display().to_string(),
            spec_sha256: None,
            pm_alias: "pm".into(),
            issue_id: None,
            repo: None,
            base_ref: None,
            state: "open".into(),
            max_revisions: 2,
            stall_secs: None,
            error: None,
            created: 0.0,
            updated: 0.0,
        };
        let task = Task {
            id: "t1".into(),
            job_id: "j1".into(),
            title: None,
            role: "worker".into(),
            assignee: Some("cloud-1".into()),
            spec_path: Some(spec.display().to_string()),
            acceptance: None,
            worktree: None,
            branch: None,
            base_sha: None,
            head_sha: None,
            state: "draft".into(),
            revision: 0,
            dispatch_message: None,
            error: None,
            created: 0.0,
            updated: 0.0,
        };
        let body = cloud_kickoff_body(&job, &task, 1).unwrap();
        assert!(
            body.contains("spec text truncated; the inlined copy is incomplete"),
            "{body}"
        );
        assert!(!body.contains("UNIQUE_TAIL_MARKER"), "{body}");
        assert!(body.contains("SHA:"), "{body}");
        assert!(body.len() <= 48_000, "kickoff is {} bytes", body.len());
        assert!(!body.contains(&spec.display().to_string()), "{body}");
    }

    /// CAD-160: replaces the old cut, which kept the SHA trailer by
    /// truncating the acceptance criteria.
    #[test]
    fn cloud_kickoff_keeps_criteria_whole_and_refuses_what_cannot_fit() {
        let dir = tempfile::tempdir().unwrap();
        let spec = dir.path().join("spec.md");
        std::fs::write(&spec, "Keep this short spec sentence.").unwrap();
        let job = Job {
            id: "j1".into(),
            title: None,
            spec_path: spec.display().to_string(),
            spec_sha256: None,
            pm_alias: "pm".into(),
            issue_id: None,
            repo: None,
            base_ref: None,
            state: "open".into(),
            max_revisions: 2,
            stall_secs: None,
            error: None,
            created: 0.0,
            updated: 0.0,
        };
        let task = Task {
            id: "t1".into(),
            job_id: "j1".into(),
            title: None,
            role: "worker".into(),
            assignee: Some("cloud-1".into()),
            spec_path: Some(spec.display().to_string()),
            acceptance: Some("A".repeat(60_000)),
            worktree: None,
            branch: None,
            base_sha: None,
            head_sha: None,
            state: "draft".into(),
            revision: 0,
            dispatch_message: None,
            error: None,
            created: 0.0,
            updated: 0.0,
        };
        // CAD-160: criteria that cannot fit whole refuse, naming the
        // ceiling and the spec file — they are never cut.
        let err = cloud_kickoff_body(&job, &task, 1).unwrap_err().to_string();
        assert!(err.contains("48000-char"), "{err}");
        assert!(err.contains(&spec.display().to_string()), "{err}");
        // Long criteria beside a long spec: the spec gives way, the
        // criteria and the report contract stay whole.
        let criteria = "A".repeat(40_000);
        std::fs::write(&spec, "S".repeat(20_000)).unwrap();
        let mut task = task;
        task.acceptance = Some(criteria.clone());
        let body = cloud_kickoff_body(&job, &task, 1).unwrap();
        assert!(body.len() <= 48_000, "kickoff is {} bytes", body.len());
        assert!(body.contains(&format!(" Acceptance: {criteria}.")));
        assert!(body.contains("spec text truncated"), "{body}");
        assert!(
            body.ends_with("Do not report a SHA you have not committed."),
            "{body}"
        );
        assert!(body.contains("`SHA: <40-hex>`"), "{body}");
    }

    #[test]
    fn cloud_kickoff_omits_the_spec_when_the_issue_alone_exceeds_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let spec = dir.path().join("spec.md");
        std::fs::write(&spec, "UNIQUE_SPEC_BODY must not survive").unwrap();
        let job = Job {
            id: "j1".into(),
            title: None,
            spec_path: spec.display().to_string(),
            spec_sha256: None,
            pm_alias: "pm".into(),
            issue_id: Some("i".repeat(50_000)),
            repo: None,
            base_ref: None,
            state: "open".into(),
            max_revisions: 2,
            stall_secs: None,
            error: None,
            created: 0.0,
            updated: 0.0,
        };
        let task = Task {
            id: "t1".into(),
            job_id: "j1".into(),
            title: None,
            role: "worker".into(),
            assignee: Some("cloud-1".into()),
            spec_path: Some(spec.display().to_string()),
            acceptance: None,
            worktree: None,
            branch: None,
            base_sha: None,
            head_sha: None,
            state: "draft".into(),
            revision: 0,
            dispatch_message: None,
            error: None,
            created: 0.0,
            updated: 0.0,
        };
        let body = cloud_kickoff_body(&job, &task, 1).unwrap();
        assert!(body.len() <= 48_000, "kickoff is {} bytes", body.len());
        assert!(
            body.ends_with("Do not report a SHA you have not committed."),
            "{body}"
        );
        assert!(
            body.contains("spec text omitted"),
            "oversized issue did not drop the spec: {body}"
        );
        assert!(!body.contains("UNIQUE_SPEC_BODY"), "{body}");
    }

    #[test]
    fn cloud_omit_host_paths_strips_a_backticked_path() {
        let text = "see `/home/ubuntu/secret` and \"/tmp/x\" and (~/notes/a) plus ~/bare and https://example.com/a";
        let out = omit_host_paths(text);
        assert!(!out.contains("/home/ubuntu/secret"), "{out}");
        assert!(!out.contains("/tmp/x"), "{out}");
        assert!(!out.contains("~/notes"), "{out}");
        assert!(!out.contains("~/bare"), "{out}");
        assert!(out.contains("https://example.com/a"), "{out}");
        assert!(out.contains("see"), "{out}");
    }

    #[test]
    fn cloud_omit_host_paths_keeps_a_versioned_api_path() {
        let text = "call `/v3/organizations/acme/sessions` and `/v3beta1/organizations/acme/repositories` but not `/var/www/notes` or `/etc/hosts`";
        let out = omit_host_paths(text);
        assert!(out.contains("/v3/organizations/acme/sessions"), "{out}");
        assert!(
            out.contains("/v3beta1/organizations/acme/repositories"),
            "{out}"
        );
        assert!(!out.contains("/var/www/notes"), "{out}");
        assert!(!out.contains("/etc/hosts"), "{out}");
    }

    #[test]
    fn cloud_two_holds_on_one_agent_escalate_twice() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg(&s, "w1", &cwd);
        reg(&s, "pm", &cwd);
        s.enqueue("w1", "one", Some("pm"), "m1", "user").unwrap();
        s.enqueue("w1", "two", Some("pm"), "m2", "user").unwrap();
        let first = s.message("m1").unwrap().unwrap();
        let second = s.message("m2").unwrap().unwrap();
        s.escalate_cloud_hold(&first, "budget").unwrap();
        s.escalate_cloud_hold(&first, "budget").unwrap();
        s.escalate_cloud_hold(&second, "budget").unwrap();
        let escalations = s
            .events("w1", 0, 40)
            .unwrap()
            .iter()
            .filter(|event| event.kind == "cloud_recover_escalated")
            .count();
        assert_eq!(escalations, 2);
        let notices: Vec<_> = s
            .messages("pm")
            .unwrap()
            .into_iter()
            .filter(|message| message.body.contains("stopped polling"))
            .collect();
        assert_eq!(notices.len(), 2);
        assert!(notices.iter().all(|message| {
            message.body.contains("`cadence message reconcile ")
                && message
                    .body
                    .contains(" --status <completed|failed|interrupted>` (add `--sha <40-hex>`")
                && message
                    .body
                    .contains("`cadence agent unfence w1 --status <completed|failed|interrupted>`")
                && message.body.contains("not fenced")
                && !message.body.contains("cadence agent stop")
        }));
    }
