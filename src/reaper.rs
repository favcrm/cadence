//! The daemon as child subreaper (CAD-308), and the one way cadence
//! starts a child process.
//!
//! **Why.** `peer::operator_proof` refuses any peer that descends from
//! the daemon. A process under a daemon-launched tree (a runner recipe,
//! a managed provider's tool) could leave that ancestry with a detach
//! (`setsid -f`, a double fork, `daemon(3)`): its parent exits, the
//! kernel re-parents it to the nearest *child subreaper* ancestor — by
//! default init — and, with its env scrubbed and stdio redirected, it
//! passed as the operator. [`enable`] marks the daemon process
//! `PR_SET_CHILD_SUBREAPER`, so every orphan of every tree the daemon
//! launched re-parents to the DAEMON instead, stays its descendant, and
//! the unchanged descendant check refuses it whatever its env, session
//! or stdio.
//!
//! **The cost.** The daemon now inherits those orphans' exit statuses
//! and must reap them, or they linger as zombies. It must also never
//! consume the status of a child it spawned itself: that child's owner
//! (an adapter, a runner thread, a `git` call) waits on its pid, and a
//! stolen status turns into `ECHILD` — for the stdio adapter even a
//! `kill(-pgid)` of a group id that may have been reused.
//!
//! **Ownership.** A pid is OWNED when this process spawned it. Every
//! spawn in the crate goes through [`spawn`] (or [`output`] /
//! [`status`], built on it) — `clippy.toml` disallows
//! `Command::{spawn, output, status}` everywhere else, so a new call
//! site cannot bypass the registry without a visible `allow`. [`spawn`]
//! runs `Command::spawn` and records the child's pid and start time
//! while holding [`GATE`] for reading. The reaper takes the gate for
//! writing, so it never observes a child between fork and registration.
//! No code in the crate waits on "any child" (`waitpid(-1)`, `P_ALL`)
//! except [`reap_adopted`]; owners wait only on their own pids.
//!
//! **Reaping.** Once a second [`reap_adopted`], under the write gate:
//! prune registrations whose process is provably gone (not our child
//! any more) or replaced (same pid, different start time); peek the
//! next exited child with `waitid(P_ALL, WNOWAIT)`; reap it by pid only
//! when it is NOT owned; when the head of the queue is an owned zombie
//! its owner has yet to collect, sweep the other children from
//! `/proc/self/task/*/children` and reap only the unowned exited ones.
//! Every reap names one pid (`P_PID`), and every uncertain read keeps a
//! registration — the failure mode is a zombie left for the next pass,
//! never a stolen status.
//!
//! Only the `daemon run` process enables this. In-process test daemons
//! (`daemon::serve_with` inside a test binary) do not: a test binary
//! spawns children outside this registry, so a reaper there could steal.
//! A process that never enabled it (every CLI, `cadence ui`) registers
//! nothing — there is no reaper to prune the registry, so it would only
//! grow.
//!
//! **Visibility.** An orphan no longer sees `getppid() == 1`, so a helper
//! that polls for that to exit when orphaned now lives on as a live
//! daemon child. [`adopted_report`] counts live children that are not
//! registered (what `health` shows as `adopted_live`) — nothing kills
//! them.

use std::collections::HashMap;
use std::io;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::Duration;

/// Spawners hold it for reading across `spawn` + registration; the
/// reaper holds it for writing across a whole pass.
static GATE: RwLock<()> = RwLock::new(());

/// Owned pid → its start time (`/proc/<pid>/stat` field 22) at spawn,
/// `None` when that read failed — such an entry is kept until the pid
/// is provably not our child.
static OWNED: Mutex<Option<HashMap<u32, Option<u64>>>> = Mutex::new(None);

/// Set once, by [`arm`], before any child is spawned; registration and
/// reaping happen only in a process where it is set.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Adopted children reaped since [`enable`].
static REAPED_TOTAL: AtomicU64 = AtomicU64::new(0);

/// How often the reaper looks for exited orphans.
const REAP_TICK: Duration = Duration::from_secs(1);

