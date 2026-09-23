//! CLI-side socket client: one request/response per connection.
//! Long waits (`ask`, `events --follow`) are driven client-side by
//! repeating requests, so a hung call never holds a daemon slot.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::proto;

/// State directory: `$CADENCE_STATE_DIR`, else `$XDG_STATE_HOME/cadence`,
/// else `~/.local/state/cadence`.
pub fn state_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CADENCE_STATE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
        return Ok(PathBuf::from(dir).join("cadence"));
    }
    let home = std::env::var("HOME")
        .map_err(|_| Error::internal("Cannot locate HOME for state directory"))?;
    Ok(PathBuf::from(home).join(".local/state/cadence"))
}

pub fn socket_path(state_dir: &Path) -> PathBuf {
    state_dir.join("cadence.sock")
}

/// The briefing file an actor agent reads —
/// `<state>/briefings/<root>/BRIEFING-<alias>.md` — never inside the
/// agent's cwd repository. The `<root>` segment is the upstream PM's
/// alias when `params` wires one, else the agent's own alias.
pub fn briefing_path(state_dir: &Path, params: &Value, alias: &str) -> PathBuf {
    let root = params["upstream"].as_str().unwrap_or(alias);
    state_dir
        .join("briefings")
        .join(root)
        .join(format!("BRIEFING-{alias}.md"))
}

/// `cadence daemon start`: spawn `<this binary> --state-dir <dir>
/// daemon run` detached (own session, output to `daemon.log`) and wait
/// for the socket to answer. Reports `already_running` when the socket
/// belonged to a pre-existing daemon — our child exited instead.
pub fn daemon_start(state_dir: &Path) -> Result<Value> {
    daemon_start_as(state_dir, None)
}

/// `daemon_start`, forwarding a rollout identity to the child so
/// `daemon run` can prove the caller holds the lease when the binary
/// commit differs from the one last recorded.
pub fn daemon_start_as(state_dir: &Path, as_identity: Option<&str>) -> Result<Value> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    use std::time::Instant;
    let forward = crate::rollout::authorize_daemon_spawn(state_dir, as_identity)?;
    let exe = std::env::current_exe()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_dir.join("daemon.log"))?;
    let mut command = std::process::Command::new(exe);
    if let Some(identity) = forward.as_deref().or(as_identity) {
        command.env("CADENCE_ROLLOUT_AS", identity);
    } else {
        command.env_remove("CADENCE_ROLLOUT_AS");
    }
    command
        .args(["--state-dir"])
        .arg(state_dir)
        .args(["daemon", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    // Wait until the socket answers or the child exits.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match rpc(state_dir, "health", serde_json::json!({})) {
            Ok(health) => {
                // If our child already exited, the socket belongs to a
                // pre-existing daemon — report that honestly.
                if child.try_wait().ok().flatten().is_some() {
                    return Ok(serde_json::json!({
                        "state": "already_running",
                        "socket": socket_path(state_dir),
                        "health": health,
                    }));
                }
                return Ok(serde_json::json!({
                    "state": "started",
                    "pid": child.id(),
                    "socket": socket_path(state_dir),
                    "health": health,
                }));
            }
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(e),
        }
    }
}

/// Send one request, return the result value or the wire error.
pub fn rpc(state_dir: &Path, method: &str, params: Value) -> Result<Value> {
    rpc_timeout(state_dir, method, params, Duration::from_secs(700))
}

/// `rpc` with a caller-chosen read bound — best-effort callers
/// (status footer, doctor) must degrade in a second or two rather
/// than hang a screen on a wedged daemon.
pub fn rpc_timeout(
    state_dir: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    let socket = socket_path(state_dir);
    let mut stream = UnixStream::connect(&socket).map_err(|_| {
        Error::internal(format!(
            "Daemon is not reachable at {} — start it with `cadence daemon start`",
            socket.display()
        ))
    })?;
    stream.set_read_timeout(Some(timeout))?;
    let request = proto::request(method, params);
    writeln!(stream, "{request}")?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let frame: Value = serde_json::from_str(&line)
        .map_err(|_| Error::internal("Daemon returned a malformed response"))?;
    proto::unwrap(frame)
}
