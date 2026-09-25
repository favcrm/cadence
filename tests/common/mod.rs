//! Shared harness for the split integration binaries (CAD-426): TestDaemon,
//! provider mocks, operator-proof runners and the host-wide suite lock.
//! Not every binary uses every helper.
#![allow(dead_code)]

use cadence_agent::adapter::ProviderEnv;
use cadence_agent::client;
use cadence_agent::daemon;
use cadence_agent::store::Store;
use serde_json::json;
use serde_json::Value;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::Mutex;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use tempfile::TempDir;

#[path = "../support/operator.rs"]
pub mod op;

/// [`TestDaemon::operator_rpc`]'s caller: it waits until it is off the
/// test process's ancestry (the `setsid -f` parent has exited), sends
/// one frame and lands the raw response frame atomically.
pub const OPERATOR_RPC_PY: &str = r#"
import json, os, socket, sys, time

sock_path, frame, out, runner = sys.argv[1:5]

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
s = socket.socket(socket.AF_UNIX)
s.connect(sock_path)
s.sendall((frame + "\n").encode())
line = s.makefile().readline()
s.close()
with open(out + ".tmp", "w") as f:
    f.write(line)
os.rename(out + ".tmp", out)
"#;

/// [`TestDaemon::operator_cadence`]'s runner: off the test process's
/// ancestry like `OPERATOR_RPC_PY`, it runs the cadence CLI and lands
/// `{rc, stdout, stderr}` atomically.
pub const OPERATOR_CLI_PY: &str = r#"
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
r = subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True)
with open(out + ".tmp", "w") as f:
    json.dump({"rc": r.returncode, "stdout": r.stdout.decode(errors="replace"),
               "stderr": r.stderr.decode(errors="replace")}, f)
os.rename(out + ".tmp", out)
"#;

/// Runs a spec'd command as an operator shell outside every agent and
/// outside the (in-process) daemon's tree: `setsid -f` detaches it,
/// the runner waits until it has left the test process's ancestry,
/// then runs the command with the spec's env and cwd and lands
/// `<out>.stdout`, `<out>.stderr` and `<out>` (`{"rc"}`).
pub const OPERATOR_EXEC_PY: &str = r#"
import json, os, subprocess, sys, time

spec_path, out, runner = sys.argv[1:4]
spec = json.load(open(spec_path))

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
r = subprocess.run(spec["argv"], env=spec["env"], cwd=spec["cwd"],
                   stdin=subprocess.DEVNULL, capture_output=True)
open(out + ".stdout", "wb").write(r.stdout)
open(out + ".stderr", "wb").write(r.stderr)
with open(out + ".tmp", "w") as f:
    json.dump({"rc": r.returncode}, f)
os.rename(out + ".tmp", out)
"#;

/// `Command::output`, run the way an operator's own shell reaches the
/// daemon (CAD-431): agent registration needs positive operator proof,
/// and a CLI the test process spawns directly descends from the
/// in-process daemon, which that proof refuses. The command keeps this
/// process's environment (less any agent identity) plus its own env
/// and cwd; stdin is empty.
pub trait OperatorOutput {
    fn operator_output(&mut self) -> std::io::Result<std::process::Output>;
}

