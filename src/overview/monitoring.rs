//! Monitor health and durable alerts projected for the overview and
//! board — daemon `monitor_list`/`monitor_alerts` rows validated, then
//! projected into the board-facing view; read via daemon RPCs only.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Value};

use crate::client;

use super::{now_epoch, PROBE_TIMEOUT};

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
fn monitoring_view(
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
            let row = json!({
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
            });
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
    all_alerts.sort_by_key(|a| a["seq"].as_i64().unwrap_or(0));
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

/// Validate the stable fields emitted by `monitor_list` before projecting any
/// rows. A partially readable response is not a valid empty registration:
/// failing closed keeps the Overview from claiming a healthy subset.
fn validate_monitor_rows(rows: &[Value]) -> Result<(), String> {
    for (index, row) in rows.iter().enumerate() {
        let object = row
            .as_object()
            .ok_or_else(|| format!("monitor_list row {index} is not an object"))?;
        let string_field = |field: &str| {
            object
                .get(field)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("monitor_list row {index} missing {field}"))
        };
        string_field("id")?;
        string_field("project")?;
        string_field("owner")?;
        let state = string_field("monitoring")?;
        if !matches!(state, "active" | "degraded" | "off") {
            return Err(format!(
                "monitor_list row {index} has unsupported monitoring state"
            ));
        }
        if object
            .get("interval_secs")
            .and_then(Value::as_i64)
            .is_none_or(|value| value < 1)
        {
            return Err(format!("monitor_list row {index} missing interval_secs"));
        }
        if object.get("coverage").and_then(Value::as_array).is_none() {
            return Err(format!("monitor_list row {index} missing coverage"));
        }
        let Some(delivery) = object.get("delivery").and_then(Value::as_object) else {
            return Err(format!("monitor_list row {index} missing delivery"));
        };
        if delivery
            .get("configured")
            .and_then(Value::as_bool)
            .is_none()
            || delivery.get("state").and_then(Value::as_str).is_none()
        {
            return Err(format!("monitor_list row {index} has malformed delivery"));
        }
        for field in ["open_alerts", "total_alerts"] {
            if object.get(field).and_then(Value::as_i64).is_none() {
                return Err(format!("monitor_list row {index} missing {field}"));
            }
        }
    }
    Ok(())
}

/// Validate the complete `monitor_alerts` response before exposing any
/// durable evidence to the Overview. An empty alert array is valid; a
/// missing or partially malformed response is unavailable rather than a
/// misleading healthy monitor view.
fn validate_monitor_alert_response<'a>(
    monitor_id: &str,
    response: &'a Value,
) -> Result<&'a [Value], String> {
    let object = response
        .as_object()
        .ok_or_else(|| "monitor_alerts response is not an object".to_string())?;
    let response_monitor = object
        .get("monitor")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    if response_monitor != Some(monitor_id) {
        return Err("monitor_alerts response has the wrong monitor".to_string());
    }
    if object
        .get("cursor")
        .and_then(Value::as_i64)
        .is_none_or(|value| value < 0)
    {
        return Err("monitor_alerts response has malformed cursor".to_string());
    }
    let rows = object
        .get("alerts")
        .and_then(Value::as_array)
        .ok_or_else(|| "monitor_alerts response missing alerts".to_string())?;
    for (index, row) in rows.iter().enumerate() {
        let alert = row
            .as_object()
            .ok_or_else(|| format!("monitor_alerts row {index} is not an object"))?;
        let string_field = |field: &str| {
            alert
                .get(field)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("monitor_alerts row {index} missing {field}"))
        };
        if alert
            .get("seq")
            .and_then(Value::as_i64)
            .is_none_or(|value| value < 1)
        {
            return Err(format!("monitor_alerts row {index} missing seq"));
        }
        let row_monitor = string_field("monitor")?;
        if row_monitor != monitor_id {
            return Err(format!(
                "monitor_alerts row {index} belongs to a different monitor"
            ));
        }
        string_field("task")?;
        if alert
            .get("event_seq")
            .and_then(Value::as_i64)
            .is_none_or(|value| value < 1)
        {
            return Err(format!("monitor_alerts row {index} missing event_seq"));
        }
        string_field("fingerprint")?;
        string_field("kind")?;
        if !alert.contains_key("payload") {
            return Err(format!("monitor_alerts row {index} missing payload"));
        }
        if !matches!(
            alert.get("state").and_then(Value::as_str),
            Some("open") | Some("acknowledged") | Some("resolved")
        ) {
            return Err(format!("monitor_alerts row {index} has unsupported state"));
        }
        if alert
            .get("attempts")
            .and_then(Value::as_i64)
            .is_none_or(|value| value < 0)
        {
            return Err(format!("monitor_alerts row {index} missing attempts"));
        }
        if !matches!(
            alert.get("last_error"),
            Some(value) if value.is_null() || value.as_str().is_some()
        ) {
            return Err(format!(
                "monitor_alerts row {index} has malformed last_error"
            ));
        }
        for field in ["created", "updated"] {
            if alert.get(field).and_then(Value::as_f64).is_none() {
                return Err(format!("monitor_alerts row {index} missing {field}"));
            }
        }
    }
    Ok(rows)
}

