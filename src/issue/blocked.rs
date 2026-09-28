//! CAD-757: blocked-work hygiene — the status fields stop lying about
//! blocked work, and a parked lane learns when its blocker lands.
//!
//! Three mechanisms on the same seam `issue link`/`issue set` already
//! share:
//!
//! 1. [`check_ready`]: `set status=ready` refuses while a `blocked_by`
//!    target is still open — the dispatch queue skips blocked items
//!    anyway ([`crate::daemon::next_action`] re-checks at dispatch), so
//!    a blocked `ready` is a lie, not a pick.
//! 2. [`notify_new_blocker`]: `issue link <id> blocked_by <target>` on
//!    an active issue sends a best-effort needs-you to the claim holder
//!    and the owner, naming the new blocker.
//! 3. [`sweep`]: a `doing`/`review` leaf whose blockers are open *and*
//!    whose claim is older than [`PARK_GRACE_SECS`] parks to `backlog`
//!    tagged [`PARK_TAG`] — the transition is a comment-bearing commit,
//!    and reversible by `issue set status=doing`. A later sweep that
//!    finds a tagged item fully unblocked untags it and notifies the
//!    last claimer it can resume.
//!
//! All notices ride `agent_send` best-effort: a dead alias never blocks
//! the write, and the send outcome lands in the commit comment and the
//! response JSON — never silent either way.

use std::path::Path;

use serde_json::{json, Value};

use crate::client;
use crate::error::{Error, Result};
use crate::issue::model::Front;
use crate::issue::write::{commit_front_with_comment, issue_dir, load_front};
use crate::issue::{board, time, Pm};

/// Tag the sweep leaves on an auto-parked issue — the durable marker a
/// later sweep reads to know *this* backlog item wants a resume notice
/// when its blockers close. Operators clear it by hand
/// (`issue tag <id> rm blocked-park`) to silence the notice.
pub const PARK_TAG: &str = "blocked-park";

/// A blocked `doing`/`review` may hold this long on an unchanged claim
/// before it parks itself — the claim age is the renewal clock; a lane
/// that keeps re-claiming keeps its seat.
pub const PARK_GRACE_SECS: i64 = 24 * 3600;

/// Statuses a blocker must reach to count as closed — the same pair
/// readiness and the dispatch path use.
const CLOSED: &[&str] = &["done", "dropped"];

