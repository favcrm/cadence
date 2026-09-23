//! CAD-536: `cadence doctor host` check `provider_state` — moved verbatim from src/doctor/host.rs.

use super::*;

// ---------- provider state growth ----------

struct StoreMeasure {
    label: &'static str,
    path: PathBuf,
    /// Main file bytes, or the dir total for directory stores.
    store_bytes: u64,
    wal_bytes: Option<u64>,
    /// `store_bytes` is a lower bound — the dir walk hit its budget.
    truncated: bool,
    level: Level,
}

pub(super) fn store_level(store: u64, wal: Option<u64>, t: &Thresholds) -> Level {
    if wal.is_some_and(|w| w > t.wal_fail_bytes) {
        Level::Fail
    } else if wal.is_some_and(|w| w > t.wal_warn_bytes) || store > t.store_warn_bytes {
        Level::Warn
    } else {
        Level::Ok
    }
}

/// One sqlite store (file + `-wal` sibling), absent both = skipped.
fn sqlite_store(label: &'static str, db: &Path, t: &Thresholds) -> Option<StoreMeasure> {
    let store = file_size(db);
    let wal = wal_sibling(db).and_then(|w| file_size(&w));
    if store.is_none() && wal.is_none() {
        return None;
    }
    Some(StoreMeasure {
        label,
        path: db.to_path_buf(),
        store_bytes: store.unwrap_or(0),
        wal_bytes: wal,
        truncated: false,
        level: store_level(store.unwrap_or(0), wal, t),
    })
}

/// A directory store measured by total bytes under it.
fn dir_store(label: &'static str, dir: &Path, t: &Thresholds) -> Option<StoreMeasure> {
    if !dir.is_dir() {
        return None;
    }
    let (bytes, truncated) = dir_size(dir);
    Some(StoreMeasure {
        label,
        path: dir.to_path_buf(),
        store_bytes: bytes,
        wal_bytes: None,
        truncated,
        level: store_level(bytes, None, t),
    })
}

pub(super) fn check_provider_state(scan: &Scan) -> Check {
    let t = &scan.thresholds;
    let stores: Vec<StoreMeasure> = [
        sqlite_store(
            "devin sessions.db",
            &scan.devin_data.join("cli/sessions.db"),
            t,
        ),
        sqlite_store(
            "cadence sqlite3",
            &scan.state_dir.join("cadence.sqlite3"),
            t,
        ),
        dir_store("claude projects", &scan.claude_projects, t),
        dir_store("codex sessions", &scan.codex_sessions, t),
    ]
    .into_iter()
    .flatten()
    .collect();
    eval_provider_state(&stores, t)
}

fn eval_provider_state(stores: &[StoreMeasure], t: &Thresholds) -> Check {
    let name = "provider-state";
    let threshold = json!(format!(
        "warn: wal > {} or store > {}; fail: wal > {}",
        human(t.wal_warn_bytes),
        human(t.store_warn_bytes),
        human(t.wal_fail_bytes)
    ));
    if stores.is_empty() {
        return check(
            name,
            Level::Ok,
            json!([]),
            threshold,
            "no known provider stores present".to_string(),
            String::new(),
        );
    }
    let level = stores.iter().map(|s| s.level).max().unwrap_or(Level::Ok);
    let detail = stores
        .iter()
        .map(|s| {
            let size = if s.truncated {
                format!("at least {}", human(s.store_bytes))
            } else {
                human(s.store_bytes)
            };
            match s.wal_bytes {
                Some(wal) => format!("{} {} (+wal {})", s.label, size, human(wal)),
                None => format!("{} {}", s.label, size),
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    let remedy = stores
        .iter()
        .filter(|s| s.level > Level::Ok)
        .take(2)
        .map(|s| {
            if s.wal_bytes.is_some_and(|w| w > t.wal_warn_bytes) {
                format!(
                    "sqlite3 {} 'PRAGMA wal_checkpoint(TRUNCATE);'",
                    shell_quote(&s.path.display().to_string())
                )
            } else {
                format!(
                    "du -xh --max-depth=1 {} | sort -h",
                    shell_quote(&s.path.display().to_string())
                )
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    let value = stores
        .iter()
        .map(|s| {
            json!({
                "store": s.label,
                "path": s.path,
                "store_bytes": s.store_bytes,
                "wal_bytes": s.wal_bytes,
                "level": s.level.as_str(),
                // What the daemon's WAL watch would checkpoint under
                // `[host] wal_max_bytes` — the preview surface.
                "over_checkpoint_limit": s.wal_bytes.is_some_and(|w| w > t.wal_max_bytes),
            })
        })
        .collect::<Vec<_>>();
    check(name, level, json!(value), threshold, detail, remedy)
}
