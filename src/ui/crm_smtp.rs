//! One host-custodied SMTP sender per CRM installation/context,
//! over the board (CAD-785).
//!
//! The board relays the daemon's `crm_smtp_*` actions with exact
//! typed bodies — every route is operator-proof like the daemon RPC
//! it relays, and at least as strict: unknown fields, forged
//! identity/receipt/routing fields, multi-recipient shapes and
//! query strings are all refused here before the daemon is ever
//! called. Secrets never cross this peer in either direction: the
//! request grammar carries no credential field, and responses are
//! length-capped generics that never echo addresses or content on
//! refusal.
use super::{err_response, json_response, read_body, HttpResp};
use crate::{client, error::Error};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tiny_http::Request;

const BODY_CAP: u64 = 16 * 1024;
const RESULT_CAP: usize = 256 * 1024;

#[derive(Clone, Copy)]
pub(super) enum Route {
    Bind,
    Rebind,
    Revoke,
    Show,
    TestSend,
}

pub(super) fn route(path: &str) -> Option<Route> {
    match path {
        "/api/crm-smtp/bind" => Some(Route::Bind),
        "/api/crm-smtp/rebind" => Some(Route::Rebind),
        "/api/crm-smtp/revoke" => Some(Route::Revoke),
        "/api/crm-smtp/show" => Some(Route::Show),
        "/api/crm-smtp/test-send" => Some(Route::TestSend),
        _ => None,
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Bind {
    install_id: String,
    context_id: String,
    connection_id: String,
    request_id: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Rebind {
    install_id: String,
    context_id: String,
    connection_id: String,
    expected_revision: i64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Revoke {
    install_id: String,
    context_id: String,
    expected_revision: i64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Show {
    install_id: String,
    context_id: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TestSend {
    install_id: String,
    context_id: String,
    campaign_id: String,
    to_email: String,
}

fn typed<T: serde::de::DeserializeOwned>(request: &mut Request) -> Result<T, HttpResp> {
    let body = read_body(request, BODY_CAP)?;
    serde_json::from_slice(&body).map_err(|_| err_response(400, "invalid CRM SMTP request schema"))
}

pub(super) fn handle(request: &mut Request, state: &Path, route: Route) -> HttpResp {
    if request
        .url()
        .split_once('?')
        .is_some_and(|(_, q)| !q.is_empty())
    {
        return err_response(400, "CRM SMTP query parameters are unsupported");
    }
    let (method, params) = match route {
        Route::Bind => {
            let body: Bind = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            (
                "crm_smtp_bind",
                serde_json::to_value(body).expect("typed request serializes"),
            )
        }
        Route::Rebind => {
            let body: Rebind = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            (
                "crm_smtp_rebind",
                serde_json::to_value(body).expect("typed request serializes"),
            )
        }
        Route::Revoke => {
            let body: Revoke = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            (
                "crm_smtp_revoke",
                serde_json::to_value(body).expect("typed request serializes"),
            )
        }
        Route::Show => {
            let body: Show = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            (
                "crm_smtp_show",
                serde_json::to_value(body).expect("typed request serializes"),
            )
        }
        Route::TestSend => {
            let body: TestSend = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            (
                "crm_smtp_test_send",
                serde_json::to_value(body).expect("typed request serializes"),
            )
        }
    };
    match client::rpc(state, method, params) {
        Ok(value) => {
            let output = value.to_string();
            if output.len() > RESULT_CAP {
                return err_response(502, "invalid CRM SMTP receipt");
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
            err_response(code, "CRM SMTP sender refused or unavailable")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_routes_and_strict_single_recipient_schema() {
        assert!(matches!(route("/api/crm-smtp/bind"), Some(Route::Bind)));
        assert!(matches!(
            route("/api/crm-smtp/test-send"),
            Some(Route::TestSend)
        ));
        for path in [
            "/api/crm-smtp/",
            "/api/crm-smtp/bind/extra",
            "/api/crm-smtp/send",
            "/api/crm-smtp/test-send/now",
            "/api/crm-smtp\\bind",
        ] {
            assert!(route(path).is_none(), "{path}");
        }
        // Unknown fields refuse — including forged identity, receipt
        // and bulk-recipient shapes. Single-recipient is the whole
        // grammar: `to_emails` is not a field at all.
        for body in [
            r#"{"install_id":"i","context_id":"c","campaign_id":"m","to_email":"o@example.com","by":"operator"}"#,
            r#"{"install_id":"i","context_id":"c","campaign_id":"m","to_email":"o@example.com","to_emails":["a@example.com"]}"#,
            r#"{"install_id":"i","context_id":"c","campaign_id":"m","to_email":"o@example.com","assistant_receipt":"r"}"#,
            r#"{"install_id":"i","context_id":"c","campaign_id":"m","to_email":["o@example.com"]}"#,
        ] {
            assert!(serde_json::from_str::<TestSend>(body).is_err(), "{body}");
        }
        assert!(serde_json::from_str::<Rebind>(
            r#"{"install_id":"i","context_id":"c","connection_id":"x","expected_revision":"1"}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Bind>(
            r#"{"install_id":"i","context_id":"c","connection_id":"x","request_id":"r"}"#
        )
        .is_ok());
    }
}
