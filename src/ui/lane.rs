//! CAD-608: the issue page's lane card. `GET /api/issues/<id>/lane`
//! reads `lane_show`. The writes relay `lane_ask`, `lane_instruct`,
//! `lane_interrupt`, `lane_stop`, `lane_unfence` and `lane_reassign`.
//!
//! `operator::admit` has already refused an agent session and a member
//! before any function here runs. The relay then asserts the operator
//! on the daemon connection: an in-process board otherwise looks like
//! the test process's agent ancestry. Without the test-seam feature
//! the assertion is a no-op and the board process is the proof.

use std::path::Path;

use serde_json::{json, Value};
use tiny_http::Request;

use super::{err_response, json_response, read_body, HttpResp};
use crate::client;
use crate::issue::model;
use crate::test_seam::{self, Asserted};

const BODY_CAP: u64 = 64 * 1024;

/// `<id>/lane/<verb>` from the path after `/api/issues/`.
pub(super) fn write_target(tail: &str) -> Option<(&str, &str)> {
    let (id, rest) = tail.split_once('/')?;
    let verb = rest.strip_prefix("lane/")?;
    if id.is_empty() || verb.is_empty() || verb.contains('/') {
        return None;
    }
    Some((id, verb))
}

/// `GET /api/issues/<id>/lane`.
pub(super) fn show(state_dir: &Path, id: &str) -> HttpResp {
    relay(state_dir, "lane_show", json!({"issue": id}))
}

/// `POST /api/issues/<id>/lane/<verb>`. The path's id wins over a body
/// field, so the request cannot be aimed at a different issue.
pub(super) fn post(request: &mut Request, state_dir: &Path, id: &str, verb: &str) -> HttpResp {
    let Ok(id) = model::check_id(id) else {
        return err_response(400, "bad issue id");
    };
    let method = match verb {
        "ask" => "lane_ask",
        "instruct" => "lane_instruct",
        "interrupt" => "lane_interrupt",
        "stop" => "lane_stop",
        "unfence" => "lane_unfence",
        "reassign" => "lane_reassign",
        _ => return err_response(404, "no such lane route"),
    };
    let bytes = match read_body(request, BODY_CAP) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    let mut params: Value = if bytes.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) if v.is_object() => v,
            _ => return err_response(400, "lane body must be a JSON object"),
        }
    };
    if let Some(obj) = params.as_object_mut() {
        obj.insert("issue".to_string(), json!(id));
    }
    relay(state_dir, method, params)
}

/// Operator assertion AFTER admit. The feature-off build compiles this
/// to a plain `client::rpc`.
fn relay(state_dir: &Path, method: &str, params: Value) -> HttpResp {
    let result = test_seam::scoped(Asserted::Operator, || {
        client::rpc(state_dir, method, params)
    });
    match result {
        Ok(out) => json_response(out),
        Err(e) => super::home::rpc_err(&e, method),
    }
}