impl OperatorOutput for std::process::Command {
    fn operator_output(&mut self) -> std::io::Result<std::process::Output> {
        use std::os::unix::process::ExitStatusExt;
        let mut env: std::collections::BTreeMap<String, String> = std::env::vars_os()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .filter(|(k, _)| k != "CADENCE_ALIAS" && k != "CADENCE_ROLLOUT_AS")
            .collect();
        for (k, v) in self.get_envs() {
            let k = k.to_string_lossy().into_owned();
            match v {
                Some(v) => env.insert(k, v.to_string_lossy().into_owned()),
                None => env.remove(&k),
            };
        }
        let mut argv = vec![self.get_program().to_string_lossy().into_owned()];
        argv.extend(self.get_args().map(|a| a.to_string_lossy().into_owned()));
        let cwd = match self.get_current_dir() {
            Some(d) => d.to_path_buf(),
            None => std::env::current_dir()?,
        };
        let dir = std::env::temp_dir().join(format!("opx-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir)?;
        let (script, spec, out) = (dir.join("run.py"), dir.join("spec.json"), dir.join("out"));
        std::fs::write(&script, OPERATOR_EXEC_PY)?;
        std::fs::write(
            &spec,
            json!({"argv": argv, "env": env, "cwd": cwd}).to_string(),
        )?;
        let status = std::process::Command::new("setsid")
            .arg("-f")
            .arg("python3")
            .arg(&script)
            .arg(&spec)
            .arg(&out)
            .arg(std::process::id().to_string())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        assert!(status.success(), "setsid -f failed: {status}");
        let deadline = Instant::now() + Duration::from_secs(120);
        while !out.exists() {
            assert!(
                Instant::now() < deadline,
                "operator command {argv:?} never finished"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let rc: Value = serde_json::from_str(&std::fs::read_to_string(&out)?)?;
        let rc = rc["rc"].as_i64().unwrap_or(1);
        let output = std::process::Output {
            status: if rc >= 0 {
                std::process::ExitStatus::from_raw((rc as i32) << 8)
            } else {
                std::process::ExitStatus::from_raw(-rc as i32)
            },
            stdout: std::fs::read(dir.join("out.stdout"))?,
            stderr: std::fs::read(dir.join("out.stderr"))?,
        };
        let _ = std::fs::remove_dir_all(&dir);
        Ok(output)
    }
}

/// A `cli(&[args]) -> (ok, json)` closure for the issue/dispatch CLI
/// fixtures: the cadence bin runs with `CADENCE_PM_DIR`/`HOME` bound, the
/// test's own bin dir first on PATH (so a spawned `cadence` resolves),
/// and the operator-proof call shape ([`OperatorOutput`]). Panics when the
/// reply is not JSON.
pub fn cadence_cli_json(
    state: &Path,
    pm_dir: &Path,
    home: &Path,
) -> impl Fn(&[&str]) -> (bool, Value) {
    let (state, pm_dir, home) = (
        state.to_path_buf(),
        pm_dir.to_path_buf(),
        home.to_path_buf(),
    );
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    move |args: &[&str]| -> (bool, Value) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .operator_output()
            .unwrap();
        let text = if out.stdout.is_empty() {
            String::from_utf8_lossy(&out.stderr).to_string()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        (
            out.status.success(),
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
        )
    }
}

/// The unwrapped sibling of [`cadence_cli_json`]: `cli_raw(&[args]) ->
/// (exit_code, stdout, stderr)` for tests that want the streams split —
/// e.g. an expected failure still needs its own assertion. Runs the bin
/// as a plain child (`.output()`), not the operator-proof exec.
pub fn cadence_cli_raw(
    state: &Path,
    pm_dir: &Path,
    home: &Path,
) -> impl Fn(&[&str]) -> (i32, String, String) {
    let (state, pm_dir, home) = (
        state.to_path_buf(),
        pm_dir.to_path_buf(),
        home.to_path_buf(),
    );
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    move |args: &[&str]| -> (i32, String, String) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }
}

pub struct TestDaemon {
    pub dir: TempDir,
    pub state: PathBuf,
    pub handle: Option<JoinHandle<cadence_agent::Result<()>>>,
    /// A `daemon run` process instead of an in-process daemon
    /// ([`TestDaemon::start_process_in`]).
    pub process: Option<std::process::Child>,
}

impl TestDaemon {
    pub fn start() -> Self {
        Self::start_opts(daemon_opts())
    }

    /// `start` with explicit daemon options — slot tests shrink the
    /// pools this way.
    pub fn start_opts(opts: daemon::ServeOptions) -> Self {
        suite_slot();
        let dir = TempDir::new().unwrap();
        let state = dir.path().to_path_buf();
        std::fs::create_dir_all(&state).unwrap();
        let owned = state.clone();
        let handle = thread::spawn(move || daemon::serve_with(&owned, opts));
        let daemon = Self {
            dir,
            state,
            handle: Some(handle),
            process: None,
        };
        daemon.wait_health();
        daemon
    }

    /// Start a daemon over a pre-seeded state directory.
    pub fn start_on(state: PathBuf) -> Self {
        Self::start_on_opts(state, daemon_opts())
    }

    /// `start_on` with explicit daemon options — slot tests shrink the
    /// pools or inject the clock this way.
    pub fn start_on_opts(state: PathBuf, opts: daemon::ServeOptions) -> Self {
        suite_slot();
        let dir = TempDir::new().unwrap(); // keeps lifetime uniform
        let owned = state.clone();
        let handle = thread::spawn(move || daemon::serve_with(&owned, opts));
        let daemon = Self {
            dir,
            state,
            handle: Some(handle),
            process: None,
        };
        daemon.wait_health();
        daemon
    }

    /// CAD-308: a real `cadence daemon run` PROCESS serving a state dir
    /// in `dir` — the production shape. Only that process is the child
    /// subreaper: an in-process daemon never enables it, because a test
    /// binary spawns children outside the reaper's registry. The daemon
    /// takes this test's provider env (`test_env`) as its process env
    /// at spawn, so install mock commands and `CADENCE_PM_DIR` BEFORE
    /// starting it; HOME is a directory inside `dir`. Drop shuts it
    /// down and reaps it — SIGKILL after 15s — even while a failed
    /// assertion unwinds (CAD-306).
    pub fn start_process_in(dir: TempDir) -> Self {
        suite_slot();
        let state = dir.path().to_path_buf();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let log = std::fs::File::create(dir.path().join("daemon-process.log")).unwrap();
        let process = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&state)
            .args(["daemon", "run"])
            .env("HOME", &home)
            .env_remove("CADENCE_ALIAS")
            .env_remove("CADENCE_ROLLOUT_AS")
            .envs(test_env().vars())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        let daemon = Self {
            dir,
            state,
            handle: None,
            process: Some(process),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while daemon.rpc("health", json!({})).is_err() {
            assert!(
                Instant::now() < deadline,
                "daemon run never became healthy: {}",
                std::fs::read_to_string(daemon.dir.path().join("daemon-process.log"))
                    .unwrap_or_default()
            );
            thread::sleep(Duration::from_millis(50));
        }
        daemon
    }

    pub fn wait_health(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.rpc("health", json!({})).is_ok() {
                return;
            }
            assert!(Instant::now() < deadline, "daemon did not become healthy");
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn rpc(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        client::rpc(&self.state, method, params)
    }

    /// `rpc` from a caller that is provably the operator however the
    /// suite is run. Operator-only methods (`slot_reconcile`,
    /// `approval_record`, …) refuse any connection whose ancestry
    /// carries an agent, and when the suite itself runs in an agent
    /// pane this test process is one (CAD-291). The call is made the
    /// way an operator shell outside every pane looks to the daemon:
    /// `setsid -f` hands it to a fresh session leader reparented off
    /// this process's ancestry, `env_clear` leaves no `CADENCE_ALIAS`,
    /// and stdio is not a pane tty. `peer::operator_proof` documents
    /// that shape as the residual it accepts, so the gate itself is
    /// untouched — agent-descended callers are still refused.
    pub fn operator_rpc(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let script = self.dir.path().join("operator-rpc.py");
        if !script.exists() {
            std::fs::write(&script, OPERATOR_RPC_PY).unwrap();
        }
        let out = self.dir.path().join(format!(
            "operator-rpc-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let frame = json!({"method": method, "params": params}).to_string();
        let status = std::process::Command::new("setsid")
            .arg("-f")
            .arg("python3")
            .arg(&script)
            .arg(client::socket_path(&self.state))
            .arg(&frame)
            .arg(&out)
            .arg(std::process::id().to_string())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "setsid -f failed: {status}");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !out.exists() {
            assert!(
                Instant::now() < deadline,
                "operator rpc {method} never answered"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let frame: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        cadence_agent::proto::unwrap(frame)
    }

    /// `rpc` from a caller that is deterministically unattributed
    /// (`Who::Unproven`), however the suite is run: detached exactly
    /// like [`Self::operator_rpc`] but carrying a `CADENCE_ALIAS` its
    /// ancestry cannot prove — operator evidence fails on the env mark
    /// and no pane names it. Plain `rpc` cannot stand in for this: in
    /// an agent pane the test process is unproven, but in CI it IS the
    /// operator — the two callers a rule treats differently.
    pub fn unproven_rpc(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let script = self.dir.path().join("operator-rpc.py");
        if !script.exists() {
            std::fs::write(&script, OPERATOR_RPC_PY).unwrap();
        }
        let out = self.dir.path().join(format!(
            "unproven-rpc-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let frame = json!({"method": method, "params": params}).to_string();
        let status = std::process::Command::new("setsid")
            .arg("-f")
            .arg("python3")
            .arg(&script)
            .arg(client::socket_path(&self.state))
            .arg(&frame)
            .arg(&out)
            .arg(std::process::id().to_string())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("CADENCE_ALIAS", "unproven-lane")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "setsid -f failed: {status}");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !out.exists() {
            assert!(
                Instant::now() < deadline,
                "unproven rpc {method} never answered"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let frame: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        cadence_agent::proto::unwrap(frame)
    }

    /// A fixture registration — the operator's act. Plain `rpc` from
    /// this process, unless a test planted this very process as a pane
    /// ([`plant_self`]): then this process IS that agent, which the
    /// registration caller rule (CAD-149) refuses, so the call goes
    /// through [`Self::operator_rpc`]. A suite run inside an agent pane
    /// carries `CADENCE_ALIAS` on its ancestry without any plant —
    /// unattributed then, and refused for missing operator proof, so
    /// that refusal goes the operator's way too. Gate refusals read
    /// "… is an operator action …" or "… not provably the operator …";
    /// the retry replays the same call, so a refusal that is not about
    /// proof comes back unchanged.
    pub fn fixture_rpc(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        if self.rpc("agent_show", json!({"alias": SELF_LANE})).is_ok() {
            return self.operator_rpc(method, params);
        }
        match self.rpc(method, params.clone()) {
            Err(e)
                if e.to_string().contains("operator action")
                    || e.to_string().contains("not provably the operator") =>
            {
                self.operator_rpc(method, params)
            }
            r => r,
        }
    }

    /// `cadence --state-dir <state> <args>` run the way
    /// [`Self::operator_rpc`] calls: as an operator shell outside every
    /// pane, however the suite is run. Answers `(success, stdout,
    /// stderr)`.
    pub fn operator_cadence(&self, args: &[&str]) -> (bool, String, String) {
        let script = self.dir.path().join("operator-cli.py");
        if !script.exists() {
            std::fs::write(&script, OPERATOR_CLI_PY).unwrap();
        }
        let out = self.dir.path().join(format!(
            "operator-cli-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let status = std::process::Command::new("setsid")
            .arg("-f")
            .arg("python3")
            .arg(&script)
            .arg(&out)
            .arg(std::process::id().to_string())
            .arg(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&self.state)
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.dir.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "setsid -f failed: {status}");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !out.exists() {
            assert!(
                Instant::now() < deadline,
                "operator cadence {args:?} never finished"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        (
            v["rc"] == 0,
            v["stdout"].as_str().unwrap_or_default().to_string(),
            v["stderr"].as_str().unwrap_or_default().to_string(),
        )
    }

    pub fn register(&self, alias: &str) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        self.fixture_rpc(
            "agent_register",
            json!({"alias": alias, "provider": "fake",
                   "endpoint_kind": "fake", "cwd": cwd}),
        )
        .unwrap();
    }

    pub fn wait_agent(&self, alias: &str, want: &str, secs: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let agent = self
                .rpc("agent_show", json!({"alias": alias}))
                .unwrap()
                .remove("agent");
            if agent["state"].as_str() == Some(want) {
                return agent;
            }
            assert!(
                Instant::now() < deadline,
                "agent {alias} never reached {want}: {agent}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn wait_message(&self, alias: &str, id: &str, want: &[&str], secs: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let show = self.rpc("agent_show", json!({"alias": alias})).unwrap();
            let messages = show["messages"].as_array().unwrap();
            if let Some(m) = messages.iter().find(|m| m["id"].as_str() == Some(id)) {
                if want.contains(&m["state"].as_str().unwrap_or("")) {
                    return m.clone();
                }
            }
            assert!(
                Instant::now() < deadline,
                "message {id} never reached {want:?}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn message_state(&self, alias: &str, id: &str) -> String {
        self.rpc("agent_show", json!({"alias": alias})).unwrap()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"].as_str() == Some(id))
            .unwrap()["state"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        // CAD-384: `shutdown` is the operator's (or the rollout lease
        // holder's). A test that planted this process as a pane
        // ([`plant_self`]), or a suite run inside an agent pane, is
        // refused by the caller rule — the daemon answered, so stop it
        // the way an operator shell would.
        if let Err(e) = self.rpc("shutdown", json!({})) {
            if e.to_string().contains("caller rule") {
                let _ = self.operator_rpc("shutdown", json!({}));
            }
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        // Our own child: waiting on it can never touch another process.
        if let Some(mut process) = self.process.take() {
            let deadline = Instant::now() + Duration::from_secs(15);
            while matches!(process.try_wait(), Ok(None)) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(50));
            }
            let _ = process.kill();
            let _ = process.wait();
        }
    }
}

pub trait Get {
    fn remove(&mut self, key: &str) -> Value;
}
impl Get for Value {
    fn remove(&mut self, key: &str) -> Value {
        self.as_object_mut().unwrap().remove(key).unwrap()
    }
}

// ---- fence recovery: operator reconcile + agent unfence ----

/// The DISCONNECT keyword makes the fake provider drop mid-turn — the
/// message lands `unknown` and fences the agent.
pub fn fence_agent(d: &TestDaemon, alias: &str, id: &str) {
    d.rpc(
        "agent_send",
        json!({"alias": alias, "text": "DISCONNECT", "message": id}),
    )
    .unwrap();
    d.wait_message(alias, id, &["unknown"], 15);
    d.wait_agent(alias, "attention", 10);
}

pub fn event_kinds(d: &TestDaemon, alias: &str) -> Vec<String> {
    d.rpc("agent_events", json!({"alias": alias})).unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect()
}

/// Releases the CAD-241 snapshot barrier once, including when the test
/// panics, so `serve` cannot stay parked after a failed assertion.
pub struct SnapshotGate {
    pub barrier: Option<Arc<Barrier>>,
}

impl SnapshotGate {
    pub fn release(&mut self) {
        if let Some(barrier) = self.barrier.take() {
            barrier.wait();
        }
    }
}

impl Drop for SnapshotGate {
    fn drop(&mut self) {
        self.release();
    }
}

/// CAD-406: set only on a child spawned by [`run_signal_child`]; names
/// the one test that child runs.
pub const SIGNAL_CHILD_ENV: &str = "CADENCE_TEST_SIGNAL_CHILD";

/// Why this process may not signal itself, or `None` inside a
/// [`run_signal_child`] child. Every `TestDaemon` registers a
/// process-wide SIGTERM/SIGINT hook that is never unregistered, and
/// plain `cargo test` runs all tests in one process: a self-signal there
/// shuts down every concurrent daemon (CAD-406's 15-16 collateral
/// failures), not just the one under test.
pub fn self_signal_refusal(child_env: Option<&str>) -> Option<&'static str> {
    match child_env {
        Some(test) if !test.is_empty() => None,
        _ => Some(
            "refusing to signal the shared test process: it would stop every \
             concurrent TestDaemon (CAD-406); run the test through run_signal_child",
        ),
    }
}

/// SIGTERM this process — the only sanctioned self-signal in the
/// suite, and only from a [`run_signal_child`] child.
pub fn sigterm_own_process() {
    let env = std::env::var(SIGNAL_CHILD_ENV).ok();
    if let Some(why) = self_signal_refusal(env.as_deref()) {
        panic!("{why}");
    }
    unsafe { libc::kill(std::process::id() as i32, libc::SIGTERM) };
}

/// Run the `#[ignore]`d `test` alone in a fresh copy of this test binary,
/// so a signal it sends itself reaches only its own daemons, and assert
/// that it ran and passed. A child whose signal never lands can block in
/// its own teardown, so it is killed and failed after `limit`.
pub fn run_signal_child(test: &str, limit: Duration) {
    let logs = TempDir::new().unwrap();
    let out_path = logs.path().join("out.log");
    let out = std::fs::File::create(&out_path).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", test, "--ignored", "--test-threads=1"])
        .stdout(out.try_clone().unwrap())
        .stderr(out)
        .env(SIGNAL_CHILD_ENV, test)
        // A plain libtest child: the outer run's suite lock and
        // nextest markers are not its to honour.
        .env_remove("CADENCE_SUITE_LOCK")
        .env_remove("CADENCE_REVIEW_SUITE_LOCK_HELD");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("NEXTEST") {
            child.env_remove(key);
        }
    }
    let mut child = child.spawn().unwrap();
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(100));
    };
    let text = std::fs::read_to_string(&out_path).unwrap_or_default();
    let Some(status) = status else {
        panic!("{test} did not finish in its child within {limit:?}: {text}");
    };
    assert!(status.success(), "{test} failed in its child: {text}");
    assert!(
        text.contains("test result: ok. 1 passed"),
        "{test} must actually run in its child: {text}"
    );
}

// ---- mock Codex provider over real stdio (no model calls) ----

/// Serialize tests that set real process environment variables — the
/// mock-side knobs a provider child inherits (`MOCK_TMUX_STATE`, …).
/// Provider launch commands never go through the environment: see
/// `test_env`.
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

thread_local! {
    static TEST_ENV: ProviderEnv = ProviderEnv::default();
    static TEST_STALL_SAMPLE: std::sync::Arc<std::sync::atomic::AtomicU64> =
        std::sync::Arc::default();
}

/// Shrink this test's stall screen-sample interval (0 = daemon default)
/// — a per-daemon option, never `CADENCE_STALL_SAMPLE_SECS` in the
/// shared process env, and live for daemons already running.
pub fn stall_sample(secs: u64) {
    TEST_STALL_SAMPLE.with(|s| s.store(secs, std::sync::atomic::Ordering::Relaxed));
}

/// The tracker's pre-commit hook runs `cadence` from PATH (`issue
/// init` installs it) — under `cargo test` that resolves to the
/// installed release, whose lint predates whatever this tree adds
/// (a `workflows/` dir flags as "no issue.md" and refuses the
/// commit). Put the binary under test first on PATH, once per
/// process: every child spawned afterwards — fixture CLIs, a
/// `daemon run` process, the git commits an in-process daemon makes
/// — lints with the code being tested. The `issue init` hook tests
/// do the same per-command; this covers the paths that inherit the
/// process env instead.
pub fn hook_bin_on_path() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
            .parent()
            .unwrap()
            .to_path_buf();
        let mut path: Vec<PathBuf> =
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
        path.insert(0, bin_dir);
        std::env::set_var("PATH", std::env::join_paths(path).unwrap());
    });
}

/// This test's provider launch overrides (mock commands). Each test
/// runs on its own thread, so every daemon it starts — restarts
/// included — shares them, and no other test's daemon ever sees them.
pub fn test_env() -> ProviderEnv {
    hook_bin_on_path();
    TEST_ENV.with(ProviderEnv::clone)
}

/// Host-wide suite slot (CAD-71). With `CADENCE_SUITE_LOCK` set, an
/// unfiltered run of this binary — the full suite — holds that
/// exclusive `flock` for the process lifetime, so concurrent full
/// suites on one host take turns instead of starving each other into
/// load flakes. A filtered run (one test, one group) never queues. The
/// first daemon a test starts acquires it; the kernel releases it at
/// exit. The wait is bounded (`CADENCE_SUITE_LOCK_WAIT_SECS`, default
/// 3600) and every test panics with the reason if it runs out.
pub static SUITE_SLOT: std::sync::OnceLock<Result<Option<std::fs::File>, String>> =
    std::sync::OnceLock::new();

pub fn suite_slot() {
    if let Err(msg) = SUITE_SLOT.get_or_init(acquire_suite_slot) {
        panic!("{msg}");
    }
}

/// libtest's positional arguments are test-name filters; these flags
/// take a separate value that is not one.
pub fn is_filtered_run() -> bool {
    const VALUED: &[&str] = &[
        "--test-threads",
        "--skip",
        "--logfile",
        "--color",
        "--format",
        "--shuffle-seed",
        "-Z",
    ];
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if VALUED.contains(&a.as_str()) {
            args.next();
        } else if !a.starts_with('-') {
            return true;
        }
    }
    false
}

/// Nextest launches every test in a process-per-test child and exposes
/// `NEXTEST=1` plus `NEXTEST_EXECUTION_MODE`. A filtered child cannot
/// safely acquire the host slot itself. The outer review process owns
/// the flock and explicitly clears the child path instead.
pub fn is_nextest_run() -> bool {
    std::env::var("NEXTEST").ok().as_deref() == Some("1")
        || std::env::var("NEXTEST_EXECUTION_MODE").is_ok()
}

pub fn nextest_outer_lock_required(
    nextest: bool,
    lock_path: Option<&str>,
    review_held: bool,
) -> Result<(), &'static str> {
    if !nextest {
        return Ok(());
    }
    if review_held && lock_path.is_none() {
        return Ok(());
    }
    Err(
        "nextest requires the external CADENCE_SUITE_LOCK; run `cadence review` or use the pinned outer wrapper",
    )
}

pub fn acquire_suite_slot() -> Result<Option<std::fs::File>, String> {
    use std::io::Write;
    use std::os::unix::io::AsRawFd;
    let path = std::env::var("CADENCE_SUITE_LOCK")
        .ok()
        .filter(|p| !p.is_empty());
    let review_held = std::env::var("CADENCE_REVIEW_SUITE_LOCK_HELD")
        .ok()
        .as_deref()
        == Some("1");
    nextest_outer_lock_required(is_nextest_run(), path.as_deref(), review_held)
        .map_err(str::to_string)?;
    let Some(path) = path else { return Ok(None) };
    if is_filtered_run() {
        return Ok(None);
    }
    let wait_secs: u64 = std::env::var("CADENCE_SUITE_LOCK_WAIT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3600);
    let path = PathBuf::from(path);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| {
            format!(
                "cannot create the directory of the host suite slot {} \
                 (CADENCE_SUITE_LOCK): {e} — fix the path or unset the variable",
                path.display()
            )
        })?;
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| {
            format!(
                "cannot open the host suite slot {} (CADENCE_SUITE_LOCK): {e} \
                 — fix the path or unset the variable",
                path.display()
            )
        })?;
    let epoch = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    };
    // Raw stderr, not eprintln!: libtest captures the macros per test,
    // and the queueing must be visible while it happens.
    let say = |msg: String| {
        let _ = std::io::stderr().write_all(format!("{msg}\n").as_bytes());
    };
    let start = Instant::now();
    let mut announced = false;
    loop {
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            say(format!(
                "suite slot {} acquired at epoch {} after {}s (pid {})",
                path.display(),
                epoch(),
                start.elapsed().as_secs(),
                std::process::id()
            ));
            return Ok(Some(file));
        }
        if !announced {
            say(format!(
                "suite slot {} busy — another full suite runs on this host; \
                 waiting up to {wait_secs}s (epoch {})",
                path.display(),
                epoch()
            ));
            announced = true;
        }
        if start.elapsed() >= Duration::from_secs(wait_secs) {
            return Err(format!(
                "timed out after {wait_secs}s waiting for the host suite slot {} \
                 (CADENCE_SUITE_LOCK) — another full suite still holds it; \
                 unset the variable to run unserialized",
                path.display()
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

pub fn daemon_opts() -> daemon::ServeOptions {
    // CAD-339 (review round 1, I2): a test daemon never falls back to the
    // host's `$HOME/pm`. Unless the test binds a tracker, it gets a
    // per-test path that does not exist.
    if test_env().var("CADENCE_PM_DIR").is_none() {
        let none = std::env::temp_dir().join(format!(
            "cadence-test-no-pm-{}",
            uuid::Uuid::new_v4().simple()
        ));
        test_env().set("CADENCE_PM_DIR", none.to_str().unwrap());
    }
    daemon::ServeOptions {
        provider_env: test_env(),
        stall_sample_secs: TEST_STALL_SAMPLE.with(std::sync::Arc::clone),
        // Explicit defaults keep test daemons hermetic — a real pm.yaml
        // [host] table on the dev host must never leak into a test.
        slots: Some(cadence_agent::slots::SlotConfig::default()),
        slot_clock: None,
        release_shutdown_snapshot: None,
        // CAD-199: the agent-gc timer stays off unless a test pins it.
        agent_gc: Some(daemon::AgentGcSetting::default()),
        // CAD-96: idle auto-stop is ON by default in production; test
        // daemons pin it off so no test's agent is stopped mid-test.
        auto_stop: Some(daemon::AutoStopSetting::off()),
        auto_stop_clock: None,
        // CAD-339: the report router scans a tracker; only the master
        // tests (which bind CADENCE_PM_DIR) turn it on.
        report_router: Some(0),
        // CAD-477: the checkup judges lanes on its own; only the
        // checkup tests turn it on.
        checkup: Some(0),
        // CAD-484: no idle-lane dispatch seam — checkup tests that
        // pick a ticket inject their own.
        checkup_dispatch: None,
        idle_poll: None,
        stop: None,
        // CAD-313: links and sessions expire by the wall clock unless a
        // test injects one.
        operator_clock: None,
    }
}

/// A stdio JSON-RPC provider speaking just enough of the app-server wire
/// to reach each failure mode. Writes its pid to a file for leak checks.
pub const MOCK_PY: &str = r#"
import json, os, sys, threading, time
pidfile, mode = sys.argv[1], sys.argv[2]
turn_count = 0
# interrupt-text / interrupt-tool (CAD-323): the turn in flight, waiting
# for `turn/interrupt` — every interrupt lands in <pidfile>.interrupts.
# hold: turn t-<n> completes once <pidfile>.release-t-<n> exists.
in_flight = None
with open(pidfile, "w") as f:
    f.write(str(os.getpid()))
emit_lock = threading.Lock()
def emit(msg):
    with emit_lock:
        sys.stdout.write(json.dumps(msg) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    try: msg = json.loads(line)
    except Exception: continue
    mid, method = msg.get("id"), msg.get("method")
    if mid is None: continue
    if method == "initialize":
        if mode == "slow-init": time.sleep(30)
        emit({"id": mid, "result": {"serverInfo": {"name": "mock", "version": "0"}}})
    elif method == "model/list":
        # Metadata-only response: configured Codex tests can exercise the
        # pair validator without making a paid model turn.
        if mode == "bad-model-list":
            emit({"id": mid, "result": {}})
        else:
            emit({"id": mid, "result": {"data": [
                {"id": "gpt-5.6-luna", "model": "gpt-5.6-luna",
                 "isDefault": False,
                 "supportedReasoningEfforts": [
                     {"reasoningEffort": "low"},
                     {"reasoningEffort": "medium"},
                     {"reasoningEffort": "high"},
                     {"reasoningEffort": "xhigh"},
                     {"reasoningEffort": "max"}]},
                {"id": "mock-model", "model": "mock-model",
                 "isDefault": True,
                 "supportedReasoningEfforts": [{"reasoningEffort": "medium"}]}
            ], "nextCursor": None}})
    elif method in ("thread/start", "thread/resume"):
        # Record the launch payload before answering so tests can read
        # exactly what reached the wire (<pidfile>.requests).
        with open(pidfile + ".requests", "a") as rf:
            rf.write(json.dumps({"method": method,
                                 "params": msg.get("params", {})}) + "\n")
        if mode == "bad-thread":
            emit({"id": mid, "result": {"thread": {}}})
        else:
            launch = msg.get("params", {})
            effort = launch.get("config", {}).get("model_reasoning_effort", "medium")
            model = launch.get("model", "mock-model")
            emit({"id": mid, "result": {"thread": {
                "id": "th-1", "sessionId": "s-1", "model": model,
                "reasoningEffort": effort},
                "model": model, "reasoningEffort": effort}})
    elif method == "account/rateLimits/read":
        if mode in ("no-quota", "quota-recover"):
            emit({"id": mid, "error": {"code": -32601,
                 "message": "rate limits unavailable in this auth mode"}})
        else:
            emit({"id": mid, "result": {
                "accountId": "acct-codex-test",
                "rateLimits": {
                    "primary": {"usedPercent": 23,
                                 "windowDurationMins": 60,
                                 "resetsAt": 1900000000},
                    "secondary": None},
                "rateLimitsByLimitId": {
                    "codex": {"usedPercent": 7,
                              "windowDurationMins": 10080,
                              "resetsAt": 1900100000}},
                "planType": "mock-pro"}})
    elif method == "turn/start":
        turn_count += 1
        if mode == "bad-turn":
            emit({"id": mid, "result": {"turn": {}}})
        elif mode == "die-after-start":
            emit({"id": mid, "result": {"turn": {"id": "t-1"}}}); os._exit(0)
        elif mode == "silent":
            emit({"id": mid, "result": {"turn": {"id": "t-1"}}})
        elif mode == "hold":
            tid = "t-%d" % turn_count
            emit({"id": mid, "result": {"turn": {"id": tid}}})
            in_flight = tid
            def finish(tid=tid):
                while not os.path.exists(pidfile + ".release-" + tid):
                    time.sleep(0.05)
                emit({"method": "turn/completed", "params": {"turn": {
                    "id": tid, "status": "completed", "items": [
                        {"id": "f-" + tid, "type": "agentMessage",
                         "text": "MOCK_OK", "phase": "final_answer"}]}}})
            threading.Thread(target=finish, daemon=True).start()
        elif mode in ("interrupt-text", "interrupt-tool") and turn_count == 1:
            # The first turn streams, then waits for turn/interrupt;
            # later turns complete like "ok".
            tid = "t-%d" % turn_count
            emit({"id": mid, "result": {"turn": {"id": tid}}})
            emit({"method": "item/completed", "params": {"turnId": tid, "item": {
                "id": "i0", "type": "agentMessage", "text": "working",
                "phase": "commentary"}}})
            if mode == "interrupt-tool":
                emit({"method": "item/started", "params": {"turnId": tid, "item": {
                    "id": "cmd-1", "type": "commandExecution",
                    "command": "sleep 300", "status": "inProgress"}}})
            in_flight = tid
        else:
            emit({"id": mid, "result": {"turn": {"id": "t-1"}}})
            def item(iid, text, phase=None):
                it = {"id": iid, "type": "agentMessage", "text": text}
                if phase:
                    it["phase"] = phase
                emit({"method": "item/completed",
                      "params": {"turnId": "t-1", "item": it}})
                return it
            if mode in ("items", "items-mixed"):
                # A commentary agentMessage persisted mid-turn (CAD-319).
                item("i0", "looking", "commentary")
            if mode == "items-mixed":
                # Unphased beside a final: Codex's result is the final
                # only, so the thread keeps this one (CAD-320).
                item("i0b", "an aside")
            if mode in ("items", "items-mixed"):
                # The final answer persists as an item too; the turn
                # result carries it, so the thread must not repeat it
                # (CAD-320).
                item("i1", "MOCK_OK", "final_answer")
            if mode == "items-unphased":
                # An older Codex: no phases, the result joins them all.
                done = [item("i0", "first"), item("i1", "second")]
                emit({"method": "turn/completed", "params": {"turn": {
                    "id": "t-1", "status": "completed", "items": done}}})
                continue
            if mode == "items-die":
                # Items persist, then the process dies: the turn is
                # unknown and its result empty — the thread keeps both.
                item("i0", "partial")
                item("i1", "almost there", "final_answer")
                os._exit(0)
            if mode == "heartbeat":
                # ~3.6s of streamed activity, never silent for long.
                for _ in range(12):
                    time.sleep(0.3)
                    emit({"method": "item/agentMessage/delta",
                          "params": {"turnId": "t-1", "delta": "."}})
            if mode in ("quota-update", "quota-recover"):
                if mode == "quota-recover":
                    update = {"accountId": "acct-recovered",
                              "rateLimits": {"primary": {"usedPercent": 42}}}
                elif turn_count == 1:
                    # First update explicitly clears nullable window fields;
                    # the account id is tested separately as a conservative
                    # identity field and must survive its explicit null.
                    update = {"accountId": None,
                              "rateLimits": {"primary": {
                                  "usedPercent": 42,
                                  "windowDurationMins": None,
                                  "resetsAt": None}}}
                else:
                    # The second update omits the nullable fields entirely.
                    # Omission must preserve their already-cleared state.
                    update = {"rateLimits": {"primary": {"usedPercent": 44}}}
                emit({"method": "account/rateLimits/updated", "params": update})
            emit({"method": "turn/completed", "params": {"turn": {
                "id": "t-1", "status": "completed", "items": [
                    {"id": "i1", "type": "agentMessage",
                     "text": "MOCK_OK", "phase": "final_answer"}]}}})
    elif method == "turn/interrupt":
        p = msg.get("params", {})
        with open(pidfile + ".interrupts", "a") as f:
            f.write(json.dumps(p) + "\n")
        if in_flight is None or p.get("turnId") != in_flight:
            emit({"id": mid, "error": {"code": -32600,
                  "message": "no active turn to interrupt"}})
            continue
        emit({"id": mid, "result": {}})
        items = []
        if mode == "interrupt-tool":
            # The killed command completes with its partial output.
            cmd = {"id": "cmd-1", "type": "commandExecution",
                   "command": "sleep 300", "status": "failed",
                   "aggregatedOutput": "partial line 1\n", "exitCode": None}
            emit({"method": "item/completed",
                  "params": {"turnId": in_flight, "item": cmd}})
            items.append(cmd)
        emit({"method": "turn/completed", "params": {"turn": {
            "id": in_flight, "status": "interrupted", "items": items,
            "error": None}}})
        in_flight = None
"#;

pub struct MockCodex {
    pub pidfile: PathBuf,
}

// ---- mock Codex app-server over a real WebSocket (no model calls) ----

/// A WebSocket JSON-RPC provider speaking the same app-server wire as
/// MOCK_PY. Parses `--listen ws://host:port` from argv (appended by the
/// transport), handshakes with stdlib sockets, and serves text frames.
/// Turn text directives: `DIE` closes the TCP connection after the ack,
/// `DIE2` sends a WS close frame instead, `FRAG` delivers
/// turn/completed as two continuations with an interleaved ping,
/// `NEED_INPUT:x` raises a server->client approval request that must be
/// answered before the turn completes, `NEED_INPUT_EXT:x` raises one
/// then resolves it externally via `serverRequest/resolved` once
/// `<pidfile>.resolve` appears. Modes: `silent` never completes
/// non-seed turns, `no-upgrade` accepts TCP but never answers the WS
/// handshake, `drip` feeds a valid 101 one byte/second,
/// `bad-upgrade` answers 200 instead of 101, `ping-first` pings before
/// the non-seed ack and records pong receipt in `<pidfile>.pong`.
/// The handshake is strict: a Sec-WebSocket-Key that does not decode
/// to exactly 16 bytes is refused with 400.
pub const MOCK_WS_PY: &str = r##"
import base64, hashlib, json, os, socket, struct, sys, threading, time

pidfile, mode = sys.argv[1], sys.argv[2]
url = sys.argv[sys.argv.index("--listen") + 1]
host, port = url.split("://", 1)[1].split(":")
srv = socket.socket()
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind((host, int(port)))
srv.listen(4)
with open(pidfile, "w") as f:
    f.write(str(os.getpid()))
GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
SEED = "Cadence endpoint initialization"

def recv_exact(conn, n):
    data = b""
    while len(data) < n:
        chunk = conn.recv(n - len(data))
        if not chunk:
            return None
        data += chunk
    return data

def read_frame(conn):
    hdr = recv_exact(conn, 2)
    if hdr is None:
        return None, None
    opcode, flags = hdr[0] & 0x0F, hdr[1]
    length = flags & 0x7F
    if length == 126:
        length = struct.unpack(">H", recv_exact(conn, 2))[0]
    elif length == 127:
        length = struct.unpack(">Q", recv_exact(conn, 8))[0]
    mask = recv_exact(conn, 4) if flags & 0x80 else b""
    payload = recv_exact(conn, length) if length else b""
    if payload is None:
        return None, None
    if mask:
        payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
    return opcode, payload

def send_frag(conn, fin, opcode, payload):
    n = len(payload)
    if n < 126:
        hdr = bytes([(0x80 if fin else 0) | opcode, n])
    elif n < 65536:
        hdr = bytes([(0x80 if fin else 0) | opcode, 126]) + struct.pack(">H", n)
    else:
        hdr = bytes([(0x80 if fin else 0) | opcode, 127]) + struct.pack(">Q", n)
    conn.sendall(hdr + payload)

def send_frame(conn, opcode, payload):
    send_frag(conn, True, opcode, payload)

def send_json(conn, msg):
    send_frame(conn, 1, json.dumps(msg).encode())

def send_fragmented_complete(conn, turn, text):
    # One message split across two continuations with an interleaved
    # ping: the client must reassemble it and answer the control frame.
    body = json.dumps({"method": "turn/completed", "params": {"turn": {
        "id": turn, "status": "completed", "items": [
            {"id": "i1", "type": "agentMessage",
             "text": text, "phase": "final_answer"}]}}}).encode()
    half = len(body) // 2
    send_frag(conn, False, 0x1, body[:half])
    send_frame(conn, 0x9, b"mid-frag")
    send_frag(conn, True, 0x0, body[half:])

def complete(conn, turn, text):
    send_json(conn, {"method": "turn/completed", "params": {"turn": {
        "id": turn, "status": "completed", "items": [
            {"id": "i1", "type": "agentMessage",
             "text": text, "phase": "final_answer"}]}}})

def handshake(conn):
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = conn.recv(4096)
        if not chunk:
            return False
        data += chunk
    key = ""
    for line in data.decode().split("\r\n"):
        if line.lower().startswith("sec-websocket-key:"):
            key = line.split(":", 1)[1].strip()
    # RFC 6455: the client nonce must decode to exactly 16 bytes.
    # Refuse anything else, as a strict standards-compliant server does.
    try:
        valid = len(base64.b64decode(key)) == 16
    except Exception:
        valid = False
    if not valid:
        conn.sendall(b"HTTP/1.1 400 Bad Request\r\n\r\n")
        return False
    accept = base64.b64encode(
        hashlib.sha1((key + GUID).encode()).digest()).decode()
    conn.sendall((
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Accept: {accept}\r\n\r\n").encode())
    return True

def ping_check(conn):
    # Ping, then expect a pong before completing: proves the client
    # serializes control replies on the same write path as requests.
    send_frame(conn, 0x9, b"ping-check")
    conn.settimeout(5)
    pong = False
    try:
        while True:
            op, _ = read_frame(conn)
            if op is None:
                break
            if op == 0xA:
                pong = True
                break
    except Exception:
        pass
    conn.settimeout(None)
    with open(pidfile + ".pong", "w") as f:
        f.write("yes" if pong else "no")

def external_resolve(conn):
    # An attached TUI answered the approval: once the test drops the
    # trigger file, resolve the pending request outside the client and
    # let the turn finish without a client response.
    deadline = time.time() + 30
    while not os.path.exists(pidfile + ".resolve"):
        if time.time() > deadline:
            break
        time.sleep(0.05)
    send_json(conn, {"method": "serverRequest/resolved",
        "params": {"requestId": "srv-1", "threadId": "th-1"}})
    complete(conn, "t-1", "MOCK_OK")

def read_request(conn):
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = conn.recv(4096)
        if not chunk:
            return None
        data += chunk
    key = ""
    for line in data.decode().split("\r\n"):
        if line.lower().startswith("sec-websocket-key:"):
            key = line.split(":", 1)[1].strip()
    return key

def handle(conn):
    if mode == "no-upgrade":
        # Accept TCP, never answer the handshake. The client's bounded
        # handshake must give up and clean up the owned process.
        time.sleep(3600)
        return
    if mode == "drip":
        # Answer with a valid 101 one byte per second: only an absolute
        # deadline bounds this — per-read timeouts never fire.
        key = read_request(conn)
        if key is None:
            return
        accept = base64.b64encode(
            hashlib.sha1((key + GUID).encode()).digest()).decode()
        response = ("HTTP/1.1 101 Switching Protocols\r\n"
            "Upgrade: websocket\r\nConnection: Upgrade\r\n"
            f"Sec-WebSocket-Accept: {accept}\r\n\r\n").encode()
        for byte in response:
            conn.sendall(bytes([byte]))
            time.sleep(1)
        time.sleep(3600)
        return
    if mode == "bad-upgrade":
        # Refuse the upgrade outright: HTTP 200, not 101.
        if read_request(conn) is None:
            return
        conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
        time.sleep(3600)
        return
    if not handshake(conn):
        return
    approvals = set()
    while True:
        op, payload = read_frame(conn)
        if op is None or op == 8:
            break
        if op == 9:
            send_frame(conn, 0xA, payload)
            continue
        if op != 1:
            continue
        try:
            msg = json.loads(payload)
        except Exception:
            continue
        mid, method = msg.get("id"), msg.get("method")
        if method is None:
            if mid in approvals:
                approvals.discard(mid)
                if not approvals:
                    complete(conn, "t-1", "MOCK_OK")
            continue
        if method == "initialize":
            if mode == "slow-init":
                # Marks the handshake done and initialize in flight.
                open(pidfile + ".init", "w").close()
                time.sleep(30)
            send_json(conn, {"id": mid, "result": {
                "serverInfo": {"name": "mock-ws", "version": "0"}}})
        elif method == "model/list":
            send_json(conn, {"id": mid, "result": {"data": [
                {"id": "gpt-5.6-luna", "model": "gpt-5.6-luna",
                 "isDefault": False,
                 "supportedReasoningEfforts": [
                     {"reasoningEffort": "low"},
                     {"reasoningEffort": "medium"},
                     {"reasoningEffort": "high"},
                     {"reasoningEffort": "xhigh"},
                     {"reasoningEffort": "max"}]},
                {"id": "mock-model", "model": "mock-model",
                 "isDefault": True,
                 "supportedReasoningEfforts": [{"reasoningEffort": "medium"}]}
            ], "nextCursor": None}})
        elif method in ("thread/start", "thread/resume"):
            # Record the launch payload before answering (<pidfile>.requests).
            with open(pidfile + ".requests", "a") as rf:
                rf.write(json.dumps({"method": method,
                                     "params": msg.get("params", {})}) + "\n")
            launch = msg.get("params", {})
            effort = launch.get("config", {}).get("model_reasoning_effort", "medium")
            model = launch.get("model", "mock-model")
            send_json(conn, {"id": mid, "result": {"thread": {
                "id": "th-1", "sessionId": "s-1", "model": model,
                "reasoningEffort": effort},
                "model": model, "reasoningEffort": effort}})
        elif method == "account/rateLimits/read":
            if mode == "no-quota":
                send_json(conn, {"id": mid, "error": {"code": -32601,
                    "message": "rate limits unavailable in this auth mode"}})
            else:
                send_json(conn, {"id": mid, "result": {
                    "accountId": "acct-codex-test",
                    "rateLimits": {"primary": {"usedPercent": 23,
                        "windowDurationMins": 60, "resetsAt": 1900000000}},
                    "rateLimitsByLimitId": {}, "planType": "mock-pro"}})
        elif method == "turn/start":
            text = ""
            try:
                text = msg["params"]["input"][0]["text"]
            except Exception:
                pass
            if mode == "ping-first" and not text.startswith(SEED):
                ping_check(conn)
            send_json(conn, {"id": mid, "result": {"turn": {"id": "t-1"}}})
            if text.startswith("DIE2"):
                send_frame(conn, 8, b"")
                return
            if text.startswith("DIE"):
                conn.close()
                return
            if text.startswith("FRAG"):
                send_fragmented_complete(conn, "t-1", "MOCK_OK")
                continue
            if text.startswith(SEED):
                complete(conn, "t-1", "READY")
            elif mode == "silent":
                pass
            elif text.startswith("NEED_INPUT_EXT"):
                approvals.add("srv-1")
                send_json(conn, {"id": "srv-1",
                    "method": "item/commandExecution/requestApproval",
                    "params": {"command": "x"}})
                external_resolve(conn)
                return
            elif text.startswith("NEED_INPUT2"):
                # Two outstanding approvals: answering one must leave
                # the agent waiting_input until both are answered.
                approvals.update(("srv-1", "srv-2"))
                for rid in ("srv-1", "srv-2"):
                    send_json(conn, {"id": rid,
                        "method": "item/commandExecution/requestApproval",
                        "params": {"command": "x"}})
            elif text.startswith("NEED_INPUT"):
                approvals.add("srv-1")
                send_json(conn, {"id": "srv-1",
                    "method": "item/commandExecution/requestApproval",
                    "params": {"command": "x"}})
            else:
                complete(conn, "t-1", "MOCK_OK")
        elif method == "turn/interrupt":
            send_json(conn, {"id": mid, "result": {}})
    conn.close()

while True:
    conn, _ = srv.accept()
    threading.Thread(target=handle, args=(conn,), daemon=True).start()
"##;

impl TestDaemon {
    /// Install a mock codex command for `mode`, returning its pidfile.
    pub fn mock_codex(&self, mode: &str) -> MockCodex {
        let pidfile = self.dir.path().join(format!("mock-{mode}.pid"));
        let script = self.dir.path().join(format!("mock-{mode}.py"));
        std::fs::write(&script, MOCK_PY).unwrap();
        test_env().set(
            "CADENCE_CODEX_COMMAND",
            format!(
                "python3 {} {} {}",
                script.display(),
                pidfile.display(),
                mode
            ),
        );
        MockCodex { pidfile }
    }

    /// Install a mock WebSocket app-server command for `mode`. `dir`
    /// hosts the script + pidfile and must outlive every daemon that
    /// will spawn it (restart tests use the seeded state dir).
    pub fn mock_codex_ws_at(&self, dir: &Path, mode: &str) -> MockCodex {
        let pidfile = dir.join(format!("mock-ws-{mode}.pid"));
        let script = dir.join(format!("mock-ws-{mode}.py"));
        std::fs::write(&script, MOCK_WS_PY).unwrap();
        test_env().set(
            "CADENCE_CODEX_WS_COMMAND",
            format!(
                "python3 {} {} {}",
                script.display(),
                pidfile.display(),
                mode
            ),
        );
        MockCodex { pidfile }
    }

    pub fn mock_codex_ws(&self, mode: &str) -> MockCodex {
        self.mock_codex_ws_at(self.dir.path(), mode)
    }

    pub fn register_codex(&self, alias: &str) {
        self.register_kind(alias, "managed");
    }

    pub fn register_codex_ws(&self, alias: &str) {
        self.register_kind(alias, "managed-ws");
    }

    pub fn register_kind(&self, alias: &str, endpoint_kind: &str) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        self.fixture_rpc(
            "agent_register",
            json!({"alias": alias, "provider": "codex",
                   "endpoint_kind": endpoint_kind, "cwd": cwd}),
        )
        .unwrap();
    }

    pub fn register_codex_params(&self, alias: &str, endpoint_kind: &str, params: Value) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        self.fixture_rpc(
            "agent_register",
            json!({"alias": alias, "provider": "codex",
                   "endpoint_kind": endpoint_kind, "cwd": cwd,
                   "params": params.to_string()}),
        )
        .unwrap();
    }
}

impl Drop for MockCodex {
    fn drop(&mut self) {
        test_env().remove("CADENCE_CODEX_COMMAND");
        test_env().remove("CADENCE_CODEX_WS_COMMAND");
    }
}

/// The mock codex appends one `{"method","params"}` line per
/// `thread/start`/`thread/resume` to `<pidfile>.requests` — read it to
/// assert exactly what reached the wire.
pub fn mock_requests(mock: &MockCodex) -> Vec<Value> {
    std::fs::read_to_string(format!("{}.requests", mock.pidfile.display()))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

pub fn pid_alive(path: &Path) -> bool {
    let Ok(pid) = std::fs::read_to_string(path) else {
        return true; // not written yet -> treat as alive until proven
    };
    PathBuf::from(format!("/proc/{}", pid.trim())).exists()
}

pub fn wait_pid_gone(path: &Path, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while pid_alive(path) {
        assert!(
            Instant::now() < deadline,
            "provider process still alive after {secs}s"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// Poll `agent_probe` until the pane reads idle — a daemon-side cause
/// ordered after the endpoint's own process has exec'd and painted:
/// the mocks write their argv/env dumps BEFORE the first screen paint,
/// so `idle` proves those dumps are on disk. `wait_agent` on `idle`
/// alone only proves the transport opened (spawn happened); under load
/// the child may still be booting. Read launch-shape files only after
/// this — never poll the file itself, whose previous generation's
/// content looks valid while stale.
pub fn wait_probe_idle(d: &TestDaemon, alias: &str, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let probe = d.rpc("agent_probe", json!({"alias": alias})).unwrap();
        if probe["idle"] == true {
            return;
        }
        assert!(Instant::now() < deadline, "probe never read {alias} idle");
        thread::sleep(Duration::from_millis(50));
    }
}

// ---- mock Devin TUI over a mock tmux (no model calls) ----

/// Fake `tmux` speaking just enough of the CLI for the pty adapter.
/// `tmux -L <sock> <cmd> <args>`; per-socket state lives under
/// `<mockdir>/tmux-state/<sock>/`. `new-session` really spawns the pane
/// command (`bash -c`) in its own process group so pane_pid and the
/// /proc lock-descendant checks exercise real ownership logic.
pub const MOCK_TMUX_PY: &str = r##"#!/usr/bin/env python3
import os, re, signal, subprocess, sys, time

args = sys.argv[1:]
if args[0] == "-L":
    sock = args[1]; args = args[2:]
state = os.path.join(os.environ["MOCK_TMUX_STATE"], sock)
os.makedirs(state, exist_ok=True)

def sess_path(name, ext):
    return os.path.join(state, name + "." + ext)

def sess_pid(name):
    try:
        pid = int(open(sess_path(name, "pid")).read().strip())
        os.kill(pid, 0)
        return pid
    except Exception:
        return None

def die(msg, code=1):
    sys.stderr.write(msg + "\n"); sys.exit(code)

def awrite(path, text):
    # Atomic write — a capture-pane reader sees whole content or none.
    tmp = path + ".tmp"
    with open(tmp, "w") as f: f.write(text)
    os.rename(tmp, path)

def aappend(path, text):
    try: cur = open(path).read()
    except OSError: cur = ""
    awrite(path, cur + text)

cmd, rest = args[0], args[1:]
# Every invocation lands in calls.log — tests that must count a probe
# (e.g. `cadence status` probing exactly once per pty agent) read it.
try:
    with open(os.path.join(state, "calls.log"), "a") as f:
        f.write(cmd + " " + " ".join(rest) + "\n")
except Exception:
    pass
# Deterministic latency injection: MOCK_TMUX_HOLD=<secs> delays the
# command named by MOCK_TMUX_HOLD_CMD (default display-message) — the
# harness's way to make an adapter probe straggle past the daemon's
# stop grace without any sleep in test code. MOCK_TMUX_HOLD_FMT narrows
# the hold to calls whose args contain it (e.g. only #{pane_dead}), so
# latency lands on the probe under test instead of every probe.
hold = float(os.environ.get("MOCK_TMUX_HOLD", "0"))
hold_fmt = os.environ.get("MOCK_TMUX_HOLD_FMT", "")
if hold and cmd == os.environ.get("MOCK_TMUX_HOLD_CMD", "display-message") \
        and (not hold_fmt or hold_fmt in rest):
    time.sleep(hold)
# MOCK_TMUX_FAIL=<cmd> makes that subcommand die — deterministic
# failure injection, e.g. a transient capture-pane outage while a
# gate probe runs.
if cmd and cmd == os.environ.get("MOCK_TMUX_FAIL", ""):
    die("mock injected failure")
if cmd == "new-session":
    name = rest[rest.index("-s") + 1]
    cwd = rest[rest.index("-c") + 1] if "-c" in rest else os.getcwd()
    pane_cmd = rest[-1]
    pane = os.path.join(state, name)
    env = dict(os.environ, FAKE_PANE=pane)
    # tmux -e VAR=value exports into the pane process env.
    for i, a in enumerate(rest[:-1]):
        if a == "-e" and "=" in rest[i + 1]:
            k, v = rest[i + 1].split("=", 1)
            env[k] = v
    # Detach the pane's stdio to a file — the adapter's Command::output
    # would otherwise wait on pipes the long-lived pane inherited.
    log = open(sess_path(name, "log"), "ab")
    proc = subprocess.Popen(["bash", "-c", pane_cmd], cwd=cwd, env=env,
                            stdin=subprocess.DEVNULL, stdout=log,
                            stderr=log, start_new_session=True)
    open(sess_path(name, "pid"), "w").write(str(proc.pid))
    open(sess_path(name, "screen"), "a").close()
    sys.exit(0)
if cmd == "has-session":
    name = rest[rest.index("-t") + 1]
    sys.exit(0 if sess_pid(name) else 1)
if cmd == "display-message":
    name = rest[rest.index("-t") + 1]
    fmt = rest[-1]
    pid = sess_pid(name)
    if fmt == "#{pane_pid}":
        # A dead pane keeps its pid (tmux keeps dead panes); emulate.
        try: print(int(open(sess_path(name, "pid")).read().strip()))
        except Exception: die("no such session")
    elif fmt == "#{pane_dead}":
        print("0" if pid else "1")
    elif fmt == "#{pane_in_mode}":
        try: print(open(sess_path(name, "mode")).read().strip() or "0")
        except FileNotFoundError: print("0")
    elif fmt == "#{cursor_x},#{cursor_y}":
        # The cursor sits on the TUI's input line: column 2 (right
        # after the prompt glyph + space) when empty, or after the
        # staged draft. Row = the rendered input row — the staged line
        # capture-pane appends when text is staged, else the last
        # prompt-glyph row of the screen.
        try: staged = open(sess_path(name, "input")).read()
        except FileNotFoundError: staged = ""
        try: rows = open(sess_path(name, "screen")).read().splitlines()
        except FileNotFoundError: rows = []
        if staged:
            print("%d,%d" % (2 + len(staged), len(rows)))
        else:
            y = max((i for i, l in enumerate(rows)
                     if l.strip().startswith(("❯", "❭", "»"))), default=0)
            print("2,%d" % y)
    else: die("unknown format " + fmt)
    sys.exit(0)
if cmd == "capture-pane":
    name = rest[rest.index("-t") + 1]
    # Count captures so tests can prove the stall ticker only samples
    # panes while a turn is running.
    try:
        with open(sess_path(name, "captures"), "a") as f: f.write("c")
    except OSError: pass
    out = ""
    try: out += open(sess_path(name, "screen")).read()
    except FileNotFoundError: die("no such session")
    # The input line renders like the TUI's own: the pane's `.glyph`
    # file (written by its TUI; `❭` is the Devin default) + staged draft.
    try:
        staged = open(sess_path(name, "input")).read()
        if staged:
            try: glyph = open(sess_path(name, "glyph")).read().strip() or "❭"
            except FileNotFoundError: glyph = "❭"
            out += glyph + " " + staged + "\n"
    except FileNotFoundError: pass
    # Test-controlled extra screen content — a file the test writes to
    # make the pane look busy, approval-blocked, etc. A `tui-once` file
    # replaces it for exactly one capture: the rename claims it
    # atomically, so no second capture can see it however the test's
    # writes interleave with this read.
    once = sess_path(name, "tui-once")
    claimed = "%s.%d" % (once, os.getpid())
    try:
        os.rename(once, claimed)
        out += open(claimed).read()
        os.unlink(claimed)
    except FileNotFoundError:
        try: out += open(sess_path(name, "tui-state")).read()
        except FileNotFoundError: pass
    # Real tmux only prints the pane with `-p` — without it the capture
    # lands in the paste buffer and stdout stays empty. Emulate that so
    # a dropped `-p` fails loudly here the way it does on a real pane.
    if "-p" not in rest:
        sys.exit(0)
    # Real tmux keeps SGR attributes (and OSC 8 links) only with `-e`;
    # a plain capture drops them. Screen files may carry a styled frame.
    if "-e" not in rest:
        out = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)",
                     "", out)
    sys.stdout.write(out); sys.exit(0)
if cmd == "load-buffer":
    open(os.path.join(state, "buffer"), "w").write(open(rest[-1]).read())
    sys.exit(0)
if cmd == "paste-buffer":
    name = rest[rest.index("-t") + 1]
    # A `.swallow` file models a busy TUI dropping the bracketed paste:
    # the write path "works" but the text never reaches the screen.
    if not os.path.exists(sess_path(name, "swallow")):
        aappend(sess_path(name, "input"),
                open(os.path.join(state, "buffer")).read())
    sys.exit(0)
if cmd == "send-keys":
    name = rest[rest.index("-t") + 1]
    for key in rest[rest.index("-t") + 2:]:
        if key == "--":  # ends tmux option parsing — not a key
            continue
        aappend(sess_path(name, "input"),
                "<ENTER>" if key == "Enter" else "<KEY:" + key + ">")
    sys.exit(0)
if cmd == "set-option":
    # Record option writes so tests can assert pane defaults.
    with open(os.path.join(state, "setopt.log"), "a") as f:
        f.write(" ".join(rest) + "\n")
    sys.exit(0)
if cmd == "kill-session":
    name = rest[rest.index("-t") + 1]
    pid = sess_pid(name)
    if pid:
        try: os.killpg(pid, signal.SIGKILL)
        except ProcessLookupError: pass
    sys.exit(0)
if cmd == "list-clients":
    # Attached terminal clients: one tty per line of `<session>.clients`
    # (absent = none attached). An unknown session fails like tmux.
    name = rest[rest.index("-t") + 1].lstrip("=")
    if not sess_pid(name):
        die("can't find session: " + name)
    try: sys.stdout.write(open(sess_path(name, "clients")).read())
    except OSError: pass
    sys.exit(0)
die("unhandled tmux cmd " + cmd)
"##;

/// Fake `devin` TUI: takes the real session lock (`flock`, visible via
/// /proc/fd to the adapter's ownership scan), mirrors the pane input
/// file, and answers an `<ENTER>`-terminated paste by writing the
/// submitted line and a `MOCK_REPLY` to the screen file.
/// `$FAKE_PANE` (set by the mock tmux) points at the session state.
pub const MOCK_DEVIN_PY: &str = r#"
import fcntl, json, os, socket, sys, time

locks = sys.argv[1]
sid = sys.argv[sys.argv.index("-r") + 1] if "-r" in sys.argv else \
    "mock-session-%d" % os.getpid()
os.makedirs(locks, exist_ok=True)
lf = open(os.path.join(locks, sid + ".lock"), "a")
try:
    fcntl.flock(lf, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    print("session_locked: %s" % sid); sys.exit(1)
open(os.environ["FAKE_PANE"] + ".sid", "w").write(sid)
# Record the launch argv — tests assert flags are replayed on resume.
# Temp + rename so a reader never sees the file torn mid-write.
_argv_tmp = os.environ["FAKE_PANE"] + ".argv.tmp"
open(_argv_tmp, "w").write("\n".join(sys.argv))
os.rename(_argv_tmp, os.environ["FAKE_PANE"] + ".argv")
# Record the pane env the adapter exported via tmux -e.
_env_tmp = os.environ["FAKE_PANE"] + ".env.tmp"
open(_env_tmp, "w").write(
    "CADENCE_ALIAS=%s\nCADENCE_STATE_DIR=%s\n" % (
        os.environ.get("CADENCE_ALIAS", ""),
        os.environ.get("CADENCE_STATE_DIR", "")))
os.rename(_env_tmp, os.environ["FAKE_PANE"] + ".env")
def awrite(path, text):
    # Atomic write — a capture-pane reader sees whole content or none.
    tmp = path + ".tmp"
    with open(tmp, "w") as f: f.write(text)
    os.rename(tmp, path)
def aappend(path, text):
    try: cur = open(path).read()
    except OSError: cur = ""
    awrite(path, cur + text)
def memory_rpc():
    # Test-only bridge: the lockholding provider process opens the real
    # daemon socket, so SO_PEERCRED and /proc ancestry see this pane rather
    # than the integration-test process.  The request/response files are
    # opt-in and scoped to this mock pane; production providers have no such
    # bridge.
    req_path = os.environ["FAKE_PANE"] + ".memory-rpc"
    try:
        raw = open(req_path).read()
    except FileNotFoundError:
        return
    try:
        request = json.loads(raw)
        sock_path = os.path.join(os.environ["CADENCE_STATE_DIR"], "cadence.sock")
        chunks = []
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(15)
            sock.connect(sock_path)
            sock.sendall((json.dumps(request, separators=(",", ":")) + "\n").encode())
            while True:
                chunk = sock.recv(65536)
                if not chunk:
                    break
                chunks.append(chunk)
                if b"\n" in chunk:
                    break
        response = json.loads(b"".join(chunks).split(b"\n", 1)[0].decode())
    except Exception as exc:
        response = {"ok": False, "error": {"kind": "internal", "message": str(exc)}}
    try:
        os.unlink(req_path)
    except FileNotFoundError:
        pass
    awrite(os.environ["FAKE_PANE"] + ".memory-rpc.response", json.dumps(response))
aappend(os.environ["FAKE_PANE"] + ".screen",
        "Mock Devin TUI [%s]\n" % sid +
        # An idle input line — the same shape the real TUI shows so the
        # screen probe recognizes an empty prompt.
        "❭ Ask Devin to build features, fix bugs, or work on your code\n")
while True:
    memory_rpc()
    inp = os.environ["FAKE_PANE"] + ".input"
    try:
        data = open(inp).read()
    except FileNotFoundError:
        data = ""
    if "<ENTER>" in data:
        text, rest = data.split("<ENTER>", 1)
        if os.path.exists(os.environ["FAKE_PANE"] + ".hold-enter"):
            # Enter swallowed: the marker is consumed but the draft
            # stays staged in the input line, unsubmitted.
            awrite(inp, text + rest)
        else:
            awrite(inp, rest)
            if text.strip():
                aappend(os.environ["FAKE_PANE"] + ".screen",
                        "> %s\nMOCK_REPLY: %s\n" % (text.strip(), text.strip()))
    # C-c, or Esc (the Claude/Devin interrupt key, CAD-323): the turn
    # stops and the staged input clears.
    if "<KEY:C-c>" in data or "<KEY:Escape>" in data:
        awrite(inp, "")
        aappend(os.environ["FAKE_PANE"] + ".screen", "^C interrupt\n")
    time.sleep(0.05)
"#;

pub struct MockDevin {
    pub _guard: std::sync::MutexGuard<'static, ()>,
    pub dir: PathBuf,
    pub locks: PathBuf,
}

/// Install the mock tmux/devin pair. Set the env overrides BEFORE a
/// daemon starts so its auto-relaunch sees them.
pub fn install_mock_devin(dir: &Path) -> MockDevin {
    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let locks = dir.join("devin-locks");
    let tmux_state = dir.join("tmux-state");
    std::fs::create_dir_all(&locks).unwrap();
    std::fs::create_dir_all(&tmux_state).unwrap();
    let tmux = dir.join("tmux");
    let devin_py = dir.join("mock-devin.py");
    std::fs::write(&tmux, MOCK_TMUX_PY).unwrap();
    std::fs::write(&devin_py, MOCK_DEVIN_PY).unwrap();
    // The adapter execs the tmux binary directly (no shell), so the
    // mock must be executable.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Real env, under this mock's ENV_LOCK: the mock tmux runs as a fresh
    // child per call and finds its state only through inherited env.
    std::env::set_var("MOCK_TMUX_STATE", &tmux_state);
    test_env().set("CADENCE_TMUX_COMMAND", tmux.display().to_string());
    test_env().set(
        "CADENCE_DEVIN_COMMAND",
        format!("python3 {} {}", devin_py.display(), locks.display()),
    );
    test_env().set("CADENCE_DEVIN_LOCKS", locks.display().to_string());
    MockDevin {
        _guard: guard,
        dir: dir.to_path_buf(),
        locks,
    }
}

impl TestDaemon {
    /// Install the mock tmux/devin pair for `dir` (which must outlive
    /// every daemon that will launch panes) and return their paths.
    pub fn mock_devin_at(&self, dir: &Path) -> MockDevin {
        install_mock_devin(dir)
    }

    pub fn mock_devin(&self) -> MockDevin {
        self.mock_devin_at(self.dir.path())
    }

    pub fn register_devin(&self, alias: &str, session: Option<&str>) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        let params = session.map(|s| json!({"session": s}).to_string());
        self.fixture_rpc(
            "agent_register",
            json!({"alias": alias, "provider": "devin",
                   "endpoint_kind": "pty", "cwd": cwd, "params": params}),
        )
        .unwrap();
    }

    /// The tmux-side state dir for a given agent session name.
    pub fn pane_file(&self, mock: &MockDevin, alias: &str, ext: &str) -> PathBuf {
        // The adapter derives its socket name from the state dir.
        mock.dir
            .join("tmux-state")
            .join(socket_for(&self.state))
            .join(format!("{alias}.{ext}"))
    }

    /// Ask a live mock Devin pane to issue one daemon RPC over the real
    /// Unix socket. The Python process owns the request connection, so the
    /// daemon sees its actual SO_PEERCRED pid and /proc ancestry.
    pub fn memory_rpc(
        &self,
        mock: &MockDevin,
        alias: &str,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, String> {
        let request_path = self.pane_file(mock, alias, "memory-rpc");
        let response_path = self.pane_file(mock, alias, "memory-rpc.response");
        let _ = std::fs::remove_file(&response_path);
        let request = json!({"method": method, "params": params});
        let temporary = request_path.with_extension("memory-rpc.tmp");
        std::fs::write(&temporary, request.to_string()).map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, &request_path).map_err(|e| e.to_string())?;

        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(text) = std::fs::read_to_string(&response_path) {
                let frame: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                if frame["ok"] == true {
                    return Ok(frame["result"].clone());
                }
                let message = frame
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("memory RPC refused")
                    .to_string();
                return Err(message);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "memory RPC from {alias} timed out; request={request_path:?}"
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl TestDaemon {
    pub fn mock_stub(&self) -> MockStub {
        install_mock_stub(self.dir.path())
    }

    /// Register a pty agent on the stub (test-double) profile.
    pub fn register_stub(&self, alias: &str, params: Value) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        self.fixture_rpc(
            "agent_register",
            json!({"alias": alias, "provider": "tui-stub",
                   "endpoint_kind": "pty", "cwd": cwd,
                   "params": params.to_string()}),
        )
        .unwrap();
    }

    /// The tmux-side state dir for a stub-pane agent session name.
    pub fn stub_pane_file(&self, mock: &MockStub, alias: &str, ext: &str) -> PathBuf {
        mock.dir
            .join("tmux-state")
            .join(socket_for(&self.state))
            .join(format!("{alias}.{ext}"))
    }
}

/// Temp-file + rename write: a concurrent `capture-pane` (or mock TUI
/// loop) sees whole content or none — no torn mid-write reads, so the
/// stall sampler only ever hashes a real screen.
pub fn atomic_write(path: PathBuf, contents: impl AsRef<[u8]>) {
    let tmp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name().unwrap().to_string_lossy()
    ));
    std::fs::write(&tmp, contents).unwrap();
    std::fs::rename(&tmp, &path).unwrap();
}

/// Panes legitimately outlive a daemon (shutdown detaches), so clean
/// any survivors ourselves by their recorded pane pids.
pub fn kill_mock_panes(dir: &Path) {
    if let Ok(socks) = std::fs::read_dir(dir.join("tmux-state")) {
        for sock in socks.flatten() {
            if let Ok(files) = std::fs::read_dir(sock.path()) {
                for f in files.flatten() {
                    if f.file_name().to_string_lossy().ends_with(".pid") {
                        if let Ok(pid) = std::fs::read_to_string(f.path())
                            .unwrap_or_default()
                            .trim()
                            .parse::<i32>()
                        {
                            unsafe { libc::killpg(pid, libc::SIGKILL) };
                        }
                    }
                }
            }
        }
    }
}

impl Drop for MockDevin {
    fn drop(&mut self) {
        kill_mock_panes(&self.dir);
        test_env().remove("CADENCE_TMUX_COMMAND");
        test_env().remove("CADENCE_DEVIN_COMMAND");
        test_env().remove("CADENCE_DEVIN_LOCKS");
        std::env::remove_var("MOCK_TMUX_STATE");
    }
}

/// Fake `stub` TUI — the second profile's endpoint: different prompt
/// glyph (`»`), different placeholder, different session-lock dir. It
/// proves the adapter's gate/render/claim mechanics come from the
/// profile, not from Devin-shaped constants. Same contract as
/// MOCK_DEVIN_PY: flock `<locks>/<sid>.lock`, `-r` resumes.
pub const MOCK_STUB_PY: &str = r#"
import fcntl, os, sys, time

locks = sys.argv[1]
sid = sys.argv[sys.argv.index("-r") + 1] if "-r" in sys.argv else \
    "stub-session-%d" % os.getpid()
os.makedirs(locks, exist_ok=True)
lf = open(os.path.join(locks, sid + ".lock"), "a")
try:
    fcntl.flock(lf, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    print("session_locked: %s" % sid); sys.exit(1)
open(os.environ["FAKE_PANE"] + ".sid", "w").write(sid)
# The pane's own input-line glyph — the mock tmux renders staged text
# with it, so a staged draft reads as this TUI's prompt line.
open(os.environ["FAKE_PANE"] + ".glyph", "w").write("»")
def awrite(path, text):
    # Atomic write — a capture-pane reader sees whole content or none.
    tmp = path + ".tmp"
    with open(tmp, "w") as f: f.write(text)
    os.rename(tmp, path)
def aappend(path, text):
    try: cur = open(path).read()
    except OSError: cur = ""
    awrite(path, cur + text)
aappend(os.environ["FAKE_PANE"] + ".screen",
        "Mock Stub TUI [%s]\n" % sid +
        # The stub profile's empty-prompt signature.
        "» stub ready\n")
while True:
    inp = os.environ["FAKE_PANE"] + ".input"
    try:
        data = open(inp).read()
    except FileNotFoundError:
        data = ""
    if "<ENTER>" in data:
        text, rest = data.split("<ENTER>", 1)
        if os.path.exists(os.environ["FAKE_PANE"] + ".hold-enter"):
            # Enter swallowed: the marker is consumed but the draft
            # stays staged in the input line, unsubmitted.
            awrite(inp, text + rest)
        else:
            awrite(inp, rest)
            if text.strip():
                aappend(os.environ["FAKE_PANE"] + ".screen",
                        "> %s\nSTUB_REPLY: %s\n" % (text.strip(), text.strip()))
    time.sleep(0.05)
"#;

pub struct MockStub {
    pub _guard: std::sync::MutexGuard<'static, ()>,
    pub dir: PathBuf,
    pub locks: PathBuf,
}

/// Install the mock tmux/stub pair — the same private-tmux harness as
/// `install_mock_devin`, pointing the adapter at the stub profile's env
/// overrides instead.
pub fn install_mock_stub(dir: &Path) -> MockStub {
    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let locks = dir.join("stub-locks");
    let tmux_state = dir.join("tmux-state");
    std::fs::create_dir_all(&locks).unwrap();
    std::fs::create_dir_all(&tmux_state).unwrap();
    let tmux = dir.join("tmux");
    let stub_py = dir.join("mock-stub.py");
    std::fs::write(&tmux, MOCK_TMUX_PY).unwrap();
    std::fs::write(&stub_py, MOCK_STUB_PY).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Real env, under this mock's ENV_LOCK: the mock tmux runs as a fresh
    // child per call and finds its state only through inherited env.
    std::env::set_var("MOCK_TMUX_STATE", &tmux_state);
    test_env().set("CADENCE_TMUX_COMMAND", tmux.display().to_string());
    test_env().set(
        "CADENCE_STUB_COMMAND",
        format!("python3 {} {}", stub_py.display(), locks.display()),
    );
    test_env().set("CADENCE_STUB_LOCKS", locks.display().to_string());
    MockStub {
        _guard: guard,
        dir: dir.to_path_buf(),
        locks,
    }
}

impl Drop for MockStub {
    fn drop(&mut self) {
        kill_mock_panes(&self.dir);
        test_env().remove("CADENCE_TMUX_COMMAND");
        test_env().remove("CADENCE_STUB_COMMAND");
        test_env().remove("CADENCE_STUB_LOCKS");
        std::env::remove_var("MOCK_TMUX_STATE");
    }
}

/// Fake `claude` TUI — the Claude profile's endpoint. Instead of a
/// session lock it publishes the real registry's shape:
/// `<sessions>/<pid>.json` = `{"pid", "sessionId", "cwd", "procStart"}`.
/// `--session-id`/`--resume` arrive as argv (the profile appends them
/// to the verbatim override); MOCK_CLAUDE_SWAP makes the registry
/// claim a different session than asked — a changed-owner fence.
/// `$FAKE_PANE` (set by the mock tmux) points at the session state.
pub const MOCK_CLAUDE_TUI_PY: &str = r#"
import json, os, sys, time

sessions = sys.argv[1]
if "--resume" in sys.argv:
    sid = sys.argv[sys.argv.index("--resume") + 1]
elif "--session-id" in sys.argv:
    sid = sys.argv[sys.argv.index("--session-id") + 1]
else:
    sid = "mock-claude-%d" % os.getpid()
os.makedirs(sessions, exist_ok=True)
# Record the launch argv so tests can assert the profile's flags —
# temp + rename so a reader never sees the file torn mid-write.
_argv_tmp = os.environ["FAKE_PANE"] + ".argv.tmp"
open(_argv_tmp, "w").write(" ".join(sys.argv[1:]))
os.rename(_argv_tmp, os.environ["FAKE_PANE"] + ".argv")
pid = os.getpid()
# /proc/self/stat field 22 — what the real registry's procStart is.
try:
    stat = open("/proc/self/stat").read()
    proc_start = stat[stat.rindex(")") + 1:].split()[19]
except Exception:
    proc_start = None
entry = {"pid": pid, "sessionId": sid, "cwd": os.getcwd(),
         "procStart": proc_start, "kind": "interactive"}
if os.environ.get("MOCK_CLAUDE_SWAP"):
    entry["sessionId"] = "swapped-" + sid
# MOCK_CLAUDE_NO_REGISTRY keeps the pane alive but never publishes the
# session — the adapter's open wait then runs to its deadline, the
# transient-proof-timeout shape a Claude resume must survive.
if not os.environ.get("MOCK_CLAUDE_NO_REGISTRY"):
    open(os.path.join(sessions, "%d.json" % pid), "w").write(json.dumps(entry))
# Record the pane env the adapter exported via tmux -e.
_env_tmp = os.environ["FAKE_PANE"] + ".env.tmp"
open(_env_tmp, "w").write(
    "CADENCE_ALIAS=%s\nCADENCE_STATE_DIR=%s\n" % (
        os.environ.get("CADENCE_ALIAS", ""),
        os.environ.get("CADENCE_STATE_DIR", "")))
os.rename(_env_tmp, os.environ["FAKE_PANE"] + ".env")
# The pane's own input-line glyph + a boxed empty prompt — the shape
# the real TUI shows so the screen probe recognizes idle.
open(os.environ["FAKE_PANE"] + ".glyph", "w").write("❯")
def awrite(path, text):
    # Atomic write — a capture-pane reader sees whole content or none.
    tmp = path + ".tmp"
    with open(tmp, "w") as f: f.write(text)
    os.rename(tmp, path)
def aappend(path, text):
    try: cur = open(path).read()
    except OSError: cur = ""
    awrite(path, cur + text)
aappend(os.environ["FAKE_PANE"] + ".screen",
        "Mock Claude TUI [%s]\n" % sid +
        "  [Opus] mock-mode on\n" +
        "─" * 40 + "\n❯ \n" + "─" * 40 + "\n")
while True:
    inp = os.environ["FAKE_PANE"] + ".input"
    try:
        data = open(inp).read()
    except FileNotFoundError:
        data = ""
    if "<ENTER>" in data:
        text, rest = data.split("<ENTER>", 1)
        awrite(inp, rest)
        if text.strip():
            # The submitted line echoes into the transcript and the
            # box re-renders empty below it.
            aappend(os.environ["FAKE_PANE"] + ".screen",
                    "> %s\nMOCK_REPLY: %s\n" % (text.strip(), text.strip()) +
                    "─" * 40 + "\n❯ \n" + "─" * 40 + "\n")
    # C-c, or Esc (the Claude/Devin interrupt key, CAD-323): the turn
    # stops and the staged input clears.
    if "<KEY:C-c>" in data or "<KEY:Escape>" in data:
        awrite(inp, "")
        aappend(os.environ["FAKE_PANE"] + ".screen", "^C interrupt\n")
    time.sleep(0.05)
"#;

pub struct MockClaudeTui {
    pub _guard: std::sync::MutexGuard<'static, ()>,
    pub dir: PathBuf,
    pub sessions: PathBuf,
}

/// Install the mock tmux/claude pair — the same private-tmux harness as
/// `install_mock_devin`, pointing the adapter at the claude profile's
/// env overrides instead.
pub fn install_mock_claude_tui(dir: &Path) -> MockClaudeTui {
    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sessions = dir.join("claude-sessions");
    let tmux_state = dir.join("tmux-state");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::create_dir_all(&tmux_state).unwrap();
    let tmux = dir.join("tmux");
    let claude_py = dir.join("mock-claude.py");
    std::fs::write(&tmux, MOCK_TMUX_PY).unwrap();
    std::fs::write(&claude_py, MOCK_CLAUDE_TUI_PY).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Real env, under this mock's ENV_LOCK: the mock tmux runs as a fresh
    // child per call and finds its state only through inherited env.
    std::env::set_var("MOCK_TMUX_STATE", &tmux_state);
    test_env().set("CADENCE_TMUX_COMMAND", tmux.display().to_string());
    test_env().set(
        "CADENCE_CLAUDE_TUI_COMMAND",
        format!("python3 {} {}", claude_py.display(), sessions.display()),
    );
    test_env().set("CADENCE_CLAUDE_SESSIONS", sessions.display().to_string());
    MockClaudeTui {
        _guard: guard,
        dir: dir.to_path_buf(),
        sessions,
    }
}

impl TestDaemon {
    pub fn mock_claude_tui(&self) -> MockClaudeTui {
        install_mock_claude_tui(self.dir.path())
    }

    /// Register a pty agent on the claude profile.
    pub fn register_claude_pty(&self, alias: &str, params: Value) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        self.fixture_rpc(
            "agent_register",
            json!({"alias": alias, "provider": "claude",
                   "endpoint_kind": "pty", "cwd": cwd,
                   "params": params.to_string()}),
        )
        .unwrap();
    }

    /// The tmux-side state dir for a claude-pane agent session name.
    pub fn claude_pane_file(&self, mock: &MockClaudeTui, alias: &str, ext: &str) -> PathBuf {
        mock.dir
            .join("tmux-state")
            .join(socket_for(&self.state))
            .join(format!("{alias}.{ext}"))
    }
}

impl Drop for MockClaudeTui {
    fn drop(&mut self) {
        kill_mock_panes(&self.dir);
        test_env().remove("CADENCE_TMUX_COMMAND");
        test_env().remove("CADENCE_CLAUDE_TUI_COMMAND");
        test_env().remove("CADENCE_CLAUDE_SESSIONS");
        std::env::remove_var("MOCK_CLAUDE_SWAP");
        std::env::remove_var("MOCK_CLAUDE_NO_REGISTRY");
        std::env::remove_var("MOCK_TMUX_STATE");
    }
}

/// Fake `cursor-agent` TUI — the Cursor profile's endpoint. The real
/// TUI holds an open fd on `~/.cursor/chats/<hash>/<chat>/store.db`
/// for the session's whole life, so the mock opens the same file and
/// keeps it — the profile's ownership scan finds it under
/// `<chats>/mockhash/<chat>/store.db`. `create-chat` mints the id a
/// fresh launch resumes; `--resume` arrives as argv (the profile
/// appends it to the verbatim override); MOCK_CURSOR_SWAP makes the
/// pane open a different chat than asked — a changed-owner fence.
/// `$FAKE_PANE` (set by the mock tmux) points at the session state.
pub const MOCK_CURSOR_TUI_PY: &str = r#"
import os, sys, time, uuid

chats = sys.argv[1]
if "create-chat" in sys.argv:
    print(uuid.uuid4()); sys.exit(0)
sid = sys.argv[sys.argv.index("--resume") + 1] if "--resume" in sys.argv \
    else "missing-resume"
if os.environ.get("MOCK_CURSOR_SWAP"):
    sid = "swapped-" + sid
# A chat named by MOCK_CURSOR_DIE_ON is unresumable — the real TUI
# exits on a deleted/foreign chat, so the mock does too.
if sid == os.environ.get("MOCK_CURSOR_DIE_ON"):
    sys.exit(1)
chat_dir = os.path.join(chats, "mockhash", sid)
os.makedirs(chat_dir, exist_ok=True)
# The real TUI holds an fd on the chat's store.db for its whole life —
# the profile's ownership proof scans /proc fds for exactly this.
db = open(os.path.join(chat_dir, "store.db"), "a")
# Record the launch argv so tests can assert the profile's flags —
# temp + rename so a reader never sees the file torn mid-write.
_argv_tmp = os.environ["FAKE_PANE"] + ".argv.tmp"
open(_argv_tmp, "w").write(" ".join(sys.argv[1:]))
os.rename(_argv_tmp, os.environ["FAKE_PANE"] + ".argv")
# Record the pane env the adapter exported via tmux -e.
_env_tmp = os.environ["FAKE_PANE"] + ".env.tmp"
open(_env_tmp, "w").write(
    "CADENCE_ALIAS=%s\nCADENCE_STATE_DIR=%s\n" % (
        os.environ.get("CADENCE_ALIAS", ""),
        os.environ.get("CADENCE_STATE_DIR", "")))
os.rename(_env_tmp, os.environ["FAKE_PANE"] + ".env")
# The pane's own input-line glyph + the idle frame — the shape the real
# TUI shows so the screen probe recognizes idle.
open(os.environ["FAKE_PANE"] + ".glyph", "w").write("→")
with open(os.environ["FAKE_PANE"] + ".screen", "a") as f:
    f.write("Mock Cursor TUI [%s]\n" % sid)
    f.write("  → Plan, search, build anything\n")
    f.write("  Cursor Grok 4.6 High\n  /mock · main\n")
while True:
    inp = os.environ["FAKE_PANE"] + ".input"
    try:
        data = open(inp).read()
    except FileNotFoundError:
        data = ""
    if "<ENTER>" in data:
        text, rest = data.split("<ENTER>", 1)
        open(inp, "w").write(rest)
        if text.strip():
            with open(os.environ["FAKE_PANE"] + ".screen", "a") as f:
                f.write("  %s\nMOCK_REPLY: %s\n" % (text.strip(), text.strip()))
                # The submitted line echoes into the transcript and the
                # input's watermark flips to the follow-up form.
                f.write("  → Add a follow-up\n")
                f.write("  Cursor Grok 4.6 High\n  /mock · main\n")
    if "<KEY:C-c>" in data:
        open(inp, "w").write("")
        with open(os.environ["FAKE_PANE"] + ".screen", "a") as f:
            f.write("^C interrupt\n")
    time.sleep(0.05)
"#;

pub struct MockCursorTui {
    pub _guard: std::sync::MutexGuard<'static, ()>,
    pub dir: PathBuf,
    pub chats: PathBuf,
    /// The owning daemon's state dir, when the mock was installed
    /// through `TestDaemon::mock_cursor_tui`. Drop shuts the daemon
    /// down BEFORE clearing the env overrides: an in-process daemon
    /// that outlives the mock could still rebuild a profile in its
    /// teardown window, and a profile built without
    /// `CADENCE_CURSOR_CHATS` resolves the real `~/.cursor` — scans
    /// the user's real chats, and would merge `Shell(cadence)` into
    /// the real `cli-config.json`.
    pub state: Option<PathBuf>,
}

pub fn install_mock_cursor_tui_inner(dir: &Path, state: Option<PathBuf>) -> MockCursorTui {
    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let chats = dir.join("cursor-chats");
    let tmux_state = dir.join("tmux-state");
    std::fs::create_dir_all(&chats).unwrap();
    std::fs::create_dir_all(&tmux_state).unwrap();
    let tmux = dir.join("tmux");
    let cursor_py = dir.join("mock-cursor.py");
    std::fs::write(&tmux, MOCK_TMUX_PY).unwrap();
    std::fs::write(&cursor_py, MOCK_CURSOR_TUI_PY).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    // A `cursor-agent`-named symlink onto python3: the pane's argv[0]
    // then names the real binary, so the profile's `--resume` argv
    // proof (not only the store.db fd) is exercised in integration.
    let out = std::process::Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    let python = String::from_utf8(out.stdout).unwrap().trim().to_string();
    let cursor_bin = dir.join("cursor-agent");
    std::os::unix::fs::symlink(&python, &cursor_bin).unwrap();
    // Real env, under this mock's ENV_LOCK: the mock tmux runs as a fresh
    // child per call and finds its state only through inherited env.
    std::env::set_var("MOCK_TMUX_STATE", &tmux_state);
    test_env().set("CADENCE_TMUX_COMMAND", tmux.display().to_string());
    test_env().set(
        "CADENCE_CURSOR_COMMAND",
        format!(
            "{} {} {}",
            cursor_bin.display(),
            cursor_py.display(),
            chats.display()
        ),
    );
    test_env().set("CADENCE_CURSOR_CHATS", chats.display().to_string());
    // A swap/die-on set by an earlier test must not leak into this
    // install.
    std::env::remove_var("MOCK_CURSOR_SWAP");
    std::env::remove_var("MOCK_CURSOR_DIE_ON");
    MockCursorTui {
        _guard: guard,
        dir: dir.to_path_buf(),
        chats,
        state,
    }
}

impl TestDaemon {
    pub fn mock_cursor_tui(&self) -> MockCursorTui {
        install_mock_cursor_tui_inner(self.dir.path(), Some(self.state.clone()))
    }

    /// Register a pty agent on the cursor profile.
    pub fn register_cursor_pty(&self, alias: &str, params: Value) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        self.fixture_rpc(
            "agent_register",
            json!({"alias": alias, "provider": "cursor",
                   "endpoint_kind": "pty", "cwd": cwd,
                   "params": params.to_string()}),
        )
        .unwrap();
    }

    /// The tmux-side state dir for a cursor-pane agent session name.
    pub fn cursor_pane_file(&self, mock: &MockCursorTui, alias: &str, ext: &str) -> PathBuf {
        mock.dir
            .join("tmux-state")
            .join(socket_for(&self.state))
            .join(format!("{alias}.{ext}"))
    }
}

impl Drop for MockCursorTui {
    fn drop(&mut self) {
        // The daemon goes first: while it lives, a rebuilt profile
        // must still see the overrides. `TestDaemon::drop` re-runs
        // shutdown idempotently and only joins the thread.
        if let Some(state) = &self.state {
            let _ = client::rpc(state, "shutdown", json!({}));
            let deadline = Instant::now() + Duration::from_secs(10);
            while client::rpc(state, "health", json!({})).is_ok() {
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        kill_mock_panes(&self.dir);
        test_env().remove("CADENCE_TMUX_COMMAND");
        test_env().remove("CADENCE_CURSOR_COMMAND");
        test_env().remove("CADENCE_CURSOR_CHATS");
        std::env::remove_var("MOCK_CURSOR_SWAP");
        std::env::remove_var("MOCK_CURSOR_DIE_ON");
        std::env::remove_var("MOCK_TMUX_STATE");
    }
}

/// Mirror of the adapter's `cadence-<fnv64(state_dir)>` socket name.
pub fn socket_for(state_dir: &Path) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in state_dir.to_string_lossy().as_bytes() {
        h = (h ^ u64::from(*b)).wrapping_mul(0x100000001b3);
    }
    format!("cadence-{h:016x}")
}

pub fn pty_token(d: &TestDaemon, alias: &str, id: &str) -> String {
    d.wait_message(alias, id, &["running"], 20);
    running_token(d, id)
}

/// A running message's turn token, read from the store: the daemon
/// shows it only to the owning agent's own pane or endpoint (CAD-375),
/// and the test process plays that worker's report without being it.
pub fn running_token(d: &TestDaemon, id: &str) -> String {
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.query_row("SELECT turn_id FROM messages WHERE id=?1", [id], |r| {
        r.get::<_, String>(0)
    })
    .unwrap()
}

/// Report a pty turn's result and wait for it to complete. Since CAD-250
/// an actor holds one report-owing turn at a time, so a test that sends
/// a second task reports the first before the next can be claimed.
pub fn pty_report_done(d: &TestDaemon, alias: &str, id: &str) {
    let token = pty_token(d, alias, id);
    d.rpc(
        "message_report",
        json!({"message": id, "token": token, "kind": "result", "text": "done"}),
    )
    .unwrap();
    d.wait_message(alias, id, &["completed"], 10);
}

// ---- CAD-162: turn tokens are judged by the endpoint's own scheme ----

/// A fresh 32-hex generation that no endpoint minted.
pub const CAD162_OTHER_GEN: &str = "fedcba9876543210fedcba9876543210";

pub fn cad162_sql(d: &TestDaemon, sql: &str, params: &[&str]) {
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(sql, rusqlite::params_from_iter(params.iter()))
        .unwrap();
}

/// Point message `id`'s running turn at `token` (what a report must
/// match first), leaving everything else as the daemon wrote it.
pub fn cad162_set_turn(d: &TestDaemon, id: &str, token: &str) {
    cad162_sql(
        d,
        "UPDATE messages SET turn_id=?1 WHERE id=?2",
        &[token, id],
    );
}

pub fn cad162_message(d: &TestDaemon, alias: &str, id: &str) -> Value {
    d.rpc("agent_show", json!({"alias": alias})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("no message {id} on {alias}"))
        .clone()
}

/// Both report kinds with `token` are refused as stale, and the message
/// is untouched: still `running` under `token` with no ack recorded.
pub fn cad162_assert_refused(d: &TestDaemon, alias: &str, id: &str, token: &str, what: &str) {
    let before = cad162_message(d, alias, id);
    for kind in ["ack", "result"] {
        let err = d
            .rpc(
                "message_report",
                json!({"message": id, "token": token, "kind": kind, "text": "forged"}),
            )
            .expect_err(&format!("{what}: {kind} with {token} accepted as current"));
        assert!(
            err.to_string().contains("stale endpoint generation"),
            "{what}: {err}"
        );
    }
    let after = cad162_message(d, alias, id);
    assert_eq!(after["state"], "running", "{what}: {after}");
    // The token itself is read from the store: the daemon withholds a
    // running turn's token from every connection but its agent's (CAD-375).
    assert_eq!(running_token(d, id), token, "{what}: {after}");
    assert_eq!(after["result"], before["result"], "{what}: {after}");
    assert!(after["result"]["ack"].is_null(), "{what}: {after}");
}

/// `git init` + one empty commit so `worktree add -b` has a HEAD.
pub fn git_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(&args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
    }
}

/// `git status --porcelain` — empty means the repo is byte-identical
/// to its index+HEAD (audit N8's launch-purity check).
pub fn git_porcelain(repo: &Path) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(out.status.success(), "git status: {:?}", out.stderr);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Spawn the real `cadence` binary under a scratch HOME (skill install
/// targets `$HOME` directly — no daemon involved).
pub fn hold_rollout_lease(home: &Path, state: &Path) {
    // An `--as` claim must be provably the operator (CAD-384): claim from
    // an operator shell, not as a child of the in-process daemon.
    let out = operator_cadence_at(
        home,
        state,
        &[
            "rollout",
            "claim",
            "--reason",
            "restart test",
            "--as",
            "operator:test",
            "--ttl",
            "2h",
        ],
    );
    assert!(
        out.status.success(),
        "rollout claim failed: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn cadence_at(home: &Path, state: &Path, args: &[&str]) -> std::process::Output {
    cadence_at_cmd(home, state, args).output().unwrap()
}

/// [`cadence_at`] as an operator shell outside every pane (CAD-384): the
/// operator verbs — `daemon stop`/`restart`, `agent stop`/`resume`, … —
/// need operator proof, which a child of this process (the in-process
/// daemon) never has. See [`OperatorOutput`].
pub fn operator_cadence_at(home: &Path, state: &Path, args: &[&str]) -> std::process::Output {
    cadence_at_cmd(home, state, args).operator_output().unwrap()
}

pub fn cadence_at_cmd(home: &Path, state: &Path, args: &[&str]) -> std::process::Command {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(state)
        .args(args)
        .env("HOME", home)
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_ROLLOUT_AS")
        // A `daemon restart` child daemon is a separate process: it
        // gets this test's mock commands as its own env, and only it.
        .envs(test_env().vars());
    cmd
}

// ==== inbox endpoint kind ====

impl TestDaemon {
    /// Register a mailbox: provider+kind `inbox`, durable pseudo-endpoint,
    /// no actor.
    pub fn register_inbox(&self, alias: &str) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        self.fixture_rpc(
            "agent_register",
            json!({"alias": alias, "provider": "inbox",
                   "endpoint_kind": "inbox", "cwd": cwd}),
        )
        .unwrap();
    }

    /// Register a pty devin agent with arbitrary endpoint params.
    pub fn register_devin_opts(&self, alias: &str, params: Value) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        self.fixture_rpc(
            "agent_register",
            json!({"alias": alias, "provider": "devin",
                   "endpoint_kind": "pty", "cwd": cwd,
                   "params": params.to_string()}),
        )
        .unwrap();
    }

    /// All recorded events for an alias.
    pub fn events(&self, alias: &str) -> Vec<Value> {
        self.rpc("agent_events", json!({"alias": alias})).unwrap()["events"]
            .as_array()
            .unwrap()
            .clone()
    }

    /// Poll until an event of `kind` exists (bounded).
    pub fn wait_event(&self, alias: &str, kind: &str, secs: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(e) = self
                .events(alias)
                .into_iter()
                .find(|e| e["kind"].as_str() == Some(kind))
            {
                return e;
            }
            assert!(
                Instant::now() < deadline,
                "agent {alias} never emitted {kind}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Poll until an event of `kind` satisfying `pred` exists
    /// (bounded). Payload-scoped — an earlier event that merely shares
    /// the kind is never returned (CAD-222: a late `turn_stalled` for
    /// one message must not answer a wait meant for another's).
    pub fn wait_event_where(
        &self,
        alias: &str,
        kind: &str,
        pred: impl Fn(&Value) -> bool,
        secs: u64,
    ) -> Value {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(e) = self
                .events(alias)
                .into_iter()
                .find(|e| e["kind"].as_str() == Some(kind) && pred(e))
            {
                return e;
            }
            assert!(
                Instant::now() < deadline,
                "agent {alias} never emitted a matching {kind}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// CAD-185: prove the per-daemon `CADENCE_PTY_RETRY_SECS=1` was honoured.
/// Each render miss that retried is followed by the next attempt's
/// `submitting` event; with the knob the gap is ~1s, with the default 5s
/// it is >= 5s. Timed from durable event stamps, not the test's clock.
pub fn assert_retry_gaps_under(d: &TestDaemon, alias: &str, id: &str, want: usize, max_secs: f64) {
    let events = d.events(alias);
    let of = |kind: &str| -> Vec<f64> {
        events
            .iter()
            .filter(|e| e["kind"] == kind && e["payload"]["message"].as_str() == Some(id))
            .filter_map(|e| e["at"].as_f64())
            .collect()
    };
    let (misses, starts) = (of("paste_not_rendered"), of("submitting"));
    let gaps: Vec<f64> = misses
        .iter()
        .take(misses.len().saturating_sub(1))
        .map(|miss| {
            let next = starts
                .iter()
                .copied()
                .find(|at| at > miss)
                .unwrap_or_else(|| panic!("no attempt after the miss at {miss}: {events:?}"));
            next - miss
        })
        .collect();
    assert_eq!(gaps.len(), want, "retry gaps {gaps:?}");
    assert!(
        gaps.iter().all(|gap| *gap < max_secs),
        "retry base knob ignored — gaps {gaps:?} not under {max_secs}s"
    );
}

/// Emit a test-only timing trace for a routed PTY delivery. The daemon's
/// durable event/message timestamps are the phase clock here: using them
/// avoids charging the test's 50ms RPC polling to a render or retry phase.
/// A `submitting` row is the durable attempt-start boundary and a
/// `paste_not_rendered` row is its completion. The next `submitting` row is
/// the observable retry wake; no separate wake event exists. This is evidence
/// for the follow-up audit, not a change to the delivery contract.
pub fn emit_park_phase_trace(d: &TestDaemon, test_name: &str, alias: &str, routed_id: &str) {
    fn at(value: &Value) -> Option<f64> {
        value["at"].as_f64()
    }

    fn delta(start: Option<f64>, end: Option<f64>) -> Value {
        match (start, end) {
            (Some(start), Some(end)) if end >= start => json!(end - start),
            _ => Value::Null,
        }
    }

    let events = d.events(alias);
    let starts: Vec<&Value> = events
        .iter()
        .filter(|event| {
            event["kind"] == "submitting" && event["payload"]["message"].as_str() == Some(routed_id)
        })
        .collect();
    let misses: Vec<&Value> = events
        .iter()
        .filter(|event| {
            event["kind"] == "paste_not_rendered"
                && event["payload"]["message"].as_str() == Some(routed_id)
        })
        .collect();
    let parked = events.iter().find(|event| {
        event["kind"] == "delivery_parked"
            && event["payload"]["message"].as_str() == Some(routed_id)
    });
    let show = d.rpc("agent_show", json!({"alias": alias})).unwrap();
    let message = show["messages"]
        .as_array()
        .and_then(|messages| messages.iter().find(|message| message["id"] == routed_id));
    let enqueue_at = message.and_then(|message| message["created"].as_f64());
    let attempt_phases: Vec<Value> = misses
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let started_at = starts.get(index).and_then(|event| at(event));
            json!({
                "attempt": index + 1,
                "started_at_epoch_s": started_at,
                "completion_at_epoch_s": at(event),
                "enqueue_to_start_s": if index == 0 {
                    delta(enqueue_at, started_at)
                } else {
                    Value::Null
                },
                "render_attempt_s": delta(started_at, at(event)),
                "retry": event["payload"]["retry"],
            })
        })
        .collect();
    let retry_phases: Vec<Value> = misses
        .windows(2)
        .enumerate()
        .map(|(index, pair)| {
            json!({
                "after_attempt": index + 1,
                "retry_wake_at_epoch_s": starts.get(index + 1).and_then(|event| at(event)),
                "retry_to_next_attempt_start_s": delta(
                    at(pair[0]),
                    starts.get(index + 1).and_then(|event| at(event)),
                ),
            })
        })
        .collect();
    let parked_at = parked.and_then(at);
    let completed_at = message.and_then(|message| message["completed"].as_f64());
    let final_agent = show.get("agent").map(|agent| {
        json!({
            "state": agent["state"],
            "dead": agent["dead"],
            "updated_epoch_s": agent["updated"],
        })
    });
    let report = json!({
        "schema": "cad173.e4a.phase-trace.v1",
        "test": test_name,
        "alias": alias,
        "message": routed_id,
        "enqueue_at_epoch_s": enqueue_at,
        "attempts": attempt_phases,
        "submitting_events": starts.len(),
        "retry_gaps": retry_phases,
        "park_at_epoch_s": parked_at,
        "park_after_attempt4_s": delta(misses.last().and_then(|event| at(event)), parked_at),
        "failed_state_at_epoch_s": completed_at,
        "park_to_failed_state_s": delta(parked_at, completed_at),
        "enqueue_to_failed_state_s": delta(enqueue_at, completed_at),
        "message_state": message.map(|message| message["state"].clone()),
        "agent": final_agent,
        "clock": "durable events.at and messages.created/completed (epoch seconds)",
        "attempt_boundary": "submitting event is attempt start; paste_not_rendered is completion; next submitting event is the retry wake",
    });
    eprintln!("CAD173_E4A_PHASE {report}");
}

/// Send `text` to a fake worker that replies to `pm`, and return the
/// routed delivery id once the worker's own turn has completed. The
/// route and the completion commit together.
pub fn route_worker_result(d: &TestDaemon, worker: &str, pm: &str, id: &str, text: &str) -> String {
    d.rpc(
        "agent_send",
        json!({"alias": worker, "text": text, "message": id, "reply_to": pm}),
    )
    .unwrap();
    d.wait_message(worker, id, &["completed"], 15);
    d.rpc("agent_show", json!({"alias": pm})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| {
            m["source"] == "worker_result" && m["body"].as_str().unwrap_or_default().contains(id)
        })
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

// ==== managed claude (stream-json) endpoint ====

/// A mock Claude stream-json provider over real stdio — speaks the
/// observed wire: per user line it emits `system/init`, an `assistant`
/// event, then one `result`. argv is `<script> <pidfile> <mode>` then
/// the real CLI flags appended by the adapter (`--session-id|--resume`,
/// `--permission-mode`, `--allowedTools`, `--model`) — recorded to
/// `<pidfile>.argv` for launch-shape assertions. The pane env lands in
/// `<pidfile>.env` for scrub/injection checks.
///
/// Modes:
///   ok               — result success, text `MOCK_OK:<prompt>`
///   fail             — result error_during_execution, is_error, errors[]
///   deny             — result success carrying a non-empty
///                      permission_denials array
///   die              — exits on the first user message (mid-turn death)
///   text-die         — init and one "working" text block, then exits
///   bad-session      — init reports a session id that is not argv's
///   await-interrupt  — no result until interrupted: the stream-json
///                      interrupt control request (CAD-323) answers a
///                      control_response, then an aborted error result
///                      (`terminal_reason: aborted_streaming`), like the
///                      real CLI; SIGINT still yields an interrupted one
///   interrupt-tool   — like await-interrupt, but a Bash tool_use is in
///                      flight: the interrupt first records its partial
///                      tool_result (is_error), then `aborted_tools`
///   Every control request lands in `<pidfile>.controls` and a SIGINT in
///   `<pidfile>.sigint` — proof of which interrupt the adapter sent.
///   replay           — replays the `<pidfile>.fixture` events verbatim,
///                      rewriting session_id fields to the argv id
///   heartbeat        — activity every ~0.3s for ~3.6s, then success —
///                      a turn longer than a short idle window
///   silent           — init, then nothing; stays alive (idle fence)
///   chatty           — activity every ~0.3s forever, never a result
///                      (absolute-cap fence)
///   tooluse          — one assistant tool_use block, then success
///   permit           — asks the configured `--mcp-config` server to
///                      approve a Bash call for the prompt; allow →
///                      MOCK_OK, deny → DENIED:<message> plus a
///                      permission_denials entry. The verdict lands in
///                      `<pidfile>.verdict` too, so tests can observe a
///                      denial even after the daemon is gone.
pub const MOCK_CLAUDE_PY: &str = r#"
import json, os, signal, subprocess, sys, time

pidfile = sys.argv[1]
# Mode travels in the pidfile basename — the daemon scrubs CADENCE_*
# from the child env, so an env var would never arrive.
mode = os.path.basename(pidfile).removeprefix("claude-").removesuffix(".pid")
argv = sys.argv[2:]
sid = ""
for i, a in enumerate(argv):
    if a in ("--session-id", "--resume") and i + 1 < len(argv):
        sid = argv[i + 1]
with open(pidfile, "w") as f:
    f.write(str(os.getpid()))
# Atomic dump — a reader between truncate and write must never see a
# torn file; the same temp+rename shape .env uses below.
argv_tmp = pidfile + ".argv.tmp"
with open(argv_tmp, "w") as f:
    f.write("\n".join(sys.argv))
os.rename(argv_tmp, pidfile + ".argv")
env_tmp = pidfile + ".env.tmp"
with open(env_tmp, "w") as f:
    for k in sorted(os.environ):
        f.write("%s=%s\n" % (k, os.environ[k]))
os.rename(env_tmp, pidfile + ".env")

count = [0]

def emit(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()

def result(**kw):
    count[0] += 1
    base = {"type": "result", "session_id": sid, "num_turns": 1,
            "total_cost_usd": 0.001, "result_index": count[0] - 1}
    base.update(kw)
    emit(base)

def init():
    # bad-session reports a session the process was NOT opened with.
    reported = "00000000-foreign-session" if current_mode() == "bad-session" else sid
    emit({"type": "system", "subtype": "init", "session_id": reported,
          "model": "mock-claude", "tools": []})

def on_sigint(signum, frame):
    with open(pidfile + ".sigint", "a") as f:
        f.write("SIGINT\n")
    in_flight[0] = None
    init()
    result(subtype="interrupted", is_error=False, result="INTERRUPTED",
           stop_reason="interrupted")

signal.signal(signal.SIGINT, on_sigint)

# The fixture path rides a sidecar like `.mode` — never the env, which
# every concurrent test's mock child would inherit.
fixture = open(pidfile + ".fixture").read().strip() if os.path.exists(pidfile + ".fixture") else None
fixture_lines = open(fixture).read().splitlines() if fixture else []

def current_mode():
    # <pidfile>.mode overrides the env mode per message — lets a test
    # switch a resumed provider from "die" to "ok".
    try:
        return open(pidfile + ".mode").read().strip()
    except FileNotFoundError:
        return mode

# ---- brokered permission flow (permit mode) ----
mcp_proc = None
mcp_next_id = [0]

def mcp_server():
    """Spawn the `--mcp-config` server once, like the real CLI: env
    from the config overlays ours, then initialize/initialized."""
    global mcp_proc
    if mcp_proc is not None:
        return mcp_proc
    cfg_path = None
    for i, a in enumerate(argv):
        if a == "--mcp-config" and i + 1 < len(argv):
            cfg_path = argv[i + 1]
    if cfg_path is None:
        return None
    srv = json.load(open(cfg_path))["mcpServers"]["cadence"]
    env = dict(os.environ)
    env.update(srv.get("env", {}))
    proc = subprocess.Popen([srv["command"]] + srv["args"],
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            env=env, text=True, bufsize=1)
    mcp_proc = proc
    mcp_rpc(proc, "initialize",
            {"protocolVersion": "2025-11-25", "capabilities": {},
             "clientInfo": {"name": "mock-claude", "version": "0"}})
    proc.stdin.write(json.dumps(
        {"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
    proc.stdin.flush()
    mcp_rpc(proc, "tools/list", {})
    return proc

def mcp_rpc(proc, method, params):
    mcp_next_id[0] += 1
    proc.stdin.write(json.dumps(
        {"jsonrpc": "2.0", "id": mcp_next_id[0],
         "method": method, "params": params}) + "\n")
    proc.stdin.flush()
    line = proc.stdout.readline()
    if not line:
        raise RuntimeError("mcp server exited")
    return json.loads(line)

def record_verdict(verdict):
    tmp = pidfile + ".verdict.tmp"
    with open(tmp, "w") as f:
        f.write(json.dumps(verdict))
    os.rename(tmp, pidfile + ".verdict")

def ask_permission(command_text):
    """One approve call — returns the verdict object the server put
    inside the text content block (allow/deny), or a local deny when
    the broker is unreachable."""
    proc = mcp_server()
    if proc is None:
        verdict = {"behavior": "deny",
                   "message": "no --mcp-config on argv"}
        record_verdict(verdict)
        return verdict
    try:
        resp = mcp_rpc(proc, "tools/call",
                       {"name": "approve",
                        "arguments": {"tool_name": "Bash",
                                      "input": {"command": command_text},
                                      "tool_use_id": "tu_permit_%d"
                                      % mcp_next_id[0]}})
        text = resp["result"]["content"][0]["text"]
        verdict = json.loads(text)
    except Exception as e:
        verdict = {"behavior": "deny", "message": "mcp call failed: %s" % e}
    record_verdict(verdict)
    return verdict

# The turn left waiting for an interrupt ("text" or "tool"), if any.
in_flight = [None]

def on_control(msg):
    with open(pidfile + ".controls", "a") as f:
        f.write(json.dumps(msg) + "\n")
    req = msg.get("request", {})
    emit({"type": "control_response",
          "response": {"subtype": "success",
                       "request_id": msg.get("request_id"), "response": {}}})
    if req.get("subtype") != "interrupt" or in_flight[0] is None:
        return  # an idle CLI acknowledges and does nothing
    kind, in_flight[0] = in_flight[0], None
    if kind == "tool":
        # The aborted tool's partial output, as the CLI records it.
        emit({"type": "user", "session_id": sid,
              "message": {"role": "user", "content": [
                  {"type": "tool_result", "tool_use_id": "tu_int",
                   "is_error": True,
                   "content": "partial line 1\n[Request interrupted by user for tool use]"}]}})
    result(subtype="error_during_execution", is_error=True,
           errors=["[Request interrupted by user]"], stop_reason=None,
           terminal_reason="aborted_tools" if kind == "tool" else "aborted_streaming")

for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    if msg.get("type") == "control_request":
        on_control(msg)
        continue
    if msg.get("type") != "user":
        continue
    mode_now = current_mode()
    content = msg["message"]["content"]
    text = content if isinstance(content, str) else \
        " ".join(b.get("text", "") for b in content)
    if mode_now == "die":
        os._exit(0)
    if mode_now == "replay":
        for raw in fixture_lines:
            try:
                ev = json.loads(raw)
            except Exception:
                continue
            if "session_id" in ev:
                ev["session_id"] = sid
            emit(ev)
        continue
    init()
    emit({"type": "assistant",
          "message": {"role": "assistant",
                      "content": [{"type": "text", "text": "working"}]},
          "session_id": sid})
    if mode_now == "text-die":
        os._exit(0)  # dies mid-turn, after a text block (CAD-320)
    if mode_now == "await-interrupt":
        in_flight[0] = "text"
        continue  # the interrupt (control request or SIGINT) ends it
    if mode_now == "interrupt-tool":
        emit({"type": "assistant", "session_id": sid,
              "message": {"role": "assistant", "content": [
                  {"type": "tool_use", "id": "tu_int", "name": "Bash",
                   "input": {"command": "sleep 300"}}]}})
        in_flight[0] = "tool"
        continue
    if mode_now == "hold":
        # The turn stays open until the test drops `<pidfile>.release`,
        # then completes like "ok" (CAD-162: ack mid-turn).
        while not os.path.exists(pidfile + ".release"):
            time.sleep(0.05)
    if mode_now == "silent":
        while True:
            time.sleep(5)  # alive but eventless — the idle fence path
    if mode_now == "chatty":
        while True:
            emit({"type": "assistant",
                  "message": {"role": "assistant",
                              "content": [{"type": "text", "text": "."}]},
                  "session_id": sid})
            time.sleep(0.3)
    if mode_now == "heartbeat":
        for _ in range(12):
            emit({"type": "assistant",
                  "message": {"role": "assistant",
                              "content": [{"type": "text", "text": "."}]},
                  "session_id": sid})
            time.sleep(0.3)
    if mode_now == "tooluse":
        emit({"type": "assistant",
              "message": {"role": "assistant",
                          "content": [{"type": "tool_use", "name": "Bash",
                                       "input": {"command": "true"}}]},
              "session_id": sid})
    if mode_now == "fail":
        result(subtype="error_during_execution", is_error=True,
               errors=["mock exploded"], stop_reason="error")
        continue
    if mode_now == "permit":
        # The real CLI blocks on the permission-prompt tool here — one
        # approve call per tool use; the verdict decides the outcome.
        verdict = ask_permission(text)
        if verdict.get("behavior") == "allow":
            result(subtype="success", is_error=False,
                   result="MOCK_OK:" + text, stop_reason="end_turn",
                   permission_denials=[])
        else:
            message = verdict.get("message", "denied")
            denials = [{"tool_name": "Bash",
                        "tool_use_id": "tu_permit",
                        "tool_input": {"command": text},
                        "message": message}]
            result(subtype="success", is_error=False,
                   result="DENIED:" + message, stop_reason="end_turn",
                   permission_denials=denials)
        continue
    denials = []
    if mode_now == "deny":
        denials = [{"tool_name": "Bash", "tool_use_id": "tu_1",
                    "tool_input": {"command": "touch /tmp/x"}}]
    result(subtype="success", is_error=False, result="MOCK_OK:" + text,
           stop_reason="end_turn", permission_denials=denials)
"#;

pub struct MockClaude {
    pub pidfile: PathBuf,
}

impl TestDaemon {
    /// Install a mock claude command for `mode` (optionally replaying
    /// `fixture`), returning its pidfile path.
    pub fn mock_claude(&self, mode: &str, fixture: Option<&Path>) -> MockClaude {
        let pidfile = self.dir.path().join(format!("claude-{mode}.pid"));
        let script = self.dir.path().join(format!("claude-{mode}.py"));
        std::fs::write(&script, MOCK_CLAUDE_PY).unwrap();
        test_env().set(
            "CADENCE_CLAUDE_COMMAND",
            format!("python3 {} {}", script.display(), pidfile.display()),
        );
        if let Some(f) = fixture {
            std::fs::write(pidfile.with_extension("pid.fixture"), f.to_str().unwrap()).unwrap();
        }
        MockClaude { pidfile }
    }

    pub fn register_claude(&self, alias: &str, params: Value) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        let params = (!params.is_null()).then(|| params.to_string());
        self.fixture_rpc(
            "agent_register",
            json!({"alias": alias, "provider": "claude",
                   "endpoint_kind": "managed", "cwd": cwd,
                   "params": params}),
        )
        .unwrap();
    }

    /// Poll `agent_requests` until one request is pending (bounded).
    pub fn wait_request(&self, alias: &str, secs: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let requests = self.rpc("agent_requests", json!({"alias": alias})).unwrap()["requests"]
                .as_array()
                .unwrap()
                .clone();
            if let Some(req) = requests.first() {
                return req.clone();
            }
            assert!(
                Instant::now() < deadline,
                "agent {alias} never showed a pending request"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Pending request handles for an alias right now.
    pub fn requests(&self, alias: &str) -> Vec<Value> {
        self.rpc("agent_requests", json!({"alias": alias})).unwrap()["requests"]
            .as_array()
            .unwrap()
            .clone()
    }
}

impl Drop for MockClaude {
    fn drop(&mut self) {
        test_env().remove("CADENCE_CLAUDE_COMMAND");
        test_env().remove("CADENCE_MCP_PERMISSION_COMMAND");
    }
}

// ==== brokered claude permissions (cadence mcp-permission) ====

/// Point the daemon's generated `--mcp-config` at the real cadence
/// binary — `current_exe` is the test binary without the override.
pub fn broker_command() {
    test_env().set(
        "CADENCE_MCP_PERMISSION_COMMAND",
        env!("CARGO_BIN_EXE_cadence"),
    );
}

/// A spawned `cadence mcp-permission` talking stdio — drives the real
/// server binary directly for wire-shape and restart assertions.
pub struct Mcp {
    pub child: std::process::Child,
    pub next_id: u64,
}

impl Mcp {
    pub fn spawn(state: &Path, alias: &str, timeout_secs: u64) -> Self {
        let child = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .args(["mcp-permission"])
            .env("CADENCE_STATE_DIR", state)
            .env("CADENCE_ALIAS", alias)
            .env("CADENCE_PERMISSION_TIMEOUT_SECS", timeout_secs.to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut mcp = Self { child, next_id: 0 };
        mcp.rpc(
            "initialize",
            json!({"protocolVersion": "2025-11-25", "capabilities": {},
                   "clientInfo": {"name": "test", "version": "0"}}),
        );
        mcp.notify("notifications/initialized");
        mcp
    }

    pub fn notify(&mut self, method: &str) {
        use std::io::Write;
        let line = json!({"jsonrpc": "2.0", "method": method});
        writeln!(self.child.stdin.as_mut().unwrap(), "{line}").unwrap();
    }

    /// One request → one response line (blocks until it arrives).
    pub fn rpc(&mut self, method: &str, params: Value) -> Value {
        use std::io::{BufRead, BufReader, Write};
        self.next_id += 1;
        let id = self.next_id;
        let line = json!({"jsonrpc": "2.0", "id": id,
                          "method": method, "params": params});
        writeln!(self.child.stdin.as_mut().unwrap(), "{line}").unwrap();
        let mut out = String::new();
        BufReader::new(self.child.stdout.as_mut().unwrap())
            .read_line(&mut out)
            .unwrap();
        serde_json::from_str(&out).unwrap_or_else(|_| panic!("mcp EOF: {out}"))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ==================== jobs / tasks / verdicts (M3a) ====================

pub const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const SHA_C: &str = "cccccccccccccccccccccccccccccccccccccccc";

impl TestDaemon {
    /// A fake worker joined to `pm`'s group (`params.upstream`).
    pub fn register_member(&self, alias: &str, pm: &str) {
        let cwd = self.dir.path().to_str().unwrap().to_string();
        self.fixture_rpc(
            "agent_register",
            json!({"alias": alias, "provider": "fake",
                   "endpoint_kind": "fake", "cwd": cwd,
                   "params": json!({"upstream": pm}).to_string()}),
        )
        .unwrap();
    }

    /// A spec file in the temp dir; returns (path, sha256 hex).
    pub fn spec_file(&self, name: &str, content: &str) -> (String, String) {
        use sha2::{Digest, Sha256};
        let path = self.dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        (
            path.to_str().unwrap().to_string(),
            format!("{:x}", Sha256::digest(content.as_bytes())),
        )
    }

    pub fn job_new(&self, pm: &str, job: &str, spec: &str, spec_sha: &str) -> Value {
        self.rpc(
            "job_new",
            json!({"pm": pm, "job": job, "spec": spec, "spec_sha256": spec_sha}),
        )
        .unwrap()
    }

    pub fn task_state(&self, task: &str) -> String {
        self.rpc("task_show", json!({"task": task})).unwrap()["task"]["state"]
            .as_str()
            .unwrap()
            .to_string()
    }

    pub fn job_state(&self, job: &str) -> String {
        self.rpc("job_show", json!({"job": job})).unwrap()["job"]["state"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// `job dispatch` — returns the full RPC payload. Dispatch is the
    /// operator's act (`BY_OPERATOR`), so it goes the operator's way —
    /// a suite running in an agent pane could not prove it otherwise.
    pub fn job_dispatch(&self, task: &str, extra: Value) -> cadence_agent::Result<Value> {
        let mut p = json!({"task": task});
        for (k, v) in extra.as_object().unwrap_or(&serde_json::Map::new()) {
            p[k] = v.clone();
        }
        self.operator_rpc("task_dispatch", p)
    }

    /// A verdict from the operator: the reviewer is the verified
    /// connection (CAD-372), so the call is made from a caller that is
    /// provably the operator however the suite is run.
    pub fn job_verdict(
        &self,
        task: &str,
        sha: &str,
        verdict: &str,
    ) -> cadence_agent::Result<Value> {
        self.operator_rpc(
            "task_verdict",
            json!({"task": task, "sha": sha, "verdict": verdict}),
        )
    }

    /// Wait for a task state.
    pub fn wait_task(&self, task: &str, want: &str, secs: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let t = self.rpc("task_show", json!({"task": task})).unwrap()["task"].clone();
            if t["state"].as_str() == Some(want) {
                return t;
            }
            assert!(
                Instant::now() < deadline,
                "task {task} never reached {want}: {t}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Repo fixture for verdict verification: `main` + branch `cadence/fix`
/// one commit ahead, checked out in `.cadence/wt/fix`. `origin`:
/// `None` = no remote; a bare-path string is pushed for real; a
/// GitHub URL is recorded as the remote and its tracking ref placed
/// by hand (never fetched).
pub struct VerifyRepo {
    pub _tmp: TempDir,
    pub repo: PathBuf,
    pub worktree: PathBuf,
    pub base: String,
    pub head: String,
}

/// A fake `gh` that logs each invocation to $FAKE_GH_LOG (one
/// tab-joined line) and answers from the environment — same pattern
/// as tests/scripts/test_qa_verdict.py. FAKE_GH_FAIL forces exit N.
/// FAKE_GH_RUNS answers the `ci.yml` runs listing (CAD-267).
pub const FAKE_GH: &str = r#"#!/usr/bin/env bash
(IFS=$'\t'; printf '%s\n' "$*") >> "$FAKE_GH_LOG"
if [ -n "${FAKE_GH_FAIL:-}" ]; then echo "fake gh: forced failure" >&2; exit "$FAKE_GH_FAIL"; fi
no_runs='{"total_count": 0, "workflow_runs": []}'
case "$1 ${2:-}" in
  "pr list") printf '%s\n' "${FAKE_GH_PRS:-[]}" ;;
  "pr view") printf '{"number": %s, "headRefOid": "%s"}\n' "${FAKE_GH_PR_NUM:-9}" "${FAKE_GH_HEAD:-}" ;;
  "api --method") echo '{}' ;;
  "api repos/"*"/actions/workflows/ci.yml/runs?"*) printf '%s\n' "${FAKE_GH_RUNS:-$no_runs}" ;;
  "api repos/"*) printf '{"default_branch": "%s"}\n' "${FAKE_GH_DEFAULT_BRANCH:-main}" ;;
  *) echo "fake gh: unexpected call: $*" >&2; exit 64 ;;
esac
"#;

/// Fake-gh dir + call log; env vars for `cadence_cli`.
pub struct FakeGh {
    pub _tmp: TempDir,
    pub bin: PathBuf,
    pub log: PathBuf,
}

pub fn fake_gh() -> FakeGh {
    let tmp = TempDir::new().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    std::fs::write(&gh, FAKE_GH).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let log = tmp.path().join("gh.log");
    std::fs::write(&log, "").unwrap();
    FakeGh {
        _tmp: tmp,
        bin,
        log,
    }
}

impl FakeGh {
    /// Env overlay for cadence_cli: this gh first on PATH + the log +
    /// any FAKE_GH_* knobs.
    pub fn envs(&self, extra: &[(String, String)]) -> Vec<(String, String)> {
        let mut v = vec![
            (
                "PATH".to_string(),
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            ),
            (
                "FAKE_GH_LOG".to_string(),
                self.log.to_str().unwrap().to_string(),
            ),
        ];
        for (k, val) in extra {
            v.push((k.clone(), val.clone()));
        }
        v
    }

    pub fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

// ---------- CAD-52: stall detection ----------

/// Poll until `alias` has at least `want` events of `kind`.
pub fn wait_event_count(
    d: &TestDaemon,
    alias: &str,
    kind: &str,
    want: usize,
    secs: u64,
) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let found: Vec<Value> = d
            .events(alias)
            .into_iter()
            .filter(|e| e["kind"].as_str() == Some(kind))
            .collect();
        if found.len() >= want {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "{alias}: wanted {want} {kind} events, got {}: {:?}",
            found.len(),
            d.events(alias)
                .iter()
                .map(|e| e["kind"].as_str().unwrap_or("?").to_string())
                .collect::<Vec<_>>()
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// The stored message list for `alias` — notices land here for both
/// actor and inbox recipients.
pub fn messages_for(d: &TestDaemon, alias: &str) -> Vec<Value> {
    d.rpc("agent_show", json!({"alias": alias})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .clone()
}

/// Poll until `alias` stores a message whose `source` matches.
pub fn wait_source(
    d: &TestDaemon,
    alias: &str,
    source: &str,
    want: usize,
    secs: u64,
) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let found: Vec<Value> = messages_for(d, alias)
            .into_iter()
            .filter(|m| m["source"].as_str() == Some(source))
            .collect();
        if found.len() >= want {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "{alias}: wanted {want} '{source}' messages, got {:?}",
            messages_for(d, alias)
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// A fake actor with explicit launch params — `stall_secs` included.
pub fn register_fake_opts(d: &TestDaemon, alias: &str, params: Value) {
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.fixture_rpc(
        "agent_register",
        json!({"alias": alias, "provider": "fake",
               "endpoint_kind": "fake", "cwd": cwd,
               "params": params.to_string()}),
    )
    .unwrap();
}

// ---- CAD-102: approval menus are a probe state, not busy churn ----

/// The real Devin permission menu — option rows and the selection
/// footer ABOVE a still-visible busy input box (the CAD-102 incident
/// layout): everything the analyzer must see sits ~11 rows above the
/// frame end.
pub const DEVIN_MENU: &str = "\
❭ run the shell command: printenv FOO
 ⏺ Running command
 └ $ printenv FOO

❭ 1 Yes  (Approve once)
· 2 Yes, allow `printenv` commands
· 3 Yes, always allow `printenv` commands in `tmp`
· 4 Yes, always allow `printenv` commands in all projects
· 5 Yes, switch to bypass mode
· 6 Edit command
· 7 Describe change to command
· 8 No
↑↓ select · ↵ confirm · esc cancel
⠸ Thinking · 5s (esc twice to interrupt)
❭ Guide Devin while it works
";

// ---- CAD-152: `agent recover-submit` — one Enter for a pasted, unsubmitted draft ----

/// The kickoff whose submit is lost. Distinctive, so an audit event
/// quoting it is caught; single-line, no stub command prefix.
pub const RECOVER_BODY: &str = "CAD152-KICKOFF read the brief at docs/brief.md and report \
                            with your turn token when you start";

/// The stub TUI's input width (`adapter::pty::stub::STUB_INPUT_WIDTH`).
pub const STUB_INPUT_WIDTH: usize = 40;

// ---- CAD-55: `cadence dispatch` + `cadence issue finish` against a live daemon ----

/// CAD-275: age every file under `dir` past `issue finish`'s
/// 30-minute active window — the state of a lane nobody has touched
/// since. Without it a lane written seconds ago is "in use".
pub fn idle(dir: &Path) {
    let st = std::process::Command::new("find")
        .arg(dir)
        .args(["-exec", "touch", "-h", "-d", "2 hours ago", "{}", "+"])
        .status()
        .unwrap();
    assert!(st.success(), "backdate {}", dir.display());
}

// ==== operator IX: cadence status, daemon restart, events tail ====

/// The tracker a `cadence status` run reads: the test's own
/// (`CADENCE_PM_DIR`, or `HOME/pm`) when it passes one, else a per-call
/// path that does not exist — never the host's `$HOME/pm`, which
/// `status` would otherwise read and, since CAD-403, cache line times
/// into (the rule `daemon_opts` applies to test daemons).
pub fn status_tracker_env(cmd: &mut std::process::Command, envs: &[(&str, &Path)]) {
    cmd.env_remove("CADENCE_PM_DIR");
    if !envs
        .iter()
        .any(|(k, _)| matches!(*k, "CADENCE_PM_DIR" | "HOME"))
    {
        let none = std::env::temp_dir().join(format!(
            "cadence-test-no-pm-{}",
            uuid::Uuid::new_v4().simple()
        ));
        cmd.env("CADENCE_PM_DIR", none);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
}

/// `cadence status --json` against a daemon's socket — the JSON shape
/// is the contract; extra args (`--group`) and env (`CADENCE_PM_DIR`)
/// thread through.
pub fn status_json(state: &Path, extra: &[&str], envs: &[(&str, &Path)]) -> Value {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(state)
        .arg("status")
        .arg("--json")
        .args(extra)
        .env_remove("CADENCE_ALIAS");
    status_tracker_env(&mut cmd, envs);
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "status output not json: {e}: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// `cadence status` table form — same invocation, no --json.
pub fn status_table(state: &Path, envs: &[(&str, &Path)]) -> String {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(state)
        .arg("status")
        .env_remove("CADENCE_ALIAS");
    status_tracker_env(&mut cmd, envs);
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// `cadence issue …` with the tracker pointed at `pm`.
pub fn issue_cli(home: &Path, state: &Path, pm: &Path, args: &[&str]) {
    let bin = env!("CARGO_BIN_EXE_cadence");
    let out = std::process::Command::new(bin)
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .env("HOME", home)
        .env("CADENCE_PM_DIR", pm)
        // The tracker's pre-commit hook runs `cadence` from PATH —
        // put the just-built binary first so a stale ambient install
        // can't answer `issue lint` (the board harness does the same).
        .env(
            "PATH",
            format!(
                "{}:{}",
                std::path::Path::new(bin).parent().unwrap().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "issue {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A fixture repo: `origin` (bare) + `repo` (clone) whose main moved
/// after the PR branches cut, four open PR heads under refs/pull/N/head
/// (7 = clean merge + a new wait-test, 8 = pairwise conflict with 7,
/// 9 = clean, 10 = merge conflict with main), a `cadence-review.toml`
/// driving trivial commands, and a fake `gh` answering from fixtures.
pub struct ReviewFixture {
    pub repo: PathBuf,
    pub state: PathBuf,
    pub fakebin: PathBuf,
    pub fakedir: PathBuf,
    pub gate_log: PathBuf,
    pub suite_ran: PathBuf,
    pub suite_lock: PathBuf,
    pub head7: String,
}
// ==== CAD-83: `cadence overview` — daemon-dependent rows ====

/// `cadence overview --json` against a scratch daemon's state dir;
/// `pm` binds a tracker dir via CADENCE_PM_DIR, `envs` add PATH etc.
pub fn overview_at(home: &Path, state: &Path, pm: Option<&Path>, envs: &[(&str, String)]) -> Value {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(state)
        .args(["overview", "--json"])
        .env("HOME", home)
        .env_remove("CADENCE_ALIAS");
    if let Some(pm) = pm {
        cmd.env("CADENCE_PM_DIR", pm);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "overview: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

pub fn git_at(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// ==== CAD-251: an inbox nobody drains warns, never refuses ====

/// A mailbox with endpoint params (thresholds, upstream).
pub fn register_inbox_with(d: &TestDaemon, alias: &str, params: Value) {
    d.fixture_rpc(
        "agent_register",
        json!({"alias": alias, "provider": "inbox", "endpoint_kind": "inbox",
               "params": params.to_string()}),
    )
    .unwrap();
}

// ---------- CAD-95: shared cargo target dir ----------

/// pm + repo + home + state under one temp dir, a `cli` that runs the
/// binary with the test env, and a `cli_at` that also sets cwd (the
/// doctor checks scan the repo it is launched from).
pub struct SharedTarget {
    pub _tmp: TempDir,
    pub pm_dir: PathBuf,
    pub repo: PathBuf,
    pub home: PathBuf,
    pub state: PathBuf,
    pub bin_dir: PathBuf,
}

impl SharedTarget {
    pub fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let (pm_dir, repo, home, state) = (
            tmp.path().join("pm"),
            tmp.path().join("repo"),
            tmp.path().join("home"),
            tmp.path().join("state"),
        );
        for dir in [&pm_dir, &repo, &home, &state] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let git = |args: &[&str]| {
            let o = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(
                o.status.success(),
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&o.stderr)
            );
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(repo.join("f"), "x").unwrap();
        // A real repo ignores its build output — `target/` must not
        // read as dirty for `git status` or `issue finish`.
        std::fs::write(repo.join(".gitignore"), "/target\n").unwrap();
        // A tiny standalone bin crate so tests can build real per-lane
        // binaries in the worktrees.
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"marker\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
            .parent()
            .unwrap()
            .to_path_buf();
        let s = Self {
            _tmp: tmp,
            pm_dir,
            repo,
            home,
            state,
            bin_dir,
        };
        assert!(s.cli(&["issue", "init"]).0);
        let repo_s = s.repo.canonicalize().unwrap().to_str().unwrap().to_string();
        assert!(
            s.cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s])
                .0
        );
        s
    }

    pub fn cli_at(&self, cwd: &Path, args: &[&str]) -> (i32, String, String) {
        self.cli_at_env(cwd, args, &[])
    }

    pub fn cli_at_env(
        &self,
        cwd: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> (i32, String, String) {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
        cmd.arg("--state-dir")
            .arg(&self.state)
            .args(args)
            .env("CADENCE_PM_DIR", &self.pm_dir)
            .env("HOME", &self.home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .current_dir(cwd);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    pub fn cli(&self, args: &[&str]) -> (bool, Value) {
        let (code, stdout, stderr) = self.cli_at(&self.repo, args);
        let text = if stdout.is_empty() { stderr } else { stdout };
        (
            code == 0,
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
        )
    }

    pub fn set_build_target_dir(&self, value: &str) {
        let yaml_path = self.pm_dir.join("demo/project.yaml");
        let yaml = std::fs::read_to_string(&yaml_path).unwrap();
        // Drop any prior appended `build:` block before adding ours —
        // serde rejects a duplicate field.
        let kept: Vec<&str> = yaml
            .lines()
            .filter(|l| *l != "build:" && !l.starts_with("  target_dir:"))
            .collect();
        std::fs::write(
            &yaml_path,
            format!("{}\nbuild:\n  target_dir: {value}\n", kept.join("\n")),
        )
        .unwrap();
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.pm_dir)
            .args(["add", "-A"])
            .output()
            .unwrap();
        assert!(o.status.success());
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.pm_dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "cfg",
            ])
            .output()
            .unwrap();
        assert!(o.status.success());
    }

    pub fn new_issue(&self, title: &str) {
        assert!(self.cli(&["issue", "new", title, "--project", "demo"]).0);
    }

    pub fn worktree_of(&self, id: &str) -> PathBuf {
        let show = self.cli(&["issue", "show", id, "--json"]).1;
        show["refs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["kind"] == "worktree")
            .and_then(|r| r["path"].as_str())
            .map(PathBuf::from)
            .expect("worktree ref")
    }

    pub fn git(&self, dir: &Path, args: &[&str]) -> String {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    /// `cargo build` the fixture's `marker` crate inside `wt`.
    pub fn cargo_build(&self, wt: &Path) {
        let o = std::process::Command::new("cargo")
            .arg("build")
            .arg("--quiet")
            .current_dir(wt)
            .env_remove("CARGO_TARGET_DIR")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "cargo build: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    /// Write the marker crate's source so the built binary prints
    /// `marker` — each lane carries a distinct build.
    pub fn set_marker(&self, wt: &Path, marker: &str) {
        std::fs::write(
            wt.join("src/main.rs"),
            format!("fn main() {{ println!(\"{marker}\"); }}\n"),
        )
        .unwrap();
    }
}

// ---- session start|end: stub daemon socket, fixture pm + repo (CAD-92) ----

/// Canned `agent_show` data for one alias. `flip` replaces the answer
/// after the first `agent_show` — an agent that goes busy between the
/// fleet snapshot and the pre-stop re-check.
pub struct StubAgent {
    pub row: Value,
    pub messages: Vec<Value>,
    pub queued: i64,
    pub unknown: i64,
    pub flip: Option<Value>,
}

impl StubAgent {
    /// After the first `agent_show`, serve `flip` — the race case.
    pub fn flipping(mut self, flip: Value) -> Self {
        self.flip = Some(flip);
        self
    }
    /// The agent's working directory — `--project` membership is
    /// issue-owner first, cwd-under-repo second.
    pub fn with_cwd(mut self, cwd: &Path) -> Self {
        self.row["cwd"] = json!(cwd.to_str().unwrap_or("/"));
        self
    }
}

/// The daemon wire protocol on `<state>/cadence.sock` with canned
/// answers — the session verbs under test connect exactly like they
/// would to the real daemon. `calls` records `(method, params)` so a
/// test can prove `--dry-run` mutated nothing and which agent a stop
/// actually named.
pub struct StubDaemon {
    pub calls: Arc<Mutex<Vec<(String, Value)>>>,
    pub _thread: JoinHandle<()>,
}

// ---------- CAD-136: report intake ----------

/// pm + two repos (the `cadence` project and a `product` project) +
/// home + state under one temp dir; `cli_at` runs the real binary with
/// cwd control — report routing is decided by kind and cwd, so the
/// fixture keeps both an inside-a-project cwd and a foreign one.
pub struct ReportFx {
    pub _tmp: TempDir,
    pub pm_dir: PathBuf,
    pub notes_dir: PathBuf,
    pub cadence_repo: PathBuf,
    pub product_repo: PathBuf,
    pub foreign_cwd: PathBuf,
    pub home: PathBuf,
    pub state: PathBuf,
    pub bin_dir: PathBuf,
}

impl ReportFx {
    pub fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let (pm_dir, notes_dir, cadence_repo, product_repo, foreign_cwd, home, state) = (
            tmp.path().join("pm"),
            tmp.path().join("notes"),
            tmp.path().join("cadence-repo"),
            tmp.path().join("product-repo"),
            tmp.path().join("nowhere"),
            tmp.path().join("home"),
            tmp.path().join("state"),
        );
        for dir in [&pm_dir, &notes_dir, &home, &state, &foreign_cwd] {
            std::fs::create_dir_all(dir).unwrap();
        }
        git_repo(&cadence_repo);
        git_repo(&product_repo);
        let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
            .parent()
            .unwrap()
            .to_path_buf();
        let s = Self {
            _tmp: tmp,
            pm_dir,
            notes_dir,
            cadence_repo,
            product_repo,
            foreign_cwd,
            home,
            state,
            bin_dir,
        };
        assert!(s.cli(&["issue", "init"]).0);
        // init defaults notes_dir to the shared /var/www/agent-notes —
        // a stray real note tagged `Issue: C-1` would flip a derived
        // status and flake these tests, so point it at the temp dir.
        let pm_yaml = s.pm_dir.join("pm.yaml");
        let text = std::fs::read_to_string(&pm_yaml).unwrap();
        let text = text
            .lines()
            .map(|l| {
                if l.starts_with("notes_dir:") {
                    format!("notes_dir: {}", s.notes_dir.display())
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&pm_yaml, format!("{text}\n")).unwrap();
        for (key, prefix, repo) in [
            ("cadence", "C", s.cadence_repo.clone()),
            ("product", "P", s.product_repo.clone()),
        ] {
            let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
            let (ok, out) = s.cli(&[
                "issue", "project", "add", key, "--prefix", prefix, "--repo", &repo_s,
            ]);
            assert!(ok, "project add {key}: {out}");
        }
        s
    }

    pub fn cli(&self, args: &[&str]) -> (bool, Value) {
        self.cli_at(&self.product_repo, args)
    }

    pub fn cli_at(&self, cwd: &Path, args: &[&str]) -> (bool, Value) {
        self.cli_at_env(cwd, args, &[]).2
    }

    /// `(success, stderr, parsed stdout-or-stderr-json)` — stderr kept
    /// separate so refusal tests can assert on the message text.
    pub fn cli_at_env(
        &self,
        cwd: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> (bool, String, (bool, Value)) {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
        cmd.arg("--state-dir")
            .arg(&self.state)
            .args(args)
            .env("CADENCE_PM_DIR", &self.pm_dir)
            .env("HOME", &self.home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .current_dir(cwd);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let text = if out.stdout.is_empty() {
            stderr.clone()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        (
            out.status.success(),
            stderr,
            (
                out.status.success(),
                serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
            ),
        )
    }

    pub fn issue_body(&self, project: &str, id: &str) -> String {
        std::fs::read_to_string(self.pm_dir.join(project).join(id).join("issue.md")).unwrap()
    }

    pub fn tracker_log(&self, n: usize) -> String {
        git_at(
            &self.pm_dir,
            &["log", &format!("-{n}"), "--format=%s%n%(trailers)"],
        )
    }
}

// ==================== task reports (CAD-341) ====================

/// A `cadence.report/2` file with the six reflection headings.
pub fn task_report_text(front: &str) -> String {
    let body = [
        "Expected",
        "Evidence",
        "Cause",
        "Correction",
        "Lesson",
        "Next",
    ]
    .iter()
    .map(|h| format!("## {h}\n\n{h} text.\n"))
    .collect::<Vec<_>>()
    .join("\n");
    format!("---\n{front}---\n\n{body}")
}

pub fn wait_monitor_state(d: &TestDaemon, monitor: &str, want: &str, secs: u64) -> Value {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let value = d.rpc("monitor_show", json!({"monitor": monitor})).unwrap()["monitor"].clone();
        if value["monitoring"].as_str() == Some(want) {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "monitor {monitor} never reached {want}: {value}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

pub fn epoch_now() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

// ---------- CAD-217: operator approval evidence ------------------------

/// `mergedAt` of `audit_repo`'s PR n is 2026-09-20T12:00:0nZ.
pub const AUDIT_MERGE_EPOCH: f64 = 1_789_905_600.0;

/// A fake `gh` for the live audit path (CAD-287): `pr list` answers
/// `$FAKE_GH_DIR/prs.json`, `statuses/<sha>` answers
/// `status-<sha>.json` (or `[]`), the combined endpoint has nothing.
/// Every call is logged to `calls.log`. `audit_live_gh` prepends the
/// shebang and `FAKE_GH_DIR` — baked in, not a process-wide env var
/// that parallel tests would race on.
pub const AUDIT_FAKE_GH: &str = r#"
printf '%s\n' "$*" >> "$FAKE_GH_DIR/calls.log"
case "$1 $2" in
  "pr list"*) cat "$FAKE_GH_DIR/prs.json" ;;
  "api repos/x/y/statuses/"*)
    sha=${2#repos/x/y/statuses/}; sha=${sha%%\?*}
    if [ -f "$FAKE_GH_DIR/status-$sha.json" ]; then cat "$FAKE_GH_DIR/status-$sha.json"; else echo '[]'; fi ;;
  "api repos/x/y/commits/"*) echo '{"statuses": []}' ;;
  *) echo "fake gh: unexpected call: $*" >&2; exit 64 ;;
esac
"#;

// ---------- CAD-113: build slots ----------

/// A daemon with a shrunken slot config — hermetic (ServeOptions wins
/// over pm.yaml, so no host config can leak in).
pub fn slot_opts(
    build: usize,
    suite: usize,
    starve: u64,
    priority: &[&str],
) -> daemon::ServeOptions {
    slot_opts_clock(build, suite, starve, priority, None)
}

/// `slot_opts` with an injected slot clock: a shared counter the test
/// advances instead of sleeping — starvation tests stay deterministic
/// under host load.
pub fn slot_opts_clock(
    build: usize,
    suite: usize,
    starve: u64,
    priority: &[&str],
    clock: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
) -> daemon::ServeOptions {
    daemon::ServeOptions {
        slots: Some(cadence_agent::slots::SlotConfig {
            build_slots: build,
            suite_slots: suite,
            starve_secs: starve,
            priority_lanes: priority.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }),
        slot_clock: clock.map(|c| {
            std::sync::Arc::new(move || c.load(std::sync::atomic::Ordering::Relaxed) as f64)
                as std::sync::Arc<dyn Fn() -> f64 + Send + Sync>
        }),
        ..daemon_opts()
    }
}

/// Plant `alias` as a live pty pane rooted at `pid` — the endpoint
/// facts the slot caller-identity derivation reads (CAD-113). The row
/// stays otherwise inert: registered as an actorless `inbox` pair and
/// marked `enabled=0`, so neither a register-time `set_identity` nor a
/// restart's relaunch sweep can overwrite or detach the planted facts.
/// `slot_*` RPCs derive caller identity from `SO_PEERCRED` + /proc
/// ancestry, so a test lane is only reachable from processes whose
/// ancestry includes this pid.
pub fn plant_pane(d: &TestDaemon, alias: &str, pid: u32) {
    // Register as an `inbox` mailbox: the pair owns no actor, so no
    // async `set_identity` can land after this plant and overwrite
    // the pid (`agent_register` on an existing alias errors —
    // idempotent on a daemon restarted over a kept state dir). And
    // `enabled=0` keeps a restarted daemon's relaunch sweep from
    // spawning a pty actor for the row — its open cannot verify a
    // planted pane and the exit-detach clears the pid the pane map
    // resolves callers by (the CAD-113 CI flake).
    let _ = d.fixture_rpc(
        "agent_register",
        json!({"alias": alias, "provider": "inbox",
               "endpoint_kind": "inbox",
               "cwd": d.dir.path().to_str().unwrap()}),
    );
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET endpoint_kind='pty', pid=?1, pid_start=?3, enabled=0, \
            generation='planted', session_id='planted' WHERE alias=?2",
        rusqlite::params![pid as i64, alias, proc_start(pid)],
    )
    .unwrap();
}

/// `/proc/<pid>/stat` field 22 — what the daemon records as a pid's
/// `pid_start` (CAD-385), so a planted row names exactly that process.
pub fn proc_start(pid: u32) -> Option<i64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

/// The lane every in-process `d.rpc` slot call derives: the test
/// process's own pid planted as this alias's pane.
pub const SELF_LANE: &str = "pane-self";

/// Plant the test process itself as `SELF_LANE`'s pane — after this,
/// `d.rpc` slot calls and `Command`-spawned cadence CLIs all run as
/// that lane (their ancestry always includes the test pid).
pub fn plant_self(d: &TestDaemon) {
    plant_pane(d, SELF_LANE, std::process::id());
}

/// A long-lived `bash` whose pid is planted as a lane's pane:
/// commands written to its stdin run as its children, so their
/// socket-peer identity derives that lane — the only way to get a
/// second connection identity in-process tests can't reach.
pub struct LaneShell {
    pub child: std::process::Child,
    pub stdin: std::process::ChildStdin,
    pub stdout: BufReader<std::process::ChildStdout>,
    pub dir: TempDir,
    pub seq: u64,
}

impl LaneShell {
    pub fn spawn(home: &Path) -> LaneShell {
        let mut child = std::process::Command::new("bash")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .env("HOME", home)
            .envs(test_env().vars())
            .spawn()
            .unwrap();
        LaneShell {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            dir: TempDir::new().unwrap(),
            seq: 0,
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Run a bash fragment under this lane; answer (exit code, output).
    pub fn run(&mut self, cmd: &str) -> (i64, String) {
        let tag = format!("__lane_rc_{}__", self.seq);
        self.seq += 1;
        // The bare `echo` first guarantees the marker opens a fresh
        // line even when the command's output ends mid-line.
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

    /// `cadence <args>` run under this lane's identity.
    pub fn cadence(&mut self, state: &Path, args: &str) -> (i64, String) {
        self.run(&format!(
            "{} --state-dir {} {args}",
            env!("CARGO_BIN_EXE_cadence"),
            state.display()
        ))
    }

    /// One raw JSONL RPC under this lane's identity — the answer is
    /// the wire frame (`{"ok":…, "result"|"error":…}`).
    pub fn rpc(&mut self, state: &Path, method: &str, params: Value) -> Value {
        let req = self.dir.path().join(format!("req-{}.json", self.seq));
        std::fs::write(
            &req,
            cadence_agent::proto::request(method, params).to_string(),
        )
        .unwrap();
        let (rc, out) = self.run(&format!(
            "python3 -c 'import socket,sys;\
             s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);\
             s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");\
             print(s.makefile().readline())' {} {}",
            client::socket_path(state).display(),
            req.display()
        ));
        assert_eq!(rc, 0, "lane rpc failed: {out}");
        serde_json::from_str(out.trim()).unwrap()
    }
}

impl Drop for LaneShell {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// CAD-149 / CAD-304 S3 fleet: `pm` and `pm2` are group roots, `w1` a
/// worker in pm's group — each a planted pane whose [`LaneShell`]
/// commands derive that agent from `/proc` ancestry. The rows stay
/// inert like [`plant_pane`]'s; `w1` is planted as a claude pty so its
/// own `--next-launch model/effort` validates.
pub struct GuardPanes {
    pub pm: LaneShell,
    pub pm2: LaneShell,
    pub w1: LaneShell,
    pub _home: TempDir,
}

pub fn plant_member_pane(
    d: &TestDaemon,
    alias: &str,
    provider: &str,
    upstream: Option<&str>,
    pid: u32,
) {
    let mut req = json!({"alias": alias, "provider": "inbox",
                         "endpoint_kind": "inbox",
                         "cwd": d.dir.path().to_str().unwrap()});
    if let Some(pm) = upstream {
        req["params"] = json!(json!({"upstream": pm}).to_string());
    }
    d.fixture_rpc("agent_register", req).unwrap();
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET provider=?1, endpoint_kind='pty', pid=?2, pid_start=?4, \
            enabled=0, generation='planted', session_id='planted' WHERE alias=?3",
        rusqlite::params![provider, pid as i64, alias, proc_start(pid)],
    )
    .unwrap();
}

pub fn guard_panes(d: &TestDaemon) -> GuardPanes {
    let home = TempDir::new().unwrap();
    let pm = LaneShell::spawn(home.path());
    let pm2 = LaneShell::spawn(home.path());
    let w1 = LaneShell::spawn(home.path());
    plant_member_pane(d, "pm", "inbox", None, pm.pid());
    plant_member_pane(d, "pm2", "inbox", None, pm2.pid());
    plant_member_pane(d, "w1", "claude", Some("pm"), w1.pid());
    GuardPanes {
        pm,
        pm2,
        w1,
        _home: home,
    }
}

/// One raw RPC from a process tied to no pane that is not provably the
/// operator either: its own environment carries an agent's alias (a
/// detached pane process that kept its env looks like this).
pub fn unprovable_rpc(d: &TestDaemon, method: &str, params: Value) -> Value {
    let out = std::process::Command::new("python3")
        .arg("-c")
        .arg(
            "import socket,sys;s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);\
             s.sendall(sys.argv[2].encode()+b'\\n');print(s.makefile().readline())",
        )
        .arg(client::socket_path(&d.state))
        .arg(cadence_agent::proto::request(method, params).to_string())
        .env("CADENCE_ALIAS", "detached-w1")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    serde_json::from_slice(&out.stdout).unwrap()
}

pub fn frame_err(frame: &Value) -> String {
    frame["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// `slot_acquire` with the test process's pid — alive for the whole
/// test, so the pid check never reaps a live waiter here. The lane
/// param is ignored by the daemon (identity is the connection's);
/// callers pass SELF_LANE for honesty.
pub fn slot_acquire(d: &TestDaemon, kind: &str, lane: &str, req: &str) -> Value {
    slot_acquire_pid(d, kind, lane, std::process::id(), req)
}

/// `slot_acquire` claiming an explicit pid — must be the test process
/// or one of its /proc ancestors, or the daemon refuses.
pub fn slot_acquire_pid(d: &TestDaemon, kind: &str, lane: &str, pid: u32, req: &str) -> Value {
    d.rpc(
        "slot_acquire",
        json!({"kind": kind, "lane": lane, "pid": pid,
               "request_id": req}),
    )
    .unwrap()
}

/// A managed "claude" provider for the CAD-230 enrollment tests. It
/// never answers a turn; it performs slot RPCs on the test's behalf —
/// as itself (the enrolled root), from a child (a verified descendant)
/// or from a double-forked `setsid` grandchild (off the root's
/// ancestry; `detached-bare` also drops `CADENCE_ALIAS`). `exec` runs
/// an argv as a tool subprocess and lands `{rc, out, err}`. Requests
/// arrive as `<cmd_dir>/req-N.json`, answers land as `resp-N.json`; a
/// `"$PID"` pid param means the performing process.
pub const MOCK_ENROLL_PY: &str = r#"
import json, os, socket, subprocess, sys, threading, time

def rpc(sock_path, frame):
    params = frame.setdefault("params", {})
    if params.get("pid") == "$PID":
        params["pid"] = os.getpid()
    s = socket.socket(socket.AF_UNIX)
    s.connect(sock_path)
    s.sendall((json.dumps(frame) + "\n").encode())
    line = s.makefile().readline()
    s.close()
    return json.loads(line)

def land(path, value):
    with open(path + ".tmp", "w") as f:
        json.dump(value, f)
    os.rename(path + ".tmp", path)

def off_lineage(pid):
    p = os.getpid()
    while p > 1:
        if p == pid:
            return False
        with open("/proc/%d/status" % p) as f:
            p = int([l for l in f if l.startswith("PPid:")][0].split()[1])
    return True

if sys.argv[1] == "--child":
    sock_path, frame, out = sys.argv[2:5]
    land(out, rpc(sock_path, json.loads(frame)))
    sys.exit(0)

if sys.argv[1] == "--detached":
    sock_path, frame, out, root = sys.argv[2:6]
    if os.fork() > 0:
        os.wait()
        sys.exit(0)
    os.setsid()
    if os.fork() > 0:
        os._exit(0)
    while not off_lineage(int(root)):
        time.sleep(0.02)
    land(out, rpc(sock_path, json.loads(frame)))
    os._exit(0)

pidfile, sock_path, cmd_dir = sys.argv[1:4]
with open(pidfile + ".tmp", "w") as f:
    f.write(str(os.getpid()))
os.rename(pidfile + ".tmp", pidfile)

def serve():
    n = 0
    while True:
        req = os.path.join(cmd_dir, "req-%d.json" % n)
        if not os.path.exists(req):
            time.sleep(0.02)
            continue
        cmd = json.load(open(req))
        out = os.path.join(cmd_dir, "resp-%d.json" % n)
        if cmd["how"] == "exec":
            # A tool subprocess running a real command (CAD-276).
            r = subprocess.run(cmd["argv"], capture_output=True, text=True)
            land(out, {"rc": r.returncode, "out": r.stdout, "err": r.stderr})
            n += 1
            continue
        frame = json.dumps(cmd["frame"])
        me = sys.executable, os.path.abspath(__file__)
        if cmd["how"] == "self":
            land(out, rpc(sock_path, cmd["frame"]))
        elif cmd["how"] == "child":
            # Answer only once the child is reaped: its hold's holder
            # is then provably dead for the next reap pass.
            subprocess.run([*me, "--child", sock_path, frame, out + ".c"], check=True)
            os.rename(out + ".c", out)
        else:
            # `detached-bare`: the detach also scrubs CADENCE_ALIAS.
            env = dict(os.environ)
            if cmd["how"] == "detached-bare":
                env.pop("CADENCE_ALIAS", None)
            subprocess.run([*me, "--detached", sock_path, frame, out,
                            str(os.getpid())], check=True, env=env)
        n += 1

threading.Thread(target=serve, daemon=True).start()
for _ in sys.stdin:
    pass
"#;

/// The managed provider under test and its request channel.
pub struct ManagedWorker {
    pub pid: u32,
    pub cmd_dir: TempDir,
    pub seq: u64,
    pub _mock: MockClaude,
}

/// The enrollment mock installed as this test's claude command, not yet
/// registered — [`ManagedWorker::install`] before a `daemon run` process
/// exists (it takes its env at spawn), then [`Self::enroll`].
pub struct ManagedWorkerMock {
    pub cmd_dir: TempDir,
    pub mock: MockClaude,
}

impl ManagedWorker {
    /// Register `alias` as a managed claude endpoint running the
    /// enrollment mock and wait for the daemon to enroll it.
    pub fn start(d: &TestDaemon, alias: &str) -> ManagedWorker {
        Self::start_role(d, alias, "worker")
    }

    /// [`Self::start`] registering the endpoint with `role`.
    pub fn start_role(d: &TestDaemon, alias: &str, role: &str) -> ManagedWorker {
        Self::install(d.dir.path(), &d.state, alias).enroll_role(d, alias, role)
    }

    /// Write the enrollment mock into `dir` and install it as the claude
    /// command for a daemon serving `state`.
    pub fn install(dir: &Path, state: &Path, alias: &str) -> ManagedWorkerMock {
        let cmd_dir = TempDir::new().unwrap();
        let pidfile = dir.join(format!("claude-{alias}.pid"));
        let script = dir.join("claude-enroll.py");
        std::fs::write(&script, MOCK_ENROLL_PY).unwrap();
        test_env().set(
            "CADENCE_CLAUDE_COMMAND",
            format!(
                "python3 {} {} {} {}",
                script.display(),
                pidfile.display(),
                client::socket_path(state).display(),
                cmd_dir.path().display()
            ),
        );
        ManagedWorkerMock {
            cmd_dir,
            mock: MockClaude { pidfile },
        }
    }
}

impl ManagedWorkerMock {
    /// Register `alias` as a managed claude worker and wait for `d` to
    /// enroll the provider it launched.
    pub fn enroll(self, d: &TestDaemon, alias: &str) -> ManagedWorker {
        self.enroll_role(d, alias, "worker")
    }

    /// [`Self::enroll`] registering the endpoint with `role`.
    pub fn enroll_role(self, d: &TestDaemon, alias: &str, role: &str) -> ManagedWorker {
        let ManagedWorkerMock { cmd_dir, mock } = self;
        let cwd = d.dir.path().to_str().unwrap().to_string();
        d.fixture_rpc(
            "agent_register",
            json!({"alias": alias, "provider": "claude",
                   "endpoint_kind": "managed", "cwd": cwd, "role": role}),
        )
        .unwrap();
        ManagedWorkerMock { cmd_dir, mock }.enrolled(d, alias)
    }

    /// Wait for `d` to enroll the provider it launched for an `alias`
    /// someone else registered (CAD-339: `master_start`).
    pub fn enrolled(self, d: &TestDaemon, alias: &str) -> ManagedWorker {
        let ManagedWorkerMock { cmd_dir, mock } = self;
        // The daemon's own record of the provider it launched — the
        // mock may not even have written its pidfile yet.
        let deadline = Instant::now() + Duration::from_secs(20);
        let pid = loop {
            let enrolled = d
                .events(alias)
                .into_iter()
                .find(|e| e["kind"].as_str() == Some("slot_enrolled"));
            if let Some(e) = enrolled {
                break e["payload"]["root_pid"].as_u64().unwrap() as u32;
            }
            assert!(
                Instant::now() < deadline,
                "{alias} was never enrolled: {:?}",
                d.events(alias)
            );
            thread::sleep(Duration::from_millis(25));
        };
        ManagedWorker {
            pid,
            cmd_dir,
            seq: 0,
            _mock: mock,
        }
    }
}

impl ManagedWorker {
    /// One slot RPC performed `how` = `self` | `child` | `detached` |
    /// `detached-bare`; the answer is the wire frame.
    pub fn rpc(&mut self, how: &str, method: &str, params: Value) -> Value {
        self.request(
            json!({"how": how, "frame": {"method": method, "params": params}}),
            &format!("{how} {method}"),
        )
    }

    /// Run `argv` as the provider's tool subprocess — `{rc, out, err}`.
    pub fn exec(&mut self, argv: &[&str]) -> Value {
        self.request(
            json!({"how": "exec", "argv": argv}),
            argv.join(" ").as_str(),
        )
    }

    pub fn request(&mut self, cmd: Value, what: &str) -> Value {
        let n = self.send(cmd);
        self.answer(n, what)
    }

    /// Hand the worker one command without waiting — its number for
    /// [`Self::answer`]. The worker serves commands one at a time.
    pub fn send(&mut self, cmd: Value) -> u64 {
        let n = self.seq;
        self.seq += 1;
        let req = self.cmd_dir.path().join(format!("req-{n}.json"));
        let tmp = req.with_extension("tmp");
        std::fs::write(&tmp, cmd.to_string()).unwrap();
        std::fs::rename(&tmp, &req).unwrap();
        n
    }

    /// Wait for command `n`'s answer.
    pub fn answer(&self, n: u64, what: &str) -> Value {
        let resp = self.cmd_dir.path().join(format!("resp-{n}.json"));
        let deadline = Instant::now() + Duration::from_secs(20);
        while !resp.exists() {
            assert!(
                Instant::now() < deadline,
                "managed worker never answered {what}"
            );
            thread::sleep(Duration::from_millis(20));
        }
        serde_json::from_str(&std::fs::read_to_string(&resp).unwrap()).unwrap()
    }
}

/// The `daemon run` process's pid, and that it is the child subreaper.
pub fn subreaper_daemon_pid(d: &TestDaemon) -> u64 {
    let health = d.rpc("health", json!({})).unwrap();
    assert_eq!(health["child_subreaper"], true, "{health}");
    health["pid"].as_u64().unwrap()
}

/// The wire frame is a refusal whose message names `verb` and `rule`.
pub fn assert_refused(frame: &Value, verb: &str, rule: &str, what: &str) {
    assert_eq!(frame["ok"], false, "{what}: {frame}");
    let msg = frame["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains(verb) && msg.contains(rule),
        "{what}: expected '{verb}' + '{rule}' in {frame}"
    );
}

/// A pane's claimed identity, in every field a caller could forge.
pub const FORGED_IDENTITY: &[(&str, &str)] = &[
    ("pane", "operator"),
    ("by", "operator"),
    ("reviewer", "operator"),
    ("owner", "operator"),
];

/// `params` with `field` claiming `value`.
pub fn forged(params: &Value, field: &str, value: &str) -> Value {
    let mut p = params.clone();
    p[field] = json!(value);
    p
}

/// Run the built CLI against `d`'s state dir.
pub fn launch_cli(d: &TestDaemon, args: &[&str]) -> std::process::Output {
    // The operator's shell: launches register agents (CAD-431).
    std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(args)
        .operator_output()
        .unwrap()
}

// ---------- CAD-109: pre-publish secret scan ----------

/// A synthetic token: `prefix` plus `n` letters and digits drawn from a
/// seeded SHA-256 stream. It is built at run time so this file never holds a
/// credential-shaped literal.
pub fn cad109_token(prefix: &str, seed: &str, n: usize) -> String {
    use sha2::{Digest, Sha256};
    const ALPHANUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = prefix.to_string();
    let mut counter = 0u32;
    while out.len() < prefix.len() + n {
        for b in Sha256::digest(format!("{seed}:{counter}").as_bytes()) {
            if out.len() < prefix.len() + n {
                out.push(ALPHANUM[b as usize % ALPHANUM.len()] as char);
            }
        }
        counter += 1;
    }
    out
}

// ==== CAD-201 / CAD-202: pty lane process tree and cwd integrity ====

/// `(state, sid, start_time)` from `/proc/<pid>/stat` — `None` once the
/// pid is gone.
pub fn lane_stat(pid: u32) -> Option<(char, u32, u64)> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let end = text.rfind(')')?;
    let f: Vec<&str> = text[end + 1..].split_whitespace().collect();
    Some((
        f.first()?.chars().next()?,
        f.get(3)?.parse().ok()?,
        f.get(19)?.parse().ok()?,
    ))
}

/// Test-owned `sleep` children, SIGKILLed on drop if a failed assertion
/// left them running — matched by pid + start time, never pid alone.
pub struct LaneSleepers(pub Vec<(u32, u64)>);

impl Drop for LaneSleepers {
    fn drop(&mut self) {
        for &(pid, start) in &self.0 {
            if lane_stat(pid).is_some_and(|(state, _, s)| s == start && state != 'Z') {
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            }
        }
    }
}

/// Run the cadence CLI against `d` with a hermetic tracker/home.
pub fn lane_cli(d: &TestDaemon, pm_dir: &Path, home: &Path, args: &[&str]) -> (bool, Value) {
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(args)
        .env("CADENCE_PM_DIR", pm_dir)
        .env("HOME", home)
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("CADENCE_ALIAS")
        .operator_output()
        .unwrap();
    let text = if out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stderr).to_string()
    } else {
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    (
        out.status.success(),
        serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
    )
}

/// Live `daemon run` processes serving `state` — read from /proc, so a
/// child that lost the singleton lock and exited is not counted.
pub fn daemon_run_pids(state: &Path) -> Vec<u64> {
    let state = state.to_str().unwrap();
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u64>() else {
            continue;
        };
        let Ok(bytes) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<String> = bytes
            .split(|b| *b == 0)
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        let serves = args
            .windows(4)
            .any(|w| w[0] == "--state-dir" && w[1] == state && w[2] == "daemon" && w[3] == "run");
        if serves {
            pids.push(pid);
        }
    }
    pids
}

/// `daemon_run_pids`, narrowed to processes whose executable is this
/// test build — a reaper must never signal anything else.
pub fn own_daemon_run_pids(state: &Path) -> Vec<u64> {
    let bin = Path::new(env!("CARGO_BIN_EXE_cadence"));
    let bin = bin.canonicalize().unwrap_or_else(|_| bin.to_path_buf());
    daemon_run_pids(state)
        .into_iter()
        .filter(|pid| {
            std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|exe| {
                let exe = exe.to_string_lossy();
                Path::new(exe.strip_suffix(" (deleted)").unwrap_or(&exe)) == bin
            })
        })
        .collect()
}

/// Poll until no `daemon run` of this build serves `state`, or `within`
/// passes. Returns the pids still alive.
pub fn wait_daemons_gone(state: &Path, within: Duration) -> Vec<u64> {
    let deadline = Instant::now() + within;
    loop {
        let pids = own_daemon_run_pids(state);
        if pids.is_empty() || Instant::now() >= deadline {
            return pids;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// CAD-306: reaps every `daemon run` a test's CLI calls started against
/// `state`, on every exit path. `Drop` also runs while a failed
/// assertion unwinds, so a test that panics before its own
/// `daemon stop` no longer orphans a detached daemon to init.
///
/// Declare it right after the state dir's owner (`TempDir` or
/// `TestDaemon`) so it drops first, while the directory still exists.
/// It asks `daemon stop` (bounded), then SIGTERM, then SIGKILL — and
/// only ever touches processes running this test build with this
/// state dir on their command line, never the fleet daemon.
pub struct DaemonReaper {
    pub state: PathBuf,
    /// HOME for the `daemon stop` call — never the operator's.
    pub home: TempDir,
}

impl DaemonReaper {
    pub fn new(state: &Path) -> Self {
        Self {
            state: state.to_path_buf(),
            home: TempDir::new().unwrap(),
        }
    }
}

impl Drop for DaemonReaper {
    fn drop(&mut self) {
        if own_daemon_run_pids(&self.state).is_empty() {
            return;
        }
        let stop = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&self.state)
            .args(["daemon", "stop"])
            .env("HOME", self.home.path())
            .env_remove("CADENCE_ALIAS")
            .env_remove("CADENCE_ROLLOUT_AS")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Ok(mut stop) = stop {
            let deadline = Instant::now() + Duration::from_secs(10);
            while matches!(stop.try_wait(), Ok(None)) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(50));
            }
            let _ = stop.kill();
            let _ = stop.wait();
        }
        let mut left = wait_daemons_gone(&self.state, Duration::from_secs(1));
        for signal in [libc::SIGTERM, libc::SIGKILL] {
            if left.is_empty() {
                return;
            }
            for pid in &left {
                unsafe { libc::kill(*pid as libc::pid_t, signal) };
            }
            left = wait_daemons_gone(&self.state, Duration::from_secs(2));
        }
        if !left.is_empty() {
            eprintln!(
                "DaemonReaper: daemon run {left:?} for {} survived SIGKILL",
                self.state.display()
            );
        }
    }
}

// ---- CAD-96: idle auto-stop (default ON in production, pinned here) ----

/// A daemon with idle auto-stop pinned to `setting` and its clock at
/// wall time plus the returned offset (seconds) — a test ages every
/// agent by moving the offset, never by sleeping.
pub fn auto_stop_daemon(
    setting: daemon::AutoStopSetting,
) -> (TestDaemon, std::sync::Arc<std::sync::atomic::AtomicI64>) {
    let offset = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let d = TestDaemon::start_opts(auto_stop_opts(setting, &offset));
    (d, offset)
}

/// Daemon options with idle auto-stop pinned to `setting` on a clock
/// at wall time plus `offset` seconds.
pub fn auto_stop_opts(
    setting: daemon::AutoStopSetting,
    offset: &std::sync::Arc<std::sync::atomic::AtomicI64>,
) -> daemon::ServeOptions {
    let o = std::sync::Arc::clone(offset);
    daemon::ServeOptions {
        auto_stop: Some(setting),
        auto_stop_clock: Some(std::sync::Arc::new(move || {
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs_f64()
                + o.load(std::sync::atomic::Ordering::SeqCst) as f64
        })),
        ..daemon_opts()
    }
}

/// Wait until `alias` is stopped AND its `agent_auto_stopped` record
/// has landed — the stop path writes `stopped` first, the timer records
/// the event once that path returns.
pub fn wait_auto_stopped(d: &TestDaemon, alias: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let agent = d.rpc("agent_show", json!({"alias": alias})).unwrap()["agent"].clone();
        if agent["state"] == "stopped" && agent["state_label"].is_string() {
            return agent;
        }
        assert!(
            Instant::now() < deadline,
            "{alias} never auto-stopped: {agent}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// Serve the board in-process over `pm` and the daemon's `state` on a
/// free loopback port and wait for health. A lost bind race (another
/// test took the port) retries on a fresh one; the probe's `Host`
/// names the port, so only OUR server answers 200.
pub fn start_board(pm: &Path, state: &Path) -> u16 {
    start_board_with(pm, state, false)
}

/// [`start_board`], optionally serving read-only (`--read-only`).
pub fn start_board_with(pm: &Path, state: &Path, read_only: bool) -> u16 {
    start_board_gh(pm, state, read_only, None)
}

/// [`start_board_with`] whose Merge runs `gh` (CAD-431: a fake).
pub fn start_board_gh(pm: &Path, state: &Path, read_only: bool, gh: Option<PathBuf>) -> u16 {
    start_board_sync(pm, state, read_only, gh, None)
}

/// [`start_board_gh`] whose delivery sync (CAD-446) runs every `every`.
pub fn start_board_sync(
    pm: &Path,
    state: &Path,
    read_only: bool,
    gh: Option<PathBuf>,
    every: Option<Duration>,
) -> u16 {
    use std::io::Read;
    let overall = Instant::now() + Duration::from_secs(20);
    loop {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let (sd, pd, gh) = (state.to_path_buf(), pm.to_path_buf(), gh.clone());
        thread::spawn(move || {
            let opts = cadence_agent::ui::ServeOpts {
                host: "127.0.0.1".to_string(),
                port,
                read_only,
                gh,
                delivery_sync_every: every,
                ..Default::default()
            };
            let _ = cadence_agent::ui::serve(&sd, &pd, &opts);
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                s.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let probe = format!("GET /api/health HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n");
                let _ = s.write_all(probe.as_bytes());
                let mut buf = String::new();
                if s.read_to_string(&mut buf).is_ok() && buf.contains("200") {
                    return port;
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            assert!(Instant::now() < overall, "board server did not start");
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// A board started by [`start_operator_board`]; dropping it runs
/// `cadence ui stop`.
pub struct OperatorBoard(pub PathBuf);

impl Drop for OperatorBoard {
    fn drop(&mut self) {
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&self.0)
            .args(["ui", "stop"])
            .output();
    }
}

/// Plain HTTP over bash's `/dev/tcp`: `$1` is the port, `$2` the raw
/// request; the reply goes to stdout.
pub const DEV_TCP_CLIENT: &str =
    r#"exec 3<>"/dev/tcp/127.0.0.1/$1"; printf '%s' "$2" >&3; cat <&3"#;

/// Forwards ONE connection from a fresh loopback port (printed first)
/// to 127.0.0.1:`argv[1]` — the shape of the operator's tailnet relay
/// (`socat TCP-LISTEN:13010,fork TCP:127.0.0.1:3010`), whose forking
/// child is the board's TCP peer.
pub const RELAY_PY: &str = r#"
import socket, sys, threading
ls = socket.socket()
ls.bind(("127.0.0.1", 0))
ls.listen(1)
print(ls.getsockname()[1], flush=True)
c, _ = ls.accept()
u = socket.create_connection(("127.0.0.1", int(sys.argv[1])))
def pump(a, b):
    while True:
        data = a.recv(65536)
        if not data:
            break
        b.sendall(data)
    try:
        b.shutdown(socket.SHUT_WR)
    except OSError:
        pass
t = threading.Thread(target=pump, args=(u, c))
t.start()
pump(c, u)
t.join()
"#;

/// CAD-359/360 fixture: a tracker with project `demo` (one repo) bound
/// to a fresh daemon through its own env, and a CLI runner over both.
pub struct PlanFixture {
    pub d: TestDaemon,
    pub tmp: TempDir,
    pub pm_dir: PathBuf,
}

impl PlanFixture {
    pub fn start() -> PlanFixture {
        Self::start_with(daemon_opts())
    }

    pub fn start_with(opts: daemon::ServeOptions) -> PlanFixture {
        Self::start_on(move || TestDaemon::start_opts(opts))
    }

    /// [`Self::start`] over the daemon `daemon` starts — a real `daemon
    /// run` process ([`TestDaemon::start_process_in`]) takes its env at
    /// spawn, after `CADENCE_PM_DIR` is set here.
    pub fn start_on(daemon: impl FnOnce() -> TestDaemon) -> PlanFixture {
        let tmp = TempDir::new().unwrap();
        let (pm_dir, repo) = (tmp.path().join("pm"), tmp.path().join("repo"));
        for sub in ["home", "tmp"] {
            std::fs::create_dir_all(tmp.path().join(sub)).unwrap();
        }
        std::fs::create_dir_all(&repo).unwrap();
        let git = |dir: &Path, args: &[&str]| {
            let o = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success(), "git {args:?}: {o:?}");
        };
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("f"), "x").unwrap();
        git(&repo, &["add", "-A"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "init",
            ],
        );
        // The daemon's own env: never the host's ~/pm.
        test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
        let f = PlanFixture {
            d: daemon(),
            tmp,
            pm_dir,
        };
        assert!(f.cli(&["issue", "init"]).0);
        let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
        let (ok, out) = f.cli(&[
            "issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s,
        ]);
        assert!(ok, "{out}");
        f
    }

    /// `cadence <args>` against this tracker and daemon, outside any
    /// pane: (success, stdout JSON or stderr text as a JSON string).
    pub fn cli(&self, args: &[&str]) -> (bool, Value) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&self.d.state)
            .args(args)
            .env("CADENCE_PM_DIR", &self.pm_dir)
            .env("HOME", self.tmp.path().join("home"))
            .env("XDG_CONFIG_HOME", self.tmp.path().join("home/.config"))
            .env("XDG_DATA_HOME", self.tmp.path().join("home/.local/share"))
            .env("XDG_STATE_HOME", self.tmp.path().join("home/.local/state"))
            .env("TMPDIR", self.tmp.path().join("tmp"))
            .env_remove("CADENCE_ALIAS")
            .operator_output()
            .unwrap();
        let text = if out.stdout.is_empty() {
            String::from_utf8_lossy(&out.stderr).to_string()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        let value = serde_json::from_str(text.trim()).unwrap_or(Value::String(text));
        (out.status.success(), value)
    }

    pub fn commits(&self) -> usize {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.pm_dir)
            .args(["rev-list", "--count", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }

    pub fn last_commit(&self) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.pm_dir)
            .args(["log", "-1", "--format=%B"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    pub fn front(&self, id: &str) -> cadence_agent::issue::model::Front {
        let text =
            std::fs::read_to_string(self.pm_dir.join("demo").join(id).join("issue.md")).unwrap();
        cadence_agent::issue::parse::parse_issue(&text).unwrap().0
    }

    /// Rewrite an issue file directly — a hand edit, or what an older
    /// binary that does not know a field writes back.
    pub fn write_front(&self, id: &str, front: &cadence_agent::issue::model::Front) {
        let path = self.pm_dir.join("demo").join(id).join("issue.md");
        let text = std::fs::read_to_string(&path).unwrap();
        let body = cadence_agent::issue::parse::parse_issue(&text).unwrap().1;
        std::fs::write(
            &path,
            cadence_agent::issue::parse::render(front, &body).unwrap(),
        )
        .unwrap();
    }

    /// Branches and worktrees `issue start` would have made in the repo.
    pub fn lanes(&self) -> (String, bool) {
        let repo = self.tmp.path().join("repo");
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["branch", "--list", "cadence/*"])
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            repo.join(".cadence").join("wt").exists(),
        )
    }

    pub fn propose(&self, text: &str) -> cadence_agent::Result<Value> {
        self.d
            .operator_rpc("plan_propose", json!({"project": "demo", "text": text}))
    }

    /// Daemon-stream events of `kind`, read over a read-only connection:
    /// `Store::open` would run crash recovery and reset the runtime rows
    /// of planted panes mid-test.
    pub fn daemon_events(&self, kind: &str) -> Vec<Value> {
        let conn = rusqlite::Connection::open_with_flags(
            self.d.state.join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut stmt = conn
            .prepare("SELECT payload FROM events WHERE alias=?1 AND kind=?2 ORDER BY seq")
            .unwrap();
        let rows = stmt
            .query_map(rusqlite::params![Store::DAEMON_STREAM, kind], |r| {
                r.get::<_, String>(0)
            })
            .unwrap();
        rows.map(|r| serde_json::from_str(&r.unwrap()).unwrap())
            .collect()
    }
}

pub const PLAN_MD: &str = "---\ntitle: Onboarding\ngoal: First chat in five minutes\n\
non_goals: [billing]\n---\n\nWhy this plan.\n\n## Wizard\nsize: L\nagent: dev-1\n\n\
The setup wizard.\n\n### Acceptance\n- [ ] wizard runs\n- [ ] chat opens\n\n\
## Docs\nsize: S\ndepends_on: 1\n\n### Acceptance\n- [ ] README updated\n\n\
## Polish\n\n### Acceptance\n- [ ] copy reviewed\n";

/// The two-step workflow every test below adds to `demo`.
pub const WF_TWO_STEP: &str = "---\ntitle: \"Change: {{title}}\"\ngoal: \"Ship {{title}}\"\n\
inputs:\n  title: { ask: \"What change?\" }\n  note: { optional: true }\n---\n\n\
## Do {{title}}\nagent: dev-1\nsize: S\n\n### Acceptance\n- [ ] done\n\n\
## Check {{title}}\nagent: qa-1\ndepends_on: 1\n\n### Acceptance\n- [ ] verified\n";

/// A workflow whose two tickets are pinned apart by `distinct:` — the
/// second ticket's `agent:` is the reviewer (like code-change.md), so
/// worker == reviewer refuses `not_distinct` at render.
pub const WF_PAIR: &str = "---\ntitle: \"Pair: {{title}}\"\ngoal: \"Do {{title}}\"\n\
inputs:\n  title: {}\n  worker: {}\n  reviewer: {}\ndistinct: [worker, reviewer]\n---\n\n\
## Do {{title}}\nagent: {{worker}}\n\n### Acceptance\n- [ ] done\n\n\
## Check {{title}}\nagent: {{reviewer}}\ndepends_on: 1\n\n### Acceptance\n- [ ] verified\n";

/// A workflow with a bare `agent:` placeholder — a one-line input that
/// is no alias (`has spaces`) still renders an invalid plan, so the
/// skeleton guard answers `render_diverged`.
pub const WF_ALIAS: &str = "---\ntitle: \"Alias: {{runner}}\"\ngoal: g\n\
inputs:\n  runner: {}\n---\n\n\
## Work\nagent: {{runner}}\n\n### Acceptance\n- [ ] done\n";

/// Write `text` into the fixture's scratch dir; answer its path.
pub fn wf_file(f: &PlanFixture, name: &str, text: &str) -> String {
    let path = f.tmp.path().join(name);
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_string()
}

/// `workflow add` the text as `name` in `demo` — asserts success.
pub fn wf_add(f: &PlanFixture, name: &str, text: &str) -> Value {
    let file = wf_file(f, &format!("{name}.md"), text);
    let (ok, out) = f.cli(&[
        "workflow",
        "add",
        name,
        "--project",
        "demo",
        "--file",
        &file,
    ]);
    assert!(ok, "workflow add {name}: {out}");
    out
}

/// The issue's body text (what follows the frontmatter).
pub fn issue_body(f: &PlanFixture, id: &str) -> String {
    let text = std::fs::read_to_string(f.pm_dir.join("demo").join(id).join("issue.md")).unwrap();
    cadence_agent::issue::parse::parse_issue(&text).unwrap().1
}

/// One raw HTTP/1.0 exchange with the board: `(status, body)`.
pub fn board_http(port: u16, request: &str) -> (u16, String) {
    use std::io::Read;
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).ok();
    s.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    s.read_to_string(&mut response).unwrap();
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .or_else(|| response.split_once("\n\n"))
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

pub fn board_get(port: u16, path: &str) -> (u16, String) {
    board_http(
        port,
        &format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    )
}

/// The Host a raw board request carries: the board's own name when the
/// headers carry an operator session (CAD-313: sessions live only on
/// `cadence-<port>.localhost`), else plain `127.0.0.1:<port>`.
pub fn board_host_for(port: u16, headers: &str) -> String {
    if headers.contains("Cookie: cadence_operator_") {
        format!("cadence-{port}.localhost:{port}")
    } else {
        format!("127.0.0.1:{port}")
    }
}

pub const THREAD_GUARDS: &str = "Content-Type: application/json\r\nX-Cadence-Board: 1\r\n";

/// Sign in to the board on `port` as the operator (CAD-313): the real
/// `cadence ui login` link, exchanged at `POST /api/session`.
pub fn sign_in(state: &Path, port: u16) -> op::Session {
    op::sign_in(env!("CARGO_BIN_EXE_cadence"), state, port)
}

/// The write guards plus a signed-in operator's Origin and cookie.
pub fn op_guards(op: &op::Session) -> String {
    format!("{THREAD_GUARDS}{}", op.headers())
}

/// A raw board POST to `path` with `headers` (each `Name: value\r\n`).
pub fn cad328_post(port: u16, path: &str, headers: &str, body: &str) -> String {
    let host = board_host_for(port, headers);
    format!(
        "POST {path} HTTP/1.0\r\nHost: {host}\r\n{headers}\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// A relay started as its own session — `setsid -f`, env cleared, stdio
/// null: the shape of a gateway (nginx, `socat`, cloudflared) the
/// operator runs — forwarding ONE connection from a fresh loopback port
/// (written to `argv[2]`) to 127.0.0.1:`argv[1]`.
pub const SESSION_RELAY_PY: &str = r#"
import os, socket, sys, threading
target, portfile = int(sys.argv[1]), sys.argv[2]
ls = socket.socket()
ls.bind(("127.0.0.1", 0))
ls.listen(1)
with open(portfile + ".tmp", "w") as f:
    f.write(str(ls.getsockname()[1]))
os.rename(portfile + ".tmp", portfile)
ls.settimeout(60)
c, _ = ls.accept()
u = socket.create_connection(("127.0.0.1", target))
def pump(a, b):
    while True:
        data = a.recv(65536)
        if not data:
            break
        b.sendall(data)
    try:
        b.shutdown(socket.SHUT_WR)
    except OSError:
        pass
t = threading.Thread(target=pump, args=(u, c))
t.start()
pump(c, u)
t.join()
"#;

// ==== CAD-339: the master agent ====

/// A three-ticket plan, as the master would write it.
pub const MASTER_PLAN: &str = "---\ntitle: Reminders\ngoal: Users get a reminder email\n---\n\n\
## Schema\nsize: S\nagent: w1\n\nThe reminders table.\n\n### Acceptance\n- [ ] migration adds reminders\n\n\
## Sender\nsize: M\nagent: w1\ndepends_on: 1\n\n### Acceptance\n- [ ] an email goes out at the due time\n\n\
## Settings\nsize: S\nagent: w2\ndepends_on: 1\n\n### Acceptance\n- [ ] a user can turn reminders off\n";

/// The six reflection headings a done/question report carries.
pub const REFLECTION: &str = "## Expected\ne\n## Evidence\nv\n## Cause\nc\n## Correction\nnone\n\
## Lesson\nnone\n## Next\nnone\n";

impl PlanFixture {
    /// A fixture whose daemon runs the report router.
    pub fn start_routed() -> PlanFixture {
        Self::start_with(daemon::ServeOptions {
            report_router: Some(1),
            ..daemon_opts()
        })
    }

    /// `cli` as agent `alias` (its `CADENCE_ALIAS`), outside any pane.
    pub fn cli_as(&self, alias: &str, args: &[&str]) -> (bool, Value) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&self.d.state)
            .args(args)
            .env("CADENCE_PM_DIR", &self.pm_dir)
            .env("HOME", self.tmp.path().join("home"))
            .env("XDG_CONFIG_HOME", self.tmp.path().join("home/.config"))
            .env("TMPDIR", self.tmp.path().join("tmp"))
            .env("CADENCE_ALIAS", alias)
            .output()
            .unwrap();
        let text = if out.stdout.is_empty() {
            String::from_utf8_lossy(&out.stderr).to_string()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        let value = serde_json::from_str(text.trim()).unwrap_or(Value::String(text));
        (out.status.success(), value)
    }

    /// `master_start` (as the operator) with the enrollment mock as the
    /// master's claude; waits for the daemon to enroll it.
    pub fn start_master(&self) -> (ManagedWorker, Value) {
        self.start_master_with(json!({"provider": "claude"}))
    }

    /// [`Self::start_master`] with explicit `master_start` params.
    pub fn start_master_with(&self, params: Value) -> (ManagedWorker, Value) {
        // The daemon's `$HOME` is the fixture's — `master start` copies
        // a Claude login from it (CAD-439), never from the host's.
        test_env().set("HOME", self.tmp.path().join("home").to_str().unwrap());
        // CAD-439: the master's provider runs under `cadence confine`.
        // The built binary applies it (`current_exe` is this runner),
        // and the mock's own files — script, pidfile, command dir — are
        // the only extra paths it gets: never the test's state dir.
        // `/proc` too: the mock's detached grandchild walks its ancestry
        // there (the real provider gets only `/proc/self`).
        let mock_dir = self.tmp.path().join("mock");
        std::fs::create_dir_all(&mock_dir).unwrap();
        let mock = ManagedWorker::install(&mock_dir, &self.d.state, "master");
        test_env().set("CADENCE_CONFINE_COMMAND", env!("CARGO_BIN_EXE_cadence"));
        test_env().set(cadence_agent::master::CONFINE_EXTRA_READ_ENV, "/proc");
        test_env().set(
            cadence_agent::master::CONFINE_EXTRA_WRITE_ENV,
            format!("{}:{}", mock_dir.display(), mock.cmd_dir.path().display()),
        );
        let out = self.d.operator_rpc("master_start", params).unwrap();
        assert_eq!(out["alias"], "master", "{out}");
        (mock.enrolled(&self.d, "master"), out)
    }

    /// The shell line the master's tool subprocess runs for `cadence
    /// <args>` — isolated home, the daemon's state dir.
    pub fn master_line(&self, args: &str) -> String {
        let home = self.tmp.path().join("home");
        format!(
            "env HOME={h} XDG_CONFIG_HOME={h}/.config TMPDIR={t} {bin} --state-dir {s} {args}",
            h = home.display(),
            // CAD-439: the master's own TMPDIR — `/tmp` is outside it.
            t = cadence_agent::master::tmpdir(&self.d.state).display(),
            bin = env!("CARGO_BIN_EXE_cadence"),
            s = self.d.state.display(),
        )
    }

    /// `cadence <args>` run BY the master — a tool subprocess of its
    /// provider, so the daemon attributes it to `master`.
    pub fn as_master(&self, m: &mut ManagedWorker, args: &str) -> (bool, Value) {
        let r = m.exec(&["sh", "-c", &self.master_line(args)]);
        let out = r["out"].as_str().unwrap_or_default();
        let text = if out.trim().is_empty() {
            r["err"].as_str().unwrap_or_default()
        } else {
            out
        };
        let value =
            serde_json::from_str(text.trim()).unwrap_or_else(|_| Value::String(text.to_string()));
        (r["rc"] == 0, value)
    }

    /// A file in the master's own temp dir — inside its confinement
    /// (CAD-439), so the master's `--file` reads reach it.
    pub fn file(&self, name: &str, text: &str) -> String {
        let dir = cadence_agent::master::tmpdir(&self.d.state);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path.to_str().unwrap().to_string()
    }

    pub fn thread(&self, alias: &str) -> Vec<Value> {
        self.d
            .rpc("thread_read", json!({"alias": alias, "limit": 500}))
            .unwrap()["entries"]
            .as_array()
            .unwrap()
            .clone()
    }

    /// Wait for an entry of the master's thread containing `needle`.
    pub fn wait_thread(&self, needle: &str, secs: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(e) = self
                .thread("master")
                .into_iter()
                .find(|e| e["text"].as_str().is_some_and(|t| t.contains(needle)))
            {
                return e;
            }
            assert!(
                Instant::now() < deadline,
                "no master thread entry contains {needle:?}: {:#?}",
                self.thread("master")
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn messages_of(&self, alias: &str) -> Vec<Value> {
        self.d.rpc("agent_show", json!({"alias": alias})).unwrap()["messages"]
            .as_array()
            .unwrap()
            .clone()
    }

    pub fn needs_me(&self) -> Vec<Value> {
        let home = self.tmp.path().join("home");
        overview_at(&home, &self.d.state, Some(&self.pm_dir), &[])["needs_me"]
            .as_array()
            .unwrap()
            .clone()
    }
}

// ==== CAD-431: the worker loop — review routing, verdicts, merge ====

/// A fake `gh` for the operator's process: `pr view` answers from
/// `gh-state.json` beside it, `pr merge` enqueues (refusing a head that
/// is not the pinned one) or disables auto-merge. Every call is logged
/// to `gh.log`.
pub const FAKE_GH_PY: &str = include_str!("../fixtures/fake-gh.py");

pub const LOOP_PR: &str = "https://github.com/acme/app/pull/7";

/// The loop's fixture: a routed tracker + daemon, the master, a managed
/// worker `w1`, managed agents `r1` (the reviewer: first by alias among
/// same-provider peers) and `r2` (a bystander), and a fake `gh` only the
/// operator's process has on its PATH.
pub struct LoopFixture {
    pub f: PlanFixture,
    pub m: ManagedWorker,
    pub w1: ManagedWorker,
    pub r1: ManagedWorker,
    pub r2: ManagedWorker,
    pub gh_dir: PathBuf,
}

impl LoopFixture {
    /// Up to D-2 dispatched by the master to w1.
    pub fn dispatched() -> LoopFixture {
        Self::dispatched_plan(MASTER_PLAN)
    }

    /// [`Self::dispatched`] from `plan` (epic D-1, first ticket D-2).
    /// The project's repo has the GitHub remote the loop's PRs live in.
    pub fn dispatched_plan(plan_md: &str) -> LoopFixture {
        let f = PlanFixture::start_routed();
        let yaml = f.pm_dir.join("demo/project.yaml");
        let text = std::fs::read_to_string(&yaml).unwrap();
        let mut project: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
        project["repos"][0]["remote"] = "https://github.com/Acme/app.git".into();
        std::fs::write(&yaml, serde_yaml::to_string(&project).unwrap()).unwrap();
        let git = |args: &[&str]| {
            let o = std::process::Command::new("git")
                .arg("-C")
                .arg(&f.pm_dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success(), "git {args:?}: {o:?}");
        };
        git(&["add", "-A"]);
        git(&["commit", "-qm", "demo: repo remote"]);
        let (mut m, _) = f.start_master();
        let w1 = ManagedWorker::start(&f.d, "w1");
        let r1 = ManagedWorker::start(&f.d, "r1");
        let r2 = ManagedWorker::start(&f.d, "r2");
        let plan = f.file("plan.md", plan_md);
        let (ok, out) = f.as_master(
            &mut m,
            &format!("plan propose --project demo --file {plan}"),
        );
        assert!(ok, "{out}");
        f.d.operator_rpc("plan_approve", json!({"epic": "D-1"}))
            .unwrap();
        let (ok, sent) = f.as_master(&mut m, "master dispatch D-2");
        assert!(ok, "{sent}");
        let gh_dir = f.tmp.path().join("ghbin");
        std::fs::create_dir_all(&gh_dir).unwrap();
        let gh = gh_dir.join("gh");
        std::fs::write(&gh, FAKE_GH_PY).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let lf = LoopFixture {
            f,
            m,
            w1,
            r1,
            r2,
            gh_dir,
        };
        lf.set_gh(&"0".repeat(40), "OPEN", false, false);
        assert_eq!(lf.rec()["state"], "working", "{}", lf.rec());
        lf
    }

    pub fn set_gh(&self, head: &str, state: &str, green: bool, auto: bool) {
        std::fs::write(
            self.gh_dir.join("gh-state.json"),
            json!({"head": head, "state": state, "green": green, "auto": auto}).to_string(),
        )
        .unwrap();
    }

    pub fn gh_log(&self) -> String {
        std::fs::read_to_string(self.gh_dir.join("gh.log")).unwrap_or_default()
    }

    pub fn rec(&self) -> Value {
        self.f
            .d
            .rpc("delivery_list", json!({"issue": "D-2"}))
            .unwrap()["records"][0]
            .clone()
    }

    pub fn wait_rec(&self, what: &str, ok: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let r = self.rec();
            if ok(&r) {
                return r;
            }
            assert!(Instant::now() < deadline, "never {what}: {r:#}");
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// What a refusal must leave untouched: tracker commits, the loop's
    /// record, and every queued message.
    pub fn snapshot(&self) -> (usize, String, usize, usize, usize) {
        (
            self.f.commits(),
            std::fs::read_to_string(self.f.d.state.join("delivery.json")).unwrap_or_default(),
            self.f.messages_of("w1").len(),
            self.f.messages_of("r1").len(),
            self.gh_log()
                .lines()
                .filter(|l| l.contains("pr merge"))
                .count(),
        )
    }

    /// `cadence <args>` run by a managed agent's tool subprocess.
    pub fn as_agent(&mut self, who: &str, args: &str) -> (bool, Value) {
        let line = format!(
            "env CADENCE_PM_DIR={} {}",
            self.f.pm_dir.display(),
            self.f.master_line(args)
        );
        let agent = match who {
            "w1" => &mut self.w1,
            "r1" => &mut self.r1,
            "r2" => &mut self.r2,
            _ => unreachable!(),
        };
        let r = agent.exec(&["sh", "-c", &line]);
        let out = r["out"].as_str().unwrap_or_default();
        let text = if out.trim().is_empty() {
            r["err"].as_str().unwrap_or_default()
        } else {
            out
        };
        let value =
            serde_json::from_str(text.trim()).unwrap_or_else(|_| Value::String(text.to_string()));
        (r["rc"] == 0, value)
    }

    /// `cadence <args>` from the operator's own shell, with the fake
    /// `gh` first on its PATH.
    pub fn operator(&self, args: &[&str]) -> (bool, Value) {
        let path = format!(
            "PATH={}:{}",
            self.gh_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let bin = env!("CARGO_BIN_EXE_cadence");
        let state = self.f.d.state.to_str().unwrap().to_string();
        let mut argv = vec!["env", path.as_str(), bin, "--state-dir", state.as_str()];
        argv.extend_from_slice(args);
        let script = self.f.d.dir.path().join("operator-cli.py");
        if !script.exists() {
            std::fs::write(&script, OPERATOR_CLI_PY).unwrap();
        }
        let out = self
            .f
            .d
            .dir
            .path()
            .join(format!("op-{}.json", uuid::Uuid::new_v4().simple()));
        let status = std::process::Command::new("setsid")
            .arg("-f")
            .arg("python3")
            .arg(&script)
            .arg(&out)
            .arg(std::process::id().to_string())
            .args(&argv)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.f.d.dir.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let deadline = Instant::now() + Duration::from_secs(60);
        while !out.exists() {
            assert!(
                Instant::now() < deadline,
                "operator {args:?} never finished"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let text = if v["stdout"].as_str().unwrap_or_default().trim().is_empty() {
            v["stderr"].as_str().unwrap_or_default().to_string()
        } else {
            v["stdout"].as_str().unwrap_or_default().to_string()
        };
        let value = serde_json::from_str(text.trim()).unwrap_or(Value::String(text));
        (v["rc"] == 0, value)
    }

    /// The worker files `done` at `sha` (its own report, by alias).
    pub fn done(&self, sha: &str) {
        self.done_on("D-2", sha, LOOP_PR);
    }

    pub fn done_on(&self, id: &str, sha: &str, pr: &str) {
        let report = self.f.file(
            &format!("done-{id}-{sha}-{}.md", pr.len()),
            &format!("---\nkind: done\nsha: {sha}\npr: {pr}\n---\n{REFLECTION}"),
        );
        let (ok, out) = self.f.cli_as(
            "w1",
            &[
                "report", "file", "--task", id, "--kind", "done", "--file", &report,
            ],
        );
        assert!(ok, "{out}");
    }

    /// w1's messages whose id starts with `prefix`.
    pub fn w1_messages(&self, prefix: &str) -> Vec<Value> {
        self.f
            .messages_of("w1")
            .into_iter()
            .filter(|m| m["id"].as_str().is_some_and(|i| i.starts_with(prefix)))
            .collect()
    }

    pub fn verdict_file(&self, name: &str, verdict: &str, sha: &str, extra: &str) -> String {
        self.f.file(
            name,
            &format!(
                "---\nverdict: {verdict}\nsha: {sha}\n{extra}---\n{verdict} findings: the \
                 migration lacks a down step.\n"
            ),
        )
    }

    pub fn verdict_as(&mut self, who: &str, verdict: &str, sha: &str) -> (bool, Value) {
        let file = self.verdict_file(&format!("v-{who}-{verdict}-{sha}.md"), verdict, sha, "");
        self.as_agent(
            who,
            &format!("report file --task D-2 --kind verdict --file {file}"),
        )
    }

    pub fn needs(&self, kind: &str) -> Vec<Value> {
        self.f
            .needs_me()
            .into_iter()
            .filter(|r| {
                r["kind"] == kind
                    || r["causes"]
                        .as_array()
                        .is_some_and(|c| c.iter().any(|c| c["cause"] == kind))
            })
            .collect()
    }
}

/// Three independent tickets for w1 — two PRs can race for one ticket.
pub const LOOP_PLAN: &str = "---\ntitle: Reminders\ngoal: Users get a reminder email\n---\n\n\
## Schema\nsize: S\nagent: w1\n\n### Acceptance\n- [ ] migration adds reminders\n\n\
## Sender\nsize: S\nagent: w1\n\n### Acceptance\n- [ ] an email goes out\n\n\
## Settings\nsize: S\nagent: w1\n\n### Acceptance\n- [ ] a user can turn reminders off\n";

// ==== CAD-449: a merged delivery marks its ticket done ====

/// Seven independent tickets for w1 (D-2 … D-8).
pub const DONE_PLAN: &str = "---\ntitle: Reminders\ngoal: Users get a reminder email\n---\n\n\
## Schema\nsize: S\nagent: w1\n\n### Acceptance\n- [ ] migration adds reminders\n\n\
## Sender\nsize: S\nagent: w1\n\n### Acceptance\n- [ ] an email goes out\n\n\
## Settings\nsize: S\nagent: w1\n\n### Acceptance\n- [ ] a user can turn reminders off\n\n\
## Digest\nsize: S\nagent: w1\n\n### Acceptance\n- [ ] a weekly digest goes out\n\n\
## Snooze\nsize: S\nagent: w1\n\n### Acceptance\n- [ ] a user can snooze one\n\n\
## Audit\nsize: S\nagent: w1\n\n### Acceptance\n- [ ] every send is logged\n\n\
## Export\nsize: S\nagent: w1\n\n### Acceptance\n- [ ] a user can export them\n";

impl LoopFixture {
    pub fn rec_of(&self, id: &str) -> Value {
        self.f.d.rpc("delivery_list", json!({"issue": id})).unwrap()["records"][0].clone()
    }

    pub fn wait_of(&self, id: &str, what: &str, ok: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let r = self.rec_of(id);
            if ok(&r) {
                return r;
            }
            assert!(Instant::now() < deadline, "{id} never {what}: {r:#}");
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// w1 reports `sha` done on `pr`; the review goes to r1, which
    /// PASSes it through `report_verdict`.
    pub fn pass_on(&mut self, id: &str, sha: &str, pr: &str) {
        self.done_on(id, sha, pr);
        let rec = self.wait_of(id, "in review", |r| {
            r["state"] == "reviewing" && r["head"] == sha
        });
        assert_eq!(rec["reviewer"], "r1", "{rec}");
        let file = self.verdict_file(&format!("v-{id}-{sha}.md"), "pass", sha, "");
        let (ok, out) = self.as_agent(
            "r1",
            &format!("report file --task {id} --kind verdict --file {file}"),
        );
        assert!(ok, "{out}");
        assert_eq!(out["delivery"]["state"], "passed", "{out}");
    }

    /// `cadence delivery sync <id>` from the operator's shell: its row.
    pub fn sync_of(&self, id: &str) -> Value {
        let (ok, out) = self.operator(&["delivery", "sync", id]);
        assert!(ok, "{out}");
        out["synced"][0].clone()
    }

    /// `delivery_observe` straight from the operator's connection — a
    /// replay of what a sync would hand the daemon.
    pub fn observe(&self, id: &str, head: &str, state: &str) -> Value {
        self.f
            .d
            .operator_rpc(
                "delivery_observe",
                json!({"issue": id, "head": head, "pr_state": state, "ci_green": true}),
            )
            .unwrap()
    }

    pub fn daemon_events(&self, kind: &str) -> Vec<Value> {
        self.f
            .d
            .rpc("agent_events", json!({"alias": "daemon", "tail": true}))
            .unwrap()["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e["kind"] == kind)
            .collect()
    }

    /// Tracker commits in which a merge marked `id` done (not the
    /// operator's own `issue set`).
    pub fn done_commits(&self, id: &str) -> Vec<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.f.pm_dir)
            .args(["log", "--format=%B%x00"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|c| {
                c.trim_start()
                    .starts_with(&format!("{id}: set status=done — "))
            })
            .map(str::to_string)
            .collect()
    }
}

/// A one-ticket plan for w1 — the second plan of the wake test.
pub const DIGEST_PLAN: &str = "---\ntitle: Digest\ngoal: Users get a weekly digest\n---\n\n\
## Digest\nsize: S\nagent: w1\n\n### Acceptance\n- [ ] a digest goes out weekly\n";

/// Two independent tickets and one waiting on the first, for the
/// merge-end wakes.
pub const WAKE_PLAN: &str = "---\ntitle: Export\ngoal: Users export CSV\n---\n\n\
## Schema\nsize: S\nagent: w1\n\nThe export table.\n\n### Acceptance\n- [ ] migration adds exports\n\n\
## Button\nsize: S\nagent: w1\n\n### Acceptance\n- [ ] a user can press export\n\n\
## Report\nsize: S\nagent: w1\ndepends_on: 1\n\n### Acceptance\n- [ ] exports are counted\n";

// ---- CAD-324: continuity packs ----

/// A plan with one ticket for `lead` and one for `other`.
pub const CAD324_PLAN: &str = "---\ntitle: CSV export\ngoal: Users export their data\n---\n\n\
## Build the exporter\nsize: M\nagent: lead\ndepends_on: 2\n\n### Acceptance\n- [ ] a CSV downloads\n\n\
## Write the export docs\nsize: S\nagent: other\n\n### Acceptance\n- [ ] docs name the columns\n";

// ---- CAD-378: code-area owners and advisory path leases ----

/// The `areas:` fixture: `caller` (src/peer.rs + src/daemon/) owned by
/// the PM `pm-own`, at most one open PR.
pub const CAD378_AREAS: &str =
    "---\nproject: demo\nareas:\n  caller:\n    paths: [src/peer.rs, src/daemon/]\n    \
     owner: pm-own\n    max_open_prs: 1\n---\n# Demo\n";

impl PlanFixture {
    /// `issue new` + `issue set paths=` + `issue start --by <pm>`; the
    /// start's JSON. The lane is then bound the way a real dispatch
    /// binds it — a record in the daemon's `dispatches.json` naming
    /// the pm, worktree and branch at send time (`dispatch_record`
    /// writes exactly this, pm from the caller's connection).
    pub fn cad378_lane(&self, title: &str, id: &str, paths: &str, by: &str) -> Value {
        let (ok, out) = self.cli(&["issue", "new", title, "--project", "demo"]);
        assert!(ok, "{out}");
        let (ok, out) = self.cli(&["issue", "set", id, &format!("paths={paths}")]);
        assert!(ok, "{out}");
        let (ok, out) = self.cli(&["issue", "start", id, "--by", by]);
        assert!(ok, "start never refuses on a lease: {out}");
        cadence_agent::issue::areas::record_dispatch(
            &self.d.state,
            id,
            json!({
                "pm": by,
                "worktree": out["worktree"],
                "branch": out["branch"],
                "message": "fixture-kickoff",
            }),
            false,
        )
        .unwrap();
        out
    }

    /// Commit `file` in a lane's worktree (fixture identity).
    pub fn cad378_commit(&self, worktree: &str, file: &str) {
        let wt = Path::new(worktree);
        std::fs::create_dir_all(wt.join(file).parent().unwrap()).unwrap();
        std::fs::write(wt.join(file), "change\n").unwrap();
        for args in [
            vec!["add", "-A"],
            vec![
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "lane change",
            ],
        ] {
            let o = std::process::Command::new("git")
                .arg("-C")
                .arg(wt)
                .args(&args)
                .output()
                .unwrap();
            assert!(o.status.success(), "git {args:?}: {o:?}");
        }
    }

    /// The overview's `area_ack` rows (a merged row counts by its causes).
    pub fn cad378_ack_rows(&self) -> Vec<Value> {
        let (ok, view) = self.cli(&["overview", "--json"]);
        assert!(ok, "{view}");
        view["needs_me"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| {
                r["kind"] == "area_ack"
                    || r["causes"]
                        .as_array()
                        .is_some_and(|cs| cs.iter().any(|c| c["cause"] == "area_ack"))
            })
            .cloned()
            .collect()
    }
}
