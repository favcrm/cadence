//! No-follow file mutations, index materialization, and git commit helpers.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

use super::facts::{git_dir_of, git_raw};
use crate::error::{Error, Result};
use crate::fd_fs::DirFd;
use crate::issue::git;
use crate::issue::Pm;

/// Commit the INDEX state of `paths` — not the working tree.
///
/// A keep must commit exactly what the crashed writer staged. The
/// ordinary commit path stages the working tree's content first, which
/// would silently commit the unstaged version instead, so the recovery
/// builds the tree from the index itself, in a temporary index, and
/// advances HEAD only if it is still the reviewed one.
pub(super) fn commit_index_state(pm: &Pm, paths: &[String], message: &str) -> Result<String> {
    // The audit trailers an ordinary tracker commit carries: the actor
    // (this recovery ran on an operator connection) and the lease
    // epoch, so a reader can order this commit against the others
    // across a restart or a holdover. The exact-index commit bypasses
    // [`Pm::commit`], so it has to name them itself.
    let mut full = format!(
        "{message}\n\nActor: {}\n",
        crate::issue::write::actor_who("operator", None)
    );
    if let Some(epoch) = pm.lease_epoch() {
        full.push_str(&format!("Lease-Epoch: {epoch}\n"));
    }
    let git_dir = git_dir_of(pm)?;
    let head = git(&pm.dir, &["rev-parse", "HEAD"])?;
    let tmp_index = git_dir.join(format!("cadence-recovery-index-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp_index);
    let env = [("GIT_INDEX_FILE", tmp_index.as_os_str())];
    // The commit's tree: HEAD plus the selected index entries.
    run_git(pm, &env, &[Os::new("read-tree"), Os::new(&head)], None)?;
    for path in paths {
        let entry = git(&pm.dir, &["ls-files", "--stage", "--", path])?;
        if entry.trim().is_empty() {
            // Not in the index: a staged deletion, or a path that only
            // exists in the working tree. It must not enter the commit.
            run_git(
                pm,
                &env,
                &[
                    Os::new("update-index"),
                    Os::new("--force-remove"),
                    Os::new("--"),
                    Os::new(path),
                ],
                None,
            )?;
        } else {
            run_git(
                pm,
                &env,
                &[Os::new("update-index"), Os::new("--index-info")],
                Some(entry.as_bytes()),
            )?;
        }
    }
    // The tree this commit would carry, before the hook runs.
    let expected = run_git(pm, &env, &[Os::new("write-tree")], None)?;
    // The pre-commit hook, exactly as git would run it, against the
    // content this commit is about to carry.
    run_pre_commit_hook(pm, &git_dir, &tmp_index)?;
    // A hook may change the index — a formatter that stages, a hook
    // that adds a path. The commit must carry exactly what was
    // reviewed: HEAD plus the confirmed staged entries, and nothing
    // else. So the tree is compared, not trusted.
    let tree = run_git(pm, &env, &[Os::new("write-tree")], None)?;
    if tree != expected {
        return Err(Error::rejected(
            "the tracker's pre-commit hook changed the content that was about to be committed. \
             Nothing was committed. Review again so the confirmed content is what the hook \
             sees; a retry unchanged will not help.",
        ));
    }
    let commit = run_git(
        pm,
        &[],
        &[
            Os::new("-c"),
            Os::new("user.name=cadence"),
            Os::new("-c"),
            Os::new("user.email=cadence@localhost"),
            Os::new("commit-tree"),
            Os::new(&tree),
            Os::new("-p"),
            Os::new(&head),
            Os::new("-m"),
            Os::new(&full),
        ],
        None,
    )?;
    // HEAD advances only if it is still the reviewed one. A child killed
    // after committing the ref but before reporting leaves the outcome
    // UNKNOWN: the ref is re-read rather than trusting the exit status.
    if let Err(e) = run_git(
        pm,
        &[],
        &[
            Os::new("update-ref"),
            Os::new("HEAD"),
            Os::new(&commit),
            Os::new(&head),
        ],
        None,
    ) {
        let now = git(&pm.dir, &["rev-parse", "HEAD"]).unwrap_or_default();
        if now.trim() == commit.trim() {
            return Ok(commit);
        }
        return Err(Error::unknown(format!(
            "the recovery commit {} was created but HEAD could not be advanced ({e}); HEAD is \
             {}. The outcome is UNKNOWN: the commit may or may not be referenced. Inspect the \
             tracker before doing anything else; a retry is not safe.",
            commit.trim(),
            now.trim()
        )));
    }
    let _ = std::fs::remove_file(&tmp_index);
    Ok(commit)
}

/// Open the parent directory of `path` inside the tracker, walking
/// every component with `O_NOFOLLOW`.
///
/// The recovery must not write through a symlinked ancestor: a symlink
/// swapped in between the binding check and the mutation would take the
/// write outside the tracker. Each component is therefore opened
/// without following links, and the last one is returned as an open
/// directory descriptor together with the final name.
fn open_parent_no_follow(root: &Path, path: &str) -> std::io::Result<(DirFd, std::ffi::CString)> {
    use std::ffi::CString;
    let bad = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the path is not a literal path inside the tracker",
        )
    };
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Err(bad());
    }
    let mut dir = DirFd::open_nofollow(root).map_err(|err| {
        if err.kind() == std::io::ErrorKind::InvalidInput {
            bad()
        } else {
            err
        }
    })?;
    for part in &parts[..parts.len() - 1] {
        let c = CString::new(part.as_bytes()).map_err(|_| bad())?;
        dir = match dir.open_child_dir(&c) {
            Ok(next) => next,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // The crash removed the directory. The committed state has
                // it, so it is recreated — every existing component was
                // already opened without following a link.
                dir.mkdir(&c, 0o755)?;
                dir.open_child_dir(&c)?
            }
            Err(err) => return Err(err),
        };
    }
    let name = CString::new(parts[parts.len() - 1].as_bytes()).map_err(|_| bad())?;
    Ok((dir, name))
}

