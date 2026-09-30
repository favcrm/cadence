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
use std::process::{Command, Output, Stdio};
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

    fn cmd(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
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
        cmd
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.cmd(args, env).output().unwrap()
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

// ---------- tailnet opt-in (`CADENCE_SANDBOX_ALLOW_GLOBAL`) ----------

const SANDBOX_TS_DNS: &str = "node.tail1234.ts.net";

/// A fake `tailscale` first on PATH (same shape as the one in
/// tests/board_tailnet.rs): `status` answers `status.json`,
/// `serve status` reports `serve.map` (`<dns>:<port>\t<target>`) in the
/// real `Web` shape, `serve --bg` appends to it. Every argv lands in
/// `calls.log`, so a refusal can prove the tailnet was never asked.
fn fake_tailscale(parent: &Path) -> PathBuf {
    let d = parent.join("tsbin");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("dns"), SANDBOX_TS_DNS).unwrap();
    std::fs::write(d.join("calls.log"), "").unwrap();
    std::fs::write(
        d.join("status.json"),
        format!(
            r#"{{"BackendState":"Running","Self":{{"DNSName":"{SANDBOX_TS_DNS}."}},"CertDomains":["{SANDBOX_TS_DNS}"]}}"#
        ),
    )
    .unwrap();
    let script = r#"#!/usr/bin/env bash
d="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
echo "$*" >> "$d/calls.log"
case "${1:-}" in
status)
  cat "$d/status.json"
  ;;
serve)
  case "${2:-}" in
  status)
    printf '{"Web":{'
    first=1
    if [ -f "$d/serve.map" ]; then
      while IFS=$'\t' read -r key target; do
        [ -n "$key" ] || continue
        [ "$first" -eq 0 ] && printf ','
        first=0
        printf '"%s":{"Handlers":{"/":{"Proxy":"%s"}}}' "$key" "$target"
      done < "$d/serve.map"
    fi
    printf '}}'
    ;;
  --bg)
    port="${3#--https=}"
    printf '%s:%s\t%s\n' "$(cat "$d/dns")" "$port" "$4" >> "$d/serve.map"
    ;;
  --https=*)
    if [ "${3:-}" = "off" ]; then
      key="$(cat "$d/dns"):${2#--https=}"
      grep -v "^$key" "$d/serve.map" > "$d/.sm" 2>/dev/null || true
      mv "$d/.sm" "$d/serve.map"
    fi
    ;;
  esac
  ;;
esac
exit 0
"#;
    let path = d.join("tailscale");
    std::fs::write(&path, script).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    d
}

/// Env pairs for one sandboxed `cadence` run with the fake `tailscale`
/// first on PATH; `allow` is the opt-in value (None = unset).
fn ts_sandbox_env<'a>(path: &'a str, allow: Option<&'a str>) -> Vec<(&'a str, &'a str)> {
    let mut env = vec![("CADENCE_PROFILE", "sandbox:x"), ("PATH", path)];
    if let Some(v) = allow {
        env.push(("CADENCE_SANDBOX_ALLOW_GLOBAL", v));
    }
    env
}

fn ts_calls(fake: &Path) -> String {
    std::fs::read_to_string(fake.join("calls.log")).unwrap()
}

