//! Field, tag, link, comment and artifact edits.

use super::*;

/// `key=value` pairs that set `status=done` — the one test `issue set`
/// and `issue edit` share for the worktree-open hint.
pub(crate) fn sets_done(pairs: &[String]) -> bool {
    pairs.iter().any(|p| {
        p.split_once('=').is_some_and(|(k, v)| {
            k.trim().eq_ignore_ascii_case("status") && v.trim().eq_ignore_ascii_case("done")
        })
    })
}

/// The gates a `set` runs after its pairs applied, comparing the
/// edited `front` against `prev` (the front as loaded). Shared by
/// `issue set` and `issue edit`, so neither can skip one. Returns the
/// recorded `--force` reason when the done-evidence gate was overridden.
/// `staged_artifacts` names attachments the same write is about to add
/// (only `issue edit` has any); they count as done-evidence.
pub(crate) fn check_set_gates(
    pm: &Pm,
    project: &project::Project,
    prev: &Front,
    front: &Front,
    force: Option<&str>,
    staged_artifacts: &[String],
) -> Result<Option<String>> {
    let mut forced = None;
    if front.status != prev.status {
        // CAD-360: an unapproved plan's tickets stay in backlog.
        crate::issue::plan::check_status_write(&pm.dir, front, &front.status)?;
        if front.status == "done" {
            check_done_evidence(pm, front, force, staged_artifacts)?;
            forced = force.map(|r| r.trim().to_string());
        }
        // CAD-757: `ready` is the dispatch queue — a blocked ticket
        // cannot sit in it.
        if front.status == "ready" {
            crate::issue::blocked::check_ready(&pm.dir, front)?;
        }
    }
    if front.milestone != prev.milestone {
        check_milestone(pm, project, front.milestone.as_deref())?;
    }
    if front.item_type != prev.item_type {
        check_type_change(pm, front)?;
    }
    Ok(forced)
}

/// `issue set <ID>… key=value…` — the writable frontmatter fields, on
/// one issue or several. All-or-nothing: every id and every pair is
/// validated before any file changes, and the batch is one commit.
/// `force` is the `--force <reason>` override for the status=done
/// evidence gate ([`check_done_evidence`]); every other write ignores it.
pub fn set_fields(
    pm: &Pm,
    ids: &[String],
    pairs: &[String],
    actor: &str,
    force: Option<&str>,
) -> Result<Value> {
    set_fields_if_rev(pm, ids, pairs, actor, force, None)
}