/// `cmd.spawn()`, with the child registered as owned before any reaper
/// pass can see it. The only sanctioned way to start a process.
#[allow(clippy::disallowed_methods)]
pub fn spawn(cmd: &mut Command) -> io::Result<Child> {
    let _gate = GATE.read().unwrap_or_else(|e| e.into_inner());
    let child = cmd.spawn()?;
    if !ENABLED.load(Ordering::SeqCst) {
        return Ok(child);
    }
    let pid = child.id();
    // The child is unreaped here: its owner has not got it back yet and
    // no reaper pass can run, so its stat (live or zombie) is its own.
    let start = proc_starttime(pid);
    OWNED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(pid, start);
    Ok(child)
}

/// `Command::output` through [`spawn`]: stdin is null and stdout and
/// stderr are captured — always, whatever `cmd` configured for them.
pub fn output(cmd: &mut Command) -> io::Result<Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    spawn(cmd)?.wait_with_output()
}

/// `Command::status` through [`spawn`]: stdio as configured (inherited
/// by default), wait for the exit.
pub fn status(cmd: &mut Command) -> io::Result<ExitStatus> {
    spawn(cmd)?.wait()
}

/// Make this process the child subreaper for everything it launches and
/// start the reaper thread. Call once, first thing in `daemon run`,
/// before any child is spawned. An `Err` means the kernel refused the
/// flag — the daemon must not run without it.
pub fn enable() -> io::Result<()> {
    arm()?;
    std::thread::Builder::new()
        .name("subreaper".into())
        .spawn(|| loop {
            std::thread::sleep(REAP_TICK);
            reap_adopted();
        })?;
    eprintln!(
        "subreaper: pid {} is the child subreaper of every process it launches; \
         adopted orphans are reaped, owned children are left to their owners (CAD-308)",
        std::process::id()
    );
    Ok(())
}

/// Turn registration on, then mark the process child subreaper — both
/// before any child exists, so every child it ever has is registered.
fn arm() -> io::Result<()> {
    ENABLED.store(true, Ordering::SeqCst);
    // SAFETY: plain prctl with integer arguments.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// One adopted live child, for [`adopted_report`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adopted {
    pub pid: u32,
    pub comm: String,
    pub age_secs: u64,
}

/// What the subreaper holds: live children this process did NOT spawn
/// (adopted orphans still running — a `getppid() == 1` watcher that no
/// longer fires, the tmux server it started), the oldest `limit` of
/// them, and how many adopted children it has reaped. All zero when the
/// reaper is not enabled.
pub fn adopted_report(limit: usize) -> (usize, Vec<Adopted>, u64) {
    if !ENABLED.load(Ordering::SeqCst) {
        return (0, Vec::new(), 0);
    }
    // Exclusive, so no child is caught between spawn and registration.
    let _gate = GATE.write().unwrap_or_else(|e| e.into_inner());
    let guard = OWNED.lock().unwrap_or_else(|e| e.into_inner());
    let clk = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let uptime_ticks = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|u| u.split_whitespace().next()?.parse::<f64>().ok())
        .map_or(0, |secs| (secs * clk as f64) as u64);
    let mut live: Vec<Adopted> = children()
        .into_iter()
        .filter(|pid| !guard.as_ref().is_some_and(|m| m.contains_key(pid)))
        .filter_map(|pid| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let (head, rest) = stat.rsplit_once(')')?;
            if rest.trim_start().starts_with('Z') {
                return None;
            }
            let start: u64 = rest.split_whitespace().nth(19)?.parse().ok()?;
            Some(Adopted {
                pid,
                comm: head.split_once('(').map_or("", |(_, c)| c).to_string(),
                age_secs: uptime_ticks.saturating_sub(start) / clk,
            })
        })
        .collect();
    let count = live.len();
    live.sort_by(|a, b| b.age_secs.cmp(&a.age_secs).then(a.pid.cmp(&b.pid)));
    live.truncate(limit);
    (count, live, REAPED_TOTAL.load(Ordering::SeqCst))
}

