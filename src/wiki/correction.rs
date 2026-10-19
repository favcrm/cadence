//! Collection corrections stored as ordinary feedback issues.
//!
//! Intake does not grant `issue_created` and does not read a conversation.
//! Question and answer text are the caller's report. The reviewer is the
//! verified human subject on the owner session. A second session for the
//! same subject is still that subject: session ids and the `operator`
//! alias are not an identity and cannot pass self-review.

use std::fs;
use std::path::Path;

use serde_json::{json, Value};

use super::collection::{self, Loaded};
use super::revision;
use super::Caller;
use crate::error::{Error, Result};
use crate::issue::model::{self, CommentFront, Front};
use crate::issue::write;
use crate::issue::{board, parse, project, time, Pm, PmLock};

const TEXT_CAP: usize = 8 * 1024;
const CITATION_CAP: usize = 20;
const LIST_CAP: usize = 100;
/// The reserved record name, defined once by the tracker's artifact grammar
/// so a caller upload can never write it (see `model::reserved_artifact_name`).
const ARTIFACT: &str = model::RESERVED_ARTIFACT_NAMES[0];
/// This module's own markers. A record without all of them, or one that does
/// not name its own issue and project, is not a correction.
const RECORD_KIND: &str = "knowledge-correction";
const RECORD_SCHEMA: u64 = 1;
const RECORD_CLAIMS: &str = "reported-untrusted";

pub(crate) struct Human {
    pub sub: String,
    pub display: String,
}

pub(crate) fn list(pm: &Pm, caller: &Caller, path: &str) -> Result<Value> {
    let loaded = collection::root_and_project(pm, caller, path)?;
    let requests = match &loaded.review_project {
        Some(project) => load_requests(pm, caller, &loaded, project)?,
        None => Vec::new(),
    };
    Ok(json!({
        "path": loaded.root,
        "requests": requests,
        "permissions": permissions(caller, loaded.review_project.is_some()),
    }))
}

pub(crate) fn submit(
    pm: &Pm,
    caller: &Caller,
    path: &str,
    body: &Value,
    human: &Human,
) -> Result<Value> {
    require_subject(&human.sub)?;
    if !matches!(caller, Caller::User(_) | Caller::Operator) {
        return Err(Error::rejected(
            "wiki correction refused: only a verified board user can report a correction",
        ));
    }
    let loaded = collection::root_and_project(pm, caller, path)?;
    let project_key = loaded.review_project.clone().ok_or_else(|| {
        Error::rejected(
            "wiki correction refused: this collection has no owner-configured review project",
        )
    })?;
    let report = parse_report(pm, caller, &loaded.root, body)?;
    let projects = project::list(&pm.dir)?;
    let project = projects
        .into_iter()
        .find(|item| item.key == project_key)
        .ok_or_else(|| {
            Error::rejected(format!(
                "wiki correction refused: review project '{project_key}' does not exist"
            ))
        })?;
    let artifact = artifact_value(&loaded, &report, human, "pending", None, None, None);
    let id = create_issue(pm, &project, &report, &artifact, &caller.actor())?;
    correction_value(pm, caller, &loaded, &id)
}

