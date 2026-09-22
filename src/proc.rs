//! One bounded child-process runner. A child's stdout and stderr are
//! drained on reader threads *while* it runs: waiting for exit before
//! reading deadlocks as soon as the output outgrows the pipe, and a
//! host past `fs.pipe-user-pages-soft` hands out 4 KB pipes, so a few
//! KB of `git log` was enough to turn every call into a timeout.

use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum BoundedError {
    /// The program could not be started.
    Spawn(std::io::Error),
    /// Still running at the deadline — killed and reaped; carries what
    /// it had written by then.
    TimedOut { stdout: Vec<u8>, stderr: Vec<u8> },
    /// Waiting on the child failed.
    Wait(std::io::Error),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputBounds {
    pub stdout_exceeded: bool,
    pub stderr_exceeded: bool,
}

impl std::fmt::Display for BoundedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) | Self::Wait(e) => write!(f, "{e}"),
            Self::TimedOut { .. } => write!(f, "timed out"),
        }
    }
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    })
}

fn drain_limited(
    pipe: Option<impl Read + Send + 'static>,
    limit: usize,
) -> Receiver<(Vec<u8>, bool)> {
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut exceeded = false;
        if let Some(mut pipe) = pipe {
            let mut chunk = [0u8; 8192];
            loop {
                let Ok(read) = pipe.read(&mut chunk) else {
                    break;
                };
                if read == 0 {
                    break;
                }
                let before = buf.len();
                if before < limit {
                    let remaining = limit - before;
                    buf.extend_from_slice(&chunk[..read.min(remaining)]);
                }
                if before.saturating_add(read) > limit {
                    exceeded = true;
                }
            }
        }
        let _ = sender.send((buf, exceeded));
    });
    receiver
}

/// Observe a child without reaping it. Keeping the leader unreaped preserves
/// its process-group id if a descendant keeps a pipe open after the leader
/// exits; the caller can then terminate the group before the final wait.
fn child_exited(pid: libc::pid_t) -> Result<bool, std::io::Error> {
    let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { info.si_pid() } == pid)
}

fn reap_child(pid: libc::pid_t) -> Result<std::process::ExitStatus, std::io::Error> {
    let mut raw_status = 0;
    loop {
        let result = unsafe { libc::waitpid(pid, &mut raw_status, 0) };
        if result == pid {
            return Ok(std::process::ExitStatus::from_raw(raw_status));
        }
        if result == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error);
        }
    }
}

fn kill_process_group(pid: libc::pid_t) {
    // SAFETY: the limited runner only calls this while the child is either
    // unreaped or known to be alive via waitid(WNOWAIT), so its process-group
    // id cannot have been reused for an unrelated process group.
    unsafe { libc::kill(-pid, libc::SIGKILL) };
}

fn receive_limited(
    receiver: &Receiver<(Vec<u8>, bool)>,
    deadline: Instant,
) -> Option<(Vec<u8>, bool)> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    match receiver.recv_timeout(remaining) {
        Ok(result) => Some(result),
        Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => None,
    }
}

/// Run `cmd` to completion or `timeout`, whichever is first. stdin is
/// null, stdout and stderr are captured in full. A non-zero exit is
/// not an error here — it is in the returned `Output::status`. The
/// child leads its own process group and the deadline kills the whole
/// group, so a grandchild holding the pipes cannot stall the readers.
pub fn run_bounded(cmd: &mut Command, timeout: Duration) -> Result<Output, BoundedError> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(BoundedError::Spawn)?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(Some(status)),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => break Ok(None),
            Err(e) => break Err(e),
        }
    };
    if !matches!(status, Ok(Some(_))) {
        // SAFETY: plain syscall; the child is unreaped, so its pid —
        // also its process-group id — cannot have been reused.
        unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
        let _ = child.wait();
    }
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    match status {
        Ok(Some(status)) => Ok(Output {
            status,
            stdout,
            stderr,
        }),
        Ok(None) => Err(BoundedError::TimedOut { stdout, stderr }),
        Err(e) => Err(BoundedError::Wait(e)),
    }
}

