//! CAD-312: `cadence setup` — the idempotent first run, driven through
//! the real binary. Each test owns a short root under `/tmp` (the
//! socket path stays under 107 bytes) holding HOME, every XDG dir and
//! TMPDIR; PATH is a dir of fake provider CLIs plus `/usr/bin:/bin`, and
//! the caller's cadence env is cleared — nothing here reaches the host's
//! live daemon, tracker, board or provider sign-ins. Board ports are
//! taken from 3110-3199, never 3010.

// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;
use tempfile::TempDir;

/// Written into a fake credentials file and printed by a fake status
/// command — it must never reach setup's output.
const SECRET: &str = "sk-SETUP-SENTINEL-3f9a";

struct Host {
    tmp: TempDir,
}

impl Host {
    fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("c312")
            .tempdir_in("/tmp")
            .unwrap();
        for d in ["home", "state", "config", "data", "cache", "tmp", "bin"] {
            std::fs::create_dir_all(tmp.path().join(d)).unwrap();
        }
        let host = Self { tmp };
        // claude: signed in by its status exit code, which prints an
        // account line setup must not echo.
        host.fake(
            "claude",
            &format!(
                "case \"$1 $2\" in\n\
                 '--version ') echo '9.9.9 (Claude Code)' ;;\n\
                 'auth status') echo 'logged in as {SECRET}'; exit 0 ;;\n\
                 *) exit 2 ;;\nesac"
            ),
        );
        // codex: installed, signed out.
        host.fake(
            "codex",
            "case \"$1 $2\" in\n\
             '--version ') echo 'codex-cli 0.0.1' ;;\n\
             'login status') echo 'Not logged in'; exit 1 ;;\n\
             *) exit 2 ;;\nesac",
        );
        // devin: signed in by its credentials file's presence.
        host.fake("devin", "echo 'devin 1.2.3'");
        let creds = host.path("data/devin/credentials.toml");
        std::fs::create_dir_all(creds.parent().unwrap()).unwrap();
        std::fs::write(&creds, format!("token = \"{SECRET}\"\n")).unwrap();
        // pi: installed, no auth.json. cursor-agent: not installed.
        host.fake("pi", "echo '0.85.1'");
        host
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.tmp.path().join(rel)
    }

    fn fake(&self, name: &str, body: &str) {
        let file = self.path("bin").join(name);
        std::fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn state_dir(&self) -> PathBuf {
        self.path("state/cadence")
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
        cmd.args(args)
            .env("HOME", self.path("home"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            .env("TMPDIR", self.path("tmp"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("bin").display()),
            );
        for var in [
            "CADENCE_STATE_DIR",
            "CADENCE_PM_DIR",
            "CADENCE_PROFILE",
            "CADENCE_ALIAS",
            "CADENCE_ROLLOUT_AS",
            "CADENCE_SANDBOX_ROOT",
            "CADENCE_SANDBOX_ALLOW_GLOBAL",
            "PI_CODING_AGENT_DIR",
            "TMUX",
        ] {
            cmd.env_remove(var);
        }
        cmd.output().unwrap()
    }

    fn setup_json(&self, port: u16) -> Vec<Value> {
        let out = self.run(&["setup", "--json", "--no-open", "--port", &port.to_string()]);
        let text = text(&out);
        assert!(out.status.success(), "setup failed: {text}");
        assert!(!text.contains(SECRET), "setup printed a credential: {text}");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
            .collect()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.run(&["ui", "stop"]);
        let _ = self.run(&["daemon", "stop"]);
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A board port in 3110-3199 — never production's 3010 — held for
/// the test's lifetime. Tests may run as separate processes (nextest)
/// or as threads (cargo test), so the lease is an exclusive `flock` on
/// `/tmp/cadence-test-ports/<port>.lock`: it excludes other threads and
/// other processes alike, and the kernel releases it when the test
/// ends, however it ends. The scan starts at a pid-derived offset so
/// concurrent processes rarely contend, and a port something else
/// (outside this scheme) already listens on is skipped.
struct PortLease {
    port: u16,
    _lock: std::fs::File,
}

fn test_port() -> PortLease {
    use std::os::fd::AsRawFd;
    let dir = Path::new("/tmp/cadence-test-ports");
    std::fs::create_dir_all(dir).unwrap();
    let span = 90;
    let start = std::process::id() as usize * 31 % span;
    for i in 0..span {
        let port = 3110 + ((start + i) % span) as u16;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("{port}.lock")))
            .unwrap();
        // SAFETY: plain syscall on a descriptor this function owns.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            continue;
        }
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return PortLease { port, _lock: lock };
        }
    }
    panic!("no free port in 3110-3199");
}

fn by_check(lines: &[Value]) -> BTreeMap<String, Value> {
    lines
        .iter()
        .map(|l| (l["check"].as_str().unwrap().to_string(), l.clone()))
        .collect()
}

