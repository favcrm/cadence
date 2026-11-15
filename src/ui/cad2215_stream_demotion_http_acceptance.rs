//! CAD-2215 independent live-role stream acceptance.
//! The product implementer must not edit or weaken this check.
//! The native role lookup is a mutable test fixture: this exercises the real
//! session/HTTP/stream path, NOT the external platform membership proof API.

use super::*;

fn native_role_rig(demoted: Arc<AtomicBool>) -> Rig {
    let dir = tempfile::Builder::new()
        .prefix("cad2215-native-role")
        .tempdir()
        .unwrap();
    let state = dir.path().to_path_buf();
    let pm = state.join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();

    // The listener must exist first because native membership pins the board
    // realm at daemon startup. It is synthetic local authority, not the
    // production private membership-proof endpoint.
    let key = Ed25519KeyPair::from_seed_unchecked(&[140; 32]).unwrap();
    let public_key = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let verify_public = key.public_key().as_ref().to_vec();
    let platform = loopback_server();
    let platform_port = match platform.server_addr() {
        tiny_http::ListenAddr::IP(a) => a.port(),
        _ => panic!("fixture platform bound an unexpected address"),
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
                "native fixture daemon did not start"
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
    assert!(board_port > 0, "native fixture board could not bind");
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

fn thread_stream(rig: &Rig, cookie: &str, who: &'static str) -> EventStream {
    let mut socket = TcpStream::connect(("127.0.0.1", rig.board_port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(25)))
        .unwrap();
    write!(socket, "GET /api/threads/cad2215-worker/stream HTTP/1.1\r\nHost: {}\r\nCookie: {}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n", rig.public_host, cookie).unwrap();
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(
        line.starts_with("HTTP/1.1 200 "),
        "{who} thread stream refused: {line}"
    );
    loop {
        line.clear();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "{who} thread stream closed in headers"
        );
        if line == "\r\n" || line == "\n" {
            break;
        }
    }
    let mut stream = EventStream { reader, who };
    let first = stream
        .next(Instant::now() + Duration::from_secs(25), "thread hello")
        .expect("thread stream closed");
    assert!(
        first.contains(": ping"),
        "{who} thread stream did not initialize: {first}"
    );
    stream
}

fn append_thread(shared: &crate::daemon::Shared, kind: &str, text: &str) {
    let seq = shared
        .store
        .thread_append(
            "cad2215-worker",
            crate::store::NewEntry {
                role: crate::store::ROLE_SYSTEM,
                kind,
                text,
                payload: None,
                message_id: None,
            },
        )
        .unwrap();
    assert!(seq.is_some(), "thread fixture entry was not persisted");
}

#[test]
fn same_live_owner_session_loses_main_and_thread_stream_privilege_on_demotion() {
    let demoted = Arc::new(AtomicBool::new(false));
    let rig = native_role_rig(demoted.clone());
    seed(&rig);
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    shared.store.ensure_thread("cad2215-worker").unwrap();
    let old_cookie = login(&rig, "cad2215-role-old", "owner");
    let control_cookie = login(&rig, "cad2215-role-control", "owner");
    let mut old_main = EventStream::open(&rig, &old_cookie, "old owner main");
    let mut control_main = EventStream::open(&rig, &control_cookie, "control owner main");
    let mut old_thread = thread_stream(&rig, &old_cookie, "old owner thread");
    let mut control_thread = thread_stream(&rig, &control_cookie, "control owner thread");

    let secret = rig.pm.join("secret/SEC-1/issue.md");
    std::fs::write(&secret, issue_text("SEC-1", "CAD2215_ROLE_BEFORE_MAIN")).unwrap();
    assert!(control_main
        .through("CAD2215_ROLE_BEFORE_MAIN")
        .contains("CAD2215_ROLE_BEFORE_MAIN"));
    assert!(old_main
        .through("CAD2215_ROLE_BEFORE_MAIN")
        .contains("CAD2215_ROLE_BEFORE_MAIN"));
    append_thread(
        &shared,
        crate::store::KIND_ARTIFACT,
        "CAD2215_ROLE_BEFORE_THREAD",
    );
    assert!(control_thread
        .through("CAD2215_ROLE_BEFORE_THREAD")
        .contains("CAD2215_ROLE_BEFORE_THREAD"));
    assert!(old_thread
        .through("CAD2215_ROLE_BEFORE_THREAD")
        .contains("CAD2215_ROLE_BEFORE_THREAD"));

    // Mutate the CURRENT native membership lookup, not the saved cookie.
    // A same-cookie ordinary HTTP read positively proves effective demotion.
    demoted.store(true, Ordering::SeqCst);
    let projects = get(&rig, &old_cookie, "/api/projects").to_string();
    assert!(
        !projects.contains("secret"),
        "same session did not lose private project on demotion"
    );
    let owner_projects = get(&rig, &control_cookie, "/api/projects").to_string();
    assert!(
        owner_projects.contains("secret"),
        "control operator lost recovery project"
    );

    // Control operator receives each protected post-demotion marker. The old
    // stream must close or omit it before a later public checkpoint.
    std::fs::write(
        &secret,
        issue_text("SEC-1", "CAD2215_ROLE_AFTER_MAIN_PRIVATE"),
    )
    .unwrap();
    assert!(control_main
        .through("CAD2215_ROLE_AFTER_MAIN_PRIVATE")
        .contains("CAD2215_ROLE_AFTER_MAIN_PRIVATE"));
    append_thread(
        &shared,
        crate::store::KIND_ARTIFACT,
        "CAD2215_ROLE_AFTER_THREAD_PRIVATE",
    );
    assert!(control_thread
        .through("CAD2215_ROLE_AFTER_THREAD_PRIVATE")
        .contains("CAD2215_ROLE_AFTER_THREAD_PRIVATE"));
    let visible = rig.pm.join("visible/VIS-1/issue.md");
    std::fs::write(
        &visible,
        issue_text("VIS-1", "CAD2215_ROLE_AFTER_MAIN_PUBLIC"),
    )
    .unwrap();
    assert!(control_main
        .through("CAD2215_ROLE_AFTER_MAIN_PUBLIC")
        .contains("CAD2215_ROLE_AFTER_MAIN_PUBLIC"));
    append_thread(
        &shared,
        crate::store::KIND_ASSISTANT_TEXT,
        "CAD2215_ROLE_AFTER_THREAD_PUBLIC",
    );
    assert!(control_thread
        .through("CAD2215_ROLE_AFTER_THREAD_PUBLIC")
        .contains("CAD2215_ROLE_AFTER_THREAD_PUBLIC"));
    let old_main_frames = old_main.until_checkpoint_or_close("CAD2215_ROLE_AFTER_MAIN_PUBLIC");
    let old_thread_frames =
        old_thread.until_checkpoint_or_close("CAD2215_ROLE_AFTER_THREAD_PUBLIC");
    assert!(
        !old_main_frames.contains("CAD2215_ROLE_AFTER_MAIN_PRIVATE"),
        "demoted session received private main frame"
    );
    assert!(
        !old_thread_frames.contains("CAD2215_ROLE_AFTER_THREAD_PRIVATE"),
        "demoted session received operator-only thread entry"
    );
}
