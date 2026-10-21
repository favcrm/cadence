//! Staged write transactions and done-on-merge handling.

use super::*;

/// Apply `key=value` pairs to one front in memory; returns the
/// `key=value` summary tokens. Nothing is written here, so a bulk edit
/// can validate every issue before it touches a file.
pub(crate) fn apply_pairs(
    project: &project::Project,
    front: &mut Front,
    pairs: &[String],
) -> Result<Vec<String>> {
    let mut changed = Vec::new();
    for kv in pairs {
        let (key, value) = kv
            .split_once('=')
            .ok_or_else(|| Error::rejected(format!("set pair '{kv}' is not key=value")))?;
        if key == "stage" || key == "stage_at" {
            return Err(Error::rejected(
                "an epic's stage is a gate decision — move it with \
                 `cadence issue epic stage <EPIC> <stage>`",
            ));
        }
        if !model::SETTABLE.contains(&key) {
            return Err(Error::rejected(format!(
                "'{key}' is not settable — one of {}",
                model::SETTABLE.join(" ")
            )));
        }
        match key {
            "status" => {
                model::check_status(value)?;
                front.status = value.to_string();
            }
            "priority" => {
                model::check_priority(value)?;
                front.priority = value.to_string();
            }
            "title" => {
                if value.is_empty() {
                    return Err(Error::rejected("title cannot be empty"));
                }
                front.title = value.to_string();
            }
            "owner" => front.owner = (!value.is_empty()).then(|| value.to_string()),
            "component" => {
                if !value.is_empty() {
                    check_component(project, value)?;
                }
                front.component = (!value.is_empty()).then(|| value.to_string());
            }
            "tags" => {
                front.tags = check_tags(project, &split_tags(value))?;
                changed.push(format!("tags={}", front.tags.join(",")));
                continue;
            }
            // CAD-378: advisory planned paths; an empty value clears.
            "paths" => {
                front.paths = crate::issue::areas::parse_paths(value)?;
                changed.push(format!("paths={}", front.paths.join(",")));
                continue;
            }
            // CAD-405: an empty value clears each of these.
            "type" => {
                if !value.is_empty() {
                    model::check_type(value)?;
                }
                front.item_type = (!value.is_empty()).then(|| value.to_string());
            }
            "milestone" => {
                if !value.is_empty() && !model::valid_tag(value) {
                    return Err(Error::rejected(format!(
                        "Invalid milestone '{value}' — 1-32 lowercase letters, digits or hyphens"
                    )));
                }
                front.milestone = (!value.is_empty()).then(|| value.to_string());
            }
            "size" => {
                let size = value.to_ascii_uppercase();
                if !size.is_empty() && !model::SIZES.iter().any(|(s, _)| *s == size) {
                    return Err(Error::rejected(format!(
                        "Unknown size '{value}' — one of S M L"
                    )));
                }
                front.size = (!size.is_empty()).then_some(size);
            }
            _ => unreachable!(),
        }
        changed.push(format!("{key}={value}"));
    }
    Ok(changed)
}

/// One issue of a bulk edit, loaded and edited in memory.
pub(crate) struct Staged {
    pub(crate) id: String,
    dir: PathBuf,
    pub(crate) front: Front,
    body: String,
}

/// Load every id (each once, in the order given) and run `edit` on its
/// front; `edit` answers whether it changed anything, and unchanged
/// issues drop out of the batch. Any unknown id or rejected edit fails
/// the whole batch before a single file is written. Call under the PM
/// lock.
pub(crate) fn stage(
    pm: &Pm,
    ids: &[String],
    mut edit: impl FnMut(&project::Project, &mut Front) -> Result<bool>,
) -> Result<Vec<Staged>> {
    let mut staged: Vec<Staged> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for id in ids {
        if !seen.insert(id) {
            continue;
        }
        let (project, dir) = issue_dir(pm, id)?;
        let (mut front, body) = load_front(&dir)?;
        let changed = edit(&project, &mut front).map_err(|e| match e {
            Error::Rejected(m) if ids.len() > 1 => Error::rejected(format!("{id}: {m}")),
            other => other,
        })?;
        if !changed {
            continue;
        }
        staged.push(Staged {
            id: id.clone(),
            dir,
            front,
            body,
        });
    }
    Ok(staged)
}

