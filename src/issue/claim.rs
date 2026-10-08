//! CAD-383: claims — who holds an issue that is in flight, and the check
//! `dispatch`, `issue start` and `issue claim` run before they touch it.
//!
//! **Holders** are the issue's `claim.by` (the PM, or whoever took the
//! issue) and its `owner` (the lane doing the work). **Requesters** are
//! everyone a request speaks for: `dispatch` names the PM (`--reply-to`,
//! default `CADENCE_ALIAS`) and the worker (`--to`); `issue start` the
//! requester (`--by`, else `--pm`, else `CADENCE_ALIAS`, else
//! `operator`) and the owner it would record (`--owner`/`--assignee`);
//! `issue claim` only the requester. A request that shares any name
//! with the holders is the holder's own — re-dispatching to the same
//! worker, or the claiming PM handing the issue to another worker, goes
//! through unchanged.
//!
//! Otherwise the issue belongs to someone else. In `doing`/`review` that
//! refuses, naming the holder and the claim age, unless the request
//! carries `--take-over <reason>` — the take-over replaces the claim and
//! the owner and is recorded as a tracker comment in its own commit
//! (`claim take-over by …` in `issue log`). In `backlog`/`ready` an owner
//! is an assignment or the project's `default_owner`, not work in
//! flight, so it only warns.
//!
//! **Liveness** (CAD-755): the daemon's checkup maps the agent registry
//! to `claim.stale` — a holder stopped, fenced (`attention`), offline or
//! endpoint-dead past [`STALE_GRACE`] gets the marker; a live holder
//! clears it. A stale claim needs no `--take-over`: the marker is the
//! recorded reason the claim no longer stands. Holders outside the
//! registry (operators, foreign PMs) are never marked — `issue claim`
//! refreshes their claim by hand. `owner` is untouched either way.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::line_times::LineTimes;
use crate::issue::model::{Claim, Front};
use crate::issue::{history, time, write, Pm};

/// Statuses where a foreign holder refuses; elsewhere it only warns.
pub const PROTECTED: &[&str] = &["doing", "review"];

/// Longest claim note or take-over reason kept.
const NOTE_MAX: usize = 500;

/// Per-issue bound on the git fallback that dates an owner-only claim.
const OWNER_CLOCK_TIMEOUT: Duration = Duration::from_secs(2);

/// CAD-755: how long a holder's agent may be dead before its claim is
/// marked stale — long enough to cover a restart, short enough that a
/// fenced lane does not hold a ticket hostage.
pub const STALE_GRACE: Duration = Duration::from_secs(30 * 60);

/// What [`check`] found for a request that may proceed.
#[derive(Debug, Default, Clone)]
pub struct Check {
    /// The issue belongs to someone else but is not protected — the
    /// request proceeds and says so.
    pub warning: Option<String>,
    /// The request displaces another holder with `--take-over`.
    pub take_over: Option<TakeOver>,
}

impl Check {
    /// The request is not the holder's own — its claim replaces the
    /// recorded one.
    pub fn foreign(&self) -> bool {
        self.warning.is_some() || self.take_over.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct TakeOver {
    /// The claim holder displaced (`claim.by`, else the owner).
    pub from: String,
    /// `pm-a (owner w1), claimed 2h ago` — for the comment.
    pub holder: String,
    pub reason: String,
}

/// An alias a claim may carry — the comment-author grammar, since the
/// claimant authors the claim's comment.
pub fn check_alias(alias: &str, flag: &str) -> Result<()> {
    // A leading '-' is an argv flag the next time this name is passed
    // to a command. Refuse it at the grammar, before any caller interpolates.
    if alias.starts_with('-') {
        return Err(Error::rejected(format!(
            "{flag} '{alias}' is not an alias — a leading '-' would be read as an argv flag"
        )));
    }
    if alias.is_empty()
        || alias.len() > 64
        || !alias
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(Error::rejected(format!(
            "{flag} '{alias}' is not an alias — letters, digits, '-' or '_'"
        )));
    }
    Ok(())
}

/// A take-over reason or claim note: trimmed, one line, non-empty for a
/// take-over, at most [`NOTE_MAX`] bytes.
pub fn clean_text(flag: &str, text: &str) -> Result<String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Error::rejected(format!(
            "{flag} is empty — a take-over needs the reason the current claim no longer stands"
        )));
    }
    if text.len() > NOTE_MAX || text.chars().any(char::is_control) {
        return Err(Error::rejected(format!(
            "{flag} must be one line of at most {NOTE_MAX} bytes"
        )));
    }
    Ok(text.to_string())
}

