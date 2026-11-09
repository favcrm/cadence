//! CAD-2215 independent new-board / old-daemon agent-read acceptance.
//! The test-only downgrade flags simulate missing relay acknowledgements.
//! This is HTTP authorization coverage, not local-agent OS isolation.

use super::*;

#[allow(clippy::type_complexity)] // one fixture callback, one shape
fn agent_legacy_rig(
    flags: Arc<std::sync::atomic::AtomicU8>,
    observer: Arc<dyn Fn(&str, &str) + Send + Sync>,
) -> Rig {
    let dir = tempfile::Builder::new()
        .prefix("cad2215-legacy-agent")
        .tempdir()
        .unwrap();
    let state = dir.path().to_path_buf();
    let pm = state.join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    {
        let daemon_state = state.clone();
        let opts = ServeOptions {
            test_seam: true,
            assist_request_sweep_observer: Some(observer),
            stop: Some(stop.clone()),
            agent_reads_ignore_relay: Some(flags),
            ..ServeOptions::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        threads.push(std::thread::spawn(move || {
            crate::daemon::serve_with(&daemon_state, opts).unwrap()
        }));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "agent legacy fixture daemon did not start"
            );
            std::thread::yield_now();
        }
    }
    let key = Ed25519KeyPair::from_seed_unchecked(&[140; 32]).unwrap();
    let public_key = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let verify_public = key.public_key().as_ref().to_vec();
    let platform = loopback_server();
    let platform_port = match platform.server_addr() {
        tiny_http::ListenAddr::IP(a) => a.port(),
        _ => panic!("fixture platform bound unexpected address"),
    };
    let public_host = format!("localhost:{platform_port}");
    let issuer = format!("http://{public_host}");
    let authority: Arc<Mutex<Authority>> = Arc::new(Mutex::new(Authority {
        status: 200,
        ..Authority::default()
    }));
    {
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = crate::daemon::Shared::new(&state, &opts).unwrap();
        for sub in ["ana", "ben"] {
            shared
                .store
                .assistant_bind(
                    ORG,
                    sub,
                    &format!("assist_{sub}"),
                    &format!("assist-agent-{sub}"),
                    Some("rev0"),
                )
                .unwrap();
            authority
                .lock()
                .unwrap()
                .members
                .insert(sub.to_string(), MemberAuthority::live(sub));
        }
    }
    {
        let (stop, authority, issuer, public_host) = (
            stop.clone(),
            authority.clone(),
            issuer.clone(),
            public_host.clone(),
        );
        threads.push(std::thread::spawn(move || loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(Some(request)) = platform.recv_timeout(Duration::from_millis(200)) {
                answer_platform(
                    request,
                    &authority,
                    &issuer,
                    &public_host,
                    &public_key,
                    &verify_public,
                );
            }
        }));
    }
    let mut board_port = 0;
    for port in 3110..=3199u16 {
        if port == platform_port {
            continue;
        }
        let (startup, ready) = std::sync::mpsc::channel();
        let board = ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.clone()),
            startup: Some(startup),
            test_seam: true,
            allow_hosts: vec![public_host.clone()],
            public: Some(PublicBoard {
                host: public_host.clone(),
                issuer: issuer.clone(),
                company: ORG.into(),
                authorize_url: format!("{issuer}/authorize"),
            }),
            ..ServeOpts::default()
        };
        let (state, pm) = (state.clone(), pm.clone());
        let thread = std::thread::spawn(move || drop(crate::ui::serve(&state, &pm, &board)));
        match ready.recv_timeout(Duration::from_secs(30)).unwrap() {
            Ok(()) => {
                board_port = port;
                threads.push(thread);
                break;
            }
            Err(_) => {
                thread.join().unwrap();
            }
        }
    }
    assert!(board_port > 0, "agent legacy fixture board could not bind");
    let rig = Rig {
        _dir: dir,
        pm,
        board_port,
        public_host,
        issuer,
        key,
        authority,
        stop,
        _threads: threads,
    };
    enroll_queue_service(&rig);
    rig
}

fn agent_http(rig: &Rig, cookie: &str, path: &str) -> (u16, String) {
    let mut response = http_agent()
        .get(&format!("http://127.0.0.1:{}{path}", rig.board_port))
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("Cookie", cookie)
        .call()
        .expect("board request failed");
    let status = response.status().as_u16();
    // Never try to drain a successful SSE stream when an expected refusal
    // has failed: report its status directly instead of hanging the test.
    let body = if path.starts_with("/api/stream") && status == 200 {
        "<unexpected accepted SSE>".to_string()
    } else {
        response
            .body_mut()
            .read_to_string()
            .expect("board response body")
    };
    (status, body)
}

