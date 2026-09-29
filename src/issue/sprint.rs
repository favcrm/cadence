//! CAD-758: sprint batch verbs — the pick-batch ritual as tracker
//! commands instead of hand-run `ls`/`tag`/`set` passes.
//!
//! The model agreed on 2026-09-28 is three-layered: `status: ready`
//! is the dispatch queue, a tag like `sprint-2026w40` is the *named
//! pick batch* an agent draws from this cycle (multi-valued, so it
//! stacks with `milestone` which stays the persistent release scope),
//! and `blocked_by`+`priority` order the batch. These verbs keep the
//! ritual honest without inventing a fourth scope object:
//!
//! - [`close`]: reports per-item outcome over the tagged set (done /
//!   in-flight / untouched / blocked stragglers), strips the tag from
//!   finished items, and moves every survivor in one bulk commit —
//!   re-tagged to `--next` or dropped to its plain status with
//!   `--drop`. The velocity line (done vs carry-over) is the sprint
//!   review.
//! - [`open`]: populates a batch tag from computed-ready leaves in
//!   priority-then-id order up to `--cap`, skipping blocked items —
//!   the same set the dispatch path would refuse anyway.
//!
//! Nothing here is hardcoded to `sprint-*`: any tag works, so
//! `batch-*`, `week-*` or a project scheme all ride the same verbs.

use std::path::Path;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::write::{check_tags, commit_staged, stage};
use crate::issue::{board, model, Pm};

/// Default batch size — sized to review throughput, not worker count.
pub const DEFAULT_CAP: usize = 10;

/// Priority rank for pick order — P0 first, an unset priority sorts
/// last (unsized issues don't jump the queue).
fn priority_rank(priority: &str) -> u8 {
    priority
        .strip_prefix('P')
        .and_then(|d| d.parse::<u8>().ok())
        .unwrap_or(u8::MAX)
}

/// The issues carrying `tag`, as live views (blocked/ready computed).
fn tagged_views<'v>(views: &'v [board::View], tag: &str) -> Vec<&'v board::View> {
    views
        .iter()
        .filter(|v| v.issue.front.tags.iter().any(|t| t == tag))
        .collect()
}

fn live_views(pm: &Pm, state_dir: Option<&Path>) -> Result<Vec<board::View>> {
    let issues = board::load_all(&pm.dir, None)?;
    let jobs = state_dir.map(board::fetch_job_outcomes).unwrap_or_default();
    Ok(board::views_with_jobs(
        &pm.config.notes_dir(),
        issues,
        &jobs,
    ))
}