/// The issue's holders, claim first, de-duplicated.
pub fn holders(front: &Front) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let claim = front.claim.as_ref().map(|c| c.by.as_str());
    for h in [claim, front.owner.as_deref()].into_iter().flatten() {
        if !h.is_empty() && !out.contains(&h) {
            out.push(h);
        }
    }
    out
}

/// When the current claim started: `claim.at`, else — an issue owned
/// before claims existed — the tracker commit that last changed its
/// `owner:` line, bounded by `timeout`. `None` when neither answers.
pub fn since(pm_dir: &Path, project: &str, front: &Front, timeout: Duration) -> Option<i64> {
    if let Some(c) = &front.claim {
        return time::parse_iso(&c.at);
    }
    front.owner.as_ref()?;
    history::owner_changed_at(pm_dir, project, &front.id, timeout)
}

/// `pm-a (owner w1), claimed 2h ago` / `owner w1, owned 3d ago`.
fn describe(front: &Front, since: Option<i64>, now: i64) -> String {
    let age = since
        .map(|t| format!("{} ago", crate::inbox::fmt_age((now - t).max(0) as u64)))
        .unwrap_or_else(|| "age unknown".to_string());
    match (&front.claim, front.owner.as_deref()) {
        (Some(c), Some(o)) if o != c.by => format!("{} (owner {o}), claimed {age}", c.by),
        (Some(c), _) => format!("{}, claimed {age}", c.by),
        (None, Some(o)) => format!("owner {o}, owned {age}"),
        (None, None) => "nobody".to_string(),
    }
}

/// The claim check. `requesters` are the names the request speaks for
/// (see the module doc); `verb` names the refused command. `since` is
/// only evaluated when the issue belongs to someone else.
pub fn check(
    front: &Front,
    requesters: &[&str],
    take_over: Option<&str>,
    verb: &str,
    since: impl FnOnce() -> Option<i64>,
) -> Result<Check> {
    let held = holders(front);
    if held.is_empty() || requesters.iter().any(|r| held.contains(r)) {
        return Ok(Check::default());
    }
    let holder = describe(front, since(), time::now_epoch());
    let id = &front.id;
    let status = &front.status;
    let asking = requesters.join(" → ");
    if !PROTECTED.contains(&status.as_str()) {
        return Ok(Check {
            warning: Some(format!(
                "{id} is {status} and held by {holder} — only doing/review claims are \
                 enforced, so {verb} by {asking} proceeds"
            )),
            take_over: None,
        });
    }
    if let Some(reason) = take_over {
        return Ok(Check {
            warning: None,
            take_over: Some(TakeOver {
                from: held[0].to_string(),
                holder,
                reason: clean_text("--take-over", reason)?,
            }),
        });
    }
    // CAD-755: the daemon's stale marker already carries the reason the
    // claim no longer stands — taking it over needs no --take-over.
    if let Some(c) = front.claim.as_ref().filter(|c| c.stale.is_some()) {
        return Ok(Check {
            warning: None,
            take_over: Some(TakeOver {
                from: held[0].to_string(),
                holder,
                reason: format!("stale claim: {}", c.stale.as_deref().unwrap_or_default()),
            }),
        });
    }
    Err(Error::invalid(
        "claimed",
        format!(
            "{id} is {status} and held by {holder} — {verb} by {asking} refused so a \
             second lane does not start on it. Ask {} (`cadence issue show {id}`), or \
             pass --take-over \"<reason>\" to take it over (recorded on the issue)",
            held[0]
        ),
    ))
}

/// The take-over comment and the `issue log` subject for it.
pub fn take_over_record(by: &str, t: &TakeOver) -> (String, String) {
    (
        format!("Take-over by {by} from {}: {}", t.holder, t.reason),
        format!("claim take-over by {by} from {}", t.from),
    )
}

/// `claim` as JSON with its age — `null` when unclaimed.
pub fn json(front: &Front, now: i64) -> Value {
    match &front.claim {
        Some(c) => json!({
            "by": c.by, "at": c.at, "session": c.session,
            "last_seen": c.last_seen, "note": c.note, "stale": c.stale,
            "age_secs": time::parse_iso(&c.at).map(|t| (now - t).max(0)),
        }),
        None => Value::Null,
    }
}

