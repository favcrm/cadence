//! CAD-536: `cadence doctor host` check `worktrees` — moved verbatim from src/doctor/host.rs.

use super::*;

use crate::worktree::layout;

pub(super) fn check_worktrees(scan: &Scan) -> Check {
    let name = "worktrees";
    let threshold =
        json!("warn: any worktree whose branch is merged or whose tracker ref is closed");
    let Some(root) = repo_root(&scan.cwd) else {
        return check(
            name,
            Level::Ok,
            json!({"skipped": true}),
            threshold,
            format!("{} is not inside a git repo", scan.cwd.display()),
            String::new(),
        );
    };
    let wt_root = layout::worktrees_dir(&root);
    if !wt_root.is_dir() {
        return check(
            name,
            Level::Ok,
            json!({"skipped": true}),
            threshold,
            format!("no {} under {}", layout::WORKTREES_REL, root.display()),
            String::new(),
        );
    }
    let (stale, remedies, scanned) = stale_worktrees(scan, &root, &wt_root);
    // The shared cargo cache counts once, at the repo level — it is
    // not part of any worktree's own footprint.
    let shared = crate::worktree::shared_target_dir(&root);
    let shared_size = shared.is_dir().then(|| {
        let (bytes, truncated) = dir_size(&shared);
        json!({"path": shared, "bytes": bytes, "bytes_truncated": truncated})
    });
    let level = if stale.is_empty() {
        Level::Ok
    } else {
        Level::Warn
    };
    let shared_note = shared_size
        .as_ref()
        .map(|s| {
            format!(
                "; shared cargo cache {}{}",
                if s["bytes_truncated"].as_bool().unwrap_or(false) {
                    "at least "
                } else {
                    ""
                },
                human(s["bytes"].as_u64().unwrap_or(0))
            )
        })
        .unwrap_or_default();
    let detail = if stale.is_empty() {
        format!("{scanned} worktrees, none stale{shared_note}")
    } else {
        format!(
            "{} of {} worktrees stale ({}{}{})",
            stale.len(),
            scanned,
            if stale
                .iter()
                .any(|s| s["bytes_truncated"].as_bool().unwrap_or(false))
            {
                "at least "
            } else {
                ""
            },
            human(stale.iter().map(|s| s["bytes"].as_u64().unwrap_or(0)).sum()),
            shared_note
        )
    };
    check(
        name,
        level,
        json!({
            "scanned": scanned,
            "stale": stale,
            "shared_cargo_target": shared_size,
        }),
        threshold,
        detail,
        remedies.into_iter().take(4).collect::<Vec<_>>().join("; "),
    )
}
