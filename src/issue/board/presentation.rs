//! Board wire-payload presentation, including contextual file/history reads.
//! Loading, filtering and status/readiness derivation remain in the parent.

use super::{Issue, View};
use crate::issue::{history, model, parse};
use cadence_core::issue::{CardChecks, CardCounts, IssueCard};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

/// Content hash of the issue's `issue.md` — the `if_rev` token write
/// calls may send for optimistic concurrency.
pub fn rev_of(issue: &Issue) -> Value {
    json!(crate::issue::write::issue_rev(&issue.dir).unwrap_or_default())
}

/// Compact card payload for `GET /api/issues` and `issue ls --json`.
pub fn card_json(view: &View) -> Value {
    card_json_rev(
        view,
        &crate::issue::write::issue_rev(&view.issue.dir).unwrap_or_default(),
    )
}

/// [`card_json`] with the `rev` token supplied — the board's read model
/// hashes `issue.md` once per change instead of once per card render.
pub fn card_json_rev(view: &View, rev: &str) -> Value {
    let f = &view.issue.front;
    let card = IssueCard {
        id: f.id.clone(),
        project: view.issue.project.clone(),
        title: f.title.clone(),
        status: view.status.clone(),
        status_source: view.status_source.to_string(),
        priority: f.priority.clone(),
        owner: f.owner.clone(),
        claim: crate::issue::claim::card(f, crate::issue::time::now_epoch()),
        component: f.component.clone(),
        tags: f.tags.clone(),
        paths: f.paths.clone(),
        parent: f.parent.clone(),
        blocked_by: f.blocked_by.clone(),
        relates: f.relates.clone(),
        duplicate_of: f.duplicate_of.clone(),
        refs: f.refs.clone(),
        container: view.container,
        ready: view.ready,
        blocked: view.blocked,
        blocked_reason: view.blocked_reason.map(str::to_string),
        created: f.created.clone(),
        rev: rev.to_string(),
        counts: CardCounts {
            comments: view.issue.comments.len(),
            artifacts: view.issue.artifacts.len(),
            refs: f.refs.len(),
        },
        checks: CardChecks {
            done: view.checks_done,
            total: view.checks_total,
        },
    };
    serde_json::to_value(card).expect("issue card serializes")
}

/// One epic — an issue with children — with its children's progress:
/// `total`, a count per status (derived statuses), `done_ratio` =
/// done ÷ (total − dropped), how many open children are blocked, and
/// the distinct owners. Children outside `views` are not counted.
pub fn epic_json(epic: &View, views_by_id: &HashMap<String, &View>) -> Value {
    let kids: Vec<&View> = epic
        .children
        .iter()
        .filter_map(|id| views_by_id.get(id).copied())
        .collect();
    let count = |status: &str| kids.iter().filter(|k| k.status == status).count();
    let counts: serde_json::Map<String, Value> = model::STATUSES
        .iter()
        .map(|s| (s.to_string(), json!(count(s))))
        .collect();
    let live = kids.len() - count("dropped");
    let done_ratio = if live == 0 {
        0.0
    } else {
        (count("done") as f64 / live as f64 * 100.0).round() / 100.0
    };
    let blocked = kids
        .iter()
        .filter(|k| k.blocked && !matches!(k.status.as_str(), "done" | "dropped"))
        .count();
    let mut owners: Vec<&str> = kids
        .iter()
        .filter_map(|k| k.issue.front.owner.as_deref())
        .collect();
    owners.sort_unstable();
    owners.dedup();
    let f = &epic.issue.front;
    json!({
        "id": f.id,
        "project": epic.issue.project,
        "title": f.title,
        "status": epic.status,
        "priority": f.priority,
        "owner": f.owner,
        "tags": f.tags,
        "total": kids.len(),
        "counts": counts,
        "done_ratio": done_ratio,
        "blocked": blocked,
        "owners": owners,
        "children": epic.children,
    })
}

/// Every epic in `views`, optionally one project's, in id order.
pub fn epics_json(views: &[View], project_key: Option<&str>) -> Vec<Value> {
    let by_id: HashMap<String, &View> = views
        .iter()
        .map(|v| (v.issue.front.id.clone(), v))
        .collect();
    views
        .iter()
        .filter(|v| v.container && project_key.is_none_or(|p| v.issue.project == p))
        .map(|v| epic_json(v, &by_id))
        .collect()
}

fn link_ref(views_by_id: &HashMap<String, &View>, id: &str) -> Value {
    match views_by_id.get(id) {
        Some(v) => json!({
            "id": id,
            "title": v.issue.front.title,
            "status": v.status,
            "status_source": v.status_source,
            "missing": false,
        }),
        None => json!({"id": id, "missing": true}),
    }
}

