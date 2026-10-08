//! Owned Linux child construction. No caller PID, command, path, argv, env,
//! descriptor number or diagnostic JSON is an input. The constructor remains
//! the tracer for the entire child lifetime; its death kills the traced child.
use super::super::{carrier, files, refused, seal, Deadline, Result};
use super::channel;
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

const OWNER_FD: RawFd = 3;
const AUDIT_ARCH_X86_64: u32 = 0xc000003e;
const ALLOW: u32 = 0x7fff0000;
const TRACE: u32 = 0x7ff00000;
const KILL: u32 = 0x80000000;
const DENY: u32 = 0x00050000 | libc::EPERM as u32;

#[derive(Clone, Copy)]
pub(super) enum Principal {
    Installer,
    Recipient,
}
impl Principal {
    fn id(self) -> u32 {
        match self {
            Self::Installer => 21000,
            Self::Recipient => 21001,
        }
    }
}

fn statement(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}
fn equal(k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter {
        code: 0x15,
        jt,
        jf,
        k,
    }
}

/// Default-deny x86_64 filter. In particular no fork/clone, ptrace, process_vm,
/// setns/unshare, mount, credential change, socket creation, SCM_RIGHTS or
/// seccomp reconfiguration. exec is intercepted, NEVER directly allowed.
/// Read-only filesystem operations permit existing measurement mechanics;
/// the elected immutable mount policy remains a separate prerequisite.
fn filter() -> Vec<libc::sock_filter> {
    let mut out = vec![
        statement(0x20, 4), // seccomp_data.arch
        equal(AUDIT_ARCH_X86_64, 1, 0),
        statement(0x06, KILL),
        statement(0x20, 0), // seccomp_data.nr; x32 numbers fail the allowlist
        equal(libc::SYS_execve as u32, 0, 1),
        statement(0x06, TRACE),
        equal(libc::SYS_execveat as u32, 0, 1),
        statement(0x06, TRACE),
    ];
    // Needed by the static Rust runtime, verify-only ring and finite private
    // stream adapter. No writable open: openat flags checked below. openat2 is
    // intentionally absent until its fixed caller is converted to held FDs.
    for nr in [
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_close,
        libc::SYS_fstat,
        libc::SYS_newfstatat,
        libc::SYS_lseek,
        libc::SYS_pread64,
        libc::SYS_readlink,
        libc::SYS_readlinkat,
        libc::SYS_fgetxattr,
        libc::SYS_getdents64,
        libc::SYS_fstatfs,
        libc::SYS_poll,
        libc::SYS_ppoll,
        libc::SYS_mmap,
        libc::SYS_mprotect,
        libc::SYS_munmap,
        libc::SYS_brk,
        libc::SYS_madvise,
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
    ] {
        out.push(equal(nr as u32, 0, 1));
        out.push(statement(0x06, ALLOW));
    }
    // Fixed readback/duplex setup only, never credential/securebits mutation.
    out.extend([
        equal(libc::SYS_prctl as u32, 0, 9),
        statement(0x20, 16), // args[0]
        equal(libc::PR_GET_SECUREBITS as u32, 5, 0),
        equal(libc::PR_GET_KEEPCAPS as u32, 4, 0),
        equal(libc::PR_GET_NO_NEW_PRIVS as u32, 3, 0),
        equal(libc::PR_GET_SECCOMP as u32, 2, 0),
        statement(0x06, DENY),
        statement(0x06, DENY),
        statement(0x06, ALLOW),
        statement(0x06, DENY),
        statement(0x20, 0),
    ]);
    for nr in [libc::SYS_fcntl, libc::SYS_getsockopt] {
        out.push(equal(nr as u32, 0, 1));
        out.push(statement(0x06, ALLOW));
    }
    out.extend([
        equal(libc::SYS_openat as u32, 0, 4),
        statement(0x20, 32), // args[2] low word: flags
        statement(
            0x54,
            !(libc::O_CLOEXEC
                | libc::O_NONBLOCK
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_PATH) as u32,
        ),
        equal(0, 0, 1),
        statement(0x06, ALLOW),
        statement(0x06, DENY),
    ]);
    out
}
fn install_filter(program: &mut [libc::sock_filter]) -> Result<()> {
    let mut p = libc::sock_fprog {
        len: u16::try_from(program.len()).map_err(|_| refused())?,
        filter: program.as_mut_ptr(),
    };
    if unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            2,
            &mut p as *mut libc::sock_fprog,
            0,
            0,
        )
    } != 0
    {
        return Err(refused());
    }
    Ok(())
}

