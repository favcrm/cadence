//! Campaign-send relays (CAD-786): the board forwards
//! `crm_send_prepare|approve|show|list|resolve` to the daemon behind
//! the same operator proof every write takes, and serves the
//! recipient's unsubscribe page at `/unsubscribe/<token>` — a route
//! where the token itself is the only credential, so it sits outside
//! the board's session gate entirely.
//!
//! Body schemas are strict: `deny_unknown_fields` refuses smuggled
//! fields before the daemon's own allowlist sees them, and the relay
//! rebuilds params from typed fields — the request can never claim
//! identity, routing or secret material.
use super::{err_response, json_response, read_body, HttpResp};
use crate::client;
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use tiny_http::{Header, Request, Response};

const BODY_CAP: u64 = 48 * 1024;

/// Relayed send verbs; `Prepare`/`Approve`/`Resolve` are writes,
/// `Show`/`List` are GET reads.
#[derive(Clone, Copy)]
pub(super) enum Route {
    Prepare,
    Approve,
    Resolve,
    Show,
    List,
    /// `POST /api/crm-send/origin` sets/clears the unsubscribe
    /// origin; `GET /api/crm-send/origin` reads it.
    Origin,
}

pub(super) fn route(path: &str) -> Option<Route> {
    match path {
        "/api/crm-send/prepare" => Some(Route::Prepare),
        "/api/crm-send/approve" => Some(Route::Approve),
        "/api/crm-send/resolve" => Some(Route::Resolve),
        "/api/crm-send/show" => Some(Route::Show),
        "/api/crm-send/list" => Some(Route::List),
        "/api/crm-send/origin" => Some(Route::Origin),
        _ => None,
    }
}

impl Route {
    pub(super) fn is_read(self) -> bool {
        matches!(self, Self::Show | Self::List | Self::Origin)
    }

    /// `Origin` is read over GET, written over POST — dispatch
    /// splits by method, so the method selects the meaning.
    pub(super) fn is_origin(self) -> bool {
        matches!(self, Self::Origin)
    }
}

/// POST bodies carry exactly the verb's allowlist — unknown fields
/// are refused by serde before the daemon sees them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareBody {
    install_id: String,
    context_id: String,
    campaign_id: String,
    audience_freeze_id: String,
    request_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveBody {
    install_id: String,
    context_id: String,
    send_id: String,
    send_digest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveBody {
    install_id: String,
    context_id: String,
    send_id: String,
    customer_id: String,
    resolution: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginBody {
    unsubscribe_origin: serde_json::Value,
}

fn relay(state_dir: &Path, method: &str, params: serde_json::Value) -> HttpResp {
    match client::rpc(state_dir, method, params) {
        Ok(out) => json_response(out),
        Err(e) => err_response(400, &e.to_string()),
    }
}

pub(super) fn handle_write(request: &mut Request, state_dir: &Path, route: Route) -> HttpResp {
    let body = match read_body(request, BODY_CAP) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    match route {
        Route::Prepare => {
            let parsed: PrepareBody = match super::parse_json(&body) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            relay(
                state_dir,
                "crm_send_prepare",
                json!({
                    "install_id": parsed.install_id,
                    "context_id": parsed.context_id,
                    "campaign_id": parsed.campaign_id,
                    "audience_freeze_id": parsed.audience_freeze_id,
                    "request_id": parsed.request_id,
                }),
            )
        }
        Route::Approve => {
            let parsed: ApproveBody = match super::parse_json(&body) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            relay(
                state_dir,
                "crm_send_approve",
                json!({
                    "install_id": parsed.install_id,
                    "context_id": parsed.context_id,
                    "send_id": parsed.send_id,
                    "send_digest": parsed.send_digest,
                }),
            )
        }
        Route::Resolve => {
            let parsed: ResolveBody = match super::parse_json(&body) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            relay(
                state_dir,
                "crm_send_resolve",
                json!({
                    "install_id": parsed.install_id,
                    "context_id": parsed.context_id,
                    "send_id": parsed.send_id,
                    "customer_id": parsed.customer_id,
                    "resolution": parsed.resolution,
                }),
            )
        }
        Route::Show | Route::List => err_response(405, "method not allowed"),
        Route::Origin => {
            let parsed: OriginBody = match super::parse_json(&body) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            relay(
                state_dir,
                "crm_send_origin_set",
                json!({"unsubscribe_origin": parsed.unsubscribe_origin}),
            )
        }
    }
}

pub(super) fn handle_read(
    state_dir: &Path,
    route: Route,
    query: &dyn Fn(&str) -> Option<String>,
) -> HttpResp {
    match route {
        Route::Show => {
            let (Some(install), Some(context), Some(send_id)) =
                (query("install_id"), query("context_id"), query("send_id"))
            else {
                return err_response(400, "install_id, context_id and send_id are required");
            };
            relay(
                state_dir,
                "crm_send_show",
                json!({
                    "install_id": install,
                    "context_id": context,
                    "send_id": send_id,
                }),
            )
        }
        Route::List => {
            let (Some(install), Some(context)) = (query("install_id"), query("context_id")) else {
                return err_response(400, "install_id and context_id are required");
            };
            let mut params = json!({"install_id": install, "context_id": context});
            if let Some(campaign) = query("campaign_id") {
                params["campaign_id"] = json!(campaign);
            }
            relay(state_dir, "crm_send_list", params)
        }
        Route::Origin => relay(state_dir, "crm_send_origin_show", json!({})),
        _ => err_response(405, "method not allowed"),
    }
}

/// The token portion of `/unsubscribe/<token>` — the exact 43-char
/// base64url shape the daemon minted; anything else is a plain 404.
pub(super) fn unsubscribe_token(path: &str) -> Option<String> {
    let token = path.strip_prefix("/unsubscribe/")?;
    (token.len() == 43
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    .then(|| token.to_string())
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The recipient's confirmation page — one form, one click, no
/// session, no JavaScript. The token travels in the path only.
pub(super) fn unsubscribe_page(token: &str) -> HttpResp {
    let page = format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Unsubscribe</title></head>\
         <body style=\"font-family:sans-serif;max-width:480px;margin:64px auto;\">\
         <h1>Unsubscribe</h1>\
         <p>Confirm that you no longer wish to receive these messages.</p>\
         <form method=\"post\" action=\"/unsubscribe/{}\">\
         <button type=\"submit\" style=\"padding:8px 20px;\">Unsubscribe</button>\
         </form></body></html>",
        html_escape(token)
    );
    let mut resp = Response::from_string(page).with_status_code(200);
    resp.add_header(Header::from_bytes("Content-Type", "text/html").unwrap());
    resp
}

/// POST: the one-click suppression — the same constant answer
/// whether or not the token redeemed.
pub(super) fn unsubscribe_redeem(token: &str, state_dir: &Path) -> HttpResp {
    let _ = client::rpc(state_dir, "crm_unsubscribe_redeem", json!({"token": token}));
    let page =
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Unsubscribed</title></head>\
                <body style=\"font-family:sans-serif;max-width:480px;margin:64px auto;\">\
                <h1>Unsubscribed</h1><p>You will not receive further messages.</p></body></html>";
    let mut resp = Response::from_string(page.to_string()).with_status_code(200);
    resp.add_header(Header::from_bytes("Content-Type", "text/html").unwrap());
    resp
}
