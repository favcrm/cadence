//! The Overview slice — `GET /api/overview` and `cadence overview`.
//!
//! One derived answer to "what is waiting on a human or the PM right
//! now, and with which command" plus "is what we merged actually
//! running". Nothing is stored: agents and approvals come from daemon
//! RPCs, review/blocked/project rows from the tracker, PR and CI state
//! from `gh` behind a 60 s cache in the state dir, deploy drift from
//! git walks bounded through `proc::run_bounded`. Every source degrades
//! — an unreachable daemon, a missing tracker, or a failing `gh`
//! narrows the screen instead of failing it.
//!
//! The sources run concurrently and every external probe is bounded
//! (CAD-249): daemon RPCs by [`PROBE_TIMEOUT`] under a pass budget, `gh`
//! by [`GH_TIMEOUT`] plus a caller wait past which the board serves the
//! cache. A probe that misses its bound becomes a `degraded` note.
//! Rows naming the same subject merge into one row carrying `causes`
//! (CAD-252).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(test)]
use std::sync::mpsc::{channel, Receiver};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::adapter::registry;
use crate::client;
use crate::inbox;
use crate::issue::line_times::LineTimes;
use crate::issue::{self, board, claim, project, report};

mod commands;
mod github;
mod github_cache;
mod main_ci;
mod monitoring;
mod needs_merge;
pub use commands::{
    cmd_agent_answer, cmd_agent_attach, cmd_agent_respond, cmd_agent_resume, cmd_agent_show,
    cmd_agent_unfence, cmd_delivery_decline, cmd_delivery_merge, cmd_delivery_sync, cmd_inbox,
    cmd_issue_set_ready, cmd_issue_show, CMD_DELIVERY_SYNC, CMD_ISSUE_SYNC, CMD_RESTART_WHEN_IDLE,
    CMD_UPGRADE_LATEST_MAIN,
};
use github::{build_repo_match, compute_drift, gh_repo, git_text, GH_TIMEOUT};
#[cfg(test)]
use github_cache::github_bounded_notify;
#[cfg(test)]
use github_cache::write_cache;
use github_cache::{cache_file, github, github_bounded, read_cache};
use main_ci::main_ci_view;
#[allow(unused_imports)] // Preserve the existing crate-visible type path.
pub(crate) use main_ci::MainCiAlerts;
#[cfg(test)]
use main_ci::{classify_main_ci, main_ci_alerts, CiState, ShaCi};
pub use monitoring::monitoring;
use needs_merge::{merge_by_subject, sort_needs};

/// Git identity baked in by build.rs — `unknown` when git or a repo
/// was absent at build time, which every consumer reads as "cannot
/// tell", never as zero.
pub const BUILD_COMMIT: &str = env!("CADENCE_BUILD_COMMIT");
/// The full build id `cadence --version` prints (`0.1.0+<sha>`, the
/// same concat main.rs gives clap). The board announces it in the
/// stream's `hello` frame and answers it on `/api/version` so an older
/// tab bundle knows it is stale (CAD-573).
pub const BUILD_ID: &str = concat!(env!("CARGO_PKG_VERSION"), "+", env!("CADENCE_BUILD_COMMIT"));
pub const BUILD_TIME: &str = env!("CADENCE_BUILD_TIME");
pub const BUILD_REMOTE: &str = env!("CADENCE_BUILD_REMOTE");
pub const BUILD_ROOT: &str = env!("CADENCE_BUILD_ROOT");

/// `gh` results are cached this long in the state dir — the board and
/// the CLI share one cache, and GitHub is the only source allowed to
/// be a network call.
const GH_CACHE_SECS: i64 = 60;
const GH_CACHE_MAX_SECS: i64 = 3600;

/// Only the overview's display cache. Delivery observations have their
/// own bounded scheduler and must not inherit this setting (CAD-627).
fn gh_cache_secs(raw: Option<&str>) -> i64 {
    raw.and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|n| (GH_CACHE_SECS..=GH_CACHE_MAX_SECS).contains(n))
        .unwrap_or(GH_CACHE_SECS)
}
/// Read bound on each daemon RPC the overview makes (CAD-249).
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// The per-agent probe pass starts no probe past this budget; agents
/// it never reached become one `degraded` note.
const PROBE_BUDGET: Duration = Duration::from_secs(4);
/// Concurrent per-agent probes.
const PROBE_WORKERS: usize = 8;
/// The board waits this long on a gh refresh before serving the last
/// cache; the refresh keeps running and lands for the next request.
const GH_BOARD_WAIT: Duration = Duration::from_millis(1500);

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// `YYYY-MM-DDTHH:MM:SSZ` → epoch — the shape `issue::time::iso`
/// writes and the shape `gh` returns for `updatedAt`.
fn parse_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[19] != b'Z' {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> { s.get(from..to)?.parse().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    // Days-from-civil (Howard Hinnant).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + h * 3600 + mi * 60 + sec)
}

/// `(#123)` at the end of a squash-merge subject → the PR number.
fn pr_number(subject: &str) -> Option<u64> {
    let tail = subject.trim_end();
    let inner = tail.strip_suffix(')')?.rsplit("(#").next()?;
    let n: u64 = inner.parse().ok()?;
    Some(n)
}

/// Every rollup entry other than the `qa-verdict` gate must be a pass:
/// CheckRun → COMPLETED + SUCCESS/SKIPPED/NEUTRAL, StatusContext →
/// SUCCESS. An empty rollup means no CI configured — treated green;
/// the human gate is the verdict.
fn checks_green(rollup: &[Value]) -> bool {
    rollup.iter().all(|e| {
        let name = e["context"].as_str().or(e["name"].as_str()).unwrap_or("");
        if name.eq_ignore_ascii_case("qa-verdict") {
            return true;
        }
        if e["__typename"].as_str() == Some("CheckRun") || e["conclusion"].is_string() {
            let done = e["status"].as_str() == Some("COMPLETED") || e["status"].is_null();
            let ok = matches!(
                e["conclusion"].as_str(),
                Some("SUCCESS" | "SKIPPED" | "NEUTRAL")
            );
            return done && ok;
        }
        e["state"].as_str() == Some("SUCCESS")
    })
}

/// The `qa-verdict` commit status on the rollup — "SUCCESS", "FAILURE",
/// "PENDING", or `None` when nobody posted one.
fn verdict_state(rollup: &[Value]) -> Option<String> {
    for e in rollup {
        let name = e["context"].as_str().or(e["name"].as_str()).unwrap_or("");
        if name.eq_ignore_ascii_case("qa-verdict") {
            return e["state"]
                .as_str()
                .or(e["conclusion"].as_str())
                .map(str::to_string);
        }
    }
    None
}

/// A needs-me row before the subject merge and the urgency sort.
struct Item {
    rank: u8,
    age: i64,
    /// `(kind, id)` — rows naming the same agent, issue or PR merge
    /// into one ([`merge_by_subject`]).
    subject: (&'static str, String),
    /// Aliases the row belongs to — `--group` keeps a row when one of
    /// them is a group member.
    agents: Vec<String>,
    /// Who is responsible for acting on the row (CAD-253): an agent
    /// row's upstream PM, an issue or PR row's issue owner, a stale
    /// inbox's owner. `None` — nobody resolvable.
    owner: Option<String>,
    /// Set by [`classify_needs`]; a merged row takes its most urgent cause's.
    audience: Audience,
    /// Epoch seconds when the row's condition began — the unhandled
    /// clock (CAD-253). `None` when the kind has no reliable start: the
    /// row then escalates by owner only, never by age.
    since: Option<i64>,
    json: Value,
}

/// A team-class needs-me row unhandled this long is the operator's
/// (CAD-253, operator decision 2026-09-23).
pub(crate) const ESCALATE_AFTER_SECS: i64 = 60 * 60;

/// Who a needs-me row is for, most urgent first — the derived `Ord`
/// is the merge order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Audience {
    /// "Needs your decision".
    Operator,
    /// A live owner can act on it.
    Team,
    /// Waiting on something outside the fleet (a restart when idle).
    Dependency,
    /// Nothing to decide.
    Info,
}

impl Audience {
    fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Team => "team",
            Self::Dependency => "dependency",
            Self::Info => "info",
        }
    }

    /// The class a kind starts in. Only `team` rows escalate. An
    /// unknown kind is team work, so it can still escalate.
    fn of_kind(kind: &str) -> Self {
        match kind {
            // A fenced agent's exit is `agent unfence` / `message
            // reconcile`, which only the operator may run (CAD-374).
            // CAD-339 Needs-you: a question the master escalated and a
            // plan awaiting approval are the operator's to decide.
            // CAD-477: a checkup-escalated blocked report and a stopped
            // agent holding queued work take the same operator row.
            // CAD-484: an idle lane with no safe next step is the
            // operator's call too.
            "approval" | "fenced" | "question" | "plan" | "blocked" | "stopped"
            | "next_action"
            // CAD-139: a researched idea waiting on the operator, and a
            // near-duplicate the pipeline linked instead of researching.
            | "idea_plan" | "idea_duplicate"
            // CAD-615: the master wants to run a command.
            | "master_permission" => Self::Operator,
            // CAD-431: the merge decision, a review that did not
            // converge, one nobody can take, and auto-merge left on a
            // moved head are the operator's.
            "merge_decision"
            | "review_escalated"
            | "review_unstaffed"
            | "auto_merge_on"
            | "delivery_unreadable"
            // CAD-449: a merged ticket the merge could not mark done.
            | "merged_not_done"
            // CAD-506: a restart-reconciled send and a read-back
            // mismatch are the operator's to resolve (§5.4).
            | "effect_reconcile" | "effect_unverified" => Self::Operator,
            "drift" => Self::Dependency,
            // CAD-439: informs the operator; nothing for the team.
            "inbox_unread" | "tracker_behind" | "master_unconfined" | "master_login"
            // CAD-506: a draft ran without a press — information only.
            | "platform_draft" => Self::Info,
            // CAD-446: the board's own GitHub read is failing or is not
            // the operator's — merge decisions may lag; nothing to decide.
            "delivery_sync" => Self::Info,
            _ => Self::Team,
        }
    }
}

/// Owner liveness from the `agent_list` rows the overview already read.
struct Owners<'a> {
    reachable: bool,
    agents: HashMap<&'a str, &'a Value>,
}

impl<'a> Owners<'a> {
    fn new(reachable: bool, agents: &'a [Value]) -> Self {
        Self {
            reachable,
            agents: agents
                .iter()
                .filter_map(|a| a["alias"].as_str().map(|alias| (alias, a)))
                .collect(),
        }
    }

    /// Why `owner` cannot act on a row — `None` when a live owner can.
    fn cannot_act(&self, owner: Option<&str>) -> Option<String> {
        let Some(owner) = owner.filter(|o| !o.is_empty()) else {
            return Some("no owner".into());
        };
        if owner == inbox::OPERATOR {
            return Some("owner is the operator".into());
        }
        if !self.reachable {
            return Some(format!("owner {owner} unknown — daemon unreachable"));
        }
        let Some(a) = self.agents.get(owner) else {
            return Some(format!("owner {owner} is absent"));
        };
        if a["dead"].as_bool().unwrap_or(false) {
            return Some(format!("owner {owner} is dead"));
        }
        match a["state"].as_str() {
            Some("attention") => return Some(format!("owner {owner} is fenced")),
            Some("stopped") => return Some(format!("owner {owner} is stopped")),
            _ => {}
        }
        // A mailbox owner acts only through whoever drains it.
        if a["inbox_health"]["stale"].as_bool().unwrap_or(false) {
            return Some(format!("owner {owner} has no inbox consumer"));
        }
        None
    }
}

/// Resolve every row's `audience` + `audience_reason` (CAD-253). Kinds
/// already operator-class stay operator; a team row escalates to the
/// operator when its owner cannot act or its condition has stood
/// unhandled past `escalate_after` seconds — measured from `since`,
/// never from the subject's age; a row without `since` escalates by
/// owner only. Runs before [`merge_by_subject`] so each cause is judged
/// on its own owner and clock. The CLI and the board render this
/// field — neither maps kinds to audiences.
fn classify_needs(items: &mut [Item], owners: &Owners, now: i64, escalate_after: i64) {
    for it in items {
        let kind = it.json["kind"].as_str().unwrap_or_default();
        let unhandled = it.since.map(|s| (now - s).max(0));
        let (audience, reason) = match Audience::of_kind(kind) {
            Audience::Operator => (Audience::Operator, Some("operator decision".to_string())),
            Audience::Team => match owners.cannot_act(it.owner.as_deref()) {
                Some(why) => (Audience::Operator, Some(why)),
                None if unhandled.is_some_and(|u| u > escalate_after) => (
                    Audience::Operator,
                    Some(format!("unhandled {}m", unhandled.unwrap_or(0) / 60)),
                ),
                None => (
                    Audience::Team,
                    Some(format!(
                        "owner {} can act",
                        it.owner.as_deref().unwrap_or("")
                    )),
                ),
            },
            other => (other, None),
        };
        it.audience = audience;
        it.json["audience"] = json!(audience.as_str());
        it.json["audience_reason"] = json!(reason);
    }
}

fn item(
    rank: u8,
    kind: &str,
    title: &str,
    age: i64,
    project: &str,
    link: Option<&str>,
    command: &str,
) -> Item {
    let age = age.max(0);
    // Until `about` names it, a row is its own subject.
    let id = format!("{kind}:{title}");
    Item {
        rank,
        age,
        json: json!({
            "kind": kind, "cause": kind, "title": title, "age": age,
            "project": project, "link": link, "command": command,
            "subject": {"kind": "row", "id": id}, "since": null,
        }),
        subject: ("row", id),
        agents: Vec::new(),
        owner: None,
        audience: Audience::of_kind(kind),
        since: None,
    }
}

/// CAD-1219: display-only text caps (characters).
const SHORT_TITLE_MAX: usize = 50;
const WHY_MAX: usize = 140;

/// Invisible characters: Unicode format (Cf) plus default-ignorable code
/// points (combining grapheme joiner, Hangul fillers, variation selectors).
fn strip_invisible(raw: &str) -> String {
    use std::sync::OnceLock;
    static INVISIBLE: OnceLock<regex::Regex> = OnceLock::new();
    INVISIBLE
        .get_or_init(|| {
            regex::Regex::new(r"[\p{Cf}\p{Default_Ignorable_Code_Point}]")
                .expect("valid Unicode categories")
        })
        .replace_all(raw, "")
        .into_owned()
}

/// Top-level `cadence` verbs. The word `cadence` followed by one (or by a
/// flag) reads as a command; "Cadence board shows plans" does not.
const CADENCE_VERBS: &[&str] = &[
    "self",
    "done",
    "inbox",
    "issue",
    "plan",
    "send",
    "dispatch",
    "join",
    "agent",
    "build-slot",
    "secret",
    "update",
    "dev",
    "status",
    "doctor",
    "memory",
    "wiki",
    "master",
    "audit",
    "message",
    "report",
    "overview",
    "rollout",
    "upgrade",
    "review",
    "ui",
    "org",
    "help",
];

/// True when `lower` (already lowercased) has `cadence` followed by a verb
/// or a flag. Cyrillic/Armenian lookalikes of its letters are folded first.
fn reads_as_cadence_command(lower: &str) -> bool {
    let folded: String = lower
        .chars()
        .map(|c| match c {
            '\u{430}' => 'a',
            '\u{441}' => 'c',
            '\u{435}' => 'e',
            '\u{501}' => 'd',
            '\u{578}' => 'n',
            c => c,
        })
        .collect();
    folded.match_indices("cadence").any(|(i, m)| {
        let word: String = folded[i + m.len()..]
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        word.starts_with('-') || CADENCE_VERBS.contains(&word.as_str())
    })
}

/// Make `raw` safe to show as a card's plain-words text: NFKC-folded
/// (so fullwidth lookalikes become their ASCII form), whitespace and
/// newlines collapsed, control and format characters dropped, clipped to
/// `max` characters on a char boundary. Anything that reads as a
/// command (a backtick, `$(`, `${`, `cadence` followed by a verb or flag) yields
/// `None`, so the field is omitted rather than shown. Never returns an
/// empty string.
fn display_text(raw: &str, max: usize) -> Option<String> {
    use unicode_normalization::UnicodeNormalization;
    let spaced: String = strip_invisible(raw)
        .nfkc()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    let joined = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    // A markdown heading marker is not part of the words.
    let text = joined.trim_start_matches('#').trim_start().to_string();
    if text.is_empty()
        || text.contains('`')
        || text.contains("$(")
        || text.contains("${")
        || reads_as_cadence_command(&strip_invisible(&text.to_lowercase()))
    {
        return None;
    }
    if text.chars().count() <= max {
        return Some(text);
    }
    let clipped: String = text.chars().take(max.saturating_sub(1)).collect();
    Some(format!("{}…", clipped.trim_end()))
}

/// [`display_text`] limited to the first sentence — for `why`.
fn display_sentence(raw: &str, max: usize) -> Option<String> {
    let text = display_text(raw, usize::MAX)?;
    let end = text
        .char_indices()
        .find(|&(i, c)| {
            let head = text[..i].to_lowercase();
            matches!(c, '.' | '!' | '?')
                && text[i + c.len_utf8()..].starts_with(' ')
                && !(head.ends_with("e.g") || head.ends_with("i.e"))
        })
        .map_or(text.len(), |(i, c)| i + c.len_utf8());
    display_text(&text[..end], max)
}

