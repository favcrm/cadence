//! Board endpoints for the operator's PR-head approval (CAD-1218):
//!
//! - `POST /api/approvals/approve` `{"repo","pr","head"}` — relay the
//!   daemon's `approval_record_shown`: record a `merge` approval for the
//!   exact head the drawer showed.
//! - `POST /api/approvals/<id>/revoke` `{"reason"}` — relay the daemon's
//!   `approval_revoke`.
//!
//! Both are operator-only in `operator::WRITE_ROUTES`, so `admit` has run
//! the write guards, the session check and the HTTP-peer operator proof
//! before a handler here runs. Each handler then accepts only
//! `Caller::Operator` — a public AgenticOS `owner` session passes `admit`
//! to an operator-only route but is no approver (`approver_not_allowed`).
//! The deciding actor is the one `board_caller` derived, relayed as
//! `request_actor`; the daemon applies the approver allowlist again. The
//! bodies deny unknown fields, so no request names the source, action or id.

use serde::Deserialize;
use serde_json::json;
use tiny_http::Request;

use super::home::rpc_err;
use super::operator::Caller;
use super::{
    coded_response, err_response, guard_fail, json_response, parse_json, pct_decode, read_body,
    HttpResp, ServeOpts,
};
use crate::client;

const BODY_CAP: u64 = 16 * 1024;
const REASON_MAX: usize = 400;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveReq {
    repo: String,
    pr: u64,
    head: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeReq {
    reason: String,
}

pub(super) enum Route<'a> {
    Approve,
    Revoke(&'a str),
}

pub(super) fn route(path: &str) -> Option<Route<'_>> {
    let tail = path.strip_prefix("/api/approvals/")?;
    if tail == "approve" {
        return Some(Route::Approve);
    }
    let id = tail.strip_suffix("/revoke")?;
    (!id.is_empty() && !id.contains('/')).then_some(Route::Revoke(id))
}

pub(super) fn handle(
    request: &mut Request,
    state_dir: &std::path::Path,
    caller: &Caller,
    route: Route<'_>,
) -> HttpResp {
    // An allowlist: only a proven operator board session approves.
    let Caller::Operator(actor) = caller else {
        return guard_fail("approver_not_allowed", NOT_APPROVER);
    };
    let bytes = match read_body(request, BODY_CAP) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    let (method, params) = match route {
        Route::Approve => {
            let req: ApproveReq = match parse_json(&bytes) {
                Ok(req) => req,
                Err(resp) => return resp,
            };
            (
                "approval_record_shown",
                json!({"repo": req.repo, "pr": req.pr, "head": req.head, "request_actor": actor}),
            )
        }
        Route::Revoke(id) => {
            let req: RevokeReq = match parse_json(&bytes) {
                Ok(req) => req,
                Err(resp) => return resp,
            };
            let reason = req.reason.trim();
            if reason.is_empty() || reason.len() > REASON_MAX {
                return coded_response(
                    400,
                    "reason_required",
                    "a revoke needs a short reason",
                    None,
                );
            }
            (
                "approval_revoke_shown",
                json!({"id": id, "reason": reason, "request_actor": actor}),
            )
        }
    };
    match client::rpc(state_dir, method, params) {
        Ok(out) => json_response(out),
        Err(e) if e.code() == Some("head_moved") => {
            coded_response(409, "head_moved", &e.to_string(), None)
        }
        Err(e) => rpc_err(&e, method),
    }
}

const NOT_APPROVER: &str = "only the operator can record or revoke an approval from the board";

/// `GET /api/approvals/state`: exactly `repo`, `pr` and `head`, each once,
/// the head the full lowercase 40-hex SHA — else 400 before any lookup.
pub(super) fn state(
    request: &Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    raw_query: &str,
) -> HttpResp {
    let caller = match super::operator::admit_operator_read_caller(request, state_dir, opts) {
        Ok(caller) => caller,
        Err(resp) => return resp,
    };
    let Caller::Operator(actor) = caller else {
        return guard_fail("approver_not_allowed", NOT_APPROVER);
    };
    let (mut repo, mut pr, mut head) = (None, None, None);
    for pair in raw_query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let slot = match key {
            "repo" => &mut repo,
            "pr" => &mut pr,
            "head" => &mut head,
            _ => return err_response(400, "unknown query key"),
        };
        let Some(value) = pct_decode(value).filter(|_| slot.is_none()) else {
            return err_response(400, "each of repo, pr and head is given once");
        };
        *slot = Some(value);
    }
    let (Some(repo), Some(pr), Some(head)) = (repo, pr, head) else {
        return err_response(400, "repo, pr and head are required");
    };
    let Some(pr) = pr.parse::<u64>().ok().filter(|n| *n > 0) else {
        return err_response(400, "pr must be a pull request number");
    };
    if head.len() != 40
        || !head
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return err_response(400, "head must be the full lowercase 40-character SHA");
    }
    let params = json!({"repo": repo, "pr": pr, "head": head, "request_actor": actor});
    match client::rpc(state_dir, "approval_state", params) {
        Ok(out) => json_response(out),
        Err(e) => rpc_err(&e, "approval_state"),
    }
}
