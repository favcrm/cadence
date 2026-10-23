//! CAD-1772 independent FAILING-FIRST checks, written by the check author
//! from the ticket before the fix. The implementer must not edit or weaken
//! this file. Harness helpers are copied from
//! `tests/cad1767_followup_acceptance.rs` (that file is not edited).
//!
//! 1. The master's "Recruit" conversation is operator-only, like Home: a
//!    member read of it on every route (thread reads, `/stream`, agent
//!    detail, master state, and a route sweep) carries none of its
//!    entries or turn text; the owner sees them.
//! 2. `cadence agent roles` (the `agent_roles` read): the master may run
//!    it on its Home turn (allowlist and the Pi guard argv, which derives
//!    from the allowlist) and it lists the 12 catalogue roles; an
//!    app-scoped master turn is refused as other non-app verbs are.
//!
//! Run: `cargo test --features test-seam --test suite cad1772_ -- --test-threads 1`
#![cfg(all(feature = "test-seam", target_os = "linux"))]
#![allow(dead_code)]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cadence_agent::store::{NewAgent, Store};
use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon, operator_auth, ui};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use std::io::{Read, Write};
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

// ------------------------------------------------------------------- G1

fn session_header(cwd: &std::path::Path) -> String {
    format!(
        "{}\n",
        json!({"type": "session", "version": 3, "id": "c1767", "cwd": cwd})
    )
}

impl LaneFx {
    /// Stop and resume `alias`; wait until a new endpoint is idle.
    fn restart(&self, alias: &str) {
        let before = self.show(alias)["pid"].clone();
        self.op("agent_stop", json!({"alias": alias}));
        self.until(&format!("{alias} stopped"), || {
            self.show(alias)["state"] == "stopped"
        });
        self.op("agent_resume", json!({"alias": alias}));
        self.until(&format!("{alias} idle again"), || {
            let shown = self.show(alias);
            shown["state"] == "idle" && shown["pid"].is_number() && shown["pid"] != before
        });
    }

    /// The `--session` argument of the newest launch journaled in `cwd`.
    fn last_session(&self, cwd: &std::path::Path) -> Option<String> {
        self.journal(cwd)
            .into_iter()
            .filter_map(|j| j.get("argv").cloned())
            .next_back()
            .and_then(|argv| {
                let argv: Vec<String> = serde_json::from_value(argv).ok()?;
                let i = argv.iter().position(|a| a == "--session")?;
                argv.get(i + 1).cloned()
            })
    }