/// Whether this process is marked child subreaper (`PR_GET_CHILD_SUBREAPER`).
pub fn is_subreaper() -> bool {
    let mut flag: libc::c_int = 0;
    // SAFETY: the kernel writes one int through the pointer.
    let rc = unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut flag as *mut libc::c_int) };
    rc == 0 && flag != 0
}

/// One reaper pass (see the module doc). Returns how many adopted
/// children it reaped.
pub fn reap_adopted() -> usize {
    if !ENABLED.load(Ordering::SeqCst) {
        return 0;
    }
    let reaped = reap_pass();
    REAPED_TOTAL.fetch_add(reaped as u64, Ordering::SeqCst);
    reaped
}

fn reap_pass() -> usize {
    let _gate = GATE.write().unwrap_or_else(|e| e.into_inner());
    let mut guard = OWNED.lock().unwrap_or_else(|e| e.into_inner());
    let owned = guard.get_or_insert_with(HashMap::new);
    owned.retain(|&pid, start| still_owned(pid, *start));
    let mut reaped = 0;
    loop {
        let Some(pid) = peek_exited() else {
            return reaped;
        };
        if !owned.contains_key(&pid) && reap_if_exited(pid) {
            reaped += 1;
            continue;
        }
        // An owned zombie heads the queue until its owner collects it —
        // `P_ALL` cannot see past it. Sweep the rest by pid.
        for child in children() {
            if !owned.contains_key(&child) {
                reaped += usize::from(reap_if_exited(child));
            }
        }
        return reaped;
    }
}

/// Keep a registration unless the pid is provably not the process we
/// spawned: no longer our child at all (its owner reaped it), or our
/// child with a different start time (the pid was reused by an adopted
/// orphan after the owner reaped). Anything unreadable keeps it.
fn still_owned(pid: u32, start: Option<u64>) -> bool {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: WNOWAIT only observes; the kernel fills `info`.
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT | libc::__WALL,
        )
    };
    if rc == -1 {
        return io::Error::last_os_error().raw_os_error() != Some(libc::ECHILD);
    }
    match (start, proc_starttime(pid)) {
        (Some(then), Some(now)) => then == now,
        _ => true,
    }
}

/// The pid of some exited child, without reaping it — `None` when no
/// child has exited (or there are no children).
fn peek_exited() -> Option<u32> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: WNOWAIT only observes; the kernel fills `info`.
    let rc = unsafe {
        libc::waitid(
            libc::P_ALL,
            0,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT | libc::__WALL,
        )
    };
    let pid = unsafe { info.si_pid() };
    (rc == 0 && pid > 0).then_some(pid as u32)
}

/// Reap child `pid` if it has exited; true when it was reaped. Names
/// exactly one pid — never any other child's status.
fn reap_if_exited(pid: u32) -> bool {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: plain syscall on one pid; the kernel fills `info`.
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::__WALL,
        )
    };
    rc == 0 && unsafe { info.si_pid() } == pid as libc::pid_t
}

