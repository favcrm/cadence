//! Merge-content and post-merge evidence.

use super::*;

/// Does the merge commit carry the reviewed head's change? Real merge
/// commits: ancestry. Squashes: the merge's patch-id vs the reviewed
/// diff's patch-id.
pub(super) fn contains_head(repo: &Path, row: &mut Row) {
    // Verify the *reviewed* head when a verdict named one — `headRefOid`
    // is mutable (a push to an undeleted branch moves it), so the note's
    // binding is the stable claim to check. With no note, the landed
    // head is all there is.
    let Some(head) = row
        .reviewed_head
        .clone()
        .or_else(|| row.landed_head.clone())
    else {
        row.contains_head = "unknown (no head recorded)".into();
        return;
    };
    if git(repo, &["cat-file".into(), "-e".into(), head.clone()]).is_err() {
        row.contains_head = "unknown (head not in local object store — never fetched)".into();
        row.unknowns
            .push(("contains_head".into(), format!("{head} not in local clone")));
        return;
    }
    // Direct ancestry — true merges and fast-forwards.
    if git(
        repo,
        &[
            "merge-base".into(),
            "--is-ancestor".into(),
            head.clone(),
            row.merge_sha.clone(),
        ],
    )
    .is_ok()
    {
        row.contains_head = "yes (ancestor)".into();
        row.tree_match = "yes (ancestor)".into();
        return;
    }
    // Squash: patch-id of the squash delta vs the reviewed diff.
    let merge_patch = patch_id(repo, &format!("{}^..{}", row.merge_sha, row.merge_sha));
    let base = git(
        repo,
        &[
            "merge-base".into(),
            format!("{}^", row.merge_sha),
            head.clone(),
        ],
    )
    .ok()
    .map(|s| s.trim().to_string());
    let head_patch = base.and_then(|b| patch_id(repo, &format!("{b}..{head}")));
    match (merge_patch, head_patch) {
        (Some(m), Some(h)) if m == h => {
            row.contains_head = "yes (patch-id match)".into();
            row.tree_match = "yes (patch-id match)".into();
        }
        (Some(_), Some(_)) => {
            row.contains_head = "no (patch-id differs)".into();
            row.tree_match = "no (patch-id differs)".into();
        }
        _ => {
            row.contains_head = "unknown (patch-id unavailable)".into();
            row.unknowns
                .push(("contains_head".into(), "patch-id comparison failed".into()));
        }
    }
}

/// Stable patch-id for a diff range (`git diff <range> | git patch-id`).
/// The `diff` half is `run_bounded`; `patch-id` reads a bounded in-memory
/// buffer, so its unbounded-looking wait is CPU-only.
pub(super) fn patch_id(repo: &Path, range: &str) -> Option<String> {
    use std::io::Write;
    let mut diff = Command::new("git");
    diff.arg("-C").arg(repo).args(["diff", "--patch", range]);
    let out = crate::proc::run_bounded(&mut diff, GIT_TIMEOUT).ok()?;
    if !out.status.success() {
        return None;
    }
    let mut pid = Command::new("git");
    let mut child = crate::reaper::spawn(
        pid.arg("patch-id")
            .arg("--stable")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped()),
    )
    .ok()?;
    child.stdin.take()?.write_all(&out.stdout).ok()?;
    let out = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace().next().map(str::to_string)
}

/// Every `(sha, body)` on the default branch — scanned once for the
/// per-row revert check in `post_merge`. Bounded: a revert of a merge
/// older than the newest 10k commits is archaeological, not evidence.
pub(super) fn branch_log(repo: &Path, default_ref: &str) -> Vec<(String, String)> {
    git(
        repo,
        &[
            "log".into(),
            "-n".into(),
            "10000".into(),
            "--format=%H%x1f%B%x1e".into(),
            default_ref.into(),
        ],
    )
    .map(|log| {
        log.split('\x1e')
            .filter_map(|entry| {
                let mut p = entry.split('\x1f');
                Some((p.next()?.to_string(), p.next().unwrap_or("").to_string()))
            })
            .collect()
    })
    .unwrap_or_default()
}

/// Post-merge outcome: later `Revert` commits on the default branch,
/// then daemon events for restart markers near the merge.
pub(super) fn post_merge(log: &[(String, String)], ev: &StoreEvidence, row: &mut Row) {
    // Revert: a later commit whose body says `This reverts commit <sha>`
    // or whose subject reverts the PR title.
    for (sha, body) in log {
        if sha == &row.merge_sha {
            continue;
        }
        let reverted = body.contains(&format!("This reverts commit {}", row.merge_sha))
            || row
                .pr
                .is_some_and(|n| body.contains("Revert") && body.contains(&format!("#{n}")));
        if reverted {
            row.revert = format!("yes ({})", &sha[..7.min(sha.len())]);
            break;
        }
    }
    // Daemon restart: an event named restart near the merge — the
    // event feed is the daemon's own record; absent kinds stay unknown.
    if let Some(at) = row.merged_at {
        for (kind, t) in &ev.events {
            if kind.contains("restart") && *t >= at && *t <= at + POST_MERGE_WINDOW_SECS {
                row.daemon_restart = format!("yes (event {kind})");
            }
        }
    }
    for (field, val) in [
        ("smoke", &row.smoke),
        ("daemon_restart", &row.daemon_restart),
    ] {
        if val.starts_with("unknown") {
            row.unknowns.push((
                field.into(),
                "no ops-merge note or daemon event records it".into(),
            ));
        }
    }
}