pub(crate) fn review(
    pm: &Pm,
    caller: &Caller,
    path: &str,
    body: &Value,
    human: &Human,
) -> Result<Value> {
    require_subject(&human.sub)?;
    if !matches!(caller, Caller::Operator) {
        return Err(Error::rejected(
            "wiki correction refused: source-check review is the collection owner's",
        ));
    }
    let loaded = collection::root_and_project(pm, caller, path)?;
    let project_key = loaded.review_project.clone().ok_or_else(|| {
        Error::rejected("wiki correction refused: this collection has no review project")
    })?;
    let id = body
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::rejected("wiki correction refused: id is required"))?;
    let id = model::check_id(id)?;
    let decision = body
        .get("decision")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::rejected("wiki correction refused: decision is required"))?;
    if decision != "accept" && decision != "reject" {
        return Err(Error::rejected(
            "wiki correction refused: decision must be accept or reject",
        ));
    }
    let reason = required_text(body, "review reason", "reason")?;
    let source_checked = body
        .get("source_checked")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            Error::rejected("wiki correction refused: source_checked must be a boolean")
        })?;
    reject_review_fields(body)?;
    let (project, dir) = write::issue_dir(pm, &id)?;
    if project.key != project_key {
        return Err(Error::rejected(
            "wiki correction refused: that issue is not this collection's review project",
        ));
    }
    // One lock covers the reload, the validations and the persistence: two
    // reviewers can no longer both read `pending`, and a failed step below
    // restores the record instead of leaving the decision behind.
    let lock = pm.lock_for_paths(std::slice::from_ref(&dir))?;
    let artifact_path = dir.join("artifacts").join(ARTIFACT);
    let current = stored_record(&dir, &id, &project_key)?;
    if current["path"].as_str() != Some(loaded.root.as_str()) {
        return Err(Error::rejected(
            "wiki correction refused: that correction belongs to another collection",
        ));
    }
    if current["status"].as_str() != Some("pending") {
        return Err(Error::rejected(
            "wiki correction refused: this correction was already reviewed",
        ));
    }
    let submitter = current["submitted_by"]["id"].as_str().unwrap_or("");
    if require_subject(submitter).is_err() {
        return Err(Error::rejected(
            "wiki correction refused: the submitter is not a verified human subject, so this record cannot be reviewed",
        ));
    }
    if submitter == human.sub {
        return Err(Error::rejected(
            "wiki correction refused: the reviewer is the person who submitted it",
        ));
    }
    if decision == "accept" {
        if !source_checked {
            return Err(Error::rejected(
                "wiki correction refused: acceptance requires an explicit source check",
            ));
        }
        if let Some(conflict) = accept_conflict(pm, caller, &loaded, &current)? {
            return Ok(conflict);
        }
    }
    let status = if decision == "accept" {
        "accepted"
    } else {
        "rejected"
    };
    let mut next = current.clone();
    next["status"] = json!(status);
    next["reviewed_by"] = json!({"id": human.sub, "display": human.display});
    next["review_reason"] = json!(reason);
    next["source_checked"] = json!(decision == "accept" && source_checked);
    next["collection_sha256"] = json!(loaded.sidecar_sha256);
    let bytes = serde_json::to_vec_pretty(&next)
        .map_err(|e| Error::internal(format!("wiki correction: {e}")))?;
    bytes_push_store(
        pm,
        &lock,
        ReviewCommit {
            dir: &dir,
            artifact_path: &artifact_path,
            id: &id,
            reason: &reason,
            human,
            status,
        },
        &bytes,
    )?;
    correction_value(pm, caller, &loaded, &id)
}

fn accept_conflict(
    pm: &Pm,
    caller: &Caller,
    loaded: &Loaded,
    current: &Value,
) -> Result<Option<Value>> {
    let stale = |why: &str| {
        json!({
            "conflict": "stale",
            "reason": why,
            "path": loaded.root,
            "id": current["id"],
        })
    };
    if current["collection_sha256"].as_str() != loaded.sidecar_sha256.as_deref() {
        return Ok(Some(stale(
            "collection metadata changed after the correction was submitted",
        )));
    }
    let Some(citations) = current["citations"].as_array() else {
        return Ok(Some(stale("the correction has no cited sources")));
    };
    if citations.is_empty() {
        return Ok(Some(stale("the correction has no cited sources")));
    }
    for citation in citations {
        let path = citation["path"].as_str().unwrap_or("");
        let git = citation["revision"].as_str().unwrap_or("");
        let sha = citation["sha256"].as_str().unwrap_or("");
        if path.is_empty() || git.is_empty() || sha.is_empty() {
            return Ok(Some(stale("a citation is missing its revision or digest")));
        }
        if !collection::under_root(&loaded.root, path) {
            return Ok(Some(stale("a citation is outside the collection")));
        }
        let live = match revision::live_source(pm, caller, path) {
            Ok(live) => live,
            Err(_) => return Ok(Some(stale("a cited source is not readable"))),
        };
        if live.sha256 != sha {
            return Ok(Some(stale(
                "a cited source changed and cannot inherit the submitted revision",
            )));
        }
        match revision::pin(pm, caller, path, git) {
            Ok(pinned) if pinned.git == git && pinned.sha256 == sha => {}
            _ => {
                return Ok(Some(stale(
                    "the cited commit no longer contains that source digest",
                )))
            }
        }
    }
    Ok(None)
}

/// The issue artifact a review decision rewrites, with the attribution and
/// status the commit and comment carry.
struct ReviewCommit<'a> {
    dir: &'a Path,
    artifact_path: &'a Path,
    id: &'a str,
    reason: &'a str,
    human: &'a Human,
    status: &'a str,
}

