//! Read-only git/worktree fact gathering and cryptographic state-binding helpers.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::BINDING_VERSION;
use crate::error::{Error, Result};
use crate::issue::git;
use crate::issue::Pm;

/// The binding over everything a decision depends on.
pub(super) fn state_binding(pm: &Pm, leftovers: &[String], foreign: &[String]) -> Result<String> {
    let mut h = Sha256::new();
    h.update(BINDING_VERSION.as_bytes());
    h.update(b"\0tracker\0");
    h.update(pm.dir.to_string_lossy().as_bytes());
    h.update(b"\0head\0");
    h.update(
        git(&pm.dir, &["rev-parse", "HEAD"])
            .unwrap_or_default()
            .as_bytes(),
    );
    // The SYMBOLIC target too: a symbolic HEAD can move to a different
    // branch at the same commit without changing the object id.
    h.update(b"\0symbolic_head\0");
    h.update(
        git(&pm.dir, &["symbolic-ref", "-q", "HEAD"])
            .unwrap_or_default()
            .as_bytes(),
    );
    h.update(b"\0index\0");
    h.update(file_digest(&index_path(pm)?)?.as_bytes());
    h.update(b"\0marker\0");
    h.update(marker_facts(pm)?.to_string().as_bytes());
    h.update(b"\0snapshot\0");
    h.update(snapshot_facts(pm)?.to_string().as_bytes());
    h.update(b"\0leftovers\0");
    for p in leftovers {
        h.update(p.as_bytes());
        h.update(b"\0");
        h.update(worktree_facts(pm, p)?.to_string().as_bytes());
        // The ancestors' identity: a symlink replaced between the
        // review and the execute changes the binding, so the recovery
        // refuses instead of writing through it.
        h.update(ancestor_identity(pm, p)?.as_bytes());
        h.update(b"\0");
    }
    h.update(b"\0foreign\0");
    for p in foreign {
        h.update(p.as_bytes());
        h.update(b"\0");
    }
    Ok(format!("{:x}", h.finalize()))
}

pub(super) fn digest_of(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

pub(super) fn git_dir_of(pm: &Pm) -> Result<PathBuf> {
    let out = git(&pm.dir, &["rev-parse", "--absolute-git-dir"])?;
    Ok(PathBuf::from(out))
}

pub(super) fn index_path(pm: &Pm) -> Result<PathBuf> {
    Ok(git_dir_of(pm)?.join("index"))
}

/// The crash marker, as facts: it is evidence, never authority.
pub(super) fn marker_facts(pm: &Pm) -> Result<Value> {
    let path = pm.dir.join(".write.lock");
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            use std::os::unix::fs::MetadataExt;
            Ok(json!({
                "present": true,
                "bytes": file_digest(&path)?,
                "size": meta.len(),
                "mode": meta.mode(),
                "dev": meta.dev(),
                "ino": meta.ino(),
                "content": std::fs::read_to_string(&path).unwrap_or_default(),
            }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({ "present": false })),
        Err(e) => Err(Error::internal(format!(
            "{} could not be examined ({e}); the interrupted state cannot be inspected, so no \
             recovery can be authorized.",
            path.display()
        ))),
    }
}

/// The dirty snapshot: the attribution baseline. It is never rewritten.
pub(super) fn snapshot_facts(pm: &Pm) -> Result<Value> {
    let path = git_dir_of(pm)?.join("cadence-write.dirty.v2");
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            use std::os::unix::fs::MetadataExt;
            Ok(json!({
                "present": true,
                "bytes": file_digest(&path)?,
                "size": meta.len(),
                "mtime": meta.mtime(),
                "mtime_nsec": meta.mtime_nsec(),
                "dev": meta.dev(),
                "ino": meta.ino(),
            }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({ "present": false })),
        Err(e) => Err(Error::internal(format!(
            "{} could not be examined ({e}); the attribution baseline cannot be read, so no \
             recovery can be authorized.",
            path.display()
        ))),
    }
}

/// The facts of one path: what the index holds, what the working tree
/// holds, and the complete content of both.
pub(super) fn path_facts(pm: &Pm, paths: &[String]) -> Result<Value> {
    let mut out = Vec::new();
    for path in paths {
        out.push(json!({
            "path": path,
            "staged_blob": staged_blob(pm, path)?,
            "staged_content": staged_content(pm, path)?,
            "worktree": worktree_facts(pm, path)?,
            "staged_diff": git(&pm.dir, &["diff", "--cached", "--no-color", "--", path])?,
            "unstaged_diff": git(&pm.dir, &["diff", "--no-color", "--", path])?,
        }));
    }
    Ok(Value::Array(out))
}

/// The identity of every ancestor of a path, from the tracker root
/// down. A recovery must not write through a symlinked ancestor, and a
/// symlink that is REPLACED between the review and the execute has to
/// change the binding — so the ancestors' identity is part of it.
fn ancestor_identity(pm: &Pm, path: &str) -> Result<String> {
    use std::os::unix::fs::MetadataExt;
    let mut out = String::new();
    let mut dir = pm.dir.clone();
    let parts: Vec<&str> = path.split('/').collect();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        dir = dir.join(part);
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) => {
                out.push_str(&format!(
                    "{part}:{}:{}:{}:{};",
                    meta.dev(),
                    meta.ino(),
                    meta.mode(),
                    meta.file_type().is_symlink()
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                out.push_str(&format!("{part}:absent;"));
            }
            Err(e) => {
                return Err(Error::internal(format!(
                    "{} could not be examined ({e}); the interrupted state cannot be \
                     inspected, so no recovery can be authorized.",
                    dir.display()
                )))
            }
        }
    }
    Ok(out)
}

