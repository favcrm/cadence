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
//! The new writer's file carries [`MARKER`] plus an optional diagnostic
//! trailer naming the holder (`pid`, `host`, `version`, `started_at`,
//! `cmd`); an empty or foreign file — or MARKER with a malformed tail —
//! is a legacy lock of unknown owner and is never removed, aged out or
//! guessed at. A marker file left under a flock we now hold was written
//! by a new writer that is gone (the kernel proved it) — so it can be
//! reused, but only after the tracker's git state is shown to hold
//! nothing the crashed writer left: a write touching the leftovers is
//! refused, never replayed. A write that declares paths disjoint from
//! the leftovers ([`Pm::lock_for_paths`]) is admitted with a scope that
//! its commit must stay inside.
//!
//! "Nothing the crashed writer left" is measured against a snapshot.
//! Every writer records the untracked and modified paths that already
//! existed when it took the lock (`.git/cadence-write.dirty`). A
//! long-lived foreign file therefore stays foreign after a crash; only
//! an `index.lock`, a merge/rebase/cherry-pick marker, a staged entry
//! or a path that was not in the snapshot counts as interrupted.
//!
//! A `.write.lock` whose content is exactly [`MARKER`] — or MARKER plus
//! a strictly well-formed trailer — is taken for a dead new-protocol
//! writer's marker once the flock is ours; liveness comes from the
//! flock alone, the trailer is diagnostic only and never proof of
//! death.
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
//! CAD-1190: a waiter does not poll. It parks a helper thread in a
//! blocking `flock(LOCK_EX)` (the kernel queues blocked lockers) and
//! takes the lock the helper wins, bounded by the same deadline; a
//! helper whose waiter gave up releases at once. The dirty snapshot
//! lists untracked directories as one `dir/` entry (`git status
//! --untracked-files=normal`) instead of one line per file; a crashed
//! writer's file inside a directory that was already untracked is found
//! by modification time when a crash needs classifying. A guard held
//! longer than a second reports `held_ms`, pid, command and declared
//! paths on stderr when it drops.
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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::{Pm, MARKER_LOCK_FILE};
use crate::error::{Error, Result};

/// Content of `.write.lock` while a new-protocol writer holds it:
/// [`MARKER`] plus an optional diagnostic trailer, one `key=value`
/// line each. Liveness comes from the flock alone; the trailer only
/// names the holder in refusal messages and doctor output.
pub(super) const MARKER: &str =
    "cadence-pm-write-lock v2 (kernel flock on .git/cadence-write.flock)\n";
/// Trailer keys a writer may append after [`MARKER`]. The set is
/// closed: an unknown key is a malformed tail, and a malformed tail
/// is a legacy lock, never ours. `cmd` is the binary basename only:
/// a full argv could leak secrets (CAD-108), a basename cannot.
const TRAILER_KEYS: [&str; 5] = ["pid", "host", "version", "started_at", "cmd"];

/// Diagnostic trailer parsed from a `.write.lock` this build wrote.
/// Every field is optional: markers from builds that predate trailers
/// carry none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MarkerMeta {
    pub pid: Option<u32>,
    pub host: Option<String>,
    pub version: Option<String>,
    pub started_at: Option<i64>,
    pub cmd: Option<String>,
}

/// What `.write.lock` content means. `Ours` only for exact [`MARKER`]
/// or MARKER plus a strictly well-formed trailer; anything else —
/// empty, foreign, non-UTF-8, or MARKER with a malformed tail — is
/// `Foreign` and is never removed, aged out or guessed at.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MarkerClass {
    Ours(Option<MarkerMeta>),
    Foreign,
}

/// Trailer values are short printable tokens: no whitespace, no path
/// separators, no shell metacharacters. Anything else is malformed.
fn trailer_value(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+' | b':'))
}

fn parse_marker(content: &[u8]) -> MarkerClass {
    let text = match std::str::from_utf8(content) {
        Ok(t) => t,
        Err(_) => return MarkerClass::Foreign,
    };
    let Some(tail) = text.strip_prefix(MARKER) else {
        return MarkerClass::Foreign;
    };
    if tail.is_empty() {
        return MarkerClass::Ours(None);
    }
    let mut meta = MarkerMeta::default();
    let mut seen: u8 = 0;
    for line in tail.split_terminator('\n') {
        let Some((k, v)) = line.split_once('=') else {
            return MarkerClass::Foreign;
        };
        if !TRAILER_KEYS.contains(&k) || !trailer_value(v) {
            return MarkerClass::Foreign;
        }
        let bit = 1u8 << TRAILER_KEYS.iter().position(|t| *t == k).unwrap_or(5);
        if seen & bit != 0 {
            return MarkerClass::Foreign;
        }
        seen |= bit;
        match k {
            "pid" => match v.parse::<u32>() {
                Ok(n) => meta.pid = Some(n),
                Err(_) => return MarkerClass::Foreign,
            },
            "host" => meta.host = Some(v.to_string()),
            "version" => meta.version = Some(v.to_string()),
            "started_at" => match v.parse::<i64>() {
                Ok(n) => meta.started_at = Some(n),
                Err(_) => return MarkerClass::Foreign,
            },
            "cmd" => meta.cmd = Some(v.to_string()),
            _ => return MarkerClass::Foreign,
        }
    }
    MarkerClass::Ours(Some(meta))
}

