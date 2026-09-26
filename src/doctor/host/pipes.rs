//! CAD-536: `cadence doctor host` check `pipes` — moved verbatim from src/doctor/host.rs.

use super::*;

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt;

/// A default pipe is 64 KiB = 16 pages; `pipe-user-pages-*` limits are
/// page counts, so a pipe count maps to an estimate through this.
const PAGES_PER_PIPE: u64 = 16;

// ---------- pipe pressure (linux) ----------

#[derive(Default)]
pub(super) struct PipeStats {
    pub(super) fds: u64,
    pub(super) pipes: u64,
    pub(super) est_pages: u64,
    /// Top fifo holders: (pid, count), descending, at most 3.
    pub(super) top: Vec<(u32, u64)>,
    pub(super) vanished: u64,
    pub(super) denied: u64,
}

/// Count `pipe:[inode]` targets under `<proc>/<pid>/fd` for processes
/// owned by `scan.uid`. A pid vanishing or an unreadable fd dir is
/// normal — counted, never fatal.
pub(super) fn scan_pipes(scan: &Scan) -> PipeStats {
    let mut stats = PipeStats::default();
    let mut inodes = BTreeSet::new();
    let mut counts: BTreeMap<u32, u64> = BTreeMap::new();
    let Ok(pids) = std::fs::read_dir(&scan.proc_root) else {
        return stats;
    };
    for ent in pids.flatten() {
        let Some(pid) = ent.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        // The pipe-user-pages limit is per-user: only this user's pids.
        let Ok(meta) = ent.metadata() else {
            stats.vanished += 1;
            continue;
        };
        if meta.uid() != scan.uid {
            continue;
        }
        let fds = match std::fs::read_dir(ent.path().join("fd")) {
            Ok(fds) => fds,
            Err(e) => {
                match e.kind() {
                    std::io::ErrorKind::NotFound => stats.vanished += 1,
                    std::io::ErrorKind::PermissionDenied => stats.denied += 1,
                    _ => {}
                }
                continue;
            }
        };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let text = target.to_string_lossy();
            if let Some(inode) = text
                .strip_prefix("pipe:[")
                .and_then(|s| s.strip_suffix(']'))
            {
                stats.fds += 1;
                inodes.insert(inode.to_string());
                *counts.entry(pid).or_default() += 1;
            }
        }
    }
    stats.pipes = inodes.len() as u64;
    stats.est_pages = stats.pipes * PAGES_PER_PIPE;
    let mut top: Vec<(u32, u64)> = counts.into_iter().collect();
    top.sort_by_key(|e| std::cmp::Reverse(e.1));
    top.truncate(3);
    stats.top = top;
    stats
}

pub(super) fn check_pipes(scan: &Scan) -> Check {
    let name = "pipes";
    let t = &scan.thresholds;
    let threshold = json!("warn: est. pipe pages > fs.pipe-user-pages-soft");
    if !scan.linux {
        return check(
            name,
            Level::Ok,
            json!({"skipped": true}),
            threshold,
            "pipe pressure is linux-only".to_string(),
            String::new(),
        );
    }
    let sys = scan.proc_root.join("sys/fs");
    let soft = read_u64_file(&sys.join("pipe-user-pages-soft")).unwrap_or(t.pipe_est_pages_warn);
    let max_size = read_u64_file(&sys.join("pipe-max-size"));
    let stats = scan_pipes(scan);
    let clamped = stats.est_pages > soft;
    let level = if clamped { Level::Warn } else { Level::Ok };
    let mut detail = format!(
        "{} pipes on {} fds (~{} pages, soft {})",
        stats.pipes, stats.fds, stats.est_pages, soft
    );
    if clamped {
        detail.push_str(" — new pipes clamp to one page");
    }
    if let Some(max) = max_size {
        detail.push_str(&format!("; pipe-max-size {}", human(max)));
        if max < MIB {
            detail.push_str(" (already small)");
        }
    }
    if !stats.top.is_empty() {
        detail.push_str(&format!(
            "; top: {}",
            stats
                .top
                .iter()
                .map(|(pid, n)| format!("pid {pid} ×{n}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if stats.denied + stats.vanished > 0 {
        detail.push_str(&format!(
            "; {} unreadable, {} vanished mid-scan",
            stats.denied, stats.vanished
        ));
    }
    let remedy = kill_remedy(
        &scan.proc_root,
        &stats.top.iter().map(|(pid, _)| *pid).collect::<Vec<_>>(),
        "the biggest FIFO holders — stopping them releases the user's pipe pages",
    );
    let value = json!({
        "pipe_fds": stats.fds,
        "unique_pipes": stats.pipes,
        "est_pages": stats.est_pages,
        "soft_limit_pages": soft,
        "pipe_max_size_bytes": max_size,
        "clamped": clamped,
        "top": stats.top.iter().map(|(pid, n)| json!({"pid": pid, "fds": n})).collect::<Vec<_>>(),
        "unreadable": stats.denied,
        "vanished": stats.vanished,
    });
    check(name, level, value, threshold, detail, remedy)
}
