//! Board-facing monitoring projection helpers for daemon monitor and alert rows.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::HashMap;

use serde_json::{json, Value};

/// The monitor daemon deliberately has no external delivery provider in this
/// increment.  The board can still make its durable local inbox useful by
/// projecting the daemon's evidence into the overview and letting an operator
/// acknowledge an alert there.
fn monitor_alert_action(kind: &str, monitor_owner: &str) -> (&'static str, String, &'static str) {
    match kind {
        "approval_menu" => (
            "Review the approval request",
            "operator".to_string(),
            "operator approval required",
        ),
        "attention" => (
            "Resolve the worker attention request",
            "operator".to_string(),
            "operator reconciliation required",
        ),
        "turn_silent_end" => (
            "Reconcile the silent turn outcome",
            "operator".to_string(),
            "operator reconciliation required",
        ),
        "turn_unknown" => (
            "Inspect the uncertain turn and reconcile its worker",
            "operator".to_string(),
            "operator reconciliation required",
        ),
        "draft_pending" => (
            "Route the draft to an independent reviewer",
            "reviewer".to_string(),
            "independent review required",
        ),
        "delivery_parked" => (
            "Inspect the parked delivery before retrying",
            monitor_owner.to_string(),
            "monitor owner may retry; delivery is not automatic",
        ),
        "paste_not_rendered" => (
            "Inspect the PTY render miss and recover the worker",
            monitor_owner.to_string(),
            "monitor owner may recover the worker",
        ),
        "delivery_stalled" => (
            "Inspect the ready pane and redeliver or cancel the queued message",
            monitor_owner.to_string(),
            "monitor owner may redeliver; the queued message is not moving",
        ),
        "turn_stalled" => (
            "Inspect the stalled turn and reconcile its worker",
            monitor_owner.to_string(),
            "monitor owner may reconcile the worker",
        ),
        "dispatch_blocked" => (
            "Resolve the guarded dispatch prerequisite, then retry the task",
            monitor_owner.to_string(),
            "operator decision required; coordinator cannot bypass the guard",
        ),
        _ => (
            "Inspect the monitor evidence and choose the next owner",
            monitor_owner.to_string(),
            "operator decision required",
        ),
    }
}

fn local_monitor_delivery() -> Value {
    json!({
        "configured": false,
        "state": "ui_local_only",
        "push": false,
        "detail": "Durable alerts are visible and acknowledged in this UI; external push delivery is unconfigured",
    })
}

