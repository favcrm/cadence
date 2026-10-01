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
//! reused, but only after the tracker's git state is shown to hold
//! nothing the crashed writer left: an interrupted write is refused,
//! never replayed.
//!
//! "Nothing the crashed writer left" is measured against a snapshot.
//! Every writer records the untracked and modified paths that already
//! existed when it took the lock (`.git/cadence-write.dirty`). A
//! long-lived foreign file therefore stays foreign after a crash; only
//! an `index.lock`, a merge/rebase/cherry-pick marker, a staged entry
//! or a path that was not in the snapshot counts as interrupted.
//!
//! A `.write.lock` whose content is exactly [`MARKER`] is taken for a
//! dead new-protocol writer's marker once the flock is ours; liveness
//! comes from the flock alone.
//!
//! `flock` belongs to the open file description, which a `fork` copies,
//! and `close` of one descriptor does not release it while a forked
//! child still holds a copy (until it execs and `O_CLOEXEC` closes it).
//! So every holder here is an [`Unlock`]: dropping it calls
//! `flock(LOCK_UN)`, which releases the lock for every descriptor that
//! shares the description, a forked child's included. Without that, the
//! lock read as held for tens of milliseconds after its guard dropped
//! whenever another thread spawned a process in between (CAD-948,
//! `lock_tests::a_forked_child_does_not_keep_the_lock_after_the_guard_drops`).
//!
//! A tracker whose `.git` is a gitfile
//! (`--separate-git-dir`, a worktree) keeps its coordination files in
//! the git dir `git rev-parse --git-dir` names.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{Pm, MARKER_LOCK_FILE};
use crate::error::{Error, Result};

/// Content of `.write.lock` while a new-protocol writer holds it.
pub(super) const MARKER: &str =
    "cadence-pm-write-lock v2 (kernel flock on .git/cadence-write.flock)\n";
const FLOCK_FILE: &str = "cadence-write.flock";
const DIRTY_FILE: &str = "cadence-write.dirty";
const TMP_PREFIX: &str = "cadence-write.tmp-";
const WAIT: Duration = Duration::from_secs(15);
/// A non-waiting acquire retries this often when it sees the kernel
/// lock taken, so a state probe's instant of ownership is not "busy".
const PROBE_RETRIES: u32 = 3;
const PROBE_RETRY_GAP: Duration = Duration::from_millis(2);

/// Held for the whole tracker mutation. Dropping removes the legacy
/// fence file first (only if it is still the file this writer made),
/// then releases the kernel lock with an explicit `LOCK_UN`.
#[derive(Debug)]
pub struct PmLock {
    legacy: PathBuf,
    /// `(dev, ino)` of the fence file this writer holds.
    ident: (u64, u64),
    /// False while a reused marker is still under suspicion: a refusal
    /// must leave it in place so the next attempt checks again.
    armed: bool,
    _flock: Unlock,
}

/// A held `flock` that is released explicitly on drop. `close` alone
/// would leave the lock held by any forked copy of the descriptor.
#[derive(Debug)]
pub(crate) struct Unlock(pub(crate) File);

