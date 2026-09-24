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
    let Some(reason) = take_over else {
        return Err(Error::invalid(
            "claimed",
            format!(
                "{id} is {status} and held by {holder} — {verb} by {asking} refused so a \
                 second lane does not start on it. Ask {} (`cadence issue show {id}`), or \
                 pass --take-over \"<reason>\" to take it over (recorded on the issue)",
                held[0]
            ),
        ));
    };
    Ok(Check {
        warning: None,
        take_over: Some(TakeOver {
            from: held[0].to_string(),
            holder,
            reason: clean_text("--take-over", reason)?,
        }),
    })
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
            "by": c.by, "at": c.at, "note": c.note,
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
        note,
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
        pm, &dir, &front, &next, &body, &by, &text, &subject, actor,
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
            note: None,
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
}
