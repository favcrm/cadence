//! CAD-1562: the hosted WhatsApp door client (`agenticos.whatsapp-door.v1`).
//!
//! A hosted tenant container reaches FavCRM's WhatsApp connector only
//! through AgenticOS (AOS-287): `http://api.internal/v1/runtime/whatsapp/*`,
//! with no credential. The platform derives the workspace from the lease,
//! holds the FavCRM grant, binds the `idempotency-key` header and the
//! reply digest in its send ledger and executes a reply at most once per
//! key. This module owns, for the pinned contract only:
//!
//! - the strict request bodies and query parameters;
//! - the reply digest the host draws its SLOT approval over, and the key,
//!   derived from the host effect id;
//! - the classification of every reply answer into [`ReplyState`],
//!   failing closed: only a self-consistent executed `sent` is `Sent`;
//!   anything ambiguous after the request left is `Uncertain`, which is
//!   terminal (never resent; reconciled by a read of `reply.status`).
//!
//! The door is constructed only on a daemon with a real hosted lease
//! (`agenticos::attach`); a self-hosted daemon has none and every
//! WhatsApp verb refuses.

use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// The pinned door contract every `data` must carry.
pub const CONTRACT: &str = "agenticos.whatsapp-door.v1";
const PATH: &str = "/v1/runtime/whatsapp";
const TIMEOUT: Duration = Duration::from_secs(30);
const BODY_CAP: u64 = 1024 * 1024;
/// The contract's reply text bound (1–4096 characters).
pub const TEXT_CHARS: usize = 4096;

/// The door client: where `api.internal` is, nothing else. No bearer.
#[derive(Clone)]
pub struct HostedWhatsapp {
    base: String,
    http: ureq::Agent,
}

/// One reply as the host staged it. The digest covers exactly these.
pub struct Reply {
    pub conversation_id: String,
    pub text: String,
    pub effect_id: String,
    pub reply_to_message_id: Option<String>,
}

/// The four terminal outcomes plus `Pending` (not executed; the owner
/// has not approved the key in AgenticOS yet — re-presenting the same
/// key after approval executes it once).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyState {
    Sent,
    Pending,
    Failed,
    Refused,
    Uncertain,
}

impl ReplyState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sent => "sent",
            Self::Pending => "pending",
            Self::Failed => "failed",
            Self::Refused => "refused",
            Self::Uncertain => "uncertain",
        }
    }
}

/// A classified reply answer. Identifiers only; never message content.
#[derive(Clone, Debug)]
pub struct ReplyOutcome {
    pub state: ReplyState,
    pub code: Option<String>,
    pub repeated: bool,
    pub provider_message_id: Option<String>,
    pub favcrm_message_id: Option<String>,
}

impl ReplyOutcome {
    fn bare(state: ReplyState, code: &str) -> Self {
        Self {
            state,
            code: Some(code.to_string()),
            repeated: false,
            provider_message_id: None,
            favcrm_message_id: None,
        }
    }
    pub fn to_json(&self) -> Value {
        json!({
            "state": self.state.as_str(),
            "code": self.code,
            "repeated": self.repeated,
            "provider_message_id": self.provider_message_id,
            "favcrm_message_id": self.favcrm_message_id,
        })
    }
}

/// §3.6 digest: sha256 hex over the JSON object
/// `{"v":1,"conversationId","text","effectId","replyToMessageId"}` in
/// exactly that field order (JavaScript `JSON.stringify` of the literal),
/// `replyToMessageId` `null` when absent.
pub fn reply_digest(reply: &Reply) -> String {
    let canonical = format!(
        "{{\"v\":1,\"conversationId\":{},\"text\":{},\"effectId\":{},\"replyToMessageId\":{}}}",
        json!(reply.conversation_id),
        json!(reply.text),
        json!(reply.effect_id),
        json!(reply.reply_to_message_id),
    );
    hex(&Sha256::digest(canonical.as_bytes()))
}

