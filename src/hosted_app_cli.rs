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
        "GET" => agent.get(url.as_str()).header("Authorization", authorization.as_str()).call(),
        "POST" => agent.post(url.as_str()).header("Authorization", authorization.as_str())
            .send_json(body.unwrap_or(&serde_json::json!({}))),
        _ => unreachable!("bounded method checked above"),
    }.map_err(|_| Error::rejected("hosted app request failed; a write may be uncertain: retain its operation_id, then read/reconcile it; do not redispatch"))?;
    let status = response.status().as_u16();
    let operation_id = response
        .headers()
        .get("x-hosted-app-operation-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| valid_id(value))
        .map(str::to_owned);
    if (300..400).contains(&status) {
        return Err(Error::rejected(
            "hosted app redirect refused; no local fallback was attempted",
        ));
    }
    let mut bytes = Vec::new();
    response
        .into_body()
        .as_reader()
        .take(1_048_577)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1_048_576 {
        return Err(Error::rejected("hosted app response exceeded 1 MiB; retain operation_id and reconcile an uncertain write"));
    }
    let body = serde_json::from_slice(&bytes)
        .map_err(|_| Error::rejected("hosted app response was not JSON; retain operation_id and reconcile an uncertain write"))?;
    Ok(AppReply {
        status,
        body,
        operation_id,
    })
}
