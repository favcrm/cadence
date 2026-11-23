//! Issue detail, links, and merged activity presentation helpers.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use super::super::{Issue, View};
use crate::issue::{history, parse};
use cadence_core::board_api::{
    ActivityEntry, ArtifactRow, CommentRow, HistoryEntry, IssueDetail, IssueLinks, LinkRef, NoteRow,
};
use cadence_core::issue::CardChecks;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

fn link_ref(views_by_id: &HashMap<String, &View>, id: &str) -> LinkRef {
    match views_by_id.get(id) {
        Some(v) => LinkRef {
            id: id.to_string(),
            title: Some(v.issue.front.title.clone()),
            status: Some(v.status.clone()),
            status_source: Some(v.status_source.to_string()),
            missing: false,
        },
        None => LinkRef {
            id: id.to_string(),
            title: None,
            status: None,
            status_source: None,
            missing: true,
        },
    }
}

/// Full drawer payload for `GET /api/issues/:id` and `issue show --json`.
/// `history` is `git log` for issue.md — cheap at this scale.
pub fn detail_json(pm_dir: &Path, view: &View, views_by_id: &HashMap<String, &View>) -> Value {
    serde_json::to_value(detail(pm_dir, view, views_by_id)).expect("issue detail serializes")
}

/// [`detail_json`] as the typed wire struct, without `work` and `agents`.
pub fn detail(pm_dir: &Path, view: &View, views_by_id: &HashMap<String, &View>) -> IssueDetail {
    let f = &view.issue.front;
    let link_list = |ids: &[String]| ids.iter().map(|id| link_ref(views_by_id, id)).collect();
    let history = git_log(pm_dir, &view.issue);
    let (commits, commits_skipped) = history::code_commits(pm_dir, &view.issue);
    IssueDetail {
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
        container: view.container,
        ready: view.ready,
        blocked: view.blocked,
        blocked_reason: view.blocked_reason.map(str::to_string),
        created: f.created.clone(),
        rev: crate::issue::write::issue_rev(&view.issue.dir).unwrap_or_default(),
        frontmatter: serde_json::to_value(f).expect("front matter serializes"),
        body: view.issue.body.clone(),
        path: view
            .issue
            .dir
            .join("issue.md")
            .to_string_lossy()
            .into_owned(),
        links: IssueLinks {
            parent: f.parent.as_ref().map(|p| link_ref(views_by_id, p)),
            children: link_list(&view.children),
            blocked_by: link_list(&f.blocked_by),
            blocks: link_list(&view.blocks),
            relates: link_list(&f.relates),
            duplicate_of: f.duplicate_of.as_ref().map(|d| link_ref(views_by_id, d)),
            duplicates: link_list(&view.duplicates),
        },
        refs: f.refs.clone(),
        artifacts: view
            .issue
            .artifacts
            .iter()
            .map(|(name, size)| ArtifactRow {
                name: name.clone(),
                size: *size,
            })
            .collect(),
        comments: view
            .issue
            .comments
            .iter()
            .map(|c| CommentRow {
                name: c.name.clone(),
                author: c.front.author.clone(),
                at: c.front.at.clone(),
                kind: c.front.kind.clone(),
                body: c.body.clone(),
            })
            .collect(),
        reports: crate::issue::task_report::list(&view.issue.dir, &f.id),
        notes_chain: view
            .chain
            .iter()
            .map(|n| NoteRow {
                name: n.name.clone(),
                kind: n.kind.clone(),
                at: n.at.clone(),
                title: n.title.clone(),
            })
            .collect(),
        acceptance: parse::acceptance_items(&view.issue.body)
            .iter()
            .map(parse::AcceptanceItem::row)
            .collect(),
        size: f.size.clone(),
        // CAD-359/360: plan state, tickets and weighted progress — null
        // unless this issue is a plan epic.
        plan: crate::issue::plan::plan_json(view, views_by_id),
        history: history.clone(),
        activity: activity(view, history),
        commits,
        commits_skipped,
        checks: CardChecks {
            done: view.checks_done,
            total: view.checks_total,
        },
        work: None,
        agents: None,
    }
}

/// One merged activity stream: notes chain + comments + file commits.
/// `GET /api/issues/:id/activity` serves this directly; the drawer gets
/// it inside `detail_json`.
pub fn activity_json(pm_dir: &Path, view: &View) -> Vec<Value> {
    activity(view, git_log(pm_dir, &view.issue))
        .into_iter()
        .map(|a| serde_json::to_value(a).expect("activity serializes"))
        .collect()
}

fn activity(view: &View, history: Vec<HistoryEntry>) -> Vec<ActivityEntry> {
    let entry = |at: &str, kind: &str| ActivityEntry {
        at: at.to_string(),
        kind: kind.to_string(),
        note_kind: None,
        title: None,
        name: None,
        author: None,
        body: None,
        commit: None,
        subject: None,
    };
    let mut activity: Vec<ActivityEntry> = Vec::new();
    for n in &view.chain {
        activity.push(ActivityEntry {
            note_kind: Some(n.kind.clone()),
            title: Some(n.title.clone()),
            name: Some(n.name.clone()),
            ..entry(&n.at, "note")
        });
    }
    for c in &view.issue.comments {
        activity.push(ActivityEntry {
            author: Some(c.front.author.clone()),
            body: Some(c.body.clone()),
            name: Some(c.name.clone()),
            ..entry(&c.front.at, "comment")
        });
    }
    for h in history {
        activity.push(ActivityEntry {
            commit: Some(h.commit),
            subject: Some(h.subject),
            ..entry(&h.at, "commit")
        });
    }
    activity.sort_by(|a, b| a.at.cmp(&b.at));
    activity
}

/// `git log --format` for this issue's `issue.md` — the file history
/// half of the activity feed. Empty when the PM dir is not a repo.
fn git_log(pm_dir: &Path, issue: &Issue) -> Vec<HistoryEntry> {
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
            Some(HistoryEntry {
                commit: parts.next()?.to_string(),
                at: parts.next()?.to_string(),
                subject: parts.next().unwrap_or_default().to_string(),
            })
        })
        .collect()
}
