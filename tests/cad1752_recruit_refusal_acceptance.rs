//! CAD-1752 independent refusal acceptance, written from the ticket and
//! the PM's brief by the acceptance-check author, not the implementer.
//! The implementer must not edit or weaken this file.
//!
//! Every check drives a real in-process seam daemon over its real Unix
//! socket and, for the HTTP peers, a real board: a loopback board (operator
//! session, agent and unproven callers) or a public board that verifies a
//! platform-signed member or owner assertion (the CAD-1331 fixture).
//! Caller identity is the explicit test seam only.
//!
//! Guards covered (each test isolates one guard; the operator, or the
//! board owner, makes the same request in the same state as the positive
//! control):
//! 1. `POST /api/agents` with `catalogue` is OperatorOnly on the board: a
//!    member, an agent and an unproven caller are refused by the board's
//!    own guard (`check` names it), and nothing is registered.
//! 2. `agent_register` with `catalogue` from an agent caller is refused and
//!    leaves no `<state>/agents/<alias>/home` behind.
//! 3. A catalogue register naming `provider`, `endpoint_kind`, `profile`, or
//!    `params.catalogue|look|kind`, or an unknown role id or look, is refused.
//! 4. `agent_instructions_set` refuses every agent caller (the target, its
//!    PM, a stranger) and an unproven caller, and refuses the master and a
//!    profile agent as targets even for the operator; a refusal leaves the
//!    stored instructions, the briefing file and the `instructions_updated`
//!    event log unchanged.
//! 5. `POST /api/agents/{alias}/instructions` is OperatorOnly on the board.
//!
//! Plus one positive delivery check: a board catalogue hire writes the
//! role section of the briefing, queues the kickoff, creates the 0700 home
//! dir and records `params.catalogue`; a following edit rewrites the role
//! section, queues `job-update-<alias>-…` and answers `applies: next_turn`.
//!
//! Run (isolated HOME/XDG/TMPDIR, CADENCE_SUITE_LOCK set):
//! `cargo test --features test-seam --test suite cad1752_ -- --test-threads 2`
#![cfg(all(feature = "test-seam", target_os = "linux"))]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cadence_agent::store::{NewAgent, Store};
use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon, operator_auth, ui};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, Instant};

const COMPANY: &str = "c1752-company";
const KEY_SEED: [u8; 32] = [175; 32];
const PI_BLOCK: &str = "\npi:\n  models:\n    allow: [\"fake/model-1\"]\n    default: {master: \"fake/model-1\", worker: \"fake/model-1\"}\n";

#[derive(Clone, Copy, PartialEq)]
enum Board {
    None,
    Loopback,
    Public,
}

struct Fx {
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    port: u16,
    host: String,
    issuer: String,
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        for thread in self.threads.drain(..).rev() {
            let _ = thread.join();
        }
    }
}

struct Reply {
    status: u16,
    body: Value,
    raw: String,
    cookie: Option<String>,
}

impl Fx {
    fn start(board: Board) -> Self {
        assert!(
            std::env::var_os("CADENCE_SOCKET").is_none(),
            "fixture BLOCKED: inherited socket override"
        );
        let root = tempfile::Builder::new().prefix("c1752-").tempdir().unwrap();
        let mut fx = Self {
            root,
            stop: Arc::new(AtomicBool::new(false)),
            threads: vec![],
            port: 0,
            host: String::new(),
            issuer: String::new(),
        };
        std::fs::create_dir_all(fx.root.path().join("h")).unwrap();
        cadence_agent::issue::Pm::init(&fx.pm()).unwrap();
        let yaml = fx.pm().join("pm.yaml");
        let mut text = std::fs::read_to_string(&yaml).unwrap();
        text.push_str(PI_BLOCK);
        std::fs::write(&yaml, text).unwrap();

        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", fx.pm().to_str().unwrap());
        env.set("HOME", fx.root.path().join("h").to_str().unwrap());
        let opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(fx.stop.clone()),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        let state = fx.state();
        fx.threads.push(std::thread::spawn(move || {
            daemon::serve_with(&state, opts).unwrap();
        }));
        let deadline = Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&fx.state(), "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&fx.state()).is_none()
        {
            assert!(Instant::now() < deadline, "daemon never started");
            std::thread::sleep(Duration::from_millis(50));
        }
        if board == Board::Public {
            fx.start_platform();
        }
        if board != Board::None {
            fx.start_board(board);
        }
        fx
    }

