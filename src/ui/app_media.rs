//! CAD-1365: the board's media relay for a live mounted screen —
//! `POST /api/app-media/read` → `app_media_read`, the same tool-relay
//! idiom [`super::app_screens::tool_invoke`] runs for `app_tool_invoke`.
//!
//! Authority is the request's own proven session (the
//! `cadence_operator_{port}` cookie token + `X-Cadence-Session` key +
//! classified origin — the same `check_session` proof the screen mint
//! uses), relayed to the daemon's native check. The route is
//! `RouteClass::OperatorOnly` (an unlisted write fails closed to it);
//! the daemon re-proves the session AND the server-bound mount action
//! context — live digest, approval, active context and the mounted
//! tag's declared `media.read` permission — before any catalog work.
//!
//! The body is CLOSED: exactly `{action_token, input}`. Credentials,
//! identity or scope selectors (`token`, `key`, `origin`, `workspace`,
//! `creator`, `caller`, …) in the body are refused 400 before any
//! daemon call — the request's classified session is the only
//! credential this route relays. The frame itself never reaches this
//! route (`connect-src 'none'`); the host-held action token is relayed
//! only by the native parent. Daemon refusals map to their normal
//! non-2xx reply — this handler never fabricates success. Catalog reads
//! and bounded local previews perform no provider fetch or generation.
//! Publish, chat handoff and original download have separate relays below.

use serde_json::Value;
use tiny_http::Request;

use super::{app_screens, err_response, home, read_body, HttpResp, ServeOpts};
use crate::client;

/// The media-read body is two small fields — the same cap the sibling
/// tool invoke uses.
const BODY_CAP: u64 = 4 * 1024;

/// `POST /api/app-media/read` → `true` for exactly that path.
pub(super) fn read_route(path: &str) -> bool {
    path == "/api/app-media/read"
}

pub(super) fn chat_attach_route(path: &str) -> bool {
    path == "/api/app-media/chat-attach"
}

/// `POST /api/app-media/publish` → `true` for exactly that path.
pub(super) fn publish_route(path: &str) -> bool {
    path == "/api/app-media/publish"
}

/// `POST /api/app-media/original` → `true` for exactly that path.
pub(super) fn original_route(path: &str) -> bool {
    path == "/api/app-media/original"
}

/// The original-download relay. Body `{action_token, asset_id}` only —
/// any other key (a credential, identity, ref, url or scope selector)
/// refuses 400 before the daemon sees it. The session fields come from
/// the request's own classified credential, never the body. The daemon
/// re-proves everything and returns the verified original bytes, which
/// are served as an attachment under the verified allowlisted image
/// MIME and a filename derived from the native asset id — never a
/// path, ref, token or URL. Errors stay ordinary JSON.
pub(super) fn media_original(
    request: &mut Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
) -> HttpResp {
    let bytes = match read_body(request, BODY_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let body: Value = match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) if v.is_object() => v,
        _ => return err_response(400, "media original body must be a JSON object"),
    };
    if body.as_object().is_some_and(|o| {
        o.keys()
            .any(|k| !matches!(k.as_str(), "action_token" | "asset_id"))
    }) {
        return err_response(400, "media original body contains unsupported fields");
    }
    let action_token = body
        .get("action_token")
        .and_then(Value::as_str)
        .unwrap_or("");
    let asset_id = body.get("asset_id").and_then(Value::as_str).unwrap_or("");
    if action_token.is_empty() || asset_id.is_empty() {
        return err_response(400, "media original needs action_token and asset_id");
    }
    let Some((token, key, origin)) = app_screens::request_credentials(request, opts) else {
        return err_response(403, "media original needs a live operator session");
    };
    match client::rpc_app_media_original(
        state_dir,
        serde_json::json!({
            "action_token": action_token,
            "token": token,
            "key": key,
            "origin": origin,
            "asset_id": asset_id,
        }),
    ) {
        Ok((header, bytes)) => {
            let media_type = header["media_type"].as_str().unwrap_or("");
            let ext = match media_type {
                "image/png" => "png",
                "image/jpeg" => "jpg",
                "image/webp" => "webp",
                _ => {
                    return err_response(500, "daemon original MIME is not allowlisted");
                }
            };
            let mut resp = HttpResp::from_data(bytes);
            resp.add_header(tiny_http::Header::from_bytes("Content-Type", media_type).unwrap());
            resp.add_header(
                tiny_http::Header::from_bytes(
                    "Content-Disposition",
                    format!("attachment; filename=\"{asset_id}.{ext}\""),
                )
                .unwrap(),
            );
            resp.add_header(tiny_http::Header::from_bytes("Cache-Control", "no-store").unwrap());
            resp.add_header(
                tiny_http::Header::from_bytes(
                    "Content-Security-Policy",
                    "sandbox; default-src 'none'",
                )
                .unwrap(),
            );
            resp
        }
        Err(e) => home::rpc_err(&e, "app_media_original"),
    }
}

