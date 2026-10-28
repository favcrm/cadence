//! Session-bound HTTP peers for mounted social draft actions. The child supplies
//! only action token/alias/intent data; this relay attaches the actual session.
use super::{
    app_screens, err_response, header_value, home, parse_multipart, read_body, HttpResp, ServeOpts,
};
use crate::client;
use serde_json::{json, Value};
use std::io::Write as _;
use tiny_http::{Method, Request};

pub(super) fn route(path: &str) -> Option<&'static str> {
    match path {
        "/api/app-social-drafts/create" => Some("app_social_draft_create"),
        "/api/app-social-drafts/list" => Some("app_social_draft_list"),
        "/api/app-social-drafts/show" => Some("app_social_draft_show"),
        "/api/app-social-drafts/update" => Some("app_social_draft_update"),
        "/api/app-social-drafts/discard" => Some("app_social_draft_discard"),
        "/api/app-social-drafts/asset" => Some("app_social_draft_asset"),
        "/api/app-social-drafts/image" => Some("app_social_image_upload"),
        "/api/app-social-drafts/uploaded" => Some("app_social_upload_asset"),
        "/api/app-social-drafts/sources/show" => Some("app_social_sources_show"),
        "/api/app-social-drafts/sources/save" => Some("app_social_sources_save"),
        "/api/app-social-drafts/effect-stage" => Some("app_effect_stage"),
        "/api/app-social-drafts/confirm" => Some("app_social_effect_confirm"),
        "/api/app-social-drafts/schedule" => Some("app_social_effect_schedule"),
        "/api/app-social-drafts/reschedule" => Some("app_social_effect_reschedule"),
        "/api/app-social-drafts/cancel" => Some("app_social_effect_cancel"),
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
    if method == "app_social_image_upload" {
        return upload_image(request, state_dir, opts);
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
        Ok(v) if method == "app_social_draft_asset" || method == "app_social_upload_asset" => {
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
                || crate::platform::agenticos_external::image::uploaded_image_mime(&bytes, &mime)
                    .is_err()
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

/// `POST /api/app-social-drafts/image` — the composer's own-image upload.
/// OperatorOnly in `WRITE_ROUTES`; the body is multipart with a `file` part
/// and only transport selectors (`action_token`, `alias`, `name`/`filename`).
/// The route proves the live session, stages the bytes under an exclusively
/// created path in the shared upload dir, relays `app_social_image_upload`
/// (the daemon re-proves mount/context/slot and normalises), and removes
/// this request's staging on every outcome.
fn upload_image(request: &mut Request, state_dir: &std::path::Path, opts: &ServeOpts) -> HttpResp {
    const IMAGE_CAP: usize = 2 * 1024 * 1024 + 64 * 1024;
    const UPLOAD_FIELDS: &[&str] = &["action_token", "alias", "name", "filename"];
    let Some((token, key, origin)) = app_screens::request_credentials(request, opts) else {
        return err_response(403, "image upload needs a live operator session");
    };
    let ct = header_value(request, "Content-Type").unwrap_or_default();
    let Some(boundary) = ct
        .trim()
        .strip_prefix("multipart/form-data; boundary=")
        .map(str::trim)
        .map(|b| b.trim_matches('"'))
    else {
        return err_response(400, "image upload needs a multipart/form-data boundary");
    };
    if boundary.is_empty() || boundary.len() > 100 {
        return err_response(400, "bad multipart boundary");
    }
    let body = match read_body(request, IMAGE_CAP as u64) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let (fields, file) = match parse_multipart(&body, boundary) {
        Ok(v) => v,
        Err(why) => return err_response(400, &why),
    };
    let Some(bytes) = file else {
        return err_response(400, "image upload needs a 'file' part");
    };
    if bytes.is_empty() || bytes.len() > IMAGE_CAP {
        return err_response(400, "image upload is outside the size bound");
    }
    if let Some(field) = fields.keys().find(|k| !UPLOAD_FIELDS.contains(&k.as_str())) {
        return err_response(
            400,
            &format!("image upload field '{field}' is not accepted"),
        );
    }
    let Some(action_token) = fields
        .get("action_token")
        .map(String::as_str)
        .filter(|v| !v.is_empty() && v.len() <= 256)
    else {
        return err_response(400, "image upload needs its action token");
    };
    let Some(alias) = fields
        .get("alias")
        .map(String::as_str)
        .filter(|v| !v.is_empty())
    else {
        return err_response(400, "image upload needs its tool alias");
    };
    // The alias is app vocabulary (dots allowed), never a route/path; the
    // daemon re-proves it against the live screen's declared tools.
    if alias.len() > 128 {
        return err_response(400, "bad image upload tool alias");
    }
    let name = fields
        .get("filename")
        .or_else(|| fields.get("name"))
        .cloned()
        .unwrap_or_else(|| "image.jpg".to_string());
    let uploads = state_dir.join(crate::wiki::UPLOAD_DIR);
    if let Err(e) = std::fs::create_dir_all(&uploads) {
        return err_response(500, &format!("image upload staging failed: {e}"));
    }
    let tmp = uploads.join(format!("app-social-{}", uuid::Uuid::new_v4().simple()));
    let mut staged = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
    {
        Ok(f) => f,
        Err(e) => return err_response(500, &format!("image upload staging failed: {e}")),
    };
    if let Err(e) = staged.write_all(&bytes).and_then(|()| staged.sync_all()) {
        drop(staged);
        let _ = std::fs::remove_file(&tmp);
        return err_response(500, &format!("image upload staging failed: {e}"));
    }
    drop(staged);
    let params = json!({
        "token": token,
        "key": key,
        "origin": origin,
        "action_token": action_token,
        "tool_alias": alias,
        "name": name,
        "tmp": tmp.to_string_lossy(),
    });
    let outcome = client::rpc(state_dir, "app_social_image_upload", params);
    let _ = std::fs::remove_file(&tmp);
    match outcome {
        Ok(v) => super::json_response(v),
        Err(e) => home::rpc_err(&e, "app_social_image_upload"),
    }
}
