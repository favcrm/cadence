//! CAD-2215 independent signed-board in-flight agent-detail revocation test.
//! The hook controls scheduling only; it grants no authority or OS isolation.

use super::*;

fn revoke_race_board(
    rig: &mut Rig,
    read_hook: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    show_hook: Option<Arc<dyn Fn(&str) + Send + Sync>>,
) {
    for port in 3110..=3199u16 {
        if port == rig.board_port || rig.public_host == format!("localhost:{port}") {
            continue;
        }
        let (startup, ready) = std::sync::mpsc::channel();
        let opts = ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(rig.stop.clone()),
            startup: Some(startup),
            test_seam: true,
            allow_hosts: vec![rig.public_host.clone()],
            public: Some(PublicBoard {
                host: rig.public_host.clone(),
                issuer: rig.issuer.clone(),
                company: ORG.into(),
                authorize_url: format!("{}/authorize", rig.issuer),
            }),
            agent_detail_read_hook: read_hook.clone(),
            agent_detail_show_hook: show_hook.clone(),
            ..ServeOpts::default()
        };
        let state = rig._dir.path().to_path_buf();
        let pm = rig.pm.clone();
        let thread = std::thread::spawn(move || drop(crate::ui::serve(&state, &pm, &opts)));
        match ready
            .recv_timeout(Duration::from_secs(30))
            .expect("hooked board startup witness")
        {
            Ok(()) => {
                rig.board_port = port;
                rig._threads.push(thread);
                return;
            }
            Err(_) => {
                thread.join().unwrap();
            }
        }
    }
    panic!("hooked agent detail board could not bind");
}

fn revoke_race_get(port: u16, host: &str, cookie: &str, path: &str) -> (u16, String) {
    let mut response = http_agent()
        .get(&format!("http://127.0.0.1:{port}{path}"))
        .header("Host", host)
        .header("Cookie", cookie)
        .header("Origin", &format!("http://{host}"))
        .header("Sec-Fetch-Site", "same-origin")
        .call()
        .expect("signed board GET failed");
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .expect("board response body");
    (status, body)
}

