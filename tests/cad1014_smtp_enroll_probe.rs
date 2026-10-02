//! CAD-1014 reproduction + fix verification for the user-verified bug:
//! "add SMTP connection shows connection management refused or
//! unavailable". Synthetic isolated daemon + board only; no real sends,
//! and the probe asserts the credential never echoes in any response.
//!
//! Root cause found by tracing connectionClient → `POST /api/connections`
//! → `connection_create`: the daemon refuses a first enrollment with the
//! structured code `custody_unprotected` (custody not isolated; needs the
//! operator's recorded `accept_same_uid_risk`), and the board collapsed
//! EVERY daemon failure into one opaque string — so the operator never
//! learned the actual, actionable reason.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, TestDaemon};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

const SECRET: &str = "Zx9Qw8Er7Ty6Ui5Op4As3Df2Gh1Jk0";

fn smtp_body(risk: bool) -> Value {
    let mut p = json!({"provider":"smtp","account":"news","shape":"smtp",
        "host":"localhost","port":465,"tls_mode":"implicit",
        "username":"u","secret":SECRET,"sender":"news@example.com",
        "scopes":["email:send"]});
    if risk {
        p["accept_same_uid_risk"] = json!(true);
    }
    p
}

/// Daemon-level: with the smtp adapter attached (as production `serve`
/// does) the first enrollment without the risk flag refuses
/// `custody_unprotected`; the same body succeeds once it is set.
#[test]
fn cad1014_smtp_enroll_without_risk_refuses_custody() {
    let mut opts = daemon_opts();
    cadence_agent::platform::smtp::attach(&mut opts);
    let w = TestDaemon::start_opts(opts);
    let refused = w.operator_rpc("connection_create", smtp_body(false));
    let text = refused.unwrap_err().to_string();
    assert!(
        text.contains("custody_unprotected") || text.contains("accept-same-uid-risk"),
        "expected the custody gate, got: {text}"
    );
    let ok = w.operator_rpc("connection_create", smtp_body(true));
    assert!(ok.is_ok(), "accepted-risk enrollment refused: {ok:?}");
    assert_eq!(
        ok.unwrap()["connection"]["provider"].as_str(),
        Some("smtp"),
        "enrolled connection is not smtp"
    );
}

/// Board-level: `POST /api/connections` with a shape-`smtp` body and no
/// risk acceptance used to answer the opaque "connection management
/// refused or unavailable". It must now answer 409 with the stable
/// `custody_unprotected` code and an actionable message — and must never
/// echo the secret.
#[test]
fn cad1014_board_smtp_enroll_exposes_custody_code() {
    let root = tempfile::tempdir().unwrap();
    let pm = Pm::init(&root.path().join("pm")).unwrap();
    let mut opts = daemon_opts();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
    cadence_agent::platform::smtp::attach(&mut opts);
    let daemon = TestDaemon::start_opts(opts);
    let stop = Arc::new(AtomicBool::new(false));
    // Reserve an ephemeral port, release, then bind the board on it.
    let port = {
        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        probe.local_addr().unwrap().port()
    };
    let (startup, ready) = std::sync::mpsc::channel();
    let opts = cadence_agent::ui::ServeOpts {
        host: "127.0.0.1".into(),
        port,
        stop: Some(stop.clone()),
        test_seam: cfg!(feature = "test-seam"),
        startup: Some(startup),
        ..Default::default()
    };
    let state = daemon.state.clone();
    let pm_dir = pm.dir.clone();
    let thread = std::thread::spawn(move || cadence_agent::ui::serve(&state, &pm_dir, &opts));
    match ready
        .recv_timeout(Duration::from_secs(10))
        .expect("board startup timed out")
    {
        Ok(()) => {}
        Err(kind) => panic!("board failed to start on {port}: {kind}"),
    }
    let guard = scopeguard(stop, thread);

    let session = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &daemon.state, port);
    let body = smtp_body(false).to_string();
    let (code, _head, response) =
        common::op::raw(port, &session.request("POST", "/api/connections", &body));
    assert!(
        !response.contains(SECRET),
        "credential echoed in HTTP response: {response}"
    );
    assert_eq!(code, 409, "custody refusal should be 409: {response}");
    let parsed: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(
        parsed["code"].as_str(),
        Some("custody_unprotected"),
        "{parsed}"
    );
    assert!(
        parsed["error"].as_str().unwrap_or("").contains("custody"),
        "opaque message returned: {parsed}"
    );
    drop(guard);
}

/// Drop guard that stops the board and joins its thread.
struct BoardGuard(
    Arc<AtomicBool>,
    Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
);
impl Drop for BoardGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
        if let Some(t) = self.1.take() {
            let _ = t.join();
        }
    }
}
fn scopeguard(
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<cadence_agent::Result<()>>,
) -> BoardGuard {
    BoardGuard(stop, Some(thread))
}