    /// Every stated trace of a refused pointer: event kinds/payloads and
    /// the provider log lines that mention `session.current`.
    fn pointer_reasons(&self, alias: &str) -> Vec<String> {
        let conn = rusqlite::Connection::open_with_flags(
            self.state().join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        let mut stmt = conn
            .prepare("SELECT kind || ' ' || payload FROM events WHERE alias=?1")
            .unwrap();
        let mut out: Vec<String> = stmt
            .query_map([alias], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .filter(|e| e.contains("session.current"))
            .collect();
        for log in [
            self.state().join(format!("agents/{alias}.provider.log")),
            self.state().join("daemon.log"),
        ] {
            out.extend(
                std::fs::read_to_string(log)
                    .unwrap_or_default()
                    .lines()
                    .filter(|l| l.contains("session.current"))
                    .map(str::to_string),
            );
        }
        out
    }
}

// ------------------------------------------------------------------- G2

/// Every entry kind in a thread page, and whether the proposal leaked.
fn page_kinds(page: &Value) -> Vec<String> {
    page["entries"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|e| e["kind"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

// ------------------------------------------------------------------- G3

// ----------------------------------------- G2 (operator decision, 2026-10-23)
//
// Members MAY read agent threads, but every member read of
// `/api/threads/<alias>` (plain, tail, before, conversation=, /stream)
// strips operator-only content: artifact cards, hire proposals, and the
// master's Home conversation. The operator still gets the full thread.

const HELPER_CHAT: &str = "helper-ordinary-needle";
const HOME_CHAT: &str = "master-home-secret-needle";

impl Fx {
    fn append(&self, store: &Store, thread: &str, role: &str, kind: &str, text: &str) -> i64 {
        store
            .thread_append_to(
                thread,
                cadence_agent::store::NewEntry {
                    role,
                    kind,
                    text,
                    payload: None,
                    message_id: None,
                },
            )
            .unwrap()
            .expect("thread exists")
    }

    /// An artifact card written by the store's own put path.
    fn artifact_card(&self, store: &Store, thread: &str, by: &str) -> String {
        use cadence_agent::store::{ArtifactStore, LocalArtifacts, PutArtifact};
        LocalArtifacts {
            store,
            state_dir: &self.state(),
        }
        .put(
            PutArtifact {
                id: None,
                kind: "md",
                title: "artifact-card-needle",
                thread_id: thread,
                scope: "home",
                context_id: "",
                created_by: by,
                message_id: "m-c1767-art",
            },
            b"# operator only",
        )
        .unwrap()
        .id
    }

    fn proposal(&self, store: &Store, thread: &str) {
        store
            .hire_proposal_put(
                thread,
                "m-c1767-hp",
                "Proposed hire: Accountant as proposal-needle",
                json!({"role": "accountant", "title": "Accountant", "name": "proposal-needle",
                       "job": "Chase invoices.", "digest": "c1767b"}),
            )
            .unwrap();
    }

    /// A bounded read of an SSE route: the `data:` JSON events that arrive
    /// within `window`.
    fn sse(&self, path: &str, cookie: &str, window: Duration) -> (u16, Vec<Value>) {
        let token = Seam::token_at(&self.state()).unwrap();
        let request = format!(
            "GET {path} HTTP/1.0\r\nHost: {host}\r\nX-Cadence-Board: 1\r\n\
             Origin: http://{host}\r\nSec-Fetch-Site: same-origin\r\n\
             {AS_HEADER}: operator\r\n{TOKEN_HEADER}: {token}\r\nCookie: {cookie}\r\n\r\n",
            host = self.host,
        );
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let until = Instant::now() + window;
        let mut raw = Vec::new();
        let mut buf = [0u8; 8192];
        while Instant::now() < until {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => raw.extend_from_slice(&buf[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => panic!("sse read: {e}"),
            }
        }
        let text = String::from_utf8_lossy(&raw).to_string();
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let events = text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
            .collect();
        (status, events)
    }
}

/// Operator-only content found in a list of entries.
fn operator_only(entries: &[Value], home: Option<&str>) -> Vec<String> {
    entries
        .iter()
        .filter(|e| {
            e["kind"] == "artifact"
                || e["kind"] == "hire_proposal"
                || home.is_some_and(|h| e["thread"] == h)
                || e.to_string().contains(HOME_CHAT)
                || e.to_string().contains("artifact-card-needle")
                || e.to_string().contains("proposal-needle")
        })
        .map(|e| format!("{} {}", e["kind"], e["text"]))
        .collect()
}

fn entries_of(page: &Value) -> Vec<Value> {
    page["entries"].as_array().cloned().unwrap_or_default()
}

// ------------------------------- G2 round 2: bypasses of the member view
//
// From the Spec/security REVISE on PR #1290: the member view must hold for
// every spelling the daemon resolves to the master (its native thread or
// session id), and the master's live turn state must not carry the
// operator's message body to a member.

const NATIVE_THREAD: &str = "c1767a00-0000-4000-8000-00000000a001";
const NATIVE_SESSION: &str = "c1767a00-0000-4000-8000-00000000a002";

/// The master row with native ids and a Home thread holding operator chat,
/// an artifact card and a hire proposal.
fn master_with_home(fx: &Fx) -> (Store, String) {
    fx.seed("master", None, None);
    let store = fx.store();
    store
        .set_identity_with_quota(
            "master",
            &cadence_agent::adapter::Identity {
                thread_id: NATIVE_THREAD.into(),
                session_id: NATIVE_SESSION.into(),
                model: None,
                effort: None,
                pid: std::process::id(),
                endpoint: None,
                generation: None,
                attach: None,
            },
            None,
        )
        .unwrap();
    let home = store.ensure_thread("master").unwrap().id;
    fx.append(&store, &home, "operator", "message", HOME_CHAT);
    fx.artifact_card(&store, &home, "master");
    fx.proposal(&store, &home);
    fx.append(&store, &home, "agent", "assistant_text", HOME_CHAT);
    (store, home)
}

const LIVE_TURN: &str = "live-turn-body-secret-needle";

impl Fx {
    /// The first `event: master` frame of a stream, as raw text.
    fn master_frame(&self, path: &str, cookie: &str) -> Option<String> {
        let token = Seam::token_at(&self.state()).unwrap();
        let request = format!(
            "GET {path} HTTP/1.0\r\nHost: {host}\r\nX-Cadence-Board: 1\r\n\
             Origin: http://{host}\r\nSec-Fetch-Site: same-origin\r\n\
             {AS_HEADER}: operator\r\n{TOKEN_HEADER}: {token}\r\nCookie: {cookie}\r\n\r\n",
            host = self.host,
        );
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        let mut raw = Vec::new();
        let mut buf = [0u8; 8192];
        while Instant::now() < until {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => raw.extend_from_slice(&buf[..n]),
                Err(_) => {}
            }
        }
        let text = String::from_utf8_lossy(&raw).to_string();
        let start = text.find("event: master")?;
        let rest = &text[start..];
        Some(rest[..rest.find("\n\n").unwrap_or(rest.len())].to_string())
    }
}

// ------------------------------------ G2 round 3: agent detail event tail
//
// PM finding on PR #1290: `GET /api/agents/{alias}` returns the agent's
// last 20 events to any signed-in viewer, and a `turn_finished` event
// carries the turn's result; for the master that is its Home reply.

const HOME_REPLY: &str = "master-home-reply-secret-needle";

// ------------------------------- G2 round 4: no message bodies in the detail

/// Every JSON path in `value` whose string contains `needle`.
fn paths_with(value: &Value, needle: &str, path: &str, out: &mut Vec<String>) {
    match value {
        Value::String(s) if s.contains(needle) => out.push(path.to_string()),
        Value::Object(map) => {
            for (k, v) in map {
                paths_with(v, needle, &format!("{path}.{k}"), out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                paths_with(v, needle, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

const BODY_QUEUED: &str = "queued-body-needle-a";
const BODY_RUNNING: &str = "running-body-needle-b";
const RESULT_DONE: &str = "finished-result-needle-c";

/// Queue `id` with `body` to `alias` as the operator (thread_send for the
/// master, agent_send otherwise) and optionally make it run or finish.
fn turn(fx: &Fx, store: &Store, alias: &str, id: &str, body: &str, to: &str, result: Option<&str>) {
    let verb = if alias == "master" {
        "thread_send"
    } else {
        "agent_send"
    };
    fx.op(verb, json!({"alias": alias, "text": body, "message": id}));
    if to == "queued" {
        return;
    }
    let token = cadence_agent::adapter::registry::PI_MANAGED_TURN_TOKENS
        .mint("0123456789abcdef0123456789abcdef");
    rusqlite::Connection::open(fx.state().join("cadence.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE messages SET state='submitting', started=1.0 WHERE id=?1",
            [id],
        )
        .unwrap();
    store.mark_running(id, &token).unwrap();
    if let Some(text) = result {
        let message = store.message(id).unwrap().unwrap();
        store
            .finish(
                &message,
                "completed",
                &json!({"status": "completed", "text": text}),
                None,
            )
            .unwrap();
    }
}

/// Needle → JSON paths in a detail payload.
fn body_paths(body: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for needle in [BODY_QUEUED, BODY_RUNNING, RESULT_DONE] {
        let mut paths = Vec::new();
        paths_with(body, needle, "$", &mut paths);
        out.extend(paths.into_iter().map(|p| format!("{needle} @ {p}")));
    }
    out
}

// ------------------------------------------------ round 5: member route sweep
//
// Operator decision (CAD-1767): a non-operator never receives operator
// message text or operator-only content, for any agent. Instead of one
// route at a time, sweep every board GET route the source names.

const SWEEP_NEEDLES: &[&str] = &[
    HOME_CHAT,                // master Home: operator message and reply
    "artifact-card-needle",   // artifact card title
    "proposal-needle",        // hire proposal
    "sweep-master-queued-n1", // queued operator message to the master
    "sweep-master-running-n2",
    "sweep-master-result-n3",
    "sweep-helper-queued-n4", // operator messages to an ordinary agent
    "sweep-helper-running-n5",
    "sweep-helper-result-n6",
    "sweep-saved-job-n7", // a catalogue agent's saved job
    "sweepkickoffn8",     // a dispatched task's running kickoff
];

/// Every `/api/...` route the board's source names, expanded with the
/// fixture's ids. Derived from the code at test time, not a hand list:
/// each string literal starting `/api/` in `src/ui/*.rs`, with `{param}`,
/// `<param>` and `*` segments filled in, and each `strip_prefix("/api/...")`
/// family extended by every fixture id.
fn swept_routes(ids: &[String], home: &str) -> (Vec<String>, Vec<String>) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui");
    let mut literals = std::collections::BTreeSet::new();
    let mut prefixes = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !name.ends_with(".rs")
            || name.contains("test")
            || name.contains("refusal")
            || name.contains("acceptance")
        {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let mut rest = text.as_str();
        while let Some(i) = rest.find("\"/api/") {
            let tail = &rest[i + 1..];
            let end = tail.find('"').unwrap_or(0);
            let lit = &tail[..end];
            if !lit.contains(char::is_whitespace) && !lit.is_empty() {
                let before = &rest[..i];
                if before.ends_with("strip_prefix(") {
                    prefixes.insert(lit.to_string());
                }
                literals.insert(lit.to_string());
            }
            rest = &tail[end.max(1)..];
        }
    }
    let excluded = |r: &str| r.contains("logout") || r.contains("/update") || r.contains("/device");
    let mut routes = std::collections::BTreeSet::new();
    let mut skipped = Vec::new();
    let fill = |lit: &str, id: &str| -> String {
        lit.split('/')
            .map(|seg| {
                if seg == "*"
                    || (seg.starts_with('{') && seg.ends_with('}'))
                    || (seg.starts_with('<') && seg.ends_with('>'))
                {
                    id.to_string()
                } else {
                    seg.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("/")
    };
    for lit in &literals {
        let lit = lit.trim_end_matches('?');
        if excluded(lit) {
            skipped.push(format!("{lit} (excluded: side effect or network)"));
            continue;
        }
        if lit.contains('*') || lit.contains('{') || lit.contains('<') {
            for id in ids {
                routes.insert(fill(lit, id));
            }
        } else {
            routes.insert(lit.to_string());
        }
    }
    for p in &prefixes {
        let base = p.trim_end_matches('/');
        for id in ids {
            routes.insert(format!("{base}/{id}"));
        }
    }
    for id in ids {
        routes.insert(format!("/api/threads/{id}?tail=1"));
        routes.insert(format!("/api/threads/{id}?conversation={home}"));
        routes.insert(format!("/api/threads/{id}/stream"));
    }
    routes.insert("/api/master/summary?since=0".into());
    (routes.into_iter().collect(), skipped)
}

impl Fx {
    /// A bounded GET for the sweep: `(status, raw body)`. Streams are read
    /// for a short window; a timeout is status 0.
    fn sweep_get(&self, path: &str, cookie: &str) -> (u16, String) {
        let token = Seam::token_at(&self.state()).unwrap();
        let request = format!(
            "GET {path} HTTP/1.0\r\nHost: {host}\r\nX-Cadence-Board: 1\r\n\
             Origin: http://{host}\r\nSec-Fetch-Site: same-origin\r\n\
             {AS_HEADER}: operator\r\n{TOKEN_HEADER}: {token}\r\nCookie: {cookie}\r\n\r\n",
            host = self.host,
        );
        let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", self.port)) else {
            return (0, String::new());
        };
        stream
            .set_read_timeout(Some(Duration::from_millis(250)))
            .unwrap();
        if stream.write_all(request.as_bytes()).is_err() {
            return (0, String::new());
        }
        let window = if path.contains("stream") {
            Duration::from_millis(1500)
        } else {
            Duration::from_secs(8)
        };
        let until = Instant::now() + window;
        let mut raw = Vec::new();
        let mut buf = [0u8; 16384];
        while Instant::now() < until {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => raw.extend_from_slice(&buf[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => break,
            }
        }
        let text = String::from_utf8_lossy(&raw).to_string();
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let body = text
            .split_once("\r\n\r\n")
            .map(|(_, b)| b.to_string())
            .unwrap_or_default();
        (status, body)
    }
}

/// Where each needle sits in a body: JSON paths when it parses, else the
/// raw text (SSE frames are scanned per `data:` line as JSON too).
fn needle_hits(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let docs: Vec<Value> = match serde_json::from_str::<Value>(body) {
        Ok(v) => vec![v],
        Err(_) => body
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .filter_map(|d| serde_json::from_str(d.trim()).ok())
            .collect(),
    };
    for needle in SWEEP_NEEDLES {
        let mut paths = Vec::new();
        for (i, doc) in docs.iter().enumerate() {
            paths_with(doc, needle, &format!("$[{i}]"), &mut paths);
        }
        if paths.is_empty() && body.contains(needle) {
            paths.push("raw".into());
        }
        out.extend(paths.into_iter().map(|p| format!("{needle} @ {p}")));
    }
    out
}

impl Fx {
    /// Read a stream for a bounded window while `act` changes state.
    fn stream_while(&self, path: &str, cookie: &str, act: impl FnOnce()) -> String {
        let token = Seam::token_at(&self.state()).unwrap();
        let request = format!(
            "GET {path} HTTP/1.0\r\nHost: {host}\r\nX-Cadence-Board: 1\r\n\
             Origin: http://{host}\r\nSec-Fetch-Site: same-origin\r\n\
             {AS_HEADER}: operator\r\n{TOKEN_HEADER}: {token}\r\nCookie: {cookie}\r\n\r\n",
            host = self.host,
        );
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let reader = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(8);
            let mut raw = Vec::new();
            let mut buf = [0u8; 16384];
            while Instant::now() < until {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => raw.extend_from_slice(&buf[..n]),
                    Err(_) => {}
                }
            }
            String::from_utf8_lossy(&raw).to_string()
        });
        std::thread::sleep(Duration::from_millis(1500));
        act();
        reader.join().unwrap()
    }
}

// ------------------------------------------------------------ 1. Recruit

const RECRUIT_NEEDLES: &[&str] = &[
    "recruit-operator-needle",
    "recruit-reply-needle",
    "recruit-proposal-needle",
    "recruit-running-needle",
    "recruit-queued-needle",
];

/// Where each recruit needle sits in a body (JSON paths, else raw).
fn recruit_hits(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let docs: Vec<Value> = match serde_json::from_str::<Value>(body) {
        Ok(v) => vec![v],
        Err(_) => body
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .filter_map(|d| serde_json::from_str(d.trim()).ok())
            .collect(),
    };
    for needle in RECRUIT_NEEDLES {
        let mut paths = Vec::new();
        for (i, doc) in docs.iter().enumerate() {
            paths_with(doc, needle, &format!("$[{i}]"), &mut paths);
        }
        if paths.is_empty() && body.contains(needle) {
            paths.push("raw".into());
        }
        out.extend(paths.into_iter().map(|p| format!("{needle} @ {p}")));
    }
    out
}

/// The master's Recruit conversation, created through the master
/// conversation API. The ticket fixes the API (`conversation_create` on
/// the master) but not the request shape for a non-app conversation, so
/// the shapes the implementer may choose are tried in order; the first
/// that answers a conversation id is used. Today `conversation_create`
/// requires `install_id`, so none is accepted.
fn recruit_conversation(fx: &Fx) -> Result<String, String> {
    let mut tried = Vec::new();
    for params in [
        json!({"alias": "master", "recruit": true}),
        json!({"alias": "master", "kind": "recruit"}),
        json!({"alias": "master", "subject": "Recruit"}),
    ] {
        match fx.rpc(Asserted::Operator, "conversation_create", params.clone()) {
            Ok(out) => {
                if let Some(id) = out["conversation"]["id"].as_str() {
                    eprintln!("CAD-1772 recruit conversation via {params}: {id}");
                    // Reused afterwards: the same call answers the same id.
                    let again = fx.op("conversation_create", params);
                    assert_eq!(
                        again["conversation"]["id"],
                        json!(id),
                        "the Recruit conversation is not reused"
                    );
                    return Ok(id.to_string());
                }
                tried.push(format!("{params}: no id in {out}"));
            }
            Err(e) => tried.push(format!("{params}: {e}")),
        }
    }
    Err(format!(
        "no request shape created the master's Recruit conversation: {tried:#?}"
    ))
}

#[test]
fn cad1772_recruit_conversation_is_operator_only() {
    let fx = Fx::start(Board::Public);
    let (store, home) = master_with_home(&fx);
    let recruit = recruit_conversation(&fx).unwrap_or_else(|e| panic!("{e}"));
    assert_ne!(recruit, home, "the Recruit conversation is the Home thread");
    let n = |i: usize| RECRUIT_NEEDLES[i];
    fx.append(&store, &recruit, "operator", "message", n(0));
    fx.append(&store, &recruit, "agent", "assistant_text", n(1));
    store
        .hire_proposal_put(
            &recruit,
            "m-recruit-hp",
            &format!("Proposed hire: Accountant as {}", n(2)),
            json!({"role": "accountant", "title": "Accountant", "name": n(2),
                   "job": "Chase invoices.", "digest": "c1772"}),
        )
        .unwrap();
    // A running and a queued master turn in the Recruit conversation.
    for (id, text) in [("r-run", n(3)), ("r-queued", n(4))] {
        fx.op(
            "thread_send",
            json!({"alias": "master", "text": text, "message": id,
                   "conversation": recruit}),
        );
    }
    let token = cadence_agent::adapter::registry::PI_MANAGED_TURN_TOKENS
        .mint("0123456789abcdef0123456789abcdef");
    rusqlite::Connection::open(fx.state().join("cadence.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE messages SET state='submitting', started=1.0 WHERE id='r-run'",
            [],
        )
        .unwrap();
    store.mark_running("r-run", &token).unwrap();

    let owner = fx.platform_cookie("owner");
    let member = fx.platform_cookie("member");
    let mut targeted = Vec::new();
    for name in ["master", NATIVE_THREAD, NATIVE_SESSION] {
        targeted.extend([
            format!("/api/threads/{name}?conversation={recruit}"),
            format!("/api/threads/{name}?conversation={recruit}&tail=1"),
            format!("/api/threads/{name}?conversation={recruit}&before=999"),
            format!("/api/threads/{name}/stream?conversation={recruit}"),
            format!("/api/agents/{name}"),
        ]);
    }
    targeted.push("/api/master/state".into());
    targeted.push("/api/threads/master/stream".into());
    targeted.push("/api/agents".into());

    // Control: the owner sees the Recruit conversation and its live turn.
    let mut owner_seen = std::collections::BTreeSet::new();
    for route in &targeted {
        let (status, body) = fx.sweep_get(route, &owner);
        if status == 200 {
            for hit in recruit_hits(&body) {
                owner_seen.insert(hit.split(" @ ").next().unwrap().to_string());
            }
        }
    }
    let mut wrong: Vec<String> = [n(0), n(1), n(2), n(3)]
        .iter()
        .filter(|needle| !owner_seen.contains(**needle))
        .map(|needle| format!("control: the owner never saw {needle}"))
        .collect();

    // The member: the targeted routes, then the whole derived route sweep
    // with the Recruit conversation id among the fixture ids.
    let ids: Vec<String> = [
        "master",
        NATIVE_THREAD,
        NATIVE_SESSION,
        &home,
        &recruit,
        "1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let (swept, skipped) = swept_routes(&ids, &recruit);
    let mut admitted = 0;
    for route in targeted.iter().chain(swept.iter()) {
        let (status, body) = fx.sweep_get(route, &member);
        if status != 200 {
            continue;
        }
        admitted += 1;
        let hits = recruit_hits(&body);
        if !hits.is_empty() {
            wrong.push(format!("{route}: {hits:?}"));
        }
    }
    eprintln!(
        "CAD-1772 recruit sweep: {} targeted + {} derived routes, {admitted} answered 200 \
         to the member; skipped {skipped:?}",
        targeted.len(),
        swept.len()
    );
    assert!(
        wrong.is_empty(),
        "Recruit conversation content reached a member: {wrong:#?}"
    );
}

// ------------------------------------------------------- 2. agent roles

/// The live master turn in an app conversation (social-content's General).
fn app_turn(fx: &Fx, store: &Store, id: &str) {
    let source = format!(
        "{}/tests/fixtures/apps/social-content",
        env!("CARGO_MANIFEST_DIR")
    );
    let install = fx.op("app_workspace_install", json!({"source": source}))["install_id"]
        .as_str()
        .unwrap()
        .to_string();
    let conversation = fx.op(
        "conversation_create",
        json!({"alias": "master", "install_id": install, "general": true}),
    )["conversation"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    fx.op(
        "thread_send",
        json!({"alias": "master", "text": "app hi", "message": id,
               "app": {"install_id": install}, "conversation": conversation}),
    );
    mark_running(fx, store, id);
}

fn mark_running(fx: &Fx, store: &Store, id: &str) {
    // The fixture master is a `claude`/`managed` row (Fx::seed), so its
    // turn tokens are minted under that endpoint's scheme.
    let token = cadence_agent::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS
        .mint("0123456789abcdef0123456789abcdef");
    rusqlite::Connection::open(fx.state().join("cadence.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE messages SET state='submitting', started=1.0 WHERE id=?1",
            [id],
        )
        .unwrap();
    store.mark_running(id, &token).unwrap();
}

fn finish(fx: &Fx, id: &str) {
    rusqlite::Connection::open(fx.state().join("cadence.sqlite3"))
        .unwrap()
        .execute("UPDATE messages SET state='completed' WHERE id=?1", [id])
        .unwrap();
}

/// `cadence agent roles` as a spawned CLI against the fixture daemon.
fn cli_roles(fx: &Fx, who: &str) -> (bool, String) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.env("HOME", fx.root.path().join("h"))
        .env("XDG_STATE_HOME", fx.root.path().join("xdg"))
        .env("CADENCE_TEST_AS", who)
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_STATE_DIR")
        .env_remove("CADENCE_PROFILE")
        .env("CADENCE_PM_DIR", fx.pm())
        .args([
            "--state-dir",
            fx.state().to_str().unwrap(),
            "agent",
            "roles",
        ]);
    for name in cadence_agent::adapter::PROVIDER_COMMAND_VARS {
        cmd.env(name, cadence_agent::adapter::REFUSED_COMMAND);
    }
    let out = cadence_agent::reaper::output(&mut cmd).unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn cad1772_master_can_list_roles_and_app_turns_cannot() {
    use cadence_agent::master::{allowed_tools, Profile};
    let roles: Vec<&str> = cadence_agent::agent_roles::ROLES
        .iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(roles.len(), 12, "the catalogue is 12 roles: {roles:?}");
    let mut wrong = Vec::new();

    // The master allowlist (which the Pi master guard's argv rules are
    // generated from) admits `cadence agent roles` on Home only.
    let is_roles =
        |t: &&str| *t == "Bash(cadence agent roles)" || *t == "Bash(cadence agent roles *)";
    if !allowed_tools(Profile::Home).iter().any(is_roles) {
        wrong.push("the Home allowlist has no `Bash(cadence agent roles…)` rule".to_string());
    }
    if allowed_tools(Profile::App).iter().any(is_roles) {
        wrong.push("an app session's allowlist admits `cadence agent roles`".to_string());
    }

    let fx = Fx::start(Board::None);
    let (store, _home) = master_with_home(&fx);
    // The CLI command exists and lists the 12 roles.
    let (ok, text) = cli_roles(&fx, "operator");
    if !ok || roles.iter().any(|id| !text.contains(id)) {
        wrong.push(format!("`cadence agent roles` (operator): ok={ok}: {text}"));
    }
    // The master on its live Home turn may read the roles.
    fx.op(
        "thread_send",
        json!({"alias": "master", "text": "who can I hire", "message": "roles-home"}),
    );
    mark_running(&fx, &store, "roles-home");
    // The command is a static catalogue read; the daemon `agent_roles` read
    // is not required to admit the master (recorded for the report only).
    eprintln!(
        "CAD-1772 master Home turn agent_roles RPC: {:?}",
        fx.rpc(Asserted::Agent("master".into()), "agent_roles", json!({}))
            .map(|out| out["roles"].as_array().map(Vec::len))
    );
    let (ok, text) = cli_roles(&fx, "agent:master");
    if !ok || roles.iter().any(|id| !text.contains(id)) {
        wrong.push(format!(
            "`cadence agent roles` as the master on Home: ok={ok}: {text}"
        ));
    }
    finish(&fx, "roles-home");

    // An app-scoped master turn is refused, as other non-app verbs are.
    app_turn(&fx, &store, "roles-app");
    let control = fx.rpc(Asserted::Agent("master".into()), "issue_list", json!({}));
    let refused_like_issue = control
        .as_ref()
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    eprintln!("CAD-1772 app turn issue_list (non-app verb control): {control:?}");
    match fx.rpc(Asserted::Agent("master".into()), "agent_roles", json!({})) {
        Err(e) if e.to_string().contains("app conversation") => {
            eprintln!("CAD-1772 app turn agent_roles refused: {e}");
        }
        other => wrong.push(format!(
            "app turn agent_roles was not refused like other non-app verbs \
             ({refused_like_issue}): {other:?}"
        )),
    }
    assert!(wrong.is_empty(), "agent roles: {wrong:#?}");
}

// -------------------------------------- round 2: the reserved install id

const APP_CHAT: &str = "real-recruit-app-chat-needle";

/// Spec REVISE on PR #1295: the Recruit conversation is recognised by the
/// pseudo-install id `recruit`, which is also a valid real install id. A
/// real app install named `recruit` must stay an app: either the id is
/// refused when the install/context/conversation is made, or its
/// conversation runs with the APP profile and members get the ordinary
/// app-conversation view. The real Recruit conversation still behaves as
/// Recruit (control).
#[test]
fn cad1772_real_install_named_recruit_is_not_the_recruit_conversation() {
    use cadence_agent::store::app_contexts::ContextConfig;
    let fx = Fx::start(Board::Public);
    let (store, _home) = master_with_home(&fx);
    // A live endpoint generation, so the minted turn tokens are current
    // (hire_propose checks the token).
    store
        .set_identity_with_quota(
            "master",
            &cadence_agent::adapter::Identity {
                thread_id: NATIVE_THREAD.into(),
                session_id: NATIVE_SESSION.into(),
                model: None,
                effort: None,
                pid: std::process::id(),
                endpoint: None,
                generation: Some("0123456789abcdef0123456789abcdef".into()),
                attach: None,
            },
            None,
        )
        .unwrap();
    let owner = fx.platform_cookie("owner");
    let member = fx.platform_cookie("member");
    let mut wrong = Vec::new();
    let reserved = |what: &str, e: &cadence_agent::Error| {
        eprintln!("CAD-1772 a real install named recruit refused at {what}: {e}");
        matches!(e, cadence_agent::Error::Rejected(_)) && e.to_string().contains("recruit")
    };

    // An app installation named `recruit`, landing in the store the way an
    // app's first context does (the store's own context path).
    let made = (|| -> Result<Option<(String, String)>, String> {
        let ctx = match store.app_context_create(
            "recruit",
            &ContextConfig::new("Recruit Ltd", Default::default()).unwrap(),
            "req-c1772-real-recruit",
        ) {
            Ok(out) => out["context"]["id"].as_str().unwrap().to_string(),
            Err(e) if reserved("context create", &e) => return Ok(None),
            Err(e) => return Err(format!("context create failed otherwise: {e}")),
        };
        match fx.rpc(
            Asserted::Operator,
            "conversation_create",
            json!({"alias": "master", "install_id": "recruit", "context_id": ctx}),
        ) {
            Ok(out) => Ok(Some((
                ctx,
                out["conversation"]["id"].as_str().unwrap().to_string(),
            ))),
            Err(e) if reserved("conversation create", &e) => Ok(None),
            Err(e) => Err(format!("conversation create failed otherwise: {e}")),
        }
    })();
    match made {
        Err(e) => wrong.push(e),
        Ok(None) => {} // the id is reserved: nothing can be confused with Recruit
        Ok(Some((ctx, conv))) => {
            fx.append(&store, &conv, "agent", "assistant_text", APP_CHAT);
            fx.op(
                "thread_send",
                json!({"alias": "master", "text": "app turn", "message": "real-recruit-turn",
                       "app": {"install_id": "recruit", "context_id": ctx},
                       "conversation": conv}),
            );
            mark_running(&fx, &store, "real-recruit-turn");
            // The APP profile: non-app verbs, hire_propose and agent roles
            // are refused like on any app turn.
            for (verb, params) in [
                ("issue_list", json!({})),
                ("agent_roles", json!({})),
                ("hire_propose", json!({"role": "accountant"})),
            ] {
                match fx.rpc(Asserted::Agent("master".into()), verb, params) {
                    Err(e) if e.to_string().contains("app conversation") => {}
                    other => wrong.push(format!(
                        "{verb} on a real `recruit` app turn ran as Home: {other:?}"
                    )),
                }
            }
            finish(&fx, "real-recruit-turn");
            // A member gets the ordinary app-conversation view.
            let route = format!("/api/threads/master?conversation={conv}");
            for (who, cookie) in [("owner", &owner), ("member", &member)] {
                let reply = fx.http(&Asserted::Operator, "GET", &route, None, Some(cookie), None);
                if reply.status != 200 || !reply.raw.contains(APP_CHAT) {
                    wrong.push(format!(
                        "{who}: the real `recruit` app conversation was stripped like \
                         Recruit/Home ({}): {}",
                        reply.status, reply.raw
                    ));
                }
            }
        }
    }

    // Control: the real Recruit conversation still runs as Recruit.
    let recruit = fx.op(
        "conversation_create",
        json!({"alias": "master", "kind": "recruit"}),
    )["conversation"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    fx.append(
        &store,
        &recruit,
        "agent",
        "assistant_text",
        RECRUIT_NEEDLES[1],
    );
    fx.op(
        "thread_send",
        json!({"alias": "master", "text": "recruit turn", "message": "recruit-turn",
               "conversation": recruit}),
    );
    mark_running(&fx, &store, "recruit-turn");
    match fx.rpc(
        Asserted::Agent("master".into()),
        "hire_propose",
        json!({"role": "accountant"}),
    ) {
        Ok(out) if out["state"] == "proposed" => {}
        other => wrong.push(format!(
            "control: hire_propose on the Recruit turn: {other:?}"
        )),
    }
    let route = format!("/api/threads/master?conversation={recruit}");
    let m = fx.http(
        &Asserted::Operator,
        "GET",
        &route,
        None,
        Some(&member),
        None,
    );
    if m.raw.contains(RECRUIT_NEEDLES[1]) {
        wrong.push(format!(
            "control: a member read the Recruit conversation: {}",
            m.raw
        ));
    }
    assert!(wrong.is_empty(), "a real install named recruit: {wrong:#?}");
}

/// Only the operator makes the Recruit conversation: a member, an agent
/// (the master itself included) and an unproven caller are refused on the
/// daemon RPC and on `POST /api/threads/master/conversations`.
#[test]
fn cad1772_recruit_create_refuses_non_operators() {
    let mut wrong = Vec::new();
    let body = json!({"kind": "recruit"});
    let route = "/api/threads/master/conversations";

    // Daemon RPC and loopback board: agents and an unproven caller.
    let fx = Fx::start(Board::Loopback);
    fx.seed("master", None, None);
    fx.seed("helper", None, None);
    let params = json!({"alias": "master", "kind": "recruit"});
    for who in [
        Asserted::Agent("master".into()),
        Asserted::Agent("helper".into()),
        Asserted::Unproven,
    ] {
        match fx.rpc(who.clone(), "conversation_create", params.clone()) {
            Err(e) => eprintln!("CAD-1772 RPC create as {}: {e}", who.as_str()),
            Ok(out) => wrong.push(format!("RPC {}: admitted {out}", who.as_str())),
        }
    }
    let (cookie, key) = fx.operator_session();
    for alias in ["master", "helper"] {
        let reply = fx.http(
            &Asserted::Agent(alias.into()),
            "POST",
            route,
            Some(&body),
            None,
            None,
        );
        if reply.status != 403 {
            wrong.push(format!(
                "board as agent {alias}: {} {}",
                reply.status, reply.raw
            ));
        }
    }
    let reply = fx.http(
        &Asserted::Unproven,
        "POST",
        route,
        Some(&body),
        Some(&cookie),
        Some(&key),
    );
    if reply.status != 403 {
        wrong.push(format!("board unproven: {} {}", reply.status, reply.raw));
    }
    // Control: the operator, on both.
    let reply = fx.http(
        &Asserted::Operator,
        "POST",
        route,
        Some(&body),
        Some(&cookie),
        Some(&key),
    );
    if reply.status != 200 || reply.body["conversation"]["id"].as_str().is_none() {
        wrong.push(format!(
            "control: operator board create: {} {}",
            reply.status, reply.raw
        ));
    }
    if fx
        .rpc(Asserted::Operator, "conversation_create", params)
        .is_err()
    {
        wrong.push("control: operator RPC create refused".into());
    }
    drop(fx);

    // Public board: a member is refused; the owner is the control.
    let fx = Fx::start(Board::Public);
    fx.seed("master", None, None);
    let member = fx.platform_cookie("member");
    let reply = fx.http(
        &Asserted::Operator,
        "POST",
        route,
        Some(&body),
        Some(&member),
        None,
    );
    if reply.status != 403 {
        wrong.push(format!("board member: {} {}", reply.status, reply.raw));
    }
    let owner = fx.platform_cookie("owner");
    let reply = fx.http(
        &Asserted::Operator,
        "POST",
        route,
        Some(&body),
        Some(&owner),
        None,
    );
    if reply.status != 200 {
        wrong.push(format!(
            "control: owner board create: {} {}",
            reply.status, reply.raw
        ));
    }
    assert!(
        wrong.is_empty(),
        "Recruit create by a non-operator: {wrong:#?}"
    );
}
