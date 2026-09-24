//! The MVP worker loop (CAD-431): a ticket the master dispatched goes to
//! its worker; the worker's `done` report (with `sha:` and `pr:`) goes to
//! an independent reviewer; the reviewer's `verdict` report sends a
//! REVISE back to the worker (at most [`MAX_REVISE`] times, then the
//! operator decides) or a PASS forward; a PASS on a green head becomes
//! one "merge?" decision in the operator's Needs-you.
//!
//! This module is the record and the pure rules. The daemon owns every
//! transition (`daemon/delivery_rpc.rs`) and is the only writer of
//! `<state>/delivery.json` — a report file can never move a ticket
//! through the loop, just as a report file can never put a question in
//! Needs-you (CAD-339).
//!
//! GitHub is touched only from the operator's own process: [`sync`]
//! (`cadence delivery sync`) reads each PR's head, CI and diff stats and
//! hands them to the daemon's operator-only `delivery_observe`; `cadence
//! delivery merge` enqueues with `gh pr merge --auto --squash
//! --match-head-commit <reviewed head>`. The daemon never runs `gh`, and
//! no agent environment needs GitHub credentials for the loop.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};

/// REVISE verdicts a ticket takes before the operator decides — the
/// delivery workflow's "at most 2 review rounds".
pub const MAX_REVISE: u32 = 2;
/// Bytes of a verdict's first line shown in Needs-you and messages.
pub const SUMMARY_MAX: usize = 200;
/// The `gh` the operator's process runs unless told otherwise.
pub const GH: &str = "gh";
/// Bound on each `gh` call [`sync`] and the merge action make.
pub const GH_TIMEOUT: Duration = Duration::from_secs(30);

/// Where a ticket is in the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// The worker has it — dispatched, or sent back with a REVISE.
    Working,
    /// A reviewer has a kickoff for `head`.
    Reviewing,
    /// A review is due but no agent can take it; retried every pass.
    Unstaffed,
    /// PASS on `reviewed.sha`; the merge decision waits on green CI.
    Passed,
    /// The operator enqueued the merge, pinned to `reviewed.sha`.
    Enqueued,
    /// [`MAX_REVISE`] REVISE verdicts — the operator decides.
    Escalated,
    Merged,
    Declined,
    /// The PR was closed without merging.
    Closed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Reviewing => "reviewing",
            State::Unstaffed => "unstaffed",
            State::Passed => "passed",
            State::Enqueued => "enqueued",
            State::Escalated => "escalated",
            State::Merged => "merged",
            State::Declined => "declined",
            State::Closed => "closed",
        }
    }

    /// No further transition happens.
    pub fn terminal(self) -> bool {
        matches!(self, State::Merged | State::Declined | State::Closed)
    }
}

/// One recorded verdict.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VerdictRec {
    pub verdict: String,
    pub sha: String,
    pub reviewer: String,
    pub summary: String,
    /// The report's path in the tracker, `<ID>/reports/<name>`.
    pub report: String,
    pub at: i64,
}

/// What the operator's process last saw on GitHub for the PR.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Observed {
    pub head: String,
    /// `OPEN`, `MERGED` or `CLOSED`.
    pub pr_state: String,
    pub ci_green: bool,
    pub auto_merge: bool,
    pub additions: u64,
    pub deletions: u64,
    pub files: u64,
    pub at: i64,
}

/// CAD-449: what a merge did to the ticket's tracker status. Written
/// once, on the transition into [`State::Merged`]; `pending` is retried
/// by the router pass until it settles.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TicketDone {
    /// The merge marked it done.
    Marked { at: i64 },
    /// Left as it was: already `done` or `dropped`, or changed by hand
    /// while the write was pending (`status` is what it was found at).
    Kept { status: String },
    /// The merge is not the reviewed one; the operator sets the status.
    Refused { why: String },
    /// The tracker write failed (busy, a failing hook); retried — but
    /// only while the status is still `from`, what it was at the merge.
    /// A record without `from` is never marked by a retry.
    Pending {
        why: String,
        #[serde(default)]
        from: Option<String>,
    },
}