/// [`set_fields`] bound to the revision shown — the compare half of
/// compare-and-swap. The check runs inside the tracker lock, at the
/// point of write, so a revision move between read and write refuses
/// instead of landing. A mismatch answers `Ok` with a `conflict`
/// payload (the route maps it, like `patch_issue`'s); only callers
/// passing `if_rev` ever see one.
#[track_caller]
pub fn set_fields_if_rev(
    pm: &Pm,
    ids: &[String],
    pairs: &[String],
    actor: &str,
    force: Option<&str>,
    if_rev: Option<&str>,
) -> Result<Value> {
    if ids.is_empty() || pairs.is_empty() {
        return Err(Error::rejected(
            "set needs an id and key=value pairs — e.g. `cadence issue set CAD-16 status=doing`",
        ));
    }
    let dirs: Vec<PathBuf> = ids
        .iter()
        .map(|id| issue_dir(pm, id).map(|(_, dir)| dir))
        .collect::<Result<_>>()?;
    let lock = pm.lock_for_paths(&dirs)?;
    if let Some(want) = if_rev {
        for id in ids {
            let (_, dir) = issue_dir(pm, id)?;
            if let Some(conflict) = check_rev(&dir, Some(want))? {
                let mut out = conflict;
                out["id"] = serde_json::json!(id);
                return Ok(out);
            }
        }
    }
    let mut changed = Vec::new();
    let mut forced: Option<String> = None;
    let staged = stage(pm, ids, |project, front| {
        let prev = front.clone();
        changed = apply_pairs(project, front, pairs)?;
        if let Some(reason) = check_set_gates(pm, project, &prev, front, force, &[])? {
            forced = Some(reason);
        }
        Ok(true)
    })?;
    let summary = match &forced {
        Some(r) => format!("set {} (forced: {r})", changed.join(" ")),
        None => format!("set {}", changed.join(" ")),
    };
    let (ids, foreign) = commit_staged_scoped(pm, &lock, &staged, &summary, actor)?;
    // The post-merge reminder (CAD-94): when this set marks an issue
    // done while a worktree ref is still open, the CLI prints the
    // one-line `issue finish` hint for each of these ids.
    let worktree_open: Vec<&String> = if sets_done(pairs) {
        staged
            .iter()
            .filter(|s| {
                s.front
                    .refs
                    .iter()
                    .any(|r| r.kind == "worktree" && r.closed != Some(true))
            })
            .map(|s| &s.id)
            .collect()
    } else {
        Vec::new()
    };
    let mut out = json!({"id": ids[0], "ids": ids, "set": changed,
              "worktree_open": worktree_open, "committed": true});
    attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// `issue tag <ID>… add|rm <tag>…` — add or remove tags on one issue or
/// several, same all-or-nothing batch as `set`. Adding a tag an issue
/// already has, or removing one it lacks, leaves that issue unchanged;
/// a batch that changes nothing is refused.
#[track_caller]
pub fn tag_edit(pm: &Pm, ids: &[String], add: bool, tags: &[String], actor: &str) -> Result<Value> {
    let verb = if add { "add" } else { "rm" };
    if ids.is_empty() || tags.is_empty() {
        return Err(Error::rejected(
            "tag needs ids, add|rm and tags — e.g. `cadence issue tag CAD-16 CAD-17 add ui`",
        ));
    }
    let tags = model::normalize_tags(tags)?;
    let dirs: Vec<PathBuf> = ids
        .iter()
        .map(|id| issue_dir(pm, id).map(|(_, dir)| dir))
        .collect::<Result<_>>()?;
    let lock = pm.lock_for_paths(&dirs)?;
    let staged = stage(pm, ids, |project, front| {
        let before = front.tags.clone();
        if add {
            let mut all = before.clone();
            all.extend(tags.iter().cloned());
            front.tags = check_tags(project, &all)?;
        } else {
            front.tags.retain(|t| !tags.contains(t));
        }
        Ok(front.tags != before)
    })?;
    if staged.is_empty() {
        return Err(Error::rejected(format!(
            "tag {verb} {} changes nothing — `cadence issue show <ID>` lists tags",
            tags.join(" ")
        )));
    }
    let tagged: Vec<Value> = staged
        .iter()
        .map(|s| json!({"id": s.id, "tags": s.front.tags}))
        .collect();
    let (ids, foreign) = commit_staged_scoped(
        pm,
        &lock,
        &staged,
        &format!("tag {verb} {}", tags.join(" ")),
        actor,
    )?;
    let mut out =
        json!({"ids": ids, "tag": verb, "tags": tags, "issues": tagged, "committed": true});
    attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// The HTTP PATCH surface: typed fields instead of `key=value` pairs,
/// `body` replaces the markdown body (frontmatter untouched), `if_rev`
/// optimistic concurrency, and a derived status refuses `status`.
pub struct IssuePatch {
    pub status: Option<String>,
    pub priority: Option<String>,
    /// `Some("")` clears owner.
    pub owner: Option<String>,
    /// `Some("")` clears component.
    pub component: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    /// Replaces the tag list; `Some([])` clears it.
    pub tags: Option<Vec<String>>,
}

#[track_caller]
pub fn patch_issue(
    pm: &Pm,
    id: &str,
    patch: &IssuePatch,
    if_rev: Option<&str>,
    actor: &str,
    state_dir: Option<&Path>,
) -> Result<Value> {
    let (project, dir) = issue_dir(pm, id)?;
    let lock = pm.lock_for_paths(std::slice::from_ref(&dir))?;
    if let Some(conflict) = check_rev(&dir, if_rev)? {
        return Ok(conflict);
    }
    if patch.status.is_some() {
        // A derived status is not file-writable: roll-up containers,
        // job-bound and note-driven issues refuse with the reason.
        let issues = board::load_all(&pm.dir, None)?;
        let jobs = state_dir.map(board::fetch_job_outcomes).unwrap_or_default();
        let views = board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs);
        let src = views
            .iter()
            .find(|v| v.issue.front.id == id)
            .map(|v| v.status_source)
            .unwrap_or("file");
        if src != "file" {
            return Ok(json!({
                "id": id,
                "conflict": "status_derived",
                "status_source": src,
                "reason": format!(
                    "{id} status is derived from {src} — the file field is not authoritative"
                ),
            }));
        }
    }
    let (mut front, mut body) = load_front(&dir)?;
    let before = front.clone();
    let mut changed = Vec::new();
    if let Some(v) = &patch.status {
        model::check_status(v)?;
        front.status = v.clone();
        changed.push(format!("status={v}"));
    }
    if let Some(v) = &patch.priority {
        model::check_priority(v)?;
        front.priority = v.clone();
        changed.push(format!("priority={v}"));
    }
    if let Some(v) = &patch.title {
        if v.is_empty() {
            return Err(Error::rejected("title cannot be empty"));
        }
        front.title = v.clone();
        changed.push("title".to_string());
    }
    if let Some(v) = &patch.owner {
        front.owner = (!v.is_empty()).then(|| v.clone());
        changed.push(format!("owner={v}"));
    }
    if let Some(v) = &patch.component {
        if !v.is_empty() {
            check_component(&project, v)?;
        }
        front.component = (!v.is_empty()).then(|| v.clone());
        changed.push(format!("component={v}"));
    }
    if let Some(v) = &patch.tags {
        front.tags = check_tags(&project, v)?;
        changed.push(format!("tags={}", front.tags.join(",")));
    }
    if let Some(v) = &patch.body {
        body = v.clone();
        changed.push("body".to_string());
    }
    if changed.is_empty() {
        return Err(Error::rejected(
            "nothing to patch — send at least one field",
        ));
    }
    // CAD-971: the board is never weaker than `issue set` — the same
    // gates (plan approval, done-evidence, ready/blocked), no `--force`.
    check_set_gates(pm, &project, &before, &front, None, &[])?;
    let file = dir.join("issue.md");
    let prev = file_preimage(&file);
    save_front(&dir, &front, &body)?;
    let foreign = match commit_scoped(
        pm,
        &lock,
        std::slice::from_ref(&file),
        &format!("{id}: set {}", changed.join(" ")),
        &[id],
        actor,
    ) {
        Ok(f) => f,
        Err(e) => {
            restore_preimage(&file, prev);
            return Err(e);
        }
    };
    let mut post = Vec::new();
    let warnings = blocked_warnings_best_effort(pm, id, state_dir, &mut post);
    let mut out = json!({"id": id, "set": changed, "committed": true, "warnings": warnings});
    attach_post_warnings(&mut out, &post);
    attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// One link/unlink applied to `front` in memory — every check `issue
/// link` makes except the structural one over the whole board. Shared
/// with `issue edit`.
pub(crate) fn apply_link(
    pm: &Pm,
    front: &mut Front,
    kind: &str,
    target: &str,
    unlink: bool,
) -> Result<()> {
    let id = front.id.clone();
    let id = id.as_str();
    match kind {
        "blocked_by" | "relates" => {
            let list = if kind == "blocked_by" {
                &mut front.blocked_by
            } else {
                &mut front.relates
            };
            if unlink {
                if !list.iter().any(|t| t == target) {
                    return Err(Error::rejected(format!(
                        "{id} has no {kind} link to {target} — \
                         `cadence issue show {id}` lists links"
                    )));
                }
                list.retain(|t| t != target);
            } else {
                if target == id {
                    return Err(Error::rejected(format!("{id} cannot link to itself")));
                }
                board::find_issue(&pm.dir, target)?; // must exist
                if list.iter().any(|t| t == target) {
                    return Err(Error::rejected(format!(
                        "{id} already links {kind} {target} — nothing to do"
                    )));
                }
                list.push(target.to_string());
            }
        }
        "parent" | "duplicate_of" => {
            if kind == "parent" {
                // CAD-360: a plan ticket keeps its epic; nothing joins
                // a plan by link.
                crate::issue::plan::check_parent_change(
                    &pm.dir,
                    front,
                    (!unlink).then_some(target),
                )?;
            }
            let slot = if kind == "parent" {
                &mut front.parent
            } else {
                &mut front.duplicate_of
            };
            if unlink {
                if slot.as_deref() != Some(target) {
                    return Err(Error::rejected(format!(
                        "{id} has {kind}={} — not {target}",
                        slot.as_deref().unwrap_or("none")
                    )));
                }
                *slot = None;
            } else {
                if target == id {
                    return Err(Error::rejected(format!("{id} cannot link to itself")));
                }
                board::find_issue(&pm.dir, target)?;
                *slot = Some(target.to_string());
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

/// `issue link` / `issue unlink`. `blocked_by`/`relates` are list
/// fields; `parent`/`duplicate_of` are scalars.
#[allow(clippy::too_many_arguments)]
#[track_caller]
pub fn link(
    pm: &Pm,
    id: &str,
    kind: &str,
    target: &str,
    unlink: bool,
    if_rev: Option<&str>,
    actor: &str,
    state_dir: Option<&Path>,
) -> Result<Value> {
    model::check_link_kind(kind)?;
    model::check_id(target)?;
    let (_project, dir) = issue_dir(pm, id)?;
    let lock = pm.lock_for_paths(std::slice::from_ref(&dir))?;
    if let Some(conflict) = check_rev(&dir, if_rev)? {
        return Ok(conflict);
    }
    let (mut front, body) = load_front(&dir)?;
    let verb = if unlink { "unlink" } else { "link" };
    apply_link(pm, &mut front, kind, target, unlink)?;
    // Structural check before the file is written — a rejected link
    // leaves the folder untouched.
    let issues = board::load_all(&pm.dir, None)?;
    let mut preview: Vec<board::Issue> = issues;
    if let Some(this) = preview.iter_mut().find(|i| i.front.id == id) {
        this.front = front.clone();
    }
    check_structure(&preview, id)?;
    let file = dir.join("issue.md");
    let prev = file_preimage(&file);
    save_front(&dir, &front, &body)?;
    let foreign = match commit_scoped(
        pm,
        &lock,
        std::slice::from_ref(&file),
        &format!("{id}: {verb} {kind} {target}"),
        &[id, target],
        actor,
    ) {
        Ok(f) => f,
        Err(e) => {
            restore_preimage(&file, prev);
            return Err(e);
        }
    };
    let mut post = Vec::new();
    let warnings = blocked_warnings_best_effort(pm, id, state_dir, &mut post);
    let mut out = json!({"id": id, "link": kind, "target": target,
              "unlink": unlink, "committed": true, "warnings": warnings});
    attach_post_warnings(&mut out, &post);
    // CAD-757: a new blocker on an active issue tells its holders —
    // by name, best-effort, never blocking the link itself.
    if kind == "blocked_by" && !unlink {
        out["blocker_notice"] =
            crate::issue::blocked::notify_new_blocker(&front, target, state_dir);
    }
    attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// A ref as `issue ref` stores it: an http(s) target is a `url`,
/// anything else a `path`. Shared with `issue edit`.
pub(crate) fn new_ref(
    kind: &str,
    target: &str,
    label: Option<&str>,
    worktree: Option<&str>,
    agent: Option<&str>,
) -> Ref {
    let is_url = target.starts_with("http://") || target.starts_with("https://");
    Ref {
        kind: kind.to_string(),
        url: is_url.then(|| target.to_string()),
        path: (!is_url).then(|| target.to_string()),
        label: label.map(str::to_string),
        closed: None,
        worktree: worktree.map(str::to_string),
        cargo_target: None,
        agent: agent.map(str::to_string),
    }
}

/// `issue ref <ID> <kind> <url-or-path> [--label x]`. A scheme makes it
/// `url:`; everything else is a `path:` (previews store publish paths).
/// `worktree` scopes a `message` ref to the pair it was dispatched
/// against; other kinds leave it `None`.
#[allow(clippy::too_many_arguments)]
#[track_caller]
pub fn add_ref(
    pm: &Pm,
    id: &str,
    kind: &str,
    target: &str,
    label: Option<&str>,
    worktree: Option<&str>,
    agent: Option<&str>,
    if_rev: Option<&str>,
    actor: &str,
) -> Result<Value> {
    model::check_ref_kind(kind)?;
    model::check_ref_value(target)?;
    let (_project, dir) = issue_dir(pm, id)?;
    let lock = pm.lock_for_paths(std::slice::from_ref(&dir))?;
    if let Some(conflict) = check_rev(&dir, if_rev)? {
        return Ok(conflict);
    }
    let (mut front, body) = load_front(&dir)?;
    front
        .refs
        .push(new_ref(kind, target, label, worktree, agent));
    let file = dir.join("issue.md");
    let prev = file_preimage(&file);
    save_front(&dir, &front, &body)?;
    let foreign = match commit_scoped(
        pm,
        &lock,
        std::slice::from_ref(&file),
        &format!("{id}: ref {kind}"),
        &[id],
        actor,
    ) {
        Ok(f) => f,
        Err(e) => {
            restore_preimage(&file, prev);
            return Err(e);
        }
    };
    let mut out = json!({"id": id, "ref": {"kind": kind, "target": target},
              "committed": true});
    attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// Mark every open `<kind>` ref naming `target` closed — kept as
/// history but no longer counted by `issue finish`. A dispatch whose
/// send failed leaves its pre-recorded `message` ref orphaned;
/// closing it keeps the attempt's history without letting it read as
/// a live binding (or a "dispatch recorded during finish" stale
/// reason) to a concurrent finish.
#[track_caller]
pub fn close_ref(pm: &Pm, id: &str, kind: &str, target: &str, actor: &str) -> Result<()> {
    let (_project, dir) = issue_dir(pm, id)?;
    let lock = pm.lock_for_paths(std::slice::from_ref(&dir))?;
    let (mut front, body) = load_front(&dir)?;
    let mut hit = false;
    for r in &mut front.refs {
        if r.kind == kind && r.path.as_deref() == Some(target) && r.closed != Some(true) {
            r.closed = Some(true);
            hit = true;
        }
    }
    if hit {
        let file = dir.join("issue.md");
        let prev = file_preimage(&file);
        save_front(&dir, &front, &body)?;
        if let Err(e) = commit_scoped(
            pm,
            &lock,
            std::slice::from_ref(&file),
            &format!("{id}: ref {kind} closed"),
            &[id],
            actor,
        ) {
            restore_preimage(&file, prev);
            return Err(e);
        }
    }
    Ok(())
}

/// The author a comment is recorded under: `--author`, else
/// `CADENCE_ALIAS`, else `operator` — one derivation for `issue
/// comment` and `issue edit`, so neither opens a way to forge identity.
pub(crate) fn comment_author(author: Option<&str>) -> Result<String> {
    let author = author
        .map(str::to_string)
        .or_else(|| std::env::var("CADENCE_ALIAS").ok())
        .unwrap_or_else(|| "operator".to_string());
    if author.is_empty()
        || author.len() > 64
        || !author
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(Error::rejected(format!(
            "Bad comment author '{author}' — letters, digits, '-' or '_'"
        )));
    }
    Ok(author)
}

/// `issue comment <ID> -m|--file` — one create-only file under
/// `comments/`, named by UTC time + author.
#[track_caller]
pub fn add_comment(
    pm: &Pm,
    id: &str,
    body: &str,
    author: Option<&str>,
    kind: Option<&str>,
    if_rev: Option<&str>,
    actor: &str,
) -> Result<Value> {
    let (_project, dir) = issue_dir(pm, id)?;
    let author_opt = author;
    let author = comment_author(author_opt)?;
    if body.trim().is_empty() {
        return Err(Error::rejected("Comment body is empty — pass -m or --file"));
    }
    // CAD-109: a credential-shaped body is refused before anything is
    // written; warn-only findings ride along in the result.
    let secret_warnings = crate::secret::guard(&format!("{id}: comment"), body)?;
    let lock = pm.lock_for_paths(std::slice::from_ref(&dir))?;
    if let Some(conflict) = check_rev(&dir, if_rev)? {
        return Ok(conflict);
    }
    let comments = dir.join("comments");
    if comments.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{id}: comments/ is a symlink — refusing to write outside the PM dir"
        )));
    }
    std::fs::create_dir_all(&comments)?;
    let epoch = time::now_epoch();
    let front = crate::issue::model::CommentFront {
        author: author.clone(),
        at: time::iso(epoch),
        kind: kind.map(str::to_string),
    };
    let text = parse::render(&front, body)?;
    let path = create_exclusive(
        &comments,
        &format!("{}-{author}.md", time::basic(epoch)),
        text.as_bytes(),
    )?;
    // A failed commit leaves no comment file behind — an orphan would
    // sit foreign and uncommitted under the next writer's eye.
    let foreign = match commit_who_scoped(
        pm,
        &lock,
        std::slice::from_ref(&path),
        &format!("{id}: comment by {author}"),
        &[id],
        actor,
        author_opt,
    ) {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
    };
    let mut out = json!({"id": id, "comment": path.file_name().map(|n| n.to_string_lossy().to_string()),
              "author": author, "committed": true});
    if !secret_warnings.is_empty() {
        out["secret_warnings"] = crate::secret::warnings_json(&secret_warnings);
    }
    attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// CAD-383: write `front` and one comment by `author` in a single tracker
/// commit (`subject` without the id). Call under the PM lock. A failed
/// commit restores `prev` and removes the comment, so a refusal leaves
/// the issue as it was.
#[allow(clippy::too_many_arguments)]
pub(crate) fn commit_front_with_comment(
    pm: &Pm,
    dir: &Path,
    prev: &Front,
    front: &Front,
    body: &str,
    author: &str,
    kind: &str,
    text: &str,
    subject: &str,
    actor: &str,
) -> Result<String> {
    let id = front.id.as_str();
    crate::issue::claim::check_alias(author, "Claimant")?;
    // CAD-109: a credential-shaped note or reason is refused here too.
    crate::secret::guard(&format!("{id}: comment"), text)?;
    let comments = dir.join("comments");
    if comments.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{id}: comments/ is a symlink — refusing to write outside the PM dir"
        )));
    }
    std::fs::create_dir_all(&comments)?;
    let epoch = time::now_epoch();
    let meta = model::CommentFront {
        author: author.to_string(),
        at: time::iso(epoch),
        kind: Some(kind.to_string()),
    };
    let rendered = parse::render(&meta, text)?;
    let path = create_exclusive(
        &comments,
        &format!("{}-{author}.md", time::basic(epoch)),
        rendered.as_bytes(),
    )?;
    let committed = save_front(dir, front, body).and_then(|_| {
        commit_who(
            pm,
            &[dir.join("issue.md"), path.clone()],
            &format!("{id}: {subject}"),
            &[id],
            actor,
            Some(author),
        )
    });
    if let Err(e) = committed {
        let _ = save_front(dir, prev, body);
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    Ok(path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default())
}

/// Store one already-validated task report (CAD-341) as a create-only
/// file under `reports/`, named by UTC time + agent. Byte-identical
/// content already on the ticket is not written twice: the existing
/// file is returned with `duplicate: true` and nothing is committed.
#[track_caller]
pub fn add_report(
    pm: &Pm,
    id: &str,
    front: &crate::issue::task_report::Front,
    body: &str,
    actor: &str,
) -> Result<Value> {
    use crate::issue::task_report::DIR;
    let (_project, dir) = issue_dir(pm, id)?;
    let agent = front.agent.clone().unwrap_or_default();
    let kind = front.kind.map(|k| k.as_str()).unwrap_or_default();
    let text = parse::render(front, body)?;
    let lock = pm.lock_for_paths(std::slice::from_ref(&dir))?;
    let reports = dir.join(DIR);
    if reports.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{id}: {DIR}/ is a symlink — refusing to write outside the PM dir"
        )));
    }
    std::fs::create_dir_all(&reports)?;
    let out = |name: &str, duplicate: bool| {
        json!({"id": id, "report": name, "path": format!("{id}/{DIR}/{name}"),
               "kind": kind, "agent": agent, "committed": !duplicate,
               "duplicate": duplicate})
    };
    for name in crate::issue::task_report::names(&dir) {
        if std::fs::read(reports.join(&name)).is_ok_and(|b| b == text.as_bytes()) {
            return Ok(out(&name, true));
        }
    }
    let path = create_exclusive(
        &reports,
        &format!("{}-{agent}.md", time::basic(time::now_epoch())),
        text.as_bytes(),
    )?;
    // A failed commit must not leave the file behind: a retry would hit
    // the duplicate short-circuit and never commit it.
    let foreign = match commit_who_scoped(
        pm,
        &lock,
        std::slice::from_ref(&path),
        &format!("{id}: report {kind} by {agent}"),
        &[id],
        actor,
        Some(&agent),
    ) {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut out = out(&name, false);
    attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// `issue attach <ID> <file>` — copy into `artifacts/` (basename only),
/// create-only, under `artifact_max_bytes`.
pub fn attach(pm: &Pm, id: &str, file: &Path) -> Result<Value> {
    let meta = std::fs::metadata(file)
        .map_err(|e| Error::rejected(format!("Cannot read {}: {e}", file.display())))?;
    if !meta.is_file() {
        return Err(Error::rejected(format!("{} is not a file", file.display())));
    }
    if meta.len() > pm.config.artifact_max_bytes {
        return Err(Error::rejected(format!(
            "{} is {} bytes — over the {} cap; link it instead: \
             `cadence issue ref {id} url <location>`",
            file.display(),
            meta.len(),
            pm.config.artifact_max_bytes
        )));
    }
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| Error::rejected("Attachment needs a plain file name"))?;
    let bytes = std::fs::read(file)?;
    attach_bytes(pm, id, &name, &bytes, true, "")
}

