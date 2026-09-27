//! CAD-139: an idea ticket can trigger one research turn and one plan
//! turn, then stops. The operator's decision is a recorded object.
//! Nothing in this module opens a pull request, writes repository
//! code, creates child tickets, or messages a developer. Children
//! exist only after `decide` approves a plan that already sits at
//! the gate.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::model::Front;
use crate::issue::{board, time, write, Pm};

/// No operator decision for this long after the plan is ready.
pub const STALE_AFTER_SECS: i64 = 14 * 86_400;
const STATE_FILE: &str = "idea-pipeline.json";
const PROMPT_CHARS: usize = 1_200;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub issue: String,
    pub project: String,
    pub state: String,
    #[serde(default)]
    pub event_emitted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_message: Option<String>,
    /// Unix seconds when the research turn was queued. Counts toward
    /// the daily cap. Absent when no turn was spent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_ready_at: Option<i64>,
    #[serde(default)]
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommendation: Option<String>,
    #[serde(default)]
    pub tickets: Vec<Proposed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<Decision>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Proposed {
    pub title: String,
    pub acceptance: String,
}

/// The operator's decision. This is the record — not a chat line.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Decision {
    pub id: String,
    pub issue: String,
    pub project: String,
    pub action: String,
    pub source: String,
    pub recorded_via: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub park_until: Option<String>,
    #[serde(default)]
    pub children: Vec<String>,
    pub at: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    records: BTreeMap<String, Record>,
}

pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(STATE_FILE)
}

pub fn load(state_dir: &Path) -> Result<BTreeMap<String, Record>> {
    let file = path(state_dir);
    if !file.exists() {
        return Ok(BTreeMap::new());
    }
    let text = fs::read_to_string(&file)
        .map_err(|e| Error::internal(format!("read {}: {e}", file.display())))?;
    let parsed: File = serde_json::from_str(&text)
        .map_err(|e| Error::internal(format!("idea pipeline state: {e}")))?;
    Ok(parsed.records)
}

pub fn save(state_dir: &Path, records: &BTreeMap<String, Record>) -> Result<()> {
    let file = path(state_dir);
    let text = serde_json::to_string_pretty(&File {
        records: records.clone(),
    })
    .map_err(|e| Error::internal(format!("idea pipeline state: {e}")))?;
    let tmp = file.with_extension("json.tmp");
    fs::write(&tmp, text).map_err(|e| Error::internal(format!("write {}: {e}", tmp.display())))?;
    fs::rename(&tmp, &file)
        .map_err(|e| Error::internal(format!("rename {}: {e}", file.display())))?;
    Ok(())
}

pub fn is_idea(front: &Front) -> bool {
    front.kind.as_deref() == Some("idea") || front.tags.iter().any(|t| t == "idea")
}

pub fn research_message_id(issue: &str) -> String {
    format!("idea-r-{}", issue.to_ascii_lowercase())
}

pub fn plan_message_id(issue: &str) -> String {
    format!("idea-p-{}", issue.to_ascii_lowercase())
}

pub fn utc_day(epoch: i64) -> i64 {
    epoch.div_euclid(86_400)
}