/// A fresh claim stamped now.
pub fn new_claim(by: &str, note: Option<String>) -> Claim {
    Claim {
        by: by.to_string(),
        at: time::iso(time::now_epoch()),
        session: None,
        last_seen: None,
        note,
        stale: None,
    }
}

/// Claim ages for many issues — `status`, `overview` and the board's
/// poll. An owner-only issue is dated from the tracker's cached line
/// times (CAD-403), never a git walk per issue; without them (git did
/// not answer in time) it reports no age.
pub struct Clock<'a> {
    times: Option<&'a LineTimes>,
}

impl<'a> Clock<'a> {
    pub fn new(times: Option<&'a LineTimes>) -> Self {
        Self { times }
    }

    pub fn since(&self, project: &str, front: &Front) -> Option<i64> {
        if let Some(c) = &front.claim {
            return time::parse_iso(&c.at);
        }
        front.owner.as_ref()?;
        self.times?.owner_at(project, &front.id)
    }
}

/// One in-flight claim row for `status` and `overview`: every
/// doing/review issue with a holder.
pub fn row(project: &str, front: &Front, since: Option<i64>, now: i64) -> Value {
    json!({
        "issue": front.id,
        "project": project,
        "status": front.status,
        "by": front.claim.as_ref().map(|c| c.by.clone()).or_else(|| front.owner.clone()),
        "owner": front.owner,
        "note": front.claim.as_ref().and_then(|c| c.note.clone()),
        "stale": front.claim.as_ref().and_then(|c| c.stale.clone()),
        "since": since.map(time::iso),
        "age_secs": since.map(|t| (now - t).max(0)),
    })
}

/// The requester for `issue claim`/`release`: `--by`, else
/// `CADENCE_ALIAS`, else `operator`.
fn requester(by: Option<&str>, actor: &str) -> Result<String> {
    if let Some(by) = by {
        check_alias(by, "--by")?;
    }
    Ok(write::actor_who(actor, by))
}

