//! CAD-1901 acceptance, independently authored from the ticket. The
//! implementation authors must not edit or weaken this check.
//!
//! Black box over the real board HTTP dispatcher and the real daemon
//! session store. Hosted named member sessions are minted by
//! `POST /__platform/session` from Ed25519 assertions verified against a
//! local JWKS; the operator session is a real `operator_link_mint` ->
//! `POST /api/session` exchange. Only the board's native peer connection
//! uses the supported test-seam fixture (`X-Cadence-Test-As/-Token`);
//! session identity, signatures, project policy and read authorization
//! are never mocked.
//!
//! The rule under test: `/api/memories` and `/api/memories/<project>/<slug>`
//! apply the canonical project access policy. An authenticated member with
//! no grant on a private project must see none of its memory cards, load
//! errors, identifiers, body or path; its detail answer must be the same
//! absence an unknown memory gets. A granted member and the operator keep
//! reading the very same records.
//!
//! The check therefore FAILS before the fix (the ungranted list returns the
//! private card and the malformed record's error) and passes only when the
//! read routes enforce the policy. Isolated temp tracker; loopback ports
//! 3110-3199; production state is never touched.
//!
//! Run after the change:
//!   cargo test --features test-seam --test suite \
//!     cad1901_memory_project_access_acceptance -- --nocapture --test-threads 1
#![cfg(all(feature = "test-seam", target_os = "linux"))]

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use cadence_agent::adapter::ProviderEnv;
use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon, issue::Pm, operator_auth, ui};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tempfile::{Builder, TempDir};

const PRIVATE_KEY: &str = "cad1901-private";
const OPEN_KEY: &str = "cad1901-open";
const GRANTED_SUB: &str = "member-a";
const UNGRANTED_SUB: &str = "member-b";
const SECRET_SLUG: &str = "cad1901-secret-lesson";
const BROKEN_SLUG: &str = "cad1901-broken";
const OPEN_SLUG: &str = "cad1901-open-lesson";
const UNKNOWN_SLUG: &str = "cad1901-no-such-memory";
const SECRET_FACT: &str = "PRIVATE-FACT-CANARY-1901";
const SECRET_WHY: &str = "PRIVATE-WHY-CANARY-1901";
const SECRET_HOW: &str = "PRIVATE-HOW-CANARY-1901";
const OPEN_FACT: &str = "OPEN-FACT-CANARY-1901";

const PUBLIC_HOST: &str = "cad1901.board.localhost";
const COMPANY: &str = "cad1901-fixture-company";
const KID: &str = "cad1901-key";
const SIGNER_SEED: [u8; 32] = [0x19; 32];

/// Serialises this module's process-wide environment mutation with the
/// other fixtures that install a temp HOME (the suite may run several
/// tests in one process; nextest gives each its own).
static FIXTURE_ENV: Mutex<()> = Mutex::new(());

