//! CAD-536: `cadence doctor host` check `config` — moved verbatim from src/doctor/host.rs.

use super::*;

/// Host pressure: load1 vs cpu count plus io stall, with the slot
/// queue in the detail so a hot host names its cause. Everything
/// reads `scan.proc_root`, so tests fabricate both files.
/// `pm.yaml [host]` itself: ok when absent or applied, warn naming the
/// error when it could not be applied (defaults in force, WAL watch off).
pub(super) fn check_config(scan: &Scan) -> Check {
    let (level, detail, remedy) = match &scan.thresholds.config_error {
        None => (
            Level::Ok,
            "host thresholds applied".to_string(),
            String::new(),
        ),
        Some(e) => (
            Level::Warn,
            format!("{e} — every threshold is at its default and the WAL checkpoint watch is off"),
            "fix the [host] table in pm.yaml (unknown keys and bad values are refused)".to_string(),
        ),
    };
    Check {
        name: "config",
        level,
        value: json!({"error": scan.thresholds.config_error}),
        threshold: Value::Null,
        detail,
        remedy,
    }
}
