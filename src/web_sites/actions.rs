//! Owner-approval action requests, reconciliation, and read-back for `/v1/web-sites`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use serde_json::{json, Map, Value};

use super::{
    is_id, load_pending, new_request_key, reject, save_pending, Intent, PendingRecord, WEB_VERSION,
};
use crate::error::{Error, Result};
use crate::remote_cli::{self, RemoteTarget};

/// Publish, rollback and unpublish. A matching open action intent is resumed
/// with its original key and body; the revision is read only when a new intent
/// is made, never to replace an unresolved one.
pub fn request_action(
    target: &RemoteTarget,
    operation: &str,
    slug: Option<&str>,
    release: Option<&str>,
    wake_timeout: u64,
) -> Result<Value> {
    let identity = remote_cli::web_identity(target)?;
    let mut record = load_pending(target, &identity)?;
    let (site_slug, site_id, release_id) =
        resolve_target(&record, target, operation, slug, release)?;

    // An open intent for the same operation and release is reused verbatim.
    let existing = record.intents.iter().position(|i| {
        i.kind == operation
            && i.slug == site_slug
            && i.release_id == release_id
            && i.state == "open"
    });
    let existing = if let Some(index) = existing {
        if let Some(action_id) = record.intents[index].action_id.as_deref() {
            let action =
                read_known_action(target, &record.intents[index], action_id, wake_timeout)?;
            if let Some(state) = terminal_action_state(action["state"].as_str()) {
                record.intents[index].state = state.into();
                save_pending(target, &identity, &mut record)?;
                None
            } else {
                Some(index)
            }
        } else {
            Some(index)
        }
    } else {
        None
    };
    // An existing open intent may already have crossed the network, even if
    // this invocation cannot send. Its original key must remain recoverable.
    let previously_open = existing.is_some();
    let index = match existing {
        Some(index) => index,
        None => {
            let revision = fetch_revision(target, &site_id, wake_timeout)?;
            let mut body = Map::new();
            body.insert("version".into(), json!(WEB_VERSION));
            body.insert("workspaceId".into(), json!(target.org_id));
            body.insert("operation".into(), json!(operation));
            if let Some(rel) = &release_id {
                body.insert("releaseId".into(), json!(rel));
            }
            body.insert("expectedRevision".into(), json!(revision));
            let request_key = new_request_key(&format!("web-{operation}"))?;
            body.insert("requestKey".into(), json!(request_key));
            record.intents.push(Intent {
                kind: operation.to_string(),
                slug: site_slug.clone(),
                digest: String::new(),
                spa: false,
                release_id: release_id.clone(),
                expected_revision: Some(revision),
                request_key,
                body: Some(Value::Object(body)),
                site_id: Some(site_id.clone()),
                upload_id: None,
                action_id: None,
                state: "open".into(),
            });
            save_pending(target, &identity, &mut record)?;
            record.intents.len() - 1
        }
    };
    let intent = record.intents[index].clone();
    let payload = serde_json::to_vec(
        intent
            .body
            .as_ref()
            .ok_or_else(|| reject("the pending action has no recorded body"))?,
    )
    .map_err(|e| Error::internal(e.to_string()))?;
    let action = match create_action_reconciled(
        target,
        &site_id,
        &site_slug,
        &payload,
        &intent.request_key,
        previously_open,
        wake_timeout,
    ) {
        Ok(action) => action,
        Err(ActionCreateFailure::DefiniteRefusal(error)) => {
            record.intents[index].state = "failed".into();
            save_pending(target, &identity, &mut record)?;
            return Err(error);
        }
        Err(ActionCreateFailure::Unknown(error)) => return Err(error),
    };
    if !action_matches(&intent, &action) {
        return Err(action_unknown(target, &intent.slug, &intent.request_key));
    }
    let action_id = action["actionId"].as_str().unwrap_or_default().to_string();
    if is_id(&action_id, "act_") {
        record.intents[index].action_id = Some(action_id.clone());
    }
    let state = action["state"].as_str().unwrap_or("unknown").to_string();
    if matches!(
        state.as_str(),
        "completed" | "declined" | "failed" | "expired"
    ) {
        record.intents[index].state = if state == "completed" {
            "done"
        } else {
            "failed"
        }
        .into();
    }
    save_pending(target, &identity, &mut record)?;
    report_action(target, &intent.request_key, &action)
}