fn bytes_push_store(pm: &Pm, lock: &PmLock, commit: ReviewCommit<'_>, bytes: &[u8]) -> Result<()> {
    let (dir, artifact_path) = (commit.dir, commit.artifact_path);
    let previous = write::file_preimage(artifact_path);
    fs::write(artifact_path, bytes)?;
    // Restore the record on any later failure: a refused review must not
    // leave the decision on disk, or the retry reads as already reviewed.
    let comment = match comment_file(dir, commit.human, commit.status, commit.reason) {
        Ok(comment) => comment,
        Err(error) => {
            write::restore_preimage(artifact_path, previous);
            return Err(error);
        }
    };
    let paths = [artifact_path.to_path_buf(), comment];
    if let Err(error) = write::commit_scoped(
        pm,
        lock,
        &paths,
        &format!("{}: correction {}", commit.id, commit.status),
        &[commit.id],
        &commit.human.sub,
    ) {
        write::restore_preimage(artifact_path, previous);
        let _ = fs::remove_file(&paths[1]);
        return Err(error);
    }
    Ok(())
}

fn comment_file(
    dir: &Path,
    human: &Human,
    status: &str,
    reason: &str,
) -> Result<std::path::PathBuf> {
    let comments = dir.join("comments");
    fs::create_dir_all(&comments)?;
    let author = write::comment_author(Some(&human.sub))?;
    let front = CommentFront {
        author: author.clone(),
        at: time::iso(time::now_epoch()),
        kind: Some("feedback".to_string()),
    };
    let body = format!(
        "Source-check decision: {status}.\n\nThe reported question and answer remain untrusted claims.\n\n{reason}\n"
    );
    let text = parse::render(&front, &body)?;
    let path = comments.join(format!("{}-{author}.md", time::basic(time::now_epoch())));
    if path.exists() {
        return Err(Error::rejected(
            "wiki correction refused: the review comment name already exists",
        ));
    }
    fs::write(&path, text)?;
    Ok(path)
}

fn create_issue(
    pm: &Pm,
    project: &project::Project,
    report: &Report,
    artifact: &Value,
    actor: &str,
) -> Result<String> {
    let _lock = pm.lock()?;
    let id = format!(
        "{}-{}",
        project.prefix,
        write::next_id(&pm.dir.join(&project.key), &project.prefix)?
    );
    let dir = pm.dir.join(&project.key).join(&id);
    if dir.exists() {
        return Err(Error::rejected(format!("Issue '{id}' already exists")));
    }
    let title = cap_line(&report.question);
    let mut front = Front::new(&id, &title, &time::iso(time::now_epoch()));
    front.tags = write::check_tags(project, &["intake".to_string(), "feedback".to_string()])?;
    front.kind = Some("feedback".to_string());
    let body = issue_body(report);
    let _warnings = crate::secret::guard(&format!("{id}: correction"), &body)?;
    fs::create_dir_all(dir.join("comments"))?;
    fs::create_dir_all(dir.join("artifacts"))?;
    write::save_front(&dir, &front, &body)?;
    let artifact_path = dir.join("artifacts").join(ARTIFACT);
    let mut stored = artifact.clone();
    stored["id"] = json!(id);
    stored["project"] = json!(project.key);
    let bytes = serde_json::to_vec_pretty(&stored)
        .map_err(|e| Error::internal(format!("wiki correction: {e}")))?;
    fs::write(&artifact_path, bytes)?;
    let issues = board::load_all(&pm.dir, None)?;
    if let Err(error) = write::check_structure(&issues, &id) {
        let _ = fs::remove_dir_all(&dir);
        return Err(error);
    }
    if let Err(error) = write::commit(
        pm,
        &[dir.join("issue.md"), artifact_path],
        &format!("{id}: report feedback"),
        &[&id],
        actor,
    ) {
        let _ = fs::remove_dir_all(&dir);
        return Err(error);
    }
    Ok(id)
}

struct Report {
    question: String,
    answer: String,
    expected_answer: String,
    reason: String,
    citations: Vec<Value>,
}

