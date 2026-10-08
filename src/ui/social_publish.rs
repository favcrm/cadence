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
    /// CAD-1041: `POST /api/social-publishes/<id>/send-now` — the
    /// operator's explicit send of one named queued intent. Relays the
    /// operator-only `social_publish_send_now` RPC; the HTTP peer
    /// carries the same `intent_id` the daemon claims by identity.
    SendNow(&'a str),
    /// CAD-979: `POST /api/social-media-imports` → `social_publish_media_import`.
    MediaImport,
    /// CAD-1123 HP4: `POST /api/social-publish-starts` → `social_publish_start`.
    /// The body names a run and a mode only; the daemon derives the rest.
    Start,
    /// CAD-1143: operator-only prepare for an immutable, nondispatchable
    /// owner-authorized intent. The relay carries no caller-supplied material.
    PrepareIntent,
    /// CAD-1143: operator-only attach; the daemon obtains and verifies the
    /// signed AOS queue receipt before atomically attaching and queueing.
    AttachIntent,
    /// Advisory, scoped completion observation; never attach authority.
    StatusIntent,
    /// Terminal local cancellation of a prepared or unclaimed attachment.
    CancelIntent,
    /// CAD-1123 HP4: `POST /api/social-publishes/<id>/reschedule` →
    /// `social_publish_reschedule` (compare-and-swap on the queued intent).
    Reschedule(&'a str),
    /// AOS-150's dedicated, short-lived issuer assertion read. This route
    /// returns a bare immutable descriptor and bypasses board cookies only
    /// because the daemon verifies the separate AOS JWS and consumes its jti.
    OwnerIntentRead(&'a str),
}
fn segment(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub(super) fn route(path: &str) -> Option<Route<'_>> {
    if let Some(intent_id) = path.strip_prefix("/api/social-owner-intent/") {
        if owner_intent_segment(intent_id) {
            return Some(Route::OwnerIntentRead(intent_id));
        }
        return None;
    }
    // CAD-979: the operator media-import write route is a distinct path so a
    // mistaken GET on it cannot be read as a `Show` by `is_read`.
    if path == "/api/social-media-imports" {
        return Some(Route::MediaImport);
    }
    if path == "/api/social-publish-starts" {
        return Some(Route::Start);
    }
    if path == "/api/social-publish-intents/prepare" {
        return Some(Route::PrepareIntent);
    }
    if path == "/api/social-publish-intents/attach" {
        return Some(Route::AttachIntent);
    }
    if path == "/api/social-publish-intents/status" {
        return Some(Route::StatusIntent);
    }
    if path == "/api/social-publish-intents/cancel" {
        return Some(Route::CancelIntent);
    }
    if path == "/api/social-publishes" {
        return Some(Route::List);
    }
    let tail = path.strip_prefix("/api/social-publishes/")?;
    if let Some((id, verb)) = tail.split_once('/') {
        if !segment(id) {
            return None;
        }
        // Dispatch stays off-board except the operator's own send-now:
        // claim-due/reconcile/report 404 here.
        return match verb {
            "cancel" => Some(Route::Cancel(id)),
            "send-now" => Some(Route::SendNow(id)),
            "reschedule" => Some(Route::Reschedule(id)),
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
fn owner_intent_segment(id: &str) -> bool {
    let Some(first) = id.as_bytes().first() else {
        return false;
    };
    id.len() <= 120
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

impl<'a> Route<'a> {
    pub(super) fn is_read(self) -> bool {
        matches!(self, Self::List | Self::Show(_) | Self::OwnerIntentRead(_))
    }

    pub(super) fn owner_intent_id(self) -> Option<&'a str> {
        match self {
            Self::OwnerIntentRead(id) => Some(id),
            _ => None,
        }
    }
}
fn present_opt<'de, D>(de: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(de).map(Some)
}
fn present_i64<'de, D>(de: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    i64::deserialize(de).map(Some)
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
/// CAD-1123 HP4: publish start. Unknown fields are refused, so a forged
/// destination, grant, scope or approval in the body is a 400 here and a
/// strict-field refusal at the daemon.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Start {
    request_id: String,
    run_id: String,
    mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    due_epoch: Option<i64>,
}
/// CAD-1143: prepare names a host-minted request and run only. The daemon
/// derives scope, artifact, binding, account and all publication material.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PrepareIntent {
    request_id: String,
    run_id: String,
    mode: String,
    #[serde(
        default,
        deserialize_with = "present_i64",
        skip_serializing_if = "Option::is_none"
    )]
    due_epoch: Option<i64>,
}
/// CAD-1143: attach carries only the opaque prepared selector and exact
/// installation/context scope; no grant or owner-exchange result is accepted.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AttachIntent {
    prepared_id: String,
    install_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
}
/// Status/cancel require all scope keys, including an explicit
/// `context_id: null` — an absent key never silently scopes to the
/// workspace; serde's Option accepts null or omits the field, so the
/// field is a raw Value checked for null-or-nonempty-string.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PreparedIntentScope {
    prepared_id: String,
    install_id: String,
    context_id: Value,
}
/// CAD-1123 HP4: reschedule names the intent's own scope, the time the
/// operator saw and the new time.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Reschedule {
    install_id: String,
    #[serde(
        default,
        deserialize_with = "present_opt",
        skip_serializing_if = "Option::is_none"
    )]
    context_id: Option<String>,
    expected_due_epoch: i64,
    due_epoch: i64,
}