/// Write the staged fronts and make the one commit that names them all
/// — subject `<ID>[, <ID>…]: <summary>`, one `Issue:` trailer per id.
/// Only the staged issues' `issue.md` files reach the commit. Returns
/// the committed ids plus the foreign paths the commit saw; a failed
/// write or commit restores every file it touched (CAD-454).
pub(crate) fn commit_staged(
    pm: &Pm,
    staged: &[Staged],
    summary: &str,
    actor: &str,
) -> Result<(Vec<String>, Vec<String>)> {
    commit_staged_inner(pm, None, staged, summary, actor)
}

/// [`commit_staged`] through a scoped [`PmLock`]: every staged file
/// must stay inside the lock's admitted paths. Callers that took
/// [`Pm::lock_for_paths`](Pm::lock_for_paths) commit through here.
pub(crate) fn commit_staged_scoped(
    pm: &Pm,
    lock: &PmLock,
    staged: &[Staged],
    summary: &str,
    actor: &str,
) -> Result<(Vec<String>, Vec<String>)> {
    commit_staged_inner(pm, Some(lock), staged, summary, actor)
}

pub(super) fn commit_staged_inner(
    pm: &Pm,
    lock: Option<&PmLock>,
    staged: &[Staged],
    summary: &str,
    actor: &str,
) -> Result<(Vec<String>, Vec<String>)> {
    if staged.is_empty() {
        return Ok((vec![], vec![]));
    }
    let originals: Vec<(PathBuf, Option<Vec<u8>>)> = staged
        .iter()
        .map(|s| {
            let file = s.dir.join("issue.md");
            let prev = file_preimage(&file);
            (file, prev)
        })
        .collect();
    let paths: Vec<PathBuf> = originals.iter().map(|(f, _)| f.clone()).collect();
    let ids: Vec<&str> = staged.iter().map(|s| s.id.as_str()).collect();
    let written = staged
        .iter()
        .try_for_each(|s| save_front(&s.dir, &s.front, &s.body))
        .and_then(|_| match lock {
            Some(l) => commit_scoped(
                pm,
                l,
                &paths,
                &format!("{}: {summary}", ids.join(", ")),
                &ids,
                actor,
            ),
            None => commit(
                pm,
                &paths,
                &format!("{}: {summary}", ids.join(", ")),
                &ids,
                actor,
            ),
        });
    match written {
        Ok(foreign) => Ok((ids.iter().map(|i| i.to_string()).collect(), foreign)),
        Err(e) => {
            for (file, prev) in originals {
                restore_preimage(&file, prev);
            }
            Err(e)
        }
    }
}

