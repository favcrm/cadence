//! CAD-1206 acceptance, written from the ticket by someone other than the
//! implementer. Black box: drives the real `cadence` binary through its CLI
//! only. The implementer may not edit or weaken this file.
//!
//! The rule under test: `ui stop` / `dev down` may signal only the board of
//! THIS store, i.e. a process whose exe is `cadence` or `cadence-<suffix>`
//! AND whose argv is `ui run` with this store's `--state-dir`. A `ui.pid`
//! pointing at anything else is never signalled. And a real board started
//! from a build named `cadence-new` is a board: `dev down` stops it.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
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
        let root = Builder::new().prefix("c1206-").tempdir_in("/tmp").unwrap();
        for dir in ["home", "xdg", "tmp", "boxes", "locks", "bin", "plain"] {
            std::fs::create_dir(root.path().join(dir)).unwrap();
        }
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BINARY);
        cmd.env("HOME", self.path("home"))
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
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = self.command(args);
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

fn pid_list(root: &Path) -> Vec<u32> {
    let mut pids = vec![];
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        if let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse().ok()) {
            pids.push(pid);
        }
    }
    let _ = root;
    pids
}

/// Pids running from, or naming in argv or environment, anything under the
/// temp root (pgrep -f would match this very test).
fn procs_under(root: &Path) -> Vec<u32> {
    let needle = root.to_string_lossy().into_owned();
    let me = std::process::id();
    let mut out: Vec<u32> = pid_list(root)
        .into_iter()
        .filter(|pid| *pid != me)
        .filter(|pid| {
            let exe_hit = std::fs::read_link(format!("/proc/{pid}/exe"))
                .is_ok_and(|e| e.to_string_lossy().starts_with(&needle));
            exe_hit
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
    strangers: Vec<Child>,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = self.host.run(&["dev", "down", "dv"]);
        for c in &mut self.strangers {
            let _ = c.kill();
            let _ = c.wait();
        }
        reap_under(self.host.root.path());
        std::thread::sleep(SETTLE);
        reap_under(self.host.root.path());
    }
}

/// A copy of `/bin/sh` under `exe_name` (so /proc/<pid>/exe has that
/// basename), started with the argv of a board for `named_state` and kept
/// alive by an open stdin. `sh -c 'read x' ui run …` leaves the extra words
/// as positional parameters, so the argv reads `<exe> -c … ui run
/// --state-dir <dir>`.
fn spawn_stranger(host: &Host, exe_name: &str, named_state: &Path) -> Child {
    // One directory per stranger: a copy cannot be rewritten while running.
    let dir = Builder::new().tempdir_in(host.path("bin")).unwrap().keep();
    let exe = dir.join(exe_name);
    std::fs::copy("/bin/sh", &exe).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Another test thread forking while the copy was written can briefly
    // hold it open for writing (ETXTBSY): retry the exec.
    let mut tries = 0;
    let mut child = loop {
        let mut cmd = Command::new(&exe);
        cmd.arg0(exe.to_str().unwrap())
            .args(["-c", "read x", "ui", "run", "--state-dir"])
            .arg(named_state)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let spawned = cadence_agent::reaper::spawn(&mut cmd);
        match spawned {
            Ok(c) => break c,
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && tries < 100 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("spawning stranger {exe_name}: {e}"),
        }
    };
    // Keep stdin open for the life of the child.
    let stdin = child.stdin.take().unwrap();
    std::mem::forget(stdin);
    let pid = child.id();
    assert!(
        wait_until(Duration::from_secs(5), || {
            std::fs::read_link(format!("/proc/{pid}/exe"))
                .is_ok_and(|e| e.file_name().is_some_and(|n| n == exe_name))
        }),
        "stranger {exe_name} did not start"
    );
    child
}

fn new_build(host: &Host, name: &str) -> PathBuf {
    let bin = host.path("bin").join(name);
    std::fs::copy(BINARY, &bin).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// A free port in the dev range. Tests run in parallel and each host has its
/// own lock dir, so each test claims its port by a base of its own.
fn free_port(base: u16) -> String {
    (base..3200)
        .chain(3110..base)
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("no free port in 3110-3199")
        .to_string()
}

fn board_running(host: &Host, name: &str) -> bool {
    let out = host.run(&["dev", "status", name]);
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .map(|v| v["ui"] == "running")
        .unwrap_or(false)
}

/// Shared body of (a) and (b): a `ui.pid` that points at a stranger must
/// never lead to that stranger being signalled, by `ui stop` or `dev down`.
fn stranger_is_never_signalled(
    exe_name: &str,
    argv_names_this_store: bool,
    what: &str,
    port_base: u16,
) {
    let host = Host::new();
    let mut c = Cleanup {
        host: &host,
        strangers: vec![],
    };

    // Plain store: `ui stop`.
    let plain = host.path("plain/state");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o700)).unwrap();
    let other = host.path("plain/other-state");
    let named = if argv_names_this_store {
        &plain
    } else {
        &other
    };
    let s1 = spawn_stranger(&host, exe_name, named);
    let pid1 = s1.id();
    c.strangers.push(s1);
    std::fs::write(plain.join("ui.pid"), pid1.to_string()).unwrap();

    let out = host.run(&["--state-dir", plain.to_str().unwrap(), "ui", "stop"]);
    let all = text(&out);
    assert!(
        !all.contains("panicked") && out.status.code().is_some_and(|c| c < 100),
        "{what}: `ui stop` crashed\n{all}"
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        alive(pid1),
        "{what}: `ui stop` signalled a stranger ({exe_name}) it must not touch\n{all}"
    );

    // Dev store: `dev down`. The real board is stopped first, then the
    // stranger takes its place in ui.pid.
    let up = host.run(&["dev", "up", "--name", "dv", "--port", &free_port(port_base)]);
    assert!(up.status.success(), "dev up: {}", text(&up));
    assert!(
        wait_until(Duration::from_secs(15), || board_running(&host, "dv")),
        "dev up: board never reported running"
    );
    let dv_state = host.path("boxes/dv/state");
    let stop = host.run(&["--state-dir", dv_state.to_str().unwrap(), "ui", "stop"]);
    assert!(
        stop.status.success(),
        "stopping the real board: {}",
        text(&stop)
    );
    let named2 = if argv_names_this_store {
        dv_state.clone()
    } else {
        other.clone()
    };
    let s2 = spawn_stranger(&host, exe_name, &named2);
    let pid2 = s2.id();
    c.strangers.push(s2);
    std::fs::write(dv_state.join("ui.pid"), pid2.to_string()).unwrap();

    let down = host.run(&["dev", "down", "dv"]);
    let all = text(&down);
    assert!(
        !all.contains("panicked") && down.status.code().is_some_and(|c| c < 100),
        "{what}: `dev down` crashed\n{all}"
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        alive(pid2),
        "{what}: `dev down` signalled a stranger ({exe_name}) it must not touch\n{all}"
    );

    // Our own processes only: strangers are reaped by Cleanup.
    drop(c);
    assert_nothing_runs_from(&host);
}

