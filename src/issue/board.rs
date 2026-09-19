//! Read side: load every issue folder under the PM dir and compute the
//! board view — derived status, readiness, inverse links, counts.
//! Everything derived is computed here; the folders store only what
//! cannot be derived.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::model::{self, CommentFront, Front};
use crate::issue::{history, notes, parse, project};

/// A loaded issue folder.
#[derive(Clone, Debug)]
pub struct Issue {
    pub project: String,
    pub dir: PathBuf,
    pub front: Front,
    pub body: String,
    pub comments: Vec<Comment>,
    /// `(basename, size)` under `artifacts/`.
    pub artifacts: Vec<(String, u64)>,
}

#[derive(Clone, Debug)]
pub struct Comment {
    pub name: String,
    pub front: CommentFront,
    pub body: String,
}

/// An issue plus everything the board derives from the set.
#[derive(Clone, Debug)]
pub struct View {
    pub issue: Issue,
    /// Effective status after derivation.
    pub status: String,
    /// `rollup` | `notes` | `file` — where `status` came from.
    pub status_source: &'static str,
    /// Has children — never dispatched; status is a roll-up.
    pub container: bool,
    /// status is backlog|ready and every blocked_by target is done|dropped.
    pub ready: bool,
    /// At least one blocked_by target is not done|dropped.
    pub blocked: bool,
    pub children: Vec<String>,
    pub blocks: Vec<String>,
    pub duplicates: Vec<String>,
    /// Notes chain tagged to this issue (empty without `Issue:` lines).
    pub chain: Vec<notes::Note>,
    /// `Some("job blocked")` when the bound job's least-advanced live
    /// task is blocked — the status itself stays notes-or-file.
    pub blocked_reason: Option<&'static str>,
    pub checks_done: u64,
    pub checks_total: u64,
}

/// `lstat`-style truth: a real directory, never a symlink. PM folders
/// must be actual folders — a link could point outside the PM dir.
pub fn is_real_dir(path: &Path) -> bool {
    path.symlink_metadata()
        .map(|m| m.is_dir() && !m.file_type().is_symlink())
        .unwrap_or(false)
}

/// `lstat`-style truth: a real file, never a symlink.
pub fn is_real_file(path: &Path) -> bool {
    path.symlink_metadata()
        .map(|m| m.is_file() && !m.file_type().is_symlink())
        .unwrap_or(false)
}

