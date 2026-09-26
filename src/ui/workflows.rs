//! Board endpoints for a project's workflows (CAD-496) — the
//! Workflows section's reads and the operator's "new run" proposal:
//!
//! - `GET /api/projects/<key>/workflows` — every stored workflow,
//!   [`workflow::ls`] rows enriched by [`workflow::show`]: name, title,
//!   the declared inputs (`ask` + `optional`), the ticket count, check
//!   errors and the gate-approval state.
//! - `GET /api/projects/<key>/workflows/<name>/preview?inputs=<json>` —
//!   the plan file `plan propose --workflow` would render
//!   ([`workflow::render`]) plus the approval state, so the run form
//!   shows exactly what Propose hands the daemon. A render refusal —
//!   a missing required input, an unknown name — is data here
//!   (`{"error": …}`), not a failed request.
//! - `POST /api/projects/<key>/workflows/<name>/propose` `{"inputs"}`
//!   relays the daemon's `plan_propose` verbatim — the same call
//!   `cadence plan propose --workflow` makes; the board keeps no second
//!   propose path, and the resulting plan waits in Needs you like any
//!   other.
//!
//! Propose is `OperatorOnly` in `operator::WRITE_ROUTES` (an unlisted
//! shape would fail the same way), because the board relays over its
//! own daemon connection: the daemon attributes the proposal to whoever
//! that connection proves — the proven operator or nobody — so an
//! agent's HTTP request must never reach it. `admit` runs the operator
//! proof on the HTTP peer before the handler runs; the daemon's own
//! gate still refuses what the board must not decide — an unapproved
//! workflow answers `workflow_unapproved`.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{json, Map, Value};
use tiny_http::Request;

use super::{err_response, home, json_response, parse_json, read_body, HttpResp};
use crate::client;
use crate::issue::{app, model, workflow, Pm};

/// A propose request's inputs — `{"inputs": {"<name>": "<value>"}}`.
/// Like the daemon, nothing else is read: attribution is the board's
/// connection.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposeReq {
    inputs: Option<BTreeMap<String, String>>,
}

/// A body of run inputs is small — plan proposals carry prose, not files.
const BODY_CAP: u64 = 64 * 1024;

/// What `/api/projects/<key>/workflows…` names — the list, or one
/// workflow's preview.
pub(super) enum ReadRoute<'a> {
    List(&'a str),
    Preview(&'a str, &'a str),
}

/// `(key, name)` for `POST /api/projects/<key>/workflows/<name>/propose`
/// — `<name>` may be `<app>/<wf>` for an installed app's workflow
/// (CAD-547) — `None` when the path is not that route.
pub(super) fn propose_route(path: &str) -> Option<(&str, &str)> {
    let tail = path.strip_prefix("/api/projects/")?;
    let (key, rest) = tail.split_once('/')?;
    let rest = rest.strip_prefix("workflows/")?;
    // `<name>/propose` or `<app>/<wf>/propose` — the name is the path
    // minus its trailing `propose` segment.
    let (name, verb) = rest.rsplit_once('/')?;
    (!key.is_empty() && verb == "propose").then_some((key, name))
}

/// The read route for a path under `/api/projects/<key>/workflows` —
/// exactly `workflows`, `workflows/`, `workflows/<name>/preview` or
/// `workflows/<app>/<wf>/preview` (CAD-547).
pub(super) fn read_route(path: &str) -> Option<ReadRoute<'_>> {
    let tail = path.strip_prefix("/api/projects/")?;
    let (key, rest) = tail.split_once('/')?;
    let rest = rest.strip_prefix("workflows")?;
    if key.is_empty() {
        return None;
    }
    match rest {
        "" | "/" => Some(ReadRoute::List(key)),
        tail => {
            let tail = tail.strip_prefix('/')?;
            let (name, sub) = tail.rsplit_once('/')?;
            (sub == "preview" && !name.is_empty()).then_some(ReadRoute::Preview(key, name))
        }
    }
}

/// `GET` dispatch for the workflow reads.
pub(super) fn read(
    pm: &Pm,
    state_dir: &std::path::Path,
    query: &dyn Fn(&str) -> Option<String>,
    route: ReadRoute<'_>,
) -> HttpResp {
    match route {
        ReadRoute::List(key) => list(pm, state_dir, key),
        ReadRoute::Preview(key, name) => preview(&pm.dir, state_dir, key, name, query),
    }
}

/// The row the Workflows card shows — `show`'s detail trimmed to what
/// the section lists: title, inputs, ticket count, approval.
fn row(detail: &Value) -> Value {
    json!({
        "project": detail["project"],
        "name": detail["name"],
        "title": detail["title"],
        "goal": detail["goal"],
        "tickets": detail["tickets"].as_array().map_or(0, Vec::len),
        "inputs": detail["inputs"],
        "approved": detail["approved"],
        "digest": detail["digest"],
        "errors": detail["errors"],
        "notes": detail["notes"],
    })
}

