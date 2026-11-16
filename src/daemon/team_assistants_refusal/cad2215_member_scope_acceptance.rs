use super::*;

fn promoted_member_job(fx: &Fx, sub: &str, receipt: &Value) -> String {
    assert_eq!(
        receipt["status"], "accepted",
        "request was not accepted: {receipt}"
    );
    assert_eq!(
        receipt.as_object().unwrap().len(),
        2,
        "request receipt exposed extra fields: {receipt}"
    );
    let request_id = receipt["request_id"]
        .as_str()
        .expect("request receipt lacks ID");
    let outcomes = fx
        .shared
        .sweep_member_job_requests_for_test()
        .expect("real promotion sweep failed");
    assert!(
        outcomes
            .iter()
            .any(|outcome| outcome.request_id == request_id && outcome.outcome == "created"),
        "free fixture request was not promoted by the real worker: {outcomes:?}"
    );
    let list = fx.op(
        "member_job_list",
        json!({"member_envelope":fx.member_envelope(sub)}),
    );
    let row = list["requests"]
        .as_array()
        .expect("owner request list missing")
        .iter()
        .find(|row| row["request_id"] == request_id)
        .expect("owner request row missing");
    assert_eq!(row["status"], "created");
    row["job"]
        .as_str()
        .expect("created owner request lacks job ID")
        .to_string()
}

struct MemberWork {
    fx: Fx,
    owner: String,
    job: String,
    task: String,
}

