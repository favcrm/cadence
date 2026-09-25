//! CAD-506 / ADR 0006 §5.2, §5.4, §5.6: the effect gate and the
//! pending-effect lifecycle, proven two ways.
//!
//! `contract_vectors_run_against_the_real_gate` executes every vector
//! in `contracts/connected-platform/v1/vectors.json` against a live
//! daemon wired to the fixture `FakePlatform` — the C1–C10 behaviour
//! the shared contract pins, driven through the real RPC surface
//! (socket identity for callers, the operator proof for the press).
//!
//! The adversarial tests below it mutation-proof the daemon-enforced
//! guards the vectors cannot express: forged and unproven callers,
//! cross-agent visibility, a handle collision with a brokered request,
//! a grant revoked between stage and press, concurrent presses, and
//! the no-credential-bytes hunt over the whole store.

// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use cadence_agent::contract_fixture::{FakePlatform, ReadBack, ToolTable, Verified};
use cadence_agent::platform::PlatformAdapter;
use cadence_agent::{client, daemon, proto};
use serde_json::{json, Value};
use tempfile::TempDir;

/// The credential bytes the leak hunt searches every surface for.
const TOKEN: &str = concat!("cadp_effect_testt", "oken_b1c2d3e4f5g6");

// ---------- harness ----------

/// In-process daemon with the platform-adapter map wired — the gate's
/// `platforms` plus the test-only `effect_execute_gate` crash seam
/// (`crash` set ⇒ the press records `decided` then the "daemon dies"
/// before Execute; §5.4 step 8's window).
struct Daemon {
    state: PathBuf,
    pm: PathBuf,
    _dir: TempDir,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
    platforms: HashMap<String, Arc<dyn PlatformAdapter>>,
    crash: Arc<AtomicBool>,
}

