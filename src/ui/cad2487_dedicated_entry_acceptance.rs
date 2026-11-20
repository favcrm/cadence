//! Independent CAD-2487 HTTP acceptance: the same original platform grant
//! must still be live when a signed-in owner continues credential entry.
#![cfg(all(test, target_os = "linux", feature = "test-seam"))]

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
const COMPANY: &str = "fixture-workspace";
const DEPLOYMENT: &str = "fixture-board";
const SUBJECT: &str = "fixture-owner";
const KEY: &str = "fixture-key";
const REQUEST: &str = "bdb2c121-85fa-443e-8849-92a0f9434187";

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
        .header("Origin", format!("https://{HOST}"))
        .header("Sec-Fetch-Site", "same-origin")
        .header("X-Cadence-Board", "1");
    if let Some(cookie) = cookie {
        req = req.header("Cookie", cookie);
    }
    req.send_json(body).expect("board HTTP transport")
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
fn cad2487_owner_entry_requires_same_live_platform_session() {
    // The issuer TLS seam is process-global. A separate test process makes
    // this fixture independent of every other concurrent issuer test.
    let module = module_path!().split_once("::").unwrap().1;
    let name = format!("{module}::dedicated_owner_entry_isolated");
    let output = Command::new(std::env::current_exe().unwrap())
        .env_clear()
        .arg("--exact")
        .arg(name)
        .arg("--ignored")
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

#[test]
#[ignore = "run by cad2487_owner_entry_requires_same_live_platform_session in an isolated process"]
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
    let opts = crate::daemon::ServeOptions {
        test_seam: true,
        stop: Some(stop.clone()),
        membership_mode_for_test: Some(mode),
        provider_env: crate::adapter::ProviderEnv::refusing_providers(),
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let daemon_state = state.clone();
    servers.threads.push(std::thread::spawn(move || {
        crate::daemon::serve_with(&daemon_state, opts).unwrap()
    }));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !state.join("cadence.sock").exists() {
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
            credential_entry_origin: Some("https://entry.example.test".into()),
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
    let (status, _) = body(post(
        port,
        "/__platform/session",
        None,
        json!({"assertion":assertion,"state":state_value}),
    ));
    assert_eq!(
        status, 403,
        "missing initiating cookie must not mint a session"
    );
    let (status, _) = body(post(
        port,
        "/__platform/session",
        Some(&cookie),
        json!({"assertion":assertion,"state":"f".repeat(64)}),
    ));
    assert_eq!(
        status, 403,
        "mismatched browser intent must not mint a session"
    );
    let session = post(
        port,
        "/__platform/session",
        Some(&cookie),
        json!({"assertion":assertion,"state":state_value}),
    );
    assert_eq!(
        session.status().as_u16(),
        200,
        "live signed owner must open the board session"
    );
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
    let request = || json!({"request_id":REQUEST});
    let (status, value) = body(post(
        port,
        "/api/credential-entry/prepare",
        Some(&board_cookie),
        request(),
    ));
    assert_eq!(
        status, 409,
        "live owner must reach the genuine pending-request guard: {value}"
    );
    assert_eq!(value["code"], "request_changed");
    let before = issuer.granted.load(Ordering::SeqCst);
    assert!(
        before >= 2,
        "mint and entry both need live verifier responses"
    );
    let db = rusqlite::Connection::open_with_flags(
        state.join("cadence.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let counts = || -> (i64, i64) {
        (
            db.query_row(
                "SELECT count(*) FROM credential_request_nonces",
                [],
                |row| row.get(0),
            )
            .unwrap(),
            db.query_row(
                "SELECT count(*) FROM credential_request_attempts",
                [],
                |row| row.get(0),
            )
            .unwrap(),
        )
    };
    let before_mutation = counts();
    issuer.revoked.store(true, Ordering::SeqCst);
    let (status, value) = body(post(
        port,
        "/api/credential-entry/prepare",
        Some(&board_cookie),
        request(),
    ));
    assert!(
        matches!(status, 401 | 403),
        "withdrawn original session must be refused: {status} {value}"
    );
    assert!(
        issuer.denied.load(Ordering::SeqCst) > 0,
        "the actual verifier must answer 403 for the original jti"
    );
    assert_eq!(
        counts(),
        before_mutation,
        "withdrawn authority cannot create credential nonces or attempts"
    );
    drop(db);
    drop(servers);
}