fn parse_report(pm: &Pm, caller: &Caller, root: &str, body: &Value) -> Result<Report> {
    reject_report_fields(body)?;
    let mut citations = Vec::new();
    let rows = body
        .get("citations")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::rejected("wiki correction refused: citations must be an array"))?;
    if rows.is_empty() || rows.len() > CITATION_CAP {
        return Err(Error::rejected(
            "wiki correction refused: cite between 1 and 20 sources",
        ));
    }
    for row in rows {
        citations.push(parse_citation(pm, caller, root, row)?);
    }
    Ok(Report {
        question: required_text(body, "question", "question")?,
        answer: required_text(body, "answer", "answer")?,
        expected_answer: required_text(body, "expected answer", "expected_answer")?,
        reason: required_text(body, "reason", "reason")?,
        citations,
    })
}

fn parse_citation(pm: &Pm, caller: &Caller, root: &str, row: &Value) -> Result<Value> {
    let obj = row
        .as_object()
        .ok_or_else(|| Error::rejected("wiki correction refused: a citation must be an object"))?;
    for key in obj.keys() {
        if !matches!(key.as_str(), "path" | "revision" | "section" | "page") {
            return Err(Error::rejected(format!(
                "wiki correction refused: citation field '{key}' is not accepted"
            )));
        }
    }
    let path = obj
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::rejected("wiki correction refused: a citation needs a path"))?;
    let path = super::normalize(path)?;
    if !collection::under_root(root, &path) || path == root {
        return Err(Error::rejected(format!(
            "wiki correction refused: '{path}' is outside this collection"
        )));
    }
    let segs: Vec<&str> = path.split('/').collect();
    super::allowed(caller, super::Op::Read, &segs)?;
    let selector = obj
        .get("revision")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::rejected("wiki correction refused: a citation needs a revision"))?;
    let pinned = revision::pin(pm, caller, &path, selector)?;
    if pinned.sha256.is_empty() {
        return Err(Error::rejected(
            "wiki correction refused: a citation must name a file revision",
        ));
    }
    let section = match obj.get("section") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) if !text.trim().is_empty() && text.len() <= 200 => {
            Some(text.trim().to_string())
        }
        Some(_) => {
            return Err(Error::rejected(
                "wiki correction refused: section must be a short string",
            ))
        }
    };
    let page = match obj.get("page") {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => {
            let page = n.as_u64().filter(|page| *page <= 100_000).ok_or_else(|| {
                Error::rejected("wiki correction refused: page must be a non-negative integer")
            })?;
            Some(page)
        }
        Some(_) => {
            return Err(Error::rejected(
                "wiki correction refused: page must be a non-negative integer",
            ))
        }
    };
    Ok(json!({
        "path": path,
        "revision": pinned.git,
        "sha256": pinned.sha256,
        "section": section,
        "page": page,
    }))
}

fn artifact_value(
    loaded: &Loaded,
    report: &Report,
    human: &Human,
    status: &str,
    reviewed_by: Option<&Human>,
    review_reason: Option<&str>,
    _source_checked: Option<bool>,
) -> Value {
    json!({
        "schema": 1,
        "kind": "knowledge-correction",
        "status": status,
        "path": loaded.root,
        "question": report.question,
        "answer": report.answer,
        "expected_answer": report.expected_answer,
        "reason": report.reason,
        "citations": report.citations,
        "submitted_by": {"id": human.sub, "display": human.display},
        "reviewed_by": reviewed_by.map(|human| json!({"id": human.sub, "display": human.display})),
        "review_reason": review_reason,
        "collection_sha256": loaded.sidecar_sha256,
        "claims": "reported-untrusted",
    })
}

fn issue_body(report: &Report) -> String {
    let mut citations = String::new();
    for citation in &report.citations {
        citations.push_str(&format!(
            "- {} @ {} sha256:{}\n",
            citation["path"].as_str().unwrap_or(""),
            citation["revision"].as_str().unwrap_or(""),
            citation["sha256"].as_str().unwrap_or("")
        ));
    }
    format!(
        "Reported correction. The question and answer below are the submitter's untrusted claims. They are not a verified model turn, not a private conversation fetch, and not source authority.\n\n## Reported question\n\n{}\n\n## Reported answer\n\n{}\n\n## Expected answer\n\n{}\n\n## Reason\n\n{}\n\n## Citations\n\n{citations}",
        report.question, report.answer, report.expected_answer, report.reason
    )
}

