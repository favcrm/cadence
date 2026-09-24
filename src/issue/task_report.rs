//! Task reports (CAD-341) — one generic `cadence.report/2` record a
//! worker files when a job finishes, a question needs an answer or a
//! blocker stops it. A report is a Markdown file in the ticket folder,
//! `<pm>/<project>/<ID>/reports/<UTC-basic>-<agent>.md`, written only
//! through [`crate::issue::write::add_report`]:
//!
//! ```markdown
//! ---
//! schema: cadence.report/2
//! kind: done              # done | question | blocked | answer | verdict
//! task: CAD-341
//! agent: dev-1
//! sha: <40 or 64 hex>     # optional; a verdict's reviewed head
//! pr: https://github.com/o/r/pull/7   # done only, optional
//! constraints: [copied verbatim from the kickoff]
//! context_feedback:
//!   used: [{id: L-12, helpful: true}]
//!   wrong: [{id: L-7, why: renamed flag}]
//!   reread: [src/index.ts]
//! ---
//! ## Expected
//! ## Evidence
//! ## Cause
//! ## Correction
//! ## Lesson
//! ## Next
//! ```
//!
//! The body carries the six reflection headings of the cadence skill,
//! each exactly once. A `question` also carries `options`, `impact` and
//! `state: input-required`; the other kinds carry none of them. An
//! `answer` carries `answers: <question report file name>` and a free
//! body instead of the headings; a question is open until an answer
//! names it (files stay create-only — the question is never edited).
//! A `verdict` (CAD-431) is a reviewer's judgement of one head:
//! `verdict: pass|revise`, the reviewed `sha`, and the findings as a
//! free body. It is filed only through the daemon (`report_verdict`),
//! which checks the caller is the ticket's assigned reviewer and the
//! sha is the head under review — [`prepare`] refuses the kind.
//! With `CADENCE_ALIAS` set, `agent` must be that alias. A new
//! kind is a new [`Kind`] value — the record, writer and readers stay
//! the same. Unknown frontmatter fields are refused, not ignored, so a
//! typo cannot silently drop feedback.
//!
//! The intake verbs of `cadence report` (question/feedback/idea/bug →
//! a backlog issue) are a different thing and live in `report.rs`.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::{board, model, parse, write, Pm};

/// The frontmatter `schema` value; written on every stored report.
pub const SCHEMA: &str = "cadence.report/2";
/// Same cap as an intake report — refused, never truncated.
pub const BODY_MAX: usize = crate::issue::report::BODY_MAX;
/// The reflection headings (cadence skill), in their canonical order.
pub const SECTIONS: [&str; 6] = [
    "Expected",
    "Evidence",
    "Cause",
    "Correction",
    "Lesson",
    "Next",
];
/// A question waits on someone else's input.
const INPUT_REQUIRED: &str = "input-required";
/// The folder under the ticket that holds reports.
pub const DIR: &str = "reports";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Done,
    Question,
    Blocked,
    /// Answers one `question` report on the same ticket (`answers:`).
    Answer,
    /// A reviewer's PASS/REVISE on one head (CAD-431); daemon-filed.
    Verdict,
}

/// A reviewer's judgement of one head (CAD-431).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Revise,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Revise => "revise",
        }
    }
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Done => "done",
            Kind::Question => "question",
            Kind::Blocked => "blocked",
            Kind::Answer => "answer",
            Kind::Verdict => "verdict",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Used {
    pub id: String,
    pub helpful: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wrong {
    pub id: String,
    pub why: String,
}

/// Which supplied context helped, which was wrong, and what the agent
/// still had to re-read — the feedback half of the learning loop.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextFeedback {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub used: Vec<Used>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wrong: Vec<Wrong>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reread: Vec<String>,
}

impl ContextFeedback {
    fn is_empty(&self) -> bool {
        self.used.is_empty() && self.wrong.is_empty() && self.reread.is_empty()
    }
}

