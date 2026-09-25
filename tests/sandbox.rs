//! CAD-310: `cadence sandbox` — isolation, production refusal and the
//! reset guard, driven through the real binary. Every test points HOME,
//! XDG_STATE_HOME and CADENCE_SANDBOX_ROOT at its own temp dirs and
//! clears the caller's cadence env, so nothing here can reach the
//! host's live daemon, tracker or board.

// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use cadence_agent::client;
use serde_json::{json, Value};
use tempfile::TempDir;

/// One isolated host: `home/`, `xdg/` (so production's default state
/// dir is `xdg/cadence`) and `sandboxes/` as the sandbox base. Drop
/// stops every sandbox a test brought up, pass or fail.
struct Host {
    tmp: TempDir,
    started: Vec<String>,
}

impl Host {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        for d in ["home", "xdg", "sandboxes"] {
            std::fs::create_dir_all(tmp.path().join(d)).unwrap();
        }
        Self {
            tmp,
            started: Vec::new(),
        }
    }
    fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }
    fn xdg(&self) -> PathBuf {
        self.tmp.path().join("xdg")
    }
    fn base(&self) -> PathBuf {
        self.tmp.path().join("sandboxes")
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
        cmd.args(args)
            .env("HOME", self.home())
            .env("XDG_STATE_HOME", self.xdg())
            .env("CADENCE_SANDBOX_ROOT", self.base())
            // `sandbox up`'s free pick shares 3110-3199 with the
            // `tests/setup.rs` flock leases — honour them or the pick
            // can steal a port a setup test just leased.
            .env(
                cadence_agent::sandbox::TEST_PORT_LOCK_DIR,
                "/tmp/cadence-test-ports",
            );
        for var in [
            "CADENCE_STATE_DIR",
            "CADENCE_PM_DIR",
            "CADENCE_PROFILE",
            "CADENCE_ALIAS",
            "CADENCE_ROLLOUT_AS",
            "CADENCE_SANDBOX_ALLOW_GLOBAL",
            "XDG_DATA_HOME",
        ] {
            cmd.env_remove(var);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    fn up(&mut self, name: &str, extra: &[&str]) -> Value {
        self.up_with(name, extra, &[])
    }

    /// `up` on an ephemeral port: only the isolation test takes a port
    /// from the shared 3110-3199 range, so parallel tests never race
    /// for one.
    fn up_free(&mut self, name: &str, env: &[(&str, &str)]) -> Value {
        let port = free_port().to_string();
        self.up_with(name, &["--port", &port], env)
    }

    /// `up` with extra environment for the sandbox's daemon and board.
    fn up_with(&mut self, name: &str, extra: &[&str], env: &[(&str, &str)]) -> Value {
        self.started.push(name.to_string());
        let mut args = vec!["sandbox", "up", name];
        args.extend_from_slice(extra);
        let out = self.run(&args, env);
        assert!(out.status.success(), "up {name}: {}", text(&out));
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        for name in std::mem::take(&mut self.started) {
            let _ = self.run(&["sandbox", "down", &name], &[]);
        }
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn refused(out: &Output, needle: &str) {
    assert!(!out.status.success(), "expected refusal: {}", text(out));
    assert!(
        text(out).contains(needle),
        "want {needle:?} in: {}",
        text(out)
    );
}

fn http_get(port: u16, path: &str) -> Option<(u16, String)> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(s, "GET {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n").ok()?;
    let mut buf = String::new();
    s.read_to_string(&mut buf).ok()?;
    let status = buf.split_whitespace().nth(1)?.parse().ok()?;
    let body = buf.split_once("\r\n\r\n").map(|(_, b)| b.to_string())?;
    Some((status, body))
}

fn http_status(port: u16, path: &str) -> Option<u16> {
    http_get(port, path).map(|(status, _)| status)
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn daemon_answers(state: &Path) -> bool {
    client::rpc_timeout(state, "health", json!({}), Duration::from_secs(5)).is_ok()
}

/// Keep the suite's fence on `port` until the returned `File` drops:
/// `up` holds the pick-to-bind lease only until its board binds, so
/// the port the sandbox goes on claiming is free and unfenced the
/// moment `down` kills the board — a parallel `test_port` can take it
/// before the probe or the next `up`. Holding the `flock` here keeps
/// it fenced across `down`, and recording `name`'s claim lets a later
/// `up` know the hold is for this sandbox.
fn fence_port(port: u16, name: &str) -> std::fs::File {
    use std::os::fd::AsRawFd;
    let mut lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(Path::new("/tmp/cadence-test-ports").join(format!("{port}.lock")))
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    // SAFETY: plain syscall on a descriptor this function owns.
    while unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "port {port} fence never freed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    lock.set_len(0).unwrap();
    lock.write_all(cadence_agent::sandbox::port_claim(name).as_bytes())
        .unwrap();
    lock
}

/// Poll for a file another process writes, bounded.
fn wait_file(path: &Path, secs: u64) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            return text;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A managed-claude stand-in: dumps every `CADENCE_*` variable it was
/// started with, then idles until its stdin closes.
const MOCK_CLAUDE_ENV_PY: &str = r#"
import os, sys
out = sys.argv[1]
with open(out + ".tmp", "w") as f:
    for k in sorted(os.environ):
        if k.startswith("CADENCE_"):
            f.write(k + "=" + os.environ[k] + "\n")
os.rename(out + ".tmp", out)
for _ in sys.stdin:
    pass
"#;

/// `up` builds a marked root with its own state dir, tracker and a
/// 3110+ port, starts a daemon and board under the sandbox profile —
/// the daemon never syncs the skill into HOME — and `down` stops both.
/// A second `up` reuses the root and its port.
#[test]
fn sandbox_up_isolates_state_tracker_and_port_then_down_stops_it() {
    let mut host = Host::new();
    let v = host.up("iso", &[]);
    let root = host.base().join("iso");
    let state = root.join("state");
    assert_eq!(v["root"], json!(root));
    assert_eq!(v["state_dir"], json!(state));
    assert_eq!(v["pm_dir"], json!(root.join("pm")));
    assert_eq!(v["profile"], "sandbox:iso");
    let port = v["port"].as_u64().unwrap() as u16;
    assert!((3110..=3199).contains(&port), "{v}");
    assert_eq!(v["url"], format!("http://127.0.0.1:{port}"));
    // The sandbox claims this port through `down` and the next `up`;
    // hold the suite's fence for it so a parallel test cannot take it
    // in the window where the board is down.
    let _fence = fence_port(port, "iso");

    let marker: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".cadence-sandbox")).unwrap())
            .unwrap();
    assert_eq!(marker["name"], "iso");
    assert_eq!(marker["profile"], "sandbox:iso");
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&state).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
    assert!(root.join("pm/pm.yaml").is_file(), "tracker initialised");
    let env_file = std::fs::read_to_string(root.join("sandbox.env")).unwrap();
    assert!(
        env_file.contains("export CADENCE_PROFILE='sandbox:iso'"),
        "{env_file}"
    );
    assert!(env_file.contains(&format!("export CADENCE_STATE_DIR='{}'", state.display())));

    // The detached daemon inherited the profile; the board answers.
    let health = client::rpc(&state, "health", json!({})).unwrap();
    assert_eq!(health["sandbox"], "iso", "{health}");
    assert_eq!(http_status(port, "/api/health"), Some(200));
    // Nothing global: no skill in HOME, no production state dir.
    assert!(
        !host.home().join(".agents").exists(),
        "skill sync must skip"
    );
    let log = std::fs::read_to_string(state.join("daemon.log")).unwrap();
    assert!(log.contains("skill: skipped (sandbox profile)"), "{log}");
    assert!(
        !host.xdg().join("cadence").exists(),
        "production state untouched"
    );
    assert!(
        !host.home().join("pm").exists(),
        "production tracker untouched"
    );

    let env = host.run(&["sandbox", "env", "iso"], &[]);
    assert!(env.status.success(), "{}", text(&env));
    assert_eq!(String::from_utf8_lossy(&env.stdout), env_file);
    let ls = host.run(&["sandbox", "ls"], &[]);
    let ls: Value = serde_json::from_slice(&ls.stdout).unwrap();
    assert_eq!(ls["sandboxes"][0]["name"], "iso", "{ls}");
    assert_eq!(ls["sandboxes"][0]["daemon"], "running", "{ls}");

    let down = host.run(&["sandbox", "down", "iso"], &[]);
    assert!(down.status.success(), "{}", text(&down));
    let down: Value = serde_json::from_slice(&down.stdout).unwrap();
    assert_eq!(down["daemon"], "stopped", "{down}");
    assert_eq!(down["ui"], "stopped", "{down}");
    assert!(!daemon_answers(&state));
    assert_eq!(http_status(port, "/api/health"), None);

    // Idempotent: the same root comes back on the same port.
    let again = host.up("iso", &[]);
    assert_eq!(again["port"], json!(port), "{again}");
    let marker2: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".cadence-sandbox")).unwrap())
            .unwrap();
    assert_eq!(marker2["created_at"], marker["created_at"]);
    assert!(daemon_answers(&state));
}

