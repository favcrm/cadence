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
//! frontmatter and [`gate`] refuses to start any of its tickets until
//! that plan is `approved`. Issues outside plans are never gated here.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::model::{self, Front};
use crate::issue::parse::{self, AcceptanceItem};
use crate::issue::{board, claim, write, Pm};

/// A parsed plan file.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanDoc {
    pub title: String,
    pub goal: String,
    pub non_goals: Vec<String>,
    /// Text between the frontmatter and the first ticket.
    pub intro: String,
    pub tickets: Vec<Ticket>,
}

/// One ticket of a plan.
#[derive(Clone, Debug, PartialEq)]
pub struct Ticket {
    pub title: String,
    pub description: String,
    pub acceptance: Vec<AcceptanceItem>,
    pub depends_on: Vec<Dep>,
    pub size: Option<String>,
    pub agent: Option<String>,
}

/// A ticket dependency: another ticket of the same plan (0-based) or an
/// existing issue.
#[derive(Clone, Debug, PartialEq)]
pub enum Dep {
    Ticket(usize),
    Issue(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanMeta {
    title: String,
    goal: String,
    #[serde(default)]
    non_goals: NonGoals,
}

#[derive(Deserialize, Default)]
#[serde(untagged)]
enum NonGoals {
    #[default]
    None,
    One(String),
    Many(Vec<String>),
}

fn one_line(what: &str, value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(Error::rejected(format!("Plan {what} is empty")));
    }
    if value.contains(['\n', '\r']) {
        return Err(Error::rejected(format!("Plan {what} must be one line")));
    }
    Ok(value.to_string())
}

/// Parse a plan file. Every refusal names the ticket it is about.
pub fn parse_plan(text: &str) -> Result<PlanDoc> {
    let (yaml, body) = parse::split_front(text).map_err(|e| {
        Error::rejected(format!(
            "{e} — a plan file starts with frontmatter: title, goal, non_goals"
        ))
    })?;
    let meta: PlanMeta = serde_yaml::from_str(yaml)
        .map_err(|e| Error::rejected(format!("plan frontmatter: {e}")))?;
    let title = one_line("title", &meta.title)?;
    let goal = meta.goal.trim().to_string();
    if goal.is_empty() {
        return Err(Error::rejected("Plan goal is empty"));
    }
    let non_goals = match meta.non_goals {
        NonGoals::None => vec![],
        NonGoals::One(s) => vec![s],
        NonGoals::Many(v) => v,
    }
    .into_iter()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
    .collect();

    // Split the body at level-two headings outside fenced code.
    let mut intro = String::new();
    let mut sections: Vec<(String, String)> = vec![];
    let mut fences = parse::Fences::default();
    for line in body.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        if !fences.is_code(content) {
            if let Some((2, heading)) = parse::heading(content) {
                sections.push((heading.to_string(), String::new()));
                continue;
            }
        }
        match sections.last_mut() {
            Some((_, text)) => text.push_str(line),
            None => intro.push_str(line),
        }
    }
    if sections.is_empty() {
        return Err(Error::rejected(
            "Plan has no tickets — write one `## <ticket title>` section per ticket",
        ));
    }
    let count = sections.len();
    let tickets = sections
        .iter()
        .enumerate()
        .map(|(n, (heading, text))| parse_ticket(n, count, heading, text))
        .collect::<Result<Vec<_>>>()?;
    Ok(PlanDoc {
        title,
        goal,
        non_goals,
        intro: intro.trim().to_string(),
        tickets,
    })
}

fn parse_ticket(n: usize, count: usize, heading: &str, text: &str) -> Result<Ticket> {
    let label = format!("Ticket {} \"{heading}\"", n + 1);
    if heading.is_empty() {
        return Err(Error::rejected(format!("Ticket {} has no title", n + 1)));
    }
    let mut size = None;
    let mut agent = None;
    let mut depends_on = vec![];
    let mut lines = text.split_inclusive('\n').peekable();
    // Metadata: `key: value` lines directly under the heading.
    while let Some(line) = lines.peek() {
        let Some((key, value)) = line.trim().split_once(':') else {
            break;
        };
        let value = value.trim();
        match key.trim() {
            "size" => {
                let s = value.to_ascii_uppercase();
                if !model::SIZES.iter().any(|(k, _)| *k == s) {
                    return Err(Error::rejected(format!(
                        "{label}: size '{value}' — one of S, M, L"
                    )));
                }
                size = Some(s);
            }
            "agent" => {
                claim::check_alias(value, &format!("{label}: agent"))?;
                agent = Some(value.to_string());
            }
            "depends_on" => {
                let list = value.trim_start_matches('[').trim_end_matches(']');
                for token in list.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                    depends_on.push(parse_dep(&label, n, count, token)?);
                }
            }
            _ => break,
        }
        lines.next();
    }
    // The rest: a `### Acceptance` subsection, everything else is the
    // description.
    let mut description = String::new();
    let mut acceptance = String::new();
    let mut in_acceptance = false;
    let mut sections = 0;
    let mut fences = parse::Fences::default();
    for line in lines {
        let content = line.trim_end_matches(['\n', '\r']);
        if !fences.is_code(content) {
            if let Some((level, title)) = parse::heading(content) {
                if level <= 3 {
                    in_acceptance = level == 3 && title.eq_ignore_ascii_case("acceptance");
                    if in_acceptance {
                        sections += 1;
                        continue;
                    }
                }
            }
        }
        if in_acceptance {
            acceptance.push_str(line);
        } else {
            description.push_str(line);
        }
    }
    if sections > 1 {
        return Err(Error::rejected(format!(
            "{label} has more than one `### Acceptance` section"
        )));
    }
    // CAD-298: no ticket without acceptance.
    if acceptance.trim().is_empty() {
        return Err(Error::rejected(format!(
            "{label} has no acceptance criteria — add a `### Acceptance` checklist \
             (`- [ ] criterion`); a plan ticket without acceptance is refused (CAD-298)"
        )));
    }
    let acceptance = parse::parse_acceptance_input(&acceptance)
        .map_err(|e| Error::rejected(format!("{label}: {e}")))?;
    Ok(Ticket {
        title: heading.to_string(),
        description: description.trim().to_string(),
        acceptance,
        depends_on,
        size,
        agent,
    })
}

