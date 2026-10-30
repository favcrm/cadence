//! Board surface for the hosted merge gate (CAD-1946).
//!
//! `GET /api/delivery/gate?repo&pr&head` runs the check. The operator
//! sees the checklist; the response is the script's document.
//! `POST /api/delivery/gate/enqueue` with `{"repo","pr","head"}` runs
//! the enqueue when that check is clean. Neither body names an actor,
//! a class, a verdict or a signing key. A head that moved is
//! `409 head_moved` and records nothing.

use serde::Deserialize;
use serde_json::Value;
use tiny_http::Request;

use super::approvals::approver;
use super::home::rpc_err;
use super::operator::Caller;
use super::{
    coded_response, err_response, guard_fail, json_response, parse_json, pct_decode, read_body,
    HttpResp, ServeOpts,
};
use crate::client;

const BODY_CAP: u64 = 8 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnqueueReq {
    repo: String,
    pr: u64,
    head: String,
}

pub(super) fn enqueue(
    request: &mut Request,
    state_dir: &std::path::Path,
    caller: &Caller,
) -> HttpResp {
    let Some(actor) = approver(caller) else {
        return guard_fail(
            "approver_not_allowed",
            "only the operator can enqueue from the board",
        );
    };
    // The actor is checked and then not sent. The daemon's authority
    // is this board's operator connection, the same one approvals use.
    let _ = actor;
    let bytes = match read_body(request, BODY_CAP) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    let req: EnqueueReq = match parse_json(&bytes) {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    answered(
        client::rpc(
            state_dir,
            "delivery_gate_enqueue",
            serde_json::json!({"repo": req.repo, "pr": req.pr, "head": req.head}),
        ),
        true,
    )
}

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
    if approver(&caller).is_none() {
        return guard_fail(
            "approver_not_allowed",
            "only the operator can read the merge check from the board",
        );
    }
    let (mut repo, mut pr, mut head) = (None, None, None);
    for pair in raw_query.split('&') {
        if pair.is_empty() {
            continue;
        }
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
    // A moved head is a checklist row, not an error: the card has to say
    // what is missing. Enqueue is the call that must refuse before signing.
    match client::rpc(
        state_dir,
        "delivery_gate_check",
        serde_json::json!({"repo": repo, "pr": pr, "head": head}),
    ) {
        Ok(out) => json_response(out),
        Err(error) => rpc_err(&error, "delivery_gate"),
    }
}

fn answered(result: Result<Value, crate::error::Error>, refuse_moved: bool) -> HttpResp {
    match result {
        Ok(out) if refuse_moved && head_moved(&out) => coded_response(
            409,
            "head_moved",
            "the PR head moved since it was shown",
            None,
        ),
        Ok(out) => json_response(out),
        Err(error) if error.code() == Some("head_moved") => {
            coded_response(409, "head_moved", &error.to_string(), None)
        }
        Err(error) if error.code() == Some("approver_not_allowed") => guard_fail(
            "approver_not_allowed",
            "only the operator can enqueue from the board",
        ),
        Err(error) => rpc_err(&error, "delivery_gate"),
    }
}

fn head_moved(value: &Value) -> bool {
    value
        .get("reasons")
        .and_then(Value::as_array)
        .is_some_and(|reasons| {
            reasons.iter().any(|reason| {
                reason
                    .as_str()
                    .is_some_and(|text| text.contains("not the pinned"))
            })
        })
}
