//! Bounded human CLI transport for the selected workspace's published app lifecycle.
use std::io::Read;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::remote_cli::RemoteTarget;
use serde_json::Value;

pub struct AppReply {
    pub status: u16,
    pub body: Value,
    pub operation_id: Option<String>,
}

fn unknown_outcome(target: &RemoteTarget, operation_id: Option<&str>) -> Error {
    match operation_id {
        Some(id) => Error::unknown(format!(
            "hosted app mutation outcome is unknown; in the same workspace inspect \
             `cadence --org {} app catalog operation {id}` and reconcile with \
             `cadence --org {} app catalog reconcile {id}` before deciding what to do; \
             do not retry blindly",
            target.org, target.org
        )),
        None => Error::unknown(format!(
            "hosted app mutation outcome is unknown and no usable operation ID was supplied; \
             inspect `cadence --org {} app catalog ls` and consult the workspace owner \
             before retrying",
            target.org
        )),
    }
}

fn nonempty(value: &Value) -> bool {
    value.as_str().is_some_and(|text| !text.is_empty())
}

fn sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Check the source-owned lifecycle fields needed to trust an outcome. A
/// current owner may reconcile an operation initiated by a different owner.
fn confirmed_operation(target: &RemoteTarget, data: &Value, expected_id: &str) -> bool {
    let consent = &data["consent"];
    let Some(kind @ ("install" | "update" | "revoke")) = data["kind"].as_str() else {
        return false;
    };
    let digest = consent["digest"].as_str();
    let artifact_hash = consent
        .get("artifact_sha256")
        .is_some_and(|value| value.is_null() || value.as_str().is_some_and(sha256_hex));
    let artifact_version = consent
        .get("artifact_version")
        .is_some_and(|value| value.is_null() || nonempty(value));
    if data["version"] != "hosted-app-lifecycle.v1"
        || data["operation_id"] != expected_id
        || data["workspace_id"] != target.org_id
        || !nonempty(&data["app"])
        || !nonempty(&consent["actor_id"])
        || !digest
            .and_then(|value| value.strip_prefix("sha256:"))
            .is_some_and(sha256_hex)
        || !artifact_hash
        || !artifact_version
        || consent["at"].as_u64().is_none()
        || data["updated_at"].as_u64().is_none()
    {
        return false;
    }
    match data["state"].as_str() {
        Some("succeeded") => {
            let installation = &data["installation"];
            nonempty(&installation["installId"])
                && nonempty(&installation["version"])
                && nonempty(&installation["generation"])
                && installation["app"] == data["app"]
                && installation["digest"] == consent["digest"]
                && matches!(
                    installation["approval"]["state"].as_str(),
                    Some("approved" | "unapproved" | "unknown")
                )
                && (kind != "revoke" || installation["approval"]["state"] == "unapproved")
                && data.get("refusal") == Some(&Value::Null)
        }
        Some("refused") if data.get("installation") == Some(&Value::Null) => {
            match data.get("refusal") {
                Some(Value::Null) => true,
                Some(refusal) => {
                    refusal["version"] == "hosted-app-mutation.v1"
                        && refusal["operation_id"] == expected_id
                        && refusal["operation"] == kind
                        && refusal["outcome"] == "refused"
                        && refusal["mutation"] == "not_started"
                        && refusal["code"] == "native_precondition_refused"
                }
                None => false,
            }
        }
        Some("prepared" | "dispatched") => {
            data.get("installation") == Some(&Value::Null)
                && data.get("refusal") == Some(&Value::Null)
        }
        _ => false,
    }
}

fn confirmed_mutation(
    target: &RemoteTarget,
    suffix: &str,
    request: Option<&Value>,
    expected_id: Option<&str>,
    reply_id: Option<&str>,
    subject_id: Option<&str>,
    reply: &Value,
) -> bool {
    let Some(expected_id) = expected_id else {
        return false;
    };
    if reply_id != Some(expected_id) || reply["ok"] != true {
        return false;
    }
    if suffix == "/action" {
        let Some(action) = request.and_then(|value| value.get("action")) else {
            return false;
        };
        let Some(listing) = action["listing_id"].as_str() else {
            return false;
        };
        let Some(version) = action["version"].as_str() else {
            return false;
        };
        let Some(digest) = action["digest"].as_str() else {
            return false;
        };
        let state = if action.get("install_id").is_some() {
            "updated"
        } else {
            "installed"
        };
        return reply["data"]["state"] == state
            && reply["data"]["version"] == version
            && reply["data"]["digest"] == digest
            && reply["data"]["catalogue"]["id"] == listing
            && reply["data"]["catalogue"]["version"] == version
            && reply["data"]["catalogue"]["digest"] == digest;
    }
    if suffix.ends_with("/reconcile") {
        return confirmed_operation(target, &reply["data"], expected_id);
    }
    if suffix == "/revoke" {
        let (Some(request), Some(subject_id)) = (request, subject_id) else {
            return false;
        };
        let (Some(install_id), Some(digest), Some(generation)) = (
            request["install_id"].as_str(),
            request["expected_digest"].as_str(),
            request["expected_generation"].as_str(),
        ) else {
            return false;
        };
        let data = &reply["data"];
        if !confirmed_operation(target, data, expected_id)
            || data["kind"] != "revoke"
            || data["consent"]["actor_id"] != subject_id
            || data["consent"]["digest"] != digest
        {
            return false;
        }
        return match data["state"].as_str() {
            Some("succeeded") => {
                data["installation"]["installId"] == install_id
                    && data["installation"]["generation"] == generation
            }
            Some("refused") => true,
            _ => false,
        };
    }
    false
}