/// Production is off limits: port 3010, a root whose tracker is
/// HOME/pm, a state dir symlinked onto production's, a `..` base, a
/// root at production's HOME-default state dir, an unreadable root and
/// a caller shell exported at the sandbox's dirs all refuse before
/// anything is created or started.
#[test]
fn sandbox_up_refuses_production_dirs_and_port_3010() {
    let host = Host::new();

    let out = host.run(&["sandbox", "up", "p", "--port", "3010"], &[]);
    refused(&out, "3010");
    assert!(!host.base().join("p").exists());

    let out = host.run(&["sandbox", "up", "Bad/Name"], &[]);
    refused(&out, "must match");

    // Root == HOME, so the sandbox tracker would be HOME/pm.
    let tmp = host.tmp.path().to_str().unwrap();
    let out = host.run(&["sandbox", "up", "home"], &[("CADENCE_SANDBOX_ROOT", tmp)]);
    refused(&out, "overlaps the production tracker");
    assert!(!host.home().join("pm").exists());

    // A state dir that resolves onto production's.
    let prod = host.xdg().join("cadence");
    std::fs::create_dir_all(&prod).unwrap();
    std::fs::create_dir_all(host.base().join("sym")).unwrap();
    std::os::unix::fs::symlink(&prod, host.base().join("sym/state")).unwrap();
    let out = host.run(&["sandbox", "up", "sym"], &[]);
    refused(&out, "overlaps the production state dir");
    assert!(
        !prod.join("cadence.sock").exists(),
        "no daemon on production"
    );
    assert_eq!(std::fs::read_dir(&prod).unwrap().count(), 0);

    // A socket path past the Unix limit would only fail in the
    // detached daemon — refused up front, nothing created.
    let deep = host.tmp.path().join("d".repeat(120));
    let out = host.run(
        &["sandbox", "up", "deep"],
        &[("CADENCE_SANDBOX_ROOT", deep.to_str().unwrap())],
    );
    refused(&out, "Unix socket limit");
    assert!(!deep.exists());

    // A `..` in the base would land the sandbox inside production's
    // state dir: refused before anything is created.
    let dotdot = format!("{}/missing/../xdg", host.tmp.path().display());
    let out = host.run(
        &["sandbox", "up", "cadence"],
        &[("CADENCE_SANDBOX_ROOT", &dotdot)],
    );
    refused(&out, "no `.` or `..`");
    assert!(!host.tmp.path().join("missing").exists());
    assert_eq!(std::fs::read_dir(&prod).unwrap().count(), 0);

    // Production's HOME-default state dir counts even with
    // XDG_STATE_HOME set.
    let home_state = host.home().join(".local/state");
    let out = host.run(
        &["sandbox", "up", "cadence"],
        &[("CADENCE_SANDBOX_ROOT", home_state.to_str().unwrap())],
    );
    refused(&out, "overlaps the production state dir");
    assert!(!home_state.join("cadence").exists());

    // A root that cannot be read is refused, not treated as empty.
    std::fs::write(host.base().join("afile"), "x").unwrap();
    refused(&host.run(&["sandbox", "up", "afile"], &[]), "cannot read");

    // A shell exported at a live cadence that is this sandbox's dir.
    let exported = host.base().join("exp/state");
    let out = host.run(
        &["sandbox", "up", "exp"],
        &[("CADENCE_STATE_DIR", exported.to_str().unwrap())],
    );
    refused(&out, "exported CADENCE_STATE_DIR");
    assert!(!host.base().join("exp").exists());
}

