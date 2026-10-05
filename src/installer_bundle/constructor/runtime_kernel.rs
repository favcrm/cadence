//! Separate runtime actor factory. Enrollment children and their filter are
//! unchanged. This owns a fixed measured Client executable, not an adopted PID.
//! The runtime actor has threads and the four RW enclaves, never another exec,
//! process fork, namespace/credential/capability change or root authority.
use super::super::{carrier, files, refused, seal, Deadline, Result};
use super::{custody, lifecycle};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::time::{Duration, Instant};
const ALLOW: u32 = 0x7fff0000;
const DENY: u32 = 0x00050000 | libc::EPERM as u32;
fn s(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}
fn eq(k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter {
        code: 0x15,
        jt,
        jf,
        k,
    }
}
fn filter() -> Vec<libc::sock_filter> {
    let mut p = vec![
        s(0x20, 4),
        eq(0xc000003e, 1, 0),
        s(0x06, 0x80000000),
        s(0x20, 0),
        eq(libc::SYS_execveat as u32, 0, 1),
        s(0x06, 0x7ff00000),
    ];
    // clone3 cannot be inspected: ENOSYS makes pthread use inspectable clone.
    p.extend([
        eq(libc::SYS_clone3 as u32, 0, 1),
        s(0x06, 0x00050000 | libc::ENOSYS as u32),
    ]);
    // Only pthread flags, never a new process or namespace. Upper flags zero.
    p.extend([
        eq(libc::SYS_clone as u32, 0, 12),
        s(0x20, 20),
        eq(0, 1, 0),
        s(0x06, DENY),
        s(0x20, 16),
        s(0x54, !(0x003d0f00u32)),
        eq(0, 1, 0),
        s(0x06, DENY),
        s(0x20, 16),
        s(0x54, libc::CLONE_THREAD as u32),
        eq(libc::CLONE_THREAD as u32, 0, 1),
        s(0x06, ALLOW),
        s(0x06, DENY),
        s(0x20, 0),
    ]);
    // AF_UNIX only; no network socket, raw socket, or socketpair process escape.
    for nr in [libc::SYS_socket, libc::SYS_socketpair] {
        p.extend([
            eq(nr as u32, 0, 4),
            s(0x20, 16),
            eq(libc::AF_UNIX as u32, 0, 1),
            s(0x06, ALLOW),
            s(0x06, DENY),
            s(0x20, 0),
        ]);
    }
    // Readbacks only. A sealed process may not remove NNP or change securebits.
    p.extend([
        eq(libc::SYS_prctl as u32, 0, 7),
        s(0x20, 16),
        eq(libc::PR_GET_SECUREBITS as u32, 4, 0),
        eq(libc::PR_GET_KEEPCAPS as u32, 3, 0),
        eq(libc::PR_GET_NO_NEW_PRIVS as u32, 2, 0),
        eq(libc::PR_GET_SECCOMP as u32, 1, 0),
        s(0x06, DENY),
        s(0x06, ALLOW),
        s(0x20, 0),
    ]);
    for nr in [
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_readv,
        libc::SYS_writev,
        libc::SYS_pread64,
        libc::SYS_pwrite64,
        libc::SYS_close,
        libc::SYS_close_range,
        libc::SYS_fstat,
        libc::SYS_newfstatat,
        libc::SYS_statx,
        libc::SYS_lseek,
        libc::SYS_readlink,
        libc::SYS_readlinkat,
        libc::SYS_getdents64,
        libc::SYS_fstatfs,
        libc::SYS_poll,
        libc::SYS_ppoll,
        libc::SYS_select,
        libc::SYS_pselect6,
        libc::SYS_mmap,
        libc::SYS_mprotect,
        libc::SYS_munmap,
        libc::SYS_brk,
        libc::SYS_madvise,
        libc::SYS_mremap,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_sigaltstack,
        libc::SYS_futex,
        libc::SYS_clock_gettime,
        libc::SYS_gettimeofday,
        libc::SYS_nanosleep,
        libc::SYS_clock_nanosleep,
        libc::SYS_getrandom,
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_getppid,
        libc::SYS_getuid,
        libc::SYS_geteuid,
        libc::SYS_getgid,
        libc::SYS_getegid,
        libc::SYS_getresuid,
        libc::SYS_getresgid,
        libc::SYS_getgroups,
        libc::SYS_getrlimit,
        libc::SYS_prlimit64,
        libc::SYS_arch_prctl,
        libc::SYS_set_tid_address,
        libc::SYS_set_robust_list,
        libc::SYS_rseq,
        libc::SYS_exit,
        libc::SYS_exit_group,
        libc::SYS_fcntl,
        libc::SYS_getsockopt,
        libc::SYS_setsockopt,
        libc::SYS_connect,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_accept,
        libc::SYS_accept4,
        libc::SYS_getsockname,
        libc::SYS_getpeername,
        libc::SYS_recvfrom,
        libc::SYS_sendto,
        libc::SYS_recvmsg,
        libc::SYS_sendmsg,
        libc::SYS_shutdown,
        libc::SYS_openat,
        libc::SYS_openat2,
        libc::SYS_fsync,
        libc::SYS_fdatasync,
        libc::SYS_ftruncate,
        libc::SYS_mkdir,
        libc::SYS_mkdirat,
        libc::SYS_unlink,
        libc::SYS_unlinkat,
        libc::SYS_rename,
        libc::SYS_renameat,
        libc::SYS_renameat2,
        libc::SYS_chown,
        libc::SYS_fchown,
        libc::SYS_fchownat,
        libc::SYS_fchmod,
        libc::SYS_fchmodat,
        libc::SYS_chmod,
        libc::SYS_flock,
        libc::SYS_umask,
        libc::SYS_getcwd,
        libc::SYS_chdir,
        libc::SYS_fchdir,
        libc::SYS_pipe2,
        libc::SYS_dup,
        libc::SYS_dup2,
        libc::SYS_dup3,
        libc::SYS_epoll_create1,
        libc::SYS_epoll_ctl,
        libc::SYS_epoll_wait,
        libc::SYS_epoll_pwait,
        libc::SYS_eventfd2,
        libc::SYS_ioctl,
        libc::SYS_sched_yield,
        libc::SYS_sched_getaffinity,
        libc::SYS_getrusage,
        libc::SYS_fgetxattr,
        libc::SYS_utimensat,
    ] {
        p.extend([eq(nr as u32, 0, 1), s(0x06, ALLOW)]);
    }
    p.push(s(0x06, DENY));
    p
}
fn trace(op: libc::c_uint, pid: u32, data: usize) -> Result<()> {
    if unsafe { libc::ptrace(op, pid as i32, 0usize, data) } != 0 {
        return Err(refused());
    }
    Ok(())
}
fn wait(pid: u32, deadline: Deadline) -> Result<i32> {
    loop {
        deadline.check()?;
        let mut st = 0;
        let n = unsafe { libc::waitpid(pid as i32, &mut st, libc::WNOHANG | libc::__WALL) };
        if n == pid as i32 {
            return Ok(st);
        }
        if n < 0 {
            return Err(refused());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn stopped(st: i32, event: i32) -> Result<()> {
    if !libc::WIFSTOPPED(st) || libc::WSTOPSIG(st) != libc::SIGTRAP || st >> 16 != event {
        Err(refused())
    } else {
        Ok(())
    }
}
fn pipe() -> Result<(File, File)> {
    let mut p = [-1; 2];
    if unsafe { libc::pipe2(p.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err(refused());
    }
    Ok(unsafe { (File::from_raw_fd(p[0]), File::from_raw_fd(p[1])) })
}
/// Actual own-created supervisor AND daemon. Private construction fields; no
/// Default, Deserialize, Clone, from_pid, or RuntimeProof conversion exists.
pub(crate) struct OwnedDaemon {
    pid: u32,
    birth: u64,
    pidfd: File,
    executable: files::HeldArtifact,
    namespaces: Vec<File>,
    shared_gid: u32,
    pub(super) control: UnixDatagram,
    logs: [File; 2],
    detached: bool,
    reaped: bool,
}
impl OwnedDaemon {
    pub(super) fn spawn(
        executable: files::HeldArtifact,
        listeners: [RawFd; 2],
        deadline: Deadline,
    ) -> Result<Self> {
        lifecycle::runtime_proof(deadline.0)?.recheck(deadline.0)?;
        if carrier::ids()? != [0; 6]
            || custody::field(&custody::status(std::process::id())?, "Threads:")? != "1"
        {
            return Err(refused());
        }
        executable.recheck(deadline)?;
        // Elected fixed image NSS group, resolved in the single-thread parent.
        // This enables only the shared views/socket DAC, not private Store/root.
        let shared_gid = crate::adapter::pi_guest::topology::resolve_gid_pub(
            crate::adapter::pi_guest::acct::SHARED_GROUP,
        )?;
        if shared_gid == 0 || shared_gid == 21000 {
            return Err(refused());
        }
        let (control, child) = UnixDatagram::pair().map_err(|_| refused())?;
        control.set_nonblocking(true).map_err(|_| refused())?;
        let (stdin, input) = pipe()?;
        let (stdout, output) = pipe()?;
        let (stderr, errors) = pipe()?;
        let exec = executable.fd();
        let mut program = filter();
        // Parent-materialized duplicates prevent FD3/4/5 alias overwrites.
        let mut held = Vec::new();
        for fd in [child.as_raw_fd(), listeners[0], listeners[1], exec] {
            let fd = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
            if fd < 0 {
                return Err(refused());
            }
            held.push(unsafe { File::from_raw_fd(fd) });
        }
        let exec_fd = held[3].as_raw_fd();
        let parent = std::process::id();
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(refused());
        }
        if pid == 0 {
            let run = (|| -> Result<()> {
                if unsafe { libc::ptrace(libc::PTRACE_TRACEME, 0, 0usize, 0usize) } != 0
                    || unsafe { libc::raise(libc::SIGSTOP) } != 0
                {
                    return Err(refused());
                }
                for (src, dst) in [
                    (stdin.as_raw_fd(), 0),
                    (output.as_raw_fd(), 1),
                    (errors.as_raw_fd(), 2),
                    (held[0].as_raw_fd(), 3),
                    (held[1].as_raw_fd(), 4),
                    (held[2].as_raw_fd(), 5),
                ] {
                    if unsafe { libc::dup2(src, dst) } < 0 {
                        return Err(refused());
                    }
                }
                let exec = held[3].as_raw_fd();
                if unsafe { libc::syscall(libc::SYS_close_range, 6u32, (exec - 1) as u32, 0u32) }
                    != 0
                    || unsafe {
                        libc::syscall(libc::SYS_close_range, (exec + 1) as u32, u32::MAX, 0u32)
                    } != 0
                {
                    return Err(refused());
                }
                let mut kernel = seal::Linux;
                let last = kernel.prepare()?;
                if unsafe { libc::setgroups(1, &shared_gid) } != 0
                    || unsafe { libc::setresgid(21000, 21000, 21000) } != 0
                    || unsafe { libc::setresuid(21000, 21000, 21000) } != 0
                    || unsafe { libc::chdir(c"/".as_ptr()) } != 0
                    || unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) } != 0
                    || unsafe { libc::getppid() } != parent as i32
                {
                    return Err(refused());
                }
                kernel.finish(last)?;
                let prog = libc::sock_fprog {
                    len: program.len() as u16,
                    filter: program.as_mut_ptr(),
                };
                if unsafe {
                    libc::prctl(
                        libc::PR_SET_SECCOMP,
                        libc::SECCOMP_MODE_FILTER,
                        &prog as *const _,
                        0,
                        0,
                    )
                } != 0
                {
                    return Err(refused());
                }
                let argv = [c"cadence-owned-runtime-daemon".as_ptr(), std::ptr::null()];
                let env = [std::ptr::null::<libc::c_char>()];
                unsafe {
                    libc::syscall(
                        libc::SYS_execveat,
                        exec,
                        c"".as_ptr(),
                        argv.as_ptr(),
                        env.as_ptr(),
                        libc::AT_EMPTY_PATH,
                    )
                };
                Err(refused())
            })();
            let _ = run;
            unsafe { libc::_exit(125) };
        }
        drop(child);
        drop(stdin);
        drop(input);
        drop(output);
        drop(errors);
        drop(held);
        let p = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0u32) } as i32;
        if p < 0 {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            let _ = wait(
                pid as u32,
                Deadline(Instant::now() + Duration::from_secs(1)),
            );
            return Err(refused());
        }
        let mut actor = Self {
            pid: pid as u32,
            birth: 0,
            pidfd: unsafe { File::from_raw_fd(p) },
            executable,
            namespaces: Vec::new(),
            shared_gid,
            control,
            logs: [stdout, stderr],
            detached: false,
            reaped: false,
        };
        let st = wait(actor.pid, deadline)?;
        if !libc::WIFSTOPPED(st) || libc::WSTOPSIG(st) != libc::SIGSTOP {
            return Err(refused());
        }
        trace(
            libc::PTRACE_SETOPTIONS,
            actor.pid,
            (libc::PTRACE_O_TRACEEXEC | libc::PTRACE_O_TRACESECCOMP | libc::PTRACE_O_EXITKILL)
                as usize,
        )?;
        actor.birth = crate::peer::proc_starttime(actor.pid).ok_or_else(refused)?;
        for name in ["user", "pid", "mnt"] {
            actor
                .namespaces
                .push(File::open(format!("/proc/{}/ns/{name}", actor.pid)).map_err(|_| refused())?)
        }
        trace(libc::PTRACE_CONT, actor.pid, 0)?;
        stopped(wait(actor.pid, deadline)?, libc::PTRACE_EVENT_SECCOMP)?;
        let mut regs: libc::user_regs_struct = unsafe { std::mem::zeroed() };
        if unsafe { libc::ptrace(libc::PTRACE_GETREGS, pid, 0usize, &mut regs as *mut _) } != 0
            || regs.orig_rax != libc::SYS_execveat as u64
            || regs.rdi != exec_fd as u64
            || regs.r10 != libc::AT_EMPTY_PATH as u64
        {
            return Err(refused());
        }
        // The FD came from OUR held executable duplicate; recheck selected inode.
        let fd = File::open(format!("/proc/{pid}/fd/{}", regs.rdi)).map_err(|_| refused())?;
        if files::measure(
            &fd,
            super::pin(&super::context::runtime_parts()?.0.manifest.artifacts.client)?,
            0,
            deadline,
        )? != *actor.executable.selected_stamp()
        {
            return Err(refused());
        }
        unsafe { *libc::__errno_location() = 0 };
        let word = unsafe { libc::ptrace(libc::PTRACE_PEEKDATA, pid, regs.rsi as usize, 0usize) };
        if unsafe { *libc::__errno_location() } != 0 || word as u8 != 0 {
            return Err(refused());
        }
        trace(libc::PTRACE_CONT, actor.pid, 0)?;
        stopped(wait(actor.pid, deadline)?, libc::PTRACE_EVENT_EXEC)?;
        actor.recheck(deadline.0)?;
        Ok(actor)
    }
    pub(super) fn release(&mut self, until: Instant) -> Result<()> {
        self.recheck(until)?;
        // No tracer remains: kernel TRACE on ANY subsequent exec returns ENOSYS.
        // pthread-only clone is deliberately separate from enrollment's filter.
        trace(libc::PTRACE_DETACH, self.pid, 0)?;
        self.detached = true;
        self.recheck(until)
    }
    pub(crate) fn require_peer(&self, stream: &UnixStream, until: Instant) -> Result<()> {
        self.recheck(until)?;
        let mut c: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of_val(&c) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut c as *mut libc::ucred).cast(),
                &mut len,
            )
        } != 0
            || len as usize != std::mem::size_of_val(&c)
            || c.pid != self.pid as i32
            || c.uid != 21000
            || c.gid != 21000
        {
            return Err(refused());
        }
        self.recheck(until)
    }
    pub(super) fn recheck(&self, until: Instant) -> Result<()> {
        let d = Deadline(until);
        lifecycle::runtime_proof(until)?.recheck(until)?;
        let mut poll = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut poll, 1, 0) } != 0
            || crate::peer::proc_starttime(self.pid) != Some(self.birth)
        {
            return Err(refused());
        }
        self.executable.live_correspondence(self.pid, d)?;
        let status = custody::status(self.pid)?;
        for (key, value) in [
            ("Uid:", "21000\t21000\t21000\t21000"),
            ("Gid:", "21000\t21000\t21000\t21000"),
            ("NoNewPrivs:", "1"),
            ("Seccomp:", "2"),
            ("CapInh:", "0000000000000000"),
            ("CapPrm:", "0000000000000000"),
            ("CapEff:", "0000000000000000"),
            ("CapBnd:", "0000000000000000"),
            ("CapAmb:", "0000000000000000"),
        ] {
            if custody::field(&status, key)?.trim() != value {
                return Err(refused());
            }
        }
        let groups = custody::field(&status, "Groups:")?
            .split_ascii_whitespace()
            .map(|s| s.parse::<u32>().map_err(|_| refused()))
            .collect::<Result<Vec<_>>>()?;
        if groups != [self.shared_gid] {
            return Err(refused());
        }
        let tracer = if self.detached { 0 } else { std::process::id() };
        if custody::field(&status, "TracerPid:")? != tracer.to_string() {
            return Err(refused());
        }
        let threads: usize = custody::field(&status, "Threads:")?
            .parse()
            .map_err(|_| refused())?;
        if !(1..=128).contains(&threads) {
            return Err(refused());
        }
        for (name, held) in ["user", "pid", "mnt"].into_iter().zip(&self.namespaces) {
            let now = File::open(format!("/proc/{}/ns/{name}", self.pid)).map_err(|_| refused())?;
            let a = held.metadata().map_err(|_| refused())?;
            let b = now.metadata().map_err(|_| refused())?;
            let parent = File::open(format!("/proc/self/ns/{name}"))
                .map_err(|_| refused())?
                .metadata()
                .map_err(|_| refused())?;
            if (a.dev(), a.ino()) != (b.dev(), b.ino())
                || (a.dev(), a.ino()) != (parent.dev(), parent.ino())
            {
                return Err(refused());
            }
        }
        Ok(())
    }
    pub(super) fn cancel(&self) {
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0u32,
            )
        };
        let d = Deadline(Instant::now() + Duration::from_secs(2));
        while let Ok(st) = wait(self.pid, d) {
            if libc::WIFEXITED(st) || libc::WIFSIGNALED(st) {
                break;
            }
            let _ = trace(libc::PTRACE_CONT, self.pid, libc::SIGKILL as usize);
        }
    }
    pub(super) fn drain_logs(&self) -> Result<()> {
        let mut buf = [0u8; 8192];
        for f in &self.logs {
            let rc = unsafe { libc::read(f.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if rc < 0
                && !matches!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::EAGAIN | libc::EINTR)
                )
            {
                return Err(refused());
            }
        }
        Ok(())
    }
}
impl Drop for OwnedDaemon {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        let d = Deadline(Instant::now() + Duration::from_secs(2));
        while let Ok(st) = wait(self.pid, d) {
            if libc::WIFEXITED(st) || libc::WIFSIGNALED(st) {
                self.reaped = true;
                break;
            }
            let _ = trace(libc::PTRACE_CONT, self.pid, libc::SIGKILL as usize);
        }
    }
}
