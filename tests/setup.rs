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
                "echo \"$*\" >> \"$TMPDIR/probes.txt\"\n\
                 case \"$1 $2\" in\n\
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
        self.command(args).output().unwrap()
    }

    /// The binary under this host's isolated environment.
    fn command(&self, args: &[&str]) -> std::process::Command {
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
        cmd
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

    // CAD-339 and CAD-313 are reported, never implemented here. This
    // binary has `master start` (CAD-339), so setup names it as the fix
    // and still installs nothing itself.
    assert_eq!(status(&checks, "master"), "missing");
    assert!(
        checks["master"]["fix"]
            .as_str()
            .is_some_and(|f| f.ends_with("master start")),
        "{}",
        checks["master"]
    );
    assert!(checks["master"]["detail"]
        .as_str()
        .unwrap()
        .contains("`master start` installs them (CAD-339)"));
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

// ---------- CAD-327: the board's detect-only `/api/setup` ----------

/// A foreground `ui run` on a leased port, killed on drop.
struct Board {
    child: std::process::Child,
    port: u16,
}

impl Board {
    fn start(host: &Host, port: u16) -> Self {
        Self::start_with(host, port, &[])
    }

    fn start_with(host: &Host, port: u16, extra: &[&str]) -> Self {
        let port_s = port.to_string();
        let mut args = vec!["ui", "run", "--port", &port_s];
        args.extend_from_slice(extra);
        let child = host
            .command(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let board = Self { child, port };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while board.request("GET", "/api/health").0 != 200 {
            assert!(std::time::Instant::now() < deadline, "board never answered");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        board
    }

    /// `(status, body)`; `(0, "")` when nothing answers.
    fn request(&self, method: &str, path: &str) -> (u16, String) {
        use std::io::{Read, Write};
        let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", self.port)) else {
            return (0, String::new());
        };
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(90)))
            .unwrap();
        let req = format!(
            "{method} {path} HTTP/1.0\r\nHost: 127.0.0.1:{}\r\nContent-Length: 0\r\n\r\n",
            self.port
        );
        if stream.write_all(req.as_bytes()).is_err() {
            return (0, String::new());
        }
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf);
        let text = String::from_utf8_lossy(&buf).to_string();
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
        (status, body.to_string())
    }
}