struct EnvRestore(Vec<(&'static str, Option<OsString>)>);

impl EnvRestore {
    fn install(root: &Path) -> Self {
        let entries = [
            ("HOME", root.join("home")),
            ("XDG_CONFIG_HOME", root.join("config")),
            ("XDG_STATE_HOME", root.join("xdg-state")),
            ("XDG_DATA_HOME", root.join("data")),
            ("XDG_CACHE_HOME", root.join("cache")),
            ("TMPDIR", root.to_path_buf()),
            ("CADENCE_SUITE_LOCK", root.join("locks/suite")),
            ("CADENCE_PM_DIR", root.join("pm")),
        ];
        for (key, path) in &entries {
            let dir = if *key == "CADENCE_SUITE_LOCK" {
                path.parent().unwrap()
            } else {
                path.as_path()
            };
            std::fs::create_dir_all(dir).unwrap();
        }
        let saved = entries
            .iter()
            .map(|(key, _)| (*key, std::env::var_os(key)))
            .collect::<Vec<_>>();
        // A hosted daemon captures its membership realm from these names,
        // and the seam build's fixture issuer transport from the TLS pair.
        // Ambient values would change this fixture's mode, so they are
        // removed for its lifetime and restored afterwards.
        let mut saved = saved;
        for key in [
            "AGENTICOS_COMPANY",
            "AGENTICOS_INSTANCE",
            "AGENTICOS_BOARD_HOST",
            "AGENTICOS_BOARD_ISSUER",
            "AGENTICOS_BOARD_COMPANY",
            "CADENCE_AGENTICOS_URL",
            "CADENCE_TEST_TLS_CA",
            "CADENCE_TEST_TLS_CONNECT",
        ] {
            saved.push((key, std::env::var_os(key)));
            std::env::remove_var(key);
        }
        for (key, value) in entries {
            std::env::set_var(key, value);
        }
        Self(saved)
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (key, value) in self.0.drain(..) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

struct Session {
    cookie: String,
    key: String,
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("board response is not JSON ({e}): {}", self.body))
    }

    fn cookie(&self) -> String {
        self.headers
            .iter()
            .find(|(name, _)| name == "set-cookie")
            .map(|(_, value)| {
                value
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            })
            .unwrap_or_else(|| panic!("response carried no session cookie: {}", self.body))
    }
}

/// One HTTP/1.0 request on a fresh loopback connection (no implicit cookie
/// jar, no chunk decoding). `Host` is the board origin the request claims,
/// so the public and loopback surfaces are distinguished exactly as a
/// browser would.
fn request(
    port: u16,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &str,
) -> Reply {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .unwrap_or_else(|e| panic!("connect to isolated board {port}: {e}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\
         Content-Length: {}\r\n",
        body.len()
    )
    .unwrap();
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n").unwrap();
    }
    write!(stream, "\r\n").unwrap();
    if !body.is_empty() {
        stream.write_all(body.as_bytes()).unwrap();
    }
    stream.flush().unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).expect("HTTP status line");
    assert!(!line.is_empty(), "board closed without an HTTP status line");
    let status: u16 = line
        .split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("malformed status line {line:?}"))
        .parse()
        .unwrap_or_else(|e| panic!("malformed status {line:?}: {e}"));
    let mut headers = Vec::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).expect("HTTP header line");
        assert!(read > 0, "board closed inside the HTTP headers");
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.trim_end().split_once(':') {
            headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    let mut body = String::new();
    reader
        .read_to_string(&mut body)
        .expect("HTTP response body");
    Reply {
        status,
        headers,
        body,
    }
}

struct Board {
    port: u16,
    host: String,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<cadence_agent::Result<()>>>,
}

