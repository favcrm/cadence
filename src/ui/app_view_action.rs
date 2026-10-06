//! Strict operator-only HTTP peer for the pinned app-actions/v2 slice.
//!
//! Route identity (install/context/form/action/record) is derived from the
//! path and forwarded as daemon selectors. The body accepts only the three
//! existing pins, typed input and update-only expected_revision. This board
//! handler is a relay; the daemon remains authoritative for the verified
//! same-snapshot descriptor/binding/action proof and the record CAS.

use super::{err_response, json_response, read_body, HttpResp};
use crate::client;
use crate::error::Error;
use serde_json::{json, Map, Value};
use std::path::Path;
use tiny_http::Request;

const BODY_CAP: u64 = 64 * 1024;
const RESULT_CAP: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) enum Route<'a> {
    Create {
        install: &'a str,
        context: &'a str,
        view: &'a str,
        action: &'a str,
    },
    Update {
        install: &'a str,
        context: &'a str,
        view: &'a str,
        action: &'a str,
        record: &'a str,
    },
}

fn segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn view_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && matches!(value.as_bytes().first(), Some(first) if first.is_ascii_lowercase())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn action_id(value: &str) -> bool {
    let mut segments = value.split('.');
    let mut count = 0usize;
    segments.all(|part| {
        count += 1;
        count <= 4
            && !part.is_empty()
            && part.len() <= 64
            && part.starts_with(|c: char| c.is_ascii_lowercase())
            && part.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
            })
    }) && count >= 2
}

pub(super) fn route(path: &str) -> Option<Route<'_>> {
    let tail = path.strip_prefix("/api/app-installations/")?;
    let parts = tail.split('/').collect::<Vec<_>>();
    if parts.len() != 7 && parts.len() != 9 {
        return None;
    }
    let (install, contexts, context, views, view, actions, action) = (
        parts[0], parts[1], parts[2], parts[3], parts[4], parts[5], parts[6],
    );
    if !segment(install)
        || contexts != "contexts"
        || !segment(context)
        || views != "views"
        || !view_id(view)
        || actions != "actions"
        || !action_id(action)
    {
        return None;
    }
    match parts.as_slice() {
        [_, _, _, _, _, _, "customer.create"] => Some(Route::Create {
            install,
            context,
            view,
            action,
        }),
        [_, _, _, _, _, _, "customer.update", "records", record] if segment(record) => {
            Some(Route::Update {
                install,
                context,
                view,
                action,
                record,
            })
        }
        _ => None,
    }
}

