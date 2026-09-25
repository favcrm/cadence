//! `cadence-agent-exec` — the setuid-root drop helper (ADR 0007 §5 L1,
//! CAD-512 T2). T1 installs it `root:cadence-launch` `4750` at
//! `/opt/cadence/libexec/cadence-agent-exec`; three verbs only:
//!
//!   exec [--env K=V]… -- <argv…>   spawn as the agent uid
//!   kill <pid> <signal>            signal an agent-uid pid
//!   inspect <pid>                  print an agent-uid pid's /proc facts
//!
//! This file is the privileged syscall layer — every boundary decision
//! (caller group, verb set, pid/signal bounds, env allowlist, target-id
//! sanity) is a pure unit-tested function in `policy`. The order is the
//! contract: close inherited fds, gate the caller, resolve the target
//! from the fixed name and vet its ids, validate argv, drop
//! setgroups→setgid→setuid and verify, set no_new_privs, then act.
//! Nothing runs as root after the drop; a defect before it is why this
//! file stays small.

// The policy module is pure and portable — it compiles and its unit
// tests run on every target; only the syscall layer below is
// Linux-only, so on other targets its items go unused.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod policy;

#[cfg(target_os = "linux")]
use std::ffi::{CStr, CString, OsString};
#[cfg(target_os = "linux")]
use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "linux")]
use std::process::exit;

/// Exit codes: refusal is a policy verdict, failure is a syscall or
/// provisioning problem; exec's own errno maps to the shell
/// conventions so a daemon-side wait status stays meaningful.
#[cfg(target_os = "linux")]
const FAILED: i32 = 1;
#[cfg(target_os = "linux")]
const REFUSED: i32 = 2;
#[cfg(target_os = "linux")]
const CANNOT_EXEC: i32 = 127;

#[cfg(target_os = "linux")]
struct Account {
    uid: u32,
    gid: u32,
    name: String,
    home: String,
    shell: String,
}

#[cfg(target_os = "linux")]
fn main() {
    exit(run());
}

/// ADR 0007 §5 L1 is a Linux boundary (getres*, /proc, close_range);
/// the cross-build job still compiles this bin on macOS/aarch64, so a
/// stub stands in — an accidental invocation there refuses loudly
/// rather than silently doing nothing.
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("cadence-agent-exec: this helper runs on Linux only");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
fn run() -> i32 {
    close_fds();
    // The caller gate runs before anything else: group membership is a
    // kernel fact about *this process*, taken from getgid + getgroups.
    let uid = unsafe { libc::getuid() };
    let launch_gid = group_named(policy::LAUNCH_GROUP);
    if let Err(why) = policy::caller_is_member(&caller_groups(), launch_gid) {
        return refuse(&why);
    }
    let agent = match account_named(policy::AGENT_USER) {
        Some(a) => a,
        None => return fail("the agent account is not provisioned (T1)"),
    };
    let shared_gid = match group_named(policy::SHARED_GROUP) {
        Some(g) => g,
        None => return fail("the shared group is not provisioned (T1)"),
    };
    let mut supplementary = vec![agent.gid, shared_gid];
    supplementary.sort_unstable();
    supplementary.dedup();
    // Vet the resolved identity before any privileged call: uid 0 — or
    // gid 0 anywhere in the group set — would make the drop a stay as
    // root, and an agent uid equal to the caller's real uid is the
    // operator under the agent's name. Refusal precedes setgroups.
    if let Err(why) = policy::agent_ids_are_safe(agent.uid, &supplementary, uid) {
        return refuse(&why);
    }
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let request = match policy::parse(&args) {
        Ok(r) => r,
        Err(why) => return refuse(&why),
    };
    if let Err(e) = drop_to(agent.uid, agent.gid, &supplementary) {
        return fail(&format!("privilege drop: {e}"));
    }
    if !verify_drop(agent.uid, agent.gid, &supplementary) {
        return fail("privilege drop did not take — refusing to continue");
    }
    // The drop is proven; seal it before any verb. no_new_privs is
    // one-way and survives execve — the exec'd child can never regain
    // privilege through a setuid or file-capability exec — and a failed
    // prctl refuses the request outright.
    if let Err(e) = set_no_new_privs() {
        return fail(&format!("PR_SET_NO_NEW_PRIVS: {e}"));
    }
    match request {
        policy::Request::Exec { env, argv } => exec(&agent, &env, &argv),
        policy::Request::Kill { pid, signal } => kill_verb(pid, signal, agent.uid),
        policy::Request::Inspect { pid } => inspect_verb(pid, agent.uid),
    }
}

#[cfg(target_os = "linux")]
fn refuse(why: &str) -> i32 {
    eprintln!("cadence-agent-exec: refused: {why}");
    REFUSED
}

