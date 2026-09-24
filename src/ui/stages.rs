//! Board endpoints behind the Projects screen's epics and milestones
//! (CAD-432, over the CAD-405 work model):
//!
//! - `POST /api/epics/<EPIC>/stage` `{"stage", "note"?}` — relay the
//!   daemon's `epic_stage`. The board never writes the tracker for a
//!   stage move: the daemon checks the move and commits it.
//! - `GET /api/milestones?project=` — `cadence milestone ls --json`'s
//!   rows: size-weighted progress and worst health per milestone.
//!
//! A stage move is relayed over the BOARD's own daemon connection, so
//! the daemon attributes every relayed move — routine or operator-gated
//! — to whoever the board process is: the operator. The board therefore
//! applies the operator rule to every move, not only to moves into an
//! operator stage: the write guards, then an agent-attributed caller is
//! refused (403 `operator_only`), then the HTTP peer must carry
//! CAD-276's positive operator proof (403 `operator_proof`) — the same
//! [`home::operator_write`] path the plan decisions take. Agents move
//! stages through their own `cadence issue epic stage`, where the
//! daemon attributes the move to their lane. Identity-shaped request
//! fields are never read: the body denies unknown fields.

use serde::Deserialize;
use serde_json::json;
use tiny_http::Request;

use super::home::{operator_write, rpc_err};
use super::{err_response, json_response, parse_json, read_body, HttpResp, ServeOpts};
use crate::client;
use crate::issue::{board, model, Pm};

/// A stage name and an optional note.
const BODY_CAP: u64 = 16 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StageReq {
    stage: String,
    note: Option<String>,
}

/// The epic id of `/api/epics/<id>/stage`, `None` for any other path.
pub(super) fn stage_route(path: &str) -> Option<&str> {
    let id = path.strip_prefix("/api/epics/")?.strip_suffix("/stage")?;
    (!id.is_empty() && !id.contains('/')).then_some(id)
}

/// `POST /api/epics/<epic>/stage`.
pub(super) fn move_stage(
    request: &mut Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    epic: &str,
) -> HttpResp {
    if let Err(resp) = operator_write(request, state_dir, opts, "a stage move") {
        return resp;
    }
    let Ok(epic) = model::check_id(epic) else {
        return err_response(400, "bad epic id");
    };
    let bytes = match read_body(request, BODY_CAP) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    let req: StageReq = match parse_json(&bytes) {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    let stage = req.stage.trim();
    if stage.is_empty() {
        return err_response(400, "a stage move needs a stage");
    }
    let note = req.note.as_deref().map(str::trim).filter(|n| !n.is_empty());
    match client::rpc(
        state_dir,
        "epic_stage",
        // Every relayed move is the operator's decision: the daemon
        // then demands the operator on this (the board's) connection
        // for any target, so a board started under an agent can never
        // land the operator's move as that agent's.
        json!({"epic": epic, "stage": stage, "note": note, "operator_decision": true}),
    ) {
        Ok(out) => json_response(out),
        Err(e) => rpc_err(&e, "epic_stage"),
    }
}

/// `GET /api/milestones?project=` — every milestone row, or one
/// project's.
pub(super) fn milestones(
    state_dir: &std::path::Path,
    pm_dir: &std::path::Path,
    project: Option<&str>,
) -> HttpResp {
    if project.is_some_and(|p| !model::valid_key(p)) {
        return err_response(400, "bad project key");
    }
    let pm = match Pm::at(pm_dir) {
        Ok(pm) => pm,
        Err(e) => return err_response(503, &e.to_string()),
    };
    // Every project loads so cross-project children count.
    let read = super::read_model::get(state_dir, pm_dir).board(&pm, None);
    let by_id: std::collections::HashMap<String, &board::View> = read.by_id();
    let ctx = read.ctx(&by_id);
    json_response(json!({
        "milestones": crate::issue::work::milestones_json(&ctx, &read.views, project),
    }))
}
