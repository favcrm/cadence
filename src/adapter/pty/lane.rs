//! Pty lane integrity: the process and directory facts of a pane.
//!
//! **Process tree (CAD-201).** tmux starts every pane's command as a
//! session leader (`setsid`), so the pane root's pid is also the id of
//! the session every descendant inherits — including MCP servers that
//! move to their own process group and outlive the pane. At open the
//! adapter records the root's identity ([`PaneRoot`]: pid, start time,
//! session id and endpoint generation). On `agent stop`, after the
//! pane is killed, [`reap_session`] finds the processes still in that
//! session, re-verifies the identity before every signal, sends
//! SIGTERM, waits a bounded drain, re-samples by pid + start time and
//! SIGKILLs only the survivors whose identity still matches.
//!
//! Limits, by construction: a process that calls `setsid` itself
//! leaves the session and is not ours to find; a pid is never
//! signalled on its pid alone — a changed start time (pid reuse), a
//! changed session, or a zombie is skipped; and nothing outside the
//! recorded session is ever touched.
//!
//! **Working directory (CAD-202).** [`pane_cwd`] reads the pane's cwd
//! from `/proc` — the terminal's foreground process group leader when
//! it belongs to the pane's session (what tmux's `pane_current_path`
//! reports), else the pane root — and flags a deleted directory.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// The `/proc/<pid>/stat` fields lane integrity reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStat {
    /// Field 3 — `R`, `S`, `Z`, …
    pub state: char,
    /// Field 5 — process group id.
    pub pgid: u32,
    /// Field 6 — session id.
    pub sid: u32,
    /// Field 8 — foreground process group of the controlling
    /// terminal; `-1` without one.
    pub tpgid: i64,
    /// Field 22 — start time in clock ticks since boot. With the pid
    /// it is the process identity: a reused pid never carries the
    /// same start time.
    pub start_time: u64,
}

/// Parse `/proc/<pid>/stat`. The command name may contain spaces and
/// parentheses, so fields are counted after the *last* `)`.
pub fn proc_stat(pid: u32) -> Option<ProcStat> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_stat(&text)
}

fn parse_stat(text: &str) -> Option<ProcStat> {
    let end = text.rfind(')')?;
    let fields: Vec<&str> = text[end + 1..].split_whitespace().collect();
    Some(ProcStat {
        state: fields.first()?.chars().next()?,
        pgid: fields.get(2)?.parse().ok()?,
        sid: fields.get(3)?.parse().ok()?,
        tpgid: fields.get(5)?.parse().ok()?,
        start_time: fields.get(19)?.parse().ok()?,
    })
}

/// The recorded identity of a pane's root process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRoot {
    pub pid: u32,
    pub start_time: u64,
    /// Session id of the root — equal to `pid` when tmux made it the
    /// session leader, which is the only case the reaper acts on.
    pub sid: u32,
    /// Endpoint generation of the open that recorded it.
    pub generation: String,
}

impl PaneRoot {
    /// Read the identity of `pid` now. `None` when the process is
    /// gone or its stat is unreadable.
    pub fn capture(pid: u32, generation: &str) -> Option<Self> {
        let stat = proc_stat(pid)?;
        Some(Self {
            pid,
            start_time: stat.start_time,
            sid: stat.sid,
            generation: generation.to_string(),
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "pid": self.pid,
            "start_time": self.start_time,
            "sid": self.sid,
            "session_leader": self.sid == self.pid,
            "generation": self.generation,
        })
    }

    pub fn from_json(v: &Value) -> Option<Self> {
        Some(Self {
            pid: u32::try_from(v.get("pid")?.as_u64()?).ok()?,
            start_time: v.get("start_time")?.as_u64()?,
            sid: u32::try_from(v.get("sid")?.as_u64()?).ok()?,
            generation: v.get("generation")?.as_str()?.to_string(),
        })
    }

    /// What the root pid is now: gone (the pane was killed — the
    /// expected case after stop), still the recorded process, or a
    /// different process on a reused pid.
    pub fn check(&self) -> RootState {
        match proc_stat(self.pid) {
            None => RootState::Gone,
            Some(s) if s.state == 'Z' => RootState::Gone,
            Some(s) if s.start_time == self.start_time => RootState::Same,
            Some(s) => RootState::Reused(s.start_time),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootState {
    Gone,
    Same,
    /// A different process holds the pid (its start time).
    Reused(u64),
}

/// One session member, identified by pid + start time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Member {
    pub pid: u32,
    pub start_time: u64,
}

impl Member {
    fn to_json(self) -> Value {
        json!({"pid": self.pid, "start_time": self.start_time})
    }

    /// Is this still the same live process, in the same session?
    fn matches(&self, sid: u32) -> bool {
        proc_stat(self.pid)
            .is_some_and(|s| s.state != 'Z' && s.start_time == self.start_time && s.sid == sid)
    }
}

/// Every live (non-zombie) process in `root`'s session that started no
/// earlier than the root — a session's members are all forked after
/// its leader, so an older process claiming the sid is not ours. The
/// daemon's own process is never a member.
pub fn session_members(root: &PaneRoot) -> Vec<Member> {
    let own = std::process::id();
    let mut members = Vec::new();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return members;
    };
    for entry in procs.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if pid == own {
            continue;
        }
        let Some(stat) = proc_stat(pid) else {
            continue;
        };
        if stat.sid == root.sid && stat.state != 'Z' && stat.start_time >= root.start_time {
            members.push(Member {
                pid,
                start_time: stat.start_time,
            });
        }
    }
    members.sort_by_key(|m| m.pid);
    members
}