impl Daemon {
    fn start(platforms: HashMap<String, Arc<dyn PlatformAdapter>>) -> Self {
        let dir = TempDir::new().unwrap();
        let state = dir.path().join("state");
        let pm = dir.path().join("pm");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&pm).unwrap();
        let mut d = Self {
            state,
            pm,
            _dir: dir,
            stop: Arc::new(AtomicBool::new(false)),
            handle: None,
            platforms,
            crash: Arc::new(AtomicBool::new(false)),
        };
        d.serve();
        d
    }

    fn serve(&mut self) {
        let env = cadence_agent::adapter::ProviderEnv::default();
        env.set("CADENCE_PM_DIR", self.pm.to_str().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = stop.clone();
        let crash = self.crash.clone();
        let opts = daemon::ServeOptions {
            provider_env: env,
            report_router: Some(0),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            slots: Some(cadence_agent::slots::SlotConfig::default()),
            agent_gc: Some(daemon::AgentGcSetting::default()),
            stop: Some(stop),
            platforms: self.platforms.clone(),
            effect_execute_gate: Some(Arc::new(move |_| !crash.load(Ordering::SeqCst))),
            ..Default::default()
        };
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

    /// §5.4 step 8: a real daemon restart — the serve loop drops,
    /// `Store::open` re-runs recover, and reconciliation reads the
    /// durable rows. The `FakePlatform` survives (it is the platform).
    fn restart(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.serve();
    }

    fn op(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let frame = op::operator_rpc(&client::socket_path(&self.state), method, params);
        proto::unwrap(frame)
    }

    /// A read-only connection to the durable store — observes row state
    /// without touching an RPC (a `platform_effects` read runs the
    /// source-cancel scan; vectors' `uncaught` edits must not trip it).
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

/// A long-lived bash planted as an agent's pane — `tests/platform.rs`'s
/// `Lane`. `role`/`params` are the register fields: `params.upstream`
/// names the PM that authorises this agent's presses.
struct Lane {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    dir: TempDir,
    seq: u64,
    /// The row's `endpoint`/`state` as planted — a daemon restart's
    /// recover() clears runtime fields on every non-inbox agent, so the
    /// surviving pane replants them (what re-adoption does live).
    endpoint: Option<String>,
    state: String,
    alias: String,
}

impl Lane {
    fn spawn_as(d: &Daemon, alias: &str, params: Option<&str>, role: &str) -> Lane {
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
                             "cwd": d.state.to_str().unwrap()});
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

    /// The pane survived a daemon restart but the row's runtime fields
    /// did not — recover() clears them on every non-inbox agent. Write
    /// the planted facts back, as live re-adoption would.
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
/// operator: detached like the operator call but carrying a
/// `CADENCE_ALIAS` its ancestry cannot prove (`Who::Unproven`).
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

fn refused(r: cadence_agent::Result<Value>) -> String {
    match r {
        Err(e) => e.to_string(),
        Ok(v) => panic!("call was admitted: {v}"),
    }
}

fn enroll(d: &Daemon, platform: &str, account: &str, scopes: &[&str]) {
    d.op(
        "platform_enroll",
        json!({"accept_same_uid_risk": true, "platform": platform,
               "account": account, "scopes": scopes, "shape": "token",
               "token": TOKEN}),
    )
    .unwrap_or_else(|e| panic!("enroll {platform}/{account}: {e}"));
}

fn grant(d: &Daemon, agent: &str, platform: &str, account: &str, scopes: &[&str]) {
    d.op(
        "platform_grant",
        json!({"agent": agent, "platform": platform,
               "account": account, "scopes": scopes}),
    )
    .unwrap_or_else(|e| panic!("grant {agent} {platform}/{account}: {e}"));
}

/// Every row of every table as text — the credential hunt.
fn db_snapshot(d: &Daemon) -> String {
    use rusqlite::types::ValueRef;
    let conn = d.db();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut out = String::new();
    for table in tables {
        let mut stmt = conn.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
        let cols = stmt.column_count();
        let rows: Vec<String> = stmt
            .query_map([], |r| {
                Ok((0..cols)
                    .map(|i| match r.get_ref(i).unwrap() {
                        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                        ValueRef::Blob(b) => {
                            format!("BLOB({}b){}", b.len(), String::from_utf8_lossy(b))
                        }
                        other => format!("{other:?}"),
                    })
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        out.push_str(&format!("## {table}\n{}\n", rows.join("\n")));
    }
    out
}

/// One effect row straight from the store — no scan, no RPC.
fn effect_row(d: &Daemon, request: &str) -> Option<Value> {
    let conn = d.db();
    conn.query_row(
        "SELECT state, close_reason, decision, outcome, needs_you, effect_id, tool
         FROM platform_effects WHERE request=?1",
        rusqlite::params![request],
        |r| {
            Ok(json!({
                "state": r.get::<_, String>(0)?,
                "close_reason": r.get::<_, Option<String>>(1)?,
                "decision": r.get::<_, Option<String>>(2)?
                    .map(|s| serde_json::from_str::<Value>(&s).unwrap()),
                "outcome": r.get::<_, Option<String>>(3)?
                    .map(|s| serde_json::from_str::<Value>(&s).unwrap()),
                "needs_you": r.get::<_, i64>(4)? == 1,
                "effect_id": r.get::<_, String>(5)?,
                "tool": r.get::<_, String>(6)?,
            }))
        },
    )
    .ok()
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

#[path = "support/operator.rs"]
mod op;

// ---------- the §5.6 vector runner ----------

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("contracts/connected-platform/v1")
}

fn load_vector_fixture() -> Value {
    let text = std::fs::read_to_string(fixture_dir().join("vectors.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// Assert the live record carries every specimen field. `decision.at`
/// is a wall-clock stamp — asserted present and RFC-3339-shaped, not
/// equal to the specimen's example.
fn assert_record(actual: &Value, specimen: &Value, ctx: &str) {
    let mut live = actual.clone();
    if let Some(d) = live.get_mut("decision") {
        let at = d["at"].as_str().unwrap_or_default();
        assert!(
            at.contains('T') && at.ends_with('Z'),
            "{ctx}: decision.at={at}"
        );
        d.as_object_mut().unwrap().remove("at");
    }
    let mut want = specimen.clone();
    if let Some(d) = want.get_mut("decision") {
        d.as_object_mut().unwrap().remove("at");
    }
    assert_eq!(live, want, "{ctx}: record disagrees with the specimen");
}

/// The pending-effect record the gate's `platform_effects` read reports
/// for `request` — operator-scoped, so the whole row is visible.
fn live_record(d: &Daemon, request: &str) -> Value {
    let out = d
        .op("platform_effects", json!({}))
        .expect("operator platform_effects");
    out["effects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["request"].as_str() == Some(request))
        .cloned()
        .unwrap_or_else(|| panic!("no effect row for {request}: {out}"))
}

/// One vector, executed end to end against the real gate.
fn run_vector(vector: &Value) {
    let id = vector["id"].as_str().unwrap();
    let given = &vector["given"];
    let agent = given["agent"].as_str().unwrap();
    let account = given["account"].as_str().unwrap();

    // The adapter: vector table (or the shared one), its reported
    // manifest version, source artifacts and adapter knobs.
    let table_json = match &given["tool_table"] {
        Value::String(s) if s == "default" => serde_json::from_str::<Value>(
            std::fs::read_to_string(fixture_dir().join("fake-tool-table.json"))
                .unwrap()
                .as_str(),
        )
        .unwrap(),
        other => other.clone(),
    };
    let table = ToolTable::from_json(&table_json)
        .unwrap_or_else(|e| panic!("{id}: tool table unparseable: {e}"));
    let platform = table.platform.clone();
    let fake = Arc::new(FakePlatform::new(table));
    if let Some(v) = given.get("reported_manifest_version") {
        fake.set_reported_manifest_version(v.as_str().map(str::to_string));
    }
    if let Some(sources) = given.get("sources").and_then(Value::as_object) {
        for (name, content) in sources {
            fake.write_source(name, content.as_str().unwrap());
        }
    }
    if let Some(adapter) = given.get("adapter") {
        match adapter["read_back"].as_str() {
            Some("mismatch") => fake.set_read_back(ReadBack::Mismatch),
            Some("unknown") => fake.set_read_back(ReadBack::Unknown),
            _ => {}
        }
        if let Some(fail) = adapter.get("fail") {
            fake.fail_tool(
                fail["tool"].as_str().unwrap(),
                fail["error"].as_str().unwrap(),
            );
        }
    }

    // The agent's PM: the first non-operator presser that isn't the
    // caller itself (op-pm/pm-agent), else a default upstream that
    // still exists so outcome messages have a lane.
    let upstream = vector["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["action"].as_str() == Some("press"))
        .filter_map(|s| s["by"]["member"].as_str())
        .find(|m| *m != "operator" && *m != agent)
        .unwrap_or("op-pm")
        .to_string();

    let mut platforms: HashMap<String, Arc<dyn PlatformAdapter>> = HashMap::new();
    platforms.insert(platform.clone(), fake.clone());
    let mut d = Daemon::start(platforms);
    enroll(
        &d,
        &platform,
        account,
        &["widgets:read", "widgets:write", "widgets:publish"],
    );

    // Pressers and the upstream register before the caller so
    // `effective_pm` resolves (the PM row must already exist).
    let mut lanes: HashMap<String, Lane> = HashMap::new();
    let mut members: Vec<(&str, &str)> = Vec::new();
    for step in vector["steps"].as_array().unwrap() {
        if step["action"].as_str() != Some("press") {
            continue;
        }
        let member = step["by"]["member"].as_str().unwrap();
        let role = step["by"]["role"].as_str().unwrap();
        if member != "operator" && member != agent && !members.iter().any(|(m, _)| *m == member) {
            members.push((member, role));
        }
    }
    if !members.iter().any(|(m, _)| *m == upstream) {
        members.push((&upstream, "pm"));
    }
    for (member, role) in members {
        // The record's role vocabulary is operator/pm/agent; the agent
        // row's launch role is pm|worker — "agent" registers as worker.
        let row_role = if role == "pm" { "pm" } else { "worker" };
        lanes.insert(
            member.to_string(),
            Lane::spawn_as(&d, member, None, row_role),
        );
    }
    let mut caller = Lane::spawn_as(
        &d,
        agent,
        Some(&json!({"upstream": upstream}).to_string()),
        "worker",
    );
    grant(
        &d,
        agent,
        &platform,
        account,
        &["widgets:read", "widgets:write", "widgets:publish"],
    );

    // The row this vector tracks — the most recent `call`'s request.
    let mut current_request: Option<String> = None;
    let mut executions_before = fake.execution_count();

    for (i, step) in vector["steps"].as_array().unwrap().iter().enumerate() {
        let expect = &step["expect"];
        let ctx = || format!("{id} step {i} ({})", step["action"].as_str().unwrap());
        match step["action"].as_str().unwrap() {
            "call" => {
                let tool = step["tool"].as_str().unwrap();
                let mut params = json!({
                    "platform": platform,
                    "account": account,
                    "tool": tool,
                    "input": step.get("arguments").cloned().unwrap_or(json!({})),
                });
                // A named handle is the caller-chosen request
                // (`req-{handle}`); a record specimen without one pins
                // the handle directly.
                let request = step["handle"]
                    .as_str()
                    .map(|h| format!("req-{h}"))
                    .or_else(|| expect["record"]["request"].as_str().map(str::to_string));
                if let Some(r) = &request {
                    params["request"] = json!(r);
                }
                let out = caller
                    .rpc(&d, "platform_call", params)
                    .unwrap_or_else(|e| panic!("{}: platform_call: {e}", ctx()));
                if let Some(want) = expect["result"].as_str() {
                    assert_eq!(out["result"], json!(want), "{}", ctx());
                }
                if expect["effect_id"] == json!(true) {
                    assert!(out["effect_id"].as_str().is_some(), "{}", ctx());
                }
                if out["request"].is_string() {
                    current_request = Some(out["request"].as_str().unwrap().to_string());
                }
                if let Some(state) = expect.get("state") {
                    match state.as_str() {
                        Some(s) => {
                            let req = current_request.clone().unwrap();
                            let rec = live_record(&d, &req);
                            assert_eq!(rec["state"], json!(s), "{}", ctx());
                        }
                        None => {
                            // No row exists for this call's request.
                            assert!(out.get("record").is_none(), "{}", ctx());
                        }
                    }
                }
                if let Some(spec) = expect.get("record") {
                    let req = current_request.clone().unwrap();
                    assert_record(&live_record(&d, &req), spec, &ctx());
                }
            }
            "press" => {
                let req = current_request.clone().unwrap();
                let mut params = json!({
                    "alias": agent,
                    "request": req,
                    "decision": step["decision"].as_str().unwrap(),
                });
                if let Some(r) = step["reason"].as_str() {
                    params["reason"] = json!(r);
                }
                let member = step["by"]["member"].as_str().unwrap();
                if step["crash_after_decision"] == json!(true) {
                    d.crash.store(true, Ordering::SeqCst);
                }
                let out = if member == "operator" {
                    d.op("agent_respond", params)
                } else {
                    // A press by the requesting agent itself goes through
                    // the caller's own lane — "an agent answers its own
                    // pending effect" is one of the refusals under test.
                    let lane = if member == agent {
                        &mut caller
                    } else {
                        lanes
                            .get_mut(member)
                            .unwrap_or_else(|| panic!("{id}: no lane for presser {member}"))
                    };
                    lane.rpc(&d, "agent_respond", params)
                };
                match expect["press"].as_str() {
                    Some("applied") => {
                        out.unwrap_or_else(|e| panic!("{}: press refused: {e}", ctx()));
                    }
                    Some("refused") => {
                        let _ = refused(out);
                    }
                    _ => panic!("{}: press expectation missing", ctx()),
                }
            }
            "edit_source" => {
                fake.write_source(
                    step["source"].as_str().unwrap(),
                    step["content"].as_str().unwrap(),
                );
                if step["uncaught"] != json!(true) {
                    // The caught case: the next touchpoint's scan
                    // observes the drift — a `platform_effects` read is
                    // the observer every vector driver has.
                    let _ = d.op("platform_effects", json!({}));
                }
            }
            "restart" => {
                d.crash.store(false, Ordering::SeqCst);
                d.restart();
                // The panes survived; recover() cleared their runtime
                // fields. Re-plant, as live re-adoption would — the
                // retried call and later presses need their identities.
                for lane in lanes.values() {
                    lane.replant(&d);
                }
                caller.replant(&d);
            }
            "caller_deadline" => {
                let req = current_request.clone().unwrap();
                let out = caller
                    .rpc(&d, "request_wait", json!({"request": req, "wait": 0}))
                    .unwrap_or_else(|e| panic!("{}: request_wait: {e}", ctx()));
                if expect["wait"].as_str() == Some("ended") {
                    assert_eq!(out["state"], json!("waiting"), "{}", ctx());
                }
            }
            other => panic!("{id}: unknown action {other}"),
        }

        // Behavioural assertions common to every step.
        let req = current_request.clone();
        let row = req.as_deref().and_then(|r| effect_row(&d, r));
        if let Some(state) = expect.get("state").filter(|s| !s.is_null()) {
            // `state` after an `uncaught` edit asserts the pre-scan
            // truth — read the row, never a scanning RPC.
            let state = state.as_str().unwrap();
            match step["action"].as_str().unwrap() {
                "call" | "press" | "edit_source" | "restart" | "caller_deadline" => {
                    let row = row
                        .clone()
                        .unwrap_or_else(|| live_record(&d, req.as_deref().unwrap()));
                    assert_eq!(
                        row["state"].as_str().unwrap(),
                        state,
                        "{}: row state",
                        ctx()
                    );
                }
                _ => {}
            }
        }
        if let Some(reason) = expect["close_reason"].as_str() {
            assert_eq!(
                row.as_ref().unwrap()["close_reason"].as_str().unwrap(),
                reason,
                "{}: close_reason",
                ctx()
            );
        }
        if let Some(fired) = expect["fired"].as_bool() {
            assert_eq!(
                fake.execution_count() > executions_before,
                fired,
                "{}: fired",
                ctx()
            );
        }
        executions_before = fake.execution_count();
        if let Some(n) = expect["executions"].as_u64() {
            let tool = row
                .as_ref()
                .map(|r| r["tool"].as_str().unwrap().to_string())
                .unwrap();
            assert_eq!(fake.executions_of(&tool) as u64, n, "{}: executions", ctx());
        }
        if let Some(v) = expect.get("verified").filter(|v| !v.is_null()) {
            let outcome = row.as_ref().unwrap()["outcome"].clone();
            assert_eq!(outcome["verified"], *v, "{}: verified", ctx());
        }
        if let Some(ny) = expect["needs_you"].as_bool() {
            let effects = d.op("platform_effects", json!({})).unwrap();
            let effect_id = row.as_ref().unwrap()["effect_id"].as_str().unwrap();
            let flagged = effects["needs_you"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["effect_id"].as_str() == Some(effect_id));
            assert_eq!(flagged, ny, "{}: needs_you", ctx());
        }
        if expect["delivered"].as_str() == Some("message") {
            let effect_id = row.as_ref().unwrap()["effect_id"].as_str().unwrap();
            let (lane, body) = outcome_message(&d, effect_id)
                .unwrap_or_else(|| panic!("{}: no outcome message", ctx()));
            assert_eq!(lane, upstream, "{}: outcome lane", ctx());
            assert!(body.contains(effect_id), "{}: outcome body", ctx());
            assert!(!body.contains(TOKEN), "{}: outcome leaked", ctx());
        }
    }
}

#[test]
fn contract_vectors_run_against_the_real_gate() {
    let vectors = load_vector_fixture();
    for vector in vectors["vectors"].as_array().unwrap() {
        run_vector(vector);
    }
}

// ---------- adversarial guards ----------

/// No adapter for a platform = no classification = fail closed. And a
/// platform call that never enrolled cannot name an account either.
#[test]
fn platform_call_fails_closed_without_adapter_or_grant() {
    let d = Daemon::start(HashMap::new());
    let mut agent = Lane::spawn_as(&d, "w1", None, "worker");
    let err = refused(agent.rpc(
        &d,
        "platform_call",
        json!({"platform": "fixture", "account": "acct-1",
               "tool": "widgets.list", "input": {}}),
    ));
    assert!(err.contains("no adapter"), "{err}");

    // An adapter exists but the caller holds no grant — refused before
    // a row ever exists.
    let mut platforms: HashMap<String, Arc<dyn PlatformAdapter>> = HashMap::new();
    platforms.insert("fixture".to_string(), Arc::new(FakePlatform::standard()));
    let d2 = Daemon::start(platforms);
    enroll(&d2, "fixture", "acct-1", &["widgets:read"]);
    let mut a2 = Lane::spawn_as(&d2, "w2", None, "worker");
    let err = refused(a2.rpc(
        &d2,
        "platform_call",
        json!({"platform": "fixture", "account": "acct-1",
               "tool": "widgets.list", "input": {}}),
    ));
    assert!(err.contains("grant"), "{err}");
    let conn = d2.db();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM platform_effects", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "a refused stage still wrote a row");
}

/// Identity the connection cannot prove is refused on every effect
/// surface; a forged `by`/`agent` field on `platform_call` is refused,
/// never silently trusted.
#[test]
fn unproven_and_forged_callers_get_nothing() {
    let mut platforms: HashMap<String, Arc<dyn PlatformAdapter>> = HashMap::new();
    platforms.insert("fixture".to_string(), Arc::new(FakePlatform::standard()));
    let d = Daemon::start(platforms);
    enroll(
        &d,
        "fixture",
        "acct-1",
        &["widgets:read", "widgets:write", "widgets:publish"],
    );
    let mut agent = Lane::spawn_as(&d, "w1", None, "worker");
    grant(
        &d,
        "w1",
        "fixture",
        "acct-1",
        &["widgets:read", "widgets:write", "widgets:publish"],
    );

    // Unproven caller: call/effects/close all refuse.
    for (method, params) in [
        (
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.publish", "input": {"widget": "w1"}}),
        ),
        ("platform_effects", json!({})),
        ("platform_effect_close", json!({"request": "req-x"})),
    ] {
        let frame = unprovable_rpc(&d, method, params);
        assert_eq!(frame["ok"], false, "{method} admitted an unproven caller");
    }

    // Forged identity fields on a call: refused outright.
    let err = refused(agent.rpc(
        &d,
        "platform_call",
        json!({"platform": "fixture", "account": "acct-1",
               "tool": "widgets.publish", "input": {"widget": "w1"},
               "agent": "w1"}),
    ));
    assert!(err.contains("identity") || err.contains("refused"), "{err}");

    // An agent caller sees only its own effects.
    agent
        .rpc(
            &d,
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.publish", "input": {"widget": "w1"},
                   "request": "req-own-1"}),
        )
        .unwrap();
    let mut other = Lane::spawn_as(&d, "w2", None, "worker");
    grant(&d, "w2", "fixture", "acct-1", &["widgets:read"]);
    let err = refused(other.rpc(&d, "platform_effects", json!({"agent": "w1"})));
    assert!(err.contains("own"), "{err}");
    let own = other.rpc(&d, "platform_effects", json!({})).unwrap();
    assert_eq!(own["effects"], json!([]), "w2 saw w1's effect");

    // And it cannot close another agent's waiting row.
    let err = refused(other.rpc(
        &d,
        "platform_effect_close",
        json!({"request": "req-own-1", "reason": "snoop"}),
    ));
    assert!(err.contains("refused") || err.contains("own"), "{err}");
    // The row is still waiting — the refused close wrote nothing.
    assert_eq!(effect_row(&d, "req-own-1").unwrap()["state"], "waiting");
}

/// The CAD-366 review flag CAD-506 owns: `agent_requests` was a
/// Rule::Read surface — any agent listed every agent's pending rows,
/// `input` included. Now a pending row discloses to the operator, the
/// owning agent, and the owner's PM (the reviewer the open notice
/// directs here); a peer or an unproven caller is refused.
#[test]
fn agent_requests_scoped_to_owner_pm_and_operator() {
    let mut platforms: HashMap<String, Arc<dyn PlatformAdapter>> = HashMap::new();
    platforms.insert("fixture".to_string(), Arc::new(FakePlatform::standard()));
    let d = Daemon::start(platforms);
    enroll(&d, "fixture", "acct-1", &["widgets:publish"]);
    let mut pm = Lane::spawn_as(&d, "pm1", None, "pm");
    let mut agent = Lane::spawn_as(
        &d,
        "w1",
        Some(r#"{"broker_approvals": true, "upstream": "pm1"}"#),
        "worker",
    );
    let mut peer = Lane::spawn_as(&d, "w2", None, "worker");
    grant(&d, "w1", "fixture", "acct-1", &["widgets:publish"]);

    // Two pending rows for w1: a brokered approval and a staged effect.
    agent
        .rpc(
            &d,
            "request_open",
            json!({"alias": "w1", "request": "req-brokered",
                   "tool": "bash", "input_summary": "a declared input"}),
        )
        .unwrap();
    agent
        .rpc(
            &d,
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.publish", "input": {"widget": "w1"},
                   "request": "req-staged"}),
        )
        .unwrap();

    // The owner sees both; the ward's PM sees both (it reviews for the
    // press); the operator sees both.
    for out in [
        agent
            .rpc(&d, "agent_requests", json!({"alias": "w1"}))
            .unwrap(),
        pm.rpc(&d, "agent_requests", json!({"alias": "w1"}))
            .unwrap(),
        d.op("agent_requests", json!({"alias": "w1"})).unwrap(),
    ] {
        let handles: Vec<&str> = out["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["request"].as_str())
            .collect();
        assert!(handles.contains(&"req-brokered"), "{handles:?}");
        assert!(handles.contains(&"req-staged"), "{handles:?}");
    }

    // A peer sees none — refused outright, not filtered to empty: a
    // pending row is the caller's business alone.
    let err = refused(peer.rpc(&d, "agent_requests", json!({"alias": "w1"})));
    assert!(err.contains("refused"), "{err}");
    // So is a detached, unproven caller.
    let frame = unprovable_rpc(&d, "agent_requests", json!({"alias": "w1"}));
    assert_eq!(frame["ok"], false, "an unproven caller read pending rows");
    // w2's own (empty) list still answers.
    let own = peer
        .rpc(&d, "agent_requests", json!({"alias": "w2"}))
        .unwrap();
    assert_eq!(own["requests"], json!([]));
}

/// An agent may retire its own waiting row; it may not retire a row in
/// `reconcile` — that state is exactly the ambiguity only a human
/// resolves (§5.4 step 8).
#[test]
fn agent_close_scoped_to_own_waiting_rows() {
    let mut platforms: HashMap<String, Arc<dyn PlatformAdapter>> = HashMap::new();
    platforms.insert("fixture".to_string(), Arc::new(FakePlatform::standard()));
    let d = Daemon::start(platforms);
    enroll(&d, "fixture", "acct-1", &["widgets:publish"]);
    let mut agent = Lane::spawn_as(&d, "w1", None, "worker");
    grant(&d, "w1", "fixture", "acct-1", &["widgets:publish"]);
    agent
        .rpc(
            &d,
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.publish", "input": {"widget": "w1"},
                   "request": "req-self-close"}),
        )
        .unwrap();
    let out = agent
        .rpc(
            &d,
            "platform_effect_close",
            json!({"request": "req-self-close", "reason": "changed my mind"}),
        )
        .unwrap();
    assert_eq!(out["state"], json!("closed"));
    assert_eq!(
        effect_row(&d, "req-self-close").unwrap()["close_reason"],
        "changed my mind"
    );
}

/// Two presses on one staged effect: exactly one lands, the platform
/// fires once — the atomic decide is the claim.
#[test]
fn concurrent_presses_execute_exactly_once() {
    let fake = Arc::new(FakePlatform::standard());
    let mut platforms: HashMap<String, Arc<dyn PlatformAdapter>> = HashMap::new();
    platforms.insert("fixture".to_string(), fake.clone());
    let d = Arc::new(Daemon::start(platforms));
    enroll(&d, "fixture", "acct-1", &["widgets:publish"]);
    let mut agent = Lane::spawn_as(&d, "w1", None, "worker");
    grant(&d, "w1", "fixture", "acct-1", &["widgets:publish"]);
    agent
        .rpc(
            &d,
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.publish", "input": {"widget": "w1"},
                   "request": "req-race"}),
        )
        .unwrap();

    // Twenty operator presses at once — the agent_respond calls are
    // detached the way operator_rpc already is; each is its own
    // process, so "at once" is honest.
    let handles: Vec<_> = (0..20)
        .map(|_| {
            let state = d.state.clone();
            thread::spawn(move || {
                let frame = op::operator_rpc(
                    &client::socket_path(&state),
                    "agent_respond",
                    json!({"alias": "w1", "request": "req-race",
                           "decision": "accept"}),
                );
                frame["ok"].as_bool() == Some(true)
            })
        })
        .collect();
    let applied = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|ok| *ok)
        .count();
    assert_eq!(applied, 1, "more than one press landed");
    assert_eq!(fake.executions_of("widgets.publish"), 1, "double fire");
    assert_eq!(effect_row(&d, "req-race").unwrap()["state"], "done");
}

/// A brokered request cannot take a staged effect's handle — one
/// handle = one request = one execution, across both registries.
#[test]
fn request_open_cannot_shadow_an_effect_handle() {
    let mut platforms: HashMap<String, Arc<dyn PlatformAdapter>> = HashMap::new();
    platforms.insert("fixture".to_string(), Arc::new(FakePlatform::standard()));
    let d = Daemon::start(platforms);
    enroll(&d, "fixture", "acct-1", &["widgets:publish"]);
    let mut agent = Lane::spawn_as(&d, "w1", Some(r#"{"broker_approvals": true}"#), "worker");
    grant(&d, "w1", "fixture", "acct-1", &["widgets:publish"]);
    agent
        .rpc(
            &d,
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.publish", "input": {"widget": "w1"},
                   "request": "req-shared"}),
        )
        .unwrap();
    let err = refused(agent.rpc(
        &d,
        "request_open",
        json!({"alias": "w1", "request": "req-shared",
               "tool": "bash", "input_summary": "x"}),
    ));
    assert!(err.contains("effect"), "{err}");
}

/// The grant checked at stage is re-checked inside Execute: revoke
/// between stage and press and the accept closes `grant_revoked`
/// instead of firing.
#[test]
fn grant_revoked_between_stage_and_press_cancels() {
    let fake = Arc::new(FakePlatform::standard());
    let mut platforms: HashMap<String, Arc<dyn PlatformAdapter>> = HashMap::new();
    platforms.insert("fixture".to_string(), fake.clone());
    let d = Daemon::start(platforms);
    enroll(&d, "fixture", "acct-1", &["widgets:publish"]);
    let mut agent = Lane::spawn_as(&d, "w1", None, "worker");
    grant(&d, "w1", "fixture", "acct-1", &["widgets:publish"]);
    agent
        .rpc(
            &d,
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.publish", "input": {"widget": "w1"},
                   "request": "req-revoke"}),
        )
        .unwrap();
    // Full revoke: the agent now holds nothing on the account. The
    // ungrant drain itself closes the waiting row — but even where a
    // row survives (a scope subset revoked), Execute's re-check is the
    // last line: stage a second effect after re-granting just the
    // read scope.
    d.op(
        "platform_ungrant",
        json!({"agent": "w1", "platform": "fixture", "account": "acct-1",
               "scopes": ["widgets:publish"]}),
    )
    .unwrap();
    assert_eq!(
        effect_row(&d, "req-revoke").unwrap()["state"],
        "closed",
        "the ungrant drain left the row waiting"
    );
    assert_eq!(fake.executions_of("widgets.publish"), 0);
}

/// I1: `agent_events` is a `Rule::Read` surface — a peer agent, or any
/// caller the socket cannot name, reads another agent's stream. So the
/// agent-lane `request_opened` for a staged send carries only the
/// handle the press follows and the routing fields — never the staged
/// input's text (the send's message body), and a read/draft's
/// `platform_called` event is the same. The full staged input lives in
/// the caller-scoped `platform_effects` record alone.
#[test]
fn agent_events_carry_no_input_derived_text() {
    let mut platforms: HashMap<String, Arc<dyn PlatformAdapter>> = HashMap::new();
    platforms.insert("fixture".to_string(), Arc::new(FakePlatform::standard()));
    let d = Daemon::start(platforms);
    enroll(
        &d,
        "fixture",
        "acct-1",
        &["widgets:read", "widgets:write", "widgets:publish"],
    );
    let mut agent = Lane::spawn_as(&d, "w1", None, "worker");
    let mut peer = Lane::spawn_as(&d, "w2", None, "worker");
    grant(
        &d,
        "w1",
        "fixture",
        "acct-1",
        &["widgets:read", "widgets:write", "widgets:publish"],
    );

    // A staged send whose body is the canary, then a draft call whose
    // input is another — both write agent-lane events.
    agent
        .rpc(
            &d,
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.publish",
                   "input": {"widget": "w1", "body": "SEND-CANARY-8f4e2a"},
                   "request": "req-quiet"}),
        )
        .unwrap();
    agent
        .rpc(
            &d,
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.preview",
                   "input": {"text": "DRAFT-CANARY-1b9c7d"}}),
        )
        .unwrap();

    // A peer reads w1's stream — admitted (agent_events stays
    // Rule::Read), but no input-derived text may be on it.
    let stream = peer
        .rpc(&d, "agent_events", json!({"alias": "w1"}))
        .unwrap();
    let text = stream.to_string();
    assert!(
        !text.contains("SEND-CANARY-8f4e2a"),
        "send body leaked: {text}"
    );
    assert!(
        !text.contains("DRAFT-CANARY-1b9c7d"),
        "draft input leaked: {text}"
    );
    // The open event still names the handle and routing fields — the
    // press flow is intact — but no input-bearing key survives.
    let opened = stream["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "request_opened" && e["payload"]["request"] == "req-quiet")
        .unwrap_or_else(|| panic!("no request_opened for req-quiet: {stream}"));
    let payload = &opened["payload"];
    assert_eq!(payload["kind"], "effect");
    assert_eq!(payload["tool"], "widgets.publish");
    assert_eq!(payload["platform"], "fixture");
    assert_eq!(payload["account"], "acct-1");
    for field in ["input_summary", "input", "preview"] {
        assert!(
            payload.get(field).is_none(),
            "agent-lane event carries {field}: {payload}"
        );
    }
    let called = stream["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "platform_called")
        .expect("no platform_called event");
    assert!(
        called["payload"].get("input_summary").is_none(),
        "platform_called carries input_summary: {}",
        called["payload"]
    );

    // An unproven caller reads the same stream: same absence.
    let frame = unprovable_rpc(&d, "agent_events", json!({"alias": "w1"}));
    assert_eq!(frame["ok"], true, "unproven read should be admitted");
    let text = frame.to_string();
    assert!(
        !text.contains("SEND-CANARY-8f4e2a"),
        "send body leaked: {text}"
    );
    assert!(
        !text.contains("DRAFT-CANARY-1b9c7d"),
        "draft input leaked: {text}"
    );

    // The owner itself still reads the full record — only the event
    // lane is stripped.
    let own = agent.rpc(&d, "platform_effects", json!({})).unwrap();
    let rec = own["effects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["request"] == "req-quiet")
        .unwrap();
    assert!(rec["input_summary"]
        .as_str()
        .unwrap()
        .contains("SEND-CANARY-8f4e2a"));
}

/// I2: `String::truncate` panics mid-char and every byte cap here sits
/// on caller-controlled UTF-8 (`tool`, `input`). A multibyte char
/// straddling each cap — the record's 1024 summary, the daemon's
/// 16384 preview, the fixture's own 512 preview — must cap at the
/// boundary, never panic the connection thread.
#[test]
fn utf8_straddling_every_text_cap_cannot_panic() {
    let fake = Arc::new(FakePlatform::standard());
    let mut platforms: HashMap<String, Arc<dyn PlatformAdapter>> = HashMap::new();
    platforms.insert("fixture".to_string(), fake.clone());
    let d = Daemon::start(platforms);
    enroll(&d, "fixture", "acct-1", &["widgets:publish"]);
    let mut agent = Lane::spawn_as(&d, "w1", None, "worker");
    grant(&d, "w1", "fixture", "acct-1", &["widgets:publish"]);

    // input_summary cap (1024): the summary is
    // "widgets.publish body=<v> widget=w1"; the 21-byte prefix puts a
    // "€" (3 bytes) at byte 1023 — dead centre of the cap.
    let body = format!("{}€{}", "x".repeat(1002), "y".repeat(20));
    let out = agent
        .rpc(
            &d,
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.publish",
                   "input": {"body": body, "widget": "w1"},
                   "request": "req-cap-summary"}),
        )
        .unwrap_or_else(|e| panic!("summary cap panicked the call: {e}"));
    assert_eq!(out["result"], "staged");
    let rec = live_record(&d, "req-cap-summary");
    let summary = rec["input_summary"].as_str().unwrap();
    assert!(summary.len() <= 1024, "summary over cap: {}", summary.len());
    assert!(summary.is_char_boundary(summary.len()));

    // The fixture's own preview cap (512): widgets.publish renders
    // `publish {widget} to fixture/acct-1` — an 8-byte head and an
    // 18-byte tail, so a widget name of 503 x's then "€" straddles it.
    let widget = format!("{}€{}", "x".repeat(503), "y".repeat(10));
    agent
        .rpc(
            &d,
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.publish",
                   "input": {"widget": widget},
                   "request": "req-cap-fixture"}),
        )
        .unwrap_or_else(|e| panic!("fixture preview cap panicked the call: {e}"));

    // The daemon's preview cap (16384) sits above the fixture's own —
    // an adapter whose preview ignores its own bound still cannot
    // panic the gate. `LoudPreview` renders past the cap with a "€"
    // straddling it.
    struct LoudPreview(FakePlatform, String);
    impl PlatformAdapter for LoudPreview {
        fn table(&self) -> &ToolTable {
            self.0.table()
        }
        fn reported_manifest_version(&self) -> Option<String> {
            self.0.reported_manifest_version()
        }
        fn preview(&self, _account: &str, _tool: &str, _input: &Value) -> String {
            self.1.clone()
        }
        fn execute(
            &self,
            credential: &[u8],
            tool: &str,
            input: &Value,
            idempotency_key: &str,
            expected_hash: Option<&str>,
        ) -> std::result::Result<Value, String> {
            PlatformAdapter::execute(
                &self.0,
                credential,
                tool,
                input,
                idempotency_key,
                expected_hash,
            )
        }
        fn read_back(&self, tool: &str, input: &Value) -> Verified {
            self.0.read_back(tool, input)
        }
        fn source_hash(&self, _agent: &str, source: &str) -> Option<String> {
            self.0.source_hash(source)
        }
    }
    // 16383 p's, then "€" — its lead byte at 16383 means the 16384th
    // byte is mid-char.
    let loud = LoudPreview(
        FakePlatform::standard(),
        format!("{}€{}", "p".repeat(16383), "q".repeat(20)),
    );
    let mut platforms: HashMap<String, Arc<dyn PlatformAdapter>> = HashMap::new();
    platforms.insert("fixture".to_string(), Arc::new(loud));
    let d2 = Daemon::start(platforms);
    enroll(&d2, "fixture", "acct-1", &["widgets:publish"]);
    let mut agent2 = Lane::spawn_as(&d2, "w1", None, "worker");
    grant(&d2, "w1", "fixture", "acct-1", &["widgets:publish"]);
    agent2
        .rpc(
            &d2,
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.publish", "input": {"widget": "w1"},
                   "request": "req-cap-preview"}),
        )
        .unwrap_or_else(|e| panic!("preview cap panicked the call: {e}"));
    let rec = live_record(&d2, "req-cap-preview");
    let preview = rec["preview"].as_str().unwrap();
    assert!(
        preview.len() <= 16384,
        "preview over cap: {}",
        preview.len()
    );

    // Both daemons outlived every straddle.
    d.op("health", json!({})).unwrap();
    d2.op("health", json!({})).unwrap();
}