impl Item {
    /// CAD-1219: optional plain-words `short_title` and `why` for a To do
    /// card. Display text only — nothing reads them back. A field whose
    /// source is missing or fails [`display_text`] is left out.
    fn set_display(&mut self, short_title: Option<&str>, why: Option<&str>) {
        if let Some(t) = short_title.and_then(|t| display_text(t, SHORT_TITLE_MAX)) {
            self.json["short_title"] = json!(t);
        }
        // A reason that only repeats the title adds nothing.
        if let Some(w) = why
            .and_then(|w| display_sentence(w, WHY_MAX))
            .filter(|w| self.json["short_title"].as_str() != Some(w))
        {
            self.json["why"] = json!(w);
        }
    }

    fn with_display(mut self, short_title: Option<&str>, why: Option<&str>) -> Self {
        self.set_display(short_title, why);
        self
    }

    /// Name the row's subject: `agent`, `issue`, `pr`, `repo`,
    /// `deploy`, `tracker` or `report`.
    fn about(mut self, kind: &'static str, id: &str) -> Self {
        self.json["subject"] = json!({"kind": kind, "id": id});
        self.subject = (kind, id.to_string());
        self
    }

    fn for_agent(mut self, alias: &str) -> Self {
        if !alias.is_empty() && !self.agents.iter().any(|a| a == alias) {
            self.agents.push(alias.to_string());
        }
        self
    }

    /// Name who must act on the row (CAD-253 escalation reads it).
    fn owned_by(mut self, owner: Option<&str>) -> Self {
        self.owner = owner.filter(|o| !o.is_empty()).map(str::to_string);
        self
    }

    /// When the row's condition began (epoch secs), when known.
    fn since(mut self, since: Option<i64>) -> Self {
        self.set_since(since);
        self
    }

    fn set_since(&mut self, since: Option<i64>) {
        self.since = since;
        self.json["since"] = json!(since);
    }
}

/// CAD-431 Needs-you rows from the daemon's worker-loop record — the
/// only source; a report file cannot raise them. One "merge?" row per
/// PASS that the operator's process saw open and green at the reviewed
/// head, with owner, age, PR link, the verdict's summary and diff stats;
/// one row per review that did not converge or that nobody can take;
/// one per PR whose auto-merge must be turned off.
fn delivery_items(state_dir: &Path, now: i64) -> Vec<Item> {
    let mut out = Vec::new();
    let records = match crate::delivery::load(state_dir) {
        Ok(records) => records,
        Err(e) => {
            // Never a silent empty loop: the operator sees it is broken.
            out.push(
                item(
                    12,
                    "delivery_unreadable",
                    &format!("the review loop's record is unreadable — {e}"),
                    0,
                    "",
                    None,
                    "cadence delivery ls",
                )
                .about("tracker", "delivery.json"),
            );
            return out;
        }
    };
    for rec in records.into_values() {
        let id = rec.issue.as_str();
        let age = now - rec.since;
        let pr = rec.pr.as_deref();
        if rec.disable_auto {
            out.push(
                item(
                    18,
                    "auto_merge_on",
                    &format!("{id}: auto-merge is on for a head nobody approved — turn it off"),
                    age,
                    &rec.project,
                    pr,
                    &cmd_delivery_sync(id),
                )
                .about("issue", id)
                .for_agent(&rec.worker)
                .since(Some(rec.since)),
            );
        }
        if rec.state == crate::delivery::State::Merged {
            if let Some(why) = rec.ticket_done.as_ref().and_then(|d| d.open()) {
                out.push(
                    item(
                        20,
                        "merged_not_done",
                        &format!("{id}: merged, but not marked done — {why}"),
                        age,
                        &rec.project,
                        pr,
                        &format!("cadence issue set {id} status=done"),
                    )
                    .about("issue", id)
                    .for_agent(&rec.worker)
                    .since(Some(rec.since)),
                );
            }
        }
        let row = match rec.state {
            crate::delivery::State::Passed if rec.merge_ready() => {
                let v = rec
                    .verdict
                    .clone()
                    .unwrap_or_else(|| crate::delivery::VerdictRec {
                        verdict: String::new(),
                        sha: String::new(),
                        reviewer: String::new(),
                        summary: String::new(),
                        report: String::new(),
                        at: rec.since,
                    });
                let o = rec.observed.clone().unwrap_or_default();
                let pr_ref = pr.and_then(crate::delivery::pr_ref);
                let number = pr_ref
                    .as_deref()
                    .map(|r| format!(" {r}"))
                    .unwrap_or_default();
                let mut row = item(
                    // A concrete, reviewed merge action outranks the
                    // generic rank-20 "no safe next step" alert.
                    19,
                    "merge_decision",
                    &format!(
                        "merge? {id}{number} by {} — PASS by {}: {} (+{} −{}, {} files)",
                        rec.worker, v.reviewer, v.summary, o.additions, o.deletions, o.files
                    ),
                    now - v.at,
                    &rec.project,
                    pr,
                    &cmd_delivery_merge(id),
                )
                .about("issue", id)
                .for_agent(&rec.worker)
                .owned_by(Some(&rec.worker))
                .since(Some(v.at));
                row.json["merge"] = json!({
                    "issue": id, "pr": pr, "pr_ref": pr_ref, "sha": v.sha, "owner": rec.worker,
                    "reviewer": v.reviewer, "verdict_summary": v.summary,
                    "report": v.report, "additions": o.additions,
                    "deletions": o.deletions, "files": o.files,
                    "decline": cmd_delivery_decline(id),
                });
                Some(row)
            }
            crate::delivery::State::Escalated => Some(
                item(
                    24,
                    "review_escalated",
                    &format!(
                        "{id}: {} REVISE verdicts — the review did not converge",
                        rec.revisions
                    ),
                    age,
                    &rec.project,
                    pr,
                    &cmd_issue_show(id),
                )
                .about("issue", id)
                .for_agent(&rec.worker)
                .since(Some(rec.since)),
            ),
            crate::delivery::State::Unstaffed => Some(
                item(
                    26,
                    "review_unstaffed",
                    &format!("{id}: no reviewer is staffed for its review"),
                    age,
                    &rec.project,
                    pr,
                    "cadence agent list",
                )
                .about("issue", id)
                .for_agent(&rec.worker)
                .since(Some(rec.since)),
            ),
            _ => None,
        };
        out.extend(row);
    }
    out
}

/// CAD-446: a board delivery-sync problem as one Needs-you `info` row
/// (`kind: delivery_sync`, subject `tracker:<subject>`), shaped like
/// every other row after classification and the subject merge. `title`
/// is the board's text — already one line, bounded and redacted;
/// `since` is when the problem began (epoch secs).
pub(crate) fn delivery_sync_row(title: &str, subject: &str, since: i64, now: i64) -> Value {
    let mut rows = vec![item(
        120,
        "delivery_sync",
        title,
        now - since,
        "",
        None,
        CMD_DELIVERY_SYNC,
    )
    .about("tracker", subject)
    .since(Some(since))];
    classify_needs(
        &mut rows,
        &Owners::new(false, &[]),
        now,
        ESCALATE_AFTER_SECS,
    );
    merge_by_subject(rows)
        .into_iter()
        .next()
        .map(|i| i.json)
        .unwrap_or(Value::Null)
}

/// Row scope for `cadence overview --project/--group` (CAD-252).
#[derive(Clone, Debug, Default)]
pub struct Scope {
    /// Keep rows attributed to this tracker project key.
    pub project: Option<String>,
    /// Keep rows owned by this group root or one of its members
    /// (`params.upstream == root`), the way `cadence status --group`
    /// resolves a group.
    pub group: Option<String>,
}

/// How one overview build bounds its sources (CAD-249).
#[derive(Clone, Debug)]
pub struct Options {
    pub scope: Scope,
    /// Serve the gh block from the cache only — never fetch, never write.
    pub cache_only: bool,
    /// How long to wait on a gh refresh before serving the last cache.
    pub gh_wait: Duration,
    /// Display-cache age, clamped to 60..=3600 seconds. The default reads
    /// CADENCE_OVERVIEW_GH_CACHE_SECS from the board/CLI environment.
    pub gh_cache_secs: i64,
    /// Read bound on each daemon RPC.
    pub probe_timeout: Duration,
    /// The per-agent probe pass starts no probe past this budget.
    pub probe_budget: Duration,
}

impl Options {
    /// One-shot callers (`cadence overview`, `session`): the process
    /// exits after one build, so a background gh refresh would never
    /// land — wait it out (each gh call is bounded by [`GH_TIMEOUT`]).
    pub fn cli() -> Self {
        Self {
            scope: Scope::default(),
            cache_only: false,
            gh_wait: GH_TIMEOUT * 2 + Duration::from_secs(1),
            gh_cache_secs: gh_cache_secs(
                std::env::var("CADENCE_OVERVIEW_GH_CACHE_SECS")
                    .ok()
                    .as_deref(),
            ),
            probe_timeout: PROBE_TIMEOUT,
            probe_budget: PROBE_BUDGET,
        }
    }

    /// The long-lived board server: past [`GH_BOARD_WAIT`] the last
    /// cache is served (`github.state: stale`, with `as_of`) while the
    /// refresh finishes in the background for the next request.
    pub fn board() -> Self {
        Self {
            gh_wait: GH_BOARD_WAIT,
            ..Self::cli()
        }
    }
}

// ---------- default-branch CI (CAD-267) ----------

/// The whole screen. `pm_dir` names the tracker dir (it may not exist
/// — that just empties the tracker sections); `state_dir` names the
/// runtime dir (daemon socket + the gh cache).
pub fn overview(state_dir: &Path, pm_dir: &Path) -> Value {
    unscoped(overview_with(state_dir, pm_dir, &Options::cli()))
}

/// The overview with the gh block served from the cache only — a dry
/// run must write nothing, cache included, so it never fetches.
pub(crate) fn overview_cached(state_dir: &Path, pm_dir: &Path) -> Value {
    let opts = Options {
        cache_only: true,
        ..Options::cli()
    };
    unscoped(overview_with(state_dir, pm_dir, &opts))
}

/// `GET /api/overview` — bounded gh wait, background refresh.
pub fn overview_board(state_dir: &Path, pm_dir: &Path) -> Value {
    unscoped(overview_with(state_dir, pm_dir, &Options::board()))
}

/// Only a scope can fail a build, and these callers pass none.
fn unscoped(view: Result<Value, String>) -> Value {
    view.unwrap_or_else(|e| json!({"error": e}))
}

/// A `degraded` note: one source answered late or not at all, and the
/// screen narrowed instead of failing.
fn degraded(source: &str, subject: &str, detail: impl Into<String>) -> Value {
    json!({"source": source, "subject": subject, "detail": detail.into()})
}

/// What one agent's probes returned. A mailbox (no actor) needs none —
/// its `agent_list` row carries the backlog and health.
#[derive(Clone, Default)]
struct AgentProbe {
    show: Option<Value>,
    requests: Vec<Value>,
    /// A running/submitted turn, a busy pane, or a probe that could not
    /// tell — any of them holds the drift restart back.
    holds_drift: bool,
    /// The daemon predates `agent_probe`.
    probe_unknown: bool,
    /// The probe budget ran out before this agent was reached.
    unprobed: bool,
    degraded: Vec<Value>,
}

/// One daemon RPC under the overview's bounds: the per-call timeout,
/// clipped to what is left of the pass's deadline.
fn bounded_rpc(
    state_dir: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
    deadline: Instant,
) -> Result<Value, String> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left < Duration::from_millis(50) {
        return Err("overview probe budget spent".to_string());
    }
    let bound = left.min(timeout);
    client::rpc_timeout(state_dir, method, params, bound).map_err(|e| {
        let text = e.to_string();
        // A read timeout surfaces as EAGAIN/"timed out" — name the bound.
        if text.contains("os error 11") || text.contains("timed out") {
            format!("no answer within {}ms", bound.as_millis())
        } else {
            text
        }
    })
}

/// Show, pending requests and (for an idle pty pane) the pane probe of
/// one agent, each bounded.
fn probe_agent(state_dir: &Path, a: &Value, timeout: Duration, deadline: Instant) -> AgentProbe {
    let alias = a["alias"].as_str().unwrap_or_default();
    let provider = a["provider"].as_str().unwrap_or_default();
    let kind = a["endpoint_kind"].as_str().unwrap_or_default();
    let mut p = AgentProbe::default();
    // A mailbox has no pane and no requests: its `agent_list` row
    // carries the backlog. Only rows that predate the `inbox` block
    // (an older daemon) need the show read for the queued count.
    let mailbox = !registry::has_actor(provider, kind);
    if mailbox && a["inbox"]["queued"].is_i64() {
        return p;
    }
    if deadline.saturating_duration_since(Instant::now()) < Duration::from_millis(50) {
        p.unprobed = true;
        // A mailbox never holds a restart back.
        p.holds_drift = !mailbox;
        return p;
    }
    let rpc = |method: &str| {
        let params = if method == "agent_show" {
            json!({"alias": alias, "active_only": true})
        } else {
            json!({"alias": alias})
        };
        bounded_rpc(state_dir, method, params, timeout, deadline)
    };
    match rpc("agent_show") {
        Ok(v) => p.show = Some(v),
        Err(e) => p.degraded.push(degraded("agent_show", alias, e)),
    }
    if mailbox {
        return p;
    }
    match rpc("agent_requests") {
        Ok(v) => p.requests = v["requests"].as_array().cloned().unwrap_or_default(),
        Err(e) if e.contains("Unknown method") => {}
        Err(e) => p.degraded.push(degraded("agent_requests", alias, e)),
    }
    // Drift is only actionable when everything is idle: any running or
    // submitted message, or any busy pty pane, holds it back — and so
    // does a show that never answered.
    let busy_turn = p.show.as_ref().is_none_or(|show| {
        show["messages"].as_array().is_some_and(|ms| {
            ms.iter().any(|m| {
                matches!(
                    m["state"].as_str().unwrap_or_default(),
                    "running" | "submitted"
                )
            })
        })
    });
    if busy_turn {
        p.holds_drift = true;
    } else if kind == "pty" && a["endpoint"].is_string() {
        match rpc("agent_probe") {
            Ok(v) if v["idle"].as_bool().unwrap_or(false) => {}
            Ok(_) => p.holds_drift = true,
            // "Unknown method" — a daemon that predates the RPC; any
            // other failure reads as busy, conservatively.
            Err(e) if e.contains("Unknown method") => p.probe_unknown = true,
            Err(e) => {
                p.holds_drift = true;
                p.degraded.push(degraded("agent_probe", alias, e));
            }
        }
    }
    p
}

/// Probe every agent on [`PROBE_WORKERS`] threads — the daemon answers
/// each connection on its own thread, so the pass costs the slowest
/// probes, not their sum. No probe starts past `opts.probe_budget`.
fn probe_agents(state_dir: &Path, agents: &[Value], opts: &Options) -> Vec<AgentProbe> {
    let deadline = Instant::now() + opts.probe_budget;
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<AgentProbe>>> = agents.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..PROBE_WORKERS.min(agents.len()) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(a) = agents.get(i) else {
                    break;
                };
                let p = probe_agent(state_dir, a, opts.probe_timeout, deadline);
                *slots[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
            });
        }
    });
    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .unwrap_or_else(|e| e.into_inner())
                .unwrap_or_default()
        })
        .collect()
}

/// The daemon's half of the screen: reachability, build identity, the
/// agent rows and their probes.
#[derive(Clone)]
struct DaemonView {
    reachable: bool,
    info: Option<Value>,
    agents: Vec<Value>,
    probes: Vec<AgentProbe>,
    degraded: Vec<Value>,
}

fn daemon_view(state_dir: &Path, opts: &Options) -> DaemonView {
    let t = opts.probe_timeout;
    let mut view = DaemonView {
        // Reachability comes from `health` — a pre-daemon_info daemon
        // answers it, so an old build never reads as "unreachable"
        // while `agent list` works. `daemon_info` only carries the id.
        reachable: client::rpc_timeout(state_dir, "health", json!({}), t).is_ok(),
        info: None,
        agents: Vec::new(),
        probes: Vec::new(),
        degraded: Vec::new(),
    };
    if !view.reachable {
        return view;
    }
    view.info = client::rpc_timeout(state_dir, "daemon_info", json!({}), t).ok();
    match client::rpc_timeout(state_dir, "agent_list", json!({}), t) {
        Ok(v) => view.agents = v["agents"].as_array().cloned().unwrap_or_default(),
        Err(e) => view.degraded.push(degraded(
            "agent_list",
            "",
            format!("agent rows missing — {e}"),
        )),
    }
    view.probes = probe_agents(state_dir, &view.agents, opts);
    let unprobed: Vec<&str> = view
        .agents
        .iter()
        .zip(&view.probes)
        .filter(|(_, p)| p.unprobed)
        .filter_map(|(a, _)| a["alias"].as_str())
        .collect();
    if !unprobed.is_empty() {
        view.degraded.push(degraded(
            "agent_probe_budget",
            "",
            format!(
                "{} agent(s) not probed within {:.1}s: {}",
                unprobed.len(),
                opts.probe_budget.as_secs_f64(),
                unprobed.join(", ")
            ),
        ));
    }
    for p in &mut view.probes {
        view.degraded.append(&mut p.degraded);
    }
    view
}

/// The daemon-side sources of one build — the agent probes and the
/// monitoring block. The board's read model (CAD-325) keeps the last
/// pass and rebuilds the screen from it when only the tracker moved.
#[derive(Clone)]
pub struct DaemonSources {
    daemon: DaemonView,
    monitoring: Value,
}

