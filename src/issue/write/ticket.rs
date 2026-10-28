//! The one host issue for an accepted app-run disposition.
//!
//! The daemon reserves the suggestion and stores its text. This module is
//! the PM writer: it searches for the request marker and, when that issue
//! is absent, creates one through [`new_issue`]. A crash between those two
//! steps retries onto the same issue. Two markers for one request refuse.

use super::new_issue;
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

/// The issue whose body carries this request marker, if there is exactly one.
pub fn find_app_ticket(pm: &Pm, request_id: &str) -> Result<Option<String>> {
    crate::proto::identifier(request_id, "app run ticket request ID")?;
    let marker = app_ticket_marker(request_id);
    let mut found = Vec::new();
    for project in std::fs::read_dir(&pm.dir)
        .map_err(|e| Error::rejected(format!("cannot read the tracker: {e}")))?
    {
        let project =
            project.map_err(|e| Error::rejected(format!("cannot read the tracker: {e}")))?;
        let name = project.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with('.') || !project.path().is_dir() {
            continue;
        }
        for issue in std::fs::read_dir(project.path())
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
            if text.lines().any(|line| line == marker) {
                let id = issue.file_name();
                let Some(id) = id.to_str() else {
                    return Err(Error::rejected(
                        "ticket request marker is in a non-UTF-8 issue folder",
                    ));
                };
                model::check_id(id)?;
                found.push(id.to_string());
            }
        }
    }
    match found.len() {
        0 => Ok(None),
        1 => Ok(Some(found.remove(0))),
        _ => Err(Error::rejected(
            "ticket request marker matches more than one issue",
        )),
    }
}

/// Create the one issue for a daemon reservation, or return the issue a
/// previous attempt already committed under the same request marker.
pub fn materialize_app_run_ticket(
    pm: &Pm,
    cwd: &Path,
    project: &str,
    reserved: &Value,
) -> Result<Value> {
    model::check_key(project)?;
    let request_id = reserved_str(reserved, "request_id")?;
    if let Some(id) = find_app_ticket(pm, request_id)? {
        return Ok(json!({"id": id, "created": false}));
    }
    let suggestion = reserved_str(reserved, "suggestion")?;
    let body = app_ticket_body(reserved)?;
    let created = new_issue(
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
    )?;
    let id = created
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::internal("new issue did not return an id"))?
        .to_string();
    // A second writer can pass the pre-check before this commit lands.
    // Bind the single marker that remains, and refuse when two survive.
    match find_app_ticket(pm, request_id)? {
        Some(found) => Ok(json!({"id": found, "created": found == id})),
        None => Err(Error::internal(
            "created ticket does not carry its request marker",
        )),
    }
}