/// `reset` deletes only a direct child of the base holding its own
/// marker — a stopped-first real sandbox goes, an unmarked dir, a
/// foreign marker and a symlinked root all stay.
#[test]
fn sandbox_reset_requires_the_marker() {
    let mut host = Host::new();

    let plain = host.base().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("keep"), "x").unwrap();
    refused(&host.run(&["sandbox", "reset", "plain"], &[]), "no sandbox");
    // `up` will not adopt it either.
    refused(
        &host.run(&["sandbox", "up", "plain"], &[]),
        "holds no sandbox marker",
    );
    assert!(plain.join("keep").is_file());

    let foreign = host.base().join("foreign");
    std::fs::create_dir_all(&foreign).unwrap();
    std::fs::write(foreign.join(".cadence-sandbox"), r#"{"name":"other"}"#).unwrap();
    refused(
        &host.run(&["sandbox", "reset", "foreign"], &[]),
        "belongs to sandbox",
    );
    assert!(foreign.is_dir());

    let outside = host.tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join(".cadence-sandbox"), r#"{"name":"lnk"}"#).unwrap();
    std::os::unix::fs::symlink(&outside, host.base().join("lnk")).unwrap();
    refused(
        &host.run(&["sandbox", "reset", "lnk"], &[]),
        "outside the sandbox base",
    );
    assert!(outside.join(".cadence-sandbox").is_file());

    let v = host.up_free("rst", &[]);
    let state = PathBuf::from(v["state_dir"].as_str().unwrap());
    assert!(daemon_answers(&state));
    let out = host.run(&["sandbox", "reset", "rst"], &[]);
    assert!(out.status.success(), "{}", text(&out));
    let r: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(r["state"], "removed", "{r}");
    assert_eq!(r["daemon"], "stopped", "{r}");
    assert!(!host.base().join("rst").exists());
    assert!(!daemon_answers(&state));
    assert!(plain.is_dir() && foreign.is_dir());
}

