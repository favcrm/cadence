//! Operator-only HTTP peer for the bound live view read (CAD-867).
//!
//! The descriptor-driven surface never calls the raw per-source reads
//! — it relays `app_view_read` with the installation, view, op and the
//! three expected digests (bundle, descriptor, binding) carried on a
//! bounded URL grammar. Identity, actor, source, SQL and routing keys
//! are never transport fields: the URL segments name install/view/
//! record; the query names the three digest assertions plus the
//! optional context and — on a list only — pagination. Unknown,
//! duplicate, null or out-of-bounds selectors refuse before the daemon
//! is touched.
//!
//! Routes:
//!   GET /api/app-installations/<install>/views/<view>/rows
//!       ?digest=&descriptor=&binding=&context_id=&limit=&cursor=&query=
//!   GET /api/app-installations/<install>/views/<view>/rows/<record>
//!       ?digest=&descriptor=&binding=&context_id=
//! A `rows` route derives the `list` op, a `rows/<id>` route the
//! `show` op — the caller never picks the op name itself. The query's
//! `digest`/`descriptor`/`binding` map onto the RPC's
//! `digest`/`view_descriptor_digest`/`view_binding_digest` pins.
use super::{err_response, json_response, HttpResp};
use crate::client;
use crate::error::Error;
use serde_json::{json, Value};
use std::path::Path;
use tiny_http::Request;

/// A context holds at most 100 records of at most 16KiB each; the
/// bound row envelope projects only bound text/enum/tags cells, so the
/// relayed receipt stays well under this cap.
const RESULT_CAP: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) enum Route<'a> {
    /// `GET …/views/<view>/rows` — a `list` read.
    List(&'a str, &'a str),
    /// `GET …/views/<view>/rows/<record>` — a `show` read.
    Show(&'a str, &'a str, &'a str),
}

fn segment(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

pub(super) fn route(path: &str) -> Option<Route<'_>> {
    let tail = path.strip_prefix("/api/app-installations/")?;
    let mut parts = tail.split('/');
    let install = parts.next()?;
    if !segment(install) || parts.next()? != "views" {
        return None;
    }
    let view = parts.next()?;
    if !segment(view) {
        return None;
    }
    match parts.next()? {
        "rows" => match parts.next() {
            None => Some(Route::List(install, view)),
            Some(record) if segment(record) && parts.next().is_none() => {
                Some(Route::Show(install, view, record))
            }
            _ => None,
        },
        _ => None,
    }
}

impl Route<'_> {
    pub(super) fn is_read(self) -> bool {
        true
    }
}

/// A digest claim is `sha256:<64 lowercase hex>`; anything else is a
/// schema refusal, never a silent skip.
fn digest_ok(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|h| {
        h.len() == 64
            && h.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    })
}

