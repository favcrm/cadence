//! CAD-1767 independent guard checks (G1-G3), written by the check author
//! before the fix, from the brief `claudedocs/dogfood/brief-CAD-1767.md`.
//! The implementer must not edit or weaken this file. Harness patterns are
//! copied from `tests/cad1752_recruit_refusal_acceptance.rs`.
//!
//! - G1: a `session.current` pointer that leaves `<state>/agents/<alias>/`
//!   through `..` or a symlink is never used; the launch falls back to
//!   `session.jsonl` and the reason is stated. A valid pointer is used.
//! - G2: a member reader never receives a `hire_proposal` entry; the
//!   operator does. (The member-assist-history half is in-crate:
//!   `src/daemon/cad1767_member_history_acceptance.rs`.)
//! - G3: `agent_register` with `look` and no `catalogue` is refused with a
//!   stated reason and writes nothing; `look` with `catalogue` works.
//!
//! Run: `cargo test --features test-seam --test suite cad1767_ -- --test-threads 1`
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

/// G1. Guard: the `session.current` containment check in
/// `pi_current_session` / `record_current_session`
/// (`src/adapter/pi_launch.rs`).
#[test]
fn cad1767_session_pointer_outside_the_agent_dir_is_not_used() {
    let fx = LaneFx::start();
    let cwd = fx.path("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let alias = fx.hire(json!({"alias": "sess-1", "provider": "pi",
                               "cwd": cwd.to_string_lossy(),
                               "params": r#"{"confine":false}"#}));
    // The daemon's own path spelling of the worker dir.
    let dir = fx.state().join("agents").join(&alias);
    let pointer = dir.join("session.current");
    let fallback = dir.join("session.jsonl");
    assert_eq!(
        fx.last_session(&cwd).as_deref(),
        Some(fallback.to_string_lossy().as_ref()),
        "first launch"
    );

    // Outside targets: a sibling agent's dir (reached with `..`) and a
    // file outside the state dir (reached through a symlink inside).
    let sibling = fx.state().join("agents/other");
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::write(sibling.join("evil.jsonl"), session_header(&cwd)).unwrap();
    std::fs::write(fx.path("outside/evil2.jsonl"), session_header(&cwd)).unwrap();
    std::os::unix::fs::symlink(fx.path("outside/evil2.jsonl"), dir.join("link.jsonl")).unwrap();
    let dotdot = dir.join("..").join("other").join("evil.jsonl");
    let linked = dir.join("link.jsonl");

    let mut wrong = Vec::new();
    for (what, target) in [("dot-dot", &dotdot), ("symlink", &linked)] {
        std::fs::write(&pointer, target.to_string_lossy().as_bytes()).unwrap();
        fx.restart(&alias);
        let used = fx.last_session(&cwd);
        eprintln!("CAD-1767 {what} pointer: launch used {used:?}");
        if used.as_deref() != Some(fallback.to_string_lossy().as_ref()) {
            wrong.push(format!(
                "{what}: launch used {used:?}, not {}",
                fallback.display()
            ));
        }
    }
    let reasons = fx.pointer_reasons(&alias);
    eprintln!("CAD-1767 pointer reasons: {reasons:?}");
    if reasons.is_empty() {
        wrong.push("no event or log line states why the pointer was not used".into());
    }
    assert!(wrong.is_empty(), "outside session pointers: {wrong:#?}");

    // Control: a real file inside the worker dir is used.
    let sessions = dir.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let inside = sessions.join("1_inside.jsonl");
    std::fs::write(&inside, session_header(&cwd)).unwrap();
    std::fs::write(&pointer, inside.to_string_lossy().as_bytes()).unwrap();
    fx.restart(&alias);
    assert_eq!(
        fx.last_session(&cwd).as_deref(),
        Some(inside.to_string_lossy().as_ref()),
        "a valid pointer inside the worker dir was not used"
    );
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

/// G2 (board half). Guard: every member-readable read of the master's
/// Home thread hides `hire_proposal` entries; the operator sees them.
#[test]
fn cad1767_members_never_receive_hire_proposals() {
    let fx = Fx::start(Board::Public);
    fx.seed("master", None, None);
    let store = fx.store();
    let home = store.ensure_thread("master").unwrap().id;
    let proposal = json!({"role": "accountant", "title": "Accountant",
                          "name": "ledger-needle", "job": "Chase invoices.",
                          "digest": "c1767"});
    let seq = store
        .hire_proposal_put(
            &home,
            "m-c1767",
            "Proposed hire: Accountant as ledger-needle",
            proposal,
        )
        .unwrap();

    let owner = fx.platform_cookie("owner");
    let member = fx.platform_cookie("member");
    let routes = [
        "/api/threads/master".to_string(),
        "/api/threads/master?tail=1".to_string(),
        format!("/api/threads/master?before={}", seq + 1),
        format!("/api/threads/master?conversation={home}"),
        format!("/api/threads/master?conversation={home}&tail=1"),
    ];
    // Control: the owner (the operator on this surface) sees the card.
    let reply = fx.http(
        &Asserted::Operator,
        "GET",
        &routes[0],
        None,
        Some(&owner),
        None,
    );
    assert_eq!(reply.status, 200, "owner thread read: {}", reply.raw);
    assert!(
        page_kinds(&reply.body).iter().any(|k| k == "hire_proposal"),
        "control: the owner does not see the proposal: {}",
        reply.raw
    );
    let mut wrong = Vec::new();
    for route in &routes {
        let reply = fx.http(&Asserted::Operator, "GET", route, None, Some(&member), None);
        eprintln!("CAD-1767 member {route}: {}", reply.status);
        if reply.status == 200
            && (page_kinds(&reply.body).iter().any(|k| k == "hire_proposal")
                || reply.raw.contains("ledger-needle"))
        {
            wrong.push(format!("{route}: {}", reply.raw));
        }
    }
    assert!(
        wrong.is_empty(),
        "a member received the hire proposal: {wrong:#?}"
    );
}

// ------------------------------------------------------------------- G3

/// G3. Guard: `rpc_register` refuses `look` without `catalogue`.
#[test]
fn cad1767_register_refuses_look_without_catalogue() {
    let fx = Fx::start(Board::None);
    let cwd = fx.root.path().join("blank");
    std::fs::create_dir_all(&cwd).unwrap();
    let agents_before = fx.store().agents().unwrap().len();
    let mut wrong = Vec::new();
    for (what, alias, params) in [
        (
            "start blank with look",
            "look-1",
            json!({"alias": "look-1", "provider": "claude", "endpoint_kind": "managed",
                   "cwd": cwd.to_string_lossy(), "look": "guard"}),
        ),
        (
            "mailbox with look",
            "look-m",
            json!({"alias": "look-m", "provider": "inbox", "endpoint_kind": "inbox",
                   "look": "guard"}),
        ),
    ] {
        match fx.rpc(Asserted::Operator, "agent_register", params) {
            Err(err)
                if matches!(err, cadence_agent::Error::Rejected(_))
                    && err.to_string().contains("look") => {}
            other => wrong.push(format!("{what}: {other:?}")),
        }
        if fx.agent(alias).is_some() {
            wrong.push(format!("{what}: row created"));
        }
        if fx.home(alias).exists() {
            wrong.push(format!("{what}: home dir created"));
        }
    }
    assert!(wrong.is_empty(), "look without catalogue: {wrong:#?}");
    assert_eq!(
        fx.store().agents().unwrap().len(),
        agents_before,
        "rows written"
    );

    // Control: look with catalogue still works.
    fx.op(
        "agent_register",
        json!({"alias": "look-2", "catalogue": "accountant", "look": "guard"}),
    );
    assert_eq!(fx.agent("look-2").unwrap().params.unwrap()["look"], "guard");
}

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

/// G2.1. A non-master agent's thread: members keep its ordinary chat but
/// never get the artifact card or the hire proposal; the owner gets all.
#[test]
fn cad1767_member_agent_thread_reads_strip_operator_only_entries() {
    let fx = Fx::start(Board::Public);
    fx.seed("helper", None, None);
    let store = fx.store();
    let thread = store.ensure_thread("helper").unwrap().id;
    fx.append(&store, &thread, "operator", "message", HELPER_CHAT);
    fx.artifact_card(&store, &thread, "helper");
    fx.proposal(&store, &thread);
    let last = fx.append(&store, &thread, "agent", "assistant_text", HELPER_CHAT);
    let routes = [
        "/api/threads/helper".to_string(),
        "/api/threads/helper?tail=1".to_string(),
        format!("/api/threads/helper?before={}", last + 1),
        format!("/api/threads/helper?conversation={thread}"),
        format!("/api/threads/helper?conversation={thread}&tail=1"),
    ];
    let owner = fx.platform_cookie("owner");
    let member = fx.platform_cookie("member");
    let mut wrong = Vec::new();
    for route in &routes {
        let op = fx.http(&Asserted::Operator, "GET", route, None, Some(&owner), None);
        let kinds: Vec<Value> = entries_of(&op.body)
            .iter()
            .map(|e| e["kind"].clone())
            .collect();
        if op.status != 200
            || !kinds.contains(&json!("artifact"))
            || !kinds.contains(&json!("hire_proposal"))
        {
            wrong.push(format!("control {route}: owner {} {kinds:?}", op.status));
        }
        let m = fx.http(&Asserted::Operator, "GET", route, None, Some(&member), None);
        let entries = entries_of(&m.body);
        let ordinary = entries.iter().filter(|e| e["text"] == HELPER_CHAT).count();
        if m.status != 200 || ordinary != 2 {
            wrong.push(format!(
                "{route}: member lost ordinary chat ({} {ordinary}/2): {}",
                m.status, m.raw
            ));
        }
        let leaked = operator_only(&entries, None);
        if !leaked.is_empty() {
            wrong.push(format!("{route}: member got {leaked:?}"));
        }
    }
    assert!(wrong.is_empty(), "member agent-thread reads: {wrong:#?}");
}

/// G2.2. The master's thread: a member gets none of the operator's Home
/// conversation, on every variant; the owner gets it.
#[test]
fn cad1767_member_master_reads_carry_no_home_entries() {
    let fx = Fx::start(Board::Public);
    fx.seed("master", None, None);
    let store = fx.store();
    let home = store.ensure_thread("master").unwrap().id;
    fx.append(&store, &home, "operator", "message", HOME_CHAT);
    let last = fx.append(&store, &home, "agent", "assistant_text", HOME_CHAT);
    let routes = [
        "/api/threads/master".to_string(),
        "/api/threads/master?tail=1".to_string(),
        format!("/api/threads/master?before={}", last + 1),
        format!("/api/threads/master?conversation={home}"),
        format!("/api/threads/master?conversation={home}&tail=1"),
    ];
    let owner = fx.platform_cookie("owner");
    let member = fx.platform_cookie("member");
    let mut wrong = Vec::new();
    for route in &routes {
        let op = fx.http(&Asserted::Operator, "GET", route, None, Some(&owner), None);
        if op.status != 200 || !op.raw.contains(HOME_CHAT) {
            wrong.push(format!(
                "control {route}: owner {} lacks Home chat",
                op.status
            ));
        }
        let m = fx.http(&Asserted::Operator, "GET", route, None, Some(&member), None);
        eprintln!("CAD-1767 member {route}: {}", m.status);
        let leaked = operator_only(&entries_of(&m.body), Some(&home));
        if !leaked.is_empty() || m.raw.contains(HOME_CHAT) {
            wrong.push(format!("{route}: member got Home entries {leaked:?}"));
        }
    }
    assert!(wrong.is_empty(), "member master reads: {wrong:#?}");
}

/// G2.3. `/stream`: a member's event stream carries no artifact card, no
/// hire proposal and no master Home entry; the owner's stream does.
#[test]
fn cad1767_member_thread_stream_strips_operator_only_entries() {
    let fx = Fx::start(Board::Public);
    fx.seed("helper", None, None);
    fx.seed("master", None, None);
    let store = fx.store();
    let thread = store.ensure_thread("helper").unwrap().id;
    fx.append(&store, &thread, "operator", "message", HELPER_CHAT);
    fx.artifact_card(&store, &thread, "helper");
    fx.proposal(&store, &thread);
    let home = store.ensure_thread("master").unwrap().id;
    fx.append(&store, &home, "operator", "message", HOME_CHAT);
    fx.artifact_card(&store, &home, "master");
    fx.proposal(&store, &home);
    let owner = fx.platform_cookie("owner");
    let member = fx.platform_cookie("member");
    let window = Duration::from_secs(3);
    let mut wrong = Vec::new();
    for (alias, path, home_id) in [
        ("helper", "/api/threads/helper/stream".to_string(), None),
        (
            "master",
            "/api/threads/master/stream".to_string(),
            Some(home.as_str()),
        ),
        (
            "master",
            format!("/api/threads/master/stream?conversation={home}"),
            Some(home.as_str()),
        ),
    ] {
        let (status, events) = fx.sse(&path, &owner, window);
        let kinds: Vec<Value> = events.iter().map(|e| e["kind"].clone()).collect();
        if status != 200
            || !kinds.contains(&json!("artifact"))
            || !kinds.contains(&json!("hire_proposal"))
        {
            wrong.push(format!("control {path}: owner {status} {kinds:?}"));
        }
        let (status, events) = fx.sse(&path, &member, window);
        eprintln!(
            "CAD-1767 member stream {path}: {status}, {} events",
            events.len()
        );
        let leaked = operator_only(&events, home_id);
        if !leaked.is_empty() {
            wrong.push(format!("{path}: member stream got {leaked:?}"));
        }
        if alias == "helper" && status == 200 && !events.iter().any(|e| e["text"] == HELPER_CHAT) {
            wrong.push(format!("{path}: member stream lost the ordinary chat"));
        }
    }
    assert!(wrong.is_empty(), "member streams: {wrong:#?}");
}
