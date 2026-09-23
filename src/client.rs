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
/// for the socket to answer. Reports `already_running` when another
/// process holds the singleton lock, `started` only when the process
/// this call spawned became the daemon.
pub fn daemon_start(state_dir: &Path) -> Result<Value> {
    daemon_start_as(state_dir, None)
}

/// Read bound for one `health` probe while starting — a wedged
/// daemon must not hang `daemon start` for the full rpc timeout.
const START_HEALTH_TIMEOUT: Duration = Duration::from_secs(5);

/// `daemon_start`, forwarding a rollout identity to the child so
/// `daemon run` can prove the caller holds the lease when the binary
/// commit differs from the one last recorded.
///
/// The verdict comes from the singleton lock, never from "the socket
/// answered": only the process holding `cadence.lock` binds the socket,
/// and `health` reports that process's pid. So:
/// - a daemon already answers before we spawn → `already_running`,
///   nothing spawned;
/// - we spawned, and `health` reports our child's pid → `started`;
/// - we spawned, and `health` reports another pid → our child lost the
///   lock (a concurrent start won the race between our check and our
///   spawn). We wait for the child to exit on the lock, then report
///   `already_running`. If the other daemon exits first and our child
///   takes the lock after all, its pid answers and we report `started`.
///
/// The pre-spawn check is a `health` call, not a lock probe: a
/// try-lock-and-release would itself hold the lock for an instant and
/// can make a concurrent start's child fail its non-blocking lock with
/// no daemon left running.
pub fn daemon_start_as(state_dir: &Path, as_identity: Option<&str>) -> Result<Value> {
    use std::time::Instant;
    let forward = crate::rollout::authorize_daemon_spawn(state_dir, as_identity)?;
    if let Ok(health) = rpc_timeout(
        state_dir,
        "health",
        serde_json::json!({}),
        START_HEALTH_TIMEOUT,
    ) {
        return Ok(already_running(state_dir, health));
    }
    let mut child = spawn_daemon_run(state_dir, forward.as_deref().or(as_identity))?;
    let child_pid = u64::from(child.id());
    let mut last_error = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        // This round's `health` answer, when it came from a daemon that
        // is not our child.
        let foreign = match rpc_timeout(
            state_dir,
            "health",
            serde_json::json!({}),
            START_HEALTH_TIMEOUT,
        ) {
            Ok(health) if health["pid"].as_u64() == Some(child_pid) => {
                return Ok(serde_json::json!({
                    "state": "started",
                    "pid": child.id(),
                    "socket": socket_path(state_dir),
                    "health": health,
                }));
            }
            Ok(health) => Some(health),
            Err(e) => {
                last_error = Some(e);
                None
            }
        };
        // Our child exiting means it never became the daemon; the
        // lock holder that answered is the one running. Until one
        // answers, the holder may still be binding the socket.
        let child_exited = child.try_wait()?.is_some();
        let timed_out = Instant::now() >= deadline;
        if child_exited || timed_out {
            if let Some(health) = foreign {
                return Ok(already_running(state_dir, health));
            }
        }
        if timed_out {
            return Err(
                last_error.unwrap_or_else(|| Error::internal("daemon did not answer within 10s"))
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn already_running(state_dir: &Path, health: Value) -> Value {
    serde_json::json!({
        "state": "already_running",
        "socket": socket_path(state_dir),
        "health": health,
    })
}

/// Spawn `daemon run` detached: its own session, output to
/// `daemon.log`, and the rollout identity only as an argument.
fn spawn_daemon_run(state_dir: &Path, identity: Option<&str>) -> Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let exe = std::env::current_exe()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_dir.join("daemon.log"))?;
    let mut command = std::process::Command::new(exe);
    command.env_remove("CADENCE_ROLLOUT_AS");
    command
        .args(["--state-dir"])
        .arg(state_dir)
        .args(["daemon", "run"]);
    if let Some(identity) = identity {
        command.arg("--rollout-as").arg(identity);
    }
    command
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
    Ok(command.spawn()?)
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