#[cfg(target_os = "linux")]
fn fail(why: &str) -> i32 {
    eprintln!("cadence-agent-exec: {why}");
    FAILED
}

/// After the drop is verified and before the verbs — most importantly
/// before `execve`. The flag is one-way and survives exec, so the
/// exec'd child can never regain privilege through a setuid binary or
/// a file-capability exec. A failed prctl is a refusal, not a warning.
#[cfg(target_os = "linux")]
fn set_no_new_privs() -> std::io::Result<()> {
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// fds >2 belong to the caller (the daemon's open files included) — a
/// setuid binary neither holds them across the drop nor leaks them
/// into the child. `close_range(3, ~0)` is the full sweep; a kernel
/// without it (pre-5.9) takes the `/proc/self/fd` walk, the only
/// fallback with no numeric cap — a descriptor seated above any fixed
/// bound still appears in the listing.
#[cfg(target_os = "linux")]
fn close_fds() {
    #[cfg(target_os = "linux")]
    unsafe {
        if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) == 0 {
            return;
        }
    }
    close_fds_via_procfs();
}

/// The pre-5.9 sweep: `/proc/self/fd` names exactly the descriptors
/// this process holds. The readdir fd lands in the listing but is
/// already closed when the closes run, and nothing opens in between,
/// so its stale entry is an EBADF no-op. If /proc itself is unreadable
/// the last resort sweeps to the kernel's own OPEN_MAX — floored at
/// 1024, never capped (the 65536 cap was the bug being removed: a
/// descriptor opened before an rlimit drop can sit above any fixed
/// bound).
#[cfg(target_os = "linux")]
fn close_fds_via_procfs() {
    let mut targets = Vec::new();
    if let Ok(dir) = std::fs::read_dir("/proc/self/fd") {
        for entry in dir.flatten() {
            if let Some(fd) = policy::fd_entry(&entry.file_name()) {
                targets.push(fd);
            }
        }
    }
    if targets.is_empty() {
        let max = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) }
            .max(1024)
            .min(i32::MAX as i64) as i32;
        for fd in 3..max {
            unsafe { libc::close(fd) };
        }
        return;
    }
    for fd in targets {
        unsafe { libc::close(fd) };
    }
}

/// The caller's kernel group set: primary gid plus supplementary.
#[cfg(target_os = "linux")]
fn caller_groups() -> Vec<u32> {
    let mut set = vec![unsafe { libc::getgid() }];
    let n = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if n > 0 {
        let mut buf = vec![0u32; n as usize];
        let got = unsafe { libc::getgroups(n, buf.as_mut_ptr()) };
        set.extend_from_slice(&buf[..got.max(0) as usize]);
    }
    set
}

#[cfg(target_os = "linux")]
fn group_named(name: &str) -> Option<u32> {
    let name = CString::new(name).ok()?;
    let group = unsafe { libc::getgrnam(name.as_ptr()) };
    (!group.is_null()).then(|| unsafe { (*group).gr_gid })
}

