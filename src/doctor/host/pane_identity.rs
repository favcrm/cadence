//! CAD-536: `cadence doctor host` check `pane_identity` — moved verbatim from src/doctor/host.rs.

use super::*;

// ---------- leaked temp dirs ----------

/// CAD-385: every agent row's recorded pid carries the process start
/// time read when it was recorded (`agents.pid_start`), and the daemon,
/// the board and `rollout release --force` map a caller's pid to an
/// alias only while that start still matches. A row with NO recorded
/// start — written before schema v14, by a daemon on an older build, or
/// with `/proc` unreadable — cannot be told apart from a reused pid, so
/// it fails closed: any caller descending from it is refused. Those
/// rows warn here with the remedy. A row whose pid now names another
/// process (or none) is stale: it maps nothing, so it is reported but
/// does not warn. Read-only; `/proc` is `scan.proc_root`.
pub(super) fn check_pane_identity(scan: &Scan) -> Check {
    let name = "pane-identity";
    let threshold = json!(
        "warn: a recorded agent pid has no recorded process start time (fails closed \
         for caller identity)"
    );
    let path = scan.state_dir.join("cadence.sqlite3");
    let skipped = |detail: String| {
        check(
            name,
            Level::Ok,
            json!({"skipped": true}),
            threshold.clone(),
            detail,
            String::new(),
        )
    };
    if !path.exists() {
        return skipped("no store — no recorded agent pids".to_string());
    }
    let Ok(conn) = crate::store::open_read_only(&path) else {
        return skipped("store not readable read-only — see `sessions`".to_string());
    };
    // A store older than v14 has no `pid_start`: every row is legacy.
    let has_start = conn
        .prepare("SELECT 1 FROM pragma_table_info('agents') WHERE name='pid_start'")
        .and_then(|mut st| st.exists([]))
        .unwrap_or(false);
    let start = if has_start { "pid_start" } else { "NULL" };
    let rows: Vec<(String, u32, Option<u64>)> = match conn.prepare(&format!(
        "SELECT alias, pid, {start} FROM agents WHERE pid IS NOT NULL AND pid > 1 \
         ORDER BY alias"
    )) {
        Ok(mut st) => st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            })
            .map(|rows| {
                rows.flatten()
                    .filter_map(|(alias, pid, start)| {
                        Some((
                            alias,
                            u32::try_from(pid).ok()?,
                            start.and_then(|s| u64::try_from(s).ok()),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => return skipped("store has no readable agents table".to_string()),
    };
    let mut legacy = Vec::new();
    let mut stale = Vec::new();
    for (alias, pid, recorded) in &rows {
        let now = proc_stat(&scan.proc_root.join(pid.to_string())).map(|p| p.start_jiffies);
        match (recorded, now) {
            (None, _) => legacy.push(format!("{alias} (pid {pid})")),
            (Some(r), Some(n)) if *r == n => {}
            (Some(_), _) => stale.push(format!("{alias} (pid {pid})")),
        }
    }
    let value = json!({
        "recorded": rows.len(),
        "no_start_time": legacy,
        "stale": stale,
        "schema_has_pid_start": has_start,
    });
    if legacy.is_empty() {
        let detail = if stale.is_empty() {
            format!(
                "{} recorded agent pid(s), each proven by its start time",
                rows.len()
            )
        } else {
            format!(
                "{} recorded agent pid(s); stale (pid reused or gone — maps no alias): {}",
                rows.len(),
                stale.join(", ")
            )
        };
        return check(name, Level::Ok, value, threshold, detail, String::new());
    }
    check(
        name,
        Level::Warn,
        value,
        threshold,
        format!(
            "{} agent row(s) with a pid but no recorded process start time — callers \
             descending from them are refused: {}",
            legacy.len(),
            legacy.join(", ")
        ),
        crate::peer::PID_START_REMEDY.to_string(),
    )
}