fn load_requests(
    pm: &Pm,
    caller: &Caller,
    loaded: &Loaded,
    project_key: &str,
) -> Result<Vec<Value>> {
    let dir = pm.dir.join(project_key);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if out.len() >= LIST_CAP {
            break;
        }
        let dir = entry.path();
        let Some(id) = dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Ok(value) = stored_record(&dir, id, project_key) else {
            continue;
        };
        if value["path"].as_str() != Some(loaded.root.as_str()) {
            continue;
        }
        if let Ok(row) = correction_from(pm, caller, loaded, &value, id) {
            out.push(row);
        }
    }
    out.sort_by(|a, b| b["id"].as_str().cmp(&a["id"].as_str()));
    Ok(out)
}

/// The correction record in `dir`, accepted only when it carries this
/// module's own markers and names its own issue and review project. A file
/// any other writer drops into `artifacts/` is not a correction.
fn stored_record(dir: &Path, id: &str, project_key: &str) -> Result<Value> {
    let bytes = fs::read(dir.join("artifacts").join(ARTIFACT)).map_err(|_| {
        Error::rejected("wiki correction refused: that issue has no correction record")
    })?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::rejected("wiki correction refused: correction record is not JSON"))?;
    if value["kind"].as_str() != Some(RECORD_KIND)
        || value["schema"].as_u64() != Some(RECORD_SCHEMA)
        || value["claims"].as_str() != Some(RECORD_CLAIMS)
        || value["id"].as_str() != Some(id)
        || value["project"].as_str() != Some(project_key)
    {
        return Err(Error::rejected(
            "wiki correction refused: that record was not created by this workflow",
        ));
    }
    Ok(value)
}

fn correction_value(pm: &Pm, caller: &Caller, loaded: &Loaded, id: &str) -> Result<Value> {
    let (_project, dir) = write::issue_dir(pm, id)?;
    let value = stored_record(&dir, id, loaded.review_project.as_deref().unwrap_or(""))?;
    correction_from(pm, caller, loaded, &value, id)
}

fn correction_from(
    pm: &Pm,
    caller: &Caller,
    loaded: &Loaded,
    value: &Value,
    id: &str,
) -> Result<Value> {
    let status = value["status"].as_str().unwrap_or("pending");
    let current = accept_conflict(pm, caller, loaded, value)?.is_none();
    Ok(json!({
        "id": id,
        "project": value["project"],
        "path": loaded.root,
        "status": status,
        "question": value["question"],
        "answer": value["answer"],
        "expected_answer": value["expected_answer"],
        "reason": value["reason"],
        "citations": value["citations"],
        "submitted_by": value["submitted_by"],
        "reviewed_by": value["reviewed_by"],
        "review_reason": value["review_reason"],
        "current": current,
    }))
}

fn permissions(caller: &Caller, configured: bool) -> Value {
    json!({
        "submit": configured && matches!(caller, Caller::User(_) | Caller::Operator),
        "review": matches!(caller, Caller::Operator),
    })
}

fn require_subject(sub: &str) -> Result<()> {
    let ok = !sub.is_empty()
        && sub.len() <= 64
        && !sub.eq_ignore_ascii_case("operator")
        && sub
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(Error::rejected(
            "wiki correction refused: identity must be the verified human subject, not an operator alias or a session id",
        ))
    }
}

fn required_text(body: &Value, label: &str, field: &str) -> Result<String> {
    let text = body
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::rejected(format!("wiki correction refused: {label} is required")))?;
    let text = text.trim();
    // Multiline reports are normal: newlines and tabs stay, every other
    // control character is still refused.
    let unsafe_control = text
        .chars()
        .any(|ch| ch.is_control() && ch != '\n' && ch != '\t');
    if text.is_empty() || text.len() > TEXT_CAP || unsafe_control {
        return Err(Error::rejected(format!(
            "wiki correction refused: {label} must be 1 to {TEXT_CAP} bytes of text without unsafe control characters"
        )));
    }
    Ok(text.to_string())
}

fn reject_report_fields(body: &Value) -> Result<()> {
    let obj = body
        .as_object()
        .ok_or_else(|| Error::rejected("wiki correction refused: the body must be an object"))?;
    for key in obj.keys() {
        if !matches!(
            key.as_str(),
            "path" | "question" | "answer" | "expected_answer" | "reason" | "citations"
        ) {
            return Err(Error::rejected(format!(
                "wiki correction refused: field '{key}' is not accepted"
            )));
        }
    }
    Ok(())
}

