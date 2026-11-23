//! CAD-1853: the board's read/write relay for cloud-agent connections.
//!
//! Reads are operator-only and relay the daemon's own projection; writes are
//! classified `OperatorOnly` in [`super::operator::WRITE_ROUTES`], so the
//! session, same-origin and process-proof gates run before any daemon call.
//! A browser request cannot name a sender, role, tenant or remote id: the
//! body carries only the local thread, text and operation id, and the daemon
//! derives the rest. Cloud-agent history stays in canonical Threads, never a
//! second store here.

use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::Request;

use super::{err_response, json_response, parse_json, read_body, HttpResp};
use crate::client;
use crate::error::Error;

const BODY_CAP: u64 = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SendRequest {
    pub thread_id: String,
    pub operation_id: String,
    pub text: String,
}

fn rpc_err(error: &Error) -> HttpResp {
    match error {
        Error::Structured(structure) => super::coded_response(
            match structure.kind {
                "gate" => 403,
                "conflict" => 409,
                "unknown" => 504,
                _ => 400,
            },
            &structure.code,
            &structure.message,
            structure.revision,
        ),
        Error::OutcomeUnknown(message) => {
            super::coded_response(504, "cloud_agent_uncertain", message, None)
        }
        _ => err_response(400, &error.to_string()),
    }
}

/// `GET /api/cloud-agents` — every connection this daemon holds.
pub(super) fn list(state_dir: &std::path::Path) -> HttpResp {
    match client::rpc(state_dir, "cloud_agent_list", json!({})) {
        Ok(value) => json_response(value),
        Err(error) => rpc_err(&error),
    }
}

/// `GET /api/cloud-agents/<id>` — one connection's identity, runtime,
/// capabilities, sessions and canonical Threads links.
pub(super) fn show(state_dir: &std::path::Path, id: &str) -> HttpResp {
    match client::rpc(state_dir, "cloud_agent_show", json!({"cloud_agent_id": id})) {
        Ok(value) => json_response(value),
        Err(error) => rpc_err(&error),
    }
}

/// `POST /api/cloud-agents/<id>/messages` — the board's chat message. The
/// `OperatorOnly` class ran already; `thread_id` is the canonical thread the
/// entry lands in.
pub(super) fn post_message(
    request: &mut Request,
    state_dir: &std::path::Path,
    id: &str,
) -> HttpResp {
    let bytes = match read_body(request, BODY_CAP) {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    let body: SendRequest = match parse_json(&bytes) {
        Ok(body) => body,
        Err(response) => return response,
    };
    match client::rpc(
        state_dir,
        "cloud_agent_send",
        json!({
            "cloud_agent_id": id,
            "thread_id": body.thread_id,
            "operation_id": body.operation_id,
            "text": body.text,
            // The board send is the operator's explicit request; there is no
            // background wake and no implicit turn from a read.
            "wake": true,
        }),
    ) {
        Ok(value) => json_response(value),
        Err(error) => rpc_err(&error),
    }
}

/// `POST /api/cloud-agents/<id>/operations/<operation_id>/read` — readback.
pub(super) fn post_read(
    request: &mut Request,
    state_dir: &std::path::Path,
    id: &str,
    operation_id: &str,
) -> HttpResp {
    let bytes = match read_body(request, 1024) {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    if !bytes.is_empty() {
        match parse_json::<Value>(&bytes) {
            Ok(Value::Object(object)) if object.is_empty() => {}
            Ok(_) => return err_response(400, "readback takes an empty JSON body"),
            Err(response) => return response,
        }
    }
    match client::rpc(
        state_dir,
        "cloud_agent_read",
        json!({"cloud_agent_id": id, "operation_id": operation_id}),
    ) {
        Ok(value) => json_response(value),
        Err(error) => rpc_err(&error),
    }
}
