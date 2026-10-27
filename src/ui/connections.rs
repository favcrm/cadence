//! Strict proven-operator account management. No grants, execution or discovery.
use super::{err_response, json_response, read_body, HttpResp};
use crate::{client, error::Error};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;
use tiny_http::{Header, Request, Response, StatusCode};

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
    Test(&'a str),
    Custody(&'a str),
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
    if let Some(verb) = path.strip_prefix("/api/connection-custody/") {
        return match verb {
            "initialize" | "unlock" | "lock" | "status" => Some(Route::Custody(verb)),
            _ => None,
        };
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
            "test" => Some(Route::Test(id)),
            _ => None,
        };
    }
    segment(tail).then_some(Route::Show(tail))
}
impl Route<'_> {
    pub(super) fn is_read(self) -> bool {
        matches!(
            self,
            Self::Providers | Self::List | Self::Show(_) | Self::Custody("status")
        )
    }
}
// CAD-785: the create/rotate bodies carry either the opaque token
// shape or the typed SMTP shape (host, port, TLS mode, username,
// secret, sender). Every field is optional at the peer: the body
// forwards only present fields and the daemon's per-shape grammar
// is the authority (a token shape with SMTP fields, or an SMTP
// shape with a token, refuses there). `deny_unknown_fields` still
// closes the schema here.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Custody {
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    accept_same_uid_risk: Option<bool>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Revoke {
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    expected_connection_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    expected_revision: Option<u64>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Create {
    provider: String,
    account: String,
    shape: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tls_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sender: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sender_name: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tls_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sender: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sender_name: Option<String>,
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
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    expected_connection_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    expected_revision: Option<u64>,
}
fn present<'de, D, T>(de: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(de).map(Some)
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Test {
    #[serde(deserialize_with = "required_nullable_revision")]
    expected_revision: Option<u64>,
    #[serde(deserialize_with = "required_nullable_digest")]
    expected_registration_digest: Option<String>,
}

fn required_nullable_revision<'de, D>(de: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<serde_json::Value>::deserialize(de)? {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(n)) => n
            .as_u64()
            .filter(|value| *value > 0)
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("invalid expected_revision")),
        Some(_) => Err(serde::de::Error::custom("invalid expected_revision")),
    }
}

fn required_nullable_digest<'de, D>(de: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<serde_json::Value>::deserialize(de)? {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => {
            let rest = s
                .strip_prefix("sha256:")
                .ok_or_else(|| serde::de::Error::custom("invalid expected_registration_digest"))?;
            if rest.len() != 64
                || !rest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err(serde::de::Error::custom(
                    "invalid expected_registration_digest",
                ));
            }
            Ok(Some(s))
        }
        Some(_) => Err(serde::de::Error::custom(
            "invalid expected_registration_digest",
        )),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
