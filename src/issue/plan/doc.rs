//! Plan document parsing and model types.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::issue::parse::{self, AcceptanceItem};
use crate::issue::{claim, model};

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