/// Civil date in UTC. `0` is `1970-01-01`.
pub fn ymd(epoch: i64) -> String {
    let z = epoch.div_euclid(86_400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn cap_reached(records: &BTreeMap<String, Record>, now: i64, max: u32) -> bool {
    let day = utc_day(now);
    let n = records
        .values()
        .filter(|r| r.research_at.is_some_and(|t| utc_day(t) == day))
        .count();
    n >= max as usize
}

pub fn stale_due(ready_at: i64, now: i64) -> bool {
    now.saturating_sub(ready_at) >= STALE_AFTER_SECS
}

#[derive(Clone, Debug)]
pub struct Roles {
    pub researcher: String,
    pub architect: String,
}

/// `team.yaml` `roles.researcher.alias` and `roles.architect.alias`.
/// Missing either role means the pipeline cannot spend a turn.
pub fn team_roles(pm_dir: &Path, project: &str) -> Option<Roles> {
    let text = fs::read_to_string(pm_dir.join(project).join("team.yaml")).ok()?;
    let y: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
    let researcher = y["roles"]["researcher"]["alias"]
        .as_str()
        .filter(|s| !s.is_empty())?
        .to_string();
    let architect = y["roles"]["architect"]["alias"]
        .as_str()
        .filter(|s| !s.is_empty())?
        .to_string();
    Some(Roles {
        researcher,
        architect,
    })
}

pub fn one_line(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut space = false;
    for ch in text.chars() {
        let ch = if ch.is_control() { ' ' } else { ch };
        if ch == ' ' {
            if space || out.is_empty() {
                continue;
            }
            space = true;
        } else {
            space = false;
        }
        out.push(ch);
        if out.chars().count() >= max_chars {
            break;
        }
    }
    out.trim().to_string()
}

pub fn research_prompt(issue: &str, title: &str, body: &str) -> String {
    format!(
        "ROLE:researcher Issue {issue}. Research this idea and reply with one note, no code. \
         Cover what already exists in this repo, prior art outside it, feasibility, rough cost \
         and operational burden, and the main risk. Title: {title}. Body: {body}",
        title = one_line(title, 200),
        body = one_line(body, PROMPT_CHARS),
    )
}

pub fn plan_prompt(issue: &str, title: &str, body: &str, research: &str) -> String {
    format!(
        "ROLE:architect Issue {issue}. Write a plan with at least three options including \
         do-nothing and the smallest useful thing, a recommendation, acceptance checks a \
         reviewer could run, and a proposed ticket breakdown. No code, no pull request, and \
         do not dispatch a developer. Use a single line with the fields options, \
         recommendation, acceptance, and tickets. Separate options with |. Separate tickets \
         with || and use a title, then an em dash, then the acceptance. Title: {title}. \
         Idea: {body}. Research: {research}",
        title = one_line(title, 200),
        body = one_line(body, PROMPT_CHARS),
        research = one_line(research, PROMPT_CHARS),
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedPlan {
    pub recommendation: String,
    pub tickets: Vec<Proposed>,
}

/// The architect's reply. A template echo (`<third>`, `<title>`) is
/// not a plan. `do-nothing` and a smallest-useful option are required.
pub fn parse_plan(text: &str) -> std::result::Result<ParsedPlan, String> {
    let lower = text.to_ascii_lowercase();
    let options = field(&lower, text, "options:", &["recommendation:"])
        .ok_or_else(|| "plan is missing the options field".to_string())?;
    let recommendation = field(&lower, text, "recommendation:", &["acceptance:"])
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "plan is missing a recommendation".to_string())?;
    let acceptance = field(&lower, text, "acceptance:", &["tickets:"])
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "plan is missing acceptance checks".to_string())?;
    let tickets_raw = field(&lower, text, "tickets:", &[])
        .ok_or_else(|| "plan is missing the tickets field".to_string())?;
    let opts: Vec<String> = options
        .split('|')
        .map(normalize_option)
        .filter(|s| !s.is_empty())
        .collect();
    if opts.len() < 3 {
        return Err("plan needs at least three options".into());
    }
    if opts.iter().any(|o| o.contains('<')) {
        return Err("plan options still contain a template placeholder".into());
    }
    if !opts.iter().any(|o| o == "do-nothing" || o == "donothing") {
        return Err("plan options must include do-nothing".into());
    }
    if !opts.iter().any(|o| o.contains("smallest")) {
        return Err("plan options must include the smallest useful thing".into());
    }
    let mut tickets = Vec::new();
    for part in tickets_raw.split("||") {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (title, acc) = part
            .split_once(" — ")
            .or_else(|| part.split_once(" -- "))
            .ok_or_else(|| format!("ticket {part:?} needs a title and an acceptance"))?;
        let title = title.trim();
        let acc = acc.trim();
        if title.is_empty() || acc.is_empty() || title.contains('<') || acc.contains('<') {
            return Err("a proposed ticket still contains a placeholder".into());
        }
        tickets.push(Proposed {
            title: one_line(title, 120),
            acceptance: one_line(acc, 400),
        });
    }
    if tickets.is_empty() {
        return Err("plan lists no proposed tickets".into());
    }
    let _ = acceptance;
    Ok(ParsedPlan {
        recommendation: one_line(recommendation, 200),
        tickets,
    })
}

fn field<'a>(lower: &str, raw: &'a str, key: &str, next: &[&str]) -> Option<&'a str> {
    let start = lower.find(key)? + key.len();
    let rest = &lower[start..];
    let end = next
        .iter()
        .filter_map(|n| rest.find(n))
        .min()
        .unwrap_or(rest.len());
    Some(raw[start..start + end].trim())
}