/// `issue claim <ID> [--by] [--note] [--take-over <reason>]` — record
/// (or refresh) a claim the dispatch/start check sees, for a PM whose
/// lanes run outside cadence. One tracker commit: the claim,
/// `backlog|ready` → `doing`, and a comment; `owner` (the lane) is left
/// alone except on a take-over, which makes the claimant owner. Refused on done/dropped issues and, in
/// doing/review, when someone else holds it — unless `--take-over`.
#[track_caller]
pub fn claim(
    pm: &Pm,
    id: &str,
    by: Option<&str>,
    note: Option<&str>,
    take_over: Option<&str>,
    actor: &str,
) -> Result<Value> {
    let by = requester(by, actor)?;
    let note = note.map(|n| clean_text("--note", n)).transpose()?;
    let (project, dir) = write::issue_dir(pm, id)?;
    let _lock = pm.lock()?;
    let (front, body) = write::load_front(&dir)?;
    if matches!(front.status.as_str(), "done" | "dropped") {
        return Err(Error::rejected(format!(
            "{id} is {} — reopen it (`cadence issue set {id} status=ready`) before claiming it",
            front.status
        )));
    }
    let clock = || since(&pm.dir, &project.key, &front, OWNER_CLOCK_TIMEOUT);
    let mut checked = check(&front, &[by.as_str()], take_over, "issue claim", clock)?;
    // The owner lane is a holder, but claiming would unseat the PM that
    // claimed it — that is a take-over too.
    if let Some(c) = front.claim.as_ref().filter(|c| c.by != by) {
        if !checked.foreign() {
            // CAD-755: an owner reclaiming a stale claim needs no
            // --take-over either — the marker carries the reason.
            if c.stale.is_some() {
                checked.take_over = Some(TakeOver {
                    from: c.by.clone(),
                    holder: describe(&front, time::parse_iso(&c.at), time::now_epoch()),
                    reason: format!("stale claim: {}", c.stale.as_deref().unwrap_or_default()),
                });
            } else {
                let Some(reason) = take_over else {
                    return Err(Error::invalid(
                        "claimed",
                        format!(
                            "{id} is claimed by {} and {by} is its owner — pass --take-over \
                             \"<reason>\" to take the claim itself",
                            c.by
                        ),
                    ));
                };
                checked.take_over = Some(TakeOver {
                    from: c.by.clone(),
                    holder: describe(&front, time::parse_iso(&c.at), time::now_epoch()),
                    reason: clean_text("--take-over", reason)?,
                });
            }
        }
    }
    let mut next = front.clone();
    let refreshed = !checked.foreign() && front.claim.as_ref().is_some_and(|c| c.by == by);
    next.claim = Some(new_claim(&by, note.clone()));
    // `owner` stays the lane: a claim leaves it alone, so the worker a
    // later dispatch names becomes owner. A take-over displaces the old
    // lane, so the claimant stands in until it dispatches one.
    if checked.take_over.is_some() {
        next.owner = Some(by.clone());
    }
    if matches!(next.status.as_str(), "backlog" | "ready") {
        // CAD-360: claiming moves the issue into work — the same status
        // write rule as `issue set`, so an unapproved plan's ticket is
        // refused before anything is written.
        crate::issue::plan::check_status_write(&pm.dir, &front, "doing")?;
        next.status = "doing".to_string();
    }
    let suffix = note
        .as_deref()
        .map(|n| format!(": {n}"))
        .unwrap_or_default();
    let (text, subject) = match &checked.take_over {
        Some(t) => take_over_record(&by, t),
        None if refreshed => (
            format!("Claim refreshed by {by}{suffix}"),
            format!("claim refreshed by {by}"),
        ),
        None => (format!("Claimed by {by}{suffix}"), format!("claim by {by}")),
    };
    let text = match &checked.warning {
        Some(w) => format!("{text}\nWarning: {w}"),
        None => text,
    };
    let comment = write::commit_front_with_comment(
        pm, &dir, &front, &next, &body, &by, "claim", &text, &subject, actor,
    )?;
    let now = time::now_epoch();
    Ok(json!({
        "id": id,
        "claim": json(&next, now),
        "owner": next.owner,
        "status": next.status,
        "refreshed": refreshed,
        "warning": checked.warning,
        "take_over": checked.take_over.as_ref().map(|t| json!({"from": t.from, "reason": t.reason})),
        "comment": comment,
        "committed": true,
    }))
}

/// `issue release <ID> [--by] [--note]` — the holder gives the issue
/// up: the claim is cleared, and `owner` too when it is the releaser.
/// Status is left alone. Anyone else is refused, naming the holder.
#[track_caller]
pub fn release(
    pm: &Pm,
    id: &str,
    by: Option<&str>,
    note: Option<&str>,
    actor: &str,
) -> Result<Value> {
    let by = requester(by, actor)?;
    let note = note.map(|n| clean_text("--note", n)).transpose()?;
    let (project, dir) = write::issue_dir(pm, id)?;
    let _lock = pm.lock()?;
    let (front, body) = write::load_front(&dir)?;
    let held = holders(&front);
    if held.is_empty() {
        return Err(Error::rejected(format!(
            "{id} has no claim or owner to release"
        )));
    }
    if !held.contains(&by.as_str()) {
        let holder = describe(
            &front,
            since(&pm.dir, &project.key, &front, OWNER_CLOCK_TIMEOUT),
            time::now_epoch(),
        );
        return Err(Error::invalid(
            "claimed",
            format!(
                "{id} is held by {holder} — only a holder can release it; \
                 `cadence issue claim {id} --take-over \"<reason>\"` takes it instead"
            ),
        ));
    }
    let mut next = front.clone();
    next.claim = None;
    if next.owner.as_deref() == Some(by.as_str()) {
        next.owner = None;
    }
    let suffix = note
        .as_deref()
        .map(|n| format!(": {n}"))
        .unwrap_or_default();
    let comment = write::commit_front_with_comment(
        pm,
        &dir,
        &front,
        &next,
        &body,
        &by,
        "claim",
        &format!("Released by {by}{suffix}"),
        &format!("release by {by}"),
        actor,
    )?;
    Ok(json!({
        "id": id,
        "claim": Value::Null,
        "owner": next.owner,
        "status": next.status,
        "comment": comment,
        "committed": true,
    }))
}

/// How old an unchanged live claim's `last_seen` gets before the sweep
/// re-stamps it (CAD-1150).
const LIVE_RESTAMP_SECS: i64 = 3600;