/// CAD-756: `status=done` needs evidence — one of: a recorded `ref
/// pr:`, a worktree ref `finish` already closed, or a review/verdict
/// artifact in the issue folder. `force` substitutes a recorded
/// reason (it lands in the commit summary — never silent). The
/// reconcile sweep and delivery merge path write through
/// `mark_done_on_merge`, which IS the evidence — they never reach
/// this gate. Only the close direction is gated: `done` → anything
/// reopens freely, `dropped` is exempt by design, and a `done` →
/// `done` rewrite never asked a question it could fail.
pub(super) fn check_done_evidence(
    pm: &Pm,
    front: &Front,
    force: Option<&str>,
    staged_artifacts: &[String],
) -> Result<()> {
    if let Some(reason) = force {
        return if reason.trim().is_empty() {
            Err(Error::rejected(
                "--force needs a reason — it is recorded on the commit",
            ))
        } else {
            Ok(())
        };
    }
    if front
        .refs
        .iter()
        .any(|r| r.kind == "pr" || r.kind == "commit")
    {
        return Ok(());
    }
    if front
        .refs
        .iter()
        .any(|r| r.kind == "worktree" && r.closed == Some(true))
    {
        return Ok(());
    }
    let (_project, dir) = issue_dir(pm, &front.id)?;
    let is_verdictish = |name: &str| {
        let n = name.to_ascii_lowercase();
        n.starts_with("review") || n.contains("verdict")
    };
    // CAD-908: an attachment staged in the same `issue edit` counts,
    // like a same-edit `--ref pr:` — it lands in the same commit.
    let verdictish = staged_artifacts.iter().any(|n| is_verdictish(n))
        || std::fs::read_dir(dir.join("artifacts"))
            .map(|rd| {
                rd.flatten().any(|e| {
                    // CAD-971: only a real artifact counts — a valid
                    // (allowlisted, non-hidden) name on a regular file,
                    // never a leftover temp, a symlink or a directory.
                    let name = e.file_name().to_string_lossy().to_string();
                    model::valid_artifact_name(&name)
                        && e.file_type().map(|t| t.is_file()).unwrap_or(false)
                        && is_verdictish(&name)
                })
            })
            .unwrap_or(false);
    if verdictish {
        return Ok(());
    }
    Err(Error::rejected(format!(
        "{}: status=done needs evidence — a `ref pr:`, a closed worktree \
         ref, or a review/verdict artifact in the issue folder. \
         `cadence issue reconcile --dry-run` shows the merge classification; \
         `--force \"<reason>\"` records an override in the commit",
        front.id
    )))
}

/// A failed done transaction carries only the status observed while
/// holding the tracker lock. Before that read, its status is unknown.
#[derive(Debug)]
pub struct DoneWriteFailure {
    pub error: Error,
    pub from: Option<String>,
}

impl std::fmt::Display for DoneWriteFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}

impl std::error::Error for DoneWriteFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// CAD-1359: the clock/wait seam behind the lock retry loop in
/// [`mark_done_on_merge_waiting`]. `now` feeds the deadline's
/// remaining-time arithmetic; `sleep` is the backoff after a failed
/// real `try_lock`. Production uses [`RealWait`]; a test runtime may
/// report a virtual `now` that advances only through these backoff
/// waits, so the same real refusal loop runs deterministically. The
/// tracker lock is never mocked, the `wait` budget is never
/// overridden, and no environment or config selects a runtime — the
/// caller chooses it in code, intra-crate only.
pub(crate) trait WaitClock {
    fn now(&self) -> Instant;
    fn sleep(&self, d: Duration);
}

/// `Instant::now` + `std::thread::sleep` — the original semantics.
pub(crate) struct RealWait;

impl WaitClock for RealWait {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d)
    }
}

/// CAD-449: a merged delivery marks its ticket done — one frontmatter
/// write and one tracker commit, `<ID>: set status=done — <why>`, whose
/// `Actor:` trailer is `actor` (the observer and the delivery). A ticket
/// already `done` or `dropped` is left alone: answers `Some(status)`
/// and writes nothing. `None` means it was marked done. The plan gate
/// every status write passes ([`crate::issue::plan::check_status_write`])
/// applies here too. With `expect`, the ticket is marked only while its
/// status is still that one — a status changed since (the operator
/// reopened or moved it) is left alone and answered like `done`.
///
/// It never waits for the tracker lock: a busy tracker is an error the
/// caller may retry only with a previously observed status (the daemon
/// holds its own lock here). A failed commit
/// leaves nothing behind — `issue.md` is restored, and the commit
/// itself unstages the path, so no later write can commit
/// `status: done` for it.
pub fn mark_done_on_merge(
    pm: &Pm,
    id: &str,
    why: &str,
    actor: &str,
    expect: Option<&str>,
) -> std::result::Result<Option<String>, DoneWriteFailure> {
    mark_done_on_merge_waiting(pm, id, why, actor, expect, Duration::ZERO)
}