/// `issue sprint close <tag> --next <tag>|--drop [--dry-run]` — one
/// commit moves every affected item: the tag comes off finished work
/// and off every survivor (re-tagged to `next` when given). The JSON
/// report is the review artifact; `dry_run` reports without writing.
#[allow(clippy::too_many_arguments)]
pub fn close(
    pm: &Pm,
    tag: &str,
    next: Option<&str>,
    drop: bool,
    dry_run: bool,
    state_dir: Option<&Path>,
    actor: &str,
) -> Result<Value> {
    let tag = model::normalize_tags(&[tag.to_string()])?
        .into_iter()
        .next()
        .unwrap();
    let next = match next {
        Some(n) => Some(
            model::normalize_tags(&[n.to_string()])?
                .into_iter()
                .next()
                .unwrap(),
        ),
        None => None,
    };
    if next.is_none() && !drop {
        return Err(Error::rejected(
            "sprint close needs a survivor choice — `--next <tag>` re-tags \
             them into the next batch, `--drop` returns them to their \
             plain status (`--dry-run` previews first)",
        ));
    }
    if next.as_deref() == Some(tag.as_str()) {
        return Err(Error::rejected(format!(
            "--next {tag} is the tag being closed — name the next batch, \
             or `--drop` to close without a successor"
        )));
    }
    let views = live_views(pm, state_dir)?;
    let tagged = tagged_views(&views, &tag);
    if tagged.is_empty() {
        return Err(Error::rejected(format!(
            "no issues carry tag '{tag}' — `cadence issue ls --tag {tag}`",
        )));
    }
    // Classification is read from the pre-write board — a survivor is
    // anything not closed; blocked stragglers get their own list
    // whichever way the status reads.
    let mut done: Vec<&str> = Vec::new();
    let mut in_flight: Vec<&str> = Vec::new();
    let mut untouched: Vec<&str> = Vec::new();
    let mut blocked: Vec<String> = Vec::new();
    for v in &tagged {
        let f = &v.issue.front;
        if v.blocked {
            blocked.push(f.id.clone());
        }
        match v.status.as_str() {
            "done" | "dropped" => done.push(&f.id),
            "doing" | "review" => in_flight.push(&f.id),
            _ => untouched.push(&f.id),
        }
    }
    let report = |write: &str| {
        json!({
            "tag": tag,
            "write": write,
            "done": done,
            "in_flight": in_flight,
            "untouched": untouched,
            "blocked": blocked,
            "velocity": {
                "done": done.len(),
                "carry_over": in_flight.len() + untouched.len(),
            },
        })
    };
    if dry_run {
        return Ok(report("dry_run"));
    }
    let _lock = pm.lock()?;
    // The done set sheds the tag; survivors shed it too and take the
    // next tag in the same commit when `--next` named one.
    let ids: Vec<String> = tagged.iter().map(|v| v.issue.front.id.clone()).collect();
    let survivors: std::collections::HashSet<&str> = tagged
        .iter()
        .filter(|v| !matches!(v.status.as_str(), "done" | "dropped"))
        .map(|v| v.issue.front.id.as_str())
        .collect();
    let staged = stage(pm, &ids, |project, front| {
        let mut tags: Vec<String> = front.tags.iter().filter(|t| **t != tag).cloned().collect();
        if let Some(n) = &next {
            if survivors.contains(front.id.as_str()) {
                tags.push(n.clone());
            }
        }
        let tags = check_tags(project, &tags)?;
        let changed = tags != front.tags;
        front.tags = tags;
        Ok(changed)
    })?;
    let summary = match &next {
        Some(n) => format!("sprint close {tag} — carry to {n}"),
        None => format!("sprint close {tag}"),
    };
    let (ids, foreign) = commit_staged(pm, &staged, &summary, actor)?;
    let mut out = report("committed");
    out["ids"] = json!(ids);
    if !foreign.is_empty() {
        out["foreign_files"] = json!(foreign);
    }
    Ok(out)
}

