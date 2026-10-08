//! CAD-1207 acceptance, written from the ticket by someone other than the
//! implementer. Black box: drives the real `cadence` binary through its CLI
//! only. The implementer may not edit or weaken this file.
//!
//! The rule under test: `dev up` without `--build` starts the dev Cadence
//! from `current_exe()`. A board is only findable by
//! `ui stop` / `dev down` when that exe is named `cadence` or
//! `cadence-<suffix>`, so a binary named otherwise is refused up front,
//! naming the binary, before anything is started.
//!
//! Not covered: the 10 s readiness timeout of `ui start`. Nothing public
//! (no env, flag or `test-seam` hook) makes a spawned board bind but never
//! become ready, so no honest black-box case exists for it here.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::{Builder, TempDir};

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");
const SETTLE: Duration = Duration::from_secs(2);

struct Host {
    root: TempDir,
}

impl Host {
    fn new() -> Self {
        // Short /tmp root: unix socket paths are limited to 107 bytes.
        let root = Builder::new().prefix("c1207-").tempdir_in("/tmp").unwrap();
        for dir in ["home", "xdg", "tmp", "boxes", "locks", "bin"] {
            std::fs::create_dir(root.path().join(dir)).unwrap();
        }
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// A copy of the real binary under `name` (so /proc/<pid>/exe has it).
    fn copy_as(&self, name: &str) -> PathBuf {
        let bin = self.path("bin").join(name);
        std::fs::copy(BINARY, &bin).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    fn run_with(&self, exe: &Path, args: &[&str]) -> Output {
        let mut cmd = Command::new(exe);
        cmd.current_dir(self.root.path())
            .env("HOME", self.path("home"))
            .env("XDG_STATE_HOME", self.path("xdg"))
            .env("TMPDIR", self.path("tmp"))
            .env("CADENCE_SANDBOX_ROOT", self.path("boxes"))
            .env("CADENCE_TEST_PORT_LOCK_DIR", self.path("locks"))
            .env("CADENCE_SUITE_LOCK", self.path("locks/suite.lock"))
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("CADENCE_PM_DIR")
            .env_remove("CADENCE_HOME")
            .env_remove("CADENCE_PROFILE")
            .env_remove("CADENCE_ALIAS")
            .env_remove("CADENCE_ROLLOUT_AS")
            .env_remove("CADENCE_SANDBOX_ALLOW_GLOBAL")
            .env_remove("XDG_DATA_HOME");
        for name in cadence_agent::adapter::PROVIDER_COMMAND_VARS {
            cmd.env(name, cadence_agent::adapter::REFUSED_COMMAND);
        }
        for name in ["CARGO_HOME", "RUSTUP_HOME"] {
            if let Some(value) = std::env::var_os(name) {
                cmd.env(name, value);
            }
        }
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = cadence_agent::reaper::spawn(&mut cmd).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(child.wait_with_output());
        });
        rx.recv_timeout(Duration::from_secs(60))
            .expect("cadence did not finish in 60s")
            .unwrap()
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
    .to_lowercase()
}

/// Pids running from, or naming in argv or environment, anything under the
/// temp root: exe path OR `--state-dir <root>/…` argv (pgrep -f would match
/// this very test).
fn procs_under(root: &Path) -> Vec<u32> {
    let needle = root.to_string_lossy().into_owned();
    let me = std::process::id();
    let mut out: Vec<u32> = std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()))
        .filter(|pid| *pid != me)
        .filter(|pid| {
            std::fs::read_link(format!("/proc/{pid}/exe"))
                .is_ok_and(|e| e.to_string_lossy().starts_with(&needle))
                || ["cmdline", "environ"].iter().any(|f| {
                    std::fs::read(format!("/proc/{pid}/{f}"))
                        .map(|b| String::from_utf8_lossy(&b).contains(&needle))
                        .unwrap_or(false)
                })
        })
        .collect();
    out.sort_unstable();
    out
}

fn alive(pid: u32) -> bool {
    // A zombie still has a /proc entry; count it as gone.
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| !s.contains(") Z"))
        .unwrap_or(false)
}

fn wait_until(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + limit;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// SIGTERM, then SIGKILL, every process under the root; wait for each to go.
fn reap_under(root: &Path) {
    for signal in [libc::SIGTERM, libc::SIGKILL] {
        let pids = procs_under(root);
        if pids.is_empty() {
            return;
        }
        for pid in &pids {
            unsafe { libc::kill(*pid as i32, signal) };
        }
        wait_until(Duration::from_secs(10), || procs_under(root).is_empty());
    }
}

fn assert_nothing_runs_from(host: &Host) {
    let left = procs_under(host.root.path());
    assert!(
        left.is_empty(),
        "processes left running from the temp root: {left:?}"
    );
    std::thread::sleep(SETTLE);
    let late = procs_under(host.root.path());
    assert!(
        late.is_empty(),
        "processes appeared after the settle window (late spawn): {late:?}"
    );
}

/// Reaps whatever the test started, however the assertions went.
struct Cleanup<'a> {
    host: &'a Host,
    stopper: PathBuf,
    names: Vec<&'static str>,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        for n in &self.names {
            let _ = self.host.run_with(&self.stopper, &["dev", "down", n]);
        }
        reap_under(self.host.root.path());
        std::thread::sleep(SETTLE);
        reap_under(self.host.root.path());
    }
}

