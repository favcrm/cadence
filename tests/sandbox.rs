//! CAD-310: `cadence sandbox` — isolation, production refusal and the
//! reset guard, driven through the real binary. Every test points HOME,
//! XDG_STATE_HOME and CADENCE_SANDBOX_ROOT at its own temp dirs and
//! clears the caller's cadence env, so nothing here can reach the
//! host's live daemon, tracker or board.

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
            .env("CADENCE_SANDBOX_ROOT", self.base());
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

fn http_status(port: u16, path: &str) -> Option<u16> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(s, "GET {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n").ok()?;
    let mut buf = String::new();
    s.read_to_string(&mut buf).ok()?;
    buf.split_whitespace().nth(1)?.parse().ok()
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
/// HOME/pm, a state dir symlinked onto production's, and a caller
/// shell exported at the sandbox's dirs all refuse before anything is
/// created or started.
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

    let port = free_port().to_string();
    let v = host.up("rst", &["--port", &port]);
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

/// The tailnet is host-wide: under a sandbox profile both ways onto it
/// refuse before tailscale is ever asked.
#[test]
fn tailscale_is_refused_under_a_sandbox_profile() {
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
    let v = host.up_with("cl", &[], &[("CADENCE_CLAUDE_COMMAND", &command)]);
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
}
