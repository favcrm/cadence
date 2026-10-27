//! Revision, commit, staging and post-commit helpers for issue writes.

use super::*;

/// `Actor:` trailer resolution, in order: an explicit `--author`/`--by`
/// (`who`), the API actor string (`actor`), `CADENCE_ALIAS`, else
/// `operator`. The same chain feeds `issue comment`'s author fallback.
pub(crate) fn actor_who(actor: &str, who: Option<&str>) -> String {
    who.map(str::to_string)
        .or_else(|| (!actor.is_empty()).then(|| actor.to_string()))
        .or_else(|| std::env::var("CADENCE_ALIAS").ok())
        .filter(|w| !w.is_empty())
        .unwrap_or_else(|| "operator".to_string())
}

/// Commit with the actor named when one is given — API writes show as
/// `CAD-16: set status=review (operator (ui))`; CLI writes pass "" and
/// keep the bare subject. Every commit also carries trailers after a
/// blank line: `Issue: <ID>` once per issue the write touches (links
/// record both ends) and `Actor: <who>` — the truthful actor history
/// reads instead of the git author. `paths` are exactly the files the
/// write touched — they are all the commit stages (CAD-454); the
/// returned list names foreign paths the commit saw but left alone.
pub(crate) fn commit(
    pm: &Pm,
    paths: &[PathBuf],
    message: &str,
    ids: &[&str],
    actor: &str,
) -> Result<Vec<String>> {
    commit_who(pm, paths, message, ids, actor, None)
}

pub(crate) fn commit_who(
    pm: &Pm,
    paths: &[PathBuf],
    message: &str,
    ids: &[&str],
    actor: &str,
    who: Option<&str>,
) -> Result<Vec<String>> {
    let full = commit_message(message, ids, actor, who);
    pm.commit(paths, &full)
}

pub(super) fn commit_message(
    message: &str,
    ids: &[&str],
    actor: &str,
    who: Option<&str>,
) -> String {
    let subject = if actor.is_empty() {
        message.to_string()
    } else {
        format!("{message} ({actor})")
    };
    let mut trailers = String::new();
    for id in ids {
        trailers.push_str(&format!("Issue: {id}\n"));
    }
    trailers.push_str(&format!("Actor: {}\n", actor_who(actor, who)));
    format!("{subject}\n\n{trailers}")
}

/// [`commit`] through a scoped [`PmLock`]: the commit's paths must
/// stay inside the lock's admitted paths (see
/// [`Pm::commit_scoped`]). Callers that took
/// [`Pm::lock_for_paths`](Pm::lock_for_paths) commit through here.
pub(crate) fn commit_scoped(
    pm: &Pm,
    lock: &PmLock,
    paths: &[PathBuf],
    message: &str,
    ids: &[&str],
    actor: &str,
) -> Result<Vec<String>> {
    commit_who_scoped(pm, lock, paths, message, ids, actor, None)
}

/// [`commit_who`] through a scoped [`PmLock`].
pub(crate) fn commit_who_scoped(
    pm: &Pm,
    lock: &PmLock,
    paths: &[PathBuf],
    message: &str,
    ids: &[&str],
    actor: &str,
    who: Option<&str>,
) -> Result<Vec<String>> {
    let full = commit_message(message, ids, actor, who);
    pm.commit_scoped(lock, paths, &full)
}

/// Content hash of `issue.md` — the optimistic-concurrency token the
/// write API calls `if_rev`. FNV-1a: no deps, stable across versions.
pub fn issue_rev(dir: &Path) -> Result<String> {
    let bytes = std::fs::read(dir.join("issue.md"))?;
    Ok(rev_bytes(&bytes))
}

pub(super) fn rev_bytes(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("fnv1a:{h:016x}")
}

/// Optimistic-concurrency check, run under the write lock. Returns a
/// conflict payload (the route maps it to 409) or None.
pub(super) fn check_rev(dir: &Path, if_rev: Option<&str>) -> Result<Option<Value>> {
    let Some(want) = if_rev else {
        return Ok(None);
    };
    let cur = issue_rev(dir)?;
    if want != cur {
        return Ok(Some(json!({"conflict": "if_rev", "current_rev": cur})));
    }
    Ok(None)
}

/// Lint parity at write time: an issue whose status is ready|doing|
/// review while a blocked_by target is unfinished succeeds but the
/// response carries this warning.
pub(crate) fn blocked_warnings(pm: &Pm, id: &str, state_dir: Option<&Path>) -> Result<Vec<String>> {
    let issues = board::load_all(&pm.dir, None)?;
    let jobs = state_dir.map(board::fetch_job_outcomes).unwrap_or_default();
    let views = board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs);
    let Some(v) = views.iter().find(|v| v.issue.front.id == id) else {
        return Ok(vec![]);
    };
    if !(v.blocked && matches!(v.status.as_str(), "ready" | "doing" | "review")) {
        return Ok(vec![]);
    }
    let open: Vec<String> = v
        .issue
        .front
        .blocked_by
        .iter()
        .filter(|b| {
            views
                .iter()
                .find(|o| &o.issue.front.id == *b)
                .map(|o| !matches!(o.status.as_str(), "done" | "dropped"))
                .unwrap_or(true)
        })
        .cloned()
        .collect();
    Ok(vec![format!(
        "{id} is {} but still waits on {}",
        v.status,
        open.join(", ")
    )])
}

