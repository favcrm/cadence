//! Real native public-board health requests, with synthetic pre-provisioned sessions.
use super::*;
use crate::daemon::{test_events, AutoStopSetting, ServeOptions};
use crate::operator_auth::{Auth, BoardUser, PublicRealm};
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

// Isolate the real daemon's other detached workers from the rest of the suite.
#[test]
fn native_public_health_is_bound_to_the_requesting_session() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "ui::hosted_health_session_test::native_public_health_child",
            "--exact", "--ignored", "--nocapture", "--test-threads=1",
        ])
        .output()
        .expect("native health probe did not start");
    assert!(output.status.success(), "native health probe failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
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
    let realm = PublicRealm { host: host.into(), issuer: "http://health-issuer.localhost".into(), company: "health-fixture".into() };
    crate::board_identity::write_config(&state, &crate::board_identity::Config {
        host: realm.host.clone(), issuer: realm.issuer.clone(), company: realm.company.clone(),
    }).unwrap();
    let now = crate::issue::time::now_epoch();
    let mut auth = Auth::load(&state);
    let user = |sub: &str| BoardUser { sub: sub.into(), email: format!("{sub}@example.test"), name: sub.into(), role: "member".into(), handle: sub.into() };
    let a = auth.open_public_bound(user("health-a"), "health-jti-a", now + 60, "fixture", realm.clone(), now).unwrap().unwrap();
    let b = auth.open_public_bound(user("health-b"), "health-jti-b", now + 60, "fixture", realm.clone(), now).unwrap().unwrap();
    drop(auth);

    let stop = Arc::new(AtomicBool::new(false));
    let events = test_events::subscribe(&state);
    let opts = ServeOptions { test_seam: true, stop: Some(stop.clone()), checkup: Some(0), report_router: Some(0), auto_stop: Some(AutoStopSetting::off()), ..ServeOptions::default() };
    opts.provider_env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let daemon_state = state.clone();
    let daemon = std::thread::spawn(move || crate::daemon::serve_with(&daemon_state, opts).unwrap());
    events.wait_for(test_events::Event::AssistNoticesDrained, 1, Duration::from_secs(30)).expect("daemon did not start");
    let (startup, ready) = std::sync::mpsc::channel();
    let board_opts = ServeOpts {
        host: "127.0.0.1".into(), port: PORT, stop: Some(stop.clone()), startup: Some(startup),
        allow_hosts: vec![host.into()], public: Some(PublicBoard {
            host: realm.host.clone(), issuer: realm.issuer.clone(), company: realm.company.clone(),
            authorize_url: format!("{}/authorize", realm.issuer),
        }), ..ServeOpts::default()
    };
    let board_state = state.clone();
    let board_pm = pm.clone();
    let board = std::thread::spawn(move || crate::ui::serve(&board_state, &board_pm, &board_opts).unwrap());
    ready.recv_timeout(Duration::from_secs(30)).expect("board did not answer").expect("board could not bind");
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(20))).build().into();
    let request = |token: Option<&str>, expected: Option<&str>| {
        let mut call = agent.get(format!("http://127.0.0.1:{PORT}/api/health")).header("Host", host);
        if let Some(token) = token { call = call.header("Cookie", format!("__Host-aos-board-session={token}")); }
        if let Some(expected) = expected { call = call.header("X-Cadence-Expected-Session", expected); }
        let mut response = call.call().expect("health request failed");
        let status = response.status().as_u16();
        let proof = response.headers().get("X-Cadence-Verified-Session").map(|v| v.to_str().unwrap().to_owned());
        let body: Value = response.body_mut().read_json().unwrap();
        (status, proof, body)
    };
    let current = request(Some(&b.token), Some(&b.session.id));
    assert_eq!(current.0, 200);
    assert_eq!(current.1.as_deref(), Some(b.session.id.as_str()));
    assert!(current.2.get("issues").is_some(), "admitted health has counts");
    let mismatch = request(Some(&b.token), Some(&a.session.id));
    assert_eq!(mismatch.0, 409);
    assert!(mismatch.1.is_none() && mismatch.2.get("issues").is_none(), "another tab's counts must be withheld");
    let ambiguous = request(Some(&b.token), Some(&format!("{}, {}", b.session.id, a.session.id)));
    assert_eq!(ambiguous.0, 409);
    assert!(ambiguous.1.is_none() && ambiguous.2.get("issues").is_none());
    let legacy = request(Some(&b.token), None);
    assert_eq!(legacy.0, 200);
    assert!(legacy.1.is_none() && legacy.2.get("issues").is_some());
    let anonymous = request(None, None);
    assert_eq!(anonymous.0, 200);
    assert!(anonymous.1.is_none() && anonymous.2.get("issues").is_none());
    let missing = request(None, Some(&b.session.id));
    assert_eq!(missing.0, 401);
    assert!(missing.1.is_none() && missing.2.get("issues").is_none());
    stop.store(true, Ordering::SeqCst);
    board.join().unwrap();
    daemon.join().unwrap();
}