/// Best-effort hostname for the marker trailer, via libc: no new
/// dependency for one syscall. Absent on error — the trailer simply
/// omits the key.
fn host_name() -> Option<String> {
    let mut buf = [0 as libc::c_char; 256];
    // SAFETY: a valid buffer with its length, owned by this frame.
    if unsafe { libc::gethostname(buf.as_mut_ptr(), buf.len()) } != 0 {
        return None;
    }
    // SAFETY: gethostname succeeded, so the buffer holds a NUL-ended name.
    let name = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy();
    let name = name.trim().to_string();
    if trailer_value(&name) {
        Some(name)
    } else {
        None
    }
}

fn cmd_name() -> Option<String> {
    let base = std::env::current_exe()
        .ok()?
        .file_name()?
        .to_string_lossy()
        .into_owned();
    trailer_value(&base).then_some(base)
}

/// Marker content for a new fence file: [`MARKER`] plus a best-effort
/// diagnostic trailer. Values that fail validation are omitted, never
/// mangled: the parser must accept whatever this writes.
fn marker_content() -> String {
    let mut out = MARKER.to_string();
    let mut line = |k: &str, v: &str| {
        if trailer_value(v) {
            out.push_str(k);
            out.push('=');
            out.push_str(v);
            out.push('\n');
        }
    };
    line("pid", &std::process::id().to_string());
    if let Some(h) = host_name() {
        line("host", &h);
    }
    line("version", env!("CARGO_PKG_VERSION"));
    line("started_at", &super::time::now_epoch().to_string());
    if let Some(b) = cmd_name() {
        line("cmd", &b);
    }
    out
}

/// A process holding a coordination file, found by scanning /proc.
/// `comm` is the kernel's 15-byte process name, never argv: naming a
/// holder must not leak secrets. Linux only: without /proc there is
/// nothing to scan, so the type does not exist there either.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(target_os = "linux")]
pub struct HolderInfo {
    pub pid: u32,
    pub comm: String,
}

/// Result of a best-effort holder scan. `Unavailable` where /proc is
/// absent (CAD-315) or unreadable: callers degrade, never crash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolderScan {
    Unavailable,
    NoneFound,
    #[cfg(target_os = "linux")]
    Found(Vec<HolderInfo>),
}

#[cfg(target_os = "linux")]
fn sanitize_comm(c: &str) -> String {
    let c: String = c.chars().filter(|ch| !ch.is_control()).take(32).collect();
    if c.is_empty() {
        "?".to_string()
    } else {
        c
    }
}

/// Find processes holding the file at `(dev, ino)` by scanning
/// `/proc/<pid>/fd`, comparing via fstat so hardlinks and renames
/// cannot hide a holder. The caller itself is excluded. Best effort:
/// exited processes and permission-denied fd dirs are skipped, and
/// the result is capped — this runs on refusal paths, not hot ones.
#[cfg(target_os = "linux")]
fn scan_holders(dev: u64, ino: u64) -> HolderScan {
    use std::os::unix::fs::MetadataExt;
    let me = std::process::id();
    let Ok(proc_) = std::fs::read_dir("/proc") else {
        return HolderScan::Unavailable;
    };
    let mut found = Vec::new();
    for entry in proc_.flatten() {
        let pid: u32 = match entry.file_name().to_str().and_then(|s| s.parse().ok()) {
            Some(p) if p != me => p,
            _ => continue,
        };
        let dir = entry.path().join("fd");
        let Ok(fds) = std::fs::read_dir(&dir) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(m) = std::fs::metadata(fd.path()) else {
                continue;
            };
            if m.dev() == dev && m.ino() == ino {
                let comm = std::fs::read_to_string(entry.path().join("comm"))
                    .map(|c| sanitize_comm(c.trim()))
                    .unwrap_or_else(|_| "?".to_string());
                found.push(HolderInfo { pid, comm });
                break;
            }
        }
        if found.len() >= 8 {
            break;
        }
    }
    if found.is_empty() {
        HolderScan::NoneFound
    } else {
        HolderScan::Found(found)
    }
}

#[cfg(not(target_os = "linux"))]
fn scan_holders(_dev: u64, _ino: u64) -> HolderScan {
    HolderScan::Unavailable
}

/// One human-readable holder attribution for refusal messages and
/// doctor output: who holds the file, what the marker trailer claims,
/// or a positive statement that nobody does.
fn describe_holder(scan: &HolderScan, meta: Option<&MarkerMeta>) -> String {
    let mut s = match scan {
        #[cfg(target_os = "linux")]
        HolderScan::Found(hs) => {
            let who: Vec<String> = hs
                .iter()
                .map(|h| format!("pid {} ({})", h.pid, h.comm))
                .collect();
            format!("holder: {}", who.join(", "))
        }
        HolderScan::NoneFound => "no live holder found".to_string(),
        HolderScan::Unavailable => "holder scan unavailable on this platform".to_string(),
    };
    if let Some(m) = meta {
        let mut bits = Vec::new();
        if let Some(p) = m.pid {
            bits.push(format!("pid={p}"));
        }
        if let Some(h) = &m.host {
            bits.push(format!("host={h}"));
        }
        if let Some(v) = &m.version {
            bits.push(format!("version={v}"));
        }
        if let Some(t) = m.started_at {
            bits.push(format!("started_at={t}"));
        }
        if let Some(b) = &m.cmd {
            bits.push(format!("cmd={b}"));
        }
        if !bits.is_empty() {
            s.push_str("; marker trailer ");
            s.push_str(&bits.join(" "));
        }
    }
    s
}
const FLOCK_FILE: &str = "cadence-write.flock";
const DIRTY_FILE: &str = "cadence-write.dirty";
const TMP_PREFIX: &str = "cadence-write.tmp-";
const WAIT: Duration = Duration::from_secs(15);
/// A waiter parked in a blocking `flock` re-checks the lease and actor
/// fences this often.
const WAIT_SLICE: Duration = Duration::from_millis(250);
/// Most helper threads parked in a blocking `flock` at once, in this
/// process. A helper cannot be cancelled: one whose waiter gave up at
/// the deadline stays parked until the holder releases, then drops the
/// lock at once. Past this many, a waiter polls instead (the pre-CAD-1190
/// behaviour), so a long hold cannot pile up threads.
const MAX_PARKED: usize = 32;
static PARKED: AtomicUsize = AtomicUsize::new(0);
/// A hold longer than this is reported when the guard drops.
const HOLD_REPORT: Duration = Duration::from_secs(1);
/// A non-waiting acquire retries this often when it sees the kernel
/// lock taken, so a state probe's instant of ownership is not "busy".
const PROBE_RETRIES: u32 = 3;
const PROBE_RETRY_GAP: Duration = Duration::from_millis(2);

