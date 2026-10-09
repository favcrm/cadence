//! Session-bound HTTP peers for mounted social draft actions. The child supplies
//! only action token/alias/intent data; this relay attaches the actual session.
use super::{app_screens, err_response, home, read_body, HttpResp, ServeOpts};
use crate::client;
use serde_json::{json, Value};
use tiny_http::{Method, Request};

pub(super) fn route(path: &str) -> Option<&'static str> {
    match path {
        "/api/app-social-drafts/create" => Some("app_social_draft_create"),
        "/api/app-social-drafts/list" => Some("app_social_draft_list"),
        "/api/app-social-drafts/show" => Some("app_social_draft_show"),
        "/api/app-social-drafts/update" => Some("app_social_draft_update"),
        "/api/app-social-drafts/discard" => Some("app_social_draft_discard"),
        "/api/app-social-drafts/asset" => Some("app_social_draft_asset"),
        "/api/app-social-drafts/sources/show" => Some("app_social_sources_show"),
        "/api/app-social-drafts/sources/save" => Some("app_social_sources_save"),
        "/api/app-social-drafts/effect-stage" => Some("app_effect_stage"),
        _ => None,
    }
}
pub(super) fn handle(
    request: &mut Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    method: &str,
) -> HttpResp {
    if *request.method() != Method::Post {
        return err_response(405, "method not allowed");
    }
    let bytes = match read_body(request, 64 * 1024) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let mut body: Value = match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) if v.is_object() => v,
        _ => return err_response(400, "social draft request must be an object"),
    };
    let Some((token, key, origin)) = app_screens::request_credentials(request, opts) else {
        return err_response(403, "social draft action needs a live operator session");
    };
    let Some(object) = body.as_object_mut() else {
        return err_response(400, "social draft request must be an object");
    };
    if object.keys().any(|k| {
        matches!(
            k.as_str(),
            "token" | "key" | "origin" | "install_id" | "context_id" | "tenant_id" | "actor"
        )
    }) {
        return err_response(
            400,
            "social draft request contains host-owned authority fields",
        );
    }
    object.insert("token".into(), json!(token));
    object.insert("key".into(), json!(key));
    object.insert("origin".into(), json!(origin));
    match client::rpc(state_dir, method, body) {
        Ok(v) if method == "app_social_draft_asset" => {
            let install = v["install_id"].as_str().unwrap_or("");
            let asset = v["asset_id"].as_str().unwrap_or("");
            let db = match rusqlite::Connection::open_with_flags(
                state_dir.join("cadence.sqlite3"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            ) {
                Ok(db) => db,
                Err(_) => return err_response(502, "social draft asset custody refused"),
            };
            let read = db.query_row(
                "SELECT asset_type,asset_digest,asset FROM app_tool_results WHERE id=? AND install_id=?",
                rusqlite::params![asset,install],
                |r| Ok((r.get::<_,Option<String>>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,Option<Vec<u8>>>(2)?)),
            );
            let (Some(mime), Some(digest), Some(bytes)) = (match read {
                Ok(row) => row,
                Err(_) => return err_response(404, "social draft image unavailable"),
            }) else {
                return err_response(404, "social draft image unavailable");
            };
            if !matches!(mime.as_str(), "image/jpeg" | "image/png")
                || bytes.is_empty()
                || bytes.len() > 2 * 1024 * 1024
                || crate::store::app_runs::artifact_digest(&bytes) != digest
                || v["digest"] != digest
                || v["mime"] != mime
                || v["size_bytes"] != bytes.len()
                || image::load_from_memory(&bytes).is_err()
            {
                return err_response(502, "social draft asset custody refused");
            }
            let mut response =
                tiny_http::Response::from_data(bytes).with_status_code(tiny_http::StatusCode(200));
            response.add_header(tiny_http::Header::from_bytes("Content-Type", mime).unwrap());
            response
                .add_header(tiny_http::Header::from_bytes("Cache-Control", "no-store").unwrap());
            response.add_header(
                tiny_http::Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap(),
            );
            response
        }
        Ok(v) => super::json_response(v),
        Err(e) => home::rpc_err(&e, method),
    }
}
