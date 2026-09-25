//! CAD-512 / ADR 0007 §5 L1 — the live proof for `cadence-agent-exec`.
//!
//! **Ignored by default: this file needs T1's provisioned host.** The
//! unit tests beside the binary (`src/bin/cadence-agent-exec/policy.rs`)
//! are the always-run half; this file is the setuid half and exists so
//! T1 has a ready-made acceptance run.
//!
//! T1 runbook — after §5's provision block has run on the host and the
//! helper is installed `root:cadence-launch 4750` at
//! `/opt/cadence/libexec/cadence-agent-exec`:
//!
//! ```text
//! cargo build --bin cadence-agent-exec
//! sudo install -o root -g cadence-launch -m 4750 \
//!     target/debug/cadence-agent-exec /opt/cadence/libexec/
//! sudo -E cargo test --test agent_exec -- --ignored --test-threads 1
//! ```
//!
//! (`sudo` because `non_member_is_refused` must spawn as a uid outside
//! the launch group; the member paths work as plain `ubuntu` too.)
//! `CADENCE_AGENT_EXEC` overrides the binary path for ad-hoc runs.
//!
//! Every test skips — loudly, on stderr — rather than fails on a host
//! missing the provision, so an accidental `--ignored` sweep stays
//! green without meaning anything passed.

// A test binary never runs the CAD-308 reaper (only `daemon run`
// does), so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn bin() -> PathBuf {
    std::env::var_os("CADENCE_AGENT_EXEC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/opt/cadence/libexec/cadence-agent-exec"))
}

fn run(args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("spawn helper")
}