impl Drop for Board {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `/api/setup` on a fresh host: every setup check, detect only. It
/// creates nothing (no state dir, tracker, skill, daemon), never echoes
/// a provider's output, and refuses every write method.
#[test]
fn board_setup_is_detect_only_and_writes_nothing() {
    let lease = test_port();
    let host = Host::new();
    let board = Board::start(&host, lease.port);
    let before = install_snapshot(&host);

    let (code, body) = board.request("GET", "/api/setup");
    assert_eq!(code, 200, "{body}");
    assert!(
        !body.contains(SECRET),
        "a provider's output reached the board: {body}"
    );
    let payload: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(payload["detect_only"], true);
    let lines = payload["checks"].as_array().unwrap().clone();
    for line in &lines {
        for key in ["check", "status", "detail", "fix", "group"] {
            assert!(line.get(key).is_some(), "{key} missing: {line}");
        }
    }
    let checks = by_check(&lines);
    // Absent, and reported — never created or started.
    for name in ["state_dir", "tracker", "skill", "daemon", "master"] {
        assert_eq!(status(&checks, name), "missing", "{}", checks[name]);
    }
    for name in ["state_dir", "tracker", "skill", "daemon"] {
        assert!(checks[name]["fix"].is_string(), "{}", checks[name]);
    }
    // CAD-448/CAD-439: the master's own login is its own check — missing
    // until its separate CLAUDE_CONFIG_DIR holds a .credentials.json; a
    // host that cannot confine runs the master on the operator's login
    // and asks for nothing.
    if cadence_agent::confine::available().is_ok() {
        assert_eq!(status(&checks, "master_login"), "missing");
        let login_fix = checks["master_login"]["fix"].as_str().unwrap();
        assert!(login_fix.contains("claude auth login"), "{login_fix}");
        assert!(login_fix.contains("CLAUDE_CONFIG_DIR="), "{login_fix}");
    } else {
        assert_eq!(status(&checks, "master_login"), "ok");
        assert!(checks["master_login"]["detail"]
            .as_str()
            .unwrap()
            .contains("--unconfined"));
    }
    // The board answering is the board running.
    assert_eq!(status(&checks, "ui"), "ok", "{}", checks["ui"]);
    assert!(checks["ui"]["detail"]
        .as_str()
        .unwrap()
        .contains(&format!("127.0.0.1:{}", lease.port)));
    // Providers: version and sign-in from setup's own signals.
    assert_eq!(status(&checks, "claude"), "ok", "{}", checks["claude"]);
    assert!(checks["claude"]["detail"]
        .as_str()
        .unwrap()
        .contains("9.9.9"));
    assert_eq!(checks["claude"]["group"], "provider");
    assert_eq!(status(&checks, "codex"), "missing");
    assert_eq!(checks["codex"]["fix"], "codex login");
    assert_eq!(status(&checks, "devin"), "ok", "{}", checks["devin"]);
    assert_eq!(checks["master"]["group"], "master");
    assert_eq!(checks["master_login"]["group"], "master");
    assert_eq!(checks["daemon"]["group"], "environment");
    // CAD-448: the wizard's master step gets the providers `master
    // start` accepts — the signed-in claude with its exact command;
    // devin is signed in but can never be the master, so it is not
    // offered.
    let offers = payload["master"]["providers"].as_array().unwrap();
    assert_eq!(offers.len(), 1, "{offers:?}");
    assert_eq!(offers[0]["bin"], "claude");
    assert_eq!(offers[0]["ready"], true, "{offers:?}");
    // The board serves its own state dir: the command pastes as is.
    assert_eq!(
        offers[0]["start"].as_str().unwrap(),
        "cadence master start --provider claude"
    );
    // A re-check runs the probes again and still writes nothing.
    std::thread::sleep(std::time::Duration::from_secs(6));
    let (code, body) = board.request("GET", "/api/setup?fresh=1");
    assert_eq!(code, 200, "{body}");
    assert!(!body.contains(SECRET), "{body}");

    for method in ["POST", "PATCH", "DELETE"] {
        let (code, body) = board.request(method, "/api/setup");
        assert_eq!(code, 405, "{method}: {body}");
    }
    assert!(
        !host.state_dir().exists(),
        "the board created the state dir"
    );
    assert!(
        !host.path("home/pm").exists(),
        "the board created a tracker"
    );
    assert_eq!(
        before,
        install_snapshot(&host),
        "/api/setup changed the host"
    );
}

/// CAD-448: once the master's own `CLAUDE_CONFIG_DIR` holds a login the
/// wizard reads its presence — never the file's contents.
#[test]
fn board_setup_reports_the_masters_own_login() {
    let lease = test_port();
    let host = Host::new();
    let creds = host.state_dir().join("master/claude/.credentials.json");
    std::fs::create_dir_all(creds.parent().unwrap()).unwrap();
    std::fs::write(&creds, format!("{{\"secret\": \"{SECRET}\"}}")).unwrap();
    let board = Board::start(&host, lease.port);
    let (code, body) = board.request("GET", "/api/setup");
    assert_eq!(code, 200, "{body}");
    assert!(
        !body.contains(SECRET),
        "the login file's contents leaked: {body}"
    );
    let payload: Value = serde_json::from_str(&body).unwrap();
    let checks = by_check(payload["checks"].as_array().unwrap());
    assert_eq!(
        status(&checks, "master_login"),
        "ok",
        "{}",
        checks["master_login"]
    );
    if cadence_agent::confine::available().is_ok() {
        assert!(checks["master_login"]["detail"]
            .as_str()
            .unwrap()
            .contains("own login"));
    }
    assert!(checks["master_login"]["fix"].is_null());
}

/// A provider CLI that never answers cannot hold the page: each probe
/// is bounded, so the whole run answers in bounded time.
#[test]
fn board_setup_answers_in_bounded_time_when_a_cli_hangs() {
    let lease = test_port();
    let host = Host::new();
    host.fake("codex", "sleep 120");
    let board = Board::start(&host, lease.port);
    let started = std::time::Instant::now();
    let (code, body) = board.request("GET", "/api/setup");
    let elapsed = started.elapsed();
    assert_eq!(code, 200, "{body}");
    // codex: `--version` and `login status`, 5 s each.
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "took {elapsed:?}"
    );
    let payload: Value = serde_json::from_str(&body).unwrap();
    let checks = by_check(payload["checks"].as_array().unwrap());
    assert_eq!(status(&checks, "codex"), "unknown", "{}", checks["codex"]);
    let detail = checks["codex"]["detail"].as_str().unwrap();
    assert!(detail.contains("version unknown"), "{detail}");
    assert!(detail.contains("did not answer"), "{detail}");
}