/// The tailnet and port 3010 are production's: under a sandbox profile
/// both ways onto the tailnet refuse before tailscale is ever asked,
/// and a board with no port of its own does not default to 3010.
#[test]
fn tailscale_and_port_3010_are_refused_under_a_sandbox_profile() {
    let host = Host::new();
    let state = host.tmp.path().join("state");
    let state_arg = state.to_str().unwrap();
    for args in [
        vec!["--state-dir", state_arg, "ui", "tailscale", "start"],
        vec!["--state-dir", state_arg, "ui", "start", "--tailscale"],
    ] {
        let out = host.run(&args, &[("CADENCE_PROFILE", "sandbox:x")]);
        refused(&out, "refused under CADENCE_PROFILE=sandbox:x");
        assert!(text(&out).contains("tailscale"), "{}", text(&out));
    }
    for args in [
        vec!["--state-dir", state_arg, "ui", "start"],
        vec!["--state-dir", state_arg, "ui", "start", "--port", "3010"],
    ] {
        let out = host.run(&args, &[("CADENCE_PROFILE", "sandbox:x")]);
        refused(&out, "port 3010 is the production board");
    }
    assert!(!state.join("ui.pid").exists(), "no board started");
}

/// A claude worker in a sandbox keeps the sandbox's tracker and
/// profile — without them its `cadence issue …` reaches production's
/// tracker, ungated. The mock records the env the daemon hands it;
/// the test override itself is still scrubbed.
#[test]
fn a_sandbox_claude_worker_keeps_the_sandbox_tracker_and_profile() {
    let mut host = Host::new();
    let dump = host.tmp.path().join("claude.env");
    let script = host.tmp.path().join("claude.py");
    std::fs::write(&script, MOCK_CLAUDE_ENV_PY).unwrap();
    let command = format!("python3 {} {}", script.display(), dump.display());
    let v = host.up_free("cl", &[("CADENCE_CLAUDE_COMMAND", &command)]);
    let state = PathBuf::from(v["state_dir"].as_str().unwrap());
    let pm = PathBuf::from(v["pm_dir"].as_str().unwrap());
    client::rpc(
        &state,
        "agent_register",
        json!({"alias": "w1", "provider": "claude", "endpoint_kind": "managed",
               "cwd": host.tmp.path().to_str().unwrap()}),
    )
    .unwrap();
    let env = wait_file(&dump, 20);
    assert!(
        env.contains(&format!("CADENCE_PM_DIR={}\n", pm.display())),
        "{env}"
    );
    assert!(env.contains("CADENCE_PROFILE=sandbox:cl\n"), "{env}");
    assert!(
        env.contains(&format!("CADENCE_STATE_DIR={}\n", state.display())),
        "{env}"
    );
    assert!(env.contains("CADENCE_ALIAS=w1\n"), "{env}");
    assert!(
        !env.contains("CADENCE_CLAUDE_COMMAND="),
        "override leaked: {env}"
    );

    // `down` stops the live worker before the daemon, not just the daemon.
    let down = host.run(&["sandbox", "down", "cl"], &[]);
    assert!(down.status.success(), "{}", text(&down));
    let down: Value = serde_json::from_slice(&down.stdout).unwrap();
    assert_eq!(down["agents_stopped"], 1, "{down}");
    assert_eq!(down["daemon"], "stopped", "{down}");
}

