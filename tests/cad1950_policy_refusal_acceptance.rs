//! CAD-1950 independent acceptance: a verified hosted **member** cannot change
//! a project's access policy over the real board HTTP peer, and the refusal
//! leaves the recorded `project.yaml` untouched.
//!
//! The read is the control: the same member may read the policy of a
//! workspace-visible project, so the refusal below is the operator-only role
//! guard and not a 404 or an unreachable route.
#![cfg(feature = "test-seam")]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cadence_agent::{
    client, daemon,
    test_seam::{Asserted, Seam, AS_HEADER, TOKEN_HEADER},
    ui,
};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const HOST_BASE: &str = "cad1950.board.localhost";
const COMPANY: &str = "cad1950-company";
const KEY_SEED: [u8; 32] = [150; 32];

/// The project the member tries to change, and its recorded policy. Visible,
/// so the member's read is allowed and the write's refusal is the role guard.
const PROJECT: &str = "pay";
const RECORDED: &str =
    "key: pay\nprefix: PAY\npolicy:\n  version: 1\n  visibility: workspace_visible\n";

struct Fixture {
    state: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    port: u16,
    public_host: String,
    issuer: String,
    token: String,
    policy_path: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let state = tempfile::Builder::new().prefix("c1950").tempdir().unwrap();
        let state_dir = state.path().to_path_buf();
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();

        // The tracker the board serves: a real PM skeleton (`Pm::at` refuses a
        // bare directory) holding one project with a recorded policy.
        let pm_dir = state_dir.join("pm");
        cadence_agent::issue::Pm::init(&pm_dir).unwrap();
        let project_dir = pm_dir.join(PROJECT);
        std::fs::create_dir_all(&project_dir).unwrap();
        let policy_path = project_dir.join("project.yaml");
        std::fs::write(&policy_path, RECORDED).unwrap();

        let platform = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", platform.server_addr());
        let key = Ed25519KeyPair::from_seed_unchecked(&KEY_SEED).unwrap();
        let jwks = json!({"keys": [{
            "kty": "OKP", "crv": "Ed25519", "kid": "cad1950",
            "x": URL_SAFE_NO_PAD.encode(key.public_key().as_ref())
        }]});
        let platform_stop = stop.clone();
        threads.push(std::thread::spawn(move || {
            while !platform_stop.load(Ordering::SeqCst) {
                let Ok(Some(request)) = platform.recv_timeout(Duration::from_millis(100)) else {
                    continue;
                };
                let response = tiny_http::Response::from_string(jwks.to_string()).with_header(
                    tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                );
                let _ = request.respond(response);
            }
        }));