/// N1: the platform write runs under the staged row's `effect_id` as
/// its idempotency key (C9) — a key derived any other way lets a
/// retried press fire twice against the platform's dedupe.
#[test]
fn send_executes_under_its_effect_id_key() {
    let fake = Arc::new(FakePlatform::standard());
    let mut platforms: HashMap<String, Arc<dyn PlatformAdapter>> = HashMap::new();
    platforms.insert("fixture".to_string(), fake.clone());
    let d = Daemon::start(platforms);
    enroll(&d, "fixture", "acct-1", &["widgets:publish"]);
    let mut agent = Lane::spawn_as(&d, "w1", None, "worker");
    grant(&d, "w1", "fixture", "acct-1", &["widgets:publish"]);
    agent
        .rpc(
            &d,
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.publish", "input": {"widget": "w1"},
                   "request": "req-key"}),
        )
        .unwrap();
    d.op(
        "agent_respond",
        json!({"alias": "w1", "request": "req-key", "decision": "accept"}),
    )
    .unwrap();
    let row = effect_row(&d, "req-key").unwrap();
    let executions = fake.executions();
    assert_eq!(executions.len(), 1, "{executions:?}");
    assert_eq!(
        executions[0].idempotency_key,
        row["effect_id"].as_str().unwrap(),
        "the platform write must carry the staged effect_id as its key"
    );
}

