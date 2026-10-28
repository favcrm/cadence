//! The one host issue for an accepted app-run disposition.
//!
//! The daemon reserves the suggestion and stores its text. This module is
//! the PM writer: under the tracker lock it searches for the request
//! marker and, when that issue is absent, creates one. A crash between
//! those steps retries onto the same issue only when its body is this
//! reservation. A marker that belongs to another project or another
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
            found.push((project_name.clone(), id.to_string(), body));
        }
    }
    if found.len() > 1 {
        return Err(Error::rejected(
            "ticket request marker matches more than one issue",
        ));
    }
    let Some((found_project, id, body)) = found.pop() else {
        return Ok(None);
    };
    if found_project != project || body.as_deref() != Some(expected_body) {
        return Err(Error::rejected(
            "ticket request marker does not match this reservation",
        ));
    }
    Ok(Some(id))
}

/// Create the one issue for a daemon reservation, or return the issue a
/// previous attempt already committed under the same request marker.
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
