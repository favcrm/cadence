//! CAD-1453 independent boundary acceptance: bounded search covers the whole
//! trusted workspace, and an otherwise valid sign-in loses access immediately
//! when its actor leaves the live roster. Both native RPC and real board HTTP
//! are exercised; only the external platform (JWKS and roster) is a fixture.
#![cfg(feature = "test-seam")]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cadence_agent::{
    board_identity, client, daemon,
    test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER},
    ui,
};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const COMPANY: &str = "cad1453-acceptance-company";
const SUBJECT: &str = "cad1453-current-member";
const TARGET: &str = "cad1453-last-member";
const TARGET_NAME: &str = "Zoë 東京 + &";
const KEY_SEED: [u8; 32] = [145; 32];

struct Fixture {
    state: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    current_member: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    port: u16,
    host: String,
    issuer: String,
    seam_token: String,
}

// All fixture listeners stay in the explicitly reserved acceptance-test range.
fn listener() -> tiny_http::Server {
    (3110..=3199)
        .find_map(|port| tiny_http::Server::http(("127.0.0.1", port)).ok())
        .expect("no free acceptance-test port in 3110..3199")
}

fn port(server: &tiny_http::Server) -> u16 {
    server.server_addr().to_ip().unwrap().port()
}

fn exchange(port: u16, request: &[u8]) -> (u16, Vec<String>, Vec<u8>) {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    stream.write_all(request).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let split = response.windows(4).position(|v| v == b"\r\n\r\n").unwrap();
    let head = std::str::from_utf8(&response[..split]).unwrap();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    let headers = head.lines().skip(1).map(str::to_owned).collect();
    (status, headers, response[split + 4..].to_vec())
}

impl Fixture {
    fn new() -> Self {
        let state = tempfile::Builder::new()
            .prefix("c1453-search")
            .tempdir()
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let current_member = Arc::new(AtomicBool::new(true));
        let mut threads = Vec::new();
        let platform = listener();
        let issuer = format!("http://127.0.0.1:{}", port(&platform));
        let public = listener();
        let public_port = port(&public);
        // localhost resolves without a machine-wide hosts-file mutation. The
        // canonical host points at the roster/HTTP relay, not a fake daemon.
        let host = format!("localhost:{public_port}");
        let board_reservation = listener();
        let board_port = port(&board_reservation);
        let key = Ed25519KeyPair::from_seed_unchecked(&KEY_SEED).unwrap();
        let jwks = json!({"keys": [{
            "kty":"OKP", "crv":"Ed25519", "kid":"cad1453-acceptance",
            "x":URL_SAFE_NO_PAD.encode(key.public_key().as_ref())
        }]});
        let platform_stop = stop.clone();
        threads.push(std::thread::spawn(move || {
            while !platform_stop.load(Ordering::SeqCst) {
                let Ok(Some(request)) = platform.recv_timeout(Duration::from_millis(100)) else {
                    continue;
                };
                let _ = request.respond(
                    tiny_http::Response::from_string(jwks.to_string()).with_header(
                        tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                    ),
                );
            }
        }));
        let proxy_stop = stop.clone();
        let live_member = current_member.clone();
        let roster_issuer = issuer.clone();
        threads.push(std::thread::spawn(move || {
            while !proxy_stop.load(Ordering::SeqCst) {
                let Ok(Some(mut request)) = public.recv_timeout(Duration::from_millis(100)) else {
                    continue;
                };
                if request.url() == "/__platform/threads/roster" {
                    let mut members: Vec<Value> = (0..25).map(|n| json!({
                        "subject_id":format!("cad1453-member-{n:02}"),
                        "name":format!("Workspace Member {n:02}"), "role":"member"
                    })).collect();
                    // Deliberately after the first twenty eligible members.
                    members.push(json!({"subject_id":TARGET,"name":TARGET_NAME,"role":"member"}));
                    if live_member.load(Ordering::SeqCst) {
                        members.insert(0, json!({"subject_id":SUBJECT,"name":"Current Caller Only","role":"member"}));
                    }
                    let roster = json!({"issuer":roster_issuer,"organization_id":COMPANY,"members":members});
                    let _ = request.respond(tiny_http::Response::from_string(roster.to_string())
                        .with_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap()));
                    continue;
                }
                // Relay to an actual ui::serve HTTP peer. The original Host,
                // cookie, origin, and test-seam proof survive unchanged.
                // Each request gets a worker so a board RPC can synchronously
                // fetch the roster from this listener without deadlocking it.
                std::thread::spawn(move || {
                    let mut body = Vec::new();
                    request.as_reader().read_to_end(&mut body).unwrap();
                    let mut wire = format!("{} {} HTTP/1.0\r\n", request.method(), request.url());
                    for header in request.headers() {
                        let name = header.field.as_str().as_str();
                        if !name.eq_ignore_ascii_case("content-length") && !name.eq_ignore_ascii_case("connection") {
                            wire.push_str(&format!("{header}\r\n"));
                        }
                    }
                    wire.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
                    let mut bytes = wire.into_bytes();
                    bytes.extend(body);
                    let (status, headers, body) = exchange(board_port, &bytes);
                    let mut response = tiny_http::Response::from_data(body).with_status_code(status);
                    for header in headers {
                        let (name, value) = header.split_once(':').unwrap();
                        if !name.eq_ignore_ascii_case("content-length") && !name.eq_ignore_ascii_case("connection") {
                            response.add_header(tiny_http::Header::from_bytes(name, value.trim()).unwrap());
                        }
                    }
                    let _ = request.respond(response);
                });
            }
        }));