/// Probe the daemon and read the monitors, concurrently.
pub fn daemon_sources(state_dir: &Path, opts: &Options) -> DaemonSources {
    std::thread::scope(|s| {
        let daemon = s.spawn(|| daemon_view(state_dir, opts));
        let monitoring = s.spawn(|| monitoring(state_dir));
        DaemonSources {
            daemon: daemon.join().expect("overview daemon probe panicked"),
            monitoring: monitoring
                .join()
                .expect("overview monitoring read panicked"),
        }
    })
}

/// What a long-lived caller hands a build instead of re-reading it: the
/// indexed tracker's views and a recent daemon pass. Status and claim
/// times need no hand-off — [`LineTimes`] is cached per tracker HEAD
/// (CAD-403).
pub struct Reuse<'a> {
    pub views: &'a [board::View],
    pub sources: &'a DaemonSources,
}

/// Wall time the tracker line times may take to load per build (a cold
/// cache or a moved HEAD walks git); past it rows get no clock
/// (owner-only escalation) and one `degraded` note.
const STATUS_CLOCK_BUDGET: Duration = Duration::from_secs(3);

/// When each issue entered its current effective status (CAD-253). A
/// `file` status is the tracker's last `status:` change, read from the
/// cached [`LineTimes`] (CAD-403 — no git walk per issue); a `notes`
/// status is the deriving note's time; a `rollup` or `job` status has
/// no single change to point at, so no clock.
struct StatusClock<'a> {
    /// `None` when the line times could not be loaded in time.
    times: Option<&'a LineTimes>,
    /// Issues asked for whose `file` status has no clock because the
    /// line times are missing, each counted once.
    skipped: std::collections::HashSet<String>,
}

impl<'a> StatusClock<'a> {
    fn new(times: Option<&'a LineTimes>) -> Self {
        Self {
            times,
            skipped: Default::default(),
        }
    }

    fn since(&mut self, v: &board::View) -> Option<i64> {
        let id = &v.issue.front.id;
        match v.status_source {
            "file" => match self.times {
                Some(t) => t.status_at(&v.issue.project, id),
                None => {
                    self.skipped.insert(id.clone());
                    None
                }
            },
            "notes" => v.chain.last().and_then(|n| parse_iso(&n.at)),
            _ => None,
        }
    }
}

/// The tracker project an agent works in: the longest declared repo
/// path its cwd sits under (worktrees under `.cadence/wt/` included).
/// "" — a global row — when nothing matches.
fn agent_project(a: &Value, repos: &[(PathBuf, String)]) -> String {
    let Some(cwd) = a["cwd"].as_str().filter(|c| !c.is_empty()) else {
        return String::new();
    };
    let cwd = PathBuf::from(cwd);
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    repos
        .iter()
        .filter(|(path, _)| cwd.starts_with(path))
        .max_by_key(|(path, _)| path.components().count())
        .map(|(_, key)| key.clone())
        .unwrap_or_default()
}

/// The needs-me rows one agent contributes: its `agent_list` row (state,
/// stall view, mailbox backlog and health) plus its probe.
/// CAD-439 (operator decision): a confined master whose own provider
/// config dir (`master/claude`, `master/pi`) holds no login cannot
/// authenticate — one info row naming the command that gives it its
/// own, until the login exists.
fn master_login_item(a: &Value, state_dir: &Path, project: &str, now: i64) -> Option<Item> {
    let alias = a["alias"].as_str().unwrap_or_default();
    let provider = a["provider"].as_str().unwrap_or("claude");
    let confined =
        crate::master::is_confined(Some(&a["params"]), crate::confine::available().is_ok());
    if !crate::master::is_master(alias)
        || !confined
        || a["state"].as_str() == Some("stopped")
        || crate::master::has_login_for(provider, state_dir)
    {
        return None;
    }
    let command = crate::master::login_command_for(provider, state_dir);
    let age = now - a["updated"].as_f64().unwrap_or(now as f64) as i64;
    Some(
        item(
            90,
            "master_login",
            &format!("master has no {provider} login — give it its own: {command}"),
            age,
            project,
            None,
            &command,
        )
        .about("agent", alias)
        .for_agent(alias)
        .owned_by(None),
    )
}

fn agent_items(a: &Value, probe: &AgentProbe, project: &str, now: i64) -> Vec<Item> {
    let alias = a["alias"].as_str().unwrap_or_default();
    let age = now - a["updated"].as_f64().unwrap_or(now as f64) as i64;
    // The agent's PM acts on its rows (CAD-253); a root agent has none.
    let pm = a["params"]["upstream"].as_str();
    let row = |rank: u8, kind: &str, title: &str, age: i64, command: &str| {
        item(rank, kind, title, age, project, None, command)
            .about("agent", alias)
            .for_agent(alias)
            .owned_by(pm)
    };
    let mut items = Vec::new();
    // CAD-439 (operator decision): a master started `--unconfined` on a
    // host without Landlock shows for as long as it is registered and
    // not stopped.
    if crate::master::is_master(alias)
        && crate::master::unconfined(Some(&a["params"]))
        && a["state"].as_str() != Some("stopped")
    {
        items.push(
            row(
                90,
                "master_unconfined",
                "master runs unconfined — no filesystem sandbox on this host; it can read and \
                 write your files",
                age,
                &cmd_agent_show(alias),
            )
            .owned_by(None),
        );
    }
    // Condition clocks (CAD-253): a daemon-measured age is a start
    // time; `updated` is not — any params/model write moves it.
    let secs_ago = |key: &str| a[key].as_f64().map(|s| now - s as i64);
    let queued = a["inbox"]["queued"]
        .as_i64()
        .or_else(|| probe.show.as_ref().and_then(|s| s["queued"].as_i64()))
        .unwrap_or(0);
    // CAD-413: an auto-resume for queued work failed — the agent is
    // down with a message waiting. The more specific row: it replaces
    // the generic `fenced` row a failed open would otherwise raise.
    let resume_failed = &a["auto_resume_failed"];
    if resume_failed.is_object() {
        let at = resume_failed["at"].as_f64().map(|at| at as i64);
        items.push(
            row(
                30,
                "auto_resume_failed",
                &format!(
                    "agent {alias} auto-resume failed — message {} waiting: {}",
                    resume_failed["message"].as_str().unwrap_or("?"),
                    resume_failed["reason"].as_str().unwrap_or("unknown error"),
                ),
                at.map_or(age, |at| now - at),
                &cmd_agent_resume(alias),
            )
            .since(at),
        );
    } else if a["state"].as_str() == Some("attention") {
        items.push(
            row(
                30,
                "fenced",
                &format!("agent {alias} fenced — reconcile then resume"),
                age,
                &cmd_agent_unfence(alias),
            )
            // Operator only (CAD-374): the PM escalates, it cannot act.
            .owned_by(None)
            .since(fenced_since(a, probe)),
        );
    }
    // CAD-477: a stopped agent still holding queued work is a lane
    // nobody drives. The idle timer's own stop is auto-resume's to
    // restart (CAD-413) — every other stop is the operator's.
    if a["state"].as_str() == Some("stopped")
        && !a["auto_stopped"].is_object()
        && !resume_failed.is_object()
        && queued > 0
    {
        items.push(
            row(
                30,
                "stopped",
                &format!("agent {alias} stopped with {queued} queued — resume it"),
                age,
                &cmd_agent_resume(alias),
            )
            .owned_by(None),
        );
    }
    if a["stalled"].as_bool().unwrap_or(false) {
        items.push(
            row(
                40,
                "stalled",
                &format!("agent {alias} turn silent"),
                a["silent_secs"].as_f64().unwrap_or(age as f64) as i64,
                &cmd_agent_show(alias),
            )
            .since(secs_ago("silent_secs")),
        );
    }
    // CAD-520: the delivery watchdog — a queued head has outlived
    // `delivery_watch_secs` while the pane probes ready. Something is
    // wedged between the queue and the pane; the row names the agent,
    // the wedged message, and the probe's verdict.
    if let Some(ds) = a["delivery_stalled"].as_object() {
        let msg = ds["message"].as_str().unwrap_or("?");
        let verdict = ds["verdict"].as_str().unwrap_or("idle");
        items.push(row(
            30,
            "delivery_stalled",
            &format!(
                "agent {alias} message {msg} queued past the watchdog bound while \
                     the pane probes '{verdict}' — delivery stalled"
            ),
            age,
            &cmd_agent_show(alias),
        ));
    }
    // A sampled approval menu ranks with brokered approvals — the pane
    // is waiting on a human either way.
    if let Some(line) = a["pane_menu"].as_str() {
        items.push(row(
            20,
            "approval_menu",
            &format!("agent {alias} approval menu: {line}"),
            age,
            &cmd_agent_answer(alias),
        ));
    }
    // CAD-250: an unreported turn holds the actor's queue — a row only
    // once real work waits behind it, so a healthy turn in progress is
    // never needs-me noise. Past the bound the turn goes `unknown` and
    // the `fenced` row takes over.
    let awaiting = &a["awaiting_report"];
    let behind = awaiting["queued_behind"].as_i64().unwrap_or(0);
    if awaiting.is_object() && behind > 0 {
        let waited = awaiting["since_secs"].as_u64().unwrap_or(0);
        let bound = match awaiting["remaining_secs"].as_u64() {
            Some(left) => format!("unknown in {}", inbox::fmt_age(left)),
            None => "no report bound set".to_string(),
        };
        items.push(
            row(
                40,
                "awaiting_report",
                &format!(
                    "agent {alias} awaiting report for {} — {behind} queued behind it; {bound}",
                    inbox::fmt_age(waited)
                ),
                waited as i64,
                &cmd_agent_show(alias),
            )
            .since(Some(now - waited as i64)),
        );
    }
    if a["silent_ended"].as_bool().unwrap_or(false) {
        // CAD-468: the daemon fires one report-reminder nudge at the
        // silent-end edge — say so on the row when a `sys-nudge-` row
        // carrying the report command exists for this agent, so the
        // operator knows the worker was already prompted in band.
        let reminded = probe.show.as_ref().is_some_and(|s| {
            s["messages"].as_array().is_some_and(|ms| {
                ms.iter().any(|m| {
                    m["source"].as_str() == Some("nudge")
                        && m["id"]
                            .as_str()
                            .is_some_and(|id| id.starts_with("sys-nudge-"))
                        && m["body"]
                            .as_str()
                            .is_some_and(|b| b.contains("message result"))
                })
            })
        });
        let text = if reminded {
            format!(
                "agent {alias} turn ended at an idle pane — never reported; report reminder sent"
            )
        } else {
            format!("agent {alias} turn ended at an idle pane — never reported")
        };
        items.push(
            row(
                40,
                "silent_end",
                &text,
                a["ended_secs"].as_f64().unwrap_or(age as f64) as i64,
                &cmd_agent_attach(alias),
            )
            .since(secs_ago("ended_secs")),
        );
    }
    if a["provider"].as_str() == Some(registry::INBOX) && queued > 0 {
        items.push(row(
            100,
            "inbox_unread",
            &format!("{queued} unread for {alias}"),
            age,
            &cmd_inbox(alias),
        ));
    }
    // CAD-251: a mailbox past its unread threshold with no recent
    // `inbox_read`, attributed to its owner (group root, else operator).
    let health = &a["inbox_health"];
    if health["stale"].as_bool().unwrap_or(false) {
        let owner = health["owner"].as_str().unwrap_or(inbox::OPERATOR);
        let oldest = health["oldest_unread_age_secs"].as_i64().unwrap_or(0);
        let mut stale = row(
            95,
            "inbox_stale",
            &format!(
                "inbox {alias} has no consumer — {} unread, oldest {}, owner {owner}",
                health["unread"].as_u64().unwrap_or(0),
                inbox::fmt_age(oldest.max(0) as u64),
            ),
            oldest,
            &cmd_inbox(alias),
        )
        .for_agent(owner)
        .owned_by(Some(owner))
        .since(
            health["oldest_unread_age_secs"]
                .as_i64()
                .map(|secs| now - secs),
        );
        stale.json["owner"] = json!(owner);
        items.push(stale);
    }
    for req in &probe.requests {
        let handle = req["request"].as_str().unwrap_or_default();
        let method = req["method"].as_str().unwrap_or("request");
        let what = if method == "item/tool/requestUserInput" {
            "input request"
        } else {
            "approval"
        };
        items.push(row(
            20,
            "approval",
            &format!("{method} {what} for {alias}"),
            age,
            &cmd_agent_respond(alias, handle, method),
        ));
    }
    items
}

/// CAD-213: quota-blocked, retrying, and still-queued intake relay work.
/// The poller does not wake a provider to discover this; the row is the
/// PM's attention surface until a later sync can dispatch.
fn relay_attention_items(state_dir: &Path, now: i64) -> Vec<Item> {
    crate::issue::relay::attention(state_dir, now)
        .into_iter()
        .map(|row| {
            let since = (row.age_secs > 0).then_some(now.saturating_sub(row.age_secs));
            let mut item = item(
                86,
                "intake_relay",
                &row.title,
                row.age_secs,
                &row.project,
                None,
                &row.command,
            )
            .about("report", &row.subject_id)
            .since(since);
            if let Some(owner) = row.owner.as_deref() {
                item = item.for_agent(owner).owned_by(Some(owner));
            }
            item
        })
        .collect()
}

/// CAD-506 §5.4: Needs-you rows from the durable pending-effect table
/// and the information-only draft rows (Q3). One `platform_effects`
/// read — an operator-proven board sees all rows; a board run inside
/// a pane gets a caller-rule refusal, which degrades to no rows rather
/// than a board error.
fn platform_effect_items(state_dir: &Path, now: i64, timeout: Duration) -> Vec<Item> {
    let Ok(view) = client::rpc_timeout(state_dir, "platform_effects", json!({}), timeout) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in view["needs_you"].as_array().cloned().unwrap_or_default() {
        let (kind, what) = match e["state"].as_str().unwrap_or_default() {
            "reconcile" => (
                "effect_reconcile",
                "accepted send lacks a proven outcome — inspect the platform, then close",
            ),
            _ => (
                "effect_unverified",
                "send's read-back does not match the approved input",
            ),
        };
        let id = e["effect_id"].as_str().unwrap_or_default();
        let agent = e["agent"].as_str().unwrap_or_default();
        out.push(
            item(
                18,
                kind,
                &format!(
                    "{}: {}",
                    e["tool"].as_str().unwrap_or("platform send"),
                    what
                ),
                0,
                "",
                None,
                &format!("cadence platform effect-close --effect-id {id}"),
            )
            .about("effect", id)
            .for_agent(agent)
            .owned_by(Some("operator")),
        );
    }
    for d in view["drafts"].as_array().cloned().unwrap_or_default() {
        let agent = d["agent"].as_str().unwrap_or_default();
        let label = d["label"].as_str().unwrap_or("draft");
        let summary = d["input_summary"].as_str().unwrap_or_default();
        let id = d["artifact"].as_str().unwrap_or_default().to_string();
        let ran = (d["ran_at"].as_f64().unwrap_or(now as f64)) as i64;
        out.push(
            item(
                96,
                "platform_draft",
                &format!("{agent} ran {label}: {summary}"),
                (now - ran).max(0),
                "",
                None,
                "cadence platform effects",
            )
            .about("draft", &id)
            .for_agent(agent)
            .since(Some(ran)),
        );
    }
    out
}

/// `(earliest start, latest finish)` over a PR head's rollup, epoch
/// secs: a CheckRun contributes `startedAt` and `completedAt`, a status
/// context its `startedAt` (when it was posted).
fn rollup_span(rollup: &[Value]) -> (Option<i64>, Option<i64>) {
    // gh reports a not-yet-started check as `0001-01-01T00:00:00Z`.
    let at = |c: &Value, key: &str| c[key].as_str().and_then(parse_iso).filter(|t| *t > 0);
    let starts = rollup.iter().filter_map(|c| at(c, "startedAt"));
    let ends = rollup
        .iter()
        .filter_map(|c| at(c, "completedAt").or_else(|| at(c, "startedAt")));
    (starts.min(), ends.max())
}

/// A fence began when its turn went `unknown`: the earliest `completed`
/// among the agent's unknown messages (the probe's `agent_show`). A
/// fence with no such message (a provider disconnect while idle, a
/// restart mismatch) falls back to the row's `updated`: every write
/// that enters `attention` stamps it and no unstamped write does, so an
/// agent still in `attention` has held it at least since `updated` — a
/// later params or model write only shortens the clock, never inflates it.
fn fenced_since(a: &Value, probe: &AgentProbe) -> Option<i64> {
    let unknown = probe
        .show
        .as_ref()
        .and_then(|show| show["messages"].as_array())
        .and_then(|ms| {
            ms.iter()
                .filter(|m| m["state"].as_str() == Some("unknown"))
                .filter_map(|m| m["completed"].as_f64())
                .map(|t| t as i64)
                .min()
        });
    unknown.or_else(|| a["updated"].as_f64().map(|t| t as i64))
}

/// Scope the merged rows: `--project` keeps rows attributed to the key,
/// `--group` keeps rows owned by the root or a member.
fn scope_rows(items: Vec<Item>, project: Option<&str>, members: Option<&[String]>) -> Vec<Item> {
    items
        .into_iter()
        .filter(|i| project.is_none_or(|p| i.json["project"].as_str() == Some(p)))
        .filter(|i| members.is_none_or(|m| i.agents.iter().any(|a| m.contains(a))))
        .collect()
}

