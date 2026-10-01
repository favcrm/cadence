//! Selected Git bundle resolver (CAD-970 R3 foundation).
//!
//! A *selected* source is a credential-free HTTPS repository URL, an
//! exact 40-hex commit, and a canonical relative bundle directory —
//! all required, all validated at declaration, before `git` is ever
//! invoked. [`resolve`] clones the repository into an owned temporary
//! directory, verifies the selected commit is a commit, checks it out
//! detached, proves `HEAD` is that same commit, and returns a bounded
//! snapshot of the selected bundle directory — `rel-path → UTF-8 text`
//! — plus the source provenance.
//!
//! This is a standalone library operation, NOT an install path: it
//! takes no actor/context, writes no record or journal, grants no
//! approval, and performs no manifest/workflow validation, admission
//! or digest — those stay the caller's responsibility. The result is
//! an owned in-memory snapshot; no `PathBuf` is handed back for a
//! caller (or a co-host writer) to reopen.
//!
//! **Process boundary.** Every git step runs as
//! `timeout --kill-after=<K> <T> git -c core.hooksPath=/dev/null -c
//! core.fsmonitor=false …` through [`crate::reaper::output`], which
//! does NOT itself bound the child — the separate `timeout` argv
//! carries the fixed wall-clock deadline, and `--kill-after` is the
//! finite TERM→KILL escalation so a stuck step always ends. The
//! `timeout` binary is the same deployment dependency the legacy app
//! installer already requires; when it is absent the step fails, it
//! never falls back to an unbounded run. The clone is FULL
//! (`--no-checkout --no-tags`, no `--depth`): any selected commit the
//! repo history carries resolves, and `rev-parse --verify
//! <sha>^{commit}`, `checkout --detach <sha>` and an independent
//! `rev-parse HEAD` pin it — a missing commit refuses, it never falls
//! back to `HEAD` or a near-match.
//!
//! **Environment isolation.** Each step's `timeout` process gets an
//! `env_clear`'d environment carrying only the caller's `PATH` (so
//! `git` resolves the way the caller's own shell resolves it) plus
//! pinned values git inherits: a fresh per-step owned `HOME`,
//! `XDG_CONFIG_HOME`, `GIT_CONFIG_GLOBAL` (an empty owned file),
//! `GIT_TEMPLATE_DIR` (an empty owned directory), `GIT_CONFIG_NOSYSTEM=1`,
//! `GIT_TERMINAL_PROMPT=0`, `GIT_ALLOW_PROTOCOL=https` and
//! `GIT_LFS_SKIP_SMUDGE=1`. Inherited `GIT_DIR`, `GIT_WORK_TREE`,
//! `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`,
//! `GIT_CONFIG_*` count/parameter injection, credential helpers, SSH
//! and proxy commands therefore cannot reach the child; hooks are off
//! and only the HTTPS transport can run. No shell is involved — argv
//! stays argv.
//!
//! `PATH` is a trusted caller input: this module does not authenticate
//! executables found there. Clone disk/network bytes and captured git
//! output are not byte-bounded; the content caps apply to the snapshot.
//! The temporary-directory path must have no symlinked ancestors;
//! paths such as macOS's `/var/...` alias refuse rather than follow it.
//!
//! **Snapshot containment.** The clone root and the selected
//! directory's every component are opened descriptor-relative with
//! `O_NOFOLLOW`/`O_DIRECTORY`; children are listed through those
//! anchored handles and every member is `fstatat(AT_SYMLINK_NOFOLLOW)`-
//! checked, then opened `O_NOFOLLOW|O_NONBLOCK` and `fstat`ed a real
//! regular file BEFORE any byte is read — a symlink ancestor,
//! directory or member refuses, a FIFO refuses without ever blocking,
//! and a swap between listing and open loses to the post-open `fstat`.
//! No `symlink_metadata`-then-`PathBuf` rejoin and no `canonicalize`.
//! The shape and bounds are the v0 bundle shape `app.rs` enforces:
//! `app.md` plus flat `workflows/`/`rubrics/`/`templates/` only —
//! nothing else, no dotfiles, no nested dirs — at most 128 files,
//! each ≤ 256 KiB (`plan::MAX_PLAN_BYTES`), ≤ 2 MiB aggregate
//! (`app::MAX_APP_BYTES`), read through `take(MAX_FILE_BYTES + 1)` so
//! no unbounded allocation happens before the bound is proved.