fn normalize_option(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.trim().chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Open issues whose title is the same, contains the other (when the
/// shorter is at least 12 characters), or shares most of its words.
pub fn near_duplicate(title: &str, open: &[(&str, &str)]) -> Option<String> {
    let mine = normalize_title(title);
    if mine.is_empty() {
        return None;
    }
    let mine_tokens = tokens(&mine);
    for (id, other) in open {
        let theirs = normalize_title(other);
        if theirs.is_empty() {
            continue;
        }
        if mine == theirs {
            return Some((*id).to_string());
        }
        let (short, long) = if mine.len() <= theirs.len() {
            (mine.as_str(), theirs.as_str())
        } else {
            (theirs.as_str(), mine.as_str())
        };
        if short.len() >= 12 && long.contains(short) {
            return Some((*id).to_string());
        }
        let other_tokens = tokens(&theirs);
        if jaccard(&mine_tokens, &other_tokens) >= 0.8 {
            return Some((*id).to_string());
        }
    }
    None
}

fn normalize_title(title: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            space = false;
        } else if !space && !out.is_empty() {
            out.push(' ');
            space = true;
        }
    }
    out.trim().to_string()
}

fn tokens(title: &str) -> Vec<String> {
    title
        .split_whitespace()
        .filter(|w| w.len() >= 3)
        .map(str::to_string)
        .collect()
}

fn jaccard(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.iter().filter(|t| b.contains(t)).count();
    let mut union: Vec<&String> = a.iter().collect();
    for t in b {
        if !union.contains(&t) {
            union.push(t);
        }
    }
    inter as f64 / union.len() as f64
}

pub fn comment_once(pm: &Pm, id: &str, author: &str, kind: &str, body: &str) -> Result<bool> {
    let issue = board::find_issue(&pm.dir, id)?;
    if issue
        .comments
        .iter()
        .any(|c| c.front.kind.as_deref() == Some(kind) && c.front.author == author)
    {
        return Ok(false);
    }
    write::add_comment(pm, id, body, Some(author), Some(kind), None, "daemon")?;
    Ok(true)
}

pub fn set_status_tags(
    pm: &Pm,
    id: &str,
    status: &str,
    extra_tag: Option<&str>,
    drop_tags: &[&str],
) -> Result<()> {
    let (project, dir) = write::issue_dir(pm, id)?;
    let (front, _) = write::load_front(&dir)?;
    let mut tags = front.tags;
    for drop in drop_tags {
        tags.retain(|t| t != drop);
    }
    if let Some(tag) = extra_tag {
        if !tags.iter().any(|t| t == tag) {
            tags.push(tag.to_string());
        }
    }
    let tags = write::check_tags(&project, &tags)?;
    let pairs = vec![
        format!("status={status}"),
        format!("tags={}", tags.join(",")),
    ];
    write::set_fields(pm, &[id.to_string()], &pairs, "daemon")?;
    Ok(())
}

fn valid_park_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let (y, m, d) = (&date[0..4], &date[5..7], &date[8..10]);
    let (Ok(y), Ok(m), Ok(d)) = (y.parse::<u16>(), m.parse::<u8>(), d.parse::<u8>()) else {
        return false;
    };
    y >= 1970 && (1..=12).contains(&m) && (1..=31).contains(&d)
}