fn member_work(fx: Fx) -> MemberWork {
    with_deferred_service(&fx);
    fx.init_cursor_keyring();
    let rt = fx.member_runtime("sub-alice");
    let project = fx.pm().join("open");
    std::fs::create_dir_all(project.join("OPEN-1")).unwrap();
    std::fs::write(project.join("project.yaml"), "key: open\nprefix: OPEN\n").unwrap();
    std::fs::write(project.join("OPEN-1/issue.md"),
        "---\nid: OPEN-1\ntitle: shared project\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n").unwrap();
    let req = fx.op("member_job_request", json!({
        "member_envelope": fx.member_envelope("sub-alice"),
        "title": "private member job", "request_key": "cad2215-shared-worker", "body": "member request",
        "issue":"OPEN-1",
    }));
    let job = promoted_member_job(&fx, "sub-alice", &req);
    assert!(fx
        .shared
        .store
        .assist_request_owns(ORG, "sub-alice", &job)
        .unwrap());
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    for alias in ["unrelated", "other-assignee"] {
        fx.shared
            .store
            .register_agent(&crate::store::NewAgent {
                alias,
                provider: "fake",
                endpoint_kind: "fake",
                role: "worker",
                cwd: fx.dir.path().to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
    }
    let conn = rusqlite::Connection::open(fx.dir.path().join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE tasks SET assignee='ordinary-worker',state='running' WHERE id=?1",
        [&task],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tasks(id,job_id,title,assignee,state,created,updated) VALUES('member-other-task',?1,'other private task','other-assignee','running',1,1)",
        [&job],
    ).unwrap();
    conn.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
         VALUES('public-control-job','Public','spec.md','unrelated','OPEN-1','open',1,1);
         INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
         VALUES('public-control-task','public-control-job','Public','ordinary-worker','running',1,1);",
    ).unwrap();
    fx.shared.store.ensure_thread("ordinary-worker").unwrap();
    fx.shared
        .store
        .enqueue_task(
            "ordinary-worker",
            "MEMBER_PRIVATE_BODY_SENTINEL",
            None,
            "member-private-message",
            "user",
            Some(&task),
        )
        .unwrap();
    fx.shared
        .store
        .enqueue_task(
            "ordinary-worker",
            "PUBLIC_CONTROL_BODY",
            None,
            "public-control-message",
            "user",
            Some("public-control-task"),
        )
        .unwrap();
    fx.shared
        .store
        .event_public_scoped(
            "ordinary-worker",
            "task_dispatched",
            json!({"message":"member-private-message","marker":"MEMBER_PRIVATE_EVENT_SENTINEL"}),
            Some(&job),
            Some(&task),
        )
        .unwrap();
    MemberWork {
        fx,
        owner: rt.alias,
        job,
        task,
    }
}

#[test]
fn member_owned_work_is_hidden_from_unrelated_agent_on_shared_worker() {
    let _flag = AssistantFlagOn::arm();
    let work = member_work(Fx::new());
    let fx = &work.fx;
    let unrelated = || Asserted::Agent("unrelated".to_string());
    let assignee = || Asserted::Agent("ordinary-worker".to_string());
    let forbidden = [
        work.task.as_str(),
        "member-other-task",
        "member-private-message",
        "MEMBER_PRIVATE_BODY_SENTINEL",
        "MEMBER_PRIVATE_EVENT_SENTINEL",
    ];
    for (method, params) in [
        ("agent_list", json!({})),
        ("agent_show", json!({"alias":"ordinary-worker"})),
        ("message_list", json!({"to":"ordinary-worker"})),
        (
            "thread_read",
            json!({"alias":"ordinary-worker", "cursor_format":"opaque-v1"}),
        ),
        (
            "thread_read",
            json!({"alias":"ordinary-worker","tail":true, "cursor_format":"opaque-v1"}),
        ),
        ("agent_events", json!({"alias":"ordinary-worker"})),
        (
            "agent_events",
            json!({"alias":"ordinary-worker","tail":true}),
        ),
    ] {
        if let Ok(value) = fx.call(unrelated(), method, params) {
            let text = value.to_string();
            for secret in forbidden {
                assert!(
                    !text.contains(secret),
                    "{method} disclosed {secret}: {value}"
                );
            }
        }
    }
    let public_show = fx
        .call(
            unrelated(),
            "agent_show",
            json!({"alias":"ordinary-worker"}),
        )
        .expect("public worker projection");
    assert!(
        public_show.to_string().contains("PUBLIC_CONTROL_BODY"),
        "public control missing: {public_show}"
    );
    let public_thread = fx
        .call(
            unrelated(),
            "thread_read",
            json!({"alias":"ordinary-worker", "cursor_format":"opaque-v1"}),
        )
        .expect("public worker thread projection");
    assert_eq!(
        public_thread["_reader_scope"],
        cadence_core::board_api::THREAD_OPAQUE_SCOPE_ACK
    );
    assert!(
        public_thread.get("reply_seq").is_none() && public_thread.get("scanned_before").is_none()
    );
    for row in public_thread["entries"].as_array().expect("thread entries") {
        assert!(row.get("seq").is_none(), "global thread seq leaked: {row}");
    }
    let public_thread_text = public_thread.to_string();
    assert!(
        public_thread_text.contains("PUBLIC_CONTROL_BODY"),
        "public thread control missing: {public_thread}"
    );
    for secret in forbidden {
        assert!(
            !public_thread_text.contains(secret),
            "public thread disclosed {secret}: {public_thread}"
        );
    }
    assert!(fx
        .call(
            unrelated(),
            "message_read",
            json!({"message":"member-private-message"})
        )
        .is_err());
    assert!(fx.call(unrelated(), "agent_wait", json!({"alias":"ordinary-worker","until":"reported","message":"member-private-message","timeout":0})).is_err());

    let owner_job = fx
        .as_assistant(&work.owner, "job_show", json!({"job":work.job}))
        .expect("owner assistant's bound job");
    assert!(owner_job.to_string().contains(&work.task));
    let own_task = fx
        .call(assignee(), "task_show", json!({"task":work.task}))
        .expect("specific assigned task remains readable");
    assert!(own_task.to_string().contains(&work.task));
    assert!(
        fx.call(assignee(), "job_show", json!({"job":work.job}))
            .is_err(),
        "task assignee must not get whole job"
    );
    assert!(
        fx.call(assignee(), "task_show", json!({"task":"member-other-task"}))
            .is_err(),
        "task assignee must not get another task"
    );
    let operator = fx
        .call(
            Asserted::Operator,
            "agent_show",
            json!({"alias":"ordinary-worker"}),
        )
        .expect("operator recovery");
    assert!(operator
        .to_string()
        .contains("MEMBER_PRIVATE_BODY_SENTINEL"));
    assert!(operator.to_string().contains(&work.task));
}

#[test]
fn member_owned_unnamed_wait_stays_pending_until_readable_completion() {
    let _flag = AssistantFlagOn::arm();
    let (baseline_tx, baseline_rx) = std::sync::mpsc::channel::<()>();
    let (baseline_release_tx, baseline_release_rx) = std::sync::mpsc::channel::<()>();
    let baseline_release_rx = std::sync::Mutex::new(baseline_release_rx);
    let (scan_tx, scan_rx) = std::sync::mpsc::channel::<()>();
    let (scan_release_tx, scan_release_rx) = std::sync::mpsc::channel::<()>();
    let scan_release_rx = std::sync::Mutex::new(scan_release_rx);
    let baseline: WaitBaselineObserver = Arc::new(move |_, _| {
        baseline_tx.send(()).unwrap();
        baseline_release_rx.lock().unwrap().recv().unwrap();
    });
    let scan: WaitBaselineObserver = Arc::new(move |_, _| {
        scan_tx.send(()).unwrap();
        scan_release_rx.lock().unwrap().recv().unwrap();
    });
    let work = member_work(Fx::with_wait_observers(Some(baseline), Some(scan)));
    let conn = rusqlite::Connection::open(work.fx.dir.path().join("cadence.sqlite3")).unwrap();
    let shared = Arc::clone(&work.fx.shared);
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let result = scoped(Asserted::Agent("unrelated".to_string()), || {
            shared.dispatch(
                "agent_wait",
                &json!({"alias":"ordinary-worker","until":"reported","timeout":10}),
                std::process::id(),
            )
        });
        result_tx.send(result).unwrap();
    });
    baseline_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("baseline witness");
    conn.execute(
        "UPDATE messages SET state='completed',completed=42 WHERE id='member-private-message'",
        [],
    )
    .unwrap();
    work.fx
        .shared
        .store
        .event_public_scoped(
            "ordinary-worker",
            "turn_finished",
            json!({"message":"member-private-message"}),
            Some(&work.job),
            Some(&work.task),
        )
        .unwrap();
    baseline_release_tx.send(()).unwrap();
    scan_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("private-only scan must not hit");
    assert!(
        matches!(
            result_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ),
        "private completion returned a result"
    );
    conn.execute(
        "UPDATE messages SET state='completed',completed=42 WHERE id='public-control-message'",
        [],
    )
    .unwrap();
    work.fx
        .shared
        .store
        .event_public_scoped(
            "ordinary-worker",
            "turn_finished",
            json!({"message":"public-control-message"}),
            Some("public-control-job"),
            Some("public-control-task"),
        )
        .unwrap();
    scan_release_tx.send(()).unwrap();
    work.fx.shared.wake();
    let result = result_rx
        .recv_timeout(Duration::from_secs(12))
        .unwrap()
        .expect("public completion");
    waiter.join().unwrap();
    assert_eq!(result["message"], "public-control-message");
    assert_eq!(result["reason"], "reported");
    assert!(!result.to_string().contains("member-private-message"));
    assert!(!result.to_string().contains(&work.task));
}

