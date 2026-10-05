//! Fixed ROOT-owned helper creation. No PID adoption/command/environment/FD
//! number API; selected argv is derived only from an opaque launch permit.
use super::super::{files, refused, Deadline, Result};
use super::{custody, lifecycle};
use crate::adapter::pi_guest::owner::LaunchPermit;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};
#[derive(Clone, Copy)]
pub(crate) enum HelperPhase {
    Privileged,
    Sealed,
}
pub(crate) struct HelperStdio {
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
    pub stderr: ChildStderr,
}
/// Non-Clone actual own-created child, selected executable, pidfd and birth.
/// SO_PEERCRED is checked against THIS retained handle, not supplied metadata.
pub(crate) struct OwnedHelper {
    child: Child,
    pidfd: File,
    birth: u64,
    artifact: files::HeldArtifact,
    guest: u32,
    guest_gid: u32,
    parent: u32,
}
impl OwnedHelper {
    pub(crate) fn spawn(permit: &LaunchPermit, until: Instant) -> Result<(Self, HelperStdio)> {
        let deadline = Deadline(until.min(Instant::now() + Duration::from_secs(30)));
        lifecycle::runtime_proof(until)?.recheck(until)?;
        permit.recheck()?;
        let selected = permit.describe();
        selected
            .validate(&selected.selection)
            .map_err(|_| refused())?;
        let artifact = files::HeldArtifact::open(
            files::Artifact::Helper,
            selected.image.helper_sha256,
            deadline,
        )?;
        let routing = crate::protected_pi_profile::Routing::for_agent(
            selected.selection.role == crate::protected_pi_profile::authority::Role::Master,
            &selected.selection.model,
        )
        .map_err(|_| refused())?;
        let mut args = vec![
            "exec".to_owned(),
            "--profile".into(),
            "pi-guest".into(),
            format!("--alias-sha256={}", selected.selection.alias_sha256),
            format!("--generation={}", selected.selection.generation),
            "--".into(),
        ];
        args.extend(routing.tokens());
        let strings: Vec<std::ffi::CString> = std::iter::once("cadence-agent-exec".to_owned())
            .chain(args)
            .map(|s| std::ffi::CString::new(s).map_err(|_| refused()))
            .collect::<Result<_>>()?;
        let argv: Vec<usize> = strings
            .iter()
            .map(|s| s.as_ptr() as usize)
            .chain(std::iter::once(0))
            .collect();
        let fd = artifact.fd();
        let parent = std::process::id();
        // pre_exec owns all backing strings AND parent-materialized pointer
        // arrays. No allocation occurs after fork; never caller memory.
        let mut command = Command::new("/opt/protected/bin/cadence-agent-exec");
        command
            .env_clear()
            .current_dir("/")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(move || {
                let fail = || std::io::Error::last_os_error();
                if libc::setsid() < 0
                    || libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setresgid(21000, 0, 0) != 0
                    || libc::setresuid(21000, 0, 0) != 0
                    || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                    || libc::getppid() != parent as i32
                {
                    return Err(fail());
                }
                if fd != 7 && libc::dup3(fd, 7, libc::O_CLOEXEC) < 0 {
                    return Err(fail());
                }
                if libc::syscall(libc::SYS_close_range, 3u32, 6u32, 0u32) != 0
                    || libc::syscall(libc::SYS_close_range, 8u32, u32::MAX, 0u32) != 0
                {
                    return Err(fail());
                }
                let _keep_backings = &strings;
                let env = [std::ptr::null::<libc::c_char>()];
                libc::syscall(
                    libc::SYS_execveat,
                    7,
                    c"".as_ptr(),
                    argv.as_ptr().cast::<*const libc::c_char>(),
                    env.as_ptr(),
                    libc::AT_EMPTY_PATH,
                );
                Err(fail())
            });
        }
        let mut child = crate::reaper::spawn(&mut command).map_err(|_| refused())?;
        let pid = child.id();
        let setup = (|| {
            let birth = crate::peer::proc_starttime(pid).ok_or_else(refused)?;
            let p = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0u32) };
            if p < 0 {
                return Err(refused());
            }
            let pidfd = unsafe { File::from_raw_fd(p as i32) };
            // Helper cannot pass Arm while the root dispatcher is still
            // registering it. Keep an exit-kill tracer BEFORE any authorization.
            if unsafe {
                libc::ptrace(
                    libc::PTRACE_SEIZE,
                    pid as libc::pid_t,
                    std::ptr::null_mut::<libc::c_void>(),
                    (libc::PTRACE_O_TRACEEXEC | libc::PTRACE_O_EXITKILL) as usize,
                )
            } != 0
            {
                return Err(refused());
            }
            artifact.live_correspondence(pid, deadline)?;
            lifecycle::runtime_proof(until)?.recheck(until)?;
            permit.recheck()?;
            let stdio = HelperStdio {
                stdin: child.stdin.take().ok_or_else(refused)?,
                stdout: child.stdout.take().ok_or_else(refused)?,
                stderr: child.stderr.take().ok_or_else(refused)?,
            };
            Ok((pidfd, birth, stdio))
        })();
        match setup {
            Ok((pidfd, birth, stdio)) => Ok((
                Self {
                    child,
                    pidfd,
                    birth,
                    artifact,
                    guest: selected.guest,
                    guest_gid: selected.guest_gid,
                    parent,
                },
                stdio,
            )),
            Err(e) => {
                let _ = child.kill();
                let until = Instant::now() + Duration::from_secs(2);
                while Instant::now() < until {
                    let mut status = 0;
                    let rc = unsafe {
                        libc::waitpid(child.id() as i32, &mut status, libc::WNOHANG | libc::__WALL)
                    };
                    if rc < 0 || (rc > 0 && (libc::WIFEXITED(status) || libc::WIFSIGNALED(status)))
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e)
            }
        }
    }
    /// Continue ONLY the authenticated selected Node exec after durable consume.
    /// Unexpected trace stops/exit refuse; no caller exec/address is accepted.
    pub(crate) fn release_node(&self, permit: &LaunchPermit, until: Instant) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let deadline = Deadline(until);
        permit.recheck()?;
        loop {
            deadline.check()?;
            let mut status = 0;
            let rc = unsafe {
                libc::waitpid(
                    self.child.id() as i32,
                    &mut status,
                    libc::WNOHANG | libc::__WALL,
                )
            };
            if rc < 0 {
                return Err(refused());
            }
            if rc == 0 {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            if !libc::WIFSTOPPED(status)
                || libc::WSTOPSIG(status) != libc::SIGTRAP
                || (status >> 16) != libc::PTRACE_EVENT_EXEC
            {
                return Err(refused());
            }
            let live =
                File::open(format!("/proc/{}/exe", self.child.id())).map_err(|_| refused())?;
            let selected =
                File::open(crate::protected_pi_profile::NODE_PATH).map_err(|_| refused())?;
            let m = live.metadata().map_err(|_| refused())?;
            let s = selected.metadata().map_err(|_| refused())?;
            if (m.dev(), m.ino(), m.size()) != (s.dev(), s.ino(), s.size())
                || m.uid() != 0
                || m.gid() != 0
                || m.mode() & 0o7777 != 0o755
                || m.nlink() != 1
                || m.size() > 128 * 1024 * 1024
                || crate::adapter::pi_guest::execfd::sha256_fd(&live, m.size())
                    .map_err(|_| refused())?
                    != permit.describe().image.node_sha256
            {
                return Err(refused());
            }
            let text = custody::status(self.child.id())?;
            if custody::field(&text, "NoNewPrivs:")? != "1" {
                return Err(refused());
            }
            for cap in ["CapInh:", "CapPrm:", "CapEff:", "CapBnd:", "CapAmb:"] {
                if custody::field(&text, cap)? != "0000000000000000" {
                    return Err(refused());
                }
            }
            permit.recheck()?;
            lifecycle::runtime_proof(until)?.recheck(until)?;
            if unsafe {
                libc::ptrace(
                    libc::PTRACE_CONT,
                    self.child.id() as i32,
                    std::ptr::null_mut::<libc::c_void>(),
                    std::ptr::null_mut::<libc::c_void>(),
                )
            } != 0
            {
                return Err(refused());
            }
            return deadline.check();
        }
    }
    pub(crate) fn require_peer(
        &self,
        stream: &UnixStream,
        phase: HelperPhase,
        until: Instant,
    ) -> Result<()> {
        let deadline = Deadline(until);
        lifecycle::runtime_proof(until)?.recheck(until)?;
        let mut ready = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut ready, 1, 0) } != 0
            || crate::peer::proc_starttime(self.child.id()) != Some(self.birth)
        {
            return Err(refused());
        }
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        } != 0
            || len as usize != std::mem::size_of::<libc::ucred>()
            || cred.pid != self.child.id() as i32
            || cred.uid != 0
            || cred.gid != 0
        {
            return Err(refused());
        }
        self.artifact
            .live_correspondence(self.child.id(), deadline)?;
        let text = custody::status(self.child.id())?;
        let ids = |key: &str| -> Result<Vec<u32>> {
            custody::field(&text, key)?
                .split_ascii_whitespace()
                .map(|s| s.parse().map_err(|_| refused()))
                .collect()
        };
        if custody::field(&text, "TracerPid:")?
            .parse::<u32>()
            .map_err(|_| refused())?
            != self.parent
        {
            return Err(refused());
        }
        match phase {
            HelperPhase::Privileged => {
                if ids("Uid:")? != [21000, 0, 0, 0] || ids("Gid:")? != [21000, 0, 0, 0] {
                    return Err(refused());
                }
            }
            HelperPhase::Sealed => {
                if ids("Uid:")? != [self.guest; 4]
                    || ids("Gid:")? != [self.guest_gid; 4]
                    || custody::field(&text, "NoNewPrivs:")? != "1"
                {
                    return Err(refused());
                }
                for cap in ["CapInh:", "CapPrm:", "CapEff:", "CapBnd:", "CapAmb:"] {
                    if custody::field(&text, cap)? != "0000000000000000" {
                        return Err(refused());
                    }
                }
            }
        }
        deadline.check()
    }
}
impl Drop for OwnedHelper {
    fn drop(&mut self) {
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0u32,
            );
        }
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until {
            let mut status = 0;
            let n = unsafe {
                libc::waitpid(
                    self.child.id() as i32,
                    &mut status,
                    libc::WNOHANG | libc::__WALL,
                )
            };
            if n < 0 || (n > 0 && (libc::WIFEXITED(status) || libc::WIFSIGNALED(status))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