/// The provisioned agent uid, from the same fixed source the binary
/// resolves (`getent` is NSS, matching `getpwnam`).
fn agent_uid() -> Option<u32> {
    let out = Command::new("id")
        .args(["-u", "cadence-agent"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Is the current process carrying the launch group? (`id -G` is the
/// kernel group set — the same fact the helper checks.)
fn in_launch_group() -> bool {
    let Some(gid) = group_id("cadence-launch") else {
        return false;
    };
    let Ok(out) = Command::new("id").arg("-G").output() else {
        return false;
    };
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .any(|g| g == gid.to_string())
}

fn group_id(name: &str) -> Option<u32> {
    let out = Command::new("getent").args(["group", name]).output().ok()?;
    let line = String::from_utf8_lossy(&out.stdout);
    line.split(':').nth(2)?.trim().parse().ok()
}

fn skip(why: &str) -> bool {
    eprintln!("SKIP: {why}");
    true
}

fn provisioned() -> bool {
    bin().exists() && agent_uid().is_some() && group_id("cadence").is_some()
}

/// The caller gate: a uid outside `cadence-launch` is refused before
/// the verb runs. Needs root to become `nobody` (`runuser`) or any
/// non-member uid (`setpriv`). The refusal must come out as exit 2 —
/// before argv even mattered, so every verb shape refuses identically.
#[test]
#[ignore = "needs T1's provisioned host and a root test harness"]
fn non_member_is_refused() {
    if !provisioned() {
        assert!(skip("host is not provisioned for cadence-agent-exec"));
        return;
    }
    if unsafe { libc::geteuid() } != 0 {
        assert!(skip("needs root to run the helper as a non-member uid"));
        return;
    }
    let as_nobody = |args: &[&str]| {
        Command::new("runuser")
            .args(["-u", "nobody", "--"])
            .arg(bin())
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("runuser")
    };
    for args in [
        vec!["exec", "--", "id"],
        vec!["kill", "1", "TERM"],
        vec!["inspect", "1"],
        vec!["totally-bogus-verb"],
    ] {
        let out = as_nobody(&args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?} → {:?} {:?}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Seat an inheritable descriptor at >= 65537 — above the pre-CAD-522
/// fallback clamp. `F_DUPFD` (not `F_DUPFD_CLOEXEC`) leaves it
/// inheritable, so every helper spawned while it is open must close it
/// before the exec'd child lists its own fd table. None when the
/// rlimit cannot seat it — the caller reports a loud skip.
fn seat_fd_past_65536() -> Option<i32> {
    const WANT: u64 = 65538;
    let mut rl = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) } != 0 {
        return None;
    }
    if rl.rlim_cur < WANT {
        if rl.rlim_max < WANT {
            return None;
        }
        rl.rlim_cur = WANT;
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &rl) } != 0 {
            return None;
        }
    }
    let fd = unsafe { libc::fcntl(0, libc::F_DUPFD, 65537) };
    (fd >= 0).then_some(fd)
}

/// `exec` crosses into the agent uid and nothing else: fixed PATH,
/// passwd-derived HOME/USER/SHELL, and the group set is exactly
/// {primary, cadence} — no inherited operator groups.
#[test]
#[ignore = "needs T1's provisioned host"]
fn exec_drops_to_the_fixed_uid() {
    if !provisioned() || !in_launch_group() {
        assert!(skip("run as a cadence-launch member on a provisioned host"));
        return;
    }
    // Open before every helper spawn below: the fd is inheritable, so
    // the helper's sweep must close it or it appears in the child's
    // own fd table.
    let high_fd = seat_fd_past_65536();
    let agent = agent_uid().unwrap();
    let out = run(&["exec", "--", "id", "-u"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        agent.to_string()
    );

    let out = run(&["exec", "--", "id", "-G"]);
    let mut gids: Vec<u32> = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(|g| g.parse().unwrap())
        .collect();
    gids.sort_unstable();
    let mut want = vec![agent_primary_gid(), group_id("cadence").unwrap()];
    want.sort_unstable();
    want.dedup();
    assert_eq!(
        gids, want,
        "supplementary set is exactly {{primary, cadence}}"
    );

    // fds >2 were closed before the drop: a daemon-held fd must not
    // appear in the child's fd table — the inherited descriptor seated
    // above 65536 included. `ls` itself transiently holds fd 3 for the
    // dir handle — nothing above that may exist.
    let out = run(&["exec", "--", "ls", "/proc/self/fd"]);
    let fds = String::from_utf8_lossy(&out.stdout);
    for fd in fds.split_whitespace() {
        assert!(
            fd.parse::<u32>().unwrap() <= 3,
            "inherited fd {fd} survived the drop: {fds:?}"
        );
    }
    match high_fd {
        Some(fd) => unsafe {
            libc::close(fd);
        },
        None => eprintln!("SKIP: RLIMIT_NOFILE too low to seat fd 65537"),
    }
}

/// PR_SET_NO_NEW_PRIVS lands after the verified drop and before
/// execve — the exec'd image carries `NoNewPrivs: 1`, so it can never
/// regain privilege through a setuid or file-capability exec.
#[test]
#[ignore = "needs T1's provisioned host"]
fn exec_child_has_no_new_privs() {
    if !provisioned() || !in_launch_group() {
        assert!(skip("run as a cadence-launch member on a provisioned host"));
        return;
    }
    let out = run(&["exec", "--", "cat", "/proc/self/status"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("NoNewPrivs:\t1"), "{text}");
}

fn agent_primary_gid() -> u32 {
    let out = Command::new("id")
        .args(["-g", "cadence-agent"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

/// The child environment is the fixed base set plus validated `--env`
/// pairs only — nothing from the caller's environ survives.
#[test]
#[ignore = "needs T1's provisioned host"]
fn exec_env_is_rebuilt_not_inherited() {
    if !provisioned() || !in_launch_group() {
        assert!(skip("run as a cadence-launch member on a provisioned host"));
        return;
    }
    let out = run(&["exec", "--env", "CADENCE_TEST_MARKER=1", "--", "env"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let names: Vec<&str> = stdout.lines().filter_map(|l| l.split('=').next()).collect();
    for want in [
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "CADENCE_TEST_MARKER",
    ] {
        assert!(names.contains(&want), "missing {want} in {names:?}");
    }
    // Nothing else — no inherited caller vars at all.
    assert_eq!(names.len(), 6, "{names:?}");

    // A planted caller-side secret never crosses: put it in our own
    // environ and confirm `env` does not print it.
    let out = Command::new(bin())
        .args(["exec", "--", "env"])
        .env("CALLER_PLANTED_SECRET", "shh")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&out.stdout).contains("CALLER_PLANTED_SECRET"));

    // And an allowlist refusal is a refusal, not a strip.
    assert_eq!(
        run(&["exec", "--env", "LD_PRELOAD=/x.so", "--", "env"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        run(&["exec", "--env", "PATH=/tmp", "--", "env"])
            .status
            .code(),
        Some(2)
    );
}

/// `kill`: pid>0 + allowlisted signal at the policy layer, agent-owned
/// at the owner check, and the kernel's EPERM as the final boundary —
/// signalled for real against a live agent-uid pid.
#[test]
#[ignore = "needs T1's provisioned host"]
fn kill_bounds_and_foreign_refusal() {
    if !provisioned() || !in_launch_group() {
        assert!(skip("run as a cadence-launch member on a provisioned host"));
        return;
    }
    // Policy refusals — no signal is sent.
    for args in [
        vec!["kill", "0", "TERM"],  // group signal
        vec!["kill", "-1", "TERM"], // group signal
        vec!["kill", "1", "STOP"],  // off-allowlist signal
        vec!["kill", "1", "19"],
    ] {
        assert_eq!(run(&args).status.code(), Some(2), "{args:?}");
    }
    // pid 1 is foreign-uid: refused by the owner check, and the kernel
    // would refuse it anyway.
    assert_ne!(run(&["kill", "1", "TERM"]).status.code(), Some(0));

    // A live agent-uid pid really is signalled.
    let mut child = Command::new(bin())
        .args(["exec", "--", "sleep", "60"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let pid = child.id().to_string();
    assert_eq!(run(&["kill", &pid, "TERM"]).status.code(), Some(0));
    let status = child.wait().unwrap();
    assert!(!status.success(), "SIGTERM reached the agent-uid process");
}

/// `inspect` answers an agent-uid pid and refuses a foreign one.
#[test]
#[ignore = "needs T1's provisioned host"]
fn inspect_reads_agent_pid_only() {
    if !provisioned() || !in_launch_group() {
        assert!(skip("run as a cadence-launch member on a provisioned host"));
        return;
    }
    let mut child = Command::new(bin())
        .args(["exec", "--", "sleep", "60"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let pid = child.id().to_string();
    let out = run(&["inspect", &pid]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(&format!("uid {}", agent_uid().unwrap())),
        "{text}"
    );
    assert!(
        text.contains(&format!("gid {}", agent_primary_gid())),
        "{text}"
    );
    // The cwd field must carry a real path — an empty read (the helper
    // prints nothing after "cwd " when readlink fails) is a failure,
    // not a pass.
    let cwd = text
        .lines()
        .find_map(|l| l.strip_prefix("cwd "))
        .expect("inspect output carries no cwd line");
    assert!(
        !cwd.is_empty(),
        "empty cwd for a live agent-uid pid: {text}"
    );
    assert!(text.contains("starttime "), "{text}");
    assert_eq!(run(&["kill", &pid, "KILL"]).status.code(), Some(0));
    let _ = child.wait();

    // pid 1 is foreign — refused, and nothing about it is printed.
    let out = run(&["inspect", "1"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}