/// What a [`PmLock`] may commit. `Full` is the unscoped lock: the
/// whole tracker was clean when it was taken, or the caller asked for
/// no scope. `Paths` admits only writes disjoint from a crashed
/// writer's leftovers: `admitted` is what the caller declared,
/// `blocked` is the crash's raw leftover paths.
#[derive(Debug, Clone)]
enum Scope {
    Full,
    Paths {
        admitted: Vec<String>,
        blocked: Vec<String>,
    },
}

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
    scope: Scope,
    /// When this writer took the lock, for the hold-time report.
    since: Instant,
    _flock: Unlock,
}

/// Repo-relative path overlap, component-wise in both directions: a
/// file under an admitted dir is admitted, and a dir over a blocked
/// file is blocked. The empty scope is the tracker root: it overlaps
/// everything, so a whole-tracker write is never "disjoint".
fn paths_overlap(a: &str, b: &str) -> bool {
    a.is_empty()
        || b.is_empty()
        || a == b
        || a.starts_with(&format!("{b}/"))
        || b.starts_with(&format!("{a}/"))
}

/// True when `path` is `scope` itself or lives under it. The empty
/// scope is the tracker root and contains every path.
fn path_within(path: &str, scope: &str) -> bool {
    scope.is_empty() || path == scope || path.starts_with(&format!("{scope}/"))
}

/// True when a repo-relative path carries git pathspec syntax that
/// would expand beyond the literal string scope checks compare:
/// wildcards, character classes, backslashes, and `:(magic)` or a
/// leading `:` form. Callers never generate these (attachment and
/// comment names are charset-restricted, issue files are fixed
/// names), so refusal is fail-closed with no legitimate breakage.
pub(super) fn has_pathspec_magic(s: &str) -> bool {
    s.contains(['*', '?', '[', ']', '\\']) || s.contains(":(") || s.starts_with(':')
}

impl PmLock {
    /// Refuse a commit that leaves this lock's scope: outside the
    /// declared paths, or overlapping the crashed writer's leftovers.
    /// `Full` locks admit everything (the unscoped behavior). Called
    /// by [`Pm::commit_scoped`](super::Pm::commit_scoped); the plain
    /// commit path never takes a scoped lock.
    pub(super) fn check_commit(&self, rel: &[String]) -> Result<()> {
        let Scope::Paths { admitted, blocked } = &self.scope else {
            return Ok(());
        };
        for p in rel {
            if blocked.iter().any(|b| paths_overlap(p, b)) {
                return Err(Error::rejected(format!(
                    "the tracker has an interrupted write: refusing to commit {p}, \
                     which overlaps a crashed writer's leftovers ({}). Nothing was \
                     committed; resolve those paths first, or commit only paths \
                     disjoint from them",
                    blocked.join(", ")
                )));
            }
            if !admitted.iter().any(|a| path_within(p, a)) {
                return Err(Error::rejected(format!(
                    "commit {p} is outside this lock's admitted paths ({}); \
                     declare every path a scoped write touches",
                    admitted.join(", ")
                )));
            }
        }
        Ok(())
    }
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

/// One line naming a hold longer than [`HOLD_REPORT`]: how long
/// (`held_ms`), by which process and binary (the same `pid` and `cmd`
/// the CAD-1167 marker trailer carries) and, for a scoped write, the
/// declared paths. `None` for a short hold.
fn hold_line(held: Duration, scope: &Scope) -> Option<String> {
    if held <= HOLD_REPORT {
        return None;
    }
    let mut line = format!(
        "cadence: pm write lock held_ms={} pid={}",
        held.as_millis(),
        std::process::id()
    );
    if let Some(c) = cmd_name() {
        line.push_str(&format!(" cmd={c}"));
    }
    if let Scope::Paths { admitted, .. } = scope {
        line.push_str(&format!(" paths={}", admitted.join(",")));
    }
    Some(line)
}

#[cfg(test)]
thread_local! {
    static HOLD_LINES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl PmLock {
    /// Report a long hold on stderr: the daemon's log, a CLI's terminal.
    /// (No `tracing` subscriber is installed in this crate, so an event
    /// would go nowhere.) Never panics: a closed stderr is ignored.
    fn report_hold(&self) {
        let Some(line) = hold_line(self.since.elapsed(), &self.scope) else {
            return;
        };
        #[cfg(test)]
        HOLD_LINES.with(|l| l.borrow_mut().push(line.clone()));
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "{line}");
    }
}

impl Drop for PmLock {
    fn drop(&mut self) {
        self.report_hold();
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
    /// A live new-protocol writer holds the kernel lock. `holder`
    /// names it where the scan could (see [`describe_holder`]).
    Held { holder: String },
    /// No writer holds it and no earlier writer left work behind.
    Free,
    /// No writer holds it, but a crashed writer left the tracker
    /// mid-write. Writes touching `paths` are refused until the
    /// operator resolves them; writes to disjoint paths (via
    /// [`Pm::lock_for_paths`](super::Pm::lock_for_paths)) proceed.
    /// `foreign` paths pre-date the crash and are left alone.
    Interrupted {
        paths: Vec<String>,
        foreign: Vec<String>,
    },
    /// A `.write.lock` this build did not write: an older binary may
    /// hold it, or crashed holding it. `holder` names it where the
    /// scan could — it never proves death, and the file is never
    /// removed on that basis.
    LegacyUnknown { holder: String },
    /// The lock could not be examined.
    IoUnknown(String),
}

impl std::fmt::Display for LockState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockState::Held { .. } => write!(f, "held by a live writer"),
            LockState::Free => write!(f, "free"),
            LockState::Interrupted { paths, .. } => write!(
                f,
                "free, but a crashed writer left an interrupted write: {}",
                paths.join(", ")
            ),
            LockState::LegacyUnknown { .. } => write!(f, "legacy lock, owner unknown"),
            LockState::IoUnknown(e) => write!(f, "unknown (I/O error: {e})"),
        }
    }
}