use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};
use std::process::{Command, Output};

use tempfile::TempDir;

use crate::error::{Error, Result};
use crate::issue::{app, model, plan};

/// Largest URL a selection may carry, bytes.
const MAX_URL_BYTES: usize = 2048;

/// Largest bundle-directory path a selection may carry, bytes.
const MAX_DIR_BYTES: usize = 256;

/// The fixed wall-clock deadline of EVERY git step, enforced by the
/// `timeout` binary — `reaper::output` waits but does not bound, so
/// this separate argv is what makes each step finite. A public clone
/// that cannot finish inside it is refused, not retried with more.
const GIT_STEP_SECS: &str = "120";

/// How long a timed-out step's TERM gets before `timeout` escalates
/// to SIGKILL — a finite kill-after on every git step.
const GIT_KILL_AFTER_SECS: &str = "10";

/// The exit status `timeout` reports for a deadline kill (TERM) and
/// for the `--kill-after` escalation (SIGKILL → 128 + 9).
const TIMEOUT_TERM_CODE: i32 = 124;
const TIMEOUT_KILL_CODE: i32 = 137;

/// The v0 bundle shape — the same allowlist `app.rs` pins privately:
/// `app.md` plus flat `workflows/`, `rubrics/`, `templates/` dirs.
/// Kept identical by inspection parity, never expanded here.
const TOP_DIRS: &[&str] = &["workflows", "rubrics", "templates"];

/// Most files a bundle may carry — pinned at `app.rs`'s private
/// `MAX_FILES`.
const MAX_FILES: usize = 128;

/// Largest single file — `app.rs` defines its private `MAX_FILE_BYTES`
/// as exactly this.
const MAX_FILE_BYTES: u64 = plan::MAX_PLAN_BYTES as u64;

/// Largest bundle, all files together — `app.rs`'s crate-visible cap.
const MAX_APP_BYTES: u64 = app::MAX_APP_BYTES;

/// A checked selection: credential-free HTTPS repository URL, exact
/// lowercase-hex commit, canonical relative bundle directory. Every
/// field is validated by [`SelectedGitSource::new`] — an invalid one
/// refuses there, before any process exists.
#[derive(Clone, Debug)]
pub struct SelectedGitSource {
    url: String,
    commit: String,
    dir: String,
}

/// One resolved bundle: the selected directory's files as inert UTF-8
/// text keyed by bundle-relative path, plus the verbatim source
/// provenance. The clone's [`TempDir`] is held privately for its own
/// RAII cleanup; there is deliberately no path a caller can reopen.
#[derive(Debug)]
pub struct ResolvedGitBundle {
    pub files: BTreeMap<String, String>,
    pub url: String,
    pub commit: String,
    pub dir: String,
    /// Owns the clone for as long as the bundle lives; the bytes are
    /// already out, so the field is never read — only dropped.
    #[allow(dead_code)]
    tempdir: TempDir,
}

impl SelectedGitSource {
    /// Validate `url`, `commit` and `dir` into a checked selection.
    /// No filesystem or process touch — a refusal happens here, before
    /// `git` is ever invoked.
    pub fn new(url: &str, commit: &str, dir: &str) -> Result<Self> {
        check_url(url)?;
        check_commit(commit)?;
        check_dir(dir)?;
        Ok(Self {
            url: url.to_string(),
            commit: commit.to_string(),
            dir: dir.to_string(),
        })
    }
}

