//! Wiki HTTP guard acceptance (CAD-1569). Exercises Cadence's real board HTTP
//! guard and daemon wiki RPC against an isolated temp tracker; the
//! request has the browser session cookie/key and deliberately no Authorization.
#![cfg(feature = "test-seam")]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cadence_agent::test_seam::{Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon, operator_auth};
use ring::signature::KeyPair;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

struct Fixture {
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    clock: Arc<AtomicI64>,
    daemon: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    board: Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)>,
    port: u16,
}

type Session = (String, String);

impl Fixture {
    fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("aos137wiki")
            .tempdir()
            .unwrap();
        let pm = root.path().join("pm");
        cadence_agent::issue::Pm::init(&pm).unwrap();
        let state = root.path().join("state");
        std::fs::create_dir_all(&state).unwrap();

        // The served tracker follows the isolated legacy HOME/wiki layout.
        let wiki = pm.join("wiki/global");
        std::fs::create_dir_all(&wiki).unwrap();
        std::fs::write(wiki.join("acceptance.md"), "AOS-137 fixture page\n").unwrap();

        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let clock = Arc::new(AtomicI64::new(1_800_000_000));
        let opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(Arc::clone(&stop)),
            test_seam: true,
            operator_clock: Some({
                let clock = Arc::clone(&clock);
                Arc::new(move || clock.load(SeqCst))
            }),
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        let daemon_state = state.clone();
        let daemon = std::thread::spawn(move || daemon::serve_with(&daemon_state, opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&state).is_none()
        {
            assert!(
                !daemon.is_finished() && std::time::Instant::now() < deadline,
                "daemon down"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        let port = (3110..=3199)
            .find(|port| std::net::TcpListener::bind(("127.0.0.1", *port)).is_ok())
            .expect("free board port in 3110-3199");
        let (tx, rx) = std::sync::mpsc::channel();
        let board_stop = Arc::new(AtomicBool::new(false));
        let board_opts = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(Arc::clone(&board_stop)),
            startup: Some(tx),
            test_seam: true,
            ..Default::default()
        };
        let (board_state, board_pm) = (state, pm);
        let board = std::thread::spawn(move || {
            let _ = cadence_agent::ui::serve(&board_state, &board_pm, &board_opts);
        });
        rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
        Self {
            root,
            stop,
            clock,
            daemon: Some(daemon),
            board: Some((board_stop, board)),
            port,
        }
    }

    fn state(&self) -> std::path::PathBuf {
        self.root.path().join("state")
    }

    fn request(&self, path: &str, session: Option<&Session>) -> (u16, String) {
        let host = format!("cadence-{}.localhost:{}", self.port, self.port);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let mut request = agent
            .get(format!("http://127.0.0.1:{}{path}", self.port))
            .header("Host", &host)
            .header("Origin", format!("http://{host}"))
            .header("X-Cadence-Board", "1")
            .header(AS_HEADER, "operator")
            .header(TOKEN_HEADER, Seam::token_at(&self.state()).unwrap());
        if let Some((cookie, key)) = session {
            request = request
                .header("Cookie", cookie)
                .header("X-Cadence-Session", key);
        }
        // Deliberately do not attach Authorization: this is the browser's
        // cookie-authenticated GET form forwarded by the pending worker.
        let mut response = request.call().unwrap();
        let status = response.status().as_u16();
        (status, response.body_mut().read_to_string().unwrap())
    }

    fn operator_session(&self) -> Session {
        operator_auth::ensure_secret(&self.state()).unwrap();
        let secret = operator_auth::read_secret(&self.state()).unwrap();
        let nonce = cadence_agent::test_seam::scoped(Asserted::Operator, || {
            client::rpc(
                &self.state(),
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )
        })
        .unwrap()["nonce"]
            .as_str()
            .unwrap()
            .to_owned();
        let body = json!({"nonce": nonce}).to_string();
        let host = format!("cadence-{}.localhost:{}", self.port, self.port);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let mut response = agent
            .post(format!("http://127.0.0.1:{}/api/session", self.port))
            .header("Host", &host)
            .header("Origin", format!("http://{host}"))
            .header("X-Cadence-Board", "1")
            .header("Content-Type", "application/json")
            .header(AS_HEADER, "operator")
            .header(TOKEN_HEADER, Seam::token_at(&self.state()).unwrap())
            .send(body)
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let key = response.body_mut().read_to_string().unwrap();
        let key = serde_json::from_str::<Value>(&key).unwrap()["session_key"]
            .as_str()
            .unwrap()
            .to_owned();
        (cookie, key)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some((stop, thread)) = self.board.take() {
            stop.store(true, SeqCst);
            let _ = thread.join();
        }
        self.stop.store(true, SeqCst);
        if let Some(thread) = self.daemon.take() {
            let _ = thread.join();
        }
    }
}

#[test]
fn cookie_authenticated_wiki_reads_succeed_and_invalid_sessions_or_paths_refuse() {
    let fx = Fixture::start();
    let session = fx.operator_session();

    let (status, listing) = fx.request("/api/wiki/ls?path=global", Some(&session));
    assert_eq!(status, 200, "wiki browse: {listing}");
    assert!(
        listing.contains("acceptance.md"),
        "fixture file absent from listing: {listing}"
    );
    let (status, page) = fx.request("/api/wiki/file?path=global/acceptance.md", Some(&session));
    assert_eq!(status, 200, "wiki read: {page}");
    assert!(
        page.contains("AOS-137 fixture page"),
        "wrong file contents: {page}"
    );

    // Each refusal is pinned to the guard that produces it, so an unrelated
    // 4xx cannot satisfy the assertion.
    let other = fx.operator_session();
    for (label, presented) in [
        ("absent session", None),
        (
            "forged cookie",
            Some(&(
                format!("cadence_operator_{}=forged", fx.port),
                session.1.clone(),
            )),
        ),
        // A real cookie paired with another live session's key.
        (
            "mismatched cookie and key",
            Some(&(session.0.clone(), other.1.clone())),
        ),
    ] {
        let (status, reply) = fx.request("/api/wiki/ls?path=global", presented);
        assert_eq!(status, 401, "{label}: {reply}");
        assert!(
            reply.contains("operator_session_required"),
            "{label} refused for the wrong reason: {reply}"
        );
    }

    // Advance the daemon's deterministic auth clock beyond this real
    // session's absolute lifetime; this exercises expiry, not just bad syntax.
    fx.clock
        .store(1_800_000_000 + operator_auth::ABSOLUTE_SECS + 1, SeqCst);
    let (status, reply) = fx.request("/api/wiki/ls?path=global", Some(&session));
    assert_eq!(status, 401, "expired session: {reply}");
    assert!(
        reply.contains("operator_session_required"),
        "expired session refused for the wrong reason: {reply}"
    );

    // A fresh session keeps the path refusal independent of the expired
    // session assertion above. The valid read first proves that session is
    // live, so the 400 below can only come from the path guard.
    fx.clock.store(1_800_000_000, SeqCst);
    let fresh = fx.operator_session();
    let (status, reply) = fx.request("/api/wiki/ls?path=global", Some(&fresh));
    assert_eq!(
        status, 200,
        "fresh session not live before traversal: {reply}"
    );
    std::fs::write(
        fx.root.path().join("outside-secret"),
        "AOS-137 outside sentinel\n",
    )
    .unwrap();
    let (status, reply) = fx.request("/api/wiki/file?path=../../outside-secret", Some(&fresh));
    assert_eq!(status, 400, "traversal: {reply}");
    assert!(
        reply.contains("'.' and '..' segments are not allowed"),
        "traversal refused for the wrong reason: {reply}"
    );
    assert!(
        !reply.contains("outside sentinel"),
        "traversal leaked the outside sentinel: {reply}"
    );
}

#[test]
fn cad347_authenticated_http_memory_curation_remains_closed_after_browser_guards() {
    let fx = Fixture::start();
    let session = fx.operator_session();
    let pm_dir = fx.root.path().join("pm");
    let pm = cadence_agent::issue::Pm::at(&pm_dir).unwrap();
    cadence_agent::issue::write::project_add(&pm, "cad347-http", "CHT", &[], &[], &[], None)
        .expect("isolated project for HTTP memory refusal");
    let memory_dir = cadence_agent::memory::memory_dir(&pm, "cad347-http");
    std::fs::create_dir_all(&memory_dir).unwrap();
    let path = memory_dir.join("cad347-http-lesson.md");
    // An existing readable legacy candidate is fixture DATA, not a
    // claimed new-v2 proposal or native approval. HTTP must not curate
    // either legacy or v2 records, even with a valid browser session.
    let candidate = "---\nid: cad347-http-lesson\ntype: decision\nstatus: proposed\nscope:\n  project: true\nconfidence: high\ncreated: '2026-10-27T00:00:00Z'\n---\n\nHTTP must not confer native memory authority\n\n**Why:** browser identity is not native curation proof\n\n**How to apply:** use the supported native path\n";
    std::fs::write(&path, candidate).unwrap();
    pm.commit(
        std::slice::from_ref(&path),
        "CAD-347 HTTP refusal fixture\n",
    )
    .unwrap();
    let head = || {
        let output = cadence_agent::reaper::output(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&pm_dir)
                .args(["rev-parse", "HEAD"]),
        )
        .unwrap();
        assert!(output.status.success());
        output.stdout
    };
    let before_head = head();
    let before_bytes = std::fs::read(&path).unwrap();
    let (status, detail) = fx.request(
        "/api/memories/cad347-http/cad347-http-lesson",
        Some(&session),
    );
    assert_eq!(
        status, 200,
        "fixture must be readable before curation: {detail}"
    );
    let host = format!("cadence-{}.localhost:{}", fx.port, fx.port);
    let http: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    for operation in ["accept", "reject"] {
        for valid_origin in [false, true] {
            let origin = if valid_origin {
                format!("http://{host}")
            } else {
                "http://untrusted.invalid".to_string()
            };
            let mut response = http
                .post(format!(
                    "http://127.0.0.1:{}/api/memories/cad347-http/cad347-http-lesson/{operation}",
                    fx.port
                ))
                .header("Host", &host)
                .header("Origin", origin)
                .header("X-Cadence-Board", "1")
                .header("Content-Type", "application/json")
                .header("Cookie", &session.0)
                .header("X-Cadence-Session", &session.1)
                .header(AS_HEADER, "operator")
                .header(TOKEN_HEADER, Seam::token_at(&fx.state()).unwrap())
                .send("{}")
                .unwrap();
            let status = response.status().as_u16();
            let reply = response.body_mut().read_to_string().unwrap();
            assert_eq!(
                status,
                if valid_origin { 400 } else { 403 },
                "{operation}, valid_origin={valid_origin}: {reply}"
            );
            let reply: Value = serde_json::from_str(&reply).unwrap();
            if !valid_origin {
                assert_eq!(reply["check"].as_str(), Some("origin"));
            } else {
                assert!(reply["error"]
                    .as_str()
                    .is_some_and(|error| !error.is_empty()));
            }
            assert_eq!(std::fs::read(&path).unwrap(), before_bytes);
            assert_eq!(head(), before_head);
        }
    }
    let memories = cadence_agent::memory::load_project(&pm_dir, "cad347-http").unwrap();
    let memory = memories
        .iter()
        .find(|memory| memory.front.id == "cad347-http-lesson")
        .unwrap();
    assert_eq!(memory.front.status, "proposed");
    assert!(!cadence_agent::memory::retrieval_status(memory).0);
}