/// Record the operator's decision and, on approve, create exactly the
/// proposed backlog children. A second approve or reject returns the
/// existing object. A park can still be approved or rejected later.
pub fn decide(
    pm: &Pm,
    records: &mut BTreeMap<String, Record>,
    issue: &str,
    action: &str,
    reason: Option<&str>,
    park_until: Option<&str>,
) -> Result<Value> {
    let (project, proposed, prior) = {
        let rec = records.get(issue).ok_or_else(|| {
            Error::rejected(format!(
                "{issue} is not in the idea pipeline — it has no research record"
            ))
        })?;
        if matches!(rec.state.as_str(), "approved" | "rejected") {
            return Err(Error::rejected(format!(
                "{issue} already has an idea decision ({})",
                rec.state
            )));
        }
        if !matches!(rec.state.as_str(), "plan_ready" | "parked") {
            return Err(Error::rejected(format!(
                "{issue} is not at the operator gate (state {})",
                rec.state
            )));
        }
        if let Some(existing) = &rec.decision {
            let terminal = existing.action != "park";
            if existing.action == action || (terminal && matches!(action, "approve" | "reject")) {
                return Err(Error::rejected(format!(
                    "{issue} already has an idea decision ({})",
                    existing.action
                )));
            }
        }
        (
            rec.project.clone(),
            rec.tickets.clone(),
            rec.decision.clone(),
        )
    };
    let reason = reason.map(str::trim).filter(|s| !s.is_empty());
    match action {
        "approve" => {}
        "reject" => {
            let Some(reason) = &reason else {
                return Err(Error::rejected(
                    "a rejection needs a reason — the decision object records it",
                ));
            };
            if reason.len() > 2_000 {
                return Err(Error::rejected("rejection reason exceeds 2000 characters"));
            }
        }
        "park" => {
            let Some(date) = park_until.map(str::trim).filter(|s| !s.is_empty()) else {
                return Err(Error::rejected("park needs park_until as YYYY-MM-DD"));
            };
            if !valid_park_date(date) {
                return Err(Error::rejected("park_until must be a UTC date YYYY-MM-DD"));
            }
        }
        _ => {
            return Err(Error::rejected(
                "idea decision action must be approve, reject, or park",
            ))
        }
    }
    let mut children = prior
        .as_ref()
        .map(|d| d.children.clone())
        .unwrap_or_default();
    if action == "approve" {
        children = create_children(pm, issue, &project, &proposed, &children)?;
        set_status_tags(
            pm,
            issue,
            "done",
            Some("planned"),
            &["plan-ready", "parked", "idea-stale"],
        )?;
    } else if action == "reject" {
        set_status_tags(
            pm,
            issue,
            "dropped",
            None,
            &["plan-ready", "parked", "idea-stale"],
        )?;
    } else {
        set_status_tags(pm, issue, "review", Some("parked"), &["plan-ready"])?;
    }
    let decision = Decision {
        id: format!("idea-{}-{action}", issue.to_ascii_lowercase()),
        issue: issue.to_string(),
        project,
        action: action.to_string(),
        source: "operator".to_string(),
        recorded_via: "operator_connection".to_string(),
        reason: reason.map(str::to_string),
        park_until: park_until
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        children,
        at: time::iso(time::now_epoch()),
    };
    let rec = records.get_mut(issue).expect("record checked above");
    rec.state = match action {
        "approve" => "approved",
        "reject" => "rejected",
        _ => "parked",
    }
    .to_string();
    rec.decision = Some(decision.clone());
    Ok(json!({
        "duplicate": false,
        "decision": decision,
    }))
}

fn create_children(
    pm: &Pm,
    idea: &str,
    project: &str,
    proposed: &[Proposed],
    already: &[String],
) -> Result<Vec<String>> {
    let mut ids: Vec<String> = already.to_vec();
    let existing = board::load_all(&pm.dir, Some(project))?;
    for ticket in proposed {
        if existing
            .iter()
            .any(|iss| iss.front.parent.as_deref() == Some(idea) && iss.front.title == ticket.title)
            || ids.iter().any(|id| {
                existing
                    .iter()
                    .any(|iss| iss.front.id == *id && iss.front.title == ticket.title)
            })
        {
            if let Some(found) = existing.iter().find(|iss| {
                iss.front.parent.as_deref() == Some(idea) && iss.front.title == ticket.title
            }) {
                if !ids.contains(&found.front.id) {
                    ids.push(found.front.id.clone());
                }
            }
            continue;
        }
        let created = write::new_issue(
            pm,
            &pm.dir,
            Some(project),
            &ticket.title,
            Some("P3"),
            Some(idea),
            &[idea.to_string()],
            None,
            None,
            &[],
            None,
            "operator",
        )?;
        let id = created["id"]
            .as_str()
            .ok_or_else(|| Error::internal("new issue returned no id"))?
            .to_string();
        write::add_comment(
            pm,
            &id,
            &format!("Acceptance: {}", ticket.acceptance),
            Some("operator"),
            Some("acceptance"),
            None,
            "operator",
        )?;
        ids.push(id);
    }
    Ok(ids)
}

