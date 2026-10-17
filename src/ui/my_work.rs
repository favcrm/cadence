//! The current caller's PM issues, using the existing board identity and read model.
//! This is a personal filter, not a project-visibility or execution grant.

use std::path::Path;

use serde_json::{json, Value};
use tiny_http::Request;

use crate::issue::Pm;

use super::{operator, read_model, HttpResp, ServeOpts};

pub(super) fn payload(state_dir: &Path, pm_dir: &Path, owner: &str) -> crate::Result<Value> {
    let pm = Pm::at(pm_dir)?;
    let read = read_model::get(state_dir, pm_dir).board(&pm, None);
    let by_id = read.by_id();
    let ctx = read.ctx(&by_id);
    let mut body = json!({
        "owner": owner,
        "issues": read.views.iter()
            .filter(|view| view.issue.front.owner.as_deref() == Some(owner))
            .map(|view| read.card(&ctx, view))
            .collect::<Vec<_>>(),
    });
    read_model::merge_freshness(&mut body, read.freshness.clone());
    Ok(body)
}

pub(super) fn get(
    request: &Request,
    state_dir: &Path,
    pm_dir: &Path,
    opts: &ServeOpts,
    raw_query: &str,
) -> HttpResp {
    // No owner, subject, role or project selector: the session alone names "my".
    if !raw_query.is_empty() {
        return super::err_response(400, "My work does not accept query parameters");
    }
    let caller = match operator::board_caller(request, state_dir, opts, false, None) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    if matches!(caller, operator::Caller::Agent(_)) {
        return super::err_response(403, "My work requires a human board session");
    }
    let body = match payload(state_dir, pm_dir, caller.author()) {
        Ok(body) => body,
        Err(error) => return super::err_response(503, &error.to_string()),
    };
    // Tracker/daemon reads may await I/O. Never release the result after a
    // membership withdrawal or a caller change during that read.
    match operator::board_caller(request, state_dir, opts, false, None) {
        Ok(current)
            if !matches!(current, operator::Caller::Agent(_))
                && current.author() == caller.author() =>
        {
            super::json_response(body)
        }
        Ok(_) => super::err_response(403, "My work session changed; sign in again"),
        Err(response) => response,
    }
}