#[test]
fn signed_member_context_retry_digest_respects_private_job_on_same_issue() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let _runtime = fx.member_runtime("sub-alice");
    let repo = fx.dir.path().join("context-repo");
    std::fs::create_dir_all(repo.join("docs/cadence")).unwrap();
    std::fs::write(
        repo.join("docs/cadence/project-context.yaml"),
        "schema: 1\nproject: context\nagent_delivery: true\ndocuments: []\n",
    )
    .unwrap();
    for args in [
        &["init", "--quiet", "--initial-branch=main"][..],
        &["add", "."][..],
        &[
            "-c",
            "user.name=CAD-2215 fixture",
            "-c",
            "user.email=cad2215@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "context fixture",
        ][..],
    ] {
        let output = crate::reaper::output(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "fixture git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let pm = crate::issue::Pm::at(&fx.pm()).unwrap();
    crate::issue::write::project_add(
        &pm,
        "context",
        "CTX",
        &[repo.display().to_string()],
        &[],
        &[],
        None,
    )
    .unwrap();
    crate::issue::write::new_issue(
        &pm,
        &repo,
        Some("context"),
        "Context retry issue",
        Some("P2"),
        None,
        &[],
        None,
        None,
        &[],
        Some("CTX-1"),
        Some("Complete the fixture work.\n\n## Acceptance\n\n- [ ] Preserve the retry contract.\n"),
        "cad2215-fixture",
    )
    .unwrap();
    let start = crate::issue::start::StartArgs {
        repo: Some(repo.clone()),
        name: None,
        base: Some("main".to_string()),
        owner: None,
        job: None,
        by: Some(_runtime.alias.clone()),
        take_over: None,
    };
    crate::issue::start::run(&pm, "CTX-1", &start, "operator", fx.dir.path()).unwrap();
    let sent = fx.call(Asserted::Operator, "dispatch_send", json!({
        "alias":"ordinary-worker", "issue":"CTX-1", "text":"perform context fixture",
        "reply_to":_runtime.alias.clone(), "message":"cad2215-real-context-pin", "no_lessons":true
    })).expect("native issue dispatch must pin context");
    assert_eq!(sent["message"], "cad2215-real-context-pin");
    let pin = fx
        .shared
        .store
        .context_pin("ordinary-worker", "cad2215-real-context-pin")
        .unwrap()
        .expect("native dispatch produced no context pin");
    assert_eq!(pin["task"], "CTX-1");
    assert_eq!(pin["issue"], "CTX-1");
    let manifest: crate::context_pack::Manifest =
        serde_json::from_value(pin["manifest"].clone()).unwrap();
    let expected = manifest.task.semantic_digest();
    assert!(!expected.is_empty());
    let source_message = fx
        .shared
        .store
        .message("cad2215-real-context-pin")
        .unwrap()
        .unwrap();
    assert_eq!(source_message.issue.as_deref(), Some("CTX-1"));
    assert!(source_message.task_id.is_none());

    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope":fx.member_envelope("sub-alice"), "title":"member context job",
            "request_key":"cad2215-context-owned", "body":"private member material",
            "issue":"CTX-1",
        }),
    );
    let job = promoted_member_job(&fx, "sub-alice", &request);
    assert!(fx
        .shared
        .store
        .assist_request_owns(ORG, "sub-alice", &job)
        .unwrap());
    assert_eq!(
        fx.shared.store.job(&job).unwrap().issue_id.as_deref(),
        Some("CTX-1")
    );

    let cli_envelope = |sub: &str| {
        let now = crate::issue::time::now_epoch();
        let header = json!({"alg":"EdDSA", "typ":"agenticos-wiki-actor/1", "kid":KID});
        let claims = json!({
            "iss":fx.issuer, "aud":format!("http://{}",fx.host),
            "sub":sub, "actor":format!("users/{sub}"), "principal_kind":"user",
            "role_at_issue":"member", "organization_id":ORG, "scope":["cli.read"],
            "credential_id":format!("cred-{sub}"), "iat":now, "exp":now+120,
            "jti":format!("cad2215-context-{sub}-{}", uuid::Uuid::new_v4())
        });
        let signed = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        );
        format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(fx.key.sign(signed.as_bytes()).as_ref())
        )
    };
    let show = |envelope: Option<String>| {
        let mut params = json!({"alias":"ordinary-worker", "context_message":"cad2215-real-context-pin", "limit":0});
        if let Some(envelope) = envelope {
            params["cli_envelope"] = json!(envelope);
        }
        fx.call(Asserted::Operator, "agent_show", params)
            .expect("verified context retry read")
    };
    let stranger = show(Some(cli_envelope("sub-bob")));
    assert!(
        stranger["context_contract_digest"].is_null(),
        "non-owner learned ambiguous issue-only digest: {stranger}"
    );
    let owner = show(Some(cli_envelope("sub-alice")));
    assert_eq!(
        owner["context_contract_digest"].as_str(),
        Some(expected.as_str()),
        "member owner lost real pin: {owner}"
    );
    let operator = show(None);
    assert_eq!(
        operator["context_contract_digest"].as_str(),
        Some(expected.as_str()),
        "operator lost real pin: {operator}"
    );
}

