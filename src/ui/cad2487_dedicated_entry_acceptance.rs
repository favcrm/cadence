//! Dedicated browser acceptance exercising real HTTP, signed owner
//! revalidation, one-use entry and encrypted custody with an offline token.
#![cfg(all(
    test,
    target_os = "linux",
    target_arch = "x86_64",
    feature = "test-seam"
))]

use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::io::{BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

const HOST: &str = "dedicated.example.test";
const ENTRY_HOST: &str = "entry.example.test";
const ENTRY_ORIGIN: &str = "https://entry.example.test";
const BOARD_ORIGIN: &str = "https://dedicated.example.test";
const COMPANY: &str = "fixture-workspace";
const DEPLOYMENT: &str = "fixture-board";
const SUBJECT: &str = "fixture-owner";
const KEY: &str = "fixture-key";
const SECRET: &str = "cad2487-synthetic-enrollment-token";
const CHILD: &str = "CAD2487_BROWSER_CHILD";

fn openssl(dir: &Path, args: &[&str]) {
    assert!(Command::new("/usr/bin/openssl")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("openssl fixture")
        .success());
}

struct Issuer {
    stop: Arc<AtomicBool>,
    revoked: Arc<AtomicBool>,
    granted: Arc<AtomicUsize>,
    denied: Arc<AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
    _tls: crate::test_web_issuer_tls::TestWebIssuerTlsGuard,
}
impl Drop for Issuer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Servers {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for Servers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn issuer(dir: &Path, public_key: String, jti: String, verifier: String) -> Issuer {
    openssl(
        dir,
        &[
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-keyout",
            "ca.key",
            "-out",
            "ca.pem",
            "-days",
            "2",
            "-subj",
            "/CN=CAD-2487 test CA",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign,cRLSign",
        ],
    );
    openssl(
        dir,
        &[
            "req",
            "-new",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-keyout",
            "leaf.key",
            "-out",
            "leaf.csr",
            "-subj",
            "/CN=web-publish-issuer.test",
        ],
    );
    std::fs::write(dir.join("leaf.ext"), "subjectAltName=DNS:web-publish-issuer.test\nbasicConstraints=CA:FALSE\nkeyUsage=digitalSignature\nextendedKeyUsage=serverAuth\n").unwrap();
    openssl(
        dir,
        &[
            "x509",
            "-req",
            "-in",
            "leaf.csr",
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-days",
            "2",
            "-extfile",
            "leaf.ext",
            "-out",
            "leaf.pem",
        ],
    );
    let certs = rustls_pemfile::certs(&mut BufReader::new(
        std::fs::File::open(dir.join("leaf.pem")).unwrap(),
    ))
    .collect::<std::result::Result<Vec<_>, _>>()
    .unwrap();
    let key = rustls_pemfile::private_key(&mut BufReader::new(
        std::fs::File::open(dir.join("leaf.key")).unwrap(),
    ))
    .unwrap()
    .unwrap();
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let tls = crate::test_web_issuer_tls::TestWebIssuerTls::install(
        crate::test_web_issuer_tls::ISSUER,
        listener.local_addr().unwrap(),
        &std::fs::read(dir.join("ca.pem")).unwrap(),
    )
    .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let revoked = Arc::new(AtomicBool::new(false));
    let granted = Arc::new(AtomicUsize::new(0));
    let denied = Arc::new(AtomicUsize::new(0));
    let (s, r, g, d) = (
        stop.clone(),
        revoked.clone(),
        granted.clone(),
        denied.clone(),
    );
    let thread = std::thread::spawn(move || {
        while !s.load(Ordering::SeqCst) {
            let Ok((tcp, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            };
            tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut wire = rustls::StreamOwned::new(
                rustls::ServerConnection::new(config.clone()).unwrap(),
                tcp,
            );
            let mut bytes = Vec::new();
            let mut chunk = [0; 1024];
            while !bytes.windows(4).any(|w| w == b"\r\n\r\n") && bytes.len() < 16384 {
                match wire.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                }
            }
            let raw = String::from_utf8_lossy(&bytes);
            let first = raw.lines().next().unwrap_or("");
            let (status, body) = if first.starts_with("GET /.well-known/agenticos-board-jwks.json ") {
                (200, json!({"keys":[{"kty":"OKP","crv":"Ed25519","kid":KEY,"x":public_key}]}))
            } else if first == format!("GET /v2/board/dedicated/owner?deployment={DEPLOYMENT}&assertion={jti} HTTP/1.1") && raw.lines().any(|line| line.eq_ignore_ascii_case(&format!("authorization: Bearer {verifier}"))) {
                if r.load(Ordering::SeqCst) { d.fetch_add(1, Ordering::SeqCst); (403, json!({"ok":false})) }
                else { g.fetch_add(1, Ordering::SeqCst); (200, json!({"ok":true,"data":{"version":"dedicated-board-owner.v1","deployment_id":DEPLOYMENT,"workspace_id":COMPANY,"audience":HOST,"subject_id":SUBJECT,"assertion_jti":jti,"role":"owner"}})) }
            } else { (404, json!({"ok":false})) };
            let body = body.to_string();
            let _ = write!(wire, "HTTP/1.1 {status} response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = wire.flush();
        }
    });
    Issuer {
        stop,
        revoked,
        granted,
        denied,
        thread: Some(thread),
        _tls: tls,
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(15)))
            .build(),
    )
}
fn post(
    port: u16,
    path: &str,
    cookie: Option<&str>,
    body: Value,
) -> ureq::http::Response<ureq::Body> {
    let mut req = agent()
        .post(format!("http://127.0.0.1:{port}{path}"))
        .header("Host", HOST)
        .header("Content-Type", "application/json")
        .header("Origin", BOARD_ORIGIN)
        .header("Sec-Fetch-Site", "same-origin")
        .header("X-Cadence-Board", "1");
    if let Some(cookie) = cookie {
        req = req.header("Cookie", cookie);
    }
    req.send(serde_json::to_vec(&body).unwrap().as_slice())
        .expect("board HTTP transport")
}

fn handoff(
    port: u16,
    nonce: &str,
    request: &str,
    agent_token: Option<&str>,
) -> ureq::http::Response<ureq::Body> {
    let mut req = agent()
        .post(format!(
            "http://127.0.0.1:{port}/api/credential-entry/authenticate"
        ))
        .header("Host", ENTRY_HOST)
        .header("Origin", BOARD_ORIGIN)
        .header("Sec-Fetch-Mode", "navigate")
        .header("Sec-Fetch-Site", "cross-site")
        .header("Content-Type", "application/x-www-form-urlencoded");
    if let Some(token) = agent_token {
        req = req
            .header(crate::test_seam::AS_HEADER, "agent:enrollment-fixture")
            .header(crate::test_seam::TOKEN_HEADER, token);
    }
    req.send(format!("nonce={nonce}&request_id={request}").as_bytes())
        .expect("entry form HTTP transport")
}

fn entry_post(
    port: u16,
    request: &str,
    verb: &str,
    origin: &str,
    session: (&str, &str),
    payload: &Value,
) -> (u16, Value) {
    body(
        agent()
            .post(format!(
                "http://127.0.0.1:{port}/api/credential-requests/{request}/{verb}"
            ))
            .header("Host", ENTRY_HOST)
            .header("Origin", origin)
            .header("Content-Type", "application/json")
            .header("Sec-Fetch-Site", "same-origin")
            .header("X-Cadence-Board", "1")
            .header("Cookie", session.0)
            .header(super::operator::SESSION_HEADER, session.1)
            .send(serde_json::to_vec(payload).unwrap().as_slice())
            .expect("entry API HTTP transport"),
    )
}

fn ok(response: ureq::http::Response<ureq::Body>, step: &str) -> Value {
    let (status, value) = body(response);
    assert_eq!(status, 200, "{step}: {value}");
    value
}

fn open_entry(mut response: ureq::http::Response<ureq::Body>, request: &str) -> (String, String) {
    if response.status().as_u16() != 200 {
        let (status, value) = body(response);
        panic!("handoff must open the entry document: {status} {value}");
    }
    let cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cookie.starts_with("__Host-cadence_entry_"));
    assert!(cookie.ends_with("; Path=/; HttpOnly; SameSite=Strict; Secure"));
    let cookie = cookie.split_once(';').unwrap().0.to_string();
    let html = response.body_mut().read_to_string().unwrap();
    let (_, bootstrap) = html
        .split_once("window.__CADENCE_ENTRY_BOOTSTRAP__={key:\"")
        .expect("entry document has an in-memory key");
    let (key, _) = bootstrap.split_once('"').unwrap();
    assert!(key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(html.contains(&format!("requestId:\"{request}\"")));
    assert!(!html.contains(SECRET));
    (cookie, key.to_owned())
}

fn request_id(response: &Value) -> &str {
    response["request"]["request_id"].as_str().unwrap()
}
fn body(mut response: ureq::http::Response<ureq::Body>) -> (u16, Value) {
    let status = response.status().as_u16();
    let raw = response.body_mut().read_to_string().unwrap();
    (
        status,
        serde_json::from_str(&raw).unwrap_or_else(|_| json!({"raw":raw})),
    )
}

#[test]
fn cad2487_owner_enrolls_only_through_live_scoped_handoff() {
    if std::env::var_os(CHILD).is_some() {
        dedicated_owner_entry_isolated();
        return;
    }
    // The issuer TLS seam is process-global. A separate test process makes
    // this fixture independent of concurrent issuer tests; the outer test
    // is selected normally by CI, not as an ignored/manual test.
    let name = format!(
        "{}::cad2487_owner_enrolls_only_through_live_scoped_handoff",
        module_path!().split_once("::").unwrap().1
    );
    let home = tempfile::Builder::new()
        .prefix("cad2487-child-home-")
        .tempdir()
        .unwrap();
    let config = home.path().join("config");
    let data = home.path().join("data");
    let cache = home.path().join("cache");
    let state = home.path().join("state");
    let runtime = home.path().join("runtime");
    for dir in [&config, &data, &cache, &state, &runtime] {
        std::fs::create_dir(dir).unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .env_clear()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_DATA_HOME", &data)
        .env("XDG_CACHE_HOME", &cache)
        .env("XDG_STATE_HOME", &state)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("PATH", "/usr/bin:/bin")
        .env(CHILD, "1")
        .arg("--exact")
        .arg(name)
        .arg("--nocapture")
        .output()
        .expect("isolated acceptance process");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed;"),
        "isolated owner acceptance did not pass: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn dedicated_owner_entry_isolated() {
    block_sigchld();
    let dir = tempfile::Builder::new()
        .prefix("cad2487-owner-")
        .tempdir()
        .unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let state = dir.path().join("state");
    let pm = dir.path().join("pm");
    std::fs::create_dir(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    crate::issue::Pm::init(&pm).unwrap();
    let dist = dir.path().join("dist");
    std::fs::create_dir(&dist).unwrap();
    std::fs::write(
        dist.join("index.html"),
        "<!doctype html><html><head><title>Entry fixture shell</title></head><body></body></html>",
    )
    .unwrap();
    let key = Ed25519KeyPair::from_seed_unchecked(&[87; 32]).unwrap();
    let jti = uuid::Uuid::new_v4().to_string();
    let verifier = "a".repeat(64);
    let issuer = issuer(
        dir.path(),
        URL_SAFE_NO_PAD.encode(key.public_key().as_ref()),
        jti.clone(),
        verifier.clone(),
    );
    let verifier_path = dir.path().join("verifier");
    std::fs::write(&verifier_path, verifier).unwrap();
    std::fs::set_permissions(&verifier_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let mut servers = Servers {
        stop: stop.clone(),
        threads: Vec::new(),
    };
    let vars = std::collections::HashMap::from([
        (
            "AGENTICOS_DEDICATED_BOARD_DEPLOYMENT",
            DEPLOYMENT.to_string(),
        ),
        ("AGENTICOS_BOARD_HOST", HOST.to_string()),
        (
            "AGENTICOS_BOARD_ISSUER",
            crate::test_web_issuer_tls::ISSUER.to_string(),
        ),
        ("AGENTICOS_BOARD_COMPANY", COMPANY.to_string()),
        (
            "AGENTICOS_DEDICATED_BOARD_VERIFIER_FILE",
            verifier_path.to_str().unwrap().to_string(),
        ),
    ]);
    let mode =
        crate::daemon::TestMembershipMode::capture(&crate::lease::Hosted::default(), &|name| {
            vars.get(name).cloned()
        });
    let mut opts = crate::daemon::ServeOptions {
        test_seam: true,
        stop: Some(stop.clone()),
        membership_mode_for_test: Some(mode),
        provider_env: crate::adapter::ProviderEnv::refusing_providers(),
        ..Default::default()
    };
    crate::daemon::configure_credential_entry_fixture(&mut opts, dir.path(), ENTRY_ORIGIN)
        .expect("configure isolated owner and encrypted custody");
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let daemon_state = state.clone();
    servers.threads.push(std::thread::spawn(move || {
        crate::daemon::serve_with(&daemon_state, opts).unwrap()
    }));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !state.join("cadence.sock").exists() {
        assert!(
            !servers.threads[0].is_finished(),
            "daemon exited during startup"
        );
        assert!(Instant::now() < deadline, "daemon startup");
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut port = None;
    for candidate in 3110..3200 {
        let (tx, rx) = std::sync::mpsc::channel();
        let board_opts = ServeOpts {
            host: "127.0.0.1".into(),
            port: candidate,
            stop: Some(stop.clone()),
            startup: Some(tx),
            test_seam: true,
            board_public_only: true,
            allow_hosts: vec![HOST.into()],
            allow_origins: vec![BOARD_ORIGIN.into()],
            credential_entry_origin: Some(ENTRY_ORIGIN.into()),
            dist: Some(dist.clone()),
            public: Some(PublicBoard {
                host: HOST.into(),
                issuer: crate::test_web_issuer_tls::ISSUER.into(),
                company: COMPANY.into(),
                authorize_url: "https://portal.example.test/v2/board/authorize".into(),
            }),
            dedicated_deployment_for_test: Some(DEPLOYMENT.into()),
            ..Default::default()
        };
        let (board_state, board_pm) = (state.clone(), pm.clone());
        let board = std::thread::spawn(move || {
            let _ = crate::ui::serve(&board_state, &board_pm, &board_opts);
        });
        servers.threads.push(board);
        match rx
            .recv_timeout(Duration::from_secs(30))
            .expect("board startup disconnected")
        {
            Ok(()) => {
                port = Some(candidate);
                break;
            }
            Err(error) => {
                servers.threads.pop().unwrap().join().unwrap();
                assert_eq!(error, std::io::ErrorKind::AddrInUse, "board failed to bind");
            }
        }
    }
    let port = port.expect("no isolated board port available");
    let login = agent()
        .get(format!("http://127.0.0.1:{port}/__platform/login"))
        .header("Host", HOST)
        .call()
        .unwrap();
    assert_eq!(login.status().as_u16(), 302);
    let cookie = login
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let state_value = cookie.split_once('=').unwrap().1.to_string();
    assert_eq!(state_value.len(), 64);
    let location = login.headers().get("location").unwrap().to_str().unwrap();
    assert!(
        location.contains(&format!("state={state_value}"))
            && location.contains("deployment=fixture-board")
    );
    let now = crate::issue::time::now_epoch();
    let header = json!({"alg":"EdDSA","typ":"JWT","kid":KEY});
    let claims = json!({"iss":crate::test_web_issuer_tls::ISSUER,"aud":HOST,"company":COMPANY,"sub":SUBJECT,"email":"owner@example.test","name":"Owner","role":"owner","iat":now,"exp":now+60,"jti":jti});
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
    );
    let assertion = format!(
        "{input}.{}",
        URL_SAFE_NO_PAD.encode(key.sign(input.as_bytes()).as_ref())
    );
    let (status, response) = body(post(
        port,
        "/__platform/session",
        None,
        json!({"assertion":assertion,"state":state_value}),
    ));
    assert_eq!(
        status, 403,
        "missing initiating cookie must not mint a session: {response}"
    );
    let (status, response) = body(post(
        port,
        "/__platform/session",
        Some(&cookie),
        json!({"assertion":assertion,"state":"f".repeat(64)}),
    ));
    assert_eq!(
        status, 403,
        "mismatched browser intent must not mint a session: {response}"
    );
    let session = post(
        port,
        "/__platform/session",
        Some(&cookie),
        json!({"assertion":assertion,"state":state_value}),
    );
    if session.status().as_u16() != 200 {
        let (status, response) = body(session);
        panic!("live signed owner must open the board session: {status} {response}");
    }
    let board_cookie = session
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|h| h.to_str().ok())
        .find(|h| h.starts_with("__Host-aos-board-session="))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let board_post = |path: &str, payload| post(port, path, Some(&board_cookie), payload);
    for (action, unlocked) in [("custody_initialize", false), ("custody_unlock", true)] {
        let ticket = ok(
            board_post(
                "/api/protected-management/session",
                json!({"action": action, "params": {}}),
            ),
            action,
        );
        let receipt = ok(
            board_post(
                "/api/protected-management/action",
                json!({
                    "action": action, "params": {},
                    "nonce": ticket["nonce"], "attempt_id": ticket["attempt_id"]
                }),
            ),
            action,
        );
        assert_eq!(receipt["custody"]["unlocked"], unlocked);
    }

    let create = || {
        ok(
            board_post(
                "/api/credential-requests",
                json!({"provider":"openrouter","verb":"enroll","shape":"token"}),
            ),
            "create credential request",
        )
    };
    let first = create();
    let second = create();
    let request = request_id(&first);
    let other = request_id(&second);
    assert_ne!(request, other);
    let page = format!("/connections/enroll/{request}");
    assert_eq!(
        first["request"]["entry_url"],
        format!("{ENTRY_ORIGIN}{page}"),
    );
    let get = |host: &str, path: &str| {
        agent()
            .get(format!("http://127.0.0.1:{port}{path}"))
            .header("Host", host)
            .header("Cookie", &board_cookie)
            .call()
            .unwrap()
    };
    assert_eq!(get(ENTRY_HOST, &page).status().as_u16(), 200);
    assert!(
        get(&format!("127.0.0.1:{port}"), &page)
            .status()
            .is_client_error(),
        "a loopback host must never serve the entry form"
    );
    assert_eq!(get(HOST, &page).status().as_u16(), 404);
    assert_eq!(get(ENTRY_HOST, "/api/projects").status().as_u16(), 404);
    assert!(
        get("unlisted.example.test", &page)
            .status()
            .is_client_error(),
        "a different host must never serve the entry form"
    );
    assert!(post(
        port,
        "/api/credential-entry/prepare",
        None,
        json!({"request_id": request})
    )
    .status()
    .is_client_error());

    let prepare = |request: &str| {
        ok(
            board_post(
                "/api/credential-entry/prepare",
                json!({"request_id": request}),
            ),
            "prepare signed owner handoff",
        )["nonce"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let wrong_request_nonce = prepare(request);
    for id in [other, request] {
        let refusal = handoff(port, &wrong_request_nonce, id, None);
        assert_ne!(
            refusal.status().as_u16(),
            200,
            "wrong request spends the nonce"
        );
        assert!(refusal.headers().get("set-cookie").is_none());
    }

    let nonce = prepare(request);
    let fixture_token = crate::test_seam::Seam::token_at(&state).unwrap();
    let (status, refusal) = body(handoff(port, &nonce, request, Some(&fixture_token)));
    assert_eq!(status, 403, "agent peer must be refused: {refusal}");
    assert_eq!(refusal["check"], "session_from_agent");
    let (cookie, key) = open_entry(handoff(port, &nonce, request, None), request);
    let replay = handoff(port, &nonce, request, None);
    assert_ne!(replay.status().as_u16(), 200, "handoff replay must fail");
    assert!(replay.headers().get("set-cookie").is_none());

    let entry_call = |id: &str, verb: &str, origin: &str, payload: &Value| {
        entry_post(port, id, verb, origin, (&cookie, &key), payload)
    };
    let (status, _) = entry_call(other, "session", ENTRY_ORIGIN, &json!({}));
    assert_eq!(status, 401, "entry cookie cannot authorize another request");
    let (status, session) = entry_call(request, "session", ENTRY_ORIGIN, &json!({}));
    assert_eq!(status, 200, "mint single-use submission: {session}");
    let payload = json!({"nonce":session["nonce"],"secret":SECRET});
    let (status, refusal) = entry_call(request, "submit", BOARD_ORIGIN, &payload);
    assert_eq!(status, 403);
    assert_eq!(refusal["code"], "wrong_entry_origin");
    let (status, refusal) = body(board_post(
        &format!("/api/credential-requests/{request}/submit"),
        payload.clone(),
    ));
    assert_eq!(status, 403);
    assert_eq!(refusal["code"], "wrong_entry_origin");
    let (status, saved) = entry_call(request, "submit", ENTRY_ORIGIN, &payload);
    assert_eq!(status, 200, "synthetic token was not saved");
    assert_eq!(saved["status"], "saved");
    assert!(!saved.to_string().contains(SECRET));
    let (status, refusal) = entry_call(request, "submit", ENTRY_ORIGIN, &payload);
    assert_eq!(status, 409);
    assert_eq!(refusal["code"], "already_submitted");

    // An already-open entry session must lose its submission authority
    // when the issuer withdraws its original board sign-in.
    let (other_cookie, other_key) = open_entry(handoff(port, &prepare(other), other, None), other);
    let (status, session) = entry_post(
        port,
        other,
        "session",
        ENTRY_ORIGIN,
        (&other_cookie, &other_key),
        &json!({}),
    );
    assert_eq!(status, 200, "second open entry: {session}");
    assert!(issuer.granted.load(Ordering::SeqCst) > 1);
    issuer.revoked.store(true, Ordering::SeqCst);
    let (status, _) = entry_post(
        port,
        other,
        "submit",
        ENTRY_ORIGIN,
        (&other_cookie, &other_key),
        &json!({"nonce": session["nonce"], "secret": SECRET}),
    );
    assert!(
        matches!(status, 401 | 403),
        "revoked entry unexpectedly returned {status}"
    );
    assert!(
        issuer.denied.load(Ordering::SeqCst) > 0,
        "the issuer must refuse the original JTI"
    );

    let db = rusqlite::Connection::open_with_flags(
        state.join("cadence.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let attempts: Vec<String> = db
        .prepare("SELECT request_id FROM credential_request_attempts WHERE state = 'applied'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(attempts, [request]);
    let attempt_count: i64 = db
        .query_row(
            "SELECT count(*) FROM credential_request_attempts",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        attempt_count, 1,
        "attempts on denied paths must never persist"
    );
    let custody: String = db
        .query_row(
            "SELECT custody FROM platform_credentials WHERE platform = 'openrouter'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let locator = custody
        .strip_prefix(crate::platform::encrypted_sqlite::TAG_PREFIX)
        .expect("the connection references encrypted custody");
    let vault = rusqlite::Connection::open_with_flags(
        state.join("vault/custody.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let ciphertext: Vec<u8> = vault
        .query_row(
            "SELECT ciphertext FROM records WHERE lower(hex(locator)) = ? AND provider = 'openrouter'",
            [locator],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!ciphertext
        .windows(SECRET.len())
        .any(|w| w == SECRET.as_bytes()));
    assert_eq!(
        vault
            .query_row("SELECT count(*) FROM records", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