fn status<'a>(checks: &'a BTreeMap<String, Value>, name: &str) -> &'a str {
    checks[name]["status"].as_str().unwrap()
}

/// Every entry under `root`: kind, bytes (a symlink's target) and
/// mtime in nanoseconds — the fingerprint two runs must share.
fn snapshot(root: &Path, tmpdir: &Path, into: &mut BTreeMap<PathBuf, (String, Vec<u8>, i128)>) {
    let meta = std::fs::symlink_metadata(root).unwrap();
    let mtime = i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec());
    let (kind, bytes) = if meta.file_type().is_symlink() {
        let target = std::fs::read_link(root).unwrap();
        ("link", target.to_string_lossy().as_bytes().to_vec())
    } else if meta.is_dir() {
        ("dir", Vec::new())
    } else if meta.is_file() {
        ("file", std::fs::read(root).unwrap())
    } else {
        // The daemon's socket: its inode and mtime still count.
        ("special", meta.ino().to_le_bytes().to_vec())
    };
    into.insert(root.to_path_buf(), (kind.to_string(), bytes, mtime));
    if meta.is_dir() {
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if !excluded(&path, tmpdir) {
                snapshot(&path, tmpdir, into);
            }
        }
    }
}

/// The only paths the fingerprint skips: `tmp/` (TMPDIR — scratch the
/// daemon and git may use) and `*.log` files (the daemon and board
/// append to their own logs as they serve). Everything else under the
/// root — HOME with every skill dir and the tracker, each XDG dir, the
/// state dir and the fake CLIs — must be unchanged by a second run.
fn excluded(path: &Path, tmpdir: &Path) -> bool {
    path == tmpdir || path.extension().is_some_and(|e| e == "log")
}

fn install_snapshot(host: &Host) -> BTreeMap<PathBuf, (String, Vec<u8>, i128)> {
    let mut all = BTreeMap::new();
    snapshot(host.tmp.path(), &host.path("tmp"), &mut all);
    all
}

/// First run creates state dir, tracker, skill, daemon and board;
/// a second run reports every one `ok, already present` and changes
/// no byte and no mtime of the state dir, tracker or skill.
#[test]
fn setup_creates_once_and_a_second_run_changes_nothing() {
    // Declared before the host: dropped after it stops the board.
    let lease = test_port();
    let host = Host::new();
    let port = lease.port;

    let first = host.setup_json(port);
    for line in &first {
        let keys: Vec<&str> = line
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys.len(), 4, "{line}");
        for key in ["check", "status", "detail", "fix"] {
            assert!(line.get(key).is_some(), "{key} missing: {line}");
        }
        assert!(
            ["ok", "created", "missing", "failed", "unknown"]
                .contains(&line["status"].as_str().unwrap()),
            "{line}"
        );
        // Only master may lack a fix: `master start` is not a verb of
        // this binary until CAD-339 lands.
        if !["ok", "created"].contains(&line["status"].as_str().unwrap())
            && line["check"] != "master"
        {
            assert!(line["fix"].is_string(), "no fix: {line}");
        }
    }
    let checks = by_check(&first);
    for name in ["state_dir", "tracker", "skill", "daemon", "ui"] {
        assert_eq!(status(&checks, name), "created", "{}", checks[name]);
    }
    assert!(host.path("home/pm/pm.yaml").is_file());
    assert!(host.path("home/.agents/skills/cadence/SKILL.md").is_file());
    let mode = std::fs::metadata(host.state_dir())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    assert!(checks["ui"]["detail"]
        .as_str()
        .unwrap()
        .contains(&format!("http://127.0.0.1:{port}")));

    // Providers: version plus sign-in from a non-secret signal.
    assert_eq!(status(&checks, "claude"), "ok", "{}", checks["claude"]);
    assert!(checks["claude"]["detail"]
        .as_str()
        .unwrap()
        .contains("9.9.9"));
    assert_eq!(status(&checks, "codex"), "missing");
    assert_eq!(checks["codex"]["fix"], "codex login");
    assert_eq!(status(&checks, "devin"), "ok", "{}", checks["devin"]);
    assert_eq!(status(&checks, "pi"), "missing");
    assert_eq!(status(&checks, "cursor-agent"), "missing");
    assert!(checks["cursor-agent"]["fix"]
        .as_str()
        .unwrap()
        .contains("cursor.com/install"));

    // CAD-339 and CAD-313 are reported, never implemented here.
    // `master start` is not a verb of this binary yet: no fix to paste.
    assert_eq!(status(&checks, "master"), "missing");
    assert!(checks["master"]["fix"].is_null(), "{}", checks["master"]);
    assert!(checks["master"]["detail"]
        .as_str()
        .unwrap()
        .contains("CAD-339"));
    assert!(!host.path("home/pm/agents").exists());
    assert_eq!(status(&checks, "login"), "unknown");

    let before = install_snapshot(&host);
    // Non-vacuous: the fingerprint covers what a re-run would rewrite.
    for rel in ["ui.json", "cadence.sqlite3"] {
        assert!(before.contains_key(&host.state_dir().join(rel)), "{rel}");
    }
    for rel in [
        "home/pm/pm.yaml",
        "home/.agents/skills/cadence/SKILL.md",
        "home/.claude/skills/cadence",
        "home/.cursor/skills/cadence",
        "home/.copilot/skills/cadence",
        "config",
        "data",
        "cache",
    ] {
        assert!(before.contains_key(&host.path(rel)), "{rel}");
    }
    let second = host.setup_json(port);
    let checks = by_check(&second);
    for name in ["state_dir", "tracker", "skill", "daemon", "ui"] {
        assert_eq!(status(&checks, name), "ok", "{}", checks[name]);
        assert!(
            checks[name]["detail"]
                .as_str()
                .unwrap()
                .starts_with("already present"),
            "{}",
            checks[name]
        );
        assert!(checks[name]["fix"].is_null());
    }
    // The human form says the same, and also changes nothing.
    let human = host.run(&["setup", "--port", &port.to_string()]);
    assert!(human.status.success(), "{}", text(&human));
    let human = text(&human);
    for name in ["state_dir", "tracker", "skill", "daemon", "ui"] {
        assert!(
            human
                .lines()
                .any(|l| l.starts_with("ok") && l.contains(name) && l.contains("already present")),
            "{name}: {human}"
        );
    }
    assert!(!human.contains(SECRET), "{human}");
    assert_eq!(
        before,
        install_snapshot(&host),
        "a second setup changed the install"
    );

    // Another --port while the board runs: the running board's URL is
    // reported, the difference named, nothing moved.
    let other_lease = test_port();
    let other = other_lease.port;
    let moved = by_check(&host.setup_json(other));
    let ui = moved["ui"]["detail"].as_str().unwrap();
    assert!(ui.contains(&format!("http://127.0.0.1:{port}")), "{ui}");
    assert!(ui.contains(&format!("--port {other} differs")), "{ui}");
    assert_eq!(
        before,
        install_snapshot(&host),
        "--port on a running board changed the install"
    );

    // Stopped daemon and board over existing state: `started`, not
    // `created`.
    assert!(host.run(&["ui", "stop"]).status.success());
    assert!(host.run(&["daemon", "stop"]).status.success());
    let restarted = by_check(&host.setup_json(port));
    for name in ["daemon", "ui"] {
        assert_eq!(status(&restarted, name), "started", "{}", restarted[name]);
    }
    for name in ["state_dir", "tracker", "skill"] {
        assert_eq!(status(&restarted, name), "ok", "{}", restarted[name]);
    }
}

