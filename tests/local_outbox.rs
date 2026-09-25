//! CAD-546 / ADR 0006: the built-in `local` platform, proven through
//! the real effect gate — a `publish` stages as a `send`, sits in
//! Needs-you, and writes the local outbox only on the operator's
//! press. Nothing here special-cases `local`: staging, the
//! operator-only press, the decline path and the outcome delivery are
//! the gate's own machinery, asserted over live RPC.
//!
//! The adversarial cases mutation-proof the two rules this ticket
//! adds itself: attachment confinement (`..`, absolute paths outside
//! the worktree, symlink components — drop the check and the
//! `!exists` assertions fail) and the operator-only read side
//! (`platform_outbox` and `/api/outbox` refuse an agent or unproven
//! caller — loosen the rule and those assertions fail).
//!
//! A test binary never runs the CAD-308 reaper (only `daemon run`
//! does), so its own spawns need not go through
//! `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use cadence_agent::{client, daemon, platform::local, proto};
use serde_json::{json, Value};
use tempfile::TempDir;

/// The placeholder credential `local` enrolls — custody binds grants
/// to a record, so the operator enrolls one token; the adapter never
/// reads the bytes.
const TOKEN: &str = "local-outbox-placeholder";

/// The board origin the test adapter reports in outcomes.
const BOARD: &str = "http://127.0.0.1:3919";

// ---------- harness ----------

/// In-process daemon with the `local` adapter registered against a
/// temp outbox — the same seam `daemon::serve` uses, pinned to the
/// test's dirs.
struct Daemon {
    state: PathBuf,
    pm: PathBuf,
    outbox: PathBuf,
    _dir: TempDir,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Daemon {
    fn start() -> Self {
        let dir = TempDir::new().unwrap();
        let state = dir.path().join("state");
        let pm = dir.path().join("pm");
        let outbox = dir.path().join("outbox");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&pm).unwrap();
        let mut d = Self {
            state,
            pm,
            outbox,
            _dir: dir,
            stop: Arc::new(AtomicBool::new(false)),
            handle: None,
        };
        d.serve();
        d
    }

