//! CAD-366 / ADR 0006 §5.1, §5.3, §5.5: platform custody and grants —
//! the adversarial proof. An agent caller cannot enroll, grant, revoke
//! or set defaults; no surface (result, account row, grant, event,
//! error) ever carries credential bytes; and the grant check refuses
//! an out-of-grant call naming the missing scope before any platform
//! traffic. The operator path enrolls both exchange shapes, keeps
//! several accounts per platform, rotates without touching grants, and
//! revokes with the pending-effects drain §5.3 requires.

// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use cadence_agent::{client, daemon, proto};
use serde_json::{json, Value};
use tempfile::TempDir;

/// The credential bytes every "no leak" assertion hunts for — built
/// from parts so no source literal forms a keyword + separator +
/// value shape a secret scanner could match (generic-api-key reads
/// `token: "<10+ chars>"`; the parts alone match nothing).
const TOKEN: &str = concat!("cadp_scoped_testt", "oken_a1b2c3d4e5f6");
const TOKEN2: &str = concat!("cadp_scoped_rotatedt", "oken_9z8y7x6w5v");

// ---------- harness ----------

/// In-process daemon on a caller-owned state dir, with `pm` as the
/// tracker dir (`CADENCE_PM_DIR` reaches the daemon through
/// `provider_env`, so `platform_default_set` resolves projects).
struct Daemon {
    state: PathBuf,
    pm: PathBuf,
    _dir: TempDir,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Daemon {
    fn start() -> Self {
        let dir = TempDir::new().unwrap();
        let state = dir.path().join("state");
        let pm = dir.path().join("pm");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&pm).unwrap();
        let env = cadence_agent::adapter::ProviderEnv::default();
        env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let opts = daemon::ServeOptions {
            provider_env: env,
            report_router: Some(0),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            slots: Some(cadence_agent::slots::SlotConfig::default()),
            agent_gc: Some(daemon::AgentGcSetting::default()),
            stop: Some(stop.clone()),
            ..Default::default()
        };
        let owned = state.clone();
        let handle = thread::spawn(move || {
            let _ = daemon::serve_with(&owned, opts);
        });
        let d = Self {
            state,
            pm,
            _dir: dir,
            stop,
            handle: Some(handle),
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        while client::rpc(&d.state, "health", json!({})).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become healthy");
            thread::sleep(Duration::from_millis(50));
        }
        d
    }

    /// A direct socket call — the caller is whatever this test process
    /// looks like to the daemon (the operator in CI, an agent when the
    /// suite runs in a pane). Fixture setup never relies on it.
    fn rpc(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        client::rpc(&self.state, method, params)
    }

    /// The operator-shaped call — `tests/integration.rs`'s
    /// `operator_rpc` shape: `setsid -f` off this process's ancestry,
    /// env cleared, stdio not a pane. `peer::operator_proof`'s accepted
    /// residual, so the gate itself is never touched.
    fn op(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let frame = op::operator_rpc(&client::socket_path(&self.state), method, params);
        proto::unwrap(frame)
    }