/// [`blocked_warnings`] for a write that already committed (CAD-908):
/// the commit landed, so a failed lookup must not turn the write into
/// a reported failure — a retry would repeat a comment or attachment.
/// The failure is printed to stderr and recorded in `post` for the
/// JSON's `post_commit_warnings`; the warnings list is empty then.
pub(crate) fn blocked_warnings_best_effort(
    pm: &Pm,
    id: &str,
    state_dir: Option<&Path>,
    post: &mut Vec<String>,
) -> Vec<String> {
    match blocked_warnings(pm, id, state_dir) {
        Ok(w) => w,
        Err(e) => {
            post_commit_warning(post, format!("blocked-by warnings unavailable: {e}"));
            Vec::new()
        }
    }
}

/// Record one post-commit failure: stderr now, JSON later.
pub(crate) fn post_commit_warning(post: &mut Vec<String>, message: String) {
    eprintln!("warning: {message} (the write itself committed)");
    post.push(message);
}

/// Fold the post-commit warnings into the write's JSON result —
/// present only when a post-commit step failed.
pub(crate) fn attach_post_warnings(out: &mut Value, post: &[String]) {
    if !post.is_empty() {
        out["post_commit_warnings"] = json!(post);
    }
}

/// Write `text` to `path` atomically (temp file + rename).
pub(crate) fn atomic_write(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let permissions = std::fs::metadata(path).ok().map(|meta| meta.permissions());
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let (tmp, mut file) = loop {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = parent.join(format!(".cadence-write-{}-{seq}.tmp", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(file) => break (tmp, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    };
    let result = (|| -> std::io::Result<()> {
        file.write_all(text.as_bytes())?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result.map_err(Into::into)
}

/// A tracker file's bytes before this write touches it — `None` when
/// it does not exist. Feeds [`restore_preimage`] when the commit
/// fails, so a refused write leaves neither an index entry (the
/// commit unstages its own paths) nor a half-written file (CAD-454).
pub(crate) fn file_preimage(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

/// Undo what a write did to `path`: the old bytes go back where they
/// were; a file this write created is removed.
pub(crate) fn restore_preimage(path: &Path, prev: Option<Vec<u8>>) {
    match prev {
        Some(bytes) => {
            let _ = std::fs::write(path, bytes);
        }
        None => {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Fold the foreign paths a commit saw into the write's JSON result —
/// `foreign_files` is present only when the tracker held files the
/// write did not stage.
pub(crate) fn attach_foreign(out: &mut Value, foreign: &[String]) {
    if !foreign.is_empty() {
        out["foreign_files"] = json!(foreign);
    }
}

/// Write `bytes` to a new file at `path`, never leaving a partial one
/// (CAD-908): the bytes go to a hidden temp file in the same directory
/// first, then a hard link claims `path` — atomic, and it fails with
/// `AlreadyExists` instead of overwriting, like `create_new`. A failed
/// write leaves no file at `path`; the temp file is removed either way.
pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    write_new_seq(path, bytes, || SEQ.fetch_add(1, Ordering::Relaxed))
}

/// How many temp names [`write_new_seq`] tries before giving up.
pub(super) const TEMP_ATTEMPTS: usize = 64;

/// [`write_new`] with the temp sequence injected. A temp name that
/// already exists (pid reuse after a crash, a planted file) is skipped
/// — never removed, it is not ours — and the next sequence number is
/// tried, so a leftover temp can never surface as a false
/// `AlreadyExists` for `path` (CAD-971).
pub(super) fn write_new_seq(
    path: &Path,
    bytes: &[u8],
    mut seq: impl FnMut() -> u64,
) -> std::io::Result<()> {
    use std::io::Write;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    for _ in 0..TEMP_ATTEMPTS {
        let tmp = path.with_file_name(format!(".{name}.tmp-{}-{}", std::process::id(), seq()));
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        // From here the temp is ours: it is removed on every outcome.
        let staged = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        let claimed = staged.and_then(|()| std::fs::hard_link(&tmp, path));
        let _ = std::fs::remove_file(&tmp);
        return claimed;
    }
    Err(std::io::Error::other(format!(
        "no free temp name beside {}",
        path.display()
    )))
}

/// Create a file exclusively; on a name collision try `-2`, `-3`…
/// before the extension. Returns the created path. The file appears
/// whole or not at all ([`write_new`]).
pub(crate) fn create_exclusive(dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.to_string(), String::new()),
    };
    for n in 0..100 {
        let candidate = if n == 0 {
            name.to_string()
        } else {
            format!("{stem}-{n}{ext}")
        };
        let path = dir.join(&candidate);
        match write_new(&path, bytes) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(Error::rejected(format!(
        "Could not create a unique name for '{name}' under {}",
        dir.display()
    )))
}