/// The door key (`^[A-Za-z0-9_-]{8,128}$`) for one host effect id.
pub fn reply_key(effect_id: &str) -> String {
    let hash = Sha256::digest(format!("cadence-whatsapp-reply-v1\n{effect_id}").as_bytes());
    format!("wa-{}", hex(&hash))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn valid_key(key: &str) -> bool {
    (8..=128).contains(&key.len())
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Door error codes are identifiers; anything else is withheld.
fn code_of(document: &Value, pointer: &str) -> Option<String> {
    document
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|c| {
            !c.is_empty()
                && c.len() <= 64
                && c.chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        })
        .map(str::to_string)
}

fn id_of(data: &Value, field: &str) -> Option<String> {
    data.get(field)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty() && v.len() <= 256 && !v.chars().any(char::is_control))
        .map(str::to_string)
}

/// A conversation id travels in a URL path: a bounded opaque token.
pub fn check_conversation_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
    {
        return Err(Error::rejected("WhatsApp conversation id is invalid"));
    }
    Ok(())
}

enum Wire {
    /// The request provably never left this host.
    NotSent,
    /// The request may have reached the door; no usable answer.
    Unknown,
    Answer(u16, Option<Value>),
}

impl HostedWhatsapp {
    pub fn new(base: &str) -> Result<Self> {
        let base = base.trim().trim_end_matches('/').to_string();
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(Error::rejected(
                "hosted WhatsApp door base URL must be http(s)",
            ));
        }
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(0)
            .build();
        Ok(Self {
            base,
            http: ureq::Agent::new_with_config(config),
        })
    }

    fn finish(
        response: std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Wire {
        use ureq::Timeout;
        let mut response = match response {
            Ok(response) => response,
            Err(
                ureq::Error::HostNotFound
                | ureq::Error::ConnectionFailed
                | ureq::Error::Timeout(Timeout::Resolve | Timeout::Connect),
            ) => return Wire::NotSent,
            Err(_) => return Wire::Unknown,
        };
        let status = response.status().as_u16();
        let Ok(bytes) = response
            .body_mut()
            .with_config()
            .limit(BODY_CAP)
            .read_to_vec()
        else {
            return Wire::Unknown;
        };
        Wire::Answer(status, serde_json::from_slice(&bytes).ok())
    }

    fn get(&self, path: &str, query: &[(&str, String)]) -> Wire {
        let mut request = self.http.get(format!("{}{PATH}{path}", self.base));
        for (name, value) in query {
            request = request.query(*name, value);
        }
        Self::finish(request.call())
    }

    fn post(&self, path: &str, key: Option<&str>, body: &Value) -> Wire {
        let mut request = self.http.post(format!("{}{PATH}{path}", self.base));
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        Self::finish(request.send_json(body))
    }

    /// A read (or the connect-session mint): the contract `data`, or a
    /// refusal naming the door's code. Never a fabricated empty answer.
    fn read(wire: Wire) -> Result<Value> {
        match wire {
            Wire::Answer(_, Some(document)) if document["ok"] == true => {
                let data = document.get("data").cloned().unwrap_or(Value::Null);
                if data.get("contract").and_then(Value::as_str) != Some(CONTRACT) {
                    return Err(Error::rejected(
                        "AgenticOS WhatsApp door answered an unpinned contract",
                    ));
                }
                Ok(data)
            }
            Wire::Answer(http, Some(document)) => Err(Error::rejected(format!(
                "AgenticOS WhatsApp door refused ({})",
                code_of(&document, "/error/code").unwrap_or_else(|| format!("HTTP {http}"))
            ))),
            Wire::Answer(http, None) => Err(Error::rejected(format!(
                "AgenticOS WhatsApp door answer was not understood (HTTP {http})"
            ))),
            Wire::NotSent | Wire::Unknown => {
                Err(Error::rejected("AgenticOS WhatsApp door is unreachable"))
            }
        }
    }

    pub fn status(&self) -> Result<Value> {
        Self::read(self.get("/status", &[]))
    }

    /// The one-hour signup link. The caller must not log or persist it.
    pub fn connect_session(&self) -> Result<Value> {
        Self::read(self.post("/connect-session", None, &json!({})))
    }

    pub fn conversations(&self, cursor: Option<&str>, limit: Option<u64>) -> Result<Value> {
        Self::read(self.get("/conversations", &page(cursor, limit, 50)?))
    }

    pub fn conversation(&self, id: &str) -> Result<Value> {
        check_conversation_id(id)?;
        Self::read(self.get(&format!("/conversations/{id}"), &[]))
    }

    pub fn messages(&self, id: &str, cursor: Option<&str>, limit: Option<u64>) -> Result<Value> {
        check_conversation_id(id)?;
        Self::read(self.get(
            &format!("/conversations/{id}/messages"),
            &page(cursor, limit, 100)?,
        ))
    }

    /// §3.6: stage the reply under its key. Refuses unless the door
    /// echoes this key, the host-computed digest and a known decision —
    /// the host never approves a digest it did not compute itself.
    pub fn reply_prepare(&self, reply: &Reply) -> Result<Value> {
        let key = reply_key(&reply.effect_id);
        let mut body = json!({
            "conversationId": reply.conversation_id,
            "text": reply.text,
            "effectId": reply.effect_id,
        });
        if let Some(to) = &reply.reply_to_message_id {
            body["replyToMessageId"] = json!(to);
        }
        let data = Self::read(self.post("/reply/prepare", Some(&key), &body))?;
        if data["key"].as_str() != Some(key.as_str())
            || data["digest"].as_str() != Some(reply_digest(reply).as_str())
            || !matches!(
                data["decision"].as_str(),
                Some("granted" | "approved" | "pending")
            )
        {
            return Err(Error::rejected(
                "AgenticOS WhatsApp door staged a different reply; nothing was approved",
            ));
        }
        Ok(data)
    }

    /// §3.7: execute the staged reply (at most once per key at the door).
    pub fn reply_send(&self, key: &str, digest: &str) -> ReplyOutcome {
        classify(
            key,
            self.post("/reply/send", Some(key), &json!({"digest": digest})),
        )
    }

    /// §3.8: the recorded outcome for a key, without executing.
    pub fn reply_status(&self, key: &str) -> ReplyOutcome {
        classify(key, self.get(&format!("/reply/{key}"), &[]))
    }
}

