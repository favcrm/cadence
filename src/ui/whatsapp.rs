//! CAD-1562: the hosted WhatsApp door over the board.
//!
//! Every route relays one daemon `whatsapp_*` verb with an exact typed
//! body. Like `crm_smtp`, every route is operator-proof POST-only (even
//! the reads), so the board is at least as strict as the daemon RPC it
//! relays: unknown fields — forged identity, key, digest, company or
//! connection fields — and query strings refuse here before the daemon
//! is called. Members reach the reads only through a read-only screen
//! mount (CAD-1537), never through these routes.
use super::{err_response, json_response, read_body, HttpResp};
use crate::{client, error::Error};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tiny_http::Request;

const BODY_CAP: u64 = 48 * 1024;
const RESULT_CAP: usize = 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) enum Route {
    Status,
    ConnectSession,
    Conversations,
    Conversation,
    Messages,
    ReplyPrepare,
    ReplySend,
    ReplyStatus,
    /// CAD-1590: a door v2 verb, relayed with exactly its daemon fields.
    V2(&'static str, &'static [&'static str]),
}

/// CAD-1590: the door v2 routes, each with the exact field allowlist of
/// the daemon verb it relays (besides `install_id` and `context_id`).
const V2_ROUTES: &[(&str, &str, &[&str])] = &[
    (
        "/api/whatsapp/templates",
        "whatsapp_templates_list",
        &["status"],
    ),
    (
        "/api/whatsapp/template",
        "whatsapp_template_show",
        &["template_id"],
    ),
    (
        "/api/whatsapp/template/prepare",
        "whatsapp_template_prepare",
        &["conversation_id", "template_id", "variables"],
    ),
    (
        "/api/whatsapp/template/send",
        "whatsapp_template_send",
        &["effect_id", "digest"],
    ),
    (
        "/api/whatsapp/template/status",
        "whatsapp_template_status",
        &["effect_id"],
    ),
    (
        "/api/whatsapp/broadcasts",
        "whatsapp_broadcasts_list",
        &["cursor", "limit"],
    ),
    (
        "/api/whatsapp/broadcast",
        "whatsapp_broadcast_show",
        &["broadcast_id"],
    ),
    (
        "/api/whatsapp/broadcast/estimate",
        "whatsapp_broadcast_estimate",
        &["broadcast_id"],
    ),
    (
        "/api/whatsapp/broadcast/results",
        "whatsapp_broadcast_results",
        &["broadcast_id", "cursor", "limit", "status"],
    ),
    (
        "/api/whatsapp/broadcast/draft",
        "whatsapp_broadcast_draft",
        &[
            "broadcast_id",
            "name",
            "template_id",
            "body_parameters",
            "header_media_url",
            "audience",
            "exclusions",
        ],
    ),
    (
        "/api/whatsapp/broadcast/test/prepare",
        "whatsapp_broadcast_test_prepare",
        &["broadcast_id", "to"],
    ),
    (
        "/api/whatsapp/broadcast/test/send",
        "whatsapp_broadcast_test_send",
        &["effect_id", "digest"],
    ),
    (
        "/api/whatsapp/broadcast/prepare",
        "whatsapp_broadcast_prepare",
        &["broadcast_id", "send_at"],
    ),
    (
        "/api/whatsapp/broadcast/send",
        "whatsapp_broadcast_send",
        &["effect_id", "digest"],
    ),
    (
        "/api/whatsapp/broadcast/stop",
        "whatsapp_broadcast_stop",
        &["broadcast_id"],
    ),
];

pub(super) fn route(path: &str) -> Option<Route> {
    Some(match path {
        "/api/whatsapp/status" => Route::Status,
        "/api/whatsapp/connect-session" => Route::ConnectSession,
        "/api/whatsapp/conversations" => Route::Conversations,
        "/api/whatsapp/conversation" => Route::Conversation,
        "/api/whatsapp/messages" => Route::Messages,
        "/api/whatsapp/reply/prepare" => Route::ReplyPrepare,
        "/api/whatsapp/reply/send" => Route::ReplySend,
        "/api/whatsapp/reply/status" => Route::ReplyStatus,
        _ => {
            let (_, method, fields) = V2_ROUTES.iter().find(|(p, _, _)| *p == path)?;
            Route::V2(method, fields)
        }
    })
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    install_id: String,
    context_id: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Page {
    install_id: String,
    context_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Conversation {
    install_id: String,
    context_id: String,
    conversation_id: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Messages {
    install_id: String,
    context_id: String,
    conversation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReplyPrepare {
    install_id: String,
    context_id: String,
    conversation_id: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to_message_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReplySend {
    install_id: String,
    context_id: String,
    effect_id: String,
    digest: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReplyStatus {
    install_id: String,
    context_id: String,
    effect_id: String,
}

fn typed<T: serde::de::DeserializeOwned + Serialize>(
    request: &mut Request,
) -> Result<serde_json::Value, HttpResp> {
    let body = read_body(request, BODY_CAP)?;
    let body: T = serde_json::from_slice(&body)
        .map_err(|_| err_response(400, "invalid WhatsApp request schema"))?;
    Ok(serde_json::to_value(body).expect("typed request serializes"))
}

/// A door v2 body: an object with string `install_id`/`context_id` and
/// no field outside the relayed verb's allowlist. The daemon validates
/// every value again.
fn v2_body(
    request: &mut Request,
    method: &str,
    fields: &[&str],
) -> Result<serde_json::Value, HttpResp> {
    let cap = if method == "whatsapp_broadcast_draft" {
        200 * 1024
    } else {
        BODY_CAP
    };
    let body = read_body(request, cap)?;
    let invalid = || err_response(400, "invalid WhatsApp request schema");
    let body: serde_json::Value = serde_json::from_slice(&body).map_err(|_| invalid())?;
    let object = body.as_object().ok_or_else(invalid)?;
    let scoped = ["install_id", "context_id"]
        .iter()
        .all(|k| object.get(*k).is_some_and(serde_json::Value::is_string));
    if !scoped
        || object.keys().any(|k| {
            !matches!(k.as_str(), "install_id" | "context_id") && !fields.contains(&k.as_str())
        })
    {
        return Err(invalid());
    }
    Ok(body)
}

pub(super) fn handle(request: &mut Request, state: &Path, route: Route) -> HttpResp {
    if request
        .url()
        .split_once('?')
        .is_some_and(|(_, q)| !q.is_empty())
    {
        return err_response(400, "WhatsApp query parameters are unsupported");
    }
    let parsed = match route {
        Route::Status => typed::<Scope>(request).map(|p| ("whatsapp_status", p)),
        Route::ConnectSession => typed::<Scope>(request).map(|p| ("whatsapp_connect_session", p)),
        Route::Conversations => typed::<Page>(request).map(|p| ("whatsapp_conversations_list", p)),
        Route::Conversation => {
            typed::<Conversation>(request).map(|p| ("whatsapp_conversations_show", p))
        }
        Route::Messages => typed::<Messages>(request).map(|p| ("whatsapp_messages", p)),
        Route::ReplyPrepare => {
            typed::<ReplyPrepare>(request).map(|p| ("whatsapp_reply_prepare", p))
        }
        Route::ReplySend => typed::<ReplySend>(request).map(|p| ("whatsapp_reply_send", p)),
        Route::ReplyStatus => typed::<ReplyStatus>(request).map(|p| ("whatsapp_reply_status", p)),
        Route::V2(method, fields) => v2_body(request, method, fields).map(|p| (method, p)),
    };
    let (method, params) = match parsed {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    match client::rpc(state, method, params) {
        Ok(value) => {
            if value.to_string().len() > RESULT_CAP {
                return err_response(502, "invalid WhatsApp answer");
            }
            json_response(value)
        }
        Err(error) => {
            let operator_refusal = error.to_string().contains("operator action")
                || error.to_string().contains("not provably the operator");
            let code = if operator_refusal {
                403
            } else {
                match error {
                    Error::Rejected(_) | Error::Structured(_) => 409,
                    _ => 503,
                }
            };
            err_response(code, "WhatsApp request refused or unavailable")
        }
    }
}
