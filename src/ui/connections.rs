//! Strict proven-operator account management. No grants, execution or discovery.
use super::{err_response, json_response, read_body, HttpResp};
use crate::{client, error::Error};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use tiny_http::{Header, Request};

const BODY_CAP: u64 = 16 * 1024;
const RESULT_CAP: usize = 64 * 1024;
#[derive(Clone, Copy)]
pub(super) enum Route<'a> {
    Providers,
    List,
    Show(&'a str),
    Rotate(&'a str),
    Revoke(&'a str),
    Check(&'a str),
}
fn segment(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub(super) fn route(path: &str) -> Option<Route<'_>> {
    if path == "/api/connection-providers" {
        return Some(Route::Providers);
    }
    if path == "/api/connections" {
        return Some(Route::List);
    }
    let tail = path.strip_prefix("/api/connections/")?;
    if let Some((id, verb)) = tail.split_once('/') {
        if !segment(id) {
            return None;
        }
        return match verb {
            "rotate" => Some(Route::Rotate(id)),
            "revoke" => Some(Route::Revoke(id)),
            "status" => Some(Route::Check(id)),
            _ => None,
        };
    }
    segment(tail).then_some(Route::Show(tail))
}
impl Route<'_> {
    pub(super) fn is_read(self) -> bool {
        matches!(self, Self::Providers | Self::List | Self::Show(_))
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Create {
    provider: String,
    account: String,
    shape: String,
    token: String,
    scopes: Vec<String>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    accept_same_uid_risk: Option<bool>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Rotate {
    token: String,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    scopes: Option<Vec<String>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    accept_same_uid_risk: Option<bool>,
}
fn present<'de, D, T>(de: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(de).map(Some)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
fn typed<T: serde::de::DeserializeOwned>(request: &mut Request) -> Result<T, HttpResp> {
    let body = read_body(request, BODY_CAP)?;
    serde_json::from_slice(&body)
        .map_err(|_| err_response(400, "invalid connection request schema"))
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
        return err_response(400, "connection query parameters are unsupported");
    }
    let (method, params) = match route {
        Route::Providers => ("connection_providers", json!({})),
        Route::List if !write => ("connection_list", json!({})),
        Route::List => {
            let body: Create = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            (
                "connection_create",
                serde_json::to_value(body).expect("typed request serializes"),
            )
        }
        Route::Show(id) => ("connection_show", json!({"connection_id":id})),
        Route::Rotate(id) => {
            let body: Rotate = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let mut value = serde_json::to_value(body).expect("typed request serializes");
            value["connection_id"] = json!(id);
            ("connection_rotate", value)
        }
        Route::Revoke(id) | Route::Check(id) => {
            if let Err(response) = typed::<Empty>(request) {
                return response;
            }
            (
                if matches!(route, Route::Revoke(_)) {
                    "connection_revoke"
                } else {
                    "connection_check"
                },
                json!({"connection_id":id}),
            )
        }
    };
    match client::rpc(state, method, params.clone()) {
        Ok(value) => {
            let output = value.to_string();
            if output.len() > RESULT_CAP
                || params["token"]
                    .as_str()
                    .is_some_and(|token| !token.trim().is_empty() && output.contains(token.trim()))
            {
                return err_response(502, "invalid connection management receipt");
            }
            let mut response = json_response(value);
            response.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
            response
        }
        Err(error) => {
            // Neither serde unknown-field diagnostics nor downstream errors
            // may reflect a credential-bearing request into an HTTP response.
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
            err_response(code, "connection management refused or unavailable")
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_routes_and_strict_credential_schema() {
        assert!(matches!(
            route("/api/connections/c-1/status"),
            Some(Route::Check("c-1"))
        ));
        for p in [
            "/api/connections/",
            "/api/connections/../rotate",
            "/api/connections/id/rotate/extra",
            "/api/connections/id/bind",
            "/api/connection-providers/extra",
        ] {
            assert!(route(p).is_none());
        }
        assert!(serde_json::from_str::<Rotate>(r#"{"token":"a","token":"b"}"#).is_err());
        assert!(serde_json::from_str::<Rotate>(r#"{"token":"a","account":"other"}"#).is_err());
        assert!(serde_json::from_str::<Rotate>(r#"{"token":"a","scopes":null}"#).is_err());
        assert!(
            serde_json::from_str::<Rotate>(r#"{"token":"a","accept_same_uid_risk":null}"#).is_err()
        );
        assert!(serde_json::from_str::<Empty>(r#"{"connection_id":"other"}"#).is_err());
        assert!(serde_json::from_str::<Create>(
            r#"{"provider":"fixture","account":"a","shape":"token","token":"x","scopes":"read"}"#
        )
        .is_err());
    }
}