impl TicketDone {
    /// The ticket still waits on the operator or a retry.
    pub fn open(&self) -> Option<&str> {
        match self {
            TicketDone::Refused { why } | TicketDone::Pending { why, .. } => Some(why),
            _ => None,
        }
    }
}

/// A ticket's place in the loop — one per dispatched ticket.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub issue: String,
    pub project: String,
    pub worker: String,
    pub state: State,
    /// When `state` was entered (epoch secs) — the Needs-you age.
    pub since: i64,
    pub dispatched_at: i64,
    /// The PR URL from the worker's latest done report.
    #[serde(default)]
    pub pr: Option<String>,
    /// The head under review, or last reported.
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub reviewer: Option<String>,
    /// Review kickoffs sent.
    #[serde(default)]
    pub rounds: u32,
    /// REVISE verdicts received.
    #[serde(default)]
    pub revisions: u32,
    /// The latest verdict.
    #[serde(default)]
    pub verdict: Option<VerdictRec>,
    /// Done reports the loop has consumed (file names).
    #[serde(default)]
    pub handled: Vec<String>,
    #[serde(default)]
    pub observed: Option<Observed>,
    /// Auto-merge is on for a head nobody approved: the operator's
    /// process must run `gh pr merge --disable-auto`. Cleared by the
    /// first observation that shows it off.
    #[serde(default)]
    pub disable_auto: bool,
    /// Why the operator was brought in, or the decline reason.
    #[serde(default)]
    pub note: Option<String>,
    /// Reviewers barred from this ticket: each was on duty when the head
    /// moved without the worker, so it may have pushed that head.
    #[serde(default)]
    pub excluded: Vec<String>,
    /// CAD-449: what the merge did to the ticket's status.
    #[serde(default)]
    pub ticket_done: Option<TicketDone>,
}

impl Record {
    pub fn new(issue: &str, project: &str, worker: &str, now: i64) -> Self {
        Record {
            issue: issue.to_string(),
            project: project.to_string(),
            worker: worker.to_string(),
            state: State::Working,
            since: now,
            dispatched_at: now,
            pr: None,
            head: None,
            reviewer: None,
            rounds: 0,
            revisions: 0,
            verdict: None,
            handled: vec![],
            observed: None,
            disable_auto: false,
            note: None,
            excluded: vec![],
            ticket_done: None,
        }
    }

    pub fn enter(&mut self, state: State, now: i64) {
        if self.state != state {
            self.since = now;
        }
        self.state = state;
    }

    /// The PASS this record stands on, when the latest verdict is one.
    pub fn passed_sha(&self) -> Option<&str> {
        self.verdict
            .as_ref()
            .filter(|v| v.verdict == "pass")
            .map(|v| v.sha.as_str())
    }

    /// The merge decision is ready: a PASS, and the operator's process
    /// saw that very head open with green CI.
    pub fn merge_ready(&self) -> bool {
        let Some(sha) = self.passed_sha() else {
            return false;
        };
        self.state == State::Passed
            && self
                .observed
                .as_ref()
                .is_some_and(|o| o.head == sha && o.ci_green && o.pr_state == "OPEN")
    }

    pub fn to_json(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        v["merge_ready"] = json!(self.merge_ready());
        v["pr_ref"] = json!(self.pr.as_deref().and_then(pr_ref));
        v
    }
}

/// A PR URL as `owner/repo#n` (lowercased owner/repo), the key one PR
/// is held under and the form every surface shows.
pub fn pr_ref(url: &str) -> Option<String> {
    let (slug, n) = crate::issue::task_report::parse_pr_url(url).ok()?;
    Some(format!("{}#{n}", slug.to_ascii_lowercase()))
}

pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join("delivery.json")
}

/// Every record, for readers (Needs-you, `delivery ls`). An unreadable
/// file reads as none.
pub fn records(state_dir: &Path) -> BTreeMap<String, Record> {
    load(state_dir).unwrap_or_default()
}