/// Attach raw bytes as `artifacts/<name>` — the upload route's path.
/// Names follow the read grammar so every stored file is fetchable.
/// With `autorename` a collision becomes `<stem>-N<ext>` (CLI parity);
/// without it an existing name is a conflict the route maps to 409.
#[track_caller]
pub fn attach_bytes(
    pm: &Pm,
    id: &str,
    name: &str,
    bytes: &[u8],
    autorename: bool,
    actor: &str,
) -> Result<Value> {
    let (_project, dir) = issue_dir(pm, id)?;
    if !model::valid_artifact_name(name) {
        return Err(Error::rejected(format!(
            "Bad artifact name '{name}' — [A-Za-z0-9._-]{{1,120}}, no leading dot"
        )));
    }
    if model::reserved_artifact_name(name) {
        return Err(Error::rejected(format!(
            "{name} is a tracker-owned record and cannot be attached: only its own workflow writes it"
        )));
    }
    if bytes.len() as u64 > pm.config.artifact_max_bytes {
        return Err(Error::rejected(format!(
            "{name} is {} bytes — over the {} cap; link it instead",
            bytes.len(),
            pm.config.artifact_max_bytes
        )));
    }
    let lock = pm.lock_for_paths(std::slice::from_ref(&dir))?;
    let artifacts = dir.join("artifacts");
    if artifacts.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{id}: artifacts/ is a symlink — refusing to write outside the PM dir"
        )));
    }
    std::fs::create_dir_all(&artifacts)?;
    let path = if autorename {
        create_exclusive(&artifacts, name, bytes)?
    } else {
        let target = artifacts.join(name);
        match write_new(&target, bytes) {
            Ok(()) => target,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Ok(json!({"id": id, "conflict": "exists", "artifact": name}));
            }
            Err(e) => return Err(e.into()),
        }
    };
    let foreign = match commit_scoped(
        pm,
        &lock,
        std::slice::from_ref(&path),
        &format!("{id}: attach {name}"),
        &[id],
        actor,
    ) {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
    };
    let mut out = json!({"id": id, "artifact": path.file_name().map(|n| n.to_string_lossy().to_string()),
              "size": bytes.len(), "committed": true});
    attach_foreign(&mut out, &foreign);
    Ok(out)
}
