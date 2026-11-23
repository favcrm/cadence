//! New-issue allocation and its lock.

use super::*;

/// Allocate the next id under the write lock: `<PREFIX>-<max+1>`.
pub(crate) fn next_id(dir: &Path, prefix: &str) -> Result<u64> {
    let mut max = 0u64;
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(n) = name
            .strip_prefix(&format!("{prefix}-"))
            .and_then(|s| s.parse::<u64>().ok())
        {
            max = max.max(n);
        }
    }
    Ok(max + 1)
}

/// CAD-1600: allocate the next id and take the tracker lock scoped to
/// the new ticket's own directory.
///
/// The scope must be known before the lock is taken, and the id is
/// only known after the project is read. So the candidate id is read
/// first and re-checked under the lock: every writer serializes on the
/// same kernel lock, so an id another writer took while we waited is
/// detected and the allocation is retried with the next one. The
/// allocation therefore stays unique under concurrency, while the
/// scope stays narrow: a crashed writer's leftovers on other tickets
/// do not refuse an unrelated new ticket.
pub(super) fn lock_for_new_issue(
    pm: &Pm,
    project: &project::Project,
    explicit_id: Option<&str>,
) -> Result<(PmLock, String)> {
    /// Bounded: each round finds the id another writer took, and the
    /// ids are handed out in order, so this converges. A writer can
    /// lose once to every writer ahead of it, so the bound is generous
    /// against realistic concurrency; past it the caller is told to
    /// retry, not that the creation is refused.
    const ROUNDS: usize = 64;
    lock_for_new_issue_rounds(pm, project, explicit_id, ROUNDS)
}

/// [`lock_for_new_issue`] with the retry bound as a parameter, so the
/// exhausted case is reachable in a test.
pub(super) fn lock_for_new_issue_rounds(
    pm: &Pm,
    project: &project::Project,
    explicit_id: Option<&str>,
    rounds: usize,
) -> Result<(PmLock, String)> {
    let root = pm.dir.join(&project.key);
    for _ in 0..rounds {
        let candidate = match explicit_id {
            Some(id) => id.to_string(),
            None => format!("{}-{}", project.prefix, next_id(&root, &project.prefix)?),
        };
        let dir = root.join(&candidate);
        let lock = pm.lock_for_paths(std::slice::from_ref(&dir))?;
        // Under the lock nobody else can allocate: the id is ours if
        // it is still the next one.
        let taken = match explicit_id {
            Some(id) => id.to_string(),
            None => format!("{}-{}", project.prefix, next_id(&root, &project.prefix)?),
        };
        if taken == candidate {
            return Ok((lock, candidate));
        }
        // Another writer took this id while we waited for the lock.
        // Give the lock back and take the next one.
        drop(lock);
    }
    // Contention, not a refusal: another writer took the candidate id
    // in every round. `busy` (exit 75) is the retryable kind, so a
    // contract-compliant caller retries instead of treating the
    // creation as permanently rejected.
    Err(Error::busy(
        "the tracker is too busy to allocate a new id right now — retry in a moment",
    ))
}

/// Refusals name the allowed form.
pub fn master_issue_new_limits(
    caller_is_master: bool,
    owner: Option<&str>,
    explicit_id: Option<&str>,
    status: Option<&str>,
) -> Result<()> {
    if let Some(status) = status {
        if status != "backlog" {
            return Err(Error::rejected(
                "issue new creates a backlog ticket — omit --status or pass --status backlog. \
                 The master cannot move status; that stays the operator's `issue set`",
            ));
        }
    }
    if !caller_is_master {
        return Ok(());
    }
    if owner.is_some() {
        return Err(Error::rejected(
            "the master creates backlog tickets without --owner — omit it; the ticket records actor=master",
        ));
    }
    if explicit_id.is_some() {
        return Err(Error::rejected(
            "the master does not choose ticket ids — omit --id",
        ));
    }
    Ok(())
}

