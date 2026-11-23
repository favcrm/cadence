//! Board wire-payload presentation, including contextual file/history reads.
//! Loading, filtering and status/readiness derivation remain in the parent.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use super::{Issue, View};
use crate::issue::model;
use cadence_core::issue::{CardChecks, CardCounts, IssueCard};
use serde_json::{json, Value};
use std::collections::HashMap;

mod details;

pub use details::{activity_json, detail, detail_json};

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
    serde_json::to_value(card_rev(view, rev)).expect("issue card serializes")
}

/// The typed card with its `rev` token supplied.
pub fn card_rev(view: &View, rev: &str) -> IssueCard {
    let f = &view.issue.front;
    IssueCard {
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
    }
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