fn page(cursor: Option<&str>, limit: Option<u64>, max: u64) -> Result<Vec<(&'static str, String)>> {
    let mut query = Vec::new();
    if let Some(cursor) = cursor {
        if cursor.is_empty() || cursor.len() > 512 || cursor.chars().any(char::is_control) {
            return Err(Error::rejected("WhatsApp cursor is invalid"));
        }
        query.push(("cursor", cursor.to_string()));
    }
    if let Some(limit) = limit {
        if limit == 0 || limit > max {
            return Err(Error::rejected(format!("WhatsApp limit must be 1..{max}")));
        }
        query.push(("limit", limit.to_string()));
    }
    Ok(query)
}

/// Map one reply answer. Fails closed: only executed `sent` with an
/// authorising decision and a provider id is `Sent`.
fn classify(key: &str, wire: Wire) -> ReplyOutcome {
    let (http, document) = match wire {
        Wire::NotSent => return ReplyOutcome::bare(ReplyState::Refused, "door_unreachable"),
        Wire::Unknown => return ReplyOutcome::bare(ReplyState::Uncertain, "outcome_unknown"),
        // A 4xx without a JSON envelope never reached the ledger.
        Wire::Answer(http, None) if (400..500).contains(&http) => {
            return ReplyOutcome::bare(ReplyState::Refused, "invalid_answer")
        }
        Wire::Answer(_, None) => {
            return ReplyOutcome::bare(ReplyState::Uncertain, "answer_not_understood")
        }
        Wire::Answer(http, Some(document)) => (http, document),
    };
    let Some(data) = document.get("data").filter(|d| d.is_object()) else {
        let code = code_of(&document, "/error/code");
        // Every listed door refusal is checked before FavCRM is called.
        // `grant_revoked` can follow a FavCRM 401 on the send itself, so
        // it is not proof the reply stayed unsent.
        let refused = matches!(
            code.as_deref(),
            Some(
                "not_provisioned"
                    | "lease_lost"
                    | "capability_unavailable"
                    | "door_disabled"
                    | "favcrm_not_bound"
                    | "grant_forbidden"
                    | "module_disabled"
                    | "invalid_request"
                    | "not_prepared"
                    | "digest_mismatch"
                    | "key_conflict"
                    | "declined"
                    | "stale_approval"
                    | "window_closed"
                    | "conversation_not_found"
            )
        ) || (http == 403 && code.is_some());
        let state = if refused {
            ReplyState::Refused
        } else {
            ReplyState::Uncertain
        };
        return ReplyOutcome::bare(state, code.as_deref().unwrap_or("error"));
    };
    if data["key"].as_str() != Some(key) || !valid_key(key) || data["contract"] != CONTRACT {
        return ReplyOutcome::bare(ReplyState::Uncertain, "answer_not_bound");
    }
    let status = data["status"].as_str().unwrap_or("");
    let executed = data["executed"].as_bool();
    let authorized = matches!(data["decision"].as_str(), Some("granted" | "approved"));
    let provider = id_of(data, "providerMessageId");
    let state = match status {
        "sent" if executed == Some(true) && authorized && provider.is_some() => ReplyState::Sent,
        "pending" if executed == Some(false) => ReplyState::Pending,
        "failed" if authorized => ReplyState::Failed,
        "not_executed" => ReplyState::Refused,
        _ => ReplyState::Uncertain,
    };
    ReplyOutcome {
        state,
        code: code_of(data, "/errorCode"),
        repeated: data["repeated"] == true,
        provider_message_id: provider,
        favcrm_message_id: id_of(data, "favcrmMessageId"),
    }
}