impl Drop for Board {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct Fixture {
    _env: EnvRestore,
    _serial: MutexGuard<'static, ()>,
    root: TempDir,
    pm: PathBuf,
    state: PathBuf,
    seam: String,
    signer: Ed25519KeyPair,
    issuer: String,
    hosted: Option<Board>,
    operator: Option<Board>,
    daemon_stop: Arc<AtomicBool>,
    daemon: Option<JoinHandle<cadence_agent::Result<()>>>,
    jwks_stop: Arc<AtomicBool>,
    jwks: Option<JoinHandle<()>>,
}

/// Ports this fixture may use: the documented staging range, minus the
/// two slots `test_ports` reserves for other fixtures.
fn candidate_ports() -> impl Iterator<Item = u16> {
    (3110..=3199).filter(|port| !matches!(*port, 3139 | 3140))
}

impl Fixture {
    fn start() -> Self {
        let serial = FIXTURE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let root = Builder::new().prefix("c1901-").tempdir_in("/tmp").unwrap();
        let env = EnvRestore::install(root.path());
        let state = root.path().join("s");
        std::fs::create_dir_all(&state).unwrap();
        let pm = root.path().join("pm");
        Pm::init(&pm).unwrap();

        // A private project with one explicit member grant, and a
        // workspace-visible control project with no policy.
        let private = pm.join(PRIVATE_KEY);
        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(
            private.join("project.yaml"),
            format!(
                "key: {PRIVATE_KEY}\nprefix: C1901P\npolicy:\n  version: 1\n  \
                 visibility: private\n  members:\n  - users/{GRANTED_SUB}\n"
            ),
        )
        .unwrap();
        let open = pm.join(OPEN_KEY);
        std::fs::create_dir_all(&open).unwrap();
        std::fs::write(
            open.join("project.yaml"),
            format!("key: {OPEN_KEY}\nprefix: C1901O\n"),
        )
        .unwrap();

        write_memory(
            &pm,
            PRIVATE_KEY,
            SECRET_SLUG,
            &format!("{SECRET_FACT}\n\n**Why:** {SECRET_WHY}\n\n**How to apply:** {SECRET_HOW}\n"),
        );
        write_memory(
            &pm,
            OPEN_KEY,
            OPEN_SLUG,
            &format!(
                "{OPEN_FACT}\n\n**Why:** the visible control project stays readable\n\n\
                 **How to apply:** read it\n"
            ),
        );
        // Malformed on purpose: `load_project_report` reports
        // `<project>/<file>: <error>`. The error string is one of the
        // identifiers an ungranted caller must never receive.
        let broken = pm.join(PRIVATE_KEY).join("memory");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(
            broken.join(format!("{BROKEN_SLUG}.md")),
            format!(
                "---\nid: {BROKEN_SLUG}\ntype: rule\nstatus: accepted\ncreated: \
                 '2026-10-27T00:00:00Z'\n---\n\nthis record has no confidence field\n"
            ),
        )
        .unwrap();

        // Local platform JWKS: the daemon verifies every hosted assertion
        // against these bytes.
        let (jwks_server, jwks_port) = candidate_ports()
            .find_map(|port| {
                tiny_http::Server::http(("127.0.0.1", port))
                    .ok()
                    .map(|server| (server, port))
            })
            .expect("a free isolated port for the fixture JWKS");
        let issuer = format!("http://127.0.0.1:{jwks_port}");
        let signer = Ed25519KeyPair::from_seed_unchecked(&SIGNER_SEED).unwrap();
        let jwks_body = json!({"keys": [{
            "kty": "OKP", "crv": "Ed25519", "kid": KID,
            "x": URL_SAFE_NO_PAD.encode(signer.public_key().as_ref()),
        }]})
        .to_string();
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

        // Isolated daemon: the only session store and project-memory reader.
        let provider_env = ProviderEnv::refusing_providers();
        provider_env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let daemon_stop = Arc::new(AtomicBool::new(false));
        let daemon_opts = daemon::ServeOptions {
            provider_env,
            stop: Some(Arc::clone(&daemon_stop)),
            test_seam: true,
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
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(1)).is_ok()
                && Seam::token_at(&state).is_some()
            {
                break;
            }
            if daemon.is_finished() {
                panic!("isolated daemon exited before becoming ready");
            }
            assert!(
                Instant::now() < deadline,
                "isolated daemon readiness timed out"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let seam = Seam::token_at(&state).expect("armed daemon mints the seam credential");

        let hosted = start_hosted_board(&state, &pm, &issuer, jwks_port);
        let operator = start_operator_board(&state, &pm, &[jwks_port, hosted.port]);

        Self {
            _env: env,
            _serial: serial,
            root,
            pm,
            state,
            seam,
            signer,
            issuer,
            hosted: Some(hosted),
            operator: Some(operator),
            daemon_stop,
            daemon: Some(daemon),
            jwks_stop,
            jwks: Some(jwks),
        }
    }

    fn assertion(&self, sub: &str, jti: &str) -> String {
        let now = cadence_agent::issue::time::now_epoch();
        let b64 = URL_SAFE_NO_PAD;
        let header = json!({"alg": "EdDSA", "typ": "JWT", "kid": KID}).to_string();
        let claims = json!({
            "iss": self.issuer,
            "aud": PUBLIC_HOST,
            "sub": sub,
            "email": format!("{sub}@example.invalid"),
            "name": format!("{sub} fixture"),
            "company": COMPANY,
            "role": "member",
            // board_identity::MAX_LIFE_SECS is 60: the assertion's own
            // lifetime (exp - iat) must not exceed it, or the daemon
            // answers `assertion_expired` even though exp is in the future.
            "iat": now - 1,
            "exp": now + 55,
            "jti": jti,
        })
        .to_string();
        let signed = format!("{}.{}", b64.encode(header), b64.encode(claims));
        format!(
            "{signed}.{}",
            b64.encode(self.signer.sign(signed.as_bytes()).as_ref())
        )
    }

    /// A real hosted named member session: signed assertion, platform
    /// JWKS verification by the daemon, `__Host-` cookie back.
    fn hosted_session(&self, sub: &str, jti: &str) -> String {
        let board = self.hosted.as_ref().expect("hosted board");
        let reply = request(
            board.port,
            "POST",
            PUBLIC_HOST,
            "/__platform/session",
            &[
                ("Origin", format!("http://{PUBLIC_HOST}")),
                ("Sec-Fetch-Site", "same-origin".to_string()),
                ("Content-Type", "application/json".to_string()),
            ],
            &json!({"assertion": self.assertion(sub, jti)}).to_string(),
        );
        assert_eq!(
            reply.status, 200,
            "hosted session for {sub} must open: {}",
            reply.body
        );
        reply.cookie()
    }

    /// A real operator session on the board's own loopback name: the
    /// daemon mints the one-use link, the board's exchange returns the
    /// cookie and page key.
    fn operator_session(&self) -> Session {
        operator_auth::ensure_secret(&self.state).unwrap();
        let secret = operator_auth::read_secret(&self.state).unwrap();
        let link = scoped(Asserted::Operator, || {
            client::rpc(
                &self.state,
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )
        })
        .unwrap();
        let nonce = link["nonce"]
            .as_str()
            .expect("operator link nonce")
            .to_string();
        let board = self.operator.as_ref().expect("operator board");
        let host = board.host.clone();
        let reply = request(
            board.port,
            "POST",
            &host,
            "/api/session",
            &[
                ("Origin", format!("http://{host}")),
                ("X-Cadence-Board", "1".to_string()),
                ("Content-Type", "application/json".to_string()),
                (AS_HEADER, "operator".to_string()),
                (TOKEN_HEADER, self.seam.clone()),
            ],
            &json!({"nonce": nonce}).to_string(),
        );
        assert_eq!(
            reply.status, 200,
            "operator session must open: {}",
            reply.body
        );
        let key = reply.json()["session_key"]
            .as_str()
            .expect("operator session key")
            .to_string();
        Session {
            cookie: reply.cookie(),
            key,
        }
    }

    fn hosted_get(&self, cookie: &str, path: &str) -> Reply {
        let board = self.hosted.as_ref().expect("hosted board");
        request(
            board.port,
            "GET",
            PUBLIC_HOST,
            path,
            &[("Cookie", cookie.to_string())],
            "",
        )
    }

    /// The same hosted read with no cookie at all (missing caller proof).
    fn hosted_get_anon(&self, path: &str) -> Reply {
        let board = self.hosted.as_ref().expect("hosted board");
        request(board.port, "GET", PUBLIC_HOST, path, &[], "")
    }

    fn operator_get(&self, session: &Session, path: &str) -> Reply {
        let board = self.operator.as_ref().expect("operator board");
        let host = board.host.clone();
        request(
            board.port,
            "GET",
            &host,
            path,
            &[
                ("Cookie", session.cookie.clone()),
                ("X-Cadence-Session", session.key.clone()),
                ("Origin", format!("http://{host}")),
                (AS_HEADER, "operator".to_string()),
                (TOKEN_HEADER, self.seam.clone()),
            ],
            "",
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Boards first, then their daemon, then the JWKS endpoint.
        if let Some(board) = self.hosted.take() {
            drop(board);
        }
        if let Some(board) = self.operator.take() {
            drop(board);
        }
        self.daemon_stop.store(true, SeqCst);
        if let Some(daemon) = self.daemon.take() {
            let _ = daemon.join();
        }
        self.jwks_stop.store(true, SeqCst);
        if let Some(jwks) = self.jwks.take() {
            let _ = jwks.join();
        }
        let _ = self.root.path();
    }
}

fn write_memory(pm: &Path, project: &str, slug: &str, body: &str) {
    let dir = pm.join(project).join("memory");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{slug}.md")),
        format!(
            "---\nid: {slug}\ntype: rule\nstatus: accepted\nscope:\n  project: true\n\
             confidence: high\ncreated: '2026-10-27T00:00:00Z'\n---\n\n{body}"
        ),
    )
    .unwrap();
}

fn start_hosted_board(state: &Path, pm: &Path, issuer: &str, used: u16) -> Board {
    let mut last = String::from("no candidate port was free");
    for port in candidate_ports().filter(|port| *port != used) {
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_err() {
            last = format!("port {port} is held");
            continue;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (startup, ready) = std::sync::mpsc::channel();
        let opts = ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            // Reads only: no delivery scan runs against the shared tracker.
            read_only: true,
            allow_hosts: vec![PUBLIC_HOST.into()],
            allow_origins: vec![format!("http://{PUBLIC_HOST}")],
            public: Some(ui::PublicBoard {
                host: PUBLIC_HOST.into(),
                issuer: issuer.into(),
                company: COMPANY.into(),
                authorize_url: format!("{issuer}/authorize"),
            }),
            test_seam: true,
            stop: Some(Arc::clone(&stop)),
            startup: Some(startup),
            gh: Some("/bin/false".into()),
            ..Default::default()
        };
        let (board_state, board_pm) = (state.to_path_buf(), pm.to_path_buf());
        let handle = std::thread::spawn(move || ui::serve(&board_state, &board_pm, &opts));
        match ready.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(())) => {
                return Board {
                    port,
                    host: PUBLIC_HOST.into(),
                    stop,
                    handle: Some(handle),
                }
            }
            Ok(Err(kind)) => {
                stop.store(true, SeqCst);
                let _ = handle.join();
                last = format!("port {port}: {kind:?}");
            }
            Err(e) => {
                stop.store(true, SeqCst);
                let _ = handle.join();
                panic!("hosted board on {port} did not report readiness: {e}");
            }
        }
    }
    panic!("no isolated hosted board port in 3110-3199 ({last})");
}

