use super::*;

#[allow(clippy::type_complexity)] // one fixture callback, one shape
fn legacy_job_rig(observer: Arc<dyn Fn(&str, &str) + Send + Sync>) -> Rig {
    let dir = tempfile::Builder::new()
        .prefix("cad2215-legacy-job")
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
            stop: Some(stop.clone()),
            job_list_ignore_relay: true,
            assist_request_sweep_observer: Some(observer),
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
                "legacy-mode daemon did not start"
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
    assert!(board_port > 0, "legacy fixture board could not bind");
    Rig {
        _dir: dir,
        pm,
        board_port,
        public_host,
        issuer,
        key,
        authority,
        stop,
        _threads: threads,
    }
}

fn response_text(rig: &Rig, cookie: &str, path: &str) -> (u16, String) {
    let mut response = http_agent()
        .get(&format!("http://127.0.0.1:{}{path}", rig.board_port))
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("Cookie", cookie)
        .call()
        .unwrap();
    let status = response.status().as_u16();
    let body = response.body_mut().read_to_string().unwrap();
    (status, body)
}

#[allow(clippy::type_complexity)] // one fixture channel pair, one shape
fn queue_observer() -> (
    Arc<dyn Fn(&str, &str) + Send + Sync>,
    std::sync::mpsc::Receiver<(String, String)>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let observer = Arc::new(move |request_id: &str, outcome: &str| {
        let _ = tx.send((request_id.to_string(), outcome.to_string()));
    });
    (observer, rx)
}

fn private_fixture(
    rig: &Rig,
    outcomes: &std::sync::mpsc::Receiver<(String, String)>,
) -> (String, String) {
    seed(rig);
    let envelope = rig.envelope("ana", json!({}), json!({}));
    let (status, response) = rig.call("job_request", &envelope,
        &json!({"title":"CAD2215_LEGACY_JOB_PRIVATE", "request_key":"cad2215-legacy-job", "issue":"VIS-9"}));
    assert_eq!(status, 200, "real member job request failed: {response}");
    let request_id = response["request_id"]
        .as_str()
        .expect("missing request receipt ID")
        .to_string();
    assert!(!request_id.is_empty());
    assert_eq!(
        response,
        json!({"request_id":request_id,"status":"accepted"})
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .expect("worker did not promote this request before deadline");
        let (id, outcome) = outcomes
            .recv_timeout(remaining)
            .expect("worker observer closed or timed out");
        if id != request_id {
            continue;
        }
        match outcome.as_str() {
            "created" => break,
            "pending" => continue,
            other => panic!("real request promotion failed: {other}"),
        }
    }
    let list_envelope = rig.envelope("ana", json!({}), json!({}));
    let (list_status, list) = rig.call("job_list", &list_envelope, &json!({}));
    assert_eq!(list_status, 200, "signed owner job list failed: {list}");
    let matches: Vec<_> = list["requests"]
        .as_array()
        .expect("missing owner request list")
        .iter()
        .filter(|row| row["request_id"] == request_id)
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "created request missing or duplicated: {list}"
    );
    assert_eq!(matches[0]["status"], "created");
    let job = matches[0]["job"]
        .as_str()
        .expect("created request has no job")
        .to_string();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    assert!(shared.store.assist_request_owns(ORG, "ana", &job).unwrap());
    assert!(!shared.store.assist_request_owns(ORG, "ben", &job).unwrap());
    assert!(!shared.store.job_is_subscriber(&job, ORG, "ben").unwrap());
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    // Every legacy row has a readable issue and task list. Otherwise the
    // pre-existing project/unbound guard would reject before the new guard.
    assert_eq!(db.execute("UPDATE jobs SET issue_id='VIS-1' WHERE id IN ('cad2215-secret-job','cad2215-free-job')", []).unwrap(), 2);
    // The ownership ledger comes from the real request above. The issue rebind
    // and worker assignment are synthetic fixture setup, not a supported API.
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id='VIS-1' WHERE id=?1", [&job])
            .unwrap(),
        1
    );
    let task: String = db
        .query_row(
            "SELECT id FROM tasks WHERE job_id=?1 ORDER BY id LIMIT 1",
            [&job],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        db.execute(
            "UPDATE tasks SET assignee='cad2215-worker', state='running' WHERE id=?1",
            [&task]
        )
        .unwrap(),
        1
    );
    (job, task)
}

#[test]
fn mixed_version_job_relay_fails_closed_for_http_and_shared_sse() {
    let _flag = AssistantFlagOn::arm();
    // A current daemon is a positive control for real member ownership and
    // for the shared stream; the legacy simulation is a separate daemon.
    {
        let (observer, outcomes) = queue_observer();
        let rig = start_with_queue_service(&["ana", "ben"], Some(observer), true);
        let _keys = initialize_queue_keyring(&rig);
        let (job, task) = private_fixture(&rig, &outcomes);
        let ana = login(&rig, "ana", "member");
        let ben = login(&rig, "ben", "member");
        let owner = login(&rig, "cad2215-job-owner", "owner");
        for (cookie, sees) in [(&ana, true), (&ben, false), (&owner, true)] {
            let (status, body) = response_text(&rig, cookie, "/api/issues/VIS-1");
            assert_eq!(status, 200, "current daemon issue read failed: {body}");
            assert!(
                body.contains("VIS-1") && body.contains("cad2215-visible-task"),
                "ordinary readable issue lost: {body}"
            );
            assert_eq!(
                body.contains(&task),
                sees,
                "wrong private task projection: {body}"
            );
            if !sees {
                assert!(!body.contains(&job) && !body.contains("CAD2215_LEGACY_JOB_PRIVATE"));
            }
        }
        let mut member_stream = EventStream::open(&rig, &ben, "current member");
        let mut owner_stream = EventStream::open(&rig, &owner, "current operator");
        std::fs::write(
            rig.pm.join("visible/VIS-1/issue.md"),
            issue_text("VIS-1", "CAD2215_JOB_STREAM_CHECKPOINT"),
        )
        .unwrap();
        let owner_frames = owner_stream.through("CAD2215_JOB_STREAM_CHECKPOINT");
        let member_frames = member_stream.through("CAD2215_JOB_STREAM_CHECKPOINT");
        assert!(owner_frames.contains("CAD2215_JOB_STREAM_CHECKPOINT"));
        for secret in [job.as_str(), task.as_str(), "CAD2215_LEGACY_JOB_PRIVATE"] {
            assert!(
                !member_frames.contains(secret),
                "current member stream exposed {secret}: {member_frames}"
            );
        }
    }
    {
        let (observer, outcomes) = queue_observer();
        let rig = legacy_job_rig(observer);
        enroll_queue_service(&rig);
        let (job, task) = private_fixture(&rig, &outcomes);
        let ben = login(&rig, "ben", "member");
        let owner = login(&rig, "cad2215-job-owner", "owner");
        let (status, body) = response_text(&rig, &owner, "/api/issues/VIS-1");
        assert_eq!(status, 200, "operator recovery unavailable: {body}");
        assert!(
            body.contains(&task),
            "operator could not recover private task: {body}"
        );
        for path in ["/api/issues", "/api/issues/VIS-1", "/api/stream?entities=1"] {
            let (status, body) = response_text(&rig, &ben, path);
            assert_eq!(
                status, 503,
                "unacknowledged job list accepted on {path}: {body}"
            );
            for secret in [job.as_str(), task.as_str(), "CAD2215_LEGACY_JOB_PRIVATE"] {
                assert!(!body.contains(secret), "{path} exposed {secret}: {body}");
            }
        }
    }
}