/// N1: one handle = one call. A same-handle retry with identical
/// input dedupes to the staged row; the same handle carrying a
/// different call is a conflict — refused, never a disguised second
/// effect squatting the first's press.
#[test]
fn same_handle_different_input_is_refused() {
    let mut platforms: HashMap<String, Arc<dyn PlatformAdapter>> = HashMap::new();
    platforms.insert("fixture".to_string(), Arc::new(FakePlatform::standard()));
    let d = Daemon::start(platforms);
    enroll(&d, "fixture", "acct-1", &["widgets:publish"]);
    let mut agent = Lane::spawn_as(&d, "w1", None, "worker");
    grant(&d, "w1", "fixture", "acct-1", &["widgets:publish"]);
    let first = agent
        .rpc(
            &d,
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.publish", "input": {"widget": "a"},
                   "request": "req-dup"}),
        )
        .unwrap();
    assert_eq!(first["result"], "staged");
    // A byte-identical retry dedupes — no second row, no second event.
    let retry = agent
        .rpc(
            &d,
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.publish", "input": {"widget": "a"},
                   "request": "req-dup"}),
        )
        .unwrap();
    assert_eq!(retry["result"], "existing");
    assert_eq!(retry["effect_id"], first["effect_id"]);
    // The same handle on a different input is refused outright.
    let err = refused(agent.rpc(
        &d,
        "platform_call",
        json!({"platform": "fixture", "account": "acct-1",
               "tool": "widgets.publish", "input": {"widget": "b"},
               "request": "req-dup"}),
    ));
    assert!(err.contains("different"), "{err}");
    assert_eq!(effect_row(&d, "req-dup").unwrap()["state"], "waiting");
}