        let daemon_dir = state_dir.clone();
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", state_dir.join("no-pm").to_str().unwrap());
        let daemon_opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(stop.clone()),
            test_seam: true,
            auto_stop: Some(daemon::AutoStopSetting::off()),
            ..Default::default()
        };
        let daemon_thread = std::thread::spawn(move || {
            // A daemon that exits here is the failure the wait below reports;
            // name its reason rather than leaving a bare timeout.
            if let Err(error) = daemon::serve_with(&daemon_dir, daemon_opts) {
                eprintln!("test daemon exited: {error}");
            }
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&state_dir, "health", json!({}), Duration::from_secs(1)).is_err()
        {
            assert!(
                !daemon_thread.is_finished() && std::time::Instant::now() < deadline,
                "test daemon did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        threads.push(daemon_thread);
        let token = Seam::token_at(&state_dir).expect("daemon test-seam token");

        let board_dir = state_dir.clone();
        let board_pm = pm_dir.clone();
        let (public_port, public_host, board) = (3110..=3199)
            .find_map(|port| {
                let host = format!("{HOST_BASE}:{port}");
                let (startup, ready) = std::sync::mpsc::channel();
                let opts = ui::ServeOpts {
                    host: "127.0.0.1".into(),
                    port,
                    stop: Some(stop.clone()),
                    startup: Some(startup),
                    test_seam: true,
                    public: Some(ui::PublicBoard {
                        host: host.clone(),
                        issuer: issuer.clone(),
                        company: COMPANY.into(),
                        authorize_url: "http://platform.invalid/authorize".into(),
                    }),
                    allow_hosts: vec![host.clone()],
                    allow_origins: vec![format!("http://{host}")],
                    ..Default::default()
                };
                let (dir, pm) = (board_dir.clone(), board_pm.clone());
                let board = std::thread::spawn(move || {
                    let _ = ui::serve(&dir, &pm, &opts);
                });
                match ready.recv_timeout(Duration::from_secs(10)) {
                    Ok(Ok(())) => Some((port, host, board)),
                    Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                        let _ = board.join();
                        None
                    }
                    Ok(Err(kind)) => panic!("board startup failed on port {port}: {kind:?}"),
                    Err(error) => {
                        panic!("board startup gave no bind result on port {port}: {error}")
                    }
                }
            })
            .expect("no free hosted-board test port in 3110..3199");
        threads.push(board);

        Self {
            state,
            stop,
            threads,
            port: public_port,
            public_host,
            issuer,
            token,
            policy_path,
        }
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: Option<&str>,
    ) -> (u16, Vec<String>, Value) {
        use std::io::Write;
        let body = body.unwrap_or("");
        let cookie = cookie
            .map(|value| format!("Cookie: {value}\r\n"))
            .unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.0\r\nHost: {host}\r\nX-Cadence-Board: 1\r\nOrigin: http://{host}\r\nSec-Fetch-Site: same-origin\r\nContent-Type: application/json\r\n{as_header}: operator\r\n{token_header}: {token}\r\n{cookie}Content-Length: {}\r\n\r\n{body}",
            body.len(), host = self.public_host, as_header = AS_HEADER,
            token_header = TOKEN_HEADER, token = self.token,
        );
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response
            .split_once("\r\n\r\n")
            .expect("HTTP response headers");
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        let headers = head.lines().skip(1).map(str::to_owned).collect();
        (
            status,
            headers,
            serde_json::from_str(body).unwrap_or(Value::Null),
        )
    }

    fn member_cookie(&self) -> String {
        let b64 = URL_SAFE_NO_PAD;
        let key = Ed25519KeyPair::from_seed_unchecked(&KEY_SEED).unwrap();
        let now = cadence_agent::issue::time::now_epoch();
        let part = |value: Value| b64.encode(serde_json::to_vec(&value).unwrap());
        let signed = format!(
            "{}.{}",
            part(json!({"alg":"EdDSA", "typ":"JWT", "kid":"cad1950"})),
            part(json!({
                "iss": self.issuer, "aud": self.public_host,
                "sub":"cad1950-member", "email":"member@example.test",
                "name":"CAD-1950 Member", "company":COMPANY, "role":"member",
                "iat":now, "exp":now + 30, "jti":"cad1950-member-session"
            }))
        );
        let assertion = format!(
            "{signed}.{}",
            b64.encode(key.sign(signed.as_bytes()).as_ref())
        );
        let (status, headers, _) = self.request(
            "POST",
            "/__platform/session",
            None,
            Some(&json!({"assertion": assertion}).to_string()),
        );
        assert_eq!(status, 200, "verified member sign-in failed");
        let cookie = headers
            .iter()
            .find_map(|header| {
                header
                    .to_ascii_lowercase()
                    .starts_with("set-cookie:")
                    .then(|| {
                        header
                            .split_once(':')
                            .unwrap()
                            .1
                            .trim()
                            .split(';')
                            .next()
                            .unwrap()
                            .to_owned()
                    })
            })
            .expect("public session cookie");
        assert!(
            cookie.starts_with("__Host-aos-board-session="),
            "unexpected cookie name: {}",
            cookie.split('=').next().unwrap_or("missing")
        );
        let token = cookie.split_once('=').unwrap().1;
        let checked = cadence_agent::test_seam::scoped(Asserted::Operator, || {
            client::rpc(
                self.state.path(),
                "board_session_check",
                json!({"token": token}),
            )
        })
        .unwrap();
        assert_eq!(checked["valid"], true, "public cookie not valid at daemon");
        cookie
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for thread in self.threads.drain(..).rev() {
            let _ = thread.join();
        }
    }
}

#[test]
fn verified_member_cannot_change_a_project_policy_over_http() {
    let fixture = Fixture::new();
    let cookie = fixture.member_cookie();

    // Control: the same member may read this project's policy, so the refusal
    // below is the role guard and not an unreachable route.
    let (status, _, read) = fixture.request(
        "GET",
        &format!("/api/projects/{PROJECT}/policy"),
        Some(&cookie),
        None,
    );
    assert_eq!(status, 200, "member policy read: {read}");
    assert_eq!(read["recorded"], true, "policy read: {read}");
    assert_eq!(read["policy"]["visibility"], "workspace_visible");

    let (status, _, refusal) = fixture.request(
        "POST",
        &format!("/api/projects/{PROJECT}/policy"),
        Some(&cookie),
        Some(r#"{"visibility":"private","members":["users/member"]}"#),
    );
    assert_eq!(
        status, 403,
        "member policy write was not refused: {refusal}"
    );
    assert_eq!(
        refusal["check"], "member_role",
        "refusal was not the role guard: {refusal}"
    );
    assert_eq!(
        std::fs::read(&fixture.policy_path).unwrap(),
        RECORDED.as_bytes(),
        "a refused policy write changed project.yaml"
    );

    // The refusal is the write's alone: the recorded policy is unchanged.
    let (status, _, after) = fixture.request(
        "GET",
        &format!("/api/projects/{PROJECT}/policy"),
        Some(&cookie),
        None,
    );
    assert_eq!(status, 200, "policy read after refusal: {after}");
    assert_eq!(after["policy"]["visibility"], "workspace_visible");
    assert_eq!(after["policy"]["version"], 1);
}
