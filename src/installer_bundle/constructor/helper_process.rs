//! Root-only own-creating helper family. clone3 creates the actual PID1 and
//! pidfd atomically; no caller PID, process-group guess or observed descendant
//! list creates ownership. Killing/reaping this namespace init retires ALL its
//! descendants (including nested PID namespaces) by the kernel's init-exit rule.
use super::super::{files, refused, Deadline, Result};
use std::ffi::CString;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::time::{Duration, Instant};
#[repr(C)]
struct CloneArgs {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
    set_tid: u64,
    set_tid_size: u64,
    cgroup: u64,
}
pub(super) struct Process {
    pid: u32,
    pidfd: File,
    pub stdin: Option<File>,
    pub stdout: Option<File>,
    pub stderr: Option<File>,
}
fn pipe() -> Result<(File, File)> {
    let mut p = [-1; 2];
    if unsafe { libc::pipe2(p.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(refused());
    }
    Ok(unsafe { (File::from_raw_fd(p[0]), File::from_raw_fd(p[1])) })
}
fn wait(pid: u32, deadline: Deadline, event: i32) -> Result<()> {
    loop {
        deadline.check()?;
        let mut st = 0;
        let n = unsafe { libc::waitpid(pid as i32, &mut st, libc::WNOHANG | libc::__WALL) };
        if n < 0 {
            return Err(refused());
        }
        if n > 0 {
            if !libc::WIFSTOPPED(st)
                || libc::WSTOPSIG(st)
                    != if event == 0 {
                        libc::SIGSTOP
                    } else {
                        libc::SIGTRAP
                    }
                || st >> 16 != event
            {
                return Err(refused());
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
impl Process {
    pub(super) fn spawn(
        artifact: &files::HeldArtifact,
        strings: &[CString],
        launch_gid: u32,
        deadline: Deadline,
    ) -> Result<Self> {
        if super::custody::field(&super::custody::status(std::process::id())?, "Threads:")? != "1" {
            return Err(refused());
        }
        artifact.recheck(deadline)?;
        let argv: Vec<usize> = strings
            .iter()
            .map(|s| s.as_ptr() as usize)
            .chain(std::iter::once(0))
            .collect();
        let mut held = Vec::new();
        let mut ends = Vec::new();
        for _ in 0..3 {
            let (r, w) = pipe()?;
            ends.push(r);
            ends.push(w);
        }
        // Parent-materialized duplicates avoid overwriting source descriptors.
        for fd in [
            ends[0].as_raw_fd(),
            ends[3].as_raw_fd(),
            ends[5].as_raw_fd(),
            artifact.fd(),
        ] {
            let n = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
            if n < 0 {
                return Err(refused());
            }
            held.push(unsafe { File::from_raw_fd(n) });
        }
        let mut pidfd = -1i32;
        let args = CloneArgs {
            flags: (libc::CLONE_NEWPID | libc::CLONE_PIDFD) as u64,
            pidfd: (&mut pidfd as *mut i32) as u64,
            child_tid: 0,
            parent_tid: 0,
            exit_signal: libc::SIGCHLD as u64,
            stack: 0,
            stack_size: 0,
            tls: 0,
            set_tid: 0,
            set_tid_size: 0,
            cgroup: 0,
        };
        let pid =
            unsafe { libc::syscall(libc::SYS_clone3, &args, std::mem::size_of::<CloneArgs>()) };
        if pid < 0 {
            return Err(refused());
        } // No fork/process-group fallback.
        if pid == 0 {
            unsafe {
                if libc::getpid() != 1
                    || libc::getppid() != 0
                    || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                    || libc::getppid() != 0
                    || libc::ptrace(libc::PTRACE_TRACEME, 0, 0usize, 0usize) != 0
                    || libc::raise(libc::SIGSTOP) != 0
                {
                    libc::_exit(125)
                }
                for (i, f) in held[..3].iter().enumerate() {
                    if libc::dup2(f.as_raw_fd(), i as i32) < 0 {
                        libc::_exit(125)
                    }
                }
                if libc::dup3(held[3].as_raw_fd(), 7, libc::O_CLOEXEC) < 0
                    || libc::syscall(libc::SYS_close_range, 3u32, 6u32, 0u32) != 0
                    || libc::syscall(libc::SYS_close_range, 8u32, u32::MAX, 0u32) != 0
                    || libc::setsid() < 0
                    || libc::setgroups(1, &launch_gid) != 0
                    || libc::setresgid(21000, 0, 0) != 0
                    || libc::setresuid(21000, 0, 0) != 0
                    || libc::chdir(c"/".as_ptr()) != 0
                    || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                {
                    libc::_exit(125)
                }
                let env = [std::ptr::null::<libc::c_char>()];
                libc::syscall(
                    libc::SYS_execveat,
                    7,
                    c"".as_ptr(),
                    argv.as_ptr().cast::<*const libc::c_char>(),
                    env.as_ptr(),
                    libc::AT_EMPTY_PATH,
                );
                libc::_exit(125)
            }
        }
        if pidfd < 0 {
            // Own creation succeeded but atomic pidfd is absent: UNKNOWN.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            return Err(refused());
        }
        let mut process = Self {
            pid: pid as u32,
            pidfd: unsafe { File::from_raw_fd(pidfd) },
            stdin: None,
            stdout: None,
            stderr: None,
        };
        process.stderr = Some(ends.remove(4));
        process.stdout = Some(ends.remove(2));
        process.stdin = Some(ends.remove(1));
        drop(ends);
        drop(held);
        wait(process.pid, deadline, 0)?;
        if unsafe {
            libc::ptrace(
                libc::PTRACE_SETOPTIONS,
                process.pid as i32,
                0usize,
                (libc::PTRACE_O_TRACEEXEC | libc::PTRACE_O_EXITKILL) as usize,
            )
        } != 0
        {
            return Err(refused());
        }
        process.release()?;
        wait(process.pid, deadline, libc::PTRACE_EVENT_EXEC)?;
        artifact.live_correspondence(process.pid, deadline)?;
        Ok(process) // Remains stopped before helper's first instruction.
    }
    pub(super) fn id(&self) -> u32 {
        self.pid
    }
    pub(super) fn pidfd(&self) -> Result<File> {
        self.pidfd.try_clone().map_err(|_| refused())
    }
    pub(super) fn release(&self) -> Result<()> {
        if unsafe { libc::ptrace(libc::PTRACE_CONT, self.pid as i32, 0usize, 0usize) } != 0 {
            return Err(refused());
        }
        Ok(())
    }
    pub(super) fn kill(&self) {
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0u32,
            )
        };
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.kill();
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until {
            let mut st = 0;
            let n =
                unsafe { libc::waitpid(self.pid as i32, &mut st, libc::WNOHANG | libc::__WALL) };
            if n < 0 || (n > 0 && (libc::WIFEXITED(st) || libc::WIFSIGNALED(st))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