fn observed_agent_job(
    rig: &Rig,
    request: &Value,
    rx: &std::sync::mpsc::Receiver<(String, String)>,
) -> String {
    let request_id = request["request_id"]
        .as_str()
        .expect("request ID")
        .to_string();
    assert!(!request_id.is_empty());
    assert_eq!(
        *request,
        json!({"request_id":request_id,"status":"accepted"})
    );
    loop {
        let (id, outcome) = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("real queue worker did not report an attempt");
        if id == request_id {
            assert_eq!(
                outcome, "created",
                "real worker did not promote free fixture request"
            );
            break;
        }
    }
    let envelope = rig.envelope("ana", json!({}), json!({}));
    let (status, list) = rig.call("job_list", &envelope, &json!({}));
    assert_eq!(status, 200, "owner job list failed: {list}");
    let rows: Vec<_> = list["requests"]
        .as_array()
        .expect("request list")
        .iter()
        .filter(|r| r["request_id"] == request_id)
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["status"], "created");
    rows[0]["job"]
        .as_str()
        .expect("created request lacks job")
        .to_string()
}

#[test]
fn board_refuses_each_unacknowledged_agent_relay_without_breaking_operator_recovery() {
    let _flag = AssistantFlagOn::arm();
    let flags = Arc::new(std::sync::atomic::AtomicU8::new(0));
    let (tx, rx) = std::sync::mpsc::channel::<(String, String)>();
    let observer: Arc<dyn Fn(&str, &str) + Send + Sync> = Arc::new(move |id, outcome| {
        let _ = tx.send((id.to_string(), outcome.to_string()));
    });
    let rig = agent_legacy_rig(flags.clone(), observer);
    let _keys = initialize_queue_keyring(&rig);
    seed(&rig);
    // Both members can read the relevant project; private membership of a
    // particular job, not the project policy, is the distinction under test.
    std::fs::write(
        rig.pm.join("secret/project.yaml"),
        "key: secret\nprefix: SEC\n",
    )
    .unwrap();
    let envelope = rig.envelope("ana", json!({}), json!({}));
    let (status, request) = rig.call("job_request", &envelope, &json!({
        "title":"CAD2215_AGENT_LEGACY_PRIVATE_JOB", "request_key":"cad2215-agent-legacy", "issue":"VIS-9"
    }));
    assert_eq!(status, 200, "signed member job request failed: {request}");
    let job = observed_agent_job(&rig, &request, &rx);
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    assert!(shared.store.assist_request_owns(ORG, "ana", &job).unwrap());
    assert!(!shared.store.assist_request_owns(ORG, "ben", &job).unwrap());
    assert!(!shared.store.job_is_subscriber(&job, ORG, "ben").unwrap());
    let task = shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .expect("member task")
        .id
        .clone();
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    // Only assignment is synthetic; the job/issue and ownership ledger
    // came from the real signed request above.
    assert_eq!(
        db.execute(
            "UPDATE tasks SET assignee='cad2215-worker', state='running' WHERE id=?1",
            [&task]
        )
        .unwrap(),
        1
    );
    shared
        .store
        .enqueue_task(
            "cad2215-worker",
            "CAD2215_AGENT_LEGACY_PRIVATE_BODY",
            None,
            "cad2215-agent-legacy-private-message",
            "user",
            Some(&task),
        )
        .unwrap();
    shared.store.event_public_scoped(
        "cad2215-worker", "task_dispatched",
        json!({"message":"cad2215-agent-legacy-private-message", "marker":"CAD2215_AGENT_LEGACY_PRIVATE_EVENT"}),
        Some(&job), Some(&task),
    ).unwrap();
    let ana = login(&rig, "ana", "member");
    let ben = login(&rig, "ben", "member");
    let operator = login(&rig, "cad2215-agent-operator", "owner");
    let detail = "/api/agents/cad2215-worker";
    let secrets = [
        job.as_str(),
        task.as_str(),
        "CAD2215_AGENT_LEGACY_PRIVATE_JOB",
        "CAD2215_AGENT_LEGACY_PRIVATE_BODY",
        "CAD2215_AGENT_LEGACY_PRIVATE_EVENT",
        "cad2215-agent-legacy-private-message",
    ];

    // Current daemon positive controls: the readable ordinary worker stays
    // useful to Ben, while Ana and the operator can recover private work.
    let (status, member) = agent_http(&rig, &ben, detail);
    assert_eq!(status, 200, "current member detail unavailable: {member}");
    assert!(
        member.contains("cad2215-visible-task"),
        "ordinary shared task missing: {member}"
    );
    for secret in secrets {
        assert!(
            !member.contains(secret),
            "current member detail leaked {secret}: {member}"
        );
    }
    for cookie in [&ana, &operator] {
        let (status, body) = agent_http(&rig, cookie, detail);
        assert_eq!(status, 200, "authorized current detail refused: {body}");
        assert!(
            body.contains(&task),
            "authorized current detail lost task: {body}"
        );
    }
    let (status, body) = agent_http(&rig, &operator, detail);
    assert_eq!(status, 200);
    assert!(
        body.contains("CAD2215_AGENT_LEGACY_PRIVATE_EVENT"),
        "operator cannot recover private event: {body}"
    );

    // Exactly one method downgrades in each case. A real 503 is required;
    // returning 200 with sanitized content is not an acknowledgement check.
    for (mask, paths) in [
        (1u8, vec!["/api/agents", "/api/stream?entities=1"]),
        (2u8, vec![detail]),
        (4u8, vec![detail]),
    ] {
        flags.store(mask, std::sync::atomic::Ordering::SeqCst);
        for path in paths {
            let (status, body) = agent_http(&rig, &ben, path);
            assert_eq!(
                status, 503,
                "member accepted unacknowledged relay bit {mask} at {path}: {body}"
            );
            for secret in secrets {
                assert!(
                    !body.contains(secret),
                    "downgraded relay leaked {secret}: {body}"
                );
            }
        }
        let (status, body) = agent_http(&rig, &operator, detail);
        assert_eq!(
            status, 200,
            "operator recovery failed under relay bit {mask}: {body}"
        );
        assert!(
            body.contains(&task),
            "operator recovery omitted private task under bit {mask}: {body}"
        );
    }
    flags.store(0, std::sync::atomic::Ordering::SeqCst);
    let (status, restored) = agent_http(&rig, &ben, detail);
    assert_eq!(
        status, 200,
        "member read did not recover when current acknowledgements returned: {restored}"
    );
    assert!(restored.contains("cad2215-visible-task"));
    for secret in secrets {
        assert!(
            !restored.contains(secret),
            "restored member read leaked {secret}: {restored}"
        );
    }
}