fn read_comments(dir: &Path) -> Vec<Comment> {
    let mut out = Vec::new();
    let comments_dir = dir.join("comments");
    if !is_real_dir(&comments_dir) {
        return out;
    }
    let Ok(entries) = std::fs::read_dir(&comments_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        // file_type() does not follow links — a symlink is not a file.
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        match parse::parse_comment(&text) {
            Ok((front, body)) => out.push(Comment { name, front, body }),
            Err(_) => out.push(Comment {
                name,
                front: CommentFront {
                    author: "unknown".to_string(),
                    at: String::new(),
                    kind: Some("unparseable".to_string()),
                },
                body: text,
            }),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn read_artifacts(dir: &Path) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let artifacts = dir.join("artifacts");
    if !is_real_dir(&artifacts) {
        return out;
    }
    let Ok(entries) = std::fs::read_dir(&artifacts) else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(is_file) = entry.file_type().map(|t| t.is_file()) else {
            continue;
        };
        if !is_file {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            out.push((entry.file_name().to_string_lossy().to_string(), meta.len()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

pub fn load_issue(pm_dir: &Path, project_key: &str, id: &str) -> Result<Issue> {
    model::check_id(id)?;
    let dir = pm_dir.join(project_key).join(id);
    let file = dir.join("issue.md");
    if !is_real_dir(&dir) || !is_real_file(&file) {
        return Err(Error::rejected(format!(
            "Unknown issue '{id}' — no {}/issue.md under project '{project_key}'",
            dir.display()
        )));
    }
    let text = std::fs::read_to_string(&file).map_err(|_| {
        Error::rejected(format!(
            "Unknown issue '{id}' — no {}/issue.md under project '{project_key}'",
            dir.display()
        ))
    })?;
    let (front, body) =
        parse::parse_issue(&text).map_err(|e| Error::rejected(format!("{id}: {e}")))?;
    Ok(Issue {
        project: project_key.to_string(),
        front,
        body,
        comments: read_comments(&dir),
        artifacts: read_artifacts(&dir),
        dir,
    })
}

/// Find an issue by id across every project (links cross projects —
/// `OPS-3 relates CAD-22`).
pub fn find_issue(pm_dir: &Path, id: &str) -> Result<Issue> {
    model::check_id(id)?;
    for project in project::list(pm_dir)? {
        let dir = pm_dir.join(&project.key).join(id);
        if is_real_dir(&dir) && is_real_file(&dir.join("issue.md")) {
            return load_issue(pm_dir, &project.key, id);
        }
    }
    Err(Error::rejected(format!(
        "Unknown issue '{id}' — `cadence issue ls` lists what exists"
    )))
}

/// All issues under the PM dir; `project_key` limits to one project.
/// Unparseable issue.md files surface as a lint error — here they are
/// skipped so one bad file cannot take down the whole board.
pub fn load_all(pm_dir: &Path, project_key: Option<&str>) -> Result<Vec<Issue>> {
    let mut issues = Vec::new();
    for project in project::list(pm_dir)? {
        if let Some(only) = project_key {
            if project.key != only {
                continue;
            }
        }
        let dir = pm_dir.join(&project.key);
        if !is_real_dir(&dir) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let id = entry.file_name().to_string_lossy().to_string();
            // file_type() is lstat-style: a symlinked folder is skipped.
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if !model::valid_id(&id) || !is_dir {
                continue;
            }
            if let Ok(issue) = load_issue(pm_dir, &project.key, &id) {
                issues.push(issue);
            }
        }
    }
    issues.sort_by_key(|i| natural_key(&i.front.id));
    Ok(issues)
}

/// `CAD-16` → ("CAD", 16) so CAD-9 sorts before CAD-16.
fn natural_key(id: &str) -> (String, u64) {
    let (p, n) = id.rsplit_once('-').unwrap_or((id, "0"));
    (p.to_string(), n.parse().unwrap_or(0))
}

/// What an issue's bound job says about its status — the M3 state,
/// ahead of notes and the file field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobOutcome {
    /// The job decides the status outright (`doing`|`review`|`done`).
    Status(&'static str),
    /// The least-advanced live task is blocked: the status still comes
    /// from notes or the file, but the issue is flagged `job blocked`.
    Blocked,
}

/// Issue id → job outcome, built once per board render from `job_list`.
pub type JobOutcomes = std::collections::HashMap<String, JobOutcome>;

/// Lifecycle rank for "least-advanced" — a blocked lane is behind a
/// running one, a draft behind everything.
fn task_rank(state: &str) -> u8 {
    match state {
        "draft" => 0,
        "blocked" => 1,
        "dispatched" => 2,
        "running" => 3,
        "revising" => 4,
        "review" => 5,
        _ => 3,
    }
}

/// Map one job's task-state multiset (`job_list` `tasks` counts) to a
/// board outcome: the least-advanced non-terminal task decides —
/// dispatched/running/revising → doing, review → review, blocked →
/// flag, draft → fall through; all-terminal → done only when nothing
/// failed or was cancelled.
fn job_outcome(tasks: &Value) -> Option<JobOutcome> {
    let mut least: Option<(u8, &str)> = None;
    let (mut finished, mut dead) = (false, false);
    for (state, n) in tasks.as_object()? {
        if n.as_i64().unwrap_or(0) <= 0 {
            continue;
        }
        match state.as_str() {
            "done" | "verified" => finished = true,
            "failed" | "cancelled" => dead = true,
            // Draft tasks are unstarted templates — they must not drag a
            // job whose live tasks have real state back to "no outcome".
            "draft" => {}
            s => {
                let rank = task_rank(s);
                if least.is_none_or(|(r, _)| rank < r) {
                    least = Some((rank, s));
                }
            }
        }
    }
    match least {
        Some((_, "dispatched" | "running" | "revising")) => Some(JobOutcome::Status("doing")),
        Some((_, "review")) => Some(JobOutcome::Status("review")),
        Some((_, "blocked")) => Some(JobOutcome::Blocked),
        _ => {
            if least.is_none() && finished && !dead {
                Some(JobOutcome::Status("done"))
            } else {
                None
            }
        }
    }
}

/// Issue-bound job selection: the newest job not `done`/`failed`/
/// `cancelled`, else the newest job at all — then its task map decides.
pub fn outcomes_from_jobs(jobs: &[Value]) -> JobOutcomes {
    let mut by_issue: HashMap<String, Vec<&Value>> = HashMap::new();
    for job in jobs {
        if let Some(issue) = job["issue"].as_str().filter(|s| !s.is_empty()) {
            by_issue.entry(issue.to_string()).or_default().push(job);
        }
    }
    let mut out = JobOutcomes::new();
    for (issue, mut jobs) in by_issue {
        let newest = |a: &&Value, b: &&Value| {
            // `created` is the ordering; `updated`/`id` break ties.
            (
                b["created"].as_str().unwrap_or_default(),
                b["id"].as_str().unwrap_or_default(),
            )
                .cmp(&(
                    a["created"].as_str().unwrap_or_default(),
                    a["id"].as_str().unwrap_or_default(),
                ))
        };
        jobs.sort_by(newest);
        let chosen = jobs
            .iter()
            .find(|j| !matches!(j["state"].as_str(), Some("done" | "failed" | "cancelled")))
            .or_else(|| jobs.first());
        if let Some(outcome) = chosen.and_then(|j| job_outcome(&j["tasks"])) {
            out.insert(issue, outcome);
        }
    }
    out
}

/// Live job outcomes from the daemon — an unreachable daemon yields an
/// empty map, which degrades to pre-job derivation exactly.
pub fn fetch_job_outcomes(state_dir: &Path) -> JobOutcomes {
    match crate::client::rpc(state_dir, "job_list", json!({"all": true})) {
        Ok(list) => outcomes_from_jobs(
            list["jobs"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .as_slice(),
        ),
        Err(_) => JobOutcomes::new(),
    }
}

/// Derivation pipeline, in priority order: container roll-up, then the
/// bound job's task state, then the newest tagged agent-note, then the
/// file field. `job_blocked` is the side-channel flag for a blocked
/// task — the status itself still derives from notes or the file.
pub fn derive_status(
    children_statuses: &[&str],
    notes_dir: &Path,
    id: &str,
    file_status: &str,
    job: Option<JobOutcome>,
) -> (String, &'static str) {
    if !children_statuses.is_empty() {
        // Roll-up: any child doing|review → doing; all done|dropped →
        // done; else the file value.
        if children_statuses
            .iter()
            .any(|s| matches!(*s, "doing" | "review"))
        {
            return ("doing".to_string(), "rollup");
        }
        if children_statuses
            .iter()
            .all(|s| matches!(*s, "done" | "dropped"))
        {
            return ("done".to_string(), "rollup");
        }
        return (file_status.to_string(), "rollup");
    }
    if let Some(JobOutcome::Status(status)) = job {
        return (status.to_string(), "job");
    }
    if let Some((status, _)) = notes::derive(notes_dir, id) {
        return (status.to_string(), "notes");
    }
    (file_status.to_string(), "file")
}

/// Compute views for a set of issues with no job data — identical to
/// pre-job derivation (tests and daemon-less callers).
pub fn views(notes_dir: &Path, issues: Vec<Issue>) -> Vec<View> {
    views_with_jobs(notes_dir, issues, &JobOutcomes::new())
}

/// Compute views for a set of issues (usually everything under the PM
/// dir so cross-project links resolve) against the daemon's job state.
pub fn views_with_jobs(notes_dir: &Path, issues: Vec<Issue>, jobs: &JobOutcomes) -> Vec<View> {
    // Children edges decide containers before statuses derive.
    let children_of: HashMap<String, Vec<String>> = {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for issue in &issues {
            if let Some(parent) = &issue.front.parent {
                map.entry(parent.clone())
                    .or_default()
                    .push(issue.front.id.clone());
            }
        }
        for kids in map.values_mut() {
            kids.sort_by_key(|k| natural_key(k));
        }
        map
    };
    // Inverse links are computed, never stored: `blocks` inverts
    // `blocked_by`, `duplicates` inverts `duplicate_of`.
    let mut blocks_of: HashMap<String, Vec<String>> = HashMap::new();
    let mut duplicates_of: HashMap<String, Vec<String>> = HashMap::new();
    for issue in &issues {
        for dep in &issue.front.blocked_by {
            blocks_of
                .entry(dep.clone())
                .or_default()
                .push(issue.front.id.clone());
        }
        if let Some(dup) = &issue.front.duplicate_of {
            duplicates_of
                .entry(dup.clone())
                .or_default()
                .push(issue.front.id.clone());
        }
    }
    for ids in blocks_of.values_mut().chain(duplicates_of.values_mut()) {
        ids.sort_by_key(|i| natural_key(i));
    }
    // Roll-up needs child statuses first; child depth is ≤ 2 so a
    // child never has children — compute leaves first, containers second.
    let mut status_of: HashMap<String, (String, &'static str)> = HashMap::new();
    for issue in &issues {
        if children_of.contains_key(&issue.front.id) {
            continue;
        }
        let (s, src) = derive_status(
            &[],
            notes_dir,
            &issue.front.id,
            &issue.front.status,
            jobs.get(&issue.front.id).copied(),
        );
        status_of.insert(issue.front.id.clone(), (s, src));
    }
    for issue in &issues {
        let Some(kids) = children_of.get(&issue.front.id) else {
            continue;
        };
        let kid_statuses: Vec<&str> = kids
            .iter()
            .map(|k| {
                status_of
                    .get(k)
                    .map(|(s, _)| s.as_str())
                    // A child that failed to load or is outside the set
                    // still counts against "all done".
                    .unwrap_or("doing")
            })
            .collect();
        let (s, src) = derive_status(
            &kid_statuses,
            notes_dir,
            &issue.front.id,
            &issue.front.status,
            jobs.get(&issue.front.id).copied(),
        );
        status_of.insert(issue.front.id.clone(), (s, src));
    }
    let done = |id: &str| {
        status_of
            .get(id)
            .map(|(s, _)| matches!(s.as_str(), "done" | "dropped"))
            .unwrap_or(false)
    };
    issues
        .into_iter()
        .map(|issue| {
            let id = issue.front.id.clone();
            let children = children_of.get(&id).cloned().unwrap_or_default();
            let (status, status_source) = status_of
                .get(&id)
                .cloned()
                .unwrap_or_else(|| (issue.front.status.clone(), "file"));
            let blocked_reason =
                (jobs.get(&id) == Some(&JobOutcome::Blocked)).then_some("job blocked");
            // `blocked` covers deps and the job-blocked flag alike — a
            // job-blocked issue is neither ready nor cleanly movable.
            let blocked =
                issue.front.blocked_by.iter().any(|b| !done(b)) || blocked_reason.is_some();
            let ready = matches!(status.as_str(), "backlog" | "ready") && !blocked;
            let blocks = blocks_of.get(&id).cloned().unwrap_or_default();
            let duplicates = duplicates_of.get(&id).cloned().unwrap_or_default();
            let (checks_done, checks_total) = parse::checkbox_progress(&issue.body);
            View {
                chain: notes::chain(notes_dir, &id),
                container: !children.is_empty(),
                children,
                blocks,
                duplicates,
                status,
                status_source,
                ready,
                blocked,
                blocked_reason,
                checks_done,
                checks_total,
                issue,
            }
        })
        .collect()
}

/// Content hash of the issue's `issue.md` — the `if_rev` token write
/// calls may send for optimistic concurrency.
pub fn rev_of(issue: &Issue) -> Value {
    json!(crate::issue::write::issue_rev(&issue.dir).unwrap_or_default())
}

/// Compact card payload for `GET /api/issues` and `issue ls --json`.
pub fn card_json(view: &View) -> Value {
    let f = &view.issue.front;
    json!({
        "id": f.id,
        "project": view.issue.project,
        "title": f.title,
        "status": view.status,
        "status_source": view.status_source,
        "priority": f.priority,
        "owner": f.owner,
        "component": f.component,
        "parent": f.parent,
        "blocked_by": f.blocked_by,
        "relates": f.relates,
        "duplicate_of": f.duplicate_of,
        "refs": f.refs,
        "container": view.container,
        "ready": view.ready,
        "blocked": view.blocked,
        "blocked_reason": view.blocked_reason,
        "created": f.created,
        "rev": rev_of(&view.issue),
        "counts": {
            "comments": view.issue.comments.len(),
            "artifacts": view.issue.artifacts.len(),
            "refs": f.refs.len(),
        },
        "checks": {"done": view.checks_done, "total": view.checks_total},
    })
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
        "component": f.component,
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
        "notes_chain": chain,
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
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(pm_dir)
        // No --follow: identical issue.md templates across siblings trip
        // rename detection and leak other issues' creation commits.
        .args(["log", "--format=%h|%aI|%s", "--"])
        .arg(&rel)
        .output();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::model::Ref;

    fn issue(id: &str, status: &str) -> Issue {
        let mut front = Front::new(id, "t", "2026-09-17T00:00:00Z");
        front.status = status.to_string();
        Issue {
            project: "cadence".to_string(),
            dir: PathBuf::from("/nonexistent"),
            front,
            body: String::new(),
            comments: vec![],
            artifacts: vec![],
        }
    }

    fn view_of<'a>(views: &'a [View], id: &str) -> &'a View {
        views.iter().find(|v| v.issue.front.id == id).unwrap()
    }

    #[test]
    fn rollup_status() {
        let mut parent = issue("CAD-1", "backlog");
        let mut child = issue("CAD-2", "doing");
        child.front.parent = Some("CAD-1".to_string());
        parent.dir = PathBuf::from("/x");
        let vs = views(Path::new("/no-notes"), vec![parent, child]);
        let p = view_of(&vs, "CAD-1");
        assert!(p.container);
        assert_eq!(p.status, "doing");
        assert_eq!(p.status_source, "rollup");
        assert_eq!(p.children, vec!["CAD-2"]);
    }

    #[test]
    fn rollup_all_done() {
        let parent = issue("CAD-1", "doing");
        let mut c1 = issue("CAD-2", "done");
        let mut c2 = issue("CAD-3", "dropped");
        c1.front.parent = Some("CAD-1".to_string());
        c2.front.parent = Some("CAD-1".to_string());
        let vs = views(Path::new("/no-notes"), vec![parent, c1, c2]);
        assert_eq!(view_of(&vs, "CAD-1").status, "done");
    }

    #[test]
    fn rollup_partial_children_keep_file_status() {
        // One done child + one backlog child: not all-done, nothing
        // doing — the parent keeps its file status.
        let parent = issue("CAD-1", "review");
        let mut c1 = issue("CAD-2", "done");
        let mut c2 = issue("CAD-3", "backlog");
        c1.front.parent = Some("CAD-1".to_string());
        c2.front.parent = Some("CAD-1".to_string());
        let vs = views(Path::new("/no-notes"), vec![parent, c1, c2]);
        let p = view_of(&vs, "CAD-1");
        assert_eq!(p.status, "review");
        assert_eq!(p.status_source, "rollup");
    }

    #[test]
    fn blocked_and_ready() {
        let mut a = issue("CAD-1", "ready");
        a.front.blocked_by = vec!["CAD-2".to_string()];
        let b = issue("CAD-2", "doing");
        let vs = views(Path::new("/no-notes"), vec![a, b]);
        assert!(view_of(&vs, "CAD-1").blocked);
        assert!(!view_of(&vs, "CAD-1").ready);
        assert_eq!(view_of(&vs, "CAD-2").blocks, vec!["CAD-1"]);

        // Once the blocker is done the wait lifts.
        let mut a = issue("CAD-1", "ready");
        a.front.blocked_by = vec!["CAD-2".to_string()];
        let b = issue("CAD-2", "done");
        let vs = views(Path::new("/no-notes"), vec![a, b]);
        assert!(!view_of(&vs, "CAD-1").blocked);
        assert!(view_of(&vs, "CAD-1").ready);
    }

    #[test]
    fn duplicate_inverse() {
        let mut a = issue("CAD-1", "dropped");
        a.front.duplicate_of = Some("CAD-2".to_string());
        let b = issue("CAD-2", "backlog");
        let vs = views(Path::new("/no-notes"), vec![a, b]);
        assert_eq!(view_of(&vs, "CAD-2").duplicates, vec!["CAD-1"]);
    }

    #[test]
    fn notes_derived_status_wins() {
        let notes = tempfile::TempDir::new().unwrap();
        std::fs::write(
            notes.path().join("20260917-120000-abc-x-kickoff.md"),
            "# Kickoff\n> Issue: `CAD-1`\n\nbody\n",
        )
        .unwrap();
        let i = issue("CAD-1", "backlog");
        let vs = views(notes.path(), vec![i]);
        let v = view_of(&vs, "CAD-1");
        assert_eq!(v.status, "doing");
        assert_eq!(v.status_source, "notes");
        assert_eq!(v.chain.len(), 1);
    }

    #[test]
    fn verdict_pass_marks_done() {
        let notes = tempfile::TempDir::new().unwrap();
        std::fs::write(
            notes.path().join("20260917-120000-abc-x-kickoff.md"),
            "# Kickoff\n> Issue: `CAD-1`\n",
        )
        .unwrap();
        std::fs::write(
            notes.path().join("20260917-150000-abc-x-verdict.md"),
            "# Verdict: x\n> Issue: `CAD-1`\n\n## Verdict\n**Pass.**\n",
        )
        .unwrap();
        let i = issue("CAD-1", "doing");
        let vs = views(notes.path(), vec![i]);
        assert_eq!(view_of(&vs, "CAD-1").status, "done");
    }

    #[test]
    fn card_json_shape() {
        let mut i = issue("CAD-1", "ready");
        i.front.refs = vec![Ref {
            kind: "pr".to_string(),
            url: Some("https://x/1".to_string()),
            path: None,
            label: Some("PR #1".to_string()),
            closed: None,
        }];
        let vs = views(Path::new("/no-notes"), vec![i]);
        let card = card_json(view_of(&vs, "CAD-1"));
        assert_eq!(card["status"], "ready");
        assert_eq!(card["status_source"], "file");
        assert_eq!(card["counts"]["refs"], 1);
        assert_eq!(card["refs"][0]["label"], "PR #1");
    }

    #[test]
    fn job_task_states_map_to_board_status() {
        let counts = |states: &[&str]| {
            json!(states
                .iter()
                .map(|s| (s.to_string(), json!(1)))
                .collect::<serde_json::Map<String, Value>>())
        };
        // Single-task mappings.
        for s in ["dispatched", "running", "revising"] {
            assert_eq!(
                job_outcome(&counts(&[s])),
                Some(JobOutcome::Status("doing")),
                "{s}"
            );
        }
        assert_eq!(
            job_outcome(&counts(&["review"])),
            Some(JobOutcome::Status("review"))
        );
        for s in ["verified", "done"] {
            assert_eq!(
                job_outcome(&counts(&[s])),
                Some(JobOutcome::Status("done")),
                "{s}"
            );
        }
        assert_eq!(
            job_outcome(&counts(&["blocked"])),
            Some(JobOutcome::Blocked)
        );
        for s in ["draft", "failed", "cancelled"] {
            assert_eq!(job_outcome(&counts(&[s])), None, "{s}");
        }
        // Least-advanced non-terminal wins over any terminal task.
        assert_eq!(
            job_outcome(&counts(&["running", "done"])),
            Some(JobOutcome::Status("doing"))
        );
        assert_eq!(
            job_outcome(&counts(&["review", "running"])),
            Some(JobOutcome::Status("doing"))
        );
        // `job new` auto-creates a stub draft task — it is a template,
        // not work-in-progress, so it never suppresses real task state.
        assert_eq!(
            job_outcome(&counts(&["draft", "review"])),
            Some(JobOutcome::Status("review")),
            "the auto-created draft lane is transparent"
        );
        assert_eq!(
            job_outcome(&counts(&["draft", "draft"])),
            None,
            "a job with only drafts falls through to notes"
        );
        assert_eq!(
            job_outcome(&counts(&["blocked", "review"])),
            Some(JobOutcome::Blocked)
        );
        // Mixed terminal: done+cancelled is not "done".
        assert_eq!(job_outcome(&counts(&["done", "cancelled"])), None);
        assert_eq!(
            job_outcome(&counts(&["verified", "done"])),
            Some(JobOutcome::Status("done"))
        );
    }

    #[test]
    fn outcomes_pick_newest_live_job() {
        let job = |id: &str, issue: &str, state: &str, created: &str, tasks: &[&str]| {
            json!({
                "id": id, "issue": issue, "state": state, "created": created,
                "tasks": tasks
                    .iter()
                    .map(|s| (s.to_string(), json!(1)))
                    .collect::<serde_json::Map<String, Value>>(),
            })
        };
        let jobs = vec![
            // Older terminal job must not beat the live one.
            job("j1", "CAD-1", "done", "2026-01-01", &["done"]),
            job("j2", "CAD-1", "open", "2026-01-02", &["running"]),
            // No live job — the newest terminal one still reports done.
            job("j3", "CAD-2", "done", "2026-01-03", &["done"]),
            job("j4", "CAD-2", "cancelled", "2026-01-01", &["cancelled"]),
            // Job without an issue is ignored entirely.
            job("j5", "", "open", "2026-01-04", &["running"]),
        ];
        let out = outcomes_from_jobs(&jobs);
        assert_eq!(out["CAD-1"], JobOutcome::Status("doing"));
        assert_eq!(out["CAD-2"], JobOutcome::Status("done"));
        assert!(!out.contains_key(""));
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn job_status_beats_notes_and_flags_blocked() {
        let i = issue("CAD-1", "backlog");
        let mut jobs = JobOutcomes::new();
        jobs.insert("CAD-1".to_string(), JobOutcome::Status("doing"));
        let vs = views_with_jobs(Path::new("/no-notes"), vec![i], &jobs);
        let v = view_of(&vs, "CAD-1");
        assert_eq!(v.status, "doing");
        assert_eq!(v.status_source, "job");
        // The same job blocked keeps the file status but flags the row.
        let mut jobs = JobOutcomes::new();
        jobs.insert("CAD-1".to_string(), JobOutcome::Blocked);
        let vs = views_with_jobs(Path::new("/no-notes"), vec![issue("CAD-1", "ready")], &jobs);
        let v = view_of(&vs, "CAD-1");
        assert_eq!(v.status, "ready");
        assert_eq!(v.status_source, "file");
        assert_eq!(v.blocked_reason, Some("job blocked"));
        assert_eq!(card_json(v)["blocked_reason"], "job blocked");
    }
}
