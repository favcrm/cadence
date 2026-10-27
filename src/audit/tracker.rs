//! Tracker.

use super::*;

pub(super) fn enrich_tracker(pm: Option<&Path>, row: &mut Row) {
    let Some(pm) = pm else { return };
    for id in &row.issues {
        if let Ok(entries) = std::fs::read_dir(pm) {
            for ent in entries.flatten() {
                if ent.path().join(id).join("issue.md").exists() {
                    if let Some(key) = ent.file_name().to_str() {
                        row.project = Some(key.to_string());
                    }
                }
            }
        }
    }
}