const PUBLIC_HOST: &str = "aos137.board.localhost";
const COMPANY: &str = "aos137-fixture-company";
const PUBLIC_KEY_SEED: [u8; 32] = [137; 32];

struct PublicFixture {
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    board_stop: Arc<AtomicBool>,
    clock: Arc<AtomicI64>,
    daemon: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    board: Option<std::thread::JoinHandle<()>>,
    jwks_stop: Arc<AtomicBool>,
    jwks: Option<std::thread::JoinHandle<()>>,
    port: u16,
    issuer: String,
    agent: ureq::Agent,
}

impl PublicFixture {
    fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("aos137pub")
            .tempdir()
            .unwrap();
        let pm = root.path().join("pm");
        cadence_agent::issue::Pm::init(&pm).unwrap();
        let state = root.path().join("state");
        std::fs::create_dir_all(&state).unwrap();

        // The public member's private workspace is both read and searchable.
        let wiki = pm.join("wiki/users/usr_aos137_member");
        std::fs::create_dir_all(&wiki).unwrap();
        std::fs::write(
            wiki.join("acceptance.md"),
            "# AOS-137 Public Wiki Fixture\n\npublic-session-search-marker\n",
        )
        .unwrap();
        let committed = cadence_agent::reaper::status(std::process::Command::new("git").args([
            "-C",
            pm.to_str().unwrap(),
            "add",
            "wiki",
        ]))
        .unwrap();
        assert!(committed.success(), "fixture git add failed");
        let committed = cadence_agent::reaper::status(std::process::Command::new("git").args([
            "-C",
            pm.to_str().unwrap(),
            "-c",
            "user.name=CAD-1569 fixture",
            "-c",
            "user.email=cad1569-fixture@example.invalid",
            "commit",
            "-m",
            "CAD-1569 public wiki acceptance fixture",
        ]))
        .unwrap();
        assert!(committed.success(), "fixture git commit failed");

