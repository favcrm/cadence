//! The one host issue for an accepted app-run disposition.
//!
//! The daemon reserves the suggestion and stores its text. This module is
//! the PM writer: under the tracker lock it searches for the request
//! marker and, when that issue is absent, creates one. A crash between
//! those steps retries onto the same issue only when that issue is this
//! reservation and its exact bytes are already committed. A matching body
//! that is still uncommitted, staged, or otherwise not clean against HEAD
//! is refused: the interrupted write is left as it is, and no second issue
//! is allocated. A marker that belongs to another project or another
//! proposal is refused. Two markers for one request refuse.

use super::*;
use crate::issue::model;

pub const TICKET_REQUEST_PREFIX: &str = "cadence-app-ticket-request: ";

pub fn app_ticket_marker(request_id: &str) -> String {
    format!("{TICKET_REQUEST_PREFIX}{request_id}")
}

fn reserved_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| Error::rejected(format!("ticket reservation is missing {key}")))
}

fn reserved_count(value: &Value, key: &str) -> Result<u64> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::rejected(format!("ticket reservation is missing {key}")))
}

pub fn app_ticket_title(suggestion: &str) -> String {
    let flat = suggestion.split_whitespace().collect::<Vec<_>>().join(" ");
    let title: String = flat.chars().take(100).collect();
    if title.is_empty() {
        "Selected reviewed proposal".to_string()
    } else {
        title
    }
}

pub fn app_ticket_body(reserved: &Value) -> Result<String> {
    let request_id = reserved_str(reserved, "request_id")?;
    crate::proto::identifier(request_id, "app run ticket request ID")?;
    let run_id = reserved_str(reserved, "run_id")?;
    let artifact_id = reserved_str(reserved, "artifact_id")?;
    let digest = reserved_str(reserved, "artifact_digest")?;
    let suggestion = reserved_str(reserved, "suggestion")?;
    let index = reserved_count(reserved, "index")?;
    let count = reserved_count(reserved, "count")?;
    if index == 0 || count < 2 || index > count {
        return Err(Error::rejected(
            "ticket reservation does not name one of at least two proposals",
        ));
    }
    let marker = app_ticket_marker(request_id);
    if suggestion.lines().any(|line| line == marker) {
        return Err(Error::rejected(
            "selected proposal text collides with the ticket request marker",
        ));
    }
    Ok(format!(
        "Selected proposal from a reviewed app run. This ticket records the operator's choice and does not publish the artifact.\n\
         \n\
         {marker}\n\
         \n\
         ## Provenance\n\
         \n\
         - decision: accept\n\
         - run: {run_id}\n\
         - artifact: {artifact_id}\n\
         - digest: {digest}\n\
         - suggestion: {index} of {count}\n\
         \n\
         ## Selected proposal\n\
         \n\
         {suggestion}\n\
         \n\
         ## Acceptance\n\
         \n\
         - [ ] This issue is the single selected proposal from that reviewed run.\n"
    ))
}

/// The one issue whose body is exactly this reservation, if the marker
/// occurs once. A marker in another project, or on an issue whose
/// provenance is not this reservation, is a conflict — it is not bound.
///
/// A body match is not enough. The next-id lock is disjoint from a
/// crashed writer's directory, so this search is the fence: the exact
/// issue bytes must already be committed and clean against HEAD. An
/// uncommitted or staged match is refused without allocating another
/// issue, committing the leftover, or clearing the crash marker.
fn reserved_ticket(
    pm: &Pm,
    project: &str,
    request_id: &str,
    expected_body: &str,
) -> Result<Option<String>> {
    crate::proto::identifier(request_id, "app run ticket request ID")?;
    let marker = app_ticket_marker(request_id);
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&pm.dir)
        .map_err(|e| Error::rejected(format!("cannot read the tracker: {e}")))?
    {
        let entry = entry.map_err(|e| Error::rejected(format!("cannot read the tracker: {e}")))?;
        let Some(project_name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if project_name.starts_with('.') || !entry.path().is_dir() {
            continue;
        }
        for issue in std::fs::read_dir(entry.path())
            .map_err(|e| Error::rejected(format!("cannot read the tracker: {e}")))?
        {
            let issue =
                issue.map_err(|e| Error::rejected(format!("cannot read the tracker: {e}")))?;
            if !issue.path().is_dir() {
                continue;
            }
            let file = issue.path().join("issue.md");
            if !file.is_file() {
                continue;
            }
            let text = std::fs::read_to_string(&file)
                .map_err(|e| Error::rejected(format!("cannot read {}: {e}", file.display())))?;
            if !text.lines().any(|line| line == marker) {
                continue;
            }
            let id = issue.file_name();
            let Some(id) = id.to_str() else {
                return Err(Error::rejected(
                    "ticket request marker is in a non-UTF-8 issue folder",
                ));
            };
            model::check_id(id)?;
            let body = super::parse::parse_issue(&text).ok().map(|(_, body)| body);
            found.push((project_name.clone(), id.to_string(), body, text));
        }
    }
    if found.len() > 1 {
        return Err(Error::rejected(
            "ticket request marker matches more than one issue",
        ));
    }
    let Some((found_project, id, body, text)) = found.pop() else {
        return Ok(None);
    };
    if found_project != project || body.as_deref() != Some(expected_body) {
        return Err(Error::rejected(
            "ticket request marker does not match this reservation",
        ));
    }
    ensure_committed_reservation(pm, &found_project, &id, &text)?;
    Ok(Some(id))
}

