//! CAD-1638: independently authored acceptance check for the hosted board.
//! The implementer must not edit or weaken this check.
//!
//! A hosted board with no session cookie and no session key must refuse the
//! former anonymous directory surface (`/apps/explore`, the catalogue SPA
//! route, the catalogue proxy under `/api/public-apps*`, and the bundle at
//! `/assets/index.js`) with `board_session_required`. The sign-in protocol
//! (`/__platform/login`, `/__platform/callback`) and liveness (`/api/health`)
//! must still answer.
//!
//! Hosted coverage is the container's no-cookie boundary only. A positive
//! signed-in catalogue read is deliberately NOT exercised here: it needs the
//! platform assertion exchange and would contact the catalogue origin. The
//! signed-in catalogue is covered by the existing hosted boundary and the UI
//! gate (ui/tests/authBoundaryDirectory.test.tsx).
//!
//! Run in an admitted test slot:
//! CARGO_BUILD_JOBS=4 cargo test --features test-seam \
//!   --test cad1638_guest_directory_refusal -- --nocapture --test-threads 2
#![cfg(all(feature = "test-seam", target_os = "linux"))]

use cadence_agent::test_seam::{Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{adapter::ProviderEnv, client, daemon, issue::Pm, ui};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const PRIVATE_PROJECT: &str = "cad1638-private-workspace";
const PUBLIC_HOST: &str = "cad1638.board.invalid";

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
            .prefix("c1638-")
            .tempdir_in("/tmp")
            .unwrap();
        let env = EnvRestore::install(root.path());
        let pm = root.path().join("pm");
        Pm::init(&pm).unwrap();
        let project = pm.join(PRIVATE_PROJECT);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("project.yaml"),
            format!("key: {PRIVATE_PROJECT}\nprefix: C1638\n"),
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
                    company: "cad1638-fixture-company".into(),
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
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(handle) = self.daemon.take() {
            let _ = handle.join();
        }
    }
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
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("expected JSON response: {e}: {}", self.body))
    }

    /// Refused as a board-session failure: a 401 JSON error envelope whose
    /// `ok` is not true and whose code is `board_session_required`. Any
    /// successful catalogue envelope (`ok: true`) fails here.
    fn refused_without_session(&self) {
        assert_eq!(self.status, 401, "must be refused: {}", self.body);
        let body = self.json();
        assert_ne!(
            body["ok"],
            json!(true),
            "must not be a success envelope: {}",
            self.body
        );
        assert_eq!(
            body["error"]["code"].as_str(),
            Some("board_session_required"),
            "{}",
            self.body
        );
        assert!(!self.body.contains(PRIVATE_PROJECT));
    }
}

impl Board {
    // Each call opens a fresh connection with no implicit cookie jar, so the
    // request carries exactly the caller-supplied credentials (none here).
    // HTTP/1.0 avoids chunk decoding. No Accept header is sent, so the
    // non-navigation (JSON) branch of the hosted gate is exercised.
    fn request(&self, method: &str, path: &str, body: &str) -> Reply {
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
        if method != "HEAD" {
            reader.read_to_string(&mut body).unwrap();
        }
        Reply {
            status,
            headers,
            body,
        }
    }

    fn get(&self, path: &str) -> Reply {
        self.request("GET", path, "")
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

#[test]
fn hosted_board_refuses_guest_directory_without_session() {
    let fixture = Fixture::start();
    let board = fixture.board(false, false, true);

    // The former anonymous directory surface, including the catalogue proxy
    // and the bundle that renders it, must all demand a board session.
    for path in [
        "/apps/explore",
        "/apps/catalog/social-content",
        "/assets/index.js",
        "/api/public-apps",
        "/api/public-apps/social-content",
        "/api/public-apps/social-content/versions",
    ] {
        board.get(path).refused_without_session();
    }

    // Sign-in protocol remains intact. The login route redirects to the
    // platform authorize endpoint; the callback is served by the container
    // as the worker's own page, which bounces a reached request to `/`.
    let login = board.get("/__platform/login");
    assert_eq!(login.status, 302, "sign-in must redirect: {}", login.body);
    assert!(
        login.header("Location").is_some_and(|l| !l.is_empty()),
        "sign-in redirect needs a Location"
    );
    let callback = board.get("/__platform/callback");
    assert_eq!(
        callback.status, 302,
        "callback must still be served (container bounce): {}",
        callback.body
    );
    assert_eq!(callback.header("Location"), Some("/"));

    // Liveness stays public.
    let health = board.get("/api/health");
    assert_eq!(health.status, 200, "health must answer: {}", health.body);
}