/// Report frontmatter. Input may omit `schema`, `kind`, `task` and
/// `agent` (flags or the environment supply them); a stored report
/// always has all four. Field order is the rendered order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Front {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, alias = "author", skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
    #[serde(default, skip_serializing_if = "ContextFeedback::is_empty")]
    pub context_feedback: ContextFeedback,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact: Option<String>,
    /// `answer` only: the file name of the question report it answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<String>,
    /// `done` only: the pull request the work is in (CAD-431) —
    /// `https://github.com/<owner>/<repo>/pull/<n>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
    /// `verdict` only: pass or revise (CAD-431).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
}

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
fn parse_text(text: &str) -> Result<(Front, String)> {
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
fn check_claim(front: &Front, alias: Option<&str>) -> Result<()> {
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
fn strip_controls(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

/// A validated, secret-scanned report that has not been written yet.
pub struct Prepared {
    pub front: Front,
    body: String,
    warnings: Vec<crate::secret::Finding>,
}

impl Prepared {
    /// The ticket the report files on.
    pub fn task(&self) -> &str {
        self.front.task.as_deref().unwrap_or_default()
    }

    /// The report body as it will be stored.
    pub fn body(&self) -> &str {
        &self.body
    }
}

/// Validate and secret-scan one report without writing anything:
/// schema, headings, the caller's identity (`CADENCE_ALIAS`), and for
/// an `answer` that its question exists on the same ticket.
pub fn prepare(pm: &Pm, text: &str, task: Option<&str>, kind: Option<Kind>) -> Result<Prepared> {
    if text.len() > BODY_MAX {
        return Err(Error::rejected(format!(
            "Report exceeds the {} KB cap — trim it",
            BODY_MAX / 1024
        )));
    }
    let text = strip_controls(text);
    let (front, body) = parse_text(&text)?;
    check_claim(&front, std::env::var("CADENCE_ALIAS").ok().as_deref())?;
    let front = validate(front, &body, task, kind, &default_agent())?;
    if front.kind == Some(Kind::Verdict) {
        return Err(Error::rejected(
            "a verdict is filed through the daemon, which checks you are the ticket's \
             assigned reviewer and the sha is the head under review — \
             `cadence report file --task <ID> --kind verdict --file <f>`",
        ));
    }
    checked(pm, front, body, &text)
}

/// The board's answer (CAD-328): an `answer` report on `task` naming
/// the open `question`, filed as `agent` — the author the board derived
/// from the connection. Nothing here reads the process environment or
/// request-supplied identity: the board process's own `CADENCE_ALIAS`
/// is not the browser's. Same validation, question check and secret
/// scan as [`prepare`].
pub fn prepare_answer(
    pm: &Pm,
    task: &str,
    question: &str,
    text: &str,
    agent: &str,
) -> Result<Prepared> {
    if text.len() > BODY_MAX {
        return Err(Error::rejected(format!(
            "Answer exceeds the {} KB cap — trim it",
            BODY_MAX / 1024
        )));
    }
    let body = strip_controls(text);
    let front = Front {
        kind: Some(Kind::Answer),
        task: Some(task.to_string()),
        agent: Some(agent.to_string()),
        answers: Some(question.to_string()),
        ..Front::default()
    };
    let front = validate(front, &body, None, None, agent)?;
    let scan = format!("answers: {question}\n{body}");
    checked(pm, front, body, &scan)
}

/// The checks after validation: an `answer`'s question must exist on
/// the same ticket, and `scan` (the text as filed) must pass the secret
/// guard.
fn checked(pm: &Pm, front: Front, body: String, scan: &str) -> Result<Prepared> {
    let id = front.task.clone().unwrap_or_default();
    let (_, dir) = write::issue_dir(pm, &id)?;
    if let Some(q) = &front.answers {
        let is_question = names(&dir).contains(q)
            && std::fs::read_to_string(dir.join(DIR).join(q))
                .ok()
                .and_then(|t| load(&t, &id).ok())
                .is_some_and(|(f, _)| f.kind == Some(Kind::Question));
        if !is_question {
            return Err(Error::rejected(format!(
                "{id} has no question report '{q}' to answer — `cadence issue show {id}` \
                 lists its reports"
            )));
        }
    }
    let warnings = crate::secret::guard(&format!("{id}: report"), scan)?;
    Ok(Prepared {
        front,
        body,
        warnings,
    })
}

/// Validate and secret-scan one `verdict` for the daemon (CAD-431).
/// `agent` is the caller the daemon derived from the connection: a
/// frontmatter `agent` naming anyone else is refused, never trusted.
/// Nothing is written.
pub fn prepare_verdict(pm: &Pm, text: &str, task: &str, agent: &str) -> Result<Prepared> {
    if text.len() > BODY_MAX {
        return Err(Error::rejected(format!(
            "Verdict exceeds the {} KB cap — trim it",
            BODY_MAX / 1024
        )));
    }
    let text = strip_controls(text);
    let (front, body) = parse_text(&text)?;
    check_claim(&front, Some(agent))?;
    let front = validate(front, &body, Some(task), Some(Kind::Verdict), agent)?;
    write::issue_dir(pm, task)?;
    let warnings = crate::secret::guard(&format!("{task}: verdict"), &text)?;
    Ok(Prepared {
        front,
        body,
        warnings,
    })
}

/// The findings' first non-empty line, for a Needs-you row or a
/// message — at most `max` bytes, cut on a char boundary.
pub fn summary_line(body: &str, max: usize) -> String {
    let line = body
        .lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    if line.len() <= max {
        return line.to_string();
    }
    let mut end = max;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &line[..end])
}

/// Write a prepared report through the tracker writer. Filing
/// byte-identical content again returns the existing file
/// (`duplicate: true`) so a retried `message result` is safe.
pub fn store(pm: &Pm, p: &Prepared, actor: &str) -> Result<Value> {
    let mut out = write::add_report(pm, p.task(), &p.front, &p.body, actor)?;
    if !p.warnings.is_empty() {
        out["secret_warnings"] = crate::secret::warnings_json(&p.warnings);
    }
    Ok(out)
}

/// `cadence report file`: [`prepare`] then [`store`].
pub fn file(
    pm: &Pm,
    text: &str,
    task: Option<&str>,
    kind: Option<Kind>,
    actor: &str,
) -> Result<Value> {
    store(pm, &prepare(pm, text, task, kind)?, actor)
}

/// `20260917T172400Z-dev.md` → `2026-09-17T17:24:00Z`.
fn at_of(name: &str) -> Option<String> {
    let b = name.as_bytes();
    if b.len() < 16 || b[8] != b'T' || b[15] != b'Z' {
        return None;
    }
    let digits = |r: std::ops::Range<usize>| name[r].bytes().all(|c| c.is_ascii_digit());
    if !(digits(0..8) && digits(9..15)) {
        return None;
    }
    Some(format!(
        "{}-{}-{}T{}:{}:{}Z",
        &name[0..4],
        &name[4..6],
        &name[6..8],
        &name[9..11],
        &name[11..13],
        &name[13..15]
    ))
}

/// Parse and re-validate one stored report file. `folder_id` is the
/// ticket the file sits in — a report filed under another task is
/// malformed.
pub fn load(text: &str, folder_id: &str) -> Result<(Front, String)> {
    let (front, body) = parse_text(text)?;
    if front.schema.is_none() || front.kind.is_none() || front.agent.is_none() {
        return Err(Error::rejected("stored report lacks schema, kind or agent"));
    }
    if front.task.as_deref() != Some(folder_id) {
        return Err(Error::rejected(format!(
            "report task {:?} does not match its ticket {folder_id}",
            front.task.as_deref().unwrap_or("")
        )));
    }
    let agent = front.agent.clone().unwrap_or_default();
    let front = validate(front, &body, None, None, &agent)?;
    Ok((front, body))
}

/// Report file names under `<issue dir>/reports/` — real `.md` files
/// only, sorted (oldest first by the UTC prefix).
pub fn names(issue_dir: &Path) -> Vec<String> {
    let dir = issue_dir.join(DIR);
    if !board::is_real_dir(&dir) {
        return vec![];
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".md"))
        .collect();
    out.sort();
    out
}

/// Every report on a ticket, read-only, for `issue show` and the issue
/// detail API. An unreadable or malformed file is listed with its
/// error rather than hidden — `issue lint` refuses it at commit. A
/// question carries `open` (no answer names it yet) and `answered_by`;
/// an answer whose question is not on the ticket is listed as an error.
pub fn list(issue_dir: &Path, id: &str) -> Vec<Value> {
    let mut rows: Vec<Value> = names(issue_dir)
        .into_iter()
        .map(|name| {
            let path = format!("{id}/{DIR}/{name}");
            let at = at_of(&name);
            let text = match std::fs::read_to_string(issue_dir.join(DIR).join(&name)) {
                Ok(t) => t,
                Err(e) => {
                    return json!({"name": name, "path": path, "at": at, "error": e.to_string()})
                }
            };
            match load(&text, id) {
                Ok((f, body)) => json!({
                    "name": name, "path": path, "at": at,
                    "kind": f.kind.map(Kind::as_str), "task": f.task,
                    "agent": f.agent, "session": f.session, "sha": f.sha,
                    "state": f.state, "constraints": f.constraints,
                    "context_feedback": f.context_feedback,
                    "options": f.options, "impact": f.impact,
                    "answers": f.answers, "pr": f.pr,
                    "verdict": f.verdict.map(Verdict::as_str), "body": body,
                }),
                Err(e) => json!({"name": name, "path": path, "at": at, "error": e.to_string()}),
            }
        })
        .collect();
    let questions: Vec<String> = rows
        .iter()
        .filter(|r| r["kind"] == "question")
        .filter_map(|r| r["name"].as_str().map(str::to_string))
        .collect();
    let answers: Vec<(String, String)> = rows
        .iter()
        .filter(|r| r["kind"] == "answer")
        .filter_map(|r| {
            Some((
                r["answers"].as_str()?.to_string(),
                r["name"].as_str()?.to_string(),
            ))
        })
        .collect();
    for r in &mut rows {
        match r["kind"].as_str() {
            Some("question") => {
                let name = r["name"].as_str().unwrap_or_default().to_string();
                let by: Vec<&String> = answers
                    .iter()
                    .filter(|(q, _)| *q == name)
                    .map(|(_, a)| a)
                    .collect();
                r["open"] = json!(by.is_empty());
                r["answered_by"] = json!(by);
            }
            Some("answer") => {
                let q = r["answers"].as_str().unwrap_or_default();
                if !questions.iter().any(|n| n == q) {
                    r["error"] = json!(format!(
                        "answers '{q}', which is not a question report on {id}"
                    ));
                }
            }
            _ => {}
        }
    }
    rows
}

/// The open questions on a ticket — [`list`] rows of kind `question`
/// that no answer names yet (CAD-339: what the report router and the
/// operator's Needs-you read).
pub fn open_questions(issue_dir: &Path, id: &str) -> Vec<Value> {
    list(issue_dir, id)
        .into_iter()
        .filter(|r| r["kind"] == "question" && r["open"] == true && r["error"].is_null())
        .collect()
}

/// Is a `blocked` [`list`] row still the issue's open state? It clears
/// when the issue itself reaches done/dropped or a `done` report newer
/// than it lands — the lane moved. The checkup (CAD-477) escalates
/// only an open block; Needs-you rows only an open one.
pub fn blocked_open(reports: &[Value], blocked: &Value, issue_status: &str) -> bool {
    if blocked["kind"].as_str() != Some("blocked") || matches!(issue_status, "done" | "dropped") {
        return false;
    }
    let at = blocked["at"].as_str().unwrap_or_default();
    !reports
        .iter()
        .any(|r| r["kind"].as_str() == Some("done") && r["at"].as_str().unwrap_or_default() >= at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> String {
        SECTIONS
            .iter()
            .map(|s| format!("## {s}\n\nx\n"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn ok(yaml: &str, task: Option<&str>, kind: Option<Kind>) -> Result<Front> {
        let text = format!("---\n{yaml}---\n\n{}", body());
        let (f, b) = parse_text(&text)?;
        validate(f, &b, task, kind, "dev-1")
    }

    #[test]
    fn done_report_normalises_schema_task_kind_agent() {
        let f = ok(
            "constraints: [keep it small]\ncontext_feedback:\n  used: [{id: L-1, helpful: true}]\n  \
             wrong: [{id: L-2, why: stale}]\n  reread: [src/x.rs]\n",
            Some("CAD-1"),
            Some(Kind::Done),
        )
        .unwrap();
        assert_eq!(f.schema.as_deref(), Some(SCHEMA));
        assert_eq!(f.task.as_deref(), Some("CAD-1"));
        assert_eq!(f.kind, Some(Kind::Done));
        assert_eq!(f.agent.as_deref(), Some("dev-1"));
        assert_eq!(f.context_feedback.used[0].id, "L-1");
        // Rendered frontmatter parses back to the same record.
        let text = parse::render(&f, &body()).unwrap();
        let (again, b) = parse_text(&text).unwrap();
        assert_eq!(validate(again, &b, None, None, "x").unwrap(), f);
    }

    #[test]
    fn question_gets_input_required_and_needs_options_impact() {
        let f = ok(
            "kind: question\ntask: CAD-1\noptions: [a, b]\nimpact: blocks merge\n",
            None,
            None,
        )
        .unwrap();
        assert_eq!(f.state.as_deref(), Some(INPUT_REQUIRED));
        assert!(ok("kind: question\ntask: CAD-1\nimpact: x\n", None, None).is_err());
        assert!(ok("kind: question\ntask: CAD-1\noptions: [a]\n", None, None).is_err());
        assert!(ok(
            "kind: question\ntask: CAD-1\noptions: [a]\nimpact: x\nstate: done\n",
            None,
            None
        )
        .is_err());
        // options/impact/state belong to questions only.
        assert!(ok("kind: done\ntask: CAD-1\noptions: [a]\n", None, None).is_err());
        assert!(ok(
            "kind: blocked\ntask: CAD-1\nstate: input-required\n",
            None,
            None
        )
        .is_err());
    }

    #[test]
    fn malformed_reports_are_refused() {
        for (yaml, task, kind) in [
            ("task: CAD-1\n", None, None),                       // no kind
            ("kind: done\n", None, None),                        // no task
            ("kind: finished\ntask: CAD-1\n", None, None),       // unknown kind
            ("kind: done\ntask: CAD-1\nmood: ok\n", None, None), // unknown field
            ("kind: done\ntask: CAD-1\nsha: abc\n", None, None), // short sha
            ("kind: done\ntask: nope\n", None, None),            // bad id
            (
                "schema: cadence.report/1\nkind: done\ntask: CAD-1\n",
                None,
                None,
            ),
            ("kind: done\ntask: CAD-1\n", Some("CAD-2"), None), // task mismatch
            ("kind: done\ntask: CAD-1\n", None, Some(Kind::Blocked)), // kind mismatch
            ("kind: done\ntask: CAD-1\nagent: 'a b'\n", None, None),
            (
                "kind: done\ntask: CAD-1\ncontext_feedback:\n  wrong: [{id: L-1, why: ''}]\n",
                None,
                None,
            ),
            (
                "kind: done\ntask: CAD-1\ncontext_feedback:\n  used: [{id: L-1}]\n",
                None,
                None,
            ),
        ] {
            assert!(ok(yaml, task, kind).is_err(), "accepted: {yaml}");
        }
    }

    #[test]
    fn body_needs_each_reflection_heading_once() {
        let front = Front {
            kind: Some(Kind::Done),
            task: Some("CAD-1".into()),
            ..Front::default()
        };
        let missing = body().replace("## Lesson", "## Lessons");
        let e = validate(front.clone(), &missing, None, None, "a").unwrap_err();
        assert!(e.to_string().contains("## Lesson"), "{e}");
        let twice = format!("{}\n## Next\n", body());
        assert!(validate(front.clone(), &twice, None, None, "a").is_err());
        // A heading inside a code fence does not count.
        let fenced = body().replace("## Cause", "```\n## Cause\n```");
        assert!(validate(front.clone(), &fenced, None, None, "a").is_err());
        // No frontmatter at all: flags supply task and kind.
        let (f, b) = parse_text(&body()).unwrap();
        assert!(validate(f, &b, Some("CAD-1"), Some(Kind::Blocked), "a").is_ok());
    }

    #[test]
    fn answer_names_a_report_file_and_skips_headings() {
        let front = |answers: Option<&str>, kind: Kind| Front {
            kind: Some(kind),
            task: Some("CAD-1".into()),
            answers: answers.map(str::to_string),
            ..Front::default()
        };
        let f = validate(
            front(Some("20260923T000000Z-dev.md"), Kind::Answer),
            "ship now",
            None,
            None,
            "pm",
        )
        .unwrap();
        assert_eq!(f.kind, Some(Kind::Answer));
        // Required, a bare file name, a body; never on other kinds.
        for (a, k, b) in [
            (None, Kind::Answer, "x"),
            (Some("../CAD-2/reports/q.md"), Kind::Answer, "x"),
            (Some("q.md"), Kind::Answer, "  "),
            (Some("q.md"), Kind::Done, body().as_str()),
        ] {
            assert!(
                validate(front(a, k), b, None, None, "pm").is_err(),
                "{a:?} {k:?}"
            );
        }
    }

    #[test]
    fn claim_must_match_the_calling_alias() {
        let claimed = Front {
            agent: Some("qa-1".into()),
            ..Front::default()
        };
        assert!(check_claim(&claimed, Some("dev-1")).is_err());
        assert!(check_claim(&claimed, Some("qa-1")).is_ok());
        // No alias (operator shell) or no claim: nothing to contradict.
        assert!(check_claim(&claimed, None).is_ok());
        assert!(check_claim(&Front::default(), Some("dev-1")).is_ok());
    }

    #[test]
    fn verdict_needs_judgement_sha_and_findings() {
        let sha = "a".repeat(40);
        let front = |yaml: &str| -> Front {
            let (f, _) = parse_text(&format!("---\n{yaml}---\nx\n")).unwrap();
            f
        };
        let ok = validate(
            front(&format!(
                "kind: verdict\ntask: CAD-1\nverdict: revise\nsha: {sha}\n"
            )),
            "findings",
            None,
            None,
            "qa-1",
        )
        .unwrap();
        assert_eq!(ok.verdict, Some(Verdict::Revise));
        assert_eq!(ok.sha.as_deref(), Some(sha.as_str()));
        for (yaml, body) in [
            (format!("kind: verdict\ntask: CAD-1\nsha: {sha}\n"), "x"), // no verdict
            (
                "kind: verdict\ntask: CAD-1\nverdict: pass\n".to_string(),
                "x",
            ), // no sha
            (
                format!("kind: verdict\ntask: CAD-1\nverdict: pass\nsha: {sha}\n"),
                " ",
            ), // no findings
            (
                format!("kind: verdict\ntask: CAD-1\nverdict: maybe\nsha: {sha}\n"),
                "x",
            ),
            (
                format!("kind: done\ntask: CAD-1\nverdict: pass\nsha: {sha}\n"),
                "x",
            ), // verdict on done
        ] {
            let parsed = parse_text(&format!("---\n{yaml}---\n{body}\n"));
            let refused = match parsed {
                Err(_) => true,
                Ok((f, b)) => validate(f, &b, None, None, "qa-1").is_err(),
            };
            assert!(refused, "accepted: {yaml:?} {body:?}");
        }
    }

    #[test]
    fn pr_belongs_to_done_and_is_a_github_pull_url() {
        assert_eq!(
            parse_pr_url("https://github.com/favcrm/cadence/pull/231").unwrap(),
            ("favcrm/cadence".to_string(), 231)
        );
        for bad in [
            "http://github.com/o/r/pull/1",
            "https://github.com/o/r/pulls/1",
            "https://github.com/o/r/pull/0",
            "https://github.com/o/r/pull/01",
            "https://github.com/o/r/pull/1/files",
            "https://github.com/-o/r/pull/1",
            "https://github.com/o/r;rm/pull/1",
            "https://github.com/o/../pull/1",
            "https://evil.example/o/r/pull/1",
        ] {
            assert!(parse_pr_url(bad).is_err(), "{bad}");
        }
        let pr = "https://github.com/o/r/pull/7";
        assert!(ok(&format!("kind: done\ntask: CAD-1\npr: {pr}\n"), None, None).is_ok());
        assert!(ok(
            &format!("kind: blocked\ntask: CAD-1\npr: {pr}\n"),
            None,
            None
        )
        .is_err());
        assert!(ok("kind: done\ntask: CAD-1\npr: nope\n", None, None).is_err());
    }

    #[test]
    fn a_verdict_is_never_filed_outside_the_daemon() {
        let tmp = tempfile::tempdir().unwrap();
        let pm = Pm::init(&tmp.path().join("pm")).unwrap();
        let sha = "a".repeat(40);
        let text = format!("---\nkind: verdict\ntask: CAD-1\nverdict: pass\nsha: {sha}\n---\nok\n");
        let e = prepare(&pm, &text, None, None).err().expect("refused");
        assert!(e.to_string().contains("through the daemon"), "{e}");
        // The daemon's path refuses an author other than the caller.
        let forged = format!(
            "---\nkind: verdict\ntask: CAD-1\nagent: qa-2\nverdict: pass\nsha: {sha}\n---\nok\n"
        );
        let e = prepare_verdict(&pm, &forged, "CAD-1", "qa-1")
            .err()
            .expect("refused");
        assert!(e.to_string().contains("is not the caller"), "{e}");
    }

    #[test]
    fn summary_line_is_the_first_text_line_capped() {
        assert_eq!(summary_line("\n\n## Findings\nx", 50), "Findings");
        assert_eq!(summary_line("ééé", 3), "é…");
        assert_eq!(summary_line("", 10), "");
    }

    #[test]
    fn at_of_parses_the_filename_prefix() {
        assert_eq!(
            at_of("20260917T172400Z-dev.md").as_deref(),
            Some("2026-09-17T17:24:00Z")
        );
        assert_eq!(at_of("notes.md"), None);
    }
}