#[test]
fn private_assistant_detail_reproves_target_after_binding_revocation() {
    let _flag = AssistantFlagOn::arm();
    let mut rig = start(&["ana"]);
    seed(&rig);
    let env = rig.envelope("ana", json!({}), json!({}));
    let (status, state) = rig.call("assist_state", &env, &json!({}));
    assert_eq!(
        status, 200,
        "signed member assistant provisioning failed: {state}"
    );
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    let binding = shared
        .store
        .assistant_binding(ORG, "ana")
        .unwrap()
        .expect("real member binding");
    let alias = binding.alias.clone();
    assert!(binding.is_active());
    assert!(
        shared.store.agent_opt(&alias).unwrap().is_some(),
        "assistant runtime was not provisioned"
    );

    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let release_rx = Arc::new(Mutex::new(release_rx));
    let armed = Arc::new(AtomicBool::new(false));
    let hook: Arc<dyn Fn(&str) + Send + Sync> = {
        let target = alias.clone();
        let armed = armed.clone();
        let release_rx = release_rx.clone();
        Arc::new(move |observed| {
            if observed == target && armed.swap(false, Ordering::SeqCst) {
                entered_tx.send(()).expect("test observer disappeared");
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(25))
                    .expect("test did not release detail");
            }
        })
    };
    revoke_race_board(&mut rig, Some(hook), None);
    let ana = login(&rig, "ana", "member");
    let operator = login(&rig, "cad2215-revoke-operator", "owner");
    let path = format!("/api/agents/{alias}");
    let (status, before) = revoke_race_get(rig.board_port, &rig.public_host, &ana, &path);
    assert_eq!(
        status, 200,
        "active owner must see assistant before revocation: {before}"
    );
    assert!(
        before.contains(&alias),
        "active assistant alias missing: {before}"
    );

    armed.store(true, Ordering::SeqCst);
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let (port, host, cookie, request_path) = (
        rig.board_port,
        rig.public_host.clone(),
        ana.clone(),
        path.clone(),
    );
    let inflight = std::thread::spawn(move || {
        result_tx
            .send(revoke_race_get(port, &host, &cookie, &request_path))
            .unwrap();
    });
    entered_rx
        .recv_timeout(Duration::from_secs(25))
        .expect("in-flight detail did not reach post-read hook");
    shared
        .store
        .assistant_revoke(ORG, "ana")
        .expect("revoke actual assistant binding");
    assert!(!shared
        .store
        .assistant_binding(ORG, "ana")
        .unwrap()
        .unwrap()
        .is_active());
    // The same signed session must still be useful, and a fresh read must
    // already refuse the now-inactive private target before releasing it.
    let (public_status, public) = revoke_race_get(
        rig.board_port,
        &rig.public_host,
        &ana,
        "/api/agents/cad2215-worker",
    );
    assert_eq!(
        public_status, 200,
        "member session/public worker no longer works: {public}"
    );
    let (fresh_status, fresh) = revoke_race_get(rig.board_port, &rig.public_host, &ana, &path);
    assert!(
        fresh_status == 403 || fresh_status == 404 || fresh_status == 503,
        "fresh revoked target still readable: {fresh_status} {fresh}"
    );
    assert!(
        !fresh.contains(&alias),
        "fresh refusal leaked revoked alias: {fresh}"
    );
    let (operator_status, operator_body) =
        revoke_race_get(rig.board_port, &rig.public_host, &operator, &path);
    assert_eq!(
        operator_status, 200,
        "operator recovery unavailable: {operator_body}"
    );
    assert!(
        operator_body.contains(&alias),
        "operator cannot recover assistant: {operator_body}"
    );

    release_tx.send(()).expect("release in-flight detail");
    let (inflight_status, body) = result_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("in-flight detail result");
    inflight.join().unwrap();
    assert!(
        inflight_status == 403 || inflight_status == 404 || inflight_status == 503,
        "in-flight revoked target returned success: {inflight_status} {body}"
    );
    assert!(
        !body.contains(&alias),
        "in-flight response leaked revoked alias: {body}"
    );
    assert!(
        !body.contains("assist-agent-ana"),
        "in-flight response leaked private assistant metadata: {body}"
    );
}

#[test]
fn private_assistant_detail_rejects_real_event_guard_failure_after_revocation() {
    let _flag = AssistantFlagOn::arm();
    let mut rig = start(&["ana"]);
    seed(&rig);
    let env = rig.envelope("ana", json!({}), json!({}));
    let (status, state) = rig.call("assist_state", &env, &json!({}));
    assert_eq!(
        status, 200,
        "signed member assistant provisioning failed: {state}"
    );
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    let binding = shared
        .store
        .assistant_binding(ORG, "ana")
        .unwrap()
        .expect("real member binding");
    let alias = binding.alias.clone();
    assert!(binding.is_active());
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let release_rx = Arc::new(Mutex::new(release_rx));
    let armed = Arc::new(AtomicBool::new(false));
    let hook: Arc<dyn Fn(&str) + Send + Sync> = {
        let target = alias.clone();
        let armed = armed.clone();
        let release_rx = release_rx.clone();
        Arc::new(move |observed| {
            if observed == target && armed.swap(false, Ordering::SeqCst) {
                entered_tx.send(()).expect("post-show observer disappeared");
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(25))
                    .expect("test did not release post-show detail");
            }
        })
    };
    revoke_race_board(&mut rig, None, Some(hook));
    let ana = login(&rig, "ana", "member");
    let operator = login(&rig, "cad2215-event-revoke-operator", "owner");
    let path = format!("/api/agents/{alias}");
    let (status, before) = revoke_race_get(rig.board_port, &rig.public_host, &ana, &path);
    assert_eq!(
        status, 200,
        "active owner must see assistant before revocation: {before}"
    );
    assert!(before.contains(&alias));
    armed.store(true, Ordering::SeqCst);
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let (port, host, cookie, request_path) = (
        rig.board_port,
        rig.public_host.clone(),
        ana.clone(),
        path.clone(),
    );
    let inflight = std::thread::spawn(move || {
        result_tx
            .send(revoke_race_get(port, &host, &cookie, &request_path))
            .unwrap();
    });
    entered_rx
        .recv_timeout(Duration::from_secs(25))
        .expect("detail did not reach post-show hook");
    shared
        .store
        .assistant_revoke(ORG, "ana")
        .expect("revoke actual assistant binding");
    assert!(!shared
        .store
        .assistant_binding(ORG, "ana")
        .unwrap()
        .unwrap()
        .is_active());
    let (public_status, public) = revoke_race_get(
        rig.board_port,
        &rig.public_host,
        &ana,
        "/api/agents/cad2215-worker",
    );
    assert_eq!(
        public_status, 200,
        "session/public worker no longer works: {public}"
    );
    let (fresh_status, fresh) = revoke_race_get(rig.board_port, &rig.public_host, &ana, &path);
    assert!(
        fresh_status == 403 || fresh_status == 404 || fresh_status == 503,
        "fresh revoked target still readable: {fresh_status} {fresh}"
    );
    assert!(!fresh.contains(&alias));
    let (operator_status, operator_body) =
        revoke_race_get(rig.board_port, &rig.public_host, &operator, &path);
    assert_eq!(
        operator_status, 200,
        "operator recovery unavailable: {operator_body}"
    );
    assert!(operator_body.contains(&alias));
    release_tx.send(()).expect("release post-show detail");
    let (inflight_status, body) = result_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("in-flight detail result");
    inflight.join().unwrap();
    assert!(
        inflight_status == 403 || inflight_status == 404 || inflight_status == 503,
        "event guard refusal was swallowed into success: {inflight_status} {body}"
    );
    assert!(
        !body.contains(&alias),
        "in-flight response leaked revoked alias: {body}"
    );
    assert!(
        !body.contains("events_error"),
        "raw event failure was surfaced: {body}"
    );
    assert!(
        !body.contains("Unknown managed agent"),
        "raw daemon event refusal was surfaced: {body}"
    );
}

