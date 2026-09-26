//! CAD-536: `cadence doctor host` check `memory` — moved verbatim from src/doctor/host.rs.

use super::*;

// ---------- memory commitment + process census (linux) ----------

/// `/proc/meminfo`, the fields the watchdog needs. `Option` fields
/// distinguish "absent" from a real zero — a kernel too old for
/// `MemAvailable` must not read as "0 bytes free".
#[derive(Default)]
struct MemInfo {
    total: u64,
    available: Option<u64>,
    swap_total: Option<u64>,
    swap_free: Option<u64>,
    committed: Option<u64>,
    commit_limit: Option<u64>,
}

/// Parse `Key: NNN kB` lines from `proc_root/meminfo`. `None` when the
/// file is unreadable or `MemTotal` is missing.
fn read_meminfo(proc_root: &Path) -> Option<MemInfo> {
    let text = std::fs::read_to_string(proc_root.join("meminfo")).ok()?;
    let mut m = MemInfo::default();
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(kb) = rest
            .split_whitespace()
            .next()
            .and_then(|n| n.parse::<u64>().ok())
        else {
            continue;
        };
        let bytes = kb.saturating_mul(1024);
        match key.trim() {
            "MemTotal" => m.total = bytes,
            "MemAvailable" => m.available = Some(bytes),
            "SwapTotal" => m.swap_total = Some(bytes),
            "SwapFree" => m.swap_free = Some(bytes),
            "Committed_AS" => m.committed = Some(bytes),
            "CommitLimit" => m.commit_limit = Some(bytes),
            _ => {}
        }
    }
    (m.total > 0).then_some(m)
}

/// Memory pressure: `MemAvailable` and `SwapFree` against their
/// thresholds, `Committed_AS` against `CommitLimit` with the
/// overcommit mode named. Commitment over the limit is the CAD-154
/// failure — `fork()`/`malloc` refusal reads as EAGAIN, not ENOMEM.
pub(super) fn check_memory(scan: &Scan) -> Check {
    let name = "memory";
    let t = &scan.thresholds;
    let threshold = json!(format!(
        "warn: available <{}% RAM or swap free <{}%; fail: available <{}% \
         or (swap free <{}% with available <{}%); committed > limit is a \
         fail only under strict overcommit (mode 2)",
        t.mem_warn_pct, t.swap_warn_pct, t.mem_fail_pct, t.swap_fail_pct, t.mem_warn_pct
    ));
    if !scan.linux {
        return check(
            name,
            Level::Ok,
            json!({"skipped": true}),
            threshold,
            "memory pressure is linux-only".to_string(),
            String::new(),
        );
    }
    let Some(mem) = read_meminfo(&scan.proc_root) else {
        return check(
            name,
            Level::Ok,
            json!({"skipped": true}),
            threshold,
            "no readable meminfo".to_string(),
            String::new(),
        );
    };
    let overcommit = read_u64_file(&scan.proc_root.join("sys/vm/overcommit_memory"));
    let mut level = Level::Ok;

    let mut parts = Vec::new();
    // `avail_low` feeds the combined swap leg — swap exhaustion alone
    // is a warning; swap exhaustion *with* low MemAvailable is the
    // CAD-154 incident shape and is the fail.
    let mut avail_low = false;
    if let Some(avail) = mem.available {
        let pct = avail as f64 * 100.0 / mem.total as f64;
        avail_low = pct < t.mem_warn_pct;
        let leg = if pct < t.mem_fail_pct {
            Level::Fail
        } else if avail_low {
            Level::Warn
        } else {
            Level::Ok
        };
        level = level.max(leg);
        parts.push(format!(
            "available {} ({pct:.0}% of {})",
            human(avail),
            human(mem.total)
        ));
    } else {
        parts.push("MemAvailable absent".to_string());
    }
    match (mem.swap_total, mem.swap_free) {
        (Some(total), Some(free)) if total > 0 => {
            let pct = free as f64 * 100.0 / total as f64;
            // Swap exhausted while RAM is still available is the
            // steady state of a long-lived host — warn, not fail. The
            // fail needs both legs of the incident: swap <fail AND
            // MemAvailable already low. With MemAvailable absent the
            // other half can't be seen, so swap alone stays the vote.
            let leg = if pct < t.swap_fail_pct && (avail_low || mem.available.is_none()) {
                Level::Fail
            } else if pct < t.swap_warn_pct {
                Level::Warn
            } else {
                Level::Ok
            };
            level = level.max(leg);
            parts.push(format!(
                "swap free {} ({pct:.0}% of {})",
                human(free),
                human(total)
            ));
        }
        (Some(0), _) | (None, _) => parts.push("no swap".to_string()),
        _ => {}
    }
    // Committed_AS vs CommitLimit is only a hard signal under strict
    // overcommit (mode 2), where the kernel refuses once the limit
    // passes. Under the default heuristic (0) CommitLimit is advisory
    // — Committed_AS routinely exceeds it on a healthy host — and
    // mode 1 never enforces. An unreadable sysctl cannot prove
    // enforcement is off, so it warns rather than fails.
    let mut commit_over = false;
    if let (Some(committed), Some(limit)) = (mem.committed, mem.commit_limit) {
        commit_over = committed > limit;
        let mode = match overcommit {
            Some(0) => "heuristic",
            Some(1) => "always",
            Some(2) => "strict",
            Some(_) | None => "unknown",
        };
        parts.push(format!(
            "committed {} vs limit {} (overcommit_memory={mode})",
            human(committed),
            human(limit),
        ));
        if commit_over {
            level = level.max(match overcommit {
                Some(2) => Level::Fail,
                None => Level::Warn,
                _ => Level::Ok,
            });
        }
    }
    let mut detail = parts.join("; ");
    if commit_over && overcommit == Some(2) {
        detail.push_str(" — fork()/malloc refused (strict overcommit)");
    }
    // The remedy names the biggest process groups — counts, ages,
    // resident bytes — and never kills anything itself.
    let remedy = if level > Level::Ok {
        let groups = top_groups(census_of(scan), 3);
        if groups.is_empty() {
            "inspect `ps aux --sort=-rss | head` — cadence never kills".to_string()
        } else {
            format!(
                "largest groups: {}; restart or close the offenders — cadence never kills",
                groups
                    .iter()
                    .map(|(name, g)| group_line(name, g))
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        }
    } else {
        String::new()
    };
    let value = json!({
        "mem_total_bytes": mem.total,
        "mem_available_bytes": mem.available,
        "swap_total_bytes": mem.swap_total,
        "swap_free_bytes": mem.swap_free,
        "committed_bytes": mem.committed,
        "commit_limit_bytes": mem.commit_limit,
        "committed_over_limit": commit_over,
        "overcommit_memory": overcommit,
    });
    check(name, level, value, threshold, detail, remedy)
}