/// Full drawer payload for `GET /api/issues/:id` and `issue show --json`.
/// `history` is `git log` for issue.md — cheap at this scale.
pub fn detail_json(pm_dir: &Path, view: &View, views_by_id: &HashMap<String, &View>) -> Value {
    let f = &view.issue.front;
    let acceptance: Vec<Value> = parse::acceptance_items(&view.issue.body)
        .iter()
        .map(parse::AcceptanceItem::to_json)
        .collect();
    let link_list = |ids: &[String]| {
        ids.iter()
            .map(|id| link_ref(views_by_id, id))
            .collect::<Vec<_>>()
    };
    let links = json!({
        "parent": f.parent.as_ref().map(|p| link_ref(views_by_id, p)),
        "children": link_list(&view.children),
        "blocked_by": link_list(&f.blocked_by),
        "blocks": link_list(&view.blocks),
        "relates": link_list(&f.relates),
        "duplicate_of": f.duplicate_of.as_ref().map(|d| link_ref(views_by_id, d)),
        "duplicates": link_list(&view.duplicates),
    });
    let comments: Vec<Value> = view
        .issue
        .comments
        .iter()
        .map(|c| {
            json!({
                "name": c.name, "author": c.front.author, "at": c.front.at,
                "kind": c.front.kind, "body": c.body,
            })
        })
        .collect();
    let chain: Vec<Value> = view
        .chain
        .iter()
        .map(|n| json!({"name": n.name, "kind": n.kind, "at": n.at, "title": n.title}))
        .collect();
    let history = git_log(pm_dir, &view.issue);
    let activity = activity_json(pm_dir, view);
    let (commits, commits_skipped) = history::code_commits(pm_dir, &view.issue);
    json!({
        "id": f.id,
        "project": view.issue.project,
        "title": f.title,
        "status": view.status,
        "status_source": view.status_source,
        "priority": f.priority,
        "owner": f.owner,
        "claim": crate::issue::claim::json(f, crate::issue::time::now_epoch()),
        "component": f.component,
        "tags": f.tags,
        "paths": f.paths,
        "parent": f.parent,
        "container": view.container,
        "ready": view.ready,
        "blocked": view.blocked,
        "blocked_reason": view.blocked_reason,
        "created": f.created,
        "rev": rev_of(&view.issue),
        "frontmatter": f,
        "body": view.issue.body,
        "path": view.issue.dir.join("issue.md"),
        "links": links,
        "refs": f.refs,
        "artifacts": view.issue.artifacts.iter()
            .map(|(name, size)| json!({"name": name, "size": size}))
            .collect::<Vec<_>>(),
        "comments": comments,
        "reports": crate::issue::task_report::list(&view.issue.dir, &f.id),
        "notes_chain": chain,
        "acceptance": acceptance,
        "size": f.size,
        // CAD-359/360: plan state, tickets and weighted progress — null
        // unless this issue is a plan epic.
        "plan": crate::issue::plan::plan_json(view, views_by_id),
        "history": history,
        "activity": activity,
        "commits": commits,
        "commits_skipped": commits_skipped,
        "checks": {"done": view.checks_done, "total": view.checks_total},
    })
}

/// One merged activity stream: notes chain + comments + file commits.
/// `GET /api/issues/:id/activity` serves this directly; the drawer gets
/// it inside `detail_json`.
pub fn activity_json(pm_dir: &Path, view: &View) -> Vec<Value> {
    let mut activity: Vec<Value> = Vec::new();
    for n in &view.chain {
        activity.push(json!({
            "at": n.at, "kind": "note", "note_kind": n.kind,
            "title": n.title, "name": n.name,
        }));
    }
    for c in &view.issue.comments {
        activity.push(json!({
            "at": c.front.at, "kind": "comment", "author": c.front.author,
            "body": c.body, "name": c.name,
        }));
    }
    for h in git_log(pm_dir, &view.issue) {
        activity.push(json!({
            "at": h["at"], "kind": "commit",
            "commit": h["commit"], "subject": h["subject"],
        }));
    }
    activity.sort_by(|a, b| a["at"].as_str().cmp(&b["at"].as_str()));
    activity
}

/// `git log --format` for this issue's `issue.md` — the file history
/// half of the activity feed. Empty when the PM dir is not a repo.
fn git_log(pm_dir: &Path, issue: &Issue) -> Vec<Value> {
    let rel = issue
        .dir
        .join("issue.md")
        .strip_prefix(pm_dir)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| issue.dir.join("issue.md"));
    let out = crate::reaper::output(
        std::process::Command::new("git")
            .arg("-C")
            .arg(pm_dir)
            // No --follow: identical issue.md templates across siblings trip
            // rename detection and leak other issues' creation commits.
            .args(["log", "--format=%h|%aI|%s", "--"])
            .arg(&rel),
    );
    let Ok(out) = out else { return vec![] };
    if !out.status.success() {
        return vec![];
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '|');
            Some(json!({
                "commit": parts.next()?,
                "at": parts.next()?,
                "subject": parts.next().unwrap_or_default(),
            }))
        })
        .collect()
}
