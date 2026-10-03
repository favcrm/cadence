//! Operator-only HTTP for stable-ID local runs. Worker artifact access is RPC-only.
use super::{err_response, home, json_response, parse_json, read_body, HttpResp};
use crate::client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use tiny_http::{Header, Request};

const BODY_CAP: u64 = 48 * 1024;
const ARTIFACT_CAP: usize = 256 * 1024;

#[derive(Clone, Copy)]
pub(super) enum Route<'a> {
    Create,
    /// CAD-1123: create + approve + dispatch from the install team.
    Start,
    /// CAD-1123: the install's default team (GET shows, POST sets).
    Team(&'a str),
    List,
    Show(&'a str),
    Artifact(&'a str),
    CapabilityResults(&'a str),
    CapabilityResult(&'a str),
    /// CAD-1123: the retained image bytes of one receipt (operator read).
    CapabilityAsset(&'a str),
    InstallDecision(&'a str, bool),
    RunDecision(&'a str),
    Cancel(&'a str),
    Dispatch(&'a str),
}
fn segment(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub(super) fn route(path: &str) -> Option<Route<'_>> {
    if path == "/api/app-runs" {
        return Some(Route::List);
    }
    if let Some(id) = path.strip_prefix("/api/app-run-artifacts/") {
        return segment(id).then_some(Route::Artifact(id));
    }
    if let Some(tail) = path.strip_prefix("/api/app-capability-results/") {
        if let Some(id) = tail.strip_suffix("/asset") {
            return segment(id).then_some(Route::CapabilityAsset(id));
        }
        return segment(tail).then_some(Route::CapabilityResult(tail));
    }
    if path == "/api/app-runs/start" {
        return Some(Route::Start);
    }
    if let Some(tail) = path.strip_prefix("/api/app-runs/") {
        if let Some((id, verb)) = tail.split_once('/') {
            if !segment(id) {
                return None;
            }
            return match verb {
                "approve" => Some(Route::RunDecision(id)),
                "cancel" => Some(Route::Cancel(id)),
                "dispatch" => Some(Route::Dispatch(id)),
                "capability-results" => Some(Route::CapabilityResults(id)),
                _ => None,
            };
        }
        return segment(tail).then_some(Route::Show(tail));
    }
    let tail = path.strip_prefix("/api/app-installations/")?;
    let (id, verb) = tail.split_once('/')?;
    if !segment(id) {
        return None;
    }
    match verb {
        "approve" => Some(Route::InstallDecision(id, true)),
        "revoke" => Some(Route::InstallDecision(id, false)),
        "team" => Some(Route::Team(id)),
        _ => None,
    }
}
impl Route<'_> {
    pub(super) fn is_read(self) -> bool {
        matches!(
            self,
            Self::List
                | Self::Show(_)
                | Self::Artifact(_)
                | Self::CapabilityResults(_)
                | Self::CapabilityResult(_)
                | Self::CapabilityAsset(_)
                | Self::Team(_)
        )
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Create {
    install_id: String,
    workflow: String,
    inputs: BTreeMap<String, String>,
    request_id: String,
    owner_pm: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_link: Option<String>,
    #[serde(
        default,
        deserialize_with = "present_context",
        skip_serializing_if = "Option::is_none"
    )]
    context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_receipt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_post_id: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Start {
    install_id: String,
    workflow: String,
    inputs: BTreeMap<String, String>,
    request_id: String,
    expected_quotes: serde_json::Map<String, Value>,
    #[serde(
        default,
        deserialize_with = "present_context",
        skip_serializing_if = "Option::is_none"
    )]
    context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_receipt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_post_id: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TeamSet {
    owner_pm: String,
    roles: BTreeMap<String, String>,
    expected_revision: u64,
}
fn present_context<'de, D>(de: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(de).map(Some)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    digest: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn query(request: &Request, allow_install: bool) -> Result<Value, HttpResp> {
    let raw = request
        .url()
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or("");
    if raw.is_empty() {
        return Ok(json!({}));
    }
    if !allow_install {
        return Err(err_response(400, "query parameters are unsupported"));
    }
    let mut params = serde_json::Map::new();
    for pair in raw.split('&') {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| err_response(400, "malformed app run query"))?;
        let key =
            super::pct_decode(key).ok_or_else(|| err_response(400, "malformed app run query"))?;
        let value =
            super::pct_decode(value).ok_or_else(|| err_response(400, "malformed app run query"))?;
        if !matches!(key.as_str(), "install_id" | "context_id")
            || !segment(&value)
            || params.insert(key, json!(value)).is_some()
        {
            return Err(err_response(400, "invalid or duplicate app run selector"));
        }
    }
    if params.contains_key("context_id") && !params.contains_key("install_id") {
        return Err(err_response(400, "context selector requires installation"));
    }
    Ok(Value::Object(params))
}

pub(super) fn handle(
    request: &mut Request,
    state: &Path,
    route: Route<'_>,
    write: bool,
) -> HttpResp {
    let route = if write && matches!(route, Route::List) {
        Route::Create
    } else {
        route
    };

    let params = match query(request, matches!(route, Route::List)) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let (method, params) = match route {
        Route::List => ("app_run_list", params),
        Route::Show(id) => ("app_run_show", json!({"run_id":id})),
        Route::Artifact(id) => ("app_run_artifact", json!({"artifact_id":id})),
        Route::CapabilityResults(id) => ("app_run_capability_results", json!({"run_id":id})),
        Route::CapabilityResult(id) => ("app_run_capability_result", json!({"receipt_id":id})),
        Route::CapabilityAsset(id) => ("app_run_capability_asset", json!({"receipt_id":id})),
        Route::Create => {
            let bytes = match read_body(request, BODY_CAP) {
                Ok(bytes) => bytes,
                Err(response) => return response,
            };
            let value: Create = match parse_json(&bytes) {
                Ok(value) => value,
                Err(response) => return response,
            };
            (
                "app_run_create",
                serde_json::to_value(value).expect("typed create serializes"),
            )
        }
        Route::Start => {
            let bytes = match read_body(request, BODY_CAP) {
                Ok(bytes) => bytes,
                Err(response) => return response,
            };
            let value: Start = match parse_json(&bytes) {
                Ok(value) => value,
                Err(response) => return response,
            };
            (
                "app_run_start",
                serde_json::to_value(value).expect("typed start serializes"),
            )
        }
        Route::Team(id) if !write => ("app_install_team_show", json!({"install_id":id})),
        Route::Team(id) => {
            let bytes = match read_body(request, BODY_CAP) {
                Ok(bytes) => bytes,
                Err(response) => return response,
            };
            let value: TeamSet = match parse_json(&bytes) {
                Ok(value) => value,
                Err(response) => return response,
            };
            let mut params = serde_json::to_value(value).expect("typed team serializes");
            params["install_id"] = json!(id);
            ("app_install_team_set", params)
        }
        Route::InstallDecision(id, _) | Route::RunDecision(id) => {
            let bytes = match read_body(request, BODY_CAP) {
                Ok(bytes) => bytes,
                Err(response) => return response,
            };
            let decision: Decision = match parse_json(&bytes) {
                Ok(value) => value,
                Err(response) => return response,
            };
            match route {
                Route::InstallDecision(_, true) => (
                    "app_local_install_approve",
                    json!({"install_id":id,"digest":decision.digest}),
                ),
                Route::InstallDecision(_, false) => (
                    "app_local_install_revoke",
                    json!({"install_id":id,"digest":decision.digest}),
                ),
                _ => (
                    "app_run_approve",
                    json!({"run_id":id,"digest":decision.digest}),
                ),
            }
        }
        Route::Cancel(id) | Route::Dispatch(id) => {
            let bytes = match read_body(request, BODY_CAP) {
                Ok(bytes) => bytes,
                Err(response) => return response,
            };
            if let Err(response) = parse_json::<Empty>(&bytes) {
                return response;
            }
            (
                if matches!(route, Route::Cancel(_)) {
                    "app_run_cancel"
                } else {
                    "app_run_dispatch"
                },
                json!({"run_id":id}),
            )
        }
    };
    match client::rpc(state, method, params) {
        Ok(value) => {
            if let Route::Artifact(id) = route {
                if !artifact_valid(&value, id) {
                    return err_response(502, "invalid stored text artifact receipt");
                }
            }
            let mut response = json_response(value);
            response.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
            response
        }
        Err(error) => home::rpc_err(&error, method),
    }
}
fn artifact_valid(value: &Value, id: &str) -> bool {
    let Some(text) = value["text"].as_str() else {
        return false;
    };
    value["id"].as_str() == Some(id)
        && text.len() <= ARTIFACT_CAP
        && value["size"].as_u64() == Some(text.len() as u64)
        && matches!(
            value["media_type"].as_str(),
            Some("text/plain" | "text/markdown")
        )
        && value["digest"].as_str()
            == Some(format!("sha256:{:x}", Sha256::digest(text.as_bytes())).as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_are_exact_and_have_no_path_authority() {
        assert!(matches!(route("/api/app-runs"), Some(Route::List)));
        assert!(matches!(
            route("/api/app-runs/run-a"),
            Some(Route::Show("run-a"))
        ));
        assert!(matches!(
            route("/api/app-runs/run-a/capability-results"),
            Some(Route::CapabilityResults("run-a"))
        ));
        assert!(matches!(
            route("/api/app-capability-results/receipt-a"),
            Some(Route::CapabilityResult("receipt-a"))
        ));
        assert!(matches!(
            route("/api/app-capability-results/receipt-a/asset"),
            Some(Route::CapabilityAsset("receipt-a"))
        ));
        assert!(route("/api/app-capability-results/receipt-a/asset").is_some_and(|r| r.is_read()));
        assert!(matches!(
            route("/api/app-installations/install-a/approve"),
            Some(Route::InstallDecision("install-a", true))
        ));
        for path in [
            "/api/app-runs/../approve",
            "/api/app-runs/a/approve/extra",
            "/api/app-runs/a/retry",
            "/api/app-run-artifacts/a/b",
            "/api/app-runs/",
            "/api/app-runs/run-a/capability-results/other",
            "/api/app-capability-results/receipt-a/asset/x",
            "/api/app-capability-results/a/b/asset",
            "/api/app-capability-results//asset",
            "/api/app-capability-results/../asset",
        ] {
            assert!(route(path).is_none(), "route admitted {path}");
        }
    }

    #[test]
    fn start_and_team_routes_are_exact_and_start_is_write_only() {
        assert!(matches!(route("/api/app-runs/start"), Some(Route::Start)));
        assert!(!route("/api/app-runs/start").unwrap().is_read());
        assert!(matches!(
            route("/api/app-installations/install-a/team"),
            Some(Route::Team("install-a"))
        ));
        for path in [
            "/api/app-runs/start/x",
            "/api/app-installations/install-a/team/x",
            "/api/app-installations//team",
        ] {
            assert!(route(path).is_none(), "route admitted {path}");
        }
        // The frame-facing request carries no owner, project or role fields.
        for body in [
            r#"{"install_id":"i","workflow":"w","inputs":{},"request_id":"r","expected_quotes":{},"owner_pm":"p"}"#,
            r#"{"install_id":"i","workflow":"w","inputs":{},"request_id":"r","expected_quotes":{},"project_link":"x"}"#,
            r#"{"install_id":"i","workflow":"w","inputs":{},"request_id":"r"}"#,
        ] {
            assert!(serde_json::from_str::<Start>(body).is_err(), "{body}");
        }
        assert!(serde_json::from_str::<Start>(
            r#"{"install_id":"i","workflow":"w","inputs":{},"request_id":"r","expected_quotes":{}}"#
        )
        .is_ok());
        assert!(serde_json::from_str::<TeamSet>(
            r#"{"owner_pm":"p","roles":{},"expected_revision":0,"install_id":"other"}"#
        )
        .is_err());
    }

    #[test]
    fn request_schema_rejects_duplicate_and_identity_fields() {
        assert!(serde_json::from_str::<Decision>(r#"{"digest":"a","digest":"b"}"#).is_err());
        assert!(serde_json::from_str::<Decision>(r#"{"digest":"a","operator":true}"#).is_err());
        assert!(serde_json::from_str::<Empty>(r#"{"run_id":"other"}"#).is_err());
        assert!(serde_json::from_str::<Create>(
            r#"{"install_id":"i","workflow":"w","inputs":{"x":1},"request_id":"r","owner_pm":"p"}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Create>(
            r#"{"install_id":"i","workflow":"w","inputs":{},"request_id":"r","owner_pm":"p","selected_post_id":"p","source_receipt_id":"r","account":"other"}"#
        ).is_err());
    }

    #[test]
    fn artifact_receipt_binds_exact_id_bytes_size_and_text_type() {
        let original = json!({"id":"artifact-a","digest":format!("sha256:{:x}", Sha256::digest(b"hello")),"media_type":"text/markdown","size":5,"text":"hello"});
        assert!(artifact_valid(&original, "artifact-a"));
        assert!(!artifact_valid(&original, "artifact-b"));
        for (field, value) in [
            ("text", json!("changed")),
            ("digest", json!("sha256:wrong")),
            ("media_type", json!("text/html")),
            ("size", json!(4)),
        ] {
            let mut bad = original.clone();
            bad[field] = value;
            assert!(
                !artifact_valid(&bad, "artifact-a"),
                "tampered {field} admitted"
            );
        }
        let text = "x".repeat(ARTIFACT_CAP + 1);
        assert!(!artifact_valid(
            &json!({"id":"artifact-a","digest":format!("sha256:{:x}", Sha256::digest(text.as_bytes())),"media_type":"text/plain","size":text.len(),"text":text}),
            "artifact-a"
        ));
    }
}