/// The bounded selectors a bound read admits: the three digest
/// assertions are always required, `context_id` is optional for
/// contextless sources, `limit`/`cursor`/`query` only on a list.
/// Unknown, duplicate or malformed selectors refuse; a selector that
/// does not belong on a show route is refused, never dropped. An
/// empty `&&` span / trailing `&` is malformed grammar, never dropped
/// silently. Returns the params `Map` (object), so the caller can
/// `insert` its derived fields.
fn selectors(raw: &str, list: bool) -> Result<serde_json::Map<String, Value>, HttpResp> {
    let mut params = serde_json::Map::new();
    // No silent drop: a raw query that is only whitespace, or a pair
    // that splits to empty, is malformed — refuse it.
    if !raw.is_empty() && raw.split('&').any(|p| p.is_empty()) {
        return Err(err_response(400, "invalid app view read schema"));
    }
    for pair in raw.split('&').filter(|p| !p.is_empty()) {
        let (raw_key, raw_value) = pair
            .split_once('=')
            .ok_or_else(|| err_response(400, "invalid app view read schema"))?;
        let key = super::pct_decode(raw_key)
            .ok_or_else(|| err_response(400, "invalid app view read schema"))?;
        let value = super::pct_decode(raw_value)
            .ok_or_else(|| err_response(400, "invalid app view read schema"))?;
        let name = match key.as_str() {
            "digest" => "digest",
            "descriptor" => "view_descriptor_digest",
            "binding" => "view_binding_digest",
            "context_id" => "context_id",
            "limit" if list => "limit",
            "cursor" if list => "cursor",
            "query" if list => "query",
            // Every other key — including a list-only selector on a show
            // route — is a forged field, never ignored.
            _ => return Err(err_response(400, "invalid app view read schema")),
        };
        if params.contains_key(name) {
            return Err(err_response(400, "duplicate app view read selector"));
        }
        match name {
            "digest" | "view_descriptor_digest" | "view_binding_digest" => {
                if !digest_ok(&value) {
                    return Err(err_response(400, "invalid app view read schema"));
                }
                params.insert(name.to_string(), Value::String(value));
            }
            "context_id" | "cursor" => {
                if !segment(&value) {
                    return Err(err_response(400, "invalid app view read schema"));
                }
                params.insert(name.to_string(), Value::String(value));
            }
            "limit" => {
                let limit: i64 = value
                    .parse()
                    .ok()
                    .filter(|l| (1..=crate::store::app_records::RECORD_LIMIT).contains(l))
                    .ok_or_else(|| err_response(400, "invalid app view read schema"))?;
                params.insert(name.to_string(), json!(limit));
            }
            "query" => {
                if value.is_empty() || value.len() > 120 || value.chars().any(char::is_control) {
                    return Err(err_response(400, "invalid app view read schema"));
                }
                params.insert(name.to_string(), Value::String(value));
            }
            _ => unreachable!(),
        }
    }
    // The three digest assertions are always required — a bound read
    // can never proceed on a missing or forged digest.
    for field in ["digest", "view_descriptor_digest", "view_binding_digest"] {
        if !params.contains_key(field) {
            return Err(err_response(400, "invalid app view read schema"));
        }
    }
    Ok(params)
}

pub(super) fn handle(
    request: &mut Request,
    state: &Path,
    route: Route<'_>,
    write: bool,
) -> HttpResp {
    if write {
        return err_response(405, "method not allowed");
    }
    let raw = request.url().split_once('?').map(|(_, q)| q).unwrap_or("");
    // `selectors` returns the params `Map`; insert derived fields via
    // `Map::insert` (a `serde_json::Value` has no `insert`).
    let mut params = match selectors(raw, matches!(route, Route::List(..))) {
        Ok(map) => map,
        Err(response) => return response,
    };
    let (install, view, op) = match route {
        Route::List(i, v) => (i, v, "list"),
        Route::Show(i, v, r) => {
            params.insert("record_id".into(), Value::String(r.to_string()));
            (i, v, "show")
        }
    };
    params.insert("install_id".into(), Value::String(install.to_string()));
    params.insert("view_id".into(), Value::String(view.to_string()));
    params.insert("op".into(), Value::String(op.to_string()));
    let params = Value::Object(params);
    match client::rpc(state, "app_view_read", params) {
        Ok(value) if value.to_string().len() <= RESULT_CAP => json_response(value),
        Ok(_) => err_response(502, "app view read receipt exceeds bound"),
        Err(error) => {
            // The operator gate is the only 403 — every bounded refusal
            // (stale/wrong digest, unknown field, scope violation,
            // corrupt producer) is a 400, matching the per-source peers.
            let denied = error.to_string().contains("operator action")
                || error.to_string().contains("not provably the operator");
            let code = if denied {
                403
            } else {
                match error {
                    Error::Rejected(_) | Error::Structured(_) => 400,
                    _ => 503,
                }
            };
            err_response(code, "app view read refused or unavailable")
        }
    }
}
