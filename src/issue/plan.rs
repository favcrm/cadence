//! Plans (CAD-359/360): a proposed epic with its tickets, approved by
//! the operator before any of its work starts.
//!
//! A plan file is Markdown: YAML frontmatter (`title`, `goal`,
//! optional `non_goals`), an optional intro, then one level-two section
//! per ticket:
//!
//! ```markdown
//! ## Ticket title
//! size: M
//! agent: dev-1
//! depends_on: 1, CAD-12
//!
//! What the ticket is about.
//!
//! ### Acceptance
//! - [ ] a testable criterion
//! ```
//!
//! The metadata lines (`size`, `agent`, `depends_on`) are optional and
//! must directly follow the heading; `depends_on` names earlier or
//! later tickets by their 1-based position (`2` or `#2`) or existing
//! issues by id. Every ticket needs at least one acceptance item
//! (CAD-298). `propose` writes the epic and its tickets in one tracker
//! commit ([`write::create_plan`]); the epic carries `plan:` in its
//! frontmatter (with the ticket list) and each ticket carries
//! `plan_epic`; [`gate`] refuses to start any ticket until that plan is
//! `approved`, failing closed when membership cannot be decided. Issues
//! outside plans are never gated here. The gate is a process guard,
//! not a security boundary — see docs/design/WORK-MODEL.md, which also
//! gives the rollout order (the reader everywhere before the first
//! `plan propose`).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::model::{self, Front};
use crate::issue::parse;
use crate::issue::{board, write, Pm};

mod doc;
mod intake;

pub use doc::{parse_plan, Dep, PlanDoc, Ticket};
pub(crate) use intake::scope_digest;
pub use intake::GoalIntake;

/// Largest plan text `propose` accepts.
pub const MAX_PLAN_BYTES: usize = 256 * 1024;
/// Most tickets one plan may carry.
pub const MAX_TICKETS: usize = 50;

/// `plan propose` — secret-scan the plan text, parse it, and create
/// the epic and its tickets in one tracker commit. `allow` is the
/// caller's secret allowlist (the daemon passes its own state dir's).
/// `workflow` is the template the plan was rendered from —
/// `<name>` or `<app>/<name>` (CAD-547) — recorded on the epic so
/// `app remove` can find the app's open plans.
pub fn propose(
    pm: &Pm,
    project_key: &str,
    text: &str,
    workflow: Option<&str>,
    allow: &crate::secret::Allowlist,
    actor: &str,
    // CAD-225: ids a durable unattended intent reserved but never wrote
    // (a crashed admission) — allocation must skip them so the intent
    // stays reconcilable. Evaluated under the PM write lock.
    avoid: &dyn Fn() -> Result<HashSet<String>>,
) -> Result<Value> {
    if text.len() > MAX_PLAN_BYTES {
        return Err(Error::rejected(format!(
            "Plan is {} bytes — a plan is at most {MAX_PLAN_BYTES} bytes; split it",
            text.len()
        )));
    }
    let warnings = crate::secret::guard_with("plan", text, allow)?;
    let doc = parse_plan(text)?;
    if workflow.is_some() && doc.intake.is_some() {
        return Err(Error::rejected(
            "goal intake must be an ordinary file proposal, not a workflow render",
        ));
    }
    if doc.tickets.len() > MAX_TICKETS {
        return Err(Error::rejected(format!(
            "Plan has {} tickets — a plan carries at most {MAX_TICKETS}; split it",
            doc.tickets.len()
        )));
    }
    let mut out = write::create_plan(pm, project_key, &doc, workflow, actor, avoid)?;
    if !warnings.is_empty() {
        out["secret_warnings"] = crate::secret::warnings_json(&warnings);
    }
    Ok(out)
}

fn unreadable(id: &str, why: impl std::fmt::Display) -> Error {
    Error::invalid(
        "plan_unreadable",
        format!(
            "{id} cannot be read ({why}) — plan membership is undecidable, so this is \
             refused; fix the file (`cadence issue lint`)"
        ),
    )
}