/// `blocked_by` targets still open — the shared lookup behind the
/// `ready` gate and the notice text.
fn open_blockers(pm_dir: &Path, front: &Front) -> Vec<String> {
    front
        .blocked_by
        .iter()
        .filter(|dep| {
            board::find_issue(pm_dir, dep)
                .map(|b| !CLOSED.contains(&b.front.status.as_str()))
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}

/// `issue set <id> status=ready` refuses while a blocker is still open
/// (CAD-757): the dispatch queue would skip the ticket anyway, so the
/// honest statuses are `backlog`/`doing`+warning — not a pick nobody
/// can take.
pub fn check_ready(pm_dir: &Path, front: &Front) -> Result<()> {
    let open = open_blockers(pm_dir, front);
    if open.is_empty() {
        return Ok(());
    }
    Err(Error::rejected(format!(
        "{}: status=ready but blocked_by {} still open — \
         the queue skips blocked tickets anyway; land {} or \
         `cadence issue unlink {} blocked_by <id>` first",
        front.id,
        open.join(", "),
        if open.len() == 1 { "it" } else { "them" },
        front.id,
    )))
}

/// One control-free needs-you line to an agent alias — best-effort, the
/// outcome is returned as data. `key` is the `agent_send` idempotency
/// key: deterministic per edge so a retried sweep never double-sends.
fn send_to(state_dir: &Path, to: &str, text: &str, key: &str) -> Value {
    match client::rpc(
        state_dir,
        "agent_send",
        json!({"alias": to, "text": text, "message": key}),
    ) {
        Ok(r) => json!({"to": to, "sent": true, "duplicate": r["duplicate"].as_bool()}),
        Err(e) => json!({"to": to, "sent": false, "error": e.to_string()}),
    }
}

/// The recipients a notice speaks to: the claim holder first, then the
/// owner when they differ — both are aliases `agent_send` can try.
fn holders(front: &Front) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(by) = front.claim.as_ref().map(|c| c.by.as_str()) {
        if !by.is_empty() {
            out.push(by.to_string());
        }
    }
    if let Some(owner) = front.owner.as_deref() {
        if !owner.is_empty() && !out.iter().any(|o| o == owner) {
            out.push(owner.to_string());
        }
    }
    out
}

/// `issue link <id> blocked_by <target>` calls this after the commit
/// lands: an issue in `doing`/`review` just learned it is waiting —
/// tell whoever holds it, by name, what it waits on. Best-effort: the
/// link itself already committed, so a dead alias or a board without a
/// daemon changes nothing about it.
pub fn notify_new_blocker(front: &Front, target: &str, state_dir: Option<&Path>) -> Value {
    if !matches!(front.status.as_str(), "doing" | "review") {
        return json!({"notified": [], "skipped": format!("{} is {}", front.id, front.status)});
    }
    let to = holders(front);
    if to.is_empty() {
        return json!({"notified": [], "skipped": "no claim or owner to notify"});
    }
    let Some(dir) = state_dir else {
        return json!({"notified": [], "skipped": "no daemon state dir"});
    };
    let line = format!(
        "{} is now blocked by {} — you hold it as {}",
        front.id, target, front.status
    );
    let sent: Vec<Value> = to
        .iter()
        .map(|alias| send_to(dir, alias, &line, &format!("blocked-{}-{target}", front.id)))
        .collect();
    json!({"notified": sent})
}

/// The claim's age in seconds; `None` means nobody holds it — a seat
/// nobody took is always stale enough to park.
fn claim_age_secs(front: &Front, now: i64) -> Option<i64> {
    front
        .claim
        .as_ref()
        .and_then(|c| time::parse_iso(&c.at))
        .map(|at| now - at)
}

/// One parked/unblocked write: mutate the front, then
/// [`commit_front_with_comment`] makes the transition a comment-bearing
/// commit (and rolls both back when the commit refuses).
#[allow(clippy::too_many_arguments)]
fn write_transition(
    pm: &Pm,
    id: &str,
    dry_run: bool,
    edit: impl FnOnce(&mut Front),
    kind: &str,
    text: &str,
    subject: &str,
    actor: &str,
) -> Result<()> {
    if dry_run {
        return Ok(());
    }
    let _lock = pm.lock()?;
    let (_project, dir) = issue_dir(pm, id)?;
    let (mut front, body) = load_front(&dir)?;
    let prev = front.clone();
    edit(&mut front);
    commit_front_with_comment(
        pm, &dir, &prev, &front, &body, "sweep", kind, text, subject, actor,
    )?;
    Ok(())
}

/// The blocked-work sweep, on the daemon checkup cadence and on
/// `issue sweep`: park stale blocked `doing`/`review` leaves under
/// [`PARK_TAG`], and for a tagged item whose blockers all closed,
/// notify the last claimer and drop the tag. `dry_run` reports without
/// writing.
///
/// The claim stays on the parked issue — it is the resume address for
/// the unblocked notice, and `backlog` claims hold nothing under the
/// claim rules ([`crate::issue::claim::PROTECTED`]).
pub fn sweep(
    pm: &Pm,
    grace_secs: i64,
    dry_run: bool,
    state_dir: Option<&Path>,
    actor: &str,
) -> Result<Value> {
    let issues = board::load_all(&pm.dir, None)?;
    let jobs = state_dir.map(board::fetch_job_outcomes).unwrap_or_default();
    let views = board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs);
    let now = time::now_epoch();
    let mut parked: Vec<Value> = Vec::new();
    let mut unblocked: Vec<Value> = Vec::new();
    for v in &views {
        let front = &v.issue.front;
        if front.tags.iter().any(|t| t == PARK_TAG) && !v.blocked {
            // Last blocker closed — tell the last claimer it can resume,
            // then drop the marker. The untag commit lands even when the
            // send fails: the comment records the outcome, and a dead
            // alias must not retry-storm every pass. The send key names
            // the blocker set it waited on, so a re-park on different
            // blockers is a new notice while a retry dedupes.
            let to = holders(front);
            let key = format!("unblocked-{}-{}", front.id, front.blocked_by.join("-"));
            let line = format!(
                "{} is unblocked — its blockers closed; `cadence issue claim {}` to resume",
                front.id, front.id
            );
            let sent: Vec<Value> = to
                .iter()
                .map(|alias| {
                    if dry_run {
                        json!({"to": alias, "sent": false, "dry_run": true})
                    } else if let Some(dir) = state_dir {
                        send_to(dir, alias, &line, &key)
                    } else {
                        json!({"to": alias, "sent": false, "error": "no daemon state dir"})
                    }
                })
                .collect();
            let note = if sent.is_empty() {
                "unblocked — no claim holder to notify".to_string()
            } else {
                let parts: Vec<String> = sent
                    .iter()
                    .map(|s| {
                        let to = s["to"].as_str().unwrap_or_default();
                        if s["sent"] == true {
                            format!("notified {to}")
                        } else {
                            format!("could not notify {to}")
                        }
                    })
                    .collect();
                format!("unblocked — {}", parts.join(", "))
            };
            write_transition(
                pm,
                &front.id,
                dry_run,
                |f| f.tags.retain(|t| t != PARK_TAG),
                "park",
                &note,
                &format!("{} unblocked — {} cleared", front.id, PARK_TAG),
                actor,
            )?;
            unblocked.push(json!({"id": front.id, "sent": sent}));
            continue;
        }
        if !(v.blocked
            && matches!(v.status.as_str(), "doing" | "review")
            && !v.container
            && !front.tags.iter().any(|t| t == PARK_TAG))
        {
            continue;
        }
        let age = claim_age_secs(front, now);
        if age.is_some_and(|a| a < grace_secs) {
            continue;
        }
        let open = open_blockers(&pm.dir, front);
        write_transition(
            pm,
            &front.id,
            dry_run,
            |f| {
                f.status = "backlog".to_string();
                f.tags.push(PARK_TAG.to_string());
            },
            "park",
            &format!(
                "Parked by the blocked-work sweep: still waits on {}. \
                 The claim stays as the resume address — \
                 `cadence issue set {} status=doing` resumes by hand.",
                open.join(", "),
                front.id,
            ),
            &format!("parked — blocked by {} (auto)", open.join(", ")),
            actor,
        )?;
        parked.push(json!({
            "id": front.id,
            "from": v.status,
            "claim_age_secs": age,
            "claim": front.claim.as_ref().map(|c| c.by.as_str()),
            "open_blockers": open,
        }));
    }
    Ok(json!({
        "dry_run": dry_run,
        "grace_secs": grace_secs,
        "parked": parked,
        "unblocked": unblocked,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::model::Claim;
    use crate::issue::write::{load_front, new_issue, project_add, save_front, set_fields};

    fn tracker() -> (tempfile::TempDir, Pm) {
        let dir = tempfile::tempdir().unwrap();
        let pm = Pm::init(&dir.path().join("pm")).unwrap();
        project_add(&pm, "cadence", "CAD", &[], &[], &[], None).unwrap();
        (dir, pm)
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

    /// Fixture edit: front fields a sweep test needs — `issue.md`
    /// written but not committed; the sweep's own commit lands on top.
    fn edit(pm: &Pm, id: &str, f: impl FnOnce(&mut Front)) {
        let (_p, dir) = issue_dir(pm, id).unwrap();
        let (mut front, body) = load_front(&dir).unwrap();
        f(&mut front);
        save_front(&dir, &front, &body).unwrap();
    }

    fn claim_at(by: &str, age_secs: i64) -> Claim {
        Claim {
            by: by.to_string(),
            at: time::iso(time::now_epoch() - age_secs),
            note: None,
        }
    }

    fn status(pm: &Pm, id: &str) -> String {
        let (_p, dir) = issue_dir(pm, id).unwrap();
        load_front(&dir).unwrap().0.status
    }

    /// The gate: `ready` refuses while any blocked_by target is open,
    /// and the refusal names what it waits on.
    #[test]
    fn ready_refuses_an_open_blocker() {
        let (tmp, pm) = tracker();
        let a = mk(&pm, tmp.path(), "a");
        let b = mk(&pm, tmp.path(), "b");
        edit(&pm, &a, |f| f.blocked_by = vec![b.clone()]);
        let e = set_fields(&pm, std::slice::from_ref(&a), &["status=ready".to_string()], "t").unwrap_err();
        assert!(e.to_string().contains(&b), "{e}");
        assert_eq!(status(&pm, &a), "backlog");
    }

    #[test]
    fn ready_passes_when_the_blocker_is_done() {
        let (tmp, pm) = tracker();
        let a = mk(&pm, tmp.path(), "a");
        let b = mk(&pm, tmp.path(), "b");
        edit(&pm, &a, |f| f.blocked_by = vec![b.clone()]);
        edit(&pm, &b, |f| f.status = "done".to_string());
        set_fields(&pm, std::slice::from_ref(&a), &["status=ready".to_string()], "t").unwrap();
        assert_eq!(status(&pm, &a), "ready");
    }

    /// patch_issue — the RPC/board writer — rides the same gate.
    #[test]
    fn patch_ready_refuses_an_open_blocker() {
        let (tmp, pm) = tracker();
        let a = mk(&pm, tmp.path(), "a");
        let b = mk(&pm, tmp.path(), "b");
        edit(&pm, &a, |f| f.blocked_by = vec![b.clone()]);
        let out = crate::issue::write::patch_issue(
            &pm,
            &a,
            &crate::issue::write::IssuePatch {
                status: Some("ready".to_string()),
                priority: None,
                owner: None,
                component: None,
                title: None,
                body: None,
                tags: None,
            },
            None,
            "t",
            None,
        );
        assert!(out.is_err(), "{out:?}");
        assert_eq!(status(&pm, &a), "backlog");
    }

    /// A blocked_by link landing on a held issue names its holders in
    /// the notice attempt — send outcomes are data, not errors.
    #[test]
    fn new_blocker_on_doing_reports_the_notice() {
        let (tmp, pm) = tracker();
        let a = mk(&pm, tmp.path(), "a");
        let b = mk(&pm, tmp.path(), "b");
        edit(&pm, &a, |f| {
            f.status = "doing".to_string();
            f.claim = Some(claim_at("w1", 60));
        });
        let out =
            crate::issue::write::link(&pm, &a, "blocked_by", &b, false, None, "t", None).unwrap();
        let notice = &out["blocker_notice"];
        assert_eq!(
            notice["skipped"].as_str().unwrap_or_default(),
            "no daemon state dir",
            "{notice}"
        );
    }

    /// A bare `doing` issue has nobody to tell — the link still lands.
    #[test]
    fn new_blocker_without_holder_is_skipped_quietly() {
        let (tmp, pm) = tracker();
        let a = mk(&pm, tmp.path(), "a");
        let b = mk(&pm, tmp.path(), "b");
        edit(&pm, &a, |f| f.status = "doing".to_string());
        let out =
            crate::issue::write::link(&pm, &a, "blocked_by", &b, false, None, "t", None).unwrap();
        assert_eq!(
            out["blocker_notice"]["skipped"].as_str().unwrap(),
            "no claim or owner to notify"
        );
    }

    /// The headline: a stale claimed blocked `doing` parks itself —
    /// backlog + tag + comment, claim kept as the resume address.
    #[test]
    fn sweep_parks_a_stale_blocked_doing() {
        let (tmp, pm) = tracker();
        let a = mk(&pm, tmp.path(), "a");
        let b = mk(&pm, tmp.path(), "b");
        edit(&pm, &a, |f| {
            f.status = "doing".to_string();
            f.blocked_by = vec![b.clone()];
            f.claim = Some(claim_at("w1", 48 * 3600));
        });
        let out = sweep(&pm, PARK_GRACE_SECS, false, None, "t").unwrap();
        assert_eq!(out["parked"][0]["id"], a, "{out}");
        let (_p, dir) = issue_dir(&pm, &a).unwrap();
        let (front, _) = load_front(&dir).unwrap();
        assert_eq!(front.status, "backlog");
        assert!(front.tags.iter().any(|t| t == PARK_TAG));
        assert_eq!(front.claim.as_ref().unwrap().by, "w1");
        let comments = std::fs::read_dir(dir.join("comments")).unwrap().count();
        assert_eq!(comments, 1);
    }

    /// A claim renewed inside the grace window keeps its seat.
    #[test]
    fn sweep_keeps_a_fresh_claim() {
        let (tmp, pm) = tracker();
        let a = mk(&pm, tmp.path(), "a");
        let b = mk(&pm, tmp.path(), "b");
        edit(&pm, &a, |f| {
            f.status = "doing".to_string();
            f.blocked_by = vec![b];
            f.claim = Some(claim_at("w1", 3600));
        });
        let out = sweep(&pm, PARK_GRACE_SECS, false, None, "t").unwrap();
        assert!(out["parked"].as_array().unwrap().is_empty(), "{out}");
        assert_eq!(status(&pm, &a), "doing");
    }

    /// `ready` and `done` are out of the park predicate — ready was
    /// gated at write time; done is terminal.
    #[test]
    fn sweep_only_parks_active_statuses() {
        let (tmp, pm) = tracker();
        let a = mk(&pm, tmp.path(), "a");
        let b = mk(&pm, tmp.path(), "b");
        edit(&pm, &a, |f| {
            f.status = "ready".to_string();
            f.blocked_by = vec![b];
            f.claim = Some(claim_at("w1", 96 * 3600));
        });
        let out = sweep(&pm, PARK_GRACE_SECS, false, None, "t").unwrap();
        assert!(out["parked"].as_array().unwrap().is_empty(), "{out}");
        assert_eq!(status(&pm, &a), "ready");
    }

    /// A rollup container's `doing` is derived — the sweep never
    /// writes a file status over it.
    #[test]
    fn sweep_skips_containers() {
        let (tmp, pm) = tracker();
        let epic = mk(&pm, tmp.path(), "epic");
        let leaf = mk(&pm, tmp.path(), "leaf");
        let b = mk(&pm, tmp.path(), "blocker");
        edit(&pm, &leaf, |f| {
            f.parent = Some(epic.clone());
            f.status = "doing".to_string();
        });
        edit(&pm, &epic, |f| {
            f.blocked_by = vec![b];
            f.claim = Some(claim_at("w1", 96 * 3600));
        });
        let out = sweep(&pm, PARK_GRACE_SECS, false, None, "t").unwrap();
        assert!(
            !out["parked"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["id"] == epic),
            "{out}"
        );
    }

    /// The resume side: all blockers closed → the tag drops and the
    /// last claimer gets the notice attempt (failed send recorded, tag
    /// still dropped — a dead alias never retry-storms).
    #[test]
    fn sweep_unblocks_and_untags() {
        let (tmp, pm) = tracker();
        let a = mk(&pm, tmp.path(), "a");
        let b = mk(&pm, tmp.path(), "b");
        edit(&pm, &a, |f| {
            f.status = "backlog".to_string();
            f.blocked_by = vec![b.clone()];
            f.tags = vec![PARK_TAG.to_string()];
            f.claim = Some(claim_at("w1", 96 * 3600));
        });
        edit(&pm, &b, |f| f.status = "done".to_string());
        let out = sweep(&pm, PARK_GRACE_SECS, false, None, "t").unwrap();
        assert_eq!(out["unblocked"][0]["id"], a, "{out}");
        assert_eq!(out["unblocked"][0]["sent"][0]["to"].as_str().unwrap(), "w1");
        let (_p, dir) = issue_dir(&pm, &a).unwrap();
        let (front, _) = load_front(&dir).unwrap();
        assert!(!front.tags.iter().any(|t| t == PARK_TAG));
    }

    /// A parked item whose blocker is still open stays parked — the
    /// tag is not a reason to touch it.
    #[test]
    fn sweep_keeps_a_still_blocked_park() {
        let (tmp, pm) = tracker();
        let a = mk(&pm, tmp.path(), "a");
        let b = mk(&pm, tmp.path(), "b");
        edit(&pm, &a, |f| {
            f.status = "backlog".to_string();
            f.blocked_by = vec![b];
            f.tags = vec![PARK_TAG.to_string()];
            f.claim = Some(claim_at("w1", 96 * 3600));
        });
        let out = sweep(&pm, PARK_GRACE_SECS, false, None, "t").unwrap();
        assert!(out["unblocked"].as_array().unwrap().is_empty(), "{out}");
        assert!(out["parked"].as_array().unwrap().is_empty(), "{out}");
    }

    #[test]
    fn sweep_dry_run_writes_nothing() {
        let (tmp, pm) = tracker();
        let a = mk(&pm, tmp.path(), "a");
        let b = mk(&pm, tmp.path(), "b");
        edit(&pm, &a, |f| {
            f.status = "doing".to_string();
            f.blocked_by = vec![b];
            f.claim = Some(claim_at("w1", 96 * 3600));
        });
        let out = sweep(&pm, PARK_GRACE_SECS, true, None, "t").unwrap();
        assert_eq!(out["parked"][0]["id"], a, "{out}");
        let (_p, dir) = issue_dir(&pm, &a).unwrap();
        let (front, _) = load_front(&dir).unwrap();
        assert_eq!(front.status, "doing");
        assert!(!front.tags.iter().any(|t| t == PARK_TAG));
    }
}