#[test]
fn monitor_member_job_coverage_respects_private_task_authority() {
    let _flag = AssistantFlagOn::arm();
    let work = member_work(Fx::new());
    let fx = &work.fx;
    let db = rusqlite::Connection::open(fx.dir.path().join("cadence.sqlite3")).unwrap();
    // Synthetic repo and assignment bindings on the real signed member job.
    db.execute(
        "UPDATE jobs SET repo='monitor-fixture' WHERE id=?1",
        [&work.job],
    )
    .unwrap();
    db.execute(
        "UPDATE jobs SET repo='monitor-fixture' WHERE id='public-control-job'",
        [],
    )
    .unwrap();
    // Give the unrelated agent nominal ownership of both monitors so that
    // heartbeat denial proves the coverage boundary, not merely ownership.
    fx.shared
        .store
        .register_monitor(
            "private-member-monitor",
            "monitor-fixture",
            "unrelated",
            60,
            std::slice::from_ref(&work.task),
            false,
            false,
        )
        .unwrap();
    fx.shared
        .store
        .register_monitor(
            "public-control-monitor",
            "monitor-fixture",
            "unrelated",
            60,
            &["public-control-task".to_string()],
            false,
            false,
        )
        .unwrap();
    db.execute(
        "INSERT INTO monitor_alerts(seq,monitor_id,task_id,event_seq,fingerprint,kind,payload,state,created,updated) VALUES(301,'private-member-monitor',?1,1,'private-member-alert','test','{\"marker\":\"MEMBER_MONITOR_SECRET\"}','open',1,1)",
        [&work.task],
    ).unwrap();
    db.execute(
        "INSERT INTO monitor_alerts(seq,monitor_id,task_id,event_seq,fingerprint,kind,payload,state,created,updated) VALUES(302,'public-control-monitor','public-control-task',2,'public-control-alert','test','{\"marker\":\"PUBLIC_MONITOR_CONTROL\"}','open',1,1)",
        [],
    ).unwrap();
    let unrelated = || Asserted::Agent("unrelated".to_string());
    let assignee = || Asserted::Agent("ordinary-worker".to_string());
    let listing = fx
        .call(unrelated(), "monitor_list", json!({}))
        .expect("public monitor list");
    let text = listing.to_string();
    assert!(
        text.contains("public-control-monitor"),
        "public monitor missing: {listing}"
    );
    for hidden in [
        "private-member-monitor",
        work.task.as_str(),
        "MEMBER_MONITOR_SECRET",
    ] {
        assert!(
            !text.contains(hidden),
            "monitor listing exposed {hidden}: {listing}"
        );
    }
    for (method, params) in [
        ("monitor_show", json!({"monitor":"private-member-monitor"})),
        (
            "monitor_alerts",
            json!({"monitor":"private-member-monitor"}),
        ),
        (
            "monitor_heartbeat",
            json!({"monitor":"private-member-monitor"}),
        ),
        (
            "monitor_alert_ack",
            json!({"monitor":"private-member-monitor","alert":301}),
        ),
        (
            "monitor_register",
            json!({"monitor":"forbidden-member-monitor","project":"monitor-fixture","tasks":[work.task]}),
        ),
    ] {
        assert!(
            fx.call(unrelated(), method, params).is_err(),
            "unrelated agent unexpectedly admitted to {method}"
        );
    }
    assert!(
        fx.shared.store.monitor("forbidden-member-monitor").is_err(),
        "refused registration mutated the store"
    );
    let alert_state: String = db
        .query_row(
            "SELECT state FROM monitor_alerts WHERE seq=301",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        alert_state, "open",
        "refused acknowledgement mutated the alert"
    );
    let heartbeat: Option<f64> = db
        .query_row(
            "SELECT heartbeat_at FROM monitors WHERE id='private-member-monitor'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        heartbeat.is_none(),
        "refused heartbeat changed monitor state"
    );
    let public_alerts = fx
        .call(
            unrelated(),
            "monitor_alerts",
            json!({"monitor":"public-control-monitor","cursor_format":"opaque-v1"}),
        )
        .expect("unrelated reader's public positive control");
    assert!(public_alerts.to_string().contains("PUBLIC_MONITOR_CONTROL"));
    fx.call(
        unrelated(),
        "monitor_heartbeat",
        json!({"monitor":"public-control-monitor"}),
    )
    .expect("public owner heartbeat");

    let assigned = fx
        .call(
            assignee(),
            "monitor_alerts",
            json!({"monitor":"private-member-monitor","cursor_format":"opaque-v1"}),
        )
        .expect("exact task assignee can inspect its task's monitor alerts");
    assert!(assigned.to_string().contains("MEMBER_MONITOR_SECRET"));
    let ack_token = assigned["alerts"]
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row["ack"].as_str())
        .expect("a scoped alert page carries its acknowledgement token")
        .to_string();
    let owner = fx
        .as_assistant(&work.owner, "task_show", json!({"task":work.task}))
        .expect("member owner retains its existing bound-task surface");
    assert!(owner.to_string().contains(&work.task));
    let operator = fx
        .call(
            Asserted::Operator,
            "monitor_alerts",
            json!({"monitor":"private-member-monitor"}),
        )
        .expect("operator monitor recovery");
    assert!(operator.to_string().contains("MEMBER_MONITOR_SECRET"));
    let acknowledged = fx
        .call(
            assignee(),
            "monitor_alert_ack",
            json!({"monitor":"private-member-monitor","ack":ack_token}),
        )
        .expect("exact task assignee can acknowledge its task alert");
    assert_eq!(acknowledged["alert"]["state"], "acknowledged");
}