/// Build the UI-facing monitoring projection from daemon RPC rows.  Keeping
/// this pure makes the state axes testable without opening the live store:
/// `last_success_at` is a completed check, while heartbeat and next-check
/// timestamps remain separate evidence.
pub(super) fn monitoring_view(
    monitors: Vec<Value>,
    alerts_by_monitor: HashMap<String, Vec<Value>>,
    alert_errors: HashMap<String, String>,
    now: i64,
) -> Value {
    let mut has_degraded = false;
    let mut has_stale = false;
    let mut has_active = false;
    let mut last_success_at: Option<f64> = None;
    let mut last_check_at: Option<f64> = None;
    let mut next_check_at: Option<f64> = None;
    let mut open_alerts = 0i64;
    let mut errors = Vec::new();
    let mut all_alerts = Vec::new();
    let mut monitor_rows = Vec::new();

    for mut monitor in monitors {
        let id = monitor["id"].as_str().unwrap_or_default().to_string();
        let raw_state = monitor["monitoring"].as_str().unwrap_or("off");
        match raw_state {
            "degraded" => has_degraded = true,
            "stale" => has_stale = true,
            "active" => has_active = true,
            _ => {}
        }
        for (field, target) in [
            ("last_success_at", &mut last_success_at),
            ("last_check_at", &mut last_check_at),
        ] {
            if let Some(value) = monitor[field].as_f64() {
                if target.is_none_or(|current| value > current) {
                    *target = Some(value);
                }
            }
        }
        if let Some(value) = monitor["next_check_at"].as_f64() {
            if next_check_at.is_none_or(|current| value < current) {
                next_check_at = Some(value);
            }
        }
        // `active` is a claim about a completed observer pass, not a
        // heartbeat.  Once the recorded schedule is overdue, surface the
        // monitor as stale even if the daemon stopped before it could write
        // a degraded row.  A missing success proof is stale as well.
        let overdue = matches!(raw_state, "active" | "degraded")
            && (monitor["next_check_at"]
                .as_f64()
                .is_none_or(|next| next <= now as f64)
                || (raw_state == "active" && monitor["last_success_at"].is_null()));
        if overdue {
            monitor["stale"] = json!(true);
            monitor["monitoring"] = json!("stale");
            has_stale = true;
            let stale_error = if monitor["last_success_at"].is_null() {
                "active monitor has no successful scan evidence"
            } else {
                "scheduled monitor reconciliation is overdue"
            };
            if monitor["error"].is_null() {
                monitor["error"] = json!(stale_error);
            }
            errors.push(json!({"monitor": id, "error": monitor["error"]}));
        }
        if !overdue {
            if let Some(error) = monitor["error"].as_str().filter(|s| !s.is_empty()) {
                errors.push(json!({"monitor": id, "error": error}));
            }
        }
        if let Some(error) = alert_errors.get(&id) {
            errors.push(json!({"monitor": id, "error": error}));
            monitor["alerts_error"] = json!(error);
        }
        let mut rows = Vec::new();
        for alert in alerts_by_monitor.get(&id).cloned().unwrap_or_default() {
            let kind = alert["kind"].as_str().unwrap_or("alert");
            let (next_action, next_owner, authority) =
                monitor_alert_action(kind, monitor["owner"].as_str().unwrap_or("operator"));
            let created = alert["created"].as_f64().unwrap_or(now as f64);
            let age_secs = ((now as f64 - created).max(0.0)).round() as i64;
            if alert["state"].as_str() == Some("open") {
                open_alerts += 1;
            }
            // A CAD-2215 scoped row carries no global position: its stable
            // keyed `id` and encrypted `ack` replace `seq`, `event_seq` and
            // the `event:<seq>` fingerprint. A proven-operator row keeps the
            // legacy numeric document.
            let row = if alert.get("seq").is_none() {
                // The board's acknowledgement route is operator-only, so a
                // scoped member projection does not need the native agent's
                // `ack` token; it is validated above and withheld here so a
                // randomized token cannot churn the SSE fingerprint.
                json!({
                    "id": alert["id"],
                    "monitor": id,
                    "project": monitor["project"],
                    "monitor_owner": monitor["owner"],
                    "task": alert["task"],
                    "kind": alert["kind"],
                    "payload": alert["payload"],
                    "state": alert["state"],
                    "attempts": alert["attempts"],
                    "last_error": alert["last_error"],
                    "created": alert["created"],
                    "updated": alert["updated"],
                    "age_secs": age_secs,
                    "next_action": next_action,
                    "next_owner": next_owner,
                    "authority": authority,
                    "evidence": {
                        "monitor": id,
                        "alert": alert["id"],
                        "payload": alert["payload"],
                    },
                })
            } else {
                json!({
                    "seq": alert["seq"],
                    "monitor": id,
                    "project": monitor["project"],
                    "monitor_owner": monitor["owner"],
                    "task": alert["task"],
                    "event_seq": alert["event_seq"],
                    "fingerprint": alert["fingerprint"],
                    "kind": alert["kind"],
                    "payload": alert["payload"],
                    "state": alert["state"],
                    "attempts": alert["attempts"],
                    "last_error": alert["last_error"],
                    "created": alert["created"],
                    "updated": alert["updated"],
                    "age_secs": age_secs,
                    "next_action": next_action,
                    "next_owner": next_owner,
                    "authority": authority,
                    "evidence": {
                        "monitor": id,
                        "alert_seq": alert["seq"],
                        "event_seq": alert["event_seq"],
                        "fingerprint": alert["fingerprint"],
                        "payload": alert["payload"],
                    },
                })
            };
            rows.push(row.clone());
            all_alerts.push(row);
        }
        monitor["alerts"] = json!(rows);
        monitor_rows.push(monitor);
    }

    let state = if monitor_rows.is_empty() {
        "stopped"
    } else if has_degraded {
        "degraded"
    } else if has_stale {
        "stale"
    } else if has_active {
        "active"
    } else {
        "stopped"
    };
    all_alerts.sort_by(
        |left, right| match (left["seq"].as_i64(), right["seq"].as_i64()) {
            (Some(left), Some(right)) => left.cmp(&right),
            _ => left["created"]
                .as_f64()
                .partial_cmp(&right["created"].as_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    left["id"]
                        .as_str()
                        .unwrap_or_default()
                        .cmp(right["id"].as_str().unwrap_or_default())
                }),
        },
    );
    let latest = |value: Option<f64>| value.map(|v| v as i64);
    json!({
        "available": true,
        "state": state,
        "last_success_at": latest(last_success_at),
        "last_check_at": latest(last_check_at),
        "next_check_at": next_check_at.map(|v| v as i64),
        "open_alerts": open_alerts,
        "errors": errors,
        "delivery": local_monitor_delivery(),
        "monitors": monitor_rows,
        "alerts": all_alerts,
    })
}

pub(crate) fn monitoring_unavailable(error: impl Into<String>) -> Value {
    json!({
        "available": false,
        "state": "unavailable",
        "last_success_at": Value::Null,
        "last_check_at": Value::Null,
        "next_check_at": Value::Null,
        "open_alerts": 0,
        "errors": [{"error": error.into()}],
        "delivery": local_monitor_delivery(),
        "monitors": [],
        "alerts": [],
    })
}