    /// Every `audit:platforms` event, in order.
    fn audit(&self) -> Vec<Value> {
        let conn = rusqlite::Connection::open_with_flags(
            self.state.join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT kind, payload FROM events WHERE alias='audit:platforms' ORDER BY rowid",
            )
            .unwrap();
        stmt.query_map([], |r| {
            Ok(json!({"kind": r.get::<_, String>(0)?,
                      "payload": serde_json::from_str::<Value>(&r.get::<_, String>(1)?)
                          .unwrap_or(Value::Null)}))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
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

/// `/proc/<pid>/stat` field 22 — what the daemon records as a pid's
/// `pid_start`, so a planted row names exactly that process.
fn proc_start(pid: u32) -> Option<i64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

/// A long-lived `bash` planted as an agent's pane: commands written to
/// its stdin run as its children, so their socket-peer identity derives
/// the alias — `tests/integration.rs`'s `LaneShell`.
struct Lane {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    dir: TempDir,
    seq: u64,
}

impl Lane {
    /// Plant a bash as `alias`'s pane. `params` is the agent's launch
    /// params JSON string (e.g. `{"broker_approvals": true}`).
    fn spawn_as(d: &Daemon, alias: &str, params: Option<&str>) -> Lane {
        let mut child = Command::new("bash")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let lane = Lane {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            dir: TempDir::new().unwrap(),
            seq: 0,
        };
        let mut req = json!({"alias": alias, "provider": "inbox",
                             "endpoint_kind": "inbox",
                             "cwd": d.state.to_str().unwrap()});
        if let Some(p) = params {
            req["params"] = json!(p);
        }
        d.op("agent_register", req)
            .unwrap_or_else(|e| panic!("register {alias}: {e}"));
        // The row stays inert like integration.rs's plant_pane: a pty
        // pane naming this bash, disabled so no actor spawn follows it.
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "UPDATE agents SET endpoint_kind='pty', pid=?1, pid_start=?3, \
                enabled=0, generation='planted', session_id='planted' WHERE alias=?2",
            rusqlite::params![lane.pid() as i64, alias, proc_start(lane.pid())],
        )
        .unwrap();
        lane
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Run a bash fragment under this lane; answer (exit code, output).
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

    /// One RPC under this lane's agent identity — answers the
    /// unwrapped result or error.
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

    /// `cadence <args>` under this lane — stdout+stderr and rc.
    fn cadence(&mut self, d: &Daemon, args: &str) -> (i64, String) {
        self.run(&format!(
            "{} --state-dir {} {args}",
            env!("CARGO_BIN_EXE_cadence"),
            d.state.display()
        ))
    }
}

impl Drop for Lane {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One raw RPC from a process tied to no pane and not provably the
/// operator: detached like the operator call but carrying a
/// `CADENCE_ALIAS` its ancestry cannot prove. Answers the wire frame.
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

fn frame_err(frame: &Value) -> String {
    frame["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// A refused `Result` — message text only.
fn refused(r: cadence_agent::Result<Value>) -> String {
    match r {
        Err(e) => e.to_string(),
        Ok(v) => panic!("call was admitted: {v}"),
    }
}

/// Every row of every table — the proof a refusal wrote nothing and
/// that no byte field holds a credential (`tests/integration.rs`'s
/// `db_snapshot`). Values render as their real content — a TEXT cell
/// as its string, a BLOB as its bytes decoded — so `contains(TOKEN)`
/// genuinely hunts; the old `{:?}` render printed `Text([..])` and a
/// stored credential could never match.
fn db_snapshot(d: &Daemon) -> String {
    use rusqlite::types::ValueRef;
    let conn = rusqlite::Connection::open_with_flags(
        d.state.join("cadence.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
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
        let mut rows: Vec<String> = stmt
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
        rows.sort();
        out.push_str(&format!("## {table}\n{}\n", rows.join("\n")));
    }
    out
}

/// One table's rows inside a snapshot — the audit stream's event
/// payloads name handles a table check must not confuse for rows.
fn table_section<'a>(dump: &'a str, table: &str) -> &'a str {
    let Some(start) = dump.find(&format!("## {table}\n")) else {
        return "";
    };
    let body = &dump[start + table.len() + 3..];
    match body.find("\n## ") {
        Some(end) => &body[..end],
        None => body,
    }
}

/// Seed `<pm>/<key>/project.yaml` so `platform_default_set`'s project
/// resolution finds it.
fn seed_project(d: &Daemon, key: &str) {
    let dir = d.pm.join(key);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("project.yaml"),
        format!("key: {key}\nprefix: {key}\n"),
    )
    .unwrap();
}

/// Operator-enroll `platform`/`account` at `scopes`; answers the result.
/// The daemon reports no custody isolation mode (today none exists —
/// ADR 0006 P4), so a first enroll needs the operator's recorded
/// `accept_same_uid_risk`; the gate itself is tested separately.
fn enroll(d: &Daemon, platform: &str, account: &str, scopes: &[&str], token: &str) -> Value {
    d.op(
        "platform_enroll",
        json!({"accept_same_uid_risk": true, "platform": platform,
               "account": account, "scopes": scopes, "shape": "token",
               "token": token}),
    )
    .unwrap_or_else(|e| panic!("enroll {platform}/{account}: {e}"))
}

/// Operator-grant `agent` `scopes` on `platform`/`account`.
fn grant(d: &Daemon, agent: &str, platform: &str, account: &str, scopes: &[&str]) -> Value {
    d.op(
        "platform_grant",
        json!({"agent": agent, "platform": platform,
               "account": account, "scopes": scopes}),
    )
    .unwrap_or_else(|e| panic!("grant {agent} {platform}/{account}: {e}"))
}

#[path = "support/operator.rs"]
mod op;

// ---------- §5.3: the operator gate on every custody mutation ----------

#[test]
fn custody_mutations_are_operator_only() {
    let d = Daemon::start();
    let mut agent = Lane::spawn_as(&d, "w1", None);
    let before = db_snapshot(&d);

    // Every mutating verb, attempted by a derived agent caller.
    for (method, params) in [
        (
            "platform_enroll",
            json!({"platform": "github", "account": "acme",
                   "scopes": ["repo:read"], "token": TOKEN}),
        ),
        (
            "platform_rotate",
            json!({"platform": "github", "account": "acme", "token": TOKEN2}),
        ),
        (
            "platform_revoke",
            json!({"platform": "github", "account": "acme"}),
        ),
        (
            "platform_grant",
            json!({"agent": "w1", "platform": "github", "account": "acme",
                   "scopes": ["repo:read"]}),
        ),
        (
            "platform_ungrant",
            json!({"agent": "w1", "platform": "github", "account": "acme"}),
        ),
        (
            "platform_default_set",
            json!({"project": "p1", "platform": "github", "account": "acme"}),
        ),
    ] {
        let err = refused(agent.rpc(&d, method, params.clone()));
        assert!(
            err.contains("operator") || err.contains("caller rule"),
            "{method} from an agent was not an operator refusal: {err}"
        );
        // The same call from a detached process carrying a forged alias.
        let frame = unprovable_rpc(&d, method, params);
        assert_eq!(frame["ok"], false, "{method} admitted an unproven caller");
        let err = frame_err(&frame);
        assert!(
            err.contains("operator") || err.contains("caller rule"),
            "{method} from an unproven caller: {err}"
        );
    }

    // A forged identity field does not launder an agent call: `by`
    // claims are refused outright by `operator_connection`.
    let err = refused(agent.rpc(
        &d,
        "platform_enroll",
        json!({"platform": "github", "account": "acme",
               "scopes": ["repo:read"], "token": TOKEN, "by": "operator"}),
    ));
    assert!(
        err.contains("operator") || err.contains("not accepted"),
        "forged `by`: {err}"
    );

    assert_eq!(before, db_snapshot(&d), "a refused mutation wrote");
}

// ---------- §5.3: enrollment, custody, and the no-bytes rule ----------

#[test]
fn enroll_token_writes_handles_only_and_0600_custody() {
    let d = Daemon::start();
    let result = enroll(&d, "github", "acme", &["repo:read", "issues:write"], TOKEN);
    let account = &result["account"];
    assert_eq!(account["platform"], "github");
    assert_eq!(account["account"], "acme");
    assert_eq!(account["scopes"], json!(["issues:write", "repo:read"]));
    assert_eq!(account["exchange"], "token");
    assert_eq!(account["by"], "operator");
    // The fingerprint is a prefix guard — present, never the token.
    let fp = account["fingerprint"].as_str().unwrap();
    assert!(!fp.is_empty() && fp.len() < TOKEN.len() + 8);
    assert!(!result.to_string().contains(TOKEN));

    // `platform_accounts` renders the same handle-only record.
    let accounts = d.rpc("platform_accounts", json!({})).unwrap();
    let text = accounts.to_string();
    assert!(text.contains("acme") && text.contains(fp));
    assert!(!text.contains(TOKEN), "accounts listing leaked: {text}");

    // Custody holds the bytes in a daemon-owned file: `<state>/custody`,
    // directory 0700, file 0600 — outside every agent's read set (the
    // master's confinement names only `master/` and `briefings/` under
    // the state dir).
    use std::os::unix::fs::MetadataExt;
    let custody = d.state.join("custody");
    assert_eq!(custody.metadata().unwrap().mode() & 0o777, 0o700);
    let creds: Vec<_> = std::fs::read_dir(&custody)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".cred"))
        .collect();
    assert_eq!(creds.len(), 1, "one credential file per enrollment");
    let cred = creds[0].path();
    assert_eq!(cred.metadata().unwrap().mode() & 0o777, 0o600);
    assert_eq!(std::fs::read_to_string(&cred).unwrap(), TOKEN);
    // The file name is a hash — no platform/account handle in it.
    assert!(!cred.file_name().unwrap().to_string_lossy().contains("acme"));

    // Nothing durable holds the bytes: the whole DB dump is clean.
    let dump = db_snapshot(&d);
    assert!(!dump.contains(TOKEN), "db holds the credential: {dump}");

    // Prove the hunt is real, not vacuous: a token planted in a TEXT
    // cell must surface in the dump — then the probe table goes and
    // the clean assert re-runs against the same state.
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute_batch("CREATE TABLE leak_probe(t TEXT)")
        .unwrap();
    conn.execute(
        "INSERT INTO leak_probe(t) VALUES(?1)",
        rusqlite::params![TOKEN],
    )
    .unwrap();
    assert!(
        db_snapshot(&d).contains(TOKEN),
        "the snapshot could not see a TEXT-column leak — the check is vacuous"
    );
    conn.execute_batch("DROP TABLE leak_probe").unwrap();
    drop(conn);
    assert!(!db_snapshot(&d).contains(TOKEN));
}

#[test]
fn personal_and_misdeclared_tokens_are_not_enrollable() {
    let d = Daemon::start();
    let before = db_snapshot(&d);
    // Every user-bound shape a platform mints is refused — and the
    // refusal must not echo the token. `accept_same_uid_risk` gets the
    // call past the custody gate so the screen itself is exercised.
    // The shapes are assembled from parts: a secret scanner reads
    // source literals, and a keyword + separator + long value must
    // never appear whole.
    for (what, token) in [
        ("GitHub classic PAT", concat!("ghp_", "1a2b3c4d5e6f")),
        (
            "GitHub fine-grained PAT",
            concat!("github_", "pat_1a2b3c4d5e6f"),
        ),
        ("GitHub user-to-server", concat!("ghu_", "1a2b3c4d5e6f")),
        ("GitHub OAuth shape", concat!("gho_", "1a2b3c4d5e6f")),
        ("GitHub refresh shape", concat!("ghr_", "1a2b3c4d5e6f")),
        ("GitLab PAT", concat!("glpat", "-1a2b3c4d5e6f")),
        // Whitespace padding does not launder a personal shape —
        // the screen trims before it classifies.
        ("padded PAT", concat!("  ghp_", "1a2b3c4d5e6f\n")),
        ("tabbed PAT", concat!("\tghu_", "1a2b3c4d5e6f ")),
    ] {
        let params = json!({"accept_same_uid_risk": true,
                            "platform": "github", "account": "acme",
                            "scopes": ["repo:read"], "token": token});
        let err = refused(d.op("platform_enroll", params));
        assert!(
            err.contains("personal") || err.contains("not enrollable"),
            "{what}: {err}"
        );
        assert!(
            !err.contains("1a2b3c4d5e6f"),
            "{what}: the refusal echoes the token"
        );
    }
    // A declared personal class refuses whatever the token's shape.
    let err = refused(d.op(
        "platform_enroll",
        json!({"accept_same_uid_risk": true, "class": "personal",
               "platform": "github", "account": "acme",
               "scopes": ["repo:read"], "token": TOKEN}),
    ));
    assert!(
        err.contains("personal") || err.contains("not enrollable"),
        "{err}"
    );
    assert!(!err.contains(TOKEN));
    // A token that is all whitespace — nothing to enroll.
    let err = refused(d.op(
        "platform_enroll",
        json!({"accept_same_uid_risk": true, "platform": "github",
               "account": "acme", "scopes": ["repo:read"],
               "token": "   \n\t "}),
    ));
    assert!(err.contains("token"), "blank token: {err}");
    // A token with whitespace inside — a credential never has one.
    let err = refused(d.op(
        "platform_enroll",
        json!({"accept_same_uid_risk": true, "platform": "github",
               "account": "acme", "scopes": ["repo:read"],
               "token": concat!("cadp broken", " token")}),
    ));
    assert!(err.contains("whitespace"), "inner whitespace: {err}");
    assert_eq!(before, db_snapshot(&d), "a refused enroll wrote");

    // An app-bound credential is not a personal token — a GitHub App
    // installation credential (`ghs_`) enrolls under its declared
    // scopes; adapter verification of scope adequacy is CAD-367's.
    let ok = d
        .op(
            "platform_enroll",
            json!({"accept_same_uid_risk": true, "platform": "github",
                   "account": "acme", "scopes": ["repo:read"],
                   "token": concat!("ghs_installationt", "oken9z8y")}),
        )
        .unwrap();
    assert_eq!(ok["state"], "enrolled");
    // Whitespace around an admissible token is trimmed, not enrolled.
    let _ = d
        .op(
            "platform_revoke",
            json!({"platform": "github", "account": "acme"}),
        )
        .unwrap();
    enroll(
        &d,
        "github",
        "acme",
        &["repo:read"],
        concat!("  cadp_scoped_pad", "ded  \n"),
    );
    let cred = std::fs::read_dir(d.state.join("custody"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "cred"))
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(cred).unwrap(),
        "cadp_scoped_padded",
        "custody must hold the trimmed credential"
    );
}

#[test]
fn consent_exchange_is_the_adapter_seam() {
    let d = Daemon::start();
    // `accept_same_uid_risk` carries the calls past the custody gate
    // so the adapter seam itself is exercised. No consent adapter is
    // registered yet (CAD-501 lands the first) — the refusal names
    // that seam instead of admitting a token.
    let err = refused(d.op(
        "platform_enroll",
        json!({"platform": "agenticos", "account": "acme",
               "scopes": ["work:read"], "shape": "consent",
               "accept_same_uid_risk": true}),
    ));
    assert!(
        err.contains("consent") && err.contains("adapter"),
        "consent without adapter: {err}"
    );
    // A `token` never rides a consent exchange — the platform issues
    // the credential to the daemon directly.
    let err = refused(d.op(
        "platform_enroll",
        json!({"platform": "agenticos", "account": "acme",
               "scopes": ["work:read"], "shape": "consent", "token": TOKEN,
               "accept_same_uid_risk": true}),
    ));
    assert!(err.contains("token"), "consent+token: {err}");
    assert!(!err.contains(TOKEN), "consent refusal echoes the token");
    // And an unknown shape is refused by name.
    let err = refused(d.op(
        "platform_enroll",
        json!({"platform": "github", "account": "acme",
               "scopes": ["repo:read"], "shape": "cookie-import", "token": TOKEN,
               "accept_same_uid_risk": true}),
    ));
    assert!(err.contains("cookie-import"), "unknown shape: {err}");
    assert!(!err.contains(TOKEN));
}

// ---------- §5.3/§5.1: grants, several accounts, the project default ----------

#[test]
fn grants_bind_an_agent_to_its_own() {
    let d = Daemon::start();
    let mut w1 = Lane::spawn_as(&d, "w1", None);
    // w2's lane exists only to plant the pane row — every call about
    // it comes from w1 or the operator.
    let _w2 = Lane::spawn_as(&d, "w2", None);
    enroll(&d, "github", "acme", &["repo:read"], TOKEN);
    enroll(
        &d,
        "github",
        "acme-bot",
        &["repo:read", "repo:write"],
        TOKEN2,
    );
    grant(&d, "w1", "github", "acme", &["repo:read"]);
    grant(&d, "w2", "github", "acme-bot", &["repo:read", "repo:write"]);

    // An agent reads its own grants — and only its own.
    let grants = w1.rpc(&d, "platform_grants", json!({})).unwrap()["grants"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0]["agent"], "w1");
    assert_eq!(grants[0]["account"], "acme");
    // Naming itself is fine; naming a peer is refused.
    let ok = w1
        .rpc(&d, "platform_grants", json!({"agent": "w1"}))
        .unwrap();
    assert_eq!(ok["grants"].as_array().unwrap().len(), 1);
    let err = refused(w1.rpc(&d, "platform_grants", json!({"agent": "w2"})));
    assert!(err.contains("own grants"), "cross-agent read: {err}");
    // The operator sees all, or one agent's.
    let all = d.op("platform_grants", json!({})).unwrap();
    assert_eq!(all["grants"].as_array().unwrap().len(), 2);
    let just_w2 = d.op("platform_grants", json!({"agent": "w2"})).unwrap();
    assert_eq!(just_w2["grants"][0]["agent"], "w2");

    // The check: granted scope admits; anything else refuses naming
    // the missing scope — before custody or platform is touched.
    let ok = w1
        .rpc(
            &d,
            "platform_check",
            json!({"platform": "github", "account": "acme", "scope": "repo:read"}),
        )
        .unwrap();
    assert_eq!(ok["granted"], true);
    let err = refused(w1.rpc(
        &d,
        "platform_check",
        json!({"platform": "github", "account": "acme", "scope": "repo:delete"}),
    ));
    assert!(
        err.contains("repo:delete"),
        "refusal must name the missing scope: {err}"
    );
    assert!(err.contains("w1"), "refusal names the agent: {err}");
    // No grant at all on the other account — refused, naming the ask.
    let err = refused(w1.rpc(
        &d,
        "platform_check",
        json!({"platform": "github", "account": "acme-bot", "scope": "repo:read"}),
    ));
    assert!(
        err.contains("repo:read") && err.contains("no grant"),
        "{err}"
    );
    // An agent cannot check for a peer.
    let err = refused(w1.rpc(
        &d,
        "platform_check",
        json!({"platform": "github", "account": "acme", "scope": "repo:read",
               "agent": "w2"}),
    ));
    assert!(err.contains("own grants"), "check as another: {err}");
    // The operator checks for a named holder.
    let ok = d
        .op(
            "platform_check",
            json!({"platform": "github", "account": "acme-bot",
                   "scope": "repo:write", "agent": "w2"}),
        )
        .unwrap();
    assert_eq!(ok["granted"], true);
    // A grant on an unenrolled account is refused — no latent grant.
    let err = refused(d.op(
        "platform_grant",
        json!({"agent": "w1", "platform": "github", "account": "ghost",
               "scopes": ["repo:read"]}),
    ));
    assert!(err.contains("not enrolled"), "{err}");
}

#[test]
fn project_default_resolves_the_account() {
    let d = Daemon::start();
    seed_project(&d, "proj1");
    let mut w1 = Lane::spawn_as(&d, "w1", None);
    // Several accounts per platform; the project names which one a
    // bare check resolves to.
    enroll(&d, "github", "acme", &["repo:read"], TOKEN);
    enroll(&d, "github", "acme-bot", &["repo:read"], TOKEN2);
    d.op(
        "platform_default_set",
        json!({"project": "proj1", "platform": "github", "account": "acme-bot"}),
    )
    .unwrap();
    let defaults = d.rpc("platform_defaults", json!({})).unwrap();
    assert_eq!(defaults["defaults"][0]["account"], "acme-bot");

    // A check naming the project resolves through its default account.
    grant(&d, "w1", "github", "acme-bot", &["repo:read"]);
    let ok = w1
        .rpc(
            &d,
            "platform_check",
            json!({"platform": "github", "project": "proj1", "scope": "repo:read"}),
        )
        .unwrap();
    assert_eq!(ok["grant"]["account"], "acme-bot");
    // With no default, the refusal says so instead of guessing.
    let err = refused(w1.rpc(
        &d,
        "platform_check",
        json!({"platform": "cloudflare", "project": "proj1", "scope": "x"}),
    ));
    assert!(err.contains("no default"), "{err}");
    // A default on a project that does not exist refuses.
    let err = refused(d.op(
        "platform_default_set",
        json!({"project": "ghost", "platform": "github", "account": "acme"}),
    ));
    assert!(err.contains("project") || err.contains("known"), "{err}");
}

// ---------- §5.3: rotate keeps grants; revoke closes effects ----------

#[test]
fn rotate_replaces_the_credential_and_keeps_grants() {
    let d = Daemon::start();
    seed_project(&d, "proj1");
    let mut w1 = Lane::spawn_as(&d, "w1", None);
    enroll(&d, "github", "acme", &["repo:read", "repo:write"], TOKEN);
    grant(&d, "w1", "github", "acme", &["repo:read"]);
    d.op(
        "platform_default_set",
        json!({"project": "proj1", "platform": "github", "account": "acme"}),
    )
    .unwrap();
    let old_fp = d.rpc("platform_accounts", json!({})).unwrap()["accounts"][0]["fingerprint"]
        .as_str()
        .unwrap()
        .to_string();

    // Rotate under the same handle — new bytes, scopes omitted (kept).
    let rotated = d
        .op(
            "platform_rotate",
            json!({"platform": "github", "account": "acme", "token": TOKEN2}),
        )
        .unwrap();
    let new_fp = rotated["account"]["fingerprint"].as_str().unwrap();
    assert_ne!(old_fp, new_fp, "rotation must change the fingerprint");
    assert!(!rotated.to_string().contains(TOKEN2));
    assert!(!rotated.to_string().contains(TOKEN));

    // Grants and the default survive untouched.
    let grant = w1.rpc(&d, "platform_grants", json!({})).unwrap();
    assert_eq!(grant["grants"].as_array().unwrap().len(), 1);
    assert_eq!(
        d.rpc("platform_defaults", json!({})).unwrap()["defaults"][0]["account"],
        "acme"
    );
    // Custody holds ONLY the new bytes.
    let custody = d.state.join("custody");
    let cred = std::fs::read_dir(&custody)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "cred"))
        .unwrap();
    assert_eq!(std::fs::read_to_string(cred).unwrap(), TOKEN2);
    // Scopes were kept — the record still carries the enrolled set.
    let acct = d.rpc("platform_accounts", json!({})).unwrap()["accounts"][0].clone();
    assert_eq!(acct["scopes"], json!(["repo:read", "repo:write"]));

    // Audit: the old credential's revoke is named, the re-enroll is
    // marked `rotated` — no bytes in either.
    let kinds: Vec<String> = d
        .audit()
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect();
    assert!(kinds.contains(&"credential_revoked".to_string()));
    let text = d.audit().iter().map(|e| e.to_string()).collect::<String>();
    assert!(text.contains("rotated"));
    assert!(
        !text.contains(TOKEN) && !text.contains(TOKEN2),
        "audit leaked: {text}"
    );

    // A plain re-enroll over a live record refuses — rotate is the verb.
    let err = refused(enroll_call(&d, "github", "acme"));
    assert!(err.contains("rotate"), "{err}");
    // Rotating a missing record refuses — enroll is the verb.
    let err = refused(d.op(
        "platform_rotate",
        json!({"platform": "github", "account": "ghost", "token": TOKEN}),
    ));
    assert!(err.contains("enroll"), "{err}");
}