#[test]
fn member_private_direct_targets_are_indistinguishable_from_absent() {
    let _flag = AssistantFlagOn::arm();
    let work = member_work(Fx::new());
    let fx = &work.fx;
    fx.shared
        .store
        .enqueue_task(
            &work.owner,
            "OWNER_PRIVATE_MAIL_SENTINEL",
            None,
            "owner-private-mail",
            "user",
            Some(&work.task),
        )
        .unwrap();
    let unrelated = || Asserted::Agent("unrelated".to_string());
    let probes = [
        ("job_show", "job", work.job.as_str(), "absent-member-job"),
        (
            "task_show",
            "task",
            work.task.as_str(),
            "absent-member-task",
        ),
        (
            "agent_show",
            "alias",
            work.owner.as_str(),
            "absent-member-agent",
        ),
        (
            "agent_events",
            "alias",
            work.owner.as_str(),
            "absent-member-agent",
        ),
        (
            "thread_read",
            "alias",
            work.owner.as_str(),
            "absent-member-agent",
        ),
        (
            "agent_wait",
            "alias",
            work.owner.as_str(),
            "absent-member-agent",
        ),
        (
            "message_read",
            "message",
            "owner-private-mail",
            "absent-member-mail",
        ),
    ];
    for (method, field, hidden, missing) in probes {
        let params = |id: &str| {
            let mut value = json!({});
            value[field] = json!(id);
            if method == "thread_read" {
                value["cursor_format"] = json!("opaque-v1");
            }
            if method == "agent_wait" {
                value["until"] = json!("reported");
                value["timeout"] = json!(0);
            }
            value
        };
        let missing_error = fx
            .call(unrelated(), method, params(missing))
            .expect_err("absent target must be refused");
        let hidden_error = fx
            .call(unrelated(), method, params(hidden))
            .expect_err("private target must be refused");
        let expected = format!("{missing_error:?}").replace(missing, "<requested>");
        let actual = format!("{hidden_error:?}").replace(hidden, "<requested>");
        assert_eq!(
            actual, expected,
            "{method} disclosed private-target existence or caller-sensitive detail"
        );
        assert!(
            !actual.contains("unrelated"),
            "{method} disclosed caller-sensitive identity: {actual}"
        );
    }

    let owner_job = fx
        .as_assistant(&work.owner, "job_show", json!({"job":work.job}))
        .expect("owner retains its real member job");
    assert_eq!(owner_job["job"]["id"], work.job);
    let owner_message = fx
        .as_assistant(
            &work.owner,
            "message_read",
            json!({"message":"owner-private-mail"}),
        )
        .expect("owner retains its own private message");
    assert!(owner_message
        .to_string()
        .contains("OWNER_PRIVATE_MAIL_SENTINEL"));
    for (method, params) in [
        ("job_show", json!({"job":work.job})),
        ("task_show", json!({"task":work.task})),
        ("agent_show", json!({"alias":work.owner})),
        ("agent_events", json!({"alias":work.owner})),
        ("thread_read", json!({"alias":work.owner})),
        ("message_read", json!({"message":"owner-private-mail"})),
    ] {
        fx.call(Asserted::Operator, method, params)
            .expect("operator recovery must remain available");
    }
}