/// A helper thread parked in a blocking `flock` on the tracker's
/// coordination file. The kernel queues blocked lockers, so waiters
/// are served in turn and the winner takes the lock without a poll
/// gap; the lock it wins is handed to the waiter, not released. The
/// wait stays bounded: the waiter times out on the channel, and a
/// helper that wins after that releases at once.
struct Waiter {
    rx: mpsc::Receiver<Unlock>,
    won: Option<Unlock>,
    /// The helper ended without a lock (a `flock` error) or its lock
    /// was taken: this waiter cannot deliver another.
    spent: bool,
}

impl Waiter {
    fn wait(&mut self, slice: Duration) {
        match self.rx.recv_timeout(slice) {
            Ok(held) => self.won = Some(held),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => self.spent = true,
        }
    }

    fn take(&mut self) -> Option<Unlock> {
        let held = self.won.take();
        if held.is_some() {
            self.spent = true;
        }
        held
    }

    fn spent(&self) -> bool {
        self.spent
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
/// `paths` are the raw repo-relative paths behind `named`, for overlap
/// checks; `global` is true when git-level markers (`index.lock`,
/// merge/rebase heads) are present — those refuse every write,
/// disjoint or not.
struct Interruption {
    named: Vec<String>,
    foreign: Vec<String>,
    paths: Vec<String>,
    global: bool,
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

/// When the dirty-path snapshot was written: the moment the last
/// writer that rewrote it took the lock. `None` without a snapshot.
fn snapshot_time(git_dir: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(git_dir.join(DIRTY_FILE))
        .and_then(|m| m.modified())
        .ok()
}

/// Files under `root/rel` (a directory that was already untracked when
/// the snapshot was taken) modified at or after `since`: a crashed
/// writer's output inside a foreign directory. `>=` because file times
/// are coarse; a tie is read as new, the refusing side. Bounded, and
/// symlinks are not followed.
///
/// Returns true when the scan stopped early: the names are then only
/// a sample, and the caller must block the whole directory.
fn files_since(
    root: &Path,
    rel: &str,
    since: std::time::SystemTime,
    out: &mut Vec<String>,
) -> bool {
    const CAP: usize = 64;
    let mut stack = vec![rel.trim_end_matches('/').to_string()];
    let mut visited = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(root.join(&dir)) else {
            continue;
        };
        for e in rd.flatten() {
            visited += 1;
            if out.len() >= CAP || visited > 500_000 {
                return true;
            }
            let Ok(m) = e.metadata() else { continue };
            let name = format!("{dir}/{}", e.file_name().to_string_lossy());
            if m.is_dir() {
                stack.push(name);
            } else if m.modified().map(|t| t >= since).unwrap_or(true) {
                out.push(name);
            }
        }
    }
    false
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

fn classify(
    tree: &Tree,
    snapshot: &HashSet<String>,
    inside: &[String],
    whole: &[String],
) -> Option<Interruption> {
    let mut named: Vec<String> = Vec::new();
    let mut foreign: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    for s in &tree.git_state {
        named.push(format!("git state {s}"));
    }
    for p in &tree.staged {
        named.push(format!("staged {p}"));
        paths.push(p.clone());
    }
    for (kind, p) in &tree.loose {
        if snapshot.contains(p) {
            foreign.push(format!("{kind} {p}"));
        } else if p.ends_with('/') && inside.iter().any(|f| f.starts_with(p.as_str())) {
            // Named by the files found inside it, below.
        } else {
            named.push(format!("{kind} {p}"));
            paths.push(p.trim_end_matches('/').to_string());
        }
    }
    for p in inside {
        named.push(format!("untracked {p}"));
        paths.push(p.clone());
    }
    // Names are a capped sample; a directory scanned in part blocks
    // everything under it, so the bound never shrinks the blocked set.
    for d in whole {
        paths.push(d.trim_end_matches('/').to_string());
    }
    if named.is_empty() {
        None
    } else {
        Some(Interruption {
            global: !tree.git_state.is_empty(),
            named,
            foreign,
            paths,
        })
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

    /// Classify `.write.lock` without touching it. `Some((class, ident))`
    /// when it opens as a regular file: `class` says whether its
    /// content is ours (exact [`MARKER`], or MARKER plus a strictly
    /// well-formed trailer — see [`parse_marker`]); `ident` is
    /// `(dev, ino)` from the descriptor read. Opened
    /// `O_NOFOLLOW | O_NONBLOCK` so a link or FIFO swapped in can
    /// neither be followed nor block a writer. The read cap covers
    /// MARKER plus a full trailer; longer content is foreign.
    fn legacy_file(&self) -> std::io::Result<Option<(MarkerClass, (u64, u64))>> {
        let path = self.dir.join(MARKER_LOCK_FILE);
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            // A symlink (ELOOP) is not our marker; owner unknown.
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
                return Ok(Some((MarkerClass::Foreign, (0, 0))))
            }
            Err(e) => return Err(e),
        };
        let m = file.metadata()?;
        let ident = (m.dev(), m.ino());
        if !m.is_file() {
            return Ok(Some((MarkerClass::Foreign, ident)));
        }
        let mut buf = Vec::new();
        file.take(4096).read_to_end(&mut buf)?;
        Ok(Some((parse_marker(&buf), ident)))
    }

    /// Holder attribution for a live writer: fstat the open flock
    /// descriptor (no path race) for the /proc scan, plus the fence
    /// file's trailer where it is ours.
    fn live_holder(&self, flock_fd: &File) -> String {
        use std::os::unix::fs::MetadataExt;
        let scan = match flock_fd.metadata() {
            Ok(m) => scan_holders(m.dev(), m.ino()),
            Err(_) => HolderScan::Unavailable,
        };
        let meta = self
            .legacy_file()
            .ok()
            .flatten()
            .and_then(|(c, _)| match c {
                MarkerClass::Ours(m) => m,
                MarkerClass::Foreign => None,
            });
        describe_holder(&scan, meta.as_ref())
    }

    /// Holder attribution for a legacy fence file: the /proc scan over
    /// the path's live inode. The content is never parsed — it is not
    /// ours by definition — and nothing here authorizes removal.
    fn legacy_holder(&self) -> String {
        use std::os::unix::fs::MetadataExt;
        let path = self.dir.join(MARKER_LOCK_FILE);
        let scan = match std::fs::metadata(&path) {
            Ok(m) => scan_holders(m.dev(), m.ino()),
            Err(_) => HolderScan::Unavailable,
        };
        describe_holder(&scan, None)
    }

    /// Holder attribution for a refusal: the flock file's live holders
    /// plus the fence trailer where it is ours (live-writer case), or
    /// the legacy file's holders (legacy case). Best effort throughout:
    /// an unresolvable git dir degrades to a static note, never an
    /// error on top of the refusal.
    fn refusal_holder(&self, legacy: bool) -> String {
        use std::os::unix::fs::MetadataExt;
        let git_dir = match self.lock_git_dir() {
            Ok(d) => d,
            Err(_) => return "holder scan skipped: git dir unresolvable".to_string(),
        };
        if legacy {
            return self.legacy_holder();
        }
        let flock_path = git_dir.join(FLOCK_FILE);
        let scan = match std::fs::metadata(&flock_path) {
            Ok(m) => scan_holders(m.dev(), m.ino()),
            Err(_) => HolderScan::Unavailable,
        };
        let meta = self
            .legacy_file()
            .ok()
            .flatten()
            .and_then(|(c, _)| match c {
                MarkerClass::Ours(m) => m,
                MarkerClass::Foreign => None,
            });
        describe_holder(&scan, meta.as_ref())
    }

    /// `git status` as the lock sees it (`--no-optional-locks`: the
    /// check must not take an index lock of its own). Untracked
    /// directories are reported as one `dir/` entry, not one line per
    /// file (CAD-1190): [`Self::interruption`] looks inside the ones
    /// that pre-date the crash, on the rare path that needs it.
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
                .args(["status", "--porcelain", "-z", "--untracked-files=normal"]),
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
            // Renames and copies carry the source path in the next NUL
            // field (`R  <to>\0<from>\0` under -z): both ends are
            // affected paths. Dropping the source was safe for global
            // refusal, but selective admission must block it too — a
            // scoped write could otherwise recreate the source while
            // the crashed rename's destination stays staged.
            let renamed_from = if code.starts_with('R') || code.starts_with('C') {
                fields.next()
            } else {
                None
            };
            if path == MARKER_LOCK_FILE || path.starts_with(".index/") {
                continue;
            }
            match code {
                "??" => loose.push(("untracked", path.to_string())),
                c if !c.starts_with(' ') => {
                    staged.push(path.to_string());
                    if let Some(from) = renamed_from {
                        staged.push(from.to_string());
                    }
                }
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
        let snapshot = read_snapshot(git_dir)?;
        // An untracked directory is one `dir/` line. A new one is the
        // crash's: name the files in it, as the per-file listing did.
        // One already in the snapshot cannot be told apart from a crashed
        // writer's new file inside it by the listing; files modified
        // since the snapshot can.
        let mut inside = Vec::new();
        let mut whole = Vec::new();
        let taken = snapshot_time(git_dir);
        for (kind, p) in &tree.loose {
            if *kind != "untracked" || !p.ends_with('/') {
                continue;
            }
            if !snapshot.contains(p) {
                // A new directory is the crash's: block all of it.
                files_since(&self.dir, p, std::time::UNIX_EPOCH, &mut inside);
                whole.push(p.clone());
            } else if let Some(since) = taken {
                if files_since(&self.dir, p, since, &mut inside) {
                    whole.push(p.clone());
                }
            }
        }
        Ok(classify(tree, &snapshot, &inside, &whole))
    }

    /// One acquisition attempt. `declared` (repo-relative or absolute
    /// paths, same convention as [`Pm::commit`](super::Pm::commit)) is
    /// `Some` for a scoped write ([`Pm::lock_for_paths`](super::Pm::lock_for_paths)):
    /// when a reused marker reveals an interrupted write, a scoped
    /// write disjoint from the leftovers is admitted with a scope its
    /// commit must stay inside; an overlapping write — and every
    /// unscoped one — is refused. `None` is today's global behavior.
    ///
    /// `queued` is a kernel lock a parked waiter already won (see
    /// [`Waiter`]): it is used as the lock taken, in place of a fresh
    /// non-blocking `flock`.
    fn attempt(&self, declared: Option<&[PathBuf]>, queued: Option<Unlock>) -> Result<Attempt> {
        let git_dir = self.lock_git_dir().map_err(|e| self.git_dir_error(e))?;
        let flock_path = git_dir.join(FLOCK_FILE);
        // From here every exit releases the lock explicitly.
        let file = match queued {
            Some(held) => held,
            None => {
                let file = open_coordination(&flock_path, true)
                    .map_err(|e| io_unknown(&flock_path, "the coordination file", e))?
                    .expect("created");
                match flock(&file, libc::LOCK_EX) {
                    Ok(true) => {}
                    Ok(false) => return Ok(Attempt::Held),
                    Err(e) => return Err(io_unknown(&flock_path, "flock", e)),
                }
                Unlock(file)
            }
        };
        // The kernel lock is ours, so any leaked temp marker is stale.
        sweep_tmp(&git_dir);
        let legacy = self.dir.join(MARKER_LOCK_FILE);
        // Publish the marker atomically: a crash can never leave an
        // empty `.write.lock` that would read as a legacy lock. The
        // content carries a diagnostic trailer (holder pid, host,
        // version, start time, binary); liveness still comes from the
        // flock alone.
        let tmp = git_dir.join(format!("{TMP_PREFIX}{}", std::process::id()));
        let tmp_file = create_exclusive(&tmp, &marker_content())
            .map_err(|e| io_unknown(&tmp, "the lock marker", e))?;
        let tmp_ident = tmp_file
            .metadata()
            .map(|m| (m.dev(), m.ino()))
            .map_err(|e| io_unknown(&tmp, "the lock marker", e))?;
        let linked = std::fs::hard_link(&tmp, &legacy);
        let _ = std::fs::remove_file(&tmp);
        let (ident, reused, stale_meta) = match linked {
            Ok(()) => (tmp_ident, false, None),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                match self.legacy_file() {
                    // Our own marker under a flock we now hold: the writer
                    // that left it is gone. Reuse it once the git state is
                    // shown free of its leftovers. Its trailer, where
                    // present, names the dead writer in a refusal.
                    Ok(Some((MarkerClass::Ours(meta), id))) => (id, true, meta),
                    Ok(_) => return Ok(Attempt::Legacy),
                    Err(e) => return Err(io_unknown(&legacy, ".write.lock", e)),
                }
            }
            Err(e) => return Err(io_unknown(&legacy, ".write.lock", e)),
        };
        // Declared paths as repo-relative strings, up front: a path
        // outside the tracker is a caller bug, refused like commit's.
        let declared_rel: Option<Vec<String>> =
            declared.map(|ps| self.rel_paths(ps)).transpose()?;
        // Dropping an unarmed lock leaves the stale marker in place, so
        // a refused attempt is re-checked by the next one.
        let mut lock = PmLock {
            legacy,
            ident,
            armed: !reused,
            scope: Scope::Full,
            since: Instant::now(),
            _flock: file,
        };
        let tree = self.tree_state(&git_dir)?;
        if reused {
            if let Some(i) = self.interruption(&git_dir, &tree)? {
                match &declared_rel {
                    // Unscoped writes keep today's global refusal.
                    None => return Err(self.interrupted_error(&i, stale_meta.as_ref())),
                    Some(admitted) => {
                        let overlap = i.global
                            || admitted
                                .iter()
                                .any(|a| i.paths.iter().any(|p| paths_overlap(a, p)));
                        if overlap {
                            return Err(self.interrupted_error(&i, stale_meta.as_ref()));
                        }
                        // Disjoint from the crash: admit, scoped. The
                        // commit must stay inside the declared paths.
                        // The crash's marker and snapshot stay exactly as
                        // the dead writer left them (unarmed, no snapshot
                        // rewrite): a scoped admission must not consume
                        // the tripwire a later writer still needs. The
                        // next writer re-classifies against the original
                        // snapshot, so only the crash's own leftovers —
                        // never this write's committed work — can refuse.
                        lock.scope = Scope::Paths {
                            admitted: admitted.clone(),
                            blocked: i.paths.clone(),
                        };
                        return Ok(Attempt::Got(lock));
                    }
                }
            }
            lock.armed = true;
        }
        // A scoped write on a clean tracker still commits scoped: the
        // admitted set is enforced even with nothing to avoid, so a
        // caller bug that commits outside its declaration refuses
        // instead of silently writing unscoped.
        if let Some(admitted) = declared_rel {
            if matches!(lock.scope, Scope::Full) {
                lock.scope = Scope::Paths {
                    admitted,
                    blocked: Vec::new(),
                };
            }
        }
        // Before any mutation: what was already dirty is not ours.
        write_snapshot(&git_dir, &tree)?;
        Ok(Attempt::Got(lock))
    }

    /// Take the writer lock, waiting up to `wait`. `declared` scopes
    /// the admission (see [`Self::attempt`]); `None` is the unscoped
    /// lock. The lease and actor fences are re-checked on every turn
    /// of the wait and again once the lock is ours: a kernel lock that
    /// frees up is not authority.
    pub(super) fn acquire(
        &self,
        wait: Option<Duration>,
        declared: Option<&[PathBuf]>,
    ) -> Result<Option<PmLock>> {
        self.fence_check()?;
        let deadline = wait.map(|w| Instant::now() + w);
        let mut retries = 0;
        let mut waiter: Option<Waiter> = None;
        loop {
            let queued = waiter.as_mut().and_then(Waiter::take);
            let busy = match self.attempt(declared, queued)? {
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
                Some(d) => {
                    // A live writer holds the kernel lock: queue behind
                    // it in the kernel instead of polling. A legacy
                    // fence file has no kernel lock to queue on, so it
                    // is polled as before.
                    if !busy && waiter.as_ref().is_none_or(Waiter::spent) {
                        waiter = self.park_waiter();
                    }
                    match waiter.as_mut().filter(|w| !w.spent()) {
                        Some(w) => {
                            w.wait(WAIT_SLICE.min(d.saturating_duration_since(Instant::now())))
                        }
                        None => std::thread::sleep(Duration::from_millis(50)),
                    }
                    self.fence_check()?;
                }
            }
        }
    }

    /// Start a helper that blocks in `flock(LOCK_EX)` on its own open
    /// file description. `None` when the coordination file cannot be
    /// opened, a thread cannot be started or [`MAX_PARKED`] helpers are
    /// already parked: the caller polls, and the next [`Self::attempt`]
    /// reports any real error.
    fn park_waiter(&self) -> Option<Waiter> {
        let git_dir = self.lock_git_dir().ok()?;
        let file = open_coordination(&git_dir.join(FLOCK_FILE), true)
            .ok()
            .flatten()?;
        if PARKED.fetch_add(1, Ordering::SeqCst) >= MAX_PARKED {
            PARKED.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("pm-lock-wait".into())
            .spawn(move || {
                // SAFETY: a valid descriptor owned by `file`.
                let got = loop {
                    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                        break true;
                    }
                    if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                        break false;
                    }
                };
                if got {
                    // A waiter that gave up has dropped `rx`: the send
                    // fails and the guard's drop releases the lock.
                    let _ = tx.send(Unlock(file));
                }
                PARKED.fetch_sub(1, Ordering::SeqCst);
            });
        if spawned.is_err() {
            PARKED.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Waiter {
            rx,
            won: None,
            spent: false,
        })
    }