/// Signal `member` only if it is still the same process in session
/// `sid`. On Linux the identity is pinned with a pidfd first, so the
/// check and the signal address the same process even if the pid is
/// recycled in between. Returns whether a signal was delivered.
fn signal_verified(member: Member, sid: u32, sig: libc::c_int) -> bool {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: plain syscalls on integer arguments; the fd is closed
        // on every path.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, member.pid as libc::pid_t, 0) };
        if fd >= 0 {
            let fd = fd as libc::c_int;
            let sent = member.matches(sid)
                && unsafe {
                    libc::syscall(
                        libc::SYS_pidfd_send_signal,
                        fd,
                        sig,
                        std::ptr::null::<libc::siginfo_t>(),
                        0,
                    )
                } == 0;
            unsafe { libc::close(fd) };
            return sent;
        }
        // ESRCH: already gone. Any other failure (ENOSYS on an old
        // kernel) falls back to verify-then-kill below.
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return false;
        }
    }
    member.matches(sid) && unsafe { libc::kill(member.pid as libc::pid_t, sig) } == 0
}

/// What one reap did — the payload of the `pane_tree_reaped` event.
#[derive(Debug, Default)]
pub struct ReapReport {
    /// Members sent SIGTERM.
    pub terminated: Vec<Member>,
    /// Of those, the ones gone before the drain ended.
    pub exited: Vec<Member>,
    /// Survivors of the drain whose identity still matched — SIGKILLed.
    pub killed: Vec<Member>,
    /// Processes still in the session at the final sample: members
    /// that appeared during the drain (never signalled) or a SIGKILL
    /// that has not landed yet.
    pub residue: Vec<Member>,
    /// Set when the reap stopped before signalling anything further.
    pub refused: Option<String>,
    pub drain_secs: f64,
}

impl ReapReport {
    pub fn to_json(&self) -> Value {
        let list = |v: &[Member]| v.iter().map(|m| m.to_json()).collect::<Vec<_>>();
        json!({
            "terminated": list(&self.terminated),
            "exited": list(&self.exited),
            "killed": list(&self.killed),
            "residue": list(&self.residue),
            "refused": self.refused,
            "drain_secs": self.drain_secs,
        })
    }
}

/// Knobs for [`reap_session`] — production uses the 60 s drain ops
/// measured for MCP children to exit after stdin EOF; tests shorten it.
pub struct ReapOptions {
    pub drain: Duration,
    /// Re-sample interval while draining.
    pub poll: Duration,
    /// Bound on waiting for SIGKILLed members to disappear.
    pub kill_wait: Duration,
}

impl Default for ReapOptions {
    fn default() -> Self {
        Self {
            drain: DEFAULT_DRAIN,
            poll: Duration::from_millis(250),
            kill_wait: Duration::from_secs(2),
        }
    }
}

/// Graceful window between SIGTERM and SIGKILL for a pane's session
/// (CAD-188 measurement: MCP children exit ~60 s after stdin EOF).
pub const DEFAULT_DRAIN: Duration = Duration::from_secs(60);

