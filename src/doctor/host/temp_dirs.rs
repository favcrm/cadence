//! CAD-536: `cadence doctor host` check `temp_dirs` — moved verbatim from src/doctor/host.rs.

use super::*;

use std::os::unix::fs::MetadataExt;

/// Temp-dir prefixes this project's helpers actually use: tempfile's
/// `.tmp*`, mktemp's `tmp.*` and cadence's own `cadence-*`
/// (`cadence-issue-at-*` exports, leaked state dirs).
const TEMP_PREFIXES: &[&str] = &["cadence-", ".tmp", "tmp."];

/// `cadence-nextest-<version>/cargo-nextest`, as written by
/// `scripts/install-cadence-nextest` into a task-local TMPDIR.
fn is_pinned_runner_dir(dir: &Path, name: &str) -> bool {
    name.strip_prefix("cadence-nextest-")
        .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.'))
        && std::fs::symlink_metadata(dir.join("cargo-nextest")).is_ok_and(|m| m.is_file())
}

pub(super) fn check_temp_dirs(scan: &Scan) -> Check {
    let name = "temp-dirs";
    let t = &scan.thresholds;
    let threshold = json!(format!(
        "warn: ≥{} dirs older than {}s, or ≥{} total",
        t.temp_warn_count,
        t.temp_min_age_secs,
        human(t.temp_warn_bytes)
    ));
    let mut hits: Vec<(PathBuf, u64)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&scan.temp_dir) {
        for ent in entries.flatten() {
            let name_s = ent.file_name().to_string_lossy().to_string();
            if !TEMP_PREFIXES.iter().any(|p| name_s.starts_with(p)) {
                continue;
            }
            let Ok(meta) = ent.metadata() else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            // The pinned test runner's task-local install is a tool, not a
            // leak: listing it put `rm -rf` on the reviewed binary and
            // every local review then blocked (CAD-273).
            if is_pinned_runner_dir(&ent.path(), &name_s) {
                continue;
            }
            // Only our own dirs — /tmp is shared, and the remedy prints
            // `rm -rf` for whatever makes the list.
            if meta.uid() != scan.uid {
                continue;
            }
            // An unreadable or future mtime (dir created mid-scan) is
            // treated as fresh — age 0 — never as a leak.
            let age = meta
                .modified()
                .ok()
                .and_then(|m| scan.now.duration_since(m).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if age < t.temp_min_age_secs {
                continue;
            }
            hits.push((ent.path(), 0));
        }
    }
    // Deterministic order with the cadence-specific leaks first —
    // read_dir order would shuffle the remedy line every run.
    hits.sort_by(|a, b| {
        let cad = |p: &PathBuf| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("cadence-"))
        };
        cad(&b.0).cmp(&cad(&a.0)).then(a.0.cmp(&b.0))
    });
    let mut total = 0;
    let mut truncated = false;
    for (path, bytes) in &mut hits {
        let (b, tr) = dir_size(path);
        *bytes = b;
        total += b;
        truncated |= tr;
    }
    let level = if hits.len() as u64 >= t.temp_warn_count || total >= t.temp_warn_bytes {
        Level::Warn
    } else {
        Level::Ok
    };
    let detail = if hits.is_empty() {
        "none".to_string()
    } else if truncated {
        format!("{} dirs, at least {} total", hits.len(), human(total))
    } else {
        format!("{} dirs, {} total", hits.len(), human(total))
    };
    let remedy = if hits.is_empty() {
        String::new()
    } else {
        format!(
            "rm -rf {}  # leaked test/state dirs older than a day{}",
            hits.iter()
                .take(5)
                .map(|(p, _)| shell_quote(&p.display().to_string()))
                .collect::<Vec<_>>()
                .join(" "),
            if hits.len() > 5 {
                format!(" ({} more not listed)", hits.len() - 5)
            } else {
                String::new()
            }
        )
    };
    let value = json!({
        "count": hits.len(),
        "bytes": total,
        "dirs": hits.iter().map(|(p, b)| json!({"path": p, "bytes": b})).collect::<Vec<_>>(),
    });
    check(name, level, value, threshold, detail, remedy)
}