/// The URL is a plain public HTTPS repository URL: exact `https://`
/// scheme, a nonempty `[A-Za-z0-9.-]` host with an optional `:port`,
/// and a nonempty repository path of visible ASCII. No userinfo (`@`),
/// no query or fragment, no percent (credential or traversal aliases
/// are never decoded), no controls, backslash, or non-ASCII bytes, and
/// no scheme that could mean an executable transport.
fn check_url(url: &str) -> Result<()> {
    if url.is_empty() || url.len() > MAX_URL_BYTES {
        return Err(Error::rejected(format!(
            "git source url is empty or over {MAX_URL_BYTES} bytes"
        )));
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return Err(Error::rejected(
            "git source url must be https://… — credential-free public HTTPS only",
        ));
    };
    for b in rest.bytes() {
        if !(0x21..0x7f).contains(&b) || matches!(b, b'%' | b'@' | b'?' | b'#' | b'\\') {
            return Err(Error::rejected(
                "git source url carries a control, space, percent, userinfo, query, \
                 fragment or backslash — it is a plain https://host/path",
            ));
        }
    }
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, p),
        None => (rest, ""),
    };
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
    {
        return Err(Error::rejected(
            "git source url needs a nonempty hostname of [A-Za-z0-9.-]",
        ));
    }
    if let Some(port) = port {
        if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::rejected("git source url port must be digits only"));
        }
    }
    if path.is_empty() {
        return Err(Error::rejected(
            "git source url needs a nonempty repository path — https://host/repo",
        ));
    }
    Ok(())
}

/// The commit is exactly 40 lowercase hex — never a branch, tag,
/// expression or near-match.
fn check_commit(commit: &str) -> Result<()> {
    if commit.len() != 40
        || !commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::rejected(
            "git source commit must be exactly 40 lowercase hex characters",
        ));
    }
    Ok(())
}

/// The bundle directory is a canonical relative path: `/`-separated
/// `[A-Za-z0-9_-]+` components only — no empty, dot, absolute,
/// backslash, percent, control or non-ASCII components — ≤256 bytes.
fn check_dir(dir: &str) -> Result<()> {
    if dir.is_empty() || dir.len() > MAX_DIR_BYTES {
        return Err(Error::rejected(format!(
            "git source dir is empty or over {MAX_DIR_BYTES} bytes"
        )));
    }
    for component in dir.split('/') {
        if component.is_empty()
            || !component
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(Error::rejected(
                "git source dir is a canonical relative path of [A-Za-z0-9_-]+ \
                 components — no empty, dot, absolute, backslash, percent, \
                 control or non-ASCII segments",
            ));
        }
    }
    Ok(())
}

/// Resolve a checked selection into its owned bundle snapshot. Runs
/// clone → commit-verify → detached checkout → HEAD-verify under the
/// bounded, scrubbed environment, then snapshots the selected
/// directory descriptor-relative. Every refusal is a plain `Err`; no
/// record, journal or authority is touched.
pub fn resolve(source: &SelectedGitSource) -> Result<ResolvedGitBundle> {
    let tmp = tempfile::tempdir()?;
    let clone_dir = tmp.path().join("clone");

    // Full clone — no depth, no tags, no checkout yet.
    git_step(tmp.path(), "clone", |cmd| {
        cmd.arg("clone")
            .arg("--quiet")
            .arg("--no-checkout")
            .arg("--no-tags")
            .arg("--")
            .arg(&source.url)
            .arg(&clone_dir);
    })?;

    // The selected object must be a commit and exactly the requested SHA.
    let verify = git_step(tmp.path(), "verify", |cmd| {
        cmd.arg("-C")
            .arg(&clone_dir)
            .arg("rev-parse")
            .arg("--verify")
            .arg(format!("{}^{{commit}}", source.commit));
    })?;
    let found = String::from_utf8_lossy(&verify.stdout).trim().to_string();
    if found != source.commit {
        return Err(Error::rejected(format!(
            "selected commit {} did not resolve to itself in the clone \
             (`rev-parse` answered '{}') — refusing rather than substituting \
             another object",
            source.commit, found
        )));
    }

    // Detached checkout of exactly that commit.
    git_step(tmp.path(), "checkout", |cmd| {
        cmd.arg("-C")
            .arg(&clone_dir)
            .arg("checkout")
            .arg("--detach")
            .arg(&source.commit);
    })?;

    // Independently prove HEAD is the selected commit — never trust
    // the checkout step's silence.
    let head = git_step(tmp.path(), "head", |cmd| {
        cmd.arg("-C").arg(&clone_dir).arg("rev-parse").arg("HEAD");
    })?;
    let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
    if head != source.commit {
        return Err(Error::rejected(format!(
            "checkout left HEAD at {head}, not the selected commit {} — refusing",
            source.commit
        )));
    }

    // Snapshot the selected directory anchored on its own descriptors.
    let clone = open_dir_path(&clone_dir)?;
    let mut dir_fd = clone;
    for component in source.dir.split('/') {
        dir_fd = open_dir_at(&dir_fd, component.as_bytes()).map_err(|e| {
            Error::rejected(format!(
                "selected bundle directory '{}' is not reachable as real \
                 directories — component '{component}': {e}",
                source.dir
            ))
        })?;
    }
    let files = snapshot_dir(&dir_fd)?;
    Ok(ResolvedGitBundle {
        files,
        url: source.url.clone(),
        commit: source.commit.clone(),
        dir: source.dir.clone(),
        tempdir: tmp,
    })
}

