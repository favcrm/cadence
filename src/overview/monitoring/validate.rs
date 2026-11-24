//! Validation helpers for daemon `monitor_list` and `monitor_alerts`
//! responses before projecting overview and board state.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use cadence_core::board_api::{MONITOR_CURSOR_FORMAT, READER_SCOPE_FIELD};
use serde_json::Value;

use crate::daemon::monitor_scope::MONITOR_READER_SCOPE_ACK;

/// Validate the stable fields emitted by `monitor_list` before projecting any
/// rows. A partially readable response is not a valid empty registration:
/// failing closed keeps the Overview from claiming a healthy subset.
pub(super) fn validate_monitor_rows(rows: &[Value]) -> Result<(), String> {
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

/// Validate a CAD-2215 scoped `monitor_list` page. It must carry the
/// daemon's reader-scope acknowledgement and no monitor row may carry the
/// internal `event_cursor` watermark; a daemon that leaked either is treated
/// as unavailable instead of trusted.
pub(super) fn validate_scoped_monitor_rows(rows: &[Value]) -> Result<(), String> {
    validate_monitor_rows(rows)?;
    for (index, row) in rows.iter().enumerate() {
        if row.get("event_cursor").is_some() {
            return Err(format!("monitor_list row {index} exposed a global cursor"));
        }
    }
    Ok(())
}

/// Validate the complete `monitor_alerts` response before exposing any
/// durable evidence to the Overview. An empty alert array is valid; a
/// missing or partially malformed response is unavailable rather than a
/// misleading healthy monitor view.
pub(super) fn validate_monitor_alert_response<'a>(
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

/// Validate a CAD-2215 scoped `monitor_alerts` page. Each row must carry the
/// stable keyed `id` and encrypted `ack` and must NOT carry a global `seq`,
/// `event_seq` or `fingerprint`; the page must acknowledge its reader scope
/// and speak the opaque cursor format. Any deviation is unavailable, never a
/// trusted numeric fallback.
pub fn validate_scoped_monitor_alert_response<'a>(
    monitor_id: &str,
    response: &'a Value,
) -> Result<&'a [Value], String> {
    if response[READER_SCOPE_FIELD].as_str() != Some(MONITOR_READER_SCOPE_ACK) {
        return Err("monitor_alerts response has no scoped reader acknowledgement".to_string());
    }
    if response["cursor_format"].as_str() != Some(MONITOR_CURSOR_FORMAT) {
        return Err("monitor_alerts response has an unsupported cursor format".to_string());
    }
    if !matches!(
        response.get("cursor"),
        Some(Value::Null) | Some(Value::String(_))
    ) {
        return Err("monitor_alerts response has malformed cursor".to_string());
    }
    let object = response
        .as_object()
        .ok_or_else(|| "monitor_alerts response is not an object".to_string())?;
    if object
        .get("monitor")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        != Some(monitor_id)
    {
        return Err("monitor_alerts response has the wrong monitor".to_string());
    }
    let rows = object
        .get("alerts")
        .and_then(Value::as_array)
        .ok_or_else(|| "monitor_alerts response missing alerts".to_string())?;
    for (index, row) in rows.iter().enumerate() {
        let alert = row
            .as_object()
            .ok_or_else(|| format!("monitor_alerts row {index} is not an object"))?;
        for forbidden in ["seq", "event_seq", "fingerprint"] {
            if alert.contains_key(forbidden) {
                return Err(format!(
                    "monitor_alerts row {index} exposed the global position field {forbidden}"
                ));
            }
        }
        let string_field = |field: &str| {
            alert
                .get(field)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("monitor_alerts row {index} missing {field}"))
        };
        string_field("id")?;
        string_field("ack")?;
        let row_monitor = string_field("monitor")?;
        if row_monitor != monitor_id {
            return Err(format!(
                "monitor_alerts row {index} belongs to a different monitor"
            ));
        }
        string_field("task")?;
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