/// The staged renames the crashed writer left: (from, to) pairs. Both
/// ends are affected, so a recovery must select both or neither.
pub(super) fn staged_renames(pm: &Pm) -> Result<Vec<(String, String)>> {
    let out = crate::reaper::output(
        Command::new("git")
            .arg("--no-optional-locks")
            .arg("-C")
            .arg(&pm.dir)
            .args(["status", "--porcelain", "-z"]),
    )
    .map_err(|e| Error::internal(format!("git status could not be run: {e}")))?;
    if !out.status.success() {
        return Err(Error::internal(format!(
            "git status failed in {}: {}",
            pm.dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut fields = text.split('\0');
    let mut renames = Vec::new();
    while let Some(rec) = fields.next() {
        if rec.len() < 4 {
            continue;
        }
        let code = &rec[..2];
        if !(code.starts_with('R') || code.starts_with('C')) {
            continue;
        }
        let to = rec[3..].to_string();
        let from = fields.next().unwrap_or_default().to_string();
        renames.push((from, to));
    }
    Ok(renames)
}

/// The blob the index holds for a path: what a keep would commit.
pub(super) fn staged_blob(pm: &Pm, path: &str) -> Result<Option<String>> {
    let out = git(&pm.dir, &["ls-files", "--stage", "--", path])?;
    let first = out.lines().next().unwrap_or("").trim().to_string();
    if first.is_empty() {
        return Ok(None);
    }
    Ok(first
        .split_whitespace()
        .nth(1)
        .map(str::to_string)
        .filter(|b| !b.is_empty()))
}

/// The working tree's own state for a path.
pub(super) fn worktree_facts(pm: &Pm, path: &str) -> Result<Value> {
    let abs = pm.dir.join(path);
    match std::fs::symlink_metadata(&abs) {
        Ok(meta) => {
            use std::os::unix::fs::MetadataExt;
            let kind = if meta.file_type().is_symlink() {
                "symlink"
            } else if meta.is_dir() {
                "directory"
            } else {
                "file"
            };
            let content = if kind == "file" {
                match std::fs::read(&abs) {
                    Ok(bytes) => Some(bytes),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => {
                        return Err(Error::internal(format!(
                            "{path} could not be read ({e}); its content cannot be inspected, \
                             so no recovery can be authorized. Restore readable evidence, then \
                             review again."
                        )))
                    }
                }
            } else {
                None
            };
            Ok(json!({
                "kind": kind,
                "size": meta.len(),
                "mode": meta.mode(),
                "exists": true,
                "bytes": content.as_ref().map(|c| format!("{:x}", Sha256::digest(c))),
                "content": content_value(content.as_deref()),
                "link_target": if kind == "symlink" {
                    std::fs::read_link(&abs).ok().map(|p| p.display().to_string())
                } else {
                    None
                },
            }))
        }
        Err(_) => Ok(json!({ "kind": "absent", "exists": false })),
    }
}

/// The bytes of a file, in a form a reviewer can actually read: the
/// text when it is text, and the hex when it is not. A binary leftover
/// is never summarized away — the review has to be able to show the
/// whole thing before anyone decides to keep or discard it.
fn content_value(bytes: Option<&[u8]>) -> Value {
    let Some(bytes) = bytes else {
        return Value::Null;
    };
    match std::str::from_utf8(bytes) {
        Ok(text) => json!({ "utf8": text, "binary": false }),
        Err(_) => {
            let mut hex = String::with_capacity(bytes.len() * 2);
            for b in bytes {
                hex.push_str(&format!("{b:02x}"));
            }
            json!({ "hex": hex, "binary": true })
        }
    }
}

/// The content the index holds for a path: what a keep would commit,
/// in a form a reviewer can read. A binary blob is shown in hex, not
/// summarized away.
fn staged_content(pm: &Pm, path: &str) -> Result<Value> {
    let Some(blob) = staged_blob(pm, path).ok().flatten() else {
        return Ok(Value::Null);
    };
    let bytes = git_raw(pm, &["cat-file", "blob", &blob])?;
    Ok(content_value(Some(&bytes)))
}

/// Git's raw output, without the trimming the text helper does: a
/// binary blob's bytes must survive intact.
pub(super) fn git_raw(pm: &Pm, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(&pm.dir);
    for a in args {
        cmd.arg(a);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = crate::reaper::spawn(&mut cmd)
        .map_err(|_| Error::internal("`git` is required and was not found on PATH"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| Error::internal(format!("git could not be run: {e}")))?;
    if !out.status.success() {
        return Err(Error::internal(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            pm.dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

/// The committed state of a path: what a discard would restore it to.
pub(super) fn head_facts(pm: &Pm, path: &str) -> Result<Value> {
    // Whether the path is in HEAD is answered by the TREE, not by
    // `cat-file -e`: that fails both for a path that is not there and
    // for an object it cannot read, and the second must not look like
    // the first.
    let listing = match git(&pm.dir, &["ls-tree", "HEAD", "--", path]) {
        Ok(l) => l,
        Err(e) => {
            if git(&pm.dir, &["rev-parse", "--verify", "HEAD"]).is_err() {
                String::new()
            } else {
                return Err(Error::internal(format!(
                    "the committed state of {path} could not be read ({e}); the interrupted \
                     state cannot be inspected, so no recovery can be authorized."
                )));
            }
        }
    };
    if listing.trim().is_empty() {
        return Ok(json!({
            "present": false,
            "effect": "the path is not in HEAD: the staged addition and the working-tree file \
                       are both removed, and the path is absent afterwards",
        }));
    }
    let blob = git(&pm.dir, &["rev-parse", &format!("HEAD:{path}")])?;
    let mode = listing
        .split_whitespace()
        .next()
        .map(str::to_string)
        .unwrap_or_default();
    let content = git_raw(pm, &["cat-file", "blob", &format!("HEAD:{path}")]).ok();
    Ok(json!({
        "present": true,
        "blob": blob,
        "mode": mode,
        "content": content_value(content.as_deref()),
        "effect": "the index entry and the working-tree file are both replaced by this \
                   committed version",
    }))
}

fn file_digest(path: &Path) -> Result<String> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(format!("{:x}", Sha256::digest(&bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("absent".to_string()),
        Err(e) => Err(Error::internal(format!(
            "{} could not be read ({e}); the interrupted state cannot be inspected, so no \
             recovery can be authorized. Restore readable evidence, then review again.",
            path.display()
        ))),
    }
}