#[cfg(test)]
mod tests {
    //! A door double that keeps the contract's semantics: prepare stages
    //! once per key and returns the digest; send executes at most once
    //! per key; `uncertain` is terminal; a closed window refuses.
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Door {
        staged: HashMap<String, String>,
        outcomes: HashMap<String, Value>,
        favcrm_calls: usize,
        window_open: bool,
        favcrm_times_out: bool,
    }

    fn envelope(code: u16, body: Value) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
        tiny_http::Response::from_data(body.to_string().into_bytes()).with_status_code(code)
    }

    fn serve(door: Arc<Mutex<Door>>) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        std::thread::spawn(move || {
            for mut request in server.incoming_requests() {
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
                let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                let key = request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("idempotency-key"))
                    .map(|h| h.value.to_string());
                let url = request.url().to_string();
                let mut door = door.lock().unwrap();
                let (code, answer) = match url.as_str() {
                    "/v1/runtime/whatsapp/reply/prepare" => {
                        let key = key.unwrap();
                        let reply = Reply {
                            conversation_id: body["conversationId"].as_str().unwrap().into(),
                            text: body["text"].as_str().unwrap().into(),
                            effect_id: body["effectId"].as_str().unwrap().into(),
                            reply_to_message_id: body["replyToMessageId"].as_str().map(Into::into),
                        };
                        let digest = reply_digest(&reply);
                        if !door.window_open {
                            (
                                409,
                                json!({"ok": false, "error": {"code": "window_closed"}}),
                            )
                        } else if door.staged.get(&key).is_some_and(|d| *d != digest) {
                            (409, json!({"ok": false, "error": {"code": "key_conflict"}}))
                        } else {
                            door.staged.insert(key.clone(), digest.clone());
                            (
                                200,
                                json!({"ok": true, "data": {"contract": CONTRACT, "key": key,
                                "digest": digest, "decision": "granted", "replyMode": "freeform"}}),
                            )
                        }
                    }
                    "/v1/runtime/whatsapp/reply/send" => {
                        let key = key.unwrap();
                        if let Some(done) = door.outcomes.get(&key) {
                            let mut done = done.clone();
                            done["data"]["repeated"] = json!(true);
                            (200, done)
                        } else if door.staged.get(&key).map(String::as_str)
                            != body["digest"].as_str()
                        {
                            (
                                409,
                                json!({"ok": false, "error": {"code": "digest_mismatch"}}),
                            )
                        } else {
                            door.favcrm_calls += 1;
                            let status = if door.favcrm_times_out {
                                "uncertain"
                            } else {
                                "sent"
                            };
                            let answer = json!({"ok": status == "sent", "data": {
                                "contract": CONTRACT, "key": key, "decision": "granted",
                                "status": status, "executed": status == "sent",
                                "repeated": false, "favcrmMessageId": "m1",
                                "providerMessageId": (status == "sent").then_some("wamid.1"),
                                "deduped": false, "errorCode": null}});
                            door.outcomes.insert(key, answer.clone());
                            (if status == "sent" { 200 } else { 409 }, answer)
                        }
                    }
                    path if path.starts_with("/v1/runtime/whatsapp/reply/") => {
                        let key = path.rsplit('/').next().unwrap();
                        match door.outcomes.get(key) {
                            Some(done) => (200, done.clone()),
                            None => (409, json!({"ok": false, "error": {"code": "not_prepared"}})),
                        }
                    }
                    _ => (
                        404,
                        json!({"ok": false, "error": {"code": "invalid_request"}}),
                    ),
                };
                let _ = request.respond(envelope(code, answer));
            }
        });
        base
    }

    fn reply(effect: &str) -> Reply {
        Reply {
            conversation_id: "conv-1".into(),
            text: "Thanks — see you at 5 \"pm\"".into(),
            effect_id: effect.into(),
            reply_to_message_id: None,
        }
    }

    #[test]
    fn cad1562_reply_executes_once_and_uncertain_is_terminal() {
        let door = Arc::new(Mutex::new(Door {
            window_open: true,
            ..Door::default()
        }));
        let client = HostedWhatsapp::new(&serve(door.clone())).unwrap();
        let staged = client.reply_prepare(&reply("wae_a")).unwrap();
        let key = reply_key("wae_a");
        assert_eq!(staged["digest"], reply_digest(&reply("wae_a")));
        let first = client.reply_send(&key, &reply_digest(&reply("wae_a")));
        assert_eq!(first.state, ReplyState::Sent);
        let again = client.reply_send(&key, &reply_digest(&reply("wae_a")));
        assert!(again.repeated && again.state == ReplyState::Sent);
        assert_eq!(door.lock().unwrap().favcrm_calls, 1);
        // Another digest under an unprepared key never executes.
        let forged = client.reply_send(&reply_key("wae_b"), "00");
        assert_eq!(forged.state, ReplyState::Refused);

        door.lock().unwrap().favcrm_times_out = true;
        client.reply_prepare(&reply("wae_c")).unwrap();
        let key = reply_key("wae_c");
        assert_eq!(
            client
                .reply_send(&key, &reply_digest(&reply("wae_c")))
                .state,
            ReplyState::Uncertain
        );
        assert_eq!(client.reply_status(&key).state, ReplyState::Uncertain);
        assert_eq!(door.lock().unwrap().favcrm_calls, 2);

        door.lock().unwrap().window_open = false;
        let closed = client.reply_prepare(&reply("wae_d")).unwrap_err();
        assert!(closed.to_string().contains("window_closed"), "{closed}");
    }

    #[test]
    fn cad1562_classification_fails_closed() {
        let key = "wa-abcdefgh";
        let data = |status: &str, decision: &str, executed: bool, provider: Option<&str>| {
            json!({"ok": status == "sent", "data": {"contract": CONTRACT, "key": key,
                "status": status, "decision": decision, "executed": executed,
                "providerMessageId": provider, "repeated": false}})
        };
        let cases = [
            (
                200,
                Some(data("sent", "granted", true, Some("p"))),
                ReplyState::Sent,
            ),
            (
                200,
                Some(data("sent", "granted", true, None)),
                ReplyState::Uncertain,
            ),
            (
                200,
                Some(data("sent", "pending", true, Some("p"))),
                ReplyState::Uncertain,
            ),
            (
                202,
                Some(data("pending", "pending", false, None)),
                ReplyState::Pending,
            ),
            (
                502,
                Some(data("failed", "approved", false, None)),
                ReplyState::Failed,
            ),
            (
                409,
                Some(data("not_executed", "approved", false, None)),
                ReplyState::Refused,
            ),
            (
                409,
                Some(data("uncertain", "approved", false, None)),
                ReplyState::Uncertain,
            ),
            (
                200,
                Some(json!({"ok": true, "data": {"key": key, "status": "sent"}})),
                ReplyState::Uncertain,
            ),
            (
                409,
                Some(json!({"ok": false, "error": {"code": "stale_approval"}})),
                ReplyState::Refused,
            ),
            (
                409,
                Some(json!({"ok": false, "error": {"code": "grant_revoked"}})),
                ReplyState::Uncertain,
            ),
            (
                500,
                Some(json!({"ok": false, "error": {"code": "boom"}})),
                ReplyState::Uncertain,
            ),
            (400, None, ReplyState::Refused),
            (502, None, ReplyState::Uncertain),
        ];
        for (http, document, want) in cases {
            assert_eq!(
                classify(key, Wire::Answer(http, document.clone())).state,
                want,
                "{http} {document:?}"
            );
        }
        assert_eq!(classify(key, Wire::NotSent).state, ReplyState::Refused);
        assert_eq!(classify(key, Wire::Unknown).state, ReplyState::Uncertain);
    }
}
