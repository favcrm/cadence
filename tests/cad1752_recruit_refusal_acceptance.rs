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

/// A refusal the daemon states (`Error::Rejected`), not an internal
/// failure such as a store constraint.
fn assert_stated_rejection(err: &cadence_agent::Error, what: &str) {
    assert!(
        matches!(err, cadence_agent::Error::Rejected(_)),
        "{what}: not a stated rejection: {err:?}"
    );
    assert!(
        !err.to_string().contains("UNIQUE constraint"),
        "{what}: the store's constraint answered, not a guard: {err}"
    );
}

/// Rule (PM decision on CAD-1752): a catalogue hire is an operator act, so
/// every agent caller is refused whatever its group position, including a
/// group root naming itself as the new agent's `upstream`.
#[test]
fn cad1752_daemon_catalogue_register_refuses_group_root_agent() {
    let fx = Fx::start(Board::None);
    fx.seed("pm-root", None, None);
    let params = json!({"alias": "acct-g", "catalogue": "accountant",
                        "params": r#"{"upstream":"pm-root"}"#});
    let grouped = client::briefing_path(&fx.state(), &json!({"upstream": "pm-root"}), "acct-g");

    let err = fx
        .rpc(
            Asserted::Agent("pm-root".into()),
            "agent_register",
            params.clone(),
        )
        .expect_err("a group-root agent hired from the catalogue into its own group");
    assert_stated_rejection(&err, "group root");
    assert_not_hired(&fx, "acct-g", "group root");
    assert!(
        !grouped.exists(),
        "group root: {} was written",
        grouped.display()
    );

    // Control: the operator, the same request, the same state.
    let out = fx.op("agent_register", params);
    assert_eq!(out["alias"], "acct-g", "{out}");
    let row = fx
        .agent("acct-g")
        .expect("operator hire registered nothing");
    assert_eq!(row.params.unwrap()["upstream"], "pm-root");
    assert_eq!(mode(&fx.home("acct-g")), 0o700);
}

/// A catalogue register naming an alias that already exists is refused
/// with a stated rejection, for an agent caller and for the operator, and
/// creates no `<state>/agents/<alias>/home` for the existing agent.
#[test]
fn cad1752_daemon_catalogue_register_existing_alias_leaves_no_home() {
    let fx = Fx::start(Board::None);
    fx.seed("pm-root", None, None);
    fx.seed("worker-1", Some(r#"{"upstream":"pm-root"}"#), None);
    fx.seed("stranger", None, Some("stranger job"));
    let params = json!({"alias": "stranger", "catalogue": "accountant"});
    assert!(
        !fx.home("stranger").exists(),
        "fixture: home already present"
    );

    for (what, who) in [
        ("an agent caller", Asserted::Agent("worker-1".into())),
        ("the operator", Asserted::Operator),
    ] {
        let err = fx
            .rpc(who, "agent_register", params.clone())
            .expect_err(&format!(
                "{what}: a catalogue hire over an existing alias was admitted"
            ));
        assert_stated_rejection(&err, what);
        assert!(
            !fx.home("stranger").exists(),
            "{what}: {} was created for the existing agent",
            fx.home("stranger").display()
        );
        let row = fx.agent("stranger").unwrap();
        assert_eq!(row.provider, "claude", "{what}: existing row changed");
        assert_eq!(row.instructions.as_deref(), Some("stranger job"), "{what}");
    }

    // Control: the operator hires under a fresh alias in the same state.
    fx.op(
        "agent_register",
        json!({"alias": "acct-x", "catalogue": "accountant"}),
    );
    assert!(fx.agent("acct-x").is_some());
    assert_eq!(mode(&fx.home("acct-x")), 0o700);
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

// ------------------------------------- stage 2: the project comes from the task
//
// Fixture: two tracker projects, `alpha` (prefix ALPHA, one repo checkout)
// and `beta` (prefix BETA). A catalogue agent lives in a home dir that is
// no repo. Dispatched work is a message row carrying `issue`, which only a
// daemon dispatch writes (`agent_send` refuses a caller `issue`); the
// fixture plants those rows straight into the store, with no actor running
// for the seeded agents.

const CATALOGUE: &str = r#"{"catalogue":"accountant","kind":"apps","look":"bookkeeper"}"#;

/// `alpha` and `beta` in the tracker; returns alpha's repo checkout.
fn stage2_projects(fx: &Fx) -> PathBuf {
    let repo = fx.root.path().join("alpha-repo");
    std::fs::create_dir_all(&repo).unwrap();
    let out = cadence_agent::reaper::output(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&repo),
    )
    .unwrap();
    assert!(out.status.success(), "git init: {out:?}");
    let repo = std::fs::canonicalize(&repo).unwrap();
    for (key, prefix, repos) in [
        (
            "alpha",
            "ALPHA",
            format!("repos:\n  - path: {}\n", repo.display()),
        ),
        ("beta", "BETA", String::new()),
    ] {
        let dir = fx.pm().join(key);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("project.yaml"),
            format!("key: {key}\nprefix: {prefix}\n{repos}"),
        )
        .unwrap();
    }
    repo
}

impl Fx {
    /// A store row with an explicit cwd and params (fixture population).
    fn seed_at(&self, alias: &str, cwd: &std::path::Path, params: Option<&str>) {
        std::fs::create_dir_all(cwd).unwrap();
        let cwd = cwd.to_str().unwrap().to_string();
        self.store()
            .register_agent(&NewAgent {
                alias,
                provider: "claude",
                endpoint_kind: "managed",
                role: "worker",
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params,
                team_role: None,
                model_policy: None,
            })
            .unwrap_or_else(|e| panic!("seed {alias}: {e}"));
    }

    /// A ticketless catalogue agent in its home dir under state.
    fn seed_catalogue(&self, alias: &str) {
        self.seed_at(alias, &self.home(alias), Some(CATALOGUE));
    }

    /// A message row as a dispatch leaves it: `issue` set, `state` given.
    fn plant_message(&self, alias: &str, id: &str, state: &str, issue: Option<&str>) {
        let conn = rusqlite::Connection::open(self.state().join("cadence.sqlite3")).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn.execute(
            "INSERT INTO messages(id, alias, body, source, state, created, issue) \
             VALUES (?1, ?2, 'fixture turn', 'dispatch', ?3, ?4, ?5)",
            rusqlite::params![
                id,
                alias,
                state,
                cadence_agent::issue::time::now_epoch() as f64,
                issue
            ],
        )
        .unwrap();
    }

    fn wiki_write(&self, who: &str, path: &str) -> cadence_agent::Result<Value> {
        self.rpc(
            Asserted::Agent(who.into()),
            "wiki_write",
            json!({"path": path, "text": format!("# {path}\n")}),
        )
    }

    fn turn_project(&self, alias: &str) -> Value {
        self.op("agent_show", json!({"alias": alias, "limit": 0}))["agent"]["turn_project"].clone()
    }
}

/// The wiki's project-folder rule refused, naming the agent's project.
fn assert_project_refusal(result: cadence_agent::Result<Value>, what: &str, project: &str) {
    let err = result.expect_err(&format!("{what}: admitted"));
    assert!(
        matches!(err, cadence_agent::Error::Rejected(_)),
        "{what}: not a stated rejection: {err:?}"
    );
    let text = err.to_string();
    assert!(
        text.contains("agents write only their own project's folder")
            && text.contains(&format!("this agent's project: {project}")),
        "{what}: refused for a different reason: {text}"
    );
}

/// Item 1. Guards: `turn_project` (`src/daemon/turn_project.rs`) gives a
/// ticketless catalogue agent no project — not even when its cwd sits in
/// a project's repo — and `wiki::allowed` (`src/wiki/mod.rs`, `projects`
/// arm) refuses a projects/ write without the matching project.
#[test]
fn cad1752_ticketless_catalogue_agent_cannot_write_project_wiki() {
    let fx = Fx::start(Board::None);
    let repo = stage2_projects(&fx);
    fx.seed_catalogue("acct-home");
    // A catalogue agent the operator registered with a repo cwd: still
    // no project without a task, because the catalogue never falls back
    // to its cwd.
    fx.seed_at("acct-repo", &repo, Some(CATALOGUE));
    fx.op(
        "wiki_write",
        json!({"path": "global/handbook.md", "text": "# handbook\n"}),
    );

    for alias in ["acct-home", "acct-repo"] {
        assert_eq!(fx.turn_project(alias), Value::Null, "{alias}: turn_project");
        assert_project_refusal(
            fx.wiki_write(alias, "projects/alpha/notes.md"),
            alias,
            "none",
        );
        // Positive: its own knowledge/ is writable and global/ readable.
        let own = fx
            .wiki_write(alias, &format!("agents/{alias}/knowledge/notes.md"))
            .unwrap_or_else(|e| panic!("{alias}: own knowledge write refused: {e}"));
        assert_eq!(own["committed"], true, "{own}");
        let read = fx
            .rpc(
                Asserted::Agent(alias.into()),
                "wiki_read",
                json!({"path": "global/handbook.md"}),
            )
            .unwrap_or_else(|e| panic!("{alias}: global read refused: {e}"));
        assert_eq!(read["text"], "# handbook\n");
    }

    // Control: the operator writes the same project page.
    fx.op(
        "wiki_write",
        json!({"path": "projects/alpha/notes.md", "text": "# notes\n"}),
    );
}

/// Item 2. Guards: `turn_project` takes the running dispatched task's
/// project, and `wiki::allowed` holds the agent to that one project.
#[test]
fn cad1752_task_project_scopes_project_wiki_writes() {
    let fx = Fx::start(Board::None);
    stage2_projects(&fx);
    fx.seed_catalogue("dev-2");
    fx.plant_message("dev-2", "dispatch-alpha-1", "running", Some("ALPHA-1"));
    assert_eq!(fx.turn_project("dev-2"), "alpha");

    let ok = fx
        .wiki_write("dev-2", "projects/alpha/notes.md")
        .unwrap_or_else(|e| panic!("the running task's project write refused: {e}"));
    assert_eq!(ok["committed"], true, "{ok}");
    assert_project_refusal(
        fx.wiki_write("dev-2", "projects/beta/notes.md"),
        "a turn on alpha writing beta",
        "alpha",
    );
    fx.op(
        "wiki_write",
        json!({"path": "projects/beta/notes.md", "text": "# beta\n"}),
    );
}

/// Item 5a. Guard: `turn_project` never guesses between two queued
/// projects when nothing is running.
#[test]
fn cad1752_two_queued_projects_give_no_project() {
    let fx = Fx::start(Board::None);
    stage2_projects(&fx);
    fx.seed_catalogue("dev-3");
    fx.plant_message("dev-3", "dispatch-alpha-2", "queued", Some("ALPHA-2"));
    fx.plant_message("dev-3", "dispatch-beta-1", "queued", Some("BETA-1"));
    assert_eq!(fx.turn_project("dev-3"), Value::Null, "a guess between two");
    assert_project_refusal(
        fx.wiki_write("dev-3", "projects/alpha/notes.md"),
        "two queued projects",
        "none",
    );
    // Control: one queued project is unambiguous.
    fx.seed_catalogue("dev-4");
    fx.plant_message("dev-4", "dispatch-alpha-3", "queued", Some("ALPHA-3"));
    assert_eq!(fx.turn_project("dev-4"), "alpha");
    let ok = fx.wiki_write("dev-4", "projects/alpha/notes.md").unwrap();
    assert_eq!(ok["committed"], true);
}

/// Item 3. Guards: `default_account` (`src/daemon/effect_rpc.rs`, under
/// `platform_call`) and `rpc_platform_check` (`src/daemon/platform_rpc.rs`)
/// refuse a ticketless agent that names no account or project.
#[test]
fn cad1752_ticketless_agent_effect_needs_explicit_account() {
    let fx = Fx::start(Board::None);
    stage2_projects(&fx);
    fx.seed_catalogue("acct-fx");
    let no_project = "'acct-fx' has no project: no running task";
    for (verb, params) in [
        (
            "platform_call",
            json!({"platform": "demo", "tool": "demo.read"}),
        ),
        (
            "platform_check",
            json!({"platform": "demo", "scope": "demo.read"}),
        ),
    ] {
        let err = fx
            .rpc(Asserted::Agent("acct-fx".into()), verb, params.clone())
            .expect_err(&format!("{verb}: a ticketless agent resolved an account"));
        assert!(
            matches!(err, cadence_agent::Error::Rejected(_))
                && err.to_string().contains(no_project),
            "{verb}: refused for a different reason: {err:?}"
        );
        // Positive: with an explicit account it gets past that check (and
        // stops later, at the grant or adapter, for a different reason).
        let mut explicit = params.clone();
        explicit["account"] = json!("acct-1");
        let later = fx.rpc(Asserted::Agent("acct-fx".into()), verb, explicit);
        if let Err(err) = &later {
            assert!(
                !err.to_string().contains("has no project"),
                "{verb}: an explicit account still hit the project rule: {err}"
            );
        }
        // Positive: with a running task on alpha the project resolves.
        if verb == "platform_check" {
            fx.plant_message("acct-fx", "dispatch-fx-1", "running", Some("ALPHA-9"));
        }
    }
    let err = fx
        .rpc(
            Asserted::Agent("acct-fx".into()),
            "platform_check",
            json!({"platform": "demo", "scope": "demo.read"}),
        )
        .expect_err("no grant exists in this fixture");
    assert!(
        !err.to_string().contains("has no project"),
        "a running task on alpha still left no project: {err}"
    );
}

/// Item 4 (agent_show) and 5b. `agent_show` carries the running task's
/// project for a catalogue agent, nothing without a task, and a
/// repo-joined agent with no task keeps its cwd's project.
#[test]
fn cad1752_agent_show_reports_the_turn_project() {
    let fx = Fx::start(Board::None);
    let repo = stage2_projects(&fx);
    fx.seed_catalogue("acct-show");
    assert_eq!(fx.turn_project("acct-show"), Value::Null);
    fx.plant_message("acct-show", "dispatch-show-1", "running", Some("ALPHA-4"));
    assert_eq!(fx.turn_project("acct-show"), "alpha");

    fx.seed_at("joined", &repo, None);
    assert_eq!(fx.turn_project("joined"), "alpha", "cwd fallback lost");
    // A running task on beta outranks the joined agent's cwd.
    fx.plant_message("joined", "dispatch-joined-1", "running", Some("BETA-2"));
    assert_eq!(fx.turn_project("joined"), "beta");
}

impl Fx {
    /// Move a planted message to `state` (a turn finishing or starting).
    fn set_message_state(&self, id: &str, state: &str) {
        let conn = rusqlite::Connection::open(self.state().join("cadence.sqlite3")).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        let changed = conn
            .execute(
                "UPDATE messages SET state=?1 WHERE id=?2",
                rusqlite::params![state, id],
            )
            .unwrap();
        assert_eq!(changed, 1, "message {id} not found");
    }
}

/// Finding from the stage-2 probe. Rule: the turn in flight decides. A
/// running turn with no issue (a ticketless chat) has no project, even
/// while a dispatch on alpha waits in the queue.
#[test]
fn cad1752_running_ticketless_turn_ignores_queued_dispatch() {
    let fx = Fx::start(Board::None);
    stage2_projects(&fx);
    fx.seed_catalogue("chat-1");
    fx.plant_message("chat-1", "chat-running", "running", None);
    fx.plant_message("chat-1", "dispatch-alpha-q", "queued", Some("ALPHA-7"));
    assert_eq!(
        fx.turn_project("chat-1"),
        Value::Null,
        "a ticketless running turn took the queued dispatch's project"
    );
    assert_project_refusal(
        fx.wiki_write("chat-1", "projects/alpha/notes.md"),
        "a ticketless running turn with alpha queued",
        "none",
    );

    // Control: the chat turn ends and the alpha dispatch runs.
    fx.set_message_state("chat-running", "completed");
    fx.set_message_state("dispatch-alpha-q", "running");
    assert_eq!(fx.turn_project("chat-1"), "alpha");
    let ok = fx
        .wiki_write("chat-1", "projects/alpha/notes.md")
        .unwrap_or_else(|e| panic!("the running alpha dispatch's write refused: {e}"));
    assert_eq!(ok["committed"], true, "{ok}");
}

/// A message in state `unknown` (delivery outcome not known) is not a
/// running turn and must not by itself give the agent a project.
#[test]
fn cad1752_unknown_state_dispatch_gives_no_project() {
    let fx = Fx::start(Board::None);
    stage2_projects(&fx);
    fx.seed_catalogue("dev-u");
    fx.plant_message("dev-u", "dispatch-alpha-u", "unknown", Some("ALPHA-8"));
    assert_eq!(
        fx.turn_project("dev-u"),
        Value::Null,
        "an 'unknown' message alone gave a project"
    );
    assert_project_refusal(
        fx.wiki_write("dev-u", "projects/alpha/notes.md"),
        "an 'unknown' alpha dispatch",
        "none",
    );

    // Control: the same dispatch running gives alpha.
    fx.set_message_state("dispatch-alpha-u", "running");
    assert_eq!(fx.turn_project("dev-u"), "alpha");
    let ok = fx.wiki_write("dev-u", "projects/alpha/notes.md").unwrap();
    assert_eq!(ok["committed"], true, "{ok}");
}

// ------------------------------- stage 2b/2c: relaunch per ticket (confined)
//
// A real daemon runs real Pi worker actors through the real `cadence
// confine` launcher; only `pi` itself is a stand-in (FAKE_PI below). The
// stand-in journals its argv and cwd into `<cwd>/pi-rpc.jsonl`, keeps a
// session file like Pi (header `cwd`, `--fork` makes a new file in
// `--session-dir`) and reports it as `sessionFile`, and holds any prompt
// containing HOLD until `<cwd>/release` exists. Dispatched messages and
// the daemon's dispatch record (`<state>/dispatches.json`) are planted:
// both are daemon-written in production.

const FAKE_PI: &str = r#"#!/usr/bin/env python3
import json, os, sys, time
args = sys.argv[1:]
def opt(name):
    return args[args.index(name) + 1] if name in args else None
cwd = os.getcwd()
def journal(obj):
    obj["cwd"] = cwd
    with open(os.path.join(cwd, "pi-rpc.jsonl"), "a") as f:
        f.write(json.dumps(obj) + "\n")
journal({"argv": args})
try:
    with open(os.path.join(cwd, "..", "alpha-2", "escape"), "w") as f:
        f.write("escaped")
    journal({"escape": "written"})
except OSError as e:
    journal({"escape": type(e).__name__})
session = opt("--session")
if opt("--fork") and os.path.exists(os.path.join(cwd, "fail-fork")):
    journal({"fork": "refused"})
    sys.exit(3)
if opt("--fork"):
    src = opt("--fork")
    with open(src) as f:
        lines = f.read().splitlines()
    header = json.loads(lines[0]) if lines else {}
    header.update({"cwd": cwd, "parentSession": src, "id": opt("--session-id")})
    session = os.path.join(opt("--session-dir"),
                           "%d_%s.jsonl" % (int(time.time() * 1000), opt("--session-id")))
    with open(session, "w") as f:
        f.write("\n".join([json.dumps(header)] + lines[1:]) + "\n")
elif session and not os.path.exists(session):
    with open(session, "w") as f:
        f.write(json.dumps({"type": "session", "cwd": cwd}) + "\n")
model = opt("--model") or "fake/model-1"
provider, _, model_id = model.partition("/")
def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
for line in sys.stdin:
    req = json.loads(line)
    kind, rid = req.get("type"), req.get("id")
    reply = {"type": "response", "id": rid, "command": kind, "success": True}
    if kind == "get_state":
        reply["data"] = {"sessionId": "fake", "sessionFile": session,
                         "thinkingLevel": "medium",
                         "model": {"provider": provider, "id": model_id}}
    if kind != "prompt":
        emit(reply)
        continue
    text = req.get("message", "")
    journal({"rpc": "prompt", "hold": "HOLD" in text})
    emit(reply)
    while "HOLD" in text and not os.path.exists(os.path.join(cwd, "release")):
        time.sleep(0.05)
    emit({"type": "message_end", "message": {"role": "assistant",
          "content": [{"type": "text", "text": "done"}], "stopReason": "stop"}})
    emit({"type": "agent_settled"})
"#;

struct LaneFx {
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    env: cadence_agent::adapter::ProviderEnv,
    /// Opened before the daemon: reopening a Store runs crash recovery.
    store: Store,
}

impl Drop for LaneFx {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        for thread in self.threads.drain(..).rev() {
            let _ = thread.join();
        }
    }
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = cadence_agent::reaper::output(
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=c1752",
                "-c",
                "user.email=c1752@example.test",
            ])
            .args(args),
    )
    .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

impl LaneFx {
    fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("c1752l-")
            .tempdir()
            .unwrap();
        let path = |p: &str| root.path().join(p);
        for dir in ["h", "outside", "s"] {
            std::fs::create_dir_all(path(dir)).unwrap();
        }
        let pm = path("pm");
        cadence_agent::issue::Pm::init(&pm).unwrap();
        let yaml = pm.join("pm.yaml");
        let mut text = std::fs::read_to_string(&yaml).unwrap();
        text.push_str(PI_BLOCK);
        std::fs::write(&yaml, text).unwrap();
        // alpha: a repo with two lane worktrees and a symlink lane that
        // points outside; other: a repo no project declares.
        for repo in ["alpha-repo", "other-repo"] {
            std::fs::create_dir_all(path(repo)).unwrap();
            git(&path(repo), &["init", "-q"]);
            git(
                &path(repo),
                &["commit", "-q", "--allow-empty", "-m", "init"],
            );
            std::fs::create_dir_all(path(repo).join(".cadence/wt")).unwrap();
        }
        for lane in ["alpha-1", "alpha-2"] {
            git(
                &path("alpha-repo"),
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    lane,
                    &format!(".cadence/wt/{lane}"),
                ],
            );
        }
        git(
            &path("other-repo"),
            &["worktree", "add", "-q", "-b", "x", ".cadence/wt/x"],
        );
        std::os::unix::fs::symlink(path("outside"), path("alpha-repo/.cadence/wt/evil")).unwrap();
        let repo = std::fs::canonicalize(path("alpha-repo")).unwrap();
        std::fs::create_dir_all(pm.join("alpha")).unwrap();
        std::fs::write(
            pm.join("alpha/project.yaml"),
            format!(
                "key: alpha\nprefix: ALPHA\nrepos:\n  - path: {}\n",
                repo.display()
            ),
        )
        .unwrap();
        let fake = path("fake-pi.py");
        std::fs::write(&fake, FAKE_PI).unwrap();

        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
        env.set("HOME", path("h").to_str().unwrap());
        env.set("CADENCE_PI_COMMAND", format!("python3 {}", fake.display()));
        env.set("CADENCE_CONFINE_COMMAND", env!("CARGO_BIN_EXE_cadence"));
        env.set(
            cadence_agent::adapter::pi::CONFINE_WORKER_EXTRA_READ_ENV,
            fake.to_str().unwrap(),
        );
        let store = Store::open(&path("s").join("cadence.sqlite3")).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let opts = daemon::ServeOptions {
            provider_env: env.clone(),
            stop: Some(stop.clone()),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        let state = path("s");
        let threads = vec![std::thread::spawn(move || {
            daemon::serve_with(&state, opts).unwrap();
        })];
        let fx = Self {
            root,
            stop,
            threads,
            env,
            store,
        };
        fx.until("daemon", || {
            client::rpc_timeout(&fx.state(), "health", json!({}), Duration::from_secs(2)).is_ok()
                && Seam::token_at(&fx.state()).is_some()
        });
        fx
    }

    fn path(&self, p: &str) -> PathBuf {
        std::fs::canonicalize(self.root.path()).unwrap().join(p)
    }
    fn state(&self) -> PathBuf {
        self.root.path().join("s")
    }
    fn lane(&self, name: &str) -> PathBuf {
        self.path("alpha-repo/.cadence/wt").join(name)
    }

    fn until(&self, what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !done() {
            assert!(Instant::now() < deadline, "{what}: timed out");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let state = self.state();
        scoped(who, || client::rpc(&state, method, params))
    }
    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }
    fn show(&self, alias: &str) -> Value {
        self.op("agent_show", json!({"alias": alias, "limit": 0}))["agent"].clone()
    }

    /// Register a pi worker as the operator and wait for its endpoint.
    fn hire(&self, params: Value) -> String {
        let alias = params["alias"].as_str().unwrap().to_string();
        self.op("agent_register", params);
        self.until(&format!("{alias} idle"), || {
            let shown = self.show(&alias);
            assert_ne!(
                shown["state"],
                "attention",
                "{alias} failed to open: {shown}; log: {}",
                std::fs::read_to_string(self.state().join(format!("agents/{alias}.provider.log")))
                    .unwrap_or_default()
            );
            shown["state"] == "idle" && shown["pid"].is_number()
        });
        alias
    }

    fn record_dispatch(&self, issue: &str, worktree: &std::path::Path) {
        let path = self.state().join("dispatches.json");
        let mut all: Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| json!({}));
        all[issue] = json!({"issue": issue, "pm": "operator", "worker": "x",
                            "worktree": worktree.to_string_lossy(), "message": "k"});
        std::fs::write(&path, all.to_string()).unwrap();
    }

    fn plant(
        &self,
        alias: &str,
        id: &str,
        issue: Option<&str>,
        worktree: Option<&std::path::Path>,
    ) {
        let conn = rusqlite::Connection::open(self.state().join("cadence.sqlite3")).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn.execute(
            "INSERT INTO messages(id, alias, body, source, state, created, issue, worktree) \
             VALUES (?1, ?2, 'fixture turn', 'dispatch', 'queued', ?3, ?4, ?5)",
            rusqlite::params![
                id,
                alias,
                cadence_agent::issue::time::now_epoch() as f64,
                issue,
                worktree.map(|w| w.to_string_lossy().into_owned())
            ],
        )
        .unwrap();
    }

    /// An operator send: queues a ticketless turn and wakes the actor.
    fn send(&self, alias: &str, id: &str, text: &str) {
        self.op(
            "agent_send",
            json!({"alias": alias, "text": text, "message": id}),
        );
    }

    fn message(&self, id: &str) -> (String, Option<String>) {
        let conn = rusqlite::Connection::open_with_flags(
            self.state().join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn.query_row("SELECT state, error FROM messages WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
    }

    fn settled(&self, id: &str) -> (String, Option<String>) {
        let mut out = (String::new(), None);
        self.until(&format!("message {id} settles"), || {
            out = self.message(id);
            matches!(out.0.as_str(), "completed" | "failed")
        });
        out
    }

    fn relaunches(&self, alias: &str) -> Vec<Value> {
        let conn = rusqlite::Connection::open_with_flags(
            self.state().join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        let mut stmt = conn
            .prepare("SELECT payload FROM events WHERE alias=?1 AND kind='relaunched_for_lane' ORDER BY seq")
            .unwrap();
        stmt.query_map([alias], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|p| serde_json::from_str(&p.unwrap()).unwrap())
            .collect()
    }

    fn journal(&self, dir: &std::path::Path) -> Vec<Value> {
        std::fs::read_to_string(dir.join("pi-rpc.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

fn confined_hire(alias: &str) -> Value {
    json!({"alias": alias, "catalogue": "developer", "params": r#"{"confine":true}"#})
}

/// Items 1 and the positive relaunch. Guards: `lane_workdir_needed` and
/// `grantable_lane` (`src/daemon/lane_relaunch.rs`) pick the recorded lane;
/// `launch_workdir` (`src/adapter/pi_confine.rs`) feeds the worker grant,
/// which then covers that lane, its git common dir and the agent's own
/// dir — and no other lane, no repo checkout root, no $HOME.
#[test]
fn cad1752_confined_hire_relaunches_into_the_recorded_lane_only() {
    let fx = LaneFx::start();
    let alias = fx.hire(confined_hire("dev-c"));
    let home = std::fs::canonicalize(fx.show(&alias)["cwd"].as_str().unwrap()).unwrap();
    let lane = fx.lane("alpha-1");
    fx.record_dispatch("ALPHA-1", &lane);
    fx.plant(&alias, "dispatch-alpha-1", Some("ALPHA-1"), Some(&lane));
    fx.send(&alias, "after-1", "ticketless follow-up");
    let (state, error) = fx.settled("dispatch-alpha-1");
    assert_eq!(state, "completed", "lane turn: {error:?}");
    fx.settled("after-1");

    let events = fx.relaunches(&alias);
    assert!(
        events.first().is_some_and(
            |e| e["issue"] == "ALPHA-1" && e["workdir"] == lane.to_string_lossy().as_ref()
        ),
        "no relaunch into the recorded lane: {events:?}"
    );
    // The stand-in ran in the lane for the dispatch, then back home.
    let starts = |dir: &std::path::Path| {
        fx.journal(dir)
            .into_iter()
            .filter(|j| j.get("argv").is_some())
            .collect::<Vec<_>>()
    };
    let in_lane = starts(&lane);
    assert_eq!(in_lane.len(), 1, "lane launches: {in_lane:?}");
    // Live Landlock: the lane launch cannot write the sibling lane.
    let escape: Vec<Value> = fx
        .journal(&lane)
        .into_iter()
        .filter_map(|j| j.get("escape").cloned())
        .collect();
    assert_eq!(
        escape,
        vec![json!("PermissionError")],
        "sibling-lane write: {escape:?}"
    );
    assert!(
        !fx.lane("alpha-2").join("escape").exists(),
        "escaped into alpha-2"
    );
    eprintln!("CAD-1752 lane launch argv: {}", in_lane[0]["argv"]);
    assert!(
        events
            .iter()
            .any(|e| e["workdir"] == home.to_string_lossy().as_ref()),
        "the ticketless follow-up did not relaunch home: {events:?}"
    );

    // The grant a lane launch builds, from the same function and env.
    let mut agent = fx.store.agent(&alias).unwrap();
    agent.params.as_mut().unwrap()["launch_workdir"] = json!(lane.to_string_lossy());
    let (_, policy) =
        cadence_agent::adapter::pi::pi_worker_confinement(&fx.env, &fx.state(), &agent);
    let write = &policy.write;
    eprintln!("CAD-1752 lane write set: {write:?}");
    let covered = |p: &std::path::Path| write.iter().any(|w| p.starts_with(w));
    let common = fx.path("alpha-repo/.git");
    assert!(write.contains(&lane), "lane not granted: {write:?}");
    assert!(covered(&common), "git common dir not granted: {write:?}");
    assert!(
        covered(&fx.state().join("agents").join(&alias))
            || covered(
                &std::fs::canonicalize(fx.state())
                    .unwrap()
                    .join("agents")
                    .join(&alias)
            ),
        "agent dir not granted: {write:?}"
    );
    for (what, path) in [
        ("another lane", fx.lane("alpha-2")),
        ("the repo checkout root", fx.path("alpha-repo/README")),
        ("$HOME", fx.path("h")),
        ("the outside dir", fx.path("outside")),
        (
            "an undeclared repo's lane",
            fx.path("other-repo/.cadence/wt/x"),
        ),
    ] {
        assert!(
            !covered(&path),
            "{what} ({}) is writable: {write:?}",
            path.display()
        );
    }
}

/// Item 2. Guard: `grantable_lane` refuses every lane that is not the
/// daemon-recorded worktree of a declared repo, failing the message with a
/// stated reason and relaunching nothing.
#[test]
fn cad1752_lane_relaunch_refusals_grant_nothing() {
    let fx = LaneFx::start();
    let alias = fx.hire(confined_hire("dev-r"));
    let pid = fx.show(&alias)["pid"].clone();
    let missing = fx.path("alpha-repo/.cadence/wt/gone");
    let evil = fx.path("alpha-repo/.cadence/wt/evil");
    let foreign = fx.path("other-repo/.cadence/wt/x");
    // ALPHA-11 has a valid record; only its message carries no worktree.
    fx.record_dispatch("ALPHA-11", &fx.lane("alpha-1"));
    fx.record_dispatch("ALPHA-12", &missing);
    fx.record_dispatch("ALPHA-13", &fx.lane("alpha-1"));
    fx.record_dispatch("ALPHA-14", &evil);
    fx.record_dispatch("ALPHA-15", &foreign);
    let cases: [(&str, &str, Option<PathBuf>, &str); 5] = [
        (
            "no-wt",
            "ALPHA-11",
            None,
            "the dispatch recorded no worktree",
        ),
        (
            "missing",
            "ALPHA-12",
            Some(missing.clone()),
            "is missing or deleted",
        ),
        (
            "other-lane",
            "ALPHA-13",
            Some(fx.lane("alpha-2")),
            "is not the worktree recorded for ALPHA-13's dispatch",
        ),
        (
            "symlink",
            "ALPHA-14",
            Some(evil.clone()),
            "is not a lane worktree (.cadence/wt/)",
        ),
        (
            "foreign",
            "ALPHA-15",
            Some(foreign.clone()),
            "is not a lane worktree (.cadence/wt/)",
        ),
    ];
    for (id, issue, wt, _) in &cases {
        fx.plant(&alias, id, Some(issue), wt.as_deref());
    }
    fx.send(&alias, "wake-r", "ticketless");
    let mut wrong = Vec::new();
    for (id, issue, _, why) in &cases {
        let (state, error) = fx.settled(id);
        let error = error.unwrap_or_default();
        if state != "failed"
            || !error.contains(&format!("relaunch for {issue} refused, nothing granted"))
            || !error.contains(why)
        {
            wrong.push(format!("{id}: {state}: {error}"));
        }
    }
    assert_eq!(fx.settled("wake-r").0, "completed");
    assert!(wrong.is_empty(), "lanes not refused as stated: {wrong:#?}");
    assert!(
        fx.relaunches(&alias).is_empty(),
        "{:?}",
        fx.relaunches(&alias)
    );
    let shown = fx.show(&alias);
    assert!(
        shown["params"]
            .get("launch_workdir")
            .is_none_or(Value::is_null),
        "{shown}"
    );
    assert_eq!(shown["pid"], pid, "the endpoint was relaunched");
    assert!(fx.journal(&fx.path("outside")).is_empty(), "ran outside");

    // Control: the recorded lane, same agent, same state.
    fx.record_dispatch("ALPHA-16", &fx.lane("alpha-1"));
    fx.plant(
        &alias,
        "ok-lane",
        Some("ALPHA-16"),
        Some(&fx.lane("alpha-1")),
    );
    fx.send(&alias, "wake-ok", "ticketless");
    assert_eq!(fx.settled("ok-lane").0, "completed");
    assert!(
        fx.relaunches(&alias)
            .iter()
            .any(|e| e["issue"] == "ALPHA-16"),
        "{:?}",
        fx.relaunches(&alias)
    );
}

/// Item 3. Guards: `rpc_register` and `rpc_set` refuse a caller-set
/// `params.launch_workdir` — for the operator as well.
#[test]
fn cad1752_caller_cannot_set_launch_workdir() {
    let fx = Fx::start(Board::None);
    fx.seed("pm-root", None, None);
    let lane = fx.root.path().join("lane");
    std::fs::create_dir_all(&lane).unwrap();
    let forged = json!({"launch_workdir": lane.to_string_lossy()}).to_string();
    let mut wrong: Vec<String> = Vec::new();
    for (what, params) in [
        (
            "catalogue hire",
            json!({"alias": "lw-1", "catalogue": "developer", "params": forged}),
        ),
        (
            "start blank",
            json!({"alias": "lw-1", "provider": "pi", "cwd": fx.root.path().to_string_lossy(),
                   "params": forged}),
        ),
    ] {
        match fx.rpc(Asserted::Operator, "agent_register", params) {
            Err(err)
                if err
                    .to_string()
                    .contains("params 'launch_workdir' is set by the daemon") => {}
            other => wrong.push(format!("register {what}: {other:?}")),
        }
        if fx.agent("lw-1").is_some() {
            wrong.push(format!("register {what}: row created"));
            fx.op("agent_remove", json!({"alias": "lw-1", "force": true}));
        }
    }
    fx.op(
        "agent_register",
        json!({"alias": "lw-1", "catalogue": "developer"}),
    );
    for (what, who) in [
        ("the operator", Asserted::Operator),
        ("the agent itself", Asserted::Agent("lw-1".into())),
    ] {
        for next_launch in [false, true] {
            let err = fx
                .rpc(
                    who.clone(),
                    "agent_set",
                    json!({"alias": "lw-1", "next_launch": next_launch,
                           "patch": {"launch_workdir": lane.to_string_lossy()}}),
                )
                .err();
            if err.is_none() {
                wrong.push(format!("{what} (next_launch={next_launch}): admitted"));
            }
            eprintln!(
                "CAD-1752 agent_set launch_workdir by {what} (next_launch={next_launch}): {err:?}"
            );
        }
    }
    match fx.rpc(
        Asserted::Operator,
        "agent_set",
        json!({"alias": "lw-1", "patch": {"launch_workdir": lane.to_string_lossy()}}),
    ) {
        Err(err)
            if err
                .to_string()
                .contains("params 'launch_workdir' is set by the daemon") => {}
        other => wrong.push(format!("operator agent set: {other:?}")),
    }
    if fx
        .agent("lw-1")
        .unwrap()
        .params
        .unwrap()
        .get("launch_workdir")
        .is_some()
    {
        wrong.push("agent set stored launch_workdir".into());
    }
    assert!(wrong.is_empty(), "launch_workdir not refused: {wrong:#?}");
    // Control: an ordinary patch from the operator applies.
    fx.op(
        "agent_set",
        json!({"alias": "lw-1", "next_launch": true, "patch": {"effort": "high"}}),
    );
}

/// Item 4. Guards: the actor checks `lane_workdir_needed` only when it
/// takes the next message (never mid-turn), and only a confined
/// catalogue Pi worker is subject to it.
#[test]
fn cad1752_no_relaunch_mid_turn_or_for_others() {
    let fx = LaneFx::start();
    let lane = fx.lane("alpha-1");
    fx.record_dispatch("ALPHA-21", &lane);
    fx.record_dispatch("ALPHA-22", &lane);
    fx.record_dispatch("ALPHA-23", &lane);

    // Mid-turn: a held ticketless turn, with the dispatch queued behind it.
    let alias = fx.hire(confined_hire("dev-m"));
    let home = std::fs::canonicalize(fx.show(&alias)["cwd"].as_str().unwrap()).unwrap();
    fx.send(&alias, "held", "HOLD this turn");
    fx.until("held turn running", || fx.message("held").0 == "running");
    fx.plant(&alias, "queued-lane", Some("ALPHA-21"), Some(&lane));
    std::thread::sleep(Duration::from_millis(1500));
    assert!(fx.relaunches(&alias).is_empty(), "relaunched mid-turn");
    assert_eq!(fx.message("held").0, "running");
    std::fs::write(home.join("release"), b"").unwrap();
    assert_eq!(fx.settled("held").0, "completed");
    fx.send(&alias, "wake-m", "ticketless");
    assert_eq!(fx.settled("queued-lane").0, "completed");
    assert!(
        fx.relaunches(&alias)
            .iter()
            .any(|e| e["issue"] == "ALPHA-21"),
        "control: the queued lane turn did not relaunch after the held turn"
    );

    // An unconfined catalogue agent and a non-catalogue confined worker.
    let open = fx.hire(json!({"alias": "dev-open", "catalogue": "developer",
                              "params": r#"{"confine":false}"#}));
    let cli = fx.hire(json!({"alias": "dev-cli", "provider": "pi",
                             "cwd": fx.path("alpha-repo").to_string_lossy(),
                             "params": r#"{"confine":true}"#}));
    let mut wrong = Vec::new();
    for (who, issue, id) in [
        (&open, "ALPHA-22", "open-lane"),
        (&cli, "ALPHA-23", "cli-lane"),
    ] {
        fx.plant(who, id, Some(issue), Some(&lane));
        fx.send(who, &format!("{id}-wake"), "ticketless");
        let state = fx.settled(id).0;
        let shown = fx.show(who);
        if state != "completed"
            || !fx.relaunches(who).is_empty()
            || !shown["params"]
                .get("launch_workdir")
                .is_none_or(Value::is_null)
        {
            wrong.push(format!(
                "{who}: {state}; relaunches {:?}",
                fx.relaunches(who)
            ));
        }
    }
    assert!(wrong.is_empty(), "relaunched outside the rule: {wrong:#?}");
}

/// A lane relaunch whose endpoint cannot open (the fork fails) settles the
/// message once as failed and fences the agent: one attempt, no fresh
/// session, no wider grant. Guard: the relaunch error arm in the actor
/// loop (`src/daemon/actor.rs`).
#[test]
fn cad1752_failed_lane_relaunch_fails_the_message_once() {
    let fx = LaneFx::start();
    // Control: a fork that succeeds, into alpha-2.
    let ok = fx.hire(confined_hire("dev-ok"));
    fx.record_dispatch("ALPHA-2", &fx.lane("alpha-2"));
    fx.plant(&ok, "fork-ok", Some("ALPHA-2"), Some(&fx.lane("alpha-2")));
    fx.send(&ok, "fork-ok-wake", "ticketless");
    assert_eq!(fx.settled("fork-ok").0, "completed", "control fork");

    // The same setup, but the stand-in exits 3 on a --fork in alpha-1.
    let lane = fx.lane("alpha-1");
    std::fs::write(lane.join("fail-fork"), b"").unwrap();
    let alias = fx.hire(confined_hire("dev-f"));
    let home = std::fs::canonicalize(fx.show(&alias)["cwd"].as_str().unwrap()).unwrap();
    let starts = |dir: &std::path::Path| -> Vec<Value> {
        fx.journal(dir)
            .into_iter()
            .filter_map(|j| j.get("argv").cloned())
            .collect()
    };
    let home_before = starts(&home).len();
    fx.record_dispatch("ALPHA-1", &lane);
    fx.plant(&alias, "fork-fail", Some("ALPHA-1"), Some(&lane));
    fx.send(&alias, "fork-fail-wake", "ticketless");
    let (state, error) = fx.settled("fork-fail");
    let error = error.unwrap_or_default();
    assert_eq!(state, "failed", "{error}");
    assert!(
        error.contains("relaunch for ALPHA-1 failed"),
        "not the relaunch failure: {error}"
    );
    fx.until("agent fenced", || fx.show(&alias)["state"] == "attention");
    std::thread::sleep(Duration::from_millis(2000));
    let shown = fx.show(&alias);
    assert_eq!(shown["state"], "attention", "{shown}");
    eprintln!("CAD-1752 fenced agent error: {}", shown["error"]);
    assert!(
        shown["error"].as_str().is_some_and(|e| !e.is_empty()),
        "attention without an error: {shown}"
    );
    // One attempt in the lane, and it was the fork.
    let in_lane: Vec<Value> = starts(&lane)
        .into_iter()
        .filter(|argv| argv.to_string().contains("dev-f"))
        .collect();
    assert_eq!(in_lane.len(), 1, "lane launch attempts: {in_lane:?}");
    assert!(in_lane[0].to_string().contains("--fork"), "{in_lane:?}");
    // No fresh-session fallback anywhere, and no relaunch home.
    assert_eq!(
        starts(&home).len(),
        home_before,
        "relaunched home after the failure"
    );
    let events = fx.relaunches(&alias);
    eprintln!("CAD-1752 relaunched_for_lane events after the failure: {events:?}");
    assert!(events.len() <= 1, "relaunch loop: {events:?}");
    // The follow-up was not run by a fallback endpoint.
    assert_ne!(fx.message("fork-fail-wake").0, "completed");
    // Not widened: the stored workdir is the recorded lane or none.
    let stored = shown["params"]
        .get("launch_workdir")
        .cloned()
        .unwrap_or(Value::Null);
    eprintln!("CAD-1752 launch_workdir after the failure: {stored}");
    assert!(
        stored.is_null() || stored == json!(lane.to_string_lossy()),
        "launch_workdir widened: {stored}"
    );
}

// ------------------------------------------- stage 3: what the board serves

/// Every object key named `key`, anywhere in `value`.
fn keys_named(value: &Value, key: &str, path: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let here = format!("{path}.{k}");
                if k == key {
                    out.push(here.clone());
                }
                keys_named(v, key, &here, out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                keys_named(v, key, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

const SAVED_JOB: &str = "Chase the c1752 saved-job-needle invoices every Friday.";

/// A catalogue hire with a saved job, registered and edited by the operator.
fn hire_with_saved_job(fx: &Fx, alias: &str) {
    fx.op(
        "agent_register",
        json!({"alias": alias, "catalogue": "accountant"}),
    );
    fx.op(
        "agent_instructions_set",
        json!({"alias": alias, "instructions": SAVED_JOB}),
    );
}

/// The job is absent: no `instructions` key and no job text anywhere.
fn assert_no_job(reply: &Reply, who: &str) {
    let mut found = Vec::new();
    keys_named(&reply.body, "instructions", "$", &mut found);
    assert!(found.is_empty(), "{who}: instructions served at {found:?}");
    assert!(
        !reply.raw.contains("saved-job-needle"),
        "{who}: the saved job text is in the payload"
    );
}

/// Guard: `admit_operator_read` decides `operator` for the detail route
/// (`src/ui/serve.rs`, `RouteId::AgentDetail`), which `agent_detail` passes
/// as `include_profile_instructions`.
#[test]
fn cad1752_board_agent_detail_hides_job_from_non_operator() {
    // Public board: the owner reads the job, a member does not.
    let fx = Fx::start(Board::Public);
    hire_with_saved_job(&fx, "acct-d");
    let route = "/api/agents/acct-d";
    let member = fx.platform_cookie("member");
    let reply = fx.http(&Asserted::Operator, "GET", route, None, Some(&member), None);
    assert_eq!(reply.status, 200, "member detail: {}", reply.raw);
    assert_eq!(reply.body["agent"]["alias"], "acct-d", "{}", reply.raw);
    assert_no_job(&reply, "member");
    let mut params = Vec::new();
    keys_named(&reply.body, "params", "$", &mut params);
    eprintln!(
        "CAD-1752 member detail params: {:?} {}",
        params, reply.body["agent"]["params"]
    );
    let owner = fx.platform_cookie("owner");
    let reply = fx.http(&Asserted::Operator, "GET", route, None, Some(&owner), None);
    assert_eq!(reply.status, 200, "owner detail: {}", reply.raw);
    assert_eq!(reply.body["agent"]["instructions"], SAVED_JOB, "owner");
    drop(fx);

    // Loopback board: the operator's session reads it, an agent does not.
    let fx = Fx::start(Board::Loopback);
    hire_with_saved_job(&fx, "acct-d");
    fx.seed("peer-1", None, None);
    let (cookie, key) = fx.operator_session();
    for who in ["acct-d", "peer-1"] {
        let reply = fx.http(&Asserted::Agent(who.into()), "GET", route, None, None, None);
        eprintln!("CAD-1752 agent {who} detail status: {}", reply.status);
        assert_no_job(&reply, who);
    }
    let reply = fx.http(
        &Asserted::Operator,
        "GET",
        route,
        None,
        Some(&cookie),
        Some(&key),
    );
    assert_eq!(reply.status, 200, "operator detail: {}", reply.raw);
    assert_eq!(reply.body["agent"]["instructions"], SAVED_JOB, "operator");
}

/// Guard: `agents_payload_from` (`src/ui/serve.rs`) copies only
/// `catalogue`/`look`/`kind` from a hire's params into the roster row.
#[test]
fn cad1752_board_agent_list_serves_only_role_identity() {
    let fx = Fx::start(Board::Public);
    fx.seed("pm-root", None, None);
    fx.seed(
        "acct-l",
        Some(
            r#"{"catalogue":"accountant","look":"bookkeeper","kind":"apps","upstream":"pm-root","launch_workdir":"/c1752/secret-lane-needle","profile":"secret-profile-needle"}"#,
        ),
        None,
    );
    let member = fx.platform_cookie("member");
    let mut row = Value::Null;
    let mut raw = String::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while row.is_null() {
        assert!(Instant::now() < deadline, "acct-l never listed: {raw}");
        let reply = fx.http(
            &Asserted::Operator,
            "GET",
            "/api/agents",
            None,
            Some(&member),
            None,
        );
        assert_eq!(reply.status, 200, "member list: {}", reply.raw);
        row = reply.body["agents"]
            .as_array()
            .and_then(|rows| rows.iter().find(|r| r["alias"] == "acct-l").cloned())
            .unwrap_or(Value::Null);
        raw = reply.raw;
        std::thread::sleep(Duration::from_millis(200));
    }
    // Positive: the role identity is served.
    assert_eq!(row["catalogue"], "accountant", "{row}");
    assert_eq!(row["look"], "bookkeeper", "{row}");
    assert_eq!(row["kind"], "apps", "{row}");
    // Nothing else of params.
    let mut wrong = Vec::new();
    for key in ["params", "launch_workdir", "profile", "upstream"] {
        keys_named(&row, key, "$", &mut wrong);
    }
    assert!(
        wrong.is_empty(),
        "params keys in the roster row: {wrong:?} {row}"
    );
    let text = row.to_string();
    for needle in ["secret-lane-needle", "secret-profile-needle"] {
        assert!(!text.contains(needle), "{needle} served in {row}");
    }
    eprintln!("CAD-1752 roster group for acct-l: {}", row["group"]);
}

/// FAILING FIRST. A catalogue hire whose role model would not launch (not
/// on the Pi allowlist) is refused by `agent_register` with the reason
/// `agent_roles` shows; nothing is left behind. Control: once the model is
/// allowlisted for workers, the same hire registers.
#[test]
fn cad1752_catalogue_register_refuses_unlaunchable_model() {
    let fx = Fx::start(Board::None);
    let snapshot = fx.op("agent_settings_get", json!({}));
    eprintln!("CAD-1752 settings config: {}", snapshot["config"]);
    let allowed = snapshot["config"].clone();
    // Two ways a role's model cannot launch for a worker:
    // - its role default is master-scoped;
    // - no role default, so the Pi policy's own fallback picks a model
    //   that is not on the worker allowlist.
    let mut master_scoped = allowed.clone();
    master_scoped["scope"]["fake/model-1"] = json!("master");
    let mut no_default = master_scoped.clone();
    no_default["roleDefaults"]["worker"] = json!("");
    let mut revision = snapshot["revision"].clone();
    let mut wrong = Vec::new();
    for (what, alias, config) in [
        ("master-scoped role default", "acct-m", master_scoped),
        ("policy fallback off the allowlist", "acct-n", no_default),
    ] {
        let saved = fx.op(
            "agent_settings_set",
            json!({"expected_revision": revision, "config": config}),
        );
        revision = saved["revision"].clone();
        let roles = fx.op("agent_roles", json!({}));
        let role = roles["roles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "accountant")
            .cloned()
            .unwrap();
        let Some(reason) = role["model"]["error"].as_str().map(str::to_string) else {
            wrong.push(format!(
                "{what}: agent_roles resolves a launchable model: {role}"
            ));
            continue;
        };
        eprintln!("CAD-1752 {what}: agent_roles error: {reason}");
        match fx.rpc(
            Asserted::Operator,
            "agent_register",
            json!({"alias": alias, "catalogue": "accountant"}),
        ) {
            Err(err)
                if matches!(err, cadence_agent::Error::Rejected(_))
                    && err.to_string().contains(&reason) => {}
            other => wrong.push(format!(
                "{what}: register answered {other:?}, not agent_roles' reason ({reason})"
            )),
        }
        if fx.agent(alias).is_some() {
            wrong.push(format!("{what}: row created"));
        }
        if fx.home(alias).exists() {
            wrong.push(format!("{what}: home dir left behind"));
        }
        if client::briefing_path(&fx.state(), &json!({}), alias).exists() {
            wrong.push(format!("{what}: briefing written"));
        }
    }
    assert!(
        wrong.is_empty(),
        "unlaunchable hires not refused: {wrong:#?}"
    );
    let saved = json!({"revision": revision});

    // Control: allowlist the model for workers again; the same hire works.
    let mut restored = allowed;
    restored["scope"]["fake/model-1"] = json!("worker");
    fx.op(
        "agent_settings_set",
        json!({"expected_revision": saved["revision"], "config": restored}),
    );
    fx.op(
        "agent_register",
        json!({"alias": "acct-ok", "catalogue": "accountant"}),
    );
    assert!(fx.agent("acct-ok").is_some());
}