fn digest(value: &Value) -> Result<String, HttpResp> {
    let value = value
        .as_str()
        .filter(|value| {
            value.strip_prefix("sha256:").is_some_and(|hex| {
                hex.len() == 64
                    && hex
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
        })
        .ok_or_else(|| err_response(400, "invalid app view action schema"))?;
    Ok(value.to_string())
}

fn object_body(raw: &[u8], update: bool) -> Result<Map<String, Value>, HttpResp> {
    let body: Value = serde_json::from_slice(raw)
        .map_err(|_| err_response(400, "invalid app view action schema"))?;
    let object = body
        .as_object()
        .ok_or_else(|| err_response(400, "invalid app view action schema"))?;
    let allowed: &[&str] = if update {
        &[
            "digest",
            "descriptor",
            "binding",
            "expected_revision",
            "input",
        ]
    } else {
        &["digest", "descriptor", "binding", "input"]
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(err_response(400, "invalid app view action schema"));
    }
    for key in ["digest", "descriptor", "binding"] {
        digest(
            object
                .get(key)
                .ok_or_else(|| err_response(400, "invalid app view action schema"))?,
        )?;
    }
    if !object.get("input").is_some_and(Value::is_object) {
        return Err(err_response(400, "invalid app view action schema"));
    }
    if update {
        object
            .get("expected_revision")
            .and_then(Value::as_i64)
            .filter(|revision| *revision > 0)
            .ok_or_else(|| err_response(400, "invalid app view action schema"))?;
    }
    Ok(object.clone())
}

pub(super) fn handle(
    request: &mut Request,
    state: &Path,
    route: Route<'_>,
    write: bool,
) -> HttpResp {
    if !write {
        return err_response(405, "method not allowed");
    }
    if request
        .url()
        .split_once('?')
        .is_some_and(|(_, query)| !query.is_empty())
    {
        return err_response(400, "invalid app view action schema");
    }
    let bytes = match read_body(request, BODY_CAP) {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    let (install, context, view, action, record, update) = match route {
        Route::Create {
            install,
            context,
            view,
            action,
        } => (install, context, view, action, None, false),
        Route::Update {
            install,
            context,
            view,
            action,
            record,
        } => (install, context, view, action, Some(record), true),
    };
    let object = match object_body(&bytes, update) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let mut params = Map::new();
    params.insert("install_id".into(), Value::String(install.to_string()));
    params.insert("context_id".into(), Value::String(context.to_string()));
    params.insert("view_id".into(), Value::String(view.to_string()));
    params.insert("action_id".into(), Value::String(action.to_string()));
    params.insert("digest".into(), object["digest"].clone());
    params.insert(
        "view_descriptor_digest".into(),
        object["descriptor"].clone(),
    );
    params.insert("view_binding_digest".into(), object["binding"].clone());
    params.insert("input".into(), object["input"].clone());
    if let Some(record) = record {
        params.insert("record_id".into(), Value::String(record.to_string()));
        params.insert(
            "expected_revision".into(),
            object["expected_revision"].clone(),
        );
    }
    match client::rpc(state, "app_view_action", json!(params)) {
        Ok(value) if value.to_string().len() <= RESULT_CAP => json_response(value),
        Ok(_) => err_response(502, "app view action receipt exceeds bound"),
        Err(error) => {
            let denied = error.to_string().contains("operator action")
                || error.to_string().contains("not provably the operator");
            let status = if denied {
                403
            } else {
                match error {
                    Error::Rejected(_) | Error::Structured(_) => 400,
                    _ => 503,
                }
            };
            err_response(status, "app view action refused or unavailable")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_routes_are_exact_and_dotted_ids_are_bounded() {
        assert!(matches!(
            route("/api/app-installations/install-a/contexts/context-a/views/customer-create-form/actions/customer.create"),
            Some(Route::Create { install: "install-a", context: "context-a", view: "customer-create-form", action: "customer.create" })
        ));
        assert!(matches!(
            route("/api/app-installations/install-a/contexts/context-a/views/customer-edit-form/actions/customer.update/records/cust-0123456789abcdef0123456789abcdef"),
            Some(Route::Update { action: "customer.update", record: "cust-0123456789abcdef0123456789abcdef", .. })
        ));
        assert!(route("/api/app-installations/install-a/contexts/context-a/views/form/actions/customer.create/records/id/extra").is_none());
        assert!(matches!(
            route("/api/app-installations/install-a/contexts/context-a/views/custom_form/actions/customer.create"),
            Some(Route::Create { view: "custom_form", .. })
        ));
        assert!(route("/api/app-installations/install-a/contexts/context-a/views/Customer-form/actions/customer.create").is_none());
        assert!(route("/api/app-installations/install-a/contexts/context-a/views/customer-form/actions/customer.delete").is_none());
        assert!(route("/api/app-installations/install-a/contexts/context-a/views/customer-form/actions/customer.create/records/cust-a").is_none());
        assert!(route("/api/app-installations/install-a/contexts/context-a/views/customer-form/actions/customer.update").is_none());
        assert!(route("/api/app-installations/Install-a/contexts/context-a/views/customer-form/actions/customer.create").is_none());
        assert!(route("/api/app-installations/install-a/contexts/context_a/views/customer-form/actions/customer.create").is_none());
        assert!(route("/api/app-installations/install-a/contexts/context-a/views/form/actions/Customer.create").is_none());
        assert!(route("/api/app-installations/install-a/contexts/context-a/views/form/actions/customer%2ecreate").is_none());
    }

    #[test]
    fn action_http_body_admits_only_pins_input_and_update_cas() {
        let pin = format!("sha256:{}", "a".repeat(64));
        let create = json!({
            "digest": pin,
            "descriptor": format!("sha256:{}", "b".repeat(64)),
            "binding": format!("sha256:{}", "c".repeat(64)),
            "input": {"display_name": "Example"}
        });
        assert!(object_body(&serde_json::to_vec(&create).unwrap(), false).is_ok());
        let mut forged = create.clone();
        forged["record_id"] = json!("client-chosen");
        assert!(object_body(&serde_json::to_vec(&forged).unwrap(), false).is_err());
        let update = json!({
            "digest": create["digest"],
            "descriptor": create["descriptor"],
            "binding": create["binding"],
            "expected_revision": 4,
            "input": {"display_name": "Changed"}
        });
        assert!(object_body(&serde_json::to_vec(&update).unwrap(), true).is_ok());
        let mut stale = update;
        stale["expected_revision"] = json!(0);
        assert!(object_body(&serde_json::to_vec(&stale).unwrap(), true).is_err());
    }
}
