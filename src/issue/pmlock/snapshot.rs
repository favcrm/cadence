//! Dirty-worktree snapshot reading, protection matching, temp-file sweeping, and crash-leftover classification.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use super::{create_exclusive, io_unknown, Interruption, Tree, DIRTY_FILE, TMP_PREFIX};
use crate::error::Result;

/// `None` when this version's snapshot does not exist (a crash by an
/// older build, which wrote another format under another name).
pub(super) fn read_snapshot(git_dir: &Path) -> Result<Option<HashSet<String>>> {
    let path = git_dir.join(DIRTY_FILE);
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_unknown(&path, "the dirty-path snapshot", e)),
    };
    let mut buf = Vec::new();
    // One byte past the cap: a snapshot larger than this build reads is
    // truncated, so its protected set is unknown and it is not trusted.
    file.take((16 << 20) + 1)
        .read_to_end(&mut buf)
        .map_err(|e| io_unknown(&path, "the dirty-path snapshot", e))?;
    if buf.len() > (16 << 20) {
        return Ok(None);
    }
    let mut names: Vec<String> = String::from_utf8_lossy(&buf)
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    // The tag proves THIS version wrote the file, including the staged
    // set. An older build's snapshot has no tag, so its staged set is
    // unknown and no provenance can be derived from it.
    if names.first().map(String::as_str) != Some("cadence-snapshot v3") {
        return Ok(None);
    }
    names.remove(0);
    Ok(Some(names.into_iter().collect()))
}

/// When the dirty-path snapshot was written: the moment the last
/// writer that rewrote it took the lock. `None` without a snapshot.
/// A file older than it is treated as pre-existing — a tie is read as
/// new, the refusing side. This only ever excludes a path from the
/// diagnostic name list; it never authorizes a recovery action.
pub(super) fn snapshot_time(git_dir: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(git_dir.join(DIRTY_FILE))
        .and_then(|m| m.modified())
        .ok()
}

/// Files under `root/rel` modified at or after `since`. Symlinks are
/// not followed.
///
/// Returns true when the scan stopped early: the names are then only
/// a sample, and the caller must block the whole directory.
pub(super) fn files_since(
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
            // A directory that cannot be read makes the scan incomplete.
            return true;
        };
        for e in rd {
            let e = match e {
                Ok(e) => e,
                // An entry the iterator could not produce makes the scan
                // incomplete, never silently shorter.
                Err(_) => return true,
            };
            visited += 1;
            if out.len() >= CAP || visited > 500_000 {
                return true;
            }
            let Ok(m) = e.metadata() else { return true };
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

pub(super) fn write_snapshot(git_dir: &Path, tree: &Tree) -> Result<()> {
    let tmp = git_dir.join(format!("{TMP_PREFIX}dirty-{}", std::process::id()));
    let path = git_dir.join(DIRTY_FILE);
    let mut content = String::from("cadence-snapshot v3\0");
    for (_, p) in &tree.loose {
        content.push_str(p);
        content.push('\0');
    }
    // The STAGED set too (CAD-1601): a path staged when the crash took
    // the lock is positive evidence that the crash did not stage it.
    // Without it, a path staged before the acquisition could be blamed
    // on the crash by a timestamp alone.
    for p in &tree.staged {
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
pub(super) fn sweep_tmp(git_dir: &Path) {
    let Ok(rd) = std::fs::read_dir(git_dir) else {
        return;
    };
    for e in rd.flatten() {
        if e.file_name().to_string_lossy().starts_with(TMP_PREFIX) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// True when the snapshot recorded `p`, or a directory that contains
/// it. Git lists an untracked directory as one `dir/` entry, so its
/// children are protected by that entry even when they are not named
/// individually.
pub(super) fn snapshot_protects(snapshot: &HashSet<String>, p: &str) -> bool {
    if snapshot.contains(p) {
        return true;
    }
    let mut end = 0;
    while let Some(i) = p[end..].find('/') {
        end += i;
        if snapshot.contains(&format!("{}/", &p[..end])) {
            return true;
        }
        end += 1;
    }
    false
}

pub(super) fn classify(
    tree: &Tree,
    snapshot: Option<&HashSet<String>>,
    inside: &[String],
    ambiguous: &[String],
    whole: &[String],
    sampled: bool,
) -> Option<Interruption> {
    let mut named: Vec<String> = Vec::new();
    let mut foreign: Vec<String> = Vec::new();
    let mut foreign_paths: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    for s in &tree.git_state {
        named.push(format!("git state {s}"));
    }
    for p in &tree.staged {
        // Positive provenance, not a timestamp: a path already dirty
        // when the crash took the lock is in the snapshot, so the crash
        // did not create it. Without a snapshot (an older build's
        // crash) nothing is proven, and the path is protected.
        if snapshot.is_some_and(|s| !snapshot_protects(s, p)) {
            named.push(format!("staged {p}"));
            paths.push(p.clone());
        } else {
            // Staged before the crash took the lock (or with no
            // snapshot): it blocks and is named, but no provenance
            // proves the crash staged it, so it is protected too.
            named.push(format!("staged {p}"));
            foreign.push(format!("staged {p}"));
            foreign_paths.push(p.clone());
            paths.push(p.clone());
        }
    }
    for (kind, p) in &tree.loose {
        if snapshot.is_none() {
            if *kind == "untracked" {
                // CAD-1208: present untracked paths cannot be told from
                // leftovers, so none is blamed.
                foreign.push(format!("{kind} {p}"));
                foreign_paths.push(p.clone());
            } else {
                // A modified tracked file: it blocks, but with no
                // snapshot nothing proves the crash modified it, so it
                // is protected too.
                named.push(format!("{kind} {p}"));
                foreign.push(format!("{kind} {p}"));
                foreign_paths.push(p.clone());
                paths.push(p.clone());
            }
        } else if snapshot.is_some_and(|s| snapshot_protects(s, p)) {
            foreign.push(format!("{kind} {p}"));
            foreign_paths.push(p.clone());
        } else if p.ends_with('/') && inside.iter().any(|f| f.starts_with(p.as_str())) {
            // Named by the files found inside it, below.
        } else {
            named.push(format!("{kind} {p}"));
            paths.push(p.trim_end_matches('/').to_string());
        }
    }
    for p in inside {
        if snapshot.is_some_and(|s| !snapshot_protects(s, p)) {
            named.push(format!("untracked {p}"));
            paths.push(p.clone());
        } else {
            foreign.push(format!("untracked {p}"));
            foreign_paths.push(p.clone());
        }
    }
    // Files under a directory that was ALREADY dirty when the crash
    // took the lock: they block and they are named, but no timestamp
    // proves the crash wrote them, so they are also protected — a
    // recovery may not act on one.
    for p in ambiguous {
        named.push(format!("untracked {p}"));
        foreign.push(format!("untracked {p}"));
        foreign_paths.push(p.clone());
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
            foreign_paths,
            paths,
            hazards: tree.git_state.clone(),
            sampled,
            snapshot_known: snapshot.is_some(),
        })
    }
}