// (a) exe name passes the rule, argv names a DIFFERENT state dir.
#[test]
fn a_board_named_process_for_another_store_is_not_signalled() {
    stranger_is_never_signalled("cadence-x", false, "(a) other store's argv", 3120);
}

// (b1) argv names THIS store, exe name outside the rule.
#[test]
fn b_exe_name_xcadence_is_not_signalled() {
    stranger_is_never_signalled("xcadence", true, "(b1) exe xcadence", 3140);
}

// (b2) same, `cadencex` (no dash) and bare `cadence-` (empty suffix).
#[test]
fn b_exe_name_cadencex_and_empty_suffix_are_not_signalled() {
    stranger_is_never_signalled("cadencex", true, "(b2) exe cadencex", 3160);
    stranger_is_never_signalled("cadence-", true, "(b2) exe cadence- (empty suffix)", 3180);
}

// (c) positive control: a board from a build named `cadence-new` IS a board.
// Fails on a tree where `dev down` cannot see such a board (it is left
// running, orphaned).
#[test]
fn c_board_from_a_cadence_new_build_is_stopped_by_dev_down() {
    let host = Host::new();
    let build = new_build(&host, "cadence-new");
    let c = Cleanup {
        host: &host,
        strangers: vec![],
    };
    let up = host.run(&[
        "dev",
        "up",
        "--name",
        "dv",
        "--port",
        &free_port(3190),
        "--build",
        build.to_str().unwrap(),
    ]);
    assert!(
        up.status.success(),
        "dev up --build cadence-new: {}",
        text(&up)
    );
    assert!(
        wait_until(Duration::from_secs(15), || board_running(&host, "dv")),
        "the board of a cadence-new build is not reported running by `dev status`"
    );
    let board: Vec<u32> = procs_under(host.root.path())
        .into_iter()
        .filter(|p| {
            std::fs::read_link(format!("/proc/{p}/exe"))
                .is_ok_and(|e| e.file_name().is_some_and(|n| n == "cadence-new"))
                && std::fs::read(format!("/proc/{p}/cmdline"))
                    .map(|b| String::from_utf8_lossy(&b).contains("\0ui\0run\0"))
                    .unwrap_or(false)
        })
        .collect();
    assert_eq!(
        board.len(),
        1,
        "expected exactly one cadence-new board: {board:?}"
    );

    let down = host.run(&["dev", "down", "dv"]);
    assert!(down.status.success(), "dev down: {}", text(&down));
    let gone = wait_until(Duration::from_secs(15), || !alive(board[0]));
    assert!(
        gone,
        "`dev down` left the cadence-new board (pid {}) running: {}",
        board[0],
        text(&down)
    );
    let _ = std::io::stderr().flush();
    // Nothing may be left before Cleanup gets a chance to hide it.
    std::thread::sleep(SETTLE);
    let left = procs_under(host.root.path());
    assert!(left.is_empty(), "orphans after `dev down`: {left:?}");
    drop(c);
    assert_nothing_runs_from(&host);
}

// Control for (a)/(b): the same stranger shape, but with an exe named exactly
// `cadence` and argv naming THIS store, is treated as the board and stopped.
// Proves the refusals above are decided by identity, not by `ui.pid` being
// ignored altogether.
#[test]
fn d_control_exact_identity_is_stopped_by_ui_stop() {
    let host = Host::new();
    let mut c = Cleanup {
        host: &host,
        strangers: vec![],
    };
    let state = host.path("plain/state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    let s = spawn_stranger(&host, "cadence", &state);
    let pid = s.id();
    c.strangers.push(s);
    std::fs::write(state.join("ui.pid"), pid.to_string()).unwrap();
    let out = host.run(&["--state-dir", state.to_str().unwrap(), "ui", "stop"]);
    assert!(
        wait_until(Duration::from_secs(10), || !alive(pid)),
        "control: `ui stop` did not stop a process with this store's board identity: {}",
        text(&out)
    );
    drop(c);
    assert_nothing_runs_from(&host);
}