/// Look `id` up across projects, strictly: `Ok(None)` only when no
/// project holds a folder of that name. A folder that exists but is a
/// symlink, lacks `issue.md` or does not parse is an error — the gate
/// never reads "unreadable" as "not a plan".
fn probe_issue(pm_dir: &Path, id: &str) -> Result<Option<board::Issue>> {
    model::check_id(id)?;
    let projects = crate::issue::project::list(pm_dir).map_err(|e| unreadable(id, e))?;
    for project in projects {
        let dir = pm_dir.join(&project.key).join(id);
        if dir.symlink_metadata().is_err() {
            continue;
        }
        return board::load_issue(pm_dir, &project.key, id)
            .map(Some)
            .map_err(|e| unreadable(id, e));
    }
    Ok(None)
}

/// The plan `front` is a ticket of: `(epic id, plan)`, or `None` for an
/// issue in no plan (and for a plan epic itself). Membership is the
/// epic's `plan.tickets` list, cross-checked with the ticket's own
/// `plan_epic` marker and its `parent`:
///
/// - an epic listing the id makes it a ticket, whatever its links say;
/// - a `plan_epic` marker whose epic is missing, unreadable, has no
///   `plan:` or does not list the id refuses (`plan_unreadable` /
///   `plan_missing`) — a rewrite that dropped one side fails closed;
/// - a `parent` that is a plan epic not listing the id refuses
///   (`plan_not_member`) — tickets join a plan only by `plan propose`;
/// - a parent that exists but cannot be read refuses; only a truly
///   absent parent passes.
pub fn membership(pm_dir: &Path, front: &Front) -> Result<Option<(String, model::Plan)>> {
    if front.plan.is_some() {
        return Ok(None);
    }
    let id = front.id.as_str();
    let all = board::load_all(pm_dir, None).map_err(|e| unreadable(id, e))?;
    let listed = all.iter().find_map(|i| {
        i.front
            .plan
            .as_ref()
            .filter(|p| p.tickets.iter().any(|t| t == id))
            .map(|p| (i.front.id.clone(), p.clone()))
    });
    if listed.is_some() {
        return Ok(listed);
    }
    if let Some(marker) = &front.plan_epic {
        return Err(match probe_issue(pm_dir, marker)? {
            None => unreadable(
                marker,
                format!("{id} names it as its plan but it does not exist"),
            ),
            Some(epic) if epic.front.plan.is_none() => Error::invalid(
                "plan_missing",
                format!(
                    "{id} is a ticket of plan {marker}, but {marker} carries no plan — it was \
                     rewritten without its `plan:` (an older cadence binary?); refused until \
                     the plan is restored"
                ),
            ),
            Some(_) => Error::invalid(
                "plan_missing",
                format!(
                    "{id} names plan {marker}, which does not list it — refused until the \
                     ticket and the plan agree"
                ),
            ),
        });
    }
    if let Some(parent) = &front.parent {
        if let Some(epic) = probe_issue(pm_dir, parent)? {
            if epic.front.plan.is_some() {
                return Err(Error::invalid(
                    "plan_not_member",
                    format!(
                        "{id} is parented to plan {parent} but is not one of its tickets — \
                         the operator approved a fixed list; propose a new plan for new work"
                    ),
                ));
            }
        }
    }
    Ok(None)
}

fn not_approved(epic: &str, state: &str, what: &str) -> Error {
    let next = if state == "proposed" {
        format!("approve it with `cadence plan approve {epic}`")
    } else {
        "propose a new plan (`cadence plan propose`) instead".to_string()
    };
    Error::invalid(
        "plan_not_approved",
        format!("plan {epic} is {state} — {next}; {what}"),
    )
}

/// CAD-360: the dispatch gate. A ticket of a plan starts only once that
/// plan is `approved`, and only with acceptance criteria; a plan epic
/// itself is never started (its tickets are). Membership is decided by
/// [`membership`], failing closed. Issues in no plan pass untouched.
///
/// This is a process guard, not a security boundary: `~/pm` is a git
/// repo any local agent can write, so it stops mistakes and keeps the
/// board honest; it does not stop a determined same-uid process.
pub fn gate(pm_dir: &Path, front: &Front, body: &str) -> Result<()> {
    gate_snapshot(pm_dir, front, body).map(drop)
}

/// Admission proof for callers preparing a fresh kickoff. The approved
/// intake epic can change even when a carried issue's scope does not.
pub(crate) fn gate_snapshot(pm_dir: &Path, front: &Front, body: &str) -> Result<Option<String>> {
    admit(pm_dir, front, body, None)
}

pub(crate) fn gate_snapshot_at_base(
    pm_dir: &Path,
    front: &Front,
    body: &str,
    base: &str,
) -> Result<Option<String>> {
    admit(pm_dir, front, body, Some(base))
}