/// Resolve the release/site a publish, rollback or unpublish names, from the
/// prepared releases of this org. A foreign or unknown release is refused
/// before any request is built.
fn resolve_target(
    record: &PendingRecord,
    target: &RemoteTarget,
    operation: &str,
    slug: Option<&str>,
    release: Option<&str>,
) -> Result<(String, String, Option<String>)> {
    match (operation, slug, release) {
        ("publish", None, Some(rel)) => {
            let found = record
                .releases
                .iter()
                .find(|r| r.release_id == rel)
                .ok_or_else(|| {
                    reject(format!(
                        "release '{rel}' is not a prepared release for org '{}'; nothing was sent",
                        target.org
                    ))
                })?;
            Ok((
                found.slug.clone(),
                found.site_id.clone(),
                Some(rel.to_string()),
            ))
        }
        ("rollback", Some(slug), Some(rel)) => {
            let found = record
                .releases
                .iter()
                .find(|r| r.slug == slug && r.release_id == rel)
                .ok_or_else(|| {
                    reject(format!(
                        "release '{rel}' is not a prepared release of '{slug}' for org '{}'; nothing was sent",
                        target.org
                    ))
                })?;
            Ok((
                found.slug.clone(),
                found.site_id.clone(),
                Some(rel.to_string()),
            ))
        }
        ("unpublish", Some(slug), None) => {
            let found = record
                .releases
                .iter()
                .rev()
                .find(|r| r.slug == slug)
                .ok_or_else(|| {
                    reject(format!(
                        "no prepared site '{slug}' for org '{}'; nothing was sent",
                        target.org
                    ))
                })?;
            Ok((found.slug.clone(), found.site_id.clone(), None))
        }
        _ => Err(reject("web action arguments are invalid")),
    }
}

/// The site's current revision, read from the scoped site record.
fn fetch_revision(target: &RemoteTarget, site_id: &str, wake_timeout: u64) -> Result<i64> {
    let response = remote_cli::web_request(
        target,
        "web.read",
        "GET",
        &format!("/v1/web-sites/sites/{site_id}"),
        &[("x-workspace-id", target.org_id.clone())],
        None,
        wake_timeout,
    )?;
    let site = remote_cli::web_envelope(response)?;
    site["revision"]
        .as_i64()
        .ok_or_else(|| reject("the host site reply had no revision"))
}

/// Create one owner-approval action. An unknown outcome (a transport failure or
/// a server error after the request may have landed) is reconciled by sending
/// the SAME body and key again, never a new key. If it is still unknown, the
/// intent stays open and is reported by its key.
enum ActionCreateFailure {
    DefiniteRefusal(Error),
    Unknown(Error),
}

fn definite_action_refusal(response: &remote_cli::WebResponse) -> bool {
    (400..500).contains(&response.status)
        && !matches!(response.status, 401 | 403)
        && response.body.as_ref().is_some_and(|body| {
            body["ok"] == Value::Bool(false)
                && body["error"]["code"]
                    .as_str()
                    .is_some_and(|code| !code.is_empty())
        })
}

fn create_action_reconciled(
    target: &RemoteTarget,
    site_id: &str,
    slug: &str,
    payload: &[u8],
    request_key: &str,
    previously_open: bool,
    wake_timeout: u64,
) -> std::result::Result<Value, ActionCreateFailure> {
    let path = format!("/v1/web-sites/sites/{site_id}/actions");
    let send = || {
        remote_cli::web_request(
            target,
            "web.prepare",
            "POST",
            &path,
            &[("content-type", "application/json".to_string())],
            Some(payload),
            wake_timeout,
        )
    };
    let mut uncertain = previously_open;
    for _ in 0..2 {
        match send() {
            Ok(response) if definite_action_refusal(&response) && !uncertain => {
                return Err(ActionCreateFailure::DefiniteRefusal(
                    remote_cli::web_envelope(response)
                        .expect_err("validated refusal envelope must be an error"),
                ));
            }
            Ok(response) if definite_action_refusal(&response) => break,
            Ok(response) if (200..300).contains(&response.status) => {
                if let Ok(action) = remote_cli::web_envelope(response) {
                    return Ok(action);
                }
                uncertain = true;
            }
            Err(error) if error.kind() != "unknown" && !uncertain => {
                return Err(ActionCreateFailure::DefiniteRefusal(error));
            }
            Ok(_) | Err(_) => uncertain = true,
        }
    }
    Err(ActionCreateFailure::Unknown(action_unknown(
        target,
        slug,
        request_key,
    )))
}