    /// The platform's JWKS endpoint for the public board's verifier.
    fn start_platform(&mut self) {
        let platform = tiny_http::Server::http("127.0.0.1:0").unwrap();
        self.issuer = format!("http://{}", platform.server_addr());
        let key = Ed25519KeyPair::from_seed_unchecked(&KEY_SEED).unwrap();
        let jwks = json!({"keys": [{
            "kty": "OKP", "crv": "Ed25519", "kid": "c1752",
            "x": URL_SAFE_NO_PAD.encode(key.public_key().as_ref())
        }]});
        let stop = self.stop.clone();
        self.threads.push(std::thread::spawn(move || {
            while !stop.load(SeqCst) {
                let Ok(Some(request)) = platform.recv_timeout(Duration::from_millis(100)) else {
                    continue;
                };
                let response = tiny_http::Response::from_string(jwks.to_string()).with_header(
                    tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                );
                let _ = request.respond(response);
            }
        }));
    }

    fn start_board(&mut self, board: Board) {
        for port in 3110..3200 {
            let host = match board {
                Board::Public => format!("c1752.board.localhost:{port}"),
                _ => format!("cadence-{port}.localhost:{port}"),
            };
            let (tx, rx) = std::sync::mpsc::channel();
            let mut opts = ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(self.stop.clone()),
                startup: Some(tx),
                test_seam: true,
                ..Default::default()
            };
            if board == Board::Public {
                opts.public = Some(ui::PublicBoard {
                    host: host.clone(),
                    issuer: self.issuer.clone(),
                    company: COMPANY.into(),
                    authorize_url: "http://platform.invalid/authorize".into(),
                });
                opts.allow_hosts = vec![host.clone()];
                opts.allow_origins = vec![format!("http://{host}")];
            }
            let (state, pm) = (self.state(), self.pm());
            let thread = std::thread::spawn(move || {
                let _ = ui::serve(&state, &pm, &opts);
            });
            match rx.recv_timeout(Duration::from_secs(20)) {
                Ok(Ok(())) => {
                    self.port = port;
                    self.host = host;
                    self.threads.push(thread);
                    return;
                }
                Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                    let _ = thread.join();
                }
                Ok(Err(kind)) => panic!("board startup failed on port {port}: {kind:?}"),
                Err(error) => panic!("board startup gave no bind result on {port}: {error}"),
            }
        }
        panic!("no free board port in 3110..3199");
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("s")
    }
    fn pm(&self) -> PathBuf {
        self.root.path().join("pm")
    }
    fn store(&self) -> Store {
        Store::open(&self.state().join("cadence.sqlite3")).unwrap()
    }
    fn home(&self, alias: &str) -> PathBuf {
        self.state().join("agents").join(alias).join("home")
    }

    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let state = self.state();
        scoped(who, || client::rpc(&state, method, params))
    }
    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }

    /// A row written straight to the store — the fixture's population,
    /// not the path under test. No actor runs for it.
    fn seed(&self, alias: &str, params: Option<&str>, instructions: Option<&str>) {
        let cwd = self.root.path().to_str().unwrap().to_string();
        self.store()
            .register_agent(&NewAgent {
                alias,
                provider: "claude",
                endpoint_kind: "managed",
                role: "worker",
                cwd: &cwd,
                sandbox: "read-only",
                instructions,
                params,
                team_role: None,
                model_policy: None,
            })
            .unwrap_or_else(|e| panic!("seed {alias}: {e}"));
    }

    fn agent(&self, alias: &str) -> Option<cadence_agent::store::Agent> {
        self.store().agent_opt(alias).unwrap()
    }

    fn instructions(&self, alias: &str) -> Option<String> {
        self.agent(alias).and_then(|a| a.instructions)
    }

    fn event_count(&self, kind: &str) -> i64 {
        let conn = rusqlite::Connection::open_with_flags(
            self.state().join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        conn.busy_timeout(Duration::from_secs(2)).unwrap();
        conn.query_row("SELECT COUNT(*) FROM events WHERE kind=?1", [kind], |r| {
            r.get(0)
        })
        .unwrap()
    }

    /// `(id, source, state, sender, sender_uid)` of every message whose id
    /// starts with `prefix`.
    fn messages(&self, prefix: &str) -> Vec<[Option<String>; 5]> {
        let conn = rusqlite::Connection::open_with_flags(
            self.state().join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        conn.busy_timeout(Duration::from_secs(2)).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, source, state, sender, sender_uid FROM messages \
                 WHERE substr(id, 1, length(?1)) = ?1 ORDER BY seq",
            )
            .unwrap();
        stmt.query_map([prefix], |r| {
            Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?])
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    /// One raw HTTP/1.0 exchange with the board. `who` is the seam's
    /// process identity of the browser peer; `cookie`/`key` the session.
    fn http(
        &self,
        who: &Asserted,
        method: &str,
        path: &str,
        body: Option<&Value>,
        cookie: Option<&str>,
        key: Option<&str>,
    ) -> Reply {
        let body = body.map(Value::to_string).unwrap_or_default();
        let mut extra = String::new();
        if let Some(cookie) = cookie {
            extra.push_str(&format!("Cookie: {cookie}\r\n"));
        }
        if let Some(key) = key {
            extra.push_str(&format!("X-Cadence-Session: {key}\r\n"));
        }
        let token = Seam::token_at(&self.state()).unwrap();
        let request = format!(
            "{method} {path} HTTP/1.0\r\nHost: {host}\r\nX-Cadence-Board: 1\r\n\
             Origin: http://{host}\r\nSec-Fetch-Site: same-origin\r\n\
             Content-Type: application/json\r\n{AS_HEADER}: {who}\r\n\
             {TOKEN_HEADER}: {token}\r\n{extra}Content-Length: {len}\r\n\r\n{body}",
            host = self.host,
            who = who.as_str(),
            len = body.len(),
        );
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, raw) = response.split_once("\r\n\r\n").expect("HTTP response head");
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        let cookie = head.lines().skip(1).find_map(|line| {
            line.to_ascii_lowercase()
                .starts_with("set-cookie:")
                .then(|| {
                    line.split_once(':')
                        .unwrap()
                        .1
                        .trim()
                        .split(';')
                        .next()
                        .unwrap()
                        .to_string()
                })
        });
        Reply {
            status,
            body: serde_json::from_str(raw).unwrap_or(Value::Null),
            raw: raw.to_string(),
            cookie,
        }
    }

    /// A loopback operator session: `(cookie, session key)`.
    fn operator_session(&self) -> (String, String) {
        operator_auth::ensure_secret(&self.state()).unwrap();
        let secret = operator_auth::read_secret(&self.state()).unwrap();
        let nonce = self.op(
            "operator_link_mint",
            json!({"secret": secret, "origin": "loopback"}),
        )["nonce"]
            .clone();
        let reply = self.http(
            &Asserted::Operator,
            "POST",
            "/api/session",
            Some(&json!({"nonce": nonce})),
            None,
            None,
        );
        assert_eq!(reply.status, 200, "operator session: {}", reply.raw);
        let key = reply.body["session_key"].as_str().unwrap().to_string();
        (reply.cookie.expect("session cookie"), key)
    }

    /// A platform-verified public-board session for `role` (`member` or
    /// `owner`).
    fn platform_cookie(&self, role: &str) -> String {
        let b64 = URL_SAFE_NO_PAD;
        let key = Ed25519KeyPair::from_seed_unchecked(&KEY_SEED).unwrap();
        let now = cadence_agent::issue::time::now_epoch();
        let part = |value: Value| b64.encode(serde_json::to_vec(&value).unwrap());
        let signed = format!(
            "{}.{}",
            part(json!({"alg":"EdDSA", "typ":"JWT", "kid":"c1752"})),
            part(json!({
                "iss": self.issuer, "aud": self.host,
                "sub": format!("c1752-{role}"), "email": format!("{role}@example.test"),
                "name": format!("CAD-1752 {role}"), "company": COMPANY, "role": role,
                "iat": now, "exp": now + 60, "jti": format!("c1752-{role}-session")
            }))
        );
        let assertion = format!(
            "{signed}.{}",
            b64.encode(key.sign(signed.as_bytes()).as_ref())
        );
        let reply = self.http(
            &Asserted::Operator,
            "POST",
            "/__platform/session",
            Some(&json!({"assertion": assertion})),
            None,
            None,
        );
        assert_eq!(reply.status, 200, "{role} sign-in failed: {}", reply.raw);
        reply.cookie.expect("public session cookie")
    }
}

