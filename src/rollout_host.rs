//! Rollout host: see the parent module.

use super::*;

/// The operator proof of [`require_operator_proof`], public for verbs
/// outside this module that gate on the same rule (CAD-561's
/// `cadence update`).
pub fn require_operator(state_dir: &Path, verb: &str) -> Result<()> {
    require_operator_proof(state_dir, verb)
}

/// `peer::operator_proof` for this process. Pane pids come from a
/// read-only peek. Enrolled roots come from `slots.json`.
pub(super) fn require_operator_proof(state_dir: &Path, verb: &str) -> Result<()> {
    // CAD-482: a seam assertion on this process answers before
    // ancestry — a fixture's spawned operator child asserts `operator`
    // in-band, where `/proc` ancestry would refuse a pane's descendant.
    if let Some(who) = crate::test_seam::process_asserted(state_dir)? {
        return match who {
            crate::test_seam::Asserted::Operator => Ok(()),
            crate::test_seam::Asserted::Agent(alias) => Err(Error::rejected(format!(
                "{verb} is an operator action — this process is test-seam \
                 agent '{alias}', not provably the operator"
            ))),
            crate::test_seam::Asserted::Unproven => Err(Error::rejected(format!(
                "{verb} is an operator action — this process carries a \
                 test-seam 'unproven' assertion, not operator proof"
            ))),
        };
    }
    let panes = registered_panes(state_dir)?;
    let roots = enrolled_roots(state_dir)?;
    let daemon_pid = daemon_pid_for_proof(state_dir)?;
    crate::peer::operator_proof(
        std::process::id(),
        unsafe { libc::getuid() },
        daemon_pid,
        &panes,
        |pid| roots.contains(&pid),
    )
    .map_err(|why| {
        Error::rejected(format!(
            "{verb} is an operator action — this process is not \
             provably the operator: {why}; run it from a shell outside every pane \
             and managed endpoint"
        ))
    })
}

/// Registered pty panes (pane pid → alias) for the operator-proof deny
/// list, the same rows [`crate::store::Store::pty_pane_pids`] reads,
/// via the rollout read-only peek, each checked against its recorded
/// start time ([`crate::peer::AgentPids::fenced`], CAD-385): a row
/// whose pid was reused denies nothing; one with no recorded start —
/// including every row of a store older than v14, which has no
/// `pid_start` column — keeps denying. No database, or no `agents`
/// table, means no panes.
pub(super) fn registered_panes(state_dir: &Path) -> Result<HashMap<u32, String>> {
    let path = db_file(state_dir);
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let peek = open_peek(&path)?;
    if !table_exists(&peek.conn, "agents")? {
        return Ok(HashMap::new());
    }
    let has_start = peek
        .conn
        .prepare("SELECT 1 FROM pragma_table_info('agents') WHERE name='pid_start'")?
        .exists([])?;
    let start = if has_start { "pid_start" } else { "NULL" };
    let mut stmt = peek.conn.prepare(&format!(
        "SELECT alias, pid, {start} FROM agents \
         WHERE endpoint_kind='pty' AND generation IS NOT NULL AND pid IS NOT NULL"
    ))?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<i64>>(2)?,
        ))
    })?;
    let mut panes = Vec::new();
    for row in rows {
        let (alias, pid, start) = row?;
        if let Ok(pid) = u32::try_from(pid) {
            panes.push((alias, pid, start.and_then(|s| u64::try_from(s).ok())));
        }
    }
    Ok(crate::peer::AgentPids::classify(panes).fenced())
}

/// Enrollment root pids from `<state>/slots.json`. A missing file means
/// nothing is enrolled. A root whose `/proc` starttime still matches
/// (or cannot be read) counts, including tombstones; a recycled pid
/// does not. Anything the file claims to be but cannot be parsed fails
/// closed — an unreadable enrollment list is not "nothing enrolled".
pub(super) fn enrolled_roots(state_dir: &Path) -> Result<HashSet<u32>> {
    let path = state_dir.join("slots.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HashSet::new());
        }
        Err(error) => {
            return Err(Error::rejected(format!(
                "rollout release --force cannot read {} ({error})",
                path.display()
            )));
        }
    };
    let doc: Value = serde_json::from_str(&text).map_err(|error| {
        Error::rejected(format!(
            "rollout release --force cannot parse {} ({error})",
            path.display()
        ))
    })?;
    let rows = match doc.get("enrollments") {
        Some(Value::Array(rows)) => rows,
        Some(_) => {
            return Err(Error::rejected(format!(
                "rollout release --force: {} enrollments is not a list",
                path.display()
            )));
        }
        // Legacy v1 holds have no enrollment list.
        None if doc.get("format").is_none() => return Ok(HashSet::new()),
        None => {
            return Err(Error::rejected(format!(
                "rollout release --force: {} has no enrollments to prove against",
                path.display()
            )));
        }
    };
    let mut roots = HashSet::new();
    for row in rows {
        let root = row.get("root").ok_or_else(|| {
            Error::rejected(format!(
                "rollout release --force: {} has an enrollment without root",
                path.display()
            ))
        })?;
        let pid = root
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| {
                Error::rejected(format!(
                    "rollout release --force: {} has an enrollment without root.pid",
                    path.display()
                ))
            })?;
        let starttime = root
            .get("starttime")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                Error::rejected(format!(
                    "rollout release --force: {} has an enrollment without root.starttime",
                    path.display()
                ))
            })?;
        if enrolled_root_matches(pid, starttime) {
            roots.insert(pid);
        }
    }
    Ok(roots)
}