pub(super) fn gate_claim(pm_dir: &Path, front: &Front, body: &str, base: &str) -> Result<()> {
    gate_snapshot_at_base(pm_dir, front, body, base).map(drop)
}

/// Claim without opening a lane. Only fresh intake admission needs scope
/// proof; ordinary plans retain their existing status-transition rule.
pub(super) fn gate_direct_claim(
    pm_dir: &Path,
    front: &Front,
    body: &str,
    entering_work: bool,
) -> Result<()> {
    if front
        .plan
        .as_ref()
        .is_some_and(|plan| plan.intake.is_some())
    {
        return gate(pm_dir, front, body);
    }
    if !entering_work && front.plan_epic.is_none() {
        return Ok(());
    }
    match membership(pm_dir, front)? {
        Some((epic, plan)) if plan.intake.is_some() => {
            admit_member(pm_dir, front, body, None, &epic, &plan).map(drop)
        }
        Some((epic, plan)) if entering_work && plan.state != "approved" => Err(not_approved(
            &epic,
            &plan.state,
            &format!("{} stays in backlog until then", front.id),
        )),
        _ => Ok(()),
    }
}

fn admit(
    pm_dir: &Path,
    front: &Front,
    body: &str,
    execution_base: Option<&str>,
) -> Result<Option<String>> {
    if front.plan.is_some() {
        let epic = &front.id;
        return Err(Error::invalid(
            "plan_epic",
            format!(
                "{epic} is a plan — dispatch its tickets, not the epic \
                 (`cadence plan show {epic}`)"
            ),
        ));
    }
    let Some((epic, plan)) = membership(pm_dir, front)? else {
        return Ok(None);
    };
    admit_member(pm_dir, front, body, execution_base, &epic, &plan)
}

fn admit_member(
    pm_dir: &Path,
    front: &Front,
    body: &str,
    execution_base: Option<&str>,
    epic: &str,
    plan: &model::Plan,
) -> Result<Option<String>> {
    if plan.state != "approved" {
        return Err(not_approved(
            epic,
            &plan.state,
            &format!("{} cannot start before its plan is approved", front.id),
        ));
    }
    if parse::acceptance_items(body).is_empty() {
        return Err(Error::invalid(
            "plan_acceptance_missing",
            format!(
                "{} is in plan {epic} and has no acceptance criteria — add them with \
                 `cadence issue acceptance {} --from <file>`",
                front.id, front.id
            ),
        ));
    }
    if plan.intake.is_some() {
        return verify_intake_dispatch(pm_dir, epic, front, body, execution_base).map(Some);
    }
    Ok(None)
}

fn verify_intake_dispatch(
    pm_dir: &Path,
    epic: &str,
    front: &Front,
    body: &str,
    execution_base: Option<&str>,
) -> Result<String> {
    let mut issue = probe_issue(pm_dir, epic)?
        .ok_or_else(|| unreadable(epic, "goal-intake epic is missing"))?;
    let project = crate::issue::project::list(pm_dir)?
        .into_iter()
        .find(|project| project.key == issue.project)
        .ok_or_else(|| unreadable(epic, "its project is missing"))?;
    let mut visited = HashSet::new();
    loop {
        let id = &issue.front.id;
        if !visited.insert(id.clone()) {
            return Err(unreadable(id, "cyclic intake revisions"));
        }
        let intake = issue
            .front
            .plan
            .as_ref()
            .filter(|plan| plan.state == "approved")
            .and_then(|plan| plan.intake.as_ref())
            .ok_or_else(|| unreadable(id, "intake approval evidence is missing"))?;
        let Some(next) = &intake.superseded_by else {
            intake.require_goal(&issue.body)?;
            intake.require_scope(front, body)?;
            require_intake_dependencies(pm_dir, &project.key, id, intake, front)?;
            intake.require_sources_at(&project, Some(front), execution_base)?;
            return Ok(issue.front.id);
        };
        let revision =
            board::load_issue(pm_dir, &project.key, next).map_err(|e| unreadable(next, e))?;
        let carries = revision
            .front
            .plan
            .as_ref()
            .filter(|plan| plan.state == "approved")
            .and_then(|plan| plan.intake.as_ref())
            .is_some_and(|next| {
                next.revises.as_deref() == Some(id.as_str())
                    && next
                        .outcomes
                        .get(&front.id)
                        .is_some_and(|outcome| outcome.reused)
            });
        if !carries {
            return Err(Error::invalid(
                "plan_evidence_stale",
                format!(
                    "{id} was superseded by {next}; {} is not in that scope",
                    front.id
                ),
            ));
        }
        issue = revision;
    }
}

