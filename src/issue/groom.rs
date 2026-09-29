//! CAD-812: backlog freshness — an advisory groom pass, the `issue
//! sweep` sibling for dormant `backlog`/`ready` work.
//!
//! The tracker already keeps *status* honest for in-flight work
//! (CAD-755 claim liveness, CAD-757 ready-gate + `blocked-park`
//! sweep). Nothing verified *content* freshness: a `ready` ticket
//! written weeks ago whose `paths:` were since refactored, or whose
//! acceptance a landed PR already satisfied, sat looking valid — the
//! same class of lie CAD-757 removed for active states.
//!
//! [`groom`] re-derives freshness from evidence at pass time, never a
//! stored field: the ticket's `paths:` / `acceptance` text / `blocked_by`
//! targets are re-checked against the project's repos and the issues
//! closed since the ticket's `created`. A candidate is re-read and
//! re-judged under its own PM lock immediately before the write, so a
//! concurrent `issue set` cannot be overwritten by a stale verdict.
//!
//! The pass never changes `status` — it is read-and-flag only. A stale
//! verdict lands as `needs-triage` + a comment + `last_groomed_at`
//! (the `blocked-park` pattern: reversible, never silent); `valid` only
//! stamps `last_groomed_at` so an untouched ticket is not re-reviewed
//! every pass. The operator decides drop or re-scope.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use crate::error::Result;
use crate::issue::model::Front;
use crate::issue::write::{commit_front_with_comment, issue_dir, load_front, save_front};
use crate::issue::{board, line_times::LineTimes, parse, project, time, Pm};

/// Tag a stale verdict leaves on a `backlog`/`ready` item — the durable
/// marker the Overview row and a later pass read. Operators clear it by
/// hand (`issue tag <id> rm needs-triage`) after re-confirming the
/// requirement; `issue groom` then stamps `last_groomed_at` on the next
/// pass and stays quiet until the ticket is touched again.
pub const TRIAGE_TAG: &str = "needs-triage";

/// Freshness evidence is re-derived only for tickets older than this —
/// a ticket filed inside the window is its own truth (its `paths:` and
/// acceptance were written against the current tree).
pub const GROOM_GRACE_SECS: i64 = 14 * 86_400;

/// A groomed ticket's path set is at most this many globs wide before
/// the `git log` lookup cost is refused.
const PATH_MAX: usize = 24;
/// `git log` budget for the close-time clock — same as the overview's.
const STATUS_CLOCK_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// The `issue.md` `## Acceptance` section's checklist items —
/// `parse::acceptance_items` returns them, a malformed/absent section
/// reads as none.
fn acceptance_items(body: &str) -> usize {
    parse::acceptance_items(body).len()
}

/// RFC 3339 UTC `created` → epoch, else `None` (never errors — a
/// missing clock just skips the age gate).
fn created_epoch(front: &Front) -> Option<i64> {
    time::parse_iso(&front.created)
}

/// A ticket is dormant enough to judge once it is past the grace
/// window — inside it the ticket is its own evidence.
fn dormant(front: &Front, now: i64, grace_secs: i64) -> bool {
    created_epoch(front).is_some_and(|c| now - c >= grace_secs)
}

/// The pass is also the daemon's checkup cadence (every ~60s), so a
/// ticket judged inside the window is *not* re-judged — `last_groomed_at`
/// is the cooldown that keeps a clean `backlog`/`ready` ticket from
/// collecting a stamp+comment every minute. A ticket still carrying
/// `needs-triage` is skipped too: the flag already stands and re-flagging
/// every pass would just spam comments.
fn groomed_recently(front: &Front, now: i64, grace_secs: i64) -> bool {
    front
        .last_groomed_at
        .as_deref()
        .and_then(time::parse_iso)
        .is_some_and(|g| now - g < grace_secs)
}