fn typed<T: serde::de::DeserializeOwned>(request: &mut Request) -> Result<T, HttpResp> {
    let body = read_body(request, BODY_CAP)?;
    serde_json::from_slice(&body)
        .map_err(|_| err_response(400, "invalid connection request schema"))
}
// Legacy forms may omit expectations. Resolve ONCE for this invocation;
// explicitly supplied fields are never reread, substituted or retried.
fn expectations(
    state: &Path,
    id: &str,
    mut params: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    if params.get("expected_connection_id").is_none() && params.get("expected_revision").is_none() {
        let shown = client::rpc(state, "connection_show", json!({"connection_id":id}))?;
        if shown["connection"]["id"].as_str() != Some(id) {
            return Err(Error::rejected("connection preflight identity mismatch"));
        }
        let revision = shown["connection"]["revision"]
            .as_u64()
            .filter(|v| *v > 0 && *v <= i64::MAX as u64)
            .ok_or_else(|| Error::rejected("connection preflight revision unavailable"))?;
        params["expected_connection_id"] = json!(id);
        params["expected_revision"] = json!(revision);
    }
    Ok(params)
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
        Route::Custody(verb) => {
            let method = match verb {
                "initialize" => "connection_custody_initialize",
                "unlock" => "connection_custody_unlock",
                "lock" => "connection_custody_lock",
                "status" => "connection_custody_status",
                _ => return err_response(400, "invalid custody operation"),
            };
            if matches!(verb, "initialize" | "unlock") {
                let body: Custody = match typed(request) {
                    Ok(body) => body,
                    Err(response) => return response,
                };
                (
                    method,
                    serde_json::to_value(body).expect("typed request serializes"),
                )
            } else {
                // GET status normally has no body; any supplied body still
                // has to satisfy the strict empty schema.
                let body = match read_body(request, BODY_CAP) {
                    Ok(body) => body,
                    Err(response) => return response,
                };
                if !body.is_empty() && serde_json::from_slice::<Empty>(&body).is_err() {
                    return err_response(400, "invalid connection request schema");
                }
                (method, json!({}))
            }
        }
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
        Route::Revoke(id) => {
            let body: Revoke = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let mut params = serde_json::to_value(body).expect("typed request serializes");
            params["connection_id"] = json!(id);
            ("connection_revoke", params)
        }
        Route::Check(id) => {
            if let Err(response) = typed::<Empty>(request) {
                return response;
            }
            ("connection_check", json!({"connection_id":id}))
        }
        Route::Test(id) => {
            let body: Test = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            (
                "connection_test",
                json!({
                    "connection_id": id,
                    "expected_revision": body.expected_revision,
                    "expected_registration_digest": body.expected_registration_digest,
                }),
            )
        }
    };
    let params = if matches!(route, Route::Rotate(_) | Route::Revoke(_)) {
        let id = params["connection_id"].as_str().expect("typed route ID");
        match expectations(state, id, params.clone()) {
            Ok(params) => params,
            Err(_) => return err_response(409, "connection preflight refused or unavailable"),
        }
    } else {
        params
    };
    match client::rpc(state, method, params.clone()) {
        Ok(value) => {
            let output = value.to_string();
            // Neither credential shape may echo in a receipt: the
            // opaque token and the SMTP secret are both screened.
            let leaks = [params["token"].as_str(), params["secret"].as_str()]
                .into_iter()
                .flatten()
                .any(|secret| !secret.trim().is_empty() && output.contains(secret.trim()));
            if output.len() > RESULT_CAP || leaks {
                return err_response(502, "invalid connection management receipt");
            }
            let mut response = json_response(value);
            response.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
            response
        }
        Err(error) => {
            // Neither serde unknown-field diagnostics nor downstream errors
            // may reflect a credential-bearing request into an HTTP response —
            // the daemon's message is never relayed verbatim (a custody error
            // can name an account). The structured `code` is stable wire
            // metadata, so the board maps it to a safe, actionable refusal and
            // echoes the code itself for the UI to key on. An unmapped code
            // keeps the historic opaque body — better a vague refusal than a
            // reflected one.
            let operator_refusal = error.to_string().contains("operator action")
                || error.to_string().contains("not provably the operator");
            if operator_refusal {
                return err_response(403, "connection management requires the signed-in operator");
            }
            // The connection path emits exactly ONE coded refusal on
            // the wire — `custody_unprotected` (`Error::invalid` in
            // `platform_rpc`'s enroll gate). `revision_conflict` is
            // never produced here (the connection CAS refuses plain
            // `Error::rejected`), so it is deliberately NOT mapped:
            // inventing a friendly conflict message would misdiagnose.
            // Every other failure keeps the historic opaque body — a
            // vague refusal beats a reflected one (a custody or host
            // error can name an account).
            let (code, message): (&str, &str) = match error.code() {
                Some("custody_unprotected") => (
                    "custody_unprotected",
                    "the daemon refused to store the credential: custody is not isolated from managed agents — enrol only if you accept the same-user custody risk (the form's custody-risk acceptance), or isolate custody first",
                ),
                // CAD-1126: a hosted SMTP enrolment is verified live; its
                // refusals carry a typed `smtp_*` code and the board speaks
                // fixed words per code, never the daemon's text.
                Some(code)
                    if crate::platform::smtp_internal::wire_message(code).is_some() =>
                {
                    let message = crate::platform::smtp_internal::wire_message(code)
                        .unwrap_or_default();
                    let body = serde_json::to_vec_pretty(&json!({"error": message, "code": code}))
                        .unwrap_or_default();
                    let mut resp = Response::from_data(body).with_status_code(StatusCode(409));
                    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                    return resp;
                }
                _ => {
                    let status = match error {
                        Error::Rejected(_) | Error::Structured(_) => 409,
                        _ => 503,
                    };
                    return err_response(status, "connection management refused or unavailable");
                }
            };
            let body = serde_json::to_vec_pretty(&json!({"error": message, "code": code}))
                .unwrap_or_default();
            let mut resp = Response::from_data(body).with_status_code(StatusCode(409));
            resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
            resp
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

    // Independently drafted client3 schema checks; native HTTP authority is
    // a separate real-peer acceptance obligation, not proved by these.
    #[test]
    fn rotate_revoke_typed_expectation_schema() {
        assert!(serde_json::from_str::<Rotate>(
            r#"{"token":"t","expected_connection_id":"conn-a","expected_revision":3}"#
        )
        .is_ok());
        assert!(serde_json::from_str::<Revoke>(
            r#"{"expected_connection_id":"conn-a","expected_revision":3}"#
        )
        .is_ok());
        // Empty revoke body parses — legacy revoke needs no expectation.
        assert!(serde_json::from_str::<Revoke>(r#"{}"#).is_ok());
        for bad in [
            r#"{"token":"t","expected_revision":"3"}"#,
            r#"{"token":"t","expected_revision":3,"bogus":1}"#,
            r#"{"expected_revision":3,"expected_revision":4}"#,
        ] {
            assert!(serde_json::from_str::<Rotate>(bad).is_err(), "{bad}");
        }
        assert!(serde_json::from_str::<Revoke>(r#"{"expected_revision":null}"#).is_err());
    }

    #[test]
    fn custody_body_is_acknowledgement_only() {
        assert!(serde_json::from_str::<Custody>(r#"{"accept_same_uid_risk":true}"#).is_ok());
        assert!(serde_json::from_str::<Custody>(r#"{}"#).is_ok());
        for bad in [
            r#"{"key_file":"/x"}"#,
            r#"{"secret":"s"}"#,
            r#"{"accept_same_uid_risk":"yes"}"#,
        ] {
            assert!(serde_json::from_str::<Custody>(bad).is_err(), "{bad}");
        }
    }
}