fn start_operator_board(state: &Path, pm: &Path, used: &[u16]) -> Board {
    let mut last = String::from("no candidate port was free");
    for port in candidate_ports().filter(|port| !used.contains(port)) {
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_err() {
            last = format!("port {port} is held");
            continue;
        }
        let host = format!("cadence-{port}.localhost:{port}");
        let stop = Arc::new(AtomicBool::new(false));
        let (startup, ready) = std::sync::mpsc::channel();
        let opts = ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            read_only: true,
            allow_hosts: vec![host.clone()],
            test_seam: true,
            stop: Some(Arc::clone(&stop)),
            startup: Some(startup),
            gh: Some("/bin/false".into()),
            ..Default::default()
        };
        let (board_state, board_pm) = (state.to_path_buf(), pm.to_path_buf());
        let handle = std::thread::spawn(move || ui::serve(&board_state, &board_pm, &opts));
        match ready.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(())) => {
                return Board {
                    port,
                    host,
                    stop,
                    handle: Some(handle),
                }
            }
            Ok(Err(kind)) => {
                stop.store(true, SeqCst);
                let _ = handle.join();
                last = format!("port {port}: {kind:?}");
            }
            Err(e) => {
                stop.store(true, SeqCst);
                let _ = handle.join();
                panic!("operator board on {port} did not report readiness: {e}");
            }
        }
    }
    panic!("no isolated operator board port in 3110-3199 ({last})");
}