/// Every child of every thread of this process, zombies included.
fn children() -> Vec<u32> {
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{}/task", std::process::id())) else {
        return Vec::new();
    };
    let mut pids: Vec<u32> = tasks
        .flatten()
        .filter_map(|t| std::fs::read_to_string(t.path().join("children")).ok())
        .flat_map(|text| {
            text.split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect::<Vec<u32>>()
        })
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// `/proc/<pid>/stat` field 22, the start time in clock ticks since
/// boot — with the pid, a process identity that survives pid reuse.
fn proc_starttime(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm (field 2) may hold spaces and parens: fields after the last
    // ')' start at 3 (state), so field 22 is index 19.
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process that never enabled the reaper (this test binary, every
    /// CLI, `cadence ui`) registers nothing however much it spawns —
    /// nothing would ever prune it — and reaps and reports nothing.
    #[test]
    fn a_process_that_never_enabled_registers_nothing() {
        assert!(!ENABLED.load(Ordering::SeqCst));
        for _ in 0..25 {
            assert!(status(&mut Command::new("true")).unwrap().success());
        }
        let owned = OWNED.lock().unwrap().as_ref().map_or(0, HashMap::len);
        assert_eq!(owned, 0);
        assert_eq!(reap_adopted(), 0);
        assert_eq!(adopted_report(5), (0, Vec::new(), 0));
    }

    /// A registration outlives nothing: once its owner has reaped the
    /// child, the pid is not our child and the registration drops.
    #[test]
    fn a_registration_ends_when_the_owner_reaps() {
        let mut child = spawn(Command::new("true").stdout(Stdio::null())).unwrap();
        let pid = child.id();
        let start = proc_starttime(pid);
        assert!(start.is_some(), "start time read while unreaped");
        assert!(still_owned(pid, start), "unreaped: still ours");
        assert!(child.wait().unwrap().success());
        assert!(!still_owned(pid, start), "reaped by its owner: gone");
    }

    /// A start time that does not match the live child is a reused pid.
    #[test]
    fn a_reused_pid_is_not_owned() {
        let mut child = spawn(Command::new("sleep").arg("5")).unwrap();
        let pid = child.id();
        let start = proc_starttime(pid).unwrap();
        assert!(still_owned(pid, Some(start)));
        assert!(!still_owned(pid, Some(start + 1)));
        assert!(still_owned(pid, None), "an unread start time keeps it");
        child.kill().unwrap();
        child.wait().unwrap();
    }

    /// `output` and `status` keep `Command`'s answers.
    #[test]
    fn output_and_status_answer_like_command() {
        let out =
            output(Command::new("sh").args(["-c", "echo out; echo err >&2; exit 3"])).unwrap();
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(out.stdout, b"out\n");
        assert_eq!(out.stderr, b"err\n");
        let st = status(Command::new("sh").args(["-c", "exit 4"])).unwrap();
        assert_eq!(st.code(), Some(4));
    }

    /// The reaper's contract, proven in a process of its own (a pass in
    /// this shared test binary could reap other tests' children): the
    /// test binary re-runs itself for [`reaper_proof_isolated`] alone.
    #[test]
    fn reaper_keeps_owned_statuses_and_reaps_adopted_orphans() {
        let out = output(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "reaper::tests::reaper_proof_isolated"])
                .args(["--ignored", "--test-threads", "1", "--nocapture"])
                .env("CADENCE_REAPER_PROOF", "1"),
        )
        .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{text}");
        assert!(text.contains("1 passed"), "{text}");
    }

    /// Children of this process in state `Z`.
    fn zombie(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
            s.rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
        })
    }

    /// Detach `n` children with `setsid -f` from a short-lived `sh`;
    /// each exits at once and re-parents to this (subreaper) process.
    /// Answers their pids once all of them are zombies here.
    fn adopt_exited_orphans(dir: &std::path::Path, tag: &str, n: usize) -> Vec<u32> {
        let file = dir.join(tag);
        let script = format!(
            "i=0; while [ $i -lt {n} ]; do setsid -f sh -c 'echo $$ >> {f}; exit 0' \
             </dev/null >/dev/null 2>&1; i=$((i+1)); done",
            f = file.display()
        );
        assert!(status(Command::new("sh").args(["-c", &script]))
            .unwrap()
            .success());
        let me = std::process::id();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let pids: Vec<u32> = std::fs::read_to_string(&file)
                .unwrap_or_default()
                .lines()
                .filter_map(|l| l.trim().parse().ok())
                .collect();
            let ready =
                pids.len() == n && pids.iter().all(|&p| zombie(p) && children().contains(&p));
            if ready {
                // Re-parented here, not to init: this process is their reaper.
                assert!(pids.iter().all(|p| children().contains(p)), "{me}");
                return pids;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "orphans {pids:?} never all exited here"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Run only by [`reaper_keeps_owned_statuses_and_reaps_adopted_orphans`],
    /// in its own process. Owned children exit first and are left
    /// UNWAITED — zombies heading the `P_ALL` queue, as an owner that
    /// has yet to collect — then orphans are adopted behind them. A pass
    /// reaps exactly the orphans (the sweep path); every owner then
    /// still collects its own child's exit code. A second round, with
    /// no owned zombie in front, reaps through the peek path.
    #[test]
    #[ignore = "run in a process of its own by reaper_keeps_owned_statuses_and_reaps_adopted_orphans"]
    fn reaper_proof_isolated() {
        if std::env::var_os("CADENCE_REAPER_PROOF").is_none() {
            return;
        }
        arm().unwrap();
        assert!(is_subreaper());
        // Registered at spawn, with its own start time.
        let mut probe = spawn(&mut Command::new("true")).unwrap();
        let registered = OWNED
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|m| m.get(&probe.id()).copied());
        assert!(matches!(registered, Some(Some(_))), "{registered:?}");
        assert!(probe.wait().unwrap().success());
        // A live adopted orphan is counted, never killed; owned children
        // are not counted.
        let orphan_dir = tempfile::tempdir().unwrap();
        let pidf = orphan_dir.path().join("live-orphan.pid");
        let script = format!(
            "setsid -f sh -c 'echo $$ > {f}; exec sleep 30' </dev/null >/dev/null 2>&1",
            f = pidf.display()
        );
        assert!(status(Command::new("sh").args(["-c", &script]))
            .unwrap()
            .success());
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let live: u32 = loop {
            let pid = std::fs::read_to_string(&pidf)
                .ok()
                .and_then(|t| t.trim().parse().ok());
            if let Some(pid) = pid.filter(|p| children().contains(p)) {
                break pid;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no live orphan adopted"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut owned_sleeper = spawn(Command::new("sleep").arg("30")).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let (count, oldest, reaped) = loop {
            let report = adopted_report(5);
            if report.1.first().is_some_and(|a| a.comm == "sleep") {
                break report;
            }
            assert!(std::time::Instant::now() < deadline, "{report:?}");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!((count, reaped), (1, 0), "{oldest:?}");
        assert_eq!(oldest[0].pid, live);
        unsafe { libc::kill(live as libc::pid_t, libc::SIGKILL) };
        owned_sleeper.kill().unwrap();
        assert!(owned_sleeper.wait().is_ok());
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !zombie(live) {
            assert!(std::time::Instant::now() < deadline, "orphan never exited");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(reap_adopted(), 1, "the killed orphan");
        assert_eq!(adopted_report(5), (0, Vec::new(), 1));
        let dir = tempfile::tempdir().unwrap();
        let mut owned: Vec<(Child, i32)> = (0..5)
            .map(|i| {
                let code = 10 + i;
                let child =
                    spawn(Command::new("sh").args(["-c", &format!("exit {code}")])).unwrap();
                (child, code)
            })
            .collect();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !owned.iter().all(|(c, _)| zombie(c.id())) {
            assert!(
                std::time::Instant::now() < deadline,
                "owned children never exited"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let first = peek_exited().unwrap();
        assert!(
            owned.iter().any(|(c, _)| c.id() == first),
            "an owned zombie heads the queue"
        );
        let adopted = adopt_exited_orphans(dir.path(), "round-1", 8);
        assert_eq!(reap_adopted(), adopted.len());
        for pid in &adopted {
            assert!(
                !std::path::Path::new(&format!("/proc/{pid}")).exists(),
                "{pid} reaped"
            );
        }
        for (child, code) in &mut owned {
            assert!(
                zombie(child.id()),
                "owned {} left for its owner",
                child.id()
            );
            assert_eq!(child.wait().unwrap().code(), Some(*code));
        }
        assert_eq!(reap_adopted(), 0);
        let adopted = adopt_exited_orphans(dir.path(), "round-2", 6);
        assert_eq!(reap_adopted(), adopted.len());
        assert_eq!(REAPED_TOTAL.load(Ordering::SeqCst), 1 + 8 + 6);
        assert!(children().iter().all(|&p| !zombie(p)), "{:?}", children());
    }

    #[test]
    fn parses_starttime_after_a_paren_comm() {
        let own = proc_starttime(std::process::id()).unwrap();
        assert!(own > 0);
    }
}