/// Like [`run_bounded`], but retain at most `limit` bytes from each output
/// stream while still draining the child. The returned bounds identify which
/// stream exceeded its limit, allowing callers that expose output to fail
/// closed without buffering unbounded Git or helper output.
pub fn run_bounded_limited(
    cmd: &mut Command,
    timeout: Duration,
    limit: usize,
) -> Result<(Output, OutputBounds), BoundedError> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(BoundedError::Spawn)?;
    let stdout = drain_limited(child.stdout.take(), limit);
    let stderr = drain_limited(child.stderr.take(), limit);
    let deadline = Instant::now() + timeout;
    let pid = child.id() as libc::pid_t;
    let mut exited = false;
    let mut wait_error = None;
    while Instant::now() < deadline {
        match child_exited(pid) {
            Ok(true) => {
                exited = true;
                break;
            }
            Ok(false) => std::thread::sleep(Duration::from_millis(5)),
            Err(error) => {
                wait_error = Some(error);
                break;
            }
        }
    }

    let mut stdout_result = None;
    let mut stderr_result = None;
    let mut timed_out = !exited && wait_error.is_none();
    if exited {
        // The leader is deliberately still a zombie here. If either reader
        // misses the same deadline, terminate its group before reaping it.
        stdout_result = receive_limited(&stdout, deadline);
        stderr_result = receive_limited(&stderr, deadline);
        if stdout_result.is_none() || stderr_result.is_none() {
            kill_process_group(pid);
            timed_out = true;
        }
    } else if timed_out {
        kill_process_group(pid);
    }

    if wait_error.is_none() {
        // The leader is either already exited (WNOWAIT) or was killed while
        // still running. In both cases it is safe to reap now.
        let status = reap_child(pid).map_err(BoundedError::Wait)?;
        if timed_out {
            if stdout_result.is_none() {
                stdout_result = stdout.try_recv().ok();
            }
            if stderr_result.is_none() {
                stderr_result = stderr.try_recv().ok();
            }
            let (stdout, _) = stdout_result.unwrap_or_default();
            let (stderr, _) = stderr_result.unwrap_or_default();
            return Err(BoundedError::TimedOut { stdout, stderr });
        }
        let (stdout, stdout_exceeded) = stdout_result.unwrap_or_default();
        let (stderr, stderr_exceeded) = stderr_result.unwrap_or_default();
        return Ok((
            Output {
                status,
                stdout,
                stderr,
            },
            OutputBounds {
                stdout_exceeded,
                stderr_exceeded,
            },
        ));
    }

    let (stdout, _) = stdout_result.unwrap_or_default();
    let (stderr, _) = stderr_result.unwrap_or_default();
    if let Some(error) = wait_error {
        Err(BoundedError::Wait(error))
    } else {
        Err(BoundedError::TimedOut { stdout, stderr })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        cmd
    }

    const MB: usize = 1 << 20;

    #[test]
    fn large_stdout_is_drained() {
        let started = Instant::now();
        let out = run_bounded(
            &mut sh("head -c 1048576 /dev/zero"),
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout.len(), MB);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn large_stderr_is_drained() {
        let started = Instant::now();
        let out = run_bounded(
            &mut sh("head -c 1048576 /dev/zero >&2"),
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stderr.len(), MB);
        assert!(out.stdout.is_empty());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn timeout_kills_and_reaps() {
        let started = Instant::now();
        let err = run_bounded(
            &mut sh("echo $$; exec sleep 30"),
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5));
        let BoundedError::TimedOut { stdout, .. } = err else {
            panic!("expected a timeout, got {err:?}");
        };
        let pid: i32 = String::from_utf8_lossy(&stdout).trim().parse().unwrap();
        // Signal 0 probes existence: a zombie would still answer.
        let probe = unsafe { libc::kill(pid, 0) };
        assert_eq!(probe, -1, "pid {pid} still exists");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[test]
    fn timeout_survives_a_grandchild_holding_the_pipe() {
        let started = Instant::now();
        let err = run_bounded(&mut sh("sleep 30 & wait"), Duration::from_millis(300)).unwrap_err();
        assert!(matches!(err, BoundedError::TimedOut { .. }));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn non_zero_exit_is_reported() {
        let out = run_bounded(&mut sh("echo no >&2; exit 3"), Duration::from_secs(10)).unwrap();
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "no");
    }

    #[test]
    fn missing_program_is_a_spawn_error() {
        let err = run_bounded(
            &mut Command::new("/definitely/missing/prog"),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(matches!(err, BoundedError::Spawn(_)));
    }

    #[test]
    fn limited_output_reports_each_stream_bound() {
        let (out, bounds) = run_bounded_limited(
            &mut sh("head -c 64 /dev/zero; head -c 64 /dev/zero >&2"),
            Duration::from_secs(2),
            8,
        )
        .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout.len(), 8);
        assert_eq!(out.stderr.len(), 8);
        assert!(bounds.stdout_exceeded);
        assert!(bounds.stderr_exceeded);
    }

    #[test]
    fn limited_runner_bounds_a_leader_with_a_pipe_holding_descendant() {
        let started = Instant::now();
        let err = run_bounded_limited(&mut sh("sleep 30 & exit 0"), Duration::from_millis(300), 64)
            .unwrap_err();
        assert!(matches!(err, BoundedError::TimedOut { .. }));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
