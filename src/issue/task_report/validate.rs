//! Report frontmatter parsing, required-section checks, and claim validation
//! for `task_report`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use crate::error::{Error, Result};
use crate::issue::{model, parse};

use super::{Front, Kind, INPUT_REQUIRED, SCHEMA, SECTIONS};

/// A GitHub pull request named by URL: `(owner/repo, number)`. Only
/// `https://github.com/<owner>/<repo>/pull/<n>` is accepted — the slug
/// and number reach `gh` as separate arguments, so every part is
/// checked against a narrow grammar.
pub fn parse_pr_url(url: &str) -> Result<(String, u64)> {
    let bad = || {
        Error::rejected(format!(
            "'{url}' is not a pull request URL — expected \
             https://github.com/<owner>/<repo>/pull/<number>"
        ))
    };
    let rest = url.strip_prefix("https://github.com/").ok_or_else(bad)?;
    let parts: Vec<&str> = rest.trim_end_matches('/').split('/').collect();
    let [owner, repo, "pull", n] = parts.as_slice() else {
        return Err(bad());
    };
    let name_ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && !s.starts_with(['.', '-'])
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    let number: u64 = n.parse().map_err(|_| bad())?;
    if !name_ok(owner) || !name_ok(repo) || number == 0 || n.starts_with('0') {
        return Err(bad());
    }
    Ok((format!("{owner}/{repo}"), number))
}

/// Split and parse a report file. A file without a `---` fence is all
/// body with empty frontmatter — flags then supply kind and task.
pub(super) fn parse_text(text: &str) -> Result<(Front, String)> {
    let trimmed = text.strip_prefix('\u{feff}').unwrap_or(text);
    if !trimmed.starts_with("---\n") && !trimmed.starts_with("---\r\n") {
        return Ok((Front::default(), trimmed.to_string()));
    }
    let (yaml, body) = parse::split_front(trimmed)?;
    let front = if yaml.trim().is_empty() {
        Front::default()
    } else {
        serde_yaml::from_str(yaml)
            .map_err(|e| Error::rejected(format!("report frontmatter is invalid: {e}")))?
    };
    Ok((front, body.to_string()))
}