    pub(super) fn acquire_default(&self) -> Result<PmLock> {
        self.acquire_for(WAIT)
    }

    pub(super) fn acquire_scoped_default(&self, declared: &[PathBuf]) -> Result<PmLock> {
        Ok(self
            .acquire(Some(WAIT), Some(declared))?
            .expect("a waiting acquire answers"))
    }

    pub(super) fn acquire_for(&self, wait: Duration) -> Result<PmLock> {
        Ok(self
            .acquire(Some(wait), None)?
            .expect("a waiting acquire answers"))
    }

    fn busy_error(&self, legacy: bool) -> Error {
        // A live writer is not evidence that authority changed: app
        // validation defers on busy rather than revoking approval.
        // Both refusals name the holder where the scan could — a
        // positive "no live holder found" included — so the operator
        // can tell waiting from clearing without a separate probe.
        let holder = self.refusal_holder(legacy);
        if legacy {
            // Not transient: this build never removes or ages out the
            // file, so a retry loop would spin forever (CAD-876). A
            // non-retryable gate with its own code sends the caller to
            // the rollout owner instead.
            Error::gate_coded(
                "legacy_write_lock",
                format!(
                    "PM dir is locked by a legacy write lock ({}) that this build did not \
                     write; an older cadence writer may be live, or may have \
                     crashed holding it ({holder}). This build \
                     will not write beside it and will not remove it, and retrying will \
                     not help. Do not delete it as routine: the rollout owner must \
                     clear it during a quiescent migration, after every pre-flock \
                     cadence process is stopped",
                    self.dir.join(MARKER_LOCK_FILE).display()
                ),
            )
        } else {
            Error::busy(format!(
                "PM dir is locked by another live writer (kernel lock on \
                 .git/cadence-write.flock); it frees when that process exits ({holder})"
            ))
        }
    }

