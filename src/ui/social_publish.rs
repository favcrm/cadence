//! Operator-only HTTP relay for frozen publish intents (CAD-787).
//!
//! Exposes schedule/cancel/show/list to the board by relaying the daemon
//! `social_publish_*` RPC (771 backend, landed AOS-94). claim_due,
//! reconcile and report stay off-board: `route` matches nothing for them
//! so they 404. Backend refusal codes pass through verbatim via
//! `home::rpc_err` — the relay never re-validates. Writer/reviewer enrich
//! best-effort from a read-only `app_run_show` join on the frozen run id;
//! a missing run yields nulls, never a failed read.
use super::{err_response, home, json_response, parse_json, read_body, HttpResp};
use crate::client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use tiny_http::{Header, Request};

const BODY_CAP: u64 = 48 * 1024;

#[derive(Clone, Copy)]
pub(super) enum Route<'a> {
    Schedule,
    List,
    Show(&'a str),
    Cancel(&'a str),
    /// CAD-979: `POST /api/social-media-imports` → `social_publish_media_import`.
    MediaImport,
}
fn segment(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub(super) fn route(path: &str) -> Option<Route<'_>> {
    // CAD-979: the operator media-import write route is a distinct path so a
    // mistaken GET on it cannot be read as a `Show` by `is_read`.
    if path == "/api/social-media-imports" {
        return Some(Route::MediaImport);
    }
    if path == "/api/social-publishes" {
        return Some(Route::List);
    }
    let tail = path.strip_prefix("/api/social-publishes/")?;
    if let Some((id, verb)) = tail.split_once('/') {
        if !segment(id) {
            return None;
        }
        // Dispatch stays off-board: claim-due/reconcile/report 404 here.
        return match verb {
            "cancel" => Some(Route::Cancel(id)),
            _ => None,
        };
    }
    // Single-segment dispatch tails 404 too — only intent ids show.
    if !segment(tail) {
        return None;
    }
    if matches!(tail, "claim-due" | "reconcile" | "report") {
        return None;
    }
    Some(Route::Show(tail))
}
impl Route<'_> {
    pub(super) fn is_read(self) -> bool {
        matches!(self, Self::List | Self::Show(_))
    }
}
fn present_opt<'de, D>(de: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(de).map(Some)
}
/// Artifact-freeze schedule grammar: digests and connection derive
/// server-side from the approved run. Unknown fields are refused.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Schedule {
    request_id: String,
    install_id: String,
    #[serde(
        default,
        deserialize_with = "present_opt",
        skip_serializing_if = "Option::is_none"
    )]
    context_id: Option<String>,
    run_id: String,
    effect_id: String,
    artifact_id: String,
    bundle_digest: String,
    slot: String,
    destination_id: String,
    toolkit: String,
    #[serde(
        default,
        deserialize_with = "present_opt",
        skip_serializing_if = "Option::is_none"
    )]
    media_key: Option<String>,
    grant_id: String,
    approval_id: String,
    due_epoch: i64,
    timezone: String,
}
/// CAD-979: operator media-import body — the approved run's exact
/// provenance + scope. Unknown fields refused; `context_id` is the
/// exact/null-preserving scope bound.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MediaImport {
    request_id: String,
    install_id: String,
    #[serde(
        default,
        deserialize_with = "present_opt",
        skip_serializing_if = "Option::is_none"
    )]
    context_id: Option<String>,
    run_id: String,
    artifact_id: String,
    bundle_digest: String,
    slot: String,
    /// CAD-979 v9: the send-intent `(toolkit, destination_id)` drive the
    /// local→AOS `connectionId` resolution before upload. Required.
    toolkit: String,
    destination_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn query(request: &Request) -> Result<Value, HttpResp> {
    let raw = request
        .url()
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or("");
    if raw.is_empty() {
        return Ok(json!({}));
    }
    let mut params = serde_json::Map::new();
    for pair in raw.split('&') {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| err_response(400, "malformed social publish query"))?;
        let key = super::pct_decode(key)
            .ok_or_else(|| err_response(400, "malformed social publish query"))?;
        let value = super::pct_decode(value)
            .ok_or_else(|| err_response(400, "malformed social publish query"))?;
        if !matches!(key.as_str(), "install_id" | "context_id")
            || !segment(&value)
            || params.insert(key, json!(value)).is_some()
        {
            return Err(err_response(
                400,
                "invalid or duplicate social publish selector",
            ));
        }
    }
    if params.contains_key("context_id") && !params.contains_key("install_id") {
        return Err(err_response(400, "context selector requires installation"));
    }
    Ok(Value::Object(params))
}