/// A group's aliases — the root plus every agent whose upstream names
/// it (one level, like `cadence status --group`). An alias no agent
/// carries is an error, not an empty screen.
fn group_members(view: &DaemonView, root: &str) -> Result<Vec<String>, String> {
    if !view.reachable {
        return Err(format!("cannot resolve group '{root}': daemon unreachable"));
    }
    if !view
        .agents
        .iter()
        .any(|a| a["alias"].as_str() == Some(root))
    {
        return Err(format!(
            "unknown group '{root}' — no registered agent has that alias (see `cadence agent list`)"
        ));
    }
    Ok(view
        .agents
        .iter()
        .filter(|a| {
            a["alias"].as_str() == Some(root) || a["params"]["upstream"].as_str() == Some(root)
        })
        .filter_map(|a| a["alias"].as_str().map(str::to_string))
        .collect())
}

/// Build the screen under `opts`. The daemon probes, the gh refresh and
/// the tracker read run concurrently — each bounded — so the view costs
/// its slowest source, not their sum. Fails only on a scope naming an
/// unknown project key or group.
pub fn overview_with(state_dir: &Path, pm_dir: &Path, opts: &Options) -> Result<Value, String> {
    overview_from(state_dir, pm_dir, opts, None)
}

/// The board read model's build (CAD-325): the tracker, daemon and clock
/// inputs come from `reuse`; only `gh` (from its cache, waiting at most
/// `gh_wait`) and the local git reads run here.
pub fn overview_board_from(
    state_dir: &Path,
    pm_dir: &Path,
    reuse: Reuse<'_>,
    gh_wait: Duration,
) -> Value {
    let opts = Options {
        gh_wait,
        ..Options::board()
    };
    unscoped(overview_from(state_dir, pm_dir, &opts, Some(reuse)))
}

fn overview_from(
    state_dir: &Path,
    pm_dir: &Path,
    opts: &Options,
    reuse: Option<Reuse<'_>>,
) -> Result<Value, String> {
    let now = now_epoch();
    let pm = issue::Pm::at(pm_dir).ok();
    let projects = pm
        .as_ref()
        .map(|pm| project::list(&pm.dir).unwrap_or_default())
        .unwrap_or_default();
    if let Some(key) = opts.scope.project.as_deref() {
        if !projects.iter().any(|p| p.key == key) {
            let known: Vec<&str> = projects.iter().map(|p| p.key.as_str()).collect();
            return Err(if known.is_empty() {
                format!(
                    "unknown project '{key}' — no tracker projects under {}",
                    pm_dir.display()
                )
            } else {
                format!("unknown project '{key}' — known: {}", known.join(", "))
            });
        }
    }
    let mut slugs = Vec::new();
    let mut slug_project: HashMap<String, String> = HashMap::new();
    let mut repo_paths: Vec<(PathBuf, String)> = Vec::new();
    // Slug → the declared local clone its first-parent log comes from.
    let mut slug_clone: HashMap<String, PathBuf> = HashMap::new();
    for p in &projects {
        for r in &p.repos {
            let path = r.path.as_deref().map(|path| {
                let path = project::expand_home(path);
                path.canonicalize().unwrap_or(path)
            });
            if let Some(remote) = &r.remote {
                let norm = project::normalize_remote(remote);
                if let Some(slug) = norm.strip_prefix("github.com/") {
                    slug_project.insert(slug.to_string(), p.key.clone());
                    slugs.push(slug.to_string());
                    if let Some(path) = &path {
                        slug_clone
                            .entry(slug.to_string())
                            .or_insert_with(|| path.clone());
                    }
                }
            }
            if let Some(path) = path {
                repo_paths.push((path, p.key.clone()));
            }
        }
    }
    slugs.sort();
    slugs.dedup();

    // ---- the three sources, concurrently (what `reuse` lacks) ----
    let reused = reuse.as_ref();
    let (fresh_sources, (gh_repos, gh_state), fresh_views) = std::thread::scope(|s| {
        let sources = reused
            .is_none()
            .then(|| s.spawn(|| daemon_sources(state_dir, opts)));
        let gh = s.spawn(|| {
            if opts.cache_only {
                github_repos_cached(state_dir, &slugs)
            } else {
                github_bounded(state_dir, &slugs, opts.gh_wait, opts.gh_cache_secs, gh_repo)
            }
        });
        // Local files only — the notes index keeps it one pass.
        let tracker = reused.is_none().then(|| {
            s.spawn(|| {
                pm.as_ref().map(|pm| {
                    let issues = board::load_all(&pm.dir, None).unwrap_or_default();
                    board::views(&pm.config.notes_dir(), issues)
                })
            })
        });
        (
            sources.map(|h| h.join().expect("overview daemon sources panicked")),
            gh.join().expect("overview gh refresh panicked"),
            tracker.and_then(|h| h.join().expect("overview tracker read panicked")),
        )
    });
    let sources = match (reused, &fresh_sources) {
        (Some(r), _) => r.sources,
        (None, Some(fresh)) => fresh,
        (None, None) => unreachable!("sources are gathered unless reused"),
    };
    let daemon = &sources.daemon;
    let monitoring_view = sources.monitoring.clone();
    let views: Option<&[board::View]> = match reused {
        Some(r) => pm.as_ref().map(|_| r.views),
        None => fresh_views.as_deref(),
    };
    let mut degraded_notes = daemon.degraded.clone();
    if !slugs.is_empty() {
        if let Some(e) = gh_state["error"].as_str() {
            degraded_notes.push(degraded("github", "", e));
        }
    }
    let members = match opts.scope.group.as_deref() {
        Some(root) => Some(group_members(daemon, root)?),
        None => None,
    };

    // ---- daemon rows ----
    let mut needs: Vec<Item> = Vec::new();
    let mut panes_idle = daemon.reachable;
    // An old daemon without `agent_probe` cannot confirm a pane is idle
    // — the drift row must say so instead of vanishing quietly.
    let mut probes_unknown = false;
    for (a, probe) in daemon.agents.iter().zip(&daemon.probes) {
        let project = agent_project(a, &repo_paths);
        needs.extend(agent_items(a, probe, &project, now));
        needs.extend(master_login_item(a, state_dir, &project, now));
        panes_idle &= !probe.holds_drift;
        probes_unknown |= probe.probe_unknown;
    }
    if daemon.reachable {
        needs.extend(platform_effect_items(state_dir, now, opts.probe_timeout));
    }
    needs.extend(relay_attention_items(state_dir, now));

    // ---- tracker rows ----
    // Lowercased headRefName of every open PR — an issue in review
    // counts as "has a PR" when a `cadence/<id-lowercase>-…` branch is
    // open, even without an explicit `pr` ref.
    let mut open_pr_branches: Vec<String> = Vec::new();
    for data in gh_repos.values() {
        for pr in data["prs"].as_array().cloned().unwrap_or_default() {
            if let Some(head) = pr["headRefName"].as_str() {
                open_pr_branches.push(head.to_lowercase());
            }
        }
    }
    // `cadence/<id-lowercase>-` branch prefix → (issue id, owner): a
    // PR row belongs to the issue's owner for `--group`.
    let mut branch_issue: Vec<(String, String, Option<String>)> = Vec::new();
    let mut projects_out = Vec::new();
    if let (Some(pm), Some(views)) = (&pm, views) {
        let mut status_of: HashMap<String, String> = HashMap::new();
        for v in views {
            status_of.insert(v.issue.front.id.clone(), v.status.clone());
            branch_issue.push((
                crate::worktree::layout::issue_branch_prefix(&v.issue.front.id),
                v.issue.front.id.clone(),
                v.issue.front.owner.clone(),
            ));
        }
        let view_of: HashMap<&str, &board::View> = views
            .iter()
            .map(|v| (v.issue.front.id.as_str(), v))
            .collect();
        let line_times = LineTimes::load(&pm.dir, STATUS_CLOCK_BUDGET).ok();
        let mut clock = StatusClock::new(line_times.as_ref());
        let mut intake: Vec<Item> = Vec::new();
        let mut backlog_stale: Vec<Item> = Vec::new();
        let escalations = crate::master::escalations(state_dir);
        // CAD-484: a next-action row hides only when its lane is
        // provably not at rest — the daemon answered and the lane is
        // busy, queued or working. An unreachable daemon or an
        // unlisted alias cannot disprove the flag, so the row stands.
        let lane_working: HashMap<String, bool> = daemon
            .agents
            .iter()
            .zip(&daemon.probes)
            .filter_map(|(a, p)| {
                let alias = a["alias"].as_str()?;
                let queued = a["inbox"]["queued"]
                    .as_i64()
                    .or_else(|| p.show.as_ref().and_then(|s| s["queued"].as_i64()))
                    .unwrap_or(0);
                Some((
                    alias.to_string(),
                    a["state"].as_str() != Some("idle") || p.holds_drift || queued > 0,
                ))
            })
            .collect();
        for v in views {
            let id = v.issue.front.id.as_str();
            let project = v.issue.project.as_str();
            let owner = v.issue.front.owner.as_deref().unwrap_or_default();
            let age = parse_iso(&v.issue.front.created)
                .map(|c| now - c)
                .unwrap_or(0);
            let branch_prefix = crate::worktree::layout::issue_branch_prefix(id);
            let open_pr = v
                .issue
                .front
                .refs
                .iter()
                .any(|r| r.kind == "pr" && r.closed != Some(true))
                || open_pr_branches
                    .iter()
                    .any(|b| b.starts_with(&branch_prefix));
            let gate_tag = v
                .issue
                .front
                .tags
                .iter()
                .any(|t| matches!(t.as_str(), "plan-ready" | "parked" | "idea-stale"));
            if v.status == "review" && !open_pr && !gate_tag {
                needs.push(
                    item(
                        70,
                        "review_no_pr",
                        &format!("{id} in review with no open PR"),
                        age,
                        project,
                        None,
                        &cmd_issue_show(id),
                    )
                    .about("issue", id)
                    .for_agent(owner)
                    .owned_by(Some(owner))
                    .since(clock.since(v)),
                );
            }
            let unblocked = !v.issue.front.blocked_by.is_empty()
                && v.issue
                    .front
                    .blocked_by
                    .iter()
                    .all(|b| status_of.get(b).map(String::as_str) == Some("done"));
            if !matches!(v.status.as_str(), "done" | "dropped") && unblocked {
                // Unblocked when the last blocker reached done; one
                // blocker without a clock leaves the row without one.
                let since = v
                    .issue
                    .front
                    .blocked_by
                    .iter()
                    .map(|b| view_of.get(b.as_str()).and_then(|bv| clock.since(bv)))
                    .collect::<Option<Vec<i64>>>()
                    .and_then(|ts| ts.into_iter().max());
                needs.push(
                    item(
                        80,
                        "blocked_ready",
                        &format!("{id} unblocked — blockers all done"),
                        age,
                        project,
                        None,
                        &cmd_issue_set_ready(id),
                    )
                    .about("issue", id)
                    .for_agent(owner)
                    .owned_by(Some(owner))
                    .since(since),
                );
            }
            // CAD-339 Needs-you: a plan waiting for the operator's
            // decision, and every open question the master escalated —
            // with the master's summary, the question and its options.
            if let Some(plan) = v
                .issue
                .front
                .plan
                .as_ref()
                .filter(|p| p.state == "proposed")
            {
                let since = parse_iso(&plan.proposed_at);
                let mut row = item(
                    25,
                    "plan",
                    &format!(
                        "{id} plan proposed by {} — {} ({} tickets)",
                        plan.proposed_by,
                        v.issue.front.title,
                        plan.tickets.len()
                    ),
                    since.map_or(age, |t| now - t),
                    project,
                    None,
                    &format!("cadence plan show {id} && cadence plan approve {id}"),
                )
                .about("issue", id)
                .since(since)
                .with_display(Some(&v.issue.front.title), None);
                row.json["plan"] = json!({"epic": id, "proposed_by": plan.proposed_by,
                                          "tickets": plan.tickets});
                needs.push(row);
            }
            // Only the daemon's escalation record puts a question here —
            // a report file never can (review round 1, I3); reports are
            // parsed only for tickets that have one.
            let escalated_here = escalations.keys().any(|k| k.starts_with(&format!("{id}/")));
            let open = if escalated_here {
                issue::task_report::open_questions(&v.issue.dir, id)
            } else {
                vec![]
            };
            for q in open {
                let key = format!("{id}/{}", q["name"].as_str().unwrap_or_default());
                let Some(up) = escalations.get(&key).and_then(Value::as_object) else {
                    continue;
                };
                let since = q["at"].as_str().and_then(parse_iso);
                let mut row = item(
                    20,
                    "question",
                    &format!(
                        "{id} question from {} — {}",
                        q["agent"].as_str().unwrap_or_default(),
                        q["impact"].as_str().unwrap_or_default()
                    ),
                    since.map_or(age, |t| now - t),
                    project,
                    None,
                    &format!(
                        "cadence issue show {id}  # answer: cadence report file --task {id} \
                         --kind answer (answers: {})",
                        q["name"].as_str().unwrap_or_default()
                    ),
                )
                .about(
                    "report",
                    &format!("{id}/{}", q["name"].as_str().unwrap_or_default()),
                )
                .for_agent(q["agent"].as_str().unwrap_or_default())
                .since(since)
                .with_display(
                    // A refused summary falls back to the body.
                    up.get("summary")
                        .and_then(Value::as_str)
                        .filter(|s| display_text(s, SHORT_TITLE_MAX).is_some())
                        .or(q["body"].as_str()),
                    q["impact"].as_str(),
                );
                row.json["question"] = json!({
                    "issue": id, "report": q["name"], "agent": q["agent"],
                    "options": q["options"], "impact": q["impact"], "body": q["body"],
                });
                row.json["summary"] = up.get("summary").cloned().unwrap_or(Value::Null);
                row.json["escalated_by"] = up.get("by").cloned().unwrap_or(Value::Null);
                needs.push(row);
            }
            // CAD-477: a blocked report the checkup escalated is the
            // operator's to unblock — one row while it is still the
            // issue's open state; a newer `done` report or a done
            // issue clears it.
            if escalated_here {
                let reports = issue::task_report::list(&v.issue.dir, id);
                for b in reports
                    .iter()
                    .filter(|r| r["kind"].as_str() == Some("blocked"))
                {
                    let name = b["name"].as_str().unwrap_or_default();
                    let Some(up) = escalations
                        .get(&format!("{id}/{name}"))
                        .and_then(Value::as_object)
                    else {
                        continue;
                    };
                    if !issue::task_report::blocked_open(&reports, b, &v.status) {
                        continue;
                    }
                    let since = b["at"].as_str().and_then(parse_iso);
                    let mut row = item(
                        20,
                        "blocked",
                        &format!(
                            "{id} blocked — {} reports it cannot proceed",
                            b["agent"].as_str().unwrap_or_default()
                        ),
                        since.map_or(age, |t| now - t),
                        project,
                        None,
                        &format!("cadence issue show {id}"),
                    )
                    .about("report", &format!("{id}/{name}"))
                    .for_agent(b["agent"].as_str().unwrap_or_default())
                    .since(since);
                    row.json["blocked"] = json!({
                        "issue": id, "report": name, "agent": b["agent"], "body": b["body"],
                    });
                    row.json["summary"] = up.get("summary").cloned().unwrap_or(Value::Null);
                    row.json["escalated_by"] = up.get("by").cloned().unwrap_or(Value::Null);
                    needs.push(row);
                }
            }
            // CAD-484: the checkup's one Needs-you when an idle lane
            // had no safe next step. The record is keyed
            // `{issue}/next-action`; the row hides the moment the lane
            // provably works again — a queued kickoff, a running turn,
            // a busy pane — and never needs a record delete.
            if let Some(up) = escalations
                .get(&format!("{id}/next-action"))
                .and_then(Value::as_object)
                .filter(|u| u["kind"].as_str() == Some("next_action"))
            {
                let agent = up["agent"].as_str().unwrap_or_default();
                if !lane_working.get(agent).copied().unwrap_or(false) {
                    let since = up["at"].as_str().and_then(parse_iso);
                    let mut row = item(
                        20,
                        "next_action",
                        &format!(
                            "{id} — {} is idle with no safe next step — {}",
                            agent,
                            up["summary"].as_str().unwrap_or_default()
                        ),
                        since.map_or(age, |t| now - t),
                        project,
                        None,
                        &format!("cadence issue show {id}"),
                    )
                    .about("issue", id)
                    .for_agent(agent)
                    .since(since);
                    row.json["next_action"] = json!({"issue": id, "agent": agent});
                    row.json["summary"] = up.get("summary").cloned().unwrap_or(Value::Null);
                    row.json["escalated_by"] = up.get("by").cloned().unwrap_or(Value::Null);
                    needs.push(row);
                }
            }
            // CAD-139: the idea pipeline stops here. The operator
            // approves, rejects, or parks; nothing else is dispatched.
            if v.status == "review" && v.issue.front.tags.iter().any(|t| t == "plan-ready") {
                needs.push(
                    item(
                        20,
                        "idea_plan",
                        &format!(
                            "idea plan ready for your decision — {id} {}",
                            v.issue.front.title
                        ),
                        age,
                        project,
                        None,
                        &format!("cadence idea decide {id} approve"),
                    )
                    .about("issue", id)
                    .since(clock.since(v)),
                );
            }
            if v.status == "backlog"
                && v.issue.front.tags.iter().any(|t| t == "idea")
                && v.issue.front.duplicate_of.is_some()
            {
                let other = v.issue.front.duplicate_of.as_deref().unwrap_or("");
                needs.push(
                    item(
                        20,
                        "idea_duplicate",
                        &format!("{id} looks like {other} — decide whether to keep it"),
                        age,
                        project,
                        None,
                        &format!("cadence issue show {id}"),
                    )
                    .about("issue", id)
                    .since(clock.since(v)),
                );
            }
            // CAD-812: a `backlog`/`ready` leaf the groom pass flagged
            // `needs-triage` — the requirement may have drifted. Surfaces
            // under `blocked_ready` (a flag, not a pick gate): oldest
            // first, for the owner to re-confirm or re-scope. Collected
            // and capped like intake so a pile of stale tickets cannot
            // bury real work.
            if matches!(v.status.as_str(), "backlog" | "ready")
                && v.issue.front.tags.iter().any(|t| t == "needs-triage")
            {
                let since = v.issue.front.last_groomed_at.as_deref().and_then(parse_iso);
                backlog_stale.push(
                    item(
                        82,
                        "backlog_stale",
                        &format!("{id} needs-triage — groom flagged drift"),
                        age,
                        project,
                        None,
                        &cmd_issue_show(id),
                    )
                    .about("issue", id)
                    .for_agent(owner)
                    .owned_by(Some(owner))
                    .since(since),
                );
            }
            // `cadence report` intake: a backlog-tagged row surfaces
            // until triage moves it off backlog — the effective status
            // (notes-derived counts too) is what clears it.
            if v.status == "backlog" && v.issue.front.tags.iter().any(|t| t == "intake") {
                let kind_tag = v
                    .issue
                    .front
                    .kind
                    .as_deref()
                    .or_else(|| {
                        v.issue
                            .front
                            .tags
                            .iter()
                            .find(|t| *t != "intake")
                            .map(String::as_str)
                    })
                    .unwrap_or("intake");
                intake.push(
                    item(
                        85,
                        "intake",
                        &format!("{id} {kind_tag} report — {}", v.issue.front.title),
                        age,
                        project,
                        None,
                        &format!("cadence report show {id}"),
                    )
                    .about("issue", id)
                    .for_agent(owner)
                    .owned_by(Some(owner)),
                );
            }
        }
        // Cap the intake block — hundreds of untriaged reports must not
        // bury real work. Oldest first, then one summary row.
        intake.sort_by_key(|i| std::cmp::Reverse(i.age));
        let intake_extra = intake
            .len()
            .checked_sub(report::NEEDS_ME_CAP)
            .filter(|n| *n > 0);
        if intake_extra.is_some() {
            intake.truncate(report::NEEDS_ME_CAP);
        }
        // Clocks only for the rows that surface — one read each.
        for it in &mut intake {
            let since = view_of
                .get(it.subject.1.as_str())
                .and_then(|v| clock.since(v));
            it.set_since(since);
        }
        if let Some(extra) = intake_extra {
            intake.push(
                item(
                    85,
                    "intake",
                    &format!("… {extra} more intake reports"),
                    0,
                    "",
                    None,
                    "cadence report ls",
                )
                .about("report", "intake-overflow"),
            );
        }
        // Cap the stale-backlog block the same way — oldest first, one
        // summary row past the cap.
        backlog_stale.sort_by_key(|i| std::cmp::Reverse(i.age));
        let stale_extra = backlog_stale
            .len()
            .checked_sub(report::NEEDS_ME_CAP)
            .filter(|n| *n > 0);
        if stale_extra.is_some() {
            backlog_stale.truncate(report::NEEDS_ME_CAP);
        }
        if let Some(extra) = stale_extra {
            backlog_stale.push(
                item(
                    82,
                    "backlog_stale",
                    &format!("… {extra} more stale backlog items"),
                    0,
                    "",
                    None,
                    "cadence issue ls --project <p>",
                )
                .about("issue", "backlog-stale-overflow"),
            );
        }
        needs.extend(backlog_stale);
        needs.extend(intake);
        let mut delivery = delivery_items(state_dir, now);
        for row in delivery
            .iter_mut()
            .filter(|r| r.json["kind"] == "merge_decision")
        {
            let title = row.json["merge"]["issue"]
                .as_str()
                .and_then(|i| view_of.get(i))
                .map(|bv| bv.issue.front.title.clone());
            row.set_display(title.as_deref(), None);
        }
        needs.extend(delivery);
        if !clock.skipped.is_empty() {
            degraded_notes.push(degraded(
                "tracker_status_time",
                "",
                format!(
                    "{} issue(s) past the status-time budget — those rows escalate by owner only",
                    clock.skipped.len()
                ),
            ));
        }
        // CAD-383: in-flight claims per project, with their age.
        let claim_clock = claim::Clock::new(line_times.as_ref());
        // CAD-378: the review loop's recorded PR per issue, for lanes
        // whose tracker carries no `pr` ref.
        let delivery_prs: std::collections::BTreeMap<String, String> =
            crate::delivery::records(state_dir)
                .into_values()
                .filter(|r| !r.state.terminal())
                .filter_map(|r| r.pr.map(|pr| (r.issue, pr)))
                .collect();
        for p in &projects {
            if opts.scope.project.as_deref().is_some_and(|k| k != p.key) {
                continue;
            }
            let mut open_by_status = serde_json::Map::new();
            let mut oldest_review: Option<i64> = None;
            let mut claims: Vec<Value> = Vec::new();
            for v in views.iter().filter(|v| v.issue.project == p.key) {
                if matches!(v.status.as_str(), "done" | "dropped") {
                    continue;
                }
                let front = &v.issue.front;
                if matches!(v.status.as_str(), "doing" | "review")
                    && !claim::holders(front).is_empty()
                {
                    let since = claim_clock.since(&p.key, front);
                    let mut row = claim::row(&p.key, front, since, now);
                    row["status"] = json!(v.status);
                    claims.push(row);
                }
                let n = open_by_status
                    .get(&v.status)
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                open_by_status.insert(v.status.clone(), json!(n + 1));
                if v.status == "review" {
                    let age = parse_iso(&v.issue.front.created)
                        .map(|c| now - c)
                        .unwrap_or(0);
                    oldest_review = Some(oldest_review.map_or(age, |o| o.max(age)));
                }
            }
            // CAD-378: open lanes, the code areas they plan or change,
            // and overlaps (the board's overlay); a lane with a PR that
            // changes an area owned by someone else is a Needs-you row
            // until the owner's PM or the operator acks it.
            let (areas, areas_error) = issue::areas::load_or_error(&pm.dir, &p.key);
            let project_issues: Vec<&issue::board::Issue> = views
                .iter()
                .filter(|v| v.issue.project == p.key)
                .map(|v| &v.issue)
                .collect();
            let lanes = issue::areas::open_lanes(state_dir, &project_issues, &delivery_prs);
            let acks = issue::areas::acks(state_dir);
            let has_pr = |l: &issue::areas::Lane| {
                let prefix = crate::worktree::layout::issue_branch_prefix(&l.issue);
                open_pr_branches.iter().any(|b| b.starts_with(&prefix))
            };
            for need in issue::areas::ack_needs(&areas, &lanes, &acks, has_pr) {
                let owner = need.area.pm.clone();
                let mut row = item(
                    28,
                    "area_ack",
                    &need.title(),
                    0,
                    &need.project,
                    need.pr.as_deref(),
                    &issue::areas::cmd_ack(&need.issue, &need.area.name),
                )
                .about("issue", &need.issue)
                .for_agent(owner.as_deref().unwrap_or_default())
                .owned_by(Some(owner.as_deref().unwrap_or(inbox::OPERATOR)));
                row.json["area"] = json!({
                    "issue": need.issue, "area": need.area.name, "owner": need.area.owner(),
                    "pm": need.area.pm, "files": need.files, "pr": need.pr,
                    "worker": need.worker,
                });
                needs.push(row);
            }
            projects_out.push(json!({
                "key": p.key,
                "open_by_status": open_by_status,
                "oldest_review_age": oldest_review,
                "claims": claims,
                "lanes": issue::areas::overlay(&areas, &lanes),
                "areas": areas.iter().map(|a| a.name.clone()).collect::<Vec<_>>(),
                "areas_error": areas_error,
            }));
        }
        // Tracker behind its upstream — local refs only, never a fetch.
        if let Ok(behind) = git_text(
            &pm.dir,
            &[
                "rev-list".into(),
                "--count".into(),
                "HEAD..@{upstream}".into(),
            ],
        ) {
            if let Ok(n) = behind.trim().parse::<i64>() {
                if n > 0 {
                    needs.push(
                        item(
                            110,
                            "tracker_behind",
                            &format!("tracker {n} commit(s) behind upstream"),
                            0,
                            "",
                            None,
                            CMD_ISSUE_SYNC,
                        )
                        .about("tracker", "pm"),
                    );
                }
            }
        }
    }

    // ---- GitHub rows: merge-ready, verdict-less, main CI ----
    let mut main_ci: Vec<Value> = Vec::new();
    for (slug, data) in &gh_repos {
        let project = slug_project.get(slug).cloned().unwrap_or_default();
        for pr in data["prs"].as_array().cloned().unwrap_or_default() {
            let n = pr["number"].as_i64().unwrap_or(0);
            let title = pr["title"].as_str().unwrap_or("");
            let url = pr["url"].as_str().map(str::to_string);
            let rollup = pr["statusCheckRollup"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let age = pr["updatedAt"]
                .as_str()
                .and_then(parse_iso)
                .map(|u| now - u)
                .unwrap_or(0);
            let head_ref = pr["headRefName"].as_str().unwrap_or("").to_lowercase();
            let owner = branch_issue
                .iter()
                .find(|(prefix, _, _)| head_ref.starts_with(prefix.as_str()))
                .and_then(|(_, _, owner)| owner.clone())
                .unwrap_or_default();
            let subject = format!("{slug}#{n}");
            // Rollup times for this head (CAD-253): a CheckRun's
            // `completedAt`, a status context's `startedAt`.
            let (first_check, last_check) = rollup_span(&rollup);
            match verdict_state(&rollup).as_deref() {
                Some("SUCCESS") if checks_green(&rollup) => {
                    // The verdict binds this head — a push after it
                    // makes the copied command refuse instead of
                    // merging an unreviewed head.
                    let head = pr["headRefOid"].as_str().unwrap_or("");
                    needs.push(
                        item(
                            10,
                            "merge",
                            &format!("PR #{n} {title} — verdict pass, checks green"),
                            age,
                            &project,
                            url.as_deref(),
                            &format!(
                                "gh pr merge {n} --repo {slug} --squash --admin --match-head-commit {head}"
                            ),
                        )
                        .about("pr", &subject)
                        .for_agent(&owner)
                        .owned_by(Some(&owner))
                        // Merge-ready once the last check or verdict landed.
                        .since(last_check),
                    );
                }
                Some("SUCCESS") | Some("FAILURE") | Some("ERROR") => {}
                _ => needs.push(
                    item(
                        60,
                        "pr_no_verdict",
                        &format!("PR #{n} {title} — no verdict"),
                        age,
                        &project,
                        url.as_deref(),
                        &format!("gh pr view {n} --repo {slug}"),
                    )
                    .about("pr", &subject)
                    .for_agent(&owner)
                    .owned_by(Some(&owner))
                    // Verdict-less since this head's first check started;
                    // a head with no checks yet has no clock.
                    .since(first_check),
                ),
            }
        }
        let clone = slug_clone.get(slug).map(PathBuf::as_path);
        let (view, rows) = main_ci_view(slug, &project, data, clone, now);
        if let Some(e) = view["error"].as_str() {
            degraded_notes.push(degraded("github_ci", slug, e));
        }
        if !view.is_null() && opts.scope.project.as_deref().is_none_or(|k| k == project) {
            main_ci.push(view);
        }
        needs.extend(rows);
    }
    main_ci.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));

    // ---- deploy drift: is what we merged actually running ----
    let info = daemon.info.clone();
    let mut drift = if !daemon.reachable {
        json!({"matched": false, "reason": "daemon unreachable — cannot tell"})
    } else if info.is_none() {
        // Reachable but predates `daemon_info` — the build commit is
        // unreadable, so drift is unknowable, never zero.
        json!({
            "matched": false,
            "reason": "daemon build unknown (daemon predates daemon_info) — restart to enable drift",
        })
    } else {
        match build_repo_match(&projects) {
            None => json!({
                "matched": false,
                "build_commit": info.as_ref().map(|i| i["build_commit"].clone()).unwrap_or(json!("unknown")),
                "reason": "no tracker repo matches the build — cannot tell",
            }),
            Some((key, repo)) => {
                let build = info
                    .as_ref()
                    .and_then(|i| i["build_commit"].as_str())
                    .unwrap_or("unknown");
                let mut d = compute_drift(&repo, build);
                d["matched"] = json!(true);
                d["project"] = json!(key);
                d
            }
        }
    };
    if drift["known"].as_bool().unwrap_or(false) && drift["count"].as_i64().unwrap_or(0) > 0 {
        if panes_idle {
            let n = drift["count"].as_i64().unwrap_or(0);
            let project = drift["project"].as_str().unwrap_or("");
            needs.push(
                item(
                    50,
                    "drift",
                    &format!("{n} merged commit(s) not running — all panes idle"),
                    0,
                    project,
                    None,
                    CMD_UPGRADE_LATEST_MAIN,
                )
                .about("deploy", project),
            );
        } else {
            // Held back, but say why — a busy pane is different from
            // a daemon that cannot answer `agent_probe` at all.
            drift["held"] = json!(if probes_unknown {
                "cannot tell whether panes are idle — daemon predates agent_probe"
            } else {
                "a pane is busy — restart only when idle"
            });
        }
    }

    // CAD-615: a pending permission request is the operator's to decide.
    for req in crate::master_perm::board_requests(state_dir, now).unwrap_or_default() {
        let age = (now - req.created).max(0);
        let command = req.argv.join(" ");
        let title = if req.decision_label.is_empty() {
            format!(
                "master wants to run: {command} — risk {}",
                match req.risk {
                    crate::master_perm::Risk::Low => "low",
                    crate::master_perm::Risk::Medium => "medium",
                    crate::master_perm::Risk::High => "high",
                }
            )
        } else {
            format!("{} — {command}", req.decision_label)
        };
        let mut row = item(
            if req.status == "pending" { 15 } else { 40 },
            "master_permission",
            &title,
            age,
            "",
            None,
            &format!("cadence master allow-once {}", req.id),
        )
        .about("permission", &req.id)
        .since(Some(req.created))
        .with_display(
            // The title is the label, else the sanitized reason (omitted
            // when refused); the full reason is shown separately as data.
            Some(req.decision_label.as_str())
                .filter(|l| !l.is_empty())
                .or(Some(req.reason.as_str())),
            (!req.decision_label.is_empty()).then_some(req.reason.as_str()),
        );
        row.json["permission"] = crate::master_perm::request_json(&req);
        row.json["reason"] = json!(req.reason);
        needs.push(row);
    }

    classify_needs(
        &mut needs,
        &Owners::new(daemon.reachable, &daemon.agents),
        now,
        ESCALATE_AFTER_SECS,
    );
    let mut needs = scope_rows(
        merge_by_subject(needs),
        opts.scope.project.as_deref(),
        members.as_deref(),
    );
    sort_needs(&mut needs);
    // CAD-574: the operator's dismissals suppress their subjects for
    // every reader of this build — a snooze's clock is judged here.
    let dismissed = crate::needs_dismiss::dismissed(state_dir);
    if !dismissed.is_empty() {
        needs.retain(|i| {
            !crate::needs_dismiss::row_suppressed(
                &json!({"subject": {"kind": i.subject.0, "id": i.subject.1.as_str()},
                        "since": i.since, "age": i.age}),
                &dismissed,
                now,
            )
        });
    }
    let daemon_json = match info {
        Some(mut i) => {
            i["reachable"] = json!(daemon.reachable);
            i
        }
        None if daemon.reachable => json!({
            "reachable": true,
            "info": "daemon predates daemon_info — build identity unreadable",
        }),
        None => json!({"reachable": false}),
    };
    Ok(json!({
        "needs_me": needs.iter().map(|i| i.json.clone()).collect::<Vec<_>>(),
        "drift": drift,
        "projects": projects_out,
        "github": gh_state,
        "main_ci": main_ci,
        "daemon": daemon_json,
        "monitoring": monitoring_view,
        "degraded": degraded_notes,
        "scope": {"project": opts.scope.project, "group": opts.scope.group},
        "generated_at": now,
    }))
}