fn require_intake_dependencies(
    pm_dir: &Path,
    project: &str,
    epic: &str,
    intake: &GoalIntake,
    front: &Front,
) -> Result<()> {
    let mut historical = Vec::new();
    for id in &front.blocked_by {
        let dependency = board::find_issue(pm_dir, id).map_err(|e| unreadable(id, e))?;
        if !cadence_core::issue::is_closed(&dependency.front.status) {
            return Err(Error::invalid(
                "plan_dependency_unready",
                format!("{} cannot start while prerequisite {id} is open", front.id),
            ));
        }
        if intake.outcomes.contains_key(id) {
            intake.require_scope(&dependency.front, &dependency.body)?;
        } else if dependency.project == project && intake.revises.is_some() {
            historical.push(dependency);
        }
    }
    if historical.is_empty() {
        return Ok(());
    }
    // A completed prerequisite may be omitted from a revision, not from a
    // retained ticket's dependency evidence. Look up its last approved scope.
    let mut visited = HashSet::new();
    let mut successor = std::borrow::Cow::Borrowed(epic);
    let mut previous = intake.revises.clone();
    while !historical.is_empty() {
        let Some(id) = previous else {
            break;
        };
        if !visited.insert(id.clone()) {
            return Err(unreadable(&id, "cyclic intake ancestry"));
        }
        let previous_epic =
            board::load_issue(pm_dir, project, &id).map_err(|e| unreadable(&id, e))?;
        let (state, record) = previous_epic
            .front
            .plan
            .and_then(|plan| plan.intake.map(|intake| (plan.state, intake)))
            .filter(|(_, record)| record.superseded_by.as_deref() == Some(successor.as_ref()))
            .ok_or_else(|| unreadable(&id, "broken intake ancestry"))?;
        if state == "approved" {
            for index in (0..historical.len()).rev() {
                let dependency = &historical[index];
                if record.outcomes.contains_key(&dependency.front.id) {
                    record.require_scope(&dependency.front, &dependency.body)?;
                    historical.swap_remove(index);
                }
            }
        }
        successor = std::borrow::Cow::Owned(id);
        previous = record.revises;
    }
    Ok(())
}

/// CAD-339: the master's dispatch gate — [`gate`], and the issue must
/// be a ticket of an **approved** plan. Where [`gate`] lets an issue in
/// no plan through, the master is refused: it dispatches only work the
/// operator approved. Same fail-closed membership rules.
pub fn gate_master(pm_dir: &Path, front: &Front, body: &str) -> Result<()> {
    gate(pm_dir, front, body)?;
    match membership(pm_dir, front)? {
        Some((_, plan)) if plan.state == "approved" => Ok(()),
        _ => Err(Error::invalid(
            "master_outside_plan",
            format!(
                "{} is not a ticket of an approved plan — the master dispatches only \
                 approved plan tickets; propose a plan (`cadence plan propose`) and \
                 wait for the operator's approval",
                front.id
            ),
        )),
    }
}

/// [`gate`] by issue id — for callers holding only an id (the daemon's
/// job dispatch). An id no project holds passes (jobs need not name a
/// tracker issue); one that exists but cannot be read refuses.
pub fn gate_id(pm_dir: &Path, id: &str) -> Result<()> {
    match probe_issue(pm_dir, id)? {
        Some(issue) => gate(pm_dir, &issue.front, &issue.body),
        None => Ok(()),
    }
}

/// CAD-360: a ticket of a plan that is not approved may only sit in
/// `backlog` or be `dropped` — the board never shows unapproved work
/// as ready or in flight. Called by every status write.
pub fn check_status_write(pm_dir: &Path, front: &Front, status: &str) -> Result<()> {
    if matches!(status, "backlog" | "dropped") {
        return Ok(());
    }
    match membership(pm_dir, front)? {
        Some((epic, plan)) if plan.state != "approved" => Err(not_approved(
            &epic,
            &plan.state,
            &format!("{} stays in backlog until then", front.id),
        )),
        _ => Ok(()),
    }
}

