//! CAD-1703 acceptance: `My work` refuses foreign selectors over the real
//! HTTP path. A real daemon, a real seam-armed loopback board, and a real
//! operator session minted through `operator_link_mint`/`operator_session_open`.
//! No re-implemented guard: every case reaches `serve`'s dispatch and
//! `my_work::get` / `get_issue`.

#![cfg(feature = "test-seam")]

use cadence_agent::daemon::ServeOptions;
use cadence_agent::issue::{write as issue_write, Pm};
use cadence_agent::test_seam::Seam;
use serde_json::{json, Value};
use std::sync::{atomic::AtomicBool, Arc};
use std::time::{Duration, Instant};

const PROJECT: &str = "fixture";
const OWNER: &str = "operator";
const OTHER: &str = "other-human";

/// Stops and joins the board and daemon threads on drop so a panic
/// cannot leak a listener into the next test.
struct Threads(Arc<AtomicBool>, Vec<std::thread::JoinHandle<()>>);

impl Drop for Threads {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        for thread in self.1.drain(..).rev() {
            let _ = thread.join();
        }
    }
}

fn http(
    port: u16,
    path: &str,
    seam: Option<(&str, &str)>,
    session: Option<(&str, &str)>,
) -> (u16, Value) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let host = format!("cadence-{port}.localhost:{port}");
    let mut req = agent
        .get(format!("http://127.0.0.1:{port}{path}"))
        .header("Host", &host)
        .header("X-Cadence-Board", "1")
        .header("Origin", format!("http://{host}"));
    if let Some((who, token)) = seam {
        req = req
            .header(cadence_agent::test_seam::AS_HEADER, who)
            .header(cadence_agent::test_seam::TOKEN_HEADER, token);
    }
    if let Some((token, key)) = session {
        req = req
            .header("Cookie", format!("cadence_operator_{port}={token}"))
            .header("X-Cadence-Session", key);
    }
    let mut response = req.call().unwrap();
    let status = response.status().as_u16();
    let body = response.body_mut().read_json().unwrap_or(Value::Null);
    (status, body)
}