// ---------- shared with `cadence session` ----------

/// `session`'s reconcile/handoff share the gh fetch (and its cache)
/// rather than re-running `gh pr list` — same slug set, same data.
pub(crate) fn github_repos(state_dir: &Path, slugs: &[String]) -> (HashMap<String, Value>, Value) {
    github(state_dir, slugs)
}

/// The cache body only — never fetches, never writes. `session end
/// --dry-run` must write nothing at all, cache included, so it reads
/// what a previous real fetch left and reports `unavailable` when the
/// cache is empty or covers a different slug set.
pub(crate) fn github_repos_cached(
    state_dir: &Path,
    slugs: &[String],
) -> (HashMap<String, Value>, Value) {
    match read_cache(&cache_file(state_dir)) {
        Some(c) if c.slugs == slugs => (
            c.repos,
            json!({"state": "cached", "at": c.at, "as_of": c.at}),
        ),
        _ => (HashMap::new(), json!({"state": "unavailable"})),
    }
}

/// Drift of an arbitrary commit against the repo's default ref —
/// `session start` measures the *binary* build this way.
pub(crate) fn drift_of(repo: &Path, commit: &str) -> Value {
    compute_drift(repo, commit)
}

/// Which tracker project owns the repo this binary was built from.
pub(crate) fn build_repo_match_pub(projects: &[project::Project]) -> Option<(String, PathBuf)> {
    build_repo_match(projects)
}

/// Verdict + checks on a PR rollup, for the session-end handoff.
pub(crate) fn verdict_state_pub(rollup: &[Value]) -> Option<String> {
    verdict_state(rollup)
}