fn monitoring_unavailable(error: impl Into<String>) -> Value {
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

/// Read monitor health and durable alerts through the daemon socket.  The
/// board is allowed to show an explicit unavailable state when the daemon is
/// older than the monitor RPC; it must not open the live SQLite store itself.
pub fn monitoring(state_dir: &Path) -> Value {
    let now = now_epoch();
    let list = match client::rpc_timeout(state_dir, "monitor_list", json!({}), PROBE_TIMEOUT) {
        Ok(value) => value,
        Err(error) => {
            return monitoring_unavailable(error.to_string());
        }
    };
    let Some(rows) = list["monitors"].as_array() else {
        return monitoring_unavailable("monitor_list response missing monitors");
    };
    if let Err(error) = validate_monitor_rows(rows) {
        return monitoring_unavailable(error);
    }
    let monitors = rows.to_vec();
    let mut alerts_by_monitor = HashMap::new();
    for monitor in &monitors {
        let id = monitor["id"].as_str().unwrap_or_default();
        match client::rpc_timeout(
            state_dir,
            "monitor_alerts",
            json!({"monitor": id, "open": false, "limit": 100}),
            PROBE_TIMEOUT,
        ) {
            Ok(value) => match validate_monitor_alert_response(id, &value) {
                Ok(rows) => {
                    alerts_by_monitor.insert(id.to_string(), rows.to_vec());
                }
                Err(error) => {
                    return monitoring_unavailable(format!("monitor_alerts '{id}': {error}"));
                }
            },
            Err(error) => {
                return monitoring_unavailable(format!("monitor_alerts '{id}': {error}"));
            }
        }
    }
    monitoring_view(monitors, alerts_by_monitor, HashMap::new(), now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitoring_projection_keeps_real_scan_and_actionable_alert_evidence() {
        let monitors = vec![json!({
            "id": "watch-cadence",
            "project": "cadence",
            "owner": "watchdog",
            "monitoring": "degraded",
            "last_success_at": 90.0,
            "last_check_at": 110.0,
            "next_check_at": 170.0,
            "delivery": {"configured": false, "state": "unconfigured"},
            "error": "socket closed",
        })];
        let mut alerts_by_monitor = HashMap::new();
        alerts_by_monitor.insert(
            "watch-cadence".to_string(),
            vec![json!({
                "seq": 7,
                "monitor": "watch-cadence",
                "task": "cad-176-t1",
                "event_seq": 44,
                "fingerprint": "turn-stalled:44",
                "kind": "turn_stalled",
                "payload": {"message": "synthetic evidence"},
                "state": "open",
                "attempts": 1,
                "last_error": Value::Null,
                "created": 100.0,
                "updated": 100.0,
            })],
        );

        let view = monitoring_view(monitors, alerts_by_monitor, HashMap::new(), 130);
        assert_eq!(view["state"], "degraded");
        assert_eq!(view["last_success_at"], 90);
        assert_eq!(view["last_check_at"], 110);
        assert_eq!(view["open_alerts"], 1);
        assert_eq!(view["delivery"]["configured"], false);
        assert_eq!(view["delivery"]["push"], false);
        let alert = &view["alerts"][0];
        assert_eq!(alert["age_secs"], 30);
        assert_eq!(alert["next_owner"], "watchdog");
        assert_eq!(alert["authority"], "monitor owner may reconcile the worker");
        assert_eq!(alert["evidence"]["event_seq"], 44);
        assert_eq!(alert["evidence"]["fingerprint"], "turn-stalled:44");
    }

    #[test]
    fn turn_unknown_overview_requires_operator_reconciliation() {
        let monitors = vec![json!({
            "id": "unknown-monitor",
            "project": "cadence",
            "owner": "operator",
            "monitoring": "active",
            "last_success_at": 90.0,
            "last_check_at": 100.0,
            "next_check_at": 160.0,
            "error": Value::Null,
        })];
        let mut alerts_by_monitor = HashMap::new();
        alerts_by_monitor.insert(
            "unknown-monitor".to_string(),
            vec![json!({
                "seq": 3,
                "monitor": "unknown-monitor",
                "task": "unknown-task",
                "event_seq": 9,
                "fingerprint": "event:9",
                "kind": "turn_unknown",
                "payload": {"reason": "bounded"},
                "state": "open",
                "attempts": 0,
                "last_error": Value::Null,
                "created": 100.0,
                "updated": 100.0,
            })],
        );
        let view = monitoring_view(monitors, alerts_by_monitor, HashMap::new(), 130);
        let alert = &view["alerts"][0];
        assert_eq!(alert["kind"], "turn_unknown");
        assert_eq!(alert["next_owner"], "operator");
        assert_eq!(alert["authority"], "operator reconciliation required");
        let action = alert["next_action"].as_str().unwrap();
        assert!(action.contains("Inspect"), "{action}");
        assert!(action.contains("reconcil"), "{action}");
    }

    #[test]
    fn monitoring_projection_reports_stopped_without_registration() {
        let view = monitoring_view(Vec::new(), HashMap::new(), HashMap::new(), 130);
        assert_eq!(view["available"], true);
        assert_eq!(view["state"], "stopped");
        assert_eq!(view["last_success_at"], Value::Null);
        assert!(view["alerts"].as_array().unwrap().is_empty());
    }

    #[test]
    fn monitoring_projection_marks_overdue_active_scan_stale() {
        let view = monitoring_view(
            vec![json!({
                "id": "watch",
                "project": "cadence",
                "owner": "watchdog",
                "monitoring": "active",
                "last_success_at": 90.0,
                "last_check_at": 90.0,
                "next_check_at": 100.0,
                "error": Value::Null,
            })],
            HashMap::new(),
            HashMap::new(),
            101,
        );
        assert_eq!(view["state"], "stale");
        assert_eq!(view["monitors"][0]["monitoring"], "stale");
        assert_eq!(view["monitors"][0]["stale"], true);
        assert_eq!(
            view["errors"][0]["error"],
            "scheduled monitor reconciliation is overdue"
        );
    }

    #[test]
    fn monitoring_projection_keeps_stale_aggregate_when_active_row_follows() {
        let overdue = json!({
            "id": "overdue",
            "project": "cadence",
            "owner": "watchdog",
            "monitoring": "active",
            "last_success_at": 90.0,
            "last_check_at": 90.0,
            "next_check_at": 100.0,
            "error": Value::Null,
        });
        let current = json!({
            "id": "current",
            "project": "cadence",
            "owner": "watchdog",
            "monitoring": "active",
            "last_success_at": 110.0,
            "last_check_at": 110.0,
            "next_check_at": 200.0,
            "error": Value::Null,
        });
        let forward = monitoring_view(
            vec![overdue.clone(), current.clone()],
            HashMap::new(),
            HashMap::new(),
            101,
        );
        let reverse = monitoring_view(vec![current, overdue], HashMap::new(), HashMap::new(), 101);
        assert_eq!(forward["state"], "stale", "{forward}");
        assert_eq!(reverse["state"], "stale", "{reverse}");
    }

    #[test]
    fn malformed_monitor_rows_fail_closed_before_projection() {
        assert!(validate_monitor_rows(&[]).is_ok());
        let error = validate_monitor_rows(&[json!({"project": "cadence"})]).unwrap_err();
        assert!(error.contains("row 0") && error.contains("id"), "{error}");
        let unavailable = monitoring_unavailable(error);
        assert_eq!(unavailable["available"], false);
        assert_eq!(unavailable["state"], "unavailable");
        assert!(unavailable["errors"][0]["error"]
            .as_str()
            .unwrap()
            .contains("row 0"));
        let error = validate_monitor_rows(&[json!({"id": "watch"})]).unwrap_err();
        assert!(
            error.contains("row 0") && error.contains("project"),
            "{error}"
        );
        let error = validate_monitor_rows(&[json!({
            "id": "watch",
            "project": "cadence",
            "owner": "watchdog",
            "monitoring": "bogus",
            "interval_secs": 60,
            "coverage": [],
            "delivery": {"configured": false, "state": "unconfigured"},
            "open_alerts": 0,
            "total_alerts": 0,
        })])
        .unwrap_err();
        assert!(error.contains("unsupported monitoring state"), "{error}");
        let unavailable = monitoring_unavailable(error);
        assert_eq!(unavailable["available"], false);
        assert_eq!(unavailable["state"], "unavailable");
    }

    #[test]
    fn monitor_alert_response_validation_accepts_empty_and_rejects_partial() {
        let empty = json!({"monitor": "watch", "alerts": [], "cursor": 0});
        assert!(validate_monitor_alert_response("watch", &empty)
            .unwrap()
            .is_empty());

        let missing_alerts = json!({"monitor": "watch", "cursor": 0});
        let error = validate_monitor_alert_response("watch", &missing_alerts).unwrap_err();
        assert!(error.contains("missing alerts"), "{error}");
        let unavailable = monitoring_unavailable(format!("monitor_alerts 'watch': {error}"));
        assert_eq!(unavailable["available"], false);

        let valid_alert = json!({
            "seq": 1,
            "monitor": "watch",
            "task": "task-1",
            "event_seq": 2,
            "fingerprint": "event:2",
            "kind": "turn_stalled",
            "payload": {"message": "synthetic"},
            "state": "open",
            "attempts": 0,
            "last_error": Value::Null,
            "created": 10.0,
            "updated": 10.0,
        });
        let response = json!({"monitor": "watch", "alerts": [valid_alert], "cursor": 1});
        assert_eq!(
            validate_monitor_alert_response("watch", &response)
                .unwrap()
                .len(),
            1
        );

        let malformed_alert = json!({
            "seq": 1,
            "monitor": "watch",
            "task": "task-1",
            "event_seq": 2,
            "fingerprint": "event:2",
            "kind": "turn_stalled",
            "state": "open",
            "attempts": 0,
            "last_error": Value::Null,
            "created": 10.0,
            "updated": 10.0,
        });
        let response = json!({
            "monitor": "watch",
            "alerts": [malformed_alert],
            "cursor": 1,
        });
        let error = validate_monitor_alert_response("watch", &response).unwrap_err();
        assert!(error.contains("missing payload"), "{error}");

        let wrong_monitor = json!({"monitor": "other", "alerts": [], "cursor": 0});
        let error = validate_monitor_alert_response("watch", &wrong_monitor).unwrap_err();
        assert!(error.contains("wrong monitor"), "{error}");
    }
}