/// A free port in the dev range; tests run in parallel, each with its own
/// lock dir, so each claims its port from a base of its own.
fn free_port(base: u16) -> String {
    (base..3200)
        .chain(3110..base)
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("no free port in 3110-3199")
        .to_string()
}

fn board_running(host: &Host, exe: &Path, name: &str) -> bool {
    let out = host.run_with(exe, &["dev", "status", name]);
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .map(|v| v["ui"] == "running")
        .unwrap_or(false)
}

/// A refusal is exit 3 (rejected) carrying a reason sentence that names the
/// binary. A clap usage error (2), a crash (101/signal) or a bare non-zero
/// never counts.
fn assert_refused_naming(out: &Output, what: &str, exe_name: &str) {
    let all = text(out);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{what}: expected the rejected exit class (3)\n{all}"
    );
    assert!(
        !all.contains("unrecognized subcommand")
            && !all.contains("usage:")
            && !all.contains("panicked"),
        "{what}: this is a usage error or crash, not a refusal\n{all}"
    );
    assert!(
        all.contains(exe_name),
        "{what}: the reason does not name the binary `{exe_name}`\n{all}"
    );
}

// (a1) `dev up` without --build from a binary outside the name rule.
#[test]
fn a_dev_up_without_build_is_refused_for_a_misnamed_binary() {
    for (n, bad) in ["cad-dev", "cadencex"].into_iter().enumerate() {
        let host = Host::new();
        let exe = host.copy_as(bad);
        let ok = host.copy_as("cadence-ok");
        let c = Cleanup {
            host: &host,
            stopper: ok,
            names: vec!["x"],
        };
        let port = free_port(3110 + 20 * n as u16);
        let out = host.run_with(&exe, &["dev", "up", "--name", "x", "--port", &port]);
        assert_refused_naming(&out, &format!("dev up from `{bad}`"), bad);
        assert!(
            procs_under(host.root.path()).is_empty(),
            "dev up from `{bad}` started something: {:?}",
            procs_under(host.root.path())
        );
        let state = host.path("boxes/x/state");
        assert!(
            !state.join("cadence.sock").exists() && !state.join("ui.pid").exists(),
            "dev up from `{bad}` left a daemon socket or ui.pid"
        );
        drop(c);
        assert_nothing_runs_from(&host);
    }
}

// (b) positive control: a correctly named copy (`cadence-dev`) starts a
// board with `dev up` (no --build) that `dev down` stops, leaving nothing.
#[test]
fn b_cadence_dev_copy_starts_a_board_that_dev_down_stops() {
    let host = Host::new();
    let exe = host.copy_as("cadence-dev");
    let c = Cleanup {
        host: &host,
        stopper: exe.clone(),
        names: vec!["x"],
    };
    let port = free_port(3170);
    let up = host.run_with(&exe, &["dev", "up", "--name", "x", "--port", &port]);
    assert!(up.status.success(), "dev up (cadence-dev): {}", text(&up));
    assert!(
        wait_until(Duration::from_secs(15), || board_running(&host, &exe, "x")),
        "the board of a `cadence-dev` binary is not reported running"
    );
    let board: Vec<u32> = procs_under(host.root.path())
        .into_iter()
        .filter(|p| {
            std::fs::read_link(format!("/proc/{p}/exe"))
                .is_ok_and(|e| e.file_name().is_some_and(|n| n == "cadence-dev"))
                && std::fs::read(format!("/proc/{p}/cmdline"))
                    .map(|b| String::from_utf8_lossy(&b).contains("\0ui\0run\0"))
                    .unwrap_or(false)
        })
        .collect();
    assert_eq!(board.len(), 1, "expected exactly one board: {board:?}");

    let down = host.run_with(&exe, &["dev", "down", "x"]);
    assert!(down.status.success(), "dev down: {}", text(&down));
    assert!(
        wait_until(Duration::from_secs(15), || !alive(board[0])),
        "`dev down` left the board (pid {}) running: {}",
        board[0],
        text(&down)
    );
    std::thread::sleep(SETTLE);
    let left = procs_under(host.root.path());
    assert!(left.is_empty(), "orphans after `dev down`: {left:?}");
    drop(c);
    assert_nothing_runs_from(&host);
}