#[test]
fn cad1703_my_work_refuses_foreign_selectors_over_http() {
    let dir = tempfile::Builder::new().prefix("c1703").tempdir().unwrap();
    let state = dir.path().join("s");
    let pm_dir = dir.path().join("pm");
    std::fs::create_dir_all(&state).unwrap();
    let pm = Pm::init(&pm_dir).unwrap();
    issue_write::project_add(&pm, PROJECT, "FIX", &[], &[], &[], None).unwrap();
    let mine = issue_write::new_issue(
        &pm,
        &pm.dir,
        Some(PROJECT),
        "Owner's own work",
        None,
        None,
        &[],
        Some(OWNER),
        None,
        &[],
        None,
        None,
        "operator",
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let theirs = issue_write::new_issue(
        &pm,
        &pm.dir,
        Some(PROJECT),
        "Other person's confidential work",
        None,
        None,
        &[],
        Some(OTHER),
        None,
        &[],
        None,
        None,
        "operator",
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Real daemon: owns the socket and mints the seam token.
    let stop = Arc::new(AtomicBool::new(false));
    let opts = ServeOptions {
        test_seam: true,
        stop: Some(stop.clone()),
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let daemon_state = state.clone();
    let mut threads = Threads(stop.clone(), Vec::new());
    threads.1.push(std::thread::spawn(move || {
        cadence_agent::daemon::serve_with(&daemon_state, opts).unwrap()
    }));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
        assert!(Instant::now() < deadline, "daemon never started");
        std::thread::sleep(Duration::from_millis(50));
    }
    let seam = Seam::token_at(&state).unwrap();

    // Real operator session through the native link mint + open.
    cadence_agent::operator_auth::ensure_secret(&state).unwrap();
    let secret = cadence_agent::operator_auth::read_secret(&state).unwrap();
    let daemon = |method: &str, params: Value| {
        cadence_agent::test_seam::scoped(cadence_agent::test_seam::Asserted::Operator, || {
            cadence_agent::client::rpc(&state, method, params)
        })
        .unwrap_or_else(|e| panic!("{method}: {e}"))
    };
    let nonce = daemon(
        "operator_link_mint",
        json!({"secret": secret, "origin": "loopback"}),
    )["nonce"]
        .clone();
    let opened = daemon(
        "operator_session_open",
        json!({"nonce": nonce, "origin": "loopback"}),
    );
    let token = opened["token"].as_str().unwrap().to_string();
    let key = opened["key"].as_str().unwrap().to_string();

    // Seam-armed loopback board on a free 3110-3199 port.
    let first = (std::process::id() % 80) as u16;
    let port;
    let mut offset = 0u16;
    let startup_deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < startup_deadline, "board startup deadline");
        let candidate = 3110 + (first + offset) % 80;
        let (startup, ready) = std::sync::mpsc::channel();
        let board = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port: candidate,
            stop: Some(stop.clone()),
            startup: Some(startup),
            test_seam: true,
            ..Default::default()
        };
        let (s, p) = (state.clone(), pm_dir.clone());
        let thread = std::thread::spawn(move || drop(cadence_agent::ui::serve(&s, &p, &board)));
        let remaining = startup_deadline.saturating_duration_since(Instant::now());
        match ready.recv_timeout(remaining) {
            Ok(Ok(())) => {
                threads.1.push(thread);
                port = candidate;
                break;
            }
            Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                assert!(thread.join().is_ok());
                offset += 1;
                assert!(offset < 80, "all fixture board candidates busy");
            }
            other => {
                threads.1.push(thread);
                panic!("fixture board startup failed on port {candidate}: {other:?}");
            }
        }
    }

    let session = Some((token.as_str(), key.as_str()));
    let operator = Some(("operator", seam.as_str()));

    // POSITIVE CONTROL: the real operator session reads its own work, and
    // only its own. Without this every refusal below could be a dead route.
    let (status, body) = http(port, "/api/my-work", operator, session);
    assert_eq!(status, 200, "positive control must read: {body}");
    let ids: Vec<&str> = body["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&mine.as_str()), "own issue missing: {body}");
    assert!(
        !ids.contains(&theirs.as_str()),
        "foreign issue leaked: {body}"
    );

    // 1. `owner=<other>` and other forged selectors are refused with 400
    //    and the body carries no foreign issue id.
    for path in [
        format!("/api/my-work?owner={OTHER}"),
        format!("/api/my-work?subject={OTHER}"),
        "/api/my-work?tab=active&tab=done".to_string(),
        "/api/my-work?cursor=50".to_string(),
        "/api/my-work?cursor=0.0.1.cad.-1".to_string(),
    ] {
        let (status, body) = http(port, &path, operator, session);
        assert_eq!(status, 400, "{path} must be refused: {body}");
        assert!(
            !body.to_string().contains(&theirs),
            "{path} refusal carried a foreign issue id: {body}"
        );
    }

    // 2. Issue-detail: another person's issue is refused as not-assigned,
    //    with no provenance.
    let (status, body) = http(
        port,
        &format!("/api/my-work/issues/{theirs}"),
        operator,
        session,
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["assigned"], json!(false), "{body}");
    assert_eq!(body["assignment"], Value::Null, "{body}");

    // 3. Positive control for the issue route: own issue is assigned.
    let (status, body) = http(
        port,
        &format!("/api/my-work/issues/{mine}"),
        operator,
        session,
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["assigned"], json!(true), "{body}");

    // 4. No session -> refused (401); the caller is never the operator.
    let (status, body) = http(port, "/api/my-work", None, None);
    assert_eq!(status, 401, "no-session caller must be refused: {body}");
    assert!(!body.to_string().contains(&theirs), "{body}");

    // 5. Agent caller -> 403.
    let (status, body) = http(
        port,
        "/api/my-work",
        Some(("agent:worker", seam.as_str())),
        session,
    );
    assert_eq!(status, 403, "agent caller must be refused: {body}");
    assert!(!body.to_string().contains(&theirs), "{body}");

    drop(threads);
}
