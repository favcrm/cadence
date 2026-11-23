//! Monitor health and durable alerts projected for the overview and
//! board — daemon `monitor_list`/`monitor_alerts` rows validated, then
//! projected into the board-facing view; read via daemon RPCs only.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::{HashMap, HashSet};
use std::path::Path;

use cadence_core::board_api::{MONITOR_CURSOR_FORMAT, READER_SCOPE_FIELD};
use serde_json::{json, Value};

use crate::client;
use crate::daemon::monitors_rpc::MONITOR_READER_SCOPE_ACK;

use super::{now_epoch, PROBE_TIMEOUT};

mod validate;
mod view;

pub use validate::validate_scoped_monitor_alert_response;
use validate::{
    validate_monitor_alert_response, validate_monitor_rows, validate_scoped_monitor_rows,
};
#[doc(hidden)]
pub use view::monitoring_unavailable;
use view::monitoring_view;

/// Read monitor health and durable alerts through the daemon socket.  The
/// board is allowed to show an explicit unavailable state when the daemon is
/// older than the monitor RPC; it must not open the live SQLite store itself.
pub fn monitoring(state_dir: &Path) -> Value {
    monitoring_with(state_dir, None, &json!({}))
}

/// The board-side reader-scoped projection. `relay_params` is built from the
/// verified read admission (`ReadAdmission::relay_proof().attach`), never
/// from a request claim: it carries `public_session`, `cli_envelope` or the
/// operator-proven `read_as` identity the daemon re-verifies. Without one the
/// projection fails closed with an explicit unavailable state — the board
/// never falls back to the operator's numeric monitor document.
#[doc(hidden)]
pub fn monitoring_for_tasks(
    state_dir: &Path,
    allowed: &HashSet<String>,
    relay_params: &Value,
) -> Value {
    monitoring_with(state_dir, Some(allowed), relay_params)
}

/// The relay fields a verified read admission attaches. A value that names
/// none of them is not a proven reader and must not reach the daemon as the
/// board's own operator connection.
fn relay_request(relay_params: &Value) -> Option<serde_json::Map<String, Value>> {
    let object = relay_params.as_object()?.clone();
    if ["public_session", "read_as", "cli_envelope"]
        .iter()
        .any(|field| object.contains_key(*field))
    {
        Some(object)
    } else {
        None
    }
}

fn monitoring_with(
    state_dir: &Path,
    allowed: Option<&HashSet<String>>,
    relay_params: &Value,
) -> Value {
    let now = now_epoch();
    let scoped = allowed.is_some();
    let relay = if scoped {
        match relay_request(relay_params) {
            Some(relay) => relay,
            None => return monitoring_unavailable("monitoring reader scope is unavailable"),
        }
    } else {
        serde_json::Map::new()
    };
    let list = match client::rpc_timeout(
        state_dir,
        "monitor_list",
        Value::Object(relay.clone()),
        PROBE_TIMEOUT,
    ) {
        Ok(value) => value,
        Err(error) => {
            return monitoring_unavailable(error.to_string());
        }
    };
    let Some(rows) = list["monitors"].as_array() else {
        return monitoring_unavailable("monitor_list response missing monitors");
    };
    if scoped {
        if list[READER_SCOPE_FIELD].as_str() != Some(MONITOR_READER_SCOPE_ACK) {
            return monitoring_unavailable("monitor scope is unavailable");
        }
        if let Err(error) = validate_scoped_monitor_rows(rows) {
            return monitoring_unavailable(format!("monitor scope is unavailable: {error}"));
        }
    } else if let Err(error) = validate_monitor_rows(rows) {
        return monitoring_unavailable(error);
    }
    let monitors = rows
        .iter()
        .filter(|monitor| {
            allowed.is_none_or(|allowed| {
                monitor["coverage"].as_array().is_some_and(|coverage| {
                    !coverage.is_empty()
                        && coverage
                            .iter()
                            .all(|task| task.as_str().is_some_and(|task| allowed.contains(task)))
                })
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut alerts_by_monitor = HashMap::new();
    for monitor in &monitors {
        let id = monitor["id"].as_str().unwrap_or_default();
        let mut params = relay.clone();
        params.insert("monitor".to_string(), json!(id));
        params.insert("open".to_string(), json!(false));
        params.insert("limit".to_string(), json!(100));
        if scoped {
            params.insert("cursor_format".to_string(), json!(MONITOR_CURSOR_FORMAT));
        }
        match client::rpc_timeout(
            state_dir,
            "monitor_alerts",
            Value::Object(params),
            PROBE_TIMEOUT,
        ) {
            Ok(value) => {
                let validated = if scoped {
                    validate_scoped_monitor_alert_response(id, &value)
                } else {
                    validate_monitor_alert_response(id, &value)
                };
                match validated {
                    Ok(rows) => {
                        if let Some(allowed) = allowed {
                            let coverage =
                                monitor["coverage"].as_array().expect("validated coverage");
                            if rows.iter().any(|alert| {
                                alert["task"].as_str().is_none_or(|task| {
                                    !allowed.contains(task)
                                        || !coverage
                                            .iter()
                                            .any(|covered| covered.as_str() == Some(task))
                                })
                            }) {
                                return monitoring_unavailable(
                                    "monitor alert scope is unavailable",
                                );
                            }
                        }
                        alerts_by_monitor.insert(id.to_string(), rows.to_vec());
                    }
                    Err(error) => {
                        return monitoring_unavailable(format!("monitor_alerts '{id}': {error}"));
                    }
                }
            }
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