/// Every record, for the daemon's writes: a file that exists but does
/// not parse refuses rather than being replaced by an empty map.
pub fn load(state_dir: &Path) -> Result<BTreeMap<String, Record>> {
    match std::fs::read_to_string(path(state_dir)) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| Error::internal(format!("delivery.json is unreadable: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e.into()),
    }
}

/// Replace the records atomically. The daemon serializes writers.
pub fn save(state_dir: &Path, all: &BTreeMap<String, Record>) -> Result<()> {
    use std::io::Write;
    let file = path(state_dir);
    let tmp = file.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(all)?)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &file)?;
    // The rename itself is durable once the directory is synced.
    std::fs::File::open(state_dir)?.sync_all()?;
    Ok(())
}

/// One agent the reviewer rule may choose.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub alias: String,
    pub provider: String,
    pub state: String,
    pub enabled: bool,
    /// The agent's PM (`params.upstream`), when it is a group member.
    pub upstream: Option<String>,
}

/// Every alias in `worker`'s group line: the worker itself, its
/// upstream chain (its PM, that PM's PM, …), and every agent whose own
/// upstream chain reaches the worker (members it registered, and
/// theirs). None of them is independent of the worker's work.
fn worker_group(worker: &str, agents: &[Candidate]) -> Vec<String> {
    let up = |alias: &str| {
        agents
            .iter()
            .find(|a| a.alias == alias)
            .and_then(|a| a.upstream.clone())
    };
    // Chains are bounded by the agent count, so a cycle cannot loop.
    let chain = |from: &str| {
        let mut out = Vec::new();
        let mut at = up(from);
        while let Some(u) = at {
            if out.contains(&u) || out.len() > agents.len() {
                break;
            }
            at = up(&u);
            out.push(u);
        }
        out
    };
    let mut group = vec![worker.to_string()];
    group.extend(chain(worker));
    for a in agents {
        if chain(&a.alias).iter().any(|u| u == worker) {
            group.push(a.alias.clone());
        }
    }
    group
}

/// The independent reviewer for a worker's head: never the worker or
/// anyone in its group line ([`worker_group`]), never the master, never
/// an alias in `exclude`, never a fenced (`attention`), disabled or
/// inbox agent. The previous round's reviewer keeps the ticket while it
/// qualifies — the verdicts stay comparable; a caller passes `None` for
/// `previous` (and the old reviewer in `exclude`) when the head moved
/// without the worker, since whoever pushed must not review its own
/// commits. Otherwise a different provider from the worker's wins when
/// one is staffed, else another session of the same provider; ties go
/// to the alias order.
pub fn pick_reviewer(
    worker: &str,
    worker_provider: Option<&str>,
    previous: Option<&str>,
    exclude: &[String],
    agents: &[Candidate],
) -> Option<String> {
    let group = worker_group(worker, agents);
    let eligible: Vec<&Candidate> = agents
        .iter()
        .filter(|a| {
            !group.contains(&a.alias)
                && !exclude.contains(&a.alias)
                && !crate::master::is_master(&a.alias)
                && a.enabled
                && a.state != "attention"
                && a.provider != "inbox"
        })
        .collect();
    if let Some(prev) = previous {
        if eligible.iter().any(|a| a.alias == prev) {
            return Some(prev.to_string());
        }
    }
    let mut ranked = eligible;
    ranked.sort_by(|a, b| {
        let same = |c: &Candidate| Some(c.provider.as_str()) == worker_provider;
        same(a).cmp(&same(b)).then(a.alias.cmp(&b.alias))
    });
    ranked.first().map(|a| a.alias.clone())
}

/// Filing order of one agent's reports on a ticket: `(UTC second,
/// same-second counter)`. The writer names a report
/// `<UTC-basic>-<agent>.md` and a same-second one `…-<agent>-<n>.md`,
/// which sorts BEFORE the first by name (`-` < `.`) — so name order can
/// put a later report first. `agent` must be the report's author: an
/// alias may itself contain `-<digits>`.
pub fn filing_order(name: &str, agent: &str) -> (String, u32) {
    let stem = name.strip_suffix(".md").unwrap_or(name);
    let Some((at, rest)) = stem.split_once('-') else {
        return (stem.to_string(), 0);
    };
    let n = rest
        .strip_prefix(agent)
        .and_then(|tail| match tail {
            "" => Some(0),
            t => t.strip_prefix('-')?.parse().ok(),
        })
        .unwrap_or(0);
    (at.to_string(), n)
}