/// The supplied issue is this reservation's selected ticket, already
/// committed and clean against HEAD. The caller holds the tracker lock.
/// This reads only: it does not commit, allocate, or clear a crash marker.
pub(crate) fn confirm_committed_selected_issue(
    pm: &Pm,
    issue_id: &str,
    reserved: &Value,
) -> Result<()> {
    let expected = app_ticket_body(reserved)?;
    let (project, dir) = match super::issue_dir(pm, issue_id) {
        Ok(found) => found,
        Err(error) => {
            if error.to_string().contains("Unknown issue") {
                return Err(not_selected(issue_id));
            }
            return Err(error);
        }
    };
    let path = dir.join("issue.md");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| Error::rejected(format!("cannot read {}: {e}", path.display())))?;
    let body = super::parse::parse_issue(&text).ok().map(|(_, body)| body);
    if body.as_deref() != Some(expected.as_str()) {
        return Err(not_selected(issue_id));
    }
    ensure_committed_reservation(pm, &project.key, issue_id, &text)
}

fn not_selected(issue_id: &str) -> Error {
    Error::rejected(format!(
        "ticket bind refused: {issue_id} is not the committed selected issue for this reservation"
    ))
}

/// Refuse a reservation whose working-tree bytes are not the committed
/// blob, or whose issue directory is still dirty. Nothing is written.
fn ensure_committed_reservation(pm: &Pm, project: &str, id: &str, working: &str) -> Result<()> {
    let rel = format!("{project}/{id}/issue.md");
    let dir_rel = format!("{project}/{id}");
    let refuse = || {
        Error::rejected(format!(
            "ticket request marker matches {id} but that issue is not a committed reservation; \
             refusing to bind it. The interrupted write is left unchanged"
        ))
    };
    match committed_blob(&pm.dir, &rel)? {
        Some(head) if head == working.as_bytes() => {}
        _ => return Err(refuse()),
    }
    let status = crate::issue::git(
        &pm.dir,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--",
            &dir_rel,
        ],
    )?;
    if !status.is_empty() {
        return Err(refuse());
    }
    Ok(())
}

/// `HEAD:rel` as raw blob bytes. `Ok(None)` when that path is not in HEAD.
/// Any other git failure stays an error so a broken repository is not
/// reported as a clean miss.
fn committed_blob(dir: &std::path::Path, rel: &str) -> Result<Option<Vec<u8>>> {
    let spec = format!("HEAD:{rel}");
    let out = crate::reaper::output(
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["cat-file", "blob", &spec]),
    )
    .map_err(|_| Error::rejected("`git` is required and was not found on PATH"))?;
    if out.status.success() {
        return Ok(Some(out.stdout));
    }
    let err = String::from_utf8_lossy(&out.stderr);
    // Untracked: "exists on disk, but not in 'HEAD'". Absent: "does not
    // exist in 'HEAD'". Neither is a committed blob.
    if err.contains("not in")
        || err.contains("does not exist")
        || err.contains("Not a valid object name")
    {
        return Ok(None);
    }
    Err(Error::rejected(format!(
        "cannot read the committed ticket {rel}: {}",
        err.trim()
    )))
}

/// Create the one issue for a daemon reservation, or return the issue a
/// previous attempt already committed under the same request marker.
/// An uncommitted match is an error, not an id.
///
/// The marker is rechecked after the tracker lock is held and before the
/// new folder is written, so two callers cannot each observe an absence
/// and commit a second issue. A marker that is not this reservation is
/// refused instead of bound.
pub fn materialize_app_run_ticket(
    pm: &Pm,
    cwd: &Path,
    project: &str,
    reserved: &Value,
) -> Result<Value> {
    model::check_key(project)?;
    let request_id = reserved_str(reserved, "request_id")?;
    let suggestion = reserved_str(reserved, "suggestion")?;
    let body = app_ticket_body(reserved)?;
    let mut created = super::new::new_issue_unless(
        pm,
        cwd,
        Some(project),
        &app_ticket_title(suggestion),
        Some("P2"),
        None,
        &[],
        None,
        None,
        &[],
        None,
        Some(&body),
        "operator",
        || {
            reserved_ticket(pm, project, request_id, &body)
                .map(|id| id.map(|id| json!({"id": id, "created": false})))
        },
    )?;
    if created.get("created").is_none() {
        created["created"] = json!(true);
    }
    Ok(created)
}