    /// CAD-1189: true while a live process holds the write flock. A
    /// non-blocking probe that releases at once: no git status, no marker
    /// work, no fence check. Unreadable or absent counts as not held.
    pub(crate) fn write_lock_held(&self) -> bool {
        let Ok(dir) = self.lock_git_dir() else {
            return false;
        };
        match open_coordination(&dir.join(FLOCK_FILE), false) {
            Ok(Some(f)) => match flock(&f, libc::LOCK_EX) {
                Ok(true) => {
                    drop(Unlock(f));
                    false
                }
                Ok(false) => true,
                Err(_) => false,
            },
            _ => false,
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
                Ok(false) => {
                    return LockState::Held {
                        holder: self.live_holder(&f),
                    };
                }
                Err(e) => return LockState::IoUnknown(format!("{}: {e}", flock_path.display())),
            },
            Ok(None) => None,
            Err(e) => return LockState::IoUnknown(format!("{}: {e}", flock_path.display())),
        };
        match self.legacy_file() {
            Ok(Some((MarkerClass::Foreign, _))) => LockState::LegacyUnknown {
                holder: self.legacy_holder(),
            },
            Ok(Some((MarkerClass::Ours(_), _))) => {
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
    /// `meta` is the dead writer's marker trailer where it left one:
    /// diagnostic only, never authority.
    fn interrupted_error(&self, i: &Interruption, meta: Option<&MarkerMeta>) -> Error {
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
        let trailer = match meta {
            Some(m) => format!(
                " The crashed writer's marker said: {}.",
                describe_holder(&HolderScan::NoneFound, Some(m))
            ),
            None => String::new(),
        };
        Error::rejected(format!(
            "the tracker has an interrupted write: a writer exited without \
             finishing, leaving {}{tail}. Nothing was replayed or committed and \
             the lock itself is free.{foreign} The write's outcome is uncertain — check \
             `git -C {} status` and `cadence issue lint`, then commit or discard \
             those paths yourself; writes touching them resume once they are resolved, \
             while writes to disjoint paths (via lock_for_paths) proceed.{trailer}",
            named.join(", "),
            self.dir.display()
        ))
    }
}

#[cfg(test)]
mod cad1190_acceptance;

#[cfg(test)]
mod tests {
    //! CAD-1190: blocking wait, cheap snapshot, hold-time report. The
    //! CAD-852 crash proofs stay in `lock_tests`.
    use super::*;

    fn tracker() -> (tempfile::TempDir, Pm) {
        let dir = tempfile::tempdir().unwrap();
        let pm = Pm::init(&dir.path().join("pm")).unwrap();
        (dir, pm)
    }

    #[test]
    fn a_waiter_gets_the_lock_when_the_holder_releases_mid_wait() {
        let (_d, pm) = tracker();
        let held = pm.lock().unwrap();
        let t = Instant::now();
        std::thread::scope(|sc| {
            sc.spawn(|| {
                std::thread::sleep(Duration::from_millis(400));
                drop(held);
            });
            let got = pm.lock_for(Duration::from_secs(10)).unwrap();
            let waited = t.elapsed();
            assert!(waited >= Duration::from_millis(350), "waited {waited:?}");
            assert!(waited < Duration::from_secs(3), "waited {waited:?}");
            drop(got);
        });
    }

    #[test]
    fn a_waiter_gives_up_at_the_deadline_and_its_helper_frees_the_lock() {
        let (_d, pm) = tracker();
        let held = pm.lock().unwrap();
        let t = Instant::now();
        let err = pm.lock_for(Duration::from_millis(500)).unwrap_err();
        assert_eq!(err.code(), Some("resource_busy"), "{err}");
        assert!(err.to_string().contains("another live writer"), "{err}");
        let waited = t.elapsed();
        assert!(waited >= Duration::from_millis(450), "waited {waited:?}");
        assert!(waited < Duration::from_secs(3), "waited {waited:?}");
        // The abandoned helper wins the lock when the holder lets go and
        // must give it straight back.
        drop(held);
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(l) = pm.try_lock().unwrap() {
                drop(l);
                break;
            }
            assert!(Instant::now() < end, "the abandoned helper kept the lock");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_hold_over_one_second_reports_held_ms_pid_and_paths() {
        let (_d, pm) = tracker();
        HOLD_LINES.with(|l| l.borrow_mut().clear());
        drop(pm.lock().unwrap());
        assert!(
            HOLD_LINES.with(|l| l.borrow().is_empty()),
            "short hold reported"
        );
        let lock = pm.lock_for_paths(&[pm.dir.join("proj")]).unwrap();
        std::thread::sleep(HOLD_REPORT + Duration::from_millis(100));
        drop(lock);
        let lines = HOLD_LINES.with(|l| l.borrow().clone());
        assert_eq!(lines.len(), 1, "{lines:?}");
        let line = &lines[0];
        assert!(line.contains("held_ms="), "{line}");
        assert!(
            line.contains(&format!("pid={}", std::process::id())),
            "{line}"
        );
        assert!(line.contains("paths=proj"), "{line}");
    }

    /// The acquire snapshot lists an untracked directory as one entry.
    /// A crashed writer's new file inside a directory that was already
    /// untracked must still read as its leftover; the old file must not.
    #[test]
    fn a_crash_leftover_inside_a_foreign_untracked_dir_still_refuses() {
        let (_d, pm) = tracker();
        std::fs::create_dir_all(pm.dir.join("foreign")).unwrap();
        std::fs::write(pm.dir.join("foreign/old.txt"), "mine\n").unwrap();
        std::thread::sleep(Duration::from_millis(30));
        let mut lock = pm.lock().unwrap();
        std::thread::sleep(Duration::from_millis(30));
        // Dying here leaves the fence file behind: a crashed writer.
        lock.armed = false;
        drop(lock);
        let clean = pm.lock().unwrap();
        // The foreign file alone is not a leftover: that lock was
        // admitted. Now the crash adds a file inside the foreign dir.
        std::thread::sleep(Duration::from_millis(30));
        let mut clean = clean;
        clean.armed = false;
        std::fs::write(pm.dir.join("foreign/half-written.md"), "x\n").unwrap();
        drop(clean);
        let err = pm.lock().unwrap_err();
        assert!(err.to_string().contains("foreign/half-written.md"), "{err}");
        assert!(!err.to_string().contains("foreign/old.txt"), "{err}");
    }
}
