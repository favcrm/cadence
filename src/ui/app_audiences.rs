//! Saved segments, exclusion lists, suppressions and frozen
//! audiences; every route requires operator proof (CAD-780).
//!
//! The board relays the daemon's `app_segment_*`,
//! `app_exclusion_*`, `app_suppression_*` and `app_audience_*`
//! actions with the URL's installation, context, segment, exclusion
//! and freeze IDs as authority — body fields can never claim
//! identity (`by`, `actor`), routing (`workspace`) or discovery
//! links (`project`, `project_link`), nor smuggle the URL IDs
//! themselves. Predicate and base values are forwarded as typed JSON
//! for the daemon's allowlisted grammar; no SQL is ever built here.
//! Errors are bounded generics that never echo audience content;
//! preview samples stay bounded server-side, and frozen member lists
//! never leave the daemon at all.
use super::{err_response, json_response, read_body, HttpResp};
use crate::{client, error::Error};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use tiny_http::{Header, Request};

const BODY_CAP: u64 = 48 * 1024;
const RESULT_CAP: usize = 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) enum Route<'a> {
    SegmentSave(&'a str, &'a str),
    SegmentList(&'a str, &'a str),
    SegmentShow(&'a str, &'a str, &'a str),
    ExclusionSave(&'a str, &'a str),
    ExclusionList(&'a str, &'a str),
    ExclusionShow(&'a str, &'a str, &'a str),
    SuppressionAdd(&'a str, &'a str),
    SuppressionRemove(&'a str, &'a str),
    SuppressionList(&'a str, &'a str),
    /// POST-only preview: a read with a body, so it rides the write
    /// transport while the daemon itself writes nothing.
    Preview(&'a str, &'a str),
    Prepare(&'a str, &'a str),
    FreezeShow(&'a str, &'a str, &'a str),
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
    if !segment(install) || parts.next()? != "contexts" {
        return None;
    }
    let context = parts.next()?;
    if !segment(context) {
        return None;
    }
    let section = parts.next()?;
    let rest: Vec<&str> = parts.collect();
    // Reserved verbs precede IDs: `list` under segments/exclusions
    // and `remove`/`list` under suppressions are unaddressable as
    // IDs over HTTP (RPC still serves them), so a bulk POST can
    // never be mistaken for a single-row read, matching the
    // records peer's `csv-preview` reservation.
    match (section, rest.as_slice()) {
        ("segments", []) => Some(Route::SegmentSave(install, context)),
        ("segments", ["list"]) => Some(Route::SegmentList(install, context)),
        ("segments", [id]) if segment(id) => Some(Route::SegmentShow(install, context, id)),
        ("exclusions", []) => Some(Route::ExclusionSave(install, context)),
        ("exclusions", ["list"]) => Some(Route::ExclusionList(install, context)),
        ("exclusions", [id]) if segment(id) => Some(Route::ExclusionShow(install, context, id)),
        ("suppressions", []) => Some(Route::SuppressionAdd(install, context)),
        ("suppressions", ["remove"]) => Some(Route::SuppressionRemove(install, context)),
        ("suppressions", ["list"]) => Some(Route::SuppressionList(install, context)),
        ("audience", ["preview"]) => Some(Route::Preview(install, context)),
        ("audience", ["prepares"]) => Some(Route::Prepare(install, context)),
        ("audience", ["prepares", id]) if segment(id) => {
            Some(Route::FreezeShow(install, context, id))
        }
        _ => None,
    }
}

impl Route<'_> {
    pub(super) fn is_read(self) -> bool {
        matches!(
            self,
            Self::SegmentList(..)
                | Self::SegmentShow(..)
                | Self::ExclusionList(..)
                | Self::ExclusionShow(..)
                | Self::SuppressionList(..)
                | Self::FreezeShow(..)
        )
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Predicate {
    field: String,
    op: String,
    value: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SegmentSave {
    segment_id: String,
    name: String,
    predicates: Vec<Predicate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_revision: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExclusionSave {
    list_id: String,
    name: String,
    member_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_revision: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SuppressionAdd {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    customer_id: Option<String>,
    reason: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SuppressionRemove {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    customer_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AudiencePreview {
    base: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exclusion_list_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AudiencePrepare {
    freeze_id: String,
    base: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exclusion_list_id: Option<String>,
    max_recipients: u64,
}

fn typed<T: serde::de::DeserializeOwned>(request: &mut Request) -> Result<T, HttpResp> {
    let bytes = read_body(request, BODY_CAP)?;
    serde_json::from_slice(&bytes)
        .map_err(|_| err_response(400, "invalid app audience request schema"))
}

fn revision(value: u64) -> Result<i64, HttpResp> {
    i64::try_from(value)
        .ok()
        .filter(|revision| *revision > 0)
        .ok_or_else(|| err_response(400, "invalid app audience request schema"))
}

pub(super) fn handle(
    request: &mut Request,
    state: &Path,
    route: Route<'_>,
    write: bool,
) -> HttpResp {
    if request
        .url()
        .split_once('?')
        .is_some_and(|(_, q)| !q.is_empty())
    {
        return err_response(400, "app audience query parameters are unsupported");
    }
    let (method, params) = match route {
        Route::SegmentList(install, context) if !write => (
            "app_segment_list",
            json!({"install_id": install, "context_id": context}),
        ),
        Route::SegmentShow(install, context, id) => (
            "app_segment_show",
            json!({"install_id": install, "context_id": context, "segment_id": id}),
        ),
        Route::SegmentSave(install, context) => {
            let body: SegmentSave = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let mut params = json!({
                "install_id": install, "context_id": context,
                "segment_id": body.segment_id, "name": body.name,
            });
            let mut rules = Vec::with_capacity(body.predicates.len());
            for rule in &body.predicates {
                rules.push(json!({"field": rule.field, "op": rule.op, "value": rule.value}));
            }
            params["predicates"] = Value::Array(rules);
            if let Some(expected) = body.expected_revision {
                params["expected_revision"] = match revision(expected) {
                    Ok(value) => value.into(),
                    Err(response) => return response,
                };
            }
            ("app_segment_save", params)
        }
        Route::ExclusionList(install, context) if !write => (
            "app_exclusion_list",
            json!({"install_id": install, "context_id": context}),
        ),
        Route::ExclusionShow(install, context, id) => (
            "app_exclusion_show",
            json!({"install_id": install, "context_id": context, "list_id": id}),
        ),
        Route::ExclusionSave(install, context) => {
            let body: ExclusionSave = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let mut params = json!({
                "install_id": install, "context_id": context,
                "list_id": body.list_id, "name": body.name,
                "member_ids": body.member_ids,
            });
            if let Some(expected) = body.expected_revision {
                params["expected_revision"] = match revision(expected) {
                    Ok(value) => value.into(),
                    Err(response) => return response,
                };
            }
            ("app_exclusion_save", params)
        }
        Route::SuppressionList(install, context) if !write => (
            "app_suppression_list",
            json!({"install_id": install, "context_id": context}),
        ),
        Route::SuppressionAdd(install, context) => {
            let body: SuppressionAdd = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let mut params =
                json!({"install_id": install, "context_id": context, "reason": body.reason});
            if let Some(email) = body.email {
                params["email"] = Value::String(email);
            }
            if let Some(customer) = body.customer_id {
                params["customer_id"] = Value::String(customer);
            }
            ("app_suppression_add", params)
        }
        Route::SuppressionRemove(install, context) => {
            let body: SuppressionRemove = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let mut params = json!({"install_id": install, "context_id": context});
            if let Some(email) = body.email {
                params["email"] = Value::String(email);
            }
            if let Some(customer) = body.customer_id {
                params["customer_id"] = Value::String(customer);
            }
            ("app_suppression_remove", params)
        }
        Route::FreezeShow(install, context, id) => (
            "app_audience_show",
            json!({"install_id": install, "context_id": context, "freeze_id": id}),
        ),
        Route::Preview(install, context) => {
            let body: AudiencePreview = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let mut params =
                json!({"install_id": install, "context_id": context, "base": body.base});
            if let Some(list) = body.exclusion_list_id {
                params["exclusion_list_id"] = Value::String(list);
            }
            ("app_audience_preview", params)
        }
        Route::Prepare(install, context) => {
            let body: AudiencePrepare = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let max: i64 = match revision(body.max_recipients) {
                Ok(value) => value,
                Err(response) => return response,
            };
            let mut params = json!({
                "install_id": install, "context_id": context,
                "freeze_id": body.freeze_id, "base": body.base,
                "max_recipients": max,
            });
            if let Some(list) = body.exclusion_list_id {
                params["exclusion_list_id"] = Value::String(list);
            }
            ("app_audience_prepare", params)
        }
        // Reads served on the write transport are refused: the
        // read peer serves them after operator-read admission.
        _ => return err_response(405, "method not allowed"),
    };
    match client::rpc(state, method, params) {
        Ok(value) if value.to_string().len() <= RESULT_CAP => {
            let mut response = json_response(value);
            response.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
            response
        }
        Ok(_) => err_response(502, "app audience receipt exceeds bound"),
        Err(error) => {
            let denied = error.to_string().contains("operator action")
                || error.to_string().contains("not provably the operator");
            let code = if denied {
                403
            } else {
                match error {
                    Error::Rejected(_) | Error::Structured(_) => 409,
                    _ => 503,
                }
            };
            err_response(code, "app audience management refused or unavailable")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routes_and_body_schema_are_exact() {
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/segments"),
            Some(Route::SegmentSave("i", "c"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/segments/list"),
            Some(Route::SegmentList("i", "c"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/segments/seg-vip"),
            Some(Route::SegmentShow("i", "c", "seg-vip"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/exclusions"),
            Some(Route::ExclusionSave("i", "c"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/exclusions/list"),
            Some(Route::ExclusionList("i", "c"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/exclusions/ex-hold"),
            Some(Route::ExclusionShow("i", "c", "ex-hold"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/suppressions"),
            Some(Route::SuppressionAdd("i", "c"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/suppressions/remove"),
            Some(Route::SuppressionRemove("i", "c"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/suppressions/list"),
            Some(Route::SuppressionList("i", "c"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/audience/preview"),
            Some(Route::Preview("i", "c"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/audience/prepares"),
            Some(Route::Prepare("i", "c"))
        ));
        assert!(matches!(
            route("/api/app-installations/i/contexts/c/audience/prepares/freeze-1"),
            Some(Route::FreezeShow("i", "c", "freeze-1"))
        ));
        // Preview and prepares are POST-only transport: never reads.
        assert!(
            !route("/api/app-installations/i/contexts/c/audience/preview")
                .unwrap()
                .is_read()
        );
        assert!(
            !route("/api/app-installations/i/contexts/c/audience/prepares")
                .unwrap()
                .is_read()
        );
        assert!(
            route("/api/app-installations/i/contexts/c/audience/prepares/freeze-1")
                .unwrap()
                .is_read()
        );
        for path in [
            "/api/app-installations/i/contexts/c/segments/",
            "/api/app-installations/i/contexts/c/segments/list/extra",
            "/api/app-installations/i/contexts/c/segments/seg-vip/extra",
            "/api/app-installations/i/contexts/c/exclusions/ex-hold/extra",
            "/api/app-installations/i/contexts/c/suppressions/remove/extra",
            "/api/app-installations/i/contexts/c/audience",
            "/api/app-installations/i/contexts/c/audience/preview/extra",
            "/api/app-installations/i/contexts/c/audience/prepares/freeze-1/extra",
            "/api/app-installations/i/contexts/c/records",
            "/api/app-installations/../contexts",
        ] {
            assert!(route(path).is_none(), "route admitted {path}");
        }
        // Save bodies carry no identity: URL segments are the
        // authority, and every extra field refuses at the transport.
        assert!(serde_json::from_str::<SegmentSave>(
            r#"{"segment_id":"seg-vip","name":"VIP","predicates":[{"field":"tag","op":"eq","value":"vip"}]}"#
        )
        .is_ok());
        for body in [
            r#"{"segment_id":"seg-vip","name":"VIP","predicates":[]}"#,
            r#"{"segment_id":"seg-vip","name":"VIP","predicates":[],"install_id":"other"}"#,
            r#"{"segment_id":"seg-vip","name":"VIP","predicates":[],"by":"operator"}"#,
            r#"{"segment_id":"seg-vip","name":"VIP","predicates":[],"project":"client"}"#,
            r#"{"segment_id":"seg-vip","name":"VIP"}"#,
            r#"{"segment_id":"seg-vip","name":"VIP","predicates":[],"expected_revision":0}"#,
        ] {
            // Empty predicates pass the transport grammar (the daemon
            // names the allowed bound); unknown fields never do.
            if body.contains("\"predicates\":[]")
                && !body.contains("install_id")
                && !body.contains("\"by\"")
                && !body.contains("project")
            {
                assert!(serde_json::from_str::<SegmentSave>(body).is_ok());
            } else {
                assert!(
                    serde_json::from_str::<SegmentSave>(body).is_err(),
                    "segment save admitted {body}"
                );
            }
        }
        assert!(serde_json::from_str::<AudiencePrepare>(
            r#"{"freeze_id":"freeze-1","base":{"mode":"all"},"max_recipients":50}"#
        )
        .is_ok());
        for body in [
            r#"{"freeze_id":"freeze-1","base":{"mode":"all"},"max_recipients":50,"actor":"op"}"#,
            r#"{"freeze_id":"freeze-1","base":{"mode":"all"},"max_recipients":50,"workspace":"w"}"#,
            r#"{"freeze_id":"freeze-1","max_recipients":50}"#,
        ] {
            assert!(
                serde_json::from_str::<AudiencePrepare>(body).is_err(),
                "audience prepare admitted {body}"
            );
        }
        // `base` admits any JSON value at the transport; the daemon's
        // allowlisted grammar names the supported modes.
        assert!(serde_json::from_str::<AudiencePreview>(
            r#"{"base":{"mode":"sql","query":"SELECT 1"}}"#
        )
        .is_ok());
        assert!(serde_json::from_str::<SuppressionAdd>(
            r#"{"email":"a@example.com","reason":"bounce"}"#
        )
        .is_ok());
        assert!(serde_json::from_str::<SuppressionAdd>(
            r#"{"email":"a@example.com","reason":"bounce","project_link":"x"}"#
        )
        .is_err());
    }
}