fn parse_dep(label: &str, n: usize, count: usize, token: &str) -> Result<Dep> {
    let number = token.strip_prefix('#').unwrap_or(token);
    if let Ok(k) = number.parse::<usize>() {
        if k == 0 || k > count {
            return Err(Error::rejected(format!(
                "{label}: depends_on {token} — the plan has tickets 1..{count}"
            )));
        }
        if k - 1 == n {
            return Err(Error::rejected(format!("{label} cannot depend on itself")));
        }
        return Ok(Dep::Ticket(k - 1));
    }
    if model::valid_id(token) {
        return Ok(Dep::Issue(token.to_string()));
    }
    Err(Error::rejected(format!(
        "{label}: depends_on '{token}' — a ticket number (2 or #2) or an issue id"
    )))
}

/// `plan propose` — secret-scan the plan text, parse it, and create
/// the epic and its tickets in one tracker commit. `allow` is the
/// caller's secret allowlist (the daemon passes its own state dir's).
pub fn propose(
    pm: &Pm,
    project_key: &str,
    text: &str,
    allow: &crate::secret::Allowlist,
    actor: &str,
) -> Result<Value> {
    let warnings = crate::secret::guard_with("plan", text, allow)?;
    let doc = parse_plan(text)?;
    let mut out = write::create_plan(pm, project_key, &doc, actor)?;
    if !warnings.is_empty() {
        out["secret_warnings"] = crate::secret::warnings_json(&warnings);
    }
    Ok(out)
}

/// The plan an issue belongs to: its own (the epic) or its parent's.
fn plan_of(pm_dir: &Path, front: &Front) -> Option<(String, model::Plan)> {
    if let Some(plan) = &front.plan {
        return Some((front.id.clone(), plan.clone()));
    }
    let parent = front.parent.as_deref()?;
    // An unreadable parent is lint's problem; it is not a plan here.
    let epic = board::find_issue(pm_dir, parent).ok()?;
    epic.front.plan.map(|plan| (parent.to_string(), plan))
}

/// CAD-360: the dispatch gate. An issue whose parent epic is a plan
/// starts only once that plan is `approved`, and only with acceptance
/// criteria; a plan epic itself is never started (its tickets are).
/// Issues in no plan pass untouched.
pub fn gate(pm_dir: &Path, front: &Front, body: &str) -> Result<()> {
    let Some((epic, plan)) = plan_of(pm_dir, front) else {
        return Ok(());
    };
    if epic == front.id {
        return Err(Error::invalid(
            "plan_epic",
            format!(
                "{epic} is a plan — dispatch its tickets, not the epic \
                 (`cadence plan show {epic}`)"
            ),
        ));
    }
    match plan.state.as_str() {
        "approved" => {}
        "proposed" => {
            return Err(Error::invalid(
                "plan_not_approved",
                format!(
                    "plan {epic} is proposed — approve it with `cadence plan approve {epic}`; \
                     {} cannot start before its plan is approved",
                    front.id
                ),
            ))
        }
        state => {
            return Err(Error::invalid(
                "plan_not_approved",
                format!(
                    "plan {epic} is {state} — {} cannot start; propose a new plan \
                     (`cadence plan propose`) instead",
                    front.id
                ),
            ))
        }
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
    Ok(())
}

/// [`gate`] by issue id — for callers holding only an id (the daemon's
/// job dispatch). An id the tracker does not know passes: jobs are not
/// required to name a tracker issue.
pub fn gate_id(pm_dir: &Path, id: &str) -> Result<()> {
    match board::find_issue(pm_dir, id) {
        Ok(issue) => gate(pm_dir, &issue.front, &issue.body),
        Err(_) => Ok(()),
    }
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

/// The plan block of an epic's JSON — `Value::Null` when the issue is
/// not a plan. Tickets carry their derived status; progress is
/// size-weighted ([`progress`]).
pub fn plan_json(epic: &board::View, views_by_id: &HashMap<String, &board::View>) -> Value {
    let Some(plan) = &epic.issue.front.plan else {
        return Value::Null;
    };
    let kids: Vec<&&board::View> = epic
        .children
        .iter()
        .filter_map(|id| views_by_id.get(id))
        .collect();
    let tickets: Vec<Value> = kids
        .iter()
        .map(|k| {
            let f = &k.issue.front;
            json!({
                "id": f.id,
                "title": f.title,
                "status": k.status,
                "size": f.size,
                "weight": model::size_weight(f.size.as_deref()),
                "owner": f.owner,
                "blocked_by": f.blocked_by,
                "acceptance": parse::acceptance_items(&k.issue.body).len(),
            })
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
    json!({
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
    })
}

fn ratio(done: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64 * 100.0).round() / 100.0
    }
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
    if view.issue.front.plan.is_none() {
        return Err(Error::rejected(format!(
            "{id} is not a plan — `cadence plan propose` creates one"
        )));
    }
    Ok(json!({
        "id": id,
        "project": view.issue.project,
        "title": view.issue.front.title,
        "status": view.status,
        "plan": plan_json(view, &by_id),
    }))
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
