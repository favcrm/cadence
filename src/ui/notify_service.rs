//! CAD-1423 / AOS-238: the platform-service route for notification delivery.
//!
//! `POST /api/internal/notifications/{prepare,finish}` is diverted on the public
//! host ahead of the cookie-session gates, like `/api/assist/`. It has no
//! browser session, cookie or CSRF path. The platform's delivery proof travels
//! ONLY in the `Authorization: Bearer` header; it is moved into the RPC params
//! as `proof`. The body carries references only, and any other field is refused.
//! The daemon RPC verifies the proof before any business check.
//!
//! Success bodies are the unwrapped RPC data (prepare: the lease object; finish:
//! `{ok:true}`). Refusals use `{ok:false,error:{code,message}}` with no-store.

use serde_json::{json, Map, Value};
use tiny_http::{Header, Method, Request, Response, StatusCode};

use super::write_path::{header_value, read_body, HttpResp};
use super::ServeOpts;
use crate::client;
use crate::error::Error;

/// The cap on a platform delivery request body.
const BODY_CAP: u64 = 1024;
/// The longest compact proof accepted in the Authorization header.
const PROOF_MAX: usize = 4096;
/// The largest integer a JSON number can carry exactly.
const SAFE_MAX: i64 = 9_007_199_254_740_991;

fn fail(status: u16, code: &str, message: &str) -> HttpResp {
    let body = serde_json::to_vec(&json!({
        "ok": false,
        "error": {"code": code, "message": message},
    }))
    .unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(status));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp
}

fn unwrapped(data: Value) -> HttpResp {
    let body = serde_json::to_vec(&data).unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(200));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp
}

/// The bounded compact proof from `Authorization: Bearer <token>`.
fn bearer(request: &Request) -> Option<String> {
    let raw = header_value(request, "Authorization")?;
    let token = raw.strip_prefix("Bearer ")?;
    (!token.is_empty() && token.len() <= PROOF_MAX && token.bytes().all(|b| b.is_ascii_graphic()))
        .then(|| token.to_string())
}

/// The reference fields a delivery body may carry. Anything else is refused,
/// including any endpoint, recipient, key or proof sent in the body.
fn allowed_fields(verb: &str) -> Option<&'static [&'static str]> {
    match verb {
        "prepare" => Some(&["intent_id", "device_id"]),
        "finish" => Some(&["intent_id", "device_id", "lease_id", "outcome"]),
        _ => None,
    }
}

/// Validate the body's reference fields: a positive safe integer intent id,
/// bounded ids, and a closed outcome set. Returns the params to relay.
fn references(verb: &str, body: &Map<String, Value>) -> Result<Map<String, Value>, HttpResp> {
    let allowed = allowed_fields(verb).unwrap_or(&[]);
    if body.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(fail(
            400,
            "invalid_request",
            "a delivery field is not allowed",
        ));
    }
    let intent_ok = body
        .get("intent_id")
        .and_then(Value::as_i64)
        .is_some_and(|n| (1..=SAFE_MAX).contains(&n));
    if !intent_ok {
        return Err(fail(
            400,
            "invalid_request",
            "intent_id must be a positive safe integer",
        ));
    }
    let bounded = |field: &str| {
        body.get(field)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty() && s.len() <= 128)
    };
    if !bounded("device_id") {
        return Err(fail(400, "invalid_request", "device_id is not valid"));
    }
    if verb == "finish" {
        if !bounded("lease_id") {
            return Err(fail(400, "invalid_request", "lease_id is not valid"));
        }
        let outcome = body
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !matches!(outcome, "accepted" | "gone" | "retry" | "suppressed") {
            return Err(fail(400, "invalid_request", "outcome is not recognized"));
        }
    }
    Ok(body.clone())
}

/// Route one `/api/internal/notifications/<verb>` request. `tail` is the verb.
pub(super) fn post(
    request: &mut Request,
    method: &Method,
    tail: &str,
    state_dir: &std::path::Path,
    _opts: &ServeOpts,
) -> HttpResp {
    if *method != Method::Post {
        return fail(405, "invalid_request", "delivery calls are POST");
    }
    let rpc = match tail {
        "prepare" => "human_notification_delivery_prepare",
        "finish" => "human_notification_delivery_finish",
        _ => return fail(404, "not_found", "no such delivery route"),
    };
    let Some(proof) = bearer(request) else {
        return fail(
            401,
            "invalid_proof",
            "the delivery proof could not be verified",
        );
    };
    let bytes = match read_body(request, BODY_CAP) {
        Ok(b) => b,
        Err(_) => return fail(413, "invalid_request", "the delivery body is too large"),
    };
    let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(&bytes) else {
        return fail(
            400,
            "invalid_request",
            "the delivery body must be a JSON object",
        );
    };
    let mut params = match references(tail, &body) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    params.insert("proof".to_string(), json!(proof));
    match client::rpc(state_dir, rpc, Value::Object(params)) {
        Ok(data) => unwrapped(data),
        Err(e) => refusal(&e),
    }
}

/// Map a daemon refusal to the platform contract. Codes come from the real
/// error code where present. Anything else is a sanitized unavailable.
fn refusal(e: &Error) -> HttpResp {
    let code = e.code().unwrap_or_default();
    let msg = e.to_string();
    let is = |c: &str| code == c || msg.contains(c);
    if is("invalid_proof") {
        fail(
            401,
            "invalid_proof",
            "the delivery proof could not be verified",
        )
    } else if is("lease_active") {
        fail(
            409,
            "lease_active",
            "this delivery already has a live lease",
        )
    } else if is("stale_lease") {
        fail(409, "stale_lease", "the delivery lease is no longer live")
    } else if is("invalid_request") {
        fail(400, "invalid_request", "the delivery request is not valid")
    } else {
        fail(503, "unavailable", "notification delivery is unavailable")
    }
}