/// The daemon-composed review kickoff: one line, so a pty reviewer can
/// take it as it is. It carries the PR, the head, the acceptance
/// criteria and the pinning rules. When the criteria do not fit
/// `ceiling`, the line points at the ticket file instead of cutting
/// them.
#[allow(clippy::too_many_arguments)]
pub fn review_kickoff(
    issue: &str,
    round: u32,
    pr: &str,
    sha: &str,
    worker: &str,
    acceptance: Option<&str>,
    note: &Path,
    ceiling: usize,
) -> String {
    let build = |criteria: &str| {
        format!(
            "[review] {issue} round {round}: independently review PR {pr} at head {sha} \
             (worker {worker}; ticket {note}). {criteria}Rules: judge exactly this head — \
             check out {sha}, not the branch tip; never push to the branch; the verdict is \
             pinned to the sha, and if the head moves the daemon sends a new review. File \
             the verdict: `cadence report file --task {issue} --kind verdict --file <f>` \
             with frontmatter `verdict: pass` or `verdict: revise` and `sha: {sha}`, the \
             findings in the body. Only you can file it, and only for this head.",
            note = note.display()
        )
    };
    let criteria = match acceptance {
        Some(list) => format!("Acceptance: {list}. "),
        None => "The ticket lists no acceptance criteria — judge it against its text. ".to_string(),
    };
    let full = build(&criteria);
    if full.len() <= ceiling {
        return full;
    }
    build(&format!(
        "Acceptance: too long for this endpoint's kickoff — read all of it in {}. ",
        note.display()
    ))
}

/// The REVISE hand-back to the worker, pinned to the same ticket.
pub fn revise_message(
    issue: &str,
    reviewer: &str,
    sha: &str,
    round: u32,
    report: &str,
    summary: &str,
    pr: &str,
) -> String {
    format!(
        "[revise] {issue}: reviewer {reviewer} asks for changes at {sha} (REVISE {round} of \
         {MAX_REVISE}). Findings: {report} — {summary}. Fix it on the same branch and push, \
         then file `cadence report file --task {issue} --kind done` with `sha: <new head>` \
         and `pr: {pr}`; the new head goes back to review."
    )
}

/// `cadence delivery sync`: for every open loop with a PR, read the PR
/// from GitHub with the operator's own `gh`, hand the observation to the
/// daemon, and disable auto-merge where the daemon says the head moved
/// past what was reviewed. `only` limits it to one ticket. Runs in the
/// operator's process; the daemon refuses the observation from anyone
/// else.
pub fn sync(state_dir: &Path, only: Option<&str>, gh_bin: &str) -> Result<Value> {
    let list = crate::client::rpc(state_dir, "delivery_list", json!({}))?;
    let mut out = Vec::new();
    for rec in list["records"].as_array().cloned().unwrap_or_default() {
        let issue = rec["issue"].as_str().unwrap_or_default().to_string();
        if only.is_some_and(|o| o != issue) {
            continue;
        }
        let state = rec["state"].as_str().unwrap_or_default();
        let Some(url) = rec["pr"].as_str() else {
            continue;
        };
        // A finished loop is read again only to turn auto-merge off.
        if matches!(state, "merged" | "declined" | "closed") && rec["disable_auto"] != true {
            continue;
        }
        out.push(sync_pr(state_dir, &issue, url, gh_bin));
    }
    Ok(json!({"synced": out}))
}

/// A loop the board's unattended sync (CAD-446) reads GitHub for: a PR
/// under review, PASSed or enqueued — whose head, CI or merge GitHub may
/// change — or any loop whose auto-merge must be turned off. A ticket
/// the worker still holds (dispatched, or back after a REVISE) waits on
/// the worker, not on GitHub, and costs no `gh` call.
pub fn awaiting_github(rec: &Record) -> bool {
    rec.pr.is_some()
        && (matches!(
            rec.state,
            State::Reviewing | State::Passed | State::Enqueued
        ) || rec.disable_auto)
}

