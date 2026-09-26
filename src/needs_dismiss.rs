//! CAD-574: operator-dismissed Needs-you rows, daemon-owned.
//!
//! The rail's Snooze/Dismiss writes one record per needs-me subject
//! (`kind:id`) in `<state>/needs_dismissed.json`, written only by the
//! `needs_dismiss` RPC (operator-only by connection). The overview
//! build — CLI, board read model, `cadence session` — reads the file
//! and drops a suppressed row, so every consumer sees the same list.
//!
//! Suppression names the OCCURRENCE the operator acted on, not the
//! subject forever:
//!
//! - `dismiss` hides the row while its start (`since`, else `now-age`,
//!   the same clock `age` is derived from) is not newer than the
//!   record's `at`. A condition that ends and comes back moves `since`
//!   (or the agent's `updated` the age is computed from) past `at` and
//!   the row returns; a row that never went away stays hidden.
//! - `snooze` hides the row only while `until` is in the future —
//!   re-occurrence or not, it comes back.
//!
//! An unreadable or absent file is "nothing dismissed" — the fail-safe
//! direction: a row the operator should see is shown, never hidden by a
//! record that cannot be read. Records are keyed `{kind}:{id}` like the
//! subject field itself.

use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::Result;

/// `<state>/needs_dismissed.json`.
pub fn dismissed_path(state_dir: &Path) -> PathBuf {
    state_dir.join("needs_dismissed.json")
}

/// Every dismissal record, `{"{kind}:{id}": {kind,id,mode,at,until?}}`.
/// An unreadable file is none — every row shows, the fail-safe
/// direction.
pub fn dismissed(state_dir: &Path) -> Map<String, Value> {
    std::fs::read_to_string(dismissed_path(state_dir))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// The subject key a row's dismissal is filed under — `"{kind}:{id}"`.
pub fn key(kind: &str, id: &str) -> String {
    format!("{kind}:{id}")
}

/// Is `record` suppressing a row that began at `row_started` (epoch
/// secs — `since`, else `now-age`), judged at `now`?
fn suppressed_by(record: &Value, row_started: i64, now: i64) -> bool {
    match record["mode"].as_str() {
        Some("snooze") => record["until"].as_i64().unwrap_or(0) > now,
        _ => row_started <= record["at"].as_i64().unwrap_or(i64::MIN),
    }
}

/// Is a serialized needs-me row (`subject`, `since`, `age` fields)
/// suppressed under `records` at `now`? A row without a dismissal
/// record is never suppressed.
pub fn row_suppressed(row: &Value, records: &Map<String, Value>, now: i64) -> bool {
    let (Some(kind), Some(id)) = (
        row["subject"]["kind"].as_str(),
        row["subject"]["id"].as_str(),
    ) else {
        return false;
    };
    let Some(record) = records.get(&key(kind, id)) else {
        return false;
    };
    // The row's own start clock: `since` when the daemon measured one,
    // else `now-age` — the apparent start a changing age re-derives, so
    // a re-occurring condition still moves past `at`.
    let started = row["since"]
        .as_i64()
        .unwrap_or_else(|| now - row["age"].as_i64().unwrap_or(0));
    suppressed_by(record, started, now)
}

/// Drop `rows` whose subject carries a live dismissal record (CAD-574).
pub fn filter_rows(rows: Vec<Value>, records: &Map<String, Value>, now: i64) -> Vec<Value> {
    if records.is_empty() {
        return rows;
    }
    rows.into_iter()
        .filter(|r| !row_suppressed(r, records, now))
        .collect()
}

static WRITER: Mutex<()> = Mutex::new(());

/// Record one dismissal (tmp + rename, writers serialized in-process).
/// Dead snooze records — `until` already past — are pruned on the same
/// write so the file does not grow on quiet hosts.
pub fn record(state_dir: &Path, record: Value, now: i64) -> Result<()> {
    let _guard = WRITER.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = dismissed(state_dir);
    all.retain(|_, rec| {
        rec["mode"].as_str() != Some("snooze") || rec["until"].as_i64().unwrap_or(0) > now
    });
    let k = key(
        record["kind"].as_str().unwrap_or_default(),
        record["id"].as_str().unwrap_or_default(),
    );
    all.insert(k, record);
    let path = dismissed_path(state_dir);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&Value::Object(all))?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}
