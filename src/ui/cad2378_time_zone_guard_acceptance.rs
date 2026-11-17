//! CAD-2378 independent refusal check for the time-zone write guards (authored
//! from the ticket and the public surfaces, separate from the implementer):
//! 1. `POST /api/time-zone` scope `company`: a signed `owner` session
//!    succeeds; a `member` session, an unauthenticated caller and an invalid
//!    zone are refused, and the stored company zone never changes on refusal.
//! 2. A member's own display zone succeeds and moves neither the company zone
//!    nor another member's preference; an invalid zone is refused for it too.
//! 3. Daemon RPC `company_zone_set` refuses an agent and an unproven caller
//!    and writes nothing; the operator succeeds.

#![cfg(all(test, feature = "test-seam"))]

use super::*;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tiny_http::{Header, Response, Server};

use crate::test_seam::{scoped, Asserted};

const ORG: &str = "cad2378-synthetic-org";
const KID: &str = "cad2378-synthetic-key";
const SEED_ZONE: &str = "Asia/Tokyo";

struct Rig {
    _dir: tempfile::TempDir,
    state: PathBuf,
    board_port: u16,
    public_host: String,
    issuer: String,
    key: Ed25519KeyPair,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn start() -> Rig {
    block_sigchld();
    let dir = tempfile::Builder::new()
        .prefix("c2378-guard")
        .tempdir()
        .unwrap();
    let state = dir.path().to_path_buf();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    let pm = state.join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    std::fs::set_permissions(&pm, std::fs::Permissions::from_mode(0o700)).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();

    let key = Ed25519KeyPair::from_seed_unchecked(&[232; 32]).unwrap();
    let public_key = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let platform = (3110..=3199)
        .find_map(|port| Server::http(("127.0.0.1", port)).ok())
        .expect("no free synthetic platform port in 3110..=3199");
    let platform_port = match platform.server_addr() {
        tiny_http::ListenAddr::IP(addr) => addr.port(),
        _ => panic!("synthetic platform did not bind TCP"),
    };
    let public_host = format!("localhost:{platform_port}");
    let issuer = format!("http://{public_host}");
    let platform_stop = stop.clone();
    threads.push(std::thread::spawn(move || {
        while !platform_stop.load(Ordering::SeqCst) {
            if let Ok(Some(request)) = platform.recv_timeout(Duration::from_millis(100)) {
                let path = request.url().split('?').next().unwrap_or("");
                let (status_code, body) = if path == "/.well-known/agenticos-board-jwks.json" {
                    (
                        200,
                        json!({"keys":[{"kty":"OKP","crv":"Ed25519","kid":KID,"x":public_key}]}),
                    )
                } else {
                    (404, json!({"ok": false, "error": {"code": "not_found"}}))
                };
                let response = Response::from_string(body.to_string())
                    .with_status_code(status_code)
                    .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                let _ = request.respond(response);
            }
        }
    }));

    let opts = crate::daemon::ServeOptions {
        test_seam: true,
        stop: Some(stop.clone()),
        provider_deployments: Some(
            crate::platform::deployments::DeploymentMetadata::parse(
                br#"{"schema":1,"providers":[]}"#,
            )
            .expect("isolated deployment metadata"),
        ),
        ..crate::daemon::ServeOptions::default()
    };
    let daemon_state = state.clone();
    threads.push(std::thread::spawn(move || {
        crate::daemon::serve_with(&daemon_state, opts).unwrap()
    }));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !state.join("cadence.sock").exists() || crate::test_seam::Seam::token_at(&state).is_none()
    {
        assert!(Instant::now() < deadline, "synthetic daemon did not start");
        std::thread::sleep(Duration::from_millis(50));
    }

    let mut board_port = 0;
    for port in 3110..=3199 {
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
        let (board_state, board_pm) = (state.clone(), pm.clone());
        let thread =
            std::thread::spawn(move || drop(crate::ui::serve(&board_state, &board_pm, &board)));
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
    assert!(board_port > 0, "synthetic board could not bind");
    Rig {
        _dir: dir,
        state,
        board_port,
        public_host,
        issuer,
        key,
        stop,
        threads,
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .build(),
    )
}

fn post(
    rig: &Rig,
    path: &str,
    cookie: Option<&str>,
    body: &Value,
) -> ureq::http::Response<ureq::Body> {
    let url = format!("http://127.0.0.1:{}{path}", rig.board_port);
    let mut request = agent()
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("X-Cadence-Board", "1");
    if let Some(cookie) = cookie {
        request = request.header("Cookie", cookie);
    }
    request.send_json(body).expect("real board POST failed")
}

fn get(rig: &Rig, path: &str, cookie: Option<&str>) -> ureq::http::Response<ureq::Body> {
    let url = format!("http://127.0.0.1:{}{path}", rig.board_port);
    let mut request = agent().get(&url).header("Host", &rig.public_host);
    if let Some(cookie) = cookie {
        request = request.header("Cookie", cookie);
    }
    request.call().expect("real board GET failed")
}

fn status_and_json(mut response: ureq::http::Response<ureq::Body>) -> (u16, Value) {
    let status = response.status().as_u16();
    let raw = response.body_mut().read_to_string().unwrap_or_default();
    let parsed = serde_json::from_str(&raw).unwrap_or_else(|_| json!({"raw": raw}));
    (status, parsed)
}

fn session(rig: &Rig, sub: &str, role: &str) -> String {
    let now = crate::issue::time::now_epoch();
    let header = json!({"alg":"EdDSA","typ":"JWT","kid":KID});
    let claims = json!({
        "iss": rig.issuer,
        "aud": rig.public_host,
        "company": ORG,
        "sub": sub,
        "email": format!("{sub}@example.test"),
        "name": sub,
        "role": role,
        "iat": now,
        "exp": now + 60,
        "jti": format!("cad2378-{sub}-{}", uuid::Uuid::new_v4()),
    });
    let signed = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap()),
    );
    let assertion = format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(rig.key.sign(signed.as_bytes()).as_ref()),
    );
    let response = post(
        rig,
        "/__platform/session",
        None,
        &json!({"assertion": assertion}),
    );
    assert_eq!(
        response.status().as_u16(),
        200,
        "signed fixture SSO was refused"
    );
    response
        .headers()
        .get("set-cookie")
        .expect("public session has no cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

fn rpc_as(rig: &Rig, who: Asserted, method: &str, params: Value) -> Result<Value> {
    scoped(who, || crate::client::rpc(&rig.state, method, params))
}

fn company_zone(rig: &Rig) -> String {
    let got = rpc_as(rig, Asserted::Operator, "company_zone_get", json!({})).unwrap();
    got["timezone"].as_str().expect("company timezone").into()
}

fn view(rig: &Rig, cookie: &str) -> Value {
    let (status, body) = status_and_json(get(rig, "/api/time-zone", Some(cookie)));
    assert_eq!(status, 200, "GET /api/time-zone failed: {body}");
    body
}

fn refused(status: u16, context: &str, body: &Value) {
    assert!(
        matches!(status, 401 | 403),
        "{context} must refuse with 401/403, got {status}: {body}"
    );
}

/// Seed the company zone to a known non-default value through the operator.
fn seed(rig: &Rig) {
    rpc_as(
        rig,
        Asserted::Operator,
        "company_zone_set",
        json!({"timezone": SEED_ZONE}),
    )
    .expect("operator seeds the company zone");
    assert_eq!(company_zone(rig), SEED_ZONE);
}

#[test]
fn cad2378_http_company_zone_write_is_owner_only_and_refusal_writes_nothing() {
    let rig = start();
    let owner = session(&rig, "owner", "owner");
    let member = session(&rig, "member", "member");
    seed(&rig);

    let attempt = json!({"scope": "company", "timezone": "Europe/Paris"});
    for (label, cookie) in [("unauthenticated", None), ("member", Some(member.as_str()))] {
        let (status, body) = status_and_json(post(&rig, "/api/time-zone", cookie, &attempt));
        refused(status, &format!("{label} company zone write"), &body);
        assert_eq!(company_zone(&rig), SEED_ZONE, "{label} refusal wrote");
    }
    // The member view reports they cannot set it.
    assert_eq!(view(&rig, &member)["can_set_company"], false);

    // Positive control.
    let (status, body) = status_and_json(post(&rig, "/api/time-zone", Some(&owner), &attempt));
    assert_eq!(status, 200, "owner company zone write failed: {body}");
    assert_eq!(company_zone(&rig), "Europe/Paris");
}

#[test]
fn cad2378_member_display_zone_is_own_and_does_not_touch_company_or_peers() {
    let rig = start();
    let member = session(&rig, "member", "member");
    let peer = session(&rig, "peer", "member");
    seed(&rig);

    let (status, body) = status_and_json(post(
        &rig,
        "/api/time-zone",
        Some(&member),
        &json!({"scope": "user", "timezone": "America/New_York"}),
    ));
    assert_eq!(status, 200, "member own display zone write failed: {body}");
    assert_eq!(view(&rig, &member)["display_timezone"], "America/New_York");
    assert_eq!(company_zone(&rig), SEED_ZONE, "display write moved company");
    let peer_view = view(&rig, &peer);
    assert!(
        peer_view["display_timezone"].is_null(),
        "another member's preference changed: {peer_view}"
    );
    assert_eq!(peer_view["effective_display_timezone"], SEED_ZONE);
}

#[test]
fn cad2378_invalid_zone_is_refused_for_both_scopes_and_writes_nothing() {
    let rig = start();
    let owner = session(&rig, "owner", "owner");
    let member = session(&rig, "member", "member");
    seed(&rig);
    let (status, body) = status_and_json(post(
        &rig,
        "/api/time-zone",
        Some(&member),
        &json!({"scope": "user", "timezone": "Europe/London"}),
    ));
    assert_eq!(status, 200, "member control write failed: {body}");

    for bad in ["Mars/Olympus", "not a zone", "UTC+8", ""] {
        for (who, cookie, scope) in [("owner", &owner, "company"), ("member", &member, "user")] {
            let (status, body) = status_and_json(post(
                &rig,
                "/api/time-zone",
                Some(cookie),
                &json!({"scope": scope, "timezone": bad}),
            ));
            assert_eq!(
                status, 400,
                "{who} {scope} invalid zone {bad:?} must be 400: {body}"
            );
        }
        assert_eq!(company_zone(&rig), SEED_ZONE, "{bad:?} changed company");
        assert_eq!(
            view(&rig, &member)["display_timezone"],
            "Europe/London",
            "{bad:?} changed display zone"
        );
    }
}

#[test]
fn cad2378_rpc_company_zone_set_refuses_non_operator_callers_and_writes_nothing() {
    let rig = start();
    seed(&rig);
    let attempt = json!({"timezone": "Europe/Paris"});
    for caller in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        assert!(
            rpc_as(&rig, caller.clone(), "company_zone_set", attempt.clone()).is_err(),
            "company_zone_set must refuse {caller:?}"
        );
        assert_eq!(company_zone(&rig), SEED_ZONE, "{caller:?} refusal wrote");
    }
    // An invalid zone is refused even for the operator, and writes nothing.
    assert!(rpc_as(
        &rig,
        Asserted::Operator,
        "company_zone_set",
        json!({"timezone": "Mars/Olympus"})
    )
    .is_err());
    assert_eq!(company_zone(&rig), SEED_ZONE);
    // Positive control.
    rpc_as(&rig, Asserted::Operator, "company_zone_set", attempt).unwrap();
    assert_eq!(company_zone(&rig), "Europe/Paris");
}