        board_identity::write_config(
            state.path(),
            &board_identity::Config {
                host: host.clone(),
                issuer: issuer.clone(),
                company: COMPANY.into(),
            },
        )
        .unwrap();
        let daemon_dir = state.path().to_path_buf();
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set(
            "CADENCE_PM_DIR",
            state.path().join("no-pm").to_str().unwrap(),
        );
        let opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(stop.clone()),
            test_seam: true,
            auto_stop: Some(daemon::AutoStopSetting::off()),
            ..Default::default()
        };
        let daemon_thread = std::thread::spawn(move || {
            let _ = daemon::serve_with(&daemon_dir, opts);
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(state.path(), "health", json!({}), Duration::from_secs(1))
            .is_err()
        {
            assert!(
                !daemon_thread.is_finished() && Instant::now() < deadline,
                "acceptance daemon did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        threads.push(daemon_thread);
        let seam_token = Seam::token_at(state.path()).unwrap();
        let (startup, ready) = std::sync::mpsc::channel();
        let board_dir = state.path().to_path_buf();
        let board_pm = state.path().join("no-pm");
        let opts = ui::ServeOpts {
            host: "127.0.0.1".into(),
            port: board_port,
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
        drop(board_reservation);
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
            current_member,
            threads,
            port: public_port,
            host,
            issuer,
            seam_token,
        }
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: Option<Value>,
    ) -> (u16, Vec<String>, Value) {
        let body = body.map(|v| v.to_string()).unwrap_or_default();
        let cookie = cookie
            .map(|v| format!("Cookie: {v}\r\n"))
            .unwrap_or_default();
        let wire = format!(
            "{method} {path} HTTP/1.0\r\nHost: {host}\r\nX-Cadence-Board: 1\r\nOrigin: http://{host}\r\nSec-Fetch-Site: same-origin\r\nContent-Type: application/json\r\n{AS_HEADER}: operator\r\n{TOKEN_HEADER}: {token}\r\n{cookie}Content-Length: {}\r\n\r\n{body}",
            body.len(), host = self.host, token = self.seam_token,
        );
        let (status, headers, body) = exchange(self.port, wire.as_bytes());
        let value = serde_json::from_slice(&body).unwrap_or_else(|_| {
            panic!("non-JSON HTTP {status}: {}", String::from_utf8_lossy(&body))
        });
        (status, headers, value)
    }

    fn member_cookie(&self) -> String {
        let key = Ed25519KeyPair::from_seed_unchecked(&KEY_SEED).unwrap();
        let now = cadence_agent::issue::time::now_epoch();
        let part = |v: Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(&v).unwrap());
        let signed = format!(
            "{}.{}",
            part(json!({"alg":"EdDSA","typ":"JWT","kid":"cad1453-acceptance"})),
            part(json!({"iss":self.issuer,"aud":self.host,"sub":SUBJECT,
                "email":"acceptance@example.test","name":"Current Caller Only",
                "company":COMPANY,"role":"member","iat":now,"exp":now+30,
                "jti":"cad1453-acceptance-signin"}))
        );
        let assertion = format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(key.sign(signed.as_bytes()).as_ref())
        );
        let (status, headers, response) = self.request(
            "POST",
            "/__platform/session",
            None,
            Some(json!({"assertion":assertion})),
        );
        assert_eq!(status, 200, "verified fixture sign-in: {response}");
        headers
            .iter()
            .find_map(|h| {
                let (name, value) = h.split_once(':')?;
                name.eq_ignore_ascii_case("set-cookie")
                    .then(|| value.trim().split(';').next().unwrap().to_owned())
            })
            .expect("verified public session cookie")
    }

    fn rpc(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(Asserted::Operator, || {
            client::rpc(self.state.path(), method, params)
        })
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

fn assert_initial(result: &Value) {
    assert!(
        result["actor"].as_str().is_some_and(|id| !id.is_empty()),
        "missing verified actor: {result}"
    );
    let people = result["people"].as_array().expect("people array");
    assert_eq!(people.len(), 20, "initial results must be capped at twenty");
    assert_eq!(result["has_more"], true);
    assert!(
        people.iter().all(|p| p["subject"] != SUBJECT),
        "caller leaked into picker"
    );
    assert!(people
        .iter()
        .all(|p| p["id"].as_str().is_some_and(|id| !id.is_empty())));
}

fn assert_target(result: &Value, actor: &Value) {
    assert_eq!(&result["actor"], actor, "search changed the verified actor");
    assert_eq!(result["has_more"], false);
    let people = result["people"].as_array().unwrap();
    assert_eq!(
        people.len(),
        1,
        "search must reach the member beyond the initial page: {result}"
    );
    assert_eq!(people[0]["subject"], TARGET);
    assert_eq!(people[0]["name"], TARGET_NAME);
    assert!(people[0]["id"].as_str().is_some_and(|id| !id.is_empty()));
}

#[test]
fn whole_workspace_search_and_live_member_revocation_at_native_and_http_boundaries() {
    let fixture = Fixture::new();
    let cookie = fixture.member_cookie();
    assert!(cookie.starts_with("__Host-aos-board-session="));
    let token = cookie.split_once('=').unwrap().1;
    let checked = fixture
        .rpc("board_session_check", json!({"token":token}))
        .unwrap();
    assert_eq!(checked["valid"], true);

    let initial = fixture
        .rpc("human_people_search", json!({"token":token}))
        .unwrap();
    assert_initial(&initial);
    let (status, _, initial_http) =
        fixture.request("GET", "/api/human-threads/people", Some(&cookie), None);
    assert_eq!(status, 200, "HTTP positive control: {initial_http}");
    assert_initial(&initial_http);
    assert_eq!(initial_http["actor"], initial["actor"]);

    let target = fixture
        .rpc(
            "human_people_search",
            json!({"token":token,"query":"  zOË 東京 + &  "}),
        )
        .unwrap();
    assert_target(&target, &initial["actor"]);
    let (status, _, target_http) = fixture.request(
        "GET",
        "/api/human-threads/people?query=%20%20zO%C3%8B%20%E6%9D%B1%E4%BA%AC%20%2B%20%26%20%20",
        Some(&cookie),
        None,
    );
    assert_eq!(
        status, 200,
        "percent-encoded Unicode name query: {target_http}"
    );
    assert_target(&target_http, &initial["actor"]);
    assert_eq!(
        target_http["people"], target["people"],
        "HTTP must preserve verified member identities"
    );
    let self_only = fixture
        .rpc(
            "human_people_search",
            json!({"token":token,"query":"Current Caller Only"}),
        )
        .unwrap();
    assert_eq!(self_only["people"], json!([]));
    assert_eq!(self_only["has_more"], false);

    // Keep the signed-in stored session valid; change only the trusted live
    // roster. A cached-directory or cookie-only implementation must fail here.
    fixture.current_member.store(false, Ordering::SeqCst);
    let refusal = fixture
        .rpc("human_people_search", json!({"token":token,"query":"Zoë"}))
        .unwrap_err();
    assert_eq!(
        refusal.code(),
        Some("not_a_member"),
        "native guard did not reject removed member: {refusal}"
    );
    let (status, _, refusal) = fixture.request(
        "GET",
        "/api/human-threads/people?query=Zo%C3%AB",
        Some(&cookie),
        None,
    );
    assert_eq!(
        status, 403,
        "removed member HTTP search was not refused: {refusal}"
    );
    assert_eq!(
        refusal["code"], "not_a_member",
        "HTTP refusal must come from the live-membership guard: {refusal}"
    );
    let checked = fixture
        .rpc("board_session_check", json!({"token":token}))
        .unwrap();
    assert_eq!(
        checked["valid"], true,
        "refusal must not be an expired/invalid session false positive"
    );

    fixture.current_member.store(true, Ordering::SeqCst);
    let restored = fixture
        .rpc("human_people_search", json!({"token":token,"query":"Zoë"}))
        .unwrap();
    assert_target(&restored, &initial["actor"]);
    let (status, _, restored_http) = fixture.request(
        "GET",
        "/api/human-threads/people?query=Zo%C3%AB",
        Some(&cookie),
        None,
    );
    assert_eq!(status, 200, "restored member HTTP control: {restored_http}");
    assert_target(&restored_http, &initial["actor"]);
}