/// Same rule as slot enrollment: an unreadable starttime still matches,
/// so a root we cannot disprove stays enrolled. A different starttime
/// is a recycled pid and is not that root.
pub(super) fn enrolled_root_matches(pid: u32, starttime: u64) -> bool {
    match proc_starttime(pid) {
        Some(now) => now == starttime,
        None => true,
    }
}

/// Pid of the running daemon, or `0` when it is stopped.
///
/// `serve` holds an exclusive flock on `cadence.lock` for its whole
/// life. The holder recorded in `/proc/locks` is that daemon's pid.
/// When the lock is free, the daemon is stopped and the proof sentinel
/// is `0`: it is not a process, and caller ancestry never contains it.
pub(super) fn daemon_pid_for_proof(state_dir: &Path) -> Result<u32> {
    Ok(flock_holder(&state_dir.join("cadence.lock"))?.unwrap_or(0))
}

/// `Some(pid)` when an exclusive flock is held. `None` when the file is
/// absent or the lock is free — a successful probe lock is dropped
/// before returning, so this function does not leave a daemon lock behind.
pub(super) fn flock_holder(path: &Path) -> Result<Option<u32>> {
    flock_holder_from(path, || std::fs::read_to_string("/proc/locks"))
}

/// Reads of `/proc/locks` a held lock gets before its holder counts as
/// missing. The file is not a snapshot: the kernel renders it about a
/// page per `read` and resumes by position, so a lock taken or dropped
/// elsewhere between two reads can skip a live line (CAD-389). A miss
/// re-probes and rereads; only a lock that stays held and unlisted
/// every time is refused.
pub(super) const PROC_LOCKS_READS: usize = 5;

pub(super) fn flock_holder_from(
    path: &Path,
    mut read_locks: impl FnMut() -> std::io::Result<String>,
) -> Result<Option<u32>> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::io::AsRawFd;
    let file = match std::fs::OpenOptions::new().write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Error::rejected(format!(
                "rollout release --force cannot probe {} ({error})",
                path.display()
            )));
        }
    };
    let meta = file.metadata().map_err(|error| {
        Error::rejected(format!(
            "rollout release --force cannot stat {} ({error})",
            path.display()
        ))
    })?;
    let (major, minor) = dev_major_minor(meta.dev());
    let inode = meta.ino();
    for _ in 0..PROC_LOCKS_READS {
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(None);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(Error::rejected(format!(
                "rollout release --force cannot probe {} ({error})",
                path.display()
            )));
        }
        let text = read_locks().map_err(|error| {
            Error::rejected(format!(
                "rollout release --force cannot read /proc/locks ({error})"
            ))
        })?;
        if let Some(pid) = proc_locks_flock_writer(&text, major, minor, inode)? {
            return Ok(Some(pid));
        }
    }
    Err(Error::rejected(format!(
        "rollout release --force: {} is locked but its holder is not in /proc/locks",
        path.display()
    )))
}

/// The pid `/proc/locks` lists as the exclusive flock holder of one
/// inode, if the text has that line.
pub(super) fn proc_locks_flock_writer(
    text: &str,
    major: u32,
    minor: u32,
    inode: u64,
) -> Result<Option<u32>> {
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 6 || fields[1] != "FLOCK" || fields[3] != "WRITE" {
            continue;
        }
        let Some((dev, inode_text)) = fields[5].rsplit_once(':') else {
            continue;
        };
        let Some((major_text, minor_text)) = dev.rsplit_once(':') else {
            continue;
        };
        let (Ok(line_inode), Ok(line_major), Ok(line_minor)) = (
            inode_text.parse::<u64>(),
            u32::from_str_radix(major_text, 16),
            u32::from_str_radix(minor_text, 16),
        ) else {
            continue;
        };
        if line_inode == inode && line_major == major && line_minor == minor {
            let pid = fields[4].parse::<u32>().map_err(|_| {
                Error::rejected("rollout release --force: daemon lock holder pid is not a number")
            })?;
            if pid == 0 {
                return Err(Error::rejected(
                    "rollout release --force: daemon lock holder pid is 0",
                ));
            }
            return Ok(Some(pid));
        }
    }
    Ok(None)
}

/// A device id as `/proc/locks` prints it (`major:minor`).
#[cfg(target_os = "linux")]
pub(super) fn dev_major_minor(dev: u64) -> (u32, u32) {
    (libc::major(dev), libc::minor(dev))
}

/// Off Linux `dev_t` is 32-bit and there is no `/proc/locks` to match,
/// so `flock_holder` refuses when it cannot read it; this only has to
/// compile to the same shape.
#[cfg(not(target_os = "linux"))]
pub(super) fn dev_major_minor(dev: u64) -> (u32, u32) {
    let dev = dev as libc::dev_t;
    (libc::major(dev) as u32, libc::minor(dev) as u32)
}