/// CAD-384 round 1 (I2): `sandbox down` run by an agent of PRODUCTION
/// — its env carries a `CADENCE_ALIAS` no sandbox agent has, so the
/// sandbox daemon sees a caller with no identity and no operator proof —
/// still stops the sandbox's agents and daemon: in a sandbox, a caller
/// tied to none of its agents may. A caller carrying a SANDBOX agent's
/// alias (a detached child of that agent) is refused both.
#[test]
fn sandbox_down_from_a_production_agent_stops_the_sandbox_agents() {
    let mut host = Host::new();
    let dump = host.tmp.path().join("claude.env");
    let script = host.tmp.path().join("claude.py");
    std::fs::write(&script, MOCK_CLAUDE_ENV_PY).unwrap();
    let command = format!("python3 {} {}", script.display(), dump.display());
    let v = host.up_free("pa", &[("CADENCE_CLAUDE_COMMAND", &command)]);
    let state = PathBuf::from(v["state_dir"].as_str().unwrap());
    client::rpc(
        &state,
        "agent_register",
        json!({"alias": "w1", "provider": "claude", "endpoint_kind": "managed",
               "cwd": host.tmp.path().to_str().unwrap()}),
    )
    .unwrap();
    wait_file(&dump, 20);

    // Tied to the sandbox's own agent `w1`: refused, the agent keeps
    // running and the daemon stays up.
    let tied = host.run(&["sandbox", "down", "pa"], &[("CADENCE_ALIAS", "w1")]);
    assert!(!tied.status.success(), "{}", text(&tied));
    assert!(text(&tied).contains("caller rule"), "{}", text(&tied));
    assert!(daemon_answers(&state));
    let show = client::rpc(&state, "agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(show["agent"]["enabled"], true, "{show}");

    // A production agent's pane (an alias the sandbox never registered).
    let down = host.run(&["sandbox", "down", "pa"], &[("CADENCE_ALIAS", "prod-pm")]);
    assert!(down.status.success(), "{}", text(&down));
    let down: Value = serde_json::from_slice(&down.stdout).unwrap();
    assert_eq!(down["agents_stopped"], 1, "{down}");
    assert_eq!(down["daemon"], "stopped", "{down}");
}

/// The state dir decides, not the caller's env: a sandbox restarted
/// from a bare shell with only `--state-dir` still skips the skill
/// sync, reports its profile, serves its own tracker and refuses the
/// tailnet.
#[test]
fn a_bare_restart_of_a_sandbox_state_dir_stays_gated() {
    let mut host = Host::new();
    let v = host.up_free("bare", &[]);
    let state = PathBuf::from(v["state_dir"].as_str().unwrap());
    let pm = PathBuf::from(v["pm_dir"].as_str().unwrap());
    let port = v["port"].as_u64().unwrap() as u16;
    let down = host.run(&["sandbox", "down", "bare"], &[]);
    assert!(down.status.success(), "{}", text(&down));

    let st = state.to_str().unwrap();
    let start = host.run(&["--state-dir", st, "daemon", "start"], &[]);
    assert!(start.status.success(), "{}", text(&start));
    let health = client::rpc(&state, "health", json!({})).unwrap();
    assert_eq!(health["sandbox"], "bare", "{health}");
    assert!(
        !host.home().join(".agents").exists(),
        "skill sync must skip"
    );
    let log = std::fs::read_to_string(state.join("daemon.log")).unwrap();
    assert_eq!(
        log.matches("skill: skipped (sandbox profile)").count(),
        2,
        "{log}"
    );

    let ui = host.run(&["--state-dir", st, "ui", "start"], &[]);
    assert!(ui.status.success(), "{}", text(&ui));
    let (code, body) = http_get(port, "/api/health").unwrap();
    assert_eq!(code, 200, "{body}");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["pm_dir"], json!(pm), "{body}");
    assert!(
        !host.home().join("pm").exists(),
        "production tracker untouched"
    );

    refused(
        &host.run(&["--state-dir", st, "ui", "tailscale", "start"], &[]),
        "refused under CADENCE_PROFILE=sandbox:bare",
    );
}