/// One bounded, environment-scrubbed git step:
/// `timeout --kill-after=K T git -c core.hooksPath=/dev/null -c
/// core.fsmonitor=false <args>` through [`crate::reaper::output`].
/// `timeout`/`git` are separate argv words — no shell — and the
/// isolated environment is applied to the `timeout` process and
/// inherited by git. Every step gets its own fresh empty `env/<step>/`
/// holding HOME, XDG_CONFIG_HOME, the empty global-config file and the
/// empty template dir under `tmp`, so no step inherits caller state or
/// leaves git state for the next.
fn git_step(tmp: &Path, step: &str, args: impl FnOnce(&mut Command)) -> Result<Output> {
    let env_dir = tmp.join("env").join(step);
    let (home, xdg, template, global) = (
        env_dir.join("home"),
        env_dir.join("xdg"),
        env_dir.join("template"),
        env_dir.join("gitconfig"),
    );
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&xdg)?;
    std::fs::create_dir_all(&template)?;
    std::fs::write(&global, "")?;

    let mut cmd = Command::new("timeout");
    cmd.arg(format!("--kill-after={GIT_KILL_AFTER_SECS}"))
        .arg(GIT_STEP_SECS)
        .arg("git")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("core.fsmonitor=false");
    args(&mut cmd);
    cmd.env_clear()
        // The caller's own PATH is preserved so `timeout` and `git`
        // resolve exactly as the caller's shell resolves them — the
        // one thing the environment carries over deliberately.
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &xdg)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &global)
        .env("GIT_TEMPLATE_DIR", &template)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ALLOW_PROTOCOL", "https")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("LC_ALL", "C")
        .current_dir(&env_dir);
    let out = crate::reaper::output(&mut cmd).map_err(|e| {
        Error::rejected(format!(
            "git {step} could not start (`timeout`/`git` must be on PATH — \
             there is no unbounded fallback): {e}"
        ))
    })?;
    if !out.status.success() {
        let code = out.status.code().unwrap_or(-1);
        let tail = tail_str(stderr_text(&out), 2000);
        if code == TIMEOUT_TERM_CODE || code == TIMEOUT_KILL_CODE {
            return Err(Error::rejected(format!(
                "git {step} exceeded the {GIT_STEP_SECS}s step deadline \
                 (status {code}): {tail}"
            )));
        }
        return Err(Error::rejected(format!(
            "git {step} failed (status {code}): {tail}"
        )));
    }
    Ok(out)
}

fn stderr_text(out: &Output) -> &str {
    std::str::from_utf8(&out.stderr).unwrap_or("<non-UTF-8 stderr>")
}

