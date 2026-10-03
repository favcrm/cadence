//! CAD-1081 results, run against the real `cadence` binary: a stale
//! `ui.pid` never blocks the board or aims `ui stop` at another process.
//!
//! Every board here gets a port from 3110-3199, written to `ui.json`
//! before any pid is planted and passed as `--port`, so nothing can reach
//! the production board on 3010. Every process this file spawns is killed
//! by a drop guard, a failed assertion included.

use cadence_agent::reaper;
use serde_json::Value;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tempfile::{Builder, TempDir};

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");
static PORTS: Mutex<u16> = Mutex::new(3110);

/// A free port in 3110-3199, never repeated within this binary.
fn test_port() -> u16 {
    let mut next = PORTS.lock().unwrap();
    while *next <= 3199 {
        let port = *next;
        *next += 1;
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    panic!("no free port in 3110-3199");
}

fn alive(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .map(|s| !s.contains("State:\tZ"))
        .unwrap_or(false)
}

/// An isolated HOME, PM dir and state dir under a short /tmp root.
struct Host {
    root: TempDir,
    state: PathBuf,
}

impl Host {
    fn new(port: u16) -> Self {
        let root = Builder::new().prefix("c1081-").tempdir_in("/tmp").unwrap();
        for dir in ["home", "pm", "st"] {
            std::fs::create_dir(root.path().join(dir)).unwrap();
        }
        let state = root.path().join("st");
        std::fs::write(state.join("ui.json"), format!(r#"{{"port":{port}}}"#)).unwrap();
        Self { root, state }
    }

    fn cadence(&self, state_arg: &Path, cwd: &Path, args: &[&str]) -> Value {
        let mut cmd = Command::new(BINARY);
        cmd.arg("--state-dir")
            .arg(state_arg)
            .args(args)
            .current_dir(cwd)
            .env("HOME", self.root.path().join("home"))
            .env("XDG_STATE_HOME", self.root.path().join("home"))
            .env("CADENCE_PM_DIR", self.root.path().join("pm"))
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("CADENCE_ALIAS");
        let out = reaper::output(&mut cmd).unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!(
                "{args:?}: {e}: {text} {}",
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }

    fn ui(&self, args: &[&str]) -> Value {
        let mut full = vec!["ui"];
        full.extend_from_slice(args);
        self.cadence(&self.state, self.root.path(), &full)
    }

    fn plant_pid(&self, pid: u32) {
        std::fs::write(self.state.join("ui.pid"), pid.to_string()).unwrap();
    }
}

/// Stops this host's board on drop, a failed assertion included.
struct Board<'a>(&'a Host);

impl Drop for Board<'_> {
    fn drop(&mut self) {
        let mut cmd = Command::new(BINARY);
        cmd.arg("--state-dir")
            .arg(&self.0.state)
            .args(["ui", "stop"])
            .env("HOME", self.0.root.path().join("home"))
            .env("CADENCE_PM_DIR", self.0.root.path().join("pm"));
        let _ = reaper::output(&mut cmd);
    }
}

/// A live process that is not a board. Killed on drop.
struct Bystander(Child);

impl Bystander {
    fn new() -> Self {
        let mut cmd = Command::new("sleep");
        cmd.arg("600").stdout(Stdio::null());
        Self(reaper::spawn(&mut cmd).unwrap())
    }
    fn pid(&self) -> i32 {
        self.0.id() as i32
    }
}

impl Drop for Bystander {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// R1, the D1.2 incident: ui.pid names a live process that is not the
/// board. `ui start` starts the board anyway and `ui status` reports it.
#[test]
fn stale_live_pid_does_not_block_ui_start() {
    let host = Host::new(test_port());
    let port = test_port().to_string();
    let _board = Board(&host);
    let bystander = Bystander::new();
    host.plant_pid(bystander.0.id());

    let out = host.ui(&["start", "--port", &port]);
    assert_eq!(out["state"], "started", "{out}");
    let board = out["pid"].as_i64().unwrap();
    assert_ne!(board, bystander.pid() as i64);

    let out = host.ui(&["status"]);
    assert_eq!(out["state"], "running", "{out}");
    assert_eq!(out["pid"], board, "{out}");
}

/// R2, the forbidden harm: ui.pid names a live process that is not the
/// board. `ui stop` must not signal it.
#[test]
fn ui_stop_never_signals_a_non_board_pid() {
    let host = Host::new(test_port());
    let bystander = Bystander::new();
    host.plant_pid(bystander.0.id());

    let out = host.ui(&["stop"]);
    assert_eq!(out["state"], "stopped", "{out}");
    // `sleep` dies on SIGTERM; give a wrongly aimed signal time to land.
    let until = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < until {
        assert!(alive(bystander.pid()), "ui stop signalled a non-board pid");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// R3: a board started with a relative `--state-dir` is found and
/// stopped from another directory.
#[test]
fn relative_state_dir_board_is_found_and_stopped_from_another_cwd() {
    let host = Host::new(test_port());
    let port = test_port().to_string();
    let _board = Board(&host);

    let out = host.cadence(
        Path::new("st"),
        host.root.path(),
        &["ui", "start", "--port", &port],
    );
    assert_eq!(out["state"], "started", "{out}");
    let board = out["pid"].as_i64().unwrap() as i32;

    let elsewhere = TempDir::new().unwrap();
    let out = host.cadence(&host.state, elsewhere.path(), &["ui", "status"]);
    assert_eq!(out["state"], "running", "{out}");
    assert_eq!(out["pid"], board, "{out}");

    let out = host.cadence(&host.state, elsewhere.path(), &["ui", "stop"]);
    assert_eq!(out["pid"], board, "{out}");
    assert!(!alive(board), "ui stop from another cwd left the board");
}