fn enroll_call(d: &Daemon, platform: &str, account: &str) -> cadence_agent::Result<Value> {
    d.op(
        "platform_enroll",
        json!({"platform": platform, "account": account,
               "scopes": ["repo:read"], "token": TOKEN}),
    )
}

#[test]
fn revoke_drops_bytes_grants_default_and_closes_effects() {
    let d = Daemon::start();
    seed_project(&d, "proj1");
    let mut w1 = Lane::spawn_as(&d, "w1", Some(r#"{"broker_approvals": true}"#));
    enroll(&d, "github", "acme", &["repo:read"], TOKEN);
    grant(&d, "w1", "github", "acme", &["repo:read"]);
    d.op(
        "platform_default_set",
        json!({"project": "proj1", "platform": "github", "account": "acme"}),
    )
    .unwrap();
    // A pending effect bound to the credential — CAD-506's shape:
    // kind "effect", the credential coordinates in `input`.
    let opened = w1
        .rpc(
            &d,
            "request_open",
            json!({"alias": "w1", "kind": "effect", "tool": "github.create_issue",
                   "input_summary": "create issue via github/acme",
                   "input": {"platform": "github", "account": "acme",
                             "scope": "repo:write"}}),
        )
        .unwrap();
    let handle = opened["request"].as_str().unwrap().to_string();
    // One bound to a different account stays pending.
    let other = w1
        .rpc(
            &d,
            "request_open",
            json!({"alias": "w1", "kind": "effect", "tool": "github.create_issue",
                   "input_summary": "create issue via github/other",
                   "input": {"platform": "github", "account": "other",
                             "scope": "repo:write"}}),
        )
        .unwrap();
    let other_handle = other["request"].as_str().unwrap().to_string();

    let out = d
        .op(
            "platform_revoke",
            json!({"platform": "github", "account": "acme",
                   "reason": "token suspected leaked"}),
        )
        .unwrap();
    assert_eq!(out["state"], "revoked");
    assert_eq!(out["grants_revoked"], 1);
    assert_eq!(out["effects_closed"], json!([handle]));
    assert!(!out.to_string().contains(TOKEN));

    // Custody is empty; record, grant and default are gone — checked
    // on the tables themselves, not the dump at large (audit payloads
    // legitimately still name the account handle).
    let custody = d.state.join("custody");
    let creds: Vec<_> = std::fs::read_dir(&custody)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().ends_with(".cred"))
                .collect()
        })
        .unwrap_or_default();
    assert!(creds.is_empty(), "custody still holds bytes");
    let dump = db_snapshot(&d);
    assert!(
        !table_section(&dump, "platform_credentials").contains("acme"),
        "credential row outlived the revoke:\n{}",
        table_section(&dump, "platform_credentials")
    );
    assert!(
        !table_section(&dump, "platform_grants").contains("acme")
            && !table_section(&dump, "platform_defaults").contains("acme"),
        "grant/default rows outlived the revoke"
    );
    let grants = d.op("platform_grants", json!({})).unwrap();
    assert!(grants["grants"].as_array().unwrap().is_empty());
    assert!(d.rpc("platform_defaults", json!({})).unwrap()["defaults"]
        .as_array()
        .unwrap()
        .is_empty());
    // A check on the revoked account now refuses at the grant gate.
    let err = refused(w1.rpc(
        &d,
        "platform_check",
        json!({"platform": "github", "account": "acme", "scope": "repo:read"}),
    ));
    assert!(
        err.contains("no credential") || err.contains("no grant"),
        "{err}"
    );

    // §5.5's events, in order: grant revoked (reason), credential
    // revoked (reason), disconnected naming the closed effects.
    let events = d.audit();
    let kinds: Vec<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    for want in [
        "platform_connected",
        "scope_granted",
        "scope_revoked",
        "credential_revoked",
        "platform_disconnected",
    ] {
        assert!(kinds.contains(&want), "missing {want} in {kinds:?}");
    }
    let cred_revoked = events
        .iter()
        .find(|e| e["kind"] == "credential_revoked" && e["payload"]["reason"].is_string())
        .unwrap();
    assert_eq!(cred_revoked["payload"]["reason"], "token suspected leaked");
    let disc = events
        .iter()
        .find(|e| e["kind"] == "platform_disconnected")
        .unwrap();
    assert_eq!(disc["payload"]["effects_closed"], json!([handle]));
    // No event carries the bytes.
    let all = events.iter().map(|e| e.to_string()).collect::<String>();
    assert!(!all.contains(TOKEN), "audit leaked: {all}");

    // The bound effect's waiter sees `closed`; the other's stays open.
    let waited = w1
        .rpc(&d, "request_wait", json!({"request": handle, "wait": 1}))
        .unwrap();
    assert_eq!(waited["state"], "closed");
    let still = w1
        .rpc(
            &d,
            "request_wait",
            json!({"request": other_handle, "wait": 1}),
        )
        .unwrap();
    assert_eq!(still["state"], "waiting");
}

