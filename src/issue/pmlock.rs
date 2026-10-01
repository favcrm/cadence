//! CAD-852: the tracker write lock.
//!
//! The lock is a kernel advisory lock (`flock(2)`, `LOCK_EX`) on one
//! stable regular file, `.git/cadence-write.flock`. The kernel drops it
//! the moment the holding process dies — SIGKILL, a `timeout` wrapper's
//! SIGTERM, `process::exit`, a panic under `abort` — so no crash can
//! leave the tracker locked. The file is never unlinked (a delayed
//! cleanup has no path to steal), is opened `O_CLOEXEC | O_NOFOLLOW`
//! (a spawned git hook cannot inherit the lock and a symlink is
//! refused) and must be a regular file.
//!
//! Older cadence binaries (production ran one when this landed) still
//! use the path-existence protocol: `create_new(".write.lock")`, held
//! until the guard unlinks it. They cannot see the flock, so a new
//! writer also holds `.write.lock` for its whole write. That fences
//! both directions: an old writer meets the file and waits; a new
//! writer that meets a `.write.lock` it did not write fails closed.
//! The new writer's file carries [`MARKER`]; an empty or foreign file is
//! a legacy lock of unknown owner and is never removed, aged out or
//! guessed at. A marker file left under a flock we now hold was written
//! by a new writer that is gone (the kernel proved it) — so it can be
//! reused, but only after the tracker's git state is shown clean: an
//! interrupted write is refused, never replayed.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::{Pm, MARKER_LOCK_FILE};
use crate::error::{Error, Result};

/// Content of `.write.lock` while a new-protocol writer holds it.
pub(super) const MARKER: &str =
    "cadence-pm-write-lock v2 (kernel flock on .git/cadence-write.flock)\n";
const FLOCK_FILE: &str = "cadence-write.flock";
const WAIT: Duration = Duration::from_secs(15);

/// Held for the whole tracker mutation. Dropping removes the legacy
/// fence file first, then closes the descriptor, which releases the
/// kernel lock.
#[derive(Debug)]
pub struct PmLock {
    legacy: PathBuf,
    _flock: File,
}

impl Drop for PmLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.legacy);
    }
}

/// What a read-only probe saw. The four states are never merged: an
/// unreadable lock is not a free one, and a legacy file is not a held
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockState {
    /// A live new-protocol writer holds the kernel lock.
    Held,
    /// No writer holds it. A stale new-protocol marker may remain; the
    /// next writer checks the git state before reusing it.
    Free,
    /// A `.write.lock` this build did not write: an older binary may
    /// hold it, or crashed holding it. Owner unknown.
    LegacyUnknown,
    /// The lock could not be examined.
    IoUnknown(String),
}

impl std::fmt::Display for LockState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockState::Held => write!(f, "held by a live writer"),
            LockState::Free => write!(f, "free"),
            LockState::LegacyUnknown => write!(f, "legacy lock, owner unknown"),
            LockState::IoUnknown(e) => write!(f, "unknown (I/O error: {e})"),
        }
    }
}

enum Attempt {
    Got(PmLock),
    Held,
    Legacy,
}

fn io_unknown(what: &str, e: impl std::fmt::Display) -> Error {
    Error::internal(format!(
        "tracker lock state unknown (I/O): {what}: {e}; refusing to write"
    ))
}