/// N2: `decision.reason` and `close_reason` are durable free text —
/// bounded at 1 KiB on a char boundary, and screened against the
/// enrolled credential like every other value that lands.
#[test]
fn reasons_are_bounded_and_never_carry_the_credential() {
    let mut platforms: HashMap<String, Arc<dyn PlatformAdapter>> = HashMap::new();
    platforms.insert("fixture".to_string(), Arc::new(FakePlatform::standard()));
    let d = Daemon::start(platforms);
    enroll(&d, "fixture", "acct-1", &["widgets:publish"]);
    let mut agent = Lane::spawn_as(&d, "w1", None, "worker");
    grant(&d, "w1", "fixture", "acct-1", &["widgets:publish"]);

    // A reason carrying the enrolled token is withheld — the press
    // refuses rather than durably record the secret.
    agent
        .rpc(
            &d,
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.publish", "input": {"widget": "w1"},
                   "request": "req-reason-leak"}),
        )
        .unwrap();
    let err = d
        .op(
            "agent_respond",
            json!({"alias": "w1", "request": "req-reason-leak",
                   "decision": "decline", "reason": format!("no: {TOKEN}")}),
        )
        .expect_err("a reason carrying the credential was admitted");
    assert!(err.to_string().contains("credential"), "{err}");
    assert_eq!(
        effect_row(&d, "req-reason-leak").unwrap()["state"],
        "waiting",
        "a refused press must leave the row waiting"
    );

    // A >1KiB reason with a multibyte char on the cut lands bounded —
    // and still records the press.
    let long = format!("{}€{}", "r".repeat(1022), "z".repeat(30));
    let out = d
        .op(
            "agent_respond",
            json!({"alias": "w1", "request": "req-reason-leak",
                   "decision": "decline", "reason": long}),
        )
        .unwrap();
    assert_eq!(out["state"], "answered");
    let reason = effect_row(&d, "req-reason-leak").unwrap()["decision"]["reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(reason.len() <= 1024, "reason over cap: {}", reason.len());
    assert!(!reason.contains('€'), "cap cut mid-char: {reason}");

    // close_reason is the same shape: screened, then bounded.
    agent
        .rpc(
            &d,
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.publish", "input": {"widget": "w2"},
                   "request": "req-close-leak"}),
        )
        .unwrap();
    let err = d
        .op(
            "platform_effect_close",
            json!({"request": "req-close-leak",
                   "reason": format!("stop: {TOKEN}")}),
        )
        .expect_err("a close reason carrying the credential was admitted");
    assert!(err.to_string().contains("credential"), "{err}");
    assert_eq!(
        effect_row(&d, "req-close-leak").unwrap()["state"],
        "waiting"
    );
    let long = format!("{}€{}", "c".repeat(1022), "z".repeat(30));
    agent
        .rpc(
            &d,
            "platform_effect_close",
            json!({"request": "req-close-leak", "reason": long}),
        )
        .unwrap();
    let reason = effect_row(&d, "req-close-leak").unwrap()["close_reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        reason.len() <= 1024,
        "close reason over cap: {}",
        reason.len()
    );
    assert!(!reason.contains('€'), "cap cut mid-char: {reason}");
}