/// Remove a path inside the tracker without following a symlinked
/// ancestor: the removal is an `unlinkat` on a directory descriptor
/// opened with `O_NOFOLLOW`, so it can only ever remove the file the
/// path names inside the tracker.
pub(super) fn remove_file_no_follow(root: &Path, path: &str) -> std::io::Result<()> {
    let (dir, name) = open_parent_no_follow(root, path)?;
    dir.unlink_child(&name)
}

/// Write a path inside the tracker without following a symlinked
/// ancestor, and without truncating a file the path merely shares an
/// inode with.
///
/// The bytes go to a fresh temp file in the same directory (opened
/// `O_CREAT | O_EXCL | O_NOFOLLOW`), the mode is set on that
/// descriptor, and the temp is renamed over the path. A rename
/// replaces the directory entry: a regular file that shares its inode
/// with another path (a hard link) keeps that other path's bytes
/// intact, and no pathname is resolved again between the write and the
/// mode change.
fn write_file_no_follow(root: &Path, path: &str, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let invalid = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the path is not a literal path inside the tracker",
        )
    };
    let (dir, name) = open_parent_no_follow(root, path)?;
    let tmp = CString::new(format!(
        ".cadence-recovery-{}-{}",
        std::process::id(),
        unique_suffix()
    ))
    .map_err(|_| invalid())?;
    let mut handle = dir.open_file_nofollow(
        &tmp,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        mode as libc::mode_t,
    )?;
    let written = handle
        .write_all(bytes)
        .and_then(|_| handle.sync_all())
        .and_then(|_| {
            // The mode is set on the descriptor (`fchmod`): no pathname is
            // resolved, so a symlink swapped in cannot redirect it.
            handle.set_permissions(std::fs::Permissions::from_mode(mode))
        });
    if let Err(e) = written {
        drop(handle);
        let _ = dir.unlink_child(&tmp);
        return Err(e);
    }
    drop(handle);
    // A rename replaces the directory entry, so a hard-linked sibling
    // keeps its bytes, and it never follows the target.
    if let Err(e) = dir.rename_child(&tmp, &name) {
        let _ = dir.unlink_child(&tmp);
        return Err(e);
    }
    Ok(())
}

/// A suffix unique per process and call, for the temp file the
/// replacement is built in.
fn unique_suffix() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        + u128::from(n)
}