#[test]
fn signed_cli_refuses_unacknowledged_agent_reads_and_keeps_operator_recovery() {
    let _flag = AssistantFlagOn::arm();
    let flags = Arc::new(std::sync::atomic::AtomicU8::new(0));
    let (tx, rx) = std::sync::mpsc::channel::<(String, String)>();
    let observer: Arc<dyn Fn(&str, &str) + Send + Sync> = Arc::new(move |id, outcome| {
        let _ = tx.send((id.to_string(), outcome.to_string()));
    });
    let rig = agent_legacy_rig(flags.clone(), observer);
    seed(&rig);
    std::fs::write(
        rig.pm.join("secret/project.yaml"),
        "key: secret\nprefix: SEC\n",
    )
    .unwrap();
    let envelope = rig.envelope("ana", json!({}), json!({}));
    let (status, request) = rig.call("job_request", &envelope, &json!({
        "title":"CAD2215_CLI_LEGACY_PRIVATE_JOB", "request_key":"cad2215-cli-agent-legacy", "issue":"VIS-9"
    }));
    assert_eq!(status, 200, "signed member job request failed: {request}");
    let job = observed_agent_job(&rig, &request, &rx);
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    assert!(shared.store.assist_request_owns(ORG, "ana", &job).unwrap());
    assert!(!shared.store.assist_request_owns(ORG, "ben", &job).unwrap());
    assert!(!shared.store.job_is_subscriber(&job, ORG, "ben").unwrap());
    let task = shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .expect("member task")
        .id
        .clone();
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    // Only task assignment is synthetic; signed job, readable issue and
    // ownership ledger are the actual member request's records.
    assert_eq!(
        db.execute(
            "UPDATE tasks SET assignee='cad2215-worker', state='running' WHERE id=?1",
            [&task]
        )
        .unwrap(),
        1
    );
    shared
        .store
        .enqueue_task(
            "cad2215-worker",
            "CAD2215_CLI_LEGACY_PRIVATE_BODY",
            None,
            "cad2215-cli-legacy-private-message",
            "user",
            Some(&task),
        )
        .unwrap();
    let cli_envelope = |owner: bool| {
        let now = crate::issue::time::now_epoch();
        let (sub, actor, role) = if owner {
            ("cad2215-cli-operator", "operator", "owner")
        } else {
            ("ben", "users/ben", "member")
        };
        let header = json!({"alg":"EdDSA", "typ":"agenticos-wiki-actor/1", "kid":KID});
        let claims = json!({
            "iss":rig.issuer, "aud":format!("http://{}",rig.public_host), "sub":sub, "actor":actor,
            "principal_kind":"user", "role_at_issue":role, "organization_id":ORG,
            "scope":["cli.read"], "credential_id":format!("cad2215-{sub}"),
            "iat":now, "exp":now+120, "jti":format!("cad2215-cli-legacy-{}",uuid::Uuid::new_v4())
        });
        let signed = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        );
        format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(rig.key.sign(signed.as_bytes()).as_ref())
        )
    };
    let call = |owner: bool, verb: &str, args: Value| {
        let mut response = http_agent()
            .post(&format!(
                "http://127.0.0.1:{}/api/cli/{verb}",
                rig.board_port
            ))
            .header("Host", &rig.public_host)
            .header("Content-Type", "application/json")
            .header(
                "Authorization",
                &format!("Bearer wikienv_{}", cli_envelope(owner)),
            )
            .send_json(args)
            .expect("signed hosted CLI request failed");
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .expect("CLI response body");
        assert!(
            !body.contains("_reader_scope"),
            "internal acknowledgement leaked via {verb}: {body}"
        );
        (status, body)
    };
    let secrets = [
        job.as_str(),
        task.as_str(),
        "CAD2215_CLI_LEGACY_PRIVATE_JOB",
        "CAD2215_CLI_LEGACY_PRIVATE_BODY",
        "cad2215-cli-legacy-private-message",
    ];
    for (verb, args, mask) in [
        ("status", json!({}), 1u8),
        ("agent_list", json!({}), 1u8),
        ("agent_show", json!({"alias":"cad2215-worker"}), 2u8),
    ] {
        flags.store(0, std::sync::atomic::Ordering::SeqCst);
        let (status, member) = call(false, verb, args.clone());
        assert_eq!(
            status, 200,
            "current signed member {verb} unavailable: {member}"
        );
        assert!(
            member.contains("cad2215-worker"),
            "current signed member lost public worker: {member}"
        );
        for secret in secrets {
            assert!(
                !member.contains(secret),
                "current signed member {verb} leaked {secret}: {member}"
            );
        }
        let (status, operator) = call(true, verb, args.clone());
        assert_eq!(
            status, 200,
            "current signed operator {verb} unavailable: {operator}"
        );
        assert!(
            operator.contains(&task),
            "current signed operator {verb} lost private task: {operator}"
        );

        flags.store(mask, std::sync::atomic::Ordering::SeqCst);
        let (status, refusal) = call(false, verb, args.clone());
        assert_eq!(
            status, 503,
            "signed member {verb} accepted unacknowledged relay bit {mask}: {refusal}"
        );
        for secret in secrets {
            assert!(
                !refusal.contains(secret),
                "unacknowledged {verb} leaked {secret}: {refusal}"
            );
        }
        let (status, operator) = call(true, verb, args.clone());
        assert_eq!(
            status, 200,
            "signed operator {verb} recovery failed under bit {mask}: {operator}"
        );
        assert!(
            operator.contains(&task),
            "signed operator {verb} lost private task under bit {mask}: {operator}"
        );

        flags.store(0, std::sync::atomic::Ordering::SeqCst);
        let (status, restored) = call(false, verb, args);
        assert_eq!(
            status, 200,
            "signed member {verb} did not recover: {restored}"
        );
        assert!(restored.contains("cad2215-worker"));
        for secret in secrets {
            assert!(
                !restored.contains(secret),
                "restored signed member {verb} leaked {secret}: {restored}"
            );
        }
    }
}