/// How often the fake `claude` answered `--version` — one per run of
/// the checks.
fn claude_runs(host: &Host) -> usize {
    std::fs::read_to_string(host.path("tmp/probes.txt"))
        .unwrap_or_default()
        .lines()
        .filter(|l| *l == "--version")
        .count()
}

fn get_json(board: &Board, path: &str) -> Value {
    let (code, body) = board.request("GET", path);
    assert_eq!(code, 200, "{path}: {body}");
    serde_json::from_str(&body).unwrap()
}

/// A read-only board refuses `/api/setup` before any probe runs.
#[test]
fn board_setup_is_refused_on_a_read_only_board() {
    let lease = test_port();
    let host = Host::new();
    let board = Board::start_with(&host, lease.port, &["--read-only"]);
    let (code, body) = board.request("GET", "/api/setup?fresh=1");
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("read_only"), "{body}");
    assert_eq!(claude_runs(&host), 0, "a refused request spawned a probe");
}

/// Concurrent requests share one run; a re-check within 5 s answers
/// from it (and says when the next may run); `fresh=0` is not fresh.
#[test]
fn board_setup_shares_one_run_and_throttles_rechecks() {
    let lease = test_port();
    let host = Host::new();
    let board = std::sync::Arc::new(Board::start(&host, lease.port));
    let answers: Vec<Value> = (0..4)
        .map(|_| {
            let board = board.clone();
            std::thread::spawn(move || get_json(&board, "/api/setup"))
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect();
    assert_eq!(
        claude_runs(&host),
        1,
        "concurrent requests ran the checks twice"
    );
    let first = answers[0]["checked_at"].clone();
    assert!(answers.iter().all(|a| a["checked_at"] == first));
    assert_eq!(answers.iter().filter(|a| a["ran_now"] == true).count(), 1);

    // A re-check straight away: the same run, and when the next may be.
    let quick = get_json(&board, "/api/setup?fresh=1");
    assert_eq!(quick["ran_now"], false, "{quick}");
    assert_eq!(quick["checked_at"], first);
    let wait = quick["recheck_in_ms"].as_u64().unwrap();
    assert!(wait > 0 && wait <= 5000, "{quick}");
    assert_eq!(claude_runs(&host), 1);

    std::thread::sleep(std::time::Duration::from_millis(wait + 200));
    // `fresh=0` is an ordinary read: the minute-long run still answers.
    let zero = get_json(&board, "/api/setup?fresh=0");
    assert_eq!(zero["ran_now"], false, "{zero}");
    assert_eq!(zero["recheck_in_ms"], 0, "{zero}");
    assert_eq!(claude_runs(&host), 1);
    let again = get_json(&board, "/api/setup?fresh=true");
    assert_eq!(again["ran_now"], true, "{again}");
    assert_ne!(again["checked_at"], first);
    assert_eq!(claude_runs(&host), 2);
}