/// Restore the selected paths to their committed state: the index entry
/// and the working tree's content both go back to HEAD. The working
/// tree's copy is written WITHOUT following a symlinked ancestor, so a
/// symlink swapped in after the review cannot take the write outside
/// the tracker.
pub(super) fn discard_paths(pm: &Pm, paths: &[String]) -> Result<()> {
    for path in paths {
        // Membership in the committed TREE, not object readability: a
        // tree entry whose blob cannot be read must fail, never fall
        // through to the "added file" arm that deletes it.
        let entry = git(&pm.dir, &["ls-tree", "HEAD", "--", path])?
            .trim()
            .to_string();
        if !entry.is_empty() {
            let mode = entry
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string();
            if !mode.starts_with("100") {
                return Err(Error::rejected(format!(
                    "{path} is a {mode} entry in the committed tree, not a regular file — a \
                     discard restores regular files only. Restore it by hand and leave this \
                     path unresolved. A retry unchanged will not help."
                )));
            }
            // The committed bytes, read BEFORE the index is touched: an
            // unreadable blob leaves both layers unchanged.
            let content =
                git_raw(pm, &["cat-file", "blob", &format!("HEAD:{path}")]).map_err(|e| {
                    Error::internal(format!(
                        "the committed content of {path} could not be read ({e}). Nothing was \
                         changed: repair the object store, then review again."
                    ))
                })?;
            let wanted = if mode.ends_with("755") {
                0o755u32
            } else {
                0o644u32
            };
            run_git(
                pm,
                &[],
                &[
                    Os::new("restore"),
                    Os::new("--source=HEAD"),
                    Os::new("--staged"),
                    Os::new("--"),
                    Os::new(path),
                ],
                None,
            )?;
            write_file_no_follow(&pm.dir, path, &content, wanted).map_err(|e| {
                Error::internal(format!(
                    "the working-tree copy of {path} could not be restored from its committed \
                     version ({e}); the index entry is back and the change is PARTLY applied. \
                     Nothing was committed, the crash's marker and snapshot are intact, and \
                     the file must be restored by hand before this path is left alone."
                ))
            })?;
        } else {
            // A file the crash added: remove the staged addition and
            // the working tree's copy, so both are absent.
            run_git(
                pm,
                &[],
                &[
                    Os::new("rm"),
                    Os::new("--cached"),
                    Os::new("--force"),
                    Os::new("--ignore-unmatch"),
                    Os::new("--"),
                    Os::new(path),
                ],
                None,
            )?;
            // The worktree's copy has to be gone for the previewed
            // result to be true, and the removal must not follow a
            // symlinked ancestor. A removal that fails is a partial
            // outcome, not a completed one.
            match remove_file_no_follow(&pm.dir, path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(Error::internal(format!(
                        "the discard of {path} removed the staged addition but its \
                         working-tree file could not be removed ({e}). The change is \
                         PARTLY applied: the index entry is gone and the file is still \
                         there. Nothing was committed, the crash's marker and snapshot are \
                         intact, and the file must be removed by hand before this path is \
                         left alone."
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Run the repository's effective pre-commit hook against the content
/// the recovery is about to commit. A hook that refuses refuses the
/// recovery: the check is not weakened by committing a different way.
///
/// The hook must see the STAGED version, not the working tree's: the
/// two can differ, and the recovery commits the staged one. So the
/// hook runs against a MATERIALIZED copy of the index, in a directory
/// of its own, with a gitfile so the hook's own
/// `git rev-parse --show-toplevel` resolves to that copy. The real
/// worktree is never handed to the hook and is never touched.
///
/// The copy's directory is created owner-only and fresh, and it lives
/// exactly as long as this call: the hook runs while it exists, and it
/// is removed when the hook returns — on a refusal as much as on
/// success.
fn run_pre_commit_hook(pm: &Pm, git_dir: &Path, tmp_index: &Path) -> Result<()> {
    let Some(hook) = effective_hook(pm) else {
        return Ok(());
    };
    let Ok(meta) = std::fs::metadata(&hook) else {
        return Ok(());
    };
    if !meta.is_file() {
        return Ok(());
    }
    {
        use std::os::unix::fs::PermissionsExt;
        // Git ignores a hook that is not executable; so does the
        // recovery, or it would run a check the ordinary commit path
        // never runs.
        if meta.permissions().mode() & 0o111 == 0 {
            return Ok(());
        }
    }
    // Held for the whole call: the hook's working directory must outlive
    // the child, and the drop removes the copy however this returns.
    let materialized = materialize_index(pm, git_dir, tmp_index)?;
    let mut cmd = Command::new(&hook);
    cmd.current_dir(materialized.path())
        .env("GIT_DIR", git_dir)
        .env("GIT_INDEX_FILE", tmp_index)
        .env("GIT_WORK_TREE", materialized.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = crate::reaper::output(&mut cmd)
        .map_err(|e| Error::internal(format!("the pre-commit hook could not be run: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(Error::rejected(format!(
            "the tracker's pre-commit hook refused the recovered content: {}. Nothing was \
             committed. Fix what the hook reports and review again; a retry unchanged will \
             not help.",
            err.trim().lines().take(20).collect::<Vec<_>>().join("; ")
        )));
    }
    Ok(())
}

/// The index's content, materialized into a directory of its own.
///
/// `checkout-index` writes what the INDEX holds — the staged version —
/// not what the working tree holds, which is the whole point: the hook
/// must validate the bytes that are about to be committed. A gitfile
/// points the copy at the real repository, so the hook's own
/// `git rev-parse --show-toplevel` resolves to the copy and its lint
/// reads the staged `issue.md` bytes.
///
/// The copy is made in a fresh, owner-only directory: `0700` is
/// requested in the same step that creates it, so a permissive umask
/// can no longer widen it (a stricter one only narrows the owner bits)
/// and no default-mode directory ever exists. Nothing is created at a
/// predictable path and nothing pre-existing is removed. The returned
/// handle removes the copy on every exit path — success, refusal, and a
/// failure to start the hook.
pub(super) fn materialize_index(
    pm: &Pm,
    git_dir: &Path,
    tmp_index: &Path,
) -> Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::Builder::new()
        .prefix("cadence-recovery-materialized-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .map_err(|e| {
            Error::internal(format!(
                "a private scratch directory could not be created for the pre-commit hook \
                 ({e}); nothing was committed."
            ))
        })?;
    let out = crate::reaper::output(
        Command::new("git")
            .arg("-C")
            .arg(&pm.dir)
            .arg(format!("--git-dir={}", git_dir.display()))
            .arg(format!("--work-tree={}", dir.path().display()))
            .env("GIT_INDEX_FILE", tmp_index)
            .arg("checkout-index")
            .arg("-a"),
    )
    .map_err(|e| {
        Error::internal(format!(
            "the staged content could not be materialized for the pre-commit hook ({e}); \
             nothing was committed."
        ))
    })?;
    if !out.status.success() {
        return Err(Error::internal(format!(
            "the staged content could not be materialized for the pre-commit hook: {}. \
             Nothing was committed.",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    std::fs::write(
        dir.path().join(".git"),
        format!("gitdir: {}\n", git_dir.display()),
    )?;
    Ok(dir)
}

/// The hook Git would run for this repository: `core.hooksPath` when
/// it is set (a relative path is relative to the repository root),
/// otherwise the repository's hooks directory. An ordinary commit runs
/// that hook, so the recovery must run the same one — the recovery
/// commits through `commit-tree`, which runs no hook of its own.
fn effective_hook(pm: &Pm) -> Option<PathBuf> {
    let dir = match git(&pm.dir, &["config", "--get", "core.hooksPath"]) {
        Ok(p) if !p.trim().is_empty() => {
            let p = PathBuf::from(p.trim());
            if p.is_absolute() {
                p
            } else {
                pm.dir.join(p)
            }
        }
        _ => {
            let rel = git(&pm.dir, &["rev-parse", "--git-path", "hooks"]).ok()?;
            let p = PathBuf::from(rel);
            if p.is_absolute() {
                p
            } else {
                pm.dir.join(p)
            }
        }
    };
    Some(dir.join("pre-commit"))
}

/// A git argument that may be a flag or a value.
#[derive(Clone)]
struct Os(String);
impl Os {
    fn new(s: &str) -> Os {
        Os(s.to_string())
    }
}
impl AsRef<std::ffi::OsStr> for Os {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_ref()
    }
}

fn run_git(
    pm: &Pm,
    env: &[(&str, &std::ffi::OsStr)],
    args: &[Os],
    stdin: Option<&[u8]>,
) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(&pm.dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    for a in args {
        cmd.arg(a.as_ref());
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    let mut child = crate::reaper::spawn(&mut cmd)
        .map_err(|_| Error::internal("`git` is required and was not found on PATH"))?;
    if let Some(input) = stdin {
        use std::io::Write;
        if let Some(mut pipe) = child.stdin.take() {
            let _ = pipe.write_all(input);
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| Error::internal(format!("git could not be run: {e}")))?;
    if !out.status.success() {
        return Err(Error::internal(format!(
            "git {} failed in {}: {}",
            args.iter()
                .map(|a| a.0.clone())
                .collect::<Vec<_>>()
                .join(" "),
            pm.dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub(super) fn require_str(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            Error::rejected(format!(
                "the reviewed state is missing '{key}' — review again. A retry unchanged will \
                 not help."
            ))
        })
}

pub(super) fn require_str_list(value: &Value, key: &str) -> Result<Vec<String>> {
    let list = value.get(key).and_then(Value::as_array).ok_or_else(|| {
        Error::rejected(format!(
            "the reviewed state is missing '{key}' — review again. A retry unchanged will not \
             help."
        ))
    })?;
    Ok(list
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect())
}