/// N3: a credential that vanishes between stage and press closes the
/// row `credential_revoked` — and the requester's PM is told, exactly
/// like `source_changed`/`grant_revoked`.
#[test]
fn credential_loss_at_execute_delivers_the_outcome() {
    let fake = Arc::new(FakePlatform::standard());
    let mut platforms: HashMap<String, Arc<dyn PlatformAdapter>> = HashMap::new();
    platforms.insert("fixture".to_string(), fake.clone());
    let d = Daemon::start(platforms);
    enroll(&d, "fixture", "acct-1", &["widgets:publish"]);
    // The PM registers first so effective_pm resolves at stage.
    let _pm = Lane::spawn_as(&d, "pm1", None, "pm");
    let mut agent = Lane::spawn_as(&d, "w1", Some(r#"{"upstream": "pm1"}"#), "worker");
    grant(&d, "w1", "fixture", "acct-1", &["widgets:publish"]);
    agent
        .rpc(
            &d,
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.publish", "input": {"widget": "w1"},
                   "request": "req-cred"}),
        )
        .unwrap();
    // Custody loses the bytes while the record stays — the outlived
    // store a rotate failure or a partial revoke leaves behind.
    let custody_dir = d.state.join("custody");
    let cred = std::fs::read_dir(&custody_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().and_then(|e| e.to_str()) == Some("cred"))
        .expect("no custody file was written at enroll");
    std::fs::write(&cred, b"rotated-away").unwrap();

    d.op(
        "agent_respond",
        json!({"alias": "w1", "request": "req-cred", "decision": "accept"}),
    )
    .unwrap();
    let row = effect_row(&d, "req-cred").unwrap();
    assert_eq!(row["state"], "closed");
    assert_eq!(row["close_reason"], "credential_revoked");
    assert_eq!(fake.executions_of("widgets.publish"), 0, "still fired");
    // The outcome lands on the PM's lane like the other mid-execute
    // closes — deduped on the effect id.
    let effect_id = row["effect_id"].as_str().unwrap();
    let (lane, body) =
        outcome_message(&d, effect_id).expect("credential_revoked delivered no outcome message");
    assert_eq!(lane, "pm1");
    assert!(body.contains("credential_revoked"), "{body}");
    assert!(body.contains(effect_id), "{body}");
}

