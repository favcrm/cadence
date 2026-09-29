//! Per-installation customer record reads and writes; every route
//! requires operator proof.
//!
//! The board relays the daemon's `app_record_*` actions with the URL's
//! installation, context and record IDs as authority — body fields can
//! never claim identity (`by`, `actor`), routing (`workspace`) or
//! discovery links (`project`, `project_link`), nor smuggle the URL IDs
//! themselves. Errors are bounded generics that never echo customer
//! content; paths and SQL never leave the daemon.
use super::{err_response, json_response, read_body, HttpResp};
use crate::{client, error::Error};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use tiny_http::{Header, Request};

const BODY_CAP: u64 = 48 * 1024;
// CSV preview/import carry bounded CSV text (daemon bound 256KiB)
// plus a small JSON envelope; reads stay on the small cap.
const CSV_BODY_CAP: u64 = 320 * 1024;
// A context holds at most 100 records of at most 16KiB each.
const RESULT_CAP: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) enum Route<'a> {
    List(&'a str, &'a str),
    Show(&'a str, &'a str, &'a str),
    Update(&'a str, &'a str, &'a str),
    /// POST-only CSV preview: a read with a body, so it rides the
    /// write transport while the daemon itself writes nothing.
    CsvPreview(&'a str, &'a str),
    CsvImport(&'a str, &'a str),
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
    if !segment(context) || parts.next()? != "records" {
        return None;
    }
    let Some(record) = parts.next() else {
        return Some(Route::List(install, context));
    };
    if record.is_empty() {
        return None;
    }
    // Reserved bulk verbs precede record IDs: the ids `csv-preview`
    // and `csv-import` are unaddressable over HTTP (RPC still serves
    // them) so a bulk POST can never create or read a record, and
    // suffixed paths under them never resolve.
    match record {
        "csv-preview" | "csv-import" => {
            if parts.next().is_some() {
                return None;
            }
            return match record {
                "csv-preview" => Some(Route::CsvPreview(install, context)),
                _ => Some(Route::CsvImport(install, context)),
            };
        }
        _ => {}
    }
    if !segment(record) {
        return None;
    }
    let route = match parts.next() {
        None => Route::Show(install, context, record),
        Some("update") => Route::Update(install, context, record),
        _ => return None,
    };
    parts.next().is_none().then_some(route)
}

impl Route<'_> {
    pub(super) fn is_read(self) -> bool {
        matches!(self, Self::List(..) | Self::Show(..))
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Create {
    record_id: String,
    profile: Value,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Update {
    expected_revision: u64,
    profile: Value,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CsvPreview {
    csv_text: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CsvDecision {
    row: u64,
    action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_revision: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CsvImport {
    csv_text: String,
    preview_token: String,
    request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decisions: Option<Vec<CsvDecision>>,
}

fn typed<T: serde::de::DeserializeOwned>(request: &mut Request) -> Result<T, HttpResp> {
    typed_cap(request, BODY_CAP)
}

fn typed_cap<T: serde::de::DeserializeOwned>(
    request: &mut Request,
    cap: u64,
) -> Result<T, HttpResp> {
    let bytes = read_body(request, cap)?;
    serde_json::from_slice(&bytes)
        .map_err(|_| err_response(400, "invalid app record request schema"))
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
        return err_response(400, "app record query parameters are unsupported");
    }
    let (method, params) = match route {
        Route::List(install, context) if !write => (
            "app_record_list",
            json!({"install_id": install, "context_id": context}),
        ),
        Route::List(install, context) => {
            let body: Create = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let mut params = json!({"install_id": install, "context_id": context});
            params["record_id"] = Value::String(body.record_id);
            params["profile"] = body.profile;
            ("app_record_create", params)
        }
        Route::Show(install, context, record) => (
            "app_record_show",
            json!({"install_id": install, "context_id": context, "record_id": record}),
        ),
        Route::Update(install, context, record) => {
            let body: Update = match typed(request) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let revision: i64 = match i64::try_from(body.expected_revision) {
                Ok(revision) if revision > 0 => revision,
                _ => return err_response(400, "invalid app record request schema"),
            };
            let mut params = json!({
                "install_id": install,
                "context_id": context,
                "record_id": record,
                "expected_revision": revision,
            });
            params["profile"] = body.profile;
            ("app_record_update", params)
        }
        Route::CsvPreview(install, context) => {
            let body: CsvPreview = match typed_cap(request, CSV_BODY_CAP) {
                Ok(body) => body,
                Err(response) => return response,
            };
            (
                "app_record_csv_preview",
                json!({"install_id": install, "context_id": context, "csv_text": body.csv_text}),
            )
        }
        Route::CsvImport(install, context) => {
            let body: CsvImport = match typed_cap(request, CSV_BODY_CAP) {
                Ok(body) => body,
                Err(response) => return response,
            };
            let mut params = json!({
                "install_id": install,
                "context_id": context,
                "csv_text": body.csv_text,
                "preview_token": body.preview_token,
                "request_id": body.request_id,
            });
            if let Some(decisions) = body.decisions {
                let mut list = Vec::with_capacity(decisions.len());
                for item in &decisions {
                    let revision: Option<i64> = match item.expected_revision {
                        None => None,
                        Some(revision) => match i64::try_from(revision) {
                            Ok(revision) if revision > 0 => Some(revision),
                            _ => return err_response(400, "invalid app record request schema"),
                        },
                    };
                    let mut entry = json!({"row": item.row, "action": item.action});
                    if let Some(revision) = revision {
                        entry["expected_revision"] = revision.into();
                    }
                    list.push(entry);
                }
                params["decisions"] = Value::Array(list);
            }
            ("app_record_csv_import", params)
        }
    };
    match client::rpc(state, method, params) {
        Ok(value) if value.to_string().len() <= RESULT_CAP => {
            let mut response = json_response(value);
            response.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
            response
        }
        Ok(_) => err_response(502, "app record receipt exceeds bound"),
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
            err_response(code, "app record management refused or unavailable")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routes_and_revision_schema_are_exact() {
        assert!(matches!(
            route("/api/app-installations/install-a/contexts/context-b/records"),
            Some(Route::List("install-a", "context-b"))
        ));
        assert!(matches!(
            route("/api/app-installations/install-a/contexts/context-b/records/customer-1"),
            Some(Route::Show("install-a", "context-b", "customer-1"))
        ));
        assert!(matches!(
            route("/api/app-installations/install-a/contexts/context-b/records/customer-1/update"),
            Some(Route::Update("install-a", "context-b", "customer-1"))
        ));
        assert!(matches!(
            route("/api/app-installations/install-a/contexts/context-b/records/csv-preview"),
            Some(Route::CsvPreview("install-a", "context-b"))
        ));
        assert!(matches!(
            route("/api/app-installations/install-a/contexts/context-b/records/csv-import"),
            Some(Route::CsvImport("install-a", "context-b"))
        ));
        // The bulk verbs are POST-only transport: never reads.
        assert!(
            !route("/api/app-installations/i/contexts/c/records/csv-preview")
                .unwrap()
                .is_read()
        );
        assert!(
            !route("/api/app-installations/i/contexts/c/records/csv-import")
                .unwrap()
                .is_read()
        );
        for path in [
            "/api/app-installations/install-a/records",
            "/api/app-installations/install-a/contexts/",
            "/api/app-installations/install-a/contexts/context-b/records/",
            "/api/app-installations/install-a/contexts/context-b/records/customer-1/update/extra",
            "/api/app-installations/install-a/contexts/context-b/records/customer-1/archive",
            "/api/app-installations/install-a/contexts/context-b/records/csv-preview/extra",
            "/api/app-installations/install-a/contexts/context-b/records/csv-import/extra",
            "/api/app-installations/../contexts",
            "/api/app-records",
        ] {
            assert!(route(path).is_none(), "route admitted {path}");
        }
        for body in [
            r#"{"expected_revision":null,"profile":{}}"#,
            r#"{"expected_revision":1,"expected_revision":2}"#,
            r#"{"expected_revision":1,"profile":{},"record_id":"other"}"#,
            r#"{"expected_revision":-1,"profile":{}}"#,
            r#"{"expected_revision":1,"profile":{},"by":"operator"}"#,
            r#"{"expected_revision":1,"profile":{},"project":"client"}"#,
        ] {
            assert!(
                serde_json::from_str::<Update>(body).is_err(),
                "update admitted {body}"
            );
        }
        for body in [
            r#"{"record_id":"a","profile":null}"#,
            r#"{"record_id":"a","profile":{},"install_id":"other"}"#,
            r#"{"record_id":"a","profile":{},"context_id":"other"}"#,
            r#"{"record_id":"a","profile":{},"expected_revision":1}"#,
            r#"{"profile":{}}"#,
        ] {
            // `profile: null` fails only at the daemon's typed parse;
            // the transport grammar admits any JSON value there.
            if body.contains("null") {
                assert!(serde_json::from_str::<Create>(body).is_ok());
            } else {
                assert!(
                    serde_json::from_str::<Create>(body).is_err(),
                    "create admitted {body}"
                );
            }
        }
        // CSV transport bodies carry no identity: URL segments are the
        // authority, and every extra field refuses at the transport.
        assert!(serde_json::from_str::<CsvPreview>(r#"{"csv_text":"a,b"}"#).is_ok());
        for body in [
            r#"{"csv_text":"a,b","install_id":"other"}"#,
            r#"{"csv_text":"a,b","context_id":"other"}"#,
            r#"{"csv_text":"a,b","by":"operator"}"#,
            r#"{"csv_text":"a,b","project":"client"}"#,
            r#"{}"#,
        ] {
            assert!(
                serde_json::from_str::<CsvPreview>(body).is_err(),
                "csv preview admitted {body}"
            );
        }
        assert!(serde_json::from_str::<CsvImport>(
            r#"{"csv_text":"a","preview_token":"sha256:x","request_id":"req-1","decisions":[{"row":1,"action":"update","expected_revision":2}]}"#
        )
        .is_ok());
        for body in [
            r#"{"csv_text":"a","preview_token":"sha256:x","request_id":"req-1","install_id":"other"}"#,
            r#"{"csv_text":"a","preview_token":"sha256:x","request_id":"req-1","actor":"operator"}"#,
            r#"{"csv_text":"a","preview_token":"sha256:x","request_id":"req-1","decisions":[{"row":1,"action":"merge"}]}"#,
            r#"{"csv_text":"a","preview_token":"sha256:x"}"#,
        ] {
            // Unknown actions pass the transport grammar (the daemon
            // names the allowed set); unknown fields never do.
            if body.contains("merge") {
                assert!(serde_json::from_str::<CsvImport>(body).is_ok());
            } else {
                assert!(
                    serde_json::from_str::<CsvImport>(body).is_err(),
                    "csv import admitted {body}"
                );
            }
        }
    }
}
