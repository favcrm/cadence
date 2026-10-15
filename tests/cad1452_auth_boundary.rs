//! CAD-1452: independently authored from the approved private-board acceptance.
//! Implementation authors must not edit or weaken this check.
//!
//! Exercises the real HTTP dispatcher and real daemon login/session store.
//! The supported test seam supplies only the board's native operator peer;
//! it never supplies a browser session, tab key, or read authorization.
//! Hosted coverage is the container's no-cookie boundary, not the AgenticOS
//! edge, identity-provider handoff, or browser/sessionStorage behavior.
//!
//! Run in an admitted test slot:
//! CARGO_BUILD_JOBS=4 cargo test --features test-seam \
//!   --test cad1452_auth_boundary -- --nocapture --test-threads 2
#![cfg(all(feature = "test-seam", target_os = "linux"))]

use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{adapter::ProviderEnv, client, daemon, issue::Pm, operator_auth, ui};
use clap::Parser;
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const PRIVATE_PROJECT: &str = "cad1452-private-workspace";
const PUBLIC_HOST: &str = "cad1452.board.invalid";

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
            .collect();
        for (key, value) in entries {
            std::env::set_var(key, value);
        }
        // Toolchain homes and caller/managed identity variables are untouched.
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

struct Fixture {
    _env: EnvRestore,
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    daemon: Option<JoinHandle<cadence_agent::Result<()>>>,
    seam: String,
}

impl Fixture {
    fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("c1452-")
            .tempdir_in("/tmp")
            .unwrap();
        let env = EnvRestore::install(root.path());
        let pm = root.path().join("pm");
        Pm::init(&pm).unwrap();
        let project = pm.join(PRIVATE_PROJECT);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("project.yaml"),
            format!("key: {PRIVATE_PROJECT}\nprefix: C1452\n"),
        )
        .unwrap();
        let mut fixture = Self {
            _env: env,
            root,
            stop: Arc::new(AtomicBool::new(false)),
            daemon: None,
            seam: String::new(),
        };
        let provider_env = ProviderEnv::refusing_providers();
        provider_env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let opts = daemon::ServeOptions {
            provider_env,
            stop: Some(fixture.stop.clone()),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        let state = fixture.root.path().join("s");
        fixture.daemon = Some(thread::spawn(move || daemon::serve_with(&state, opts)));
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let state = fixture.root.path().join("s");
            if client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(1)).is_ok() {
                if let Some(token) = Seam::token_at(&state) {
                    fixture.seam = token;
                    break;
                }
            }
            if fixture.daemon.as_ref().unwrap().is_finished() {
                let outcome = fixture.daemon.take().unwrap().join();
                panic!("isolated daemon exited before becoming ready: {outcome:?}");
            }
            assert!(
                Instant::now() < deadline,
                "isolated daemon readiness timed out after 30 seconds"
            );
            thread::sleep(Duration::from_millis(50));
        }
        fixture
    }

    fn board(&self, read_only: bool, allow_anonymous_reads: bool, hosted: bool) -> Board {
        for port in 3110..=3199 {
            let stop = Arc::new(AtomicBool::new(false));
            let (startup, ready) = std::sync::mpsc::channel();
            let host = if hosted {
                PUBLIC_HOST.to_string()
            } else {
                format!("cadence-{port}.localhost:{port}")
            };
            let opts = ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                read_only,
                allow_anonymous_reads,
                allow_hosts: vec![host.clone()],
                board_public_only: hosted,
                public: hosted.then(|| ui::PublicBoard {
                    host: host.clone(),
                    issuer: "https://issuer.invalid".into(),
                    company: "cad1452-fixture-company".into(),
                    authorize_url: "https://issuer.invalid/authorize".into(),
                }),
                stop: Some(stop.clone()),
                startup: Some(startup),
                test_seam: true,
                gh: Some("/bin/false".into()),
                ..Default::default()
            };
            let state = self.root.path().join("s");
            let pm = self.root.path().join("pm");
            let handle = thread::spawn(move || ui::serve(&state, &pm, &opts));
            match ready.recv_timeout(Duration::from_secs(30)) {
                Ok(Ok(())) => {
                    return Board {
                        port,
                        host,
                        seam: self.seam.clone(),
                        stop,
                        handle: Some(handle),
                    };
                }
                Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                    handle.join().unwrap().unwrap_err();
                }
                other => {
                    stop.store(true, SeqCst);
                    panic!("isolated board failed to become ready: {other:?}");
                }
            }
        }
        panic!("no isolated board port available in 3110..3199");
    }

    fn login(&self, board: &Board) -> Session {
        let state = self.root.path().join("s");
        operator_auth::ensure_secret(&state).unwrap();
        let secret = operator_auth::read_secret(&state).unwrap();
        let link = scoped(Asserted::Operator, || {
            client::rpc(
                &state,
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )
        })
        .unwrap();
        let reply = board.request(
            "POST",
            "/api/session",
            None,
            None,
            &json!({"nonce": link["nonce"]}).to_string(),
        );
        assert_eq!(reply.status, 200, "real login exchange: {}", reply.body);
        let key = reply.json()["session_key"].as_str().unwrap().to_owned();
        let cookie = reply
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
            .expect("real login must issue a session cookie")
            .1
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        assert!(!key.is_empty());
        Session { cookie, key }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(handle) = self.daemon.take() {
            let _ = handle.join();
        }
    }
}