pub(crate) fn checks_green_pub(rollup: &[Value]) -> bool {
    checks_green(rollup)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CAD-1219: the display fields clip, strip and refuse command text.
    #[test]
    fn display_text_clips_strips_and_refuses_commands() {
        assert_eq!(
            display_text("  Send\n\temail\u{7}  to   212 customers ", 50).as_deref(),
            Some("Send email to 212 customers")
        );
        let long = "é".repeat(80);
        let clipped = display_text(&long, SHORT_TITLE_MAX).unwrap();
        assert_eq!(clipped.chars().count(), SHORT_TITLE_MAX);
        assert!(clipped.ends_with('…'));
        for bad in [
            "run `rm -rf x` now",
            "cadence issue set X status=done",
            "Cadence  issue list",
            "run cadence --state-dir x",
            "c\u{430}dence issue set X",
            "echo $(id)",
            "  \n\u{0} ",
            "cadence\u{200B} issue set X status=done",
            "please run: cadence issue set X",
            "echo ${HOME}",
            "run \u{FF40}id\u{FF40}",
        ] {
            assert_eq!(display_text(bad, 50), None, "{bad:?}");
        }
        assert_eq!(
            display_text("\u{202E}Send\u{200B} mail\u{2066}", 50).as_deref(),
            Some("Send mail")
        );
        for (raw, want) in [
            ("a\u{13430}b\u{1BCA0}c\u{34F}d\u{3164}e", "abcde"),
            ("Cadence board shows plans", "Cadence board shows plans"),
            ("## Heading words", "Heading words"),
        ] {
            assert_eq!(display_text(raw, 50).as_deref(), Some(want), "{raw:?}");
        }
        assert_eq!(
            display_sentence("Uses tools, e.g. git. Then more.", WHY_MAX).as_deref(),
            Some("Uses tools, e.g. git.")
        );
        assert_eq!(
            display_sentence("It sends mail. Then more words follow.", WHY_MAX).as_deref(),
            Some("It sends mail.")
        );
        let mut row = item(1, "plan", "t", 0, "", None, "c");
        row.set_display(Some("cadence plan approve X"), Some(""));
        assert!(row.json.get("short_title").is_none() && row.json.get("why").is_none());
    }

    #[test]
    fn iso_parses() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso("2000-01-01T00:00:00Z"), Some(946684800));
        assert_eq!(parse_iso("2026-09-19T05:33:43"), None);
        assert_eq!(parse_iso(""), None);
        assert_eq!(parse_iso("not a date at all!!!!"), None);
    }

    #[test]
    fn pr_numbers() {
        assert_eq!(pr_number("board: tailnet sharing (#51)"), Some(51));
        assert_eq!(pr_number("no number"), None);
        assert_eq!(pr_number("mid (#5) subject"), None);
        assert_eq!(pr_number("edge (#abc)"), None);
    }

    #[test]
    fn rollup_verdict_and_green() {
        let rollup = vec![
            json!({"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SUCCESS"}),
            json!({"__typename":"StatusContext","context":"qa-verdict","state":"SUCCESS"}),
        ];
        assert_eq!(verdict_state(&rollup).as_deref(), Some("SUCCESS"));
        assert!(checks_green(&rollup));
        let red = vec![
            json!({"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"FAILURE"}),
            json!({"__typename":"StatusContext","context":"qa-verdict","state":"SUCCESS"}),
        ];
        assert!(!checks_green(&red));
        let pending =
            vec![json!({"__typename":"StatusContext","context":"qa-verdict","state":"PENDING"})];
        assert!(checks_green(&pending));
        assert_eq!(verdict_state(&pending).as_deref(), Some("PENDING"));
        assert_eq!(verdict_state(&[]), None);
        assert!(checks_green(&[]));
    }

    #[test]
    fn ordering_rank_then_age() {
        let mut v = vec![
            item(60, "pr_no_verdict", "b", 5, "", None, "c"),
            item(10, "merge", "a", 1, "", None, "c"),
            item(60, "pr_no_verdict", "c", 50, "", None, "c"),
        ];
        sort_needs(&mut v);
        let kinds: Vec<&str> = v.iter().map(|i| i.json["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["merge", "pr_no_verdict", "pr_no_verdict"]);
        assert_eq!(v[1].json["title"], "c"); // older first within a kind
    }

    /// CAD-252: an agent that is both stalled and silently ended is one
    #[test]
    fn merge_ready_decision_survives_an_older_no_safe_next_step_alert() {
        let state = tempfile::TempDir::new().unwrap();
        let sha = "a".repeat(40);
        let mut rec = crate::delivery::Record::new("DEM-2", "demo", "w1", 0);
        rec.state = crate::delivery::State::Passed;
        rec.pr = Some("https://github.com/acme/demo/pull/1".into());
        rec.verdict = Some(crate::delivery::VerdictRec {
            verdict: "pass".into(),
            sha: sha.clone(),
            reviewer: "r1".into(),
            summary: "reviewed".into(),
            report: "DEM-2/reports/r1.md".into(),
            at: 0,
        });
        rec.observed = Some(crate::delivery::Observed {
            head: sha.clone(),
            pr_state: "OPEN".into(),
            ci_green: true,
            ..crate::delivery::Observed::default()
        });
        crate::delivery::save(
            state.path(),
            &std::collections::BTreeMap::from([("DEM-2".into(), rec.clone())]),
        )
        .unwrap();
        let mut rows = delivery_items(state.path(), 10);
        rows.push(
            item(
                20,
                "next_action",
                "no safe next step",
                9,
                "demo",
                None,
                "cadence issue show DEM-2",
            )
            .about("issue", "DEM-2"),
        );
        let merged = merge_by_subject(rows);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].json["kind"], "merge_decision");
        assert_eq!(merged[0].json["merge"]["sha"], sha);
        assert_eq!(merged[0].json["merge"]["reviewer"], "r1");
        assert_eq!(merged[0].json["causes"][1]["cause"], "next_action");
        rec.observed.as_mut().unwrap().ci_green = false;
        crate::delivery::save(
            state.path(),
            &std::collections::BTreeMap::from([("DEM-2".into(), rec)]),
        )
        .unwrap();
        assert!(
            delivery_items(state.path(), 10).is_empty(),
            "precedence must not bypass CI"
        );
    }

    /// row with two causes, most severe first; other subjects stay put.
    #[test]
    fn stalled_and_silent_end_merge_into_one_row() {
        let now = 1_000_000;
        let agent = json!({
            "alias": "w1", "provider": "devin", "endpoint_kind": "pty",
            "state": "busy", "updated": (now - 30) as f64,
            "stalled": true, "silent_secs": 900,
            "silent_ended": true, "ended_secs": 600,
        });
        let mut rows = agent_items(&agent, &AgentProbe::default(), "cadence", now);
        assert_eq!(rows.len(), 2, "two raw causes");
        rows.push(
            item(
                70,
                "review_no_pr",
                "CAD-1 in review",
                5,
                "cadence",
                None,
                "c",
            )
            .about("issue", "CAD-1"),
        );
        let merged = merge_by_subject(rows);
        assert_eq!(merged.len(), 2, "one row per subject");
        let w1 = &merged[0].json;
        assert_eq!(w1["subject"], json!({"kind": "agent", "id": "w1"}));
        assert_eq!(w1["kind"], "stalled", "primary cause keeps `kind`");
        assert_eq!(w1["cause"], "stalled");
        let causes: Vec<&str> = w1["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["cause"].as_str().unwrap())
            .collect();
        assert_eq!(causes, ["stalled", "silent_end"]);
        assert_eq!(w1["causes"][1]["command"], cmd_agent_attach("w1"));
        assert_eq!(w1["project"], "cadence");
        // A lone row still carries its one cause.
        assert_eq!(merged[1].json["causes"].as_array().unwrap().len(), 1);
        assert_eq!(merged[1].json["subject"]["kind"], "issue");
    }

    /// Severity decides the primary cause, not emit order; a stale inbox
    /// merges with its unread row and names its owner.
    #[test]
    fn merge_orders_causes_by_severity_and_keeps_stale_owner() {
        let now = 1_000_000;
        let inbox = json!({
            "alias": "obs", "provider": "inbox", "endpoint_kind": "inbox",
            "state": "idle", "updated": now as f64,
            "inbox": {"queued": 60},
            "inbox_health": {"stale": true, "unread": 60,
                             "oldest_unread_age_secs": 90_000, "owner": "pm"},
        });
        let merged = merge_by_subject(agent_items(&inbox, &AgentProbe::default(), "", now));
        assert_eq!(merged.len(), 1);
        let row = &merged[0];
        assert_eq!(row.json["kind"], "inbox_stale", "{}", row.json);
        assert_eq!(row.json["owner"], "pm");
        assert!(row.json["title"].as_str().unwrap().contains("60 unread"));
        let causes: Vec<&str> = row.json["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["cause"].as_str().unwrap())
            .collect();
        assert_eq!(causes, ["inbox_stale", "inbox_unread"]);
        // Attributed to the owner for `--group pm`, and to the inbox.
        assert_eq!(row.agents, ["obs", "pm"]);
    }

    // ---- CAD-253: needs-me audience from owner liveness and the
    // unhandled clock ----

    const NOW: i64 = 1_000_000;

    /// A fenced worker `w1` whose PM is `pm`: its turn went `unknown`
    /// `fenced_ago` seconds before [`NOW`]. Its last state write is two
    /// days old — the clock must never read it.
    fn fenced_worker(fenced_ago: i64) -> (Value, AgentProbe) {
        let a = json!({
            "alias": "w1", "provider": "devin", "endpoint_kind": "pty",
            "state": "attention", "updated": (NOW - 2 * 86_400) as f64,
            "params": {"upstream": "pm"}, "dead": false,
        });
        let probe = AgentProbe {
            show: Some(json!({"messages": [
                {"id": "m0", "state": "completed", "completed": (NOW - 3 * 86_400) as f64},
                {"id": "m1", "state": "unknown", "completed": (NOW - fenced_ago) as f64},
            ]})),
            ..AgentProbe::default()
        };
        (a, probe)
    }

    fn pm_row(dead: bool, state: &str) -> Value {
        json!({"alias": "pm", "provider": "devin", "endpoint_kind": "pty",
               "state": state, "dead": dead})
    }

    /// Classify then merge, the way `overview_with` does.
    fn resolve(mut rows: Vec<Item>, agents: &[Value]) -> Vec<Value> {
        classify_needs(
            &mut rows,
            &Owners::new(true, agents),
            NOW,
            ESCALATE_AFTER_SECS,
        );
        merge_by_subject(rows).into_iter().map(|i| i.json).collect()
    }

    fn fenced_rows(fenced_ago: i64, pm: Option<Value>) -> Vec<Value> {
        let (w1, probe) = fenced_worker(fenced_ago);
        let mut agents = vec![w1.clone()];
        agents.extend(pm);
        resolve(agent_items(&w1, &probe, "cadence", NOW), &agents)
    }

    /// CAD-374: only the operator may unfence or reconcile, so a fenced
    /// agent's row is the operator's from the start — live PM or dead.
    #[test]
    fn fenced_agent_goes_to_the_operator_whatever_its_pm() {
        for (pm, ago) in [
            (pm_row(true, "idle"), 120),
            (pm_row(false, "idle"), 10 * 60),
        ] {
            let out = fenced_rows(ago, Some(pm));
            assert_eq!(out.len(), 1);
            assert_eq!(out[0]["kind"], "fenced");
            assert_eq!(out[0]["since"], NOW - ago, "{}", out[0]);
            assert_eq!(out[0]["audience"], "operator", "{}", out[0]);
            assert_eq!(out[0]["audience_reason"], "operator decision");
        }
    }

    /// CAD-413: a failed auto-resume is one row naming the agent and
    /// the waiting message, clocked from the failure — it replaces the
    /// generic `fenced` row the failed open left in `attention`.
    #[test]
    fn failed_auto_resume_names_agent_and_waiting_message() {
        let (mut w1, probe) = fenced_worker(120);
        w1["auto_resume_failed"] = json!({
            "at": (NOW - 300) as f64, "message": "m-wait", "queued": 1,
            "reason": "fake open refused", "resume": "cadence agent resume w1",
        });
        let pm = pm_row(false, "idle");
        let out = resolve(agent_items(&w1, &probe, "cadence", NOW), &[w1.clone(), pm]);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0]["kind"], "auto_resume_failed", "{}", out[0]);
        let title = out[0]["title"].as_str().unwrap();
        assert!(title.contains("agent w1"), "{title}");
        assert!(title.contains("message m-wait waiting"), "{title}");
        assert!(title.contains("fake open refused"), "{title}");
        assert_eq!(out[0]["command"], "cadence agent resume w1");
        assert_eq!(out[0]["since"], NOW - 300, "{}", out[0]);
    }

    /// CAD-477: a stopped agent still holding queued work is one
    /// operator row — the idle timer's own stop is auto-resume's to
    /// restart, so it shows none; an empty queue is nobody's row.
    #[test]
    fn stopped_agent_with_queued_work_is_an_operator_row() {
        let pm = pm_row(false, "idle");
        let a = |queued: i64| {
            json!({
                "alias": "w1", "provider": "devin", "endpoint_kind": "pty",
                "state": "stopped", "updated": (NOW - 600) as f64,
                "params": {"upstream": "pm"}, "dead": false,
                "inbox": {"queued": queued},
            })
        };
        let out = resolve(
            agent_items(&a(2), &AgentProbe::default(), "cadence", NOW),
            &[a(2), pm.clone()],
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0]["kind"], "stopped", "{}", out[0]);
        assert_eq!(out[0]["audience"], "operator", "{}", out[0]);
        assert_eq!(out[0]["command"], "cadence agent resume w1");
        // The idle timer's own stop: auto-resume owns it — no row.
        let mut auto = a(2);
        auto["auto_stopped"] = json!({"at": (NOW - 60) as f64, "label": "auto-stopped"});
        let out = resolve(
            agent_items(&auto, &AgentProbe::default(), "cadence", NOW),
            &[auto.clone(), pm.clone()],
        );
        assert_eq!(out.len(), 0, "{out:?}");
        // Nothing queued — nobody's row.
        let out = resolve(
            agent_items(&a(0), &AgentProbe::default(), "cadence", NOW),
            &[a(0), pm],
        );
        assert_eq!(out.len(), 0, "{out:?}");
    }

    /// The clock is when the issue entered its status, not its age: an
    /// issue created 30 days ago that went to review 10 minutes ago is
    /// fresh team work; 74 minutes in review escalates.
    #[test]
    fn tracker_row_clock_is_its_status_change_not_the_issue_age() {
        let pm = pm_row(false, "idle");
        let review = |in_status: i64| {
            item(
                70,
                "review_no_pr",
                "CAD-1 in review",
                30 * 86_400,
                "",
                None,
                "c",
            )
            .about("issue", "CAD-1")
            .owned_by(Some("pm"))
            .since(Some(NOW - in_status))
        };
        let out = resolve(vec![review(10 * 60)], std::slice::from_ref(&pm));
        assert_eq!(out[0]["audience"], "team", "{}", out[0]);
        assert_eq!(out[0]["audience_reason"], "owner pm can act");
        let out = resolve(vec![review(74 * 60)], &[pm]);
        assert_eq!(out[0]["audience"], "operator", "{}", out[0]);
        assert_eq!(out[0]["audience_reason"], "unhandled 74m");
    }

    /// A row with no reliable start time never escalates by age, however
    /// old its subject, and never claims "unhandled".
    #[test]
    fn row_without_a_start_time_never_escalates_by_age() {
        let pm = pm_row(false, "idle");
        let old = 30 * 86_400;
        // An approval menu is a pane sample with no start, even on a
        // days-old agent row.
        let a = json!({
            "alias": "w1", "provider": "devin", "endpoint_kind": "pty",
            "state": "busy", "updated": (NOW - old) as f64,
            "params": {"upstream": "pm"}, "pane_menu": "1. Yes",
        });
        let mut rows = agent_items(&a, &AgentProbe::default(), "", NOW);
        // A tracker row whose status has no single change (a rollup),
        // and a PR head with no checks yet.
        rows.push(
            item(70, "review_no_pr", "CAD-9", old, "", None, "c")
                .about("issue", "CAD-9")
                .owned_by(Some("pm")),
        );
        rows.push(
            item(60, "pr_no_verdict", "PR #3", old, "", None, "c")
                .about("pr", "a/b#3")
                .owned_by(Some("pm"))
                .since(rollup_span(&[]).0),
        );
        let out = resolve(rows, &[a.clone(), pm]);
        assert_eq!(out.len(), 3);
        for row in &out {
            assert_eq!(row["since"], Value::Null, "{row}");
            assert_eq!(row["audience"], "team", "{row}");
            assert_eq!(row["audience_reason"], "owner pm can act", "{row}");
        }
    }

    /// A fence with no unknown turn (a disconnect while idle) is timed
    /// from the row's last write — a lower bound on time in `attention`.
    #[test]
    fn fence_without_an_unknown_turn_is_timed_from_its_last_write() {
        let pm = pm_row(false, "idle");
        let fenced = |written_ago: i64| {
            let a = json!({
                "alias": "w1", "provider": "devin", "endpoint_kind": "pty",
                "state": "attention", "updated": (NOW - written_ago) as f64,
                "params": {"upstream": "pm"},
                "error": "Provider process disconnected while idle",
            });
            let probe = AgentProbe {
                show: Some(json!({"messages": [
                    {"id": "m0", "state": "completed", "completed": (NOW - 9 * 3_600) as f64},
                ]})),
                ..AgentProbe::default()
            };
            resolve(agent_items(&a, &probe, "", NOW), &[a.clone(), pm.clone()])
        };
        let out = fenced(74 * 60);
        assert_eq!(out[0]["since"], NOW - 74 * 60, "{}", out[0]);
        // The operator's from the start (CAD-374).
        assert_eq!(out[0]["audience_reason"], "operator decision");
        // A later params write shortens the clock; it never inflates it.
        assert_eq!(fenced(10 * 60)[0]["since"], NOW - 10 * 60);
    }

    #[test]
    fn owner_that_cannot_act_escalates_with_the_reason() {
        // A team row the PM owns (a fenced row is the operator's alone,
        // CAD-374, so it cannot show the owner's reason).
        let owners_of = |pm: Option<Value>| {
            let agents: Vec<Value> = pm.into_iter().collect();
            let mut rows = vec![item(40, "stalled", "t", 60, "", None, "c").owned_by(Some("pm"))];
            classify_needs(
                &mut rows,
                &Owners::new(true, &agents),
                NOW,
                ESCALATE_AFTER_SECS,
            );
            (
                rows[0].json["audience"].clone(),
                rows[0].json["audience_reason"].clone(),
            )
        };
        assert_eq!(
            owners_of(Some(pm_row(false, "attention"))),
            (json!("operator"), json!("owner pm is fenced"))
        );
        assert_eq!(
            owners_of(Some(pm_row(false, "stopped"))),
            (json!("operator"), json!("owner pm is stopped"))
        );
        assert_eq!(
            owners_of(None),
            (json!("operator"), json!("owner pm is absent"))
        );
        let drained = json!({"alias": "pm", "provider": "inbox", "state": "idle",
                             "dead": false, "inbox_health": {"stale": true}});
        assert_eq!(
            owners_of(Some(drained)),
            (json!("operator"), json!("owner pm has no inbox consumer"))
        );
        // An unreachable daemon cannot vouch for any owner.
        let mut rows = vec![item(40, "stalled", "t", 1, "", None, "c").owned_by(Some("pm"))];
        classify_needs(
            &mut rows,
            &Owners::new(false, &[]),
            NOW,
            ESCALATE_AFTER_SECS,
        );
        assert_eq!(rows[0].json["audience"], "operator");
    }

    #[test]
    fn merge_ready_pr_with_live_issue_owner_stays_team() {
        let owner = json!({"alias": "w9", "provider": "devin", "state": "busy", "dead": false});
        // Merge-ready since the verdict landed 5 minutes ago.
        let rollup = [
            json!({"__typename": "CheckRun", "startedAt": "1970-01-12T13:16:40Z",
                   "completedAt": "1970-01-12T13:40:00Z"}),
            json!({"__typename": "StatusContext", "context": "qa-verdict",
                   "startedAt": "1970-01-12T13:41:40Z"}),
            json!({"__typename": "CheckRun", "startedAt": "0001-01-01T00:00:00Z",
                   "completedAt": "0001-01-01T00:00:00Z"}),
        ];
        let (first, last) = rollup_span(&rollup);
        assert_eq!((first, last), (Some(NOW - 1_800), Some(NOW - 300)));
        let pr = item(
            10,
            "merge",
            "PR #7 fix — verdict pass",
            3_600,
            "cadence",
            None,
            "c",
        )
        .about("pr", "acme/widgets#7")
        .for_agent("w9")
        .owned_by(Some("w9"))
        .since(last);
        let out = resolve(vec![pr], &[owner]);
        assert_eq!(out[0]["audience"], "team", "{}", out[0]);
        assert_eq!(out[0]["audience_reason"], "owner w9 can act");
    }

    #[test]
    fn row_without_a_resolvable_owner_is_the_operators() {
        let out = resolve(
            vec![
                item(90, "ci_red", "main CI failed", 60, "", None, "c").about("ci", "a/b@main"),
                // An issue with no owner (`owned_by` drops the empty one).
                item(70, "review_no_pr", "CAD-1", 60, "", None, "c")
                    .about("issue", "CAD-1")
                    .owned_by(Some("")),
                // A root agent — no upstream PM.
                item(40, "stalled", "w2", 60, "", None, "c").about("agent", "w2"),
            ],
            &[],
        );
        for row in &out {
            assert_eq!(row["audience"], "operator", "{row}");
            assert_eq!(row["audience_reason"], "no owner", "{row}");
        }
    }

    #[test]
    fn kind_class_holds_outside_team_rows() {
        let old = Some(NOW - ESCALATE_AFTER_SECS * 10);
        let out = resolve(
            vec![
                item(20, "approval", "a", 1, "", None, "c").about("agent", "w1"),
                item(50, "drift", "d", 1, "", None, "c")
                    .about("deploy", "x")
                    .since(old),
                item(100, "inbox_unread", "i", 1, "", None, "c")
                    .about("agent", "w2")
                    .since(old),
                item(110, "tracker_behind", "t", 1, "", None, "c")
                    .about("tracker", "pm")
                    .since(old),
            ],
            &[],
        );
        let got: Vec<(&str, &Value)> = out
            .iter()
            .map(|r| (r["audience"].as_str().unwrap(), &r["audience_reason"]))
            .collect();
        assert_eq!(
            got,
            [
                ("operator", &json!("operator decision")),
                ("dependency", &Value::Null),
                ("info", &Value::Null),
                ("info", &Value::Null),
            ]
        );
    }

    /// A merged row is for whoever its most urgent cause is for: a
    /// fresh approval menu (team) plus an old stall (escalated) on one
    /// agent is the operator's, and each cause keeps its own audience.
    #[test]
    fn merged_row_takes_its_most_urgent_audience() {
        let pm = pm_row(false, "idle");
        let a = json!({
            "alias": "w1", "provider": "devin", "endpoint_kind": "pty",
            "state": "busy", "updated": (NOW - 60) as f64,
            "params": {"upstream": "pm"}, "pane_menu": "1. Yes",
            "stalled": true, "silent_secs": ESCALATE_AFTER_SECS + 60,
        });
        let out = resolve(agent_items(&a, &AgentProbe::default(), "", NOW), &[pm]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["kind"], "approval_menu", "primary stays by rank");
        assert_eq!(out[0]["audience"], "operator");
        assert_eq!(out[0]["audience_reason"], "unhandled 61m");
        assert_eq!(out[0]["causes"][0]["audience"], "team");
        assert_eq!(out[0]["causes"][1]["audience"], "operator");
        assert_eq!(out[0]["causes"][1]["since"], NOW - ESCALATE_AFTER_SECS - 60);
    }

    #[test]
    fn scope_keeps_project_and_group_rows() {
        let rows = || {
            vec![
                item(40, "stalled", "a", 1, "cadence", None, "c")
                    .about("agent", "w1")
                    .for_agent("w1"),
                item(40, "stalled", "b", 1, "other", None, "c")
                    .about("agent", "w2")
                    .for_agent("w2"),
                item(90, "ci_red", "c", 1, "cadence", None, "c").about("repo", "a/b"),
            ]
        };
        let titles = |v: Vec<Item>| -> Vec<String> {
            v.iter()
                .map(|i| i.json["title"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            titles(scope_rows(rows(), Some("cadence"), None)),
            ["a", "c"]
        );
        let members = vec!["pm".to_string(), "w2".to_string()];
        assert_eq!(titles(scope_rows(rows(), None, Some(&members))), ["b"]);
        assert_eq!(titles(scope_rows(rows(), None, None)).len(), 3);
    }

    #[test]
    fn agent_project_is_longest_repo_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let wt = root.join(".cadence/wt/x");
        std::fs::create_dir_all(&wt).unwrap();
        let repos = vec![
            (root.clone(), "outer".to_string()),
            (root.join(".cadence"), "inner".to_string()),
        ];
        let a = json!({"cwd": wt.to_str().unwrap()});
        assert_eq!(agent_project(&a, &repos), "inner");
        assert_eq!(agent_project(&json!({"cwd": "/elsewhere"}), &repos), "");
        assert_eq!(agent_project(&json!({}), &repos), "");
    }

    // ---- CAD-267: default-branch CI from Actions runs ----

    /// A fake 40-hex SHA from a digit — built at runtime, never a
    /// credential-shaped literal.
    fn sha(n: u8) -> String {
        format!("{n}").repeat(40)
    }

    /// One workflow run the way the runs API lists it. `created` orders
    /// runs; `id` rises with it.
    fn run(workflow: &str, sha_n: u8, id: u64, status: &str, conclusion: Option<&str>) -> Value {
        json!({
            "id": id, "head_sha": sha(sha_n), "status": status,
            "conclusion": conclusion, "event": "push",
            "path": format!(".github/workflows/{workflow}"), "head_branch": "main",
            "html_url": format!("https://github.com/o/r/actions/runs/{id}"),
            "created_at": format!("2026-09-23T01:{:02}:00Z", id % 60),
        })
    }

    fn ci(sha_n: u8, id: u64, status: &str, conclusion: Option<&str>) -> Value {
        run("ci.yml", sha_n, id, status, conclusion)
    }

    fn states(shas: &[ShaCi]) -> Vec<(&str, CiState, Option<&str>)> {
        shas.iter()
            .map(|s| {
                (
                    &s.sha[..1],
                    s.state,
                    s.covered_by.as_deref().map(|c| &c[..1]),
                )
            })
            .collect()
    }

    /// First-parent log for SHAs 1 (oldest) … n (newest), newest first.
    fn log(n: u8) -> Vec<String> {
        (1..=n).rev().map(sha).collect()
    }

    /// Three rapid pushes: 1 passed, 2's run cancelled, 3 still pending.
    /// 2 has no covering descendant yet → ci_unverified; pending never
    /// alerts and nothing is red.
    #[test]
    fn main_ci_middle_cancelled_newest_pending_is_unverified() {
        let runs = [
            ci(3, 30, "in_progress", None),
            ci(2, 20, "completed", Some("cancelled")),
            ci(1, 10, "completed", Some("success")),
        ];
        let shas = classify_main_ci(&runs, &log(3));
        use CiState::*;
        assert_eq!(
            states(&shas),
            [
                ("3", Pending, None),
                ("2", Cancelled, None),
                ("1", Passed, None)
            ]
        );
        let a = main_ci_alerts(&shas);
        assert!(a.red.is_none(), "pending never alerts");
        assert_eq!(a.unverified.len(), 1);
        assert_eq!(a.unverified[0].sha, sha(2));
    }

    /// Newest failed: ci_red on it, and the cancelled middle stays
    /// uncovered — a failed descendant covers nothing.
    #[test]
    fn main_ci_newest_failed_is_red_and_middle_uncovered() {
        use CiState::*;
        for bad in ["failure", "timed_out", "startup_failure"] {
            let runs = [
                ci(3, 30, "completed", Some(bad)),
                ci(2, 20, "completed", Some("cancelled")),
                ci(1, 10, "completed", Some("success")),
            ];
            let shas = classify_main_ci(&runs, &log(3));
            assert_eq!(
                states(&shas),
                [
                    ("3", Failed, None),
                    ("2", Cancelled, None),
                    ("1", Passed, None)
                ],
                "{bad}"
            );
            let a = main_ci_alerts(&shas);
            assert_eq!(a.red.map(|s| s.sha.clone()), Some(sha(3)), "{bad}");
            assert_eq!(a.unverified.len(), 1, "{bad}");
            assert_eq!(a.unverified[0].sha, sha(2));
        }
    }

    /// Newest passed: the cancelled middle is covered by it — and still
    /// labelled cancelled, never passed. No alert.
    #[test]
    fn main_ci_newest_passed_covers_middle_without_passing_it() {
        let runs = [
            ci(3, 30, "completed", Some("success")),
            ci(2, 20, "completed", Some("cancelled")),
            ci(1, 10, "completed", Some("success")),
        ];
        let shas = classify_main_ci(&runs, &log(3));
        use CiState::*;
        assert_eq!(
            states(&shas),
            [
                ("3", Passed, None),
                ("2", Cancelled, Some("3")),
                ("1", Passed, None)
            ]
        );
        assert_ne!(shas[1].state, Passed);
        let a = main_ci_alerts(&shas);
        assert!(a.red.is_none());
        assert!(a.unverified.is_empty(), "covered clears the alert");
    }

    /// A SHA whose only runs are another workflow's (Handover) — even a
    /// passing one — is missing, never passed; a non-push ci run does
    /// not count either.
    #[test]
    fn main_ci_handover_only_sha_is_missing_not_passed() {
        let mut dispatch = ci(2, 21, "completed", Some("success"));
        dispatch["event"] = json!("workflow_dispatch");
        let runs = [
            run("handover.yml", 2, 22, "completed", Some("success")),
            dispatch,
            ci(1, 10, "completed", Some("success")),
        ];
        let shas = classify_main_ci(&runs, &log(2));
        use CiState::*;
        assert_eq!(states(&shas), [("2", Missing, None), ("1", Passed, None)]);
        assert!(shas[0].run_id.is_none());
        let a = main_ci_alerts(&shas);
        assert_eq!(a.unverified.len(), 1);
        assert_eq!(a.unverified[0].state, Missing);
        // A later passing SHA covers the missing one; it stays missing.
        let runs = [ci(3, 30, "completed", Some("success")), runs[0].clone()];
        let shas = classify_main_ci(&runs, &log(3));
        assert_eq!(shas[1].state, Missing);
        assert_eq!(shas[1].covered_by, Some(sha(3)));
    }

    /// Run SHAs newer than the clone's last fetch lead the list; an
    /// unlisted SHA older than every listed run is not first-parent
    /// history and is dropped. With no clone, runs alone give the order.
    #[test]
    fn main_ci_places_runs_the_clone_has_not_fetched() {
        let runs = [
            ci(4, 40, "completed", Some("success")),
            ci(2, 20, "completed", Some("cancelled")),
            ci(1, 10, "completed", Some("success")),
            // Off first-parent history (older than every listed run).
            ci(9, 5, "completed", Some("success")),
        ];
        use CiState::*;
        // The clone knows 1..2 only; 4 was pushed after its fetch.
        let shas = classify_main_ci(&runs, &log(2));
        assert_eq!(
            states(&shas),
            [
                ("4", Passed, None),
                ("2", Cancelled, Some("4")),
                ("1", Passed, None)
            ]
        );
        // No clone: every run SHA, newest run first.
        let shas = classify_main_ci(&runs, &[]);
        assert_eq!(
            shas.iter().map(|s| &s.sha[..1]).collect::<Vec<_>>(),
            ["4", "2", "1", "9"]
        );
        // Per SHA the newest run decides (a re-push of the same SHA).
        let runs = [
            ci(1, 11, "completed", Some("failure")),
            ci(1, 10, "completed", Some("success")),
        ];
        assert_eq!(classify_main_ci(&runs, &log(1))[0].state, Failed);
    }

    /// The needs-me rows: ci_red and ci_unverified share the branch's
    /// subject (one row, two causes); titles end in the slug, which
    /// `session`'s ack key reads.
    #[test]
    fn main_ci_rows_share_the_branch_subject() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q", "-b", "main"]);
        git(repo, &["config", "user.email", "t@t"]);
        git(repo, &["config", "user.name", "t"]);
        let mut shas = Vec::new();
        for i in 0..3 {
            std::fs::write(repo.join("f"), format!("{i}")).unwrap();
            git(repo, &["add", "f"]);
            git(repo, &["commit", "-qm", &format!("c{i}")]);
            shas.push(
                git_text(repo, &["rev-parse".into(), "HEAD".into()])
                    .unwrap()
                    .trim()
                    .to_string(),
            );
        }
        let with_sha = |mut r: Value, i: usize| {
            r["head_sha"] = json!(shas[i]);
            r
        };
        let data = json!({"main_ci": {"branch": "main", "runs": [
            with_sha(ci(0, 30, "completed", Some("failure")), 2),
            with_sha(ci(0, 20, "completed", Some("cancelled")), 1),
            with_sha(ci(0, 10, "completed", Some("success")), 0),
        ]}});
        let (view, rows) = main_ci_view("o/r", "cadence", &data, Some(repo), 0);
        assert_eq!(view["order"], "first_parent", "{view}");
        let got: Vec<&str> = view["shas"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["state"].as_str().unwrap())
            .collect();
        assert_eq!(got, ["failed", "cancelled", "passed"], "{view}");
        let merged = merge_by_subject(rows);
        assert_eq!(merged.len(), 1);
        let row = &merged[0].json;
        assert_eq!(row["subject"], json!({"kind": "ci", "id": "o/r@main"}));
        assert_eq!(row["kind"], "ci_red");
        assert_eq!(row["command"], "gh run view 30 --repo o/r");
        let causes: Vec<&str> = row["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["cause"].as_str().unwrap())
            .collect();
        assert_eq!(causes, ["ci_red", "ci_unverified"]);
        assert_eq!(row["causes"][1]["command"], "gh run rerun 20 --repo o/r");
        assert!(row["title"].as_str().unwrap().ends_with(" o/r"), "{row}");
        assert!(row["causes"][1]["title"]
            .as_str()
            .unwrap()
            .contains(&shas[1][..7]));
        // No ci.yml workflow: nothing to show, nothing to alert.
        let absent = json!({"main_ci": {"branch": "main", "absent": true}});
        let (view, rows) = main_ci_view("o/r", "cadence", &absent, Some(repo), 0);
        assert!(view.is_null() && rows.is_empty());
        // A failed fetch surfaces its error, never a verdict.
        let failed = json!({"main_ci": {"branch": "main", "error": "gh: boom"}});
        let (view, rows) = main_ci_view("o/r", "cadence", &failed, Some(repo), 0);
        assert_eq!(view["error"], "gh: boom");
        assert!(rows.is_empty());
    }

    /// Controlled dependency delay; each observer owns its refresh lifetime.
    #[derive(Default)]
    struct FetchGate {
        state: Mutex<FetchGateState>,
        wake: std::sync::Condvar,
    }

    #[derive(Default)]
    struct FetchGateState {
        released: bool,
        attempts: usize,
        observers: usize,
    }

    impl FetchGate {
        fn release(&self) {
            self.state.lock().unwrap().released = true;
            self.wake.notify_all();
        }

        fn attempts(&self) -> usize {
            self.state.lock().unwrap().attempts
        }

        fn fetch(&self) {
            let mut state = self.state.lock().unwrap();
            state.attempts += 1;
            while !state.released {
                state = self.wake.wait(state).unwrap();
            }
        }
    }

    /// Dropped after refresh completion, or immediately if no refresh starts.
    struct RefreshLease(std::sync::Arc<FetchGate>);

    impl Drop for RefreshLease {
        fn drop(&mut self) {
            self.0
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .observers -= 1;
            self.0.wake.notify_all();
        }
    }

    #[derive(Default)]
    struct GateHold(std::sync::Arc<FetchGate>);

    impl GateHold {
        fn observed(
            &self,
            state_dir: &Path,
            slugs: &[String],
            wait: Duration,
            cache_secs: i64,
        ) -> (HashMap<String, Value>, Value, Receiver<()>) {
            self.0.state.lock().unwrap().observers += 1;
            let lease = RefreshLease(self.0.clone());
            let fetch = self.0.clone();
            let (done_tx, done_rx) = channel();
            let out = github_bounded_notify(
                state_dir,
                slugs,
                wait,
                cache_secs,
                move |_| {
                    fetch.fetch();
                    Ok(json!({"prs": [{"number": 2}], "ci": {"state": "success"}}))
                },
                move || {
                    let _lease = lease;
                    let _ = done_tx.send(());
                },
            );
            (out.0, out.1, done_rx)
        }
    }

    impl Drop for GateHold {
        fn drop(&mut self) {
            // Release on panic too; await even workers not yet in the fetch closure.
            self.0.release();
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            let (state, _) = self
                .0
                .wake
                .wait_timeout_while(state, Duration::from_secs(15), |state| state.observers != 0)
                .unwrap_or_else(|e| e.into_inner());
            if state.observers != 0 {
                if std::thread::panicking() {
                    eprintln!("refresh cleanup timed out: {} observers", state.observers);
                } else {
                    panic!("refresh cleanup timed out: {} observers", state.observers);
                }
            }
        }
    }

    #[test]
    fn cad627_gh_cache_setting_bounds_and_default() {
        assert_eq!(gh_cache_secs(None), 60);
        assert_eq!(gh_cache_secs(Some(" 300 ")), 300);
        assert_eq!(gh_cache_secs(Some("3600")), 3600);
        for raw in ["", "0", "59", "-1", "3601", "NaN", "9999999999999999999999"] {
            assert_eq!(gh_cache_secs(Some(raw)), 60, "{raw}");
        }
    }

    #[test]
    fn cad627_gh_cache_configurable_expiry_and_stale_fallback() {
        fn fresh(_slug: &str) -> Result<Value, String> {
            Ok(json!({"prs": [{"number": 2}]}))
        }
        fn failing(_slug: &str) -> Result<Value, String> {
            Err("fixture outage".to_string())
        }
        let dir = tempfile::tempdir().unwrap();
        let slugs = vec!["acme/widgets".to_string()];
        let repos = HashMap::from([(slugs[0].clone(), json!({"prs": [{"number": 1}]}))]);
        let old = now_epoch() - 120;
        write_cache(&cache_file(dir.path()), &slugs, &repos, old);
        let wait = Duration::from_secs(2);
        let (got, state) = github_bounded(dir.path(), &slugs, wait, 300, fresh);
        assert_eq!(state["state"], "cached");
        assert_eq!(state["as_of"], old);
        assert_eq!(got[&slugs[0]]["prs"][0]["number"], 1);
        let (got, state) = github_bounded(dir.path(), &slugs, wait, 60, fresh);
        assert_eq!(state["state"], "ok");
        assert_eq!(got[&slugs[0]]["prs"][0]["number"], 2);
        let expired = now_epoch() - 301;
        write_cache(&cache_file(dir.path()), &slugs, &repos, expired);
        let (got, state) = github_bounded(dir.path(), &slugs, wait, 300, failing);
        assert_eq!(state["state"], "stale");
        assert_eq!(state["as_of"], expired);
        assert_eq!(got[&slugs[0]]["prs"][0]["number"], 1);
        assert_eq!(read_cache(&cache_file(dir.path())).unwrap().at, expired);
        let (got, state) = github_bounded(dir.path(), &slugs, wait, 300, fresh);
        assert_eq!(state["state"], "ok");
        assert_eq!(got[&slugs[0]]["prs"][0]["number"], 2);
    }

    #[test]
    fn partial_gh_failure_keeps_reused_rows_stale_until_all_repos_refresh() {
        fn partial(slug: &str) -> Result<Value, String> {
            if slug == "acme/stale" {
                Err("fixture outage".to_string())
            } else {
                Ok(json!({"prs": [{"number": 2}]}))
            }
        }
        fn fresh(_slug: &str) -> Result<Value, String> {
            Ok(json!({"prs": [{"number": 3}]}))
        }
        let dir = tempfile::tempdir().unwrap();
        let slugs = vec!["acme/fresh".to_string(), "acme/stale".to_string()];
        let prior = HashMap::from([
            (slugs[0].clone(), json!({"prs": [{"number": 1}]})),
            (slugs[1].clone(), json!({"prs": [{"number": 1}]})),
        ]);
        let old = now_epoch() - 120;
        write_cache(&cache_file(dir.path()), &slugs, &prior, old);
        let wait = Duration::from_secs(2);

        for _ in 0..2 {
            let (got, state) = github_bounded(dir.path(), &slugs, wait, 60, partial);
            assert_eq!(got[&slugs[0]]["prs"][0]["number"], 2);
            assert_eq!(got[&slugs[1]]["prs"][0]["number"], 1);
            assert_eq!(state["state"], "stale", "{state}");
            assert_eq!(state["as_of"], old, "{state}");
            assert_eq!(read_cache(&cache_file(dir.path())).unwrap().at, old);
        }

        let (got, state) = github_bounded(dir.path(), &slugs, wait, 60, fresh);
        assert_eq!(state["state"], "ok", "{state}");
        assert_eq!(got[&slugs[1]]["prs"][0]["number"], 3);
        assert!(read_cache(&cache_file(dir.path())).unwrap().at > old);
    }

    #[test]
    fn gh_failure_does_not_reuse_cache_from_a_different_slug_set() {
        fn partial(slug: &str) -> Result<Value, String> {
            if slug == "acme/new" {
                Err("fixture outage".to_string())
            } else {
                Ok(json!({"prs": [{"number": 2}]}))
            }
        }
        fn failing(_slug: &str) -> Result<Value, String> {
            Err("fixture outage".to_string())
        }
        let dir = tempfile::tempdir().unwrap();
        let prior_slugs = vec!["acme/fresh".to_string(), "acme/old".to_string()];
        let requested = vec!["acme/fresh".to_string(), "acme/new".to_string()];
        let prior = HashMap::from([
            (prior_slugs[0].clone(), json!({"prs": [{"number": 1}]})),
            (prior_slugs[1].clone(), json!({"prs": [{"number": 1}]})),
        ]);
        write_cache(
            &cache_file(dir.path()),
            &prior_slugs,
            &prior,
            now_epoch() - 120,
        );

        let (got, state) =
            github_bounded(dir.path(), &requested, Duration::from_secs(2), 60, failing);
        assert!(got.is_empty());
        assert_eq!(state["state"], "unavailable", "{state}");

        let (got, state) =
            github_bounded(dir.path(), &requested, Duration::from_secs(2), 60, partial);
        assert_eq!(got.len(), 1);
        assert_eq!(got[&requested[0]]["prs"][0]["number"], 2);
        assert_eq!(state["state"], "stale", "{state}");
        assert!(state["as_of"].is_null(), "{state}");
        assert_eq!(state["error"], "fixture outage");
        assert_eq!(
            read_cache(&cache_file(dir.path())).unwrap().slugs,
            prior_slugs
        );
    }

    #[test]
    fn cad627_overview_probe_requests_active_history() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(client::socket_path(dir.path())).unwrap();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                let result = if req["method"] == "agent_show" {
                    json!({"messages": [{"state": "unknown", "completed": 42.0}]})
                } else {
                    json!({"requests": []})
                };
                writeln!(stream, "{}", json!({"ok": true, "result": result})).unwrap();
                seen.push(req);
            }
            seen
        });
        let row = json!({"alias": "w1", "provider": "fake", "endpoint_kind": "managed"});
        let probe = probe_agent(
            dir.path(),
            &row,
            Duration::from_secs(2),
            Instant::now() + Duration::from_secs(4),
        );
        let seen = server.join().unwrap();
        assert_eq!(seen[0]["method"], "agent_show");
        assert_eq!(seen[0]["params"]["active_only"], true);
        assert_eq!(fenced_since(&row, &probe), Some(42));
        assert!(!probe.holds_drift);
    }

    /// CAD-249: a gh refresh slower than the caller's wait serves the
    /// last cache as `stale` with its `as_of` inside the bound, and the
    /// refresh still lands in the cache for the next request. CAD-1124:
    /// the slowness is a parked gate, not a sleep — the refresh provably
    /// outlives the wait, its completion is observed directly on the
    /// worker, and a single parked fetch proves single-flight.
    #[test]
    fn slow_gh_serves_stale_cache_within_the_wait() {
        let dir = tempfile::tempdir().unwrap();
        let slugs = vec!["acme/widgets".to_string()];
        let hold = GateHold::default();
        let old = now_epoch() - 600;
        let mut repos = HashMap::new();
        repos.insert(
            "acme/widgets".to_string(),
            json!({"prs": [{"number": 1}], "ci": {}}),
        );
        write_cache(&cache_file(dir.path()), &slugs, &repos, old);

        let started = Instant::now();
        let (got, state, refresh_done) = hold.observed(
            dir.path(),
            &slugs,
            Duration::from_millis(300),
            GH_CACHE_SECS,
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(state["state"], "stale", "{state}");
        assert_eq!(state["as_of"], old, "{state}");
        assert!(
            state["error"].as_str().unwrap().contains("still running"),
            "{state}"
        );
        assert_eq!(got["acme/widgets"]["prs"][0]["number"], 1);

        // A second request while the refresh is parked starts no other
        // one — single-flight, proven by the fetch count.
        let (_, again, second_done) = hold.observed(
            dir.path(),
            &slugs,
            Duration::from_millis(100),
            GH_CACHE_SECS,
        );
        assert_eq!(again["state"], "stale", "{again}");
        assert!(
            matches!(
                second_done.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Disconnected)
            ),
            "a second refresh was created while the first was parked"
        );
        assert!(
            hold.0.attempts() <= 1,
            "a second fetch attempted while parked"
        );

        // Admit the fetch; the worker's own completion signal lands
        // after refresh_github returns and the cache is written.
        hold.0.release();
        refresh_done
            .recv_timeout(Duration::from_secs(15))
            .expect("refresh worker never completed");
        assert_eq!(hold.0.attempts(), 1);
        let (got, state, cache_done) =
            hold.observed(dir.path(), &slugs, Duration::from_millis(1), GH_CACHE_SECS);
        assert!(matches!(
            cache_done.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
        assert_eq!(state["state"], "cached", "{state}");
        assert_eq!(got["acme/widgets"]["prs"][0]["number"], 2);
    }

    /// No cache at all and a slow gh: `unavailable` inside the bound,
    /// never a hang. CAD-1124: the fetch is parked on a gate, so the
    /// bounded return cannot race a finishing fixture; the worker's
    /// completion is awaited before the tempdir drops.
    #[test]
    fn slow_gh_without_cache_is_unavailable_not_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let slugs = vec!["acme/gadgets".to_string()];
        let hold = GateHold::default();
        let started = Instant::now();
        let (got, state, refresh_done) = hold.observed(
            dir.path(),
            &slugs,
            Duration::from_millis(200),
            GH_CACHE_SECS,
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(got.is_empty());
        assert_eq!(state["state"], "unavailable", "{state}");
        assert!(state["as_of"].is_null());
        // The parked refresh still runs in the background; admit it and
        // await the worker's completion before the tempdir drops.
        hold.0.release();
        refresh_done
            .recv_timeout(Duration::from_secs(15))
            .expect("refresh worker never completed");
        assert_eq!(hold.0.attempts(), 1);
    }

    /// A daemon that answers `health`/`agent_list` but never answers
    /// the per-agent probes: the overview still returns inside its
    /// bounds, naming what timed out in `degraded`.
    #[test]
    fn wedged_agent_probes_degrade_within_the_bound() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(client::socket_path(dir.path())).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut line = String::new();
                    let mut reader = BufReader::new(&stream);
                    if reader.read_line(&mut line).is_err() {
                        return;
                    }
                    let req: Value = serde_json::from_str(&line).unwrap_or_default();
                    let result = match req["method"].as_str().unwrap_or_default() {
                        "health" => json!({"state": "ready"}),
                        "daemon_info" => json!({"build_commit": "unknown"}),
                        "agent_list" => json!({"agents": [{
                            "alias": "w1", "provider": "claude",
                            "endpoint_kind": "managed", "state": "busy",
                            "updated": 0.0,
                        }]}),
                        // agent_show / agent_requests / monitors: wedged.
                        _ => {
                            std::thread::sleep(Duration::from_secs(30));
                            return;
                        }
                    };
                    let frame = json!({"ok": true, "result": result});
                    let _ = writeln!(&stream, "{frame}");
                });
            }
        });
        let opts = Options {
            probe_timeout: Duration::from_millis(300),
            probe_budget: Duration::from_millis(800),
            ..Options::board()
        };
        let started = Instant::now();
        let view = overview_with(dir.path(), &dir.path().join("no-pm"), &opts).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(view["daemon"]["reachable"], true, "{view}");
        let notes = view["degraded"].as_array().unwrap();
        assert!(
            notes
                .iter()
                .any(|d| d["source"] == "agent_show" && d["subject"] == "w1"),
            "{view}"
        );
        assert!(
            notes[0]["detail"]
                .as_str()
                .unwrap()
                .contains("no answer within"),
            "{view}"
        );
    }

    fn git(repo: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
    }

    #[test]
    fn drift_counts_commits_and_prs() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-b", "main"]);
        git(repo, &["config", "user.email", "t@t"]);
        git(repo, &["config", "user.name", "t"]);
        let commit = |msg: &str| {
            std::fs::write(repo.join("f"), msg).unwrap();
            git(repo, &["add", "f"]);
            git(repo, &["commit", "-m", msg]);
            git_text(repo, &["rev-parse".into(), "HEAD".into()])
                .unwrap()
                .trim()
                .to_string()
        };
        let base = commit("one (#1)");
        // Zero drift: building at HEAD.
        let d = compute_drift(repo, &base);
        assert_eq!(d["known"], true);
        assert_eq!(d["count"], 0);
        commit("two (#2)");
        commit("three — no pr");
        commit("four (#4)");
        let d = compute_drift(repo, &base);
        assert_eq!(d["known"], true);
        assert_eq!(d["count"], 3);
        let prs: Vec<Option<u64>> = d["commits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["pr"].as_u64())
            .collect();
        assert_eq!(prs, vec![Some(4), None, Some(2)]);
        // Unknown build — cannot tell, not zero.
        let d = compute_drift(repo, "unknown");
        assert_eq!(d["known"], false);
        assert!(d["reason"].as_str().unwrap().contains("cannot tell"));
        // A commit git cannot place — also cannot tell.
        let d = compute_drift(repo, "deadbeef".repeat(5).as_str());
        assert_eq!(d["known"], false);
    }

    #[test]
    fn drift_matches_repo_by_remote_or_path() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("clone");
        std::fs::create_dir_all(&repo).unwrap();
        let p = project::Project {
            key: "cadence".into(),
            prefix: "CAD".into(),
            repos: vec![project::Repo {
                path: Some(repo.to_string_lossy().into()),
                remote: Some("git@github.com:favcrm/cadence.git".into()),
            }],
            components: vec![],
            tags: vec![],
            default_owner: None,
            build: None,
            memory: None,
            intake: None,
        };
        // Path match against BUILD_ROOT (this crate's checkout) never
        // hits the temp clone — remote match does when remote differs…
        let m = build_repo_match(std::slice::from_ref(&p));
        // Build remote is github.com/favcrm/cadence in this checkout, so
        // the declared remote matches; on foreign checkouts (unknown
        // remote) nothing matches — either outcome is consistent.
        if BUILD_REMOTE != "unknown" {
            assert_eq!(m.map(|(k, _)| k), Some("cadence".to_string()));
        } else {
            assert!(m.is_none());
        }
        // A project whose remote differs and whose path differs never
        // matches.
        let other = project::Project {
            key: "other".into(),
            prefix: "OTH".into(),
            repos: vec![project::Repo {
                path: Some("/definitely/not/the/build".into()),
                remote: Some("git@example.com:other/repo.git".into()),
            }],
            components: vec![],
            tags: vec![],
            default_owner: None,
            build: None,
            memory: None,
            intake: None,
        };
        assert!(build_repo_match(&[other]).is_none());
    }
}
