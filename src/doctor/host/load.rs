//! CAD-536: `cadence doctor host` check `load` — moved verbatim from src/doctor/host.rs.

use super::*;

/// The load warn line when `[host] load_warn_ratio` is unset: the
/// slot plan's own ceiling plus headroom — the farm is *meant* to run
/// `(build_slots + suite_slots) × jobs_per_lane` deep, so warn above
/// 1.25× that plan (never below plain saturation). The daemon's
/// resolved config rides `scan.slots`; unreachable, the built-in
/// defaults stand in.
fn planned_load_warn_ratio(scan: &Scan, cpus: f64) -> f64 {
    let cfg = scan.slots.as_ref().map(|s| &s["config"]);
    let key = |k: &str, d: f64| cfg.and_then(|c| c[k].as_f64()).unwrap_or(d);
    let planned_jobs =
        (key("build_slots", 3.0) + key("suite_slots", 1.0)) * key("jobs_per_lane", 4.0);
    (planned_jobs * 1.25 / cpus).max(1.0)
}

pub(super) fn check_load(scan: &Scan) -> Check {
    let name = "load";
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1) as f64;
    let warn_ratio = scan
        .thresholds
        .load_warn_ratio
        .unwrap_or_else(|| planned_load_warn_ratio(scan, cpus));
    let threshold = json!({
        "load1": format!("warn > {}x cpus, fail > {}x",
                         warn_ratio,
                         warn_ratio * 2.0),
        "io_stall_avg10_pct": format!("warn > {}, fail > {}",
                                     scan.thresholds.io_stall_warn_pct,
                                     scan.thresholds.io_stall_fail_pct),
    });
    let load1 = std::fs::read_to_string(scan.proc_root.join("loadavg"))
        .ok()
        .and_then(|t| t.split_whitespace().next()?.parse::<f64>().ok());
    let io_stall = std::fs::read_to_string(scan.proc_root.join("pressure/io"))
        .ok()
        .and_then(|t| {
            t.lines()
                .find(|l| l.starts_with("some"))?
                .split_whitespace()
                .find_map(|f| f.strip_prefix("avg10="))?
                .parse::<f64>()
                .ok()
        });
    if load1.is_none() && io_stall.is_none() {
        return check(
            name,
            Level::Ok,
            json!({"skipped": true}),
            threshold,
            format!(
                "no loadavg or pressure/io under {}",
                scan.proc_root.display()
            ),
            String::new(),
        );
    }
    let ratio = load1.map(|l| l / cpus);
    let level = if ratio.is_some_and(|r| r > warn_ratio * 2.0)
        || io_stall.is_some_and(|s| s > scan.thresholds.io_stall_fail_pct)
    {
        Level::Fail
    } else if ratio.is_some_and(|r| r > warn_ratio)
        || io_stall.is_some_and(|s| s > scan.thresholds.io_stall_warn_pct)
    {
        Level::Warn
    } else {
        Level::Ok
    };
    let slots_text = match &scan.slots {
        Some(s) => {
            let held = |pool: &str| s["pools"][pool]["held"].as_array().map_or(0, Vec::len);
            let cap = |pool: &str| s["pools"][pool]["capacity"].as_u64().unwrap_or(0);
            let waiting = s["waiting"].as_array().map_or(0, Vec::len);
            let longest = s["waiting"]
                .as_array()
                .map(|w| {
                    w.iter()
                        .map(|x| x["wait_secs"].as_f64().unwrap_or(0.0))
                        .fold(0.0, f64::max)
                })
                .unwrap_or(0.0);
            format!(
                "slots {}/{} build {}/{} suite ({} waiting, longest {})",
                held("build"),
                cap("build"),
                held("suite"),
                cap("suite"),
                waiting,
                crate::slots::fmt_wait(longest)
            )
        }
        None => "slots: daemon unreachable".to_string(),
    };
    check(
        name,
        level,
        json!({
            "load1": load1, "cpus": cpus, "load_ratio": ratio,
            "io_stall_avg10": io_stall,
            "slots": scan.slots.as_ref().map(|s| s["waiting"]
                .as_array().map_or(0, Vec::len)),
        }),
        threshold,
        format!(
            "load1 {} ({}x of {} cpus), io stall {}, {}",
            load1
                .map(|l| format!("{l:.1}"))
                .unwrap_or_else(|| "-".into()),
            ratio
                .map(|r| format!("{r:.1}"))
                .unwrap_or_else(|| "-".into()),
            cpus as u64,
            io_stall
                .map(|s| format!("{s:.0}% avg10"))
                .unwrap_or_else(|| "-".into()),
            slots_text
        ),
        if level == Level::Ok {
            String::new()
        } else {
            "cadence build-slot status  # who holds the build slots".to_string()
        },
    )
}
