
    /// The conditional transition used by approval relaxation cannot
    /// overwrite a finished/fenced state or its error reason.
    #[test]
    fn conditional_state_preserves_terminal_states() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        reg(&s, "a1", &cwd);
        // waiting_input -> busy applies.
        s.set_agent_state("a1", "waiting_input", None).unwrap();
        assert!(s.set_agent_state_if("a1", "busy", "waiting_input").unwrap());
        assert_eq!(s.agent("a1").unwrap().state, "busy");
        // attention + error are preserved — no busy overwrite.
        s.set_agent_state("a1", "attention", Some("turn outcome unknown"))
            .unwrap();
        assert!(!s.set_agent_state_if("a1", "busy", "waiting_input").unwrap());
        let agent = s.agent("a1").unwrap();
        assert_eq!(agent.state, "attention");
        assert_eq!(agent.error.as_deref(), Some("turn outcome unknown"));
        // Same for stopped.
        s.set_agent_state("a1", "stopped", None).unwrap();
        assert!(!s.set_agent_state_if("a1", "busy", "waiting_input").unwrap());
        assert_eq!(s.agent("a1").unwrap().state, "stopped");
    }

    #[test]
    fn provider_quota_update_is_fenced_to_current_thread() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        s.register_agent(&NewAgent {
            alias: "codex",
            provider: "codex",
            endpoint_kind: "managed",
            role: "worker",
            cwd: cwd.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
        let identity = crate::adapter::Identity {
            thread_id: "current-thread".into(),
            session_id: "session".into(),
            model: Some("mock-model".into()),
            effort: Some("medium".into()),
            pid: 1,
            endpoint: None,
            generation: None,
            attach: None,
        };
        s.set_identity_with_quota(
            "codex",
            &identity,
            Some(json!({
                "state": "reported",
                "source": "account/rateLimits/read",
                "data": {"accountId": "acct", "rateLimits": {
                    "primary": {"usedPercent": 10}
                }}
            })),
        )
        .unwrap();
        let accepted = s
            .update_provider_quota(
                "codex",
                "codex",
                "old-thread",
                &json!({
                    "source": "account/rateLimits/updated",
                    "data": {"accountId": "spoof", "rateLimits": {
                        "primary": {"usedPercent": 99}
                    }}
                }),
            )
            .unwrap();
        assert!(!accepted);
        let quota = s.agent("codex").unwrap().quota.unwrap();
        assert_eq!(quota["thread_id"], "current-thread");
        assert_eq!(quota["account_id"], "acct");
        assert_eq!(quota["data"]["rateLimits"]["primary"]["usedPercent"], 10);
    }

    #[test]
    fn automatic_quota_guard_requires_provider_bound_canonical_evidence() {
        let observed_at = crate::issue::time::iso(crate::issue::time::now_epoch());
        let valid = json!({
            "provider": "codex",
            "assignee": "worker",
            "account_id": "acct",
            "thread_id": "thread",
            "state": "available",
            "source": "account/rateLimits/read",
            "observed_at": observed_at,
            "updated_at": observed_at,
            "data": {"accountId": "acct", "rateLimits": {"primary": {}}}
        });
        let mut agent = Agent {
            alias: "worker".into(),
            provider: "codex".into(),
            endpoint_kind: "managed-ws".into(),
            role: "worker".into(),
            team_role: None,
            cwd: "/tmp".into(),
            sandbox: "read-only".into(),
            instructions: None,
            thread_id: Some("thread".into()),
            session_id: Some("session".into()),
            model: None,
            effort: None,
            pid: None,
            pid_start: None,
            endpoint: None,
            // Even a complete-looking caller value cannot substitute for
            // provider-owned evidence.
            params: Some(json!({"quota": {"source": "provider", "remaining": 99}})),
            model_selection: None,
            quota: Some(valid),
            generation: None,
            state: "idle".into(),
            enabled: true,
            error: None,
            created: now(),
            updated: now(),
        };
        assert_eq!(automatic_quota_error(&agent), None);

        agent.quota.as_mut().unwrap()["observed_at"] = json!(now());
        assert!(automatic_quota_error(&agent)
            .unwrap()
            .contains("canonical timestamp"));

        agent.quota = Some(json!({
            "provider": "codex",
            "assignee": "worker",
            "account_id": "other",
            "thread_id": "thread",
            "state": "available",
            "source": "account/rateLimits/read",
            "observed_at": crate::issue::time::iso(crate::issue::time::now_epoch()),
            "data": {"accountId": "acct", "rateLimits": {"primary": {}}}
        }));
        assert!(automatic_quota_error(&agent)
            .unwrap()
            .contains("account identity"));
    }

    /// CAD-385 acceptance 1: every path that records an endpoint pid —
    /// a plain open and a hot-restart adoption alike — records that
    /// process's `/proc` start time with it, and every path that clears
    /// the pid clears the start: a detach, and the recovery a daemon
    /// restart runs before its panes are adopted again.
    #[test]
    fn cad385_every_recorded_pid_carries_its_start_time_and_clears_with_it() {
        let (dir, s) = store();
        reg(&s, "w1", &dir.path().join("w"));
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let child_start = crate::peer::proc_starttime(child.id()).map(|t| t as i64);
        assert!(child_start.is_some());

        s.set_identity("w1", &endpoint_at(child.id())).unwrap();
        assert_eq!(
            recorded_start(&s, "w1"),
            (Some(child.id() as i64), child_start)
        );
        assert_eq!(
            s.agent("w1").unwrap().to_json()["pid_start"],
            json!(child_start)
        );

        // A pty row reads back through the pane map with its start.
        s.conn()
            .execute("UPDATE agents SET endpoint_kind='pty' WHERE alias='w1'", [])
            .unwrap();
        assert_eq!(
            s.pty_pane_pids().unwrap(),
            vec![("w1".to_string(), child.id(), child_start.map(|t| t as u64))]
        );

        // Adoption records the adopted process — here another one.
        let me = std::process::id();
        s.set_identity_adopted("w1", &endpoint_at(me), &[]).unwrap();
        assert_eq!(
            recorded_start(&s, "w1"),
            (
                Some(me as i64),
                crate::peer::proc_starttime(me).map(|t| t as i64)
            )
        );

        s.set_state_detached("w1", "offline", None).unwrap();
        assert_eq!(recorded_start(&s, "w1"), (None, None));

        // Restart: recovery clears pid and start together.
        s.set_identity("w1", &endpoint_at(child.id())).unwrap();
        drop(s);
        let s = Store::open(&dir.path().join("t.sqlite3")).unwrap();
        assert_eq!(recorded_start(&s, "w1"), (None, None));

        // An unreadable process (a pid-less endpoint's 0) records none.
        s.set_identity("w1", &endpoint_at(0)).unwrap();
        assert_eq!(recorded_start(&s, "w1"), (Some(0), None));
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn model_defaults_migrate_conflict_and_keep_existing_rows() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let cwd_s = cwd.to_str().unwrap().to_string();
        s.register_agent(&NewAgent {
            alias: "legacy",
            provider: "claude",
            endpoint_kind: "managed",
            role: "worker",
            cwd: &cwd_s,
            sandbox: "read-only",
            instructions: None,
            params: Some(r#"{"model":"kept"}"#),
            team_role: None,
            model_policy: Some("provider_default"),
        })
        .unwrap_err();
        s.register_agent(&NewAgent {
            alias: "legacy",
            provider: "claude",
            endpoint_kind: "managed",
            role: "worker",
            cwd: &cwd_s,
            sandbox: "read-only",
            instructions: None,
            params: Some(r#"{"model":"kept"}"#),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
        let before = s.agent("legacy").unwrap();
        assert_eq!(before.param_str("model"), Some("kept"));
        assert_eq!(
            before.model_selection.as_ref().unwrap()["source"],
            "explicit"
        );
        let snap = s.model_defaults().unwrap();
        assert_eq!(snap.revision, 0);
        assert!(snap.config.providers.is_empty());

        let doc = defaults_body(
            0,
            r#"{"claude":{"default":{"mode":"model","model":"baseline-a"},"roles":{"qa":{"mode":"model","model":"qa-model"}}}}"#,
        );
        let path = dir.path().join("t.sqlite3");
        let other = Store::open(&path).unwrap();
        let saved = s.replace_model_defaults(&doc, None).unwrap();
        assert_eq!(saved.revision, 1);
        let conflict = other.replace_model_defaults(&doc, Some("operator (ui)"));
        let err = conflict.unwrap_err();
        assert_eq!(err.kind(), "conflict");
        assert_eq!(err.revision(), Some(1));
        assert_eq!(s.model_defaults().unwrap().revision, 1);
        let legacy = s.agent("legacy").unwrap();
        assert_eq!(legacy.param_str("model"), Some("kept"));
        assert_eq!(
            legacy.model_selection.as_ref().unwrap()["source"],
            "explicit"
        );

        let bad = defaults_body(
            1,
            r#"{"nope":{"default":{"mode":"provider_default"},"roles":{}}}"#,
        );
        assert!(s.replace_model_defaults(&bad, None).is_err());
        assert_eq!(s.model_defaults().unwrap().revision, 1);

        let events = s.events(Store::DAEMON_STREAM, 0, 20).unwrap();
        let audit = events
            .iter()
            .find(|event| event.kind == "model_defaults_updated")
            .unwrap();
        assert_eq!(audit.payload["revision"], 1);
        assert_eq!(audit.payload["attribution"], "local");
        assert_eq!(audit.payload["transport"], "local");
        assert!(audit.payload["before"].is_object());
        assert!(audit.payload["after"].is_object());

        s.register_agent(&claude_worker("ops1", &cwd_s, Some("ops")))
            .unwrap();
        let ops = s.agent("ops1").unwrap();
        assert_eq!(ops.role, "worker");
        assert_eq!(ops.team_role.as_deref(), Some("devops"));
        assert_eq!(ops.param_str("model"), Some("baseline-a"));
        assert_eq!(
            ops.model_selection.as_ref().unwrap()["source"],
            "provider_baseline"
        );
        s.register_agent(&claude_worker("qa1", &cwd_s, Some("qa")))
            .unwrap();
        let qa = s.agent("qa1").unwrap();
        assert_eq!(qa.role, "worker");
        assert_eq!(qa.team_role.as_deref(), Some("qa"));
        assert_eq!(qa.param_str("model"), Some("qa-model"));
        assert_eq!(
            qa.model_selection.as_ref().unwrap()["source"],
            "role_default"
        );
        assert_eq!(qa.model_selection.as_ref().unwrap()["revision"], 1);
        assert_eq!(qa.to_json()["model_source"], "configured");
        assert_eq!(qa.to_json()["model_configured"], "qa-model");

        let next = defaults_body(
            1,
            r#"{"claude":{"default":{"mode":"model","model":"baseline-b"},"roles":{}}}"#,
        );
        s.replace_model_defaults(&next, Some("operator (ui)"))
            .unwrap();
        assert_eq!(s.agent("qa1").unwrap().param_str("model"), Some("qa-model"));
        s.register_agent(&claude_worker("fresh", &cwd_s, None))
            .unwrap();
        let fresh = s.agent("fresh").unwrap();
        assert_eq!(fresh.param_str("model"), Some("baseline-b"));
        assert_eq!(
            fresh.model_selection.as_ref().unwrap()["source"],
            "provider_baseline"
        );

        s.set_params("fresh", &json!({"model": " "})).unwrap_err();
        assert_eq!(
            s.agent("fresh").unwrap().param_str("model"),
            Some("baseline-b")
        );
        s.set_params("fresh", &json!({"model": "pinned"})).unwrap();
        s.set_params("fresh", &json!({"model": Value::Null}))
            .unwrap();
        let cleared = s.agent("fresh").unwrap();
        assert!(cleared.param_str("model").is_none());
        assert_eq!(
            cleared.model_selection.as_ref().unwrap()["source"],
            "explicit_provider_default"
        );
        assert_eq!(s.agent("qa1").unwrap().param_str("model"), Some("qa-model"));

        s.register_agent(&NewAgent {
            alias: "box",
            provider: "inbox",
            endpoint_kind: "inbox",
            role: "worker",
            cwd: &cwd_s,
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: Some("pm"),
            model_policy: None,
        })
        .unwrap();
        let inbox = s.agent("box").unwrap();
        assert!(inbox.param_str("model").is_none());
        assert!(inbox.model_selection.is_none());
        assert_eq!(inbox.role, "worker");
        assert_eq!(inbox.team_role.as_deref(), Some("pm"));
        assert!(
            s.set_params("box", &json!({"model": "nope"}))
                .unwrap_err()
                .code()
                == Some("unsupported_model_setting")
        );
        assert!(s
            .agent("box")
            .unwrap()
            .params
            .unwrap_or(json!({}))
            .get("model")
            .is_none());

        s.register_agent(&NewAgent {
            alias: "pmbox",
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: &cwd_s,
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: Some("pm"),
            model_policy: None,
        })
        .unwrap();
        s.enqueue("qa1", "note", Some("pmbox"), "m-role", "user")
            .unwrap();
        let queued = s
            .events("qa1", 0, 20)
            .unwrap()
            .into_iter()
            .find(|event| event.kind == "queued")
            .unwrap();
        assert_eq!(queued.payload["recipient_identity"]["role"], "worker");
        assert!(queued.payload["recipient_identity"]
            .get("team_role")
            .is_none());

        let dup = s.register_agent(&claude_worker("qa1", &cwd_s, Some("dev")));
        assert!(dup.unwrap_err().to_string().contains("UNIQUE"));
        assert_eq!(s.agent("qa1").unwrap().team_role.as_deref(), Some("qa"));

        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.model_defaults().unwrap().revision, 2);
        assert_eq!(
            reopened.agent("qa1").unwrap().param_str("model"),
            Some("qa-model")
        );
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE agents SET model_selection=NULL WHERE alias='legacy'",
            [],
        )
        .unwrap();
        drop(conn);
        let labeled = Store::open(&path).unwrap().agent("legacy").unwrap();
        assert_eq!(
            labeled.to_json()["model_selection"]["source"],
            "legacy_configured"
        );
    }

    #[test]
    fn model_defaults_duplicate_near_cap_reports_unique() {
        let (dir, s) = store();
        let cwd = dir.path().join("w");
        let cwd_s = cwd.to_str().unwrap();
        let pad = "n".repeat(3_889);
        let params = format!(r#"{{"note":"{pad}"}}"#);
        assert!(params.len() < crate::model_defaults::MAX_STORED_PARAMS);
        s.register_agent(&NewAgent {
            alias: "near",
            provider: "claude",
            endpoint_kind: "managed",
            role: "worker",
            cwd: cwd_s,
            sandbox: "read-only",
            instructions: None,
            params: Some(&params),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
        let baseline = "m".repeat(200);
        s.replace_model_defaults(
            &defaults_body(
                0,
                &format!(
                    r#"{{"claude":{{"default":{{"mode":"model","model":"{baseline}"}},"roles":{{}}}}}}"#
                ),
            ),
            None,
        )
        .unwrap();
        let dup = s.register_agent(&NewAgent {
            alias: "near",
            provider: "claude",
            endpoint_kind: "managed",
            role: "worker",
            cwd: cwd_s,
            sandbox: "read-only",
            instructions: None,
            params: Some(&params),
            team_role: Some("qa"),
            model_policy: None,
        });
        let err = dup.unwrap_err();
        assert!(err.to_string().contains("UNIQUE"), "{err}");
        assert_ne!(err.code(), Some("params_too_large"));
        let saved = s.agent("near").unwrap();
        assert_eq!(saved.param_str("model"), None);
        assert_eq!(saved.params.unwrap().to_string(), params);
        assert!(saved.team_role.is_none());
        let fresh = s.register_agent(&NewAgent {
            alias: "fresh",
            provider: "claude",
            endpoint_kind: "managed",
            role: "worker",
            cwd: cwd_s,
            sandbox: "read-only",
            instructions: None,
            params: Some(&params),
            team_role: None,
            model_policy: None,
        });
        assert_eq!(fresh.unwrap_err().code(), Some("params_too_large"));
        assert!(s.agent_opt("fresh").unwrap().is_none());
    }

    #[test]
    fn model_defaults_deep_nesting_does_not_commit() {
        let (_dir, s) = store();
        assert_eq!(s.model_defaults().unwrap().revision, 0);
        let mut body = String::from(r#"{"expected_revision":0,"config":"#);
        body.push_str(&"[".repeat(9_000));
        body.push('0');
        body.push_str(&"]".repeat(9_000));
        body.push('}');
        assert!(body.len() <= crate::model_defaults::MAX_HTTP_BODY_BYTES);
        let err = s.replace_model_defaults(&body, None).unwrap_err();
        assert_eq!(err.code(), Some("invalid_config"));
        assert!(err.to_string().contains("nesting exceeds"), "{err}");
        assert_eq!(s.model_defaults().unwrap().revision, 0);
        assert!(s
            .events(Store::DAEMON_STREAM, 0, 50)
            .unwrap()
            .iter()
            .all(|event| event.kind != "model_defaults_updated"));
        s.replace_model_defaults(&defaults_body(0, "{}"), None)
            .unwrap();
        assert_eq!(s.model_defaults().unwrap().revision, 1);
    }