/// Why `pr` is not a PR of `project`'s own repos, if it is not: it must
/// parse as a pull request URL whose repo is one of the project's
/// `repos[].remote` (compared normalized). The daemon applies it when a
/// done report names a PR; the board's unattended sync applies it again
/// before its `gh` reads one.
pub fn project_pr_refusal(pm_dir: &Path, project: &str, pr: &str) -> Result<Option<String>> {
    let Some(key) = pr_ref(pr) else {
        return Ok(Some(format!(
            "names `pr: {pr}`, which is not a pull request URL"
        )));
    };
    let (slug, _) = crate::issue::task_report::parse_pr_url(pr)?;
    let want =
        crate::issue::project::normalize_remote(&format!("github.com/{slug}")).to_ascii_lowercase();
    let found = crate::issue::project::list(pm_dir)?
        .into_iter()
        .find(|p| p.key == project);
    let remotes: Vec<String> = found
        .iter()
        .flat_map(|p| p.repos.iter())
        .filter_map(|r| r.remote.as_deref())
        .map(|r| crate::issue::project::normalize_remote(r).to_ascii_lowercase())
        .collect();
    if remotes.contains(&want) {
        return Ok(None);
    }
    let listed = if remotes.is_empty() {
        "none — `repos[].remote` is unset".to_string()
    } else {
        remotes.join(", ")
    };
    Ok(Some(format!(
        "names {key}, which is not a repo of project {project} (its remotes: {listed})"
    )))
}

/// Observe one ticket's PR — the row [`sync`] reports for it, with
/// `error` set when reading or reporting it failed.
pub fn sync_pr(state_dir: &Path, issue: &str, url: &str, gh_bin: &str) -> Value {
    match sync_one(state_dir, issue, url, gh_bin) {
        Ok(v) => v,
        Err(e) => json!({"issue": issue, "error": e.to_string()}),
    }
}

