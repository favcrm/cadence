//! One bounded child-process runner. A child's stdout and stderr are
//! drained on reader threads *while* it runs: waiting for exit before
//! reading deadlocks as soon as the output outgrows the pipe, and a
//! host past `fs.pipe-user-pages-soft` hands out 4 KB pipes, so a few
//! KB of `git log` was enough to turn every call into a timeout.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
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
) -> std::thread::JoinHandle<(Vec<u8>, bool)> {
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
        (buf, exceeded)
    })
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
/// stream while still draining the child. The boolean reports whether either
/// stream exceeded the retained bound, allowing callers that expose output to
/// fail closed without buffering unbounded Git or helper output.
pub fn run_bounded_limited(
    cmd: &mut Command,
    timeout: Duration,
    limit: usize,
) -> Result<(Output, bool), BoundedError> {
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
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(Some(status)),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => break Ok(None),
            Err(e) => break Err(e),
        }
    };
    if !matches!(status, Ok(Some(_))) {
        unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
        let _ = child.wait();
    }
    let (stdout, stdout_exceeded) = stdout.join().unwrap_or_default();
    let (stderr, stderr_exceeded) = stderr.join().unwrap_or_default();
    match status {
        Ok(Some(status)) => Ok((
            Output {
                status,
                stdout,
                stderr,
            },
            stdout_exceeded || stderr_exceeded,
        )),
        Ok(None) => Err(BoundedError::TimedOut { stdout, stderr }),
        Err(e) => Err(BoundedError::Wait(e)),
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
}