/// A rebuilt binary brings a sandbox back up: its own state dir takes
/// no part in a rollout, so the build gate does not ask for a lease
/// the sandbox's children could never name.
#[test]
fn a_sandbox_comes_back_up_after_a_rebuild() {
    let mut host = Host::new();
    let v = host.up_free("rb", &[]);
    let state = PathBuf::from(v["state_dir"].as_str().unwrap());
    let down = host.run(&["sandbox", "down", "rb"], &[]);
    assert!(down.status.success(), "{}", text(&down));
    // What a rebuild looks like to the gate: a different recorded build.
    let conn = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    let rows = conn
        .execute(
            "UPDATE daemon_build SET commit_sha='deadbeefdead' WHERE id=1",
            [],
        )
        .unwrap();
    assert_eq!(rows, 1);
    drop(conn);
    let again = host.up_free("rb", &[]);
    assert_eq!(again["daemon"], "started", "{again}");
    let health = client::rpc(&state, "health", json!({})).unwrap();
    assert_eq!(health["sandbox"], "rb", "{health}");
}

/// A fake `tmux -L <socket> …`: sessions per socket are lines in
/// `<dir>/<socket>`; every call is logged to `<dir>/calls`.
const FAKE_TMUX_SH: &str = r#"#!/bin/sh
dir="__DIR__"
sock="$2"
shift 2
echo "$sock $*" >> "$dir/calls"
case "$1" in
  list-sessions) [ -f "$dir/$sock" ] && cat "$dir/$sock" || exit 1 ;;
  kill-server) rm -f "$dir/$sock" ;;
esac
"#;

/// A daemon stop keeps pty panes for a hot restart; `down` and
/// `reset` must not leave them running on the sandbox's tmux server —
/// `reset` would delete the state dir under them.
#[test]
fn sandbox_down_and_reset_kill_the_panes_the_daemon_left() {
    let mut host = Host::new();
    let fake = host.tmp.path().join("faketmux");
    std::fs::create_dir_all(&fake).unwrap();
    let script = fake.join("tmux");
    std::fs::write(
        &script,
        FAKE_TMUX_SH.replace("__DIR__", fake.to_str().unwrap()),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let tmux = [("CADENCE_TMUX_COMMAND", script.to_str().unwrap())];
    let v = host.up_free("pn", &tmux);
    let state = PathBuf::from(v["state_dir"].as_str().unwrap());
    let socket = cadence_agent::adapter::pty::tmux_socket(&state);
    // Two panes the daemon kept alive on its private server.
    std::fs::write(fake.join(&socket), "w1\nw2\n").unwrap();
    let down = host.run(&["sandbox", "down", "pn"], &tmux);
    assert!(down.status.success(), "{}", text(&down));
    let down: Value = serde_json::from_slice(&down.stdout).unwrap();
    assert_eq!(down["panes_killed"], 2, "{down}");
    assert!(!fake.join(&socket).exists(), "server still up");
    let calls = std::fs::read_to_string(fake.join("calls")).unwrap();
    assert!(calls.contains(&format!("{socket} kill-server")), "{calls}");

    // `reset` does the same before it deletes the root.
    host.up_free("pn", &tmux);
    std::fs::write(fake.join(&socket), "w3\n").unwrap();
    let reset = host.run(&["sandbox", "reset", "pn"], &tmux);
    assert!(reset.status.success(), "{}", text(&reset));
    let reset: Value = serde_json::from_slice(&reset.stdout).unwrap();
    assert_eq!(reset["panes_killed"], 1, "{reset}");
    assert_eq!(reset["state"], "removed", "{reset}");
    assert!(!fake.join(&socket).exists());
}

/// The marker is a hand-writable file: `<x>/state` symlinked onto
/// production's state dir beside a forged `<x>/.cadence-sandbox` must
/// not run production's database as a lease-exempt sandbox.
#[test]
fn a_forged_marker_beside_a_symlink_onto_production_is_refused() {
    let host = Host::new();
    let prod = host.xdg().join("cadence");
    std::fs::create_dir_all(&prod).unwrap();
    let x = host.tmp.path().join("x");
    std::fs::create_dir_all(&x).unwrap();
    std::os::unix::fs::symlink(&prod, x.join("state")).unwrap();
    std::fs::write(x.join(".cadence-sandbox"), r#"{"name":"x"}"#).unwrap();
    let st = x.join("state");
    let out = host.run(
        &["--state-dir", st.to_str().unwrap(), "daemon", "start"],
        &[],
    );
    refused(&out, "refusing to run it ungated");
    assert_eq!(
        std::fs::read_dir(&prod).unwrap().count(),
        0,
        "production touched"
    );
}