/// The media-publish relay. Body `{action_token, asset_id,
/// expected_revision, request_id}` only — any other key refuses before
/// the daemon sees it. The session fields are added host-side from the
/// proven request, never from the body.
pub(super) fn media_publish(
    request: &mut Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
) -> HttpResp {
    let bytes = match read_body(request, BODY_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let body: Value = match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) if v.is_object() => v,
        _ => return err_response(400, "media publish body must be a JSON object"),
    };
    // Closed body shape the daemon re-checks; a body-supplied
    // credential, caller or scope selector is refused outright — the
    // proven request session is the only authority relayed.
    if body.as_object().is_some_and(|o| {
        o.keys().any(|k| {
            !matches!(
                k.as_str(),
                "action_token" | "asset_id" | "expected_revision" | "request_id"
            )
        })
    }) {
        return err_response(400, "media publish body contains unsupported fields");
    }
    let action_token = body
        .get("action_token")
        .and_then(Value::as_str)
        .unwrap_or("");
    let asset_id = body.get("asset_id").and_then(Value::as_str).unwrap_or("");
    let expected_revision = body
        .get("expected_revision")
        .cloned()
        .unwrap_or(Value::Null);
    let request_id = body.get("request_id").and_then(Value::as_str).unwrap_or("");
    if action_token.is_empty()
        || asset_id.is_empty()
        || !expected_revision.is_i64()
        || request_id.is_empty()
    {
        return err_response(
            400,
            "media publish needs action_token, asset_id, expected_revision and request_id",
        );
    }
    let Some((token, key, origin)) = app_screens::request_credentials(request, opts) else {
        return err_response(403, "media publish needs a live operator session");
    };
    match client::rpc(
        state_dir,
        "app_media_publish",
        serde_json::json!({
            "action_token": action_token,
            "token": token,
            "key": key,
            "origin": origin,
            "asset_id": asset_id,
            "expected_revision": expected_revision,
            "request_id": request_id,
        }),
    ) {
        Ok(v) => super::json_response(v),
        Err(e) => home::rpc_err(&e, "app_media_publish"),
    }
}

/// The media-read relay. Body `{action_token, input}` only — any other
/// key refuses before the daemon sees it. The session fields are added
/// host-side from the proven request, never from the body.
pub(super) fn media_read(
    request: &mut Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
) -> HttpResp {
    let bytes = match read_body(request, BODY_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let body: Value = match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) if v.is_object() => v,
        _ => return err_response(400, "media read body must be a JSON object"),
    };
    // Closed body shape the daemon re-checks; a body-supplied
    // credential, caller or scope selector is refused outright — the
    // proven request session is the only authority relayed.
    if body.as_object().is_some_and(|o| {
        o.keys()
            .any(|k| !matches!(k.as_str(), "action_token" | "input"))
    }) {
        return err_response(400, "media read body contains unsupported fields");
    }
    let action_token = body
        .get("action_token")
        .and_then(Value::as_str)
        .unwrap_or("");
    let input = body.get("input").cloned().unwrap_or(Value::Null);
    if action_token.is_empty() || !input.is_object() {
        return err_response(400, "media read needs action_token and object input");
    }
    let Some((token, key, origin)) = app_screens::request_credentials(request, opts) else {
        return err_response(403, "media read needs a live operator session");
    };
    match client::rpc(
        state_dir,
        "app_media_read",
        serde_json::json!({
            "action_token": action_token,
            "token": token,
            "key": key,
            "origin": origin,
            "input": input,
        }),
    ) {
        Ok(v) => super::json_response(v),
        Err(e) => home::rpc_err(&e, "app_media_read"),
    }
}

pub(super) fn media_chat_attach(
    request: &mut Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
) -> HttpResp {
    let bytes = match read_body(request, BODY_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let body: Value = match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) if v.is_object() => v,
        _ => return err_response(400, "media chat attach body must be a JSON object"),
    };
    if body.as_object().is_some_and(|o| {
        o.keys().any(|k| {
            !matches!(
                k.as_str(),
                "action_token" | "asset_id" | "expected_revision" | "conversation_id"
            )
        })
    }) {
        return err_response(400, "media chat attach body contains unsupported fields");
    }
    let action_token = body
        .get("action_token")
        .and_then(Value::as_str)
        .unwrap_or("");
    let asset_id = body.get("asset_id").and_then(Value::as_str).unwrap_or("");
    let revision = body.get("expected_revision").and_then(Value::as_i64);
    let conversation_id = body.get("conversation_id");
    if action_token.is_empty()
        || asset_id.is_empty()
        || !revision.is_some_and(|v| v > 0)
        || !conversation_id.is_some_and(|v| {
            v.is_null()
                || v.as_str()
                    .is_some_and(|id| !id.is_empty() && id.len() <= 128)
        })
    {
        return err_response(400, "media chat attach needs action_token, asset_id, expected_revision and a host-selected conversation_id");
    }
    let Some((token, key, origin)) = app_screens::request_credentials(request, opts) else {
        return err_response(403, "media chat attach needs a live operator session");
    };
    match client::rpc(
        state_dir,
        "app_media_chat_attach",
        serde_json::json!({
            "action_token": action_token,
            "token": token,
            "key": key,
            "origin": origin,
            "asset_id": asset_id,
            "expected_revision": revision,
            "conversation_id": conversation_id,
        }),
    ) {
        Ok(v) => super::json_response(v),
        Err(e) => home::rpc_err(&e, "app_media_chat_attach"),
    }
}