struct Session {
    cookie: String,
    key: String,
}

struct Board {
    port: u16,
    host: String,
    seam: String,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<cadence_agent::Result<()>>>,
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("expected JSON response: {e}: {}", self.body))
    }

    fn refused(&self, code: &str) {
        assert_eq!(
            self.status, 401,
            "private read must be refused: {}",
            self.body
        );
        let body = self.json();
        let actual_code = body["code"]
            .as_str()
            .or_else(|| body["error"]["code"].as_str());
        assert_eq!(actual_code, Some(code), "{}", self.body);
        assert!(!self.body.contains(PRIVATE_PROJECT));
        assert!(!self.body.contains("event: hello"));
    }

    fn stream_open(&self) {
        assert_eq!(self.status, 200, "authorized SSE: {}", self.body);
        assert!(self.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("content-type") && value.starts_with("text/event-stream")
        }));
        assert!(self.body.contains("event: hello"), "{}", self.body);
    }
}

impl Board {
    // Each call opens a fresh connection, with no implicit cookie jar. A
    // repeat with the original cookie/key is the server side of a tab reload.
    // HTTP/1.0 avoids chunk decoding; SSE is read only through its first event
    // so accidental admission fails promptly rather than hanging the test.
    fn request(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        key: Option<&str>,
        body: &str,
    ) -> Reply {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        write!(
            stream,
            "{method} {path} HTTP/1.0\r\nHost: {}\r\nOrigin: http://{}\r\nX-Cadence-Board: 1\r\n{AS_HEADER}: operator\r\n{TOKEN_HEADER}: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n",
            self.host, self.host, self.seam, body.len()
        )
        .unwrap();
        if let Some(cookie) = cookie {
            write!(stream, "Cookie: {cookie}\r\n").unwrap();
        }
        if let Some(key) = key {
            write!(stream, "X-Cadence-Session: {key}\r\n").unwrap();
        }
        write!(stream, "\r\n{body}").unwrap();
        stream.flush().unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut headers = Vec::new();
        loop {
            line.clear();
            assert_ne!(
                reader.read_line(&mut line).unwrap(),
                0,
                "incomplete HTTP headers"
            );
            if line == "\r\n" || line == "\n" {
                break;
            }
            let (name, value) = line.trim_end().split_once(':').unwrap();
            headers.push((name.to_owned(), value.trim().to_owned()));
        }
        let mut body = String::new();
        if headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("content-type") && value.starts_with("text/event-stream")
        }) {
            loop {
                line.clear();
                assert_ne!(
                    reader.read_line(&mut line).unwrap(),
                    0,
                    "SSE closed before hello"
                );
                body.push_str(&line);
                if line.trim().is_empty() {
                    break;
                }
            }
        } else if method != "HEAD" {
            reader.read_to_string(&mut body).unwrap();
        }
        Reply {
            status,
            headers,
            body,
        }
    }

    fn get(&self, path: &str, session: Option<&Session>) -> Reply {
        self.request(
            "GET",
            path,
            session.map(|s| s.cookie.as_str()),
            session.map(|s| s.key.as_str()),
            "",
        )
    }

    fn projects_visible(&self, session: Option<&Session>) {
        let reply = self.get("/api/projects", session);
        assert_eq!(reply.status, 200, "positive private read: {}", reply.body);
        assert!(
            reply.json()["projects"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| { p["key"] == PRIVATE_PROJECT }),
            "positive control must return the isolated private project: {}",
            reply.body
        );
    }
}

impl Drop for Board {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Parser)]
struct PolicyArgs {
    #[command(flatten)]
    flags: ui::UiFlags,
}