pub fn open_issues_for_dedupe(pm_dir: &Path, skip: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for issue in board::load_all(pm_dir, None)? {
        if issue.front.id == skip {
            continue;
        }
        if matches!(issue.front.status.as_str(), "done" | "dropped") {
            continue;
        }
        out.push((issue.front.id, issue.front.title));
    }
    Ok(out)
}

pub fn link_duplicate(pm: &Pm, id: &str, target: &str) -> Result<()> {
    write::link(pm, id, "duplicate_of", target, false, None, "daemon", None)?;
    Ok(())
}

/// Overview's one-line note for a plan that is waiting.
pub fn decision_note(recommendation: Option<&str>, title: &str) -> String {
    one_line(recommendation.unwrap_or(title), 160)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::project;

    fn plan_text() -> &'static str {
        "options: do-nothing | smallest-useful | full theme recommendation: smallest-useful \
         acceptance: the gate holds tickets: Ship the toggle — a setting exists || Document it — the help page names it"
    }

    #[test]
    fn parse_plan_requires_three_options_and_concrete_tickets() {
        let parsed = parse_plan(plan_text()).unwrap();
        assert_eq!(parsed.recommendation, "smallest-useful");
        assert_eq!(parsed.tickets.len(), 2);
        assert_eq!(parsed.tickets[0].title, "Ship the toggle");
        assert!(parse_plan(
            "options: do-nothing | smallest-useful recommendation: x acceptance: y tickets: T — a"
        )
        .is_err());
        assert!(parse_plan(
            "options: do-nothing | smallest-useful | <third> recommendation: x acceptance: y tickets: <title> — <acceptance>"
        )
        .is_err());
        assert!(parse_plan(
            "options: later | bigger | full recommendation: full acceptance: y tickets: T — a"
        )
        .is_err());
    }

    #[test]
    fn near_duplicate_matches_normalized_titles_and_not_short_words() {
        let open = [("D-1", "Dark Mode for the Board!")];
        assert_eq!(
            near_duplicate("dark mode for the board", &open),
            Some("D-1".into())
        );
        assert_eq!(near_duplicate("widget export", &[("D-2", "idea")]), None);
    }

    #[test]
    fn cap_counts_only_research_starts_today() {
        let mut records = BTreeMap::new();
        let now = 1_800_000_000;
        records.insert(
            "D-1".into(),
            Record {
                issue: "D-1".into(),
                project: "demo".into(),
                state: "researching".into(),
                event_emitted: true,
                research_message: None,
                plan_message: None,
                research_at: Some(now),
                plan_ready_at: None,
                stale: false,
                recommendation: None,
                tickets: vec![],
                duplicate_of: None,
                research_note: None,
                decision: None,
            },
        );
        assert!(!cap_reached(&records, now, 3));
        assert!(cap_reached(&records, now, 1));
        assert!(!cap_reached(&records, now + 90_000, 1));
    }

    #[test]
    fn stale_waits_fourteen_days() {
        let ready = 1_700_000_000;
        assert!(!stale_due(ready, ready + STALE_AFTER_SECS - 1));
        assert!(stale_due(ready, ready + STALE_AFTER_SECS));
    }

    #[test]
    fn ymd_epoch_is_unix_date() {
        assert_eq!(ymd(0), "1970-01-01");
    }

    #[test]
    fn switch_default_is_off() {
        let policy = project::IntakePolicy::default();
        assert!(!policy.auto_research);
        assert_eq!(policy.max_per_day, 3);
    }
}