        let signer =
            ring::signature::Ed25519KeyPair::from_seed_unchecked(&PUBLIC_KEY_SEED).unwrap();
        let jwks_body = json!({"keys": [{
            "kty": "OKP", "crv": "Ed25519", "kid": "aos137",
            "x": URL_SAFE_NO_PAD.encode(signer.public_key().as_ref())
        }]})
        .to_string();
        let (jwks_server, issuer) = (3110..=3199)
            .find_map(|port| {
                tiny_http::Server::http(("127.0.0.1", port))
                    .ok()
                    .map(|server| {
                        let issuer = format!("http://{}", server.server_addr());
                        (server, issuer)
                    })
            })
            .expect("free local fixture port for platform JWKS");
        let jwks_stop = Arc::new(AtomicBool::new(false));
        let jwks_flag = Arc::clone(&jwks_stop);
        let jwks = std::thread::spawn(move || {
            while !jwks_flag.load(SeqCst) {
                let Ok(Some(request)) = jwks_server.recv_timeout(Duration::from_millis(100)) else {
                    continue;
                };
                let response = tiny_http::Response::from_string(jwks_body.clone()).with_header(
                    tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                );
                let _ = request.respond(response);
            }
        });

        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let now = cadence_agent::issue::time::now_epoch();
        let clock = Arc::new(AtomicI64::new(now));
        let daemon_opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(Arc::clone(&stop)),
            test_seam: true,
            operator_clock: Some({
                let clock = Arc::clone(&clock);
                Arc::new(move || clock.load(SeqCst))
            }),
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        let daemon_state = state.clone();
        let daemon = std::thread::spawn(move || daemon::serve_with(&daemon_state, daemon_opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&state).is_none()
        {
            assert!(
                !daemon.is_finished() && std::time::Instant::now() < deadline,
                "public fixture daemon down"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        let port = (3110..=3199)
            .find(|port| {
                *port != issuer.rsplit(':').next().unwrap().parse::<u16>().unwrap()
                    && std::net::TcpListener::bind(("127.0.0.1", *port)).is_ok()
            })
            .expect("free board port in 3110-3199");
        let (tx, rx) = std::sync::mpsc::channel();
        let board_stop = Arc::new(AtomicBool::new(false));
        let host = PUBLIC_HOST.to_string();
        let board_opts = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(Arc::clone(&board_stop)),
            startup: Some(tx),
            test_seam: true,
            public: Some(cadence_agent::ui::PublicBoard {
                host: host.clone(),
                issuer: issuer.clone(),
                company: COMPANY.into(),
                authorize_url: format!("{issuer}/authorize"),
            }),
            allow_hosts: vec![host.clone()],
            allow_origins: vec![format!("http://{host}")],
            ..Default::default()
        };
        let (board_state, board_pm) = (state.clone(), pm.clone());
        let board = std::thread::spawn(move || {
            let _ = cadence_agent::ui::serve(&board_state, &board_pm, &board_opts);
        });
        rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();

        // Fixture-only preparation of the derived search index. The HTTP
        // requests below carry only the real public cookie, never seam identity.
        cadence_agent::test_seam::scoped(Asserted::Operator, || {
            client::rpc(&state, "wiki_index_refresh", json!({}))
        })
        .unwrap();
        Self {
            root,
            stop,
            board_stop,
            clock,
            daemon: Some(daemon),
            board: Some(board),
            jwks_stop,
            jwks: Some(jwks),
            port,
            issuer,
            agent: ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .into(),
        }
    }

    fn assertion(&self, jti: &str) -> String {
        let key = ring::signature::Ed25519KeyPair::from_seed_unchecked(&PUBLIC_KEY_SEED).unwrap();
        let now = self.clock.load(SeqCst);
        let part = |value: Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).unwrap());
        let signed = format!(
            "{}.{}",
            part(json!({"alg": "EdDSA", "typ": "JWT", "kid": "aos137"})),
            part(json!({
                "iss": self.issuer, "aud": PUBLIC_HOST, "sub": "usr_aos137_member",
                "email": "member@example.invalid", "name": "AOS-137 Member",
                "company": COMPANY, "role": "member", "iat": now, "exp": now + 60,
                "jti": jti
            }))
        );
        format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(key.sign(signed.as_bytes()).as_ref())
        )
    }

    fn open_public_session(&self, jti: &str) -> String {
        let response = self
            .agent
            .post(format!("http://127.0.0.1:{}/__platform/session", self.port))
            .header("Host", PUBLIC_HOST)
            .header("Origin", format!("http://{PUBLIC_HOST}"))
            .header("Sec-Fetch-Site", "same-origin")
            .header("Content-Type", "application/json")
            .send(json!({"assertion": self.assertion(jti)}).to_string())
            .unwrap();
        assert_eq!(response.status().as_u16(), 200, "public session exchange");
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    fn get(&self, host: &str, path: &str, cookie: Option<&str>) -> (u16, String) {
        let mut request = self
            .agent
            .get(format!("http://127.0.0.1:{}{path}", self.port))
            .header("Host", host);
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        let mut response = request.call().unwrap();
        let status = response.status().as_u16();
        (status, response.body_mut().read_to_string().unwrap())
    }
}