/// `issue sprint open <tag> [--cap N] [--dry-run]` — tag the first
/// `--cap` computed-ready leaves in priority-then-id order. Blocked
/// ready items are skipped and named: the pick batch is what agents
/// can actually draw from.
pub fn open(
    pm: &Pm,
    tag: &str,
    cap: usize,
    dry_run: bool,
    state_dir: Option<&Path>,
    actor: &str,
) -> Result<Value> {
    let tag = model::normalize_tags(&[tag.to_string()])?
        .into_iter()
        .next()
        .unwrap();
    let views = live_views(pm, state_dir)?;
    let mut pool: Vec<&board::View> = views
        .iter()
        .filter(|v| {
            v.issue.front.status == "ready"
                && !v.container
                && !v.issue.front.tags.iter().any(|t| t == &tag)
        })
        .collect();
    let blocked: Vec<String> = pool
        .iter()
        .filter(|v| v.blocked)
        .map(|v| v.issue.front.id.clone())
        .collect();
    pool.retain(|v| !v.blocked);
    pool.sort_by(|a, b| {
        let (fa, fb) = (&a.issue.front, &b.issue.front);
        priority_rank(&fa.priority)
            .cmp(&priority_rank(&fb.priority))
            .then_with(|| fa.id.cmp(&fb.id))
    });
    let take: Vec<String> = pool
        .iter()
        .take(cap)
        .map(|v| v.issue.front.id.clone())
        .collect();
    let capped_out: Vec<String> = pool
        .iter()
        .skip(cap)
        .map(|v| v.issue.front.id.clone())
        .collect();
    if dry_run {
        return Ok(json!({
            "tag": tag, "write": "dry_run", "cap": cap,
            "added": take, "pool": pool.len(),
            "skipped_blocked": blocked, "capped_out": capped_out,
        }));
    }
    if take.is_empty() {
        return Err(Error::rejected(format!(
            "nothing to tag — no unblocked ready leaves outside '{tag}' \
             ({} ready blocked, {} over cap would have been surplus)",
            blocked.len(),
            capped_out.len(),
        )));
    }
    let _lock = pm.lock()?;
    let staged = stage(pm, &take, |project, front| {
        let mut tags = front.tags.clone();
        tags.push(tag.clone());
        let tags = check_tags(project, &tags)?;
        let changed = tags != front.tags;
        front.tags = tags;
        Ok(changed)
    })?;
    let (ids, foreign) = commit_staged(pm, &staged, &format!("sprint open {tag}"), actor)?;
    let mut out = json!({
        "tag": tag, "write": "committed", "cap": cap,
        "added": ids, "pool": pool.len(),
        "skipped_blocked": blocked, "capped_out": capped_out,
    });
    if !foreign.is_empty() {
        out["foreign_files"] = json!(foreign);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::write::{issue_dir, load_front, new_issue, project_add, save_front};

    fn tracker() -> (tempfile::TempDir, Pm) {
        let dir = tempfile::tempdir().unwrap();
        let pm = Pm::init(&dir.path().join("pm")).unwrap();
        project_add(&pm, "cadence", "CAD", &[], &[], &[], None).unwrap();
        (dir, pm)
    }

    fn mk(pm: &Pm, dir: &Path, title: &str, priority: Option<&str>) -> String {
        new_issue(
            pm,
            dir,
            Some("cadence"),
            title,
            priority,
            None,
            &[],
            None,
            None,
            &[],
            None,
            None,
            "t",
        )
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn edit(pm: &Pm, id: &str, f: impl FnOnce(&mut model::Front)) {
        let (_p, dir) = issue_dir(pm, id).unwrap();
        let (mut front, body) = load_front(&dir).unwrap();
        f(&mut front);
        save_front(&dir, &front, &body).unwrap();
    }

    fn tags(pm: &Pm, id: &str) -> Vec<String> {
        let (_p, dir) = issue_dir(pm, id).unwrap();
        load_front(&dir).unwrap().0.tags
    }

    /// close refuses without a survivor choice — the report is not a
    /// write path.
    #[test]
    fn close_needs_next_or_drop() {
        let (tmp, pm) = tracker();
        let a = mk(&pm, tmp.path(), "a", None);
        edit(&pm, &a, |f| f.tags = vec!["sprint-1".to_string()]);
        let e = close(&pm, "sprint-1", None, false, false, None, "t").unwrap_err();
        assert!(e.to_string().contains("--next"), "{e}");
        assert!(tags(&pm, &a).contains(&"sprint-1".to_string()));
    }

    /// close on a tag nobody carries is an error, not a silent no-op.
    #[test]
    fn close_refuses_an_unknown_tag() {
        let (_tmp, pm) = tracker();
        let e = close(&pm, "sprint-9", Some("sprint-10"), false, false, None, "t").unwrap_err();
        assert!(e.to_string().contains("no issues"), "{e}");
    }

    /// The whole contract in one pass: done sheds the tag, survivors
    /// carry it to the next batch in one commit, blocked stragglers
    /// are named, velocity counts both.
    #[test]
    fn close_retags_survivors_and_reports_velocity() {
        let (tmp, pm) = tracker();
        let done = mk(&pm, tmp.path(), "done", None);
        let work = mk(&pm, tmp.path(), "wip", None);
        let blocked = mk(&pm, tmp.path(), "blocked", None);
        let dep = mk(&pm, tmp.path(), "dep", None);
        for id in [&done, &work, &blocked] {
            edit(&pm, id, |f| f.tags = vec!["sprint-1".to_string()]);
        }
        edit(&pm, &done, |f| f.status = "done".to_string());
        edit(&pm, &work, |f| f.status = "doing".to_string());
        edit(&pm, &blocked, |f| {
            f.status = "ready".to_string();
            f.blocked_by = vec![dep];
        });
        let out = close(&pm, "sprint-1", Some("sprint-2"), false, false, None, "t").unwrap();
        assert_eq!(out["velocity"]["done"], 1);
        assert_eq!(out["velocity"]["carry_over"], 2);
        assert_eq!(out["blocked"][0], blocked);
        assert!(!tags(&pm, &done).contains(&"sprint-1".to_string()));
        assert!(!tags(&pm, &done).contains(&"sprint-2".to_string()));
        let w = tags(&pm, &work);
        assert!(w.contains(&"sprint-2".to_string()) && !w.contains(&"sprint-1".to_string()));
        let b = tags(&pm, &blocked);
        assert!(b.contains(&"sprint-2".to_string()) && !b.contains(&"sprint-1".to_string()));
    }

    /// --drop leaves survivors in their plain status with no batch tag.
    #[test]
    fn close_drop_returns_survivors_to_plain_status() {
        let (tmp, pm) = tracker();
        let a = mk(&pm, tmp.path(), "a", None);
        edit(&pm, &a, |f| {
            f.status = "ready".to_string();
            f.tags = vec!["sprint-1".to_string()];
        });
        let out = close(&pm, "sprint-1", None, true, false, None, "t").unwrap();
        assert_eq!(out["write"], "committed");
        assert!(tags(&pm, &a).is_empty());
        let (_p, dir) = issue_dir(&pm, &a).unwrap();
        assert_eq!(load_front(&dir).unwrap().0.status, "ready");
    }

    /// Closing into the tag being closed is a no-op in disguise.
    #[test]
    fn close_rejects_next_equal_to_tag() {
        let (tmp, pm) = tracker();
        let a = mk(&pm, tmp.path(), "a", None);
        edit(&pm, &a, |f| f.tags = vec!["sprint-1".to_string()]);
        let e = close(&pm, "sprint-1", Some("sprint-1"), false, false, None, "t").unwrap_err();
        assert!(e.to_string().contains("next"), "{e}");
    }

    /// open fills the batch in priority order and respects the cap —
    /// P0 before P1 before id order.
    #[test]
    fn open_orders_by_priority_and_caps() {
        let (tmp, pm) = tracker();
        let p2 = mk(&pm, tmp.path(), "p2", Some("P2"));
        let p0 = mk(&pm, tmp.path(), "p0", Some("P0"));
        let p1 = mk(&pm, tmp.path(), "p1", Some("P1"));
        for id in [&p2, &p0, &p1] {
            edit(&pm, id, |f| f.status = "ready".to_string());
        }
        let out = open(&pm, "sprint-1", 2, false, None, "t").unwrap();
        assert_eq!(out["added"].as_array().unwrap().len(), 2);
        assert_eq!(out["added"][0], p0);
        assert_eq!(out["added"][1], p1);
        assert_eq!(out["capped_out"][0], p2);
        assert!(tags(&pm, &p0).contains(&"sprint-1".to_string()));
        assert!(tags(&pm, &p2).is_empty());
    }

    /// Blocked ready items are skipped and named — the batch is what
    /// agents can actually draw.
    #[test]
    fn open_skips_blocked_and_names_them() {
        let (tmp, pm) = tracker();
        let free = mk(&pm, tmp.path(), "free", None);
        let stuck = mk(&pm, tmp.path(), "stuck", None);
        let dep = mk(&pm, tmp.path(), "dep", None);
        edit(&pm, &free, |f| f.status = "ready".to_string());
        edit(&pm, &stuck, |f| {
            f.status = "ready".to_string();
            f.blocked_by = vec![dep];
        });
        let out = open(&pm, "sprint-1", 10, false, None, "t").unwrap();
        assert_eq!(out["added"].as_array().unwrap().len(), 1);
        assert_eq!(out["skipped_blocked"][0], stuck);
        assert!(tags(&pm, &stuck).is_empty());
    }

    /// An item already in the batch is not re-added — open is a
    /// refill, not a rewrite.
    #[test]
    fn open_is_a_refill_not_a_rewrite() {
        let (tmp, pm) = tracker();
        let held = mk(&pm, tmp.path(), "held", None);
        let new = mk(&pm, tmp.path(), "new", None);
        edit(&pm, &held, |f| {
            f.status = "ready".to_string();
            f.tags = vec!["sprint-1".to_string()];
        });
        edit(&pm, &new, |f| f.status = "ready".to_string());
        let out = open(&pm, "sprint-1", 10, false, None, "t").unwrap();
        assert_eq!(out["added"].as_array().unwrap(), &vec![json!(new)]);
        assert_eq!(tags(&pm, &held), vec!["sprint-1".to_string()]);
    }

    /// dry_run previews the batch without writing it.
    #[test]
    fn open_dry_run_writes_nothing() {
        let (tmp, pm) = tracker();
        let a = mk(&pm, tmp.path(), "a", None);
        edit(&pm, &a, |f| f.status = "ready".to_string());
        let out = open(&pm, "sprint-1", 10, true, None, "t").unwrap();
        assert_eq!(out["added"].as_array().unwrap().len(), 1);
        assert!(tags(&pm, &a).is_empty());
    }

    /// Nothing ready → refuse with a reason, not an empty commit.
    #[test]
    fn open_refuses_an_empty_batch() {
        let (_tmp, pm) = tracker();
        let e = open(&pm, "sprint-1", 10, false, None, "t").unwrap_err();
        assert!(e.to_string().contains("nothing to tag"), "{e}");
    }
}
