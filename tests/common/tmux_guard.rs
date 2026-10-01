//! CAD-955: a test that starts a tmux server owns that server until it
//! exits, on every path. A bare `kill-server` at the end of a test body
//! is skipped by a failed assertion, so the kill lives in `Drop`, which
//! also runs while a panic unwinds. A server left behind keeps whatever
//! file descriptors it inherited (the suite lock among them) and piles
//! up on a shared host.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Pids whose argv carries `-L <socket>` — exactly the tmux client and
/// server of that one socket name, never another lane's or production's.
pub fn tmux_pids_on_socket(socket: &str) -> Vec<u32> {
    let mut pids = Vec::new();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return pids;
    };
    for entry in procs.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(raw) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let argv: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
        if argv
            .windows(2)
            .any(|w| w[0] == b"-L" && w[1] == socket.as_bytes())
        {
            pids.push(pid);
        }
    }
    pids
}

/// Kills the tmux server on one socket when dropped, then waits until
/// no process of that socket is left.
pub struct TmuxServerGuard {
    tmux: String,
    socket: String,
    tmpdir: Option<PathBuf>,
}

impl TmuxServerGuard {
    pub fn new(tmux: impl Into<String>, socket: impl Into<String>) -> Self {
        Self {
            tmux: tmux.into(),
            socket: socket.into(),
            tmpdir: None,
        }
    }

    /// The `TMUX_TMPDIR` the guarded server's socket lives under, when
    /// it is not the process default.
    pub fn tmpdir(mut self, dir: &Path) -> Self {
        self.tmpdir = Some(dir.to_path_buf());
        self
    }

    fn kill_and_wait(&self) {
        let mut cmd = Command::new(&self.tmux);
        cmd.arg("-L")
            .arg(&self.socket)
            .arg("kill-server")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(dir) = &self.tmpdir {
            cmd.env("TMUX_TMPDIR", dir);
        }
        // No server is already the goal state.
        let _ = cmd.status();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !tmux_pids_on_socket(&self.socket).is_empty() {
            std::thread::sleep(Duration::from_millis(25));
        }
        // Last resort, still scoped to this socket name only.
        for pid in tmux_pids_on_socket(&self.socket) {
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
    }
}

impl Drop for TmuxServerGuard {
    fn drop(&mut self) {
        self.kill_and_wait();
    }
}