/// The last `max` bytes of a possibly large stderr, char-boundary safe.
fn tail_str(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

// ---------------------------------------------------------------------
// Descriptor-anchored snapshot — O_NOFOLLOW everywhere, never a
// `PathBuf` rejoin. Mirrors `app_catalog::fs`'s libc pattern.
// ---------------------------------------------------------------------

const DIRECTORY: i32 = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC;
const FILE: i32 = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;

fn cname(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bundle path contains a NUL"))
}

fn open_at(parent: &File, name: &[u8], flags: i32) -> io::Result<File> {
    let name = cname(name)?;
    // SAFETY: plain openat; the fd is checked before it is wrapped.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_dir_at(parent: &File, name: &[u8]) -> io::Result<File> {
    open_at(parent, name, DIRECTORY)
}

/// Open `path` as a real directory, walking its components from the
/// root anchor with `O_NOFOLLOW`/`O_DIRECTORY` — a swapped or linked
/// ancestor refuses rather than being followed.
fn open_dir_path(path: &Path) -> Result<File> {
    let start = if path.is_absolute() { "/" } else { "." };
    let c = cname(start.as_bytes())?;
    // SAFETY: plain open; the fd is checked before it is wrapped.
    let fd = unsafe { libc::open(c.as_ptr(), DIRECTORY) };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let mut dir = unsafe { File::from_raw_fd(fd) };
    for part in path.components() {
        match part {
            Component::Normal(n) => dir = open_dir_at(&dir, n.as_bytes())?,
            Component::RootDir | Component::CurDir => {}
            _ => return Err(Error::rejected("clone path carries an unsafe component")),
        }
    }
    Ok(dir)
}

/// The `fstatat(AT_SYMLINK_NOFOLLOW)` kind of `name` under `parent`.
fn kind_at(parent: &File, name: &[u8]) -> io::Result<libc::mode_t> {
    let name = cname(name)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the kernel writes `stat` through the pointer.
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { stat.assume_init() }.st_mode & libc::S_IFMT)
}

/// Every entry name of `dir` (opened through a fresh `.` dup so a
/// prior read's readdir cursor is never inherited), skipping `.`/`..`.
fn list_dir(dir: &File) -> Result<Vec<Vec<u8>>> {
    struct Owned(*mut libc::DIR);
    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: created by `fdopendir` in `list_dir`, closed once.
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let dup = open_dir_at(dir, b".")?;
    let fd = dup.as_raw_fd();
    std::mem::forget(dup);
    // SAFETY: `fd` is a live directory fd `fdopendir` takes ownership of.
    let raw = unsafe { libc::fdopendir(fd) };
    if raw.is_null() {
        // SAFETY: fdopendir failed, so `fd` is still ours to close.
        unsafe {
            libc::close(fd);
        }
        return Err(io::Error::last_os_error().into());
    }
    let dir = Owned(raw);
    let mut names = Vec::new();
    loop {
        #[cfg(target_os = "linux")]
        let errno = unsafe { libc::__errno_location() };
        #[cfg(target_os = "macos")]
        let errno = unsafe { libc::__error() };
        // SAFETY: __errno_location/__error return a live thread-local.
        unsafe {
            *errno = 0;
        }
        // SAFETY: `dir.0` is a live DIR*.
        let next = unsafe { libc::readdir(dir.0) };
        if next.is_null() {
            // SAFETY: the errno cell above is still valid.
            if unsafe { *errno } != 0 {
                return Err(io::Error::last_os_error().into());
            }
            break;
        }
        // SAFETY: `readdir` returned a valid dirent; d_name is a CStr.
        let bytes = unsafe { CStr::from_ptr((*next).d_name.as_ptr()) }.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        names.push(bytes.to_vec());
        if names.len() > MAX_FILES * 4 {
            return Err(Error::rejected(
                "bundle directory carries implausibly many entries",
            ));
        }
    }
    names.sort();
    Ok(names)
}

/// Read `name` under `dir` as bounded UTF-8 text: opened `O_NOFOLLOW|
/// O_NONBLOCK` (a FIFO refuses instead of blocking a reader), then
/// `fstat`ed a regular file within the bound BEFORE any byte is read,
/// then read through `take(MAX_FILE_BYTES + 1)` — no unbounded
/// allocation ever precedes the bound.
fn read_bounded(dir: &File, name: &[u8], rel: &str) -> Result<String> {
    let file = open_at(dir, name, FILE)
        .map_err(|e| Error::rejected(format!("bundle entry '{rel}' cannot be opened: {e}")))?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(Error::rejected(format!(
            "bundle entry '{rel}' is not a regular file"
        )));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(Error::rejected(format!(
            "{rel} is {} bytes — a file is at most {MAX_FILE_BYTES}",
            meta.len()
        )));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(Error::rejected(format!(
            "{rel} is over {MAX_FILE_BYTES} bytes"
        )));
    }
    String::from_utf8(bytes).map_err(|_| {
        Error::rejected(format!(
            "{rel}: not UTF-8 text — a bundle carries text only"
        ))
    })
}