/// Without the opt-in every route onto the tailnet refuses before
/// `tailscale` is ever invoked — the two operator verbs AND a persisted
/// tailscale block in `ui.json` (which the detached `ui run` would
/// otherwise serve silently). Forged opt-in values — anything but
/// exactly `1` — refuse the same way.
#[test]
fn sandbox_tailnet_writes_refused_without_the_opt_in() {
    let host = Host::new();
    let fake = fake_tailscale(host.tmp.path());
    let path = format!(
        "{}:{}",
        fake.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let state = host.tmp.path().join("state");
    let state_arg = state.to_str().unwrap();

    for args in [
        vec!["--state-dir", state_arg, "ui", "tailscale", "start"],
        vec!["--state-dir", state_arg, "ui", "start", "--tailscale"],
    ] {
        let out = host.run(&args, &ts_sandbox_env(&path, None));
        refused(&out, "refused under CADENCE_PROFILE=sandbox:x");
        assert!(text(&out).contains("opt-in"), "{}", text(&out));
    }
    assert_eq!(ts_calls(&fake), "", "tailscale must never run");

    // A hand-written ui.json with a tailscale block: `ui start` must
    // refuse rather than silently publishing the sandbox board.
    let port = free_port();
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(
        state.join("ui.json"),
        json!({
            "port": port,
            "tailscale": {
                "dns_name": SANDBOX_TS_DNS,
                "https_port": 9450,
                "target": format!("http://127.0.0.1:{port}"),
            }
        })
        .to_string(),
    )
    .unwrap();
    let out = host.run(
        &["--state-dir", state_arg, "ui", "start"],
        &ts_sandbox_env(&path, None),
    );
    refused(&out, "refused under CADENCE_PROFILE=sandbox:x");
    assert!(
        text(&out).contains("persisted tailscale sharing"),
        "{}",
        text(&out)
    );
    assert_eq!(ts_calls(&fake), "", "tailscale must never run");
    assert!(!state.join("ui.pid").exists(), "no board started");

    // The detached board process itself resolves the same persisted
    // options: a bare `ui run` refuses rather than serve shared.
    let mut child = host
        .cmd(
            &["--state-dir", state_arg, "ui", "run"],
            &ts_sandbox_env(&path, None),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            panic!("ui run served instead of refusing the persisted tailscale block");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    refused(&out, "persisted tailscale sharing");
    assert_eq!(ts_calls(&fake), "", "tailscale must never run");

    // `ui tailscale stop` and `ui stop --tailscale-off` on the
    // hand-written block refuse outright before anything is touched —
    // a denied stop must not orphan the live mapping's record.
    std::fs::write(
        fake.join("serve.map"),
        format!("{SANDBOX_TS_DNS}:9450\thttp://127.0.0.1:{port}\n"),
    )
    .unwrap();
    for args in [
        vec!["--state-dir", state_arg, "ui", "tailscale", "stop"],
        vec!["--state-dir", state_arg, "ui", "stop", "--tailscale-off"],
    ] {
        let out = host.run(&args, &ts_sandbox_env(&path, None));
        refused(&out, "refused under CADENCE_PROFILE=sandbox:x");
    }
    assert_eq!(
        std::fs::read_to_string(fake.join("serve.map")).unwrap(),
        format!("{SANDBOX_TS_DNS}:9450\thttp://127.0.0.1:{port}\n"),
        "the live mapping is untouched"
    );
    let opts: Value =
        serde_json::from_str(&std::fs::read_to_string(state.join("ui.json")).unwrap()).unwrap();
    assert_eq!(
        opts["tailscale"]["https_port"], 9450,
        "the mapping's record survives the refused stop"
    );
    assert_eq!(ts_calls(&fake), "", "tailscale must never run");

    // Forged opt-in values: only exactly "1" opens the gate.
    for forged in ["true", "yes", " 1", ""] {
        let out = host.run(
            &["--state-dir", state_arg, "ui", "tailscale", "start"],
            &ts_sandbox_env(&path, Some(forged)),
        );
        refused(&out, "refused under CADENCE_PROFILE=sandbox:x");
    }
    assert_eq!(ts_calls(&fake), "", "tailscale must never run");
}

/// With `CADENCE_SANDBOX_ALLOW_GLOBAL=1` a sandbox may publish: an
/// identical mapping is reused without `serve --bg`, and a port that
/// already targets production's board is still a hard refusal with the
/// mapping untouched — the opt-in never lets cadence overwrite another
/// serve mapping.
#[test]
fn sandbox_tailnet_opt_in_reuses_and_refuses_a_foreign_mapping() {
    let host = Host::new();
    let fake = fake_tailscale(host.tmp.path());
    let path = format!(
        "{}:{}",
        fake.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let state = host.tmp.path().join("state");
    let state_arg = state.to_str().unwrap();
    let pm = host.tmp.path().join("pm");
    let mut env = ts_sandbox_env(&path, Some("1"));
    env.push(("CADENCE_PM_DIR", pm.to_str().unwrap()));

    let port = free_port();
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("ui.json"), json!({"port": port}).to_string()).unwrap();
    // The serve config already maps :9450 to this board — reused.
    std::fs::write(
        fake.join("serve.map"),
        format!("{SANDBOX_TS_DNS}:9450\thttp://127.0.0.1:{port}\n"),
    )
    .unwrap();
    let out = host.run(&["issue", "init"], &env);
    assert!(out.status.success(), "{}", text(&out));

    let out = host.run(
        &["--state-dir", state_arg, "ui", "tailscale", "start"],
        &env,
    );
    assert!(
        out.status.success(),
        "opt-in start must proceed: {}",
        text(&out)
    );
    let out: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out["state"], "sharing", "{out}");
    assert_eq!(out["mapping_created"], false, "{out}");
    assert!(
        !ts_calls(&fake).contains("serve --bg"),
        "identical mapping reused, no serve --bg: {}",
        ts_calls(&fake)
    );
    // Under the opt-in, `ui tailscale stop` removes the recorded
    // mapping and drops the block.
    let out = host.run(&["--state-dir", state_arg, "ui", "tailscale", "stop"], &env);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        !std::fs::read_to_string(fake.join("serve.map"))
            .unwrap()
            .contains(":9450"),
        "recorded mapping removed"
    );
    assert!(
        ts_calls(&fake).contains("serve --https=9450 off"),
        "{}",
        ts_calls(&fake)
    );
    let opts: Value =
        serde_json::from_str(&std::fs::read_to_string(state.join("ui.json")).unwrap()).unwrap();
    assert_eq!(opts["tailscale"], Value::Null);

    // Stop the board this start spawned.
    let stop = host.run(&["--state-dir", state_arg, "ui", "stop"], &env);
    assert!(stop.status.success(), "{}", text(&stop));

    // A serve port that already targets production's board (:3010)
    // refuses under the opt-in too — never overwritten.
    let state2 = host.tmp.path().join("state2");
    let state2_arg = state2.to_str().unwrap();
    std::fs::create_dir_all(&state2).unwrap();
    std::fs::write(
        fake.join("serve.map"),
        format!("{SANDBOX_TS_DNS}:9450\thttp://127.0.0.1:3010\n"),
    )
    .unwrap();
    std::fs::write(state2.join("ui.json"), json!({"port": port}).to_string()).unwrap();
    let out = host.run(
        &["--state-dir", state2_arg, "ui", "tailscale", "start"],
        &env,
    );
    refused(&out, "already targets http://127.0.0.1:3010");
    assert_eq!(
        std::fs::read_to_string(fake.join("serve.map")).unwrap(),
        format!("{SANDBOX_TS_DNS}:9450\thttp://127.0.0.1:3010\n"),
        "the foreign mapping is untouched"
    );
    assert!(!state2.join("ui.pid").exists(), "no board started");
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

/// The opt-in is granted at `up`, recorded in the marker and re-exported
/// by `sandbox env` — an operator shell that evals the env gets exactly
/// what the sandbox was started with, and a later `up` without it
/// revokes it. A forged value at `up` records false.
#[test]
fn sandbox_env_exports_the_recorded_opt_in() {
    let mut host = Host::new();
    let v = host.up_free("ga", &[("CADENCE_SANDBOX_ALLOW_GLOBAL", "1")]);
    let state = PathBuf::from(v["state_dir"].as_str().unwrap());
    let root = host.base().join("ga");
    let marker: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".cadence-sandbox")).unwrap())
            .unwrap();
    assert_eq!(marker["allow_global"], true, "{marker}");
    let env_file = std::fs::read_to_string(root.join("sandbox.env")).unwrap();
    assert!(
        env_file.contains("export CADENCE_SANDBOX_ALLOW_GLOBAL=1"),
        "{env_file}"
    );
    assert!(
        env_file.contains("unset CADENCE_ALIAS CADENCE_ROLLOUT_AS\n"),
        "granted: the unset line stays minimal: {env_file}"
    );
    let out = host.run(&["sandbox", "env", "ga"], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("export CADENCE_SANDBOX_ALLOW_GLOBAL=1"),
        "{}",
        text(&out)
    );

    // Granted and running: `up` without the variable is refused — the
    // live processes keep the grant they were started with; the marker
    // is unchanged and the board stays up.
    let out = host.run(&["sandbox", "up", "ga"], &[]);
    refused(&out, "is running with CADENCE_SANDBOX_ALLOW_GLOBAL granted");
    let marker: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".cadence-sandbox")).unwrap())
            .unwrap();
    assert_eq!(marker["allow_global"], true, "marker unchanged: {marker}");
    assert!(daemon_answers(&state), "board/daemon still up");

    // Down frees the grant change; `up` without the variable revokes it.
    let out = host.run(&["sandbox", "down", "ga"], &[]);
    assert!(out.status.success(), "{}", text(&out));
    host.up("ga", &[]);
    let marker: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".cadence-sandbox")).unwrap())
            .unwrap();
    assert_eq!(marker["allow_global"], false, "{marker}");
    let env_file = std::fs::read_to_string(root.join("sandbox.env")).unwrap();
    assert!(
        env_file.contains("unset CADENCE_ALIAS CADENCE_ROLLOUT_AS CADENCE_SANDBOX_ALLOW_GLOBAL\n"),
        "revoked: env actively clears the variable: {env_file}"
    );
    let out = host.run(&["sandbox", "env", "ga"], &[]);
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("export CADENCE_SANDBOX_ALLOW_GLOBAL"),
        "{}",
        text(&out)
    );
    // Shell proof: eval'ing the revoked env clears a leaked variable.
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "export CADENCE_SANDBOX_ALLOW_GLOBAL=1; eval \"$(cat {})\"; \
             echo \"${{CADENCE_SANDBOX_ALLOW_GLOBAL-unset}}\"",
            root.join("sandbox.env").display()
        ))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "unset");

    // Ungranted and running: `up` with the variable is refused the
    // same way; a forged value (" 1") records false, never exports.
    host.up_free("gb", &[]);
    let out = host.run(
        &["sandbox", "up", "gb"],
        &[("CADENCE_SANDBOX_ALLOW_GLOBAL", "1")],
    );
    refused(
        &out,
        "is running with CADENCE_SANDBOX_ALLOW_GLOBAL not granted",
    );
    let marker: Value = serde_json::from_str(
        &std::fs::read_to_string(host.base().join("gb").join(".cadence-sandbox")).unwrap(),
    )
    .unwrap();
    assert_eq!(marker["allow_global"], false, "{marker}");
    host.up_with("gb", &[], &[("CADENCE_SANDBOX_ALLOW_GLOBAL", " 1")]);
    let marker: Value = serde_json::from_str(
        &std::fs::read_to_string(host.base().join("gb").join(".cadence-sandbox")).unwrap(),
    )
    .unwrap();
    assert_eq!(marker["allow_global"], false, "{marker}");
    let env_file = std::fs::read_to_string(host.base().join("gb").join("sandbox.env")).unwrap();
    assert!(
        !env_file.contains("export CADENCE_SANDBOX_ALLOW_GLOBAL"),
        "{env_file}"
    );
}

