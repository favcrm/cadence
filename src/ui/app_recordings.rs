//! Trusted-host recording peer. URL scope and server-held session proof only.
use super::{json_response, operator, read_body, HttpResp, ServeOpts};
use crate::client;
use serde_json::{json, Value};
use std::path::Path;
use tiny_http::{Header, Method, Request};

#[derive(Clone, Copy)]
pub(super) enum Route<'a> {
    Root(&'a str, &'a str),
    Quote(&'a str, &'a str),
    Record(&'a str, &'a str, &'a str),
    Segments(&'a str, &'a str, &'a str),
    Finish(&'a str, &'a str, &'a str),
    Retry(&'a str, &'a str, &'a str),
}
fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
pub(super) fn route(path: &str) -> Option<Route<'_>> {
    let mut parts = path.strip_prefix("/api/app-installations/")?.split('/');
    let install = parts.next()?;
    if !id(install) || parts.next()? != "contexts" {
        return None;
    }
    let context = parts.next()?;
    if !id(context) || parts.next()? != "recordings" {
        return None;
    }
    let Some(record) = parts.next() else {
        return Some(Route::Root(install, context));
    };
    if record == "quote" {
        return parts
            .next()
            .is_none()
            .then_some(Route::Quote(install, context));
    }
    if !id(record) {
        return None;
    }
    let result = match parts.next() {
        None => Route::Record(install, context, record),
        Some("segments") => Route::Segments(install, context, record),
        Some("finish") => Route::Finish(install, context, record),
        Some("retry") => Route::Retry(install, context, record),
        _ => return None,
    };
    parts.next().is_none().then_some(result)
}
impl<'a> Route<'a> {
    pub(super) fn is_read(self) -> bool {
        matches!(self, Self::Root(..) | Self::Record(..))
    }
    fn scope(self) -> (&'a str, &'a str, Option<&'a str>) {
        match self {
            Self::Root(install, context) | Self::Quote(install, context) => {
                (install, context, None)
            }
            Self::Record(install, context, id)
            | Self::Segments(install, context, id)
            | Self::Finish(install, context, id)
            | Self::Retry(install, context, id) => (install, context, Some(id)),
        }
    }
    fn method(self, method: &Method) -> Option<&'static str> {
        match (self, method) {
            (Self::Root(..), Method::Get) => Some("app_recording_list"),
            (Self::Root(..), Method::Post) => Some("app_recording_create"),
            (Self::Quote(..), Method::Post) => Some("app_recording_quote"),
            (Self::Record(..), Method::Get) => Some("app_recording_detail"),
            (Self::Record(..), Method::Patch) => Some("app_recording_rename"),
            (Self::Segments(..), Method::Post) => Some("app_recording_segment"),
            (Self::Finish(..), Method::Post) => Some("app_recording_finish"),
            (Self::Retry(..), Method::Post) => Some("app_recording_retry"),
            _ => None,
        }
    }
}
fn failure(status: u16, code: &str, text: &str, retryable: bool) -> HttpResp {
    json_response(json!({"error":{"code":code,"text":text,"retryable":retryable}}))
        .with_status_code(status)
}
fn selectors(raw: &str, detail: bool) -> Result<Value, HttpResp> {
    let mut body = json!({});
    if raw.is_empty() {
        return Ok(body);
    }
    if raw.len() > 1024 {
        return Err(failure(
            400,
            "invalid_request",
            "Recording selectors exceed their bound.",
            false,
        ));
    }
    for pair in raw.split('&') {
        let (key, value) = pair.split_once('=').ok_or_else(|| {
            failure(
                400,
                "invalid_request",
                "Recording selectors are malformed.",
                false,
            )
        })?;
        let key = super::pct_decode(key).ok_or_else(|| {
            failure(
                400,
                "invalid_request",
                "Recording selector is malformed.",
                false,
            )
        })?;
        let value = super::pct_decode(value).ok_or_else(|| {
            failure(
                400,
                "invalid_request",
                "Recording selector is malformed.",
                false,
            )
        })?;
        if body.get(&key).is_some() {
            return Err(failure(
                400,
                "invalid_request",
                "Duplicate recording selector.",
                false,
            ));
        }
        match key.as_str() {
            "limit" if !detail => {
                body[&key] = json!(value
                    .parse::<u32>()
                    .ok()
                    .filter(|value| (1..=100).contains(value))
                    .ok_or_else(|| failure(
                        400,
                        "invalid_request",
                        "Recording page limit is invalid.",
                        false
                    ))?)
            }
            "cursor" if !detail && !value.is_empty() && value.len() <= 200 => {
                body[&key] = json!(value)
            }
            "segments_limit" if detail => {
                body[&key] = json!(value
                    .parse::<u32>()
                    .ok()
                    .filter(|value| (1..=50).contains(value))
                    .ok_or_else(|| failure(
                        400,
                        "invalid_request",
                        "Transcript page limit is invalid.",
                        false
                    ))?)
            }
            "segments_after" if detail => {
                body[&key] = json!(value
                    .parse::<i64>()
                    .ok()
                    .filter(|value| (-1..2048).contains(value))
                    .ok_or_else(|| failure(
                        400,
                        "invalid_request",
                        "Transcript cursor is invalid.",
                        false
                    ))?)
            }
            _ => {
                return Err(failure(
                    400,
                    "invalid_request",
                    "Unsupported recording selector.",
                    false,
                ))
            }
        }
    }
    Ok(body)
}

pub(super) fn handle(
    request: &mut Request,
    state: &Path,
    route: Route<'_>,
    opts: &ServeOpts,
) -> HttpResp {
    let Some(method) = route.method(request.method()) else {
        return failure(405, "invalid_request", "Method not allowed.", false);
    };
    // Write-path admission adds its CSRF/origin proof before arriving here;
    // the read gate and daemon independently require a real operator too.
    if let Err(response) = operator::admit_operator_read(request, state, opts) {
        return response;
    }
    let raw_query = request
        .url()
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or("");
    let body = if request.method() == &Method::Get {
        match selectors(raw_query, matches!(route, Route::Record(..))) {
            Ok(body) => body,
            Err(response) => return response,
        }
    } else {
        if !raw_query.is_empty() {
            return failure(
                400,
                "invalid_request",
                "Recording writes do not accept query fields.",
                false,
            );
        }
        let bytes = match read_body(request, 400 * 1024) {
            Ok(bytes) => bytes,
            Err(response) => return response,
        };
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(body) if body.is_object() => body,
            _ => {
                return failure(
                    400,
                    "invalid_request",
                    "Recording body must be a JSON object.",
                    false,
                )
            }
        }
    };
    let (install, context, id) = route.scope();
    let mut params = json!({"install_id":install,"context_id":context});
    params["body"] = body;
    if let Some(id) = id {
        params["recording_id"] = json!(id);
    }
    if let Some(Value::Object(proof)) = operator::chat_upload_session_proof(request, opts) {
        params
            .as_object_mut()
            .expect("host-owned recording envelope")
            .extend(proof);
    }
    let mut response = match client::rpc(state, method, params) {
        Ok(value) => {
            let status = match method {
                "app_recording_create" => 201,
                "app_recording_finish" | "app_recording_retry" => 202,
                "app_recording_segment" if value["segment"]["state"] != "confirmed" => 202,
                _ => 200,
            };
            json_response(value).with_status_code(status)
        }
        Err(error) => {
            let text = error.to_string();
            let code = [
                "unauthorized_scope",
                "sequence_mismatch",
                "unsupported_audio_format",
                "audio_too_large",
                "quote_required",
                "invalid_request",
            ]
            .into_iter()
            .find(|code| text.contains(code));
            if text.contains("operator action")
                || text.contains("not provably the operator")
                || code == Some("unauthorized_scope")
                || text.contains("not in this installation/context")
            {
                failure(
                    403,
                    "unauthorized_scope",
                    "This operator cannot access that recording scope.",
                    false,
                )
            } else if let Some(code) = code {
                let message=match code {"sequence_mismatch"=>"The saved recording sequence differs. Refresh before retrying.","unsupported_audio_format"=>"Audio must be complete 16 kHz mono PCM16 WAV.","audio_too_large"=>"This audio batch exceeds its supported size.","quote_required"=>"Recording availability or price changed. Refresh the quote before recording.",_=>"The recording request does not match its supported contract."};
                failure(409, code, message, false)
            } else {
                failure(503,"provider_unavailable","The recording service is unavailable. Saved source remains available; check the connection and retry.",true)
            }
        }
    };
    response.add_header(Header::from_bytes("Cache-Control", "no-store").expect("static header"));
    response.add_header(
        Header::from_bytes("X-Content-Type-Options", "nosniff").expect("static header"),
    );
    response
}