fn accountant_job() -> String {
    cadence_agent::agent_roles::find("accountant")
        .expect("accountant role")
        .instructions()
}

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Nothing a refused hire could have left: no row, no home dir.
fn assert_not_hired(fx: &Fx, alias: &str, what: &str) {
    assert!(
        fx.agent(alias).is_none(),
        "{what}: agent row '{alias}' exists"
    );
    assert!(
        !fx.home(alias).exists(),
        "{what}: {} was left behind",
        fx.home(alias).display()
    );
    let briefing = client::briefing_path(&fx.state(), &json!({}), alias);
    assert!(
        !briefing.exists(),
        "{what}: {} was written",
        briefing.display()
    );
}

/// A briefing on disk for `alias`, to prove a refusal leaves it alone.
fn plant_briefing(fx: &Fx, alias: &str, params: &Value) -> (PathBuf, Vec<u8>) {
    let path = client::briefing_path(&fx.state(), params, alias);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body = format!("briefing of {alias} before the edit\n").into_bytes();
    std::fs::write(&path, &body).unwrap();
    (path, body)
}

/// The board itself refused the unproven peer: its session attribution
/// guard honours an operator session only from a caller it can attribute
/// (`caller_identity`), before any relay to the daemon.
fn assert_board_refused_unproven(reply: &Reply) {
    assert_eq!(reply.body["check"], "caller_identity", "{}", reply.raw);
    assert!(
        reply
            .raw
            .contains("an operator session is honoured only from a caller the board can attribute"),
        "the unproven refusal did not come from the board: {}",
        reply.raw
    );
}