/// `down` leaves the tailnet block in `ui.json`; an ungranted `up`
/// must refuse before the marker or the daemon move — not half-start
/// and fail at `ui start`.
#[test]
fn sandbox_up_refuses_revocation_while_a_share_is_persisted() {
    let mut host = Host::new();
    let v = host.up_free("sb", &[("CADENCE_SANDBOX_ALLOW_GLOBAL", "1")]);
    let state = PathBuf::from(v["state_dir"].as_str().unwrap());
    let root = host.base().join("sb");
    let out = host.run(&["sandbox", "down", "sb"], &[]);
    assert!(out.status.success(), "{}", text(&out));

    // The share a granted `ui tailscale start` would have persisted.
    let port = free_port();
    std::fs::write(
        state.join("ui.json"),
        json!({
            "port": port,
            "tailscale": {
                "dns_name": "sandbox.ts.net",
                "https_port": 9460,
                "target": format!("http://127.0.0.1:{port}"),
            }
        })
        .to_string(),
    )
    .unwrap();
    let out = host.run(&["sandbox", "up", "sb"], &[]);
    refused(&out, "still has a persisted tailnet share");
    let marker: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".cadence-sandbox")).unwrap())
            .unwrap();
    assert_eq!(marker["allow_global"], true, "marker untouched: {marker}");
    assert!(
        !state.join("ui.pid").exists() && !daemon_answers(&state),
        "nothing started"
    );

    // With the share stopped the same `up` revokes cleanly.
    std::fs::write(state.join("ui.json"), json!({ "port": port }).to_string()).unwrap();
    host.up_free("sb", &[]);
    let marker: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".cadence-sandbox")).unwrap())
            .unwrap();
    assert_eq!(marker["allow_global"], false, "{marker}");
}

