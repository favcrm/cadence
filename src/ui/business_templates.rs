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
//! - `POST /api/templates/create`   `{name, html | html_base64 | docx_base64, spec}`
//! - `POST /api/templates/revise`   `{name, base, html | html_base64 | docx_base64, spec}`
//! - `POST /api/templates/approve`  `{name, version, digest, source}`
//! - `GET  /api/templates/show?name=&version=` (version optional)
//! - `POST /api/documents/create`   `{template, version, data, allow_partial?}`
//! - `GET  /api/documents/show?id=`
//! - `GET|POST /api/documents/export` (`?id=&format=` or `{id, format?}`)
//! - `GET  /api/templates/sample?name=&version=&format=` (draft or approved version,
//!   labelled sample HTML, DOCX, or PDF; stores nothing)
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
    List,
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
        "/api/templates/list" => Some(Route::List),
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
struct TemplateWriteReq {
    name: String,
    #[serde(default)]
    base: Option<i64>,
    #[serde(default)]
    docx_base64: Option<String>,
    #[serde(default)]
    html: Option<String>,
    #[serde(default)]
    html_base64: Option<String>,
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
    #[serde(default)]
    version: Option<i64>,
    data: Value,
    #[serde(default)]
    allow_partial: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentExportReq {
    id: String,
    #[serde(default)]
    format: Option<String>,
}

fn attach_template_source(
    params: &mut Value,
    spec: &Value,
    docx_base64: Option<String>,
    html: Option<String>,
    html_base64: Option<String>,
) -> std::result::Result<(), HttpResp> {
    let authoring = spec.get("kind").and_then(Value::as_str) == Some("authoring");
    if !authoring && docx_base64.is_none() && html.is_none() && html_base64.is_none() {
        return Err(err_response(
            400,
            "template source (html or docx_base64) is required",
        ));
    }
    if let Some(docx) = docx_base64 {
        params["source"] = json!(docx);
    }
    if let Some(html) = html {
        params["html"] = json!(html);
    }
    if let Some(html_b64) = html_base64 {
        params["html_base64"] = json!(html_b64);
    }
    Ok(())
}

/// Write handlers (create, revise, approve, document create/export, intake).
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
        Route::Create | Route::Revise => {
            let req: TemplateWriteReq = match parse_json(&bytes) {
                Ok(req) => req,
                Err(resp) => return resp,
            };
            let is_rev = matches!(route, Route::Revise);
            let mut params = json!({"name": req.name, "spec": req.spec});
            if is_rev {
                let Some(b) = req.base else {
                    return err_response(400, "base is required");
                };
                params["base"] = json!(b);
            } else if req.base.is_some() {
                return err_response(400, "unknown field `base`");
            }
            if let Err(resp) = attach_template_source(
                &mut params,
                &req.spec,
                req.docx_base64,
                req.html,
                req.html_base64,
            ) {
                return resp;
            }
            (
                if is_rev {
                    "business_template_revise"
                } else {
                    "business_template_create"
                },
                params,
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
            let mut params = json!({"template": req.template, "data": req.data, "allow_partial": req.allow_partial});
            if let Some(version) = req.version {
                params["version"] = json!(version);
            }
            ("business_document_create", params)
        }
        Route::DocumentExport => {
            let req: DocumentExportReq = match parse_json(&bytes) {
                Ok(req) => req,
                Err(resp) => return resp,
            };
            let mut params = json!({"id": req.id});
            if let Some(format) = req.format {
                params["format"] = json!(format);
            }
            ("business_document_export", params)
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
        Route::List | Route::Show | Route::DocumentShow | Route::Sample => {
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
    let mut format = None;
    for pair in raw_query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let Some(value) = pct_decode(value) else {
            return err_response(400, "query value is not valid");
        };
        let slot = match key {
            "name" => &mut name,
            "version" => &mut version,
            "id" => &mut id,
            "format" => &mut format,
            _ => return err_response(400, "unknown query key"),
        };
        if slot.is_some() {
            return err_response(400, "each query key is given once");
        }
        *slot = Some(value);
    }
    match route {
        Route::List => match client::rpc(state_dir, "business_template_list", json!({})) {
            Ok(out) => json_response(out),
            Err(e) => rpc_err(&e, "business_template_list"),
        },
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
            let mut params = json!({"id": id});
            if let Some(format) = format {
                params["format"] = json!(format);
            }
            match client::rpc(state_dir, "business_document_export", params) {
                Ok(out) => json_response(out),
                Err(e) => rpc_err(&e, "business_document_export"),
            }
        }
        Route::Sample => {
            let Some(name) = name else {
                return err_response(400, "name is required");
            };
            let mut params = json!({"name": name});
            if let Some(version) = version.filter(|v| !v.is_empty()) {
                let Some(version) = version.parse::<i64>().ok().filter(|v| *v > 0) else {
                    return err_response(400, "version must be a positive integer");
                };
                params["version"] = json!(version);
            }
            if let Some(format) = format {
                params["format"] = json!(format);
            }
            match client::rpc(state_dir, "business_template_sample", params) {
                Ok(out) => json_response(out),
                Err(e) => rpc_err(&e, "business_template_sample"),
            }
        }
        _ => err_response(405, "method not allowed"),
    }
}