/// No mutation retry, redirects, uploaded bytes, generic RPC, or local fallback.
pub fn request(
    target: &RemoteTarget,
    method: &str,
    suffix: &str,
    body: Option<&Value>,
) -> Result<AppReply> {
    if target.identity.is_some()
        || std::env::var_os("CADENCE_ALIAS").is_some()
        || std::env::var_os("CADENCE_GRANT_TOKEN").is_some()
    {
        return Err(Error::rejected("hosted app consent requires the human owner's plain shell, not a managed agent spending a saved human login"));
    }
    let valid_id = |value: &str| {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    let allowed = match (method, suffix) {
        ("GET", "") | ("POST", "/action" | "/revoke") => true,
        ("GET", tail) => tail
            .strip_prefix("/operations/")
            .or_else(|| tail.strip_prefix("/pending/"))
            .is_some_and(valid_id),
        ("POST", tail) => tail
            .strip_prefix("/operations/")
            .and_then(|v| v.strip_suffix("/reconcile"))
            .is_some_and(valid_id),
        _ => false,
    };
    if !allowed || !valid_id(&target.org_id) {
        return Err(Error::usage("invalid hosted app route"));
    }
    let mutating = method == "POST"
        && !(suffix == "/action"
            && body
                .and_then(|value| value.pointer("/action/intent"))
                .and_then(Value::as_str)
                == Some("check"));
    let requested_id = match suffix {
        "/action" => body.and_then(|value| value.pointer("/action/operation_id")),
        "/revoke" => body.and_then(|value| value.get("operation_id")),
        _ => None,
    }
    .and_then(Value::as_str)
    .or_else(|| {
        suffix
            .strip_prefix("/operations/")
            .and_then(|value| value.strip_suffix("/reconcile"))
    })
    .filter(|value| valid_id(value));
    let failure = |message: &str| {
        if mutating {
            unknown_outcome(target, requested_id)
        } else {
            Error::rejected(message)
        }
    };
    let deadline = Instant::now() + Duration::from_secs(45);
    let dir = crate::remote_auth::auth_dir(None)?;
    let material = crate::remote_cli::cli_material_until(&dir, target, deadline)?;
    if material.subject_id.is_none() || material.issuer.is_none() {
        return Err(Error::rejected(
            "a saved renewable human login with member and issuer pins is required",
        ));
    }
    let agent = crate::issuer_transport::agent(
        ureq::Agent::config_builder()
            .timeout_global(Some(deadline.saturating_duration_since(Instant::now())))
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None),
    );
    let url = format!(
        "{}/__platform/hosted-cadence/{}/app-installations{}",
        target.endpoint.trim_end_matches('/'),
        target.org_id,
        suffix
    );
    let authorization = format!("Bearer {}", material.access_token);
    let response = match method {
        "GET" => agent
            .get(url.as_str())
            .header("Authorization", authorization.as_str())
            .call(),
        "POST" => agent
            .post(url.as_str())
            .header("Authorization", authorization.as_str())
            .send_json(body.unwrap_or(&serde_json::json!({}))),
        _ => unreachable!("bounded method checked above"),
    }
    .map_err(|_| failure("hosted app request failed"))?;
    let status = response.status().as_u16();
    let has_operation_header = response.headers().contains_key("x-hosted-app-operation-id");
    let operation_id = response
        .headers()
        .get("x-hosted-app-operation-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| valid_id(value))
        .map(str::to_owned);
    if (300..400).contains(&status) {
        return Err(failure(
            "hosted app redirect refused; no local fallback was attempted",
        ));
    }
    let mut bytes = Vec::new();
    response
        .into_body()
        .as_reader()
        .take(1_048_577)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            if mutating {
                unknown_outcome(target, requested_id)
            } else {
                error.into()
            }
        })?;
    if bytes.len() > 1_048_576 {
        return Err(failure("hosted app response exceeded 1 MiB"));
    }
    let answer: Value =
        serde_json::from_slice(&bytes).map_err(|_| failure("hosted app response was not JSON"))?;
    // Both the current tenant and the prior hosted app protocol produce
    // these two /action refusals before any app operation is dispatched.
    let proven_pre_effect = suffix == "/action"
        && status == 503
        && answer["ok"] == false
        && answer.pointer("/error/message").is_some_and(nonempty)
        && matches!(
            answer.pointer("/error/code").and_then(Value::as_str),
            Some("release_unavailable" | "check_unavailable")
        )
        && (!has_operation_header
            || operation_id
                .as_deref()
                .is_some_and(|id| Some(id) == requested_id));
    if mutating
        && (status == 202
            || (status >= 500 && !proven_pre_effect)
            || matches!(
                answer.pointer("/error/code").and_then(Value::as_str),
                Some("outcome_uncertain" | "operation_expired")
            )
            || ((200..300).contains(&status)
                && !confirmed_mutation(
                    target,
                    suffix,
                    body,
                    requested_id,
                    operation_id.as_deref(),
                    material.subject_id.as_deref(),
                    &answer,
                )))
    {
        return Err(unknown_outcome(target, requested_id));
    }
    Ok(AppReply {
        status,
        body: answer,
        operation_id,
    })
}