    fn serve(&mut self) {
        let env = cadence_agent::adapter::ProviderEnv::default();
        env.set("CADENCE_PM_DIR", self.pm.to_str().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = stop.clone();
        let mut opts = daemon::ServeOptions {
            provider_env: env,
            report_router: Some(0),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            slots: Some(cadence_agent::slots::SlotConfig::default()),
            agent_gc: Some(daemon::AgentGcSetting::default()),
            stop: Some(stop),
            ..Default::default()
        };
        local::register_at(
            &self.state,
            &mut opts,
            self.outbox.clone(),
            BOARD.to_string(),
        );
        let owned = self.state.clone();
        self.handle = Some(thread::spawn(move || {
            let _ = daemon::serve_with(&owned, opts);
        }));
        let deadline = Instant::now() + Duration::from_secs(15);
        while client::rpc(&self.state, "health", json!({})).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become healthy");
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// An RPC from an operator-shaped process (detached, env-clean —
    /// the shape `peer::operator_proof` accepts however this suite
    /// itself is run).
    fn op(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let frame = op::operator_rpc(&client::socket_path(&self.state), method, params);
        proto::unwrap(frame)
    }

    /// A read-only connection to the durable store.
    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            self.state.join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn proc_start(pid: u32) -> Option<i64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

/// A long-lived bash planted as an agent's pane; `cwd` is the agent's
/// task worktree — what `local` confines attachments to.
struct Lane {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    dir: TempDir,
    seq: u64,
    endpoint: Option<String>,
    state: String,
    alias: String,
}

impl Lane {
    /// `params` carries the register extras — `params.upstream` names
    /// the PM this agent's presses/outcomes route to.
    fn spawn_as(d: &Daemon, alias: &str, cwd: &Path, params: Option<&str>, role: &str) -> Lane {
        let mut child = Command::new("bash")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut lane = Lane {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            dir: TempDir::new().unwrap(),
            seq: 0,
            endpoint: None,
            state: String::new(),
            alias: alias.to_string(),
        };
        let mut req = json!({"alias": alias, "provider": "inbox",
                             "endpoint_kind": "inbox", "role": role,
                             "cwd": cwd.to_str().unwrap()});
        if let Some(p) = params {
            req["params"] = json!(p);
        }
        d.op("agent_register", req)
            .unwrap_or_else(|e| panic!("register {alias}: {e}"));
        lane.replant(d);
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        let (endpoint, state): (Option<String>, String) = conn
            .query_row(
                "SELECT endpoint, state FROM agents WHERE alias=?1",
                [alias],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        lane.endpoint = endpoint;
        lane.state = state;
        lane
    }

    /// The planted pane facts the caller rule reads.
    fn replant(&self, d: &Daemon) {
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "UPDATE agents SET endpoint_kind='pty', pid=?1, pid_start=?3, \
                enabled=0, generation='planted', session_id='planted' WHERE alias=?2",
            rusqlite::params![self.pid() as i64, self.alias, proc_start(self.pid())],
        )
        .unwrap();
        if let Some(endpoint) = &self.endpoint {
            conn.execute(
                "UPDATE agents SET endpoint=?1, state=?2 WHERE alias=?3",
                rusqlite::params![endpoint, self.state, self.alias],
            )
            .unwrap();
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn run(&mut self, cmd: &str) -> (i64, String) {
        let tag = format!("__lane_rc_{}__", self.seq);
        self.seq += 1;
        writeln!(self.stdin, "{{ {cmd} ; }} 2>&1; rc=$?; echo; echo {tag}$rc").unwrap();
        self.stdin.flush().unwrap();
        let mut out = String::new();
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "lane shell exited while running: {cmd}"
            );
            if let Some(rc) = line.strip_prefix(&tag) {
                return (rc.trim().parse().unwrap(), out);
            }
            out.push_str(&line);
        }
    }

    /// One RPC whose socket peer is a child of this lane's bash — the
    /// caller rule attributes it to this agent.
    fn rpc(&mut self, d: &Daemon, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let req = self.dir.path().join(format!("req-{}.json", self.seq));
        std::fs::write(&req, proto::request(method, params).to_string()).unwrap();
        let (rc, out) = self.run(&format!(
            "python3 -c 'import socket,sys;\
             s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);\
             s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");\
             print(s.makefile().readline())' {} {}",
            client::socket_path(&d.state).display(),
            req.display()
        ));
        assert_eq!(rc, 0, "lane rpc {method} failed: {out}");
        let frame: Value = serde_json::from_str(out.trim()).unwrap();
        proto::unwrap(frame)
    }
}

impl Drop for Lane {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One RPC from a process tied to no pane and not provably the
/// operator (`Who::Unproven`): detached like the operator call but
/// carrying a `CADENCE_ALIAS` its ancestry cannot prove.
fn unprovable_rpc(d: &Daemon, method: &str, params: Value) -> Value {
    let out = Command::new("python3")
        .arg("-c")
        .arg(
            "import socket,sys;s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);\
             s.sendall(sys.argv[2].encode()+b'\\n');print(s.makefile().readline())",
        )
        .arg(client::socket_path(&d.state))
        .arg(proto::request(method, params).to_string())
        .env("CADENCE_ALIAS", "detached-agent")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    serde_json::from_slice(&out.stdout).unwrap()
}

/// One HTTP request to the board from an operator-shaped process —
/// detached (`setsid -f`), off this runner's ancestry, env clean — so
/// the route's `prove_operator_peer` accepts its peer however this
/// suite itself is run. Answers the raw reply text.
fn op_http(port: u16, request: &str) -> String {
    let dir = TempDir::new().unwrap();
    let req = dir.path().join("req.txt");
    let out = dir.path().join("out.txt");
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&req)
            .unwrap();
        f.write_all(request.as_bytes()).unwrap();
    }
    let script = dir.path().join("get.py");
    std::fs::write(
        &script,
        r#"import os, socket, sys, time

req_path, out_path, port, runner = sys.argv[1:5]

def on_lineage(pid):
    p = os.getpid()
    while p > 1:
        if p == pid:
            return True
        with open("/proc/%d/status" % p) as f:
            p = int([l for l in f if l.startswith("PPid:")][0].split()[1])
    return False

while on_lineage(int(runner)):
    time.sleep(0.02)
s = socket.create_connection(("127.0.0.1", int(port)))
s.sendall(open(req_path, "rb").read())
data = b""
while True:
    chunk = s.recv(65536)
    if not chunk:
        break
    data += chunk
with open(out_path + ".tmp", "wb") as f:
    f.write(data)
os.rename(out_path + ".tmp", out_path)
"#,
    )
    .unwrap();
    let status = Command::new("setsid")
        .arg("-f")
        .arg("python3")
        .arg(&script)
        .arg(&req)
        .arg(&out)
        .arg(port.to_string())
        .arg(std::process::id().to_string())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "setsid -f failed: {status}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !out.exists() {
        assert!(Instant::now() < deadline, "operator http never answered");
        thread::sleep(Duration::from_millis(20));
    }
    std::fs::read_to_string(&out).unwrap()
}