/// `issue new` — one folder + issue.md, under the id lock.
/// `body`, when set, replaces the default title-plus-empty-acceptance
/// body (CAD-614: the master writes it under `master/tmp` and passes
/// `--file`).
#[allow(clippy::too_many_arguments)]
#[track_caller]
pub fn new_issue(
    pm: &Pm,
    cwd: &Path,
    project_flag: Option<&str>,
    title: &str,
    priority: Option<&str>,
    parent: Option<&str>,
    blocked_by: &[String],
    owner: Option<&str>,
    component: Option<&str>,
    tags: &[String],
    explicit_id: Option<&str>,
    body: Option<&str>,
    actor: &str,
) -> Result<Value> {
    new_issue_unless(
        pm,
        cwd,
        project_flag,
        title,
        priority,
        parent,
        blocked_by,
        owner,
        component,
        tags,
        explicit_id,
        body,
        actor,
        || Ok(None),
    )
}

/// [`new_issue`], but `decide` runs after this creation holds the tracker
/// lock and before any new folder is written. `Some` returns that value
/// and creates nothing — the app-ticket writer uses it to recheck the
/// request marker under the same lock that covers the commit.
#[allow(clippy::too_many_arguments)]
#[track_caller]
pub(super) fn new_issue_unless<F>(
    pm: &Pm,
    cwd: &Path,
    project_flag: Option<&str>,
    title: &str,
    priority: Option<&str>,
    parent: Option<&str>,
    blocked_by: &[String],
    owner: Option<&str>,
    component: Option<&str>,
    tags: &[String],
    explicit_id: Option<&str>,
    body: Option<&str>,
    actor: &str,
    decide: F,
) -> Result<Value>
where
    F: FnOnce() -> Result<Option<Value>>,
{
    let project = project::resolve(&pm.dir, project_flag, cwd)?;
    let priority = priority.unwrap_or("P2");
    model::check_priority(priority)?;
    if let Some(id) = explicit_id {
        model::check_id(id)?;
        let want = format!("{}-", project.prefix);
        if !id.starts_with(&want) {
            return Err(Error::rejected(format!(
                "--id '{id}' does not match project prefix '{want}'"
            )));
        }
    }
    if let Some(component) = component {
        check_component(&project, component)?;
    }
    let tags = check_tags(&project, tags)?;
    // CAD-1600: the lock is scoped to the new ticket's own directory,
    // not to the whole project, so a crashed writer's leftovers on
    // other tickets do not refuse an unrelated allocation. The id is
    // still allocated under the same real kernel lock, so concurrent
    // writers keep getting unique ids.
    let (lock, id) = lock_for_new_issue(pm, &project, explicit_id)?;
    if let Some(existing) = decide()? {
        return Ok(existing);
    }
    let dir = pm.dir.join(&project.key).join(&id);
    if dir.exists() {
        return Err(Error::rejected(format!(
            "Issue '{id}' already exists at {}",
            dir.display()
        )));
    }
    let mut front = Front::new(&id, title, &time::iso(time::now_epoch()));
    front.priority = priority.to_string();
    // Owner is intent — `default_owner` stays a project.yaml hint for
    // later dispatch; it is never stamped onto new issues.
    front.owner = owner.map(str::to_string);
    front.component = component.map(str::to_string);
    front.tags = tags;
    front.blocked_by = blocked_by.to_vec();
    for dep in &front.blocked_by {
        model::check_id(dep)?;
    }
    let body = body
        .map(str::to_string)
        .unwrap_or_else(|| format!("{title}\n\n## Acceptance\n\n"));
    if let Some(parent) = parent {
        model::check_id(parent)?;
        // CAD-360: nothing new is parented to a plan epic.
        crate::issue::plan::check_parent_change(&pm.dir, &front, Some(parent))?;
        front.parent = Some(parent.to_string());
    }
    std::fs::create_dir_all(dir.join("comments"))?;
    std::fs::create_dir_all(dir.join("artifacts"))?;
    save_front(&dir, &front, &body)?;
    // Structural checks run after the folder exists so failures leave
    // no half-built issue (the lock serialises the whole write).
    let issues = board::load_all(&pm.dir, None)?;
    if let Err(e) = check_structure(&issues, &id) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }
    let foreign = match commit_scoped(
        pm,
        &lock,
        &[dir.join("issue.md")],
        &format!("{id}: created"),
        &[&id],
        actor,
    ) {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    };
    let mut out = json!({"id": id, "project": project.key, "path": dir, "committed": true});
    attach_foreign(&mut out, &foreign);
    Ok(out)
}