/// CAD-1027: cancel (and CAD-1041 send-now) names the intent's own
/// install and exact context; the daemon refuses any other scope.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct IntentScope {
    install_id: String,
    #[serde(
        default,
        deserialize_with = "present_opt",
        skip_serializing_if = "Option::is_none"
    )]
    context_id: Option<String>,
}

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

fn origin_port(uri: &ureq::http::Uri) -> Option<u16> {
    let port = uri.port_u16();
    if uri.authority()?.port().is_some() && port.is_none() {
        return None;
    }
    match (uri.scheme_str()?, port) {
        ("https", None) => Some(443),
        ("http", None) => Some(80),
        (_, Some(port)) => Some(port),
        _ => None,
    }
}

fn same_origin(left: &ureq::http::Uri, right: &ureq::http::Uri) -> bool {
    left.scheme_str() == right.scheme_str()
        && left
            .host()
            .zip(right.host())
            .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
        && origin_port(left) == origin_port(right)
}

/// The login redirect's configured app URL is the only portal-origin source.
/// Use its scheme and authority only; its path/query never reach the launch URL.
/// Refuse the issuer origin: it is the API/JWKS authority, not the owner UI.
fn owner_action_origin(public: &super::PublicBoard) -> Option<String> {
    let uri: ureq::http::Uri = public.authorize_url.parse().ok()?;
    let issuer_uri: ureq::http::Uri = public.issuer.parse().ok()?;
    if same_origin(&uri, &issuer_uri) {
        return None;
    }
    let scheme = uri.scheme_str()?;
    let authority = uri.authority()?;
    let host = uri.host()?;
    if authority.as_str().contains('@') || host.is_empty() {
        return None;
    }
    if scheme != "https" || authority.port().is_some() {
        return None;
    }
    Some(format!("{scheme}://{host}"))
}

/// Owner launch requires the separately resolved canonical `company_slug`.
/// Cross-check the exact production host form; never infer a slug from a host
/// prefix, environment suffix, workspace id, or caller-controlled value.
fn owner_company_slug(public: &super::PublicBoard) -> Option<&str> {
    let slug = public.company_slug.as_deref()?;
    if !super::valid_board_company_slug(slug) {
        return None;
    }
    let expected_host = format!("{slug}.cadencecloud.app");
    (public.host == expected_host).then_some(slug)
}

fn owner_portal_scope(public: Option<&super::PublicBoard>) -> Result<(String, String), HttpResp> {
    let Some(public) = public else {
        return Err(err_response(
            503,
            "not configured: owner portal needs a canonical production board host and authorize_url",
        ));
    };
    let Some(origin) = owner_action_origin(public) else {
        return Err(err_response(
            503,
            "not configured: set AGENTICOS_BOARD_AUTHORIZE_URL to a validated HTTPS app origin distinct from PublicBoard.issuer",
        ));
    };
    let Some(slug) = owner_company_slug(public) else {
        return Err(err_response(
            503,
            "not configured: PublicBoard.company_slug must be a valid canonical slug matching the production host",
        ));
    };
    Ok((origin, slug.to_owned()))
}

