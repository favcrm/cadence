//! Real native public-board health requests, with synthetic pre-provisioned sessions.
use super::*;
use crate::daemon::{test_events, AutoStopSetting, ServeOptions};
use crate::operator_auth::{Auth, BoardUser, PublicRealm};
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// Isolate the real daemon's other detached workers from the rest of the suite.
#[test]
fn native_public_health_is_bound_to_the_requesting_session() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "ui::hosted_health_session_test::native_public_health_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .output()
        .expect("native health probe did not start");
    assert!(
        output.status.success(),
        "native health probe failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore]
fn native_public_health_child() {
    const PORT: u16 = 3188;
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let pm = root.path().join("pm");
    std::fs::create_dir(&state).unwrap();
    crate::issue::Pm::init(&pm).unwrap();
    for path in [&state, &pm] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let host = "health-fixture.localhost:3188";
    let realm = PublicRealm {
        host: host.into(),
        issuer: "http://health-issuer.localhost".into(),
        company: "health-fixture".into(),
    };
    crate::board_identity::write_config(
        &state,
        &crate::board_identity::Config {
            host: realm.host.clone(),
            issuer: realm.issuer.clone(),
            company: realm.company.clone(),
        },
    )
    .unwrap();
    let now = crate::issue::time::now_epoch();
    let mut auth = Auth::load(&state);
    let user = |sub: &str| BoardUser {
        sub: sub.into(),
        email: format!("{sub}@example.test"),
        name: sub.into(),
        role: "member".into(),
        handle: sub.into(),
    };
    let a = auth
        .open_public_bound(
            user("health-a"),
            "health-jti-a",
            now + 60,
            "fixture",
            realm.clone(),
            now,
        )
        .unwrap()
        .unwrap();
    let previous_b = auth
        .open_public_bound(
            user("health-b"),
            "health-jti-old-b",
            now + 60,
            "fixture",
            realm.clone(),
            now,
        )
        .unwrap()
        .unwrap();
    let b = auth
        .open_public_bound(
            user("health-b"),
            "health-jti-b",
            now + 60,
            "fixture",
            realm.clone(),
            now,
        )
        .unwrap()
        .unwrap();
    let c = auth
        .open_public_bound(
            user("health-c"),
            "health-jti-c",
            now + 60,
            "fixture",
            realm.clone(),
            now,
        )
        .unwrap()
        .unwrap();
    drop(auth);

    let stop = Arc::new(AtomicBool::new(false));
    let events = test_events::subscribe(&state);
    let opts = ServeOptions {
        test_seam: true,
        stop: Some(stop.clone()),
        checkup: Some(0),
        report_router: Some(0),
        auto_stop: Some(AutoStopSetting::off()),
        ..ServeOptions::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let daemon_state = state.clone();
    let daemon =
        std::thread::spawn(move || crate::daemon::serve_with(&daemon_state, opts).unwrap());
    events
        .wait_for(
            test_events::Event::AssistNoticesDrained,
            1,
            Duration::from_secs(30),
        )
        .expect("daemon did not start");
    let (startup, ready) = std::sync::mpsc::channel();
    let armed = Arc::new(AtomicBool::new(false));
    let (entered, entered_rx) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let hook_armed = Arc::clone(&armed);
    let before_session_proof = Arc::new(move || {
        if hook_armed.swap(false, Ordering::SeqCst) {
            entered.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30))
                .unwrap();
        }
    });
    let board_opts = ServeOpts {
        host: "127.0.0.1".into(),
        port: PORT,
        stop: Some(stop.clone()),
        startup: Some(startup),
        before_session_proof: Some(before_session_proof),
        allow_hosts: vec![host.into()],
        public: Some(PublicBoard {
            host: realm.host.clone(),
            issuer: realm.issuer.clone(),
            company: realm.company.clone(),
            authorize_url: format!("{}/authorize", realm.issuer),
        }),
        ..ServeOpts::default()
    };
    let board_state = state.clone();
    let board_pm = pm.clone();
    let board =
        std::thread::spawn(move || crate::ui::serve(&board_state, &board_pm, &board_opts).unwrap());
    ready
        .recv_timeout(Duration::from_secs(30))
        .expect("board did not answer")
        .expect("board could not bind");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .into();
    let request = |path: &str, token: Option<&str>, expected: Option<&str>| {
        let mut call = agent
            .get(format!("http://127.0.0.1:{PORT}{path}"))
            .header("Host", host);
        if let Some(token) = token {
            call = call.header("Cookie", format!("__Host-aos-board-session={token}"));
        }
        if let Some(expected) = expected {
            call = call.header("X-Cadence-Expected-Session", expected);
        }
        let mut response = call.call().expect("health request failed");
        let status = response.status().as_u16();
        let proof = response
            .headers()
            .get("X-Cadence-Verified-Session")
            .map(|v| v.to_str().unwrap().to_owned());
        let etag = response
            .headers()
            .get("ETag")
            .map(|v| v.to_str().unwrap().to_owned());
        let body: Value = response.body_mut().read_json().unwrap();
        (status, proof, body, etag)
    };
    let metadata_a = request("/api/meta?operator=1", Some(&a.token), None);
    let metadata_b = request("/api/meta?operator=1", Some(&b.token), None);
    assert_eq!((metadata_a.0, metadata_b.0), (200, 200));
    let binding_a = metadata_a.2["session"]["binding"]
        .as_str()
        .expect("missing session binding");
    let binding_b = metadata_b.2["session"]["binding"]
        .as_str()
        .expect("missing session binding");
    assert!(binding_a.len() >= 32 && binding_b.len() >= 32 && binding_a != binding_b);
    assert_ne!(
        binding_b, b.session.id,
        "a short display id is not a session binding"
    );
    assert!(!binding_b.contains(&b.token) && binding_b != crate::operator_auth::digest(&b.token));
    let current = request("/api/health", Some(&b.token), Some(binding_b));
    assert_eq!(current.0, 200);
    assert_eq!(current.1.as_deref(), Some(binding_b));
    assert!(
        current.2.get("issues").is_some(),
        "admitted health has counts"
    );
    let cached = agent
        .get(format!("http://127.0.0.1:{PORT}/api/health"))
        .header("Host", host)
        .header("Cookie", format!("__Host-aos-board-session={}", b.token))
        .header("X-Cadence-Expected-Session", binding_b)
        .header(
            "If-None-Match",
            current.3.as_deref().expect("health ETag missing"),
        )
        .call()
        .expect("conditional health request failed");
    assert_eq!(cached.status().as_u16(), 304);
    assert_eq!(
        cached
            .headers()
            .get("X-Cadence-Verified-Session")
            .and_then(|value| value.to_str().ok()),
        Some(binding_b)
    );
    let mismatch = request("/api/health", Some(&b.token), Some(binding_a));
    assert_eq!(mismatch.0, 409);
    assert!(
        mismatch.1.is_none() && mismatch.2.get("issues").is_none(),
        "another tab's counts must be withheld"
    );
    let ambiguous = request(
        "/api/health",
        Some(&b.token),
        Some(&format!("{binding_b}, {binding_a}")),
    );
    assert_eq!(ambiguous.0, 409);
    assert!(ambiguous.1.is_none() && ambiguous.2.get("issues").is_none());
    let legacy = request("/api/health", Some(&b.token), None);
    assert_eq!(legacy.0, 200);
    assert!(legacy.1.is_none() && legacy.2.get("issues").is_some());
    let anonymous = request("/api/health", None, None);
    assert_eq!(anonymous.0, 200);
    assert!(anonymous.1.is_none() && anonymous.2.get("issues").is_none());
    let missing = request("/api/health", None, Some(binding_b));
    assert_eq!(missing.0, 401);
    assert!(missing.1.is_none() && missing.2.get("issues").is_none());
    let replaced = request("/api/health", Some(&previous_b.token), Some(binding_b));
    assert_eq!(replaced.0, 401);
    assert!(replaced.1.is_none() && replaced.2.get("issues").is_none());

    for cookies in [
        format!(
            "__Host-aos-board-session={}; aos-board-session={}",
            a.token, b.token
        ),
        format!(
            "__Host-aos-board-session={}; __Host-aos-board-session={}",
            a.token, b.token
        ),
        format!(
            "__Host-aos-board-session={}; __Host-aos-board-session=",
            a.token
        ),
        format!(
            "__Host-aos-board-session={}; __Host-aos-board-session",
            a.token
        ),
        format!(
            "__Host-aos-board-session={}, __Host-aos-board-session={}",
            a.token, b.token
        ),
    ] {
        for path in ["/api/meta?operator=1", "/api/health"] {
            let mut response = agent
                .get(format!("http://127.0.0.1:{PORT}{path}"))
                .header("Host", host)
                .header("Cookie", &cookies)
                .header("X-Cadence-Expected-Session", binding_a)
                .call()
                .unwrap();
            assert_eq!(response.status().as_u16(), 400);
            assert!(response.headers().get("Set-Cookie").is_none());
            let body: Value = response.body_mut().read_json().unwrap();
            assert_eq!(body["code"], "ambiguous_session");
            assert!(body.get("session").is_none() && body.get("issues").is_none());
        }
        let mut logout = agent
            .post(format!("http://127.0.0.1:{PORT}/api/session/logout"))
            .header("Host", host)
            .header("Origin", format!("http://{host}"))
            .header("Content-Type", "application/json")
            .header("X-Cadence-Board", "1")
            .header("Cookie", &cookies)
            .send("{}")
            .unwrap();
        assert_eq!(logout.status().as_u16(), 400);
        assert!(logout.headers().get("Set-Cookie").is_none());
        let body: Value = logout.body_mut().read_json().unwrap();
        assert_eq!(body["code"], "ambiguous_session");
        let mut sign_in = agent
            .post(format!("http://127.0.0.1:{PORT}/__platform/session"))
            .header("Host", host)
            .header("Origin", format!("http://{host}"))
            .header("Content-Type", "application/json")
            .header("Cookie", &cookies)
            .send(r#"{"assertion":"synthetic"}"#)
            .unwrap();
        assert_eq!(sign_in.status().as_u16(), 400);
        assert!(sign_in.headers().get("Set-Cookie").is_none());
        let body: Value = sign_in.body_mut().read_json().unwrap();
        assert_eq!(body["error"]["code"], "ambiguous_session");
    }
    assert_eq!(request("/api/meta?operator=1", Some(&a.token), None).0, 200);
    assert_eq!(request("/api/meta?operator=1", Some(&b.token), None).0, 200);

    armed.store(true, Ordering::SeqCst);
    let token = b.token.clone();
    let binding = binding_b.to_owned();
    let pending = std::thread::spawn(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        let mut response = agent
            .get(format!("http://127.0.0.1:{PORT}/api/health"))
            .header("Host", host)
            .header("Cookie", format!("__Host-aos-board-session={token}"))
            .header("X-Cadence-Expected-Session", binding)
            .call()
            .expect("delayed health request failed");
        let status = response.status().as_u16();
        let proof = response
            .headers()
            .get("X-Cadence-Verified-Session")
            .is_some();
        let body: Value = response.body_mut().read_json().unwrap();
        (status, proof, body)
    });
    entered_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("health read did not reach its proof barrier");
    let revoked = crate::client::rpc(
        &state,
        "operator_session_logout",
        json!({ "token": b.token }),
    )
    .expect("fixture logout failed");
    assert_eq!(revoked["revoked"], true);
    release.send(()).unwrap();
    let delayed = pending.join().unwrap();
    assert_eq!(delayed.0, 503);
    assert!(
        !delayed.1 && delayed.2.get("issues").is_none(),
        "revoked in-flight health cannot release its counts"
    );

    let metadata_c = request("/api/meta?operator=1", Some(&c.token), None);
    assert_eq!(metadata_c.0, 200);
    assert!(metadata_c.2["session"]["binding"].is_string());
    armed.store(true, Ordering::SeqCst);
    let token = c.token.clone();
    let pending = std::thread::spawn(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        let mut response = agent
            .get(format!("http://127.0.0.1:{PORT}/api/meta?operator=1"))
            .header("Host", host)
            .header("Cookie", format!("__Host-aos-board-session={token}"))
            .call()
            .expect("delayed metadata request failed");
        let status = response.status().as_u16();
        let body: Value = response.body_mut().read_json().unwrap();
        (status, body)
    });
    entered_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("metadata read did not reach its proof barrier");
    let revoked = crate::client::rpc(
        &state,
        "operator_session_logout",
        json!({ "token": c.token }),
    )
    .expect("fixture logout failed");
    assert_eq!(revoked["revoked"], true);
    release.send(()).unwrap();
    let delayed = pending.join().unwrap();
    assert_eq!(delayed.0, 503);
    assert!(
        delayed.1.get("session").is_none(),
        "revoked metadata must not admit the old session"
    );
    stop.store(true, Ordering::SeqCst);
    board.join().unwrap();
    daemon.join().unwrap();
}