fn reject_review_fields(body: &Value) -> Result<()> {
    let obj = body
        .as_object()
        .ok_or_else(|| Error::rejected("wiki correction refused: the body must be an object"))?;
    for key in obj.keys() {
        if !matches!(
            key.as_str(),
            "path" | "id" | "decision" | "reason" | "source_checked"
        ) {
            return Err(Error::rejected(format!(
                "wiki correction refused: field '{key}' is not accepted"
            )));
        }
    }
    Ok(())
}

fn cap_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or(text).trim();
    let mut out = String::new();
    for ch in line.chars() {
        if out.len() + ch.len_utf8() > 200 {
            break;
        }
        out.push(ch);
    }
    if out.is_empty() {
        "Reported correction".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A report keeps its paragraphs and tabs; only unsafe controls are
    /// refused, and an empty or oversized field still is.
    #[test]
    fn report_text_keeps_newlines_and_refuses_unsafe_controls() {
        let paragraphs = json!({"answer": "first paragraph\n\nsecond paragraph"});
        assert_eq!(
            required_text(&paragraphs, "answer", "answer").unwrap(),
            "first paragraph\n\nsecond paragraph"
        );
        let tabbed = json!({"answer": "a\tb"});
        assert_eq!(required_text(&tabbed, "answer", "answer").unwrap(), "a\tb");
        for bad in ["a\u{7}b", "\u{0}", "a\rb"] {
            let body = json!({ "answer": bad });
            assert!(
                required_text(&body, "answer", "answer").is_err(),
                "{bad:?} was accepted"
            );
        }
        assert!(required_text(&json!({"answer": "   "}), "answer", "answer").is_err());
        assert!(required_text(&json!({}), "answer", "answer").is_err());
        assert!(required_text(
            &json!({ "answer": "x".repeat(TEXT_CAP + 1) }),
            "answer",
            "answer"
        )
        .is_err());
    }

    /// A record only counts when this workflow wrote it: a copy dropped into
    /// another ticket's `artifacts/` is not that ticket's correction.
    #[test]
    fn only_the_workflows_own_record_is_a_correction() {
        let dir = tempfile::tempdir().unwrap();
        let artifacts = dir.path().join("artifacts");
        fs::create_dir_all(&artifacts).unwrap();
        let write_record = |value: &Value| {
            fs::write(artifacts.join(ARTIFACT), serde_json::to_vec(value).unwrap()).unwrap();
        };
        let record = |id: &str, project: &str| {
            json!({
                "schema": RECORD_SCHEMA,
                "kind": RECORD_KIND,
                "claims": RECORD_CLAIMS,
                "id": id,
                "project": project,
            })
        };
        write_record(&record("WID-1", "widgets"));
        assert!(stored_record(dir.path(), "WID-1", "widgets").is_ok());
        for (label, value) in [
            ("another ticket's id", record("WID-9", "widgets")),
            ("another project's record", record("WID-1", "other")),
            ("a plain note", json!({"id": "WID-1", "project": "widgets"})),
        ] {
            write_record(&value);
            assert!(
                stored_record(dir.path(), "WID-1", "widgets").is_err(),
                "{label} was read as a correction"
            );
        }
    }

    /// A refused review leaves no decision behind: when the comment cannot be
    /// written, the record goes back to what it was.
    #[test]
    fn a_failed_review_step_restores_the_record() {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let dir = pm.dir.join("widgets").join("WID-1");
        fs::create_dir_all(dir.join("artifacts")).unwrap();
        let artifact_path = dir.join("artifacts").join(ARTIFACT);
        fs::write(&artifact_path, br#"{"status":"pending"}"#).unwrap();
        // `comments/` as a plain file: the comment write fails after the
        // record has already been replaced.
        fs::write(dir.join("comments"), b"not a directory").unwrap();
        let lock = pm.lock_for_paths(std::slice::from_ref(&dir)).unwrap();
        let human = Human {
            sub: "usr-reviewer".to_string(),
            display: "Reviewer".to_string(),
        };
        let reviewed = bytes_push_store(
            &pm,
            &lock,
            ReviewCommit {
                dir: &dir,
                artifact_path: &artifact_path,
                id: "WID-1",
                reason: "checked the cited bytes",
                human: &human,
                status: "accepted",
            },
            br#"{"status":"accepted"}"#,
        );
        assert!(reviewed.is_err(), "the review reported success");
        assert_eq!(
            fs::read(&artifact_path).unwrap(),
            br#"{"status":"pending"}"#,
            "the refused decision stayed on disk"
        );
    }
}