/// CAD-360: parent links around plans. A ticket keeps its plan epic as
/// parent unless the plan was rejected, and nothing new is parented to
/// a plan epic — the operator approved a fixed list.
pub fn check_parent_change(pm_dir: &Path, front: &Front, new_parent: Option<&str>) -> Result<()> {
    if let Some(parent) = new_parent {
        if let Some(epic) = probe_issue(pm_dir, parent)? {
            if epic.front.plan.is_some() {
                return Err(Error::invalid(
                    "plan_not_member",
                    format!(
                        "{parent} is a plan — tickets join a plan only through \
                         `cadence plan propose`; propose a new plan for new work"
                    ),
                ));
            }
        }
    }
    if let Some((epic, plan)) = membership(pm_dir, front)? {
        if plan.state != "rejected" {
            return Err(Error::invalid(
                "plan_ticket_locked",
                format!(
                    "{} is a ticket of plan {epic} ({}) — its parent link cannot change \
                     unless the plan is rejected",
                    front.id, plan.state
                ),
            ));
        }
    }
    Ok(())
}

/// Size-weighted progress over `(status, size)` pairs: the weights of
/// done tickets over the weights of every ticket not dropped.
pub fn progress(tickets: &[(&str, Option<&str>)]) -> (u64, u64) {
    tickets
        .iter()
        .filter(|(status, _)| *status != "dropped")
        .fold((0, 0), |(done, total), (status, size)| {
            let w = model::size_weight(*size);
            (done + if *status == "done" { w } else { 0 }, total + w)
        })
}

fn plan_outcome_ids(plan: &model::Plan) -> impl Iterator<Item = &String> {
    plan.tickets.iter().chain(
        plan.intake
            .iter()
            .flat_map(|intake| intake.outcomes.iter())
            .filter_map(|(id, outcome)| outcome.reused.then_some(id)),
    )
}

/// The plan block of an epic's JSON — `Value::Null` when the issue is
/// not a plan. Tickets are the plan's own list and carry their derived status; progress is
/// size-weighted ([`progress`]).
pub fn plan_json(epic: &board::View, views_by_id: &HashMap<String, &board::View>) -> Value {
    let Some(plan) = &epic.issue.front.plan else {
        return Value::Null;
    };
    // The approved list, not whatever is parented today.
    let kids: Vec<&&board::View> = plan_outcome_ids(plan)
        .filter_map(|id| views_by_id.get(id))
        .collect();
    let tickets: Vec<Value> = kids
        .iter()
        .map(|k| {
            let f = &k.issue.front;
            let mut ticket = json!({
                "id": f.id,
                "title": f.title,
                "status": k.status,
                "size": f.size,
                "weight": model::size_weight(f.size.as_deref()),
                "owner": f.owner,
                "blocked_by": f.blocked_by,
                "acceptance": parse::acceptance_items(&k.issue.body).len(),
            });
            if plan
                .intake
                .as_ref()
                .and_then(|intake| intake.outcomes.get(&f.id))
                .is_some_and(|outcome| outcome.reused)
            {
                ticket["reused"] = json!(true);
            }
            ticket
        })
        .collect();
    let pairs: Vec<(&str, Option<&str>)> = kids
        .iter()
        .map(|k| (k.status.as_str(), k.issue.front.size.as_deref()))
        .collect();
    let (done, total) = progress(&pairs);
    let counts: serde_json::Map<String, Value> = model::STATUSES
        .iter()
        .map(|s| {
            let n = kids.iter().filter(|k| k.status == *s).count();
            (s.to_string(), json!(n))
        })
        .collect();
    let mut result = json!({
        "state": plan.state,
        "proposed_by": plan.proposed_by,
        "proposed_at": plan.proposed_at,
        "decided_by": plan.decided_by,
        "decided_at": plan.decided_at,
        "reason": plan.reason,
        "tickets": tickets,
        "progress": {
            "done_weight": done,
            "total_weight": total,
            "ratio": ratio(done, total),
            "counts": counts,
        },
    });
    if let Some(intake) = &plan.intake {
        result["intake"] = json!(intake);
    }
    result
}

fn ratio(done: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64 * 100.0).round() / 100.0
    }
}

