use super::*;

#[test]
fn cloud_hold_notifies_then_a_later_poll_recovers_the_sha() {
    use std::sync::atomic::AtomicUsize;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    let gets = Arc::new(AtomicUsize::new(0));
    let posts = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicBool::new(false));
    let early_post = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = server.server_addr().to_ip().unwrap();
    let gets_t = Arc::clone(&gets);
    let posts_t = Arc::clone(&posts);
    let finished_t = Arc::clone(&finished);
    let early_t = Arc::clone(&early_post);
    let stop_t = Arc::clone(&stop);
    let thread = thread::spawn(move || {
        while !stop_t.load(Ordering::SeqCst) {
            let mut req = match server.recv_timeout(Duration::from_millis(50)) {
                Ok(Some(req)) => req,
                _ => continue,
            };
            let path = req.url().to_string();
            let method = req.method().as_str().to_string();
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let (status, payload): (u16, String) = if path.contains("/repositories") {
                (
                    200,
                    json!({"repositories": [{"name": "cadence", "owner": "favcrm"}]}).to_string(),
                )
            } else if method == "POST" && path.ends_with("/sessions") {
                (
                    200,
                    json!({
                        "session_id": "devin-created",
                        "status": "running",
                        "status_detail": "working",
                        "url": "https://app.devin.ai/sessions/devin-created"
                    })
                    .to_string(),
                )
            } else if method == "POST" && path.contains("/messages") {
                let n = posts_t.fetch_add(1, Ordering::SeqCst);
                if n >= 1 && !finished_t.load(Ordering::SeqCst) {
                    early_t.store(true, Ordering::SeqCst);
                }
                (200, json!({"ok": true}).to_string())
            } else if method == "GET" && path.contains("/messages") {
                let items = if posts_t.load(Ordering::SeqCst) == 0 {
                    json!([])
                } else if posts_t.load(Ordering::SeqCst) >= 2 {
                    json!([
                        {"event_id": "evt-1", "source": "devin", "message": format!("done\nSHA: {SHA}"), "created_at": 1},
                        {"event_id": "evt-2", "source": "devin", "message": "follow-up done", "created_at": 2}
                    ])
                } else {
                    json!([{"event_id": "evt-1", "source": "devin", "message": format!("done\nSHA: {SHA}"), "created_at": 1}])
                };
                let total = items.as_array().map(|rows| rows.len()).unwrap_or(0);
                (
                        200,
                        json!({"items": items, "has_next_page": false, "end_cursor": null, "total": total})
                            .to_string(),
                    )
            } else if method == "GET" && path.contains("/sessions/") {
                if posts_t.load(Ordering::SeqCst) == 0 {
                    (
                        200,
                        json!({
                            "session_id": "devin-created",
                            "status": "running",
                            "status_detail": "working",
                            "url": "https://app.devin.ai/sessions/devin-created",
                            "pull_requests": [],
                        })
                        .to_string(),
                    )
                } else {
                    let n = gets_t.fetch_add(1, Ordering::SeqCst);
                    if n < 3 {
                        (500, json!({"error": "transient"}).to_string())
                    } else {
                        finished_t.store(true, Ordering::SeqCst);
                        (
                            200,
                            json!({
                                "session_id": "devin-created",
                                "status": "running",
                                "status_detail": "finished",
                                "url": "https://app.devin.ai/sessions/devin-created",
                                "pull_requests": [],
                            })
                            .to_string(),
                        )
                    }
                }
            } else {
                (200, json!({"ok": true}).to_string())
            };
            let _ = req.respond(tiny_http::Response::from_string(payload).with_status_code(status));
        }
    });
    let (dir, shared) = shared();
    let base = format!("http://{addr}");
    shared.provider_env.set("CADENCE_DEVIN_API_BASE", &base);
    shared
        .provider_env
        .set("CADENCE_DEVIN_API_KEY", "cog_test_secret_value");
    shared.provider_env.set("CADENCE_DEVIN_ORG_ID", "org-test");
    shared
        .provider_env
        .set("CADENCE_DEVIN_POLL_INTERVAL_MS", "20");
    shared
        .provider_env
        .set("CADENCE_DEVIN_POLL_BUDGET_MS", "120");
    let cwd = dir.path().to_str().unwrap();
    let spec = dir.path().join("spec.md");
    std::fs::write(&spec, "Do the cloud work.").unwrap();
    shared
        .store
        .register_agent(&NewAgent {
            alias: "pm",
            provider: "fake",
            endpoint_kind: "managed",
            role: "pm",
            cwd,
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let params = r#"{"repos":["favcrm/cadence"],"upstream":"pm"}"#;
    shared
        .store
        .register_agent(&NewAgent {
            alias: "cloud-1",
            provider: "devin",
            endpoint_kind: "cloud",
            role: "worker",
            cwd,
            sandbox: "read-only",
            instructions: None,
            params: Some(params),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    shared
        .store
        .create_job(
            "j1",
            None,
            spec.to_str().unwrap(),
            &"0".repeat(64),
            "pm",
            None,
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
    shared
        .store
        .create_task(
            "j1",
            "t1",
            None,
            Some("cloud-1"),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    let (_task, kickoff, dup, _) = shared
        .store
        .dispatch_task("t1", None, None, "test")
        .unwrap();
    assert!(!dup);
    shared
        .store
        .enqueue(
            "cloud-1",
            "follow-up while the session works",
            Some("pm"),
            "follow-up",
            "user",
        )
        .unwrap();
    shared.launch_actor("cloud-1").unwrap();
    let started = Instant::now();
    loop {
        if started.elapsed() > Duration::from_secs(8) {
            panic!(
                "held poll did not recover: task={:?} kickoff={:?} events={:?}",
                shared.store.task("t1").ok(),
                shared.store.message(&kickoff).ok(),
                shared.store.events("cloud-1", 0, 40).ok()
            );
        }
        if shared.store.task("t1").unwrap().head_sha.as_deref() == Some(SHA) {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !early_post.load(Ordering::SeqCst),
        "posted the next message while the session was still held"
    );
    assert!(shared
        .store
        .events("cloud-1", 0, 40)
        .unwrap()
        .iter()
        .any(|event| event.kind == "cloud_hold"));
    assert!(shared
        .store
        .messages("pm")
        .unwrap()
        .iter()
        .any(|message| { message.body.contains("not fenced") && message.body.contains("held") }));
    let follow_started = Instant::now();
    loop {
        if follow_started.elapsed() > Duration::from_secs(8) {
            panic!(
                "follow-up stayed {:?}",
                shared.store.message("follow-up").unwrap()
            );
        }
        let state = shared.store.message("follow-up").unwrap().unwrap().state;
        if state == "completed" || state == "failed" {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!early_post.load(Ordering::SeqCst));
    shared.store.set_enabled("cloud-1", false).unwrap();
    shared.begin_closing();
    let handle = {
        let lc = shared.lifecycle.lock().unwrap();
        lc.agents
            .get("cloud-1")
            .and_then(|ctl| ctl.thread.lock().unwrap().take())
    };
    if let Some(ctl) = shared.lifecycle.lock().unwrap().agents.get("cloud-1") {
        ctl.wake.notify_all();
    }
    if let Some(handle) = handle {
        let _ = handle.join();
    }
    stop.store(true, Ordering::SeqCst);
    let _ = thread.join();
}

#[test]
fn cloud_prewrite_fails_and_preserves_a_hosted_failure_report() {
    use std::sync::atomic::AtomicUsize;

    let posts = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = server.server_addr().to_ip().unwrap();
    let posts_t = Arc::clone(&posts);
    let stop_t = Arc::clone(&stop);
    let thread = thread::spawn(move || {
        while !stop_t.load(Ordering::SeqCst) {
            let mut req = match server.recv_timeout(Duration::from_millis(50)) {
                Ok(Some(req)) => req,
                _ => continue,
            };
            let path = req.url().to_string();
            let method = req.method().as_str().to_string();
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let (status, payload): (u16, String) = if path.contains("/repositories") {
                (
                    200,
                    json!({"repositories": [{"name": "cadence", "owner": "favcrm"}]}).to_string(),
                )
            } else if method == "POST" && path.ends_with("/sessions") {
                (
                    200,
                    json!({
                        "session_id": "devin-created",
                        "status": "running",
                        "status_detail": "working",
                        "url": "https://app.devin.ai/sessions/devin-created"
                    })
                    .to_string(),
                )
            } else if method == "POST" && path.contains("/messages") {
                posts_t.fetch_add(1, Ordering::SeqCst);
                (200, json!({"ok": true}).to_string())
            } else if method == "GET" && path.contains("/messages") {
                (429, json!({"error": "slow"}).to_string())
            } else if method == "GET" && path.contains("/sessions/") {
                (
                    200,
                    json!({
                        "session_id": "devin-created",
                        "status": "running",
                        "status_detail": "working",
                        "url": "https://app.devin.ai/sessions/devin-created",
                        "pull_requests": [],
                    })
                    .to_string(),
                )
            } else {
                (200, json!({"ok": true}).to_string())
            };
            let _ = req.respond(tiny_http::Response::from_string(payload).with_status_code(status));
        }
    });
    let (dir, shared) = shared();
    let base = format!("http://{addr}");
    shared.provider_env.set("CADENCE_DEVIN_API_BASE", &base);
    shared
        .provider_env
        .set("CADENCE_DEVIN_API_KEY", "cog_test_secret_value");
    shared.provider_env.set("CADENCE_DEVIN_ORG_ID", "org-test");
    shared
        .provider_env
        .set("CADENCE_DEVIN_POLL_INTERVAL_MS", "20");
    shared
        .provider_env
        .set("CADENCE_DEVIN_POLL_BUDGET_MS", "200");
    let cwd = dir.path().to_str().unwrap();
    shared
        .store
        .register_agent(&NewAgent {
            alias: "cloud-1",
            provider: "devin",
            endpoint_kind: "cloud",
            role: "worker",
            cwd,
            sandbox: "read-only",
            instructions: None,
            params: Some(r#"{"repos":["favcrm/cadence"]}"#),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    crate::local_assignment::reporting::store_kickoff_binding(
        &shared.state_dir,
        "cad1460-prewrite-report",
        "https://hosted-completion-regression.invalid",
        &crate::local_assignment::Kickoff {
            alias: "cloud-1",
            issue_id: "CAD1460",
            assignment_id: "hla_00000000000000000000000000000001",
            worktree: cwd,
            message_id: "prewrite-1",
            claim_id: "hlc_00000000000000000000000000000001",
            generation: 1,
            organization_id: "ws_hosted_fail_probe",
            audience: "https://hosted-completion-regression.invalid",
            subject_id: Some("user_member"),
            issuer: Some("https://hosted-completion-regression.invalid"),
        },
        "Synthetic terminal failure",
    )
    .unwrap();
    let report_path = shared
        .state_dir
        .join("hosted-assignments")
        .join("prewrite-1.bind");
    let report = || {
        std::fs::read(&report_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    };
    shared
        .store
        .enqueue("cloud-1", "do the task", None, "prewrite-1", "user")
        .unwrap();
    shared.launch_actor("cloud-1").unwrap();
    let started = Instant::now();
    loop {
        if started.elapsed() > Duration::from_secs(8) {
            panic!(
                "prewrite did not fail the message: {:?}",
                shared.store.message("prewrite-1").ok()
            );
        }
        if shared.store.message("prewrite-1").unwrap().unwrap().state == "failed" {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let message = shared.store.message("prewrite-1").unwrap().unwrap();
    assert_eq!(message.state, "failed");
    assert!(report().unwrap()["pending_head"].is_null());
    assert_eq!(posts.load(Ordering::SeqCst), 0);
    let events = shared.store.events("cloud-1", 0, 40).unwrap();
    assert!(events.iter().all(|event| event.kind != "cloud_hold"));
    assert!(events
        .iter()
        .all(|event| event.kind != "cloud_recover_escalated"));
    stop_cloud(&shared, &stop, thread);
}

#[test]
fn cloud_restart_during_a_hold_fences_for_reconcile() {
    let (base, calls, server) = cloud_hold_server();
    let (dir, shared) = shared();
    hold_cloud_turn(&shared, dir.path(), &base, "held-1");
    assert_eq!(calls.posts.load(Ordering::SeqCst), 1);
    // Shutdown: the actor detaches and the agent stays enabled.
    shared.begin_closing();
    join_actor(&shared, "cloud-1");
    drop(shared);

    let restarted = Shared::new(dir.path(), &ServeOptions::default()).unwrap();
    devin_env(&restarted, &base);
    relaunch_agents(&restarted).unwrap();
    thread::sleep(Duration::from_millis(300));

    let agent = restarted.store.agent("cloud-1").unwrap();
    assert_eq!(agent.state, "attention", "{:?}", agent.error);
    assert!(agent.enabled);
    assert!(
        !restarted.lifecycle.lock().unwrap().owned("cloud-1"),
        "an actor started for a fenced agent"
    );
    assert_eq!(
        restarted.store.message("held-1").unwrap().unwrap().state,
        "unknown"
    );
    assert!(restarted
        .store
        .events("cloud-1", 0, 80)
        .unwrap()
        .iter()
        .any(|event| event.kind == "relaunch_skipped"));
    assert_eq!(calls.posts.load(Ordering::SeqCst), 1, "restart posted");
    assert_eq!(calls.archives.load(Ordering::SeqCst), 0, "archived");
    assert_eq!(calls.deletes.load(Ordering::SeqCst), 0, "deleted");
    calls.stop.store(true, Ordering::SeqCst);
    let _ = server.join();
}

#[test]
fn cloud_stop_during_a_hold_does_not_archive_the_session() {
    let (base, calls, server) = cloud_hold_server();
    let (dir, shared) = shared();
    hold_cloud_turn(&shared, dir.path(), &base, "held-1");
    shared.rpc_stop(&json!({"alias": "cloud-1"})).unwrap();

    assert_eq!(calls.archives.load(Ordering::SeqCst), 0, "archived");
    assert_eq!(calls.deletes.load(Ordering::SeqCst), 0, "deleted");
    assert_eq!(calls.posts.load(Ordering::SeqCst), 1);
    let message = shared.store.message("held-1").unwrap().unwrap();
    assert_eq!(message.state, "unknown", "{:?}", message.result);
    assert_eq!(shared.store.agent("cloud-1").unwrap().state, "stopped");
    calls.stop.store(true, Ordering::SeqCst);
    let _ = server.join();
}

#[test]
fn cloud_reconcile_during_a_hold_releases_the_actor() {
    let (base, calls, server) = cloud_hold_server();
    let (dir, shared) = shared();
    hold_cloud_turn(&shared, dir.path(), &base, "held-1");
    shared
        .reconcile_message(&json!({"message": "held-1", "status": "interrupted"}))
        .unwrap();
    calls.healthy.store(true, Ordering::SeqCst);
    shared
        .store
        .enqueue("cloud-1", "next task", None, "next-1", "user")
        .unwrap();
    shared.notify_agent("cloud-1");
    let started = Instant::now();
    loop {
        let state = shared.store.message("next-1").unwrap().unwrap().state;
        if state == "completed" {
            break;
        }
        if started.elapsed() > Duration::from_secs(8) {
            panic!(
                "next message stayed {state}: {:?}",
                shared.store.events("cloud-1", 0, 80).ok()
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
    let held = shared.store.message("held-1").unwrap().unwrap();
    assert_eq!(held.state, "interrupted");
    assert_eq!(
        held.result.unwrap()["via"],
        json!("operator_reconcile"),
        "the actor overwrote the operator's reconcile"
    );
    assert_eq!(calls.posts.load(Ordering::SeqCst), 2);
    assert_ne!(shared.store.agent("cloud-1").unwrap().state, "attention");
    shared.store.set_enabled("cloud-1", false).unwrap();
    shared.begin_closing();
    join_actor(&shared, "cloud-1");
    calls.stop.store(true, Ordering::SeqCst);
    let _ = server.join();
}

#[test]
fn cloud_hold_recovery_backs_off_and_escalates_once() {
    use std::sync::atomic::AtomicUsize;

    let gets = Arc::new(AtomicUsize::new(0));
    let posts = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = server.server_addr().to_ip().unwrap();
    let gets_t = Arc::clone(&gets);
    let posts_t = Arc::clone(&posts);
    let stop_t = Arc::clone(&stop);
    let thread = thread::spawn(move || {
        while !stop_t.load(Ordering::SeqCst) {
            let mut req = match server.recv_timeout(Duration::from_millis(50)) {
                Ok(Some(req)) => req,
                _ => continue,
            };
            let path = req.url().to_string();
            let method = req.method().as_str().to_string();
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let (status, payload): (u16, String) = if path.contains("/repositories") {
                (
                    200,
                    json!({"repositories": [{"name": "cadence", "owner": "favcrm"}]}).to_string(),
                )
            } else if method == "POST" && path.ends_with("/sessions") {
                (
                    200,
                    json!({
                        "session_id": "devin-created",
                        "status": "running",
                        "status_detail": "working",
                        "url": "https://app.devin.ai/sessions/devin-created"
                    })
                    .to_string(),
                )
            } else if method == "POST" && path.contains("/messages") {
                posts_t.fetch_add(1, Ordering::SeqCst);
                (200, json!({"ok": true}).to_string())
            } else if method == "GET" && path.contains("/messages") {
                gets_t.fetch_add(1, Ordering::SeqCst);
                if posts_t.load(Ordering::SeqCst) == 0 {
                    (
                            200,
                            json!({"items": [], "has_next_page": false, "end_cursor": null, "total": 0})
                                .to_string(),
                        )
                } else {
                    (429, json!({"error": "slow down"}).to_string())
                }
            } else if method == "GET" && path.contains("/sessions/") {
                gets_t.fetch_add(1, Ordering::SeqCst);
                if posts_t.load(Ordering::SeqCst) == 0 {
                    (
                        200,
                        json!({
                            "session_id": "devin-created",
                            "status": "running",
                            "status_detail": "working",
                            "url": "https://app.devin.ai/sessions/devin-created",
                            "pull_requests": [],
                        })
                        .to_string(),
                    )
                } else {
                    (429, json!({"error": "slow down"}).to_string())
                }
            } else {
                (200, json!({"ok": true}).to_string())
            };
            let _ = req.respond(tiny_http::Response::from_string(payload).with_status_code(status));
        }
    });
    let (dir, shared) = shared();
    let base = format!("http://{addr}");
    shared.provider_env.set("CADENCE_DEVIN_API_BASE", &base);
    shared
        .provider_env
        .set("CADENCE_DEVIN_API_KEY", "cog_test_secret_value");
    shared.provider_env.set("CADENCE_DEVIN_ORG_ID", "org-test");
    shared
        .provider_env
        .set("CADENCE_DEVIN_POLL_INTERVAL_MS", "20");
    shared
        .provider_env
        .set("CADENCE_DEVIN_POLL_BUDGET_MS", "80");
    shared
        .provider_env
        .set("CADENCE_DEVIN_RECOVER_BUDGET_MS", "80");
    let cwd = dir.path().to_str().unwrap();
    shared
        .store
        .register_agent(&NewAgent {
            alias: "pm",
            provider: "fake",
            endpoint_kind: "managed",
            role: "pm",
            cwd,
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let params = r#"{"repos":["favcrm/cadence"],"upstream":"pm"}"#;
    shared
        .store
        .register_agent(&NewAgent {
            alias: "cloud-1",
            provider: "devin",
            endpoint_kind: "cloud",
            role: "worker",
            cwd,
            sandbox: "read-only",
            instructions: None,
            params: Some(params),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    shared
        .store
        .enqueue("cloud-1", "do the held task", Some("pm"), "turn-1", "user")
        .unwrap();
    shared
        .store
        .enqueue(
            "cloud-1",
            "follow-up while the session works",
            Some("pm"),
            "follow-up",
            "user",
        )
        .unwrap();
    shared.launch_actor("cloud-1").unwrap();
    let started = Instant::now();
    loop {
        if started.elapsed() > Duration::from_secs(5) {
            panic!(
                "recovery did not escalate: gets={} posts={} events={:?}",
                gets.load(Ordering::SeqCst),
                posts.load(Ordering::SeqCst),
                shared.store.events("cloud-1", 0, 40).ok()
            );
        }
        let n = shared
            .store
            .events("cloud-1", 0, 40)
            .unwrap()
            .iter()
            .filter(|event| event.kind == "cloud_recover_escalated")
            .count();
        if n >= 1 {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let during = gets.load(Ordering::SeqCst);
    assert!(
        during <= 12,
        "held recovery made {during} session GETs; expected at most 12"
    );
    thread::sleep(Duration::from_millis(300));
    let after = gets.load(Ordering::SeqCst);
    assert_eq!(
        after, during,
        "recovery kept polling after escalation ({during} -> {after})"
    );
    let escalations = shared
        .store
        .events("cloud-1", 0, 40)
        .unwrap()
        .iter()
        .filter(|event| event.kind == "cloud_recover_escalated")
        .count();
    assert_eq!(escalations, 1, "expected one escalation, saw {escalations}");
    let notices: Vec<_> = shared
        .store
        .messages("pm")
        .unwrap()
        .into_iter()
        .filter(|message| message.body.contains("stopped polling"))
        .collect();
    assert_eq!(notices.len(), 1, "expected one escalation notice");
    assert!(notices[0].body.contains("not fenced"));
    assert!(notices[0].body.contains("not replayed"));
    assert!(notices[0]
        .body
        .contains("`cadence message reconcile turn-1 --status <completed|failed|interrupted>`"));
    assert!(notices[0]
        .body
        .contains("https://app.devin.ai/sessions/devin-created"));
    assert!(!notices[0].body.contains("cadence agent stop"));
    let agent = shared.store.agent("cloud-1").unwrap();
    assert_ne!(agent.state, "attention");
    assert!(agent.enabled);
    assert_eq!(
        shared.store.message("turn-1").unwrap().unwrap().state,
        "unknown"
    );
    assert_eq!(
        shared.store.message("follow-up").unwrap().unwrap().state,
        "queued"
    );
    assert_eq!(
        posts.load(Ordering::SeqCst),
        1,
        "replayed work into the session"
    );
    shared.store.set_enabled("cloud-1", false).unwrap();
    shared.begin_closing();
    let handle = {
        let lc = shared.lifecycle.lock().unwrap();
        lc.agents
            .get("cloud-1")
            .and_then(|ctl| ctl.thread.lock().unwrap().take())
    };
    if let Some(ctl) = shared.lifecycle.lock().unwrap().agents.get("cloud-1") {
        ctl.wake.notify_all();
    }
    if let Some(handle) = handle {
        let _ = handle.join();
    }
    stop.store(true, Ordering::SeqCst);
    let _ = thread.join();
}
