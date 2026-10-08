//! Operator-only HTTP relay for generic app assistant operations.
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use tiny_http::{Header, Request};

use super::{err_response, json_response, read_body, HttpResp};
use crate::{
    client,
    error::Error,
    store::app_records::{record_db_path, RecordStore},
};

const BODY_CAP: u64 = 16 * 1024;
#[derive(Clone, Copy)]
pub(super) enum Route<'a> {
    Actions(&'a str),
    Operations(&'a str),
    Operation(&'a str, &'a str),
    Decision(&'a str, &'a str),
    Permissions(&'a str),
    Revoke(&'a str, &'a str),
    Block(&'a str),
}
impl Route<'_> {
    pub(super) fn is_read(self) -> bool {
        matches!(
            self,
            Self::Actions(_) | Self::Operations(_) | Self::Operation(_, _) | Self::Permissions(_)
        )
    }
}
fn ident(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
pub(super) fn route(path: &str) -> Option<Route<'_>> {
    let tail = path.strip_prefix("/api/app-installations/")?;
    let mut parts = tail.split('/');
    let install = parts.next()?;
    if !ident(install) || parts.next()? != "assistant" {
        return None;
    }
    let group = parts.next()?;
    let route = match (group, parts.next(), parts.next()) {
        ("actions", None, None) => Route::Actions(install),
        ("operations", None, None) => Route::Operations(install),
        ("operations", Some(id), None) if ident(id) => Route::Operation(install, id),
        ("operations", Some(id), Some("decision")) if ident(id) => Route::Decision(install, id),
        ("permissions", None, None) => Route::Permissions(install),
        ("permissions", Some("block"), None) => Route::Block(install),
        ("permissions", Some(id), Some("revoke")) if ident(id) => Route::Revoke(install, id),
        _ => return None,
    };
    parts.next().is_none().then_some(route)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    decision: String,
    expected_revision: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Revision {
    expected_revision: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Block {
    action_id: String,
    resource_id: String,
}
fn body<T: serde::de::DeserializeOwned>(request: &mut Request) -> Result<T, HttpResp> {
    let bytes = read_body(request, BODY_CAP)?;
    serde_json::from_slice(&bytes)
        .map_err(|_| err_response(400, "invalid app assistant request schema"))
}
fn stored_context(
    state: &Path,
    install: &str,
    operation: bool,
    id: &str,
) -> Result<String, HttpResp> {
    let path = record_db_path(state, install)
        .map_err(|_| err_response(404, "assistant receipt unavailable"))?;
    if !path.is_file() {
        return Err(err_response(404, "assistant receipt unavailable"));
    }
    let records = RecordStore::open(state, install)
        .map_err(|_| err_response(404, "assistant receipt unavailable"))?;
    if operation {
        records.app_assistant_operation_context(id)
    } else {
        records.app_assistant_permission_context(id)
    }
    .map_err(|_| err_response(404, "assistant receipt unavailable"))?
    .ok_or_else(|| err_response(404, "assistant receipt unavailable"))
}
fn rpc_error(error: Error) -> HttpResp {
    // Busy is transient: 503 + Retry-After, with a fixed message (the
    // daemon's lock detail stays out of this surface).
    if error.kind() == "busy" {
        if let Some(resp) = super::busy_response(&Error::busy("app assistant is busy; retry")) {
            return resp;
        }
    }
    let text = error.to_string();
    let status = if text.contains("operator") || text.contains("caller") || text.contains("session")
    {
        403
    } else if matches!(error, Error::Rejected(_) | Error::Structured(_)) {
        409
    } else {
        503
    };
    err_response(status, "app assistant operation refused or unavailable")
}
fn call(
    state: &Path,
    install: &str,
    context: &str,
    method: &str,
    mut params: serde_json::Value,
) -> HttpResp {
    params["install_id"] = json!(install);
    params["context_id"] = json!(context);
    match client::rpc(state, method, params) {
        Ok(value) => {
            let mut response = json_response(value);
            response.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
            response.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
            response
        }
        Err(error) => rpc_error(error),
    }
}
fn query_context(query: &str) -> Result<String, HttpResp> {
    let mut found = None;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(err_response(400, "invalid app assistant query"));
        };
        if key != "context_id" || found.is_some() {
            return Err(err_response(400, "unsupported app assistant query"));
        }
        if !ident(value) {
            return Err(err_response(400, "invalid app assistant context"));
        }
        found = Some(value.to_owned());
    }
    found.ok_or_else(|| err_response(400, "context_id is required"))
}
pub(super) fn handle(
    request: &mut Request,
    state: &Path,
    route: Route<'_>,
    query: &str,
) -> HttpResp {
    match route {
        Route::Actions(install) | Route::Operations(install) | Route::Permissions(install) => {
            if request.method().as_str() != "GET" {
                return err_response(405, "method not allowed");
            }
            let context = match query_context(query) {
                Ok(context) => context,
                Err(response) => return response,
            };
            let method = match route {
                Route::Actions(_) => "app_assistant_actions_operator",
                Route::Operations(_) => "app_assistant_operations",
                _ => "app_assistant_permissions",
            };
            call(state, install, &context, method, json!({}))
        }
        Route::Operation(install, id) => {
            if request.method().as_str() != "GET" {
                return err_response(405, "method not allowed");
            }
            match stored_context(state, install, true, id) {
                Ok(context) => call(
                    state,
                    install,
                    &context,
                    "app_assistant_operation_operator_show",
                    json!({"operation_id":id}),
                ),
                Err(response) => response,
            }
        }
        Route::Decision(install, id) => {
            if request.method().as_str() != "POST" {
                return err_response(405, "method not allowed");
            }
            let value: Decision = match body(request) {
                Ok(value) => value,
                Err(response) => return response,
            };
            match stored_context(state, install, true, id) {
                Ok(context) => call(
                    state,
                    install,
                    &context,
                    "app_assistant_decision",
                    json!({"operation_id":id,"decision":value.decision,"expected_revision":value.expected_revision}),
                ),
                Err(response) => response,
            }
        }
        Route::Revoke(install, id) => {
            if request.method().as_str() != "POST" {
                return err_response(405, "method not allowed");
            }
            let value: Revision = match body(request) {
                Ok(value) => value,
                Err(response) => return response,
            };
            match stored_context(state, install, false, id) {
                Ok(context) => call(
                    state,
                    install,
                    &context,
                    "app_assistant_permission_revoke",
                    json!({"permission_id":id,"expected_revision":value.expected_revision}),
                ),
                Err(response) => response,
            }
        }
        Route::Block(install) => {
            if request.method().as_str() != "POST" {
                return err_response(405, "method not allowed");
            }
            let value: Block = match body(request) {
                Ok(value) => value,
                Err(response) => return response,
            };
            let context = match query_context(query) {
                Ok(context) => context,
                Err(response) => return response,
            };
            call(
                state,
                install,
                &context,
                "app_assistant_permission_block",
                json!({"action_id":value.action_id,"resource_id":value.resource_id}),
            )
        }
    }
}

#[cfg(test)]
pub(super) fn busy_status_for_test(e: &Error) -> u16 {
    rpc_error(Error::busy(e.to_string())).status_code().0
}