impl Pm {
    /// Open (never create-over, never follow) the coordination inode.
    fn open_flock(&self, create: bool) -> std::io::Result<Option<File>> {
        let git_dir = self.dir.join(".git");
        let meta = std::fs::symlink_metadata(&git_dir)?;
        if !meta.is_dir() {
            return Err(std::io::Error::other(".git is not a real directory"));
        }
        let mut opts = OpenOptions::new();
        opts.read(true)
            .write(true)
            .create(create)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let file = match opts.open(git_dir.join(FLOCK_FILE)) {
            Ok(f) => f,
            Err(e) if !create && e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::other(
                "the coordination file is not a regular file",
            ));
        }
        Ok(Some(file))
    }

    fn flock(file: &File, op: i32) -> std::io::Result<bool> {
        // SAFETY: a valid descriptor owned by `file`.
        if unsafe { libc::flock(file.as_raw_fd(), op | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EWOULDBLOCK) => Ok(false),
            _ => Err(e),
        }
    }

    /// Classify `.write.lock` without touching it.
    fn legacy_file(&self) -> std::io::Result<Option<bool>> {
        let path = self.dir.join(MARKER_LOCK_FILE);
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_file() => {
                let mut buf = Vec::new();
                use std::io::Read;
                File::open(&path)?
                    .take(MARKER.len() as u64 + 1)
                    .read_to_end(&mut buf)?;
                Ok(Some(buf == MARKER.as_bytes()))
            }
            Ok(_) => Ok(Some(false)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn attempt(&self) -> Result<Attempt> {
        let file = self
            .open_flock(true)
            .map_err(|e| io_unknown("the coordination file", e))?
            .expect("created");
        match Self::flock(&file, libc::LOCK_EX) {
            Ok(true) => {}
            Ok(false) => return Ok(Attempt::Held),
            Err(e) => return Err(io_unknown("flock", e)),
        }
        let legacy = self.dir.join(MARKER_LOCK_FILE);
        // Publish the marker atomically: a crash can never leave an
        // empty `.write.lock` that would read as a legacy lock.
        let tmp = self
            .dir
            .join(".git")
            .join(format!("cadence-write.tmp-{}", std::process::id()));
        std::fs::write(&tmp, MARKER).map_err(|e| io_unknown("the lock marker", e))?;
        let linked = std::fs::hard_link(&tmp, &legacy);
        let _ = std::fs::remove_file(&tmp);
        match linked {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                match self.legacy_file() {
                    // Our own marker under a flock we now hold: the writer
                    // that left it is gone. Reuse it once the git state is
                    // shown clean; an interrupted write is refused and the
                    // marker stays, so the next attempt checks again.
                    Ok(Some(true)) => self.refuse_interrupted_state()?,
                    Ok(_) => return Ok(Attempt::Legacy),
                    Err(e) => return Err(io_unknown(".write.lock", e)),
                }
            }
            Err(e) => return Err(io_unknown(".write.lock", e)),
        }
        Ok(Attempt::Got(PmLock {
            legacy,
            _flock: file,
        }))
    }

    /// Take the writer lock, waiting up to `wait`. The lease and actor
    /// fences are re-checked on every turn of the wait and again once
    /// the lock is ours: a kernel lock that frees up is not authority.
    pub(super) fn acquire(&self, wait: Option<Duration>) -> Result<Option<PmLock>> {
        self.fence_check()?;
        let deadline = wait.map(|w| Instant::now() + w);
        loop {
            let busy = match self.attempt()? {
                Attempt::Got(lock) => {
                    self.fence_check()?;
                    return Ok(Some(lock));
                }
                Attempt::Held => false,
                Attempt::Legacy => true,
            };
            match deadline {
                None => return Ok(None),
                Some(d) if Instant::now() >= d => return Err(self.busy_error(busy)),
                _ => {
                    std::thread::sleep(Duration::from_millis(50));
                    self.fence_check()?;
                }
            }
        }
    }

    pub(super) fn acquire_default(&self) -> Result<PmLock> {
        self.acquire_for(WAIT)
    }

    pub(super) fn acquire_for(&self, wait: Duration) -> Result<PmLock> {
        Ok(self
            .acquire(Some(wait))?
            .expect("a waiting acquire answers"))
    }

    fn busy_error(&self, legacy: bool) -> Error {
        // A live writer is not evidence that authority changed: app
        // validation defers on busy rather than revoking approval.
        if legacy {
            Error::busy(format!(
                "PM dir is locked by a legacy write lock ({}) that this build did not \
                 write; its owner cannot be identified — an older cadence \
                 writer may be live, or may have crashed holding it. This build \
                 will not write beside it and will not remove it. Do not delete \
                 it as routine: the rollout owner clears it during a quiescent \
                 migration, after every pre-flock cadence process is stopped",
                self.dir.join(MARKER_LOCK_FILE).display()
            ))
        } else {
            Error::busy(
                "PM dir is locked by another live writer (kernel lock on \
                 .git/cadence-write.flock); it frees when that process exits"
                    .to_string(),
            )
        }
    }

    /// Read-only probe: who, if anyone, holds the tracker. Takes a
    /// shared lock for an instant, so a writer racing the probe may
    /// retry once; it never creates or removes anything.
    pub fn lock_state(&self) -> LockState {
        let file = match self.open_flock(false) {
            Ok(f) => f,
            Err(e) => return LockState::IoUnknown(e.to_string()),
        };
        if let Some(f) = &file {
            match Self::flock(f, libc::LOCK_SH) {
                Ok(true) => {}
                Ok(false) => return LockState::Held,
                Err(e) => return LockState::IoUnknown(e.to_string()),
            }
        }
        match self.legacy_file() {
            Ok(Some(false)) => LockState::LegacyUnknown,
            Ok(_) => LockState::Free,
            Err(e) => LockState::IoUnknown(e.to_string()),
        }
    }

    /// A crashed writer leaves the git index and worktree mid-write.
    /// Refuse, naming the exact state, rather than commit or replay it.
    fn refuse_interrupted_state(&self) -> Result<()> {
        let git_dir = self.dir.join(".git");
        let mut git_state: Vec<&str> = Vec::new();
        for name in [
            "index.lock",
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "rebase-merge",
            "rebase-apply",
        ] {
            if git_dir.join(name).exists() {
                git_state.push(name);
            }
        }
        let out = crate::reaper::output(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&self.dir)
                .args(["status", "--porcelain", "-z", "--untracked-files=all"]),
        )
        .map_err(|e| io_unknown("git status", e))?;
        if !out.status.success() {
            return Err(io_unknown(
                "git status",
                String::from_utf8_lossy(&out.stderr).trim(),
            ));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut dirty: Vec<String> = Vec::new();
        let mut fields = text.split('\0');
        while let Some(rec) = fields.next() {
            if rec.len() < 4 {
                continue;
            }
            let (code, path) = (&rec[..2], &rec[3..]);
            if code.starts_with('R') || code.starts_with('C') {
                fields.next();
            }
            if path == MARKER_LOCK_FILE || path.starts_with(".index/") {
                continue;
            }
            let kind = match code {
                "??" => "untracked",
                c if !c.starts_with(' ') => "staged",
                _ => "modified",
            };
            dirty.push(format!("{kind} {path}"));
        }
        if dirty.is_empty() && git_state.is_empty() {
            return Ok(());
        }
        const SHOW: usize = 8;
        let more = dirty.len().saturating_sub(SHOW);
        let mut named: Vec<String> = dirty.into_iter().take(SHOW).collect();
        named.extend(git_state.iter().map(|s| format!("git state {s}")));
        let tail = if more > 0 {
            format!(" (+{more} more)")
        } else {
            String::new()
        };
        Err(Error::rejected(format!(
            "the tracker has an interrupted write: a writer exited without \
             finishing, leaving {}{tail}. Nothing was replayed or committed and \
             the lock itself is free. The write's outcome is uncertain — check \
             `git -C {} status` and `cadence issue lint`, then commit or discard \
             those paths yourself; writes resume once the tracker is clean",
            named.join(", "),
            self.dir.display()
        )))
    }
}
