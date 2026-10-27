//! CAD-810 / CAD-821: the board's topic relays.
//!
//! Every write is `RouteClass::OperatorOnly` (listed in `WRITE_ROUTES`, and
//! unlisted writes fail closed to the same class): the board admits only an
//! operator session, and the daemon re-proves the connection on every verb.
//! The board never reads the store and never trusts a body field as sender,
//! caller or topic authority; the body is whitelisted to the verb's fields.
//!
//! - `POST /api/topics` -> `topic_create {type, source}`
//! - `POST /api/topics/<id>/subscribers` -> `topic_subscribe` / `topic_unsubscribe`
//! - `POST /api/topics/<id>/posts` -> `topic_publish`
//! - `POST /api/topics/<id>/requests` -> `topic_request` (one-target Ask)
//! - `GET  /api/topics/<id>` -> `topic_read`

use serde::Deserialize;
use serde_json::json;
use tiny_http::Request;

use super::{err_response, json_response, parse_json, read_body, HttpResp};
use crate::client;
use crate::error::Error;

/// Body cap for a topic write: a post is at most the queued-message cap.
const TOPIC_BODY_CAP: u64 = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateReq {
    #[serde(rename = "type")]
    kind: String,
    source: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipReq {
    action: String,
    alias: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PostReq {
    text: String,
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestReq {
    target: String,
    text: String,
    id: String,
}

/// `(id, sub)` for `/api/topics[/<id>[/<sub>]]`; `("", None)` is the
/// collection. `None` when the path is not a topic route.
pub(super) fn route(path: &str) -> Option<(&str, Option<&str>)> {
    let tail = path.strip_prefix("/api/topics")?;
    if tail.is_empty() {
        return Some(("", None));
    }
    let tail = tail.strip_prefix('/')?;
    let mut segs = tail.splitn(2, '/');
    let id = segs.next().unwrap_or_default();
    Some((id, segs.next()))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
}

/// `POST /api/topics` — `topic_create`.
pub(super) fn create(state_dir: &std::path::Path, request: &mut Request) -> HttpResp {
    let req: CreateReq = match body(request) {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    match client::rpc(
        state_dir,
        "topic_create",
        json!({"type": req.kind, "source": req.source}),
    ) {
        Ok(out) => json_response(out),
        Err(e) => rpc_err(&e),
    }
}

/// `POST /api/topics/<id>/subscribers` — `topic_subscribe` or
/// `topic_unsubscribe`, chosen by the body's `action`.
pub(super) fn membership(state_dir: &std::path::Path, id: &str, request: &mut Request) -> HttpResp {
    if !valid_id(id) {
        return err_response(400, "bad topic id");
    }
    let req: MembershipReq = match body(request) {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    let verb = match req.action.as_str() {
        "subscribe" => "topic_subscribe",
        "unsubscribe" => "topic_unsubscribe",
        _ => return err_response(400, "action must be subscribe or unsubscribe"),
    };
    match client::rpc(state_dir, verb, json!({"topic": id, "alias": req.alias})) {
        Ok(out) => json_response(out),
        Err(e) => rpc_err(&e),
    }
}

/// `POST /api/topics/<id>/posts` — `topic_publish` (the canonical verb).
pub(super) fn publish(state_dir: &std::path::Path, id: &str, request: &mut Request) -> HttpResp {
    if !valid_id(id) {
        return err_response(400, "bad topic id");
    }
    let req: PostReq = match body(request) {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    match client::rpc(
        state_dir,
        "topic_publish",
        json!({"topic": id, "text": req.text, "id": req.id}),
    ) {
        Ok(out) => json_response(out),
        Err(e) => rpc_err(&e),
    }
}

/// `POST /api/topics/<id>/requests` — `topic_request`, one target.
pub(super) fn request(state_dir: &std::path::Path, id: &str, request: &mut Request) -> HttpResp {
    if !valid_id(id) {
        return err_response(400, "bad topic id");
    }
    let req: RequestReq = match body(request) {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    match client::rpc(
        state_dir,
        "topic_request",
        json!({"topic": id, "target": req.target, "text": req.text, "id": req.id}),
    ) {
        Ok(out) => json_response(out),
        Err(e) => rpc_err(&e),
    }
}

/// `GET /api/topics/<id>` — `topic_read`, with the same page query as the
/// work conversation reads.
pub(super) fn read(
    state_dir: &std::path::Path,
    id: &str,
    query: &dyn Fn(&str) -> Option<String>,
) -> HttpResp {
    if !valid_id(id) {
        return err_response(400, "bad topic id");
    }
    let mut params = json!({"topic": id});
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
    match client::rpc(state_dir, "topic_read", params) {
        Ok(out) => json_response(out),
        Err(e) => rpc_err(&e),
    }
}

/// Read and parse one JSON body, capped; the parse refuses unknown fields.
fn body<T: serde::de::DeserializeOwned>(request: &mut Request) -> Result<T, HttpResp> {
    let bytes = read_body(request, TOPIC_BODY_CAP)?;
    parse_json(&bytes)
}

/// Daemon error -> HTTP: an unknown topic or alias 404, a non-member or a
/// refused caller 400 (its text names the rule), an older daemon 501.
fn rpc_err(e: &Error) -> HttpResp {
    let text = e.to_string();
    if text.starts_with("Daemon is not reachable") {
        return err_response(503, &text);
    }
    if text.contains("Unknown method 'topic_") {
        return err_response(501, "this daemon does not support topics");
    }
    if text.contains("Unknown topic") || text.contains("Unknown managed agent") {
        return err_response(404, &text);
    }
    if let Some(resp) = super::busy_response(e) {
        return resp;
    }
    match e {
        Error::Internal(_) => err_response(500, &text),
        _ => err_response(400, &text),
    }
}
