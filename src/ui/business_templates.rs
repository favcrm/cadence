//! Board endpoints for company document templates (CAD-1393). Every
//! handler relays one daemon `business_template_*` or `business_document_*`
//! method over the board's own connection; the daemon owns scope, the
//! approval digest and every refusal.
//!
//! Writes are listed in `operator::WRITE_ROUTES` as OperatorOnly, so
//! `admit` has already run the write guards, the operator session and the
//! HTTP-peer operator proof before a handler here runs. A registered agent
//! reaches none of these routes; it uses the daemon RPC or the CLI, which
//! run the same backend rules.
//!
//! - `POST /api/templates/create`   `{name, docx_base64, spec}`
//! - `POST /api/templates/revise`   `{name, base, docx_base64, spec}`
//! - `POST /api/templates/approve`  `{name, version, digest, source}`
//! - `GET  /api/templates/show?name=&version=` (version optional)
//! - `POST /api/documents/create`   `{template, version, data}`
//! - `GET  /api/documents/show?id=`
//! - `GET  /api/documents/export?id=` (DOCX and PDF as base64; a preview)
//! - `GET  /api/templates/sample?name=&version=` (draft or approved version,
//!   labelled sample DOCX and PDF as base64; stores nothing)
//! - `POST /api/templates/intake`   `{file, source}` (reference extraction;
//!   read-only, DOCX or readable PDF as base64)
//!
//! Bodies deny unknown fields and carry no company or identity field: the
//! company comes from the connection and the actor from the daemon.

use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::Request;

use super::home::rpc_err;
use super::operator::Caller;
use super::{err_response, json_response, parse_json, pct_decode, read_body, HttpResp};
use crate::client;

/// The base64 DOCX plus JSON envelope, a little above the 10 MiB source cap.
const BODY_CAP: u64 = 14 * 1024 * 1024;

pub(super) enum Route {
    Create,
    Revise,
    Approve,
    Show,
    Sample,
    Intake,
    DocumentCreate,
    DocumentShow,
    DocumentExport,
}

/// Whether a route is a write (create, revise, approve, document create).
pub(super) fn is_write(route: &Route) -> bool {
    matches!(
        route,
        Route::Create | Route::Revise | Route::Approve | Route::DocumentCreate
    )
}