impl Drop for Unlock {
    fn drop(&mut self) {
        // SAFETY: a valid descriptor owned by the file.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl Drop for PmLock {
    fn drop(&mut self) {
        // The kernel lock is released after this body, when `_flock`
        // drops, on the unarmed path as well.
        if !self.armed {
            return;
        }
        // Unlink only the inode we published: a successor's file at the
        // same path (someone removed ours and another writer created
        // theirs) must survive a delayed guard.
        if let Ok(m) = std::fs::symlink_metadata(&self.legacy) {
            if m.is_file() && (m.dev(), m.ino()) == self.ident {
                let _ = std::fs::remove_file(&self.legacy);
            }
        }
    }
}

/// What a read-only probe saw. The states are never merged: an
/// unreadable lock is not a free one, and a legacy file is not a held
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockState {
    /// A live new-protocol writer holds the kernel lock.
    Held,
    /// No writer holds it and no earlier writer left work behind.
    Free,
    /// No writer holds it, but a crashed writer left the tracker
    /// mid-write. Every write is refused until the operator resolves
    /// `paths`; `foreign` paths pre-date the crash and are left alone.
    Interrupted {
        paths: Vec<String>,
        foreign: Vec<String>,
    },
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
            LockState::Interrupted { paths, .. } => write!(
                f,
                "free, but a crashed writer left an interrupted write: {}",
                paths.join(", ")
            ),
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

/// Why the coordination directory could not be resolved.
enum GitDirError {
    NotRepo,
    Io(std::io::Error),
}

fn io_unknown(path: &Path, what: &str, e: impl std::fmt::Display) -> Error {
    Error::internal(format!(
        "tracker lock state unknown (I/O): {what} ({}): {e}; refusing to write",
        path.display()
    ))
}

/// The tracker's git state as the lock cares about it.
struct Tree {
    git_state: Vec<String>,
    staged: Vec<String>,
    /// Untracked or modified (unstaged) paths, as `(kind, path)`.
    loose: Vec<(&'static str, String)>,
}

/// What an interrupted write left, split from what was already there.
struct Interruption {
    named: Vec<String>,
    foreign: Vec<String>,
}

fn open_coordination(path: &Path, create: bool) -> std::io::Result<Option<File>> {
    let mut opts = OpenOptions::new();
    opts.read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let file = match opts.open(path) {
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

/// Create `path` exclusively without following a link at it.
fn create_exclusive(path: &Path, content: &str) -> std::io::Result<File> {
    use std::io::Write;
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    f.write_all(content.as_bytes())?;
    Ok(f)
}

fn read_snapshot(git_dir: &Path) -> Result<HashSet<String>> {
    let path = git_dir.join(DIRTY_FILE);
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(f) => f,
        // No snapshot: every dirty path counts as the crashed writer's.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(e) => return Err(io_unknown(&path, "the dirty-path snapshot", e)),
    };
    let mut buf = Vec::new();
    file.take(16 << 20)
        .read_to_end(&mut buf)
        .map_err(|e| io_unknown(&path, "the dirty-path snapshot", e))?;
    Ok(String::from_utf8_lossy(&buf)
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect())
}

fn write_snapshot(git_dir: &Path, tree: &Tree) -> Result<()> {
    let tmp = git_dir.join(format!("{TMP_PREFIX}dirty-{}", std::process::id()));
    let path = git_dir.join(DIRTY_FILE);
    let mut content = String::new();
    for (_, p) in &tree.loose {
        content.push_str(p);
        content.push('\0');
    }
    let _ = std::fs::remove_file(&tmp);
    create_exclusive(&tmp, &content).map_err(|e| io_unknown(&tmp, "the dirty-path snapshot", e))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io_unknown(&path, "the dirty-path snapshot", e)
    })
}

/// Remove temp markers a crashed writer leaked. Only called with the
/// kernel lock held, so none belongs to a live writer.
fn sweep_tmp(git_dir: &Path) {
    let Ok(rd) = std::fs::read_dir(git_dir) else {
        return;
    };
    for e in rd.flatten() {
        if e.file_name().to_string_lossy().starts_with(TMP_PREFIX) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn classify(tree: &Tree, snapshot: &HashSet<String>) -> Option<Interruption> {
    let mut named: Vec<String> = Vec::new();
    let mut foreign: Vec<String> = Vec::new();
    for s in &tree.git_state {
        named.push(format!("git state {s}"));
    }
    for p in &tree.staged {
        named.push(format!("staged {p}"));
    }
    for (kind, p) in &tree.loose {
        if snapshot.contains(p) {
            foreign.push(format!("{kind} {p}"));
        } else {
            named.push(format!("{kind} {p}"));
        }
    }
    if named.is_empty() {
        None
    } else {
        Some(Interruption { named, foreign })
    }
}

impl Pm {
    /// The directory holding the coordination files: `.git`, or the git
    /// dir a gitfile `.git` names.
    fn lock_git_dir(&self) -> std::result::Result<PathBuf, GitDirError> {
        let dot = self.dir.join(".git");
        match std::fs::symlink_metadata(&dot) {
            Ok(m) if m.is_dir() => Ok(dot),
            Ok(m) if m.is_file() => super::hooks::git_dir(&self.dir).ok_or(GitDirError::NotRepo),
            Ok(_) => Err(GitDirError::Io(std::io::Error::other(
                ".git is neither a directory nor a gitfile",
            ))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(GitDirError::NotRepo),
            Err(e) => Err(GitDirError::Io(e)),
        }
    }

    fn git_dir_error(&self, e: GitDirError) -> Error {
        match e {
            GitDirError::NotRepo => Error::rejected(format!(
                "{} is not a git repository — `cadence issue init` creates one",
                self.dir.display()
            )),
            GitDirError::Io(e) => io_unknown(&self.dir.join(".git"), "the git directory", e),
        }
    }

    /// Classify `.write.lock` without touching it. `Some((ours, ident))`
    /// when it is a regular file: `ours` says its content is exactly
    /// [`MARKER`]; `ident` is `(dev, ino)` from the descriptor read.
    /// Opened `O_NOFOLLOW | O_NONBLOCK` so a link or FIFO swapped in
    /// can neither be followed nor block a writer.
    fn legacy_file(&self) -> std::io::Result<Option<(bool, (u64, u64))>> {
        let path = self.dir.join(MARKER_LOCK_FILE);
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            // A symlink (ELOOP) is not our marker; owner unknown.
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => return Ok(Some((false, (0, 0)))),
            Err(e) => return Err(e),
        };
        let m = file.metadata()?;
        let ident = (m.dev(), m.ino());
        if !m.is_file() {
            return Ok(Some((false, ident)));
        }
        let mut buf = Vec::new();
        file.take(MARKER.len() as u64 + 1).read_to_end(&mut buf)?;
        Ok(Some((buf == MARKER.as_bytes(), ident)))
    }

    /// `git status` as the lock sees it (`--no-optional-locks`: the
    /// check must not take an index lock of its own).
    fn tree_state(&self, git_dir: &Path) -> Result<Tree> {
        let mut git_state = Vec::new();
        for name in [
            "index.lock",
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "rebase-merge",
            "rebase-apply",
        ] {
            if git_dir.join(name).exists() {
                git_state.push(name.to_string());
            }
        }
        let out = crate::reaper::output(
            std::process::Command::new("git")
                .arg("--no-optional-locks")
                .arg("-C")
                .arg(&self.dir)
                .args(["status", "--porcelain", "-z", "--untracked-files=all"]),
        )
        .map_err(|e| io_unknown(&self.dir, "git status", e))?;
        if !out.status.success() {
            return Err(io_unknown(
                &self.dir,
                "git status",
                String::from_utf8_lossy(&out.stderr).trim(),
            ));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let (mut staged, mut loose) = (Vec::new(), Vec::new());
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
            match code {
                "??" => loose.push(("untracked", path.to_string())),
                c if !c.starts_with(' ') => staged.push(path.to_string()),
                _ => loose.push(("modified", path.to_string())),
            }
        }
        Ok(Tree {
            git_state,
            staged,
            loose,
        })
    }

    /// The one classifier for "did a crashed writer leave work?" — used
    /// by the write path and by `lock_state`/doctor alike. Call it only
    /// with the kernel lock held.
    fn interruption(&self, git_dir: &Path, tree: &Tree) -> Result<Option<Interruption>> {
        Ok(classify(tree, &read_snapshot(git_dir)?))
    }

    fn attempt(&self) -> Result<Attempt> {
        let git_dir = self.lock_git_dir().map_err(|e| self.git_dir_error(e))?;
        let flock_path = git_dir.join(FLOCK_FILE);
        let file = open_coordination(&flock_path, true)
            .map_err(|e| io_unknown(&flock_path, "the coordination file", e))?
            .expect("created");
        match flock(&file, libc::LOCK_EX) {
            Ok(true) => {}
            Ok(false) => return Ok(Attempt::Held),
            Err(e) => return Err(io_unknown(&flock_path, "flock", e)),
        }
        // From here every exit releases the lock explicitly.
        let file = Unlock(file);
        // The kernel lock is ours, so any leaked temp marker is stale.
        sweep_tmp(&git_dir);
        let legacy = self.dir.join(MARKER_LOCK_FILE);
        // Publish the marker atomically: a crash can never leave an
        // empty `.write.lock` that would read as a legacy lock.
        let tmp = git_dir.join(format!("{TMP_PREFIX}{}", std::process::id()));
        let tmp_file =
            create_exclusive(&tmp, MARKER).map_err(|e| io_unknown(&tmp, "the lock marker", e))?;
        let tmp_ident = tmp_file
            .metadata()
            .map(|m| (m.dev(), m.ino()))
            .map_err(|e| io_unknown(&tmp, "the lock marker", e))?;
        let linked = std::fs::hard_link(&tmp, &legacy);
        let _ = std::fs::remove_file(&tmp);
        let (ident, reused) = match linked {
            Ok(()) => (tmp_ident, false),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                match self.legacy_file() {
                    // Our own marker under a flock we now hold: the writer
                    // that left it is gone. Reuse it once the git state is
                    // shown free of its leftovers.
                    Ok(Some((true, id))) => (id, true),
                    Ok(_) => return Ok(Attempt::Legacy),
                    Err(e) => return Err(io_unknown(&legacy, ".write.lock", e)),
                }
            }
            Err(e) => return Err(io_unknown(&legacy, ".write.lock", e)),
        };
        // Dropping an unarmed lock leaves the stale marker in place, so
        // a refused attempt is re-checked by the next one.
        let mut lock = PmLock {
            legacy,
            ident,
            armed: !reused,
            _flock: file,
        };
        let tree = self.tree_state(&git_dir)?;
        if reused {
            if let Some(i) = self.interruption(&git_dir, &tree)? {
                return Err(self.interrupted_error(&i));
            }
            lock.armed = true;
        }
        // Before any mutation: what was already dirty is not ours.
        write_snapshot(&git_dir, &tree)?;
        Ok(Attempt::Got(lock))
    }

    /// Take the writer lock, waiting up to `wait`. The lease and actor
    /// fences are re-checked on every turn of the wait and again once
    /// the lock is ours: a kernel lock that frees up is not authority.
    pub(super) fn acquire(&self, wait: Option<Duration>) -> Result<Option<PmLock>> {
        self.fence_check()?;
        let deadline = wait.map(|w| Instant::now() + w);
        let mut retries = 0;
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
                None if !busy && retries < PROBE_RETRIES => {
                    retries += 1;
                    std::thread::sleep(PROBE_RETRY_GAP);
                }
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
            // Not transient: this build never removes or ages out the
            // file, so a retry loop would spin forever (CAD-876). A
            // non-retryable gate with its own code sends the caller to
            // the rollout owner instead.
            Error::gate_coded(
                "legacy_write_lock",
                format!(
                    "PM dir is locked by a legacy write lock ({}) that this build did not \
                     write; its owner cannot be identified — an older cadence \
                     writer may be live, or may have crashed holding it. This build \
                     will not write beside it and will not remove it, and retrying will \
                     not help. Do not delete it as routine: the rollout owner must \
                     clear it during a quiescent migration, after every pre-flock \
                     cadence process is stopped",
                    self.dir.join(MARKER_LOCK_FILE).display()
                ),
            )
        } else {
            Error::busy(
                "PM dir is locked by another live writer (kernel lock on \
                 .git/cadence-write.flock); it frees when that process exits"
                    .to_string(),
            )
        }
    }

    /// Read-only probe: who, if anyone, holds the tracker. It takes the
    /// kernel lock without waiting and releases it at once, so a writer
    /// racing the probe sees it busy for an instant (a non-waiting
    /// acquire retries a few times). It holds the lock for the length
    /// of a `git status` only when a stale marker needs classifying, so
    /// the answer is exactly what the write path would decide. It never
    /// creates or removes anything.
    pub fn lock_state(&self) -> LockState {
        let git_dir = match self.lock_git_dir() {
            Ok(d) => d,
            Err(GitDirError::NotRepo) => {
                return LockState::IoUnknown(format!(
                    "{} is not a git repository",
                    self.dir.display()
                ))
            }
            Err(GitDirError::Io(e)) => {
                return LockState::IoUnknown(format!("{}: {e}", self.dir.join(".git").display()))
            }
        };
        let flock_path = git_dir.join(FLOCK_FILE);
        // Held across the classification below, released on return.
        let _probe = match open_coordination(&flock_path, false) {
            Ok(Some(f)) => match flock(&f, libc::LOCK_EX) {
                Ok(true) => Some(Unlock(f)),
                Ok(false) => return LockState::Held,
                Err(e) => return LockState::IoUnknown(format!("{}: {e}", flock_path.display())),
            },
            Ok(None) => None,
            Err(e) => return LockState::IoUnknown(format!("{}: {e}", flock_path.display())),
        };
        match self.legacy_file() {
            Ok(Some((false, _))) => LockState::LegacyUnknown,
            Ok(Some((true, _))) => {
                match self
                    .tree_state(&git_dir)
                    .and_then(|t| self.interruption(&git_dir, &t))
                {
                    Ok(None) => LockState::Free,
                    Ok(Some(i)) => LockState::Interrupted {
                        paths: i.named,
                        foreign: i.foreign,
                    },
                    Err(e) => LockState::IoUnknown(e.to_string()),
                }
            }
            Ok(None) => LockState::Free,
            Err(e) => LockState::IoUnknown(format!(
                "{}: {e}",
                self.dir.join(MARKER_LOCK_FILE).display()
            )),
        }
    }

    /// A crashed writer leaves the git index and worktree mid-write.
    /// Refuse, naming the exact state, rather than commit or replay it.
    fn interrupted_error(&self, i: &Interruption) -> Error {
        const SHOW: usize = 8;
        let more = i.named.len().saturating_sub(SHOW);
        let named: Vec<&str> = i.named.iter().take(SHOW).map(String::as_str).collect();
        let tail = if more > 0 {
            format!(" (+{more} more)")
        } else {
            String::new()
        };
        let foreign = if i.foreign.is_empty() {
            String::new()
        } else {
            format!(
                " {} foreign path(s) already present before the crash are left alone.",
                i.foreign.len()
            )
        };
        Error::rejected(format!(
            "the tracker has an interrupted write: a writer exited without \
             finishing, leaving {}{tail}. Nothing was replayed or committed and \
             the lock itself is free.{foreign} The write's outcome is uncertain — check \
             `git -C {} status` and `cadence issue lint`, then commit or discard \
             those paths yourself; writes resume once they are resolved",
            named.join(", "),
            self.dir.display()
        ))
    }
}
