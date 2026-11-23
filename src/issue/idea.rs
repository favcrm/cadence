//! CAD-139: an idea ticket can trigger one research turn and one plan
//! turn, then stops. The operator's decision is a recorded object.
//! Nothing in this module opens a pull request, writes repository
//! code, creates child tickets, or messages a developer. Children
//! exist only after `decide` approves a plan that already sits at
//! the gate.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::issue::model::Front;
use crate::issue::{board, write, Pm};

mod decision;
mod parse;

pub use decision::decide;
pub use parse::{near_duplicate, one_line, parse_plan, plan_prompt, research_prompt, ParsedPlan};

/// No operator decision for this long after the plan is ready.
pub const STALE_AFTER_SECS: i64 = 14 * 86_400;
const STATE_FILE: &str = "idea-pipeline.json";
pub(super) const PROMPT_CHARS: usize = 1_200;

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
/// `actor` (CAD-140) names who decided — the board's `request_actor`
/// (a proven tailnet login, `operator (ui)` on loopback) or
/// `operator` for direct calls. `source` stays the authority class.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Decision {
    pub id: String,
    pub issue: String,
    pub project: String,
    pub action: String,
    pub source: String,
    pub recorded_via: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
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
    done_reason: Option<&str>,
) -> Result<()> {
    set_status_tags_if_rev(pm, id, status, extra_tag, drop_tags, done_reason, None)
}

/// [`set_status_tags`] bound to the revision shown (CAD-140): the
/// revision check runs inside the tracker lock at the write, so a
/// stale operator decision refuses instead of landing.
pub fn set_status_tags_if_rev(
    pm: &Pm,
    id: &str,
    status: &str,
    extra_tag: Option<&str>,
    drop_tags: &[&str],
    done_reason: Option<&str>,
    if_rev: Option<&str>,
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
    let out =
        write::set_fields_if_rev(pm, &[id.to_string()], &pairs, "daemon", done_reason, if_rev)?;
    if out.get("conflict").is_some() {
        return Err(Error::invalid(
            "stale_view",
            format!("{id} changed since it was shown — re-read it before deciding"),
        ));
    }
    Ok(())
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
