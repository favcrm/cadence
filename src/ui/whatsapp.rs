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
}

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
        _ => return None,
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