/// `(status, body)` of a raw reply.
fn http_parts(reply: &str) -> (u16, String) {
    let status = reply
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = reply
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or("")
        .to_string();
    (status, body)
}

fn refused(r: cadence_agent::Result<Value>) -> String {
    match r {
        Err(e) => e.to_string(),
        Ok(v) => panic!("call was admitted: {v}"),
    }
}

fn enroll(d: &Daemon, account: &str) {
    d.op(
        "platform_enroll",
        json!({"accept_same_uid_risk": true, "platform": "local",
               "account": account, "scopes": ["publish"], "shape": "token",
               "token": TOKEN}),
    )
    .unwrap_or_else(|e| panic!("enroll local/{account}: {e}"));
}

fn grant(d: &Daemon, agent: &str, account: &str) {
    d.op(
        "platform_grant",
        json!({"agent": agent, "platform": "local",
               "account": account, "scopes": ["publish"]}),
    )
    .unwrap_or_else(|e| panic!("grant {agent} local/{account}: {e}"));
}

/// The pending-effect row `platform_effects` reports (operator read).
fn effect_row(d: &Daemon, request: &str) -> Option<Value> {
    let out = d
        .op("platform_effects", json!({}))
        .expect("operator platform_effects");
    out["effects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["request"] == request)
        .cloned()
}

/// The daemon message the outcome was delivered as — the dedupe id is
/// `daemon_message_id("effect", effect_id)` on the PM/upstream lane.
fn outcome_message(d: &Daemon, effect_id: &str) -> Option<(String, String)> {
    let conn = d.db();
    conn.query_row(
        "SELECT alias, body FROM messages WHERE id=?1",
        rusqlite::params![proto::daemon_message_id("effect", effect_id)],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )
    .ok()
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// `publish` input naming a project/title/body plus attachments.
fn publish_input(project: &str, attachments: &[&str]) -> Value {
    json!({
        "project": project,
        "title": "The launch post",
        "body": "Everything shipped.\n\nA body paragraph.",
        "attachments": attachments,
    })
}

/// Stage a publish from `lane`; answers the effect id.
fn stage(d: &Daemon, lane: &mut Lane, request: &str, input: Value) -> String {
    let out = lane
        .rpc(
            d,
            "platform_call",
            json!({"platform": "local", "account": "outbox",
                   "tool": "publish", "input": input,
                   "request": request}),
        )
        .unwrap_or_else(|e| panic!("stage {request}: {e}"));
    assert_eq!(out["result"], json!("staged"), "{request}: {out}");
    out["effect_id"].as_str().unwrap().to_string()
}

/// Operator-press `request`; answers the effect record.
fn press(d: &Daemon, alias: &str, request: &str) -> Value {
    d.op(
        "agent_respond",
        json!({"alias": alias, "request": request, "decision": "accept"}),
    )
    .unwrap_or_else(|e| panic!("press {request}: {e}"))["effect"]
        .clone()
}

// ---------- the acceptance path ----------

/// CAD-546's end-to-end: an agent stages a publish through the real
/// gate; a PM and the requesting agent are refused the release; the
/// operator presses; the outbox item lands with its modes; the PM
/// receives the verified outcome naming the board link; the same
/// effect id replays idempotently.
#[test]
fn publish_end_to_end_operator_release() {
    let d = Daemon::start();
    let wt = TempDir::new().unwrap();
    std::fs::create_dir_all(wt.path().join("img")).unwrap();
    std::fs::write(wt.path().join("img/shot.png"), b"PNG-BYTES").unwrap();
    std::fs::write(wt.path().join("note.txt"), b"note bytes").unwrap();
    let pm_wt = TempDir::new().unwrap();
    let mut pm = Lane::spawn_as(&d, "pm", pm_wt.path(), None, "pm");
    let mut sw = Lane::spawn_as(&d, "sw", wt.path(), Some(r#"{"upstream":"pm"}"#), "worker");
    enroll(&d, "outbox");
    grant(&d, "sw", "outbox");

    let input = publish_input("cadence", &["img/shot.png", "note.txt"]);
    let eid = stage(&d, &mut sw, "req-pub", input.clone());

    // Staged with its preview, visible as a waiting send.
    let row = effect_row(&d, "req-pub").expect("staged row");
    assert_eq!(row["state"], "waiting");
    assert_eq!(row["effect"], json!("send"));
    assert!(row["preview"].as_str().unwrap().contains("The launch post"));
    assert!(row["preview"].as_str().unwrap().contains("img/shot.png"));

    // The release is operator-only: the requester's PM cannot press,
    // the requester cannot answer its own request at all, and an
    // unproven caller is refused outright. (Mutation pin: a `local`
    // bypass or a loosened caller rule fails these.)
    let err = refused(pm.rpc(
        &d,
        "agent_respond",
        json!({"alias": "sw", "request": "req-pub", "decision": "accept"}),
    ));
    assert!(err.contains("operator-only"), "{err}");
    let err = refused(sw.rpc(
        &d,
        "agent_respond",
        json!({"alias": "sw", "request": "req-pub", "decision": "accept"}),
    ));
    assert!(err.contains("cannot answer its own request"), "{err}");
    let frame = unprovable_rpc(
        &d,
        "agent_respond",
        json!({"alias": "sw", "request": "req-pub", "decision": "accept"}),
    );
    assert_eq!(frame["ok"], json!(false), "unproven press admitted");

    // The operator's press executes; the outbox item lands complete.
    let row = press(&d, "sw", "req-pub");
    assert_eq!(row["state"], "done", "{row:?}");
    assert_eq!(row["outcome"]["verified"], json!(true));
    let item = d.outbox.join("cadence").join(&eid);
    assert_eq!(mode(&item), 0o700, "item dir mode");
    assert_eq!(mode(&item.join("attachments")), 0o700);
    assert_eq!(mode(&item.join("post.md")), 0o600);
    assert_eq!(mode(&item.join("index.json")), 0o600);
    assert_eq!(mode(&item.join("attachments/shot.png")), 0o600);
    assert_eq!(mode(&d.outbox), 0o700, "outbox root mode");
    assert_eq!(mode(&d.outbox.join("cadence")), 0o700);
    let post = std::fs::read_to_string(item.join("post.md")).unwrap();
    assert_eq!(
        post,
        "# The launch post\n\nEverything shipped.\n\nA body paragraph."
    );
    assert_eq!(
        std::fs::read(item.join("attachments/shot.png")).unwrap(),
        b"PNG-BYTES"
    );
    assert_eq!(
        std::fs::read(item.join("attachments/note.txt")).unwrap(),
        b"note bytes"
    );
    let index: Value =
        serde_json::from_str(&std::fs::read_to_string(item.join("index.json")).unwrap()).unwrap();
    assert_eq!(index["effect_id"], json!(eid));
    assert_eq!(index["project"], json!("cadence"));
    assert_eq!(index["attachments"].as_array().unwrap().len(), 2);
    let result = &index["result"];
    assert_eq!(
        result["board_url"].as_str().unwrap(),
        format!("{BOARD}/outbox?item={eid}")
    );
    assert_eq!(row["outcome"]["result"]["board_url"], result["board_url"]);

    // The PM lane receives the verified outcome — the board link rides
    // the message, deduped under the effect id.
    let (alias, body) = outcome_message(&d, &eid).expect("outcome message");
    assert_eq!(alias, "pm");
    assert!(body.contains("platform effect"), "{body}");
    assert!(body.contains("done"), "{body}");
    assert!(
        body.contains(&format!("board: {BOARD}/outbox?item={eid}")),
        "{body}"
    );

    // The requester reads its own row back: done and verified.
    let own = sw
        .rpc(&d, "platform_effects", json!({}))
        .expect("agent effects read");
    let mine = own["effects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["request"] == "req-pub")
        .cloned()
        .unwrap();
    assert_eq!(mine["state"], "done");
    assert_eq!(mine["outcome"]["verified"], json!(true));

    // Idempotent replay under the same effect id: the adapter replays
    // the recorded outcome with no second write; a different input
    // under the same key is refused.
    let adapter = {
        let mut opts = daemon::ServeOptions::default();
        local::register_at(&d.state, &mut opts, d.outbox.clone(), BOARD.to_string());
        opts.platforms.get("local").unwrap().clone()
    };
    let replayed = adapter
        .execute(TOKEN.as_bytes(), "publish", &input, &eid, None)
        .unwrap();
    assert_eq!(replayed, *result);
    let changed = adapter.execute(
        TOKEN.as_bytes(),
        "publish",
        &publish_input("cadence", &["note.txt"]),
        &eid,
        None,
    );
    assert!(changed.is_err(), "a different input replayed");

    // The outbox read model serves the item (operator RPC; the HTTP
    // gate is proven in `board_outbox_route_is_operator_only`).
    let list = d.op("platform_outbox", json!({})).unwrap();
    let items = list["items"].as_array().unwrap();
    let one = items.iter().find(|i| i["effect_id"] == eid).unwrap();
    assert_eq!(one["title"], json!("The launch post"));
    assert!(one["preview"]
        .as_str()
        .unwrap()
        .contains("Everything shipped"));
    let detail = d.op("platform_outbox", json!({"effect_id": eid})).unwrap();
    assert_eq!(detail["item"]["effect_id"], json!(eid));
    assert!(detail["item"]["post"]
        .as_str()
        .unwrap()
        .contains("# The launch post"));
}

/// Decline is terminal: the PM declines a staged send, the row closes
/// without writing, and a later press is refused. The requester cannot
/// even decline its own staged send — a send's answer belongs to its
/// PM or the operator.
#[test]
fn decline_is_terminal() {
    let d = Daemon::start();
    let wt = TempDir::new().unwrap();
    let pm_wt = TempDir::new().unwrap();
    let mut pm = Lane::spawn_as(&d, "pm", pm_wt.path(), None, "pm");
    let mut sw = Lane::spawn_as(&d, "sw", wt.path(), Some(r#"{"upstream":"pm"}"#), "worker");
    enroll(&d, "outbox");
    grant(&d, "sw", "outbox");

    let eid = stage(&d, &mut sw, "req-dec", publish_input("cadence", &[]));

    // The requester's own respond never reaches the gate.
    let err = refused(sw.rpc(
        &d,
        "agent_respond",
        json!({"alias": "sw", "request": "req-dec", "decision": "decline"}),
    ));
    assert!(err.contains("cannot answer its own request"), "{err}");

    // The PM's decline lands and is terminal.
    let out = pm
        .rpc(
            &d,
            "agent_respond",
            json!({"alias": "sw", "request": "req-dec", "decision": "decline",
                   "reason": "not today"}),
        )
        .expect("pm decline");
    assert_eq!(out["effect"]["state"], "declined");
    assert!(!d.outbox.join("cadence").join(&eid).exists());
    let err = refused(d.op(
        "agent_respond",
        json!({"alias": "sw", "request": "req-dec", "decision": "accept"}),
    ));
    assert!(err.contains("no longer waiting"), "{err}");
}

// ---------- attachment confinement ----------

/// Every escape shape refuses the whole call: `..`, an absolute path
/// outside the worktree, a symlink (file or dir component), a
/// directory, a missing file, a duplicate basename. The refused row
/// lands `failed` and no outbox item is written — mutation pin: drop
/// the confinement and the `!exists` asserts fail.
#[test]
fn attachment_confinement_refuses_escapes() {
    let d = Daemon::start();
    let wt = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    std::fs::write(wt.path().join("real.txt"), b"real").unwrap();
    std::fs::create_dir_all(wt.path().join("sub/inner")).unwrap();
    std::fs::write(wt.path().join("sub/inner/deep.txt"), b"deep").unwrap();
    std::fs::write(wt.path().join("sub/real.txt"), b"also real").unwrap();
    std::os::unix::fs::symlink("real.txt", wt.path().join("link.txt")).unwrap();
    std::os::unix::fs::symlink("sub", wt.path().join("dirlink")).unwrap();
    std::os::unix::fs::symlink(outside.path(), wt.path().join("escape")).unwrap();
    std::fs::write(outside.path().join("secret.txt"), b"SECRET").unwrap();
    let mut sw = Lane::spawn_as(&d, "sw", wt.path(), None, "worker");
    enroll(&d, "outbox");
    grant(&d, "sw", "outbox");

    let abs_inside = wt.path().join("real.txt").display().to_string();
    let abs_outside = outside.path().join("secret.txt").display().to_string();
    let cases: &[(&str, &[&str], &str)] = &[
        ("dotdot", &["../secret.txt"], "`..` escapes"),
        ("dotdot-rel", &["sub/../../x"], "`..` escapes"),
        ("abs-outside", &[&abs_outside], "inside the task worktree"),
        ("symlink-file", &["link.txt"], "symlink"),
        ("symlink-dir", &["dirlink/inner/deep.txt"], "symlink"),
        ("symlink-escape", &["escape/secret.txt"], "symlink"),
        ("directory", &["sub"], "regular file"),
        ("missing", &["nope.txt"], "cannot be opened"),
        (
            "dup-basename",
            &["real.txt", "sub/real.txt"],
            "share the name",
        ),
        (
            "dup-via-dotdot",
            &["real.txt", "sub/../real.txt"],
            "`..` escapes",
        ),
    ];
    for (i, (name, atts, want)) in cases.iter().enumerate() {
        let request = format!("req-esc-{i}");
        let eid = stage(&d, &mut sw, &request, publish_input("cadence", atts));
        let row = press(&d, "sw", &request);
        assert_eq!(row["state"], "failed", "{name}: {row:?}");
        let err = row["outcome"]["error"].as_str().unwrap_or_default();
        assert!(
            err.contains(want),
            "{name}: refusal '{err}' does not name {want}"
        );
        assert!(
            !d.outbox.join("cadence").join(&eid).exists(),
            "{name}: an item landed for a refused path"
        );
    }

    // The good shapes still land: a nested relative path and the same
    // file named by its absolute path inside the worktree.
    let eid = stage(
        &d,
        &mut sw,
        "req-ok",
        publish_input("cadence", &["sub/inner/deep.txt", &abs_inside]),
    );
    let row = press(&d, "sw", "req-ok");
    assert_eq!(row["state"], "done", "{row:?}");
    let item = d.outbox.join("cadence").join(&eid);
    assert!(item.join("attachments/deep.txt").is_file());
    assert!(item.join("attachments/real.txt").is_file());
}

/// The malformed-input shapes refuse at execute (the gate stages the
/// call regardless — input is never trusted, only validated): a bad
/// project name, an empty title, a missing body, attachments of the
/// wrong shape.
#[test]
fn malformed_inputs_fail_at_execute() {
    let d = Daemon::start();
    let wt = TempDir::new().unwrap();
    let mut sw = Lane::spawn_as(&d, "sw", wt.path(), None, "worker");
    enroll(&d, "outbox");
    grant(&d, "sw", "outbox");

    for (i, input) in [
        json!({"project": "../x", "title": "t", "body": "b"}),
        json!({"project": "cadence", "title": "", "body": "b"}),
        json!({"project": "cadence", "title": "t"}),
        json!({"project": "cadence", "title": "t", "body": "b",
               "attachments": "not-a-list"}),
    ]
    .iter()
    .enumerate()
    {
        let request = format!("req-bad-{i}");
        let eid = stage(&d, &mut sw, &request, input.clone());
        let row = press(&d, "sw", &request);
        assert_eq!(row["state"], "failed", "{input}");
        assert!(
            !d.outbox.join("cadence").join(&eid).exists(),
            "{input}: an item landed for a refused input"
        );
    }
}

/// Only `publish` exists — another tool gates as `send` (undeclared
/// tools are sends) but the adapter refuses it at execute.
#[test]
fn unknown_tool_gates_send_then_refuses() {
    let d = Daemon::start();
    let wt = TempDir::new().unwrap();
    let mut sw = Lane::spawn_as(&d, "sw", wt.path(), None, "worker");
    enroll(&d, "outbox");
    grant(&d, "sw", "outbox");
    let out = sw
        .rpc(
            &d,
            "platform_call",
            json!({"platform": "local", "account": "outbox",
                   "tool": "delete_everything",
                   "input": {}, "request": "req-tool"}),
        )
        .unwrap();
    assert_eq!(out["result"], json!("staged"), "{out}");
    let row = press(&d, "sw", "req-tool");
    assert_eq!(row["state"], "failed");
    assert!(row["outcome"]["error"]
        .as_str()
        .unwrap()
        .contains("no tool"));
}

/// The `source_hash` pin runs for `local` unchanged: `input.source`
/// naming an artifact the adapter does not hold is refused at stage —
/// never staged unpinned.
#[test]
fn source_pin_refuses_unknown_artifact() {
    let d = Daemon::start();
    let wt = TempDir::new().unwrap();
    let mut sw = Lane::spawn_as(&d, "sw", wt.path(), None, "worker");
    enroll(&d, "outbox");
    grant(&d, "sw", "outbox");
    let mut input = publish_input("cadence", &[]);
    input["source"] = json!("reviewed-post.md");
    let err = refused(sw.rpc(
        &d,
        "platform_call",
        json!({"platform": "local", "account": "outbox",
               "tool": "publish", "input": input,
               "request": "req-src"}),
    ));
    assert!(err.contains("cannot be pinned"), "{err}");
}

// ---------- the read side is operator-only ----------

/// `platform_outbox` is the operator's read: an agent caller and an
/// unproven caller are refused at the daemon gate; the operator lists
/// and fetches. (Mutation pin: a `Rule::Read` here would admit the
/// agent call and fail the assert.)
#[test]
fn outbox_rpc_is_operator_only() {
    let d = Daemon::start();
    let wt = TempDir::new().unwrap();
    let mut sw = Lane::spawn_as(&d, "sw", wt.path(), None, "worker");
    enroll(&d, "outbox");
    grant(&d, "sw", "outbox");
    stage(&d, &mut sw, "req-list", publish_input("cadence", &[]));
    press(&d, "sw", "req-list");

    let err = refused(sw.rpc(&d, "platform_outbox", json!({})));
    assert!(err.contains("operator"), "{err}");
    let frame = unprovable_rpc(&d, "platform_outbox", json!({}));
    assert_eq!(frame["ok"], json!(false), "unproven read admitted");

    let list = d.op("platform_outbox", json!({})).unwrap();
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
}

/// `/api/outbox` is no less strict than the RPC it relays: no session
/// is refused, the operator's session on an agent-attributed peer is a
/// stolen session and refused, and the proven operator reads the list.
/// The board itself runs as a real `ui start` — a detached process —
/// so `board_is_operator` proves it too.
#[test]
fn board_outbox_route_is_operator_only() {
    let d = Daemon::start();
    let wt = TempDir::new().unwrap();
    let mut sw = Lane::spawn_as(&d, "sw", wt.path(), None, "worker");
    enroll(&d, "outbox");
    grant(&d, "sw", "outbox");
    stage(&d, &mut sw, "req-board", publish_input("cadence", &[]));
    press(&d, "sw", "req-board");

    // A real board: `ui start` hands the server to a detached session
    // leader off this runner's ancestry with a clean env — the shape
    // `board_is_operator` demands.
    let home = TempDir::new().unwrap();
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let bin = env!("CARGO_BIN_EXE_cadence");
    let started = Command::new(bin)
        .arg("--state-dir")
        .arg(&d.state)
        .args(["ui", "start", "--port", &port.to_string()])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .env("CADENCE_PM_DIR", home.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        started.status.success(),
        "ui start: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    // `ui stop` on the way out — never leave a board running.
    let _stopper = UiStop(d.state.clone(), home.path().to_path_buf());
    let host = op::board_host(port);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let reply = op_http(
            port,
            &format!("GET /api/health HTTP/1.0\r\nHost: {host}\r\n\r\n"),
        );
        if reply.starts_with("HTTP/1.0 200") || reply.starts_with("HTTP/1.1 200") {
            break;
        }
        assert!(Instant::now() < deadline, "board did not start: {reply}");
        thread::sleep(Duration::from_millis(60));
    }

    // No session: refused before the peer proof even runs.
    let reply = op_http(
        port,
        &format!("GET /api/outbox HTTP/1.0\r\nHost: {host}\r\n\r\n"),
    );
    let (status, body) = http_parts(&reply);
    assert!(
        status == 401 || status == 403,
        "unauthenticated read: {reply}"
    );
    assert!(body.contains("operator"), "{body}");

    // Held by an agent-attributed peer, the operator's session is
    // stolen — refused, and the daemon revokes it on the spot.
    let session = op::sign_in(bin, &d.state, port);
    let stolen = session.request("GET", "/api/outbox", "");
    let req_file = sw.dir.path().join("stolen-req.txt");
    std::fs::write(&req_file, &stolen).unwrap();
    let (rc, reply) = sw.run(&format!(
        "exec 3<>/dev/tcp/127.0.0.1/{port}; cat {} >&3; cat <&3",
        req_file.display()
    ));
    assert_eq!(rc, 0, "{reply}");
    let (status, body) = http_parts(&reply);
    assert!(
        status == 401 || status == 403,
        "an agent-attributed peer held the operator session: {reply}"
    );
    assert!(
        body.contains("stolen") || body.contains("operator"),
        "{body}"
    );

    // The proven operator: a fresh session on a clean detached peer
    // reads the list. (Fresh — the theft attempt revoked the first.)
    let session = op::sign_in(bin, &d.state, port);
    let reply = op_http(port, &session.request("GET", "/api/outbox", ""));
    let (status, body) = http_parts(&reply);
    assert_eq!(status, 200, "{reply}");
    let parsed: Value = serde_json::from_str(&body).unwrap();
    let items = parsed["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{parsed}");
    assert!(items[0]["preview"].as_str().unwrap().contains("shipped"));
}

/// `ui stop` on drop — the board belongs to this test.
struct UiStop(PathBuf, PathBuf);

impl Drop for UiStop {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&self.0)
            .args(["ui", "stop"])
            .env("CADENCE_PM_DIR", &self.1)
            .output();
    }
}

/// A restart with a `decided` row in flight lands `reconcile`, never
/// re-fired: the crash window between the durable decision and the
/// execute is exactly what §5.4 step 8 marks for a human — the item
/// must NOT be published by the restart, and the outcome stays
/// unwritten.
#[test]
fn decided_at_restart_never_refires() {
    let dir = TempDir::new().unwrap();
    let state = dir.path().join("state");
    let pm_dir = dir.path().join("pm");
    let outbox = dir.path().join("outbox");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&pm_dir).unwrap();
    let crash = Arc::new(AtomicBool::new(false));

    let serve = |crash: Arc<AtomicBool>| {
        let env = cadence_agent::adapter::ProviderEnv::default();
        env.set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let mut opts = daemon::ServeOptions {
            provider_env: env,
            report_router: Some(0),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            slots: Some(cadence_agent::slots::SlotConfig::default()),
            agent_gc: Some(daemon::AgentGcSetting::default()),
            stop: Some(stop.clone()),
            effect_execute_gate: Some(Arc::new(move |_| !crash.load(Ordering::SeqCst))),
            ..Default::default()
        };
        local::register_at(&state, &mut opts, outbox.clone(), BOARD.to_string());
        let owned = state.clone();
        let handle = thread::spawn(move || {
            let _ = daemon::serve_with(&owned, opts);
        });
        let deadline = Instant::now() + Duration::from_secs(15);
        while client::rpc(&state, "health", json!({})).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become healthy");
            thread::sleep(Duration::from_millis(50));
        }
        (stop, handle)
    };

    let (stop, handle) = serve(crash.clone());
    let d = Daemon {
        state: state.clone(),
        pm: pm_dir.clone(),
        outbox: outbox.clone(),
        _dir: TempDir::new().unwrap(),
        stop: Arc::new(AtomicBool::new(false)),
        handle: None,
    };
    let wt = TempDir::new().unwrap();
    std::fs::write(wt.path().join("a.txt"), b"AAA").unwrap();
    let mut sw = Lane::spawn_as(&d, "sw", wt.path(), None, "worker");
    enroll(&d, "outbox");
    grant(&d, "sw", "outbox");
    let eid = stage(&d, &mut sw, "req-re", publish_input("cadence", &["a.txt"]));

    // The daemon "dies" between the durable decide and the execute:
    // the press is recorded, the row stays `decided`, nothing writes.
    crash.store(true, Ordering::SeqCst);
    let row = press(&d, "sw", "req-re");
    assert_eq!(row["state"], "decided", "{row:?}");
    assert!(!outbox.join("cadence").join(&eid).exists());

    // Restart: the row surfaces `reconcile` in Needs-you — the operator
    // reconciles by hand — and the outbox stays empty.
    stop.store(true, Ordering::SeqCst);
    let _ = handle.join();
    let (stop2, handle2) = serve(crash.clone());
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(row) = effect_row(&d, "req-re") {
            if row["state"] == "reconcile" {
                break;
            }
        }
        assert!(Instant::now() < deadline, "decided row never reconciled");
        thread::sleep(Duration::from_millis(50));
    }
    let row = effect_row(&d, "req-re").unwrap();
    assert_eq!(row["state"], "reconcile");
    assert!(!outbox.join("cadence").join(&eid).exists());
    // The PM lane heard that an accepted effect lacks a proven outcome.
    let conn = d.db();
    let flagged: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM platform_effects WHERE effect_id=?1 AND needs_you=1",
            [&eid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(flagged, 1, "the reconcile row must carry Needs-you");
    stop2.store(true, Ordering::SeqCst);
    let _ = handle2.join();
}

#[path = "support/operator.rs"]
mod op;
