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

/// State directory: `$CADENCE_STATE_DIR`, else the resolved home's
/// `state/` — `$XDG_STATE_HOME/cadence` or `~/.local/state/cadence`
/// under the legacy layout ([`crate::home`]).
pub fn state_dir() -> Result<PathBuf> {
    crate::home::state_dir()
}

/// The state directory `state_dir` resolves with `CADENCE_STATE_DIR`
/// unset — the production default a sandbox must never reach.
pub fn default_state_dir() -> Result<PathBuf> {
    crate::home::state_default()
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
/// Every return other than `started` — `already_running` or an error —
/// first makes sure the child this call spawned is gone (see
/// [`SpawnedDaemon`]): a child still alive then has not reached the
/// lock yet and would take it later, once the daemon we reported exits.
/// Only that child is ever signalled, by its `Child` handle.
///
/// The pre-spawn check is a `health` call, not a lock probe: a
/// try-lock-and-release would itself hold the lock for an instant and
/// can make a concurrent start's child fail its non-blocking lock with
/// no daemon left running.
pub fn daemon_start_as(state_dir: &Path, as_identity: Option<&str>) -> Result<Value> {
    use std::time::Instant;
    let forward = crate::rollout::authorize_daemon_spawn(state_dir, as_identity)?;
    if !precheck_forced_to_fail() {
        if let Ok(health) = rpc_timeout(
            state_dir,
            "health",
            serde_json::json!({}),
            START_HEALTH_TIMEOUT,
        ) {
            return Ok(already_running(state_dir, health));
        }
    }
    let mut child = SpawnedDaemon::new(spawn_daemon_run(
        state_dir,
        forward.as_deref().or(as_identity),
    )?);
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
                child.keep();
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

/// Debug/test builds only: `CADENCE_TEST_START_PRECHECK_FAILS=1` makes
/// `daemon start` treat its pre-spawn `health` check as failed — as when
/// it times out under load while a daemon is live — so it spawns a child
/// anyway. Release builds never read it.
fn precheck_forced_to_fail() -> bool {
    cfg!(debug_assertions)
        && std::env::var_os("CADENCE_TEST_START_PRECHECK_FAILS").is_some_and(|v| v == "1")
}

/// How long a spawned child that did not become the daemon gets to
/// exit on the singleton lock by itself before it is sent SIGTERM.
const CHILD_EXIT_GRACE: Duration = Duration::from_secs(3);
/// How long after SIGTERM before SIGKILL.
const CHILD_TERM_WAIT: Duration = Duration::from_secs(2);

/// The `daemon run` child one `daemon start` spawned. Unless [`keep`]
/// was called (the `started` path), dropping it ends the child and
/// reaps it: wait for it to exit on the lock, else SIGTERM, else
/// SIGKILL — all bounded. It signals only this child, through its
/// handle; the child is unreaped until then, so its pid cannot be
/// reused.
///
/// [`keep`]: SpawnedDaemon::keep
struct SpawnedDaemon {
    child: std::process::Child,
    keep: bool,
}

impl SpawnedDaemon {
    fn new(child: std::process::Child) -> Self {
        Self { child, keep: false }
    }

    fn id(&self) -> u32 {
        self.child.id()
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    /// The child became the daemon: leave it running.
    fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for SpawnedDaemon {
    fn drop(&mut self) {
        if !self.keep {
            end_child(&mut self.child, CHILD_EXIT_GRACE, CHILD_TERM_WAIT);
        }
    }
}

/// Make sure `child` has exited and is reaped: wait up to `grace` for
/// it to exit by itself, then SIGTERM it and wait up to `term_wait`,
/// then SIGKILL it and wait.
fn end_child(child: &mut std::process::Child, grace: Duration, term_wait: Duration) {
    if exited_within(child, grace) {
        return;
    }
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: plain kill(2) on our own unreaped child's pid.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    if exited_within(child, term_wait) {
        return;
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Poll `child` until it has exited (reaping it) or `bound` passes.
fn exited_within(child: &mut std::process::Child, bound: Duration) -> bool {
    let deadline = std::time::Instant::now() + bound;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            // Not waitable: there is nothing left of it to end.
            Err(_) => return true,
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
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
    // CAD-482: a daemon is never a caller — a restart under
    // `CADENCE_TEST_AS` must not let the child assert on its own
    // outbound RPCs. The child's `daemon run` re-arms from the state
    // dir's minted token instead.
    command.env_remove(crate::test_seam::AS_ENV);
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
    Ok(crate::reaper::spawn(&mut command)?)
}

/// Send one request, return the result value or the wire error.
pub fn rpc(state_dir: &Path, method: &str, params: Value) -> Result<Value> {
    rpc_timeout(state_dir, method, params, Duration::from_secs(700))
}

/// CAD-447: hand an answer report just filed on `issue` to the daemon,
/// which queues it to the author of the question it answers. The
/// answer stands whatever happens here: the daemon's reply, or its
/// refusal as `{"sent": false, "error"}`, is returned for the caller to
/// show.
pub fn route_answer(state_dir: &Path, issue: &str, report: &str) -> Value {
    let params = serde_json::json!({"issue": issue, "report": report});
    match rpc_timeout(state_dir, "answer_route", params, Duration::from_secs(10)) {
        Ok(v) => v,
        Err(e) => serde_json::json!({"sent": false, "error": e.to_string()}),
    }
}

/// Why an [`route_answer`] reply did not tell the asker — `None` when it
/// was sent now or earlier (a duplicate).
pub fn answer_not_told(route: &Value) -> Option<String> {
    if route["sent"] == true || route["duplicate"] == true {
        return None;
    }
    let why = ["why", "undeliverable", "error"]
        .iter()
        .find_map(|k| route[*k].as_str())
        .unwrap_or("no reason given");
    Some(match route["to"].as_str() {
        Some(to) => format!("{to}: {why}"),
        None => why.to_string(),
    })
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
    proto::unwrap(rpc_frame(state_dir, method, params, timeout)?)
}

/// `rpc` that tells the two failures apart: the outer `Err` is the
/// transport — no daemon at the socket, an I/O error, a malformed
/// frame — and the inner result is the daemon's own answer, a refusal
/// included (CAD-384: `daemon restart` must not read a caller-rule
/// refusal of `shutdown` as "not running").
pub fn rpc_answer(state_dir: &Path, method: &str, params: Value) -> Result<Result<Value>> {
    Ok(proto::unwrap(rpc_frame(
        state_dir,
        method,
        params,
        Duration::from_secs(700),
    )?))
}

/// One request/response frame over the daemon socket.
fn rpc_frame(state_dir: &Path, method: &str, params: Value, timeout: Duration) -> Result<Value> {
    let socket = socket_path(state_dir);
    let mut stream = UnixStream::connect(&socket).map_err(|_| {
        Error::internal(format!(
            "Daemon is not reachable at {} — start it with `cadence daemon start`",
            socket.display()
        ))
    })?;
    stream.set_read_timeout(Some(timeout))?;
    let mut request = proto::request(method, params);
    // CAD-482: a test-seam caller asserts its identity on the frame —
    // scoped in-process ([`crate::test_seam::scoped`]) or via
    // `CADENCE_TEST_AS` in a spawned test binary. Absent the feature
    // this attaches nothing.
    if let Some(test_caller) = crate::test_seam::caller_frame(state_dir)? {
        request[crate::test_seam::FRAME_FIELD] = test_caller;
    }
    writeln!(stream, "{request}")?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(|_| Error::internal("Daemon returned a malformed response"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    #[test]
    fn end_child_waits_for_a_child_that_exits_by_itself() {
        let mut child = Command::new("sh").args(["-c", "exit 7"]).spawn().unwrap();
        end_child(&mut child, Duration::from_secs(10), Duration::from_secs(10));
        let status = child.try_wait().unwrap().expect("reaped");
        assert_eq!(status.code(), Some(7), "{status:?}");
    }

    #[test]
    fn end_child_terms_a_child_still_running_after_the_grace() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let begin = Instant::now();
        end_child(
            &mut child,
            Duration::from_millis(200),
            Duration::from_secs(10),
        );
        let status = child.try_wait().unwrap().expect("reaped");
        assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
        assert!(begin.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn end_child_kills_a_child_that_ignores_sigterm() {
        let mut child = Command::new("sh")
            .args(["-c", "trap '' TERM; echo ready; exec sleep 30"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready.trim(), "ready");
        end_child(
            &mut child,
            Duration::from_millis(100),
            Duration::from_millis(300),
        );
        let status = child.try_wait().unwrap().expect("reaped");
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
    }

    #[test]
    fn a_kept_spawned_daemon_is_left_running() {
        let mut spawned = SpawnedDaemon::new(Command::new("sleep").arg("30").spawn().unwrap());
        spawned.keep();
        let pid = libc::pid_t::try_from(spawned.id()).unwrap();
        let begin = Instant::now();
        drop(spawned);
        assert!(begin.elapsed() < CHILD_EXIT_GRACE);
        // Still ours and unreaped: signal 0 finds it, and it is no zombie.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let running = stat
            .rsplit(')')
            .next()
            .is_some_and(|rest| !rest.trim_start().starts_with('Z'));
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
        assert!(
            alive && running,
            "kept child {pid} should still run: {stat}"
        );
    }
}