#[cfg(target_os = "linux")]
fn account_named(name: &str) -> Option<Account> {
    fn s(ptr: *const libc::c_char) -> String {
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
    let name = CString::new(name).ok()?;
    let pw = unsafe { libc::getpwnam(name.as_ptr()) };
    if pw.is_null() {
        return None;
    }
    Some(Account {
        uid: unsafe { (*pw).pw_uid },
        gid: unsafe { (*pw).pw_gid },
        name: s(unsafe { (*pw).pw_name }),
        home: s(unsafe { (*pw).pw_dir }),
        shell: s(unsafe { (*pw).pw_shell }),
    })
}

/// The canonical order: setgroups (root-only) → setgid → setuid — any
/// earlier step done after setuid is already too late, so a failure
/// anywhere refuses outright.
#[cfg(target_os = "linux")]
fn drop_to(uid: u32, gid: u32, supplementary: &[u32]) -> std::io::Result<()> {
    if unsafe { libc::setgroups(supplementary.len(), supplementary.as_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::setgid(gid) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::setuid(uid) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Prove the drop took: all three real/effective/saved ids equal the
/// target, and the group set is exactly the intended pair. A helper
/// that fails to drop must never reach the verbs.
#[cfg(target_os = "linux")]
fn verify_drop(uid: u32, gid: u32, supplementary: &[u32]) -> bool {
    let (mut r, mut e, mut s) = (0u32, 0u32, 0u32);
    if unsafe { libc::getresuid(&mut r, &mut e, &mut s) } != 0 || [r, e, s] != [uid; 3] {
        return false;
    }
    if unsafe { libc::getresgid(&mut r, &mut e, &mut s) } != 0 || [r, e, s] != [gid; 3] {
        return false;
    }
    let mut groups = caller_groups();
    groups.sort_unstable();
    groups.dedup();
    groups == supplementary
}

#[cfg(target_os = "linux")]
fn exec(agent: &Account, env: &[(OsString, OsString)], argv: &[OsString]) -> ! {
    let target = match policy::program_candidates(&argv[0]).into_iter().find(|c| {
        let Ok(c) = CString::new(c.as_bytes()) else {
            return false;
        };
        unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
    }) {
        Some(t) => t,
        None => {
            eprintln!(
                "cadence-agent-exec: {:?}: not executable on the fixed PATH",
                argv[0]
            );
            exit(CANNOT_EXEC);
        }
    };
    // execve takes NUL-terminated arrays; argv/env bytes come from the
    // kernel's own argv and cannot contain NUL.
    fn ptrs(items: &[CString]) -> Vec<*const libc::c_char> {
        items
            .iter()
            .map(|c| c.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect()
    }
    let c_target = CString::new(target.as_bytes()).expect("paths carry no NUL");
    let c_argv: Vec<CString> = argv
        .iter()
        .map(|a| CString::new(a.as_bytes()).unwrap())
        .collect();
    let c_envp: Vec<CString> = policy::child_env(&agent.home, &agent.name, &agent.shell, env)
        .iter()
        .map(|kv| CString::new(kv.as_bytes()).unwrap())
        .collect();
    let argv_ptr = ptrs(&c_argv);
    let envp_ptr = ptrs(&c_envp);
    unsafe { libc::execve(c_target.as_ptr(), argv_ptr.as_ptr(), envp_ptr.as_ptr()) };
    eprintln!(
        "cadence-agent-exec: exec {:?}: {}",
        argv[0],
        std::io::Error::last_os_error()
    );
    exit(CANNOT_EXEC);
}

/// `kill <pid> <sig>`: the pid is verified agent-owned via
/// `/proc/<pid>/status` for a clean refusal; the kernel's EPERM on the
/// `kill(2)` itself is the actual boundary (the check is advisory — a
/// pid can die and be reused between the two calls).
#[cfg(target_os = "linux")]
fn kill_verb(pid: i32, signal: i32, agent_uid: u32) -> i32 {
    if let Some((real, effective)) = proc_owner(pid) {
        if !policy::target_is_agent(real, effective, agent_uid) {
            return refuse(&format!("pid {pid} is not owned by the agent uid"));
        }
    }
    if unsafe { libc::kill(pid, signal) } == 0 {
        0
    } else {
        fail(&format!("kill {pid}: {}", std::io::Error::last_os_error()))
    }
}

/// `inspect <pid>`: agent-owned check first, then the facts the daemon
/// can no longer read cross-uid (cwd — §6's named casualty) plus the
/// stat identity fields callers verify against (ppid/sid/starttime).
/// environ is deliberately absent — it carries bearer material (§12).
#[cfg(target_os = "linux")]
fn inspect_verb(pid: i32, agent_uid: u32) -> i32 {
    let status = match std::fs::read_to_string(format!("/proc/{pid}/status")) {
        Ok(s) => s,
        Err(_) => return fail(&format!("pid {pid}: unreadable status")),
    };
    let Some((real, effective)) = policy::status_uids(&status) else {
        return fail(&format!("pid {pid}: unparseable status"));
    };
    if !policy::target_is_agent(real, effective, agent_uid) {
        return refuse(&format!("pid {pid} is not owned by the agent uid"));
    }
    let rgid = policy::status_gids(&status).map(|(r, _)| r).unwrap_or(0);
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(s) => s,
        Err(e) => return fail(&format!("pid {pid} stat: {e}")),
    };
    let Some((ppid, sid, starttime)) = policy::proc_stat_ids(&stat) else {
        return fail(&format!("pid {pid}: unparseable stat"));
    };
    let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).unwrap_or_default();
    let mut out = std::io::stdout().lock();
    let _ = write!(
        out,
        "uid {real}\ngid {rgid}\nppid {ppid}\nsid {sid}\nstarttime {starttime}\ncwd {}\n",
        policy::pct_encode(cwd.as_os_str().as_bytes())
    );
    let _ = out.flush();
    0
}

#[cfg(target_os = "linux")]
fn proc_owner(pid: i32) -> Option<(u32, u32)> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    policy::status_uids(&status)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::ffi::CString;

    /// Run `body` in a forked child and return its exit code, so the
    /// one-way process mutations under test (`close_fds`, the prctl)
    /// never scar the test process itself. `body` must stay
    /// allocation-free: a lock another test thread held at fork()
    /// stays held in the child forever.
    fn in_forked_child(body: impl FnOnce() -> i32) -> i32 {
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork: {}", std::io::Error::last_os_error());
        if pid == 0 {
            unsafe { libc::_exit(body()) };
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(
            libc::WIFEXITED(status),
            "child killed by a signal: {status:#x}"
        );
        libc::WEXITSTATUS(status)
    }

    /// Reported by a child that could not seat the proof descriptor —
    /// the parent turns it into a loud skip, not a vacuous pass.
    const SKIP: i32 = 42;

    /// A loud skip — and under `CADENCE_PROVISION_RUNBOOK=1` an
    /// exit-42 failure: the T1 runbook treats "could not test" as
    /// failed acceptance, so a sweep that cannot seat fd 65537 can
    /// never pass vacuously there.
    fn skipped(why: &str) {
        if std::env::var_os("CADENCE_PROVISION_RUNBOOK").is_some() {
            eprintln!("SKIP→FAIL(42): {why}");
            std::process::exit(42);
        }
        eprintln!("SKIP: {why}");
    }

    /// The acceptance shape for the fd sweep: a descriptor seated at
    /// or above 65537 (the pre-CAD-522 clamp line) must not survive.
    /// RLIMIT permitting — the child raises its soft limit to the hard
    /// one (no privilege needed) before dup'ing; hosts that cannot
    /// seat fd 65537 report SKIP.
    fn high_fd_child(close: fn()) -> i32 {
        in_forked_child(move || {
            const WANT: u64 = 65538;
            let mut rl = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) } != 0 {
                return SKIP;
            }
            if rl.rlim_cur < WANT {
                if rl.rlim_max < WANT {
                    return SKIP;
                }
                rl.rlim_cur = WANT;
                if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &rl) } != 0 {
                    return SKIP;
                }
            }
            let high = unsafe { libc::fcntl(0, libc::F_DUPFD, 65537) };
            if high < 0 {
                return 5; // the rlimit allowed it but the seat failed
            }
            close();
            if unsafe { libc::fcntl(high, libc::F_GETFD) } == -1 {
                0
            } else {
                1
            }
        })
    }

    #[test]
    fn close_fds_has_no_65536_clamp() {
        match high_fd_child(close_fds) {
            SKIP => skipped("RLIMIT_NOFILE cannot seat fd 65537"),
            code => assert_eq!(code, 0, "fd >= 65537 survived close_fds"),
        }
    }

    #[test]
    fn procfs_sweep_has_no_65536_clamp() {
        match high_fd_child(close_fds_via_procfs) {
            SKIP => skipped("RLIMIT_NOFILE cannot seat fd 65537"),
            code => assert_eq!(code, 0, "fd >= 65537 survived the procfs sweep"),
        }
    }

    /// The acceptance proof for no_new_privs, without root: a forked
    /// child runs our `set_no_new_privs` then execs `sleep`, and the
    /// parent reads `/proc/<pid>/status` — `NoNewPrivs:\t1` on the
    /// exec'd image is the flag surviving execve. (Any uid may set
    /// it; T1's setuid install only changes *why* it matters.)
    #[test]
    fn no_new_privs_survives_into_the_execd_child() {
        let prog = CString::new("/bin/sleep").unwrap();
        let arg0 = CString::new("sleep").unwrap();
        let arg1 = CString::new("30").unwrap();
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork: {}", std::io::Error::last_os_error());
        if pid == 0 {
            // Allocation-free from here (see in_forked_child): the
            // CStrings were built before the fork.
            if set_no_new_privs().is_err() {
                unsafe { libc::_exit(2) };
            }
            let argv = [arg0.as_ptr(), arg1.as_ptr(), std::ptr::null()];
            let envp: [*const libc::c_char; 1] = [std::ptr::null()];
            unsafe { libc::execve(prog.as_ptr(), argv.as_ptr(), envp.as_ptr()) };
            unsafe { libc::_exit(3) };
        }
        let mut proved = false;
        let mut reaped = false;
        for _ in 0..500 {
            if let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) {
                if status.contains("Name:\tsleep") {
                    proved = status.contains("NoNewPrivs:\t1");
                    break;
                }
            }
            let mut st = 0;
            if unsafe { libc::waitpid(pid, &mut st, libc::WNOHANG) } == pid {
                reaped = true;
                break; // the child died before exec — nothing to prove
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        unsafe { libc::kill(pid, libc::SIGKILL) };
        if !reaped {
            let mut st = 0;
            unsafe { libc::waitpid(pid, &mut st, 0) };
        }
        assert!(proved, "the exec'd child lacked NoNewPrivs or never ran");
    }
}
