//! CAD-2215 independent mixed-version master relay acceptance.
//! The product implementer must not edit or weaken this check.
//! The test-only daemon flag simulates an old daemon's unscoped answer; real
//! signed session, HTTP admission, thread reads and operator recovery remain.

use super::*;

fn legacy_master_rig() -> Rig {
    let dir = tempfile::Builder::new()
        .prefix("cad2215-legacy-master")
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
            master_state_ignore_relay: true,
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

fn legacy_http(rig: &Rig, cookie: &str, path: &str) -> (u16, String) {
    let mut response = http_agent()
        .get(&format!("http://127.0.0.1:{}{path}", rig.board_port))
        .header("Host", &rig.public_host)
        .header("Cookie", cookie)
        .call()
        .expect("signed HTTP request failed");
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .expect("HTTP body unreadable");
    (status, text)
}

#[test]
fn legacy_master_relay_is_refused_before_http_or_initial_sse_disclosure() {
    let rig = legacy_master_rig();
    seed(&rig);
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "master",
            provider: "fake",
            endpoint_kind: "fake",
            role: "pm",
            cwd: rig._dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let home = shared.store.ensure_thread("master").unwrap();
    shared
        .store
        .enqueue_task(
            "master",
            "CAD2215_LEGACY_PRIVATE_BODY",
            None,
            "cad2215-legacy-private-message",
            "user",
            Some("cad2215-visible-task"),
        )
        .unwrap();
    let member = login(&rig, "cad2215-legacy-member", "member");
    let owner = login(&rig, "cad2215-legacy-owner", "owner");
    let visible = get(&rig, &member, "/api/issues/VIS-1");
    assert!(
        visible.to_string().contains("VIS-1"),
        "signed member did not read visible issue"
    );

    // Positive controls use real operator session and the same actual global
    // state, not a mocked echo. The scope-ack exemption is recovery-only.
    let owner_http = get(&rig, &owner, "/api/master/state");
    let owner_sse = initial_master(&rig, &owner, "legacy operator");
    for (which, body) in [("HTTP", &owner_http), ("SSE", &owner_sse)] {
        let text = body.to_string();
        assert!(
            text.contains("cad2215-legacy-private-message"),
            "operator {which} missed real Home message"
        );
        assert!(
            text.contains(&home.id),
            "operator {which} missed real Home conversation"
        );
        assert!(
            body["queued"].as_u64().is_some_and(|n| n >= 1),
            "operator {which} missed actual queue count"
        );
    }
    for path in ["/api/master/state", "/api/threads/master/stream"] {
        let (status, body) = legacy_http(&rig, &member, path);
        assert_eq!(
            status, 503,
            "mixed-version {path} did not fail before disclosure: {body}"
        );
        for hidden in [
            "cad2215-legacy-private-message",
            "CAD2215_LEGACY_PRIVATE_BODY",
            home.id.as_str(),
        ] {
            assert!(
                !body.contains(hidden),
                "mixed-version {path} disclosed {hidden}"
            );
        }
        assert!(
            !body.contains("event: master"),
            "mixed-version {path} opened a master SSE snapshot"
        );
    }
}