/// A board ui.json shares on the tailnet is never restarted by setup —
/// that would re-run `tailscale serve`.
#[test]
fn setup_never_restarts_a_tailnet_shared_board() {
    // Declared before the host: dropped after it stops the board.
    let lease = test_port();
    let host = Host::new();
    let port = lease.port;
    host.setup_json(port);
    assert!(host.run(&["ui", "stop"]).status.success());
    let ui_json = host.state_dir().join("ui.json");
    let mut opts: Value = serde_json::from_slice(&std::fs::read(&ui_json).unwrap()).unwrap();
    opts["tailscale"] = serde_json::json!({
        "dns_name": "box.example.ts.net",
        "https_port": 9450,
        "target": format!("http://127.0.0.1:{port}"),
    });
    std::fs::write(&ui_json, serde_json::to_vec_pretty(&opts).unwrap()).unwrap();
    let before = std::fs::read(&ui_json).unwrap();
    let checks = by_check(&host.setup_json(port));
    assert_eq!(status(&checks, "ui"), "missing", "{}", checks["ui"]);
    assert!(checks["ui"]["fix"].as_str().unwrap().ends_with("ui start"));
    assert!(!host.state_dir().join("ui.pid").exists());
    assert_eq!(before, std::fs::read(&ui_json).unwrap());
}

/// A tracker dir that holds someone else's files is never initialised
/// over; the board, which needs the tracker, is not started.
#[test]
fn setup_refuses_a_non_tracker_dir_and_skips_what_needs_it() {
    let host = Host::new();
    std::fs::create_dir_all(host.path("home/pm")).unwrap();
    std::fs::write(host.path("home/pm/notes.md"), "mine\n").unwrap();
    let lease = test_port();
    let out = host.run(&["setup", "--json", "--port", &lease.port.to_string()]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let checks = by_check(&lines);
    assert_eq!(
        status(&checks, "tracker"),
        "failed",
        "{}",
        checks["tracker"]
    );
    assert_eq!(status(&checks, "ui"), "missing", "{}", checks["ui"]);
    assert!(!host.path("home/pm/pm.yaml").exists());
    assert!(!host.path("home/pm/.git").exists());
    assert!(!host.state_dir().join("ui.pid").exists());
}
