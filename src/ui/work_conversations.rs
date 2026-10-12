//! CAD-819: the board's read relay for the cross-agent work conversation
//! view — `GET /api/work-conversations` and
//! `GET /api/work-conversations/<id>`.
//!
//! Both are reads, relayed to the daemon's `work_conversation_list` /
//! `work_conversation_read`, which re-prove the operator on the
//! connection (`operator_chat`). The board never reads the store itself
//! and never trusts a query value as a participant identity: the
//! conversation id is validated by the daemon, not built here. An older
//! daemon answers 501.

use serde_json::{json, Value};

use super::{busy_response, err_response, json_response, HttpResp};
use crate::client;
use crate::error::Error;

/// `(id)` for `/api/work-conversations[/<id>]`, `None` when the path is
/// not this route at all. The bare collection is `Some("")`.
pub(super) fn route(path: &str) -> Option<&str> {
    let tail = path.strip_prefix("/api/work-conversations")?;
    match tail {
        "" => Some(""),
        _ => tail.strip_prefix('/'),
    }
}

/// `GET /api/work-conversations?project=&agent=&include_system=`
pub(super) fn list(
    state_dir: &std::path::Path,
    query: &dyn Fn(&str) -> Option<String>,
) -> HttpResp {
    let mut params = json!({});
    for field in ["project", "agent"] {
        if let Some(v) = query(field) {
            params[field] = Value::String(v);
        }
    }
    if matches!(query("include_system").as_deref(), Some("1" | "true")) {
        params["include_system"] = json!(true);
    }
    match client::rpc(state_dir, "work_conversation_list", params) {
        Ok(page) => json_response(page),
        Err(e) => rpc_err(&e),
    }
}

/// `GET /api/work-conversations/<id>?tail=1|before=|after=&limit=&project=&agent=&include_system=`
/// The list's scope (`project`, `agent`, `include_system`) is forwarded
/// so the timeline matches the list. The daemon resolves every value.
pub(super) fn read(
    state_dir: &std::path::Path,
    id: &str,
    query: &dyn Fn(&str) -> Option<String>,
) -> HttpResp {
    let mut params = json!({"conversation": id});
    if matches!(query("tail").as_deref(), Some("1" | "true")) {
        params["tail"] = json!(true);
    }
    for field in ["before", "after", "limit"] {
        if let Some(raw) = query(field) {
            match raw.trim().parse::<i64>() {
                Ok(n) => params[field] = json!(n),
                Err(_) => return err_response(400, &format!("{field} must be an integer")),
            }
        }
    }
    for field in ["project", "agent"] {
        if let Some(v) = query(field) {
            params[field] = Value::String(v);
        }
    }
    if matches!(query("include_system").as_deref(), Some("1" | "true")) {
        params["include_system"] = json!(true);
    }
    match client::rpc(state_dir, "work_conversation_read", params) {
        Ok(page) => json_response(page),
        Err(e) => rpc_err(&e),
    }
}

/// Daemon error -> HTTP, the same mapping as the threads relay: an older
/// daemon 501, an unknown conversation 404, a refusal 400.
fn rpc_err(e: &Error) -> HttpResp {
    let text = e.to_string();
    if text.starts_with("Daemon is not reachable") {
        return err_response(503, &text);
    }
    if text.contains("Unknown method 'work_conversation_") {
        return err_response(501, "this daemon does not support work conversations");
    }
    if text.contains("Unknown conversation") {
        return err_response(404, &text);
    }
    if let Some(resp) = busy_response(e) {
        return resp;
    }
    match e {
        Error::Internal(_) => err_response(500, &text),
        _ => err_response(400, &text),
    }
}
