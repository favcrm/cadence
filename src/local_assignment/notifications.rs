//! Explicit hosted directory reads and FYI receipts. No actor or content reads.

use super::{http::BoardHttp, CoordHttp};
use crate::error::{Error, Result};
use crate::remote_cli::RemoteTarget;
use serde::Deserialize;
use serde_json::{json, Value};

const VERSION: &str = "hosted-cadence-agent-directory.v1";

fn stable_id(value: &str, prefix: &str) -> bool {
    let Some(id) = value.strip_prefix(prefix) else {
        return false;
    };
    id.len() == 36
        && id.bytes().enumerate().all(|(i, byte)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

fn bounded(value: &str, max: usize) -> bool {
    let len = value.encode_utf16().count();
    len > 0 && len <= max
}

/// Return one server-authored page, including its registration and presence
/// metadata unchanged. An online runner is not a promise of execution.
pub fn directory(target: &RemoteTarget, after_id: Option<&str>) -> Result<Value> {
    if after_id.is_some_and(|id| !stable_id(id, "lag_")) {
        return Err(Error::usage(
            "--after-id must be a stable lag_<UUID> from the directory",
        ));
    }
    let mut path = format!("/__platform/hosted-cadence/{}/local-agents", target.org_id);
    if let Some(id) = after_id {
        path.push_str(&format!("?after_id={id}"));
    }
    let reply = BoardHttp::open(target)?.send("GET", &path, None)?;
    match reply.status {
        200 if reply.body["version"] == VERSION
            && reply.body["agents"].is_array()
            && (reply.body["after_id"].is_null()
                || reply.body["after_id"]
                    .as_str()
                    .is_some_and(|id| stable_id(id, "lag_"))) =>
        {
            Ok(reply.body)
        }
        401 | 403 => Err(Error::rejected(
            "hosted directory authorization refused; log in again as the selected workspace member",
        )),
        200 => Err(Error::invalid(
            "invalid_response",
            "hosted directory returned an invalid page",
        )),
        _ => Err(Error::invalid(
            "directory_unavailable",
            format!("hosted directory request failed (HTTP {})", reply.status),
        )),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: String,
    operation_id: String,
    notice_id: String,
    agent_id: String,
    kind: String,
    status: String,
}

/// Submit one intent exactly once at the HTTP layer. A caller-selected key is
/// mandatory, and every uncertain outcome returns that same key for retry.
/// References carry pointers only; neither this helper nor FYI starts work.
pub fn notify(
    target: &RemoteTarget,
    agent_id: &str,
    operation_id: &str,
    text: &str,
    issues: &[String],
    wiki: &[String],
) -> Result<Value> {
    if !stable_id(agent_id, "lag_") {
        return Err(Error::usage(
            "notify requires a stable lag_<UUID> from `cadence agent directory`",
        ));
    }
    if !bounded(operation_id, 120) {
        return Err(Error::usage("--operation-id must contain 1..120 UTF-16 code units; reuse it with identical content when retrying"));
    }
    if !bounded(text, 4000) {
        return Err(Error::usage(
            "--text must contain 1..4000 UTF-16 code units",
        ));
    }
    if issues.len() + wiki.len() > 16
        || issues.iter().chain(wiki).any(|id| !bounded(id.trim(), 200))
    {
        return Err(Error::usage("at most 16 --issue/--wiki pointers are allowed, each with a nonempty id of at most 200 UTF-16 code units"));
    }
    let references: Vec<Value> = issues
        .iter()
        .map(|id| json!({"kind": "issue", "id": id.trim()}))
        .chain(
            wiki.iter()
                .map(|id| json!({"kind": "wiki", "id": id.trim()})),
        )
        .collect();
    let body = json!({"version": VERSION, "operation_id": operation_id, "kind": "fyi", "text": text, "references": references});
    if serde_json::to_vec(&body)?.len() > 32_768 {
        return Err(Error::usage(
            "encoded FYI request exceeds the hosted 32768-byte limit",
        ));
    }
    let path = format!(
        "/__platform/hosted-cadence/{}/local-agents/{agent_id}/notify",
        target.org_id
    );
    let retry = "Retry only with the same workspace, member, agent, --operation-id, text and references; stored does not mean delivered, read or executed.";
    let reply =
        match BoardHttp::open(target).and_then(|mut http| http.send("POST", &path, Some(&body))) {
            Ok(reply) => reply,
            Err(error) => {
                return Ok(json!({
                    "version": VERSION, "operation_id": operation_id, "agent_id": agent_id,
                    "kind": "fyi", "status": "unknown", "error": error.to_string(), "retry": retry,
                }))
            }
        };
    if matches!(reply.status, 200 | 201) {
        let receipt = serde_json::from_value::<Receipt>(reply.body.clone());
        if receipt.is_ok_and(|receipt| {
            receipt.version == VERSION
                && receipt.operation_id == operation_id
                && receipt.agent_id == agent_id
                && stable_id(&receipt.notice_id, "lan_")
                && receipt.kind == "fyi"
                && receipt.status == "stored"
        }) {
            return Ok(reply.body);
        }
    }
    let code = match reply.status {
        401 | 403 => "authorization_refused",
        409 if reply.body["code"] == "idempotency_conflict" => "idempotency_conflict",
        409 if reply.body["code"] == "recipient_or_storage_unavailable" => {
            "recipient_or_storage_unavailable"
        }
        400 | 413 => "invalid_request",
        _ => "notification_outcome_unknown",
    };
    // Authorization can fail after commit; do not imply nothing was stored.
    Ok(json!({
        "version": VERSION, "operation_id": operation_id, "agent_id": agent_id,
        "kind": "fyi", "status": if matches!(code, "idempotency_conflict" | "recipient_or_storage_unavailable" | "invalid_request") { "refused" } else { "unknown" },
        "error": code, "http_status": reply.status, "retry": retry,
    }))
}
