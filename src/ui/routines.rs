//! Board HTTP endpoints for Routines, Connection Policies, and Inline
//! Credential Resume (CAD-2317).
//!
//! All write routes are declared `RouteClass::OperatorOnly` in
//! `operator::WRITE_ROUTES`, and all read routes are admitted via
//! `operator::admit_operator_read` — non-owner sessions, unauthenticated
//! peers, and agent callers are refused at the HTTP boundary before reaching
//! the daemon, and the daemon re-proves `operator_connection` on every write.

use std::path::Path;

use serde_json::{json, Value};
use tiny_http::Request;

use super::home::rpc_err;
use super::{err_response, json_response, pct_decode, read_body, HttpResp};
use crate::client;

const BODY_CAP: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Route<'a> {
    List,
    Show(&'a str),
    Update(&'a str),
    Approve(&'a str),
    Run(&'a str),
    Pause(&'a str),
    Resume(&'a str),
    Remove(&'a str),
    ConnectionPolicy(&'a str),
    ConnectionRequestContinue(&'a str),
}

impl Route<'_> {
    pub(super) fn is_read(self) -> bool {
        matches!(self, Self::List | Self::Show(_) | Self::ConnectionPolicy(_))
    }

    pub(super) fn is_writable(self) -> bool {
        !matches!(self, Self::Show(_))
    }
}

fn valid_segment(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

pub(super) fn route(path: &str) -> Option<Route<'_>> {
    if path == "/api/routines" {
        return Some(Route::List);
    }
    if let Some(tail) = path.strip_prefix("/api/routines/") {
        if let Some((id, verb)) = tail.split_once('/') {
            if !valid_segment(id) || verb.contains('/') {
                return None;
            }
            return match verb {
                "update" => Some(Route::Update(id)),
                "approve" => Some(Route::Approve(id)),
                "run" => Some(Route::Run(id)),
                "pause" => Some(Route::Pause(id)),
                "resume" => Some(Route::Resume(id)),
                "remove" => Some(Route::Remove(id)),
                _ => None,
            };
        }
        return valid_segment(tail).then_some(Route::Show(tail));
    }
    if let Some(tail) = path.strip_prefix("/api/connections/") {
        if let Some((id, "policy")) = tail.split_once('/') {
            if valid_segment(id) {
                return Some(Route::ConnectionPolicy(id));
            }
        }
        return None;
    }
    if let Some(tail) = path.strip_prefix("/api/connection-requests/") {
        if let Some((id, "continue")) = tail.split_once('/') {
            if valid_segment(id) {
                return Some(Route::ConnectionRequestContinue(id));
            }
        }
    }
    None
}

pub(super) fn handle_read(state_dir: &Path, route: Route<'_>, raw_query: &str) -> HttpResp {
    match route {
        Route::List => {
            let mut agent_alias: Option<String> = None;
            for pair in raw_query.split('&').filter(|p| !p.is_empty()) {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                let Some(decoded) = pct_decode(value) else {
                    return err_response(400, "invalid query parameter");
                };
                match key {
                    "agent" | "agent_alias" => agent_alias = Some(decoded),
                    _ => return err_response(400, "unsupported query parameter"),
                }
            }
            let mut params = json!({});
            if let Some(agent) = agent_alias {
                params["agent_alias"] = json!(agent);
            }
            match client::rpc(state_dir, "routine_list", params) {
                Ok(out) => json_response(out),
                Err(e) => rpc_err(&e, "routine_list"),
            }
        }
        Route::Show(id) => match client::rpc(state_dir, "routine_show", json!({ "id": id })) {
            Ok(out) => json_response(out),
            Err(e) => rpc_err(&e, "routine_show"),
        },
        Route::ConnectionPolicy(id) => {
            match client::rpc(
                state_dir,
                "connection_policy_get",
                json!({ "connection_id": id }),
            ) {
                Ok(out) => json_response(out),
                Err(e) => rpc_err(&e, "connection_policy_get"),
            }
        }
        _ => err_response(405, "method not allowed"),
    }
}

pub(super) fn handle_write(request: &mut Request, state_dir: &Path, route: Route<'_>) -> HttpResp {
    let bytes = match read_body(request, BODY_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let mut body: Value = if bytes.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(Value::Object(map)) => Value::Object(map),
            Ok(_) => return err_response(400, "JSON body must be an object"),
            Err(_) => return err_response(400, "invalid JSON request body"),
        }
    };

    let (method, params) = match route {
        Route::List => ("routine_create", body),
        Route::Update(id) => {
            body["id"] = json!(id);
            ("routine_update", body)
        }
        Route::Approve(id) => {
            body["id"] = json!(id);
            ("routine_approve", body)
        }
        Route::Run(id) => {
            body["id"] = json!(id);
            ("routine_run", body)
        }
        Route::Pause(id) => {
            body["id"] = json!(id);
            ("routine_pause", body)
        }
        Route::Resume(id) => {
            body["id"] = json!(id);
            ("routine_resume", body)
        }
        Route::Remove(id) => {
            body["id"] = json!(id);
            ("routine_remove", body)
        }
        Route::ConnectionPolicy(id) => {
            body["connection_id"] = json!(id);
            ("connection_policy_set", body)
        }
        Route::ConnectionRequestContinue(id) => {
            body["request_id"] = json!(id);
            ("credential_request_continue", body)
        }
        Route::Show(_) => return err_response(405, "method not allowed"),
    };

    match client::rpc(state_dir, method, params) {
        Ok(out) => json_response(out),
        Err(e) => rpc_err(&e, method),
    }
}