/// No credential byte ever lands in the store, the events, the
/// outcome message or the record — the whole lifecycle is hunted.
#[test]
fn no_credential_in_any_row_event_or_message() {
    let fake = Arc::new(FakePlatform::standard());
    let mut platforms: HashMap<String, Arc<dyn PlatformAdapter>> = HashMap::new();
    platforms.insert("fixture".to_string(), fake.clone());
    let d = Daemon::start(platforms);
    enroll(&d, "fixture", "acct-1", &["widgets:publish"]);
    let mut agent = Lane::spawn_as(&d, "w1", None, "worker");
    grant(&d, "w1", "fixture", "acct-1", &["widgets:publish"]);
    agent
        .rpc(
            &d,
            "platform_call",
            json!({"platform": "fixture", "account": "acct-1",
                   "tool": "widgets.publish", "input": {"widget": "w1"},
                   "request": "req-leak"}),
        )
        .unwrap();
    d.op(
        "agent_respond",
        json!({"alias": "w1", "request": "req-leak", "decision": "accept"}),
    )
    .unwrap();
    let dump = db_snapshot(&d);
    assert!(!dump.contains(TOKEN), "store leaked the credential");
    let effects = d.op("platform_effects", json!({})).unwrap();
    assert!(!effects.to_string().contains(TOKEN), "record leaked");
}