// ---------------------------------------------------------------- item 1

/// Guard: the endpoint table's `POST /api/agents` OperatorOnly row
/// (`src/ui/endpoints.rs`), enforced by `operator::admit` (`member_role`).
#[test]
fn cad1752_board_catalogue_register_refuses_member() {
    let fx = Fx::start(Board::Public);
    let body = json!({"alias": "acct-m", "catalogue": "accountant"});

    let member = fx.platform_cookie("member");
    let reply = fx.http(
        &Asserted::Operator,
        "POST",
        "/api/agents",
        Some(&body),
        Some(&member),
        None,
    );
    assert_eq!(reply.status, 403, "member hire not refused: {}", reply.raw);
    assert_eq!(
        reply.body["check"], "member_role",
        "the refusal is not the board's role guard: {}",
        reply.raw
    );
    assert_not_hired(&fx, "acct-m", "member hire");

    // Control: the board owner, the same request, the same state.
    let owner = fx.platform_cookie("owner");
    let reply = fx.http(
        &Asserted::Operator,
        "POST",
        "/api/agents",
        Some(&body),
        Some(&owner),
        None,
    );
    assert_eq!(reply.status, 200, "owner hire failed: {}", reply.raw);
    let row = fx.agent("acct-m").expect("owner hire registered nothing");
    assert_eq!(row.params.unwrap()["catalogue"], "accountant");
}

/// Same guard, loopback board: an agent caller (`operator_only`) and an
/// unproven peer holding a real operator session are refused by the board.
#[test]
fn cad1752_board_catalogue_register_refuses_agent_and_unproven() {
    let fx = Fx::start(Board::Loopback);
    fx.seed("pm-root", None, None);
    let body = json!({"alias": "acct-l", "catalogue": "accountant"});
    let (cookie, key) = fx.operator_session();

    let reply = fx.http(
        &Asserted::Agent("pm-root".into()),
        "POST",
        "/api/agents",
        Some(&body),
        None,
        None,
    );
    assert_eq!(reply.status, 403, "agent hire not refused: {}", reply.raw);
    assert_eq!(
        reply.body["check"], "operator_only",
        "the refusal is not the board's operator-only guard: {}",
        reply.raw
    );
    assert_not_hired(&fx, "acct-l", "agent hire");

    let reply = fx.http(
        &Asserted::Unproven,
        "POST",
        "/api/agents",
        Some(&body),
        Some(&cookie),
        Some(&key),
    );
    assert_eq!(
        reply.status, 403,
        "unproven peer with an operator session not refused: {}",
        reply.raw
    );
    assert_board_refused_unproven(&reply);
    assert_not_hired(&fx, "acct-l", "unproven hire");

    // Control: the operator's session from a proven peer.
    let reply = fx.http(
        &Asserted::Operator,
        "POST",
        "/api/agents",
        Some(&body),
        Some(&cookie),
        Some(&key),
    );
    assert_eq!(reply.status, 200, "operator hire failed: {}", reply.raw);
    assert!(
        fx.agent("acct-l").is_some(),
        "operator hire registered nothing"
    );
}