/// `mark_done_on_merge` for a CLI caller (CAD-1150): a busy tracker lock
/// is retried with a short backoff for up to `wait` before it is
/// reported. The daemon passes no wait and keeps the never-wait rule.
#[track_caller]
pub fn mark_done_on_merge_waiting(
    pm: &Pm,
    id: &str,
    why: &str,
    actor: &str,
    expect: Option<&str>,
    wait: Duration,
) -> std::result::Result<Option<String>, DoneWriteFailure> {
    mark_done_on_merge_waiting_with(pm, id, why, actor, expect, wait, &RealWait)
}

/// [`mark_done_on_merge_waiting`] on an injected [`WaitClock`]
/// (CAD-1359): the same real `try_lock` refusal loop and the same
/// `wait` budget — only the clock the deadline/backoff arithmetic
/// reads is swapped. `pub(crate)` for the reconcile shared-deadline
/// path; production passes [`RealWait`].
#[track_caller]
pub(crate) fn mark_done_on_merge_waiting_with(
    pm: &Pm,
    id: &str,
    why: &str,
    actor: &str,
    expect: Option<&str>,
    wait: Duration,
    clock: &dyn WaitClock,
) -> std::result::Result<Option<String>, DoneWriteFailure> {
    let lock = (|| -> Result<PmLock> {
        // CAD-1600: this write touches exactly one issue.md, so the
        // lock is scoped to it. An interrupted writer's leftovers on
        // other paths no longer refuse the close (see
        // `Pm::try_lock_for_paths`); overlapping leftovers and global
        // Git hazards still do.
        let (_project, dir) = issue_dir(pm, id)?;
        let file = dir.join("issue.md");
        let scope = std::slice::from_ref(&file);
        let deadline = clock.now() + wait;
        let mut backoff = Duration::from_millis(50);
        loop {
            if let Some(lock) = pm.try_lock_for_paths(scope)? {
                return Ok(lock);
            }
            let left = deadline.saturating_duration_since(clock.now());
            if left.is_zero() {
                return Err(Error::rejected(
                    "the tracker is locked by another writer (write lock)",
                ));
            }
            clock.sleep(backoff.min(left));
            backoff = (backoff * 2).min(Duration::from_millis(500));
        }
    })()
    .map_err(|error| DoneWriteFailure { error, from: None })?;
    mark_done_on_merge_locked(pm, &lock, id, why, actor, expect)
}

/// The same status CAS, plan gate and rollback under an already held tracker
/// lock. Daemon callers validate final native authority in this critical section.
/// The lock must be scoped to this issue's `issue.md` (see
/// [`mark_done_on_merge_waiting_with`]); an unscoped `Full` guard commits as
/// before, but its acquisition is refused beside any interrupted leftovers.
pub(crate) fn mark_done_on_merge_locked(
    pm: &Pm,
    lock: &PmLock,
    id: &str,
    why: &str,
    actor: &str,
    expect: Option<&str>,
) -> std::result::Result<Option<String>, DoneWriteFailure> {
    let mut from = None;
    let result = (|| -> Result<Option<String>> {
        let (_project, dir) = issue_dir(pm, id)?;
        let (mut front, body) = load_front(&dir)?;
        from = Some(front.status.clone());
        if matches!(front.status.as_str(), "done" | "dropped")
            || expect.is_some_and(|e| e != front.status)
        {
            return Ok(Some(front.status));
        }
        crate::issue::plan::check_status_write(&pm.dir, &front, "done")?;
        front.status = "done".to_string();
        let file = dir.join("issue.md");
        let original = std::fs::read(&file)?;
        let written = save_front(&dir, &front, &body).and_then(|_| {
            commit_scoped(
                pm,
                lock,
                std::slice::from_ref(&file),
                &format!("{id}: set status=done — {why}"),
                &[id],
                actor,
            )
        });
        if let Err(e) = written {
            let _ = std::fs::write(&file, &original);
            return Err(e);
        }
        Ok(None)
    })();
    result.map_err(|error| DoneWriteFailure { error, from })
}
