//! CAD-536: `cadence doctor host` check `disk` — moved verbatim from src/doctor/host.rs.

use super::*;

use std::collections::BTreeSet;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;

fn fs_free(path: &Path) -> Option<FsFree> {
    let dev = std::fs::metadata(path).ok()?.dev();
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: c_path is a live NUL-terminated string and st is a
    // properly sized out-buffer; statvfs writes into it or returns -1.
    let mut st = unsafe { std::mem::zeroed::<libc::statvfs>() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut st) } != 0 {
        return None;
    }
    // Block counts are u64 on Linux but u32 on macOS; widen them there.
    #[allow(clippy::useless_conversion)]
    let (bavail, blocks, frsize) = (
        u64::from(st.f_bavail),
        u64::from(st.f_blocks),
        u64::from(st.f_frsize),
    );
    Some(FsFree {
        path: path.to_path_buf(),
        dev,
        free: bavail.saturating_mul(frsize),
        total: blocks.saturating_mul(frsize),
    })
}

/// Pure level for one filesystem — the threshold edges live here.
pub(super) fn fs_level(free: u64, total: u64, t: &Thresholds) -> Level {
    let pct = if total == 0 {
        100.0
    } else {
        free as f64 * 100.0 / total as f64
    };
    if pct < t.disk_fail_pct || free < t.disk_fail_free_bytes {
        Level::Fail
    } else if pct < t.disk_warn_pct || free < t.disk_warn_free_bytes {
        Level::Warn
    } else {
        Level::Ok
    }
}

pub(super) fn check_disk(scan: &Scan) -> Check {
    let mut paths = vec![
        scan.state_dir.clone(),
        scan.temp_dir.clone(),
        scan.home.clone(),
    ];
    if let Some(repo) = repo_root(&scan.cwd) {
        paths.push(repo);
    }
    let probe = scan.fs_probe.unwrap_or(fs_free);
    let mut seen = BTreeSet::new();
    let mut fses = Vec::new();
    for path in paths {
        if let Some(f) = probe(&path) {
            if seen.insert(f.dev) {
                fses.push(f);
            }
        }
    }
    eval_disk(&fses, &scan.thresholds)
}

fn eval_disk(fses: &[FsFree], t: &Thresholds) -> Check {
    let name = "disk";
    let threshold = json!(format!(
        "warn: free < {}% or {}; fail: free < {}% or {}",
        t.disk_warn_pct,
        human(t.disk_warn_free_bytes),
        t.disk_fail_pct,
        human(t.disk_fail_free_bytes)
    ));
    if fses.is_empty() {
        return check(
            name,
            Level::Ok,
            json!([]),
            threshold,
            "no filesystems probed".to_string(),
            String::new(),
        );
    }
    let level = fses
        .iter()
        .map(|f| fs_level(f.free, f.total, t))
        .max()
        .unwrap_or(Level::Ok);
    let detail = fses
        .iter()
        .map(|f| {
            format!(
                "{} {:.1}% free ({})",
                f.path.display(),
                f.pct(),
                human(f.free)
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let worst = fses.iter().min_by(|a, b| {
        a.pct()
            .partial_cmp(&b.pct())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let remedy = worst
        .map(|f| {
            format!(
                "du -xh --max-depth=1 {} | sort -h  # find the growth before writes fail",
                shell_quote(&f.path.display().to_string())
            )
        })
        .unwrap_or_default();
    let value = fses
        .iter()
        .map(|f| {
            json!({
                "path": f.path,
                "free_bytes": f.free,
                "free_pct": (f.pct() * 10.0).round() / 10.0,
                "level": fs_level(f.free, f.total, t).as_str(),
            })
        })
        .collect::<Vec<_>>();
    check(name, level, json!(value), threshold, detail, remedy)
}