// ---------------------------------------------------------------- item 2

/// Guard: `authorize_register` (`src/daemon/identity.rs`), reached before
/// the home dir is created in `rpc_register`.
///
/// Not covered here, reported as a finding instead: a group-root agent
/// naming itself as `params.upstream` is admitted by `authorize_register`
/// today and gets a catalogue hire plus a home dir. That admission is not
/// encoded as expected behaviour.
#[test]
fn cad1752_daemon_catalogue_register_refuses_agent_caller() {
    let fx = Fx::start(Board::None);
    fx.seed("pm-root", None, None);
    fx.seed("other-root", None, None);
    fx.seed("worker-1", Some(r#"{"upstream":"pm-root"}"#), None);

    let cases: [(&str, Asserted, Value); 3] = [
        (
            "a worker in a group",
            Asserted::Agent("worker-1".into()),
            json!({"alias": "acct-a", "catalogue": "accountant"}),
        ),
        (
            "a group root minting a root",
            Asserted::Agent("pm-root".into()),
            json!({"alias": "acct-a", "catalogue": "accountant"}),
        ),
        (
            "a group root naming another root's group",
            Asserted::Agent("pm-root".into()),
            json!({"alias": "acct-a", "catalogue": "accountant",
                   "params": r#"{"upstream":"other-root"}"#}),
        ),
    ];
    for (what, who, params) in cases {
        let err = fx
            .rpc(who, "agent_register", params)
            .expect_err(&format!("{what}: catalogue register admitted"));
        assert!(
            err.to_string().contains("agent register refused"),
            "{what}: refused for a different reason: {err}"
        );
        assert_not_hired(&fx, "acct-a", what);
    }

    // Control: the operator, the same request.
    let out = fx.op(
        "agent_register",
        json!({"alias": "acct-a", "catalogue": "accountant"}),
    );
    assert_eq!(out["alias"], "acct-a", "{out}");
    assert!(fx.agent("acct-a").is_some());
    assert_eq!(mode(&fx.home("acct-a")), 0o700);
}

// ---------------------------------------------------------------- item 3

/// Guard: `catalogue_hire` refuses a caller-named runtime
/// (`provider`, `endpoint_kind`, `profile`).
#[test]
fn cad1752_daemon_catalogue_register_refuses_caller_runtime() {
    let fx = Fx::start(Board::None);
    for (field, value) in [
        ("provider", json!("pi")),
        ("provider", json!("claude")),
        ("endpoint_kind", json!("managed")),
        ("profile", json!("lane-pm")),
    ] {
        let mut params = json!({"alias": "acct-f", "catalogue": "accountant"});
        params[field] = value.clone();
        let err = fx
            .rpc(Asserted::Operator, "agent_register", params)
            .expect_err(&format!("catalogue + {field}={value} admitted"));
        assert!(
            err.to_string().contains(&format!(
                "a catalogue hire takes '{field}' from the install"
            )),
            "{field}={value}: refused for a different reason: {err}"
        );
        assert_not_hired(&fx, "acct-f", field);
    }
    let out = fx.op(
        "agent_register",
        json!({"alias": "acct-f", "catalogue": "accountant"}),
    );
    assert_eq!(out["provider"], "pi", "{out}");
}

/// Guard: `rpc_register` refuses caller `params.catalogue|look|kind` on a
/// catalogue hire.
#[test]
fn cad1752_daemon_catalogue_register_refuses_forged_params() {
    let fx = Fx::start(Board::None);
    for (key, forged) in [
        ("catalogue", r#"{"catalogue":"developer"}"#),
        ("look", r#"{"look":"guard"}"#),
        ("kind", r#"{"kind":"tickets"}"#),
    ] {
        let err = fx
            .rpc(
                Asserted::Operator,
                "agent_register",
                json!({"alias": "acct-p", "catalogue": "accountant", "params": forged}),
            )
            .expect_err(&format!("forged params.{key} admitted"));
        assert!(
            err.to_string().contains(&format!(
                "params '{key}' is set from the catalogue role, not the caller"
            )),
            "params.{key}: refused for a different reason: {err}"
        );
        assert_not_hired(&fx, "acct-p", key);
    }
    let out = fx.op(
        "agent_register",
        json!({"alias": "acct-p", "catalogue": "accountant", "params": "{}"}),
    );
    assert_eq!(out["alias"], "acct-p", "{out}");
    let params = fx.agent("acct-p").unwrap().params.unwrap();
    assert_eq!(params["catalogue"], "accountant");
    assert_eq!(params["kind"], "apps", "{params}");
}

/// Guard: `catalogue_hire` refuses an unknown role id and an unknown look.
#[test]
fn cad1752_daemon_catalogue_register_refuses_unknown_role_or_look() {
    let fx = Fx::start(Board::None);
    let err = fx
        .rpc(
            Asserted::Operator,
            "agent_register",
            json!({"alias": "acct-u", "catalogue": "no-such-role"}),
        )
        .expect_err("unknown catalogue id admitted");
    assert!(
        err.to_string()
            .contains("Unknown catalogue role 'no-such-role'"),
        "unknown id: {err}"
    );
    assert_not_hired(&fx, "acct-u", "unknown id");
    let err = fx
        .rpc(
            Asserted::Operator,
            "agent_register",
            json!({"alias": "acct-u", "catalogue": "accountant", "look": "no-such-look"}),
        )
        .expect_err("unknown look admitted");
    assert!(
        err.to_string().contains("Unknown look 'no-such-look'"),
        "unknown look: {err}"
    );
    assert_not_hired(&fx, "acct-u", "unknown look");
    let look = cadence_agent::agent_roles::LOOKS[0];
    fx.op(
        "agent_register",
        json!({"alias": "acct-u", "catalogue": "accountant", "look": look}),
    );
    assert_eq!(fx.agent("acct-u").unwrap().params.unwrap()["look"], look);
}

// ---------------------------------------------------------------- item 4

/// Fixture for the edit-job RPC: a PM root, its member `job-t` with a
/// stored job and a briefing on disk, and an unrelated agent.
fn edit_job_fixture(fx: &Fx) -> PathBuf {
    fx.seed("pm-root", None, None);
    fx.seed("stranger", None, None);
    fx.seed(
        "job-t",
        Some(r#"{"upstream":"pm-root"}"#),
        Some("original job"),
    );
    plant_briefing(fx, "job-t", &json!({"upstream": "pm-root"})).0
}

/// Guard: `operator_connection_on_agent` at the top of
/// `rpc_instructions_set`.
#[test]
fn cad1752_daemon_edit_job_refuses_agent_and_unproven_callers() {
    let fx = Fx::start(Board::None);
    let briefing = edit_job_fixture(&fx);
    let params = json!({"alias": "job-t", "instructions": "forged job"});
    let before = std::fs::read(&briefing).unwrap();
    for (what, who) in [
        ("the target itself", Asserted::Agent("job-t".into())),
        ("its PM", Asserted::Agent("pm-root".into())),
        ("an unrelated agent", Asserted::Agent("stranger".into())),
        ("an unproven caller", Asserted::Unproven),
    ] {
        let err = fx
            .rpc(who, "agent_instructions_set", params.clone())
            .expect_err(&format!("{what}: edit job admitted"));
        assert!(
            err.to_string()
                .contains("agent instructions is an operator action"),
            "{what}: refused for a different reason: {err}"
        );
        assert_eq!(
            fx.instructions("job-t").as_deref(),
            Some("original job"),
            "{what}"
        );
        assert_eq!(
            std::fs::read(&briefing).unwrap(),
            before,
            "{what}: briefing changed"
        );
        assert_eq!(
            fx.event_count("instructions_updated"),
            0,
            "{what}: event written"
        );
    }

    // Control: the operator, the same request.
    let out = fx.op("agent_instructions_set", params);
    assert_eq!(out["state"], "updated", "{out}");
    assert_eq!(fx.instructions("job-t").as_deref(), Some("forged job"));
    assert_eq!(fx.event_count("instructions_updated"), 1);
}

/// Guard: the master check in `rpc_instructions_set`.
#[test]
fn cad1752_daemon_edit_job_refuses_master_target() {
    let fx = Fx::start(Board::None);
    edit_job_fixture(&fx);
    fx.seed("master", None, Some("master soul"));
    let (briefing, before) = plant_briefing(&fx, "master", &json!({}));
    let err = fx
        .rpc(
            Asserted::Operator,
            "agent_instructions_set",
            json!({"alias": "master", "instructions": "forged job"}),
        )
        .expect_err("the master's instructions were replaced");
    assert!(
        err.to_string()
            .contains("the master's instructions are its SOUL and AGENT files"),
        "master: refused for a different reason: {err}"
    );
    assert_eq!(fx.instructions("master").as_deref(), Some("master soul"));
    assert_eq!(
        std::fs::read(&briefing).unwrap(),
        before,
        "briefing changed"
    );
    assert_eq!(fx.event_count("instructions_updated"), 0);

    // Control: the operator on a plain agent in the same state.
    fx.op(
        "agent_instructions_set",
        json!({"alias": "job-t", "instructions": "new job"}),
    );
    assert_eq!(fx.instructions("job-t").as_deref(), Some("new job"));
    assert_eq!(fx.event_count("instructions_updated"), 1);
}

/// Guard: the profile-agent check in `rpc_instructions_set`.
#[test]
fn cad1752_daemon_edit_job_refuses_profile_target() {
    let fx = Fx::start(Board::None);
    edit_job_fixture(&fx);
    fx.seed(
        "prof-1",
        Some(r#"{"profile":"lane-pm"}"#),
        Some("profile job"),
    );
    let (briefing, before) = plant_briefing(&fx, "prof-1", &json!({"profile": "lane-pm"}));
    let err = fx
        .rpc(
            Asserted::Operator,
            "agent_instructions_set",
            json!({"alias": "prof-1", "instructions": "forged job"}),
        )
        .expect_err("a profile agent's instructions were replaced");
    assert!(
        err.to_string()
            .contains("a profile agent's job comes from its settings profile"),
        "profile: refused for a different reason: {err}"
    );
    assert_eq!(fx.instructions("prof-1").as_deref(), Some("profile job"));
    assert_eq!(
        std::fs::read(&briefing).unwrap(),
        before,
        "briefing changed"
    );
    assert_eq!(fx.event_count("instructions_updated"), 0);

    fx.op(
        "agent_instructions_set",
        json!({"alias": "job-t", "instructions": "new job"}),
    );
    assert_eq!(fx.instructions("job-t").as_deref(), Some("new job"));
    assert_eq!(fx.event_count("instructions_updated"), 1);
}

// ---------------------------------------------------------------- item 5

const EDIT_ROUTE: &str = "/api/agents/job-t/instructions";

/// Guard: `WRITE_ROUTES` `POST /api/agents/*/instructions` OperatorOnly
/// (`member_role`).
#[test]
fn cad1752_board_edit_job_refuses_member() {
    let fx = Fx::start(Board::Public);
    let briefing = edit_job_fixture(&fx);
    let before = std::fs::read(&briefing).unwrap();
    let body = json!({"instructions": "forged job"});
    let member = fx.platform_cookie("member");
    let reply = fx.http(
        &Asserted::Operator,
        "POST",
        EDIT_ROUTE,
        Some(&body),
        Some(&member),
        None,
    );
    assert_eq!(reply.status, 403, "member edit not refused: {}", reply.raw);
    assert_eq!(reply.body["check"], "member_role", "{}", reply.raw);
    assert_eq!(fx.instructions("job-t").as_deref(), Some("original job"));
    assert_eq!(
        std::fs::read(&briefing).unwrap(),
        before,
        "briefing changed"
    );
    assert_eq!(fx.event_count("instructions_updated"), 0);

    let owner = fx.platform_cookie("owner");
    let reply = fx.http(
        &Asserted::Operator,
        "POST",
        EDIT_ROUTE,
        Some(&body),
        Some(&owner),
        None,
    );
    assert_eq!(reply.status, 200, "owner edit failed: {}", reply.raw);
    assert_eq!(fx.instructions("job-t").as_deref(), Some("forged job"));
    assert_eq!(fx.event_count("instructions_updated"), 1);
}

/// Same guard, loopback board: the target agent, its PM and an unproven
/// peer holding an operator session.
#[test]
fn cad1752_board_edit_job_refuses_agent_and_unproven() {
    let fx = Fx::start(Board::Loopback);
    let briefing = edit_job_fixture(&fx);
    let before = std::fs::read(&briefing).unwrap();
    let body = json!({"instructions": "forged job"});
    let (cookie, key) = fx.operator_session();
    for alias in ["job-t", "pm-root"] {
        let reply = fx.http(
            &Asserted::Agent(alias.into()),
            "POST",
            EDIT_ROUTE,
            Some(&body),
            None,
            None,
        );
        assert_eq!(
            reply.status, 403,
            "agent {alias} edit not refused: {}",
            reply.raw
        );
        assert_eq!(
            reply.body["check"], "operator_only",
            "{alias}: {}",
            reply.raw
        );
    }
    let reply = fx.http(
        &Asserted::Unproven,
        "POST",
        EDIT_ROUTE,
        Some(&body),
        Some(&cookie),
        Some(&key),
    );
    assert_eq!(
        reply.status, 403,
        "unproven edit not refused: {}",
        reply.raw
    );
    assert_board_refused_unproven(&reply);
    assert_eq!(fx.instructions("job-t").as_deref(), Some("original job"));
    assert_eq!(
        std::fs::read(&briefing).unwrap(),
        before,
        "briefing changed"
    );
    assert_eq!(fx.event_count("instructions_updated"), 0);

    let reply = fx.http(
        &Asserted::Operator,
        "POST",
        EDIT_ROUTE,
        Some(&body),
        Some(&cookie),
        Some(&key),
    );
    assert_eq!(reply.status, 200, "operator edit failed: {}", reply.raw);
    assert_eq!(fx.instructions("job-t").as_deref(), Some("forged job"));
}

// ------------------------------------------------- positive live delivery

/// Stage 1b: a board catalogue hire and a following edit deliver the job
/// through the briefing, against a live in-process daemon and board.
#[test]
fn cad1752_board_catalogue_hire_delivers_job_through_briefing() {
    let fx = Fx::start(Board::Loopback);
    let (cookie, key) = fx.operator_session();
    let alias = "accountant-1";

    let reply = fx.http(
        &Asserted::Operator,
        "POST",
        "/api/agents",
        Some(&json!({"alias": alias, "catalogue": "accountant"})),
        Some(&cookie),
        Some(&key),
    );
    assert_eq!(reply.status, 200, "hire failed: {}", reply.raw);
    let briefing = PathBuf::from(
        reply.body["briefing"]
            .as_str()
            .unwrap_or_else(|| panic!("no briefing path in the hire reply: {}", reply.raw)),
    );
    let text = std::fs::read_to_string(&briefing).unwrap();
    assert_eq!(
        cadence_agent::briefing::role_instructions(&text),
        Some(accountant_job().trim_matches('\n')),
        "the briefing's role section is not the accountant's job"
    );
    let kickoff = fx.messages(&format!("bootstrap-{alias}"));
    assert_eq!(kickoff.len(), 1, "kickoff rows: {kickoff:?}");
    eprintln!("CAD-1752 kickoff row (id, source, state, sender, sender_uid): {kickoff:?}");
    assert_eq!(kickoff[0][1].as_deref(), Some("bootstrap"), "{kickoff:?}");
    assert_eq!(
        kickoff[0][3].as_deref(),
        Some("operator"),
        "the kickoff is not attributed to the operator: {kickoff:?}"
    );
    assert_eq!(mode(&fx.home(alias)), 0o700, "home dir mode");
    let row = fx.agent(alias).unwrap();
    assert_eq!(row.params.as_ref().unwrap()["catalogue"], "accountant");
    assert_eq!(
        std::fs::canonicalize(&row.cwd).unwrap(),
        std::fs::canonicalize(fx.home(alias)).unwrap(),
        "a catalogue hire without a cwd runs in its home dir"
    );

    let new_job = "Chase unpaid invoices and report them.";
    let reply = fx.http(
        &Asserted::Operator,
        "POST",
        &format!("/api/agents/{alias}/instructions"),
        Some(&json!({"instructions": new_job})),
        Some(&cookie),
        Some(&key),
    );
    assert_eq!(reply.status, 200, "edit failed: {}", reply.raw);
    assert_eq!(reply.body["applies"], "next_turn", "{}", reply.raw);
    let text = std::fs::read_to_string(&briefing).unwrap();
    assert_eq!(
        cadence_agent::briefing::role_instructions(&text),
        Some(new_job)
    );
    let update = fx.messages(&format!("job-update-{alias}-"));
    assert_eq!(update.len(), 1, "job-update rows: {update:?}");
    assert_eq!(update[0][3].as_deref(), Some("operator"), "{update:?}");
    eprintln!("CAD-1752 job-update row (id, source, state, sender, sender_uid): {update:?}");
}
