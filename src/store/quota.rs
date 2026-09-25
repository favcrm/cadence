//! Provider quota evidence on agent params.

use serde_json::{json, Value};

use super::agents::Agent;
use super::now;

pub(super) fn quota_now_iso() -> String {
    crate::issue::time::iso(crate::issue::time::now_epoch())
}

/// Merge a sparse provider update. Omitted fields retain their last confirmed
/// value, while Codex's nullable window fields explicitly replace stale
/// telemetry with a JSON null. Other nullable fields, including account
/// identity, remain conservative and retain the last confirmed value.
pub(super) fn merge_quota_json(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Object(target), Value::Object(patch)) => {
            for (key, value) in patch {
                if value.is_null() {
                    if matches!(key.as_str(), "resetsAt" | "windowDurationMins") {
                        target.insert(key.clone(), Value::Null);
                    }
                    continue;
                }
                match target.get_mut(key) {
                    Some(existing) if existing.is_object() && value.is_object() => {
                        merge_quota_json(existing, value);
                    }
                    _ => {
                        target.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (target, patch) if !patch.is_null() => *target = patch.clone(),
        _ => {}
    }
}

fn quota_state(data: &Value) -> (&'static str, Option<&'static str>) {
    let Some(object) = data.as_object() else {
        return (
            "unknown",
            Some("Codex rate-limit response was not an object"),
        );
    };
    let account_id = object
        .get("accountId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    if account_id.is_none() {
        return ("unknown", Some("Codex provider omitted account id"));
    }
    let has_limits = object.get("rateLimits").is_some_and(Value::is_object)
        || object
            .get("rateLimitsByLimitId")
            .and_then(Value::as_object)
            .is_some_and(|limits| !limits.is_empty());
    if !has_limits {
        return ("unknown", Some("Codex provider omitted rate-limit buckets"));
    }
    ("available", None)
}

pub(super) fn canonical_quota(
    alias: &str,
    provider: &str,
    thread_id: &str,
    snapshot: &Value,
    observed_at: &str,
) -> Value {
    let data = snapshot.get("data").cloned().unwrap_or(Value::Null);
    let (computed_state, computed_reason) = quota_state(&data);
    let state = if computed_state == "available" {
        "available"
    } else {
        snapshot
            .get("state")
            .and_then(Value::as_str)
            .filter(|value| {
                matches!(
                    *value,
                    "unknown" | "unavailable" | "error" | "blocked" | "stale"
                )
            })
            .unwrap_or("unknown")
    };
    let reason = if state == "available" {
        None
    } else {
        snapshot
            .get("reason")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .or(computed_reason)
    };
    let mut result = json!({
        "provider": provider,
        "assignee": alias,
        "account_id": data.get("accountId").cloned().unwrap_or(Value::Null),
        "thread_id": thread_id,
        "state": state,
        "source": snapshot
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("provider quota telemetry"),
        "observed_at": observed_at,
        "updated_at": observed_at,
        "data": data,
    });
    if let Some(reason) = reason {
        result["reason"] = json!(reason);
    }
    result
}

/// Provider allowance evidence is admission evidence, not a standing grant.
/// Automatic dispatch fails closed when the latest provider-owned sample is
/// absent, stale, or does not bind to the current agent identity.
const QUOTA_EVIDENCE_MAX_AGE_SECS: f64 = 300.0;

pub(super) fn automatic_quota_error(agent: &Agent) -> Option<String> {
    let quota = agent.quota.as_ref();
    let Some(quota) = quota else {
        return Some("quota unknown: no account allowance telemetry".to_string());
    };
    let Some(quota) = quota.as_object() else {
        return Some("quota unknown: provider evidence is not an object".to_string());
    };
    if quota.get("provider").and_then(Value::as_str) != Some(agent.provider.as_str()) {
        return Some("quota unknown: allowance provider does not match agent".to_string());
    }
    if quota.get("assignee").and_then(Value::as_str) != Some(agent.alias.as_str()) {
        return Some("quota unknown: allowance is not bound to this agent".to_string());
    }
    let Some(thread_id) = agent.thread_id.as_deref().filter(|id| !id.is_empty()) else {
        return Some("quota unknown: agent has no current provider thread".to_string());
    };
    if quota.get("thread_id").and_then(Value::as_str) != Some(thread_id) {
        return Some("quota unknown: allowance is not bound to the current thread".to_string());
    }
    let Some(account_id) = quota
        .get("account_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    else {
        return Some("quota unknown: provider evidence has no account identity".to_string());
    };
    if quota
        .get("data")
        .and_then(Value::as_object)
        .and_then(|data| data.get("accountId"))
        .and_then(Value::as_str)
        != Some(account_id)
    {
        return Some("quota unknown: account identity is not provider-bound".to_string());
    }
    if !matches!(
        quota.get("source").and_then(Value::as_str),
        Some("account/rateLimits/read") | Some("account/rateLimits/updated")
    ) {
        return Some("quota unknown: provider evidence source is not canonical".to_string());
    }
    if quota.get("state").and_then(Value::as_str) != Some("available") {
        let state = quota
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Some(format!("quota {state}"));
    }
    let Some(observed_at) = quota
        .get("observed_at")
        .and_then(Value::as_str)
        .and_then(crate::issue::time::parse_iso)
    else {
        return Some("quota unknown: provider evidence has no canonical timestamp".to_string());
    };
    let age = now() - observed_at as f64;
    if !age.is_finite() || age < -30.0 || age > QUOTA_EVIDENCE_MAX_AGE_SECS {
        return Some("quota unknown: provider allowance evidence is stale".to_string());
    }
    None
}
