//! CAD-1331 independent acceptance: a verified hosted member cannot change
//! the organization's locale over the real board HTTP peer, and a refused
//! write leaves the persisted organization preference untouched.
#![cfg(feature = "test-seam")]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cadence_agent::{
    board_identity, client, daemon,
    test_seam::{Asserted, Seam, AS_HEADER, TOKEN_HEADER},
    ui,
};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const HOST_BASE: &str = "cad1331.board.localhost";
const COMPANY: &str = "cad1331-company";
const KEY_SEED: [u8; 32] = [133; 32];

struct Fixture {
    state: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    port: u16,
    public_host: String,
    issuer: String,
    token: String,
}

impl Fixture {
    fn new() -> Self {
        let state = tempfile::Builder::new().prefix("c1331").tempdir().unwrap();
        let state_dir = state.path().to_path_buf();
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();

        let platform = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", platform.server_addr());
        let key = Ed25519KeyPair::from_seed_unchecked(&KEY_SEED).unwrap();
        let jwks = json!({"keys": [{
            "kty": "OKP", "crv": "Ed25519", "kid": "cad1331",
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

        let public_port = (3110..=3199)
            .find(|port| std::net::TcpListener::bind(("127.0.0.1", *port)).is_ok())
            .expect("no free hosted-board test port in 3110..3199");
        let public_host = format!("{HOST_BASE}:{public_port}");
        board_identity::write_config(
            &state_dir,
            &board_identity::Config {
                host: public_host.clone(),
                issuer: issuer.clone(),
                company: COMPANY.into(),
            },
        )
        .unwrap();

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
            let _ = daemon::serve_with(&daemon_dir, daemon_opts);
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

        let (startup, ready) = std::sync::mpsc::channel();
        let board_dir = state_dir.clone();
        let board_pm = state_dir.join("no-pm");
        let opts = ui::ServeOpts {
            host: "127.0.0.1".into(),
            port: public_port,
            stop: Some(stop.clone()),
            startup: Some(startup),
            test_seam: true,
            public: Some(ui::PublicBoard {
                host: public_host.clone(),
                issuer: issuer.clone(),
                company: COMPANY.into(),
                authorize_url: "http://platform.invalid/authorize".into(),
            }),
            allow_hosts: vec![public_host.clone()],
            allow_origins: vec![format!("http://{public_host}")],
            ..Default::default()
        };
        threads.push(std::thread::spawn(move || {
            let _ = ui::serve(&board_dir, &board_pm, &opts);
        }));
        ready
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();

        Self {
            state,
            stop,
            threads,
            port: public_port,
            public_host,
            issuer,
            token,
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
            part(json!({"alg":"EdDSA", "typ":"JWT", "kid":"cad1331"})),
            part(json!({
                "iss": self.issuer, "aud": self.public_host,
                "sub":"cad1331-member", "email":"member@example.test",
                "name":"CAD-1331 Member", "company":COMPANY, "role":"member",
                "iat":now, "exp":now + 30, "jti":"cad1331-member-session"
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
fn verified_member_cannot_change_organization_locale_over_http() {
    let fixture = Fixture::new();
    let preferences = fixture
        .state
        .path()
        .join("operator/locale-preferences.json");
    let original = br#"{"organizations":{"cad1331-company":"zh-TW"},"users":{}}"#;
    cadence_agent::operator_auth::write_private(
        fixture.state.path(),
        "locale-preferences.json",
        original,
    )
    .unwrap();

    let cookie = fixture.member_cookie();
    let (status, _, before) = fixture.request("GET", "/api/locale", Some(&cookie), None);
    assert_eq!(status, 200, "verified member locale read: {before}");
    assert_eq!(before["organization_locale"], "zh-TW");

    let (status, _, refusal) = fixture.request(
        "POST",
        "/api/locale",
        Some(&cookie),
        Some(r#"{"scope":"organization","locale":"en"}"#),
    );
    assert_eq!(
        status, 403,
        "member organization locale write was not refused: {refusal}"
    );
    assert_eq!(
        refusal["check"], "member_role",
        "refusal was not the role guard: {refusal}"
    );
    assert_eq!(
        std::fs::read(&preferences).unwrap(),
        original,
        "refusal mutated persisted organization preference"
    );

    let (status, _, self_update) = fixture.request(
        "POST",
        "/api/locale",
        Some(&cookie),
        Some(r#"{"scope":"user","locale":"en"}"#),
    );
    assert_eq!(
        status, 200,
        "member could not set own locale: {self_update}"
    );
    assert_eq!(self_update["user_locale"], "en");
    assert_eq!(self_update["organization_locale"], "zh-TW");

    let (status, _, after) = fixture.request("GET", "/api/locale", Some(&cookie), None);
    assert_eq!(status, 200, "locale read after refusal: {after}");
    assert_eq!(
        after["organization_locale"], "zh-TW",
        "member changed the organization preference"
    );
    assert_eq!(after["user_locale"], "en");
    let persisted: Value = serde_json::from_slice(&std::fs::read(&preferences).unwrap()).unwrap();
    assert_eq!(persisted["organizations"][COMPANY], "zh-TW");
    assert_eq!(persisted["users"][COMPANY]["cad1331-member"], "en");
}
