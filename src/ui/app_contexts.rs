//! App-owned context settings; every read and write requires operator proof.
use super::{err_response, json_response, read_body, HttpResp};
use crate::{client, error::Error};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::BTreeMap, path::Path};
use tiny_http::{Header, Request};

const BODY_CAP: u64 = 48 * 1024;
// At most 100 contexts with individually bounded 32KiB configuration.
const RESULT_CAP: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) enum Route<'a> {
    List(&'a str),
    Show(&'a str, &'a str),
    Update(&'a str, &'a str),
    Archive(&'a str, &'a str),
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
    let Some(context) = parts.next() else {
        return Some(Route::List(install));
    };
    if !segment(context) {
        return None;
    }
    let route = match parts.next() {
        None => Route::Show(install, context),
        Some("update") => Route::Update(install, context),
        Some("archive") => Route::Archive(install, context),
        _ => return None,
    };
    parts.next().is_none().then_some(route)
}
impl Route<'_> {
    pub(super) fn is_read(self) -> bool {
        matches!(self, Self::List(_) | Self::Show(_, _))
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Create {
    label: String,
    input_defaults: BTreeMap<String, String>,
    request_id: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Update {
    expected_revision: u64,
    label: String,
    input_defaults: BTreeMap<String, String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Archive {
    expected_revision: u64,
}
fn typed<T: serde::de::DeserializeOwned>(request: &mut Request) -> Result<T, HttpResp> {
    let bytes = read_body(request, BODY_CAP)?;
    serde_json::from_slice(&bytes)
        .map_err(|_| err_response(400, "invalid app context request schema"))
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
        return err_response(400, "app context query parameters are unsupported");
    }
    let (method, params) = match route {
        Route::List(install) if !write => ("app_context_list", json!({"install_id":install})),
        Route::List(install) => {
            let body: Create = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let mut params = serde_json::to_value(body).expect("typed context create serializes");
            params["install_id"] = json!(install);
            ("app_context_create", params)
        }
        Route::Show(install, context) => (
            "app_context_show",
            json!({"install_id":install,"context_id":context}),
        ),
        Route::Update(install, context) => {
            let body: Update = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let mut params = serde_json::to_value(body).expect("typed context update serializes");
            params["install_id"] = json!(install);
            params["context_id"] = json!(context);
            ("app_context_update", params)
        }
        Route::Archive(install, context) => {
            let body: Archive = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let mut params = serde_json::to_value(body).expect("typed context archive serializes");
            params["install_id"] = json!(install);
            params["context_id"] = json!(context);
            ("app_context_archive", params)
        }
    };
    match client::rpc(state, method, params) {
        Ok(value) if value.to_string().len() <= RESULT_CAP => {
            let mut response = json_response(value);
            response.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
            response
        }
        Ok(_) => err_response(502, "app context receipt exceeds bound"),
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
            err_response(code, "app context management refused or unavailable")
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routes_and_revision_schema_are_exact() {
        assert!(matches!(
            route("/api/app-installations/install-a/contexts/context-b/archive"),
            Some(Route::Archive("install-a", "context-b"))
        ));
        for path in [
            "/api/app-installations/install-a/contexts/",
            "/api/app-installations/../contexts",
            "/api/app-installations/install-a/contexts/context-b/archive/extra",
            "/api/app-installations/install-a/contexts/context-b/rebind",
        ] {
            assert!(route(path).is_none());
        }
        for body in [
            r#"{"expected_revision":null}"#,
            r#"{"expected_revision":1,"expected_revision":2}"#,
            r#"{"expected_revision":1,"context_id":"other"}"#,
            r#"{"expected_revision":-1}"#,
        ] {
            assert!(serde_json::from_str::<Archive>(body).is_err());
        }
        assert!(serde_json::from_str::<Create>(
            r#"{"label":"A","input_defaults":{"source":null},"request_id":"a"}"#
        )
        .is_err());
    }
}