// ---------- custody protection: the honest-scope gate ----------

#[test]
fn enroll_refuses_custody_unprotected_until_the_operator_accepts() {
    let d = Daemon::start();
    let before = db_snapshot(&d);
    // No isolation mode protects custody from managed agents today —
    // a first enrollment refuses with the named code, before any
    // exchange runs. The refusal must not echo the token.
    let err = d
        .op(
            "platform_enroll",
            json!({"platform": "github", "account": "acme",
                   "scopes": ["repo:read"], "token": TOKEN}),
        )
        .unwrap_err();
    assert_eq!(
        err.code(),
        Some("custody_unprotected"),
        "enroll without the flag: {err}"
    );
    assert!(err.to_string().contains("same-uid"), "{err}");
    assert!(!err.to_string().contains(TOKEN));
    assert_eq!(before, db_snapshot(&d), "a refused enroll wrote");

    // The explicit operator flag enrolls — and the acceptance is on
    // the audit event, not just the call.
    let ok = d
        .op(
            "platform_enroll",
            json!({"platform": "github", "account": "acme",
                   "scopes": ["repo:read"], "token": TOKEN,
                   "accept_same_uid_risk": true}),
        )
        .unwrap();
    assert_eq!(ok["state"], "enrolled");
    let connected = d
        .audit()
        .iter()
        .find(|e| e["kind"] == "platform_connected")
        .cloned()
        .unwrap();
    assert_eq!(
        connected["payload"]["custody_risk_accepted"],
        json!("same-uid"),
        "the acceptance must be auditable: {connected}"
    );
    // Rotate inherits the enroll-time acceptance — no flag needed.
    d.op(
        "platform_rotate",
        json!({"platform": "github", "account": "acme", "token": TOKEN2}),
    )
    .unwrap();
}

