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
//!
//! CAD-1970: stream checks finish at a processed fixture entry/batch or at
//! post-mutation agent frames, never at the end of a quiet observation window.
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

    /// Read through the final fixture entry (or the filtered Home batch),
    /// then decode every `data:` event preceding that causal boundary.
    fn sse(&self, store: &Store, path: &str, cookie: &str, operator: bool) -> (u16, Vec<Value>) {
        let text = self.thread_stream(store, path, cookie, operator);
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
    // The hard read cap is a failure safeguard, not an observation window.
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
        let (status, events) = fx.sse(&store, &path, &owner, true);
        let kinds: Vec<Value> = events.iter().map(|e| e["kind"].clone()).collect();
        if status != 200
            || !kinds.contains(&json!("artifact"))
            || !kinds.contains(&json!("hire_proposal"))
        {
            wrong.push(format!("control {path}: owner {status} {kinds:?}"));
        }
        let (status, events) = fx.sse(&store, &path, &member, false);
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

/// G2 round 2, item 1. Every name the daemon resolves to the master gets
/// the member view: no Home chat, no artifact card, no hire proposal.
#[test]
fn cad1767_member_view_holds_for_every_master_spelling() {
    let fx = Fx::start(Board::Public);
    let (store, home) = master_with_home(&fx);
    // Which spellings does the daemon resolve to the master?
    let mut spellings = Vec::new();
    for name in [NATIVE_THREAD, NATIVE_SESSION, "MASTER", "Master", "master."] {
        let read = fx.rpc(
            Asserted::Operator,
            "thread_read",
            json!({"alias": name, "after": 0, "limit": 5}),
        );
        eprintln!(
            "CAD-1767 thread_read '{name}': {}",
            match &read {
                Ok(page) => format!("ok alias={}", page["alias"]),
                Err(e) => format!("err {e}"),
            }
        );
        if read.is_ok_and(|page| page.to_string().contains(HOME_CHAT)) {
            spellings.push(name.to_string());
        }
    }
    assert!(
        spellings.iter().any(|s| s == NATIVE_THREAD),
        "fixture: the native thread id does not resolve to the master: {spellings:?}"
    );
    let owner = fx.platform_cookie("owner");
    let member = fx.platform_cookie("member");
    let mut wrong = Vec::new();
    for name in &spellings {
        for route in [
            format!("/api/threads/{name}"),
            format!("/api/threads/{name}?tail=1"),
            format!("/api/threads/{name}?before=99"),
            format!("/api/threads/{name}?conversation={home}"),
        ] {
            let op = fx.http(&Asserted::Operator, "GET", &route, None, Some(&owner), None);
            if op.status != 200 || !op.raw.contains(HOME_CHAT) {
                wrong.push(format!("control {route}: owner {}", op.status));
            }
            let m = fx.http(
                &Asserted::Operator,
                "GET",
                &route,
                None,
                Some(&member),
                None,
            );
            let leaked = operator_only(&entries_of(&m.body), Some(&home));
            if !leaked.is_empty() || m.raw.contains(HOME_CHAT) {
                wrong.push(format!("{route}: member got {leaked:?}"));
            }
        }
        let path = format!("/api/threads/{name}/stream");
        let (status, events) = fx.sse(&store, &path, &owner, true);
        if status != 200 || !events.iter().any(|e| e.to_string().contains(HOME_CHAT)) {
            wrong.push(format!("control {path}: owner {status}"));
        }
        let (_, events) = fx.sse(&store, &path, &member, false);
        let leaked = operator_only(&events, Some(&home));
        if !leaked.is_empty() {
            wrong.push(format!("{path}: member stream got {leaked:?}"));
        }
    }
    assert!(wrong.is_empty(), "member view bypassed: {wrong:#?}");
}

const LIVE_TURN: &str = "live-turn-body-secret-needle";

impl Fx {
    /// The first `event: master` frame of a stream, as raw text.
    fn master_frame(&self, path: &str, cookie: &str) -> Option<String> {
        let text = self.read_until(path, cookie, "complete event: master frame", |text| {
            text.split("\n\n")
                .any(|frame| frame.lines().any(|line| line == "event: master"))
        });
        let start = text.find("event: master")?;
        let rest = &text[start..];
        Some(rest[..rest.find("\n\n").expect("complete master frame")].to_string())
    }
}

/// G2 round 2, item 2. While the operator's Home message is queued and
/// while it runs, a member's `GET /api/master/state` and the stream's
/// first `event: master` frame never carry its body; the owner's do.
#[test]
fn cad1767_member_master_state_hides_the_operators_turn() {
    let fx = Fx::start(Board::Public);
    fx.seed("master", None, None);
    let store = fx.store();
    fx.op(
        "thread_send",
        json!({"alias": "master", "text": LIVE_TURN, "message": "live-1"}),
    );
    let owner = fx.platform_cookie("owner");
    let member = fx.platform_cookie("member");
    let mut wrong = Vec::new();
    let check = |phase: &str, wrong: &mut Vec<String>| {
        let op = fx.http(
            &Asserted::Operator,
            "GET",
            "/api/master/state",
            None,
            Some(&owner),
            None,
        );
        if op.status != 200 || !op.raw.contains(LIVE_TURN) {
            wrong.push(format!(
                "control {phase}: owner state {} {}",
                op.status, op.raw
            ));
        }
        let m = fx.http(
            &Asserted::Operator,
            "GET",
            "/api/master/state",
            None,
            Some(&member),
            None,
        );
        eprintln!("CAD-1767 member master/state ({phase}): {}", m.status);
        if m.raw.contains(LIVE_TURN) {
            wrong.push(format!(
                "{phase}: member /api/master/state carries the body: {}",
                m.raw
            ));
        }
        match fx.master_frame("/api/threads/master/stream", &owner) {
            Some(frame) if frame.contains(LIVE_TURN) => {}
            other => wrong.push(format!("control {phase}: owner master frame {other:?}")),
        }
        if let Some(frame) = fx.master_frame("/api/threads/master/stream", &member) {
            if frame.contains(LIVE_TURN) {
                wrong.push(format!(
                    "{phase}: member master frame carries the body: {frame}"
                ));
            }
        }
    };
    check("queued", &mut wrong);
    // The same message, now the running turn.
    let token = cadence_agent::adapter::registry::PI_MANAGED_TURN_TOKENS
        .mint("0123456789abcdef0123456789abcdef");
    rusqlite::Connection::open(fx.state().join("cadence.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE messages SET state='submitting', started=1.0 WHERE id='live-1'",
            [],
        )
        .unwrap();
    store.mark_running("live-1", &token).unwrap();
    check("running", &mut wrong);
    assert!(
        wrong.is_empty(),
        "the operator's turn reached a member: {wrong:#?}"
    );
}

// ------------------------------------ G2 round 3: agent detail event tail
//
// PM finding on PR #1290: `GET /api/agents/{alias}` returns the agent's
// last 20 events to any signed-in viewer, and a `turn_finished` event
// carries the turn's result; for the master that is its Home reply.

const HOME_REPLY: &str = "master-home-reply-secret-needle";

/// The member's agent detail of the master carries the Home reply text
/// nowhere (events or elsewhere), also through the master's native ids;
/// the owner's detail does.
#[test]
fn cad1767_member_agent_detail_events_carry_no_master_home_text() {
    let fx = Fx::start(Board::Public);
    let (store, _home) = master_with_home(&fx);
    // A master Home turn, finished through the store's own finish path.
    fx.op(
        "thread_send",
        json!({"alias": "master", "text": "operator asks", "message": "home-done"}),
    );
    let token = cadence_agent::adapter::registry::PI_MANAGED_TURN_TOKENS
        .mint("0123456789abcdef0123456789abcdef");
    rusqlite::Connection::open(fx.state().join("cadence.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE messages SET state='submitting', started=1.0 WHERE id='home-done'",
            [],
        )
        .unwrap();
    store.mark_running("home-done", &token).unwrap();
    let message = store.message("home-done").unwrap().expect("the Home turn");
    store
        .finish(
            &message,
            "completed",
            &json!({"status": "completed", "text": HOME_REPLY}),
            None,
        )
        .unwrap();

    // Fixture: the daemon's event log does carry the reply for the
    // master (`turn_finished.result`), so the detail's tail can leak it.
    let log = fx.op("agent_events", json!({"alias": "master", "after": 0}));
    assert!(
        log.to_string().contains(HOME_REPLY),
        "fixture: turn_finished does not carry the Home reply: {log}"
    );
    let owner = fx.platform_cookie("owner");
    let member = fx.platform_cookie("member");
    let mut wrong = Vec::new();
    for name in ["master", NATIVE_THREAD, NATIVE_SESSION] {
        let route = format!("/api/agents/{name}");
        let op = fx.http(&Asserted::Operator, "GET", &route, None, Some(&owner), None);
        let m = fx.http(
            &Asserted::Operator,
            "GET",
            &route,
            None,
            Some(&member),
            None,
        );
        eprintln!(
            "CAD-1767 detail {route}: owner {} member {}",
            op.status, m.status
        );
        if name == "master" && (op.status != 200 || !op.raw.contains(HOME_REPLY)) {
            let kinds: Vec<Value> = op.body["events"]
                .as_array()
                .map(|evs| evs.iter().map(|e| json!([e["seq"], e["kind"]])).collect())
                .unwrap_or_default();
            wrong.push(format!(
                "control {route}: owner {} lacks the reply; events {kinds:?}",
                op.status
            ));
        }
        if m.raw.contains(HOME_REPLY) {
            let kinds: Vec<Value> = m.body["events"]
                .as_array()
                .map(|evs| {
                    evs.iter()
                        .filter(|e| e.to_string().contains(HOME_REPLY))
                        .map(|e| e["kind"].clone())
                        .collect()
                })
                .unwrap_or_default();
            wrong.push(format!(
                "{route}: member detail carries the reply (events {kinds:?})"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "master Home text in a member's agent detail: {wrong:#?}"
    );
}

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

/// A member's agent detail of the master carries no message body or turn
/// result anywhere, also through the native ids; the owner's carries them.
#[test]
fn cad1767_member_agent_detail_carries_no_message_bodies() {
    let fx = Fx::start(Board::Public);
    let (store, _home) = master_with_home(&fx);
    turn(
        &fx,
        &store,
        "master",
        "m-done",
        "asked before",
        "running",
        Some(RESULT_DONE),
    );
    turn(
        &fx,
        &store,
        "master",
        "m-run",
        BODY_RUNNING,
        "running",
        None,
    );
    turn(
        &fx,
        &store,
        "master",
        "m-queued",
        BODY_QUEUED,
        "queued",
        None,
    );
    let owner = fx.platform_cookie("owner");
    let member = fx.platform_cookie("member");
    let mut wrong = Vec::new();
    for name in ["master", NATIVE_THREAD, NATIVE_SESSION] {
        let route = format!("/api/agents/{name}");
        let op = fx.http(&Asserted::Operator, "GET", &route, None, Some(&owner), None);
        let op_paths = body_paths(&op.body);
        eprintln!("CAD-1767 owner {route}: {op_paths:?}");
        if name == "master" {
            for needle in [BODY_RUNNING, RESULT_DONE] {
                if !op_paths.iter().any(|p| p.starts_with(needle)) {
                    wrong.push(format!("control {route}: owner lacks {needle}"));
                }
            }
        }
        let m = fx.http(
            &Asserted::Operator,
            "GET",
            &route,
            None,
            Some(&member),
            None,
        );
        let paths = body_paths(&m.body);
        if m.status == 200 && !paths.is_empty() {
            wrong.push(format!("{route}: member payload carries {paths:?}"));
        }
        if m.status != 200 && m.raw.contains("needle") {
            wrong.push(format!("{route}: member error carries a body: {}", m.raw));
        }
    }
    assert!(
        wrong.is_empty(),
        "message bodies in a member's agent detail: {wrong:#?}"
    );
}

/// Operator decision (CAD-1767): for EVERY agent, a non-operator's agent
/// detail carries running items as ids/state only, never message text. An
/// ordinary agent with operator-sent finished, running and queued turns:
/// a member's `GET /api/agents/helper` carries no body or result anywhere;
/// the owner's carries the running body and the finished result.
#[test]
fn cad1767_member_ordinary_agent_detail_carries_no_message_bodies() {
    let fx = Fx::start(Board::Public);
    fx.seed("helper", None, None);
    let store = fx.store();
    turn(
        &fx,
        &store,
        "helper",
        "h-done",
        "asked before",
        "running",
        Some(RESULT_DONE),
    );
    turn(
        &fx,
        &store,
        "helper",
        "h-run",
        BODY_RUNNING,
        "running",
        None,
    );
    turn(
        &fx,
        &store,
        "helper",
        "h-queued",
        BODY_QUEUED,
        "queued",
        None,
    );
    let owner = fx.platform_cookie("owner");
    let member = fx.platform_cookie("member");
    let route = "/api/agents/helper";
    let op = fx.http(&Asserted::Operator, "GET", route, None, Some(&owner), None);
    let op_paths = body_paths(&op.body);
    let mut wrong = Vec::new();
    if !op_paths
        .iter()
        .any(|p| p == &format!("{BODY_RUNNING} @ $.running[0].summary"))
    {
        wrong.push(format!(
            "control: owner lacks the running body: {op_paths:?}"
        ));
    }
    if !op_paths
        .iter()
        .any(|p| p.starts_with(RESULT_DONE) && p.contains("$.events["))
    {
        wrong.push(format!(
            "control: owner lacks the finished result: {op_paths:?}"
        ));
    }
    let m = fx.http(&Asserted::Operator, "GET", route, None, Some(&member), None);
    let paths = body_paths(&m.body);
    if m.status != 200 {
        wrong.push(format!("member detail answered {}: {}", m.status, m.raw));
    }
    if !paths.is_empty() {
        wrong.push(format!("member payload carries {paths:?}"));
    }
    if m.body["running"].as_array().is_none_or(|rows| {
        rows.iter()
            .all(|r| r["id"].is_null() && r["message"].is_null())
    }) {
        wrong.push(format!(
            "member detail lost the running item ids/state: {}",
            m.body["running"]
        ));
    }
    assert!(
        wrong.is_empty(),
        "message bodies in a member's ordinary agent detail: {wrong:#?}"
    );
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
    /// A GET ending at EOF, a processed thread batch, or post-nudge agent
    /// frames. Stream-shaped invalid routes still have to reach HTTP EOF.
    fn sweep_get(&self, store: &Store, path: &str, cookie: &str, operator: bool) -> (u16, String) {
        let route = path.split('?').next().unwrap();
        let text = if route == "/api/stream" {
            self.stream_while(path, cookie, || self.nudge_agents())
        } else if route.starts_with("/api/threads/") && route.ends_with("/stream") {
            self.thread_stream(store, path, cookie, operator)
        } else {
            self.read_until(path, cookie, "HTTP EOF", |_| false)
        };
        let text = text.as_str();
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

/// Round 5. A member receives no operator text on any board GET route.
#[test]
fn cad1767_member_route_sweep_carries_no_operator_text() {
    let fx = Fx::start(Board::Public);
    let (store, home) = master_with_home(&fx);
    fx.seed("helper", None, None);
    let helper_thread = store.ensure_thread("helper").unwrap().id;
    let art = fx.artifact_card(&store, &helper_thread, "helper");
    let n = |i: usize| SWEEP_NEEDLES[i];
    turn(
        &fx,
        &store,
        "master",
        "s-mf",
        "asked",
        "running",
        Some(n(5)),
    );
    turn(&fx, &store, "master", "s-mr", n(4), "running", None);
    turn(&fx, &store, "master", "s-mq", n(3), "queued", None);
    turn(
        &fx,
        &store,
        "helper",
        "s-hf",
        "asked",
        "running",
        Some(n(8)),
    );
    turn(&fx, &store, "helper", "s-hr", n(7), "running", None);
    turn(&fx, &store, "helper", "s-hq", n(6), "queued", None);
    fx.op(
        "agent_register",
        json!({"alias": "acct-job", "catalogue": "accountant"}),
    );
    fx.op(
        "agent_instructions_set",
        json!({"alias": "acct-job", "instructions": n(9)}),
    );
    // A DISPATCHED task per agent through the daemon's own job/task/dispatch
    // path, its kickoff made running; the kickoff's head names the spec, so
    // n8 is in the running summary the fleet row binds to the task.
    let mut dispatched = Vec::new();
    for alias in ["helper", "master"] {
        let job = format!("job-sweep-{alias}");
        fx.op(
            "job_new",
            json!({"pm": alias, "job": job, "title": "sweep job",
                   "spec": format!("specs/{}-{alias}.md", n(10)),
                   "spec_sha256": "0".repeat(64),
                   "issue": if alias == "master" { "CAD-9002" } else { "CAD-9001" }}),
        );
        let task = fx.op(
            "task_new",
            json!({"job": job, "assignee": alias, "title": "sweep task"}),
        )["task"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let out = fx.rpc(
            Asserted::Operator,
            "task_dispatch",
            json!({"by": "operator", "task": task, "to": alias}),
        );
        eprintln!("CAD-1767 sweep task_dispatch {alias}: {out:?}");
        let Ok(out) = out else { continue };
        let message = out["message"]["id"]
            .as_str()
            .or_else(|| out["message"].as_str())
            .map(str::to_string)
            .unwrap_or_else(|| panic!("task_dispatch answered no message id: {out}"));
        let token = cadence_agent::adapter::registry::PI_MANAGED_TURN_TOKENS
            .mint("0123456789abcdef0123456789abcdef");
        rusqlite::Connection::open(fx.state().join("cadence.sqlite3"))
            .unwrap()
            .execute(
                "UPDATE messages SET state='submitting', started=1.0 WHERE id=?1",
                [&message],
            )
            .unwrap();
        store.mark_running(&message, &token).unwrap();
        dispatched.push(alias);
    }
    assert!(
        dispatched.contains(&"helper"),
        "fixture: no dispatched task for the ordinary agent"
    );
    let ids: Vec<String> = [
        "master",
        "helper",
        "acct-job",
        NATIVE_THREAD,
        NATIVE_SESSION,
        home.as_str(),
        art.as_str(),
        "1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let (routes, skipped) = swept_routes(&ids, &home);
    let owner = fx.platform_cookie("owner");
    let member = fx.platform_cookie("member");

    let mut leaks = Vec::new();
    let mut admitted = Vec::new();
    let mut owner_seen = std::collections::BTreeSet::new();
    for route in &routes {
        let (status, body) = fx.sweep_get(&store, route, &member, false);
        if status == 200 {
            admitted.push(route.clone());
            // Operator decision 1: an ordinary (non-master) agent's own
            // thread stays readable by members, operator messages and the
            // agent's replies included, so its chat needles (n4-n6) are
            // allowed on that agent's thread routes. Everything else -
            // master Home, artifact, proposal, saved job, and running text
            // anywhere outside the thread (decision 2) - stays asserted.
            let ordinary_thread = ["helper", "acct-job"].iter().any(|alias| {
                let base = format!("/api/threads/{alias}");
                route == &base
                    || route.starts_with(&format!("{base}?"))
                    || route.starts_with(&format!("{base}/"))
            });
            let hits: Vec<String> = needle_hits(&body)
                .into_iter()
                .filter(|hit| {
                    !(ordinary_thread
                        && [n(6), n(7), n(8), n(10)]
                            .iter()
                            .any(|nd| hit.starts_with(nd)))
                })
                .collect();
            if !hits.is_empty() {
                leaks.push(format!("{route}: {hits:?}"));
            }
        }
        let (ostatus, obody) = fx.sweep_get(&store, route, &owner, true);
        if ostatus == 200 {
            for hit in needle_hits(&obody) {
                owner_seen.insert(hit.split(" @ ").next().unwrap().to_string());
            }
        }
    }
    eprintln!(
        "CAD-1767 sweep: {} routes tried, {} answered 200 to the member",
        routes.len(),
        admitted.len()
    );
    eprintln!("CAD-1767 sweep member-200 routes: {admitted:#?}");
    eprintln!("CAD-1767 sweep skipped: {skipped:#?}");
    eprintln!("CAD-1767 sweep owner saw: {owner_seen:?}");
    // Control: the owner sees the operator text where the operator should.
    let mut wrong: Vec<String> = [n(0), n(1), n(2), n(4), n(5), n(7), n(8), n(9), n(10)]
        .iter()
        .filter(|needle| !owner_seen.contains(**needle))
        .map(|needle| format!("control: the owner never saw {needle}"))
        .collect();
    wrong.extend(leaks);

    // The entity stream: agent frames go out only when an agent changes, so
    // read while changing both agents, and require that an `event: agent`
    // frame actually arrived (a quiet stream would prove nothing).
    for (who, cookie) in [("member", &member), ("owner", &owner)] {
        let raw = fx.stream_while("/api/stream?entities=1", cookie, || fx.nudge_agents());
        let frames: Vec<&str> = raw
            .split("\n\n")
            .filter(|f| f.starts_with("event: agent") || f.contains("\nevent: agent"))
            .collect();
        eprintln!(
            "CAD-1767 sweep {who} /api/stream?entities=1: {} agent frames",
            frames.len()
        );
        if frames.is_empty() {
            wrong.push(format!(
                "{who}: no `event: agent` frame arrived on /api/stream?entities=1"
            ));
        }
        let hits: Vec<String> = frames.iter().flat_map(|f| needle_hits(f)).collect();
        if who == "member" && !hits.is_empty() {
            wrong.push(format!("/api/stream?entities=1 agent frames: {hits:?}"));
        }
        if who == "owner" && !hits.iter().any(|h| h.starts_with(n(10))) {
            wrong.push(format!(
                "control: the owner's agent frames lack {}: {hits:?}",
                n(10)
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "operator text reached a member: {wrong:#?}"
    );
}

impl Fx {
    /// Read complete frames until `done` proves the requested boundary.
    /// EOF is the boundary for finite HTTP responses; the 8 s cap only fails
    /// a stalled response and is never a successful negative assertion.
    fn read_until(
        &self,
        path: &str,
        cookie: &str,
        boundary: &str,
        mut done: impl FnMut(&str) -> bool,
    ) -> String {
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
        let until = Instant::now() + Duration::from_secs(8);
        let mut raw = Vec::new();
        let mut buf = [0u8; 16384];
        loop {
            assert!(
                Instant::now() < until,
                "{path}: timed out waiting for {boundary}"
            );
            match stream.read(&mut buf) {
                Ok(0) => {
                    let text = String::from_utf8_lossy(&raw);
                    let head = text.split_once("\r\n\r\n").map(|(h, _)| h).unwrap_or("");
                    assert!(
                        !head.to_ascii_lowercase().contains("text/event-stream"),
                        "{path}: stream closed before {boundary}"
                    );
                    break;
                }
                Ok(n) => {
                    raw.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&raw);
                    if let Some(end) = text.rfind("\n\n") {
                        if done(&text[..end + 2]) {
                            break;
                        }
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => panic!("{path}: causal read: {e}"),
            }
        }
        String::from_utf8(raw).expect("board response is UTF-8")
    }

    /// The appended sentinel follows all seeded secrets. An owner/ordinary
    /// reader must receive it; a member's master Home is wholly filtered.
    /// threads::stream emits its first ping BEFORE reading entries and its
    /// second only AFTER scanning an empty/filtered page. Keeping the entire
    /// fixture below one page makes that second ping a completion proof even
    /// when there can be no visible entry. Neither an HTTP head nor the first
    /// ping is sufficient.
    fn thread_stream(&self, store: &Store, path: &str, cookie: &str, operator: bool) -> String {
        let (route, query) = path.split_once('?').unwrap_or((path, ""));
        let alias = route
            .strip_prefix("/api/threads/")
            .and_then(|p| p.strip_suffix("/stream"))
            .expect("thread stream route");
        let mut params = json!({"alias": alias, "after": 0, "limit": 100});
        for part in query.split('&') {
            if let Some(("conversation", id)) = part.split_once('=') {
                params["conversation"] = json!(id);
            }
        }
        let Ok(page) = self.rpc(Asserted::Operator, "thread_read", params) else {
            // The actual HTTP error must complete too; do not silently skip
            // invalid native ids, artifact ids, or prefix-expanded routes.
            return self.read_until(path, cookie, "HTTP error EOF", |_| false);
        };
        let entries = page["entries"].as_array().expect("fixture entries");
        let Some(thread) = page["thread"]["id"].as_str().map(str::to_string) else {
            // The alias resolves but has no thread yet: threads::stream's
            // `thread_read` preflight still succeeds, so it answers 200 and
            // then loops emitting `: ping` forever — there is no entry and no
            // EOF to wait for. Nothing can be disclosed for a thread that does
            // not exist, and the same native read we just made is the proof of
            // that. Completing on the first ping is therefore causal: it shows
            // the stream opened and entered its loop, and the page above shows
            // the loop has nothing to serve.
            assert!(
                entries.is_empty(),
                "{path}: a page with entries must name its thread"
            );
            let mut opened = false;
            let raw = self.read_until(path, cookie, "first ping (no thread yet)", |text| {
                let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
                opened = body.split("\n\n").any(|frame| frame == ": ping");
                opened
            });
            if raw.starts_with("HTTP/1.1 200") {
                assert!(opened, "{path}: stream closed before its first ping");
            }
            return raw;
        };
        // Contract with threads::stream: its page size is 100, and its
        // second ping follows scanning + filtering that entire first page.
        // Include our sentinel in the strict <100-row fixture invariant.
        assert!(
            entries.len() + 1 < 100,
            "{path}: fixture must fit one SSE page"
        );
        let seq = self.append(
            store,
            &thread,
            "agent",
            "assistant_text",
            "causal-stream-boundary",
        );
        let sentinel = format!("id: {seq}");
        let hidden_home = !operator && page["alias"] == "master";
        let mut finished = false;
        let boundary = if hidden_home {
            "second ping after the filtered Home batch"
        } else {
            &sentinel
        };
        let raw = self.read_until(path, cookie, boundary, |text| {
            let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
            finished = if hidden_home {
                body.split("\n\n")
                    .filter(|frame| *frame == ": ping")
                    .count()
                    >= 2
            } else {
                body.split("\n\n").any(|frame| {
                    frame.lines().any(|line| line == sentinel)
                        && frame.lines().any(|line| line == "event: entry")
                })
            };
            finished
        });
        if raw.starts_with("HTTP/1.1 200") {
            assert!(finished, "{path}: stream closed before processing fixture");
        }
        raw
    }

    fn queued(&self, alias: &str) -> i64 {
        self.op("agent_show", json!({"alias": alias, "limit": 0}))["queued"]
            .as_i64()
            .expect("fixture queued count")
    }

    fn nudge_agents(&self) {
        let generation = self.messages("sweep-nudge-").len();
        for alias in ["helper", "master"] {
            self.op(
                "agent_send",
                json!({"alias": alias, "text": "causal stream nudge",
                       "message": format!("sweep-nudge-{generation}-{alias}")}),
            );
        }
    }

    /// `hello` is sent only after subscription/baseline setup. Act after
    /// that handshake, then require BOTH changed rows with their exact new
    /// queued counts: an unrelated or pre-action agent frame cannot finish
    /// this read. The running operator-only kickoff remains in those rows.
    fn stream_while(&self, path: &str, cookie: &str, act: impl FnOnce()) -> String {
        let aliases = ["helper", "master"];
        let before = aliases.map(|alias| self.queued(alias));
        let mut expected = None;
        let mut act = Some(act);
        let mut finished = false;
        let raw = self.read_until(path, cookie, "both post-nudge event: agent rows", |text| {
            let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
            if act.is_some() && body.contains("event: hello\n") {
                act.take().unwrap()();
                let after = aliases.map(|alias| self.queued(alias));
                for i in 0..aliases.len() {
                    assert_eq!(after[i], before[i] + 1, "nudge must queue a new turn");
                }
                expected = Some(after);
            }
            if let Some(expected) = expected {
                finished = aliases.iter().enumerate().all(|(i, alias)| {
                    body.split("\n\n").any(|frame| {
                        frame.lines().any(|line| line == "event: agent")
                            && frame
                                .lines()
                                .filter_map(|line| line.strip_prefix("data:"))
                                .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
                                .any(|data| {
                                    data["agent"]["alias"] == *alias
                                        && data["agent"]["queued"] == expected[i]
                                })
                    })
                });
            }
            finished
        });
        assert!(
            finished,
            "{path}: closed before both post-nudge agent frames"
        );
        raw
    }
}