#[test]
fn private_reads_require_live_tab_session_unless_local_anonymous_policy_is_explicit() {
    // The policy is deliberate CLI configuration, not a consequence of
    // read_only or a failed session probe. Both enable and clear must exist.
    assert!(
        !PolicyArgs::parse_from(["fixture"])
            .flags
            .allow_anonymous_reads
    );
    assert!(
        PolicyArgs::parse_from(["fixture", "--allow-anonymous-reads"])
            .flags
            .allow_anonymous_reads
    );
    assert!(
        PolicyArgs::parse_from(["fixture", "--no-allow-anonymous-reads"])
            .flags
            .no_allow_anonymous_reads
    );

    let fixture = Fixture::start();
    {
        let board = fixture.board(false, false, false);
        let meta = board.get("/api/meta", None);
        assert_eq!(
            meta.status, 200,
            "unsigned bootstrap must work: {}",
            meta.body
        );
        let meta_json = meta.json();
        assert_eq!(meta_json["signed_in"], false);
        assert_eq!(meta_json["anonymous_reads"], false);
        assert_eq!(meta_json["hosted"], false);
        assert!(!meta.body.contains(PRIVATE_PROJECT));
        for field in [
            "projects", "issues", "records", "overview", "agents", "pm_dir",
        ] {
            assert!(
                meta_json.get(field).is_none_or(Value::is_null),
                "bootstrap leaked {field}"
            );
        }
        let health = board.get("/api/health", None);
        assert_eq!(
            health.status, 200,
            "unsigned liveness must work: {}",
            health.body
        );
        let health_json = health.json();
        for field in ["projects", "issues", "pm_dir"] {
            assert!(
                health_json.get(field).is_none_or(Value::is_null),
                "unsigned health leaked {field}"
            );
        }
        assert!(!health.body.contains(PRIVATE_PROJECT));

        // Both collection data and long-lived observations must be refused,
        // even when the board-to-daemon native peer is a genuine operator.
        for route in [
            "/api/projects",
            "/api/issues",
            "/api/overview",
            "/api/agents",
            "/api/stream",
        ] {
            board.get(route, None).refused("operator_session_required");
        }
        assert_eq!(
            board
                .request("HEAD", "/api/projects", None, None, "")
                .status,
            401
        );

        let tab = fixture.login(&board);
        let other_tab = fixture.login(&board);
        let cookie_name = tab.cookie.split_once('=').unwrap().0;
        let forged_cookie = format!("{cookie_name}=forged");
        for route in ["/api/projects", "/api/stream"] {
            for (cookie, key) in [
                (Some(tab.cookie.as_str()), None),
                (None, Some(tab.key.as_str())),
                (Some(tab.cookie.as_str()), Some("forged-tab-key")),
                (Some(tab.cookie.as_str()), Some(other_tab.key.as_str())),
                (Some(forged_cookie.as_str()), Some(tab.key.as_str())),
            ] {
                board
                    .request("GET", route, cookie, key, "")
                    .refused("operator_session_required");
            }
        }

        // Successful login and two fresh HTTP connections rule out a server
        // that simply refuses everything. No new login occurs on re-entry.
        board.projects_visible(Some(&tab));
        let before = board.get("/api/meta", Some(&tab)).json();
        assert_eq!(before["signed_in"], true);
        assert!(before["session"].is_object());
        board.get("/api/stream", Some(&tab)).stream_open();
        board.projects_visible(Some(&tab));
        let after = board.get("/api/meta", Some(&tab)).json();
        assert_eq!(after["signed_in"], true);
        for field in ["id", "origin", "created", "expires_at"] {
            assert!(
                !before["session"][field].is_null(),
                "missing session {field}"
            );
            assert_eq!(
                after["session"][field], before["session"][field],
                "reload must reuse the same session and absolute expiry: {field}"
            );
        }

        let logout = board.request(
            "POST",
            "/api/session/logout",
            Some(&tab.cookie),
            Some(&tab.key),
            "{}",
        );
        assert!(
            (200..300).contains(&logout.status),
            "real logout must succeed: {} {}",
            logout.status,
            logout.body
        );
        for route in ["/api/projects", "/api/stream"] {
            board
                .get(route, Some(&tab))
                .refused("operator_session_required");
        }
        board.projects_visible(Some(&other_tab));
    }
    {
        // Authenticated read-only is private; write prohibition alone must
        // never authorize an unsigned reader.
        let board = fixture.board(true, false, false);
        assert_eq!(
            board.get("/api/meta", None).json()["anonymous_reads"],
            false
        );
        board
            .get("/api/projects", None)
            .refused("operator_session_required");
        board
            .get("/api/stream", None)
            .refused("operator_session_required");
        let tab = fixture.login(&board);
        board.projects_visible(Some(&tab));
    }
    {
        let board = fixture.board(true, true, false);
        let meta = board.get("/api/meta", None).json();
        assert_eq!(meta["signed_in"], false);
        assert_eq!(meta["read_only"], true);
        assert_eq!(meta["anonymous_reads"], true);
        board.projects_visible(None);
        board.get("/api/stream", None).stream_open();
        assert_eq!(
            board
                .request("POST", "/api/issues", None, None, "{}")
                .status,
            403
        );
    }
    {
        // An explicit LOCAL browsing policy can never turn a hosted board
        // into a public one. No issuer or remote service is needed to refuse
        // a request that has no hosted cookie.
        let board = fixture.board(true, true, true);
        for route in ["/api/meta", "/api/projects", "/api/issues", "/api/stream"] {
            board.get(route, None).refused("board_session_required");
        }
        assert_eq!(board.get("/api/health", None).status, 200);
    }
}