#[test]
fn promotion_recovery_reproves_original_credential_before_any_effect() {
    let _flag = AssistantFlagOn::arm();
    for ledger_already_bound in [false, true] {
        let fx = Fx::new();
        with_deferred_service(&fx);
        let sub = "sub-crash-recovery";
        let key = if ledger_already_bound {
            "bound-crash"
        } else {
            "partial-crash"
        };
        let title = "recover the actual request";
        let body = "RECOVERY_PRIVATE_BODY";
        let acceptance = "recovery acceptance";
        fx.member_runtime(sub);
        let receipt = fx.op(
            "member_job_request",
            json!({
                "member_envelope":fx.member_envelope(sub), "title":title, "body":body,
                "acceptance":acceptance, "request_key":key, "issue":FIXTURE_ISSUE
            }),
        );
        let request_id = receipt["request_id"]
            .as_str()
            .expect("receipt ID")
            .to_string();
        assert_eq!(
            receipt,
            json!({"request_id":request_id,"status":"accepted"})
        );

        // Recreate the exact durable state a crashed real publication leaves:
        // the canonical job and its exact spec, before subscribe/notice. In
        // the second branch the ownership ledger also landed before crash.
        let canonical = format!(
            "{}:{}|{}:{}|{}:{}",
            ORG.len(),
            ORG,
            sub.len(),
            sub,
            key.len(),
            key
        );
        let digest = crate::operator_auth::digest(&canonical);
        let job = format!("assist-{}", &digest[..24]);
        let spec_dir = fx.shared.state_dir.join("assist_jobs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        let spec_path = spec_dir.join(format!("{job}.md"));
        let spec = expected_spec(title, body, acceptance);
        std::fs::write(&spec_path, spec.as_bytes()).unwrap();
        let spec_str = spec_path.to_str().unwrap();
        let alias = format!("assist-agent-{sub}");
        fx.shared
            .store
            .create_job(
                &job,
                Some(title),
                spec_str,
                &sha_hex(spec.as_bytes()),
                &alias,
                Some(FIXTURE_ISSUE),
                None,
                None,
                2,
                None,
                Some(title),
                None,
                None,
                None,
                None,
                Some(acceptance),
            )
            .unwrap();
        if ledger_already_bound {
            assert_eq!(
                fx.shared
                    .store
                    .assist_request_bind(
                        ORG,
                        sub,
                        key,
                        &job,
                        title,
                        body,
                        acceptance,
                        &format!("cred-{sub}")
                    )
                    .unwrap(),
                job
            );
        }
        let before_ledger = fx.shared.store.assist_request_job(ORG, sub, key).unwrap();
        assert_eq!(
            before_ledger.as_deref(),
            ledger_already_bound.then_some(job.as_str())
        );
        assert!(fx.shared.store.job_subscribers(&job).unwrap().is_empty());
        let notices_before = fx
            .shared
            .store
            .pending_assist_job_notifications(100)
            .unwrap();
        assert!(!notices_before.iter().any(|(_, p)| p["job"] == job));
        let jobs_before = fx.shared.store.jobs(None, true).unwrap();
        let job_before = fx.shared.store.job(&job).unwrap();
        let tasks_before = format!("{:?}", fx.shared.store.tasks_for_job(&job).unwrap());
        let events_before = format!("{:?}", fx.shared.store.job_events(&job, 0, 100).unwrap());
        let files = || {
            let mut out: Vec<_> = std::fs::read_dir(&spec_dir)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            out.sort();
            out
        };
        let files_before = files();
        let tree_before = fx.tree();

        // Revoke precisely the originally recorded issuing credential. The
        // signed enqueue, live service bearer and binding remain real.
        fx.set_member(sub, |m| m.credential_live = false);
        let refused = fx.shared.sweep_member_job_requests_for_test().unwrap();
        assert!(refused.iter().any(|o| o.request_id == request_id && o.outcome == "refused"),
            "recovery did not refuse revoked original credential (bound={ledger_already_bound}): {refused:?}");
        assert_eq!(
            fx.shared.store.assist_request_job(ORG, sub, key).unwrap(),
            before_ledger,
            "revoked recovery bound or retargeted ownership ledger"
        );
        assert!(
            fx.shared.store.job_subscribers(&job).unwrap().is_empty(),
            "revoked recovery subscribed owner"
        );
        assert_eq!(
            fx.shared
                .store
                .pending_assist_job_notifications(100)
                .unwrap(),
            notices_before,
            "revoked recovery emitted a notification"
        );
        assert_eq!(
            fx.shared.store.jobs(None, true).unwrap(),
            jobs_before,
            "revoked recovery changed jobs"
        );
        assert_eq!(
            fx.shared.store.job(&job).unwrap(),
            job_before,
            "revoked recovery mutated partial job"
        );
        assert_eq!(
            format!("{:?}", fx.shared.store.tasks_for_job(&job).unwrap()),
            tasks_before
        );
        assert_eq!(
            format!("{:?}", fx.shared.store.job_events(&job, 0, 100).unwrap()),
            events_before
        );
        assert_eq!(
            std::fs::read(&spec_path).unwrap(),
            spec.as_bytes(),
            "revoked recovery changed spec"
        );
        assert_eq!(
            files(),
            files_before,
            "revoked recovery created an extra artifact"
        );
        assert_eq!(
            fx.tree(),
            tree_before,
            "revoked recovery changed tracker files"
        );

        // Restore the SAME original credential, then the same real sweep may
        // converge the existing artifact exactly once.
        fx.set_member(sub, |m| m.credential_live = true);
        let recovered = fx.shared.sweep_member_job_requests_for_test().unwrap();
        assert!(recovered.iter().any(|o| o.request_id == request_id && o.outcome == "created"),
            "live original credential could not recover (bound={ledger_already_bound}): {recovered:?}");
        assert_eq!(
            fx.shared
                .store
                .assist_request_job(ORG, sub, key)
                .unwrap()
                .as_deref(),
            Some(job.as_str())
        );
        assert_eq!(fx.shared.store.job_subscribers(&job).unwrap().len(), 1);
        let intents_before_replay: Vec<_> = fx
            .shared
            .store
            .pending_assist_job_notifications(100)
            .unwrap()
            .into_iter()
            .filter(|(_, p)| p["job"] == job)
            .collect();
        assert_eq!(
            intents_before_replay.len(),
            2,
            "recovery must emit exactly two target intents"
        );
        let masters: Vec<_> = intents_before_replay
            .iter()
            .filter(|(_, p)| p["target"] == "master")
            .collect();
        let subscribers: Vec<_> = intents_before_replay
            .iter()
            .filter(|(_, p)| p["target"] == "subscriber")
            .collect();
        assert_eq!(
            masters.len(),
            1,
            "recovery must emit exactly one master intent"
        );
        assert_eq!(
            subscribers.len(),
            1,
            "recovery must emit exactly one owner-subscriber intent"
        );
        assert_eq!(masters[0].1["alias"], crate::master::ALIAS);
        assert_eq!(subscribers[0].1["org"], ORG);
        assert_eq!(subscribers[0].1["sub"], sub);
        assert_eq!(subscribers[0].1["assistant_id"], format!("assist_{sub}"));
        assert_ne!(
            masters[0].0, subscribers[0].0,
            "target intents reused an event sequence"
        );
        let master_key = masters[0].1["key"]
            .as_str()
            .filter(|s| !s.is_empty())
            .expect("master intent key missing");
        let subscriber_key = subscribers[0].1["key"]
            .as_str()
            .filter(|s| !s.is_empty())
            .expect("subscriber intent key missing");
        assert_ne!(
            master_key, subscriber_key,
            "target intents reused a deduplication key"
        );
        assert!(intents_before_replay
            .iter()
            .all(|(_, p)| p["status"] == "pending"
                && p["state"] == "awaiting_approval"
                && p["dedupe"] == "request"));
        assert_eq!(
            fx.shared.store.jobs(None, true).unwrap().len(),
            jobs_before.len()
        );
        assert_eq!(std::fs::read(&spec_path).unwrap(), spec.as_bytes());
        assert_eq!(files(), files_before);
        let list = fx.op(
            "member_job_list",
            json!({"member_envelope":fx.member_envelope(sub)}),
        );
        let row = list["requests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["request_id"] == request_id)
            .unwrap();
        assert_eq!(row["status"], "created");
        assert_eq!(row["job"], job);
        fx.shared.sweep_member_job_requests_for_test().unwrap();
        assert_eq!(
            fx.shared.store.job_subscribers(&job).unwrap().len(),
            1,
            "recovery duplicated subscription"
        );
        let intents_after_replay: Vec<_> = fx
            .shared
            .store
            .pending_assist_job_notifications(100)
            .unwrap()
            .into_iter()
            .filter(|(_, p)| p["job"] == job)
            .collect();
        assert_eq!(
            intents_after_replay, intents_before_replay,
            "recovery replay changed or duplicated target intents"
        );
    }
}