/// Print an action without implying success. `approvalUrl` is null on the host
/// until the owner confirmation page exists, so the result is reported as
/// pending; `liveUrl` appears only on a confirmed `completed` state.
pub(super) fn terminal_action_state(state: Option<&str>) -> Option<&'static str> {
    match state {
        Some("completed") => Some("done"),
        Some("declined" | "failed" | "expired") => Some("failed"),
        _ => None,
    }
}

fn action_unknown(target: &RemoteTarget, slug: &str, request_key: &str) -> Error {
    Error::unknown(format!(
        "the web action outcome is unknown for workspace {} and request key {request_key}; \
         inspect `cadence web status {slug} --org {}` before retrying. No different key was sent",
        target.org, target.org
    ))
}

fn action_matches(intent: &Intent, action: &Value) -> bool {
    action["actionId"]
        .as_str()
        .is_some_and(|id| is_id(id, "act_"))
        && action["siteId"].as_str() == intent.site_id.as_deref()
        && action["operation"].as_str() == Some(intent.kind.as_str())
        && action["releaseId"].as_str() == intent.release_id.as_deref()
        && action["expectedRevision"].as_i64() == intent.expected_revision
        && matches!(
            action["state"].as_str(),
            Some(
                "awaiting_approval"
                    | "approved"
                    | "promoting"
                    | "completed"
                    | "declined"
                    | "failed"
                    | "expired"
            )
        )
}

/// Read back an action only through its known id and require the complete
/// immutable intent binding to match before trusting its state.
pub(super) fn read_known_action(
    target: &RemoteTarget,
    intent: &Intent,
    action_id: &str,
    wake_timeout: u64,
) -> Result<Value> {
    if !is_id(action_id, "act_") {
        return Err(reject("the pending action id is invalid; nothing was sent"));
    }
    intent
        .site_id
        .as_deref()
        .ok_or_else(|| reject("the pending action has no site binding; nothing was sent"))?;
    let response = remote_cli::web_request(
        target,
        "web.read",
        "GET",
        &format!("/v1/web-sites/actions/{action_id}"),
        &[("x-workspace-id", target.org_id.clone())],
        None,
        wake_timeout,
    )
    .map_err(|_| action_unknown(target, &intent.slug, &intent.request_key))?;
    if response.status != 200 {
        return Err(action_unknown(target, &intent.slug, &intent.request_key));
    }
    let action = remote_cli::web_envelope(response)
        .map_err(|_| action_unknown(target, &intent.slug, &intent.request_key))?;
    if action["actionId"].as_str() != Some(action_id) || !action_matches(intent, &action) {
        return Err(action_unknown(target, &intent.slug, &intent.request_key));
    }
    Ok(action)
}

fn report_action(target: &RemoteTarget, request_key: &str, action: &Value) -> Result<Value> {
    let state = action["state"].as_str().unwrap_or("unknown");
    let live = if state == "completed" {
        action["liveUrl"].clone()
    } else {
        Value::Null
    };
    let approval = match action["approvalUrl"].as_str() {
        Some(url) => json!({"url": url, "expiresAt": action["approvalExpiresAt"]}),
        None => json!({
            "status": "owner_confirmation_unavailable",
            "reason": "the owner confirmation page is not available yet; no approval was made",
        }),
    };
    Ok(json!({
        "org": target.org,
        "actionId": action["actionId"],
        "operation": action["operation"],
        "state": state,
        "requestKey": request_key,
        "liveUrl": live,
        "approval": approval,
    }))
}