/// The daemon-side claim sweep (CAD-755). `dead` maps every registered
/// agent that no longer counts as live — stopped, fenced (`attention`),
/// offline, or whose endpoint `agent_liveness` reports dead — to when
/// its registry row last changed and why. `live` names the rest of the
/// registry. A claim whose holder is dead past `grace` gets
/// `claim.stale` stamped with the reason; a claim held by a live agent
/// clears the marker on the next pass. Claims whose holder is not a
/// registered agent at all (an operator, a foreign PM) are never marked
/// — `issue claim` is their manual refresh path. `owner` is never
/// touched. Marks and heals commit separately, each one commit.
///
/// Returns the marked/healed id lists for the daemon's checkup log.
pub fn liveness_sweep(
    pm: &Pm,
    dead: &HashMap<String, (i64, String)>,
    live: &HashMap<String, Option<String>>,
    grace: Duration,
    now: i64,
    actor: &str,
) -> Result<Value> {
    let issues = crate::issue::board::load_all(&pm.dir, None)?;
    let claimed: Vec<String> = issues
        .iter()
        .filter(|i| {
            i.front.claim.is_some() && !matches!(i.front.status.as_str(), "done" | "dropped")
        })
        .map(|i| i.front.id.clone())
        .collect();
    if claimed.is_empty() {
        return Ok(json!({"marked": [], "healed": [], "refreshed": []}));
    }
    let _lock = pm.lock()?;
    let mut healed_ids = HashSet::new();
    let staged = write::stage(pm, &claimed, |_project, front| {
        // Recheck under the PM lock: the initial board snapshot can race
        // a human completing or dropping the issue.
        if matches!(front.status.as_str(), "done" | "dropped") {
            return Ok(false);
        }
        let Some(c) = front.claim.as_mut() else {
            return Ok(false);
        };
        if let Some((since, why)) = dead.get(c.by.as_str()) {
            if c.stale.is_none() && now - *since >= grace.as_secs() as i64 {
                c.stale = Some(format!("{why} since {}", time::iso(*since)));
                return Ok(true);
            }
        } else if let Some(session) = live.get(c.by.as_str()) {
            // Bound tracker writes during a frequent checkup (CAD-1150:
            // an unchanged live claim is re-stamped at most hourly, so
            // the sweep stops holding the PM lock every few minutes),
            // while retaining persisted evidence across restarts.
            let due = c
                .last_seen
                .as_deref()
                .and_then(time::parse_iso)
                .is_none_or(|seen| now - seen >= LIVE_RESTAMP_SECS);
            if due || c.stale.is_some() || c.session != *session {
                if c.stale.is_some() {
                    healed_ids.insert(front.id.clone());
                }
                c.last_seen = Some(time::iso(now));
                c.session = session.clone();
                c.stale = None;
                return Ok(true);
            }
        }
        Ok(false)
    })?;
    let mut stale = Vec::new();
    let mut healed = Vec::new();
    let mut refreshed = Vec::new();
    for s in staged {
        if s.front.claim.as_ref().is_some_and(|c| c.stale.is_some()) {
            stale.push(s);
        } else if healed_ids.contains(&s.id) {
            healed.push(s);
        } else {
            refreshed.push(s);
        }
    }
    let (marked, _) = write::commit_staged(pm, &stale, "claims marked stale", actor)?;
    let (healed, _) = write::commit_staged(pm, &healed, "claims live again", actor)?;
    let (refreshed, _) = write::commit_staged(pm, &refreshed, "claims checked live", actor)?;
    Ok(json!({"marked": marked, "healed": healed, "refreshed": refreshed}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn front(status: &str, owner: Option<&str>, claim_by: Option<&str>) -> Front {
        let mut f = Front::new("D-1", "t", "2026-09-23T00:00:00Z");
        f.status = status.to_string();
        f.owner = owner.map(str::to_string);
        f.claim = claim_by.map(|b| Claim {
            by: b.to_string(),
            at: "2026-09-23T00:00:00Z".to_string(),
            session: None,
            last_seen: None,
            note: None,
            stale: None,
        });
        f
    }

    fn run(f: &Front, asking: &[&str], take_over: Option<&str>) -> Result<Check> {
        check(f, asking, take_over, "dispatch", || Some(0))
    }

    /// The (a)/(b)/(c) matrix from the ticket, on a doing issue that
    /// pm-a dispatched to w1.
    #[test]
    fn same_holder_passes_foreign_refuses() {
        let f = front("doing", Some("w1"), Some("pm-a"));
        // (a) re-dispatch to the same worker.
        assert!(!run(&f, &["pm-a", "w1"], None).unwrap().foreign());
        // (b) the claiming PM dispatching another worker.
        assert!(!run(&f, &["pm-a", "w2"], None).unwrap().foreign());
        // Dispatching to the current owner.
        assert!(!run(&f, &["pm-b", "w1"], None).unwrap().foreign());
        // (c) a different PM and worker.
        let err = run(&f, &["pm-b", "w3"], None).unwrap_err().to_string();
        assert!(
            err.contains("pm-a (owner w1)")
                && err.contains("ago")
                && err.contains("--take-over")
                && err.contains("pm-b → w3"),
            "{err}"
        );
        // Review is protected the same way.
        let r = front("review", Some("w1"), None);
        let err = run(&r, &["pm-b"], None).unwrap_err().to_string();
        assert!(err.contains("owner w1, owned"), "{err}");
    }

    #[test]
    fn take_over_needs_a_reason() {
        let f = front("doing", Some("w1"), Some("pm-a"));
        assert!(run(&f, &["pm-b"], Some("  ")).is_err());
        assert!(run(&f, &["pm-b"], Some("a\nb")).is_err());
        let c = run(&f, &["pm-b"], Some(" lane died ")).unwrap();
        let t = c.take_over.unwrap();
        assert_eq!((t.from.as_str(), t.reason.as_str()), ("pm-a", "lane died"));
        let (text, subject) = take_over_record("pm-b", &t);
        assert!(text.starts_with("Take-over by pm-b from pm-a"), "{text}");
        assert_eq!(subject, "claim take-over by pm-b from pm-a");
    }

    #[test]
    fn alias_rejects_a_leading_dash() {
        let err = check_alias("-lane", "alias").unwrap_err().to_string();
        assert!(err.contains("leading '-'"), "{err}");
        assert!(check_alias("-", "--by").is_err());
        assert!(check_alias("lane-1", "alias").is_ok());
        // No case-fold and no unicode-fold: MASTER is a different name,
        // and a Cyrillic lookalike is not an alias at all.
        assert!(check_alias("MASTER", "alias").is_ok());
        assert!(check_alias("m\u{0430}ster", "alias").is_err());
    }

    /// Backlog/ready never refuse: an owner there only warns; unowned
    /// and unclaimed issues pass silently in every status.
    #[test]
    fn backlog_and_unowned_are_unchanged() {
        for status in ["backlog", "ready"] {
            let f = front(status, Some("bob"), None);
            let c = run(&f, &["pm-a", "w1"], None).unwrap();
            assert!(c.warning.unwrap().contains("bob"));
            assert!(c.take_over.is_none());
        }
        for status in ["backlog", "ready", "doing", "review", "done"] {
            let c = run(&front(status, None, None), &["anyone"], None).unwrap();
            assert!(!c.foreign(), "{status}");
        }
        // Done/dropped are not protected either.
        assert!(run(&front("done", Some("w1"), None), &["x"], None)
            .unwrap()
            .warning
            .is_some());
    }

    #[test]
    fn holders_dedupe_and_order() {
        assert_eq!(
            holders(&front("doing", Some("w1"), Some("pm"))),
            ["pm", "w1"]
        );
        assert_eq!(holders(&front("doing", Some("pm"), Some("pm"))), ["pm"]);
        assert!(holders(&front("doing", None, None)).is_empty());
    }

    fn stale_front(status: &str, owner: Option<&str>, claim_by: &str) -> Front {
        let mut f = front(status, owner, Some(claim_by));
        f.claim.as_mut().unwrap().stale = Some("stopped since 2026-01-01T00:00:00Z".into());
        f
    }

    /// CAD-755: a daemon-stale claim frees the issue — a foreign lane
    /// takes over without --take-over, and the marker's reason stands in
    /// for the operator's.
    #[test]
    fn stale_claim_takes_over_without_a_flag() {
        let f = stale_front("doing", Some("w1"), "pm-a");
        let c = run(&f, &["pm-b", "w3"], None).unwrap();
        let t = c.take_over.unwrap();
        assert_eq!(t.from, "pm-a");
        assert!(t.reason.contains("stale claim"), "{}", t.reason);
        // An explicit --take-over still speaks for itself.
        let c = run(&f, &["pm-b"], Some("lane confirmed dead")).unwrap();
        assert_eq!(c.take_over.unwrap().reason, "lane confirmed dead");
        // And a stale claim on an unprotected status still only warns.
        let f = stale_front("backlog", Some("w1"), "pm-a");
        assert!(run(&f, &["pm-b"], None).unwrap().warning.is_some());
        // A marker that never reached the front refuses as before.
        let f = front("doing", Some("w1"), Some("pm-a"));
        assert!(run(&f, &["pm-b", "w3"], None).is_err());
    }

    /// A tracker with project `cadence`; `issue` files tickets under it.
    fn tracker() -> (tempfile::TempDir, Pm) {
        let dir = tempfile::tempdir().unwrap();
        let pm = Pm::init(&dir.path().join("pm")).unwrap();
        write::project_add(&pm, "cadence", "CAD", &[], &[], &[], None).unwrap();
        (dir, pm)
    }

    fn issue(pm: &Pm, cwd: &Path, title: &str, owner: Option<&str>) -> String {
        write::new_issue(
            pm,
            cwd,
            Some("cadence"),
            title,
            None,
            None,
            &[],
            owner,
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

    fn front_of(pm: &Pm, id: &str) -> Front {
        let (_p, dir) = write::issue_dir(pm, id).unwrap();
        write::load_front(&dir).unwrap().0
    }

    /// CAD-755: dead past grace → stale; live again → healed; a holder
    /// the registry does not know (an operator) is never auto-marked;
    /// a dead holder inside grace keeps its claim whole.
    #[test]
    fn liveness_sweep_marks_heals_and_skips_unregistered() {
        let (tmp, pm) = tracker();
        let dead_id = issue(&pm, tmp.path(), "dead lane", Some("w1-lane"));
        let live_id = issue(&pm, tmp.path(), "live lane", None);
        let op_id = issue(&pm, tmp.path(), "operator lane", None);
        claim(&pm, &dead_id, Some("w1"), None, None, "t").unwrap();
        claim(&pm, &live_id, Some("w2"), None, None, "t").unwrap();
        claim(&pm, &op_id, Some("operator"), None, None, "t").unwrap();
        let now = time::now_epoch();
        let dead = HashMap::from([
            ("w1".to_string(), (now - 3600, "stopped".to_string())),
            ("w2".to_string(), (now - 60, "offline".to_string())),
        ]);
        // w2 is dead but inside the grace window — untouched.
        let out = liveness_sweep(&pm, &dead, &HashMap::new(), STALE_GRACE, now, "daemon").unwrap();
        assert_eq!(out["marked"], json!([dead_id.clone()]));
        let stale = front_of(&pm, &dead_id).claim.unwrap().stale.unwrap();
        assert!(stale.contains("stopped"), "{stale}");
        assert!(front_of(&pm, &live_id).claim.unwrap().stale.is_none());
        // `operator` is in no registry map — its claim stays whole.
        assert!(front_of(&pm, &op_id).claim.unwrap().stale.is_none());
        // owner is untouched by the mark.
        assert_eq!(front_of(&pm, &dead_id).owner.as_deref(), Some("w1-lane"));

        // w1 resumes → its marker clears; the operator claim is unmoved.
        let live = HashMap::from([("w1".to_string(), None)]);
        let out = liveness_sweep(&pm, &dead, &live, STALE_GRACE, now, "daemon").unwrap();
        // w1 is in both maps this pass — dead wins, nothing heals while
        // the registry still says stopped.
        assert_eq!(out["healed"], json!([]));
        let out = liveness_sweep(&pm, &HashMap::new(), &live, STALE_GRACE, now, "daemon").unwrap();
        assert_eq!(out["healed"], json!([dead_id.clone()]));
        assert!(front_of(&pm, &dead_id).claim.unwrap().stale.is_none());
    }

    #[test]
    fn live_check_persists_claim_session_and_last_seen() {
        let (tmp, pm) = tracker();
        let id = issue(&pm, tmp.path(), "live evidence", None);
        claim(&pm, &id, Some("w1"), None, None, "t").unwrap();
        let now = time::now_epoch();
        let live = HashMap::from([("w1".to_string(), Some("session-1".to_string()))]);
        liveness_sweep(&pm, &HashMap::new(), &live, STALE_GRACE, now, "daemon").unwrap();
        let persisted = front_of(&pm, &id);
        assert_eq!(json(&persisted, now)["last_seen"], json!(time::iso(now)));
        assert_eq!(json(&persisted, now)["session"], json!("session-1"));
        assert!(persisted.claim.unwrap().stale.is_none());

        // A same-session check inside the write interval makes no new
        // evidence commit; crossing it (hourly, CAD-1150) refreshes the
        // persisted time.
        let out =
            liveness_sweep(&pm, &HashMap::new(), &live, STALE_GRACE, now + 60, "daemon").unwrap();
        assert_eq!(out["healed"], json!([]));
        assert_eq!(
            json(&front_of(&pm, &id), now + 60)["last_seen"],
            json!(time::iso(now))
        );
        liveness_sweep(
            &pm,
            &HashMap::new(),
            &live,
            STALE_GRACE,
            now + 301,
            "daemon",
        )
        .unwrap();
        assert_eq!(
            json(&front_of(&pm, &id), now + 301)["last_seen"],
            json!(time::iso(now)),
            "an unchanged live claim is not re-stamped after 5 minutes"
        );
        liveness_sweep(
            &pm,
            &HashMap::new(),
            &live,
            STALE_GRACE,
            now + 3600,
            "daemon",
        )
        .unwrap();
        assert_eq!(
            json(&front_of(&pm, &id), now + 3600)["last_seen"],
            json!(time::iso(now + 3600))
        );

        // A replacement session is recorded even before the interval.
        let replacement = HashMap::from([("w1".to_string(), Some("session-2".to_string()))]);
        liveness_sweep(
            &pm,
            &HashMap::new(),
            &replacement,
            STALE_GRACE,
            now + 3601,
            "daemon",
        )
        .unwrap();
        assert_eq!(
            json(&front_of(&pm, &id), now + 3601)["session"],
            json!("session-2")
        );
    }

    #[test]
    fn terminal_claims_are_not_rewritten_by_liveness_checks() {
        let (tmp, pm) = tracker();
        for status in ["done", "dropped"] {
            let id = issue(&pm, tmp.path(), status, None);
            claim(&pm, &id, Some("w1"), None, None, "t").unwrap();
            write::set_fields(
                &pm,
                std::slice::from_ref(&id),
                &[format!("status={status}")],
                "t",
                (status == "done").then_some("terminal fixture with no release evidence"),
            )
            .unwrap();
            let before = front_of(&pm, &id).claim.unwrap();
            let now = time::now_epoch();
            let live = HashMap::from([("w1".to_string(), Some("session-1".to_string()))]);
            let out =
                liveness_sweep(&pm, &HashMap::new(), &live, STALE_GRACE, now, "daemon").unwrap();
            assert_eq!(out["refreshed"], json!([]), "{status}: {out}");
            assert_eq!(front_of(&pm, &id).claim.unwrap(), before);
        }
    }

    /// CAD-755: the whole check chain — a stale-claimed doing issue is
    /// claimed by another lane with no --take-over, and the holder's
    /// owner is never a magic backdoor either.
    #[test]
    fn stale_claim_frees_issue_claim_for_foreign_and_owner() {
        let (tmp, pm) = tracker();
        let id = issue(&pm, tmp.path(), "taken", None);
        claim(&pm, &id, Some("pm-a"), None, None, "t").unwrap();
        // Foreign lane on a live claim still refuses.
        assert!(claim(&pm, &id, Some("pm-b"), None, None, "t").is_err());
        let now = time::now_epoch();
        let dead = HashMap::from([("pm-a".to_string(), (now - 3600, "stopped".to_string()))]);
        liveness_sweep(&pm, &dead, &HashMap::new(), STALE_GRACE, now, "daemon").unwrap();
        let out = claim(&pm, &id, Some("pm-b"), None, None, "t").unwrap();
        assert_eq!(out["take_over"]["from"].as_str().unwrap(), "pm-a", "{out}");
        assert!(out["take_over"]["reason"]
            .as_str()
            .unwrap()
            .contains("stale claim"));
        // The take-over reclaims the claim under the new holder.
        assert_eq!(front_of(&pm, &id).claim.unwrap().by, "pm-b");
    }
}
