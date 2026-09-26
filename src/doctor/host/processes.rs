//! CAD-536: `cadence doctor host` check `processes` — moved verbatim from src/doctor/host.rs.

use super::*;

/// The process-group census — informational: per-family counts, total
/// resident bytes and the oldest idle instance, so a leaked session
/// is visible before it starves the host. Never alarms; `memory`
/// carries the thresholds.
pub(super) fn check_processes(scan: &Scan) -> Check {
    let name = "processes";
    let threshold = json!("informational — no threshold");
    if !scan.linux {
        return check(
            name,
            Level::Ok,
            json!({"skipped": true}),
            threshold,
            "process census is linux-only".to_string(),
            String::new(),
        );
    }
    let census = census_of(scan);
    let top = top_groups(census, 5);
    let mut detail = format!("{} procs", census.procs);
    if !top.is_empty() {
        detail.push_str(&format!(
            ": {}",
            top.iter()
                .map(|(name, g)| group_line(name, g))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if census.unreadable + census.vanished > 0 {
        detail.push_str(&format!(
            "; {} unreadable, {} vanished mid-scan",
            census.unreadable, census.vanished
        ));
    }
    let groups: Vec<Value> = census
        .groups
        .iter()
        .map(|(name, g)| {
            json!({
                "group": name,
                "count": g.count,
                "rss_bytes": g.rss_bytes,
                "uids": g.uids,
                "oldest": g.oldest.as_ref().map(|o| json!({
                    "pid": o.pid,
                    "age_secs": o.age_secs,
                    "cpu_secs": o.cpu_secs,
                    "idle": o.idle,
                })),
                "oldest_idle": g.oldest_idle.as_ref().map(|o| json!({
                    "pid": o.pid,
                    "age_secs": o.age_secs,
                    "cpu_secs": o.cpu_secs,
                })),
            })
        })
        .collect();
    let value = json!({
        "procs": census.procs,
        "groups": groups,
        "unreadable": census.unreadable,
        "vanished": census.vanished,
    });
    check(name, Level::Ok, value, threshold, detail, String::new())
}
