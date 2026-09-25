//! `cadence-agent-exec` — the setuid-root drop helper (ADR 0007 §5 L1,
//! CAD-512 T2). T1 installs it `root:cadence-launch` `4750` at
//! `/opt/cadence/libexec/cadence-agent-exec`; three verbs only:
//!
//!   exec [--env K=V]… -- <argv…>   spawn as the agent uid
//!   kill <pid> <signal>            signal an agent-uid pid
//!   inspect <pid>                  print an agent-uid pid's /proc facts
//!
//! This file is the privileged syscall layer — every boundary decision
//! (caller group, verb set, pid/signal bounds, env allowlist) is a pure
//! unit-tested function in `policy`. The order is the contract: close
//! inherited fds, gate the caller, resolve the target from the fixed
//! name, validate argv, drop setgroups→setgid→setuid and verify, then
//! act. Nothing runs as root after the drop; a defect before it is why
//! this file stays small.

mod policy;

use std::ffi::{CStr, CString, OsString};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::process::exit;

/// Exit codes: refusal is a policy verdict, failure is a syscall or
/// provisioning problem; exec's own errno maps to the shell
/// conventions so a daemon-side wait status stays meaningful.
const FAILED: i32 = 1;
const REFUSED: i32 = 2;
const CANNOT_EXEC: i32 = 127;

struct Account {
    uid: u32,
    gid: u32,
    name: String,
    home: String,
    shell: String,
}

fn main() {
    exit(run());
}

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
    if let Err(why) = policy::caller_is_not_agent(uid, agent.uid) {
        return refuse(&why);
    }
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let request = match policy::parse(&args) {
        Ok(r) => r,
        Err(why) => return refuse(&why),
    };
    let mut supplementary = vec![agent.gid, shared_gid];
    supplementary.sort_unstable();
    supplementary.dedup();
    if let Err(e) = drop_to(agent.uid, agent.gid, &supplementary) {
        return fail(&format!("privilege drop: {e}"));
    }
    if !verify_drop(agent.uid, agent.gid, &supplementary) {
        return fail("privilege drop did not take — refusing to continue");
    }
    match request {
        policy::Request::Exec { env, argv } => exec(&agent, &env, &argv),
        policy::Request::Kill { pid, signal } => kill_verb(pid, signal, agent.uid),
        policy::Request::Inspect { pid } => inspect_verb(pid, agent.uid),
    }
}

fn refuse(why: &str) -> i32 {
    eprintln!("cadence-agent-exec: refused: {why}");
    REFUSED
}

fn fail(why: &str) -> i32 {
    eprintln!("cadence-agent-exec: {why}");
    FAILED
}

/// fds >2 belong to the caller (the daemon's open files included) — a
/// setuid binary neither holds them across the drop nor leaks them into
/// the child. `close_range` is one syscall; the bounded loop is the
/// fallback for kernels before 5.9.
fn close_fds() {
    #[cfg(target_os = "linux")]
    unsafe {
        if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) == 0 {
            return;
        }
    }
    let max = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) }.clamp(1024, 65536);
    for fd in 3..max as i32 {
        unsafe { libc::close(fd) };
    }
}

/// The caller's kernel group set: primary gid plus supplementary.
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

fn group_named(name: &str) -> Option<u32> {
    let name = CString::new(name).ok()?;
    let group = unsafe { libc::getgrnam(name.as_ptr()) };
    (!group.is_null()).then(|| unsafe { (*group).gr_gid })
}

fn account_named(name: &str) -> Option<Account> {
    fn s(ptr: *const i8) -> String {
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
    fn ptrs(items: &[CString]) -> Vec<*const i8> {
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

fn proc_owner(pid: i32) -> Option<(u32, u32)> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    policy::status_uids(&status)
}