/// Concurrent `up`s with opposite opt-ins must never leave live
/// processes whose grant disagrees with the marker.
#[test]
fn sandbox_up_serializes_a_grant_change() {
    let mut host = Host::new();
    host.started.push("sb".to_string());
    let a = host
        .cmd(
            &["sandbox", "up", "sb"],
            &[("CADENCE_SANDBOX_ALLOW_GLOBAL", "1")],
        )
        .spawn()
        .unwrap();
    let b = host.cmd(&["sandbox", "up", "sb"], &[]).spawn().unwrap();
    let oa = a.wait_with_output().unwrap();
    let ob = b.wait_with_output().unwrap();

    let granted_won = oa.status.success();
    let (won, lost) = if granted_won { (&oa, &ob) } else { (&ob, &oa) };
    assert!(won.status.success(), "winner failed: {}", text(won));
    refused(lost, "sandbox down");
    let root = host.base().join("sb");
    let marker: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".cadence-sandbox")).unwrap())
            .unwrap();
    assert_eq!(
        marker["allow_global"], granted_won,
        "marker follows the winner: {marker}"
    );

    // The live board's environment is exactly the recorded grant.
    let state = root.join("state");
    let pid: i32 = std::fs::read_to_string(state.join("ui.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap();
    let board_granted = environ
        .split(|b| *b == 0)
        .any(|kv| kv == b"CADENCE_SANDBOX_ALLOW_GLOBAL=1");
    assert_eq!(
        board_granted, granted_won,
        "live board env disagrees with the marker"
    );
}
