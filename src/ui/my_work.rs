//! `GET /api/my-work` — verified hosted assignments for the signed-in board session.
//!
//! Subject and session come from the live board cookie. Query fields and an
//! Authorization header are refused. The handler verifies the member-work proof,
//! then re-reads the session before answering. A revoked session, a null role,
//! or any proof failure is a refusal, never an empty success. A local session
//! on a board without the hosted channel gets a clear unavailable answer.

use std::sync::Mutex;

use serde_json::{json, Value};
use tiny_http::{Header, Request, Response, StatusCode};

use crate::board_identity::JwksCache;
use crate::board_work::{self, VerifiedWork};
use crate::operator_auth::Origin;

use super::operator;
use super::ServeOpts;

static WORK_JWKS: Mutex<JwksCache> = Mutex::new(JwksCache::new());

fn json_status(status: u16, value: Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec_pretty(&value).unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(status));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp
}

fn denied() -> Response<std::io::Cursor<Vec<u8>>> {
    json_status(
        401,
        json!({
            "ok": false,
            "code": "board_session_required",
            "message": "A board session is required."
        }),
    )
}

fn refused() -> Response<std::io::Cursor<Vec<u8>>> {
    json_status(503, board_work::unavailable_body())
}

fn bad_subject() -> Response<std::io::Cursor<Vec<u8>>> {
    json_status(
        400,
        json!({
            "ok": false,
            "code": "subject_not_accepted",
            "message": "My work uses the signed-in board session."
        }),
    )
}

fn query_tries_to_name_subject(raw_query: &str) -> bool {
    raw_query.split('&').any(|kv| {
        let key = kv.split_once('=').map(|(k, _)| k).unwrap_or(kv);
        matches!(
            key,
            "sub" | "subject" | "role" | "session" | "bearer" | "token"
        )
    })
}

fn authorization_present(request: &Request) -> bool {
    request
        .headers()
        .iter()
        .any(|h| h.field.equiv("Authorization"))
}

fn subject_of(session: &Value) -> Option<String> {
    let sub = session
        .get("user")
        .and_then(|user| user.get("sub"))
        .and_then(|sub| sub.as_str())?;
    if sub.is_empty() || sub.len() > 128 || sub.chars().any(char::is_control) {
        None
    } else {
        Some(sub.to_string())
    }
}

/// `Ok(Some)` is a live public subject. `Ok(None)` is no session.
/// `Err` is either hosted-work unavailable (local session) or a daemon miss.
fn live_subject(
    request: &Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
) -> Result<Option<String>, Response<std::io::Cursor<Vec<u8>>>> {
    let Some(origin) = operator::request_origin_kind(request, opts) else {
        return Ok(None);
    };
    let Some(token) = operator::session_token_for(request, opts, origin) else {
        return Ok(None);
    };
    if origin != Origin::Public {
        return Err(refused());
    }
    let budget = crate::client::MetaBudget::fresh(crate::client::MetaBudget::READ_CAP);
    match budget.read(
        state_dir,
        "board_session_check",
        json!({"token": token}),
    ) {
        Ok(value) if value["valid"] == true && value["session"].is_object() => {
            Ok(subject_of(&value["session"]))
        }
        Ok(value) if value["valid"] == false => Ok(None),
        _ => Err(refused()),
    }
}

fn cookie<'a>(request: &'a Request, opts: &ServeOpts) -> Option<String> {
    let origin = operator::request_origin_kind(request, opts)?;
    operator::session_token_for(request, opts, origin)
}

fn body_of(work: &VerifiedWork) -> Value {
    let item = |row: &crate::board_work::WorkItem| {
        json!({
            "id": row.id,
            "issue_id": row.issue_id,
            "project": row.project,
            "action": row.action,
            "state": row.state,
            "generation": row.generation,
            "created_by_you": row.created_by_you,
            "assigned_to_you": row.assigned_to_you,
        })
    };
    json!({
        "ok": true,
        "role": work.role,
        "assigned": work.assigned.iter().map(item).collect::<Vec<_>>(),
        "created": work.created.iter().map(item).collect::<Vec<_>>(),
    })
}

pub(super) fn get(
    request: &Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    raw_query: &str,
) -> Response<std::io::Cursor<Vec<u8>>> {
    if query_tries_to_name_subject(raw_query) || authorization_present(request) {
        return bad_subject();
    }
    let before = match live_subject(request, state_dir, opts) {
        Ok(Some(sub)) => sub,
        Ok(None) => return denied(),
        Err(resp) => return resp,
    };
    let Some(token) = cookie(request, opts) else {
        return denied();
    };
    let Some((company, instance)) = board_work::hosted_binding(&|key| std::env::var(key).ok())
    else {
        return refused();
    };
    let work = match board_work::lookup_member_work(&token, &before, &company, &instance, &WORK_JWKS)
    {
        Ok(work) => work,
        Err(_) => return refused(),
    };
    match live_subject(request, state_dir, opts) {
        Ok(Some(sub)) if sub == before => json_status(200, body_of(&work)),
        Ok(_) => denied(),
        Err(resp) => resp,
    }
}