pub(super) fn route(path: &str) -> Option<Route> {
    match path {
        "/api/templates/create" => Some(Route::Create),
        "/api/templates/revise" => Some(Route::Revise),
        "/api/templates/approve" => Some(Route::Approve),
        "/api/templates/show" => Some(Route::Show),
        "/api/templates/sample" => Some(Route::Sample),
        "/api/templates/intake" => Some(Route::Intake),
        "/api/documents/create" => Some(Route::DocumentCreate),
        "/api/documents/show" => Some(Route::DocumentShow),
        "/api/documents/export" => Some(Route::DocumentExport),
        _ => None,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateReq {
    name: String,
    docx_base64: String,
    spec: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviseReq {
    name: String,
    base: i64,
    docx_base64: String,
    spec: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveReq {
    name: String,
    version: i64,
    digest: String,
    source: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IntakeReq {
    file: String,
    source: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentReq {
    template: String,
    version: i64,
    data: Value,
}

/// Write handlers (create, revise, approve, document create). Each is
/// reached only after `admit` proved the operator on the HTTP peer.
pub(super) fn handle_write(
    request: &mut Request,
    state_dir: &std::path::Path,
    caller: &Caller,
    route: Route,
) -> HttpResp {
    if !matches!(caller, Caller::Operator(_) | Caller::Named(_)) {
        return err_response(403, "template writes need the operator");
    }
    let bytes = match read_body(request, BODY_CAP) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    let (method, params) = match route {
        Route::Create => {
            let req: CreateReq = match parse_json(&bytes) {
                Ok(req) => req,
                Err(resp) => return resp,
            };
            (
                "business_template_create",
                json!({"name": req.name, "source": req.docx_base64, "spec": req.spec}),
            )
        }
        Route::Revise => {
            let req: ReviseReq = match parse_json(&bytes) {
                Ok(req) => req,
                Err(resp) => return resp,
            };
            (
                "business_template_revise",
                json!({"name": req.name, "base": req.base, "source": req.docx_base64, "spec": req.spec}),
            )
        }
        Route::Approve => {
            let req: ApproveReq = match parse_json(&bytes) {
                Ok(req) => req,
                Err(resp) => return resp,
            };
            (
                "business_template_approve",
                json!({"name": req.name, "version": req.version, "digest": req.digest, "source": req.source}),
            )
        }
        Route::DocumentCreate => {
            let req: DocumentReq = match parse_json(&bytes) {
                Ok(req) => req,
                Err(resp) => return resp,
            };
            (
                "business_document_create",
                json!({"template": req.template, "version": req.version, "data": req.data}),
            )
        }
        Route::Intake => {
            let req: IntakeReq = match parse_json(&bytes) {
                Ok(req) => req,
                Err(resp) => return resp,
            };
            (
                "business_template_intake",
                json!({"file": req.file, "source": req.source}),
            )
        }
        Route::Show | Route::DocumentShow | Route::DocumentExport | Route::Sample => {
            return err_response(405, "method not allowed");
        }
    };
    match client::rpc(state_dir, method, params) {
        Ok(out) => json_response(out),
        Err(e) => rpc_err(&e, method),
    }
}

/// Operator-only reads (show), admitted by `admit_operator_read_caller`.
pub(super) fn handle_read(state_dir: &std::path::Path, route: Route, raw_query: &str) -> HttpResp {
    let mut name = None;
    let mut version = None;
    let mut id = None;
    for pair in raw_query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let Some(value) = pct_decode(value) else {
            return err_response(400, "query value is not valid");
        };
        let slot = match key {
            "name" => &mut name,
            "version" => &mut version,
            "id" => &mut id,
            _ => return err_response(400, "unknown query key"),
        };
        if slot.is_some() {
            return err_response(400, "each query key is given once");
        }
        *slot = Some(value);
    }
    match route {
        Route::Show => {
            let Some(name) = name else {
                return err_response(400, "name is required");
            };
            let mut params = json!({"name": name});
            if let Some(version) = version {
                let Some(version) = version.parse::<i64>().ok().filter(|v| *v > 0) else {
                    return err_response(400, "version must be a positive integer");
                };
                params["version"] = json!(version);
            }
            match client::rpc(state_dir, "business_template_show", params) {
                Ok(out) => json_response(out),
                Err(e) => rpc_err(&e, "business_template_show"),
            }
        }
        Route::DocumentShow => {
            let Some(id) = id else {
                return err_response(400, "id is required");
            };
            match client::rpc(state_dir, "business_document_show", json!({"id": id})) {
                Ok(out) => json_response(out),
                Err(e) => rpc_err(&e, "business_document_show"),
            }
        }
        Route::DocumentExport => {
            let Some(id) = id else {
                return err_response(400, "id is required");
            };
            match client::rpc(state_dir, "business_document_export", json!({"id": id})) {
                Ok(out) => json_response(out),
                Err(e) => rpc_err(&e, "business_document_export"),
            }
        }
        Route::Sample => {
            let (Some(name), Some(version)) = (name, version) else {
                return err_response(400, "name and version are required");
            };
            let Some(version) = version.parse::<i64>().ok().filter(|v| *v > 0) else {
                return err_response(400, "version must be a positive integer");
            };
            match client::rpc(
                state_dir,
                "business_template_sample",
                json!({"name": name, "version": version}),
            ) {
                Ok(out) => json_response(out),
                Err(e) => rpc_err(&e, "business_template_sample"),
            }
        }
        _ => err_response(405, "method not allowed"),
    }
}