fn check(failures: &mut Vec<String>, name: &str, ok: bool, detail: impl std::fmt::Debug) {
    println!(
        "{} {name}: {detail:?}",
        if ok { "OBSERVED_OK" } else { "OBSERVED_FAIL" }
    );
    if !ok {
        failures.push(name.to_string());
    }
}

fn projects_of(reply: &Reply) -> Vec<String> {
    reply
        .json()
        .get("memories")
        .and_then(Value::as_array)
        .map(|cards| {
            cards
                .iter()
                .filter_map(|card| card["project"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn ungranted_member_list_and_detail_hide_private_project_memory() {
    let fx = Fixture::start();
    let mut failures = Vec::new();

    let ungranted = fx.hosted_session(UNGRANTED_SUB, "cad1901-jti-ungranted");
    let granted = fx.hosted_session(GRANTED_SUB, "cad1901-jti-granted");
    let operator = fx.operator_session();

    let secret_path = fx
        .pm
        .join(PRIVATE_KEY)
        .join("memory")
        .join(format!("{SECRET_SLUG}.md"));
    let secret_path = secret_path.to_string_lossy().into_owned();
    let leaks_secret = |body: &str| {
        body.contains(SECRET_FACT)
            || body.contains(SECRET_WHY)
            || body.contains(SECRET_HOW)
            || body.contains(SECRET_SLUG)
            || body.contains(BROKEN_SLUG)
            || body.contains(secret_path.as_str())
    };

    // 1. The ungranted member's list: visible control rows stay, the
    //    private project contributes no card, error, identifier or path.
    let list = fx.hosted_get(&ungranted, "/api/memories");
    check(
        &mut failures,
        "ungranted list is served to a valid hosted member",
        list.status == 200,
        (list.status, &list.body),
    );
    let projects = projects_of(&list);
    check(
        &mut failures,
        "ungranted list keeps the workspace-visible project's memory",
        projects.iter().any(|project| project.as_str() == OPEN_KEY),
        &projects,
    );
    check(
        &mut failures,
        "ungranted list drops every private-project card",
        !projects
            .iter()
            .any(|project| project.as_str() == PRIVATE_KEY),
        &projects,
    );
    check(
        &mut failures,
        "ungranted list discloses no private memory identifier, body or path",
        !leaks_secret(&list.body),
        &list.body,
    );
    check(
        &mut failures,
        "ungranted list discloses neither the private project nor its malformed-record error",
        !list.body.contains(PRIVATE_KEY),
        &list.body,
    );

    // The same attack narrowed to the restricted project by query.
    let targeted = fx.hosted_get(&ungranted, &format!("/api/memories?project={PRIVATE_KEY}"));
    if targeted.status == 200 {
        check(
            &mut failures,
            "targeted ungranted list contributes no private cards",
            targeted
                .json()
                .get("memories")
                .and_then(Value::as_array)
                .is_none_or(|cards| cards.is_empty()),
            &targeted.body,
        );
    }
    check(
        &mut failures,
        "targeted ungranted list discloses no private memory content",
        !leaks_secret(&targeted.body),
        &targeted.body,
    );

    // 2. Detail for the private record must answer as absent. The
    //    reference is an unknown slug in a project this session may read,
    //    so the status is the route's real "no such memory" answer.
    let unknown = fx.hosted_get(
        &ungranted,
        &format!("/api/memories/{OPEN_KEY}/{UNKNOWN_SLUG}"),
    );
    check(
        &mut failures,
        "an unknown slug in a readable project answers 404",
        unknown.status == 404,
        (unknown.status, &unknown.body),
    );
    let detail = fx.hosted_get(
        &ungranted,
        &format!("/api/memories/{PRIVATE_KEY}/{SECRET_SLUG}"),
    );
    check(
        &mut failures,
        "ungranted private detail is not a read",
        detail.status != 200,
        (detail.status, &detail.body),
    );
    check(
        &mut failures,
        "ungranted private detail answers exactly as an absent memory",
        detail.status == unknown.status,
        (detail.status, unknown.status, &detail.body, &unknown.body),
    );
    let detail_json = detail.json();
    let unknown_json = unknown.json();
    let shape = |value: &Value| {
        value
            .as_object()
            .map(|object| object.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    };
    check(
        &mut failures,
        "ungranted private detail carries the unknown answer's shape",
        shape(&detail_json) == shape(&unknown_json),
        (&detail_json, &unknown_json),
    );
    check(
        &mut failures,
        "ungranted private detail discloses no body, fact or path",
        !leaks_secret(&detail.body),
        &detail.body,
    );
    // "Responds as absent" is the same answer an ordinary missing memory
    // gets: the generic body must be byte-identical, so neither the
    // restricted project nor the requested slug is echoed back.
    check(
        &mut failures,
        "ungranted private detail is the same generic answer as an absent memory",
        detail.body == unknown.body,
        (&detail.body, &unknown.body),
    );
    check(
        &mut failures,
        "ungranted private detail echoes neither the project nor the requested slug",
        !detail.body.contains(PRIVATE_KEY) && !detail.body.contains(SECRET_SLUG),
        &detail.body,
    );

    // The same session still reads the visible project's detail, so the
    // refusal above is policy, not a broken route or session.
    let open_detail = fx.hosted_get(&ungranted, &format!("/api/memories/{OPEN_KEY}/{OPEN_SLUG}"));
    check(
        &mut failures,
        "ungranted member still reads a visible project's memory detail",
        open_detail.status == 200 && open_detail.body.contains(OPEN_FACT),
        (open_detail.status, &open_detail.body),
    );

    // 3. The explicitly granted member reads the private record through
    //    both routes.
    let granted_list = fx.hosted_get(&granted, "/api/memories");
    let granted_projects = projects_of(&granted_list);
    check(
        &mut failures,
        "granted member's list includes the private project",
        granted_list.status == 200
            && granted_projects
                .iter()
                .any(|project| project.as_str() == PRIVATE_KEY),
        (granted_list.status, &granted_projects),
    );
    let granted_card = granted_list
        .json()
        .get("memories")
        .and_then(Value::as_array)
        .and_then(|cards| {
            cards
                .iter()
                .find(|card| card["slug"].as_str() == Some(SECRET_SLUG))
                .cloned()
        });
    check(
        &mut failures,
        "granted member's list carries the private card and its file path",
        granted_card
            .as_ref()
            .is_some_and(|card| card["path"].as_str() == Some(secret_path.as_str())),
        &granted_card,
    );
    let granted_detail = fx.hosted_get(
        &granted,
        &format!("/api/memories/{PRIVATE_KEY}/{SECRET_SLUG}"),
    );
    let granted_json = granted_detail.json();
    check(
        &mut failures,
        "granted member reads the private memory body",
        granted_detail.status == 200
            && granted_json["project"].as_str() == Some(PRIVATE_KEY)
            && granted_json["slug"].as_str() == Some(SECRET_SLUG)
            && granted_json["body"]
                .as_str()
                .is_some_and(|body| body.contains(SECRET_FACT)),
        (granted_detail.status, &granted_detail.body),
    );

    // 4. The operator keeps administrative read access.
    let operator_list = fx.operator_get(&operator, "/api/memories");
    let operator_projects = projects_of(&operator_list);
    check(
        &mut failures,
        "operator's list includes the private project",
        operator_list.status == 200
            && operator_projects
                .iter()
                .any(|project| project.as_str() == PRIVATE_KEY),
        (operator_list.status, &operator_projects),
    );
    let operator_detail = fx.operator_get(
        &operator,
        &format!("/api/memories/{PRIVATE_KEY}/{SECRET_SLUG}"),
    );
    check(
        &mut failures,
        "operator reads the private memory body",
        operator_detail.status == 200
            && operator_detail.body.contains(SECRET_FACT)
            && operator_detail.body.contains(SECRET_WHY),
        (operator_detail.status, &operator_detail.body),
    );

    // 5. Missing and forged hosted sessions are refused before any memory
    //    route runs: no content from either project may leave. The status
    //    is not assumed (the refusal may be 401 or a bounce); only "not a
    //    successful read" and "no memory content" are required.
    let forged_cookie = "__Host-aos-board-session=cad1901-forged-session-token";
    let missing_list = fx.hosted_get_anon("/api/memories");
    let forged_list = fx.hosted_get(forged_cookie, "/api/memories");
    for (label, reply) in [
        ("missing-session list", &missing_list),
        ("forged-session list", &forged_list),
    ] {
        check(
            &mut failures,
            &format!("{label} is refused"),
            reply.status != 200,
            (reply.status, &reply.body),
        );
        check(
            &mut failures,
            &format!("{label} discloses no memory content"),
            !reply.body.contains(OPEN_FACT)
                && !reply.body.contains(PRIVATE_KEY)
                && !leaks_secret(&reply.body),
            &reply.body,
        );
    }
    let private_detail_path = format!("/api/memories/{PRIVATE_KEY}/{SECRET_SLUG}");
    let missing_detail = fx.hosted_get_anon(&private_detail_path);
    let forged_detail = fx.hosted_get(forged_cookie, &private_detail_path);
    for (label, reply) in [
        ("missing-session detail", &missing_detail),
        ("forged-session detail", &forged_detail),
    ] {
        check(
            &mut failures,
            &format!("{label} is refused"),
            reply.status != 200,
            (reply.status, &reply.body),
        );
        check(
            &mut failures,
            &format!("{label} discloses no memory content"),
            !reply.body.contains(OPEN_FACT) && !leaks_secret(&reply.body),
            &reply.body,
        );
    }

    // 6. Malformed private policy: the canonical project parser refuses
    //    the file, so every caller (the granted member and the operator
    //    included) fails closed. Status is not assumed; no private memory
    //    content may leave. Restoring is not required before drop.
    let private_yaml = fx.pm.join(PRIVATE_KEY).join("project.yaml");
    std::fs::write(
        &private_yaml,
        format!(
            "key: {PRIVATE_KEY}\nprefix: C1901P\npolicy:\n  version: 1\n  \
             visibility: secret\n  members:\n  - users/{GRANTED_SUB}\n"
        ),
    )
    .unwrap();
    let malformed_list = fx.hosted_get(&granted, "/api/memories");
    check(
        &mut failures,
        "malformed private policy list discloses no private memory",
        !malformed_list.body.contains(PRIVATE_KEY) && !leaks_secret(&malformed_list.body),
        &malformed_list.body,
    );
    let malformed_detail = fx.hosted_get(&granted, &private_detail_path);
    check(
        &mut failures,
        "malformed private policy detail is not a successful read",
        malformed_detail.status != 200,
        (malformed_detail.status, &malformed_detail.body),
    );
    check(
        &mut failures,
        "malformed private policy detail discloses no private memory",
        !leaks_secret(&malformed_detail.body),
        &malformed_detail.body,
    );
    let malformed_operator_list = fx.operator_get(&operator, "/api/memories");
    check(
        &mut failures,
        "malformed private policy operator list discloses no private memory",
        !malformed_operator_list.body.contains(PRIVATE_KEY)
            && !leaks_secret(&malformed_operator_list.body),
        &malformed_operator_list.body,
    );
    let malformed_operator_detail = fx.operator_get(&operator, &private_detail_path);
    check(
        &mut failures,
        "malformed private policy operator detail is not a successful read",
        malformed_operator_detail.status != 200,
        (
            malformed_operator_detail.status,
            &malformed_operator_detail.body,
        ),
    );
    check(
        &mut failures,
        "malformed private policy operator detail discloses no private memory",
        !leaks_secret(&malformed_operator_detail.body),
        &malformed_operator_detail.body,
    );

    // 7. Unreadable private policy: `project.yaml` is not a readable
    //    regular file, so canonical `project::list` omits that project
    //    and leaves the others readable. No private memory may leave, and
    //    the still-readable project stays readable when the list is 200.
    std::fs::remove_file(&private_yaml).unwrap();
    std::fs::create_dir(&private_yaml).unwrap();
    let unreadable_list = fx.hosted_get(&granted, "/api/memories");
    check(
        &mut failures,
        "unreadable private policy list discloses no private memory",
        !unreadable_list.body.contains(PRIVATE_KEY) && !leaks_secret(&unreadable_list.body),
        &unreadable_list.body,
    );
    if unreadable_list.status == 200 {
        let projects = projects_of(&unreadable_list);
        check(
            &mut failures,
            "unreadable private policy list keeps the readable project's memory",
            projects.iter().any(|project| project.as_str() == OPEN_KEY),
            &projects,
        );
    }
    let unreadable_detail = fx.hosted_get(&granted, &private_detail_path);
    check(
        &mut failures,
        "unreadable private policy detail is not a successful read",
        unreadable_detail.status != 200,
        (unreadable_detail.status, &unreadable_detail.body),
    );
    check(
        &mut failures,
        "unreadable private policy detail discloses no private memory",
        !leaks_secret(&unreadable_detail.body),
        &unreadable_detail.body,
    );

    assert!(
        failures.is_empty(),
        "CAD-1901 acceptance failures: {failures:#?}"
    );
}