/// Snapshot one already-opened bundle directory: the v0 shape —
/// `app.md` plus flat `workflows/`/`rubrics/`/`templates/` — under
/// the pinned file-count, per-file and aggregate bounds, every member
/// reached descriptor-relative with `O_NOFOLLOW`.
fn snapshot_dir(dir: &File) -> Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    let mut bytes = 0u64;
    let mut add = |dir: &File, name: &[u8], rel: String| -> Result<()> {
        if files.len() >= MAX_FILES {
            return Err(Error::rejected(format!(
                "bundle carries more than {MAX_FILES} files"
            )));
        }
        let text = read_bounded(dir, name, &rel)?;
        bytes += text.len() as u64;
        if bytes > MAX_APP_BYTES {
            return Err(Error::rejected(format!(
                "bundle is over {MAX_APP_BYTES} bytes of content"
            )));
        }
        files.insert(rel, text);
        Ok(())
    };

    let mut tops: Vec<Vec<u8>> = Vec::new();
    let mut manifest = false;
    for name in list_dir(dir)? {
        let text = std::str::from_utf8(&name)
            .map_err(|_| Error::rejected("bundle carries an entry name that is not UTF-8"))?;
        if text.starts_with('.') {
            return Err(Error::rejected(format!(
                "bundle entry '{text}': dotfiles are not app content"
            )));
        }
        match kind_at(dir, &name)? {
            libc::S_IFDIR if TOP_DIRS.contains(&text) => tops.push(name),
            libc::S_IFREG if text == app::MANIFEST => {
                manifest = true;
                add(dir, &name, text.to_string())?;
            }
            libc::S_IFLNK => {
                return Err(Error::rejected(format!(
                    "bundle entry '{text}' is a symlink — a bundle holds real \
                     files only"
                )))
            }
            _ => {
                return Err(Error::rejected(format!(
                    "bundle entry '{text}' — the v0 shape is app.md plus flat \
                     workflows/, rubrics/, templates/; everything else refuses"
                )))
            }
        }
    }
    if !manifest {
        return Err(Error::rejected(
            "selected directory has no app.md — the bundle manifest is required",
        ));
    }
    if !tops.iter().any(|n| n.as_slice() == b"workflows") {
        return Err(Error::rejected(
            "selected directory has no workflows/ — an app bundles workflows",
        ));
    }
    for top in tops {
        let top_text = std::str::from_utf8(&top)
            .map_err(|_| Error::rejected("bundle dir name is not UTF-8"))?;
        let sub = open_dir_at(dir, &top).map_err(|e| {
            Error::rejected(format!(
                "bundle directory '{top_text}' cannot be opened: {e}"
            ))
        })?;
        for leaf in list_dir(&sub)? {
            let leaf_text = std::str::from_utf8(&leaf).map_err(|_| {
                Error::rejected(format!("{top_text}/ carries a name that is not UTF-8"))
            })?;
            let rel = format!("{top_text}/{leaf_text}");
            if leaf_text.starts_with('.') {
                return Err(Error::rejected(format!(
                    "bundle entry '{rel}': dotfiles are not app content"
                )));
            }
            match kind_at(&sub, &leaf)? {
                libc::S_IFREG => {
                    if top_text == "workflows" {
                        let Some(stem) = leaf_text.strip_suffix(".md") else {
                            return Err(Error::rejected(format!(
                                "bundle entry '{rel}': workflows are plan-template \
                                 files ending in .md"
                            )));
                        };
                        if !model::valid_tag(stem) {
                            return Err(Error::rejected(format!(
                                "bundle entry '{rel}': the workflow name '{stem}' is \
                                 not tag-shaped"
                            )));
                        }
                    }
                    add(&sub, &leaf, rel)?;
                }
                libc::S_IFDIR => {
                    return Err(Error::rejected(format!(
                        "bundle entry '{rel}': v0 bundle dirs are flat — no nested \
                         folders"
                    )))
                }
                libc::S_IFLNK => {
                    return Err(Error::rejected(format!(
                        "bundle entry '{rel}' is a symlink — a bundle holds real \
                         files only"
                    )))
                }
                _ => {
                    return Err(Error::rejected(format!(
                        "bundle entry '{rel}' is not a regular file"
                    )))
                }
            }
        }
    }
    Ok(files)
}