/// Resolved repo roots the ticket's project declares — the evidence
/// source for "did the tree move under this requirement". `path`
/// entries are `~`-expanded; a project without repos yields none.
fn repo_roots(pm_dir: &Path, project_key: &str) -> Vec<PathBuf> {
    project::list(pm_dir)
        .ok()
        .into_iter()
        .flatten()
        .find(|p| p.key == project_key)
        .map(|p| {
            p.repos
                .iter()
                .filter_map(|r| r.path.as_deref())
                .map(project::expand_home)
                .filter(|root| root.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

/// `git log --format= --name-only <paths>` since `created` — the
/// files a ticket planned to touch that a commit moved. Bounded and
/// best-effort: a repo that fails (gone, not a git dir) is skipped with
/// its stderr recorded, never fatal to the pass.
fn touched_paths(root: &Path, since_iso: &str, paths: &[String]) -> Value {
    let mut args: Vec<String> = vec![
        "log".into(),
        "--format=".into(), // name-only lines, no commit noise
        "--name-only".into(),
        format!("--since={since_iso}"),
        "--".into(),
    ];
    args.extend(paths.iter().take(PATH_MAX).cloned());
    let out = crate::reaper::output(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .arg("--no-optional-locks")
            .args(&args),
    );
    match out {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            let touched: Vec<String> = text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .take(200)
                .map(str::to_string)
                .collect();
            json!({"ok": true, "touched": touched})
        }
        Ok(o) => json!({
            "ok": false,
            "error": String::from_utf8_lossy(&o.stderr).trim().chars().take(200).collect::<String>(),
        }),
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    }
}

/// The freshness verdict the pass commits to the ticket — one of
/// `valid` | `stale` | `superseded`, plus the evidence behind it.
#[derive(Debug)]
struct Verdict {
    kind: &'static str,
    /// The concrete evidence lines a reviewer reads — touched paths
    /// and the landed issues that now cover the requirement.
    reasons: Vec<String>,
    /// Paths a `stale` verdict names — folded into the comment.
    touched: Vec<String>,
}

/// Judge one dormant leaf against the tree and the tracker.
///
/// `stale` — the ticket names `paths:` that moved since `created`, or
///   its `blocked_by` is satisfied but the status never advanced.
/// `superseded` — an issue `done`/`dropped` overlapping the same
///   `paths:` whose status line last moved *after* this ticket was
///   filed (the close lands inside the freshness window — the
///   requirement reads as covered). Without tracker history the close
///   time is unknown, so the sibling is not counted: a false
///   `superseded` is worse than a missed one.
/// `valid` — nothing moved that the ticket cares about.
fn judge(
    pm: &Pm,
    front: &Front,
    views: &[board::View],
    repos: &[PathBuf],
    times: Option<&LineTimes>,
    now: i64,
) -> Verdict {
    let mut reasons = Vec::new();
    let mut touched_all: Vec<String> = Vec::new();

    // Path evidence: the files a ticket planned to touch moved under it.
    let since_iso = created_epoch(front)
        .map(time::iso)
        .unwrap_or_else(|| time::iso(now));
    let mut moved_paths: Vec<String> = Vec::new();
    if !front.paths.is_empty() {
        for root in repos {
            let probe = touched_paths(root, &since_iso, &front.paths);
            if probe["ok"] == true {
                if let Some(t) = probe["touched"].as_array() {
                    for f in t.iter().filter_map(|f| f.as_str()) {
                        if !moved_paths.iter().any(|m| m == f) {
                            moved_paths.push(f.to_string());
                        }
                    }
                }
            }
        }
    }
    if !moved_paths.is_empty() {
        touched_all = moved_paths.clone();
        reasons.push(format!(
            "{} path(s) moved since filed: {}",
            moved_paths.len(),
            moved_paths
                .iter()
                .take(6)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // Superseded: a sibling ticket covering the same paths closed
    // inside this ticket's freshness window — the requirement reads as
    // landed. `status_at` is the git-history clock for the close;
    // absent history the sibling cannot be dated, so it is not counted.
    if !front.paths.is_empty() {
        if let (Some(times), Some(created)) = (times, created_epoch(front)) {
            let covering: Vec<String> = views
                .iter()
                .filter(|v| {
                    v.issue.front.id != front.id
                        && matches!(v.status.as_str(), "done" | "dropped")
                        && !v.issue.front.paths.is_empty()
                        && v.issue
                            .front
                            .paths
                            .iter()
                            .any(|p| front.paths.iter().any(|q| q == p))
                        && times
                            .status_at(&v.issue.project, &v.issue.front.id)
                            .is_some_and(|closed| closed >= created)
                })
                .map(|v| v.issue.front.id.clone())
                .collect();
            if !covering.is_empty() {
                return Verdict {
                    kind: "superseded",
                    reasons: vec![format!(
                        "covered by {} — closed inside the freshness window",
                        covering.join(", ")
                    )],
                    touched: touched_all,
                };
            }
        }
    }

    // Duplicate: `duplicate_of` names a still-open issue — the
    // requirement lives elsewhere, so this one should be dropped or
    // re-scoped, not sit in ready.
    if let Some(other) = front.duplicate_of.as_deref() {
        if board::find_issue(&pm.dir, other)
            .map(|i| !matches!(i.front.status.as_str(), "done" | "dropped"))
            .unwrap_or(false)
        {
            return Verdict {
                kind: "dup",
                reasons: vec![format!("duplicate_of {other} — that issue is still open")],
                touched: touched_all,
            };
        }
    }

    // A blocked_by that is now satisfied but the status never moved —
    // the requirement stood waiting on something already done.
    let unblocked = !front.blocked_by.is_empty()
        && front.blocked_by.iter().all(|b| {
            board::find_issue(&pm.dir, b)
                .map(|i| matches!(i.front.status.as_str(), "done" | "dropped"))
                .unwrap_or(false)
        });
    if unblocked {
        reasons.push(format!(
            "blockers {} all closed — the ticket never advanced",
            front.blocked_by.join(", ")
        ));
    }

    // Acceptance drift: a path-scoped ticket with no checklist is a
    // requirement nobody can verify — flag it for the operator.
    if !front.paths.is_empty() && acceptance_items(&front_body(pm, front)) == 0 {
        reasons.push("no acceptance recorded for a path-scoped ticket".to_string());
    }

    if reasons.is_empty() {
        Verdict {
            kind: "valid",
            reasons: vec![],
            touched: vec![],
        }
    } else {
        Verdict {
            kind: "stale",
            reasons,
            touched: touched_all,
        }
    }
}

/// `issue.md` body for the acceptance probe — best-effort, an
/// unloadable file reads as no acceptance.
fn front_body(pm: &Pm, front: &Front) -> String {
    issue_dir(pm, &front.id)
        .and_then(|(_p, dir)| load_front(&dir).map(|(_f, b)| b))
        .unwrap_or_default()
}

/// Write the one comment-bearing commit for a verdict. The caller
/// holds the PM lock; the front was just reloaded under it, so the
/// verdict and the write share one snapshot.
fn write_verdict(
    pm: &Pm,
    front: &Front,
    verdict: &Verdict,
    now: i64,
    actor: &str,
    dry_run: bool,
) -> Result<()> {
    if dry_run {
        return Ok(());
    }
    let (_p, dir) = issue_dir(pm, &front.id)?;
    let (mut f, body) = load_front(&dir)?;
    let prev = f.clone();
    f.last_groomed_at = Some(time::iso(now));
    let (kind, text, subject) = match verdict.kind {
        "stale" | "superseded" | "dup" => {
            if !f.tags.iter().any(|t| t == TRIAGE_TAG) {
                f.tags.push(TRIAGE_TAG.to_string());
            }
            let evidence = verdict.reasons.join("; ");
            let kind = verdict.kind;
            (
                "groom",
                format!(
                    "Groom pass: {kind}.\n\nEvidence: {evidence}.\n\
                     Re-confirm the requirement, then `cadence issue tag {} rm {TRIAGE_TAG}`; \
                     or `cadence issue set {} status=doing` to pick it up.",
                    f.id, f.id
                ),
                format!("groom {kind} — {evidence}"),
            )
        }
        _ => {
            // `valid` stamps `last_groomed_at` only — a clean ticket
            // collects no comment, just the cooldown marker. The commit
            // still carries the `Issue:` trailer; a failed write restores
            // the prior front.
            if let Err(e) = save_front(&dir, &f, &body).and_then(|_| {
                crate::issue::write::commit(
                    pm,
                    std::slice::from_ref(&dir.join("issue.md")),
                    &format!("{}: groom valid", f.id),
                    &[f.id.as_str()],
                    actor,
                )
                .map(|_| ())
            }) {
                let _ = save_front(&dir, &prev, &body);
                return Err(e);
            }
            return Ok(());
        }
    };
    commit_front_with_comment(
        pm, &dir, &prev, &f, &body, "groom", kind, &text, &subject, actor,
    )
    .map(|_| ())
}

/// `cadence issue groom [--project P] [--dry-run] [--grace SECS]` —
/// the advisory freshness pass over open `backlog`/`ready` leaves.
///
/// Re-derives each dormant ticket's freshness from evidence (path
/// movement, closed siblings on the same paths, satisfied blockers)
/// at pass time; a ticket re-judged `stale`/`superseded` gets
/// `needs-triage` + comment + `last_groomed_at`, `valid` only stamps
/// `last_groomed_at`. `intake` items are skipped — already under the
/// triage SLA. `dry_run` reports without writing.
pub fn groom(
    pm: &Pm,
    project: Option<&str>,
    grace_secs: i64,
    dry_run: bool,
    actor: &str,
) -> Result<Value> {
    groom_with_hooks(pm, project, grace_secs, dry_run, actor, || {}, || {})
}

/// [`groom`] with a test seam: `after_advisory_snapshot` fires once
/// the candidate list is read (a competing writer can move an item
/// before its locked re-judge); `after_locked_snapshot` fires per
/// candidate after its under-lock reload. Both no-op in production.
fn groom_with_hooks(
    pm: &Pm,
    project: Option<&str>,
    grace_secs: i64,
    dry_run: bool,
    actor: &str,
    after_advisory_snapshot: impl FnOnce(),
    mut after_locked_snapshot: impl FnMut(),
) -> Result<Value> {
    let now = time::now_epoch();
    // Advisory enumeration: every candidate is reloaded and re-judged
    // under its own PM lock immediately before the write.
    let issues = board::load_all(&pm.dir, project)?;
    let views = board::views(&pm.config.notes_dir(), issues);
    let candidates: Vec<String> = views
        .iter()
        .filter(|v| {
            matches!(v.status.as_str(), "backlog" | "ready")
                && !v.container
                && !v.issue.front.tags.iter().any(|t| t == "intake")
                && v.issue.front.kind.is_none() // intake kinds carry `kind`
        })
        .map(|v| v.issue.front.id.clone())
        .collect();
    after_advisory_snapshot();

    let mut flagged: Vec<Value> = Vec::new();
    let mut stamped: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut verdicts: Vec<Value> = Vec::new(); // per-candidate, dry_run
    for id in candidates {
        let _lock = pm.lock()?;
        // Re-derive the whole evidence set under this candidate's lock —
        // the eligibility fields AND the sibling/container view the
        // verdict reads (blocked.rs's recheck, same pattern). A sibling
        // closing or a child appearing between snapshot and lock must not
        // commit an outdated verdict.
        let current = board::load_all(&pm.dir, project)?;
        let live = board::views(&pm.config.notes_dir(), current);
        let Some(v) = live.iter().find(|v| v.issue.front.id == id) else {
            continue; // dropped or renamed between snapshot and lock
        };
        after_locked_snapshot();
        // The close-time clock rides the same locked snapshot as the
        // sibling set: a `done` committed during the race must appear in
        // `live` AND have a `status_at` time, or `superseded` would be
        // judged against a close the clock cannot yet date.
        let times = LineTimes::load(&pm.dir, STATUS_CLOCK_BUDGET).ok();
        let front = &v.issue.front;
        // Recheck under the lock: a competing writer can move it off
        // backlog/ready, mark it intake, or make it a container after the
        // advisory snapshot.
        if !matches!(front.status.as_str(), "backlog" | "ready")
            || v.container
            || front.tags.iter().any(|t| t == "intake")
            || front.kind.is_some()
        {
            skipped.push(front.id.clone());
            continue;
        }
        if !dormant(front, now, grace_secs)
            || groomed_recently(front, now, grace_secs)
            || front.tags.iter().any(|t| t == TRIAGE_TAG)
        {
            skipped.push(front.id.clone());
            continue;
        }
        let repos = repo_roots(&pm.dir, &v.issue.project);
        let verdict = judge(pm, front, &live, &repos, times.as_ref(), now);
        verdicts.push(json!({
            "id": front.id,
            "verdict": verdict.kind,
            "reasons": verdict.reasons,
            "paths_touched": verdict.touched,
        }));
        if verdict.kind == "valid" {
            // `valid`: stamp `last_groomed_at` silently — the cooldown
            // marker, no comment (a clean ticket collects no noise).
            stamped.push(front.id.clone());
            write_verdict(pm, front, &verdict, now, actor, dry_run)?;
            continue;
        }
        write_verdict(pm, front, &verdict, now, actor, dry_run)?;
        flagged.push(json!({
            "id": front.id,
            "verdict": verdict.kind,
            "reasons": verdict.reasons,
            "paths_touched": verdict.touched,
        }));
    }
    Ok(json!({
        "dry_run": dry_run,
        "grace_secs": grace_secs,
        "flagged": flagged,
        "stamped": stamped,
        "skipped": skipped,
        "verdicts": verdicts,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::write::{new_issue, project_add, save_front};

    fn tracker() -> (tempfile::TempDir, Pm) {
        let dir = tempfile::tempdir().unwrap();
        let pm = Pm::init(&dir.path().join("pm")).unwrap();
        project_add(&pm, "cadence", "CAD", &[], &[], &[], None).unwrap();
        (dir, pm)
    }

    /// A tracker whose `cadence` project declares `repo` as a repo —
    /// the path-evidence cases need it.
    fn tracker_with_repo() -> (tempfile::TempDir, Pm, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        let pm = Pm::init(&dir.path().join("pm")).unwrap();
        project_add(
            &pm,
            "cadence",
            "CAD",
            &[repo.to_string_lossy().to_string()],
            &[],
            &[],
            None,
        )
        .unwrap();
        (dir, pm, repo)
    }

    fn mk(pm: &Pm, dir: &Path, title: &str) -> String {
        new_issue(
            pm,
            dir,
            Some("cadence"),
            title,
            None,
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

    /// Fixture edit: write `issue.md` front without committing — the
    /// groom pass's own commit lands on top.
    fn edit(pm: &Pm, id: &str, f: impl FnOnce(&mut Front)) {
        let (_p, dir) = issue_dir(pm, id).unwrap();
        let (mut front, body) = load_front(&dir).unwrap();
        f(&mut front);
        save_front(&dir, &front, &body).unwrap();
    }

    fn dormant(pm: &Pm, id: &str, paths: &[&str]) {
        let p: Vec<String> = paths.iter().map(|s| s.to_string()).collect();
        edit(pm, id, |f| {
            f.paths = p;
            f.created = time::iso(time::now_epoch() - (GROOM_GRACE_SECS + 86_400));
        });
    }

    fn git(root: &Path, args: &[&str]) {
        crate::reaper::output(Command::new("git").arg("-C").arg(root).args(args)).unwrap();
    }

    fn git_commit(root: &Path, msg: &str) {
        git(
            root,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-m",
                msg,
            ],
        );
    }

    /// A fresh ticket inside the grace window is skipped, not judged.
    #[test]
    fn fresh_ticket_is_skipped() {
        let (tmp, pm) = tracker();
        let id = mk(&pm, tmp.path(), "t");
        edit(&pm, &id, |f| f.paths = vec!["src.rs".to_string()]);
        let out = groom(&pm, None, GROOM_GRACE_SECS, true, "t").unwrap();
        assert!(out["flagged"].as_array().unwrap().is_empty(), "{out}");
        assert!(
            out["skipped"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s.as_str() == Some(id.as_str())),
            "{out}"
        );
    }

    /// A dormant ticket whose `paths:` file moved in a repo the project
    /// declares — real `git log` evidence, the stale verdict.
    #[test]
    fn moved_path_is_flagged_stale() {
        let (_tmp, pm, repo) = tracker_with_repo();
        let id = mk(&pm, _tmp.path(), "t");
        dormant(&pm, &id, &["src.rs"]);
        // File lands after the ticket's `created` — the tree moved under it.
        std::fs::write(repo.join("src.rs"), "fn a() {}").unwrap();
        git(&repo, &["add", "src.rs"]);
        git_commit(&repo, "add src");
        let out = groom(&pm, None, GROOM_GRACE_SECS, true, "t").unwrap();
        let flagged = out["flagged"].as_array().unwrap();
        assert!(
            flagged
                .iter()
                .any(|r| r["id"].as_str() == Some(id.as_str())),
            "{out}"
        );
    }

    /// A dormant path-scoped ticket with no `paths:` movement but no
    /// acceptance checklist is still flagged — unverifiable requirement.
    #[test]
    fn path_scoped_without_acceptance_is_flagged() {
        let (tmp, pm) = tracker();
        let id = mk(&pm, tmp.path(), "t");
        dormant(&pm, &id, &["src.rs"]);
        let out = groom(&pm, None, GROOM_GRACE_SECS, true, "t").unwrap();
        assert!(
            out["flagged"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["id"].as_str() == Some(id.as_str())),
            "{out}"
        );
    }

    /// The verdict is advisory: `needs-triage` + `last_groomed_at`,
    /// never a status move — `backlog` stays `backlog`.
    #[test]
    fn flag_is_advisory_status_unchanged() {
        let (tmp, pm) = tracker();
        let id = mk(&pm, tmp.path(), "t");
        dormant(&pm, &id, &["src.rs"]);
        let out = groom(&pm, None, GROOM_GRACE_SECS, false, "t").unwrap();
        assert!(
            out["flagged"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["id"].as_str() == Some(id.as_str())),
            "{out}"
        );
        let (_p, dir) = issue_dir(&pm, &id).unwrap();
        let (front, _) = load_front(&dir).unwrap();
        assert_eq!(front.status, "backlog", "groom never moves status");
        assert!(front.tags.iter().any(|t| t == TRIAGE_TAG));
        assert!(front.last_groomed_at.is_some());
        // One comment-bearing commit records the verdict.
        assert_eq!(std::fs::read_dir(dir.join("comments")).unwrap().count(), 1);
    }

    /// A `valid` verdict stamps `last_groomed_at` only — the ticket is
    /// not re-reviewed until touched.
    #[test]
    fn valid_stamps_last_groomed_only() {
        let (tmp, pm) = tracker();
        let id = mk(&pm, tmp.path(), "t");
        edit(&pm, &id, |f| {
            f.created = time::iso(time::now_epoch() - (GROOM_GRACE_SECS + 86_400));
        });
        let out = groom(&pm, None, GROOM_GRACE_SECS, false, "t").unwrap();
        assert!(out["flagged"].as_array().unwrap().is_empty(), "{out}");
        assert!(
            out["stamped"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s.as_str() == Some(id.as_str())),
            "{out}"
        );
        let (_p, dir) = issue_dir(&pm, &id).unwrap();
        let (front, _) = load_front(&dir).unwrap();
        assert!(front.last_groomed_at.is_some());
        assert!(!front.tags.iter().any(|t| t == TRIAGE_TAG));
    }

    /// A ticket judged inside the window is not re-judged — the
    /// checkup runs every ~60s, and without the cooldown a clean
    /// `backlog` ticket would collect a stamp+comment every pass.
    #[test]
    fn groomed_inside_window_is_not_rejudged() {
        let (tmp, pm) = tracker();
        let id = mk(&pm, tmp.path(), "t");
        dormant(&pm, &id, &[]);
        // First pass: valid, stamped.
        groom(&pm, None, GROOM_GRACE_SECS, false, "t").unwrap();
        let (_p, dir) = issue_dir(&pm, &id).unwrap();
        let (front, _) = load_front(&dir).unwrap();
        let first = front.last_groomed_at.clone();
        let comments = std::fs::read_dir(dir.join("comments")).unwrap().count();
        // Second pass within the window: skipped, no new stamp/comment.
        let out = groom(&pm, None, GROOM_GRACE_SECS, false, "t").unwrap();
        assert!(
            out["skipped"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s.as_str() == Some(id.as_str())),
            "{out}"
        );
        let (_p, dir) = issue_dir(&pm, &id).unwrap();
        let (front, _) = load_front(&dir).unwrap();
        assert_eq!(front.last_groomed_at, first, "cooldown must not re-stamp");
        assert_eq!(
            std::fs::read_dir(dir.join("comments")).unwrap().count(),
            comments,
            "cooldown must not add a comment"
        );
    }

    /// `--dry-run` reports every judged candidate's verdict —
    /// `valid`/`stale`/`superseded`/`dup` — not just the flagged ids.
    #[test]
    fn dry_run_lists_verdicts() {
        let (tmp, pm) = tracker();
        let stale = mk(&pm, tmp.path(), "stale");
        let clean = mk(&pm, tmp.path(), "clean");
        dormant(&pm, &stale, &["src.rs"]); // path-scoped, no acceptance
        edit(&pm, &clean, |f| {
            f.created = time::iso(time::now_epoch() - (GROOM_GRACE_SECS + 86_400));
        });
        let out = groom(&pm, None, GROOM_GRACE_SECS, true, "t").unwrap();
        let verdicts = out["verdicts"].as_array().unwrap();
        assert!(
            verdicts
                .iter()
                .any(|r| r["id"].as_str() == Some(stale.as_str())
                    && r["verdict"].as_str() == Some("stale")),
            "{out}"
        );
        assert!(
            verdicts
                .iter()
                .any(|r| r["id"].as_str() == Some(clean.as_str())
                    && r["verdict"].as_str() == Some("valid")),
            "{out}"
        );
    }

    /// `duplicate_of` naming an open issue is a `dup` verdict — the
    /// requirement lives elsewhere.
    #[test]
    fn duplicate_of_open_is_dup() {
        let (tmp, pm) = tracker();
        let a = mk(&pm, tmp.path(), "a");
        let b = mk(&pm, tmp.path(), "b");
        dormant(&pm, &a, &[]);
        edit(&pm, &a, |f| f.duplicate_of = Some(b.clone()));
        let out = groom(&pm, None, GROOM_GRACE_SECS, true, "t").unwrap();
        assert!(
            out["verdicts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["id"].as_str() == Some(a.as_str())
                    && r["verdict"].as_str() == Some("dup")),
            "{out}"
        );
    }

    /// A `dup` flag is still advisory — tag + comment, status untouched.
    #[test]
    fn dup_flag_is_advisory() {
        let (tmp, pm) = tracker();
        let a = mk(&pm, tmp.path(), "a");
        let b = mk(&pm, tmp.path(), "b");
        dormant(&pm, &a, &[]);
        edit(&pm, &a, |f| f.duplicate_of = Some(b));
        groom(&pm, None, GROOM_GRACE_SECS, false, "t").unwrap();
        let (_p, dir) = issue_dir(&pm, &a).unwrap();
        let (front, _) = load_front(&dir).unwrap();
        assert_eq!(front.status, "backlog");
        assert!(front.tags.iter().any(|t| t == TRIAGE_TAG));
    }

    /// The sibling evidence AND its close-time clock are re-derived
    /// under the lock: a sibling committed `done` between the advisory
    /// snapshot and the locked re-judge is seen *and* dated, so `a` is
    /// judged `superseded` — not missed by a stale view or a stale clock.
    #[test]
    fn sibling_close_racing_snapshot_is_recognised_under_lock() {
        let (tmp, pm) = tracker();
        let a = mk(&pm, tmp.path(), "a");
        let b = mk(&pm, tmp.path(), "b");
        dormant(&pm, &a, &["shared.rs"]);
        edit(&pm, &b, |f| f.paths = vec!["shared.rs".to_string()]);
        // Commit the tracker so `LineTimes` has history to read, then
        // `b` is still open at the advisory snapshot. The writer closes
        // `b` (a real commit, so status_at can date it) before `a`'s
        // locked re-judge.
        let (_p, pmdir) = issue_dir(&pm, &a).unwrap();
        let _ =
            crate::issue::write::commit(&pm, &[pmdir.join("issue.md")], "seed", &[a.as_str()], "t");
        let out = std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::channel();
            let writer_pm = &pm;
            let writer_id = b.clone();
            let writer = scope.spawn(move || {
                rx.recv().unwrap();
                let _w = writer_pm.lock().unwrap();
                edit(writer_pm, &writer_id, |f| f.status = "done".to_string());
                // Commit the close so LineTimes records a close time.
                let (_p, bdir) = issue_dir(writer_pm, &writer_id).unwrap();
                let (f, body) = load_front(&bdir).unwrap();
                let prev = f.clone();
                crate::issue::write::commit(
                    writer_pm,
                    &[bdir.join("issue.md")],
                    "close b",
                    &[writer_id.as_str()],
                    "t",
                )
                .unwrap();
                let _ = (prev, body);
            });
            groom_with_hooks(
                &pm,
                None,
                GROOM_GRACE_SECS,
                true, // dry-run — observe the verdict, write nothing
                "t",
                || {
                    tx.send(()).unwrap();
                    writer.join().unwrap();
                },
                || {},
            )
            .unwrap()
        });
        let verdict = out["verdicts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"].as_str() == Some(a.as_str()))
            .cloned()
            .unwrap_or(json!(null));
        assert_eq!(
            verdict["verdict"].as_str(),
            Some("superseded"),
            "sibling closed under lock must drive superseded: {out}"
        );
    }

    /// `intake` items are skipped — already under the triage SLA.
    #[test]
    fn intake_items_are_skipped() {
        let (tmp, pm) = tracker();
        let id = mk(&pm, tmp.path(), "t");
        dormant(&pm, &id, &["src.rs"]);
        edit(&pm, &id, |f| f.tags = vec!["intake".to_string()]);
        let out = groom(&pm, None, GROOM_GRACE_SECS, false, "t").unwrap();
        assert!(out["flagged"].as_array().unwrap().is_empty(), "{out}");
        let (_p, dir) = issue_dir(&pm, &id).unwrap();
        let (front, _) = load_front(&dir).unwrap();
        assert!(front.last_groomed_at.is_none(), "intake never groomed");
    }

    /// `--dry-run` writes nothing: no tag, no stamp, no comment.
    #[test]
    fn dry_run_writes_nothing() {
        let (tmp, pm) = tracker();
        let id = mk(&pm, tmp.path(), "t");
        dormant(&pm, &id, &["src.rs"]);
        let out = groom(&pm, None, GROOM_GRACE_SECS, true, "t").unwrap();
        assert!(
            out["flagged"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["id"].as_str() == Some(id.as_str())),
            "{out}"
        );
        let (_p, dir) = issue_dir(&pm, &id).unwrap();
        let (front, _) = load_front(&dir).unwrap();
        assert!(!front.tags.iter().any(|t| t == TRIAGE_TAG));
        assert!(front.last_groomed_at.is_none());
        let comments = dir.join("comments");
        let n = if comments.exists() {
            std::fs::read_dir(&comments).unwrap().count()
        } else {
            0
        };
        assert_eq!(n, 0, "dry-run writes no comment");
    }

    /// AGENTS.md adversarial: a competing `issue set` that moves the
    /// ticket to `doing` after the advisory snapshot must win — the
    /// under-lock recheck skips it instead of overwriting a stale flag.
    #[test]
    fn concurrent_set_off_backlog_is_not_overwritten() {
        let (tmp, pm) = tracker();
        let id = mk(&pm, tmp.path(), "t");
        dormant(&pm, &id, &["src.rs"]);
        let out = std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::channel();
            let writer_pm = &pm;
            let writer_id = id.clone();
            // After the advisory snapshot, a human moves the ticket to
            // doing under its own PM lock before the groom's re-judge.
            let writer = scope.spawn(move || {
                rx.recv().unwrap();
                let _w = writer_pm.lock().unwrap();
                edit(writer_pm, &writer_id, |f| f.status = "doing".to_string());
            });
            groom_with_hooks(
                &pm,
                None,
                GROOM_GRACE_SECS,
                false,
                "t",
                || {
                    tx.send(()).unwrap();
                    writer.join().unwrap();
                },
                || {},
            )
            .unwrap()
        });
        // The ticket left backlog before the locked re-judge — it is
        // skipped, never flagged on a status it no longer holds.
        let (_p, dir) = issue_dir(&pm, &id).unwrap();
        let (front, _) = load_front(&dir).unwrap();
        assert_eq!(front.status, "doing", "{out}");
        assert!(
            !front.tags.iter().any(|t| t == TRIAGE_TAG),
            "moved ticket must not be flagged: {out}"
        );
    }
}