/// `plan ls` — one row per issue carrying a plan: state, proposer,
/// decision, the ticket ids and size-weighted progress (CAD-437).
/// Value flags repeat and comma-join and match ANY of their values;
/// different flags AND.
pub fn ls(
    pm: &Pm,
    states: &[String],
    projects: &[String],
    sort: Option<&str>,
    limit: Option<usize>,
    fields: &[String],
) -> Result<Value> {
    for s in states {
        if !model::PLAN_STATES.contains(&s.as_str()) {
            return Err(crate::filter::unknown("state", s, model::PLAN_STATES));
        }
    }
    for p in projects {
        model::check_key(p)?;
        if !crate::issue::project::list(&pm.dir)?
            .iter()
            .any(|pr| &pr.key == p)
        {
            return Err(crate::issue::project::unknown_project(p, &pm.dir));
        }
    }
    let views = board::views(&pm.config.notes_dir(), board::load_all(&pm.dir, None)?);
    let by_id: HashMap<String, &board::View> = views
        .iter()
        .map(|v| (v.issue.front.id.clone(), v))
        .collect();
    let mut rows: Vec<Value> = views
        .iter()
        .filter(|v| v.issue.front.plan.is_some())
        .map(|v| {
            let f = &v.issue.front;
            let p = f.plan.as_ref().unwrap();
            let kids: Vec<&&board::View> =
                plan_outcome_ids(p).filter_map(|id| by_id.get(id)).collect();
            let pairs: Vec<(&str, Option<&str>)> = kids
                .iter()
                .map(|k| (k.status.as_str(), k.issue.front.size.as_deref()))
                .collect();
            let (done, total) = progress(&pairs);
            json!({
                "id": f.id,
                "project": v.issue.project,
                "title": f.title,
                "status": v.status,
                "state": p.state,
                "proposed_by": p.proposed_by,
                "proposed_at": p.proposed_at,
                "decided_by": p.decided_by,
                "decided_at": p.decided_at,
                "reason": p.reason,
                "tickets": p.tickets,
                "progress": {
                    "done_weight": done,
                    "total_weight": total,
                    "ratio": ratio(done, total),
                },
            })
        })
        .filter(|r| crate::filter::any_of(states, r["state"].as_str()))
        .filter(|r| crate::filter::any_of(projects, r["project"].as_str()))
        .collect();
    const PLAN_SORTS: &[(&str, &str)] = &[
        ("id", "id"),
        ("project", "project"),
        ("title", "title"),
        ("status", "status"),
        ("state", "state"),
        ("proposed_by", "proposed_by"),
        ("proposed_at", "proposed_at"),
        ("progress", "progress.ratio"),
    ];
    if let Some(spec) = sort {
        crate::filter::sort_rows(&mut rows, spec, PLAN_SORTS, "id")?;
    }
    crate::filter::apply_limit(&mut rows, limit);
    crate::filter::apply_fields(&mut rows, fields)?;
    Ok(json!({"plans": rows, "count": rows.len()}))
}