fn add_owner_action_url(
    value: &mut Value,
    origin: &str,
    company_slug: &str,
) -> Result<(), HttpResp> {
    let prepared = value
        .get("prepared")
        .and_then(Value::as_object)
        .ok_or_else(|| err_response(502, "server returned an invalid prepared owner intent"))?;
    let prepared_id = prepared
        .get("prepared_id")
        .and_then(Value::as_str)
        .filter(|id| owner_intent_segment(id))
        .map(str::to_owned)
        .ok_or_else(|| err_response(502, "server returned an invalid prepared owner selector"))?;
    let owner = prepared
        .get("owner_intent")
        .and_then(Value::as_object)
        .ok_or_else(|| err_response(502, "server returned an invalid prepared owner selector"))?;
    let intent_id = owner
        .get("intent_id")
        .and_then(Value::as_str)
        .filter(|id| *id == prepared_id.as_str())
        .map(str::to_owned)
        .ok_or_else(|| err_response(502, "server returned an invalid prepared owner selector"))?;
    let intent_digest = owner
        .get("intent_digest")
        .and_then(Value::as_str)
        .filter(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .map(str::to_owned)
        .ok_or_else(|| err_response(502, "server returned an invalid prepared owner digest"))?;
    value["owner_action_url"] = json!(format!(
        "{origin}/social-owner-action?company_slug={company_slug}&intent_id={intent_id}&intent_digest={intent_digest}"
    ));
    Ok(())
}

pub(super) fn handle_owner_intent_read(
    request: &Request,
    state: &Path,
    intent_id: &str,
) -> HttpResp {
    if !owner_intent_segment(intent_id)
        || request.url().contains('?')
        || request
            .headers()
            .iter()
            .filter(|header| header.field.equiv("Authorization"))
            .count()
            != 1
        || request
            .headers()
            .iter()
            .filter(|header| header.field.equiv("Accept"))
            .count()
            != 1
        || super::write_path::header_value(request, "Accept").as_deref() != Some("application/json")
        || request
            .headers()
            .iter()
            .any(|header| header.field.equiv("Transfer-Encoding"))
        || request
            .headers()
            .iter()
            .filter(|header| header.field.equiv("Content-Length"))
            .count()
            > 1
        || super::write_path::header_value(request, "Content-Length")
            .is_some_and(|length| length.trim() != "0")
    {
        return err_response(400, "malformed signed owner-intent read");
    }
    let authorization =
        super::write_path::header_value(request, "Authorization").unwrap_or_default();
    let Some(assertion) = authorization.strip_prefix("Bearer ").filter(|token| {
        !token.is_empty()
            && token.len() <= 8 * 1024
            && !token.bytes().any(|byte| byte.is_ascii_whitespace())
    }) else {
        return err_response(401, "owner intent assertion required");
    };
    match client::rpc(
        state,
        "social_owner_intent_read",
        json!({"intent_id":intent_id,"assertion":assertion}),
    ) {
        Ok(descriptor) => {
            let mut response = json_response(descriptor);
            response.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
            response.add_header(Header::from_bytes("Vary", "Authorization").unwrap());
            response.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
            response
        }
        Err(_) => err_response(403, "owner intent assertion refused"),
    }
}

pub(super) fn handle(
    request: &mut Request,
    state: &Path,
    route: Route<'_>,
    write: bool,
    public: Option<&super::PublicBoard>,
) -> HttpResp {
    let route = if write && matches!(route, Route::List) {
        Route::Schedule
    } else {
        route
    };
    let owner_portal = if matches!(route, Route::PrepareIntent) {
        Some(match owner_portal_scope(public) {
            Ok(scope) => scope,
            Err(response) => return response,
        })
    } else {
        None
    };
    let (method, params) = match route {
        Route::OwnerIntentRead(_) => {
            return err_response(405, "the signed owner-intent read has a dedicated handler");
        }
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
            let scope: IntentScope = match parse_json(&bytes) {
                Ok(value) => value,
                Err(response) => return response,
            };
            let mut params = serde_json::to_value(scope).expect("typed cancel serializes");
            params["intent_id"] = json!(id);
            ("social_publish_cancel", params)
        }
        Route::SendNow(id) => {
            let bytes = match read_body(request, BODY_CAP) {
                Ok(bytes) => bytes,
                Err(response) => return response,
            };
            let scope: IntentScope = match parse_json(&bytes) {
                Ok(value) => value,
                Err(response) => return response,
            };
            // The relay passes the path id and the typed scope only; the
            // daemon applies the same `operator_connection` gate and the
            // scoped claim the RPC does.
            let mut params = serde_json::to_value(scope).expect("typed scope serializes");
            params["intent_id"] = json!(id);
            ("social_publish_send_now", params)
        }
        Route::Start => {
            let bytes = match read_body(request, BODY_CAP) {
                Ok(bytes) => bytes,
                Err(response) => return response,
            };
            let value: Start = match parse_json(&bytes) {
                Ok(value) => value,
                Err(response) => return response,
            };
            (
                "social_publish_start",
                serde_json::to_value(value).expect("typed start serializes"),
            )
        }
        Route::PrepareIntent => {
            let bytes = match read_body(request, BODY_CAP) {
                Ok(bytes) => bytes,
                Err(response) => return response,
            };
            let value: PrepareIntent = match parse_json(&bytes) {
                Ok(value) => value,
                Err(response) => return response,
            };
            (
                "app_publish_intent_prepare",
                serde_json::to_value(value).expect("typed intent prepare serializes"),
            )
        }
        Route::AttachIntent => {
            let bytes = match read_body(request, BODY_CAP) {
                Ok(bytes) => bytes,
                Err(response) => return response,
            };
            let value: AttachIntent = match parse_json(&bytes) {
                Ok(value) => value,
                Err(response) => return response,
            };
            (
                "app_publish_intent_attach",
                serde_json::to_value(value).expect("typed intent attach serializes"),
            )
        }
        Route::StatusIntent | Route::CancelIntent => {
            let bytes = match read_body(request, BODY_CAP) {
                Ok(bytes) => bytes,
                Err(response) => return response,
            };
            let value: PreparedIntentScope = match parse_json(&bytes) {
                Ok(value) => value,
                Err(response) => return response,
            };
            if !matches!(value.context_id, Value::Null | Value::String(_)) {
                return err_response(400, "context_id must be null or a scope string");
            }
            let method = if matches!(route, Route::StatusIntent) {
                "app_publish_intent_status"
            } else {
                "app_publish_intent_cancel"
            };
            (
                method,
                serde_json::to_value(value).expect("typed intent scope serializes"),
            )
        }
        Route::Reschedule(id) => {
            let bytes = match read_body(request, BODY_CAP) {
                Ok(bytes) => bytes,
                Err(response) => return response,
            };
            let value: Reschedule = match parse_json(&bytes) {
                Ok(value) => value,
                Err(response) => return response,
            };
            let mut params = serde_json::to_value(value).expect("typed reschedule serializes");
            params["intent_id"] = json!(id);
            ("social_publish_reschedule", params)
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
        Ok(mut value) => {
            if let Some((origin, slug)) = owner_portal.as_ref() {
                if let Err(response) = add_owner_action_url(&mut value, origin, slug) {
                    return response;
                }
            }
            let mut response = json_response(enrich_envelope(value, state));
            response.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
            if matches!(route, Route::StatusIntent) {
                response.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
            }
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
        // CAD-1041: send-now routes exactly; a forged or second verb
        // segment still 404s.
        assert!(matches!(
            route("/api/social-publishes/intent-a/send-now"),
            Some(Route::SendNow("intent-a"))
        ));
        // CAD-1123 HP4: start and reschedule route exactly, and are writes.
        assert!(matches!(
            route("/api/social-publish-starts"),
            Some(Route::Start)
        ));
        assert!(matches!(
            route("/api/social-publishes/intent-a/reschedule"),
            Some(Route::Reschedule("intent-a"))
        ));
        assert!(!Route::Start.is_read() && !Route::Reschedule("a").is_read());
        for path in [
            "/api/social-publish-starts/extra",
            "/api/social-publishes/intent-a/reschedule/extra",
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