impl Drop for PublicFixture {
    fn drop(&mut self) {
        self.board_stop.store(true, SeqCst);
        if let Some(thread) = self.board.take() {
            let _ = thread.join();
        }
        self.stop.store(true, SeqCst);
        if let Some(thread) = self.daemon.take() {
            let _ = thread.join();
        }
        self.jwks_stop.store(true, SeqCst);
        if let Some(thread) = self.jwks.take() {
            let _ = thread.join();
        }
        let _ = self.root.path();
    }
}

#[test]
fn public_workspace_wiki_reads_require_its_real_host_bound_session() {
    let fx = PublicFixture::start();
    let cookie = fx.open_public_session("aos137-public-session");

    for (path, evidence) in [
        (
            "/api/wiki/ls?path=users%2Fusr_aos137_member",
            "acceptance.md",
        ),
        (
            "/api/wiki/file?path=users%2Fusr_aos137_member%2Facceptance.md",
            "AOS-137 Public Wiki Fixture",
        ),
        (
            "/api/wiki/search?q=public-session-search-marker&path=users%2Fusr_aos137_member",
            "acceptance.md",
        ),
        (
            "/api/wiki/history?path=users%2Fusr_aos137_member%2Facceptance.md",
            "CAD-1569 public wiki acceptance fixture",
        ),
    ] {
        let (status, body) = fx.get(PUBLIC_HOST, path, Some(&cookie));
        assert_eq!(status, 200, "{path}: {body}");
        assert!(body.contains(evidence), "{path} omitted {evidence}: {body}");
    }

    let cases: [(&str, &str, Option<&str>, u16, &str); 3] = [
        (
            "absent public session",
            PUBLIC_HOST,
            None,
            401,
            "board_session_required",
        ),
        (
            "forged public session",
            PUBLIC_HOST,
            Some("__Host-aos-board-session=forged"),
            401,
            "board_session_required",
        ),
        // A cookie minted for this workspace is not accepted on a sibling
        // workspace host: the Host guard refuses before any session check.
        (
            "cross-workspace cookie replay",
            "other.board.localhost",
            Some(cookie.as_str()),
            421,
            "Host not allowed",
        ),
    ];
    for (label, host, presented, want, marker) in cases {
        let (status, body) = fx.get(host, "/api/wiki/ls?path=users", presented);
        assert_eq!(status, want, "{label}: {body}");
        assert!(
            body.contains(marker),
            "{label} refused for the wrong reason: {body}"
        );
    }

    let (status, body) = fx.get(
        PUBLIC_HOST,
        "/api/wiki/ls?path=users%2Fusr_aos137_member",
        Some(&cookie),
    );
    assert_eq!(
        status, 200,
        "public session not live before traversal: {body}"
    );
    std::fs::write(
        fx.root.path().join("outside-secret"),
        "AOS-137 outside sentinel\n",
    )
    .unwrap();
    let (status, body) = fx.get(
        PUBLIC_HOST,
        "/api/wiki/file?path=..%2F..%2Foutside-secret",
        Some(&cookie),
    );
    assert_eq!(status, 400, "traversal: {body}");
    assert!(
        body.contains("'.' and '..' segments are not allowed"),
        "traversal refused for the wrong reason: {body}"
    );
    assert!(
        !body.contains("outside sentinel"),
        "traversal leaked the outside sentinel: {body}"
    );
    fx.clock.store(
        fx.clock.load(SeqCst) + operator_auth::ABSOLUTE_SECS + 1,
        SeqCst,
    );
    let (status, body) = fx.get(
        PUBLIC_HOST,
        "/api/wiki/ls?path=users%2Fusr_aos137_member",
        Some(&cookie),
    );
    assert_eq!(status, 401, "expired public session: {body}");
    assert!(
        body.contains("board_session_required"),
        "expired public session refused for the wrong reason: {body}"
    );
}