fn trace(request: libc::c_uint, pid: u32, data: usize) -> Result<()> {
    if unsafe { libc::ptrace(request, pid as libc::pid_t, 0usize, data) } != 0 {
        return Err(refused());
    }
    Ok(())
}
fn wait(pid: u32, deadline: Deadline) -> Result<i32> {
    loop {
        deadline.check()?;
        let mut status = 0;
        let n = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
        if n == pid as libc::pid_t {
            return Ok(status);
        }
        if n < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return Err(refused());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn stop(status: i32, event: i32) -> Result<()> {
    if !libc::WIFSTOPPED(status) || libc::WSTOPSIG(status) != libc::SIGTRAP || status >> 16 != event
    {
        return Err(refused());
    }
    Ok(())
}
fn pipes() -> Result<(File, File)> {
    let mut pair = [-1; 2];
    if unsafe { libc::pipe2(pair.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err(refused());
    }
    Ok(unsafe { (File::from_raw_fd(pair[0]), File::from_raw_fd(pair[1])) })
}

/// Holds the actual PID identity, executable inode, namespace handles, private
/// duplex endpoint and every stdio peer. Never Clone; no public constructor.
/// Killing/reaping this object is physical cleanup ONLY, not owner retirement.
pub(super) struct HeldChild {
    pid: u32,
    starttime: u64,
    pidfd: File,
    executable: files::HeldArtifact,
    namespaces: Vec<File>,
    owner: UnixStream,
    input: File,
    output: File,
    errors: File,
    principal: Principal,
    reaped: bool,
    exec_completed: bool,
}
impl HeldChild {
    pub(super) fn pid(&self) -> u32 {
        self.pid
    }
    pub(super) fn starttime(&self) -> u64 {
        self.starttime
    }
    pub(super) fn owner(&self) -> &UnixStream {
        &self.owner
    }
    pub(super) fn resume(&mut self, deadline: Deadline) -> Result<()> {
        if !self.exec_completed {
            return Err(refused());
        }
        self.corroborate(deadline)?;
        trace(libc::PTRACE_CONT, self.pid, 0)
    }

    /// Root remains the held tracer. An unexpected signal, exit or later exec
    /// can never become successful construction or consumption evidence.
    pub(super) fn recheck(&mut self, deadline: Deadline) -> Result<()> {
        deadline.check()?;
        let mut status = 0;
        let rc = unsafe { libc::waitpid(self.pid as libc::pid_t, &mut status, libc::WNOHANG) };
        if rc == self.pid as libc::pid_t {
            if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                self.reaped = true;
            }
            return Err(refused()); // never continue a second exec stop
        }
        if rc < 0 || !self.exec_completed {
            return Err(refused());
        }
        for pipe in [&self.output, &self.errors] {
            let mut byte = [0u8; 1];
            let n = unsafe { libc::read(pipe.as_raw_fd(), byte.as_mut_ptr().cast(), 1) };
            if n >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EAGAIN) {
                return Err(refused()); // stdout/diagnostics never authorize custody
            }
        }
        self.corroborate(deadline)
    }
    fn corroborate(&self, deadline: Deadline) -> Result<()> {
        if crate::peer::proc_starttime(self.pid) != Some(self.starttime) {
            return Err(refused());
        }
        let live = File::open(format!("/proc/{}/exe", self.pid)).map_err(|_| refused())?;
        files::correspond(&live, self.executable.selected_stamp())?;
        self.executable.recheck(deadline)?;
        for (name, held) in ["user", "pid", "mnt"].iter().zip(&self.namespaces) {
            let current =
                File::open(format!("/proc/{}/ns/{name}", self.pid)).map_err(|_| refused())?;
            use std::os::unix::fs::MetadataExt;
            let a = current.metadata().map_err(|_| refused())?;
            let b = held.metadata().map_err(|_| refused())?;
            if (a.dev(), a.ino()) != (b.dev(), b.ino()) {
                return Err(refused());
            }
        }
        let status = bounded_status(&format!("/proc/{}/status", self.pid))?;
        let value = |name: &str| -> Result<&str> {
            let mut fields = status.lines().filter_map(|l| l.strip_prefix(name));
            let first = fields.next().ok_or_else(refused)?.trim();
            if fields.next().is_some() {
                return Err(refused());
            }
            Ok(first)
        };
        let ids = format!("{0}\t{0}\t{0}\t{0}", self.principal.id());
        if value("Uid:")? != ids
            || value("Gid:")? != ids
            || !value("Groups:")?.is_empty()
            || value("NoNewPrivs:")? != "1"
            || value("Threads:")? != "1"
            || value("Seccomp:")? != "2"
            || value("TracerPid:")? != std::process::id().to_string()
        {
            return Err(refused());
        }
        for cap in ["CapInh:", "CapPrm:", "CapEff:", "CapBnd:", "CapAmb:"] {
            if value(cap)? != "0000000000000000" {
                return Err(refused());
            }
        }
        deadline.check()
    }
    pub(super) fn cleanup(&mut self) {
        if self.reaped {
            return;
        }
        // pidfd targets ONLY the child this constructor acquired. Never signal
        // a PID sampled from a frame/procfs/another agent.
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0u32,
            );
        }
        let until = Deadline(std::time::Instant::now() + Duration::from_secs(1));
        while let Ok(status) = wait(self.pid, until) {
            if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                self.reaped = true;
                break;
            }
            let _ = trace(libc::PTRACE_CONT, self.pid, libc::SIGKILL as usize);
        }
        // A timeout is UNKNOWN; a controller must keep its durable obligation.
        let _ = self.owner.shutdown(std::net::Shutdown::Both);
    }
}
impl Drop for HeldChild {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn close_except(exec: RawFd) -> Result<()> {
    if exec <= OWNER_FD {
        return Err(refused());
    }
    if exec > OWNER_FD + 1
        && unsafe {
            libc::syscall(
                libc::SYS_close_range,
                (OWNER_FD + 1) as u32,
                (exec - 1) as u32,
                0u32,
            )
        } != 0
    {
        return Err(refused());
    }
    if unsafe { libc::syscall(libc::SYS_close_range, (exec + 1) as u32, u32::MAX, 0u32) } != 0 {
        return Err(refused());
    }
    Ok(())
}
fn duplicate(source: RawFd, target: RawFd) -> Result<()> {
    if source == target {
        if unsafe { libc::fcntl(target, libc::F_SETFD, 0) } < 0 {
            return Err(refused());
        }
    } else if unsafe { libc::dup3(source, target, 0) } < 0 {
        return Err(refused());
    }
    Ok(())
}

fn bounded_status(path: &str) -> Result<String> {
    let mut status = String::new();
    File::open(path)
        .map_err(|_| refused())?
        .take(65537)
        .read_to_string(&mut status)
        .map_err(|_| refused())?;
    if status.len() > 65536 {
        return Err(refused());
    }
    Ok(status)
}

/// Invoked only after independently authenticated image/topology custody.
/// `executable` is a held fixed artifact, never a path or caller descriptor.
pub(super) fn construct(
    executable: files::HeldArtifact,
    principal: Principal,
    deadline: Deadline,
) -> Result<HeldChild> {
    deadline.check()?;
    if carrier::ids()? != [0; 6] {
        return Err(refused());
    }
    let root_status = bounded_status("/proc/self/status")?;
    if !root_status.lines().any(|l| l == "Threads:\t1") {
        return Err(refused());
    }
    executable.recheck(deadline)?;
    let exec = executable.fd();
    if exec <= OWNER_FD {
        return Err(refused());
    }
    let (owner, child_owner) = UnixStream::pair().map_err(|_| refused())?;
    channel::nonblocking(owner.as_raw_fd())?;
    channel::nonblocking(child_owner.as_raw_fd())?;
    let (child_in, input) = pipes()?;
    let (output, child_out) = pipes()?;
    let (errors, child_err) = pipes()?;
    let mut program = filter(); // allocation is BEFORE fork
    let id = principal.id();
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(refused());
    }
    if pid == 0 {
        let result = (|| -> Result<()> {
            if unsafe { libc::ptrace(libc::PTRACE_TRACEME, 0, 0usize, 0usize) } != 0
                || unsafe { libc::raise(libc::SIGSTOP) } != 0
            {
                return Err(refused());
            }
            // All allocated peers are >3. Install stdio FIRST, FD3 LAST so
            // overwriting inherited FD3 cannot destroy a selected source.
            duplicate(child_in.as_raw_fd(), 0)?;
            duplicate(child_out.as_raw_fd(), 1)?;
            duplicate(child_err.as_raw_fd(), 2)?;
            duplicate(child_owner.as_raw_fd(), OWNER_FD)?;
            close_except(exec)?;
            let mut kernel = seal::Linux;
            let last = kernel.prepare()?;
            if unsafe { libc::setgroups(0, std::ptr::null()) } != 0
                || unsafe { libc::setresgid(id, id, id) } != 0
                || unsafe { libc::setresuid(id, id, id) } != 0
                || unsafe { libc::chdir(c"/".as_ptr()) } != 0
            {
                return Err(refused());
            }
            kernel.finish(last)?;
            if carrier::ids()? != [id; 6] {
                return Err(refused());
            }
            install_filter(&mut program)?;
            let argv = [c"cadence-enrolled-child".as_ptr(), std::ptr::null()];
            let env: [*const libc::c_char; 1] = [std::ptr::null()];
            unsafe {
                libc::syscall(
                    libc::SYS_execveat,
                    exec,
                    c"".as_ptr(),
                    argv.as_ptr(),
                    env.as_ptr(),
                    libc::AT_EMPTY_PATH,
                );
            }
            Err(refused())
        })();
        let _ = result;
        unsafe { libc::_exit(125) } // no destructor on recycled child FDs
    }
    drop(child_owner);
    drop(child_in);
    drop(child_out);
    drop(child_err);
    // From this point acquire cleanup ownership before any fallible measurement.
    // pidfd_open on a direct unreaped child cannot race PID reuse.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0u32) } as i32;
    if fd < 0 {
        // This PID comes directly from OUR fork and is unreaped, not caller data.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        let cleanup = Deadline(std::time::Instant::now() + Duration::from_secs(1));
        while let Ok(status) = wait(pid as u32, cleanup) {
            if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                break;
            }
            let _ = trace(libc::PTRACE_CONT, pid as u32, libc::SIGKILL as usize);
        }
        return Err(refused());
    }
    let mut child = HeldChild {
        pid: pid as u32,
        starttime: 0,
        pidfd: unsafe { File::from_raw_fd(fd) },
        executable,
        namespaces: Vec::new(),
        owner,
        input,
        output,
        errors,
        principal,
        reaped: false,
        exec_completed: false,
    };
    let initial = wait(child.pid, deadline)?;
    if !libc::WIFSTOPPED(initial) || libc::WSTOPSIG(initial) != libc::SIGSTOP {
        return Err(refused());
    }
    trace(
        libc::PTRACE_SETOPTIONS,
        child.pid,
        (libc::PTRACE_O_TRACEEXEC | libc::PTRACE_O_TRACESECCOMP | libc::PTRACE_O_EXITKILL) as usize,
    )?;
    child.starttime = crate::peer::proc_starttime(child.pid).ok_or_else(refused)?;
    for name in ["user", "pid", "mnt"] {
        child
            .namespaces
            .push(File::open(format!("/proc/{}/ns/{name}", child.pid)).map_err(|_| refused())?);
    }
    trace(libc::PTRACE_CONT, child.pid, 0)?;
    stop(wait(child.pid, deadline)?, libc::PTRACE_EVENT_SECCOMP)?;
    let mut regs: libc::user_regs_struct = unsafe { std::mem::zeroed() };
    if unsafe { libc::ptrace(libc::PTRACE_GETREGS, pid, 0usize, &mut regs as *mut _) } != 0
        || regs.orig_rax != libc::SYS_execveat as u64
        || regs.rdi != exec as u64
        || regs.r10 != libc::AT_EMPTY_PATH as u64
    {
        return Err(refused());
    }
    // Check the empty-path byte at the held, single-threaded pre-exec stop.
    unsafe {
        *libc::__errno_location() = 0;
    }
    let word = unsafe { libc::ptrace(libc::PTRACE_PEEKDATA, pid, regs.rsi as usize, 0usize) };
    if unsafe { *libc::__errno_location() } != 0 || word as u8 != 0 {
        return Err(refused());
    }
    // Exactly ONE exec continuation. Once untrusted child code runs, every
    // subsequent exec is a tracer stop rejected by recheck; never resumed.
    trace(libc::PTRACE_CONT, child.pid, 0)?;
    stop(wait(child.pid, deadline)?, libc::PTRACE_EVENT_EXEC)?;
    child.exec_completed = true;
    child.corroborate(deadline)?;
    // Leave child stopped: authenticated PREPARED must precede barrier release.
    Ok(child)
}