/// Best-effort writer/reviewer from the frozen run's material. Read-only;
/// a missing run yields nulls and never fails the intent read.
fn enrich(intent: &mut Value, state: &Path) {
    let run_id = intent
        .get("frozen")
        .and_then(|frozen| frozen.get("run_id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let (writer, reviewer) = match client::rpc(state, "app_run_show", json!({"run_id": run_id})) {
        Ok(run) => {
            let writer = run
                .get("snapshot")
                .and_then(|snapshot| snapshot.get("inputs"))
                .and_then(|inputs| inputs.get("writer"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let reviewer = run
                .get("reviews")
                .and_then(Value::as_array)
                .and_then(|reviews| {
                    reviews.iter().find(|review| {
                        review.get("decision").and_then(Value::as_str) == Some("approve")
                    })
                })
                .and_then(|review| review.get("reviewer"))
                .and_then(Value::as_str)
                .unwrap_or("");
            (
                (!writer.is_empty()).then_some(writer.to_owned()),
                (!reviewer.is_empty()).then_some(reviewer.to_owned()),
            )
        }
        Err(_) => (None, None),
    };
    intent["writer"] = writer.map_or(Value::Null, Value::String);
    intent["reviewer"] = reviewer.map_or(Value::Null, Value::String);
}

fn enrich_envelope(mut value: Value, state: &Path) -> Value {
    if let Some(intent) = value.get_mut("intent") {
        enrich(intent, state);
    }
    if let Some(intents) = value.get_mut("intents").and_then(Value::as_array_mut) {
        for intent in intents {
            enrich(intent, state);
        }
    }
    value
}

pub(super) fn handle(
    request: &mut Request,
    state: &Path,
    route: Route<'_>,
    write: bool,
) -> HttpResp {
    let route = if write && matches!(route, Route::List) {
        Route::Schedule
    } else {
        route
    };
    let (method, params) = match route {
        Route::List => {
            let params = match query(request) {
                Ok(value) => value,
                Err(response) => return response,
            };
            ("social_publish_list", params)
        }
        Route::Show(id) => ("social_publish_show", json!({"intent_id": id})),
        Route::Schedule => {
            let bytes = match read_body(request, BODY_CAP) {
                Ok(bytes) => bytes,
                Err(response) => return response,
            };
            let value: Schedule = match parse_json(&bytes) {
                Ok(value) => value,
                Err(response) => return response,
            };
            (
                "social_publish_schedule",
                serde_json::to_value(value).expect("typed schedule serializes"),
            )
        }
        Route::Cancel(id) => {
            let bytes = match read_body(request, BODY_CAP) {
                Ok(bytes) => bytes,
                Err(response) => return response,
            };
            if let Err(response) = parse_json::<Empty>(&bytes) {
                return response;
            }
            ("social_publish_cancel", json!({"intent_id": id}))
        }
        Route::MediaImport => {
            let bytes = match read_body(request, BODY_CAP) {
                Ok(bytes) => bytes,
                Err(response) => return response,
            };
            let value: MediaImport = match parse_json(&bytes) {
                Ok(value) => value,
                Err(response) => return response,
            };
            (
                "social_publish_media_import",
                serde_json::to_value(value).expect("typed media import serializes"),
            )
        }
    };
    match client::rpc(state, method, params) {
        Ok(value) => {
            let mut response = json_response(enrich_envelope(value, state));
            response.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
            response
        }
        Err(error) => home::rpc_err(&error, method),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_are_exact_and_dispatch_stays_off_board() {
        assert!(matches!(route("/api/social-publishes"), Some(Route::List)));
        assert!(matches!(
            route("/api/social-publishes/intent-a"),
            Some(Route::Show("intent-a"))
        ));
        assert!(matches!(
            route("/api/social-publishes/intent-a/cancel"),
            Some(Route::Cancel("intent-a"))
        ));
        for path in [
            "/api/social-publishes/claim-due",
            "/api/social-publishes/reconcile",
            "/api/social-publishes/report",
            "/api/social-publishes/intent-a/report",
            "/api/social-publishes/intent-a/claim-due",
            "/api/social-publishes/intent-a/cancel/extra",
            "/api/social-publishes/../show",
            "/api/social-publishes/a/cancel/extra",
            "/api/social-publishes/",
            "/api/social-publishes/a/b/c",
        ] {
            assert!(
                route(path).is_none(),
                "dispatch or traversal served: {path}"
            );
        }
        assert!(route("/api/social-publishes/bad id").is_none());
        assert!(Route::List.is_read() && Route::Show("a").is_read());
        assert!(!Route::Schedule.is_read() && !Route::Cancel("a").is_read());
    }
}