/// `plan show <EPIC>` — the epic, its plan state, tickets and progress.
pub fn show(pm: &Pm, id: &str) -> Result<Value> {
    model::check_id(id)?;
    let views = board::views(&pm.config.notes_dir(), board::load_all(&pm.dir, None)?);
    let by_id: HashMap<String, &board::View> = views
        .iter()
        .map(|v| (v.issue.front.id.clone(), v))
        .collect();
    let view = by_id
        .get(id)
        .ok_or_else(|| Error::rejected(format!("Unknown issue '{id}'")))?;
    let Some(plan) = &view.issue.front.plan else {
        return Err(Error::rejected(format!(
            "{id} is not a plan — `cadence plan propose` creates one"
        )));
    };
    let mut result = json!({
        "id": id,
        "project": view.issue.project,
        "title": view.issue.front.title,
        "status": view.status,
        "plan": plan_json(view, &by_id),
    });
    if let Some(intake) = &plan.intake {
        let evidence = (|| -> Result<()> {
            if let Some(next) = &intake.superseded_by {
                return Err(Error::invalid(
                    "plan_superseded",
                    format!("plan {id} was superseded by {next}"),
                ));
            }
            intake.require_goal(&view.issue.body)?;
            for outcome in intake.outcomes.keys() {
                let issue = &by_id
                    .get(outcome)
                    .ok_or_else(|| unreadable(outcome, "outcome issue is missing"))?
                    .issue;
                intake.require_scope(&issue.front, &issue.body)?;
            }
            let project = crate::issue::project::list(&pm.dir)?
                .into_iter()
                .find(|project| project.key == view.issue.project)
                .ok_or_else(|| unreadable(id, "its project is missing"))?;
            intake.require_sources(&project, None)
        })();
        result["intake_evidence"] = match evidence {
            Ok(()) => json!({"state": "current"}),
            Err(error) => {
                json!({"state": "blocked", "code": error.code(), "reason": error.to_string()})
            }
        };
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &str = "---\ntitle: Onboarding\ngoal: A first chat in five minutes\n\
non_goals: [billing]\n---\n\nWhy this plan.\n\n## Wizard\nsize: L\nagent: dev-1\n\n\
The setup wizard.\n\n### Acceptance\n- [ ] wizard runs\n- [ ] chat opens\n\n\
## Docs\ndepends_on: 1, CAD-9\n\n```md\n## not a ticket\n- [ ] not a criterion\n```\n\n\
### Acceptance\n- [ ] README updated\n";

    #[test]
    fn parses_frontmatter_tickets_and_metadata() {
        let doc = parse_plan(PLAN).unwrap();
        assert_eq!(doc.title, "Onboarding");
        assert_eq!(doc.goal, "A first chat in five minutes");
        assert_eq!(doc.non_goals, vec!["billing"]);
        assert_eq!(doc.intro, "Why this plan.");
        assert_eq!(doc.tickets.len(), 2);
        let wizard = &doc.tickets[0];
        assert_eq!(wizard.title, "Wizard");
        assert_eq!(wizard.size.as_deref(), Some("L"));
        assert_eq!(wizard.agent.as_deref(), Some("dev-1"));
        assert_eq!(wizard.description, "The setup wizard.");
        assert_eq!(wizard.acceptance.len(), 2);
        let docs = &doc.tickets[1];
        assert_eq!(
            docs.depends_on,
            vec![Dep::Ticket(0), Dep::Issue("CAD-9".into())]
        );
        assert_eq!(
            docs.acceptance.len(),
            1,
            "fenced checkbox is not a criterion"
        );
        assert!(docs.description.contains("## not a ticket"));
    }

    #[test]
    fn refuses_a_ticket_without_acceptance() {
        let text =
            "---\ntitle: T\ngoal: G\n---\n## One\n### Acceptance\n- [ ] ok\n## Two\nno criteria\n";
        let err = parse_plan(text).unwrap_err().to_string();
        assert!(err.contains("Ticket 2 \"Two\" has no acceptance"), "{err}");
        let stub = "---\ntitle: T\ngoal: G\n---\n## One\n### Acceptance\n- [ ]\n";
        assert!(parse_plan(stub).is_err());
    }

    #[test]
    fn refuses_bad_shapes() {
        for (text, want) in [
            ("no frontmatter\n## A\n", "frontmatter"),
            (
                "---\ntitle: T\ngoal: G\nextra: x\n---\n## A\n",
                "unknown field",
            ),
            ("---\ntitle: T\ngoal: G\n---\nno tickets\n", "no tickets"),
            (
                "---\ntitle: T\ngoal: G\n---\n## A\nsize: XL\n### Acceptance\n- [ ] a\n",
                "size 'XL'",
            ),
            (
                "---\ntitle: T\ngoal: G\n---\n## A\ndepends_on: 1\n### Acceptance\n- [ ] a\n",
                "itself",
            ),
            (
                "---\ntitle: T\ngoal: G\n---\n## A\ndepends_on: 5\n### Acceptance\n- [ ] a\n",
                "tickets 1..1",
            ),
        ] {
            let err = parse_plan(text).unwrap_err().to_string();
            assert!(err.contains(want), "{text:?}: {err}");
        }
    }

    #[test]
    fn progress_is_size_weighted_and_ignores_dropped() {
        // S=1 M=3 L=8, unsized = M.
        assert_eq!(model::size_weight(Some("S")), 1);
        assert_eq!(model::size_weight(Some("M")), 3);
        assert_eq!(model::size_weight(Some("L")), 8);
        assert_eq!(model::size_weight(None), 3);
        let (done, total) = progress(&[
            ("done", Some("L")),
            ("doing", Some("S")),
            ("ready", None),
            ("dropped", Some("L")),
            ("done", Some("S")),
        ]);
        // done: L + S = 9; live: L + S + M + S = 13 (dropped L excluded).
        assert_eq!((done, total), (9, 13));
        assert_eq!(ratio(done, total), 0.69);
        assert_eq!(progress(&[]), (0, 0));
        assert_eq!(ratio(0, 0), 0.0);
    }
}