/// Terminate what is left of `root`'s session after its pane was shut
/// down. `still_ours` is the caller's action-time check (endpoint
/// generation, a concurrent reopen) — asked before SIGTERM and again
/// before SIGKILL; an `Err` stops the reap there with its reason.
/// `on_intent` sees the member list before any signal is sent, so the
/// intent is durable even if the process dies mid-drain.
pub fn reap_session(
    root: &PaneRoot,
    opts: &ReapOptions,
    still_ours: &dyn Fn() -> Result<(), String>,
    on_intent: &dyn Fn(&[Member]),
) -> ReapReport {
    let mut report = ReapReport {
        drain_secs: opts.drain.as_secs_f64(),
        ..ReapReport::default()
    };
    let refuse = |mut report: ReapReport, why: String| {
        report.refused = Some(why);
        report
    };
    if root.sid != root.pid {
        return refuse(
            report,
            format!(
                "pane root {} was not a session leader (sid {}) — its \
                 session is not the pane's to reap",
                root.pid, root.sid
            ),
        );
    }
    // SAFETY: getsid(0) only reads the caller's session id.
    let own_sid = unsafe { libc::getsid(0) };
    if own_sid >= 0 && own_sid as u32 == root.sid {
        return refuse(
            report,
            "the daemon runs inside the pane's session — refusing to signal it".to_string(),
        );
    }
    if let RootState::Reused(start) = root.check() {
        return refuse(
            report,
            format!(
                "pane root pid {} was reused (start time {start}, recorded {}) — \
                 the session id no longer names the pane's tree",
                root.pid, root.start_time
            ),
        );
    }
    let members = session_members(root);
    if members.is_empty() {
        return report;
    }
    on_intent(&members);
    if let Err(why) = still_ours() {
        return refuse(report, why);
    }
    for m in &members {
        if signal_verified(*m, root.sid, libc::SIGTERM) {
            report.terminated.push(*m);
        }
    }
    // Bounded drain: re-sample by identity until every signalled
    // member is gone or the window closes.
    let deadline = Instant::now() + opts.drain;
    loop {
        if report.terminated.iter().all(|m| !m.matches(root.sid)) {
            break;
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        std::thread::sleep(opts.poll.min(deadline - now));
    }
    let survivors: Vec<Member> = report
        .terminated
        .iter()
        .copied()
        .filter(|m| m.matches(root.sid))
        .collect();
    report.exited = report
        .terminated
        .iter()
        .copied()
        .filter(|m| !survivors.contains(m))
        .collect();
    if !survivors.is_empty() {
        if let Err(why) = still_ours() {
            report.residue = survivors;
            return refuse(report, why);
        }
        if let RootState::Reused(start) = root.check() {
            report.residue = survivors;
            return refuse(
                report,
                format!(
                    "pane root pid {} was reused during the drain (start time {start})",
                    root.pid
                ),
            );
        }
        for m in survivors {
            if signal_verified(m, root.sid, libc::SIGKILL) {
                report.killed.push(m);
            }
        }
        let deadline = Instant::now() + opts.kill_wait;
        while report.killed.iter().any(|m| m.matches(root.sid)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    // Final sample: anything still in the session is residue.
    if !matches!(root.check(), RootState::Reused(_)) {
        report.residue = session_members(root);
    }
    report
}

/// The pane's working directory as `/proc` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneCwd {
    /// The directory path, without the kernel's ` (deleted)` suffix.
    pub path: String,
    /// The directory was unlinked (or no longer resolves) — work
    /// delivered here lands nowhere.
    pub deleted: bool,
    /// The pid the cwd was read from.
    pub pid: u32,
}

impl PaneCwd {
    pub fn to_json(&self) -> Value {
        json!({"path": self.path, "deleted": self.deleted, "pid": self.pid})
    }
}

/// The pane's cwd: the terminal foreground group leader's when it is
/// in the pane's session (tmux's `pane_current_path`), else the pane
/// root's. `None` when neither is readable.
pub fn pane_cwd(pane_pid: u32) -> Option<PaneCwd> {
    let root = proc_stat(pane_pid)?;
    let foreground = u32::try_from(root.tpgid)
        .ok()
        .filter(|&fg| fg > 0 && fg != pane_pid)
        .filter(|&fg| proc_stat(fg).is_some_and(|s| s.sid == root.sid && s.state != 'Z'));
    foreground.and_then(read_cwd).or_else(|| read_cwd(pane_pid))
}

fn read_cwd(pid: u32) -> Option<PaneCwd> {
    let link = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    let text = link.to_string_lossy().into_owned();
    let (path, unlinked) = match text.strip_suffix(" (deleted)") {
        Some(p) => (p.to_string(), true),
        None => (text, false),
    };
    let deleted = unlinked || !std::path::Path::new(&path).is_dir();
    Some(PaneCwd { path, deleted, pid })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_parses_fields_after_the_last_paren() {
        // A comm with spaces and a `)` must not shift the fields.
        let line = "4242 (a (b) c) S 1 4242 4242 34816 4300 4194560 0 0 0 0 \
                    5 6 0 0 20 0 1 0 987654 1000 100";
        let s = parse_stat(line).unwrap();
        assert_eq!(s.state, 'S');
        assert_eq!(s.pgid, 4242);
        assert_eq!(s.sid, 4242);
        assert_eq!(s.tpgid, 4300);
        assert_eq!(s.start_time, 987654);
        assert!(parse_stat("garbage").is_none());
    }

    #[test]
    fn pane_root_round_trips_and_sees_itself() {
        let me = std::process::id();
        let root = PaneRoot::capture(me, "g1").unwrap();
        assert_eq!(PaneRoot::from_json(&root.to_json()), Some(root.clone()));
        assert_eq!(root.check(), RootState::Same);
        let reused = PaneRoot {
            start_time: root.start_time + 1,
            ..root
        };
        assert!(matches!(reused.check(), RootState::Reused(_)));
    }

    #[test]
    fn deleted_cwd_is_detected() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .current_dir(&path)
            .spawn()
            .unwrap();
        let live = read_cwd(child.id()).unwrap();
        assert!(!live.deleted, "{live:?}");
        drop(dir);
        let gone = read_cwd(child.id()).unwrap();
        assert!(gone.deleted, "{gone:?}");
        assert_eq!(gone.path, path.to_string_lossy());
        let _ = child.kill();
        let _ = child.wait();
    }
}