fn sync_one(state_dir: &Path, issue: &str, url: &str, gh_bin: &str) -> Result<Value> {
    let (slug, number) = crate::issue::task_report::parse_pr_url(url)?;
    let view = gh(
        gh_bin,
        &[
            "pr",
            "view",
            &number.to_string(),
            "-R",
            &slug,
            "--json",
            "headRefOid,state,statusCheckRollup,additions,deletions,changedFiles,autoMergeRequest",
        ],
    )?;
    let pr: Value = serde_json::from_str(&view)
        .map_err(|e| Error::rejected(format!("gh pr view: unreadable ({e})")))?;
    let rollup = pr["statusCheckRollup"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let observed = json!({
        "issue": issue,
        "head": pr["headRefOid"].as_str().unwrap_or_default(),
        "pr_state": pr["state"].as_str().unwrap_or_default(),
        "ci_green": crate::overview::checks_green_pub(&rollup),
        "auto_merge": !pr["autoMergeRequest"].is_null(),
        "additions": pr["additions"].as_u64().unwrap_or(0),
        "deletions": pr["deletions"].as_u64().unwrap_or(0),
        "files": pr["changedFiles"].as_u64().unwrap_or(0),
    });
    let mut answer = crate::client::rpc(state_dir, "delivery_observe", observed)?;
    if answer["disable_auto"] == true {
        gh(
            gh_bin,
            &[
                "pr",
                "merge",
                &number.to_string(),
                "-R",
                &slug,
                "--disable-auto",
            ],
        )?;
        answer["auto_merge_disabled"] = json!(true);
    }
    Ok(answer)
}

/// `cadence delivery merge <ID>`: the operator's merge decision. The
/// daemon checks the caller is the proven operator and the PASS stands
/// on the head GitHub shows with green CI (a fresh [`sync`] first);
/// then the operator's own `gh` enqueues it in the merge queue pinned
/// to the reviewed head, and the daemon records it. A refused check
/// runs no `gh` merge at all.
pub fn merge(state_dir: &Path, issue: &str, gh_bin: &str) -> Result<Value> {
    // The check runs first: an agent is refused before anything else,
    // the sync included.
    crate::client::rpc(
        state_dir,
        "delivery_merge",
        json!({"issue": issue, "phase": "authorize"}),
    )?;
    let synced = sync(state_dir, Some(issue), gh_bin)?;
    if let Some(e) = synced["synced"][0]["error"].as_str() {
        return Err(Error::rejected(format!(
            "{issue}: reading the PR failed, nothing was merged — {e}"
        )));
    }
    let check = crate::client::rpc(
        state_dir,
        "delivery_merge",
        json!({"issue": issue, "phase": "check"}),
    )?;
    let sha = check["sha"].as_str().unwrap_or_default().to_string();
    let url = check["pr"].as_str().unwrap_or_default();
    let (slug, number) = crate::issue::task_report::parse_pr_url(url)?;
    gh(
        gh_bin,
        &[
            "pr",
            "merge",
            &number.to_string(),
            "-R",
            &slug,
            "--auto",
            "--squash",
            "--match-head-commit",
            &sha,
        ],
    )?;
    crate::client::rpc(
        state_dir,
        "delivery_merge",
        json!({"issue": issue, "phase": "enqueued", "sha": sha}),
    )
}

fn gh(gh_bin: &str, args: &[&str]) -> Result<String> {
    let out = crate::proc::run_bounded(Command::new(gh_bin).args(args), GH_TIMEOUT)
        .map_err(|e| Error::rejected(format!("gh {}: {e}", args.join(" "))))?;
    if !out.status.success() {
        return Err(Error::rejected(format!(
            "gh {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(alias: &str, provider: &str) -> Candidate {
        Candidate {
            alias: alias.into(),
            provider: provider.into(),
            state: "idle".into(),
            enabled: true,
            upstream: None,
        }
    }

    #[test]
    fn reviewer_is_independent_and_prefers_another_provider() {
        let agents = vec![
            cand("a-claude", "claude"),
            cand("master", "claude"),
            cand("w1", "claude"),
            cand("z-codex", "codex"),
        ];
        // A different provider wins over alias order.
        assert_eq!(
            pick_reviewer("w1", Some("claude"), None, &[], &agents).as_deref(),
            Some("z-codex")
        );
        // None staffed: another session of the same provider.
        let same = vec![
            cand("master", "claude"),
            cand("w1", "claude"),
            cand("r", "claude"),
        ];
        assert_eq!(
            pick_reviewer("w1", Some("claude"), None, &[], &same).as_deref(),
            Some("r")
        );
        // Never the worker, never the master.
        let alone = vec![cand("master", "codex"), cand("w1", "claude")];
        assert_eq!(pick_reviewer("w1", Some("claude"), None, &[], &alone), None);
        // Fenced or disabled agents are skipped; the previous reviewer
        // keeps the ticket while it qualifies.
        let mut fenced = cand("z-codex", "codex");
        fenced.state = "attention".into();
        let agents = vec![cand("a-claude", "claude"), fenced, cand("w1", "claude")];
        assert_eq!(
            pick_reviewer("w1", Some("claude"), None, &[], &agents).as_deref(),
            Some("a-claude")
        );
        let agents = vec![cand("a", "codex"), cand("b", "codex"), cand("w1", "claude")];
        assert_eq!(
            pick_reviewer("w1", Some("claude"), Some("b"), &[], &agents).as_deref(),
            Some("b")
        );
        // A previous reviewer that is the worker now is not reused.
        assert_eq!(
            pick_reviewer("w1", Some("claude"), Some("w1"), &[], &agents).as_deref(),
            Some("a")
        );
    }

    #[test]
    fn filing_order_puts_a_same_second_report_after_the_first() {
        let mut names = vec![
            "20260924T041026Z-w1-1.md", // filed second, sorts first by name
            "20260924T041026Z-w1.md",
            "20260924T041025Z-w1.md",
            "20260924T041026Z-w1-10.md",
            "20260924T041026Z-w1-2.md",
        ];
        names.sort();
        names.sort_by_key(|n| filing_order(n, "w1"));
        assert_eq!(
            names,
            vec![
                "20260924T041025Z-w1.md",
                "20260924T041026Z-w1.md",
                "20260924T041026Z-w1-1.md",
                "20260924T041026Z-w1-2.md",
                "20260924T041026Z-w1-10.md",
            ]
        );
        // An alias that ends in digits is not mistaken for a counter.
        assert_eq!(
            filing_order("20260924T041026Z-dev-1.md", "dev-1"),
            ("20260924T041026Z".into(), 0)
        );
        assert_eq!(
            filing_order("20260924T041026Z-dev-1-3.md", "dev-1"),
            ("20260924T041026Z".into(), 3)
        );
    }

    #[test]
    fn reviewer_is_never_in_the_workers_group_line() {
        let with_up = |alias: &str, up: &str| Candidate {
            upstream: Some(up.into()),
            ..cand(alias, "codex")
        };
        let agents = vec![
            with_up("a1", "w1"),   // w1's member
            with_up("a2", "a1"),   // its member's member
            with_up("w1", "pm"),   // the worker, under pm
            with_up("pm", "boss"), // the worker's PM, under boss
            cand("boss", "codex"),
            with_up("sib", "pm"), // a sibling under the same PM
        ];
        assert_eq!(
            pick_reviewer("w1", Some("claude"), None, &[], &agents).as_deref(),
            Some("sib")
        );
        // Nor as the sticky previous reviewer.
        assert_eq!(
            pick_reviewer("w1", Some("claude"), Some("a1"), &[], &agents).as_deref(),
            Some("sib")
        );
        // A reviewer excluded (it pushed the moved head) is skipped.
        assert_eq!(
            pick_reviewer("w1", Some("claude"), None, &["sib".into()], &agents),
            None
        );
        // An upstream cycle ends.
        let cyc = vec![with_up("w1", "x"), with_up("x", "w1"), cand("r", "codex")];
        assert_eq!(
            pick_reviewer("w1", None, None, &[], &cyc).as_deref(),
            Some("r")
        );
    }

    #[test]
    fn kickoff_is_one_line_and_never_cuts_criteria() {
        let note = Path::new("/pm/demo/D-2/issue.md");
        let sha = "a".repeat(40);
        let k = review_kickoff(
            "D-2",
            1,
            "https://github.com/o/r/pull/7",
            &sha,
            "w1",
            Some("1) [ ] \"x\""),
            note,
            4000,
        );
        assert!(!k.contains('\n'), "{k}");
        assert!(k.contains(&sha) && k.contains("pull/7") && k.contains("1) [ ] \"x\""));
        assert!(k.contains("--kind verdict"), "{k}");
        let long = "y".repeat(5000);
        let k = review_kickoff("D-2", 1, "u", &sha, "w1", Some(&long), note, 4000);
        assert!(!k.contains(&long));
        assert!(k.contains("/pm/demo/D-2/issue.md"), "{k}");
    }

    #[test]
    fn merge_ready_needs_pass_on_the_observed_green_head() {
        let mut r = Record::new("D-2", "demo", "w1", 0);
        assert!(!r.merge_ready());
        r.state = State::Passed;
        r.verdict = Some(VerdictRec {
            verdict: "pass".into(),
            sha: "a".repeat(40),
            reviewer: "r1".into(),
            summary: "ok".into(),
            report: "D-2/reports/x.md".into(),
            at: 0,
        });
        assert!(!r.merge_ready(), "no observation yet");
        r.observed = Some(Observed {
            head: "a".repeat(40),
            pr_state: "OPEN".into(),
            ci_green: false,
            ..Observed::default()
        });
        assert!(!r.merge_ready(), "CI not green");
        r.observed.as_mut().unwrap().ci_green = true;
        assert!(r.merge_ready());
        r.observed.as_mut().unwrap().head = "b".repeat(40);
        assert!(!r.merge_ready(), "moved head");
    }
}
