//! Link and external-ref mutations for issues.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::Path;

use serde_json::{json, Value};

use super::super::{
    attach_foreign, attach_post_warnings, blocked_warnings_best_effort, check_rev, check_structure,
    commit_scoped, file_preimage, issue_dir, load_front, require_project_active, restore_preimage,
    save_front, Ref,
};
use crate::error::{Error, Result};
use crate::issue::model::{self, Front};
use crate::issue::{board, Pm};

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
    require_project_active(&_project)?;
    if let Ok((target_project, _)) = issue_dir(pm, target) {
        require_project_active(&target_project)?;
    }
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
    require_project_active(&_project)?;
    if let Ok((target_project, _)) = issue_dir(pm, target) {
        require_project_active(&target_project)?;
    }
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
    require_project_active(&_project)?;
    if let Ok((target_project, _)) = issue_dir(pm, target) {
        require_project_active(&target_project)?;
    }
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