/// `GET /api/projects/<key>/workflows` — stored workflows plus every
/// installed app's workflows, named `<app>/<wf>` (CAD-547).
fn list(pm: &Pm, state_dir: &std::path::Path, key: &str) -> HttpResp {
    if !model::valid_key(key) {
        return err_response(400, "bad project key");
    }
    let listed = match workflow::ls(pm, Some(key), state_dir) {
        Ok(listed) => listed,
        Err(e) if e.to_string().starts_with("No project") => {
            return err_response(404, &e.to_string())
        }
        Err(e) => return err_response(503, &e.to_string()),
    };
    let mut rows = Vec::new();
    for row in listed["workflows"].as_array().into_iter().flatten() {
        if row.get("error").is_some() {
            rows.push(row.clone());
            continue;
        }
        let Some(name) = row["name"].as_str() else {
            rows.push(row.clone());
            continue;
        };
        match workflow::show(pm, key, name, state_dir) {
            Ok(detail) => rows.push(self::row(&detail)),
            Err(e) => rows.push(json!({
                "project": key,
                "name": name,
                "error": e.to_string(),
            })),
        }
    }
    rows.extend(app::board_rows(&pm.dir, key, state_dir));
    json_response(json!({"workflows": rows}))
}

/// A workflow name for preview/propose — a stored `<name>` or an
/// installed app's `<app>/<wf>` (both halves tag-shaped).
fn valid_workflow_name(name: &str) -> bool {
    model::valid_tag(name) || app::split_ref(name).is_some()
}

/// `GET /api/projects/<key>/workflows/<name>/preview?inputs=<json>` —
/// the rendered plan, or the refusal `render` gives the run form's
/// inputs, plus the approval the daemon would apply.
fn preview(
    pm_dir: &std::path::Path,
    state_dir: &std::path::Path,
    key: &str,
    name: &str,
    query: &dyn Fn(&str) -> Option<String>,
) -> HttpResp {
    if !model::valid_key(key) || !valid_workflow_name(name) {
        return err_response(400, "bad project or workflow name");
    }
    let provided: BTreeMap<String, String> = match query("inputs") {
        None => BTreeMap::new(),
        Some(raw) => {
            let parsed: Map<String, Value> = match serde_json::from_str(&raw) {
                Ok(parsed) => parsed,
                Err(_) => return err_response(400, "inputs must be a JSON object"),
            };
            let mut out = BTreeMap::new();
            for (k, v) in parsed {
                match v.as_str() {
                    Some(value) => {
                        out.insert(k, value.to_string());
                    }
                    None => return err_response(400, "input values must be strings"),
                }
            }
            out
        }
    };
    // `<app>/<wf>` reads the installed app and reports the APP's digest
    // state (approval is whole-app); a bare name stays the stored
    // workflow's own gate digest.
    if let Some((app_name, wf)) = app::split_ref(name) {
        let text = match app::read_workflow(pm_dir, key, app_name, wf) {
            Ok(text) => text,
            Err(e) => return err_response(404, &e.to_string()),
        };
        let digest = app::digest(pm_dir, key, app_name).ok();
        let approvals = app::fetch_approvals(state_dir);
        let install_id = app::current_install_id(pm_dir, key, app_name);
        let approved = match (&digest, &approvals) {
            (Some(digest), Some(approvals)) => {
                json!(app::approved(
                    key,
                    app_name,
                    digest,
                    &install_id,
                    Some(approvals)
                ))
            }
            (Some(_), None) => json!("unknown — daemon unreachable"),
            (None, _) => Value::Null,
        };
        return match workflow::render(&text, &provided) {
            Ok(rendered) => json_response(json!({
                "project": key,
                "name": name,
                "rendered": rendered,
                "approved": approved,
                "digest": digest,
            })),
            Err(e) => json_response(json!({
                "project": key,
                "name": name,
                "rendered": Value::Null,
                "error": e.to_string(),
                "code": e.code(),
                "approved": approved,
                "digest": digest,
            })),
        };
    }
    let text = match workflow::read_for(pm_dir, key, name) {
        Ok(text) => text,
        Err(e) => {
            let msg = e.to_string();
            let code = if msg.contains("unknown workflow") {
                404
            } else {
                400
            };
            return err_response(code, &msg);
        }
    };
    let digest = workflow::gate_digest(&text).ok();
    let approvals = workflow::fetch_approvals(state_dir);
    let approved = match (&digest, &approvals) {
        (Some(digest), Some(approvals)) => {
            json!(workflow::approved(key, name, digest, Some(approvals)))
        }
        (Some(_), None) => json!("unknown — daemon unreachable"),
        (None, _) => Value::Null,
    };
    match workflow::render(&text, &provided) {
        Ok(rendered) => json_response(json!({
            "project": key,
            "name": name,
            "rendered": rendered,
            "approved": approved,
            "digest": digest,
        })),
        Err(e) => json_response(json!({
            "project": key,
            "name": name,
            "rendered": Value::Null,
            "error": e.to_string(),
            // The named refusal (one_line, not_distinct, render_diverged)
            // rides beside the reason so the run form can show it.
            "code": e.code(),
            "approved": approved,
            "digest": digest,
        })),
    }
}

/// `POST /api/projects/<key>/workflows/<name>/propose` — the same
/// `plan_propose` call `cadence plan propose --workflow` makes. The
/// board was already admitted as the operator (`operator::admit`); the
/// daemon attributes the plan to the board's own connection and still
/// applies the workflow-approval gate.
pub(super) fn propose(
    request: &mut Request,
    state_dir: &std::path::Path,
    key: &str,
    name: &str,
) -> HttpResp {
    if !model::valid_key(key) || !valid_workflow_name(name) {
        return err_response(400, "bad project or workflow name");
    }
    let bytes = match read_body(request, BODY_CAP) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    let req: ProposeReq = match parse_json(&bytes) {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    match client::rpc(
        state_dir,
        "plan_propose",
        json!({
            "project": key,
            "workflow": name,
            "inputs": req.inputs.unwrap_or_default(),
        }),
    ) {
        Ok(out) => json_response(out),
        Err(e) => home::rpc_err(&e, "plan_propose"),
    }
}