fn agent_detail_native_role_rig(demoted: Arc<AtomicBool>) -> Rig {
    let dir = tempfile::Builder::new()
        .prefix("cad2215-detail-native-role")
        .tempdir()
        .unwrap();
    let state = dir.path().to_path_buf();
    let pm = state.join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    let key = Ed25519KeyPair::from_seed_unchecked(&[140; 32]).unwrap();
    let public_key = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let verify_public = key.public_key().as_ref().to_vec();
    let platform = loopback_server();
    let platform_port = match platform.server_addr() {
        tiny_http::ListenAddr::IP(a) => a.port(),
        _ => panic!("native fixture platform bound unexpected address"),
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
    let lookup: crate::daemon::MembershipLookupForFixture =
        Arc::new(move |_hash, sub, company, instance| {
            assert_eq!(company, ORG);
            assert_eq!(instance, "abc1");
            let role = if sub == "cad2215-role-old" && demoted.load(Ordering::SeqCst) {
                crate::board_identity::Role::Member
            } else if sub == "cad2215-role-old" || sub == "cad2215-role-control" {
                crate::board_identity::Role::Operator
            } else {
                return Ok(None);
            };
            Ok(Some(role))
        });
    let mode =
        crate::daemon::TestMembershipMode::Native(crate::daemon::TestNativeMembershipContext {
            company: ORG.into(),
            instance: "abc1".into(),
            realm: crate::operator_auth::PublicRealm {
                issuer: issuer.clone(),
                company: ORG.into(),
                host: public_host.clone(),
            },
        });
    {
        let daemon_state = state.clone();
        let opts = ServeOptions {
            test_seam: true,
            stop: Some(stop.clone()),
            membership_mode_for_test: Some(mode),
            membership_lookup_for_test: Some(lookup),
            ..ServeOptions::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        threads.push(std::thread::spawn(move || {
            crate::daemon::serve_with(&daemon_state, opts).unwrap()
        }));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
            assert!(
                Instant::now() < deadline,
                "native role fixture daemon did not start"
            );
            std::thread::yield_now();
        }
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
    assert!(board_port > 0, "native role fixture board could not bind");
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

#[test]
fn in_flight_operator_agent_detail_rechecks_live_role_after_demotion() {
    let demoted = Arc::new(AtomicBool::new(false));
    let mut rig = agent_detail_native_role_rig(demoted.clone());
    seed(&rig);
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    shared
        .store
        .enqueue_task(
            "cad2215-worker",
            "CAD2215_DEMOTION_PRIVATE_BODY",
            None,
            "cad2215-demotion-private-message",
            "user",
            Some("cad2215-secret-task"),
        )
        .unwrap();
    shared
        .store
        .mark_running("cad2215-demotion-private-message", "cad2215-demotion-turn")
        .unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let release_rx = Arc::new(Mutex::new(release_rx));
    let armed = Arc::new(AtomicBool::new(false));
    let hook: Arc<dyn Fn(&str) + Send + Sync> = {
        let armed = armed.clone();
        let release_rx = release_rx.clone();
        Arc::new(move |alias| {
            if alias == "cad2215-worker" && armed.swap(false, Ordering::SeqCst) {
                entered_tx.send(()).expect("demotion observer disappeared");
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(25))
                    .expect("test did not release owner detail");
            }
        })
    };
    revoke_race_board(&mut rig, Some(hook), None);
    let old = login(&rig, "cad2215-role-old", "owner");
    let control = login(&rig, "cad2215-role-control", "owner");
    let path = "/api/agents/cad2215-worker";
    let (status, before) = revoke_race_get(rig.board_port, &rig.public_host, &old, path);
    assert_eq!(
        status, 200,
        "original operator detail unavailable: {before}"
    );
    assert!(
        before.contains("cad2215-secret-task"),
        "original operator did not see private task: {before}"
    );
    armed.store(true, Ordering::SeqCst);
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let (port, host, cookie) = (rig.board_port, rig.public_host.clone(), old.clone());
    let inflight = std::thread::spawn(move || {
        result_tx
            .send(revoke_race_get(
                port,
                &host,
                &cookie,
                "/api/agents/cad2215-worker",
            ))
            .unwrap();
    });
    entered_rx
        .recv_timeout(Duration::from_secs(25))
        .expect("operator detail did not reach post-read hook");
    demoted.store(true, Ordering::SeqCst);
    let (owner_only_status, owner_only) = revoke_race_get(
        rig.board_port,
        &rig.public_host,
        &old,
        "/api/settings/agents",
    );
    assert!(
        owner_only_status == 401 || owner_only_status == 403 || owner_only_status == 404,
        "same demoted session retained an owner-only endpoint: {owner_only_status} {owner_only}"
    );
    let (fresh_status, fresh) = revoke_race_get(rig.board_port, &rig.public_host, &old, path);
    assert_eq!(
        fresh_status, 200,
        "demoted member lost public worker: {fresh}"
    );
    assert!(fresh.contains("cad2215-visible-task"));
    for hidden in [
        "cad2215-secret-task",
        "cad2215-demotion-private-message",
        "CAD2215_DEMOTION_PRIVATE_BODY",
    ] {
        assert!(
            !fresh.contains(hidden),
            "fresh demoted member saw {hidden}: {fresh}"
        );
    }
    let (control_status, control_body) =
        revoke_race_get(rig.board_port, &rig.public_host, &control, path);
    assert_eq!(
        control_status, 200,
        "independent operator recovery unavailable: {control_body}"
    );
    assert!(control_body.contains("cad2215-secret-task"));
    release_tx.send(()).expect("release operator detail");
    let (status, body) = result_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("in-flight detail result");
    inflight.join().unwrap();
    assert!(
        status == 200 || status == 401 || status == 403 || status == 404 || status == 503,
        "unexpected response after demotion: {status} {body}"
    );
    for hidden in [
        "cad2215-secret-task",
        "cad2215-demotion-private-message",
        "CAD2215_DEMOTION_PRIVATE_BODY",
    ] {
        assert!(
            !body.contains(hidden),
            "in-flight demoted owner received {hidden}: {body}"
        );
    }
    if status == 200 {
        assert!(
            !body.contains("\"recovery\""),
            "in-flight demoted response retained operator recovery field: {body}"
        );
        assert!(
            !body.contains("\"stats\""),
            "in-flight demoted response retained operator stats: {body}"
        );
    }
}
