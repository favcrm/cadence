//! CAD-536: `cadence doctor host` check `cadence_store` — moved verbatim from src/doctor/host.rs.

use super::*;

/// CAD-316: cadence's own store — file and WAL size plus the event
/// bookkeeping retention acts on: rows, delivery rows the daemon has
/// rolled into counts, and rows past the age cut still awaiting a
/// rollup. Informational; the size thresholds stay in provider-state.
/// Opened read-only, like the census, so doctor never creates or
/// migrates the file.
pub(super) fn check_cadence_store(scan: &Scan) -> Check {
    let name = "cadence-store";
    let threshold = json!("informational — size thresholds are provider-state's");
    let db = scan.state_dir.join("cadence.sqlite3");
    let Some(store_bytes) = file_size(&db) else {
        return check(
            name,
            Level::Ok,
            json!({"path": db, "present": false}),
            threshold,
            "no cadence.sqlite3 in this state dir".to_string(),
            String::new(),
        );
    };
    let wal_bytes = wal_sibling(&db).and_then(|w| file_size(&w));
    let size = match wal_bytes {
        Some(wal) => format!(
            "cadence.sqlite3 {} (+wal {})",
            human(store_bytes),
            human(wal)
        ),
        None => format!("cadence.sqlite3 {}", human(store_bytes)),
    };
    let cutoff = scan
        .now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
        - crate::store::EVENT_ROLLUP_AGE_SECS;
    let stats = crate::store::open_read_only(&db)
        .and_then(|conn| crate::store::event_store_stats(&conn, cutoff));
    let mut value = json!({
        "path": db,
        "present": true,
        "store_bytes": store_bytes,
        "wal_bytes": wal_bytes,
    });
    let detail = match stats {
        Ok(stats) => {
            value["events"] = json!(stats.events);
            value["delivery_rolled_up"] = json!(stats.rolled_up);
            value["delivery_awaiting_rollup"] = json!(stats.awaiting_rollup);
            let days = crate::store::EVENT_ROLLUP_AGE_SECS / 86_400.0;
            format!(
                "{size}; {} events; {} delivery events rolled up into counts, \
                 {} older than {days}d awaiting rollup",
                stats.events, stats.rolled_up, stats.awaiting_rollup
            )
        }
        Err(e) => {
            value["error"] = json!(e.to_string());
            format!("{size}; events unreadable: {e}")
        }
    };
    check(name, Level::Ok, value, threshold, detail, String::new())
}