/// Comment-author grammar — the agent names the file.
fn valid_agent(a: &str) -> bool {
    !a.is_empty()
        && a.len() <= 64
        && a.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn one_line(field: &str, v: &str) -> Result<()> {
    if v.trim().is_empty() || v.contains('\n') || v.contains('\r') {
        return Err(Error::rejected(format!(
            "report {field} must be one non-empty line"
        )));
    }
    Ok(())
}

/// The `## <Heading>` lines of the body, outside fenced code blocks.
fn headings(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut fenced = false;
    for line in body.lines() {
        let t = line.trim_end();
        if t.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if let Some(h) = t.strip_prefix("## ") {
            out.push(h.trim().to_string());
        }
    }
    out
}

/// Every reflection heading present exactly once.
fn check_sections(body: &str) -> Result<()> {
    let found = headings(body);
    let mut missing = Vec::new();
    for s in SECTIONS {
        match found.iter().filter(|h| h.eq_ignore_ascii_case(s)).count() {
            0 => missing.push(format!("## {s}")),
            1 => {}
            _ => {
                return Err(Error::rejected(format!(
                    "report body repeats '## {s}' — each reflection heading appears once"
                )))
            }
        }
    }
    if !missing.is_empty() {
        return Err(Error::rejected(format!(
            "report body is missing {} — a report carries the six reflection headings \
             (Expected, Evidence, Cause, Correction, Lesson, Next)",
            missing.join(", ")
        )));
    }
    Ok(())
}

/// Validate and normalise one report. `task`/`kind` come from flags
/// and must agree with the frontmatter when both are present;
/// `default_agent` fills a missing `agent`. The result is the exact
/// frontmatter that is stored.
pub fn validate(
    mut front: Front,
    body: &str,
    task: Option<&str>,
    kind: Option<Kind>,
    default_agent: &str,
) -> Result<Front> {
    match (&front.schema, SCHEMA) {
        (Some(s), want) if s != want => {
            return Err(Error::rejected(format!(
                "report schema '{s}' is not supported — expected {SCHEMA}"
            )))
        }
        _ => front.schema = Some(SCHEMA.to_string()),
    }
    front.task = match (front.task.take(), task) {
        (Some(a), Some(b)) if a != b => {
            return Err(Error::rejected(format!(
                "report frontmatter task '{a}' disagrees with --task {b}"
            )))
        }
        (a, b) => a.or_else(|| b.map(str::to_string)),
    };
    let Some(t) = &front.task else {
        return Err(Error::rejected(
            "report needs a task — `task:` in the frontmatter or --task",
        ));
    };
    model::check_id(t)?;
    front.kind = match (front.kind, kind) {
        (Some(a), Some(b)) if a != b => {
            return Err(Error::rejected(format!(
                "report frontmatter kind '{}' disagrees with --kind {}",
                a.as_str(),
                b.as_str()
            )))
        }
        (a, b) => a.or(b),
    };
    let Some(k) = front.kind else {
        return Err(Error::rejected(
            "report needs a kind (done|question|blocked|answer|verdict) — `kind:` or --kind",
        ));
    };
    let agent = front
        .agent
        .take()
        .unwrap_or_else(|| default_agent.to_string());
    if !valid_agent(&agent) {
        return Err(Error::rejected(format!(
            "Bad report agent '{agent}' — letters, digits, '-' or '_'"
        )));
    }
    front.agent = Some(agent);
    if let Some(s) = &front.session {
        one_line("session", s)?;
    }
    if let Some(sha) = front.sha.take() {
        front.sha = Some(crate::store::check_commit_sha(&sha)?);
    }
    for c in &front.constraints {
        if c.trim().is_empty() {
            return Err(Error::rejected("report constraints must not be empty"));
        }
    }
    let fb = &front.context_feedback;
    for u in &fb.used {
        one_line("context_feedback.used id", &u.id)?;
    }
    for w in &fb.wrong {
        one_line("context_feedback.wrong id", &w.id)?;
        if w.why.trim().is_empty() {
            return Err(Error::rejected(format!(
                "report context_feedback.wrong '{}' needs a why",
                w.id
            )));
        }
    }
    for p in &fb.reread {
        one_line("context_feedback.reread path", p)?;
    }
    if k == Kind::Question {
        match front.state.as_deref() {
            None => front.state = Some(INPUT_REQUIRED.to_string()),
            Some(INPUT_REQUIRED) => {}
            Some(s) => {
                return Err(Error::rejected(format!(
                    "a question's state is {INPUT_REQUIRED}, not '{s}'"
                )))
            }
        }
        if front.options.is_empty() || front.options.iter().any(|o| o.trim().is_empty()) {
            return Err(Error::rejected(
                "a question report lists its options (`options: [...]`, none empty)",
            ));
        }
        if front.impact.as_deref().is_none_or(|i| i.trim().is_empty()) {
            return Err(Error::rejected(
                "a question report states its impact (`impact: ...`)",
            ));
        }
    } else if front.state.is_some() || !front.options.is_empty() || front.impact.is_some() {
        return Err(Error::rejected(format!(
            "state/options/impact belong to a question report, not '{}'",
            k.as_str()
        )));
    }
    match (k, front.answers.as_deref()) {
        (Kind::Answer, Some(q)) if valid_report_name(q) => {}
        (Kind::Answer, _) => {
            return Err(Error::rejected(
                "an answer report names the question it answers \
                 (`answers: <question report file name>`)",
            ))
        }
        (_, Some(_)) => {
            return Err(Error::rejected(format!(
                "`answers` belongs to an answer report, not '{}'",
                k.as_str()
            )))
        }
        (_, None) => {}
    }
    match (k, front.pr.as_deref()) {
        (Kind::Done, Some(url)) => {
            parse_pr_url(url)?;
        }
        (_, Some(_)) => {
            return Err(Error::rejected(format!(
                "`pr` belongs to a done report, not '{}'",
                k.as_str()
            )))
        }
        (_, None) => {}
    }
    match (k, front.verdict) {
        (Kind::Verdict, Some(_)) if front.sha.is_some() => {}
        (Kind::Verdict, _) => {
            return Err(Error::rejected(
                "a verdict names its judgement and the head it judged \
                 (`verdict: pass|revise`, `sha: <head>`)",
            ))
        }
        (_, Some(_)) => {
            return Err(Error::rejected(format!(
                "`verdict` belongs to a verdict report, not '{}'",
                k.as_str()
            )))
        }
        (_, None) => {}
    }
    if let Some(risk) = front.risk.as_deref() {
        if k != Kind::Verdict {
            return Err(Error::rejected("`risk` belongs to a verdict report"));
        }
        if risk.is_empty() || risk.len() > 200 || risk.chars().any(char::is_control) {
            return Err(Error::rejected(
                "verdict risk must be one nonempty line of at most 200 bytes",
            ));
        }
    }
    // An answer is a reply and a verdict is findings, not a reflection
    // — each needs a body, not the six headings.
    if matches!(k, Kind::Answer | Kind::Verdict) {
        if body.trim().is_empty() {
            return Err(Error::rejected(if k == Kind::Answer {
                "an answer report needs a body"
            } else {
                "a verdict report needs a body — its findings"
            }));
        }
    } else {
        check_sections(body)?;
    }
    Ok(front)
}

/// A stored report's file name: `[A-Za-z0-9._-]+.md`, no leading dot —
/// never a path.
fn valid_report_name(n: &str) -> bool {
    n.ends_with(".md")
        && !n.starts_with('.')
        && n.len() <= 200
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// With `CADENCE_ALIAS` set the caller is that alias: a frontmatter
/// `agent`/`author` naming anyone else is refused, never trusted.
pub(super) fn check_claim(front: &Front, alias: Option<&str>) -> Result<()> {
    match (alias.filter(|a| !a.is_empty()), front.agent.as_deref()) {
        (Some(me), Some(claimed)) if claimed != me => Err(Error::rejected(format!(
            "report agent '{claimed}' is not the caller '{me}' (CADENCE_ALIAS) — \
             a report is filed by its own author"
        ))),
        _ => Ok(()),
    }
}

/// Who files by default: the pane's alias, else `operator`.
pub fn default_agent() -> String {
    std::env::var("CADENCE_ALIAS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "operator".to_string())
}

/// C0/C1/DEL controls never reach the tracker (`\n`/`\t` survive).
pub(super) fn strip_controls(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}
