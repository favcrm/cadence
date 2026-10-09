//! Operator controls for app bindings and reviewed artifact release.
use super::{err_response, json_response, read_body, HttpResp};
use crate::{client, error::Error};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use tiny_http::{Header, Request};
const BODY_CAP: u64 = 48 * 1024;
const RESULT_CAP: usize = 4 * 1024 * 1024;
#[derive(Clone, Copy)]
pub(super) enum Route<'a> {
    Bindings(&'a str, Option<&'a str>),
    Binding(&'a str, &'a str),
    Quote(&'a str, Option<&'a str>, &'a str),
    Update(&'a str, &'a str),
    Revoke(&'a str, &'a str),
    Stage(&'a str),
    Effects(Option<&'a str>, Option<&'a str>),
    Effect(&'a str),
    Decide(&'a str),
    PublishNow(&'a str),
    Resolve(&'a str),
}
fn segment(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
pub(super) fn route(path: &str) -> Option<Route<'_>> {
    if path == "/api/app-effects" {
        return Some(Route::Effects(None, None));
    }
    if let Some(tail) = path.strip_prefix("/api/app-effects/") {
        let parts: Vec<_> = tail.split('/').collect();
        return match parts.as_slice() {
            [id] if segment(id) => Some(Route::Effect(id)),
            [id, "decide"] if segment(id) => Some(Route::Decide(id)),
            [id, "publish-now"] if segment(id) => Some(Route::PublishNow(id)),
            [id, "resolve"] if segment(id) => Some(Route::Resolve(id)),
            _ => None,
        };
    }
    if let Some(tail) = path.strip_prefix("/api/app-runs/") {
        let (run, verb) = tail.split_once('/')?;
        return (segment(run) && verb == "effects").then_some(Route::Stage(run));
    }
    let parts: Vec<_> = path
        .strip_prefix("/api/app-installations/")?
        .split('/')
        .collect();
    match parts.as_slice() {
        [install, "bindings"] if segment(install) => Some(Route::Bindings(install, None)),
        [install, "bindings", binding] if segment(install) && segment(binding) => {
            Some(Route::Binding(install, binding))
        }
        [install, "bindings", slot, "quote"] if segment(install) && segment(slot) => {
            Some(Route::Quote(install, None, slot))
        }
        [install, "bindings", binding, "update"] if segment(install) && segment(binding) => {
            Some(Route::Update(install, binding))
        }
        [install, "bindings", binding, "revoke"] if segment(install) && segment(binding) => {
            Some(Route::Revoke(install, binding))
        }
        [install, "effects"] if segment(install) => Some(Route::Effects(Some(install), None)),
        [install, "contexts", context, "bindings"] if segment(install) && segment(context) => {
            Some(Route::Bindings(install, Some(context)))
        }
        [install, "contexts", context, "bindings", slot, "quote"]
            if segment(install) && segment(context) && segment(slot) =>
        {
            Some(Route::Quote(install, Some(context), slot))
        }
        [install, "contexts", context, "effects"] if segment(install) && segment(context) => {
            Some(Route::Effects(Some(install), Some(context)))
        }
        _ => None,
    }
}
impl Route<'_> {
    pub(super) fn is_read(self) -> bool {
        matches!(
            self,
            Self::Bindings(..)
                | Self::Binding(..)
                | Self::Quote(..)
                | Self::Effects(..)
                | Self::Effect(_)
        )
    }
    pub(super) fn is_write(self) -> bool {
        matches!(
            self,
            Self::Bindings(_, None)
                | Self::Update(..)
                | Self::Revoke(..)
                | Self::Stage(_)
                | Self::Decide(_)
                | Self::PublishNow(_)
                | Self::Resolve(_)
        )
    }
}
fn nonnull_context<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Create {
    slot: String,
    connection_id: String,
    request_id: String,
    #[serde(
        default,
        deserialize_with = "nonnull_context",
        skip_serializing_if = "Option::is_none"
    )]
    context_id: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Update {
    expected_revision: u64,
    connection_id: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Revoke {
    expected_revision: u64,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Stage {
    artifact_id: String,
    slot: String,
    request_id: String,
    title: String,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Choice {
    Accept,
    Decline,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    digest: String,
    decision: Choice,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PublishNow {
    digest: String,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Resolution {
    Close,
    Acknowledge,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Resolve {
    digest: String,
    resolution: Resolution,
}
fn typed<T: serde::de::DeserializeOwned + Serialize>(
    request: &mut Request,
) -> Result<Value, HttpResp> {
    let bytes = read_body(request, BODY_CAP)?;
    let value: T = serde_json::from_slice(&bytes)
        .map_err(|_| err_response(400, "invalid app release request schema"))?;
    Ok(serde_json::to_value(value).expect("typed release request serializes"))
}
fn scoped(install: Option<&str>, context: Option<&str>) -> Value {
    let mut value = json!({});
    if let Some(id) = install {
        value["install_id"] = json!(id);
    }
    if let Some(id) = context {
        value["context_id"] = json!(id);
    }
    value
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
        return err_response(400, "app release query parameters are unsupported");
    }
    let result: Result<(&str, Value), HttpResp> = (|| {
        Ok(match route {
            Route::Bindings(install, context) if !write => {
                ("app_binding_list", scoped(Some(install), context))
            }
            Route::Bindings(install, None) => {
                let mut params = typed::<Create>(request)?;
                params["install_id"] = json!(install);
                ("app_binding_create", params)
            }
            Route::Binding(install, binding) => (
                "app_binding_show",
                json!({"install_id":install,"binding_id":binding}),
            ),
            Route::Quote(install, context, slot) => {
                let mut params = scoped(Some(install), context);
                params["slot"] = json!(slot);
                ("app_binding_quote", params)
            }
            Route::Update(install, binding) => {
                let mut params = typed::<Update>(request)?;
                params["install_id"] = json!(install);
                params["binding_id"] = json!(binding);
                ("app_binding_update", params)
            }
            Route::Revoke(install, binding) => {
                let mut params = typed::<Revoke>(request)?;
                params["install_id"] = json!(install);
                params["binding_id"] = json!(binding);
                ("app_binding_revoke", params)
            }
            Route::Stage(run) => {
                let mut params = typed::<Stage>(request)?;
                params["run_id"] = json!(run);
                ("app_effect_stage", params)
            }
            Route::Effects(install, context) => ("app_effect_list", scoped(install, context)),
            Route::Effect(effect) => ("app_effect_show", json!({"effect_id":effect})),
            Route::Decide(effect) => {
                let mut params = typed::<Decision>(request)?;
                params["effect_id"] = json!(effect);
                ("app_effect_decide", params)
            }
            Route::PublishNow(effect) => {
                let mut params = typed::<PublishNow>(request)?;
                params["effect_id"] = json!(effect);
                ("app_effect_publish_now", params)
            }
            Route::Resolve(effect) => {
                let mut params = typed::<Resolve>(request)?;
                params["effect_id"] = json!(effect);
                ("app_effect_resolve", params)
            }
            Route::Bindings(_, Some(_)) => return Err(err_response(405, "method not allowed")),
        })
    })();
    let (method, params) = match result {
        Ok(value) => value,
        Err(response) => return response,
    };
    match client::rpc(state, method, params) {
        Ok(value) if value.to_string().len() <= RESULT_CAP => {
            let mut response = json_response(value);
            response.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
            response
        }
        Ok(_) => err_response(502, "app release receipt exceeds bound"),
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
            // CAD-1096: a quote refusal names the door's sanitized code.
            let reason = match &error {
                Error::Rejected(message) if code == 409 => {
                    crate::daemon::operator_price_refusal(message)
                }
                _ => None,
            };
            err_response(
                code,
                reason.unwrap_or("app release management refused or unavailable"),
            )
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_routes_are_exact_and_do_not_infer_context_or_project() {
        assert!(matches!(
            route("/api/app-installations/install-a/bindings"),
            Some(Route::Bindings("install-a", None))
        ));
        assert!(matches!(
            route("/api/app-installations/install-a/bindings/source/quote"),
            Some(Route::Quote("install-a", None, "source"))
        ));
        assert!(matches!(
            route("/api/app-installations/install-a/contexts/brand-a/bindings/source/quote"),
            Some(Route::Quote("install-a", Some("brand-a"), "source"))
        ));
        assert!(matches!(
            route("/api/app-installations/install-a/contexts/context-a/effects"),
            Some(Route::Effects(Some("install-a"), Some("context-a")))
        ));
        assert!(matches!(
            route("/api/app-runs/run-a/effects"),
            Some(Route::Stage("run-a"))
        ));
        assert!(matches!(
            route("/api/app-effects/effect-a/decide"),
            Some(Route::Decide("effect-a"))
        ));
        for path in [
            "/api/app-installations/../bindings",
            "/api/app-installations/i/bindings/",
            "/api/app-installations/i/bindings/b/update/extra",
            "/api/app-runs/r/effects/e",
            "/api/app-effects/e/retry",
            "/api/app-effects/",
            "/api/app-installations/i/contexts/c/bindings/b",
            "/api/app-installations/i/bindings/source/quote/other",
            "/api/app-installations/i/contexts/c/bindings/source/quote/other",
        ] {
            assert!(route(path).is_none(), "admitted {path}");
        }
    }
    #[test]
    fn release_schemas_reject_forged_authority_and_caller_content() {
        let valid = r#"{"slot":"publication","connection_id":"conn-a","request_id":"bind-a"}"#;
        assert!(serde_json::from_str::<Create>(valid).is_ok());
        for body in [
            r#"{"slot":"publication","connection_id":"conn-a","request_id":"bind-a","context_id":null}"#,
            r#"{"slot":"publication","connection_id":"conn-a","request_id":"bind-a","operator":true}"#,
            r#"{"slot":"publication","slot":"other","connection_id":"conn-a","request_id":"bind-a"}"#,
            r#"{"slot":"publication","connection_id":"conn-a","request_id":"bind-a","install_id":"other"}"#,
        ] {
            assert!(
                serde_json::from_str::<Create>(body).is_err(),
                "accepted {body}"
            );
        }
        for body in [
            r#"{"expected_revision":null}"#,
            r#"{"expected_revision":1,"expected_revision":2}"#,
            r#"{"expected_revision":1,"binding_id":"other"}"#,
        ] {
            assert!(serde_json::from_str::<Revoke>(body).is_err());
        }
        assert!(serde_json::from_str::<Stage>(
            r#"{"artifact_id":"a","slot":"publication","request_id":"r","title":"Title"}"#
        )
        .is_ok());
        for field in [
            "body",
            "path",
            "provider",
            "run_id",
            "operator",
            "review_verdict",
            "grant",
        ] {
            let mut body = serde_json::json!({"artifact_id":"a","slot":"publication","request_id":"r","title":"Title"});
            body[field] = serde_json::json!("forged");
            assert!(
                serde_json::from_value::<Stage>(body).is_err(),
                "accepted {field}"
            );
        }
        for body in [
            r#"{"digest":"d","decision":"retry"}"#,
            r#"{"digest":"d","decision":"accept","effect_id":"other"}"#,
            r#"{"digest":"d","digest":"other","decision":"accept"}"#,
        ] {
            assert!(serde_json::from_str::<Decision>(body).is_err());
        }
    }
    #[test]
    fn resolution_schema_cannot_request_execution_or_replace_authority() {
        assert!(matches!(
            route("/api/app-effects/effect-a/resolve"),
            Some(Route::Resolve("effect-a"))
        ));
        assert!(serde_json::from_str::<Resolve>(r#"{"digest":"d","resolution":"close"}"#).is_ok());
        assert!(
            serde_json::from_str::<Resolve>(r#"{"digest":"d","resolution":"acknowledge"}"#).is_ok()
        );
        for body in [
            r#"{"digest":"d","resolution":"retry"}"#,
            r#"{"digest":"d","resolution":"accept"}"#,
            r#"{"digest":"d","resolution":"close","operator":true}"#,
            r#"{"digest":"d","resolution":"close","effect_id":"other"}"#,
            r#"{"digest":"d","digest":"other","resolution":"close"}"#,
        ] {
            assert!(
                serde_json::from_str::<Resolve>(body).is_err(),
                "resolution admitted {body}"
            );
        }
    }
}