#[test]
fn concurrent_enrolls_never_split_record_from_custody() {
    let d = Daemon::start();
    // Four concurrent enrolls of ONE account, distinct tokens. Exactly
    // one must land; the rest refuse the duplicate; and whatever the
    // record's fingerprint says is exactly what custody holds — a
    // record/custody tear is the bug this proves against.
    let tokens = [
        concat!("cadp_race_t", "oken_00000001"),
        concat!("cadp_race_t", "oken_00000002"),
        concat!("cadp_race_t", "oken_00000003"),
        concat!("cadp_race_t", "oken_00000004"),
    ];
    let d = &d;
    let results: Vec<cadence_agent::Result<Value>> = thread::scope(|s| {
        tokens
            .iter()
            .map(|token| {
                s.spawn(move || {
                    d.op(
                        "platform_enroll",
                        json!({"accept_same_uid_risk": true, "platform": "github",
                               "account": "acme", "scopes": ["repo:read"],
                               "token": token}),
                    )
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect()
    });
    let wins: Vec<&cadence_agent::Result<Value>> = results.iter().filter(|r| r.is_ok()).collect();
    assert_eq!(wins.len(), 1, "concurrent enrolls admitted: {results:?}");
    for r in &results {
        if let Err(e) = r {
            assert!(
                e.to_string().contains("already enrolled"),
                "a loser's refusal must name the duplicate: {e}"
            );
        }
    }

    // Custody bytes, the record fingerprint and exactly one token
    // agree — a torn pair would fail the fingerprint check at load.
    let cred = std::fs::read_dir(d.state.join("custody"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "cred"))
        .unwrap();
    let bytes = std::fs::read(&cred).unwrap();
    let fp = cadence_agent::secret::fingerprint(&bytes);
    let record_fp = d.rpc("platform_accounts", json!({})).unwrap()["accounts"][0]["fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(fp, record_fp, "record and custody disagree after the race");
    let matched: Vec<&&str> = tokens
        .iter()
        .filter(|t| t.as_bytes() == bytes.as_slice())
        .collect();
    assert_eq!(matched.len(), 1, "custody holds a foreign token");

    // A revoke racing a rotate under the same lock settles whole —
    // whichever ordering the lock picked, record and custody tell one
    // story. Revoke-first loses the rotate ("enroll first");
    // rotate-first loses nothing — the revoke then runs after it.
    d.op(
        "platform_rotate",
        json!({"platform": "github", "account": "acme", "token": TOKEN}),
    )
    .unwrap();
    let (rotate, revoke) = thread::scope(|s| {
        let r = s.spawn(|| {
            d.op(
                "platform_rotate",
                json!({"platform": "github", "account": "acme", "token": TOKEN2}),
            )
        });
        let v = s.spawn(|| {
            d.op(
                "platform_revoke",
                json!({"platform": "github", "account": "acme"}),
            )
        });
        (r.join().unwrap(), v.join().unwrap())
    });
    assert!(revoke.is_ok(), "the racing revoke must land: {revoke:?}");
    if let Err(e) = &rotate {
        // Revoke won the lock first — the rotate found no record.
        assert!(e.to_string().contains("enroll"), "rotate: {e}");
    }
    // Either way the account is revoked: no row, no custody bytes.
    assert!(d.rpc("platform_accounts", json!({})).unwrap()["accounts"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(!std::fs::read_dir(d.state.join("custody"))
        .unwrap()
        .any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".cred")));
}

#[test]
fn revoke_reason_cannot_carry_the_credential() {
    let d = Daemon::start();
    enroll(&d, "github", "acme", &["repo:read"], TOKEN);
    // The reason lands on audit AND agent-visible request_closed
    // events — a reason that is the credential is refused before any
    // of it is published.
    let err = refused(d.op(
        "platform_revoke",
        json!({"platform": "github", "account": "acme", "reason": TOKEN}),
    ));
    assert!(
        err.contains("credential") || err.contains("withheld"),
        "{err}"
    );
    assert!(
        !err.contains(TOKEN),
        "the refusal itself must not echo the reason: {err}"
    );
    // Nothing was revoked: record, custody and grants are untouched.
    assert!(std::fs::read_dir(d.state.join("custody"))
        .unwrap()
        .any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".cred")));
    assert_eq!(
        d.rpc("platform_accounts", json!({})).unwrap()["accounts"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "the refused revoke still tore the record down"
    );
    // And a clean reason revokes fine.
    let ok = d
        .op(
            "platform_revoke",
            json!({"platform": "github", "account": "acme",
                   "reason": "operator suspicion"}),
        )
        .unwrap();
    assert_eq!(ok["state"], "revoked");
}

// ---------- §5.3: no surface ever returns the credential ----------

#[test]
fn no_api_surface_returns_credential_bytes() {
    let d = Daemon::start();
    let mut w1 = Lane::spawn_as(&d, "w1", None);
    enroll(&d, "github", "acme", &["repo:read"], TOKEN);
    grant(&d, "w1", "github", "acme", &["repo:read"]);

    // Collect every readable surface: results, listings, the event
    // streams, refusals, the whole DB — the token is on none of them.
    let mut surfaces: Vec<String> = Vec::new();
    for (method, params) in [
        ("platform_accounts", json!({})),
        ("platform_defaults", json!({})),
        (
            "platform_check",
            json!({"platform": "github", "account": "acme", "scope": "repo:read"}),
        ),
    ] {
        surfaces.push(w1.rpc(&d, method, params).unwrap().to_string());
    }
    surfaces.push(
        w1.rpc(&d, "platform_grants", json!({}))
            .unwrap()
            .to_string(),
    );
    surfaces.push(refused(w1.rpc(
        &d,
        "platform_check",
        json!({"platform": "github", "account": "acme", "scope": "admin:all"}),
    )));
    surfaces.push(
        w1.rpc(&d, "agent_events", json!({"alias": "w1", "after": 0}))
            .unwrap()
            .to_string(),
    );
    surfaces.push(d.audit().iter().map(|e| e.to_string()).collect());
    surfaces.push(db_snapshot(&d));
    // The enroll result itself was already asserted clean; re-enroll
    // refusal text too.
    surfaces.push(refused(enroll_call(&d, "github", "acme")));
    for (i, text) in surfaces.iter().enumerate() {
        assert!(!text.contains(TOKEN), "surface {i} leaked: {text}");
    }
}

// ---------- input hygiene ----------

#[test]
fn malformed_scopes_and_handles_refuse_cleanly() {
    let d = Daemon::start();
    let before = db_snapshot(&d);
    // Scope names are `[A-Za-z0-9._:*-]` — anything else refuses.
    for bad in ["repo read", "repo\nread", "", "repo;drop"] {
        let err = refused(d.op(
            "platform_enroll",
            json!({"platform": "github", "account": "acme",
                   "scopes": [bad], "token": TOKEN}),
        ));
        assert!(err.contains("Scope"), "{bad:?}: {err}");
        assert!(!err.contains(TOKEN));
    }
    // Enroll needs at least one scope — a record with none would admit
    // nothing but sit as a custody row.
    let err = refused(d.op(
        "platform_enroll",
        json!({"platform": "github", "account": "acme",
               "scopes": [], "token": TOKEN}),
    ));
    assert!(err.contains("scope"), "{err}");
    // The CLI thin wrapper: `platform` verbs exist and refuse politely
    // from an unproven caller rather than panic.
    assert_eq!(before, db_snapshot(&d), "a refused call wrote");
}

#[test]
fn cli_verbs_reach_the_daemon() {
    let d = Daemon::start();
    let mut w1 = Lane::spawn_as(&d, "w1", None);
    // The token crosses once, via stdin — never argv, never output.
    let (rc, out) = w1.run(&format!(
        "printf '%s' '{TOKEN}' | {} --state-dir {} platform enroll github \
         --account acme --scope repo:read --token-stdin",
        env!("CARGO_BIN_EXE_cadence"),
        d.state.display()
    ));
    // w1 is an agent — refused, and the refusal does not echo the token.
    assert_ne!(rc, 0);
    assert!(!out.contains(TOKEN), "cli refusal leaked: {out}");

    // Operator path through the real CLI: enroll, grant, check.
    let script = d.state.join("op-cli.py");
    std::fs::write(&script, op_cli_py()).unwrap();
    let op_cli = |args: &str, stdin_text: &str| -> (bool, String) {
        let out_file = d
            .state
            .join(format!("op-cli-{}.json", uuid::Uuid::new_v4().simple()));
        // printf via env keeps the token off argv entirely.
        let status = Command::new("setsid")
            .arg("-f")
            .arg("python3")
            .arg(&script)
            .arg(&out_file)
            .arg(std::process::id().to_string())
            .arg(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&d.state)
            .args(args.split_whitespace())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("TOKEN_STDIN", stdin_text)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let deadline = Instant::now() + Duration::from_secs(30);
        while !out_file.exists() {
            assert!(Instant::now() < deadline, "op cli {args} never finished");
            thread::sleep(Duration::from_millis(20));
        }
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&out_file).unwrap()).unwrap();
        (
            v["rc"] == 0,
            format!(
                "{}{}",
                v["stdout"].as_str().unwrap_or_default(),
                v["stderr"].as_str().unwrap_or_default()
            ),
        )
    };
    let (ok, out) = op_cli(
        "platform enroll github --account acme --scope repo:read --token-stdin --accept-same-uid-risk",
        TOKEN,
    );
    assert!(ok, "enroll: {out}");
    assert!(!out.contains(TOKEN), "enroll output leaked: {out}");
    let (ok, out) = op_cli("platform accounts", "");
    assert!(ok && out.contains("acme"), "accounts: {out}");
    assert!(!out.contains(TOKEN));
    let (ok, out) = op_cli(
        "platform grant w1 github --account acme --scope repo:read",
        "",
    );
    assert!(ok, "grant: {out}");
    // Agent check through the CLI binds the caller's own alias.
    let (rc, out) = w1.cadence(&d, "platform check github --account acme --scope repo:read");
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains("\"granted\":true") || out.contains("\"granted\": true"),
        "{out}"
    );
    let (rc, out) = w1.cadence(&d, "platform check github --account acme --scope admin:all");
    assert_ne!(rc, 0);
    assert!(out.contains("admin:all"), "missing scope not named: {out}");
    assert!(!out.contains(TOKEN));
}

/// `tests/integration.rs`'s OPERATOR_CLI_PY, extended to feed one env
/// var to the child's stdin — `--token-stdin` reads it there.
fn op_cli_py() -> String {
    r#"
import json, os, subprocess, sys, time

out, runner = sys.argv[1:3]
argv = sys.argv[3:]

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
r = subprocess.run(argv, input=os.environ.get("TOKEN_STDIN", "").encode(),
                   capture_output=True)
with open(out + ".tmp", "w") as f:
    json.dump({"rc": r.returncode, "stdout": r.stdout.decode(errors="replace"),
               "stderr": r.stderr.decode(errors="replace")}, f)
os.rename(out + ".tmp", out)
"#
    .to_string()
}
